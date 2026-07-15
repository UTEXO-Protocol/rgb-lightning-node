#![allow(clippy::arc_with_non_send_sync)]
#![allow(clippy::borrow_deref_ref)]
#![allow(clippy::type_complexity)]

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet, VecDeque};
use std::future::Future;
use std::hash::{Hash, Hasher};
use std::pin::Pin;
use std::rc::{Rc, Weak};
use std::sync::Arc;

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
    Bolt11InvoiceParameters, ChainParameters, ChannelFundingType, ChannelManagerReadArgs,
    PaymentId, RecipientOnionFields, Retry, SimpleArcChannelManager,
};
use lightning::ln::peer_handler::{
    IgnoringMessageHandler, MessageHandler, PeerHandleError, PeerManager, SocketDescriptor,
};
use lightning::onion_message::messenger::DefaultMessageRouter;
use lightning::rgb_utils::{
    update_rgb_channel_amount, write_rgb_payment_info_file, ContractId, RgbInfo, RgbKvStoreExt,
    RgbPaymentInfo, RGB_PAYMENT_INFO_INBOUND_NS, RGB_PAYMENT_INFO_OUTBOUND_NS, RGB_PRIMARY_NS,
};
use lightning::routing::gossip::NetworkGraph;
use lightning::routing::router::{DefaultRouter, PaymentParameters, RouteParameters};
use lightning::routing::scoring::{
    ProbabilisticScorer, ProbabilisticScoringDecayParameters, ProbabilisticScoringFeeParameters,
};
use lightning::sign::KeysManager;
use lightning::sign::{EntropySource, InMemorySigner, NodeSigner, Recipient};
use lightning::types::payment::{PaymentHash, PaymentPreimage};
use lightning::util::config::UserConfig;
use lightning::util::errors::APIError;
use lightning::util::logger::{Logger, Record};
use lightning::util::persist::KVStoreSync;
use lightning::util::persist::MonitorName;
use lightning::util::ser::{ReadableArgs, Writeable};
use lightning_invoice::{Bolt11Invoice, Bolt11InvoiceDescription, Description};
use secp256k1::PublicKey as SecpPublicKey;
use wasm_bindgen::prelude::JsValue;

use crate::apay;
use crate::ldk_runtime::{
    LdkRuntimeFundingRequestData, LdkRuntimeFundingTxSubmissionData, LdkRuntimeLivePaymentData,
    LdkRuntimeOpenChannelRequestData, LdkRuntimeOpenChannelResultData,
};
use crate::runtime_store::{browser_persistent_state_store, RuntimeStateStore};
use crate::wasm_node_persistence::{
    WASM_LDK_BROADCAST_QUEUE_STORAGE_PREFIX, WASM_LDK_MONITORS_STORAGE_PREFIX,
    WASM_LDK_RUNTIME_STORAGE_PREFIX,
};

thread_local! {
    /// Maps LDK runtime_key → shared RGB wallet handle.
    /// Populated by `register_rgb_wallet_for_runtime`; consumed by `ensure_object_graph`.
    static RGB_WALLET_REGISTRY: RefCell<HashMap<String, Rc<RefCell<rgb_lib_wasm::Wallet>>>> =
        RefCell::new(HashMap::new());
}

/// Register the RGB wallet for a given LDK runtime key.
///
/// Must be called (with a wallet that has already called `go_online`) before the LDK object
/// graph is first built for that runtime.  Called automatically from `attach_wallet_shared`.
pub fn register_rgb_wallet_for_runtime(
    runtime_key: &str,
    wallet: Rc<RefCell<rgb_lib_wasm::Wallet>>,
) {
    RGB_WALLET_REGISTRY.with(|reg| {
        reg.borrow_mut().insert(runtime_key.to_string(), wallet);
    });
}

thread_local! {
    /// Maps LDK runtime_key → whether trusted virtual channels v0 are enabled for that node.
    /// Set from `RlnWasmNode::set_enable_virtual_channels_v0` (and at node construction); read by the
    /// live backend's `Event::OpenChannelRequest` handler to decide whether to accept an inbound
    /// scid-privacy channel as a 0-conf virtual channel. Mirrors the native node's
    /// `static_state.enable_virtual_channels_v0` gate (`src/ldk.rs`).
    static VIRTUAL_CHANNELS_V0_REGISTRY: RefCell<HashMap<String, bool>> =
        RefCell::new(HashMap::new());
}

/// Record whether trusted virtual channels v0 are enabled for a given LDK runtime key.
pub fn set_virtual_channels_v0_for_runtime(runtime_key: &str, enabled: bool) {
    VIRTUAL_CHANNELS_V0_REGISTRY.with(|reg| {
        reg.borrow_mut().insert(runtime_key.to_string(), enabled);
    });
}

/// Whether trusted virtual channels v0 are enabled for a given LDK runtime key.
/// Defaults to `false` (opt-in, matching the native node) when no value has been registered.
fn virtual_channels_v0_enabled(runtime_key: &str) -> bool {
    VIRTUAL_CHANNELS_V0_REGISTRY
        .with(|reg| reg.borrow().get(runtime_key).copied())
        .unwrap_or(false)
}

// ---------------------------------------------------------------------------
// Selecting the node's Bitcoin network
// ---------------------------------------------------------------------------
//
// The node takes an explicit network, mirroring the native node's `--network`. That network is fed
// into this registry (keyed by runtime_key) and consumed when the backend lazily builds its
// `ChannelManager`/`NetworkGraph`, so the `Init` handshake advertises the matching genesis/chain hash
// in its `networks` field (Signet `f61eee3b…`, Regtest `06226e46…`, etc.). Two entry points populate
// it (both in `src/ln_node.rs`):
//
//   1. Explicit selection — `RlnWasmNode.newWithNodeRuntimeId(proxy, rid, network)` parses the
//      network (`"mainnet" | "testnet" | "testnet4" | "signet" | "regtest"`) and calls
//      `set_network_for_runtime` at construction, before the object graph exists. `attachWallet` then
//      validates the wallet's network against it and rejects a mismatch.
//   2. Adopt-from-wallet — the bare `RlnWasmNode::new` / SDK-facade path leaves the node unconfigured;
//      `attach_wallet_shared` reads the attached wallet's network
//      (`get_wallet_data().bitcoin_network`), maps it via `rgb_network_to_bitcoin_network`, and calls
//      `set_network_for_runtime` (still before the object graph is built).
//
// Consequences:
//   - The network is captured at object-graph build time and cannot change afterward (a
//     `ChannelManager`'s chain hash is fixed for its lifetime). In the adopt-from-wallet path, attach
//     the wallet before connecting to any peer.
//   - A mismatch between your wallet network and the peer's (e.g. wallet on Regtest, LSP on Signet)
//     surfaces as "Peer does not support any of our supported chains" + a handshake disconnect.
//   - `SignetCustom` (custom/mutinynet-style signets) maps to standard `Signet` for LDK purposes.
//
// The `Regtest` default below only applies before either path has run (e.g. early bring-up / tests),
// matching the historical hardcoded behaviour.
thread_local! {
    /// Maps LDK runtime_key → the Bitcoin network the node operates on.
    /// Set from `attach_wallet_shared` (derived from the attached RGB wallet); consumed when the live
    /// backend builds the `ChannelManager`/`NetworkGraph` so the LDK handshake advertises the correct
    /// chain (the `networks` field of the `Init` message) instead of the historical Regtest default.
    static NETWORK_REGISTRY: RefCell<HashMap<String, bitcoin::Network>> =
        RefCell::new(HashMap::new());
}

/// Record the Bitcoin network for a given LDK runtime key.
///
/// Must be called before the LDK object graph is first built for that runtime (done automatically
/// from `attach_wallet_shared`, whose wallet carries the configured network).
pub fn set_network_for_runtime(runtime_key: &str, network: bitcoin::Network) {
    NETWORK_REGISTRY.with(|reg| {
        reg.borrow_mut().insert(runtime_key.to_string(), network);
    });
}

/// The Bitcoin network registered for a given LDK runtime key.
/// Defaults to `Regtest` (the historical hardcoded value) when nothing has been registered yet.
fn network_for_runtime(runtime_key: &str) -> bitcoin::Network {
    NETWORK_REGISTRY
        .with(|reg| reg.borrow().get(runtime_key).copied())
        .unwrap_or(bitcoin::Network::Regtest)
}

/// Map an rgb-lib WASM network to the `lightning::bitcoin` network used by LDK.
pub fn rgb_network_to_bitcoin_network(network: rgb_lib_wasm::BitcoinNetwork) -> bitcoin::Network {
    match network {
        rgb_lib_wasm::BitcoinNetwork::Mainnet => bitcoin::Network::Bitcoin,
        rgb_lib_wasm::BitcoinNetwork::Testnet => bitcoin::Network::Testnet,
        rgb_lib_wasm::BitcoinNetwork::Testnet4 => bitcoin::Network::Testnet4,
        rgb_lib_wasm::BitcoinNetwork::Signet => bitcoin::Network::Signet,
        rgb_lib_wasm::BitcoinNetwork::Regtest => bitcoin::Network::Regtest,
        rgb_lib_wasm::BitcoinNetwork::SignetCustom => bitcoin::Network::Signet,
    }
}

/// Pending RGB open intent keyed by `user_channel_id`.
/// Stored when `open_channel_non_virtual` is called with an `asset_id`; consumed on
/// `FundingGenerationReady` in Phase E.
#[derive(Clone)]
struct PendingRgbOpenIntent {
    contract_id: lightning::rgb_utils::ContractId,
    schema: lightning::rgb_utils::AssetSchema,
    asset_amount: u64,
    consignment_endpoint: lightning::rgb_utils::RgbTransport,
    fee_rate: u64,
    min_confirmations: u8,
}

#[derive(Clone)]
enum PendingRgbFundingWork {
    Prepare {
        user_channel_id: u128,
        temporary_channel_id: lightning::ln::types::ChannelId,
        counterparty_node_id: SecpPublicKey,
        output_script_hex: String,
        channel_value_satoshis: u64,
    },
    Complete {
        temporary_channel_id_hex: String,
        signed_psbt: String,
    },
    ValidateFunding {
        temporary_channel_id: lightning::ln::types::ChannelId,
    },
    ProcessPendingTransactions,
}

pub trait LdkLiveBackend {
    fn new_outbound_connection(&self, peer_pubkey: &str) -> Result<String, JsValue>;
    fn read_event(&self, payload_hex: &str) -> Result<(), JsValue>;
    fn read_event_for_peer(&self, _peer_pubkey: &str, payload_hex: &str) -> Result<(), JsValue> {
        self.read_event(payload_hex)
    }
    fn process_events(&self) -> Result<(), JsValue>;
    fn take_outbound_frames(&self) -> Result<Vec<String>, JsValue>;
    fn take_outbound_frames_for_peer(&self, _peer_pubkey: &str) -> Result<Vec<String>, JsValue> {
        self.take_outbound_frames()
    }
    fn socket_disconnected(&self) -> Result<(), JsValue>;
    fn socket_disconnected_for_peer(&self, _peer_pubkey: &str) -> Result<(), JsValue> {
        self.socket_disconnected()
    }
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
    fn close_live_channel(
        &self,
        channel_id: &str,
        peer_pubkey: &str,
        force: bool,
    ) -> Result<(), JsValue> {
        let _ = (channel_id, peer_pubkey, force);
        Err(JsValue::from_str(
            "close_live_channel is not supported by this backend",
        ))
    }
    /// Abandon our own side of a never-broadcast virtual channel (client-side teardown). Used when
    /// the counterparty (LSP/host) has abandoned the channel without notifying us.
    fn virtual_channel_abandon_local(
        &self,
        channel_id: &str,
        peer_pubkey: &str,
    ) -> Result<(), JsValue> {
        let _ = (channel_id, peer_pubkey);
        Err(JsValue::from_str(
            "virtual_channel_abandon_local is not supported by this backend",
        ))
    }
    /// Send a real keysend (spontaneous) HTLC over a live channel, optionally carrying RGB.
    fn keysend_live(
        &self,
        _dest_pubkey: &str,
        _amt_msat: u64,
        _contract_id: Option<String>,
        _asset_amount: Option<u64>,
    ) -> Result<LdkRuntimeLivePaymentData, JsValue> {
        Err(JsValue::from_str(
            "keysend_live is not supported by this backend",
        ))
    }
    fn send_bolt11_live(
        &self,
        _invoice: &str,
        _amt_msat: Option<u64>,
        _contract_id: Option<String>,
        _asset_amount: Option<u64>,
    ) -> Result<LdkRuntimeLivePaymentData, JsValue> {
        Err(JsValue::from_str(
            "send_bolt11_live is not supported by this backend",
        ))
    }
    fn create_bolt11_invoice_live(
        &self,
        _amt_msat: Option<u64>,
        _expiry_sec: u32,
        _contract_id: Option<String>,
        _asset_amount: Option<u64>,
    ) -> Result<String, JsValue> {
        Err(JsValue::from_str(
            "create_bolt11_invoice_live is not supported by this backend",
        ))
    }
    fn create_hodl_bolt11_invoice_live(
        &self,
        _amt_msat: Option<u64>,
        _expiry_sec: u32,
        _contract_id: Option<String>,
        _asset_amount: Option<u64>,
        _payment_hash: &str,
    ) -> Result<String, JsValue> {
        Err(JsValue::from_str(
            "create_hodl_bolt11_invoice_live is not supported by this backend",
        ))
    }
    fn claim_hodl_invoice_live(
        &self,
        _payment_hash: &str,
        _payment_preimage: &str,
    ) -> Result<bool, JsValue> {
        Err(JsValue::from_str(
            "claim_hodl_invoice_live is not supported by this backend",
        ))
    }
    fn cancel_hodl_invoice_live(&self, _payment_hash: &str) -> Result<(), JsValue> {
        Err(JsValue::from_str(
            "cancel_hodl_invoice_live is not supported by this backend",
        ))
    }
    /// All real payments tracked from the live `ChannelManager` event stream.
    fn live_payments(&self) -> Vec<LdkRuntimeLivePaymentData> {
        Vec::new()
    }
    /// One real payment by hex payment hash, if known.
    fn live_payment(&self, _payment_hash: &str) -> Option<LdkRuntimeLivePaymentData> {
        None
    }
    /// Drain channel ids the live `ChannelManager` reported closed since the last call.
    fn take_closed_live_channels(&self) -> Vec<String> {
        Vec::new()
    }
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

    fn drive_rgb_funding_work_boxed(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<(), JsValue>> + 'static>> {
        Box::pin(async { Ok(()) })
    }

    fn process_pending_rgb_transactions_boxed(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<(), JsValue>> + 'static>> {
        Box::pin(async { Ok(()) })
    }

    /// Register a fresh batch of async-payment hashes with the invoice-host / LSP peer
    /// (`async_order.new`). When `username`/`domain` are supplied a Lightning-Address attestation
    /// is included so the LSP can serve `username@domain`. Resolves once the host replies.
    fn apay_new_boxed(
        &self,
        _host_node_id: String,
        _username: Option<String>,
        _domain: Option<String>,
    ) -> Pin<Box<dyn Future<Output = Result<crate::apay::AsyncOrderNewResponse, JsValue>> + 'static>>
    {
        Box::pin(async {
            Err(JsValue::from_str(
                "apay_new is not supported by this backend",
            ))
        })
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

/// As the final hop, project the authoritative per-HTLC RGB inbound record (written by
/// `color_commitment` under `chan_id || payment_hash`) onto the bare `<payment_hash>` key that
/// `finalize_rgb_channel_payment` and `get_payment` consume. Mirrors the native node's
/// `PaymentClaimable` handling. A plain byte copy — no decode needed.
fn project_inbound_rgb_payment_info(
    kv_store: &Arc<dyn KVStoreSync + Send + Sync>,
    receiving_channel_ids: &[(lightning::ln::types::ChannelId, Option<u128>)],
    payment_hash_hex: &str,
) {
    for (chan_id, _) in receiving_channel_ids {
        let chan_id_hex = hex::encode(chan_id.0);
        let scoped = format!("{chan_id_hex}{payment_hash_hex}");
        if let Ok(data) = kv_store.read(RGB_PRIMARY_NS, RGB_PAYMENT_INFO_INBOUND_NS, &scoped) {
            if kv_store
                .write(
                    RGB_PRIMARY_NS,
                    RGB_PAYMENT_INFO_INBOUND_NS,
                    payment_hash_hex,
                    data,
                )
                .is_ok()
            {
                break;
            }
        }
    }
}

/// Settle the RGB amount of a now-confirmed HTLC into the channel's RGB split.
///
/// Ported from rgb-lightning-node's `_finalize_rgb_channel_payment`: it walks the per-channel
/// scoped `<channel_id><payment_hash>_pending` payment-info records and moves the RGB amount as
/// offered (sender) or received (receiver). No-op for plain-BTC payments (no RGB records exist).
fn finalize_rgb_channel_payment(
    payment_hash: &PaymentHash,
    receiver: bool,
    kv_store: &Arc<dyn KVStoreSync + Send + Sync>,
) -> lightning::io::Result<()> {
    let payment_hash_str = hex::encode(payment_hash.0);
    let pending_suffix = format!("{payment_hash_str}_pending");
    let mut applied_any = false;

    for inbound in [true, false] {
        let namespace = if inbound {
            RGB_PAYMENT_INFO_INBOUND_NS
        } else {
            RGB_PAYMENT_INFO_OUTBOUND_NS
        };
        let keys = kv_store.list(RGB_PRIMARY_NS, namespace)?;
        let mut applied_keys = Vec::new();
        for key in &keys {
            if !key.ends_with(&pending_suffix) || key.len() <= pending_suffix.len() {
                continue;
            }
            let channel_id_str = &key[..key.len() - pending_suffix.len()];
            if channel_id_str.len() != 64 {
                continue;
            }
            let data = match kv_store.read(RGB_PRIMARY_NS, namespace, key) {
                Ok(data) => data,
                Err(e) if e.kind() == lightning::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(e),
            };
            let rgb_payment_info: RgbPaymentInfo = match bincode::deserialize(&data) {
                Ok(info) => info,
                Err(_) => continue,
            };
            if rgb_payment_info.swap_payment && receiver != rgb_payment_info.inbound {
                continue;
            }
            let (offered, received) = if receiver {
                (0, rgb_payment_info.amount)
            } else {
                (rgb_payment_info.amount, 0)
            };
            // Only update channels with existing RGB info; skip otherwise.
            if kv_store
                .read_rgb_channel_info(channel_id_str, false)
                .is_ok()
            {
                let _ = update_rgb_channel_amount(
                    channel_id_str,
                    offered,
                    received,
                    false,
                    kv_store.as_ref(),
                );
                applied_keys.push(key.clone());
                applied_any = true;
            }
        }
        for key in &applied_keys {
            let _ = kv_store.remove(RGB_PRIMARY_NS, namespace, key, false);
        }
    }

    if applied_any {
        let raw_pending_key = format!("{payment_hash_str}_pending");
        for namespace in [RGB_PAYMENT_INFO_INBOUND_NS, RGB_PAYMENT_INFO_OUTBOUND_NS] {
            let keys = kv_store.list(RGB_PRIMARY_NS, namespace)?;
            let remaining = keys
                .iter()
                .any(|k| k.ends_with(&pending_suffix) && k.len() > pending_suffix.len());
            if !remaining {
                let _ = kv_store.remove(RGB_PRIMARY_NS, namespace, &raw_pending_key, false);
            }
        }
    }
    Ok(())
}

#[allow(dead_code)]
pub struct WasmLdkLiveBackend {
    runtime_key: String,
    node_seed32: Option<[u8; 32]>,
    self_weak: RefCell<Weak<WasmLdkLiveBackend>>,
    connected_peers: RefCell<HashSet<String>>,
    inbound_frames: RefCell<VecDeque<(String, String)>>,
    outbound_frames: RefCell<VecDeque<String>>,
    disconnected: Cell<bool>,
    descriptor_nonce: Cell<u64>,
    pending_funding_requests: RefCell<HashMap<String, LdkRuntimeFundingRequestData>>,
    pending_rgb_open_intents: RefCell<HashMap<u128, PendingRgbOpenIntent>>,
    pending_rgb_funding_work: RefCell<VecDeque<PendingRgbFundingWork>>,
    pending_rgb_prepare_results: RefCell<HashMap<String, String>>,
    submitted_funding_txids: RefCell<HashSet<Txid>>,
    /// Real payments tracked from the live `ChannelManager` event stream, keyed by hex payment hash.
    live_payments: RefCell<HashMap<String, LdkRuntimeLivePaymentData>>,
    /// Invoice hashes whose claimable HTLCs must be held until explicitly claimed or cancelled.
    hodl_payment_hashes: RefCell<HashSet<String>>,
    /// CLTV `claim_deadline` (block height) for each held hodl HTLC, so we can proactively
    /// fail it back before the deadline forces a channel close. Repopulated from replayed
    /// `PaymentClaimable` events on restart (intentionally not persisted).
    hodl_claim_deadlines: RefCell<HashMap<String, u32>>,
    /// Preimages for async-payment **recipient** invoices minted in response to a host
    /// `request_invoice`, keyed by hex payment hash. When the inbound HTLC for one of these
    /// arrives we auto-claim with the stashed preimage (we derived it deterministically, so unlike
    /// a normal external-hash/hodl invoice there is nothing to wait for).
    async_recipient_preimages: RefCell<HashMap<String, PaymentPreimage>>,
    /// Channel ids the live `ChannelManager` reported closed via `Event::ChannelClosed`, pending
    /// propagation to the runtime channel view (drained by `take_closed_live_channels`).
    closed_channels: RefCell<Vec<String>>,
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
    rgb_backend: Arc<lightning::rgb_utils::RgbBackend>,
    rgb_kv_store: Arc<dyn KVStoreSync + Send + Sync>,
    /// Custom-message handler shared with the `PeerManager`; also drives the `async_order.*`
    /// (async-payments-with-LSP) request/response wire.
    fork_custom_wire: Arc<crate::rgb_ln_wire::RgbLnForkCustomMessageHandler>,
    peer_descriptors: RefCell<HashMap<String, LiveSocketDescriptor>>,
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

// Regtest channel/on-chain feerate. The indexer derives its feerate from bitcoind's
// `-fallbackfee=0.0002` (≈ 20 sat/vB ≈ 5000 sat/kw) because regtest has no fee market, and a
// counterparty reading that estimate rejects a channel opened at the protocol floor (253 sat/kw)
// with "Peer's feerate much too low". So we must open at a realistic feerate, not the floor.
const REGTEST_CHANNEL_FEERATE_SATS_PER_KW: u32 = 5000;

// Per-channel handshake parameters for non-virtual opens, matching native `open_channel`.
const OPEN_CHANNEL_HTLC_MIN_MSAT: u64 = 3_000_000;
const MIN_CHANNEL_CONFIRMATIONS: u32 = 6;
// LND's max `to_self_delay` is 2016 blocks; stay compatible.
const CHANNEL_TO_SELF_DELAY: u16 = 2016;

impl FeeEstimator for FixedFeeEstimator {
    fn get_est_sat_per_1000_weight(
        &self,
        confirmation_target: lightning::chain::chaininterface::ConfirmationTarget,
    ) -> u32 {
        use lightning::chain::chaininterface::ConfirmationTarget::*;
        match confirmation_target {
            // Lower bounds we enforce on a peer's feerate, and the minimum we'll use for our own
            // cooperative-close / mempool txs. Keep these at the protocol floor so we still accept
            // whatever commitment feerate the counterparty chose.
            MinAllowedAnchorChannelRemoteFee
            | MinAllowedNonAnchorChannelRemoteFee
            | ChannelCloseMinimum => FEERATE_FLOOR_SATS_PER_KW,
            // Commitment and on-chain feerates: use the regtest network rate so opens are accepted.
            _ => REGTEST_CHANNEL_FEERATE_SATS_PER_KW,
        }
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
    // Back-handle to the owning ChainMonitor, populated after construction. Used to
    // signal async completion (`channel_monitor_updated`) when a monitor could not be
    // persisted synchronously to localStorage and had to be deferred to IndexedDB.
    chain_monitor: std::cell::RefCell<Option<std::sync::Weak<WasmChainMonitor>>>,
}

impl WasmPersister {
    /// Persist a monitor, never acking durability we don't have.
    ///
    /// The synchronous localStorage write is the durability gate: if it succeeds the
    /// monitor survives reload, so we return `Completed`. If it fails (quota /
    /// private-mode / serialization) we MUST NOT return `Completed` — instead we
    /// serialize once and complete the persist asynchronously through the IndexedDB
    /// tier (which survives reload and is rehydrated into localStorage on next load),
    /// reporting `channel_monitor_updated` only once that durable write succeeds.
    fn persist_or_defer(
        &self,
        monitor_name: MonitorName,
        monitor: &lightning::chain::channelmonitor::ChannelMonitor<InMemorySigner>,
        completed_update_id: u64,
    ) -> ChannelMonitorUpdateStatus {
        match persist_monitor_snapshot(&self.runtime_key, monitor_name, monitor) {
            Ok(()) => ChannelMonitorUpdateStatus::Completed,
            Err(err) => {
                web_sys::console::warn_1(&JsValue::from_str(&format!(
                    "monitor localStorage persist failed ({err:?}); deferring to IndexedDB"
                )));
                let mut bytes: Vec<u8> = Vec::new();
                if monitor.write(&mut bytes).is_err() {
                    // We cannot even serialize the monitor; halting is the safe response.
                    return ChannelMonitorUpdateStatus::UnrecoverableError;
                }
                let weak = self.chain_monitor.borrow().clone();
                spawn_local_persist_completion(
                    self.runtime_key.clone(),
                    monitor_name.to_string(),
                    bytes,
                    monitor.channel_id(),
                    completed_update_id,
                    weak,
                );
                ChannelMonitorUpdateStatus::InProgress
            }
        }
    }
}

impl chainmonitor::Persist<InMemorySigner> for WasmPersister {
    fn persist_new_channel(
        &self,
        monitor_name: MonitorName,
        monitor: &lightning::chain::channelmonitor::ChannelMonitor<InMemorySigner>,
    ) -> ChannelMonitorUpdateStatus {
        let update_id = monitor.get_latest_update_id();
        self.persist_or_defer(monitor_name, monitor, update_id)
    }

    fn update_persisted_channel(
        &self,
        monitor_name: MonitorName,
        monitor_update: Option<&lightning::chain::channelmonitor::ChannelMonitorUpdate>,
        monitor: &lightning::chain::channelmonitor::ChannelMonitor<InMemorySigner>,
    ) -> ChannelMonitorUpdateStatus {
        let update_id = monitor_update
            .map(|u| u.update_id)
            .unwrap_or_else(|| monitor.get_latest_update_id());
        self.persist_or_defer(monitor_name, monitor, update_id)
    }

    fn archive_persisted_channel(&self, monitor_name: MonitorName) {
        let _ = delete_monitor_snapshot(&self.runtime_key, monitor_name);
    }
}

/// Map a wallet-derived schema string to the fork's `AssetSchema`. The wasm RGB backend
/// only supports the fungible Nia/Ifa schemas; anything else (including `None`) falls back
/// to `Nia`, which yields the same fungible funding assignment.
fn parse_rgb_asset_schema(schema: Option<&str>) -> lightning::rgb_utils::AssetSchema {
    match schema.map(|s| s.trim().to_ascii_lowercase()).as_deref() {
        Some("ifa") => lightning::rgb_utils::AssetSchema::Ifa,
        Some("cfa") => lightning::rgb_utils::AssetSchema::Cfa,
        Some("uda") => lightning::rgb_utils::AssetSchema::Uda,
        _ => lightning::rgb_utils::AssetSchema::Nia,
    }
}

/// Storage key for a monitor snapshot identified by its stringified `MonitorName`.
fn monitor_snapshot_key_str(runtime_key: &str, name: &str) -> String {
    format!("{WASM_LDK_MONITORS_STORAGE_PREFIX}{runtime_key}:monitor:{name}")
}

/// Complete a deferred monitor persist via the durable IndexedDB tier, then signal
/// `channel_monitor_updated` so LDK can release the paused channel. If the durable
/// write also fails the channel simply stays paused (safe) until a later persist.
#[cfg(target_arch = "wasm32")]
fn spawn_local_persist_completion(
    runtime_key: String,
    name: String,
    bytes: Vec<u8>,
    channel_id: lightning::ln::types::ChannelId,
    completed_update_id: u64,
    chain_monitor: Option<std::sync::Weak<WasmChainMonitor>>,
) {
    wasm_bindgen_futures::spawn_local(async move {
        let snap_key = monitor_snapshot_key_str(&runtime_key, &name);
        let encoded = hex::encode(&bytes);
        if let Err(err) = crate::runtime_store::indexed_db_set_durable(snap_key, encoded).await {
            web_sys::console::warn_1(&JsValue::from_str(&format!(
                "deferred monitor IndexedDB persist failed ({err:?}); channel stays paused"
            )));
            return;
        }
        // Maintain the monitor index in the durable tier too, so restart can find it.
        let idx_key = monitor_index_key(&runtime_key);
        let store = browser_persistent_state_store();
        let mut index: Vec<String> = store
            .get(&idx_key)
            .ok()
            .flatten()
            .and_then(|raw| serde_json::from_str::<Vec<String>>(&raw).ok())
            .unwrap_or_default();
        if !index.iter().any(|v| v == &name) {
            index.push(name.clone());
        }
        if let Ok(raw) = serde_json::to_string(&index) {
            let _ = crate::runtime_store::indexed_db_set_durable(idx_key, raw).await;
        }
        if let Some(cm) = chain_monitor.and_then(|w| w.upgrade()) {
            if let Err(err) = cm.channel_monitor_updated(channel_id, completed_update_id) {
                web_sys::console::warn_1(&JsValue::from_str(&format!(
                    "channel_monitor_updated after deferred persist failed: {err:?}"
                )));
            }
        }
    });
}

#[cfg(not(target_arch = "wasm32"))]
fn spawn_local_persist_completion(
    _runtime_key: String,
    _name: String,
    _bytes: Vec<u8>,
    _channel_id: lightning::ln::types::ChannelId,
    _completed_update_id: u64,
    _chain_monitor: Option<std::sync::Weak<WasmChainMonitor>>,
) {
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
    network: bitcoin::Network,
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
            Arc::new(NetworkGraph::new(network, logger)),
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
            self_weak: RefCell::new(Weak::new()),
            connected_peers: RefCell::new(HashSet::new()),
            inbound_frames: RefCell::new(VecDeque::new()),
            outbound_frames: RefCell::new(VecDeque::new()),
            disconnected: Cell::new(false),
            descriptor_nonce: Cell::new(1),
            pending_funding_requests: RefCell::new(HashMap::new()),
            pending_rgb_open_intents: RefCell::new(HashMap::new()),
            pending_rgb_funding_work: RefCell::new(VecDeque::new()),
            pending_rgb_prepare_results: RefCell::new(HashMap::new()),
            submitted_funding_txids: RefCell::new(HashSet::new()),
            live_payments: RefCell::new(HashMap::new()),
            hodl_payment_hashes: RefCell::new(HashSet::new()),
            hodl_claim_deadlines: RefCell::new(HashMap::new()),
            async_recipient_preimages: RefCell::new(HashMap::new()),
            closed_channels: RefCell::new(Vec::new()),
            object_graph: RefCell::new(None),
        }
    }

    fn ensure_object_graph(&self) -> Result<(), JsValue> {
        if self.object_graph.borrow().is_some() {
            return Ok(());
        }
        let network = network_for_runtime(&self.runtime_key);
        let seed = self.derive_seed32();
        let logger = Arc::new(WasmLdkLogger);
        let fee_estimator = Arc::new(FixedFeeEstimator);
        let broadcaster = Arc::new(WasmQueueingBroadcaster {
            runtime_key: self.runtime_key.clone(),
        });
        let persister = Arc::new(WasmPersister {
            runtime_key: self.runtime_key.clone(),
            chain_monitor: std::cell::RefCell::new(None),
        });
        let chain_source = Arc::new(WasmFilter::new());
        let rgb_kv_store: Arc<dyn KVStoreSync + Send + Sync> =
            crate::browser_kv_store::ldk_browser_kv_store(&self.runtime_key);
        let rgb_backend: Arc<lightning::rgb_utils::RgbBackend> = {
            let wallet_rc = RGB_WALLET_REGISTRY
                .with(|reg| reg.borrow().get(&self.runtime_key).cloned())
                .ok_or_else(|| {
                    JsValue::from_str(
                        "no RGB wallet attached; call node.attachWallet() before starting LDK",
                    )
                })?;
            let online = wallet_rc.borrow().get_online().ok_or_else(|| {
                JsValue::from_str(
                    "RGB wallet not online; call wallet.goOnline() before starting LDK",
                )
            })?;
            Arc::new(lightning::rgb_utils::WasmRgbBackend::new(
                Rc::clone(&wallet_rc),
                online,
            ))
        };
        let keys_manager = Arc::new(KeysManager::new(
            &seed,
            unix_now_secs(),
            unix_now_nanos(),
            true,
            Arc::clone(&rgb_backend),
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
        // Wire the persister's back-handle so deferred (IndexedDB) monitor persists can
        // signal completion via `channel_monitor_updated`.
        *persister.chain_monitor.borrow_mut() = Some(Arc::downgrade(&chain_monitor));
        let restored_monitors = load_persisted_monitors(&self.runtime_key, &keys_manager)?;
        let has_persisted_monitors = !restored_monitors.is_empty();
        let (network_graph, _network_graph_restored) =
            load_network_graph_snapshot(&self.runtime_key, network, Arc::clone(&logger))?;
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
        // `accept_forwards_to_priv_channels = true`: act as a routing intermediary for multi-hop
        // payments — forward HTLCs whose outgoing hop is one of our *private* (unannounced) channels.
        // With the default `false`, LDK returns `PrivateChannelForward` and refuses to forward over
        // private channels (`can_forward_htlc_to_outgoing_channel`), which would break
        // native-A -> WASM -> native-B where the WASM->payee channel is private and reached via an
        // invoice route hint.
        // `manually_accept_inbound_channels = true`: accept inbound channels the LSP opens to us (see
        // the `Event::OpenChannelRequest` handler). Without manual acceptance LDK auto-accepts with a
        // hardcoded 354-sat dust floor, which rejects virtual RGB channels (dust=1:
        // "dust_limit_satoshis (1) is less than the implementation limit (354)").
        let mut user_config = UserConfig {
            accept_forwards_to_priv_channels: true,
            manually_accept_inbound_channels: true,
            ..Default::default()
        };
        // Drop the announcement-preference constraint (the default `true` rejects public channels
        // whose announcement preference differs from ours: "their announcement preference is
        // different from ours") and negotiate anchors so the anchor channel types the LSP proposes
        // (mandatory for colored RGB channels) are accepted — mirroring the native node (`src/ldk.rs`).
        user_config
            .channel_handshake_limits
            .force_announced_channel_preference = false;
        user_config
            .channel_handshake_config
            .negotiate_anchors_zero_fee_htlc_tx = true;
        let chain_params = ChainParameters {
            network,
            best_block: BestBlock::from_network(network),
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
                Arc::clone(&rgb_backend),
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
                Arc::clone(&rgb_backend),
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
            rgb_backend,
            rgb_kv_store,
            fork_custom_wire,
            peer_descriptors: RefCell::new(HashMap::new()),
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
            self.handle_ldk_event_sync(g, event);
        }
    }

    fn handle_ldk_event_sync(&self, g: &LdkObjectGraph, event: Event) {
        match event {
            Event::FundingGenerationReady {
                temporary_channel_id: ev_temp,
                counterparty_node_id,
                channel_value_satoshis,
                output_script,
                user_channel_id,
            } => {
                let temporary_channel_id_hex = format!("{ev_temp}");
                let is_rgb = self
                    .pending_rgb_open_intents
                    .borrow()
                    .contains_key(&user_channel_id);
                if is_rgb {
                    self.pending_rgb_funding_work.borrow_mut().push_back(
                        PendingRgbFundingWork::Prepare {
                            user_channel_id,
                            temporary_channel_id: ev_temp,
                            counterparty_node_id,
                            output_script_hex: hex::encode(output_script.as_bytes()),
                            channel_value_satoshis,
                        },
                    );
                } else {
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
            Event::ChannelPending {
                former_temporary_channel_id: Some(former_temp),
                ..
            } => {
                let former_temp_hex = format!("{former_temp}");
                let signed_psbt = self
                    .pending_rgb_prepare_results
                    .borrow()
                    .get(&former_temp_hex)
                    .cloned();
                if let Some(signed_psbt) = signed_psbt {
                    self.pending_rgb_funding_work.borrow_mut().push_back(
                        PendingRgbFundingWork::Complete {
                            temporary_channel_id_hex: former_temp_hex,
                            signed_psbt,
                        },
                    );
                }
            }
            Event::RgbFundingValidationRequired {
                temporary_channel_id,
                ..
            } => {
                self.pending_rgb_funding_work.borrow_mut().push_back(
                    PendingRgbFundingWork::ValidateFunding {
                        temporary_channel_id,
                    },
                );
            }
            Event::RgbTransactionPersistenceRequired => {
                self.pending_rgb_funding_work
                    .borrow_mut()
                    .push_back(PendingRgbFundingWork::ProcessPendingTransactions);
            }
            // ---- Inbound channel open request (LSP opens a channel to this node) ----
            // With `manually_accept_inbound_channels = true` LDK surfaces every inbound open here
            // instead of auto-accepting under a hardcoded 354-sat dust floor. Mirror the native
            // node (`src/ldk.rs` `Event::OpenChannelRequest`): a channel advertising SCID privacy
            // is a virtual channel (dust=1, never broadcast) and must be accepted 0-conf as
            // `ChannelFundingType::Virtual` so the monitor is marked manual-broadcast and the dust
            // floor is bypassed; any other channel is a regular (public/private) channel accepted
            // normally.
            Event::OpenChannelRequest {
                temporary_channel_id,
                counterparty_node_id,
                channel_type,
                ..
            } => {
                let user_channel_id = self.next_user_channel_id();
                // Gate the 0-conf virtual accept on this node's `enable_virtual_channels_v0` flag
                // (mirrors the native node's `static_state.enable_virtual_channels_v0`). When the
                // flag is disabled, a scid-privacy channel falls through to the regular accept path,
                // which rejects it at the 354-sat dust floor — the intended opt-in behavior.
                let is_virtual = virtual_channels_v0_enabled(&self.runtime_key)
                    && channel_type.supports_scid_privacy();
                let counterparty_hex = hex::encode(counterparty_node_id.serialize());
                let res = if is_virtual {
                    g.channel_manager
                        .accept_inbound_channel_from_trusted_peer_0conf(
                            &temporary_channel_id,
                            &counterparty_node_id,
                            user_channel_id,
                            None,
                            ChannelFundingType::Virtual,
                        )
                } else {
                    g.channel_manager.accept_inbound_channel(
                        &temporary_channel_id,
                        &counterparty_node_id,
                        user_channel_id,
                        None,
                    )
                };
                match res {
                    Ok(()) => ldk_live_debug(&format!(
                        "[rln-wasm-sdk ldk-live] accepted inbound {} channel {temporary_channel_id} from {counterparty_hex}",
                        if is_virtual { "virtual" } else { "regular" }
                    )),
                    Err(e) => ldk_live_debug(&format!(
                        "[rln-wasm-sdk ldk-live] failed to accept inbound channel {temporary_channel_id} from {counterparty_hex}: {e:?}"
                    )),
                }
            }
            // ---- Channel closed (cooperative or force): record for runtime-view removal ----
            // The live ChannelManager is authoritative for close completion; we propagate this to
            // the SDK channel view via reconcile (event-driven), instead of removing optimistically
            // on the close request (which would hide a still-open channel if the close stalls).
            Event::ChannelClosed { channel_id, .. } => {
                let id_hex = format!("{channel_id}");
                ldk_live_debug(&format!(
                    "[rln-wasm-sdk ldk-live] ChannelClosed channel_id={id_hex}"
                ));
                self.closed_channels.borrow_mut().push(id_hex);
            }
            // ---- Real payment lifecycle (outbound, this node is the payer) ----
            Event::PaymentSent {
                payment_hash,
                payment_preimage,
                ..
            } => {
                let hash_hex = hex::encode(payment_hash.0);
                // Settle the RGB channel split for the now-confirmed outbound HTLC.
                let _ = finalize_rgb_channel_payment(&payment_hash, false, &g.rgb_kv_store);
                self.mark_live_payment(
                    &hash_hex,
                    "succeeded",
                    Some(hex::encode(payment_preimage.0)),
                );
                ldk_live_debug(&format!(
                    "[rln-wasm-sdk ldk-live] PaymentSent hash={hash_hex}"
                ));
            }
            Event::PaymentFailed {
                payment_hash: Some(payment_hash),
                ..
            } => {
                let hash_hex = hex::encode(payment_hash.0);
                self.mark_live_payment(&hash_hex, "failed", None);
                ldk_live_debug(&format!(
                    "[rln-wasm-sdk ldk-live] PaymentFailed hash={hash_hex}"
                ));
            }
            // Surface HTLC-handling failures the live `ChannelManager` would otherwise swallow. For
            // an inbound payment this is the ONLY signal that an HTLC was rejected in the receive or
            // forward path *before* any `PaymentClaimable` (failed onion decode, unknown next hop, a
            // failed receive-path check, etc.). `failure_type` distinguishes `Receive` (we were the
            // final hop and rejected it) from `Forward`/`UnknownNextHop`/`InvalidForward` (misrouted
            // as a forward), and `failure_reason` carries the concrete LDK cause.
            Event::HTLCHandlingFailed {
                prev_channel_id,
                failure_type,
                failure_reason,
                ..
            } => {
                let best_h = g.channel_manager.current_best_block().height;
                ldk_live_debug(&format!(
                    "[rln-wasm-sdk ldk-live] HTLCHandlingFailed prev_channel={prev_channel_id} wasm_best_height={best_h} type={failure_type:?} reason={failure_reason:?}"
                ));
            }
            // ---- Real payment lifecycle (inbound, this node is the payee) ----
            Event::PaymentClaimable {
                payment_hash,
                purpose,
                amount_msat,
                receiving_channel_ids,
                claim_deadline,
                ..
            } => {
                let hash_hex = hex::encode(payment_hash.0);
                // Project the per-HTLC scoped inbound RGB record (chan_id||hash) onto the bare
                // payment-hash key so finalize can find it after the claim settles.
                project_inbound_rgb_payment_info(
                    &g.rgb_kv_store,
                    &receiving_channel_ids,
                    &hash_hex,
                );
                // Receive-path validation (parity with native `PaymentClaimable`): never
                // claim an expired or underpaid invoice — fail the HTLC back instead.
                {
                    let now = unix_now_secs();
                    let (expired, underpaid) = {
                        let live = self.live_payments.borrow();
                        match live.get(&hash_hex) {
                            Some(rec) => (
                                rec.expires_at.is_some_and(|exp| now >= exp),
                                rec.amt_msat.is_some_and(|expected| amount_msat < expected),
                            ),
                            None => (false, false),
                        }
                    };
                    if expired || underpaid {
                        g.channel_manager.fail_htlc_backwards(&payment_hash);
                        self.upsert_inbound_live_payment(&hash_hex, "failed", amount_msat);
                        ldk_live_debug(&format!(
                            "[rln-wasm-sdk ldk-live] PaymentClaimable failed back (expired={expired}, underpaid={underpaid}) hash={hash_hex}"
                        ));
                        return;
                    }
                }
                // Async-payment recipient invoices: we already know the preimage (derived
                // deterministically when we answered the host's `request_invoice`), so claim
                // immediately rather than holding like a normal external-hash/hodl invoice.
                if let Some(preimage) = self
                    .async_recipient_preimages
                    .borrow_mut()
                    .remove(&hash_hex)
                {
                    self.hodl_payment_hashes.borrow_mut().remove(&hash_hex);
                    g.channel_manager.claim_funds(preimage);
                    self.upsert_inbound_live_payment(&hash_hex, "pending", amount_msat);
                    ldk_live_debug(&format!(
                        "[rln-wasm-sdk ldk-live] PaymentClaimable async-recipient auto-claimed hash={hash_hex}"
                    ));
                    return;
                }
                let (preimage, external_hash_invoice) = match purpose {
                    lightning::events::PaymentPurpose::SpontaneousPayment(preimage) => {
                        (Some(preimage), false)
                    }
                    lightning::events::PaymentPurpose::Bolt11InvoicePayment {
                        payment_preimage,
                        ..
                    } => (payment_preimage, payment_preimage.is_none()),
                    _ => (None, false),
                };
                if external_hash_invoice || self.hodl_payment_hashes.borrow().contains(&hash_hex) {
                    self.hodl_payment_hashes
                        .borrow_mut()
                        .insert(hash_hex.clone());
                    // Track the CLTV deadline so we can fail back before a forced close.
                    if let Some(deadline) = claim_deadline {
                        self.hodl_claim_deadlines
                            .borrow_mut()
                            .insert(hash_hex.clone(), deadline);
                    }
                    self.upsert_inbound_live_payment(&hash_hex, "claimable", amount_msat);
                    ldk_live_debug(&format!(
                        "[rln-wasm-sdk ldk-live] PaymentClaimable held hash={hash_hex}"
                    ));
                    return;
                }
                if let Some(preimage) = preimage {
                    g.channel_manager.claim_funds(preimage);
                    self.upsert_inbound_live_payment(&hash_hex, "pending", amount_msat);
                    ldk_live_debug(&format!(
                        "[rln-wasm-sdk ldk-live] PaymentClaimable claimed hash={hash_hex}"
                    ));
                }
            }
            Event::PaymentClaimed {
                payment_hash,
                amount_msat,
                ..
            } => {
                let hash_hex = hex::encode(payment_hash.0);
                let _ = finalize_rgb_channel_payment(&payment_hash, true, &g.rgb_kv_store);
                self.upsert_inbound_live_payment(&hash_hex, "succeeded", amount_msat);
                ldk_live_debug(&format!(
                    "[rln-wasm-sdk ldk-live] PaymentClaimed hash={hash_hex}"
                ));
            }
            _ => {}
        }
    }

    /// Update an existing (outbound) live-payment record's status/preimage.
    fn mark_live_payment(&self, hash_hex: &str, status: &str, preimage: Option<String>) {
        let now = unix_now_secs();
        let mut map = self.live_payments.borrow_mut();
        let entry = map
            .entry(hash_hex.to_string())
            .or_insert_with(|| LdkRuntimeLivePaymentData {
                payment_hash: hash_hex.to_string(),
                status: "pending".to_string(),
                amt_msat: None,
                asset_id: None,
                asset_amount: None,
                inbound: false,
                preimage: None,
                expires_at: None,
                created_at: now,
                updated_at: now,
            });
        entry.status = status.to_string();
        if preimage.is_some() {
            entry.preimage = preimage;
        }
        entry.updated_at = now;
    }

    /// Insert/update an inbound live-payment record observed via PaymentClaimable/Claimed.
    fn upsert_inbound_live_payment(&self, hash_hex: &str, status: &str, amt_msat: u64) {
        let now = unix_now_secs();
        let mut map = self.live_payments.borrow_mut();
        let entry = map
            .entry(hash_hex.to_string())
            .or_insert_with(|| LdkRuntimeLivePaymentData {
                payment_hash: hash_hex.to_string(),
                status: status.to_string(),
                amt_msat: Some(amt_msat),
                asset_id: None,
                asset_amount: None,
                inbound: true,
                preimage: None,
                expires_at: None,
                created_at: now,
                updated_at: now,
            });
        entry.inbound = true;
        entry.status = status.to_string();
        entry.amt_msat = Some(amt_msat);
        entry.updated_at = now;
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
            match &event {
                Event::ChannelPending {
                    channel_id,
                    former_temporary_channel_id: Some(former_temp),
                    ..
                } if format!("{former_temp}") == temporary_channel_id_hex => {
                    resolved_channel_id = format!("{channel_id}");
                }
                Event::ChannelReady { channel_id, .. }
                    if format!("{channel_id}") == temporary_channel_id_hex =>
                {
                    is_ready = true;
                }
                _ => {}
            }
            self.handle_ldk_event_sync(g, event);
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

        if let Some(prev) = g.peer_descriptors.borrow_mut().remove(peer_pubkey) {
            g.peer_manager.borrow().socket_disconnected(&prev);
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
        g.peer_descriptors
            .borrow_mut()
            .insert(peer_pubkey.to_string(), descriptor);
        Ok(hex::encode(act_one))
    }

    fn read_event(&self, payload_hex: &str) -> Result<(), JsValue> {
        let peer_pubkey = self
            .connected_peers
            .borrow()
            .iter()
            .next()
            .cloned()
            .ok_or_else(|| JsValue::from_str(sdk_contracts::ERR_ACTIVE_PEER_DESCRIPTOR_MISSING))?;
        self.read_event_for_peer(&peer_pubkey, payload_hex)
    }

    fn read_event_for_peer(&self, peer_pubkey: &str, payload_hex: &str) -> Result<(), JsValue> {
        self.ensure_phase1_runtime_ready()?;
        if !self.connected_peers.borrow().contains(peer_pubkey) {
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
            .peer_descriptors
            .borrow()
            .get(peer_pubkey)
            .cloned()
            .ok_or_else(|| JsValue::from_str(sdk_contracts::ERR_ACTIVE_PEER_DESCRIPTOR_MISSING))?;
        let Ok(peer_manager) = g.peer_manager.try_borrow_mut() else {
            // Peer-manager callbacks can synchronously release outbound frames and receive the
            // counterparty's response before the current `process_events` borrow is released.
            // Queue that reentrant frame and drain it on the next processing pass.
            self.inbound_frames
                .borrow_mut()
                .push_back((peer_pubkey.to_string(), payload_hex.to_string()));
            return Ok(());
        };
        peer_manager
            .read_event(&mut desc, &bytes)
            .map_err(|_e: PeerHandleError| {
                JsValue::from_str(sdk_contracts::ERR_PEER_MANAGER_READ_EVENT_FAILED)
            })?;
        drop(peer_manager);
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
        // Process queued reentrant inbound frames in bounded passes. Calls to `process_events`
        // may synchronously cause more inbound frames, which `read_event` queues while the peer
        // manager is borrowed.
        for _ in 0..64 {
            let queued: Vec<(String, String)> =
                self.inbound_frames.borrow_mut().drain(..).collect();
            let peer_manager = g.peer_manager.borrow_mut();
            for (peer_pubkey, payload_hex) in queued {
                if let Some(mut desc) = g.peer_descriptors.borrow().get(&peer_pubkey).cloned() {
                    let bytes = hex::decode(payload_hex).map_err(|e| {
                        JsValue::from_str(&format!("invalid queued payload_hex: {e}"))
                    })?;
                    peer_manager
                        .read_event(&mut desc, &bytes)
                        .map_err(|_e: PeerHandleError| {
                            JsValue::from_str(sdk_contracts::ERR_PEER_MANAGER_READ_EVENT_FAILED)
                        })?;
                }
            }
            peer_manager.process_events();
            drop(peer_manager);
            if self.inbound_frames.borrow().is_empty() {
                break;
            }
        }
        // Service any inbound async-payment `request_invoice` calls (recipient side): mint the
        // invoice and queue the response, then flush it out to the host.
        if self.handle_pending_async_order_requests(g) {
            g.peer_manager.borrow().process_events();
        }
        // Advance received/forwarded HTLCs (no PendingHTLCsForwardable event exists in this LDK;
        // it must be driven explicitly) so inbound payments reach PaymentClaimable.
        g.channel_manager.process_pending_htlc_forwards();
        self.ingest_funding_generation_ready_events(g);
        persist_ldk_runtime_snapshots(&self.runtime_key, g)?;
        ldk_live_debug("[rln-wasm-sdk ldk-live] process_events");
        Ok(())
    }

    fn take_outbound_frames(&self) -> Result<Vec<String>, JsValue> {
        let peer_pubkey = self
            .connected_peers
            .borrow()
            .iter()
            .next()
            .cloned()
            .ok_or_else(|| JsValue::from_str(sdk_contracts::ERR_ACTIVE_PEER_DESCRIPTOR_MISSING))?;
        self.take_outbound_frames_for_peer(&peer_pubkey)
    }

    fn take_outbound_frames_for_peer(&self, peer_pubkey: &str) -> Result<Vec<String>, JsValue> {
        self.ensure_phase1_runtime_ready()?;
        if !self.connected_peers.borrow().contains(peer_pubkey) {
            return Ok(Vec::new());
        }
        let graph = self.object_graph.borrow();
        let Some(g) = graph.as_ref() else {
            return Err(JsValue::from_str(
                sdk_contracts::ERR_LDK_OBJECT_GRAPH_NOT_INITIALIZED,
            ));
        };
        let desc = g
            .peer_descriptors
            .borrow()
            .get(peer_pubkey)
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
        let peers = self
            .connected_peers
            .borrow()
            .iter()
            .cloned()
            .collect::<Vec<_>>();
        for peer_pubkey in peers {
            self.socket_disconnected_for_peer(&peer_pubkey)?;
        }
        Ok(())
    }

    fn socket_disconnected_for_peer(&self, peer_pubkey: &str) -> Result<(), JsValue> {
        self.ensure_phase1_runtime_ready()?;
        self.connected_peers.borrow_mut().remove(peer_pubkey);
        self.disconnected
            .set(self.connected_peers.borrow().is_empty());
        self.inbound_frames
            .borrow_mut()
            .retain(|(queued_peer, _)| queued_peer != peer_pubkey);
        let graph = self.object_graph.borrow();
        if let Some(g) = graph.as_ref() {
            if let Some(desc) = g.peer_descriptors.borrow_mut().remove(peer_pubkey) {
                g.peer_manager.borrow().socket_disconnected(&desc);
            }
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

    fn close_live_channel(
        &self,
        channel_id: &str,
        peer_pubkey: &str,
        force: bool,
    ) -> Result<(), JsValue> {
        self.ensure_phase1_runtime_ready()?;
        let channel_id_bytes = hex::decode(channel_id)
            .map_err(|e| JsValue::from_str(&format!("invalid channel id: {e}")))?;
        let channel_id_bytes: [u8; 32] = channel_id_bytes
            .try_into()
            .map_err(|_| JsValue::from_str("invalid channel id length"))?;
        let channel_id = lightning::ln::types::ChannelId::from_bytes(channel_id_bytes);
        let peer_pubkey_bytes = hex::decode(peer_pubkey)
            .map_err(|e| JsValue::from_str(&format!("invalid peer pubkey: {e}")))?;
        let peer_pubkey = SecpPublicKey::from_slice(&peer_pubkey_bytes)
            .map_err(|e| JsValue::from_str(&format!("invalid peer pubkey: {e}")))?;

        self.with_graph(|g| {
            if force {
                // Matches the reference rgb-lightning-node SDK (routes.rs close_channel force path):
                // broadcast the latest commitment immediately rather than negotiating a coop close.
                g.channel_manager
                    .force_close_broadcasting_latest_txn(
                        &channel_id,
                        &peer_pubkey,
                        "Manually force-closed".to_string(),
                    )
                    .map_err(|e| {
                        JsValue::from_str(&format!("live channel force-close failed: {e:?}"))
                    })?;
            } else {
                g.channel_manager
                    .close_channel(&channel_id, &peer_pubkey)
                    .map_err(|e| JsValue::from_str(&format!("live channel close failed: {e:?}")))?;
            }
            g.peer_manager.borrow().process_events();
            persist_ldk_runtime_snapshots(&self.runtime_key, g)?;
            Ok(())
        })
    }

    fn virtual_channel_abandon_local(
        &self,
        channel_id: &str,
        peer_pubkey: &str,
    ) -> Result<(), JsValue> {
        self.ensure_phase1_runtime_ready()?;
        let channel_id_bytes = hex::decode(channel_id)
            .map_err(|e| JsValue::from_str(&format!("invalid channel id: {e}")))?;
        let channel_id_bytes: [u8; 32] = channel_id_bytes
            .try_into()
            .map_err(|_| JsValue::from_str("invalid channel id length"))?;
        let channel_id = lightning::ln::types::ChannelId::from_bytes(channel_id_bytes);
        let peer_pubkey_bytes = hex::decode(peer_pubkey)
            .map_err(|e| JsValue::from_str(&format!("invalid peer pubkey: {e}")))?;
        let peer_pubkey = SecpPublicKey::from_slice(&peer_pubkey_bytes)
            .map_err(|e| JsValue::from_str(&format!("invalid peer pubkey: {e}")))?;

        self.with_graph(|g| {
            // Drop our side of a never-broadcast virtual channel. The counterparty (LSP/host) uses
            // `ErrorAction::IgnoreError` when it abandons, so it never notifies us — a client that has
            // drained its value must therefore tear down its own side. `abandon_virtual_channel`
            // removes the channel and fires `Event::ChannelClosed`, which the authoritative reconcile
            // then propagates to our channel views. `dangerous_ack=true` acknowledges that any
            // remaining (sub-HTLC-minimum dust) balance is forfeited.
            g.channel_manager
                .abandon_virtual_channel(&channel_id, &peer_pubkey, true)
                .map_err(|e| {
                    JsValue::from_str(&format!("virtual channel abandon failed: {e:?}"))
                })?;
            g.peer_manager.borrow().process_events();
            persist_ldk_runtime_snapshots(&self.runtime_key, g)?;
            Ok(())
        })
    }

    fn keysend_live(
        &self,
        dest_pubkey: &str,
        amt_msat: u64,
        contract_id: Option<String>,
        asset_amount: Option<u64>,
    ) -> Result<LdkRuntimeLivePaymentData, JsValue> {
        self.ensure_phase1_runtime_ready()?;
        if self.disconnected.get() {
            return Err(JsValue::from_str(
                sdk_contracts::ERR_PEER_TRANSPORT_DISCONNECTED,
            ));
        }
        let dest = SecpPublicKey::from_slice(
            &hex::decode(dest_pubkey.trim())
                .map_err(|_| JsValue::from_str(sdk_contracts::ERR_DEST_PUBKEY_INVALID))?,
        )
        .map_err(|_| JsValue::from_str(sdk_contracts::ERR_DEST_PUBKEY_INVALID))?;

        let rgb_payment = match (contract_id.as_ref(), asset_amount) {
            (Some(cid), Some(amount)) => {
                let contract = cid
                    .trim()
                    .parse::<ContractId>()
                    .map_err(|e| JsValue::from_str(&format!("invalid asset_id: {e}")))?;
                Some((contract, amount))
            }
            (None, None) => None,
            _ => {
                return Err(JsValue::from_str(
                    "asset_id and asset_amount must be provided together",
                ));
            }
        };

        let graph = self.object_graph.borrow();
        let Some(g) = graph.as_ref() else {
            return Err(JsValue::from_str(
                sdk_contracts::ERR_LDK_OBJECT_GRAPH_NOT_INITIALIZED,
            ));
        };

        // Real spontaneous (keysend) payment: derive a random preimage; hash is its SHA-256.
        let mut preimage_bytes = [0u8; 32];
        preimage_bytes.copy_from_slice(&g.keys_manager.get_secure_random_bytes()[..32]);
        let payment_preimage = PaymentPreimage(preimage_bytes);
        let payment_hash = PaymentHash(
            <Sha256 as bitcoin_hashes::Hash>::hash(&payment_preimage.0).to_byte_array(),
        );
        let payment_id = PaymentId(payment_hash.0);
        let hash_hex = hex::encode(payment_hash.0);

        if let Some((contract, amount)) = rgb_payment {
            // Tells the RGB HTLC machinery how much asset rides this payment (mirrors native send).
            let _ = write_rgb_payment_info_file(
                &payment_hash,
                contract,
                amount,
                false,
                false,
                &g.rgb_kv_store,
            );
        }

        let route_params = RouteParameters::from_payment_params_and_value(
            PaymentParameters::for_keysend(dest, 40, false),
            amt_msat,
            rgb_payment,
        );

        g.channel_manager
            .send_spontaneous_payment(
                Some(payment_preimage),
                RecipientOnionFields::spontaneous_empty(),
                payment_id,
                route_params,
                // Duration-based Retry uses std::time::Instant, which panics on wasm; use attempts.
                Retry::Attempts(3),
            )
            .map_err(|e| JsValue::from_str(&format!("keysend failed: {e:?}")))?;

        // Flush the freshly-queued commitment messages to the outbound frame queue.
        g.peer_manager.borrow().process_events();

        let now = unix_now_secs();
        let record = LdkRuntimeLivePaymentData {
            payment_hash: hash_hex.clone(),
            status: "pending".to_string(),
            amt_msat: Some(amt_msat),
            asset_id: contract_id,
            asset_amount,
            inbound: false,
            preimage: None,
            expires_at: None,
            created_at: now,
            updated_at: now,
        };
        self.live_payments
            .borrow_mut()
            .insert(hash_hex, record.clone());
        Ok(record)
    }

    fn send_bolt11_live(
        &self,
        invoice: &str,
        amt_msat: Option<u64>,
        contract_id: Option<String>,
        asset_amount: Option<u64>,
    ) -> Result<LdkRuntimeLivePaymentData, JsValue> {
        self.ensure_phase1_runtime_ready()?;
        if self.disconnected.get() {
            return Err(JsValue::from_str(
                sdk_contracts::ERR_PEER_TRANSPORT_DISCONNECTED,
            ));
        }
        let invoice = invoice
            .parse::<Bolt11Invoice>()
            .map_err(|e| JsValue::from_str(&format!("invalid invoice: {e}")))?;
        let resolved_amt_msat = match (invoice.amount_milli_satoshis(), amt_msat) {
            (Some(invoice_amt), Some(requested)) if invoice_amt != requested => {
                return Err(JsValue::from_str(&format!(
                    "amount didn't match invoice value of {invoice_amt}msat"
                )));
            }
            (Some(invoice_amt), _) => invoice_amt,
            (None, Some(requested)) => requested,
            (None, None) => {
                return Err(JsValue::from_str(
                    "need an amount for the given 0-value invoice",
                ));
            }
        };
        let invoice_rgb = invoice.rgb_contract_id().zip(invoice.rgb_amount());
        let requested_rgb = match (contract_id.as_ref(), asset_amount) {
            (Some(contract_id), Some(amount)) => Some((
                contract_id
                    .parse::<ContractId>()
                    .map_err(|e| JsValue::from_str(&format!("invalid asset_id: {e}")))?,
                amount,
            )),
            (None, None) => None,
            _ => {
                return Err(JsValue::from_str(
                    "asset_id and asset_amount must be provided together",
                ));
            }
        };
        if invoice_rgb.is_some() && requested_rgb.is_some() && invoice_rgb != requested_rgb {
            return Err(JsValue::from_str(
                "invoice RGB payment does not match the requested asset payment",
            ));
        }
        let rgb_payment = invoice_rgb.or(requested_rgb);
        let payment_hash = PaymentHash(invoice.payment_hash().to_byte_array());
        let payment_id = PaymentId(payment_hash.0);
        let hash_hex = hex::encode(payment_hash.0);

        let graph = self.object_graph.borrow();
        let g = graph.as_ref().ok_or_else(|| {
            JsValue::from_str(sdk_contracts::ERR_LDK_OBJECT_GRAPH_NOT_INITIALIZED)
        })?;
        if let Some((contract, amount)) = rgb_payment {
            let _ = write_rgb_payment_info_file(
                &payment_hash,
                contract,
                amount,
                false,
                false,
                &g.rgb_kv_store,
            );
        }
        let mut recipient_onion = RecipientOnionFields::secret_only(*invoice.payment_secret());
        recipient_onion.payment_metadata = invoice.payment_metadata().cloned();
        let payment_params = PaymentParameters::from_bolt11_invoice(&invoice);
        let route_params = RouteParameters::from_payment_params_and_value(
            payment_params,
            resolved_amt_msat,
            rgb_payment,
        );
        g.channel_manager
            .send_payment(
                payment_hash,
                recipient_onion,
                payment_id,
                route_params,
                Retry::Attempts(3),
            )
            .map_err(|e| JsValue::from_str(&format!("invoice payment failed: {e:?}")))?;
        g.peer_manager.borrow().process_events();

        let now = unix_now_secs();
        let record = LdkRuntimeLivePaymentData {
            payment_hash: hash_hex.clone(),
            status: "pending".to_string(),
            amt_msat: Some(resolved_amt_msat),
            asset_id: rgb_payment.map(|(contract, _)| contract.to_string()),
            asset_amount: rgb_payment.map(|(_, amount)| amount),
            inbound: false,
            preimage: None,
            expires_at: None,
            created_at: now,
            updated_at: now,
        };
        self.live_payments
            .borrow_mut()
            .insert(hash_hex, record.clone());
        Ok(record)
    }

    fn create_bolt11_invoice_live(
        &self,
        amt_msat: Option<u64>,
        expiry_sec: u32,
        contract_id: Option<String>,
        asset_amount: Option<u64>,
    ) -> Result<String, JsValue> {
        self.ensure_phase1_runtime_ready()?;
        let contract_id = contract_id
            .map(|id| {
                id.parse::<ContractId>()
                    .map_err(|e| JsValue::from_str(&format!("invalid asset_id: {e}")))
            })
            .transpose()?;
        let graph = self.object_graph.borrow();
        let g = graph.as_ref().ok_or_else(|| {
            JsValue::from_str(sdk_contracts::ERR_LDK_OBJECT_GRAPH_NOT_INITIALIZED)
        })?;
        let invoice = g
            .channel_manager
            .create_bolt11_invoice(Bolt11InvoiceParameters {
                amount_msats: amt_msat,
                description: Bolt11InvoiceDescription::Direct(Description::empty()),
                invoice_expiry_delta_secs: Some(expiry_sec),
                contract_id,
                asset_amount,
                ..Default::default()
            })
            .map_err(|e| JsValue::from_str(&format!("failed to create invoice: {e}")))?;
        let hash_hex = invoice.payment_hash().to_string();
        let now = unix_now_secs();
        self.live_payments.borrow_mut().insert(
            hash_hex.clone(),
            LdkRuntimeLivePaymentData {
                payment_hash: hash_hex,
                status: "pending".to_string(),
                amt_msat,
                asset_id: contract_id.map(|contract| contract.to_string()),
                asset_amount,
                inbound: true,
                preimage: None,
                expires_at: Some(now.saturating_add(expiry_sec as u64)),
                created_at: now,
                updated_at: now,
            },
        );
        persist_ldk_runtime_snapshots(&self.runtime_key, g)?;
        Ok(invoice.to_string())
    }

    fn create_hodl_bolt11_invoice_live(
        &self,
        amt_msat: Option<u64>,
        expiry_sec: u32,
        contract_id: Option<String>,
        asset_amount: Option<u64>,
        payment_hash: &str,
    ) -> Result<String, JsValue> {
        self.ensure_phase1_runtime_ready()?;
        let payment_hash_bytes = hex::decode(payment_hash)
            .map_err(|_| JsValue::from_str(sdk_contracts::ERR_PAYMENT_HASH_INVALID))?;
        let payment_hash = PaymentHash(
            payment_hash_bytes
                .try_into()
                .map_err(|_| JsValue::from_str(sdk_contracts::ERR_PAYMENT_HASH_INVALID))?,
        );
        let hash_hex = hex::encode(payment_hash.0);
        if self.hodl_payment_hashes.borrow().contains(&hash_hex)
            || self.live_payments.borrow().contains_key(&hash_hex)
        {
            return Err(JsValue::from_str(
                sdk_contracts::ERR_PAYMENT_HASH_ALREADY_USED,
            ));
        }
        let contract_id = contract_id
            .map(|id| {
                id.parse::<ContractId>()
                    .map_err(|e| JsValue::from_str(&format!("invalid asset_id: {e}")))
            })
            .transpose()?;
        let graph = self.object_graph.borrow();
        let g = graph.as_ref().ok_or_else(|| {
            JsValue::from_str(sdk_contracts::ERR_LDK_OBJECT_GRAPH_NOT_INITIALIZED)
        })?;
        let invoice = g
            .channel_manager
            .create_bolt11_invoice(Bolt11InvoiceParameters {
                amount_msats: amt_msat,
                description: Bolt11InvoiceDescription::Direct(Description::empty()),
                invoice_expiry_delta_secs: Some(expiry_sec),
                payment_hash: Some(payment_hash),
                contract_id,
                asset_amount,
                ..Default::default()
            })
            .map_err(|e| JsValue::from_str(&format!("failed to create HODL invoice: {e}")))?;
        let now = unix_now_secs();
        self.hodl_payment_hashes
            .borrow_mut()
            .insert(hash_hex.clone());
        self.live_payments.borrow_mut().insert(
            hash_hex.clone(),
            LdkRuntimeLivePaymentData {
                payment_hash: hash_hex,
                status: "pending".to_string(),
                amt_msat,
                asset_id: contract_id.map(|contract| contract.to_string()),
                asset_amount,
                inbound: true,
                preimage: None,
                expires_at: Some(now.saturating_add(expiry_sec as u64)),
                created_at: now,
                updated_at: now,
            },
        );
        persist_ldk_runtime_snapshots(&self.runtime_key, g)?;
        Ok(invoice.to_string())
    }

    fn claim_hodl_invoice_live(
        &self,
        payment_hash: &str,
        payment_preimage: &str,
    ) -> Result<bool, JsValue> {
        self.ensure_phase1_runtime_ready()?;
        if !self.hodl_payment_hashes.borrow().contains(payment_hash) {
            return Err(JsValue::from_str(sdk_contracts::ERR_LN_INVOICE_UNKNOWN));
        }
        let preimage_bytes = hex::decode(payment_preimage)
            .map_err(|_| JsValue::from_str(sdk_contracts::ERR_PAYMENT_PREIMAGE_INVALID))?;
        let preimage = PaymentPreimage(
            preimage_bytes
                .try_into()
                .map_err(|_| JsValue::from_str(sdk_contracts::ERR_PAYMENT_PREIMAGE_INVALID))?,
        );
        let expected_hash: [u8; 32] = hex::decode(payment_hash)
            .map_err(|_| JsValue::from_str(sdk_contracts::ERR_PAYMENT_HASH_INVALID))?
            .try_into()
            .map_err(|_| JsValue::from_str(sdk_contracts::ERR_PAYMENT_HASH_INVALID))?;
        if <Sha256 as bitcoin_hashes::Hash>::hash(&preimage.0).to_byte_array() != expected_hash {
            return Err(JsValue::from_str(
                sdk_contracts::ERR_PAYMENT_PREIMAGE_INVALID,
            ));
        }
        let status = self
            .live_payments
            .borrow()
            .get(payment_hash)
            .map(|payment| payment.status.clone())
            .ok_or_else(|| JsValue::from_str(sdk_contracts::ERR_LN_INVOICE_UNKNOWN))?;
        match status.as_str() {
            "succeeded" => return Ok(false),
            "claiming" => return Err(JsValue::from_str(sdk_contracts::ERR_LN_INVOICE_SETTLING)),
            "claimable" => {}
            _ => {
                return Err(JsValue::from_str(
                    sdk_contracts::ERR_LN_INVOICE_NOT_CLAIMABLE,
                ))
            }
        }
        let graph = self.object_graph.borrow();
        let g = graph.as_ref().ok_or_else(|| {
            JsValue::from_str(sdk_contracts::ERR_LDK_OBJECT_GRAPH_NOT_INITIALIZED)
        })?;
        g.channel_manager.claim_funds(preimage);
        self.mark_live_payment(payment_hash, "claiming", Some(payment_preimage.to_string()));
        self.hodl_claim_deadlines.borrow_mut().remove(payment_hash);
        g.peer_manager.borrow().process_events();
        Ok(true)
    }

    fn cancel_hodl_invoice_live(&self, payment_hash: &str) -> Result<(), JsValue> {
        self.ensure_phase1_runtime_ready()?;
        if !self.hodl_payment_hashes.borrow().contains(payment_hash) {
            return Err(JsValue::from_str(sdk_contracts::ERR_LN_INVOICE_UNKNOWN));
        }
        let status = self
            .live_payments
            .borrow()
            .get(payment_hash)
            .map(|payment| payment.status.clone())
            .ok_or_else(|| JsValue::from_str(sdk_contracts::ERR_LN_INVOICE_UNKNOWN))?;
        match status.as_str() {
            "succeeded" => {
                return Err(JsValue::from_str(
                    sdk_contracts::ERR_LN_INVOICE_ALREADY_CLAIMED,
                ))
            }
            "claiming" => return Err(JsValue::from_str(sdk_contracts::ERR_LN_INVOICE_SETTLING)),
            "claimable" => {}
            _ => {
                return Err(JsValue::from_str(
                    sdk_contracts::ERR_LN_INVOICE_NOT_CLAIMABLE,
                ))
            }
        }
        let payment_hash_bytes = hex::decode(payment_hash)
            .map_err(|_| JsValue::from_str(sdk_contracts::ERR_PAYMENT_HASH_INVALID))?;
        let payment_hash = PaymentHash(
            payment_hash_bytes
                .try_into()
                .map_err(|_| JsValue::from_str(sdk_contracts::ERR_PAYMENT_HASH_INVALID))?,
        );
        let graph = self.object_graph.borrow();
        let g = graph.as_ref().ok_or_else(|| {
            JsValue::from_str(sdk_contracts::ERR_LDK_OBJECT_GRAPH_NOT_INITIALIZED)
        })?;
        g.channel_manager.fail_htlc_backwards(&payment_hash);
        let hash_hex = hex::encode(payment_hash.0);
        self.mark_live_payment(&hash_hex, "cancelled", None);
        self.hodl_claim_deadlines.borrow_mut().remove(&hash_hex);
        g.peer_manager.borrow().process_events();
        Ok(())
    }

    fn live_payments(&self) -> Vec<LdkRuntimeLivePaymentData> {
        self.live_payments.borrow().values().cloned().collect()
    }

    fn live_payment(&self, payment_hash: &str) -> Option<LdkRuntimeLivePaymentData> {
        self.live_payments.borrow().get(payment_hash).cloned()
    }

    fn take_closed_live_channels(&self) -> Vec<String> {
        std::mem::take(&mut *self.closed_channels.borrow_mut())
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
            let current_height = g.channel_manager.current_best_block().height;
            if height < current_height {
                ldk_live_debug(&format!(
                    "[rln-wasm-sdk chain-sync] refusing regressed best block height={height} current={current_height}"
                ));
                return Ok(());
            }
            g.chain_monitor.best_block_updated(&header, height);
            g.channel_manager.best_block_updated(&header, height);
            // Proactively fail back held hodl HTLCs whose CLTV claim deadline has passed,
            // before the deadline forces a channel close.
            let expired: Vec<String> = self
                .hodl_claim_deadlines
                .borrow()
                .iter()
                .filter(|(_, &deadline)| height >= deadline)
                .map(|(hash, _)| hash.clone())
                .collect();
            for hash_hex in expired {
                if let Some(arr) = hex::decode(&hash_hex)
                    .ok()
                    .and_then(|bytes| <[u8; 32]>::try_from(bytes).ok())
                {
                    g.channel_manager.fail_htlc_backwards(&PaymentHash(arr));
                }
                self.hodl_payment_hashes.borrow_mut().remove(&hash_hex);
                self.hodl_claim_deadlines.borrow_mut().remove(&hash_hex);
                if let Some(rec) = self.live_payments.borrow_mut().get_mut(&hash_hex) {
                    rec.status = "cancelled".to_string();
                    rec.updated_at = unix_now_secs();
                }
                ldk_live_debug(&format!(
                    "[rln-wasm-sdk ldk-live] hodl HTLC failed back at CLTV deadline height={height} hash={hash_hex}"
                ));
            }
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
        // Parse RGB open params when an asset is specified.
        let rgb_open: Option<(
            lightning::rgb_utils::ContractId,
            u64,
            lightning::rgb_utils::RgbTransport,
        )> = if request.asset_id.is_some() {
            let contract_id_str = request.contract_id.as_deref().unwrap_or("").trim();
            let endpoint_str = request.consignment_endpoint.as_deref().unwrap_or("").trim();
            let asset_amount = request.asset_local_amount.unwrap_or(0);
            if contract_id_str.is_empty() {
                return Err(JsValue::from_str(
                    "contract_id is required for RGB channel open",
                ));
            }
            if endpoint_str.is_empty() {
                return Err(JsValue::from_str(
                    "consignment_endpoint is required for RGB channel open",
                ));
            }
            if asset_amount == 0 {
                return Err(JsValue::from_str(
                    "asset_local_amount must be > 0 for RGB channel open",
                ));
            }
            let contract_id = contract_id_str
                .parse::<lightning::rgb_utils::ContractId>()
                .map_err(|e| JsValue::from_str(&format!("invalid contract_id: {e}")))?;
            let endpoint = endpoint_str
                .parse::<lightning::rgb_utils::RgbTransport>()
                .map_err(|e| JsValue::from_str(&format!("invalid consignment_endpoint: {e}")))?;
            Some((contract_id, asset_amount, endpoint))
        } else {
            None
        };

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
        let user_channel_id = self.next_user_channel_id();
        let (rgb_endpoint, rgb_push_amount) = rgb_open
            .as_ref()
            // `asset_local_amount` remains on the opener's side. LDK's RGB amount field is
            // the amount pushed to the counterparty, so this channel opens with a zero push.
            .map(|(_, _, ep)| (Some(ep.clone()), Some(0)))
            .unwrap_or((None, None));
        // Per-channel handshake config matching native `open_channel` (non-virtual path):
        // anchors are mandatory for colored (RGB) channels; announce only when public;
        // 3M-msat HTLC floor; 6-conf funding; LND-compatible 2016-block to_self_delay.
        let with_anchors = rgb_open.is_some();
        let mut override_config = UserConfig::default();
        override_config
            .channel_handshake_config
            .announce_for_forwarding = request.public;
        override_config
            .channel_handshake_config
            .our_htlc_minimum_msat = OPEN_CHANNEL_HTLC_MIN_MSAT;
        override_config.channel_handshake_config.minimum_depth = MIN_CHANNEL_CONFIRMATIONS;
        override_config
            .channel_handshake_config
            .negotiate_anchors_zero_fee_htlc_tx = with_anchors;
        override_config.channel_handshake_limits.their_to_self_delay = CHANNEL_TO_SELF_DELAY;
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
                user_channel_id,
                None,
                Some(override_config.clone()),
                rgb_endpoint.clone(),
                rgb_push_amount,
                false,
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

        // Store the RGB open intent and temporary channel RGB state before peer processing.
        // LDK renames this state to the funding channel ID when funding is generated.
        if let Some((contract_id, asset_amount, consignment_endpoint)) = rgb_open {
            let temp_id = format!("{temporary_channel_id}");
            // Use the real asset schema (derived from wallet metadata by the caller); the wasm
            // RGB backend only supports the fungible Nia/Ifa schemas.
            let schema = parse_rgb_asset_schema(request.asset_schema.as_deref());
            let rgb_info = RgbInfo {
                contract_id,
                schema,
                local_rgb_amount: asset_amount,
                remote_rgb_amount: 0,
                batch_transfer_idx: None,
            };
            let _ = g
                .rgb_kv_store
                .write_rgb_channel_info(&temp_id, &rgb_info, false);
            let _ = g
                .rgb_kv_store
                .write_rgb_channel_info(&temp_id, &rgb_info, true);
            self.pending_rgb_open_intents.borrow_mut().insert(
                user_channel_id,
                PendingRgbOpenIntent {
                    contract_id,
                    schema,
                    asset_amount,
                    consignment_endpoint,
                    fee_rate: 1,
                    min_confirmations: 0,
                },
            );
        }

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
            // The outbound-open RGB asset is tracked via the open intent/cache and surfaced by
            // `list_live_channels` once its RGB info is persisted; not known at this return point.
            asset_id: None,
            asset_local_amount: None,
            // A just-opened channel has no spendable outbound yet; refreshed on the next reconcile.
            outbound_msat: 0,
            next_outbound_htlc_limit_msat: 0,
            // The live channel's virtual flag is surfaced by the next reconcile.
            virtual_open_mode: None,
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
            .map(|details| {
                let channel_id_str = format!("{}", details.channel_id);
                // Read the channel's RGB info from the kv store, mirroring the native node's
                // `list_channels` (`src/routes.rs`). This surfaces `asset_id` for *inbound*
                // (LSP-opened) RGB channels too — previously the runtime only knew the asset for
                // channels this node opened itself (from the outbound-open cache), so accepted RGB
                // channels showed up as vanilla. The info is written once the channel's RGB funding
                // consignment is validated.
                let rgb_info = g
                    .rgb_kv_store
                    .read_rgb_channel_info(&channel_id_str, false)
                    .ok();
                LdkRuntimeOpenChannelResultData {
                    temporary_channel_id: channel_id_str.clone(),
                    channel_id: channel_id_str,
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
                    asset_id: rgb_info.as_ref().map(|i| i.contract_id.to_string()),
                    asset_local_amount: rgb_info.as_ref().map(|i| i.local_rgb_amount),
                    outbound_msat: details.outbound_capacity_msat,
                    next_outbound_htlc_limit_msat: details.next_outbound_htlc_limit_msat,
                    // A never-broadcast virtual channel — recognized for both opened and accepted
                    // channels, so a client can tear down its accepted virtual channel.
                    virtual_open_mode: if details.trusted_no_broadcast {
                        Some("trusted_no_broadcast".to_string())
                    } else {
                        None
                    },
                }
            })
            .collect();
        Ok(channels)
    }

    fn drive_rgb_funding_work_boxed(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<(), JsValue>> + 'static>> {
        let maybe_this = self.self_weak.borrow().upgrade();
        Box::pin(async move {
            if let Some(this) = maybe_this {
                WasmLdkLiveBackend::drive_rgb_funding_work_impl(this).await?;
            }
            Ok(())
        })
    }

    fn process_pending_rgb_transactions_boxed(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<(), JsValue>> + 'static>> {
        let maybe_this = self.self_weak.borrow().upgrade();
        Box::pin(async move {
            if let Some(this) = maybe_this {
                this.run_process_pending_rgb_transactions().await?;
            }
            Ok(())
        })
    }

    fn apay_new_boxed(
        &self,
        host_node_id: String,
        username: Option<String>,
        domain: Option<String>,
    ) -> Pin<Box<dyn Future<Output = Result<crate::apay::AsyncOrderNewResponse, JsValue>> + 'static>>
    {
        let maybe_this = self.self_weak.borrow().upgrade();
        Box::pin(async move {
            let this = maybe_this
                .ok_or_else(|| JsValue::from_str("apay_new: live backend was dropped"))?;
            WasmLdkLiveBackend::apay_new_impl(this, host_node_id, username, domain).await
        })
    }
}

impl WasmLdkLiveBackend {
    /// Build, sign and ship an `async_order.new` request to the invoice-host / LSP peer, then
    /// await its JSON-RPC response. See [`LdkLiveBackend::apay_new_boxed`].
    async fn apay_new_impl(
        this: Rc<Self>,
        host_node_id: String,
        username: Option<String>,
        domain: Option<String>,
    ) -> Result<crate::apay::AsyncOrderNewResponse, JsValue> {
        if username.is_some() != domain.is_some() {
            return Err(JsValue::from_str(
                "apay_new: username and domain must be supplied together",
            ));
        }

        this.ensure_phase1_runtime_ready()?;
        let host_pubkey = hex::decode(host_node_id.trim())
            .ok()
            .and_then(|bytes| SecpPublicKey::from_slice(&bytes).ok())
            .ok_or_else(|| JsValue::from_str(sdk_contracts::ERR_PEER_PUBKEY_INVALID))?;

        // Synchronous prep: derive + sign the batch, queue the request, flush it. We must not hold
        // the `object_graph` borrow across the later `.await`, so collect everything we need here.
        let (request_id, response_rx, first_hash_index, last_hash_index, hashes) = {
            let graph = this.object_graph.borrow();
            let g = graph.as_ref().ok_or_else(|| {
                JsValue::from_str(sdk_contracts::ERR_LDK_OBJECT_GRAPH_NOT_INITIALIZED)
            })?;

            // Require a live (usable) channel with the invoice-host, mirroring the native node.
            let host_has_live_channel = g
                .channel_manager
                .list_channels()
                .into_iter()
                .any(|c| c.counterparty.node_id == host_pubkey && c.is_usable);
            if !host_has_live_channel {
                return Err(JsValue::from_str(
                    "apay_new requires a live channel with the invoice-host peer",
                ));
            }

            let local_node_id = g
                .keys_manager
                .get_node_id(Recipient::Node)
                .map_err(|_| JsValue::from_str("apay_new: failed to derive local node id"))?;
            let local_node_hex = local_node_id.to_string();
            let host_node_hex = apay::hex_str(&host_pubkey.serialize());

            // Derive the next hash batch from this node's seed.
            let start_index =
                apay::read_async_payments_next_hash_index(g.rgb_kv_store.as_ref(), &host_pubkey)
                    .map_err(|err| JsValue::from_str(&err.to_string()))?;
            let seed = this.derive_seed32();
            let preimage_root = apay::AsyncPaymentsPreimageRoot::build_from_seed(
                &seed,
                network_for_runtime(&this.runtime_key),
                &local_node_id,
            )
            .map_err(|err| JsValue::from_str(&err.to_string()))?;
            let mut params = preimage_root
                .prepare_async_order_new_params(start_index, apay::ASYNC_ORDER_MAX_HASH_BATCH_SIZE)
                .map_err(|err| JsValue::from_str(&err.to_string()))?;

            let first_hash_index = params
                .hashes
                .first()
                .map(|h| h.hash_index)
                .ok_or_else(|| JsValue::from_str("apay_new: empty hash batch"))?;
            let last_hash_index = params.hashes.last().map(|h| h.hash_index).unwrap();

            // Sign the batch commitment (and optional Lightning-Address attestation) with the
            // node key, exactly as the native `/apay/new` route does.
            let keys_manager = Arc::clone(&g.keys_manager);
            let created_at = unix_now_secs();
            let expires_at = created_at.saturating_add(apay::APAY_BATCH_EXPIRY_SECS);
            let batch = apay::build_apay_batch_commitment(
                &local_node_hex,
                &host_node_hex,
                first_hash_index,
                &params.hashes,
                created_at,
                expires_at,
                |msg| keys_manager.sign_message(msg),
            )
            .map_err(|err| JsValue::from_str(&err.to_string()))?;
            params.batch = Some(batch);

            if let (Some(username), Some(domain)) = (username.as_deref(), domain.as_deref()) {
                let address_sig = apay::build_apay_address_attestation(
                    &local_node_hex,
                    domain,
                    username,
                    0,
                    |msg| keys_manager.sign_message(msg),
                )
                .map_err(|err| JsValue::from_str(&err.to_string()))?;
                params.address_sig = Some(address_sig);
            }

            let request_id = apay::new_request_id(&local_node_hex, &host_node_hex);
            let params_value = serde_json::to_value(&params)
                .map_err(|err| JsValue::from_str(&format!("apay_new: serialize params: {err}")))?;
            let response_rx = g.fork_custom_wire.queue_async_order_new(
                host_pubkey,
                request_id.clone(),
                params_value,
            );
            (
                request_id,
                response_rx,
                first_hash_index,
                last_hash_index,
                params.hashes,
            )
        };

        // Flush the queued message out to the peer (populates outbound frames for the JS pump).
        this.process_events()?;

        // Await the host response, bounded by a timeout. The JS websocket pump keeps calling
        // `process_events` as reply frames arrive, which completes `response_rx`.
        let response_value =
            match Self::apay_await_response(response_rx, apay::ASYNC_ORDER_RESPONSE_TIMEOUT_MS)
                .await
            {
                ApayAwaitOutcome::Response(Ok(value)) => value,
                ApayAwaitOutcome::Response(Err(err)) => {
                    return Err(JsValue::from_str(&err.to_string()));
                }
                ApayAwaitOutcome::PeerClosed => {
                    return Err(JsValue::from_str(
                        "apay_new: peer connection closed before the host replied",
                    ));
                }
                ApayAwaitOutcome::TimedOut => {
                    if let Some(g) = this.object_graph.borrow().as_ref() {
                        g.fork_custom_wire.forget_response(host_pubkey, &request_id);
                    }
                    return Err(JsValue::from_str(
                        "apay_new: timed out waiting for the host response",
                    ));
                }
            };

        let result: apay::AsyncOrderNewResultWire = serde_json::from_value(response_value)
            .map_err(|err| JsValue::from_str(&format!("apay_new: invalid host response: {err}")))?;

        // Persist the host-advertised next index so the next batch never reuses hashes.
        if let Some(g) = this.object_graph.borrow().as_ref() {
            apay::write_async_payments_next_hash_index(
                g.rgb_kv_store.as_ref(),
                &host_pubkey,
                result.next_index_expected,
            )
            .map_err(|err| JsValue::from_str(&err.to_string()))?;
        }

        Ok(apay::AsyncOrderNewResponse {
            request_id,
            host_node_id: apay::hex_str(&host_pubkey.serialize()),
            protocol_version: result.protocol_version,
            order_id: result.order_id,
            status: result.status,
            accepted_through_index: result.accepted_through_index,
            next_index_expected: result.next_index_expected,
            unused_hashes: result.unused_hashes,
            refill_batch_size: result.refill_batch_size,
            first_hash_index,
            last_hash_index,
            hashes,
        })
    }

    /// Recipient side of async payments: service any inbound `async_order.request_invoice` calls
    /// queued by the wire handler. Returns true if at least one was handled (so the caller flushes
    /// the queued responses out to the host). Each request mints a BOLT11 invoice for a
    /// deterministically-derived preimage and stashes that preimage so the inbound HTLC auto-claims.
    fn handle_pending_async_order_requests(&self, g: &LdkObjectGraph) -> bool {
        let requests = g.fork_custom_wire.take_inbound_requests();
        if requests.is_empty() {
            return false;
        }
        for (sender, request_id, params_value) in requests {
            match self.process_one_request_invoice(g, sender, params_value) {
                Ok(result) => {
                    let result_value =
                        serde_json::to_value(&result).unwrap_or(serde_json::Value::Null);
                    g.fork_custom_wire
                        .queue_async_order_result(sender, &request_id, result_value);
                    ldk_live_debug(&format!(
                        "[rln-wasm-sdk ldk-live] async_order.request_invoice served hash={}",
                        result.payment_hash
                    ));
                }
                Err(err) => {
                    g.fork_custom_wire
                        .queue_async_order_error(sender, &request_id, &err);
                    ldk_live_debug(&format!(
                        "[rln-wasm-sdk ldk-live] async_order.request_invoice error {}: {}",
                        err.code, err.message
                    ));
                }
            }
        }
        true
    }

    fn process_one_request_invoice(
        &self,
        g: &LdkObjectGraph,
        _sender: SecpPublicKey,
        params_value: serde_json::Value,
    ) -> Result<apay::AsyncOrderOutboundInvoiceResultWire, apay::JsonRpcErrorWire> {
        use apay::JsonRpcErrorWire;

        let params: apay::AsyncOrderRequestInvoiceParamsWire = serde_json::from_value(params_value)
            .map_err(|err| JsonRpcErrorWire::invalid_params(format!("invalid_params: {err}")))?;

        let hash_index = params
            .hash_index
            .parse::<u64>()
            .map_err(|_| JsonRpcErrorWire::invalid_params("invalid_hash_index"))?;
        if matches!(params.asset_amount, Some(0)) {
            return Err(JsonRpcErrorWire::invalid_params("invalid_asset_amount"));
        }
        if params.description_hash.trim().is_empty() {
            return Err(JsonRpcErrorWire::invalid_params("invalid_description_hash"));
        }
        let (contract_id, asset_amount) = match (&params.asset_id, params.asset_amount) {
            (Some(asset_id), Some(asset_amount)) => (
                Some(
                    asset_id
                        .parse::<ContractId>()
                        .map_err(|_| JsonRpcErrorWire::invalid_params("invalid_asset_id"))?,
                ),
                Some(asset_amount),
            ),
            (None, None) => (None, None),
            _ => return Err(JsonRpcErrorWire::invalid_params("incomplete_rgb_info")),
        };

        // Re-derive the preimage/hash for this index and confirm it matches what the host asked
        // for — otherwise we would mint an invoice we cannot settle.
        let local_node_id = g
            .keys_manager
            .get_node_id(Recipient::Node)
            .map_err(|_| JsonRpcErrorWire::internal_error("failed to derive local node id"))?;
        let seed = self.derive_seed32();
        let preimage_root = apay::AsyncPaymentsPreimageRoot::build_from_seed(
            &seed,
            network_for_runtime(&self.runtime_key),
            &local_node_id,
        )?;
        let (payment_preimage, payment_hash) = preimage_root.derive_hash_material(hash_index)?;
        let requested = params.payment_hash.trim().to_lowercase();
        if apay::hex_str(&payment_hash.0) != requested {
            return Err(JsonRpcErrorWire::application_error(
                1104,
                "invoice_hash_mismatch",
            ));
        }
        let hash_hex = apay::hex_str(&payment_hash.0);

        // Refuse to re-mint while a prior invoice for this hash is still live.
        if self
            .async_recipient_preimages
            .borrow()
            .contains_key(&hash_hex)
            || self
                .live_payments
                .borrow()
                .get(&hash_hex)
                .is_some_and(|p| matches!(p.status.as_str(), "pending" | "claimable" | "claiming"))
        {
            return Err(JsonRpcErrorWire::application_error(1105, "stale_flow"));
        }

        let description_hash = lightning_invoice::Sha256(
            params
                .description_hash
                .trim()
                .parse::<lightning::bitcoin::hashes::sha256::Hash>()
                .map_err(|_| JsonRpcErrorWire::invalid_params("invalid_description_hash"))?,
        );

        let invoice = g
            .channel_manager
            .create_bolt11_invoice(Bolt11InvoiceParameters {
                amount_msats: Some(params.amount_msat),
                description: Bolt11InvoiceDescription::Hash(description_hash),
                invoice_expiry_delta_secs: Some(params.invoice_expiry_sec),
                min_final_cltv_expiry_delta: Some(params.min_final_cltv_expiry_delta),
                payment_hash: Some(payment_hash),
                contract_id,
                asset_amount,
                ..Default::default()
            })
            .map_err(|err| {
                JsonRpcErrorWire::internal_error(format!("request_invoice_create_failed: {err:?}"))
            })?;

        // Stash the preimage so the inbound HTLC auto-claims, and track the inbound payment.
        self.async_recipient_preimages
            .borrow_mut()
            .insert(hash_hex.clone(), payment_preimage);
        let now = unix_now_secs();
        self.live_payments.borrow_mut().insert(
            hash_hex.clone(),
            LdkRuntimeLivePaymentData {
                payment_hash: hash_hex.clone(),
                status: "pending".to_string(),
                amt_msat: Some(params.amount_msat),
                asset_id: contract_id.map(|c| c.to_string()),
                asset_amount,
                inbound: true,
                preimage: None,
                expires_at: Some(now.saturating_add(params.invoice_expiry_sec as u64)),
                created_at: now,
                updated_at: now,
            },
        );

        Ok(apay::AsyncOrderOutboundInvoiceResultWire {
            payment_hash: hash_hex,
            bolt11: invoice.to_string(),
        })
    }

    /// Race the async-order response receiver against a timeout.
    async fn apay_await_response(
        rx: crate::rgb_ln_wire::AsyncOrderResponseReceiver,
        timeout_ms: u32,
    ) -> ApayAwaitOutcome {
        use futures::future::{select, Either};

        let timeout = apay_sleep_ms(timeout_ms);
        futures::pin_mut!(rx);
        futures::pin_mut!(timeout);
        match select(rx, timeout).await {
            Either::Left((Ok(response), _)) => ApayAwaitOutcome::Response(response),
            // Sender dropped (e.g. peer disconnected) without sending a response.
            Either::Left((Err(_canceled), _)) => ApayAwaitOutcome::PeerClosed,
            Either::Right(((), _)) => ApayAwaitOutcome::TimedOut,
        }
    }
    async fn drive_rgb_funding_work_impl(this: Rc<Self>) -> Result<(), JsValue> {
        loop {
            let work_item = this.pending_rgb_funding_work.borrow_mut().pop_front();
            let Some(item) = work_item else { break };
            // Keep a copy so a transient failure does not permanently strand the channel.
            // LDK emits `FundingGenerationReady` (the source of a `Prepare` item) exactly once,
            // so a dropped item would never be re-ingested and the channel would hang in
            // "opening" forever. Re-queue on error and let the next drive tick retry it once
            // the transient condition clears (e.g. BDK catching up to a freshly mined colored
            // UTXO after esplora's incremental sync briefly evicted it from its tx graph).
            let retry_item = item.clone();
            if let Err(e) = Self::process_funding_work_item(&this, item).await {
                // Refresh the shared RGB wallet's chain view before the next attempt. The
                // dominant transient failure is a freshly mined colored UTXO that esplora's
                // incremental sync briefly evicted from BDK's tx graph, surfacing as
                // "UTXO not found in the internal database" during funding. A re-sync re-adds
                // the now-confirmed transaction so the re-queued item can succeed.
                Self::resync_rgb_wallet_best_effort(&this).await;
                this.pending_rgb_funding_work
                    .borrow_mut()
                    .push_front(retry_item);
                return Err(e);
            }
        }
        // Phase F: after draining all queued work, flush any pending RGB transaction
        // fascia (covers commitment, HTLC, and cooperative-close coloring in addition
        // to funding). This is a no-op when there is nothing to flush.
        if let Some(channel_manager) = this
            .object_graph
            .borrow()
            .as_ref()
            .map(|g| Arc::clone(&g.channel_manager))
        {
            channel_manager
                .process_pending_rgb_transactions()
                .await
                .map_err(|e| {
                    JsValue::from_str(&format!("RGB pending transactions failed: {e:?}"))
                })?;
        }
        Ok(())
    }

    /// Processes a single queued RGB funding work item.
    ///
    /// On failure this returns an error before any durable, non-idempotent state is mutated
    /// (the fallible `prepare`/`complete`/`validate` calls run first), so the caller can safely
    /// re-queue and retry the same item on a later drive tick.
    async fn process_funding_work_item(
        this: &Rc<Self>,
        item: PendingRgbFundingWork,
    ) -> Result<(), JsValue> {
        match item {
            PendingRgbFundingWork::Prepare {
                user_channel_id,
                temporary_channel_id,
                counterparty_node_id,
                output_script_hex,
                channel_value_satoshis,
            } => {
                let intent = this
                    .pending_rgb_open_intents
                    .borrow()
                    .get(&user_channel_id)
                    .cloned();
                let Some(intent) = intent else { return Ok(()) };
                let (rgb_backend, channel_manager) = {
                    let graph = this.object_graph.borrow();
                    let g = graph.as_ref().ok_or_else(|| {
                        JsValue::from_str(sdk_contracts::ERR_LDK_OBJECT_GRAPH_NOT_INITIALIZED)
                    })?;
                    (Arc::clone(&g.rgb_backend), Arc::clone(&g.channel_manager))
                };
                let output_script =
                    bitcoin::ScriptBuf::from_bytes(hex::decode(&output_script_hex).map_err(
                        |e| JsValue::from_str(&format!("invalid output_script_hex: {e}")),
                    )?);
                let request = lightning::rgb_utils::RgbFundingTransferRequest {
                    contract_id: intent.contract_id,
                    schema: intent.schema,
                    amount: intent.asset_amount,
                    output_script,
                    channel_value_satoshis,
                    consignment_endpoint: intent.consignment_endpoint,
                    network: network_for_runtime(&this.runtime_key),
                    fee_rate: intent.fee_rate,
                    min_confirmations: intent.min_confirmations,
                };
                let prepared = rgb_backend
                    .prepare_funding_transfer(request)
                    .await
                    .map_err(|e| {
                        JsValue::from_str(&format!("RGB prepare funding failed: {e:?}"))
                    })?;
                let temp_hex = format!("{temporary_channel_id}");
                this.pending_rgb_prepare_results
                    .borrow_mut()
                    .insert(temp_hex, prepared.signed_psbt);
                channel_manager
                    .funding_transaction_generated(
                        temporary_channel_id,
                        counterparty_node_id,
                        prepared.transaction,
                    )
                    .map_err(|e| {
                        JsValue::from_str(&format!("funding_transaction_generated failed: {e:?}"))
                    })?;
            }
            PendingRgbFundingWork::Complete {
                temporary_channel_id_hex,
                signed_psbt,
            } => {
                let rgb_backend = {
                    let graph = this.object_graph.borrow();
                    let g = graph.as_ref().ok_or_else(|| {
                        JsValue::from_str(sdk_contracts::ERR_LDK_OBJECT_GRAPH_NOT_INITIALIZED)
                    })?;
                    Arc::clone(&g.rgb_backend)
                };
                let txid = rgb_backend
                    .complete_funding_transfer(signed_psbt)
                    .await
                    .map_err(|e| {
                        JsValue::from_str(&format!("RGB complete funding failed: {e:?}"))
                    })?;
                this.pending_rgb_prepare_results
                    .borrow_mut()
                    .remove(&temporary_channel_id_hex);
                ldk_live_debug(&format!(
                    "[rln-wasm-sdk ldk-live] drive_rgb_funding_work: txid={txid}"
                ));
            }
            PendingRgbFundingWork::ValidateFunding {
                temporary_channel_id,
            } => {
                let channel_manager = {
                    let graph = this.object_graph.borrow();
                    let g = graph.as_ref().ok_or_else(|| {
                        JsValue::from_str(sdk_contracts::ERR_LDK_OBJECT_GRAPH_NOT_INITIALIZED)
                    })?;
                    Arc::clone(&g.channel_manager)
                };
                channel_manager
                    .process_pending_rgb_funding_validation(temporary_channel_id)
                    .await
                    .map_err(|e| {
                        JsValue::from_str(&format!("RGB funding validation failed: {e:?}"))
                    })?;
            }
            PendingRgbFundingWork::ProcessPendingTransactions => {
                // handled by the unconditional sweep in drive_rgb_funding_work_impl
            }
        }
        Ok(())
    }

    /// Re-syncs the shared RGB wallet's BDK chain view. Best-effort: any error is swallowed so
    /// the caller's own (re-queued) error remains the surfaced failure.
    async fn resync_rgb_wallet_best_effort(this: &Rc<Self>) {
        let wallet_rc =
            RGB_WALLET_REGISTRY.with(|reg| reg.borrow().get(&this.runtime_key).cloned());
        let Some(wallet_rc) = wallet_rc else { return };
        let online = wallet_rc.borrow().get_online();
        let Some(online) = online else { return };
        let _ = wallet_rc.borrow_mut().sync(online).await;
    }

    pub(crate) async fn run_process_pending_rgb_transactions(&self) -> Result<(), JsValue> {
        let channel_manager = {
            let graph = self.object_graph.borrow();
            let g = graph.as_ref().ok_or_else(|| {
                JsValue::from_str(sdk_contracts::ERR_LDK_OBJECT_GRAPH_NOT_INITIALIZED)
            })?;
            Arc::clone(&g.channel_manager)
        };
        channel_manager
            .process_pending_rgb_transactions()
            .await
            .map_err(|e| JsValue::from_str(&format!("RGB pending transactions failed: {e:?}")))
    }
}

pub fn create_wasm_ldk_live_backend(
    runtime_key: String,
    node_seed32: Option<[u8; 32]>,
) -> Result<Rc<dyn LdkLiveBackend>, JsValue> {
    if runtime_key.trim().is_empty() {
        return Err(JsValue::from_str("runtime_key cannot be empty"));
    }
    let backend = Rc::new(WasmLdkLiveBackend::new(runtime_key, node_seed32));
    *backend.self_weak.borrow_mut() = Rc::downgrade(&backend);
    Ok(backend)
}

/// Outcome of awaiting an `async_order` response receiver against a timeout.
enum ApayAwaitOutcome {
    Response(Result<serde_json::Value, crate::apay::JsonRpcErrorWire>),
    PeerClosed,
    TimedOut,
}

/// Resolve after `ms` milliseconds. Browser path uses `setTimeout`; native test path sleeps.
async fn apay_sleep_ms(ms: u32) {
    #[cfg(target_arch = "wasm32")]
    {
        let promise = js_sys::Promise::new(&mut |resolve, _reject| {
            if let Some(window) = web_sys::window() {
                let _ = window
                    .set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, ms as i32);
            } else {
                let _ = resolve.call0(&JsValue::NULL);
            }
        });
        let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        std::thread::sleep(std::time::Duration::from_millis(ms as u64));
    }
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
