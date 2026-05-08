#![allow(clippy::arc_with_non_send_sync)]
#![allow(clippy::borrow_deref_ref)]
#![allow(clippy::type_complexity)]

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet, VecDeque};
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use bitcoin_hashes::sha256::Hash as Sha256;
use bitcoin_hashes::Hash as _;
use lightning::bitcoin;
use lightning::bitcoin::block::Header;
use lightning::bitcoin::consensus::deserialize;
use lightning::bitcoin::hash_types::Txid;
use lightning::chain::chaininterface::{
    BroadcasterInterface, FeeEstimator, FEERATE_FLOOR_SATS_PER_KW,
};
use lightning::chain::chainmonitor;
use lightning::chain::Confirm;
use lightning::chain::Watch;
use lightning::chain::{BestBlock, ChannelMonitorUpdateStatus};
use lightning::events::Event;
use lightning::events::{EventsProvider, ReplayEvent};
use lightning::ln::channelmanager::{
    ChainParameters, ChannelManagerReadArgs, SimpleArcChannelManager,
};
use lightning::ln::peer_handler::{
    IgnoringMessageHandler, MessageHandler, PeerHandleError, PeerManager, SocketDescriptor,
};
use lightning::onion_message::messenger::DefaultMessageRouter;
use lightning::routing::gossip::NetworkGraph;
use lightning::routing::router::DefaultRouter;
use lightning::routing::scoring::{
    ProbabilisticScorer, ProbabilisticScoringDecayParameters, ProbabilisticScoringFeeParameters,
};
use lightning::sign::KeysManager;
use lightning::sign::{InMemorySigner, NodeSigner, Recipient};
use lightning::util::config::UserConfig;
use lightning::util::errors::APIError;
use lightning::util::logger::{Logger, Record};
use lightning::util::persist::KVStoreSync;
use lightning::util::persist::MonitorName;
use lightning::util::ser::{ReadableArgs, Writeable};
use secp256k1::PublicKey as SecpPublicKey;
use wasm_bindgen::prelude::JsValue;

use crate::ldk_runtime::{
    LdkRuntimeFundingRequestData, LdkRuntimeFundingTxSubmissionData,
    LdkRuntimeOpenChannelRequestData, LdkRuntimeOpenChannelResultData,
};
use crate::runtime_store::{browser_persistent_state_store, RuntimeStateStore};
use crate::wasm_node_persistence::{
    WASM_LDK_BROADCAST_QUEUE_STORAGE_PREFIX, WASM_LDK_MONITORS_STORAGE_PREFIX,
    WASM_LDK_RUNTIME_STORAGE_PREFIX,
};
use crate::wasm_runtime_paths::ldk_data_dir_for_runtime;

#[derive(Default)]
struct InMemoryKvStore {
    inner: Mutex<HashMap<(String, String, String), Vec<u8>>>,
}

impl KVStoreSync for InMemoryKvStore {
    fn read(
        &self,
        primary_namespace: &str,
        secondary_namespace: &str,
        key: &str,
    ) -> Result<Vec<u8>, lightning::io::Error> {
        let guard = self.inner.lock().map_err(|_| {
            lightning::io::Error::new(
                lightning::io::ErrorKind::Other,
                "InMemoryKvStore lock poisoned",
            )
        })?;
        guard
            .get(&(
                primary_namespace.to_string(),
                secondary_namespace.to_string(),
                key.to_string(),
            ))
            .cloned()
            .ok_or_else(|| {
                lightning::io::Error::new(lightning::io::ErrorKind::NotFound, "key not found")
            })
    }

    fn write(
        &self,
        primary_namespace: &str,
        secondary_namespace: &str,
        key: &str,
        buf: Vec<u8>,
    ) -> Result<(), lightning::io::Error> {
        let mut guard = self.inner.lock().map_err(|_| {
            lightning::io::Error::new(
                lightning::io::ErrorKind::Other,
                "InMemoryKvStore lock poisoned",
            )
        })?;
        guard.insert(
            (
                primary_namespace.to_string(),
                secondary_namespace.to_string(),
                key.to_string(),
            ),
            buf,
        );
        Ok(())
    }

    fn remove(
        &self,
        primary_namespace: &str,
        secondary_namespace: &str,
        key: &str,
        _lazy: bool,
    ) -> Result<(), lightning::io::Error> {
        let mut guard = self.inner.lock().map_err(|_| {
            lightning::io::Error::new(
                lightning::io::ErrorKind::Other,
                "InMemoryKvStore lock poisoned",
            )
        })?;
        guard.remove(&(
            primary_namespace.to_string(),
            secondary_namespace.to_string(),
            key.to_string(),
        ));
        Ok(())
    }

    fn list(
        &self,
        primary_namespace: &str,
        secondary_namespace: &str,
    ) -> Result<Vec<String>, lightning::io::Error> {
        let guard = self.inner.lock().map_err(|_| {
            lightning::io::Error::new(
                lightning::io::ErrorKind::Other,
                "InMemoryKvStore lock poisoned",
            )
        })?;
        Ok(guard
            .keys()
            .filter(|(p, s, _k)| p == primary_namespace && s == secondary_namespace)
            .map(|(_p, _s, k)| k.clone())
            .collect())
    }
}

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
    fn persist_state(&self) -> Result<(), JsValue> {
        Ok(())
    }
    fn restore_status(&self) -> LdkLiveRestoreStatus {
        LdkLiveRestoreStatus::default()
    }

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

#[derive(Clone, Copy, Debug, Default)]
pub struct LdkLiveRestoreStatus {
    pub channel_manager_restored: bool,
    pub monitors_restored: bool,
}

#[inline]
fn ldk_live_debug(msg: &str) {
    // Debug-only logging. This backend runs in hot paths (peer/frame
    // processing), so avoid unconditional console spam in release builds.
    #[cfg(all(target_arch = "wasm32", debug_assertions))]
    web_sys::console::log_1(&JsValue::from_str(msg));
    #[cfg(all(not(target_arch = "wasm32"), debug_assertions))]
    eprintln!("{msg}");
    #[cfg(not(debug_assertions))]
    let _ = msg;
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
    submitted_funding_txids: RefCell<HashSet<Txid>>,
    object_graph: RefCell<Option<LdkObjectGraph>>,
}

struct LdkObjectGraph {
    logger: Arc<WasmLdkLogger>,
    fee_estimator: Arc<FixedFeeEstimator>,
    broadcaster: Arc<WasmQueueingBroadcaster>,
    chain_source: Arc<WasmFilter>,
    keys_manager: Arc<KeysManager>,
    network_graph: Arc<WasmNetworkGraph>,
    scorer: Arc<std::sync::RwLock<WasmScorer>>,
    peer_manager: RefCell<WasmPeerManager>,
    chain_monitor: Arc<WasmChainMonitor>,
    channel_manager: Arc<WasmChannelManager>,
    active_descriptor: RefCell<Option<LiveSocketDescriptor>>,
    channel_manager_restored: bool,
    monitors_restored: bool,
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
    Arc<WasmFilter>,
    Arc<WasmQueueingBroadcaster>,
    Arc<FixedFeeEstimator>,
    Arc<WasmLdkLogger>,
    Arc<WasmPersister>,
    Arc<KeysManager>,
>;

type WasmChannelManager = SimpleArcChannelManager<
    WasmChainMonitor,
    WasmQueueingBroadcaster,
    FixedFeeEstimator,
    WasmLdkLogger,
>;

type WasmChannelMonitor = lightning::chain::channelmonitor::ChannelMonitor<InMemorySigner>;
type WasmNetworkGraph = NetworkGraph<Arc<WasmLdkLogger>>;
type WasmScorer = ProbabilisticScorer<Arc<WasmNetworkGraph>, Arc<WasmLdkLogger>>;

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
            record.module_path, record.line, record.args
        );
        #[cfg(target_arch = "wasm32")]
        web_sys::console::log_1(&JsValue::from_str(&msg));
        #[cfg(not(target_arch = "wasm32"))]
        let _ = msg;
    }
}

struct FixedFeeEstimator;

impl FeeEstimator for FixedFeeEstimator {
    fn get_est_sat_per_1000_weight(
        &self,
        _confirmation_target: lightning::chain::chaininterface::ConfirmationTarget,
    ) -> u32 {
        // Keep at least protocol floor for deterministic bootstrap behavior.
        FEERATE_FLOOR_SATS_PER_KW
    }
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct PendingBroadcastTx {
    txid: String,
    tx_hex: String,
}

struct WasmQueueingBroadcaster {
    runtime_key: String,
}

impl BroadcasterInterface for WasmQueueingBroadcaster {
    fn broadcast_transactions(&self, txs: &[&bitcoin::Transaction]) {
        let key = format!(
            "{WASM_LDK_BROADCAST_QUEUE_STORAGE_PREFIX}{}",
            self.runtime_key
        );
        let store = browser_persistent_state_store();
        let mut pending: Vec<PendingBroadcastTx> = store
            .get(&key)
            .ok()
            .flatten()
            .and_then(|raw: String| serde_json::from_str::<Vec<PendingBroadcastTx>>(&raw).ok())
            .unwrap_or_default();

        for tx in txs {
            let txid = tx.compute_txid().to_string();
            let tx_hex = bitcoin::consensus::encode::serialize_hex(tx);
            if let Some(existing) = pending.iter_mut().find(|entry| entry.txid == txid) {
                existing.tx_hex = tx_hex;
            } else {
                pending.push(PendingBroadcastTx { txid, tx_hex });
            }
        }

        if let Ok(raw) = serde_json::to_string(&pending) {
            let _ = store.set(&key, &raw);
        }
    }
}

struct WasmFilter {
    watched_txids: RefCell<HashSet<bitcoin::Txid>>,
    watched_outputs: RefCell<Vec<lightning::chain::WatchedOutput>>,
}

impl WasmFilter {
    fn new() -> Self {
        Self {
            watched_txids: RefCell::new(HashSet::new()),
            watched_outputs: RefCell::new(Vec::new()),
        }
    }
}

impl lightning::chain::Filter for WasmFilter {
    fn register_tx(&self, txid: &bitcoin::Txid, _script_pubkey: &bitcoin::Script) {
        self.watched_txids.borrow_mut().insert(*txid);
    }

    fn register_output(&self, output: lightning::chain::WatchedOutput) {
        self.watched_outputs.borrow_mut().push(output);
    }
}

struct WasmPersister {
    runtime_key: String,
}

impl chainmonitor::Persist<InMemorySigner> for WasmPersister {
    fn persist_new_channel(
        &self,
        monitor_name: MonitorName,
        monitor: &lightning::chain::channelmonitor::ChannelMonitor<InMemorySigner>,
    ) -> ChannelMonitorUpdateStatus {
        let _ = persist_monitor_snapshot(&self.runtime_key, monitor_name, monitor);
        ChannelMonitorUpdateStatus::Completed
    }

    fn update_persisted_channel(
        &self,
        monitor_name: MonitorName,
        _monitor_update: Option<&lightning::chain::channelmonitor::ChannelMonitorUpdate>,
        monitor: &lightning::chain::channelmonitor::ChannelMonitor<InMemorySigner>,
    ) -> ChannelMonitorUpdateStatus {
        let _ = persist_monitor_snapshot(&self.runtime_key, monitor_name, monitor);
        ChannelMonitorUpdateStatus::Completed
    }

    fn archive_persisted_channel(&self, monitor_name: MonitorName) {
        let _ = delete_monitor_snapshot(&self.runtime_key, monitor_name);
    }
}

fn channel_manager_snapshot_key(runtime_key: &str) -> String {
    format!("{WASM_LDK_RUNTIME_STORAGE_PREFIX}{runtime_key}:channel-manager")
}

fn channel_manager_snapshot_pending_key(runtime_key: &str) -> String {
    format!("{WASM_LDK_RUNTIME_STORAGE_PREFIX}{runtime_key}:channel-manager:pending")
}

fn network_graph_snapshot_key(runtime_key: &str) -> String {
    format!("{WASM_LDK_RUNTIME_STORAGE_PREFIX}{runtime_key}:network-graph")
}

fn network_graph_snapshot_pending_key(runtime_key: &str) -> String {
    format!("{WASM_LDK_RUNTIME_STORAGE_PREFIX}{runtime_key}:network-graph:pending")
}

fn scorer_snapshot_key(runtime_key: &str) -> String {
    format!("{WASM_LDK_RUNTIME_STORAGE_PREFIX}{runtime_key}:scorer")
}

fn scorer_snapshot_pending_key(runtime_key: &str) -> String {
    format!("{WASM_LDK_RUNTIME_STORAGE_PREFIX}{runtime_key}:scorer:pending")
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct ChannelManagerSnapshotEnvelope {
    schema_version: u32,
    bytes_hex: String,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct LdkBytesSnapshotEnvelope {
    schema_version: u32,
    bytes_hex: String,
}

const CHANNEL_MANAGER_SNAPSHOT_SCHEMA_VERSION: u32 = 1;
const NETWORK_GRAPH_SNAPSHOT_SCHEMA_VERSION: u32 = 1;
const SCORER_SNAPSHOT_SCHEMA_VERSION: u32 = 1;

fn persist_bytes_snapshot(
    pending_key: String,
    committed_key: String,
    schema_version: u32,
    bytes: Vec<u8>,
) -> Result<(), JsValue> {
    let envelope = LdkBytesSnapshotEnvelope {
        schema_version,
        bytes_hex: hex::encode(bytes),
    };
    let raw = serde_json::to_string(&envelope)
        .map_err(|e| JsValue::from_str(&format!("ldk snapshot encode: {e}")))?;
    let store = browser_persistent_state_store();
    store.set(&pending_key, &raw)?;
    store.set(&committed_key, &raw)?;
    store.delete(&pending_key)?;
    Ok(())
}

fn load_bytes_snapshot(
    committed_key: String,
    pending_key: String,
    max_schema_version: u32,
    label: &str,
) -> Result<Option<Vec<u8>>, JsValue> {
    let store = browser_persistent_state_store();
    let committed = store.get(&committed_key)?;
    let pending = store.get(&pending_key)?;
    let Some(raw) = committed.or(pending) else {
        return Ok(None);
    };
    let envelope: LdkBytesSnapshotEnvelope = serde_json::from_str(&raw)
        .map_err(|e| JsValue::from_str(&format!("{label} snapshot decode: {e}")))?;
    if envelope.schema_version > max_schema_version {
        return Err(JsValue::from_str(&format!(
            "{label} snapshot schema is newer than this SDK"
        )));
    }
    let bytes = hex::decode(envelope.bytes_hex)
        .map_err(|e| JsValue::from_str(&format!("{label} snapshot hex decode: {e}")))?;
    Ok(Some(bytes))
}

fn persist_channel_manager_snapshot(
    runtime_key: &str,
    channel_manager: &WasmChannelManager,
) -> Result<(), JsValue> {
    let mut bytes = Vec::new();
    channel_manager
        .write(&mut bytes)
        .map_err(|e| JsValue::from_str(&format!("channel manager encode: {e}")))?;
    let envelope = ChannelManagerSnapshotEnvelope {
        schema_version: CHANNEL_MANAGER_SNAPSHOT_SCHEMA_VERSION,
        bytes_hex: hex::encode(bytes),
    };
    let raw = serde_json::to_string(&envelope)
        .map_err(|e| JsValue::from_str(&format!("channel manager snapshot encode: {e}")))?;
    let store = browser_persistent_state_store();
    let pending_key = channel_manager_snapshot_pending_key(runtime_key);
    let committed_key = channel_manager_snapshot_key(runtime_key);
    store.set(&pending_key, &raw)?;
    store.set(&committed_key, &raw)?;
    store.delete(&pending_key)?;
    Ok(())
}

fn persist_network_graph_snapshot(
    runtime_key: &str,
    network_graph: &WasmNetworkGraph,
) -> Result<(), JsValue> {
    let mut bytes = Vec::new();
    network_graph
        .write(&mut bytes)
        .map_err(|e| JsValue::from_str(&format!("network graph encode: {e}")))?;
    persist_bytes_snapshot(
        network_graph_snapshot_pending_key(runtime_key),
        network_graph_snapshot_key(runtime_key),
        NETWORK_GRAPH_SNAPSHOT_SCHEMA_VERSION,
        bytes,
    )
}

fn persist_scorer_snapshot(runtime_key: &str, scorer: &WasmScorer) -> Result<(), JsValue> {
    let mut bytes = Vec::new();
    scorer
        .write(&mut bytes)
        .map_err(|e| JsValue::from_str(&format!("scorer encode: {e}")))?;
    persist_bytes_snapshot(
        scorer_snapshot_pending_key(runtime_key),
        scorer_snapshot_key(runtime_key),
        SCORER_SNAPSHOT_SCHEMA_VERSION,
        bytes,
    )
}

fn persist_ldk_runtime_snapshots(runtime_key: &str, g: &LdkObjectGraph) -> Result<(), JsValue> {
    persist_channel_manager_snapshot(runtime_key, &g.channel_manager)?;
    persist_network_graph_snapshot(runtime_key, &g.network_graph)?;
    let scorer = g
        .scorer
        .read()
        .map_err(|_| JsValue::from_str("scorer lock poisoned"))?;
    persist_scorer_snapshot(runtime_key, &scorer)?;
    Ok(())
}

fn monitor_index_key(runtime_key: &str) -> String {
    format!("{WASM_LDK_MONITORS_STORAGE_PREFIX}{runtime_key}:index")
}

fn monitor_snapshot_key(runtime_key: &str, monitor_name: MonitorName) -> String {
    format!("{WASM_LDK_MONITORS_STORAGE_PREFIX}{runtime_key}:monitor:{monitor_name}")
}

fn persist_monitor_snapshot(
    runtime_key: &str,
    monitor_name: MonitorName,
    monitor: &lightning::chain::channelmonitor::ChannelMonitor<InMemorySigner>,
) -> Result<(), JsValue> {
    let store = browser_persistent_state_store();

    let mut bytes: Vec<u8> = Vec::new();
    monitor
        .write(&mut bytes)
        .map_err(|e| JsValue::from_str(&format!("monitor encode: {e}")))?;
    let encoded = hex::encode(bytes);

    let item_key = monitor_snapshot_key(runtime_key, monitor_name);
    store.set(&item_key, &encoded)?;

    // Maintain an index because our store doesn't support listing keys.
    let idx_key = monitor_index_key(runtime_key);
    let mut index: Vec<String> = store
        .get(&idx_key)?
        .and_then(|raw| serde_json::from_str::<Vec<String>>(&raw).ok())
        .unwrap_or_default();
    let name = monitor_name.to_string();
    if !index.iter().any(|v| v == &name) {
        index.push(name);
        let raw = serde_json::to_string(&index)
            .map_err(|e| JsValue::from_str(&format!("monitor index encode: {e}")))?;
        store.set(&idx_key, &raw)?;
    }
    Ok(())
}

fn delete_monitor_snapshot(runtime_key: &str, monitor_name: MonitorName) -> Result<(), JsValue> {
    let store = browser_persistent_state_store();
    let item_key = monitor_snapshot_key(runtime_key, monitor_name);
    store.delete(&item_key)?;

    let idx_key = monitor_index_key(runtime_key);
    let mut index: Vec<String> = store
        .get(&idx_key)?
        .and_then(|raw| serde_json::from_str::<Vec<String>>(&raw).ok())
        .unwrap_or_default();
    let name = monitor_name.to_string();
    index.retain(|v| v != &name);
    if index.is_empty() {
        store.delete(&idx_key)?;
    } else {
        let raw = serde_json::to_string(&index)
            .map_err(|e| JsValue::from_str(&format!("monitor index encode: {e}")))?;
        store.set(&idx_key, &raw)?;
    }
    Ok(())
}

fn load_persisted_monitors(
    runtime_key: &str,
    keys_manager: &KeysManager,
) -> Result<Vec<(bitcoin::BlockHash, WasmChannelMonitor)>, JsValue> {
    let store = browser_persistent_state_store();
    let idx_key = monitor_index_key(runtime_key);
    let Some(raw_index) = store.get(&idx_key)? else {
        return Ok(Vec::new());
    };
    let index: Vec<String> = serde_json::from_str(&raw_index)
        .map_err(|e| JsValue::from_str(&format!("monitor index decode: {e}")))?;
    let mut monitors = Vec::new();
    for name in index {
        let monitor_key = format!("{WASM_LDK_MONITORS_STORAGE_PREFIX}{runtime_key}:monitor:{name}");
        let raw = store
            .get(&monitor_key)?
            .ok_or_else(|| JsValue::from_str(&format!("missing monitor snapshot: {name}")))?;
        let bytes = hex::decode(raw)
            .map_err(|e| JsValue::from_str(&format!("monitor snapshot hex decode: {e}")))?;
        let mut cursor = std::io::Cursor::new(bytes);
        let restored = <(bitcoin::BlockHash, WasmChannelMonitor)>::read(
            &mut cursor,
            (&*keys_manager, &*keys_manager),
        )
        .map_err(|e| JsValue::from_str(&format!("monitor snapshot decode: {e:?}")))?;
        monitors.push(restored);
    }
    Ok(monitors)
}

fn load_channel_manager_snapshot(runtime_key: &str) -> Result<Option<Vec<u8>>, JsValue> {
    let store = browser_persistent_state_store();
    let committed = store.get(&channel_manager_snapshot_key(runtime_key))?;
    let pending = store.get(&channel_manager_snapshot_pending_key(runtime_key))?;
    let raw = committed.or(pending);
    let Some(raw) = raw else {
        return Ok(None);
    };
    let envelope: ChannelManagerSnapshotEnvelope = serde_json::from_str(&raw)
        .map_err(|e| JsValue::from_str(&format!("channel manager snapshot decode: {e}")))?;
    if envelope.schema_version > CHANNEL_MANAGER_SNAPSHOT_SCHEMA_VERSION {
        return Err(JsValue::from_str(
            "channel manager snapshot schema is newer than this SDK",
        ));
    }
    let bytes = hex::decode(envelope.bytes_hex)
        .map_err(|e| JsValue::from_str(&format!("channel manager snapshot hex decode: {e}")))?;
    Ok(Some(bytes))
}

fn load_network_graph_snapshot(
    runtime_key: &str,
    logger: Arc<WasmLdkLogger>,
) -> Result<(Arc<WasmNetworkGraph>, bool), JsValue> {
    let Some(bytes) = load_bytes_snapshot(
        network_graph_snapshot_key(runtime_key),
        network_graph_snapshot_pending_key(runtime_key),
        NETWORK_GRAPH_SNAPSHOT_SCHEMA_VERSION,
        "network graph",
    )?
    else {
        return Ok((
            Arc::new(NetworkGraph::new(bitcoin::Network::Regtest, logger)),
            false,
        ));
    };
    let mut cursor = std::io::Cursor::new(bytes);
    let graph = <WasmNetworkGraph>::read(&mut cursor, logger)
        .map_err(|e| JsValue::from_str(&format!("network graph snapshot decode: {e:?}")))?;
    Ok((Arc::new(graph), true))
}

fn load_scorer_snapshot(
    runtime_key: &str,
    network_graph: Arc<WasmNetworkGraph>,
    logger: Arc<WasmLdkLogger>,
) -> Result<(Arc<std::sync::RwLock<WasmScorer>>, bool), JsValue> {
    let params = ProbabilisticScoringDecayParameters::default();
    let scorer = if let Some(bytes) = load_bytes_snapshot(
        scorer_snapshot_key(runtime_key),
        scorer_snapshot_pending_key(runtime_key),
        SCORER_SNAPSHOT_SCHEMA_VERSION,
        "scorer",
    )? {
        let args = (params, Arc::clone(&network_graph), Arc::clone(&logger));
        let mut cursor = std::io::Cursor::new(bytes);
        let scorer = <WasmScorer>::read(&mut cursor, args)
            .map_err(|e| JsValue::from_str(&format!("scorer snapshot decode: {e:?}")))?;
        return Ok((Arc::new(std::sync::RwLock::new(scorer)), true));
    } else {
        ProbabilisticScorer::new(params, network_graph, logger)
    };
    Ok((Arc::new(std::sync::RwLock::new(scorer)), false))
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
            submitted_funding_txids: RefCell::new(HashSet::new()),
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
        let broadcaster = Arc::new(WasmQueueingBroadcaster {
            runtime_key: self.runtime_key.clone(),
        });
        let persister = Arc::new(WasmPersister {
            runtime_key: self.runtime_key.clone(),
        });
        let chain_source = Arc::new(WasmFilter::new());
        let rgb_kv_store: Arc<dyn KVStoreSync + Send + Sync> = Arc::new(InMemoryKvStore::default());
        let ldk_data_dir: PathBuf = ldk_data_dir_for_runtime(&self.runtime_key);
        let keys_manager = Arc::new(KeysManager::new(
            &seed,
            unix_now_secs(),
            unix_now_nanos(),
            true,
            ldk_data_dir.clone(),
            Arc::clone(&rgb_kv_store),
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
        let restored_monitors = load_persisted_monitors(&self.runtime_key, &keys_manager)?;
        let has_persisted_monitors = !restored_monitors.is_empty();
        let (network_graph, _network_graph_restored) =
            load_network_graph_snapshot(&self.runtime_key, Arc::clone(&logger))?;
        let (scorer, _scorer_restored) = load_scorer_snapshot(
            &self.runtime_key,
            Arc::clone(&network_graph),
            Arc::clone(&logger),
        )?;
        let router = Arc::new(DefaultRouter::new(
            Arc::clone(&network_graph),
            Arc::clone(&logger),
            Arc::clone(&keys_manager),
            Arc::clone(&scorer),
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
        let mut channel_manager_restored = false;
        let mut monitors_restored = false;
        let channel_manager: Arc<WasmChannelManager> = if let Some(bytes) =
            load_channel_manager_snapshot(&self.runtime_key)?
        {
            let monitor_refs: Vec<&WasmChannelMonitor> = restored_monitors
                .iter()
                .map(|(_, monitor)| monitor)
                .collect();
            let read_args = ChannelManagerReadArgs::new(
                Arc::clone(&keys_manager),
                Arc::clone(&keys_manager),
                Arc::clone(&keys_manager),
                Arc::clone(&fee_estimator),
                Arc::clone(&chain_monitor),
                Arc::clone(&broadcaster),
                router,
                message_router,
                Arc::clone(&logger),
                user_config.clone(),
                monitor_refs,
                ldk_data_dir.clone(),
                Arc::clone(&rgb_kv_store),
            );
            let mut cursor = std::io::Cursor::new(bytes);
            let (_blockhash, channel_manager) =
                <(bitcoin::BlockHash, Arc<WasmChannelManager>)>::read(&mut cursor, read_args)
                    .map_err(|e| {
                        JsValue::from_str(&format!("channel manager snapshot decode: {e:?}"))
                    })?;
            channel_manager_restored = true;
            channel_manager
        } else {
            if has_persisted_monitors {
                return Err(JsValue::from_str(
                    "refusing to start LDK: ChannelMonitor snapshots exist but ChannelManager snapshot is missing",
                ));
            }
            Arc::new(lightning::ln::channelmanager::ChannelManager::new(
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
                ldk_data_dir,
                Arc::clone(&rgb_kv_store),
            ))
        };
        for (_blockhash, monitor) in restored_monitors {
            chain_monitor
                .watch_channel(monitor.channel_id(), monitor)
                .map_err(|_| JsValue::from_str("failed to register restored ChannelMonitor"))?;
            monitors_restored = true;
        }
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
            chain_source,
            keys_manager,
            network_graph,
            scorer,
            peer_manager: RefCell::new(peer_manager),
            chain_monitor,
            channel_manager,
            active_descriptor: RefCell::new(None),
            channel_manager_restored,
            monitors_restored,
        });
        self.persist_state()?;
        Ok(())
    }

    fn derive_seed32(&self) -> [u8; 32] {
        if let Some(seed) = self.node_seed32 {
            return seed;
        }
        let hash =
            <Sha256 as bitcoin_hashes::Hash>::hash(self.runtime_key.as_bytes()).to_byte_array();
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

    /// `FundingGenerationReady` is emitted after peer messages are processed; `open_channel` only
    /// drains `process_pending_events` once immediately after `create_channel`, so we also ingest
    /// here after every `peer_manager.process_events` (called from the peer session after each
    /// inbound frame).
    fn ingest_funding_generation_ready_events(&self, g: &LdkObjectGraph) {
        let collected_events: RefCell<Vec<Event>> = RefCell::new(Vec::new());
        g.channel_manager.process_pending_events(&|event: Event| {
            collected_events.borrow_mut().push(event);
            Ok::<(), ReplayEvent>(())
        });
        for event in collected_events.into_inner().into_iter() {
            if let Event::FundingGenerationReady {
                temporary_channel_id: ev_temp,
                counterparty_node_id,
                channel_value_satoshis,
                output_script,
                ..
            } = event
            {
                let temporary_channel_id_hex = format!("{ev_temp}");
                self.pending_funding_requests.borrow_mut().insert(
                    temporary_channel_id_hex.clone(),
                    LdkRuntimeFundingRequestData {
                        temporary_channel_id: temporary_channel_id_hex,
                        counterparty_node_id: hex::encode(counterparty_node_id.serialize()),
                        channel_value_satoshis,
                        output_script_hex: hex::encode(output_script.as_bytes()),
                    },
                );
            }
        }
    }

    fn derive_channel_status_from_live(
        &self,
        g: &LdkObjectGraph,
        temporary_channel_id_hex: &str,
    ) -> (String, String, bool, bool) {
        let mut is_ready = false;
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
                    }
                }
                Event::ChannelReady { channel_id, .. } => {
                    if format!("{channel_id}") == temporary_channel_id_hex {
                        is_ready = true;
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
            is_ready,
            false,
        )
    }

    fn with_graph<T>(
        &self,
        f: impl FnOnce(&LdkObjectGraph) -> Result<T, JsValue>,
    ) -> Result<T, JsValue> {
        self.ensure_object_graph()?;
        let graph = self.object_graph.borrow();
        let g = graph.as_ref().ok_or_else(|| {
            JsValue::from_str(sdk_contracts::ERR_LDK_OBJECT_GRAPH_NOT_INITIALIZED)
        })?;
        f(g)
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
        self.connected_peers
            .borrow_mut()
            .insert(peer_pubkey.to_string());

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
        ldk_live_debug(&format!(
            "[rln-wasm-sdk ldk-live] new_outbound_connection peer={} act1_bytes={}",
            peer_pubkey,
            act_one.len()
        ));
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
        let _ = hex::decode(payload_hex)
            .map_err(|e| JsValue::from_str(&format!("invalid payload_hex: {e}")))?;
        let bytes = hex::decode(payload_hex)
            .map_err(|e| JsValue::from_str(&format!("invalid payload_hex: {e}")))?;
        ldk_live_debug(&format!(
            "[rln-wasm-sdk ldk-live] read_event bytes={}",
            bytes.len()
        ));
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
            .ok_or_else(|| JsValue::from_str(sdk_contracts::ERR_ACTIVE_PEER_DESCRIPTOR_MISSING))?;
        g.peer_manager
            .borrow_mut()
            .read_event(&mut desc, &bytes)
            .map_err(|_e: PeerHandleError| {
                JsValue::from_str(sdk_contracts::ERR_PEER_MANAGER_READ_EVENT_FAILED)
            })?;
        persist_ldk_runtime_snapshots(&self.runtime_key, g)?;
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
        self.ingest_funding_generation_ready_events(g);
        persist_ldk_runtime_snapshots(&self.runtime_key, g)?;
        ldk_live_debug("[rln-wasm-sdk ldk-live] process_events");
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
            .ok_or_else(|| JsValue::from_str(sdk_contracts::ERR_ACTIVE_PEER_DESCRIPTOR_MISSING))?;
        let mut q = desc
            .outbound
            .lock()
            .map_err(|_| JsValue::from_str(sdk_contracts::ERR_OUTBOUND_QUEUE_LOCK_POISONED))?;
        let mut out = Vec::new();
        while let Some(frame) = q.pop_front() {
            out.push(hex::encode(frame));
        }
        ldk_live_debug(&format!(
            "[rln-wasm-sdk ldk-live] take_outbound_frames count={}",
            out.len()
        ));
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
            persist_ldk_runtime_snapshots(&self.runtime_key, g)?;
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
            return Err(JsValue::from_str("ldk object graph is not initialized"));
        };
        let is_connected = g
            .peer_manager
            .borrow()
            .list_peers()
            .into_iter()
            .any(|peer| peer.counterparty_node_id == target);
        ldk_live_debug(&format!(
            "[rln-wasm-sdk ldk-live] handshake_complete peer={} connected={}",
            peer_pubkey, is_connected
        ));
        Ok(is_connected)
    }

    fn local_node_pubkey(&self) -> Result<String, JsValue> {
        self.ensure_phase1_runtime_ready()?;
        let graph = self.object_graph.borrow();
        let Some(g) = graph.as_ref() else {
            return Err(JsValue::from_str("ldk object graph is not initialized"));
        };
        let node_id = g
            .keys_manager
            .get_node_id(Recipient::Node)
            .map_err(|_| JsValue::from_str("failed to derive live backend node id"))?;
        Ok(node_id.to_string())
    }

    fn persist_state(&self) -> Result<(), JsValue> {
        let graph = self.object_graph.borrow();
        let Some(g) = graph.as_ref() else {
            return Ok(());
        };
        persist_ldk_runtime_snapshots(&self.runtime_key, g)
    }

    fn restore_status(&self) -> LdkLiveRestoreStatus {
        let graph = self.object_graph.borrow();
        let Some(g) = graph.as_ref() else {
            return LdkLiveRestoreStatus::default();
        };
        LdkLiveRestoreStatus {
            channel_manager_restored: g.channel_manager_restored,
            monitors_restored: g.monitors_restored,
        }
    }

    fn chain_relevant_txids(&self) -> Result<Vec<String>, JsValue> {
        self.with_graph(|g| {
            let mut txids: HashSet<Txid> = g
                .chain_monitor
                .get_relevant_txids()
                .into_iter()
                .map(|(txid, _, _)| txid)
                .collect();
            txids.extend(g.chain_source.watched_txids.borrow().iter().copied());
            txids.extend(self.submitted_funding_txids.borrow().iter().copied());
            txids.extend(
                g.chain_source
                    .watched_outputs
                    .borrow()
                    .iter()
                    .map(|output| output.outpoint.txid),
            );
            Ok(txids.into_iter().map(|txid| txid.to_string()).collect())
        })
    }

    fn chain_apply_best_block(&self, height: u32, header_hex: &str) -> Result<(), JsValue> {
        let header_bytes =
            hex::decode(header_hex).map_err(|e| JsValue::from_str(&format!("header hex: {e}")))?;
        let header: Header = deserialize(&header_bytes)
            .map_err(|e| JsValue::from_str(&format!("header decode: {e}")))?;

        self.with_graph(|g| {
            g.chain_monitor.best_block_updated(&header, height);
            g.channel_manager.best_block_updated(&header, height);
            persist_ldk_runtime_snapshots(&self.runtime_key, g)?;
            Ok(())
        })
    }

    fn chain_apply_confirmed_tx(
        &self,
        height: u32,
        header_hex: &str,
        tx_index: usize,
        tx_hex: &str,
    ) -> Result<(), JsValue> {
        let header_bytes =
            hex::decode(header_hex).map_err(|e| JsValue::from_str(&format!("header hex: {e}")))?;
        let header: Header = deserialize(&header_bytes)
            .map_err(|e| JsValue::from_str(&format!("header decode: {e}")))?;

        let tx_bytes =
            hex::decode(tx_hex).map_err(|e| JsValue::from_str(&format!("tx hex: {e}")))?;
        let tx: bitcoin::Transaction =
            deserialize(&tx_bytes).map_err(|e| JsValue::from_str(&format!("tx decode: {e}")))?;

        self.with_graph(|g| {
            let txdata = [(tx_index, &tx)];
            g.chain_monitor
                .transactions_confirmed(&header, &txdata, height);
            g.channel_manager
                .transactions_confirmed(&header, &txdata, height);
            persist_ldk_runtime_snapshots(&self.runtime_key, g)?;
            Ok(())
        })
    }

    fn chain_apply_unconfirmed_tx(&self, txid: &str) -> Result<(), JsValue> {
        let txid: Txid = txid
            .parse()
            .map_err(|_| JsValue::from_str("invalid txid"))?;
        self.with_graph(|g| {
            g.chain_monitor.transaction_unconfirmed(&txid);
            g.channel_manager.transaction_unconfirmed(&txid);
            persist_ldk_runtime_snapshots(&self.runtime_key, g)?;
            Ok(())
        })
    }

    fn open_channel_non_virtual(
        &self,
        request: LdkRuntimeOpenChannelRequestData,
    ) -> Result<LdkRuntimeOpenChannelResultData, JsValue> {
        self.ensure_phase1_runtime_ready()?;
        if request.peer_pubkey.trim().is_empty() {
            return Err(JsValue::from_str("peer_pubkey cannot be empty"));
        }
        if request.capacity_sat == 0 {
            return Err(JsValue::from_str("capacity_sat must be > 0"));
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
            return Err(JsValue::from_str("ldk object graph is not initialized"));
        };
        let peers_before = g
            .peer_manager
            .borrow()
            .list_peers()
            .into_iter()
            .map(|p| p.counterparty_node_id.to_string())
            .collect::<Vec<_>>();
        ldk_live_debug(&format!(
            "[rln-wasm-sdk ldk-live] open_channel begin target_peer={} peers_before={:?}",
            request.peer_pubkey, peers_before
        ));
        let mut connected = g
            .peer_manager
            .borrow()
            .list_peers()
            .into_iter()
            .any(|peer| peer.counterparty_node_id == their_node_id);
        if !connected {
            for _ in 0..60 {
                g.peer_manager.borrow().process_events();
                connected = g
                    .peer_manager
                    .borrow()
                    .list_peers()
                    .into_iter()
                    .any(|peer| peer.counterparty_node_id == their_node_id);
                if connected {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            if !connected {
                ldk_live_debug(&format!(
                    "[rln-wasm-sdk ldk-live] open_channel still not connected after wait peer={}",
                    request.peer_pubkey
                ));
            }
        }

        let mut create_err: Option<JsValue> = None;
        let mut temporary_channel_id_opt = None;
        for attempt in 1..=20 {
            let peer_ids = g
                .peer_manager
                .borrow()
                .list_peers()
                .into_iter()
                .map(|p| p.counterparty_node_id.to_string())
                .collect::<Vec<_>>();
            ldk_live_debug(&format!(
                "[rln-wasm-sdk ldk-live] create_channel attempt={} target_peer={} peers={:?}",
                attempt, request.peer_pubkey, peer_ids
            ));
            match g.channel_manager.create_channel(
                their_node_id,
                request.capacity_sat,
                0,
                self.next_user_channel_id(),
                None,
                None,
                None,
                None,
            ) {
                Ok(id) => {
                    temporary_channel_id_opt = Some(id);
                    break;
                }
                Err(e) => {
                    let err_msg = match e {
                        APIError::APIMisuseError { err }
                        | APIError::FeeRateTooHigh { err, feerate: _ }
                        | APIError::ChannelUnavailable { err }
                        | APIError::InvalidRoute { err } => err,
                        APIError::MonitorUpdateInProgress => {
                            "channel monitor update in progress".to_string()
                        }
                        APIError::IncompatibleShutdownScript { script } => {
                            format!("incompatible shutdown script for peer negotiation: {script}")
                        }
                    };
                    create_err = Some(JsValue::from_str(&err_msg));
                    ldk_live_debug(&format!(
                        "[rln-wasm-sdk ldk-live] create_channel failed attempt={} target_peer={} err={}",
                        attempt, request.peer_pubkey, err_msg
                    ));
                    // Force one extra peer-manager cycle before retry.
                    g.peer_manager.borrow().process_events();
                    std::thread::sleep(std::time::Duration::from_millis(100));
                    if !err_msg.contains("Not connected to node") {
                        break;
                    }
                }
            }
        }
        let Some(temporary_channel_id) = temporary_channel_id_opt else {
            return Err(create_err
                .unwrap_or_else(|| JsValue::from_str("create_channel failed after retry")));
        };

        let temp_id = format!("{temporary_channel_id}");
        g.peer_manager.borrow().process_events();
        let (temporary_channel_id, channel_id, ready, is_usable) =
            self.derive_channel_status_from_live(g, &temp_id);
        persist_ldk_runtime_snapshots(&self.runtime_key, g)?;
        let status = if is_usable {
            "ready".to_string()
        } else if ready {
            "pending".to_string()
        } else if self
            .pending_funding_requests
            .borrow()
            .contains_key(&temporary_channel_id)
        {
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

        let temp_bytes = hex::decode(temp).map_err(|_| {
            JsValue::from_str("temporary_channel_id must be a 32-byte hex channel id")
        })?;
        if temp_bytes.len() != 32 {
            return Err(JsValue::from_str(
                "temporary_channel_id must be a 32-byte hex channel id",
            ));
        }
        let mut raw = [0u8; 32];
        raw.copy_from_slice(&temp_bytes);
        let temporary_channel_id = lightning::ln::types::ChannelId::from_bytes(raw);

        let counterparty_node_id = SecpPublicKey::from_slice(
            &hex::decode(counterparty)
                .map_err(|_| JsValue::from_str("invalid counterparty_node_id"))?,
        )
        .map_err(|_| JsValue::from_str("invalid counterparty_node_id"))?;

        let tx_bytes = hex::decode(tx_hex)
            .map_err(|_| JsValue::from_str("invalid funding_tx_hex (hex decode failed)"))?;
        let funding_tx: bitcoin::Transaction =
            bitcoin::consensus::deserialize(&tx_bytes).map_err(|e| {
                JsValue::from_str(&format!("invalid funding_tx_hex (tx decode failed): {e}"))
            })?;
        self.submitted_funding_txids
            .borrow_mut()
            .insert(funding_tx.compute_txid());

        let graph = self.object_graph.borrow();
        let Some(g) = graph.as_ref() else {
            return Err(JsValue::from_str("ldk object graph is not initialized"));
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
                APIError::IncompatibleShutdownScript { script } => JsValue::from_str(&format!(
                    "incompatible shutdown script for peer negotiation: {script}"
                )),
            })?;
        self.pending_funding_requests.borrow_mut().remove(temp);
        g.peer_manager.borrow().process_events();
        let _ = self.derive_channel_status_from_live(g, temp);
        persist_ldk_runtime_snapshots(&self.runtime_key, g)?;
        Ok(())
    }

    fn list_live_channels(&self) -> Result<Vec<LdkRuntimeOpenChannelResultData>, JsValue> {
        self.ensure_phase1_runtime_ready()?;
        let graph = self.object_graph.borrow();
        let Some(g) = graph.as_ref() else {
            return Err(JsValue::from_str("ldk object graph is not initialized"));
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
        return Err(JsValue::from_str("runtime_key cannot be empty"));
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
