use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet, VecDeque};
use std::hash::{Hash, Hasher};
use std::rc::Rc;
use std::sync::Arc;

use bitcoin_hashes::sha256::Hash as Sha256;
use bitcoin_hashes::Hash as _;
use lightning::bitcoin;
use lightning::chain::{BestBlock, ChannelMonitorUpdateStatus};
use lightning::chain::chainmonitor;
use lightning::chain::chaininterface::{BroadcasterInterface, FeeEstimator, FEERATE_FLOOR_SATS_PER_KW};
use lightning::events::Event;
use lightning::events::{EventsProvider, ReplayEvent};
use lightning::ln::channelmanager::{ChainParameters, SimpleArcChannelManager};
use lightning::ln::peer_handler::{IgnoringMessageHandler, MessageHandler, PeerHandleError, PeerManager, SocketDescriptor};
use lightning::util::errors::APIError;
use lightning::onion_message::messenger::DefaultMessageRouter;
use lightning::routing::gossip::NetworkGraph;
use lightning::routing::router::DefaultRouter;
use lightning::routing::scoring::{
    ProbabilisticScorer, ProbabilisticScoringDecayParameters, ProbabilisticScoringFeeParameters,
};
use lightning::sign::{InMemorySigner, NodeSigner, Recipient};
use lightning::sign::KeysManager;
use lightning::util::config::UserConfig;
use lightning::util::logger::{Logger, Record};
use lightning::util::persist::MonitorName;
use secp256k1::PublicKey as SecpPublicKey;
use wasm_bindgen::prelude::JsValue;

use crate::ldk_runtime::{
    LdkRuntimeFundingRequestData, LdkRuntimeFundingTxSubmissionData, LdkRuntimeOpenChannelRequestData,
    LdkRuntimeOpenChannelResultData,
};

pub trait LdkLiveBackend {
    fn new_outbound_connection(&self, peer_pubkey: &str) -> Result<String, JsValue>;
    fn read_event(&self, payload_hex: &str) -> Result<(), JsValue>;
    fn process_events(&self) -> Result<(), JsValue>;
    fn take_outbound_frames(&self) -> Result<Vec<String>, JsValue>;
    fn socket_disconnected(&self) -> Result<(), JsValue>;
    fn is_peer_handshake_complete(&self, peer_pubkey: &str) -> Result<bool, JsValue>;
    fn open_channel_non_virtual(
        &self,
        request: LdkRuntimeOpenChannelRequestData,
    ) -> Result<LdkRuntimeOpenChannelResultData, JsValue>;
    fn list_pending_funding_requests(&self) -> Result<Vec<LdkRuntimeFundingRequestData>, JsValue>;
    fn submit_funding_transaction(
        &self,
        request: LdkRuntimeFundingTxSubmissionData,
    ) -> Result<(), JsValue>;
    fn list_live_channels(&self) -> Result<Vec<LdkRuntimeOpenChannelResultData>, JsValue>;
    fn local_node_pubkey(&self) -> Result<String, JsValue>;

    fn chain_relevant_txids(&self) -> Result<Vec<String>, JsValue> {
        Ok(Vec::new())
    }

    fn chain_apply_best_block(&self, _height: u32, _header_hex: &str) -> Result<(), JsValue> {
        Ok(())
    }

    fn chain_apply_confirmed_tx(
        &self,
        _height: u32,
        _header_hex: &str,
        _tx_index: usize,
        _tx_hex: &str,
    ) -> Result<(), JsValue> {
        Ok(())
    }

    fn chain_apply_unconfirmed_tx(&self, _txid: &str) -> Result<(), JsValue> {
        Ok(())
    }
}

#[allow(dead_code)]
pub struct WasmLdkLiveBackend {
    runtime_key: String,
    node_seed32: Option<[u8; 32]>,
    connected_peers: RefCell<HashSet<String>>,
    active_peer_pubkey: RefCell<Option<String>>,
    inbound_frames: RefCell<VecDeque<String>>,
    outbound_frames: RefCell<VecDeque<String>>,
    disconnected: Cell<bool>,
    descriptor_nonce: Cell<u64>,
    pending_funding_requests: RefCell<HashMap<String, LdkRuntimeFundingRequestData>>,
    object_graph: RefCell<Option<LdkObjectGraph>>,
}

struct LdkObjectGraph {
    logger: Arc<WasmLdkLogger>,
    fee_estimator: Arc<FixedFeeEstimator>,
    broadcaster: Arc<NoopBroadcaster>,
    keys_manager: Arc<KeysManager>,
    peer_manager: RefCell<WasmPeerManager>,
    chain_monitor: Arc<WasmChainMonitor>,
    channel_manager: Arc<WasmChannelManager>,
    active_descriptor: RefCell<Option<LiveSocketDescriptor>>,
    // Phase-3.1 bootstrap only: concrete ChannelManager/PeerManager/ChainMonitor
    // objects are not yet constructed.
    channel_manager_ready: bool,
    peer_manager_ready: bool,
    chain_monitor_ready: bool,
}

type WasmPeerManager = PeerManager<
    LiveSocketDescriptor,
    Arc<WasmChannelManager>,
    Arc<IgnoringMessageHandler>,
    IgnoringMessageHandler,
    Arc<WasmLdkLogger>,
    Arc<crate::rgb_ln_wire::RgbLnForkCustomMessageHandler>,
    Arc<KeysManager>,
    Arc<WasmChainMonitor>,
>;

type WasmChainMonitor = chainmonitor::ChainMonitor<
    InMemorySigner,
    Arc<NoopFilter>,
    Arc<NoopBroadcaster>,
    Arc<FixedFeeEstimator>,
    Arc<WasmLdkLogger>,
    Arc<NoopPersister>,
    Arc<KeysManager>,
>;

type WasmChannelManager =
    SimpleArcChannelManager<WasmChainMonitor, NoopBroadcaster, FixedFeeEstimator, WasmLdkLogger>;

#[derive(Clone)]
struct LiveSocketDescriptor {
    id: u64,
    outbound: Arc<std::sync::Mutex<VecDeque<Vec<u8>>>>,
}

impl PartialEq for LiveSocketDescriptor {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}
impl Eq for LiveSocketDescriptor {}
impl Hash for LiveSocketDescriptor {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.id.hash(state);
    }
}

impl SocketDescriptor for LiveSocketDescriptor {
    fn send_data(&mut self, data: &[u8], _continue_read: bool) -> usize {
        if !data.is_empty() {
            if let Ok(mut q) = self.outbound.lock() {
                q.push_back(data.to_vec());
            }
        }
        data.len()
    }

    fn disconnect_socket(&mut self) {}
}

struct WasmLdkLogger;

impl Logger for WasmLdkLogger {
    fn log(&self, record: Record) {
        let msg = format!(
            "[rln-wasm-sdk ldk] {}:{} {}",
            record.module_path,
            record.line,
            record.args
        );
        #[cfg(target_arch = "wasm32")]
        web_sys::console::log_1(&JsValue::from_str(&msg));
        #[cfg(not(target_arch = "wasm32"))]
        let _ = msg;
    }
}

struct FixedFeeEstimator;

impl FeeEstimator for FixedFeeEstimator {
    fn get_est_sat_per_1000_weight(&self, _confirmation_target: lightning::chain::chaininterface::ConfirmationTarget) -> u32 {
        // Keep at least protocol floor for deterministic bootstrap behavior.
        FEERATE_FLOOR_SATS_PER_KW
    }
}

struct NoopBroadcaster;

impl BroadcasterInterface for NoopBroadcaster {
    fn broadcast_transactions(&self, _txs: &[&bitcoin::Transaction]) {}
}

struct NoopFilter;
impl lightning::chain::Filter for NoopFilter {
    fn register_tx(&self, _txid: &bitcoin::Txid, _script_pubkey: &bitcoin::Script) {}
    fn register_output(&self, _output: lightning::chain::WatchedOutput) {}
}

struct NoopPersister;
impl chainmonitor::Persist<InMemorySigner> for NoopPersister {
    fn persist_new_channel(
        &self,
        _monitor_name: MonitorName,
        _monitor: &lightning::chain::channelmonitor::ChannelMonitor<InMemorySigner>,
    ) -> ChannelMonitorUpdateStatus {
        ChannelMonitorUpdateStatus::Completed
    }

    fn update_persisted_channel(
        &self,
        _monitor_name: MonitorName,
        _monitor_update: Option<&lightning::chain::channelmonitor::ChannelMonitorUpdate>,
        _monitor: &lightning::chain::channelmonitor::ChannelMonitor<InMemorySigner>,
    ) -> ChannelMonitorUpdateStatus {
        ChannelMonitorUpdateStatus::Completed
    }

    fn archive_persisted_channel(
        &self,
        _monitor_name: MonitorName,
    ) {
    }
}

impl WasmLdkLiveBackend {
    fn new(runtime_key: String, node_seed32: Option<[u8; 32]>) -> Self {
        Self {
            runtime_key,
            node_seed32,
            connected_peers: RefCell::new(HashSet::new()),
            active_peer_pubkey: RefCell::new(None),
            inbound_frames: RefCell::new(VecDeque::new()),
            outbound_frames: RefCell::new(VecDeque::new()),
            disconnected: Cell::new(false),
            descriptor_nonce: Cell::new(1),
            pending_funding_requests: RefCell::new(HashMap::new()),
            object_graph: RefCell::new(None),
        }
    }

    fn ensure_object_graph(&self) -> Result<(), JsValue> {
        if self.object_graph.borrow().is_some() {
            return Ok(());
        }
        let seed = self.derive_seed32();
        let logger = Arc::new(WasmLdkLogger);
        let fee_estimator = Arc::new(FixedFeeEstimator);
        let broadcaster = Arc::new(NoopBroadcaster);
        let persister = Arc::new(NoopPersister);
        let chain_source = Arc::new(NoopFilter);
        let keys_manager = Arc::new(KeysManager::new(
            &seed,
            unix_now_secs(),
            unix_now_nanos(),
            true,
            std::path::PathBuf::from(format!("/tmp/rln_wasm_ldk_{}", &self.runtime_key)),
        ));
        let chain_monitor: Arc<WasmChainMonitor> = Arc::new(chainmonitor::ChainMonitor::new(
            Some(Arc::clone(&chain_source)),
            Arc::clone(&broadcaster),
            Arc::clone(&logger),
            Arc::clone(&fee_estimator),
            Arc::clone(&persister),
            Arc::clone(&keys_manager),
            keys_manager.get_peer_storage_key(),
        ));
        let network_graph = Arc::new(NetworkGraph::new(
            bitcoin::Network::Regtest,
            Arc::clone(&logger),
        ));
        let scorer = Arc::new(std::sync::RwLock::new(ProbabilisticScorer::new(
            ProbabilisticScoringDecayParameters::default(),
            Arc::clone(&network_graph),
            Arc::clone(&logger),
        )));
        let router = Arc::new(DefaultRouter::new(
            Arc::clone(&network_graph),
            Arc::clone(&logger),
            Arc::clone(&keys_manager),
            scorer,
            ProbabilisticScoringFeeParameters::default(),
        ));
        let message_router = Arc::new(DefaultMessageRouter::new(
            Arc::clone(&network_graph),
            Arc::clone(&keys_manager),
        ));
        let user_config = UserConfig::default();
        let chain_params = ChainParameters {
            network: bitcoin::Network::Regtest,
            best_block: BestBlock::from_network(bitcoin::Network::Regtest),
        };
        let channel_manager: Arc<WasmChannelManager> = Arc::new(
            lightning::ln::channelmanager::ChannelManager::new(
                Arc::clone(&fee_estimator),
                Arc::clone(&chain_monitor),
                Arc::clone(&broadcaster),
                router,
                message_router,
                Arc::clone(&logger),
                Arc::clone(&keys_manager),
                Arc::clone(&keys_manager),
                Arc::clone(&keys_manager),
                user_config,
                chain_params,
                unix_now_secs() as u32,
                std::path::PathBuf::from(format!("/tmp/rln_wasm_ldk_{}", &self.runtime_key)),
            ),
        );
        let pm_rand = self.derive_seed32();
        let fork_custom_wire = crate::rgb_ln_wire::rgb_ln_fork_custom_message_handler();
        let peer_manager = PeerManager::new(
            MessageHandler {
                chan_handler: Arc::clone(&channel_manager),
                route_handler: Arc::new(IgnoringMessageHandler {}),
                onion_message_handler: IgnoringMessageHandler {},
                custom_message_handler: Arc::clone(&fork_custom_wire),
                send_only_message_handler: Arc::clone(&chain_monitor),
            },
            unix_now_secs() as u32,
            &pm_rand,
            Arc::clone(&logger),
            Arc::clone(&keys_manager),
        );
        self.object_graph.borrow_mut().replace(LdkObjectGraph {
            logger,
            fee_estimator,
            broadcaster,
            keys_manager,
            peer_manager: RefCell::new(peer_manager),
            chain_monitor,
            channel_manager,
            active_descriptor: RefCell::new(None),
            channel_manager_ready: true,
            peer_manager_ready: true,
            chain_monitor_ready: true,
        });
        Ok(())
    }

    fn derive_seed32(&self) -> [u8; 32] {
        if let Some(seed) = self.node_seed32 {
            return seed;
        }
        let hash = <Sha256 as bitcoin_hashes::Hash>::hash(self.runtime_key.as_bytes()).to_byte_array();
        hash
    }

    fn ensure_phase1_runtime_ready(&self) -> Result<(), JsValue> {
        self.ensure_object_graph()?;
        let graph = self.object_graph.borrow();
        let Some(g) = graph.as_ref() else {
            return Err(JsValue::from_str(
                sdk_contracts::ERR_LDK_OBJECT_GRAPH_NOT_INITIALIZED,
            ));
        };
        let _ = (&g.logger, &g.fee_estimator, &g.broadcaster, &g.keys_manager);
        Ok(())
    }

    fn next_descriptor_id(&self) -> u64 {
        let current = self.descriptor_nonce.get();
        let next = current.saturating_add(1);
        self.descriptor_nonce.set(next);
        current
    }

    fn next_user_channel_id(&self) -> u128 {
        let hash = <Sha256 as bitcoin_hashes::Hash>::hash(
            format!("{}:{}", self.runtime_key, self.next_descriptor_id()).as_bytes(),
        )
        .to_byte_array();
        let mut out = [0u8; 16];
        out.copy_from_slice(&hash[..16]);
        u128::from_be_bytes(out)
    }

    fn derive_channel_status_from_live(
        &self,
        g: &LdkObjectGraph,
        temporary_channel_id_hex: &str,
    ) -> (String, String, bool, bool) {
        let mut status = "opening".to_string();
        let mut resolved_channel_id = temporary_channel_id_hex.to_string();

        let collected_events: RefCell<Vec<Event>> = RefCell::new(Vec::new());
        g.channel_manager.process_pending_events(&|event: Event| {
            collected_events.borrow_mut().push(event);
            Ok::<(), ReplayEvent>(())
        });
        for event in collected_events.into_inner().into_iter() {
            match event {
                Event::FundingGenerationReady {
                    temporary_channel_id: ev_temp,
                    counterparty_node_id,
                    channel_value_satoshis,
                    output_script,
                    ..
                } if format!("{ev_temp}") == temporary_channel_id_hex => {
                    status = "awaiting_funding_tx".to_string();
                    self.pending_funding_requests.borrow_mut().insert(
                        temporary_channel_id_hex.to_string(),
                        LdkRuntimeFundingRequestData {
                            temporary_channel_id: temporary_channel_id_hex.to_string(),
                            counterparty_node_id: hex::encode(counterparty_node_id.serialize()),
                            channel_value_satoshis,
                            output_script_hex: hex::encode(output_script.as_bytes()),
                        },
                    );
                }
                Event::ChannelPending {
                    channel_id,
                    former_temporary_channel_id,
                    ..
                } => {
                    let matches = former_temporary_channel_id
                        .map(|id| format!("{id}") == temporary_channel_id_hex)
                        .unwrap_or(false);
                    if matches {
                        resolved_channel_id = format!("{channel_id}");
                        status = "pending".to_string();
                    }
                }
                Event::ChannelReady { channel_id, .. } => {
                    if format!("{channel_id}") == temporary_channel_id_hex {
                        status = "ready".to_string();
                    }
                }
                _ => {}
            }
        }

        if let Some(details) = g
            .channel_manager
            .list_channels()
            .into_iter()
            .find(|c| format!("{}", c.channel_id) == temporary_channel_id_hex)
        {
            resolved_channel_id = format!("{}", details.channel_id);
            if details.is_usable {
                status = "ready".to_string();
            } else if details.is_channel_ready {
                status = "pending".to_string();
            }
            return (
                temporary_channel_id_hex.to_string(),
                resolved_channel_id,
                details.is_channel_ready,
                details.is_usable,
            );
        }
        (
            temporary_channel_id_hex.to_string(),
            resolved_channel_id,
            status == "ready",
            false,
        )
    }
}

impl LdkLiveBackend for WasmLdkLiveBackend {
    fn new_outbound_connection(&self, peer_pubkey: &str) -> Result<String, JsValue> {
        self.ensure_phase1_runtime_ready()?;
        let peer_pubkey = peer_pubkey.trim();
        if peer_pubkey.is_empty() {
            return Err(JsValue::from_str(sdk_contracts::ERR_PEER_PUBKEY_INVALID));
        }
        if SecpPublicKey::from_slice(
            &hex::decode(peer_pubkey)
                .map_err(|_| JsValue::from_str(sdk_contracts::ERR_PEER_PUBKEY_INVALID))?,
        )
        .is_err()
        {
            return Err(JsValue::from_str(sdk_contracts::ERR_PEER_PUBKEY_INVALID));
        }
        let secp_pubkey = SecpPublicKey::from_slice(
            &hex::decode(peer_pubkey)
                .map_err(|_| JsValue::from_str(sdk_contracts::ERR_PEER_PUBKEY_INVALID))?,
        )
        .map_err(|_| JsValue::from_str(sdk_contracts::ERR_PEER_PUBKEY_INVALID))?;

        self.disconnected.set(false);
        self.connected_peers.borrow_mut().insert(peer_pubkey.to_string());

        let graph = self.object_graph.borrow();
        let Some(g) = graph.as_ref() else {
            return Err(JsValue::from_str(
                sdk_contracts::ERR_LDK_OBJECT_GRAPH_NOT_INITIALIZED,
            ));
        };

        if let Some(prev) = g.active_descriptor.borrow().as_ref().cloned() {
            g.peer_manager.borrow().socket_disconnected(&prev);
            g.active_descriptor.borrow_mut().take();
        }

        let descriptor = LiveSocketDescriptor {
            id: self.next_descriptor_id(),
            outbound: Arc::new(std::sync::Mutex::new(VecDeque::new())),
        };
        let pm = g.peer_manager.borrow_mut();
        let act_one = pm
            .new_outbound_connection(secp_pubkey, descriptor.clone(), None)
            .map_err(|_e: PeerHandleError| {
                JsValue::from_str(sdk_contracts::ERR_PEER_MANAGER_NEW_OUTBOUND_FAILED)
            })?;
        pm.process_events();
        g.active_descriptor.borrow_mut().replace(descriptor);
        self.active_peer_pubkey
            .borrow_mut()
            .replace(peer_pubkey.to_string());
        Ok(hex::encode(act_one))
    }

    fn read_event(&self, payload_hex: &str) -> Result<(), JsValue> {
        self.ensure_phase1_runtime_ready()?;
        if self.disconnected.get() {
            return Err(JsValue::from_str(
                sdk_contracts::ERR_PEER_TRANSPORT_DISCONNECTED,
            ));
        }
        let payload_hex = payload_hex.trim();
        if payload_hex.is_empty() {
            return Err(JsValue::from_str(sdk_contracts::ERR_PAYLOAD_HEX_EMPTY));
        }
        let _ =
            hex::decode(payload_hex).map_err(|e| JsValue::from_str(&format!("invalid payload_hex: {e}")))?;
        let bytes =
            hex::decode(payload_hex).map_err(|e| JsValue::from_str(&format!("invalid payload_hex: {e}")))?;
        let graph = self.object_graph.borrow();
        let Some(g) = graph.as_ref() else {
            return Err(JsValue::from_str(
                sdk_contracts::ERR_LDK_OBJECT_GRAPH_NOT_INITIALIZED,
            ));
        };
        let mut desc = g
            .active_descriptor
            .borrow()
            .as_ref()
            .cloned()
            .ok_or_else(|| {
                JsValue::from_str(sdk_contracts::ERR_ACTIVE_PEER_DESCRIPTOR_MISSING)
            })?;
        g.peer_manager
            .borrow_mut()
            .read_event(&mut desc, &bytes)
            .map_err(|_e: PeerHandleError| {
                JsValue::from_str(sdk_contracts::ERR_PEER_MANAGER_READ_EVENT_FAILED)
            })?;
        Ok(())
    }

    fn process_events(&self) -> Result<(), JsValue> {
        self.ensure_phase1_runtime_ready()?;
        if self.disconnected.get() {
            return Err(JsValue::from_str(
                sdk_contracts::ERR_PEER_TRANSPORT_DISCONNECTED,
            ));
        }
        let graph = self.object_graph.borrow();
        let Some(g) = graph.as_ref() else {
            return Err(JsValue::from_str(
                sdk_contracts::ERR_LDK_OBJECT_GRAPH_NOT_INITIALIZED,
            ));
        };
        g.peer_manager.borrow().process_events();
        Ok(())
    }

    fn take_outbound_frames(&self) -> Result<Vec<String>, JsValue> {
        self.ensure_phase1_runtime_ready()?;
        if self.disconnected.get() {
            return Ok(Vec::new());
        }
        let graph = self.object_graph.borrow();
        let Some(g) = graph.as_ref() else {
            return Err(JsValue::from_str(
                sdk_contracts::ERR_LDK_OBJECT_GRAPH_NOT_INITIALIZED,
            ));
        };
        let desc = g
            .active_descriptor
            .borrow()
            .as_ref()
            .cloned()
            .ok_or_else(|| {
                JsValue::from_str(sdk_contracts::ERR_ACTIVE_PEER_DESCRIPTOR_MISSING)
            })?;
        let mut q = desc
            .outbound
            .lock()
            .map_err(|_| JsValue::from_str(sdk_contracts::ERR_OUTBOUND_QUEUE_LOCK_POISONED))?;
        let mut out = Vec::new();
        while let Some(frame) = q.pop_front() {
            out.push(hex::encode(frame));
        }
        Ok(out)
    }

    fn socket_disconnected(&self) -> Result<(), JsValue> {
        self.ensure_phase1_runtime_ready()?;
        self.disconnected.set(true);
        self.active_peer_pubkey.borrow_mut().take();
        self.inbound_frames.borrow_mut().clear();
        self.outbound_frames.borrow_mut().clear();
        let graph = self.object_graph.borrow();
        if let Some(g) = graph.as_ref() {
            if let Some(desc) = g.active_descriptor.borrow().as_ref().cloned() {
                g.peer_manager.borrow().socket_disconnected(&desc);
            }
            g.active_descriptor.borrow_mut().take();
        }
        Ok(())
    }

    fn is_peer_handshake_complete(&self, peer_pubkey: &str) -> Result<bool, JsValue> {
        self.ensure_phase1_runtime_ready()?;
        let peer_pubkey = peer_pubkey.trim();
        if peer_pubkey.is_empty() {
            return Ok(false);
        }
        let target = SecpPublicKey::from_slice(
            &hex::decode(peer_pubkey)
                .map_err(|_| JsValue::from_str(sdk_contracts::ERR_PEER_PUBKEY_INVALID))?,
        )
        .map_err(|_| JsValue::from_str(sdk_contracts::ERR_PEER_PUBKEY_INVALID))?;
        let graph = self.object_graph.borrow();
        let Some(g) = graph.as_ref() else {
            return Err(JsValue::from_str(
                sdk_contracts::ERR_LDK_OBJECT_GRAPH_NOT_INITIALIZED,
            ));
        };
        let is_connected = g
            .peer_manager
            .borrow()
            .list_peers()
            .into_iter()
            .any(|peer| peer.counterparty_node_id == target);
        Ok(is_connected)
    }

    fn local_node_pubkey(&self) -> Result<String, JsValue> {
        self.ensure_phase1_runtime_ready()?;
        let graph = self.object_graph.borrow();
        let Some(g) = graph.as_ref() else {
            return Err(JsValue::from_str(
                sdk_contracts::ERR_LDK_OBJECT_GRAPH_NOT_INITIALIZED,
            ));
        };
        let node_id = g
            .keys_manager
            .get_node_id(Recipient::Node)
            .map_err(|_| JsValue::from_str("failed to derive live backend node id"))?;
        Ok(node_id.to_string())
    }

    fn open_channel_non_virtual(
        &self,
        request: LdkRuntimeOpenChannelRequestData,
    ) -> Result<LdkRuntimeOpenChannelResultData, JsValue> {
        self.ensure_phase1_runtime_ready()?;
        if request.peer_pubkey.trim().is_empty() {
            return Err(JsValue::from_str(sdk_contracts::ERR_PEER_PUBKEY_INVALID));
        }
        if request.capacity_sat == 0 {
            return Err(JsValue::from_str(sdk_contracts::ERR_CAPACITY_SAT_ZERO));
        }
        if request.asset_id.is_some() || request.asset_local_amount.is_some() {
            return Err(JsValue::from_str(
                "native non-virtual RGB funding path is not wired yet; BTC-only channel open is currently supported",
            ));
        }

        let their_node_id = SecpPublicKey::from_slice(
            &hex::decode(request.peer_pubkey.trim())
                .map_err(|_| JsValue::from_str(sdk_contracts::ERR_PEER_PUBKEY_INVALID))?,
        )
        .map_err(|_| JsValue::from_str(sdk_contracts::ERR_PEER_PUBKEY_INVALID))?;

        let graph = self.object_graph.borrow();
        let Some(g) = graph.as_ref() else {
            return Err(JsValue::from_str(
                sdk_contracts::ERR_LDK_OBJECT_GRAPH_NOT_INITIALIZED,
            ));
        };

        let temporary_channel_id = g
            .channel_manager
            .create_channel(
                their_node_id,
                request.capacity_sat,
                0,
                self.next_user_channel_id(),
                None,
                None,
                None,
                None,
                false,
            )
            .map_err(|e| match e {
                APIError::APIMisuseError { err }
                | APIError::FeeRateTooHigh { err, feerate: _ }
                | APIError::ChannelUnavailable { err }
                | APIError::InvalidRoute { err } => JsValue::from_str(&err),
                APIError::MonitorUpdateInProgress => {
                    JsValue::from_str("channel monitor update in progress")
                }
                APIError::IncompatibleShutdownScript { script } => {
                    JsValue::from_str(&format!(
                        "incompatible shutdown script for peer negotiation: {script}"
                    ))
                }
            })?;

        let temp_id = format!("{temporary_channel_id}");
        g.peer_manager.borrow().process_events();
        let (temporary_channel_id, channel_id, ready, is_usable) =
            self.derive_channel_status_from_live(g, &temp_id);
        let status = if is_usable {
            "ready".to_string()
        } else if ready {
            "pending".to_string()
        } else if self.pending_funding_requests.borrow().contains_key(&temporary_channel_id) {
            "awaiting_funding_tx".to_string()
        } else {
            "opening".to_string()
        };

        Ok(LdkRuntimeOpenChannelResultData {
            temporary_channel_id,
            channel_id,
            peer_pubkey: request.peer_pubkey,
            capacity_sat: request.capacity_sat,
            status,
            ready,
            is_usable,
        })
    }

    fn list_pending_funding_requests(&self) -> Result<Vec<LdkRuntimeFundingRequestData>, JsValue> {
        self.ensure_phase1_runtime_ready()?;
        Ok(self
            .pending_funding_requests
            .borrow()
            .values()
            .cloned()
            .collect())
    }

    fn submit_funding_transaction(
        &self,
        request: LdkRuntimeFundingTxSubmissionData,
    ) -> Result<(), JsValue> {
        self.ensure_phase1_runtime_ready()?;
        let temp = request.temporary_channel_id.trim();
        let counterparty = request.counterparty_node_id.trim();
        let tx_hex = request.funding_tx_hex.trim();
        if temp.is_empty() || counterparty.is_empty() || tx_hex.is_empty() {
            return Err(JsValue::from_str(
                "temporary_channel_id, counterparty_node_id and funding_tx_hex are required",
            ));
        }

        let temp_bytes = hex::decode(temp)
            .map_err(|_| JsValue::from_str("temporary_channel_id must be a 32-byte hex channel id"))?;
        if temp_bytes.len() != 32 {
            return Err(JsValue::from_str(
                "temporary_channel_id must be a 32-byte hex channel id",
            ));
        }
        let mut raw = [0u8; 32];
        raw.copy_from_slice(&temp_bytes);
        let temporary_channel_id = lightning::ln::types::ChannelId::from_bytes(raw);

        let counterparty_node_id = SecpPublicKey::from_slice(
            &hex::decode(counterparty).map_err(|_| JsValue::from_str("invalid counterparty_node_id"))?,
        )
        .map_err(|_| JsValue::from_str("invalid counterparty_node_id"))?;

        let tx_bytes =
            hex::decode(tx_hex).map_err(|_| JsValue::from_str("invalid funding_tx_hex (hex decode failed)"))?;
        let funding_tx: bitcoin::Transaction = bitcoin::consensus::deserialize(&tx_bytes)
            .map_err(|e| JsValue::from_str(&format!("invalid funding_tx_hex (tx decode failed): {e}")))?;

        let graph = self.object_graph.borrow();
        let Some(g) = graph.as_ref() else {
            return Err(JsValue::from_str(
                sdk_contracts::ERR_LDK_OBJECT_GRAPH_NOT_INITIALIZED,
            ));
        };
        g.channel_manager
            .funding_transaction_generated(temporary_channel_id, counterparty_node_id, funding_tx)
            .map_err(|e| match e {
                APIError::APIMisuseError { err }
                | APIError::FeeRateTooHigh { err, feerate: _ }
                | APIError::ChannelUnavailable { err }
                | APIError::InvalidRoute { err } => JsValue::from_str(&err),
                APIError::MonitorUpdateInProgress => {
                    JsValue::from_str("channel monitor update in progress")
                }
                APIError::IncompatibleShutdownScript { script } => {
                    JsValue::from_str(&format!(
                        "incompatible shutdown script for peer negotiation: {script}"
                    ))
                }
            })?;
        self.pending_funding_requests.borrow_mut().remove(temp);
        g.peer_manager.borrow().process_events();
        let _ = self.derive_channel_status_from_live(g, temp);
        Ok(())
    }

    fn list_live_channels(&self) -> Result<Vec<LdkRuntimeOpenChannelResultData>, JsValue> {
        self.ensure_phase1_runtime_ready()?;
        let graph = self.object_graph.borrow();
        let Some(g) = graph.as_ref() else {
            return Err(JsValue::from_str(
                sdk_contracts::ERR_LDK_OBJECT_GRAPH_NOT_INITIALIZED,
            ));
        };
        let channels = g
            .channel_manager
            .list_channels()
            .into_iter()
            .map(|details| LdkRuntimeOpenChannelResultData {
                temporary_channel_id: format!("{}", details.channel_id),
                channel_id: format!("{}", details.channel_id),
                peer_pubkey: details.counterparty.node_id.to_string(),
                capacity_sat: details.channel_value_satoshis,
                status: if details.is_usable {
                    "ready".to_string()
                } else if details.is_channel_ready {
                    "pending".to_string()
                } else {
                    "opening".to_string()
                },
                ready: details.is_channel_ready,
                is_usable: details.is_usable,
            })
            .collect();
        Ok(channels)
    }
}

pub fn create_wasm_ldk_live_backend(
    runtime_key: String,
    node_seed32: Option<[u8; 32]>,
) -> Result<Rc<dyn LdkLiveBackend>, JsValue> {
    if runtime_key.trim().is_empty() {
        return Err(JsValue::from_str(sdk_contracts::ERR_RUNTIME_KEY_EMPTY));
    }
    Ok(Rc::new(WasmLdkLiveBackend::new(runtime_key, node_seed32)))
}

fn unix_now_secs() -> u64 {
    #[cfg(target_arch = "wasm32")]
    {
        (js_sys::Date::now() as u64) / 1000
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        use std::time::{SystemTime, UNIX_EPOCH};
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    }
}

fn unix_now_nanos() -> u32 {
    #[cfg(target_arch = "wasm32")]
    {
        ((js_sys::Date::now() as u64) % 1_000) as u32 * 1_000_000
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        use std::time::{SystemTime, UNIX_EPOCH};
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0)
    }
}
