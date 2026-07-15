use std::cell::Cell;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::str::FromStr;
use std::time::Duration;

use bitcoin_hashes::sha256::Hash as Sha256;
use bitcoin_hashes::Hash as _;
#[cfg(target_arch = "wasm32")]
use gloo_net::http::Request;
use lightning_invoice::Bolt11Invoice;
use lightning_invoice::Currency;
use lightning_invoice::InvoiceBuilder;
use lightning_invoice::PaymentSecret;
use secp256k1::{Message as SecpMessage, PublicKey as SecpPublicKey, Secp256k1, SecretKey};
use serde::{Deserialize, Serialize};
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::spawn_local;
#[cfg(target_arch = "wasm32")]
use wasm_bindgen_futures::JsFuture;

use crate::chain_sync::{RlnWasmChainSyncStatusData, WasmChainSyncDriver};
use crate::ldk_event_applier::{
    ensure_manual_event_ingestion_allowed, ensure_manual_status_update_allowed,
};
use crate::ldk_runtime::{
    LdkRuntimeChannelStateData, LdkRuntimeComponentsStatusData, LdkRuntimeFundingTxSubmissionData,
    LdkRuntimeManager, LdkRuntimeOpenChannelRequestData, LdkRuntimePaymentStateData,
    LdkRuntimePeerStateData, LdkRuntimeStatusData, LdkRuntimeVirtualChannelSessionData,
    LdkRuntimeVirtualChannelSessionStatusData,
};
use crate::ln_runtime_native::{NativeLnRuntimeCore, NativeLnRuntimeCoreStatusData};
use crate::ln_transport::RlnWasmLnSocketConnectOptionsData;
use crate::peer_session::{
    clear_rln_ldk_peer_manager_hooks, has_peer_manager_hooks, has_peer_manager_hooks_v2,
    install_rln_ldk_peer_manager_hooks, RlnLdkPeerManagerHooks, RlnWasmPeerSession,
    RlnWasmRustPeerManagerBridge,
};
use crate::runtime_store::{browser_persistent_state_store, RuntimeStateStore};
use crate::wasm_node_persistence::{JsonRuntimeStateStore, RuntimeScopeKeys};
use crate::{
    derive_cfa_ticker, WasmAssetCfaData, WasmIssueAssetCfaRequest, WasmIssueAssetIfaRequest,
    WasmIssueAssetNiaRequest,
};

#[inline]
fn wasm_debug(msg: &str) {
    // Debug-only logging. The WASM SDK is often driven in a browser UI where
    // unconditional console logging is noisy and can meaningfully slow down
    // E2E runs. Keep logs available for local debug builds.
    #[cfg(all(target_arch = "wasm32", debug_assertions))]
    web_sys::console::log_1(&JsValue::from_str(msg));
    #[cfg(not(all(target_arch = "wasm32", debug_assertions)))]
    let _ = msg;
}

const SDK_HTLC_MIN_MSAT: u64 = 3_000_000;
/// Lower HTLC floor permitted over virtual (`trusted_no_broadcast`) channels, matching
/// native `VIRTUAL_HTLC_MIN_MSAT`. Regular channels keep the `SDK_HTLC_MIN_MSAT` floor.
const VIRTUAL_HTLC_MIN_MSAT: u64 = 1_000;
const SDK_INVOICE_MIN_MSAT: u64 = SDK_HTLC_MIN_MSAT;
const SDK_OPENRGBCHANNEL_MIN_SAT: u64 = SDK_HTLC_MIN_MSAT / 1000 * 10 + 10;
const SDK_OPENCHANNEL_MIN_SAT: u64 = 5_506;
const SDK_OPENCHANNEL_MAX_SAT: u64 = 16_777_215;
const SDK_OPENCHANNEL_MIN_RGB_AMT: u64 = 1;
const SDK_VIRTUAL_OPEN_MODE_TRUSTED_NO_BROADCAST: &str = "trusted_no_broadcast";
const SDK_INVOICE_TYPE_AUTO_CLAIM: &str = "auto_claim";
const SDK_INVOICE_TYPE_HODL: &str = "hodl";

#[derive(Clone, Debug, Serialize)]
pub struct RlnWasmNodePeerData {
    pub pubkey: String,
    pub peer_addr: String,
    pub started: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct RlnWasmNodeInfoData {
    pub runtime: String,
    pub ldk_over_websocket: bool,
    pub num_peers: usize,
    pub num_channels: usize,
    pub num_usable_channels: usize,
}

#[derive(Clone, Debug, Serialize)]
pub struct RlnWasmNodeNetworkInfoData {
    pub network: String,
    pub height: u32,
}

#[derive(Clone, Debug, Serialize)]
pub struct RlnWasmNodeChannelData {
    pub temporary_channel_id: String,
    pub channel_id: String,
    pub peer_pubkey: String,
    pub status: String,
    pub ready: bool,
    pub is_usable: bool,
    pub public: bool,
    pub capacity_sat: u64,
    pub asset_id: Option<String>,
    pub asset_local_amount: Option<u64>,
    pub virtual_open_mode: Option<String>,
    /// This node's spendable outbound BTC capacity, in msat.
    pub outbound_msat: u64,
    /// The largest single outbound HTLC this node can currently send, in msat. Callers draining a
    /// channel (e.g. before a virtual-channel close) should pay in chunks bounded by this value.
    pub next_outbound_htlc_limit_msat: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct RlnWasmNodePaymentData {
    pub amt_msat: Option<u64>,
    pub asset_amount: Option<u64>,
    pub asset_id: Option<String>,
    pub payment_hash: String,
    pub inbound: bool,
    pub status: String,
    pub invoice_type: Option<String>,
    pub preimage: Option<String>,
    pub created_at: u64,
    pub updated_at: u64,
    pub payee_pubkey: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RlnWasmNodeRgbLnTransferData {
    pub payment_hash: String,
    pub inbound: bool,
    pub asset_id: String,
    pub asset_amount: u64,
    pub status: String,
    pub created_at: u64,
    pub updated_at: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct RlnWasmNodeSendPaymentResult {
    pub payment_id: String,
    pub payment_hash: Option<String>,
    pub payment_secret: Option<String>,
    pub status: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct RlnWasmNodeKeysendResult {
    pub payment_hash: String,
    pub payment_preimage: String,
    pub status: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct RlnWasmNodeDecodeLnInvoiceData {
    pub amt_msat: Option<u64>,
    pub expiry_sec: u64,
    pub timestamp: u64,
    pub asset_id: Option<String>,
    pub asset_amount: Option<u64>,
    pub payment_hash: String,
    pub payment_secret: String,
    pub payee_pubkey: Option<String>,
    pub network: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct RlnWasmNodeInvoiceStatusData {
    pub status: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RlnWasmNodeRelaySessionAuthData {
    pub relay_auth_token: String,
    pub relay_node_id: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct RlnWasmNodeClaimHodlInvoiceData {
    pub changed: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct RlnWasmNodeSignMessageData {
    pub signed_message: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct RlnWasmNodeCreateLnInvoiceData {
    pub invoice: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RlnWasmNodeRuntimeEventData {
    pub seq: u64,
    pub source: String,
    pub event_kind: String,
    pub payload_hex: String,
    pub payment_hash: Option<String>,
    pub status: Option<String>,
    pub applied: bool,
    pub error: Option<String>,
    pub received_at: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct RlnWasmNodeRuntimeQueueProcessData {
    pub drained: usize,
}

#[derive(Clone, Debug, Deserialize)]
struct PaymentStatusEvent {
    payment_hash: String,
    status: String,
}

#[derive(Clone, Copy)]
#[allow(dead_code)]
enum RuntimeEventApplyMode {
    StrictPaymentStatus,
    TolerantTransport,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "event")]
enum RuntimeTransportEvent {
    #[serde(rename = "peer_disconnected")]
    PeerDisconnected { peer_pubkey: String },
    #[serde(rename = "peer_reconnected")]
    PeerReconnected { peer_pubkey: String },
    #[serde(rename = "channel_closed")]
    ChannelClosed { channel_id: String },
    #[serde(rename = "channel_usable")]
    ChannelUsable { channel_id: String },
    #[serde(rename = "channel_unusable")]
    ChannelUnusable { channel_id: String },
}

impl RuntimeTransportEvent {
    fn event_kind(&self) -> &'static str {
        match self {
            Self::PeerDisconnected { .. } => "peer_disconnected",
            Self::PeerReconnected { .. } => "peer_reconnected",
            Self::ChannelClosed { .. } => "channel_closed",
            Self::ChannelUsable { .. } => "channel_usable",
            Self::ChannelUnusable { .. } => "channel_unusable",
        }
    }
}

#[derive(Clone, Debug, Serialize)]
struct RuntimeTransportEventApplyData {
    event_kind: String,
    applied: bool,
}

struct PeerEntry {
    peer_addr: String,
    session: Rc<RlnWasmPeerSession>,
}

struct ChannelEntry {
    temporary_channel_id: String,
    data: RlnWasmNodeChannelData,
}

struct PaymentEntry {
    data: RlnWasmNodePaymentData,
}

#[derive(Clone)]
struct TrustedVirtualScopeChannelData {
    peer_pubkey: String,
    local_node_pubkey: Option<String>,
}

#[derive(Clone, Debug)]
struct TrustedVirtualAuthoritativeSettlementData {
    payment_hash: String,
    from_pubkey: String,
    to_pubkey: String,
    amt_msat: Option<u64>,
    asset_id: Option<String>,
    asset_amount: Option<u64>,
    created_at: u64,
}

enum PendingPeerHookEvent {
    Payload(String),
    SocketDisconnected,
    Error(String),
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct RuntimeEventLogSnapshot {
    events: Vec<RlnWasmNodeRuntimeEventData>,
    next_seq: u64,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct RuntimeRgbLnTransferSnapshot {
    transfers: Vec<RlnWasmNodeRgbLnTransferData>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct RuntimePeerSessionSnapshot {
    sessions: Vec<RuntimePeerSessionEntryData>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct RuntimePeerSessionEntryData {
    session_key: String,
    peer_pubkey: String,
    peer_addr: String,
}

#[derive(Clone, Debug, Serialize)]
struct RuntimeReconnectPeersResultData {
    attempted: usize,
    connected: usize,
    failed: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
struct RuntimeReconnectManagerStatusData {
    running: bool,
    current_backoff_ms: u32,
}

#[derive(Clone, Debug, Serialize)]
struct RlnWasmNodeAutoDriveStatusData {
    running: bool,
    interval_ms: u32,
}

#[derive(Clone, Debug, Deserialize)]
struct WasmFundingTxSubmissionRequest {
    temporary_channel_id: String,
    counterparty_node_id: String,
    funding_tx_hex: String,
}

thread_local! {
    static RUNTIME_EVENT_LOG_STORAGE: RefCell<HashMap<String, RuntimeEventLogSnapshot>> =
        RefCell::new(HashMap::new());
    static RUNTIME_RGB_LN_TRANSFER_STORAGE: RefCell<HashMap<String, RuntimeRgbLnTransferSnapshot>> =
        RefCell::new(HashMap::new());
    static RUNTIME_PEER_SESSION_STORAGE: RefCell<HashMap<String, RuntimePeerSessionSnapshot>> =
        RefCell::new(HashMap::new());
    static TRUSTED_VIRTUAL_CHANNEL_SCOPE_STORAGE: RefCell<HashMap<String, HashMap<String, TrustedVirtualScopeChannelData>>> =
        RefCell::new(HashMap::new());
    static TRUSTED_VIRTUAL_PEER_LINK_STORAGE: RefCell<HashMap<String, u64>> =
        RefCell::new(HashMap::new());
    static TRUSTED_VIRTUAL_AUTHORITATIVE_SETTLEMENT_STORAGE: RefCell<Vec<TrustedVirtualAuthoritativeSettlementData>> =
        const { RefCell::new(Vec::new()) };
    static NODE_PUBKEY_RUNTIME_SCOPE_INDEX: RefCell<HashMap<String, HashSet<String>>> =
        RefCell::new(HashMap::new());
    static KNOWN_RUNTIME_SCOPE_KEYS: RefCell<HashSet<String>> =
        RefCell::new(HashSet::new());
    static NODE_INSTANCE_NONCE_SEQ: RefCell<u64> = const { RefCell::new(0) };
}

const RUNTIME_EVENT_LOG_PERSIST_WINDOW: usize = 512;
const RECONNECT_MANAGER_INITIAL_DELAY_MS: u32 = 500;
const RECONNECT_MANAGER_MAX_DELAY_MS: u32 = 15_000;
/// Default cadence of the autonomous drive loop (`autoDriveStart`) when no interval is supplied.
const AUTO_DRIVE_DEFAULT_INTERVAL_MS: u32 = 1_000;
/// Lower bound on the autonomous drive interval, so a caller cannot spin the loop into a tight
/// busy-loop that starves the single-threaded wasm executor.
const AUTO_DRIVE_MIN_INTERVAL_MS: u32 = 200;

#[cfg(test)]
#[path = "tests/ln_node_test_utils.rs"]
pub(crate) mod test_utils;

#[wasm_bindgen]
pub struct RlnWasmNode {
    proxy_url: String,
    node_runtime_id: Option<String>,
    persistence_keys: RuntimeScopeKeys,
    bridge: RlnWasmRustPeerManagerBridge,
    ldk_runtime: Rc<dyn LdkRuntimeManager>,
    runtime_core: NativeLnRuntimeCore,
    chain_sync: WasmChainSyncDriver,
    peers: Rc<RefCell<HashMap<String, PeerEntry>>>,
    channels: Rc<RefCell<HashMap<String, ChannelEntry>>>,
    payments: Rc<RefCell<HashMap<String, PaymentEntry>>>,
    pending_peer_hook_events: Rc<RefCell<Vec<PendingPeerHookEvent>>>,
    runtime_events: Rc<RefCell<Vec<RlnWasmNodeRuntimeEventData>>>,
    rgb_ln_transfers: Rc<RefCell<HashMap<String, RlnWasmNodeRgbLnTransferData>>>,
    next_channel_seq: RefCell<u64>,
    next_payment_seq: RefCell<u64>,
    node_instance_nonce: u64,
    next_runtime_event_seq: Rc<RefCell<u64>>,
    network: RefCell<String>,
    /// Whether the network was explicitly selected at construction (native-style `--network`).
    /// When `true`, `attach_wallet_shared` validates the wallet's network against it and errors on
    /// mismatch. When `false` (bare `new`/facade path), the node adopts the attached wallet's network.
    network_configured: Cell<bool>,
    wallet: RefCell<Option<std::rc::Rc<RefCell<rgb_lib_wasm::Wallet>>>>,
    relay_session_auth: RefCell<Option<RlnWasmNodeRelaySessionAuthData>>,
    enable_virtual_channels_v0: RefCell<bool>,
    reconnect_manager_running: Rc<RefCell<bool>>,
    reconnect_manager_backoff_ms: Rc<RefCell<u32>>,
    auto_drive_running: Rc<RefCell<bool>>,
    auto_drive_interval_ms: Rc<RefCell<u32>>,
}

#[wasm_bindgen]
impl RlnWasmNode {
    fn next_node_instance_nonce() -> u64 {
        NODE_INSTANCE_NONCE_SEQ.with(|seq| {
            let mut seq = seq.borrow_mut();
            *seq = seq.saturating_add(1);
            *seq
        })
    }

    fn ensure_runtime_ready(&self) -> Result<(), JsValue> {
        crate::ensure_sdk_node_runtime_allowed()?;
        self.runtime_core.ensure_started();
        self.ldk_runtime.ensure_started()?;
        self.ldk_runtime.virtual_channel_reconcile_sessions();
        self.ldk_runtime
            .set_identity_stable(self.identity_stable_for_channel_operations());
        Ok(())
    }

    fn has_stable_runtime_id(&self) -> bool {
        self.node_runtime_id
            .as_deref()
            .map(|id| !id.trim().is_empty())
            .unwrap_or(false)
            || self
                .persistence_keys
                .runtime_scope_key
                .contains("#runtime:")
    }

    fn has_stable_node_seed(&self) -> bool {
        crate::sdk_node_identity_seed()
            .map(|seed| !seed.trim().is_empty())
            .unwrap_or(false)
    }

    fn identity_stable_for_channel_operations(&self) -> bool {
        self.has_stable_runtime_id() && self.has_stable_node_seed()
    }

    fn ensure_stable_identity_for_channel_operations(&self) -> Result<(), JsValue> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            return Ok(());
        }

        #[cfg(target_arch = "wasm32")]
        {
            if !self.has_stable_runtime_id() {
                return Err(JsValue::from_str(
                    sdk_contracts::ERR_NODE_RUNTIME_ID_REQUIRED,
                ));
            }
            if !self.has_stable_node_seed() {
                return Err(JsValue::from_str(
                    sdk_contracts::ERR_NODE_IDENTITY_SEED_REQUIRED,
                ));
            }
            Ok(())
        }
    }

    /// Bare constructor. The network is left unset and adopted from the first attached wallet
    /// (regtest defaults apply until then). For an explicit, native-style network selection use
    /// [`new_with_node_runtime_id`](Self::new_with_node_runtime_id).
    #[wasm_bindgen(constructor)]
    pub fn new(proxy_url: String) -> Result<RlnWasmNode, JsValue> {
        Self::new_with_runtime_id_opt(proxy_url, None, None)
    }

    /// Construct a node with an explicit Bitcoin network, mirroring the native node's `--network`.
    /// The `network` string is one of `"mainnet" | "testnet" | "testnet4" | "signet" | "regtest"`
    /// (case-insensitive). The node owns this network as its single source of truth: it drives the
    /// LDK `ChannelManager`/`NetworkGraph` (and therefore the `Init` handshake chain), and
    /// `attachWallet` will reject a wallet created on a different network.
    #[wasm_bindgen(js_name = newWithNodeRuntimeId)]
    pub fn new_with_node_runtime_id(
        proxy_url: String,
        node_runtime_id: String,
        network: String,
    ) -> Result<RlnWasmNode, JsValue> {
        let network = crate::WasmRlnNetwork::parse(&network)?;
        Self::new_with_runtime_id_opt(proxy_url, Some(node_runtime_id), Some(network))
    }

    pub(crate) fn new_with_runtime_id_opt(
        proxy_url: String,
        node_runtime_id: Option<String>,
        network: Option<crate::WasmRlnNetwork>,
    ) -> Result<RlnWasmNode, JsValue> {
        if proxy_url.trim().is_empty() {
            return Err(JsValue::from_str(sdk_contracts::ERR_PROXY_URL_EMPTY));
        }
        let normalized_runtime_id = normalize_node_runtime_id(node_runtime_id)?;
        let runtime_scope_key =
            runtime_scope_key(proxy_url.trim(), normalized_runtime_id.as_deref());
        let persistence_keys = RuntimeScopeKeys::from_runtime_scope_key(runtime_scope_key.clone());
        KNOWN_RUNTIME_SCOPE_KEYS.with(|keys| {
            keys.borrow_mut().insert(runtime_scope_key.clone());
        });
        let runtime_event_snapshot =
            load_runtime_event_log_snapshot(&persistence_keys.runtime_events_storage_key);
        let runtime_events = runtime_event_snapshot
            .as_ref()
            .map(|snapshot| snapshot.events.clone())
            .unwrap_or_default();
        let next_runtime_event_seq = runtime_event_snapshot
            .as_ref()
            .map(|snapshot| snapshot.next_seq)
            .unwrap_or(0);
        let rgb_ln_transfer_snapshot =
            load_runtime_rgb_ln_transfer_snapshot(&persistence_keys.rgb_ln_transfers_storage_key);
        let rgb_ln_transfers = rgb_ln_transfer_snapshot
            .as_ref()
            .map(|snapshot| {
                snapshot
                    .transfers
                    .iter()
                    .map(|entry| (entry.payment_hash.clone(), entry.clone()))
                    .collect::<HashMap<_, _>>()
            })
            .unwrap_or_default();
        let ldk_runtime = crate::ldk_runtime::ldk_runtime_manager(
            persistence_keys.ldk_manager_registry_key.clone(),
        )?;
        let runtime_core =
            NativeLnRuntimeCore::new(persistence_keys.ldk_manager_registry_key.clone());
        // When a network is explicitly selected it becomes the node's single source of truth (like
        // the native node's `--network`); otherwise fall back to the historical `regtest` default and
        // let the first attached wallet supply the network.
        let configured_rgb_network = network.map(|n| n.as_rgb());
        let default_network_label = configured_rgb_network
            .map(rgb_network_label)
            .unwrap_or("regtest");
        let chain_sync = WasmChainSyncDriver::new(
            persistence_keys.ldk_manager_registry_key.clone(),
            default_network_label.to_string(),
        )?;
        let restored_network = if configured_rgb_network.is_some() {
            default_network_label.to_string()
        } else {
            chain_sync.status().network
        };
        let enable_virtual_channels_v0 =
            load_virtual_channels_v0_flag(&persistence_keys.virtual_channels_v0_storage_key)
                .unwrap_or_else(crate::sdk_default_enable_virtual_channels_v0);
        let node = Self {
            ldk_runtime,
            runtime_core,
            chain_sync,
            proxy_url,
            node_runtime_id: normalized_runtime_id,
            persistence_keys,
            bridge: RlnWasmRustPeerManagerBridge::new(None)?,
            peers: Rc::new(RefCell::new(HashMap::new())),
            channels: Rc::new(RefCell::new(HashMap::new())),
            payments: Rc::new(RefCell::new(HashMap::new())),
            pending_peer_hook_events: Rc::new(RefCell::new(Vec::new())),
            runtime_events: Rc::new(RefCell::new(runtime_events)),
            rgb_ln_transfers: Rc::new(RefCell::new(rgb_ln_transfers)),
            next_channel_seq: RefCell::new(0),
            next_payment_seq: RefCell::new(0),
            node_instance_nonce: Self::next_node_instance_nonce(),
            next_runtime_event_seq: Rc::new(RefCell::new(next_runtime_event_seq)),
            network: RefCell::new(restored_network),
            network_configured: Cell::new(configured_rgb_network.is_some()),
            wallet: RefCell::new(None),
            relay_session_auth: RefCell::new(None),
            enable_virtual_channels_v0: RefCell::new(enable_virtual_channels_v0),
            reconnect_manager_running: Rc::new(RefCell::new(false)),
            reconnect_manager_backoff_ms: Rc::new(RefCell::new(RECONNECT_MANAGER_INITIAL_DELAY_MS)),
            auto_drive_running: Rc::new(RefCell::new(false)),
            auto_drive_interval_ms: Rc::new(RefCell::new(AUTO_DRIVE_DEFAULT_INTERVAL_MS)),
        };
        // For an explicitly-selected network, register it with the LDK backend now — before the
        // object graph (ChannelManager/NetworkGraph) is first built — so the handshake advertises the
        // configured chain even if a wallet is never attached.
        if let Some(rgb_network) = configured_rgb_network {
            crate::ldk_live_backend::set_network_for_runtime(
                &node.persistence_keys.ldk_manager_registry_key,
                crate::ldk_live_backend::rgb_network_to_bitcoin_network(rgb_network),
            );
        }
        // Keep live LDK backend identity aligned with node_signing_identity pubkey.
        let (node_secret_key, _) = node.node_signing_identity()?;
        node.ldk_runtime
            .set_live_node_seed_hex(hex::encode(node_secret_key.secret_bytes()))?;
        node.ldk_runtime
            .set_identity_stable(node.identity_stable_for_channel_operations());
        // Native-only interop default: wire real runtime peer-manager hooks on node creation,
        // so connectPeer/openChannel never depends on scaffold bridge callbacks.
        node.install_auto_peer_manager_hooks();
        node.register_runtime_scope_for_local_pubkey();
        Ok(node)
    }

    fn with_attached_wallet<T>(
        &self,
        f: impl FnOnce(&mut rgb_lib_wasm::Wallet) -> Result<T, JsValue>,
    ) -> Result<T, JsValue> {
        if self.wallet.borrow().is_none() {
            crate::try_attach_default_wallet_to_node(self);
        }
        let wallet_ref = self
            .wallet
            .borrow()
            .clone()
            .ok_or_else(|| JsValue::from_str(sdk_contracts::ERR_WALLET_NOT_ATTACHED))?;
        let mut wallet = wallet_ref.borrow_mut();
        f(&mut wallet)
    }

    #[wasm_bindgen(js_name = attachWallet)]
    pub fn attach_wallet(&self, wallet: &crate::RlnWasmWallet) -> Result<(), JsValue> {
        self.attach_wallet_shared(Rc::clone(&wallet.inner))
    }

    pub(crate) fn attach_wallet_shared(
        &self,
        wallet: Rc<RefCell<rgb_lib_wasm::Wallet>>,
    ) -> Result<(), JsValue> {
        // Reconcile the wallet's network with the node's.
        //
        // - Network selected explicitly at construction (`network_configured`): the node owns the
        //   network (like the native `--network`), so the wallet MUST match. A mismatch is a
        //   configuration error and is rejected up front — mirroring the native node's
        //   `NetworkMismatch`, and preventing the LDK side from advertising a chain the wallet can't
        //   actually operate on.
        // - Otherwise: adopt the wallet's network as the node's, and propagate it to the LDK backend
        //   (so the `ChannelManager`/`NetworkGraph`, and thus the `networks` field of the `Init`
        //   handshake, advertise the right chain) and to the node's own network string (invoice
        //   currency, chain-sync status).
        let bitcoin_network = wallet.borrow().get_wallet_data().bitcoin_network;
        let wallet_label = rgb_network_label(bitcoin_network);
        if self.network_configured.get() {
            let node_label = self.network.borrow().clone();
            if wallet_label != node_label {
                return Err(JsValue::from_str(&format!(
                    "wallet network ({wallet_label}) does not match the node's configured network ({node_label})"
                )));
            }
        } else {
            *self.network.borrow_mut() = wallet_label.to_string();
            let _ = self.chain_sync.set_network(wallet_label);
        }
        crate::ldk_live_backend::set_network_for_runtime(
            &self.persistence_keys.ldk_manager_registry_key,
            crate::ldk_live_backend::rgb_network_to_bitcoin_network(bitcoin_network),
        );
        crate::ldk_live_backend::register_rgb_wallet_for_runtime(
            &self.persistence_keys.ldk_manager_registry_key,
            Rc::clone(&wallet),
        );
        // Seed the live-backend virtual-channels flag registry with this node's current value
        // (default/persisted) before the LDK object graph is first built, so the
        // `Event::OpenChannelRequest` handler sees the right gate even if the setter is never called.
        crate::ldk_live_backend::set_virtual_channels_v0_for_runtime(
            &self.persistence_keys.ldk_manager_registry_key,
            *self.enable_virtual_channels_v0.borrow(),
        );
        *self.wallet.borrow_mut() = Some(wallet);
        Ok(())
    }

    #[wasm_bindgen(js_name = setRelaySessionAuth)]
    pub fn set_relay_session_auth(
        &self,
        relay_auth_token: Option<String>,
        relay_node_id: Option<String>,
    ) -> Result<(), JsValue> {
        match (relay_auth_token, relay_node_id) {
            (None, None) => {
                self.relay_session_auth.borrow_mut().take();
                Ok(())
            }
            (Some(token), Some(node_id)) => {
                let token = token.trim().to_string();
                let node_id = node_id.trim().to_string();
                if token.is_empty() {
                    return Err(JsValue::from_str(sdk_contracts::ERR_RELAY_AUTH_TOKEN_EMPTY));
                }
                if node_id.is_empty() {
                    return Err(JsValue::from_str(sdk_contracts::ERR_RELAY_NODE_ID_EMPTY));
                }
                if SecpPublicKey::from_str(&node_id).is_err() {
                    return Err(JsValue::from_str(sdk_contracts::ERR_RELAY_NODE_ID_INVALID));
                }
                self.relay_session_auth
                    .borrow_mut()
                    .replace(RlnWasmNodeRelaySessionAuthData {
                        relay_auth_token: token,
                        relay_node_id: node_id,
                    });
                Ok(())
            }
            _ => Err(JsValue::from_str(
                sdk_contracts::ERR_RELAY_AUTH_TOKEN_NODE_ID_TOGETHER,
            )),
        }
    }

    #[wasm_bindgen(js_name = relaySessionAuthValue)]
    pub fn relay_session_auth_value(&self) -> Result<JsValue, JsValue> {
        match self.relay_session_auth.borrow().as_ref().cloned() {
            Some(data) => crate::js_obj(&data),
            None => Ok(JsValue::NULL),
        }
    }

    #[wasm_bindgen(js_name = relaySessionAuthJson)]
    pub fn relay_session_auth_json(&self) -> Result<String, JsValue> {
        let value = self.relay_session_auth_value()?;
        if value.is_null() || value.is_undefined() {
            return Ok("null".to_string());
        }
        let parsed: serde_json::Value = crate::js_from(value)?;
        crate::js_to_json(&parsed)
    }

    #[wasm_bindgen(js_name = setEnableVirtualChannelsV0)]
    pub fn set_enable_virtual_channels_v0(&self, enabled: bool) {
        *self.enable_virtual_channels_v0.borrow_mut() = enabled;
        persist_virtual_channels_v0_flag(
            &self.persistence_keys.virtual_channels_v0_storage_key,
            enabled,
        );
        // Mirror the flag into the live-backend registry so the `Event::OpenChannelRequest` handler
        // (which decides whether to accept inbound scid-privacy channels as 0-conf virtual channels)
        // can read it by runtime key.
        crate::ldk_live_backend::set_virtual_channels_v0_for_runtime(
            &self.persistence_keys.ldk_manager_registry_key,
            enabled,
        );
    }

    #[wasm_bindgen(js_name = enableVirtualChannelsV0Value)]
    pub fn enable_virtual_channels_v0_value(&self) -> Result<JsValue, JsValue> {
        crate::js_obj(&serde_json::json!({
            "enabled": *self.enable_virtual_channels_v0.borrow()
        }))
    }

    #[wasm_bindgen(js_name = enableVirtualChannelsV0Json)]
    pub fn enable_virtual_channels_v0_json(&self) -> Result<String, JsValue> {
        let value = self.enable_virtual_channels_v0_value()?;
        let parsed: serde_json::Value = crate::js_from(value)?;
        crate::js_to_json(&parsed)
    }

    #[wasm_bindgen(js_name = issueAssetNiaValue)]
    pub fn issue_asset_nia_value(&self, request_js: JsValue) -> Result<JsValue, JsValue> {
        self.ensure_runtime_ready()?;
        let request: WasmIssueAssetNiaRequest = serde_wasm_bindgen::from_value(request_js)
            .map_err(|e| JsValue::from_str(&format!("Invalid issue_asset_nia request: {e}")))?;
        if request.amounts.is_empty() {
            return Err(JsValue::from_str(sdk_contracts::ERR_AMOUNTS_EMPTY));
        }
        if request.ticker.trim().is_empty() {
            return Err(JsValue::from_str(sdk_contracts::ERR_TICKER_EMPTY));
        }
        if request.name.trim().is_empty() {
            return Err(JsValue::from_str(sdk_contracts::ERR_NAME_EMPTY));
        }
        let asset = self.with_attached_wallet(|wallet| {
            wallet
                .issue_asset_nia(
                    request.ticker,
                    request.name,
                    request.precision,
                    request.amounts,
                )
                .map_err(|e| JsValue::from_str(&e.to_string()))
        })?;
        crate::js_obj(&asset)
    }

    #[wasm_bindgen(js_name = issueAssetNiaJson)]
    pub fn issue_asset_nia_json(&self, request_js: JsValue) -> Result<String, JsValue> {
        let value = self.issue_asset_nia_value(request_js)?;
        let parsed: serde_json::Value = crate::js_from(value)?;
        crate::js_to_json(&parsed)
    }

    #[wasm_bindgen(js_name = issueAssetCfaValue)]
    pub fn issue_asset_cfa_value(&self, request_js: JsValue) -> Result<JsValue, JsValue> {
        self.ensure_runtime_ready()?;
        let request: WasmIssueAssetCfaRequest = serde_wasm_bindgen::from_value(request_js)
            .map_err(|e| JsValue::from_str(&format!("Invalid issue_asset_cfa request: {e}")))?;
        if request.amounts.is_empty() {
            return Err(JsValue::from_str(sdk_contracts::ERR_AMOUNTS_EMPTY));
        }
        if request.name.trim().is_empty() {
            return Err(JsValue::from_str(sdk_contracts::ERR_NAME_EMPTY));
        }
        let ticker = derive_cfa_ticker(&request.name);
        let asset = self.with_attached_wallet(|wallet| {
            wallet
                .issue_asset_ifa(
                    ticker,
                    request.name,
                    request.precision,
                    request.amounts,
                    vec![],
                    None,
                )
                .map_err(|e| JsValue::from_str(&e.to_string()))
        })?;
        let mapped = WasmAssetCfaData {
            asset_id: asset.asset_id,
            name: asset.name,
            details: request.details.or(asset.details),
            precision: asset.precision,
            issued_supply: asset.initial_supply,
            timestamp: asset.timestamp,
            added_at: asset.added_at,
            balance: asset.balance,
            media: asset.media,
        };
        crate::js_obj(&mapped)
    }

    #[wasm_bindgen(js_name = issueAssetCfaJson)]
    pub fn issue_asset_cfa_json(&self, request_js: JsValue) -> Result<String, JsValue> {
        let value = self.issue_asset_cfa_value(request_js)?;
        let parsed: serde_json::Value = crate::js_from(value)?;
        crate::js_to_json(&parsed)
    }

    #[wasm_bindgen(js_name = issueAssetIfaValue)]
    pub fn issue_asset_ifa_value(&self, request_js: JsValue) -> Result<JsValue, JsValue> {
        self.ensure_runtime_ready()?;
        let request: WasmIssueAssetIfaRequest = serde_wasm_bindgen::from_value(request_js)
            .map_err(|e| JsValue::from_str(&format!("Invalid issue_asset_ifa request: {e}")))?;
        if request.amounts.is_empty() {
            return Err(JsValue::from_str(sdk_contracts::ERR_AMOUNTS_EMPTY));
        }
        if request.ticker.trim().is_empty() {
            return Err(JsValue::from_str(sdk_contracts::ERR_TICKER_EMPTY));
        }
        if request.name.trim().is_empty() {
            return Err(JsValue::from_str(sdk_contracts::ERR_NAME_EMPTY));
        }
        let asset = self.with_attached_wallet(|wallet| {
            wallet
                .issue_asset_ifa(
                    request.ticker,
                    request.name,
                    request.precision,
                    request.amounts,
                    request.inflation_amounts,
                    request.reject_list_url,
                )
                .map_err(|e| JsValue::from_str(&e.to_string()))
        })?;
        crate::js_obj(&asset)
    }

    #[wasm_bindgen(js_name = issueAssetIfaJson)]
    pub fn issue_asset_ifa_json(&self, request_js: JsValue) -> Result<String, JsValue> {
        let value = self.issue_asset_ifa_value(request_js)?;
        let parsed: serde_json::Value = crate::js_from(value)?;
        crate::js_to_json(&parsed)
    }

    #[wasm_bindgen(js_name = connectPeer)]
    pub async fn connect_peer(
        &self,
        peer_addr: String,
        peer_pubkey: String,
    ) -> Result<(), JsValue> {
        self.ensure_runtime_ready()?;
        self.ensure_stable_identity_for_channel_operations()?;
        let peer_pubkey = peer_pubkey.trim().to_string();
        let peer_addr = peer_addr.trim().to_string();
        wasm_debug(&format!(
            "[rln-wasm-sdk connectPeer] start peer_pubkey={} peer_addr={} runtime_scope={}",
            peer_pubkey, peer_addr, self.persistence_keys.runtime_scope_key
        ));
        if peer_pubkey.trim().is_empty() {
            return Err(JsValue::from_str(sdk_contracts::ERR_PEER_PUBKEY_EMPTY));
        }
        if peer_addr.trim().is_empty() {
            return Err(JsValue::from_str(sdk_contracts::ERR_PEER_ADDR_EMPTY));
        }
        validate_peer_addr_format(&peer_addr)?;
        if SecpPublicKey::from_str(peer_pubkey.trim()).is_err() {
            return Err(JsValue::from_str(sdk_contracts::ERR_PEER_PUBKEY_INVALID));
        }
        if !has_peer_manager_hooks() {
            return Err(JsValue::from_str(
                "peer-manager hooks are not installed; install real peer-manager hooks before connectPeer",
            ));
        }
        if !has_peer_manager_hooks_v2() {
            return Err(JsValue::from_str(
                "peer-manager hooks are not V2-ready; installPeerManagerHooksFromJsV2 with take_outbound_frames is required",
            ));
        }
        // Always force a clean transport/runtime session before reconnect to avoid stale
        // descriptor/socket state leaking across retries.
        if let Some(existing) = self
            .peers
            .borrow()
            .get(&peer_pubkey)
            .map(|entry| Rc::clone(&entry.session))
        {
            let _ = existing.close().await;
        }
        self.peers.borrow_mut().remove(&peer_pubkey);
        if self.use_runtime_state_for_ln_views() {
            let _ = self
                .ldk_runtime
                .peer_socket_disconnected_for_peer(&peer_pubkey);
            let _ = self.ldk_runtime.remove_peer(&peer_pubkey);
        }

        if !self.use_runtime_state_for_ln_views() && self.peers.borrow().contains_key(&peer_pubkey)
        {
            wasm_debug("[rln-wasm-sdk connectPeer] already connected local-only mode");
            return Ok(());
        }

        let session_key =
            runtime_peer_session_key(&self.persistence_keys.runtime_scope_key, &peer_pubkey);
        // The gateway validates `session_id` conservatively (charset + max len 64).
        // Hash our internal session key to keep it stable and URL-safe.
        let _replay_session_id = Sha256::hash(session_key.as_bytes()).to_string();
        let relay_session_auth = self.relay_session_auth.borrow().clone();
        let options_js = crate::js_obj(&RlnWasmLnSocketConnectOptionsData {
            max_reconnect_attempts: Some(3),
            reconnect_initial_delay_ms: Some(250),
            reconnect_max_delay_ms: Some(4_000),
            relay_auth_token: relay_session_auth
                .as_ref()
                .map(|v| v.relay_auth_token.clone()),
            relay_node_id: relay_session_auth.as_ref().map(|v| v.relay_node_id.clone()),
            replay_transport_envelope: Some(false),
            replay_session_id: None,
            replay_last_applied_seq: None,
        })?;
        let session = match self
            .bridge
            .connect_session_with_options(
                self.proxy_url.clone(),
                peer_addr.clone(),
                peer_pubkey.clone(),
                options_js,
            )
            .await
        {
            Ok(session) => session,
            Err(err) => return Err(err),
        };
        wasm_debug(&format!(
            "[rln-wasm-sdk connectPeer] websocket connected peer_pubkey={} ws_url={}",
            peer_pubkey,
            session.websocket_url()
        ));
        if let Err(err) = session.start().await {
            return Err(err);
        }
        wasm_debug(&format!(
            "[rln-wasm-sdk connectPeer] session.start completed peer_pubkey={}",
            peer_pubkey
        ));

        let mut handshake_complete = false;
        for _ in 0..300 {
            if self
                .ldk_runtime
                .peer_is_handshake_complete(&peer_pubkey)
                .unwrap_or(false)
            {
                let _ = self.ldk_runtime.peer_process_events();
                handshake_complete = true;
                break;
            }
            sleep_ms(100).await;
        }
        if !handshake_complete {
            let msg = format!(
                "peer handshake did not complete within timeout for peer {}",
                peer_pubkey
            );
            wasm_debug(&format!("[rln-wasm-sdk connectPeer] {}", msg));
            return Err(JsValue::from_str(&msg));
        }

        self.peers.borrow_mut().insert(
            peer_pubkey.clone(),
            PeerEntry {
                peer_addr,
                session: Rc::new(session),
            },
        );
        self.persist_peer_session_state();
        if self.use_runtime_state_for_ln_views() {
            let runtime_connected = self.ldk_runtime.has_connected_peer(&peer_pubkey);
            self.ldk_runtime.upsert_peer(LdkRuntimePeerStateData {
                pubkey: peer_pubkey.clone(),
                peer_addr: self
                    .peers
                    .borrow()
                    .get(&peer_pubkey)
                    .map(|entry| entry.peer_addr.clone())
                    .unwrap_or_default(),
                started: runtime_connected,
            });
        }
        let applied = self
            .apply_and_record_transport_event(
                RuntimeTransportEvent::PeerReconnected {
                    peer_pubkey: peer_pubkey.clone(),
                },
                "node_api",
            )?
            .applied;
        if !applied {
            return Err(JsValue::from_str(
                "failed to apply peer_reconnected transport event",
            ));
        }
        wasm_debug(&format!(
            "[rln-wasm-sdk connectPeer] done peer_pubkey={} peers={} runtime_peers={}",
            peer_pubkey,
            self.peers.borrow().len(),
            self.ldk_runtime.list_peers().len()
        ));
        self.persist_runtime_event_log_state();
        Ok(())
    }

    #[wasm_bindgen(js_name = disconnectPeer)]
    pub async fn disconnect_peer(&self, peer_pubkey: String) -> Result<(), JsValue> {
        self.ensure_runtime_ready()?;
        let peer_pubkey = peer_pubkey.trim().to_string();
        if peer_pubkey.trim().is_empty() {
            return Err(JsValue::from_str(sdk_contracts::ERR_PEER_PUBKEY_EMPTY));
        }
        if SecpPublicKey::from_str(peer_pubkey.trim()).is_err() {
            return Err(JsValue::from_str(sdk_contracts::ERR_PEER_PUBKEY_INVALID));
        }

        let session = self
            .peers
            .borrow()
            .get(&peer_pubkey)
            .map(|entry| Rc::clone(&entry.session));
        if let Some(session) = session {
            session.close().await?;
        } else if !(self.use_runtime_state_for_ln_views()
            && self.ldk_runtime.has_peer(&peer_pubkey))
        {
            return Err(JsValue::from_str(sdk_contracts::ERR_PEER_NOT_CONNECTED));
        }
        let applied = self
            .apply_and_record_transport_event(
                RuntimeTransportEvent::PeerDisconnected {
                    peer_pubkey: peer_pubkey.clone(),
                },
                "node_api",
            )?
            .applied;
        if !applied {
            return Err(JsValue::from_str(sdk_contracts::ERR_PEER_NOT_CONNECTED));
        }
        self.peers.borrow_mut().remove(&peer_pubkey);
        self.persist_peer_session_state();
        self.persist_runtime_event_log_state();
        Ok(())
    }

    #[wasm_bindgen(js_name = reconnectPersistedPeersValue)]
    pub async fn reconnect_persisted_peers_value(&self) -> Result<JsValue, JsValue> {
        self.ensure_runtime_ready()?;
        let snapshot =
            load_runtime_peer_session_snapshot(&self.persistence_keys.peer_sessions_storage_key)
                .unwrap_or_default();
        let mut sessions = snapshot.sessions;
        sessions.sort_by(|a, b| a.session_key.cmp(&b.session_key));

        let mut connected = 0usize;
        let mut failed = Vec::new();
        for session in sessions.iter() {
            match self
                .connect_peer(session.peer_addr.clone(), session.peer_pubkey.clone())
                .await
            {
                Ok(()) => connected = connected.saturating_add(1),
                Err(err) => {
                    let msg = err
                        .as_string()
                        .unwrap_or_else(|| "unknown reconnect error".to_string());
                    failed.push(format!("{}: {}", session.session_key, msg));
                }
            }
        }

        crate::js_obj(&RuntimeReconnectPeersResultData {
            attempted: sessions.len(),
            connected,
            failed,
        })
    }

    #[wasm_bindgen(js_name = reconnectPersistedPeersJson)]
    pub async fn reconnect_persisted_peers_json(&self) -> Result<String, JsValue> {
        let value = self.reconnect_persisted_peers_value().await?;
        let parsed: serde_json::Value = crate::js_from(value)?;
        crate::js_to_json(&parsed)
    }

    #[wasm_bindgen(js_name = reconnectManagerStartValue)]
    pub fn reconnect_manager_start_value(&self) -> Result<JsValue, JsValue> {
        if *self.reconnect_manager_running.borrow() {
            return self.reconnect_manager_status_value();
        }
        *self.reconnect_manager_running.borrow_mut() = true;
        *self.reconnect_manager_backoff_ms.borrow_mut() = RECONNECT_MANAGER_INITIAL_DELAY_MS;

        let proxy_url = self.proxy_url.clone();
        let runtime_scope_key = self.persistence_keys.runtime_scope_key.clone();
        let peer_session_store_key = self.persistence_keys.peer_sessions_storage_key.clone();
        let relay_session_auth = self.relay_session_auth.borrow().clone();
        let peers = Rc::clone(&self.peers);
        let ldk_runtime = Rc::clone(&self.ldk_runtime);
        let running = Rc::clone(&self.reconnect_manager_running);
        let backoff_ms = Rc::clone(&self.reconnect_manager_backoff_ms);

        spawn_local(async move {
            let _ = reconnect_persisted_peers_once(
                &proxy_url,
                &runtime_scope_key,
                &peer_session_store_key,
                relay_session_auth.clone(),
                &peers,
                &ldk_runtime,
            )
            .await;

            while *running.borrow() {
                let delay = *backoff_ms.borrow();
                sleep_ms(delay).await;
                if !*running.borrow() {
                    break;
                }
                let result = reconnect_persisted_peers_once(
                    &proxy_url,
                    &runtime_scope_key,
                    &peer_session_store_key,
                    relay_session_auth.clone(),
                    &peers,
                    &ldk_runtime,
                )
                .await;
                if result.connected > 0 {
                    *backoff_ms.borrow_mut() = RECONNECT_MANAGER_INITIAL_DELAY_MS;
                } else {
                    let next = delay.saturating_mul(2).clamp(
                        RECONNECT_MANAGER_INITIAL_DELAY_MS,
                        RECONNECT_MANAGER_MAX_DELAY_MS,
                    );
                    *backoff_ms.borrow_mut() = next;
                }
            }
        });
        self.reconnect_manager_status_value()
    }

    #[wasm_bindgen(js_name = reconnectManagerStartJson)]
    pub fn reconnect_manager_start_json(&self) -> Result<String, JsValue> {
        let value = self.reconnect_manager_start_value()?;
        let parsed: serde_json::Value = crate::js_from(value)?;
        crate::js_to_json(&parsed)
    }

    #[wasm_bindgen(js_name = reconnectManagerStopValue)]
    pub fn reconnect_manager_stop_value(&self) -> Result<JsValue, JsValue> {
        *self.reconnect_manager_running.borrow_mut() = false;
        self.reconnect_manager_status_value()
    }

    #[wasm_bindgen(js_name = reconnectManagerStopJson)]
    pub fn reconnect_manager_stop_json(&self) -> Result<String, JsValue> {
        let value = self.reconnect_manager_stop_value()?;
        let parsed: serde_json::Value = crate::js_from(value)?;
        crate::js_to_json(&parsed)
    }

    #[wasm_bindgen(js_name = reconnectManagerStatusValue)]
    pub fn reconnect_manager_status_value(&self) -> Result<JsValue, JsValue> {
        crate::js_obj(&RuntimeReconnectManagerStatusData {
            running: *self.reconnect_manager_running.borrow(),
            current_backoff_ms: *self.reconnect_manager_backoff_ms.borrow(),
        })
    }

    #[wasm_bindgen(js_name = reconnectManagerStatusJson)]
    pub fn reconnect_manager_status_json(&self) -> Result<String, JsValue> {
        let value = self.reconnect_manager_status_value()?;
        let parsed: serde_json::Value = crate::js_from(value)?;
        crate::js_to_json(&parsed)
    }

    /// Start the autonomous drive loop: a self-scheduling timer that runs `node_drive_tick_once`
    /// (chain sync → live LDK → event draining → authoritative channel reconcile) every
    /// `interval_ms` (clamped to a sane minimum; `0` selects the default cadence). This lets apps
    /// receive invoices / progress channels / settle HTLCs without hand-driving `chainSyncTick`,
    /// mirroring the native daemon's background processor (PARITY_PLAN 0.2). Idempotent: calling it
    /// while already running just returns the current status.
    #[wasm_bindgen(js_name = autoDriveStartValue)]
    pub fn auto_drive_start_value(&self, interval_ms: u32) -> Result<JsValue, JsValue> {
        self.ensure_runtime_ready()?;
        if *self.auto_drive_running.borrow() {
            return self.auto_drive_status_value();
        }
        let interval = if interval_ms == 0 {
            AUTO_DRIVE_DEFAULT_INTERVAL_MS
        } else {
            interval_ms.max(AUTO_DRIVE_MIN_INTERVAL_MS)
        };
        *self.auto_drive_running.borrow_mut() = true;
        *self.auto_drive_interval_ms.borrow_mut() = interval;

        let chain_sync = self.chain_sync.clone();
        let ldk_runtime = Rc::clone(&self.ldk_runtime);
        let runtime_core = self.runtime_core.clone();
        let peers = Rc::clone(&self.peers);
        let channels = Rc::clone(&self.channels);
        let payments = Rc::clone(&self.payments);
        let pending_peer_hook_events = Rc::clone(&self.pending_peer_hook_events);
        let runtime_events = Rc::clone(&self.runtime_events);
        let next_runtime_event_seq = Rc::clone(&self.next_runtime_event_seq);
        let runtime_events_storage_key = self.persistence_keys.runtime_events_storage_key.clone();
        let running = Rc::clone(&self.auto_drive_running);
        let interval_ref = Rc::clone(&self.auto_drive_interval_ms);

        spawn_local(async move {
            while *running.borrow() {
                let delay = *interval_ref.borrow();
                sleep_ms(delay).await;
                if !*running.borrow() {
                    break;
                }
                if let Err(err) = node_drive_tick_once(
                    &chain_sync,
                    &ldk_runtime,
                    &runtime_core,
                    &peers,
                    &channels,
                    &payments,
                    &pending_peer_hook_events,
                    &runtime_events,
                    &next_runtime_event_seq,
                    &runtime_events_storage_key,
                    "auto_drive",
                )
                .await
                {
                    // Keep the loop alive across transient indexer/peer errors; surface for debugging.
                    wasm_debug(&format!(
                        "[rln-wasm-sdk auto-drive] drive tick error (continuing): {}",
                        err.as_string()
                            .unwrap_or_else(|| "unknown auto-drive error".to_string())
                    ));
                }
            }
        });
        self.auto_drive_status_value()
    }

    #[wasm_bindgen(js_name = autoDriveStartJson)]
    pub fn auto_drive_start_json(&self, interval_ms: u32) -> Result<String, JsValue> {
        let value = self.auto_drive_start_value(interval_ms)?;
        let parsed: serde_json::Value = crate::js_from(value)?;
        crate::js_to_json(&parsed)
    }

    #[wasm_bindgen(js_name = autoDriveStopValue)]
    pub fn auto_drive_stop_value(&self) -> Result<JsValue, JsValue> {
        *self.auto_drive_running.borrow_mut() = false;
        self.auto_drive_status_value()
    }

    #[wasm_bindgen(js_name = autoDriveStopJson)]
    pub fn auto_drive_stop_json(&self) -> Result<String, JsValue> {
        let value = self.auto_drive_stop_value()?;
        let parsed: serde_json::Value = crate::js_from(value)?;
        crate::js_to_json(&parsed)
    }

    #[wasm_bindgen(js_name = autoDriveStatusValue)]
    pub fn auto_drive_status_value(&self) -> Result<JsValue, JsValue> {
        crate::js_obj(&RlnWasmNodeAutoDriveStatusData {
            running: *self.auto_drive_running.borrow(),
            interval_ms: *self.auto_drive_interval_ms.borrow(),
        })
    }

    #[wasm_bindgen(js_name = autoDriveStatusJson)]
    pub fn auto_drive_status_json(&self) -> Result<String, JsValue> {
        let value = self.auto_drive_status_value()?;
        let parsed: serde_json::Value = crate::js_from(value)?;
        crate::js_to_json(&parsed)
    }

    #[wasm_bindgen(js_name = reconnectManagerOnResume)]
    pub fn reconnect_manager_on_resume(&self) {
        if !*self.reconnect_manager_running.borrow() {
            return;
        }
        let proxy_url = self.proxy_url.clone();
        let runtime_scope_key = self.persistence_keys.runtime_scope_key.clone();
        let peer_session_store_key = self.persistence_keys.peer_sessions_storage_key.clone();
        let relay_session_auth = self.relay_session_auth.borrow().clone();
        let peers = Rc::clone(&self.peers);
        let ldk_runtime = Rc::clone(&self.ldk_runtime);
        let backoff_ms = Rc::clone(&self.reconnect_manager_backoff_ms);
        spawn_local(async move {
            let result = reconnect_persisted_peers_once(
                &proxy_url,
                &runtime_scope_key,
                &peer_session_store_key,
                relay_session_auth,
                &peers,
                &ldk_runtime,
            )
            .await;
            if result.connected > 0 {
                *backoff_ms.borrow_mut() = RECONNECT_MANAGER_INITIAL_DELAY_MS;
            }
        });
    }

    #[wasm_bindgen(js_name = listPeersValue)]
    pub fn list_peers_value(&self) -> Result<JsValue, JsValue> {
        self.ensure_runtime_ready()?;
        let mut data = if self.use_runtime_state_for_ln_views() {
            self.ldk_runtime
                .list_peers()
                .into_iter()
                .map(|peer| RlnWasmNodePeerData {
                    pubkey: peer.pubkey,
                    peer_addr: peer.peer_addr,
                    started: peer.started,
                })
                .collect::<Vec<_>>()
        } else {
            self.peers
                .borrow()
                .iter()
                .map(|(pubkey, entry)| RlnWasmNodePeerData {
                    pubkey: pubkey.clone(),
                    peer_addr: entry.peer_addr.clone(),
                    started: entry.session.is_started(),
                })
                .collect::<Vec<_>>()
        };
        data.sort_by(|a, b| a.pubkey.cmp(&b.pubkey));
        crate::js_obj(&data)
    }

    #[wasm_bindgen(js_name = listPeersJson)]
    pub fn list_peers_json(&self) -> Result<String, JsValue> {
        let value = self.list_peers_value()?;
        let parsed: serde_json::Value = crate::js_from(value)?;
        crate::js_to_json(&parsed)
    }

    #[wasm_bindgen(js_name = listChannelsValue)]
    pub fn list_channels_value(&self) -> Result<JsValue, JsValue> {
        self.ensure_runtime_ready()?;
        let mut data = if self.use_runtime_state_for_ln_views() {
            let mut runtime = self
                .ldk_runtime
                .list_channels()
                .into_iter()
                .map(Self::channel_data_from_runtime_state)
                .collect::<Vec<_>>();
            // Listing must not be a destructive poll. Reconcile is driven by open/funding,
            // peer processing, and chain-sync ticks; only try it here when runtime is empty.
            if runtime.is_empty() {
                let _ = self.ldk_runtime.reconcile_channels_from_live();
                runtime = self
                    .ldk_runtime
                    .list_channels()
                    .into_iter()
                    .map(Self::channel_data_from_runtime_state)
                    .collect::<Vec<_>>();
            }
            self.merge_runtime_channels_with_local_cache(runtime)
        } else {
            self.channels
                .borrow()
                .values()
                .map(|entry| entry.data.clone())
                .collect::<Vec<_>>()
        };
        for channel in &mut data {
            if channel.virtual_open_mode.is_none()
                && self
                    .ldk_runtime
                    .virtual_channel_session_get(&channel.channel_id)
                    .is_some()
            {
                channel.virtual_open_mode =
                    Some(SDK_VIRTUAL_OPEN_MODE_TRUSTED_NO_BROADCAST.to_string());
            }
        }
        data.sort_by(|a, b| a.channel_id.cmp(&b.channel_id));
        crate::js_obj(&data)
    }

    #[wasm_bindgen(js_name = listChannelsJson)]
    pub fn list_channels_json(&self) -> Result<String, JsValue> {
        let value = self.list_channels_value()?;
        let parsed: serde_json::Value = crate::js_from(value)?;
        crate::js_to_json(&parsed)
    }

    #[wasm_bindgen(js_name = nodeInfoValue)]
    pub fn node_info_value(&self) -> Result<JsValue, JsValue> {
        self.ensure_runtime_ready()?;
        let (num_channels, num_usable_channels) = if self.use_runtime_state_for_ln_views() {
            let channels = self.ldk_runtime.list_channels();
            let num_channels = channels.len();
            let num_usable_channels = channels.iter().filter(|entry| entry.is_usable).count();
            (num_channels, num_usable_channels)
        } else {
            let channels = self.channels.borrow();
            let num_channels = channels.len();
            let num_usable_channels = channels
                .values()
                .filter(|entry| entry.data.is_usable)
                .count();
            (num_channels, num_usable_channels)
        };
        let runtime_status = self.ldk_runtime.status();
        let data = RlnWasmNodeInfoData {
            runtime: format!(
                "wasm32-unknown-unknown/{}:{}",
                runtime_status.backend, runtime_status.lifecycle_state
            ),
            ldk_over_websocket: true,
            num_peers: if self.use_runtime_state_for_ln_views() {
                self.ldk_runtime.list_peers().len()
            } else {
                self.peers.borrow().len()
            },
            num_channels,
            num_usable_channels,
        };
        crate::js_obj(&data)
    }

    #[wasm_bindgen(js_name = nodeInfoJson)]
    pub fn node_info_json(&self) -> Result<String, JsValue> {
        let value = self.node_info_value()?;
        let parsed: serde_json::Value = crate::js_from(value)?;
        crate::js_to_json(&parsed)
    }

    #[wasm_bindgen(js_name = nodePubkeyValue)]
    pub fn node_pubkey_value(&self) -> Result<JsValue, JsValue> {
        self.ensure_runtime_ready()?;
        let pubkey = self
            .local_node_pubkey_string()
            .ok_or_else(|| JsValue::from_str(sdk_contracts::ERR_NODE_PUBKEY_DERIVE_FAILED))?;
        crate::js_obj(&serde_json::json!({ "pubkey": pubkey }))
    }

    #[wasm_bindgen(js_name = nodePubkeyJson)]
    pub fn node_pubkey_json(&self) -> Result<String, JsValue> {
        let value = self.node_pubkey_value()?;
        let parsed: serde_json::Value = crate::js_from(value)?;
        crate::js_to_json(&parsed)
    }

    #[wasm_bindgen(js_name = networkInfoValue)]
    pub fn network_info_value(&self) -> Result<JsValue, JsValue> {
        self.ensure_runtime_ready()?;
        crate::js_obj(&RlnWasmNodeNetworkInfoData {
            network: self.network.borrow().clone(),
            height: self.chain_sync.latest_tip_height().unwrap_or(0),
        })
    }

    #[wasm_bindgen(js_name = chainSyncStartValue)]
    pub fn chain_sync_start_value(
        &self,
        indexer_url: String,
        poll_interval_ms: Option<u32>,
    ) -> Result<JsValue, JsValue> {
        self.ensure_runtime_ready()?;
        self.chain_sync.start(indexer_url, poll_interval_ms)?;
        let status: RlnWasmChainSyncStatusData = self.chain_sync.status();
        *self.network.borrow_mut() = status.network.clone();
        crate::js_obj(&status)
    }

    #[wasm_bindgen(js_name = chainSyncStartJson)]
    pub fn chain_sync_start_json(
        &self,
        indexer_url: String,
        poll_interval_ms: Option<u32>,
    ) -> Result<String, JsValue> {
        let value = self.chain_sync_start_value(indexer_url, poll_interval_ms)?;
        let parsed: serde_json::Value = crate::js_from(value)?;
        crate::js_to_json(&parsed)
    }

    #[wasm_bindgen(js_name = chainSyncStopValue)]
    pub fn chain_sync_stop_value(&self) -> Result<JsValue, JsValue> {
        self.ensure_runtime_ready()?;
        self.chain_sync.stop()?;
        crate::js_obj(&self.chain_sync.status())
    }

    #[wasm_bindgen(js_name = chainSyncStopJson)]
    pub fn chain_sync_stop_json(&self) -> Result<String, JsValue> {
        let value = self.chain_sync_stop_value()?;
        let parsed: serde_json::Value = crate::js_from(value)?;
        crate::js_to_json(&parsed)
    }

    #[wasm_bindgen(js_name = chainSyncStatusValue)]
    pub fn chain_sync_status_value(&self) -> Result<JsValue, JsValue> {
        self.ensure_runtime_ready()?;
        crate::js_obj(&self.chain_sync.status())
    }

    #[wasm_bindgen(js_name = chainSyncStatusJson)]
    pub fn chain_sync_status_json(&self) -> Result<String, JsValue> {
        let value = self.chain_sync_status_value()?;
        let parsed: serde_json::Value = crate::js_from(value)?;
        crate::js_to_json(&parsed)
    }

    #[wasm_bindgen(js_name = chainSyncTickValue)]
    pub async fn chain_sync_tick_value(&self) -> Result<JsValue, JsValue> {
        self.ensure_runtime_ready()?;
        // One full drive pass (chain sync → live LDK → event draining → authoritative reconcile).
        // The autonomous loop (`autoDriveStart`) runs the exact same `node_drive_tick_once`, so a
        // manually-ticked and a self-driven node progress identically.
        node_drive_tick_once(
            &self.chain_sync,
            &self.ldk_runtime,
            &self.runtime_core,
            &self.peers,
            &self.channels,
            &self.payments,
            &self.pending_peer_hook_events,
            &self.runtime_events,
            &self.next_runtime_event_seq,
            &self.persistence_keys.runtime_events_storage_key,
            "chain_sync_tick",
        )
        .await?;
        crate::js_obj(&self.chain_sync.status())
    }

    #[wasm_bindgen(js_name = chainSyncTickJson)]
    pub async fn chain_sync_tick_json(&self) -> Result<String, JsValue> {
        let value = self.chain_sync_tick_value().await?;
        let parsed: serde_json::Value = crate::js_from(value)?;
        crate::js_to_json(&parsed)
    }

    #[wasm_bindgen(js_name = chainSyncEnqueueRebroadcastTx)]
    pub fn chain_sync_enqueue_rebroadcast_tx(
        &self,
        txid: String,
        tx_hex: String,
    ) -> Result<(), JsValue> {
        self.ensure_runtime_ready()?;
        self.chain_sync.enqueue_rebroadcast_tx(txid, tx_hex)
    }

    #[wasm_bindgen(js_name = ldkRuntimeStatusValue)]
    pub fn ldk_runtime_status_value(&self) -> Result<JsValue, JsValue> {
        self.ldk_runtime
            .set_identity_stable(self.identity_stable_for_channel_operations());
        let status: LdkRuntimeStatusData = self.ldk_runtime.status();
        crate::js_obj(&status)
    }

    #[wasm_bindgen(js_name = ldkRuntimeStatusJson)]
    pub fn ldk_runtime_status_json(&self) -> Result<String, JsValue> {
        let value = self.ldk_runtime_status_value()?;
        let parsed: serde_json::Value = crate::js_from(value)?;
        crate::js_to_json(&parsed)
    }

    #[wasm_bindgen(js_name = ldkRuntimeComponentsValue)]
    pub fn ldk_runtime_components_value(&self) -> Result<JsValue, JsValue> {
        self.ensure_runtime_ready()?;
        let status: LdkRuntimeComponentsStatusData = self.ldk_runtime.component_status();
        crate::js_obj(&status)
    }

    #[wasm_bindgen(js_name = ldkRuntimeComponentsJson)]
    pub fn ldk_runtime_components_json(&self) -> Result<String, JsValue> {
        let value = self.ldk_runtime_components_value()?;
        let parsed: serde_json::Value = crate::js_from(value)?;
        crate::js_to_json(&parsed)
    }

    #[wasm_bindgen(js_name = persistLdkRuntimeState)]
    pub fn persist_ldk_runtime_state(&self) -> Result<(), JsValue> {
        self.ldk_runtime.persist_live_state()?;
        Ok(())
    }

    #[wasm_bindgen(js_name = listPendingFundingRequestsValue)]
    pub fn list_pending_funding_requests_value(&self) -> Result<JsValue, JsValue> {
        self.ensure_runtime_ready()?;
        crate::js_obj(&self.ldk_runtime.list_pending_funding_requests()?)
    }

    #[wasm_bindgen(js_name = listPendingFundingRequestsJson)]
    pub fn list_pending_funding_requests_json(&self) -> Result<String, JsValue> {
        let value = self.list_pending_funding_requests_value()?;
        let parsed: serde_json::Value = crate::js_from(value)?;
        crate::js_to_json(&parsed)
    }

    #[wasm_bindgen(js_name = submitFundingTransactionValue)]
    pub fn submit_funding_transaction_value(
        &self,
        submission_js: JsValue,
    ) -> Result<JsValue, JsValue> {
        self.ensure_runtime_ready()?;
        let submission: WasmFundingTxSubmissionRequest =
            serde_wasm_bindgen::from_value(submission_js)
                .map_err(|e| JsValue::from_str(&e.to_string()))?;
        if submission.temporary_channel_id.trim().is_empty()
            || submission.counterparty_node_id.trim().is_empty()
            || submission.funding_tx_hex.trim().is_empty()
        {
            return Err(JsValue::from_str(
                "temporary_channel_id, counterparty_node_id and funding_tx_hex are required",
            ));
        }
        self.ldk_runtime
            .submit_funding_transaction(LdkRuntimeFundingTxSubmissionData {
                temporary_channel_id: submission.temporary_channel_id,
                counterparty_node_id: submission.counterparty_node_id,
                funding_tx_hex: submission.funding_tx_hex,
            })?;
        crate::js_obj(&serde_json::json!({ "submitted": true }))
    }

    #[wasm_bindgen(js_name = submitFundingTransactionJson)]
    pub fn submit_funding_transaction_json(
        &self,
        submission_js: JsValue,
    ) -> Result<String, JsValue> {
        let value = self.submit_funding_transaction_value(submission_js)?;
        let parsed: serde_json::Value = crate::js_from(value)?;
        crate::js_to_json(&parsed)
    }

    #[wasm_bindgen(js_name = peerEngineStatusValue)]
    pub fn peer_engine_status_value(&self) -> Result<JsValue, JsValue> {
        Err(JsValue::from_str(
            "peer engine scaffold was removed; use real peer-manager hooks implementation",
        ))
    }

    #[wasm_bindgen(js_name = peerEngineStatusJson)]
    pub fn peer_engine_status_json(&self) -> Result<String, JsValue> {
        let value = self.peer_engine_status_value()?;
        let parsed: serde_json::Value = crate::js_from(value)?;
        crate::js_to_json(&parsed)
    }

    #[wasm_bindgen(js_name = networkInfoJson)]
    pub fn network_info_json(&self) -> Result<String, JsValue> {
        let value = self.network_info_value()?;
        let parsed: serde_json::Value = crate::js_from(value)?;
        crate::js_to_json(&parsed)
    }

    #[wasm_bindgen(js_name = signMessageValue)]
    pub fn sign_message_value(&self, message: String) -> Result<JsValue, JsValue> {
        self.ensure_runtime_ready()?;
        let signed_message = self.sign_node_message(message.trim())?;
        crate::js_obj(&RlnWasmNodeSignMessageData { signed_message })
    }

    #[wasm_bindgen(js_name = signMessageJson)]
    pub fn sign_message_json(&self, message: String) -> Result<String, JsValue> {
        let value = self.sign_message_value(message)?;
        let parsed: serde_json::Value = crate::js_from(value)?;
        crate::js_to_json(&parsed)
    }

    #[wasm_bindgen(js_name = closeAllPeers)]
    pub async fn close_all_peers(&self) -> Result<(), JsValue> {
        self.ensure_runtime_ready()?;
        let peer_pubkeys: Vec<String> = if self.use_runtime_state_for_ln_views() {
            self.ldk_runtime
                .list_peers()
                .into_iter()
                .map(|peer| peer.pubkey)
                .collect()
        } else {
            self.peers.borrow().keys().cloned().collect()
        };
        for pubkey in peer_pubkeys {
            self.disconnect_peer(pubkey).await?;
        }
        self.ldk_runtime.stop()?;
        self.runtime_core.stop();
        Ok(())
    }

    #[wasm_bindgen(js_name = nativeRuntimeCoreStatusValue)]
    pub fn native_runtime_core_status_value(&self) -> Result<JsValue, JsValue> {
        let status: NativeLnRuntimeCoreStatusData = self.runtime_core.status();
        crate::js_obj(&status)
    }

    #[wasm_bindgen(js_name = nativeRuntimeCoreStatusJson)]
    pub fn native_runtime_core_status_json(&self) -> Result<String, JsValue> {
        let value = self.native_runtime_core_status_value()?;
        let parsed: serde_json::Value = crate::js_from(value)?;
        crate::js_to_json(&parsed)
    }

    #[wasm_bindgen(js_name = drainNativeRuntimeQueueValue)]
    pub fn drain_native_runtime_queue_value(&self) -> Result<JsValue, JsValue> {
        self.ensure_runtime_ready()?;
        let drained = self.runtime_core.drain_events();
        crate::js_obj(&drained)
    }

    #[wasm_bindgen(js_name = drainNativeRuntimeQueueJson)]
    pub fn drain_native_runtime_queue_json(&self) -> Result<String, JsValue> {
        let value = self.drain_native_runtime_queue_value()?;
        let parsed: serde_json::Value = crate::js_from(value)?;
        crate::js_to_json(&parsed)
    }

    #[wasm_bindgen(js_name = processNativeRuntimeQueueValue)]
    pub fn process_native_runtime_queue_value(&self) -> Result<JsValue, JsValue> {
        self.ensure_runtime_ready()?;
        let drained = self.runtime_core.drain_events();
        for queued in drained.iter() {
            apply_runtime_hook_payload(
                &self.ldk_runtime,
                self.use_runtime_state_for_ln_views(),
                &self.peers,
                &self.channels,
                &self.payments,
                &self.runtime_events,
                &self.next_runtime_event_seq,
                queued.payload_hex.clone(),
                "native_runtime_queue",
            )?;
        }
        self.persist_runtime_event_log_state();
        crate::js_obj(&RlnWasmNodeRuntimeQueueProcessData {
            drained: drained.len(),
        })
    }

    #[wasm_bindgen(js_name = processNativeRuntimeQueueJson)]
    pub fn process_native_runtime_queue_json(&self) -> Result<String, JsValue> {
        let value = self.process_native_runtime_queue_value()?;
        let parsed: serde_json::Value = crate::js_from(value)?;
        crate::js_to_json(&parsed)
    }

    #[wasm_bindgen(js_name = installAutoPeerManagerHooks)]
    pub fn install_auto_peer_manager_hooks(&self) {
        let payments = self.payments.clone();
        let peers = self.peers.clone();
        let channels = self.channels.clone();
        let pending_peer_hook_events = self.pending_peer_hook_events.clone();
        let runtime_events = self.runtime_events.clone();
        let next_runtime_event_seq = self.next_runtime_event_seq.clone();
        let runtime_event_store_key = self.persistence_keys.runtime_events_storage_key.clone();
        let ldk_runtime = self.ldk_runtime.clone();
        let use_runtime_state_for_ln_views = self.use_runtime_state_for_ln_views();
        install_rln_ldk_peer_manager_hooks(RlnLdkPeerManagerHooks {
            new_outbound_connection: Rc::new({
                let ldk_runtime = ldk_runtime.clone();
                move |peer_pubkey| ldk_runtime.peer_new_outbound_connection(peer_pubkey)
            }),
            read_event: Rc::new({
                let ldk_runtime = ldk_runtime.clone();
                let pending_peer_hook_events = pending_peer_hook_events.clone();
                move |peer_pubkey, payload_hex| {
                    ldk_runtime.peer_read_event_for_peer(peer_pubkey, payload_hex)?;
                    pending_peer_hook_events
                        .borrow_mut()
                        .push(PendingPeerHookEvent::Payload(payload_hex.to_string()));
                    Ok(())
                }
            }),
            process_events: Rc::new({
                let ldk_runtime = ldk_runtime.clone();
                let payments = payments.clone();
                let peers = peers.clone();
                let channels = channels.clone();
                let pending_peer_hook_events = pending_peer_hook_events.clone();
                let runtime_events = runtime_events.clone();
                let next_runtime_event_seq = next_runtime_event_seq.clone();
                move || {
                    ldk_runtime.peer_process_events()?;
                    let _ = drain_pending_peer_hook_events(
                        &ldk_runtime,
                        use_runtime_state_for_ln_views,
                        &peers,
                        &channels,
                        &payments,
                        &pending_peer_hook_events,
                        &runtime_events,
                        &next_runtime_event_seq,
                        "peer_hook",
                    )?;
                    persist_runtime_event_log_state(
                        &runtime_event_store_key,
                        &runtime_events,
                        &next_runtime_event_seq,
                    );
                    Ok(())
                }
            }),
            take_outbound_frames: Rc::new({
                let ldk_runtime = ldk_runtime.clone();
                move |peer_pubkey| ldk_runtime.peer_take_outbound_frames_for_peer(peer_pubkey)
            }),
            socket_disconnected: Rc::new({
                let ldk_runtime = ldk_runtime.clone();
                let pending_peer_hook_events = pending_peer_hook_events.clone();
                move |peer_pubkey| {
                    ldk_runtime.peer_socket_disconnected_for_peer(peer_pubkey)?;
                    pending_peer_hook_events
                        .borrow_mut()
                        .push(PendingPeerHookEvent::SocketDisconnected);
                    Ok(())
                }
            }),
            report_error: Rc::new({
                let pending_peer_hook_events = pending_peer_hook_events.clone();
                move |message| {
                    pending_peer_hook_events
                        .borrow_mut()
                        .push(PendingPeerHookEvent::Error(message.to_string()));
                    Ok(())
                }
            }),
        });
    }

    #[wasm_bindgen(js_name = clearAutoPeerManagerHooks)]
    pub fn clear_auto_peer_manager_hooks(&self) {
        clear_rln_ldk_peer_manager_hooks();
    }

    #[wasm_bindgen(js_name = listRuntimeEventsValue)]
    pub fn list_runtime_events_value(&self) -> Result<JsValue, JsValue> {
        let mut events = self.runtime_events.borrow().clone();
        events.sort_by(|a, b| a.seq.cmp(&b.seq));
        crate::js_obj(&events)
    }

    #[wasm_bindgen(js_name = listRuntimeEventsJson)]
    pub fn list_runtime_events_json(&self) -> Result<String, JsValue> {
        let value = self.list_runtime_events_value()?;
        let parsed: serde_json::Value = crate::js_from(value)?;
        crate::js_to_json(&parsed)
    }

    #[wasm_bindgen(js_name = failPendingPayments)]
    pub fn fail_pending_payments_api(&self) -> Result<JsValue, JsValue> {
        self.ensure_runtime_ready()?;
        ensure_manual_status_update_allowed(self.use_runtime_state_for_ln_views())?;
        if self.use_runtime_state_for_ln_views() {
            let pending_hashes = self
                .ldk_runtime
                .list_payments()
                .into_iter()
                .filter(|p| p.status == "pending")
                .map(|p| p.payment_hash)
                .collect::<Vec<_>>();
            for payment_hash in pending_hashes {
                let _ = self.apply_payment_status_via_event_stream(
                    &payment_hash,
                    "failed",
                    "manual_api",
                )?;
            }
        } else {
            fail_pending_payments_with_runtime_events(
                &self.payments,
                &self.runtime_events,
                &self.next_runtime_event_seq,
                "manual_api",
                "failed",
            )?;
        }
        self.persist_runtime_event_log_state();
        self.list_payments_value()
    }

    #[wasm_bindgen(js_name = sendPaymentValue)]
    pub fn send_payment_value(
        &self,
        invoice: String,
        amt_msat: Option<u64>,
        asset_id: Option<String>,
        asset_amount: Option<u64>,
    ) -> Result<JsValue, JsValue> {
        self.ensure_runtime_ready()?;
        let invoice = invoice.trim().to_string();
        if invoice.is_empty() {
            return Err(JsValue::from_str(sdk_contracts::ERR_INVOICE_EMPTY));
        }
        if (asset_id.is_some() && asset_amount.is_none())
            || (asset_id.is_none() && asset_amount.is_some())
        {
            return Err(JsValue::from_str(
                "asset_id and asset_amount must be provided together",
            ));
        }
        if let Some(id) = &asset_id {
            if id.trim().is_empty() {
                return Err(JsValue::from_str(
                    sdk_contracts::ERR_ASSET_ID_EMPTY_IF_PROVIDED,
                ));
            }
            validate_asset_id_format(id)?;
        }
        if let Some(amount) = asset_amount {
            if amount == 0 {
                return Err(JsValue::from_str(
                    sdk_contracts::ERR_ASSET_AMOUNT_NONPOSITIVE,
                ));
            }
        }
        if let Some(msat) = amt_msat {
            if msat == 0 {
                return Err(JsValue::from_str(sdk_contracts::ERR_AMT_MSAT_NONPOSITIVE));
            }
        }

        let parsed = Bolt11Invoice::from_str(&invoice)
            .map_err(|e| JsValue::from_str(&format!("invalid invoice: {e}")))?;
        let payment_hash = parsed.payment_hash().to_string();
        let payment_id = payment_hash.clone();
        let invoice_amt_msat = parsed.amount_milli_satoshis();
        let zero_amt_invoice = invoice_amt_msat.is_none() || invoice_amt_msat == Some(0);
        if zero_amt_invoice && amt_msat.is_none() {
            return Err(JsValue::from_str(
                "need an amount for the given 0-value invoice",
            ));
        }
        let resolved_amt_msat = match (amt_msat, invoice_amt_msat) {
            (Some(requested), Some(from_invoice)) if requested != from_invoice => {
                return Err(JsValue::from_str(&format!(
                    "amount didn't match invoice value of {from_invoice}msat"
                )));
            }
            (Some(requested), _) => Some(requested),
            (None, Some(from_invoice)) => Some(from_invoice),
            (None, None) => None,
        };
        if (asset_id.is_some() || asset_amount.is_some())
            && resolved_amt_msat.unwrap_or(0) < SDK_INVOICE_MIN_MSAT
        {
            return Err(JsValue::from_str(&format!(
                "amt_msat in invoice sending an RGB asset cannot be less than {SDK_INVOICE_MIN_MSAT}"
            )));
        }
        let now = unix_now_secs();
        let payee_pubkey = parsed
            .payee_pub_key()
            .copied()
            .unwrap_or_else(|| parsed.recover_payee_pub_key())
            .to_string();
        let has_connected_peer = if self.use_runtime_state_for_ln_views() {
            if self.ldk_runtime.get_peer(&payee_pubkey).is_some() {
                self.has_connected_peer(&payee_pubkey)
            } else {
                self.has_any_connected_peer()
            }
        } else if self.peers.borrow().contains_key(&payee_pubkey) {
            self.has_connected_peer(&payee_pubkey)
        } else {
            self.has_any_connected_peer()
        };
        let data = RlnWasmNodePaymentData {
            amt_msat: resolved_amt_msat,
            asset_amount,
            asset_id,
            payment_hash: payment_hash.clone(),
            inbound: false,
            status: "pending".to_string(),
            invoice_type: None,
            preimage: None,
            created_at: now,
            updated_at: now,
            payee_pubkey: payee_pubkey.clone(),
        };

        self.payments
            .borrow_mut()
            .insert(payment_hash.clone(), PaymentEntry { data });
        if let Some(payment) = self
            .payments
            .borrow()
            .get(&payment_hash)
            .map(|entry| entry.data.clone())
        {
            self.register_rgb_ln_transfer_from_payment(&payment);
        }
        self.ldk_runtime.record_payment_initiated();
        if self.use_runtime_state_for_ln_views() {
            let runtime_payment = self
                .payments
                .borrow()
                .get(&payment_hash)
                .map(|entry| Self::payment_runtime_state_from_data(&entry.data))
                .ok_or_else(|| {
                    JsValue::from_str(sdk_contracts::ERR_PAYMENT_NOT_FOUND_AFTER_CREATION)
                })?;
            self.ldk_runtime.upsert_payment(runtime_payment);
        }
        if !has_connected_peer {
            let _ =
                self.apply_payment_status_via_event_stream(&payment_hash, "failed", "node_api")?;
        } else {
            self.emit_runtime_payment_success_event_if_applicable(&payment_hash, &payee_pubkey)?;
        }
        self.persist_runtime_event_log_state();
        let final_status = if self.use_runtime_state_for_ln_views() {
            self.ldk_runtime
                .get_payment(&payment_hash)
                .map(|payment| payment.status)
                .ok_or_else(|| {
                    JsValue::from_str(sdk_contracts::ERR_PAYMENT_NOT_FOUND_AFTER_CREATION)
                })?
        } else {
            self.payments
                .borrow()
                .get(&payment_hash)
                .map(|entry| entry.data.status.clone())
                .ok_or_else(|| {
                    JsValue::from_str(sdk_contracts::ERR_PAYMENT_NOT_FOUND_AFTER_CREATION)
                })?
        };

        crate::js_obj(&RlnWasmNodeSendPaymentResult {
            payment_id,
            payment_hash: Some(payment_hash),
            payment_secret: Some(hex::encode(parsed.payment_secret().0)),
            status: final_status,
        })
    }

    #[wasm_bindgen(js_name = sendPaymentJson)]
    pub fn send_payment_json(
        &self,
        invoice: String,
        amt_msat: Option<u64>,
        asset_id: Option<String>,
        asset_amount: Option<u64>,
    ) -> Result<String, JsValue> {
        let value = self.send_payment_value(invoice, amt_msat, asset_id, asset_amount)?;
        let parsed: serde_json::Value = crate::js_from(value)?;
        crate::js_to_json(&parsed)
    }

    #[wasm_bindgen(js_name = keysendValue)]
    pub fn keysend_value(
        &self,
        dest_pubkey: String,
        amt_msat: u64,
        asset_id: Option<String>,
        asset_amount: Option<u64>,
    ) -> Result<JsValue, JsValue> {
        self.ensure_runtime_ready()?;
        let dest_pubkey = dest_pubkey.trim().to_string();
        if dest_pubkey.trim().is_empty() {
            return Err(JsValue::from_str(sdk_contracts::ERR_DEST_PUBKEY_EMPTY));
        }
        if SecpPublicKey::from_str(dest_pubkey.trim()).is_err() {
            return Err(JsValue::from_str(sdk_contracts::ERR_DEST_PUBKEY_INVALID));
        }
        let min_htlc_msat = self.min_htlc_msat_to_dest(&dest_pubkey);
        if amt_msat < min_htlc_msat {
            return Err(JsValue::from_str(&format!(
                "amt_msat cannot be less than {min_htlc_msat}"
            )));
        }
        if (asset_id.is_some() && asset_amount.is_none())
            || (asset_id.is_none() && asset_amount.is_some())
        {
            return Err(JsValue::from_str(
                "asset_id and asset_amount must be provided together",
            ));
        }
        if let Some(id) = &asset_id {
            if id.trim().is_empty() {
                return Err(JsValue::from_str(
                    sdk_contracts::ERR_ASSET_ID_EMPTY_IF_PROVIDED,
                ));
            }
            validate_asset_id_format(id)?;
        }
        if let Some(amount) = asset_amount {
            if amount == 0 {
                return Err(JsValue::from_str(
                    sdk_contracts::ERR_ASSET_AMOUNT_NONPOSITIVE,
                ));
            }
        }

        let (_payment_id, payment_hash) = self.next_payment_identity();
        let payment_preimage = format!("{:064x}", self.next_payment_number());
        let now = unix_now_secs();
        let has_connected_peer = self.has_connected_peer(&dest_pubkey);

        let payee_pubkey = dest_pubkey.clone();
        let data = RlnWasmNodePaymentData {
            amt_msat: Some(amt_msat),
            asset_amount,
            asset_id,
            payment_hash: payment_hash.clone(),
            inbound: false,
            status: "pending".to_string(),
            invoice_type: None,
            preimage: None,
            created_at: now,
            updated_at: now,
            payee_pubkey,
        };

        self.payments
            .borrow_mut()
            .insert(payment_hash.clone(), PaymentEntry { data });
        if let Some(payment) = self
            .payments
            .borrow()
            .get(&payment_hash)
            .map(|entry| entry.data.clone())
        {
            self.register_rgb_ln_transfer_from_payment(&payment);
        }
        self.ldk_runtime.record_keysend_initiated();
        if self.use_runtime_state_for_ln_views() {
            let runtime_payment = self
                .payments
                .borrow()
                .get(&payment_hash)
                .map(|entry| Self::payment_runtime_state_from_data(&entry.data))
                .ok_or_else(|| {
                    JsValue::from_str(sdk_contracts::ERR_PAYMENT_NOT_FOUND_AFTER_KEYSEND)
                })?;
            self.ldk_runtime.upsert_payment(runtime_payment);
        }
        if !has_connected_peer {
            let _ =
                self.apply_payment_status_via_event_stream(&payment_hash, "failed", "node_api")?;
        } else {
            self.emit_runtime_payment_success_event_if_applicable(&payment_hash, &dest_pubkey)?;
        }
        self.persist_runtime_event_log_state();
        let final_status = if self.use_runtime_state_for_ln_views() {
            self.ldk_runtime
                .get_payment(&payment_hash)
                .map(|payment| payment.status)
                .ok_or_else(|| {
                    JsValue::from_str(sdk_contracts::ERR_PAYMENT_NOT_FOUND_AFTER_KEYSEND)
                })?
        } else {
            self.payments
                .borrow()
                .get(&payment_hash)
                .map(|entry| entry.data.status.clone())
                .ok_or_else(|| {
                    JsValue::from_str(sdk_contracts::ERR_PAYMENT_NOT_FOUND_AFTER_KEYSEND)
                })?
        };

        crate::js_obj(&RlnWasmNodeKeysendResult {
            payment_hash,
            payment_preimage,
            status: final_status,
        })
    }

    #[wasm_bindgen(js_name = keysendJson)]
    pub fn keysend_json(
        &self,
        dest_pubkey: String,
        amt_msat: u64,
        asset_id: Option<String>,
        asset_amount: Option<u64>,
    ) -> Result<String, JsValue> {
        let value = self.keysend_value(dest_pubkey, amt_msat, asset_id, asset_amount)?;
        let parsed: serde_json::Value = crate::js_from(value)?;
        crate::js_to_json(&parsed)
    }

    /// Send a REAL keysend (spontaneous) HTLC over a live channel via the wasm `ChannelManager`.
    ///
    /// Unlike [`keysend_value`], which records a parity-model payment that settles from the
    /// event-stream, this constructs and routes an actual HTLC to `dest_pubkey` over the wire. The
    /// returned record starts `pending`; poll [`live_payment_value`] until it becomes `succeeded`
    /// (driven by a real `PaymentSent` event). Pass `asset_id`/`asset_amount` to ride RGB on it.
    #[wasm_bindgen(js_name = keysendLiveValue)]
    pub fn keysend_live_value(
        &self,
        dest_pubkey: String,
        amt_msat: u64,
        asset_id: Option<String>,
        asset_amount: Option<u64>,
    ) -> Result<JsValue, JsValue> {
        self.ensure_runtime_ready()?;
        let dest_pubkey = dest_pubkey.trim().to_string();
        if dest_pubkey.is_empty() {
            return Err(JsValue::from_str(sdk_contracts::ERR_DEST_PUBKEY_EMPTY));
        }
        if SecpPublicKey::from_str(&dest_pubkey).is_err() {
            return Err(JsValue::from_str(sdk_contracts::ERR_DEST_PUBKEY_INVALID));
        }
        let min_htlc_msat = self.min_htlc_msat_to_dest(&dest_pubkey);
        if amt_msat < min_htlc_msat {
            return Err(JsValue::from_str(&format!(
                "amt_msat cannot be less than {min_htlc_msat}"
            )));
        }
        if (asset_id.is_some() && asset_amount.is_none())
            || (asset_id.is_none() && asset_amount.is_some())
        {
            return Err(JsValue::from_str(
                "asset_id and asset_amount must be provided together",
            ));
        }
        if let Some(id) = &asset_id {
            validate_asset_id_format(id)?;
        }
        let record =
            self.ldk_runtime
                .keysend_live(&dest_pubkey, amt_msat, asset_id, asset_amount)?;
        crate::js_obj(&record)
    }

    #[wasm_bindgen(js_name = keysendLiveJson)]
    pub fn keysend_live_json(
        &self,
        dest_pubkey: String,
        amt_msat: u64,
        asset_id: Option<String>,
        asset_amount: Option<u64>,
    ) -> Result<String, JsValue> {
        let value = self.keysend_live_value(dest_pubkey, amt_msat, asset_id, asset_amount)?;
        let parsed: serde_json::Value = crate::js_from(value)?;
        crate::js_to_json(&parsed)
    }

    /// Pay a BOLT11 invoice using the real live `ChannelManager`.
    #[wasm_bindgen(js_name = sendPaymentLiveValue)]
    pub fn send_payment_live_value(
        &self,
        invoice: String,
        amt_msat: Option<u64>,
        asset_id: Option<String>,
        asset_amount: Option<u64>,
    ) -> Result<JsValue, JsValue> {
        self.ensure_runtime_ready()?;
        let invoice = invoice.trim().to_string();
        if invoice.is_empty() {
            return Err(JsValue::from_str(sdk_contracts::ERR_INVOICE_EMPTY));
        }
        let parsed = Bolt11Invoice::from_str(&invoice)
            .map_err(|e| JsValue::from_str(&format!("invalid invoice: {e}")))?;
        if (asset_id.is_some() && asset_amount.is_none())
            || (asset_id.is_none() && asset_amount.is_some())
        {
            return Err(JsValue::from_str(
                "asset_id and asset_amount must be provided together",
            ));
        }
        if let Some(id) = &asset_id {
            validate_asset_id_format(id)?;
        }
        if amt_msat == Some(0) {
            return Err(JsValue::from_str(sdk_contracts::ERR_AMT_MSAT_NONPOSITIVE));
        }
        let record =
            self.ldk_runtime
                .send_bolt11_live(&invoice, amt_msat, asset_id, asset_amount)?;
        crate::js_obj(&RlnWasmNodeSendPaymentResult {
            payment_id: record.payment_hash.clone(),
            payment_hash: Some(record.payment_hash),
            payment_secret: Some(hex::encode(parsed.payment_secret().0)),
            status: record.status,
        })
    }

    #[wasm_bindgen(js_name = sendPaymentLiveJson)]
    pub fn send_payment_live_json(
        &self,
        invoice: String,
        amt_msat: Option<u64>,
        asset_id: Option<String>,
        asset_amount: Option<u64>,
    ) -> Result<String, JsValue> {
        let value = self.send_payment_live_value(invoice, amt_msat, asset_id, asset_amount)?;
        let parsed: serde_json::Value = crate::js_from(value)?;
        crate::js_to_json(&parsed)
    }

    /// Status of a single REAL payment by hex payment hash (from the live event stream), or null.
    #[wasm_bindgen(js_name = livePaymentValue)]
    pub fn live_payment_value(&self, payment_hash: String) -> Result<JsValue, JsValue> {
        self.ensure_runtime_ready()?;
        match self.ldk_runtime.live_payment(payment_hash.trim()) {
            Some(record) => crate::js_obj(&record),
            None => Ok(JsValue::NULL),
        }
    }

    /// Snapshot of all REAL payments tracked from the live `ChannelManager` event stream.
    #[wasm_bindgen(js_name = livePaymentsValue)]
    pub fn live_payments_value(&self) -> Result<JsValue, JsValue> {
        self.ensure_runtime_ready()?;
        let data = self.ldk_runtime.live_payments();
        crate::js_obj(&data)
    }

    #[wasm_bindgen(js_name = listPaymentsValue)]
    pub fn list_payments_value(&self) -> Result<JsValue, JsValue> {
        self.ensure_runtime_ready()?;
        let mut data = if self.use_runtime_state_for_ln_views() {
            self.ldk_runtime
                .list_payments()
                .into_iter()
                .map(Self::payment_data_from_runtime_state)
                .collect::<Vec<_>>()
        } else {
            self.payments
                .borrow()
                .values()
                .map(|entry| entry.data.clone())
                .collect::<Vec<_>>()
        };
        data.sort_by(|a, b| {
            a.created_at
                .cmp(&b.created_at)
                .then_with(|| a.payment_hash.cmp(&b.payment_hash))
        });
        crate::js_obj(&data)
    }

    #[wasm_bindgen(js_name = listPaymentsJson)]
    pub fn list_payments_json(&self) -> Result<String, JsValue> {
        let value = self.list_payments_value()?;
        let parsed: serde_json::Value = crate::js_from(value)?;
        crate::js_to_json(&parsed)
    }

    #[wasm_bindgen(js_name = listRgbLnTransfersValue)]
    pub fn list_rgb_ln_transfers_value(&self) -> Result<JsValue, JsValue> {
        self.ensure_runtime_ready()?;
        let mut data = self
            .rgb_ln_transfers
            .borrow()
            .values()
            .cloned()
            .collect::<Vec<_>>();
        data.sort_by(|a, b| {
            a.created_at
                .cmp(&b.created_at)
                .then_with(|| a.payment_hash.cmp(&b.payment_hash))
        });
        crate::js_obj(&data)
    }

    #[wasm_bindgen(js_name = listRgbLnTransfersJson)]
    pub fn list_rgb_ln_transfers_json(&self) -> Result<String, JsValue> {
        let value = self.list_rgb_ln_transfers_value()?;
        let parsed: serde_json::Value = crate::js_from(value)?;
        crate::js_to_json(&parsed)
    }

    #[wasm_bindgen(js_name = getPaymentValue)]
    pub fn get_payment_value(&self, payment_hash: String) -> Result<JsValue, JsValue> {
        self.ensure_runtime_ready()?;
        let payment_hash = payment_hash.trim().to_string();
        if payment_hash.is_empty() {
            return Err(JsValue::from_str(sdk_contracts::ERR_PAYMENT_HASH_EMPTY));
        }
        let data = if self.use_runtime_state_for_ln_views() {
            self.ldk_runtime
                .get_payment(&payment_hash)
                .map(Self::payment_data_from_runtime_state)
                .ok_or_else(|| JsValue::from_str(sdk_contracts::ERR_PAYMENT_NOT_FOUND))?
        } else {
            self.payments
                .borrow()
                .get(&payment_hash)
                .map(|entry| entry.data.clone())
                .ok_or_else(|| JsValue::from_str(sdk_contracts::ERR_PAYMENT_NOT_FOUND))?
        };
        crate::js_obj(&data)
    }

    #[wasm_bindgen(js_name = getPaymentJson)]
    pub fn get_payment_json(&self, payment_hash: String) -> Result<String, JsValue> {
        let value = self.get_payment_value(payment_hash)?;
        let parsed: serde_json::Value = crate::js_from(value)?;
        crate::js_to_json(&parsed)
    }

    #[wasm_bindgen(js_name = updatePaymentStatus)]
    pub fn update_payment_status(
        &self,
        payment_hash: String,
        status: String,
    ) -> Result<JsValue, JsValue> {
        self.ensure_runtime_ready()?;
        ensure_manual_status_update_allowed(self.use_runtime_state_for_ln_views())?;
        let payment_hash = payment_hash.trim().to_string();
        if payment_hash.is_empty() {
            return Err(JsValue::from_str(sdk_contracts::ERR_PAYMENT_HASH_EMPTY));
        }
        let normalized = normalize_payment_status(&status)?;
        let data =
            self.apply_and_record_payment_status_event(&payment_hash, &normalized, "node_api")?;
        self.persist_runtime_event_log_state();
        crate::js_obj(&data)
    }

    #[wasm_bindgen(js_name = updatePaymentStatusJson)]
    pub fn update_payment_status_json(
        &self,
        payment_hash: String,
        status: String,
    ) -> Result<String, JsValue> {
        let value = self.update_payment_status(payment_hash, status)?;
        let parsed: serde_json::Value = crate::js_from(value)?;
        crate::js_to_json(&parsed)
    }

    #[wasm_bindgen(js_name = decodeLnInvoiceValue)]
    pub fn decode_ln_invoice_value(&self, invoice: String) -> Result<JsValue, JsValue> {
        let invoice = invoice.trim();
        if invoice.is_empty() {
            return Err(JsValue::from_str(sdk_contracts::ERR_INVOICE_EMPTY));
        }
        let parsed = Bolt11Invoice::from_str(invoice)
            .map_err(|e| JsValue::from_str(&format!("invalid invoice: {e}")))?;
        let data = RlnWasmNodeDecodeLnInvoiceData {
            amt_msat: parsed.amount_milli_satoshis(),
            expiry_sec: parsed.expiry_time().as_secs(),
            timestamp: parsed.duration_since_epoch().as_secs(),
            asset_id: parsed.rgb_contract_id().map(|c| c.to_string()),
            asset_amount: parsed.rgb_amount(),
            payment_hash: parsed.payment_hash().to_string(),
            payment_secret: hex::encode(parsed.payment_secret().0),
            payee_pubkey: parsed.payee_pub_key().map(|p| p.to_string()),
            network: format!("{:?}", parsed.network()).to_lowercase(),
        };
        crate::js_obj(&data)
    }

    #[wasm_bindgen(js_name = decodeLnInvoiceJson)]
    pub fn decode_ln_invoice_json(&self, invoice: String) -> Result<String, JsValue> {
        let value = self.decode_ln_invoice_value(invoice)?;
        let parsed: serde_json::Value = crate::js_from(value)?;
        crate::js_to_json(&parsed)
    }

    #[wasm_bindgen(js_name = decodeRgbInvoiceValue)]
    pub fn decode_rgb_invoice_value(&self, invoice: String) -> Result<JsValue, JsValue> {
        let invoice = invoice.trim();
        if invoice.is_empty() {
            return Err(JsValue::from_str(sdk_contracts::ERR_INVOICE_EMPTY));
        }
        let parsed = rgb_lib_wasm::wallet::Invoice::new(invoice.to_string())
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        crate::js_obj(&parsed.invoice_data())
    }

    #[wasm_bindgen(js_name = decodeRgbInvoiceJson)]
    pub fn decode_rgb_invoice_json(&self, invoice: String) -> Result<String, JsValue> {
        let value = self.decode_rgb_invoice_value(invoice)?;
        let parsed: serde_json::Value = crate::js_from(value)?;
        crate::js_to_json(&parsed)
    }

    fn create_ln_invoice_value_internal(
        &self,
        amt_msat: Option<u64>,
        expiry_sec: u32,
        asset_id: Option<String>,
        asset_amount: Option<u64>,
        payment_hash_override: Option<String>,
        invoice_type: &str,
    ) -> Result<JsValue, JsValue> {
        self.ensure_runtime_ready()?;
        if expiry_sec == 0 {
            return Err(JsValue::from_str(sdk_contracts::ERR_EXPIRY_SEC_NONPOSITIVE));
        }
        if let Some(msat) = amt_msat {
            if msat == 0 {
                return Err(JsValue::from_str(sdk_contracts::ERR_AMT_MSAT_NONPOSITIVE));
            }
        }
        if (asset_id.is_some() && asset_amount.is_none())
            || (asset_id.is_none() && asset_amount.is_some())
        {
            return Err(JsValue::from_str(
                "asset_id and asset_amount must be provided together",
            ));
        }
        if let Some(id) = &asset_id {
            if id.trim().is_empty() {
                return Err(JsValue::from_str(
                    sdk_contracts::ERR_ASSET_ID_EMPTY_IF_PROVIDED,
                ));
            }
            validate_asset_id_format(id)?;
        }
        if let Some(amount) = asset_amount {
            if amount == 0 {
                return Err(JsValue::from_str(
                    sdk_contracts::ERR_ASSET_AMOUNT_NONPOSITIVE,
                ));
            }
        }
        if asset_id.is_some() && amt_msat.unwrap_or(0) < SDK_INVOICE_MIN_MSAT {
            return Err(JsValue::from_str(&format!(
                "amt_msat cannot be less than {SDK_INVOICE_MIN_MSAT} when transferring an RGB asset"
            )));
        }

        let (payment_hash, payment_secret) = if let Some(payment_hash_hex) = payment_hash_override {
            let payment_hash_hex = payment_hash_hex.trim().to_string();
            if payment_hash_hex.is_empty() {
                return Err(JsValue::from_str(sdk_contracts::ERR_PAYMENT_HASH_EMPTY));
            }
            let payment_hash_bytes =
                decode_fixed_hex::<32>(&payment_hash_hex, "invalid payment_hash")?;
            if self.use_runtime_state_for_ln_views() {
                if self.ldk_runtime.get_payment(&payment_hash_hex).is_some() {
                    return Err(JsValue::from_str(
                        sdk_contracts::ERR_PAYMENT_HASH_ALREADY_USED,
                    ));
                }
            } else if self.payments.borrow().contains_key(&payment_hash_hex) {
                return Err(JsValue::from_str(
                    sdk_contracts::ERR_PAYMENT_HASH_ALREADY_USED,
                ));
            }
            let payment_hash = Sha256::from_slice(&payment_hash_bytes)
                .map_err(|_| JsValue::from_str(sdk_contracts::ERR_PAYMENT_HASH_INVALID))?;
            let secret_seed = format!(
                "wasm-hodl-secret:{payment_hash_hex}:{}",
                self.next_payment_number()
            );
            let secret_hash = Sha256::hash(secret_seed.as_bytes());
            let mut secret = [0u8; 32];
            secret.copy_from_slice(secret_hash.as_ref());
            (payment_hash, PaymentSecret(secret))
        } else {
            self.next_invoice_payment_identity()
        };
        let now = unix_now_secs();
        let (node_secret_key, node_pubkey) = self.node_signing_identity()?;
        let currency = self.invoice_currency()?;

        let mut builder = InvoiceBuilder::new(currency)
            .description("rln-wasm-sdk".to_string())
            .payment_hash(payment_hash)
            .payment_secret(payment_secret)
            .duration_since_epoch(Duration::from_secs(now))
            .min_final_cltv_expiry_delta(18)
            .expiry_time(Duration::from_secs(expiry_sec as u64));
        if let Some(msat) = amt_msat {
            builder = builder.amount_milli_satoshis(msat);
        }
        let secp_ctx = Secp256k1::new();
        let invoice = builder
            .build_signed(|hash| secp_ctx.sign_ecdsa_recoverable(hash, &node_secret_key))
            .map_err(|e| JsValue::from_str(&format!("failed to create invoice: {e}")))?;

        let payment_hash_hex = invoice.payment_hash().to_string();
        let data = RlnWasmNodePaymentData {
            amt_msat: invoice.amount_milli_satoshis(),
            asset_amount,
            asset_id,
            payment_hash: payment_hash_hex.clone(),
            inbound: true,
            status: "pending".to_string(),
            invoice_type: Some(invoice_type.to_string()),
            preimage: None,
            created_at: now,
            updated_at: now,
            payee_pubkey: node_pubkey.to_string(),
        };
        self.payments
            .borrow_mut()
            .insert(payment_hash_hex, PaymentEntry { data });
        if let Some(payment) = self
            .payments
            .borrow()
            .get(&invoice.payment_hash().to_string())
            .map(|entry| entry.data.clone())
        {
            self.register_rgb_ln_transfer_from_payment(&payment);
        }
        self.ldk_runtime.record_invoice_created();
        if self.use_runtime_state_for_ln_views() {
            let runtime_payment = self
                .payments
                .borrow()
                .get(&invoice.payment_hash().to_string())
                .map(|entry| Self::payment_runtime_state_from_data(&entry.data))
                .ok_or_else(|| {
                    JsValue::from_str(sdk_contracts::ERR_PAYMENT_NOT_FOUND_AFTER_INVOICE_CREATION)
                })?;
            self.ldk_runtime.upsert_payment(runtime_payment);
        }

        crate::js_obj(&RlnWasmNodeCreateLnInvoiceData {
            invoice: invoice.to_string(),
        })
    }

    #[wasm_bindgen(js_name = createLnInvoiceValue)]
    pub fn create_ln_invoice_value(
        &self,
        amt_msat: Option<u64>,
        expiry_sec: u32,
        asset_id: Option<String>,
        asset_amount: Option<u64>,
    ) -> Result<JsValue, JsValue> {
        self.create_ln_invoice_value_internal(
            amt_msat,
            expiry_sec,
            asset_id,
            asset_amount,
            None,
            SDK_INVOICE_TYPE_AUTO_CLAIM,
        )
    }

    #[wasm_bindgen(js_name = createLnInvoiceJson)]
    pub fn create_ln_invoice_json(
        &self,
        amt_msat: Option<u64>,
        expiry_sec: u32,
        asset_id: Option<String>,
        asset_amount: Option<u64>,
    ) -> Result<String, JsValue> {
        let value = self.create_ln_invoice_value(amt_msat, expiry_sec, asset_id, asset_amount)?;
        let parsed: serde_json::Value = crate::js_from(value)?;
        crate::js_to_json(&parsed)
    }

    /// Create a BOLT11 invoice registered with the real live `ChannelManager`.
    #[wasm_bindgen(js_name = createLnInvoiceLiveValue)]
    pub fn create_ln_invoice_live_value(
        &self,
        amt_msat: Option<u64>,
        expiry_sec: u32,
        asset_id: Option<String>,
        asset_amount: Option<u64>,
    ) -> Result<JsValue, JsValue> {
        self.ensure_runtime_ready()?;
        if expiry_sec == 0 {
            return Err(JsValue::from_str(sdk_contracts::ERR_EXPIRY_SEC_NONPOSITIVE));
        }
        if amt_msat == Some(0) {
            return Err(JsValue::from_str(sdk_contracts::ERR_AMT_MSAT_NONPOSITIVE));
        }
        if (asset_id.is_some() && asset_amount.is_none())
            || (asset_id.is_none() && asset_amount.is_some())
        {
            return Err(JsValue::from_str(
                "asset_id and asset_amount must be provided together",
            ));
        }
        if let Some(id) = &asset_id {
            validate_asset_id_format(id)?;
        }
        let invoice = self.ldk_runtime.create_bolt11_invoice_live(
            amt_msat,
            expiry_sec,
            asset_id,
            asset_amount,
        )?;
        self.ldk_runtime.record_invoice_created();
        crate::js_obj(&RlnWasmNodeCreateLnInvoiceData { invoice })
    }

    #[wasm_bindgen(js_name = createLnInvoiceLiveJson)]
    pub fn create_ln_invoice_live_json(
        &self,
        amt_msat: Option<u64>,
        expiry_sec: u32,
        asset_id: Option<String>,
        asset_amount: Option<u64>,
    ) -> Result<String, JsValue> {
        let value =
            self.create_ln_invoice_live_value(amt_msat, expiry_sec, asset_id, asset_amount)?;
        let parsed: serde_json::Value = crate::js_from(value)?;
        crate::js_to_json(&parsed)
    }

    #[wasm_bindgen(js_name = createHodlLnInvoiceValue)]
    pub fn create_hodl_ln_invoice_value(
        &self,
        amt_msat: Option<u64>,
        expiry_sec: u32,
        asset_id: Option<String>,
        asset_amount: Option<u64>,
        payment_hash: String,
    ) -> Result<JsValue, JsValue> {
        if self.use_runtime_state_for_ln_views() {
            self.ensure_runtime_ready()?;
            if expiry_sec == 0 {
                return Err(JsValue::from_str(sdk_contracts::ERR_EXPIRY_SEC_NONPOSITIVE));
            }
            if amt_msat == Some(0) {
                return Err(JsValue::from_str(sdk_contracts::ERR_AMT_MSAT_NONPOSITIVE));
            }
            if (asset_id.is_some() && asset_amount.is_none())
                || (asset_id.is_none() && asset_amount.is_some())
            {
                return Err(JsValue::from_str(
                    "asset_id and asset_amount must be provided together",
                ));
            }
            if let Some(id) = &asset_id {
                validate_asset_id_format(id)?;
            }
            if asset_id.is_some() && amt_msat.unwrap_or(0) < SDK_INVOICE_MIN_MSAT {
                return Err(JsValue::from_str(&format!(
                    "amt_msat cannot be less than {SDK_INVOICE_MIN_MSAT} when transferring an RGB asset"
                )));
            }
            let payment_hash = payment_hash.trim().to_string();
            decode_fixed_hex::<32>(&payment_hash, "invalid payment_hash")?;
            let invoice = self.ldk_runtime.create_hodl_bolt11_invoice_live(
                amt_msat,
                expiry_sec,
                asset_id,
                asset_amount,
                &payment_hash,
            )?;
            self.ldk_runtime.record_invoice_created();
            return crate::js_obj(&RlnWasmNodeCreateLnInvoiceData { invoice });
        }
        self.create_ln_invoice_value_internal(
            amt_msat,
            expiry_sec,
            asset_id,
            asset_amount,
            Some(payment_hash),
            SDK_INVOICE_TYPE_HODL,
        )
    }

    #[wasm_bindgen(js_name = createHodlLnInvoiceJson)]
    pub fn create_hodl_ln_invoice_json(
        &self,
        amt_msat: Option<u64>,
        expiry_sec: u32,
        asset_id: Option<String>,
        asset_amount: Option<u64>,
        payment_hash: String,
    ) -> Result<String, JsValue> {
        let value = self.create_hodl_ln_invoice_value(
            amt_msat,
            expiry_sec,
            asset_id,
            asset_amount,
            payment_hash,
        )?;
        let parsed: serde_json::Value = crate::js_from(value)?;
        crate::js_to_json(&parsed)
    }

    #[wasm_bindgen(js_name = cancelHodlInvoiceValue)]
    pub fn cancel_hodl_invoice_value(&self, payment_hash: String) -> Result<JsValue, JsValue> {
        self.ensure_runtime_ready()?;
        let payment_hash = payment_hash.trim().to_string();
        if payment_hash.is_empty() {
            return Err(JsValue::from_str(sdk_contracts::ERR_PAYMENT_HASH_EMPTY));
        }
        decode_fixed_hex::<32>(&payment_hash, "invalid payment_hash")?;
        if self.use_runtime_state_for_ln_views() {
            self.ldk_runtime.cancel_hodl_invoice_live(&payment_hash)?;
            return crate::js_obj(&serde_json::json!({}));
        }
        let mut payment = if self.use_runtime_state_for_ln_views() {
            self.ldk_runtime
                .get_payment(&payment_hash)
                .map(Self::payment_data_from_runtime_state)
                .ok_or_else(|| JsValue::from_str(sdk_contracts::ERR_LN_INVOICE_UNKNOWN))?
        } else {
            self.payments
                .borrow()
                .get(&payment_hash)
                .map(|entry| entry.data.clone())
                .ok_or_else(|| JsValue::from_str(sdk_contracts::ERR_LN_INVOICE_UNKNOWN))?
        };
        if !matches!(payment.invoice_type.as_deref(), Some(SDK_INVOICE_TYPE_HODL)) {
            return Err(JsValue::from_str(sdk_contracts::ERR_LN_INVOICE_NOT_HODL));
        }
        match payment.status.as_str() {
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
        payment.status = "cancelled".to_string();
        payment.updated_at = unix_now_secs();
        self.sync_rgb_ln_transfer_from_payment(&payment);
        if self.use_runtime_state_for_ln_views() {
            self.ldk_runtime
                .upsert_payment(Self::payment_runtime_state_from_data(&payment));
        } else {
            self.payments
                .borrow_mut()
                .insert(payment_hash, PaymentEntry { data: payment });
        }
        self.persist_runtime_event_log_state();
        crate::js_obj(&serde_json::json!({}))
    }

    #[wasm_bindgen(js_name = cancelHodlInvoiceJson)]
    pub fn cancel_hodl_invoice_json(&self, payment_hash: String) -> Result<String, JsValue> {
        let value = self.cancel_hodl_invoice_value(payment_hash)?;
        let parsed: serde_json::Value = crate::js_from(value)?;
        crate::js_to_json(&parsed)
    }

    #[wasm_bindgen(js_name = claimHodlInvoiceValue)]
    pub fn claim_hodl_invoice_value(
        &self,
        payment_hash: String,
        payment_preimage: String,
    ) -> Result<JsValue, JsValue> {
        self.ensure_runtime_ready()?;
        let payment_hash = payment_hash.trim().to_string();
        if payment_hash.is_empty() {
            return Err(JsValue::from_str(sdk_contracts::ERR_PAYMENT_HASH_EMPTY));
        }
        let payment_hash_bytes = decode_fixed_hex::<32>(&payment_hash, "invalid payment_hash")?;
        let payment_preimage = payment_preimage.trim().to_string();
        if payment_preimage.is_empty() {
            return Err(JsValue::from_str(sdk_contracts::ERR_PAYMENT_PREIMAGE_EMPTY));
        }
        let payment_preimage_bytes =
            decode_fixed_hex::<32>(&payment_preimage, "invalid payment_preimage")?;
        let computed_hash = Sha256::hash(&payment_preimage_bytes);
        if computed_hash.to_byte_array() != payment_hash_bytes {
            return Err(JsValue::from_str(
                sdk_contracts::ERR_PAYMENT_PREIMAGE_INVALID,
            ));
        }
        if self.use_runtime_state_for_ln_views() {
            let changed = self
                .ldk_runtime
                .claim_hodl_invoice_live(&payment_hash, &payment_preimage)?;
            return crate::js_obj(&RlnWasmNodeClaimHodlInvoiceData { changed });
        }

        let mut payment = if self.use_runtime_state_for_ln_views() {
            self.ldk_runtime
                .get_payment(&payment_hash)
                .map(Self::payment_data_from_runtime_state)
                .ok_or_else(|| JsValue::from_str(sdk_contracts::ERR_LN_INVOICE_UNKNOWN))?
        } else {
            self.payments
                .borrow()
                .get(&payment_hash)
                .map(|entry| entry.data.clone())
                .ok_or_else(|| JsValue::from_str(sdk_contracts::ERR_LN_INVOICE_UNKNOWN))?
        };
        if !matches!(payment.invoice_type.as_deref(), Some(SDK_INVOICE_TYPE_HODL)) {
            return Err(JsValue::from_str(sdk_contracts::ERR_LN_INVOICE_NOT_HODL));
        }
        match payment.status.as_str() {
            "succeeded" => {
                if let Some(stored_preimage) = payment.preimage.as_deref() {
                    if stored_preimage != payment_preimage {
                        return Err(JsValue::from_str(
                            sdk_contracts::ERR_PAYMENT_PREIMAGE_INVALID,
                        ));
                    }
                }
                return crate::js_obj(&RlnWasmNodeClaimHodlInvoiceData { changed: false });
            }
            "claiming" => return Err(JsValue::from_str(sdk_contracts::ERR_LN_INVOICE_SETTLING)),
            "claimable" => {}
            _ => {
                return Err(JsValue::from_str(
                    sdk_contracts::ERR_LN_INVOICE_NOT_CLAIMABLE,
                ))
            }
        }

        payment.status = "claiming".to_string();
        payment.updated_at = unix_now_secs();
        payment.preimage = Some(payment_preimage);
        self.sync_rgb_ln_transfer_from_payment(&payment);
        payment.status = "succeeded".to_string();
        payment.updated_at = unix_now_secs();
        self.sync_rgb_ln_transfer_from_payment(&payment);

        if self.use_runtime_state_for_ln_views() {
            self.ldk_runtime
                .upsert_payment(Self::payment_runtime_state_from_data(&payment));
        } else {
            self.payments
                .borrow_mut()
                .insert(payment_hash, PaymentEntry { data: payment });
        }
        self.persist_runtime_event_log_state();
        crate::js_obj(&RlnWasmNodeClaimHodlInvoiceData { changed: true })
    }

    #[wasm_bindgen(js_name = claimHodlInvoiceJson)]
    pub fn claim_hodl_invoice_json(
        &self,
        payment_hash: String,
        payment_preimage: String,
    ) -> Result<String, JsValue> {
        let value = self.claim_hodl_invoice_value(payment_hash, payment_preimage)?;
        let parsed: serde_json::Value = crate::js_from(value)?;
        crate::js_to_json(&parsed)
    }

    #[wasm_bindgen(js_name = invoiceStatusValue)]
    pub fn invoice_status_value(&self, invoice: String) -> Result<JsValue, JsValue> {
        self.ensure_runtime_ready()?;
        let invoice = invoice.trim();
        if invoice.is_empty() {
            return Err(JsValue::from_str(sdk_contracts::ERR_INVOICE_EMPTY));
        }
        let parsed = Bolt11Invoice::from_str(invoice)
            .map_err(|e| JsValue::from_str(&format!("invalid invoice: {e}")))?;
        let payment_hash = parsed.payment_hash().to_string();
        let payment = if self.use_runtime_state_for_ln_views() {
            self.ldk_runtime
                .get_payment(&payment_hash)
                .map(Self::payment_data_from_runtime_state)
                .ok_or_else(|| JsValue::from_str(sdk_contracts::ERR_LN_INVOICE_UNKNOWN))?
        } else {
            self.payments
                .borrow()
                .get(&payment_hash)
                .map(|entry| entry.data.clone())
                .ok_or_else(|| JsValue::from_str(sdk_contracts::ERR_LN_INVOICE_UNKNOWN))?
        };
        // `Bolt11Invoice::is_expired()` reads the system clock via `SystemTime::now()`, which is
        // unimplemented on wasm32 and traps ("time not implemented on this platform"), poisoning the
        // whole node. Use the explicit-time variant with the crate's cfg-gated `unix_now_secs()`
        // helper instead — same semantics, no `SystemTime`.
        if payment.status == "pending" && parsed.would_expire(Duration::from_secs(unix_now_secs()))
        {
            let _ =
                self.apply_payment_status_via_event_stream(&payment_hash, "expired", "node_api")?;
        }
        let status = if self.use_runtime_state_for_ln_views() {
            self.ldk_runtime
                .get_payment(&payment_hash)
                .map(|entry| entry.status)
                .ok_or_else(|| JsValue::from_str(sdk_contracts::ERR_LN_INVOICE_UNKNOWN))?
        } else {
            self.payments
                .borrow()
                .get(&payment_hash)
                .map(|entry| entry.data.status.clone())
                .ok_or_else(|| JsValue::from_str(sdk_contracts::ERR_LN_INVOICE_UNKNOWN))?
        };
        crate::js_obj(&RlnWasmNodeInvoiceStatusData { status })
    }

    #[wasm_bindgen(js_name = invoiceStatusJson)]
    pub fn invoice_status_json(&self, invoice: String) -> Result<String, JsValue> {
        let value = self.invoice_status_value(invoice)?;
        let parsed: serde_json::Value = crate::js_from(value)?;
        crate::js_to_json(&parsed)
    }

    #[wasm_bindgen(js_name = updatePaymentStatusByInvoice)]
    pub fn update_payment_status_by_invoice(
        &self,
        invoice: String,
        status: String,
    ) -> Result<JsValue, JsValue> {
        self.ensure_runtime_ready()?;
        ensure_manual_status_update_allowed(self.use_runtime_state_for_ln_views())?;
        let invoice = invoice.trim();
        if invoice.is_empty() {
            return Err(JsValue::from_str(sdk_contracts::ERR_INVOICE_EMPTY));
        }
        let parsed = Bolt11Invoice::from_str(invoice)
            .map_err(|e| JsValue::from_str(&format!("invalid invoice: {e}")))?;
        let payment_hash = parsed.payment_hash().to_string();
        self.update_payment_status(payment_hash, status)
    }

    #[wasm_bindgen(js_name = updatePaymentStatusByInvoiceJson)]
    pub fn update_payment_status_by_invoice_json(
        &self,
        invoice: String,
        status: String,
    ) -> Result<String, JsValue> {
        let value = self.update_payment_status_by_invoice(invoice, status)?;
        let parsed: serde_json::Value = crate::js_from(value)?;
        crate::js_to_json(&parsed)
    }

    #[wasm_bindgen(js_name = ingestReadEventPayloadHex)]
    pub fn ingest_read_event_payload_hex(&self, payload_hex: String) -> Result<JsValue, JsValue> {
        self.ensure_runtime_ready()?;
        ensure_manual_event_ingestion_allowed(self.use_runtime_state_for_ln_views())?;
        if self.use_runtime_state_for_ln_views() {
            let received_at = unix_now_secs();
            let seq = next_runtime_event_seq(&self.next_runtime_event_seq);
            let Some(event) = parse_payment_status_event_payload(&payload_hex) else {
                let error =
                    "unrecognized event payload format for payment status update".to_string();
                let event_kind = classify_non_payment_payload_kind(&payload_hex);
                record_runtime_event(
                    &self.runtime_events,
                    RlnWasmNodeRuntimeEventData {
                        seq,
                        source: "manual_api".to_string(),
                        event_kind,
                        payload_hex,
                        payment_hash: None,
                        status: None,
                        applied: false,
                        error: Some(error.clone()),
                        received_at,
                    },
                );
                self.persist_runtime_event_log_state();
                return Err(JsValue::from_str(&error));
            };
            let result = self
                .apply_and_record_payment_status_event(
                    &event.payment_hash,
                    &event.status,
                    "manual_api",
                )
                .and_then(|payment| crate::js_obj(&payment));
            self.persist_runtime_event_log_state();
            return result;
        }
        let maybe_payment = apply_runtime_event_payload(
            &self.payments,
            &self.runtime_events,
            &self.next_runtime_event_seq,
            payload_hex,
            "manual_api",
            RuntimeEventApplyMode::StrictPaymentStatus,
        )?;
        self.persist_runtime_event_log_state();
        let payment =
            maybe_payment.ok_or_else(|| JsValue::from_str(sdk_contracts::ERR_PAYMENT_NOT_FOUND))?;
        crate::js_obj(&payment)
    }

    #[wasm_bindgen(js_name = ingestReadEventPayloadHexJson)]
    pub fn ingest_read_event_payload_hex_json(
        &self,
        payload_hex: String,
    ) -> Result<String, JsValue> {
        let value = self.ingest_read_event_payload_hex(payload_hex)?;
        let parsed: serde_json::Value = crate::js_from(value)?;
        crate::js_to_json(&parsed)
    }

    #[wasm_bindgen(js_name = ingestRuntimeTransportEventPayloadHexValue)]
    pub fn ingest_runtime_transport_event_payload_hex_value(
        &self,
        payload_hex: String,
    ) -> Result<JsValue, JsValue> {
        self.ensure_runtime_ready()?;
        ensure_manual_event_ingestion_allowed(self.use_runtime_state_for_ln_views())?;
        let result = self.apply_and_record_transport_event_from_payload_hex(
            payload_hex,
            "runtime_transport_api",
        )?;
        self.persist_runtime_event_log_state();
        crate::js_obj(&result)
    }

    #[wasm_bindgen(js_name = ingestRuntimeTransportEventPayloadHexJson)]
    pub fn ingest_runtime_transport_event_payload_hex_json(
        &self,
        payload_hex: String,
    ) -> Result<String, JsValue> {
        let value = self.ingest_runtime_transport_event_payload_hex_value(payload_hex)?;
        let parsed: serde_json::Value = crate::js_from(value)?;
        crate::js_to_json(&parsed)
    }

    #[wasm_bindgen(js_name = openChannelValue)]
    pub fn open_channel_value(
        &self,
        peer_pubkey: String,
        capacity_sat: u64,
        public: bool,
        asset_id: Option<String>,
        asset_local_amount: Option<u64>,
    ) -> Result<JsValue, JsValue> {
        self.open_channel_value_with_options(
            peer_pubkey,
            capacity_sat,
            public,
            asset_id,
            asset_local_amount,
            None,
            None,
            None,
        )
    }

    #[wasm_bindgen(js_name = openChannelValueWithOptions)]
    pub fn open_channel_value_with_options(
        &self,
        peer_pubkey: String,
        capacity_sat: u64,
        public: bool,
        asset_id: Option<String>,
        asset_local_amount: Option<u64>,
        virtual_open_mode: Option<String>,
        contract_id: Option<String>,
        consignment_endpoint: Option<String>,
    ) -> Result<JsValue, JsValue> {
        self.ensure_runtime_ready()?;
        self.ensure_stable_identity_for_channel_operations()?;
        let peer_pubkey = peer_pubkey.trim().to_string();
        wasm_debug(&format!(
            "[rln-wasm-sdk openChannel] start peer_pubkey={} capacity_sat={} public={} asset_id={:?} asset_local_amount={:?} virtual_open_mode={:?}",
            peer_pubkey, capacity_sat, public, asset_id, asset_local_amount, virtual_open_mode
        ));
        if peer_pubkey.trim().is_empty() {
            return Err(JsValue::from_str(sdk_contracts::ERR_PEER_PUBKEY_EMPTY));
        }
        if SecpPublicKey::from_str(peer_pubkey.trim()).is_err() {
            return Err(JsValue::from_str(sdk_contracts::ERR_PEER_PUBKEY_INVALID));
        }
        let normalized_virtual_open_mode = match virtual_open_mode {
            None => None,
            Some(mode) => {
                let mode = mode.trim().to_string();
                if mode.is_empty() {
                    return Err(JsValue::from_str(
                        sdk_contracts::ERR_VIRTUAL_OPEN_MODE_EMPTY,
                    ));
                }
                if mode != SDK_VIRTUAL_OPEN_MODE_TRUSTED_NO_BROADCAST {
                    return Err(JsValue::from_str(&format!(
                        "unknown virtual_open_mode: {mode}"
                    )));
                }
                Some(mode)
            }
        };
        let is_virtual_open = normalized_virtual_open_mode.is_some();
        if is_virtual_open && !*self.enable_virtual_channels_v0.borrow() {
            return Err(JsValue::from_str(
                "trusted virtual channels v0 are disabled",
            ));
        }
        if is_virtual_open && public {
            return Err(JsValue::from_str(
                sdk_contracts::ERR_VIRTUAL_CHANNELS_PUBLIC_FALSE,
            ));
        }
        if let Some(id) = &asset_id {
            if id.trim().is_empty() {
                return Err(JsValue::from_str(
                    sdk_contracts::ERR_ASSET_ID_EMPTY_IF_PROVIDED,
                ));
            }
        }
        let has_rgb = match (&asset_id, asset_local_amount) {
            (Some(_), Some(amount)) if amount < SDK_OPENCHANNEL_MIN_RGB_AMT => {
                return Err(JsValue::from_str(&format!(
                    "Channel RGB amount must be equal to or higher than {SDK_OPENCHANNEL_MIN_RGB_AMT}"
                )));
            }
            (Some(id), Some(_)) => {
                validate_asset_id_format(id)?;
                true
            }
            (None, None) => false,
            _ => {
                return Err(JsValue::from_str(
                    "asset_id and asset_local_amount must be provided together",
                ));
            }
        };
        if has_rgb && capacity_sat < SDK_OPENRGBCHANNEL_MIN_SAT {
            return Err(JsValue::from_str(&format!(
                "RGB channel amount must be equal to or higher than {SDK_OPENRGBCHANNEL_MIN_SAT} sats"
            )));
        }
        if !has_rgb && capacity_sat < SDK_OPENCHANNEL_MIN_SAT {
            return Err(JsValue::from_str(&format!(
                "Channel amount must be equal to or higher than {SDK_OPENCHANNEL_MIN_SAT} sats"
            )));
        }
        if capacity_sat > SDK_OPENCHANNEL_MAX_SAT {
            return Err(JsValue::from_str(&format!(
                "Channel amount must be equal to or less than {SDK_OPENCHANNEL_MAX_SAT} sats"
            )));
        }
        let has_peer = if self.use_runtime_state_for_ln_views() {
            self.ldk_runtime.has_connected_peer(&peer_pubkey)
        } else {
            self.peers.borrow().contains_key(&peer_pubkey)
        };
        wasm_debug(&format!(
            "[rln-wasm-sdk openChannel] peer readiness peer_pubkey={} has_peer={} runtime_mode={}",
            peer_pubkey,
            has_peer,
            self.use_runtime_state_for_ln_views()
        ));
        if !has_peer {
            return Err(JsValue::from_str(sdk_contracts::ERR_PEER_NOT_CONNECTED));
        }
        let mut seq = self.next_channel_seq.borrow_mut();
        *seq += 1;
        let next = *seq;

        let default_channel_id = format!("wasm-chan:{}:{}", peer_pubkey, next);
        let mut channel_id = default_channel_id.clone();
        let mut temporary_channel_id = format!("wasm-tmp:{}:{}", peer_pubkey, next);
        let mut non_virtual_status = "pending".to_string();
        let mut non_virtual_ready = false;
        let mut non_virtual_usable = false;
        // Derive the real RGB asset schema from wallet metadata so the colored channel records
        // the correct schema rather than a hardcoded one. Best-effort: on any lookup failure the
        // runtime defaults to Nia (both wasm-supported schemas are fungible).
        let asset_schema = if asset_id.is_some() {
            contract_id
                .clone()
                .or_else(|| asset_id.clone())
                .and_then(|id| {
                    self.with_attached_wallet(|wallet| {
                        Ok(wallet
                            .get_asset_metadata(id)
                            .ok()
                            .map(|m| match m.asset_schema {
                                rgb_lib_wasm::AssetSchema::Ifa => "ifa".to_string(),
                                _ => "nia".to_string(),
                            }))
                    })
                    .ok()
                    .flatten()
                })
        } else {
            None
        };
        if !is_virtual_open {
            let opened =
                self.ldk_runtime
                    .open_channel_non_virtual(LdkRuntimeOpenChannelRequestData {
                        peer_pubkey: peer_pubkey.clone(),
                        capacity_sat,
                        public,
                        asset_id: asset_id.clone(),
                        asset_local_amount,
                        contract_id: contract_id.clone(),
                        consignment_endpoint: consignment_endpoint.clone(),
                        asset_schema: asset_schema.clone(),
                    })?;
            let temp = opened.temporary_channel_id.trim().to_string();
            let chan = opened.channel_id.trim().to_string();
            if !temp.is_empty() {
                temporary_channel_id = temp;
            }
            if !chan.is_empty() {
                channel_id = chan;
            }
            if !opened.status.trim().is_empty() {
                non_virtual_status = opened.status;
            }
            non_virtual_ready = opened.ready;
            non_virtual_usable = opened.is_usable;
        }
        let reserved_temporary_channel_id = if is_virtual_open {
            Some(
                self.ldk_runtime
                    .virtual_channel_add_intent(&peer_pubkey, Some(temporary_channel_id.clone()))
                    .map_err(|e| JsValue::from_str(&e))?,
            )
        } else {
            None
        };
        wasm_debug(&format!(
            "[rln-wasm-sdk openChannel] allocated ids temp_id={} channel_id={} is_virtual={}",
            reserved_temporary_channel_id
                .clone()
                .unwrap_or_else(|| temporary_channel_id.clone()),
            channel_id,
            is_virtual_open
        ));
        let data = RlnWasmNodeChannelData {
            temporary_channel_id: reserved_temporary_channel_id
                .clone()
                .unwrap_or_else(|| temporary_channel_id.clone()),
            channel_id: channel_id.clone(),
            peer_pubkey: peer_pubkey.clone(),
            status: if is_virtual_open {
                "opening".to_string()
            } else {
                non_virtual_status
            },
            ready: if is_virtual_open {
                false
            } else {
                non_virtual_ready
            },
            is_usable: if is_virtual_open {
                false
            } else {
                non_virtual_usable
            },
            public,
            capacity_sat,
            asset_id,
            asset_local_amount,
            virtual_open_mode: normalized_virtual_open_mode,
            // A freshly-opened channel has no spendable outbound yet; `list_channels_value` overlays
            // live balances from the channel manager once the channel is usable.
            outbound_msat: 0,
            next_outbound_htlc_limit_msat: 0,
        };

        if self.use_runtime_state_for_ln_views() {
            self.ldk_runtime
                .upsert_channel(Self::channel_runtime_state_from_data(&data));
        }
        // Also maintain the local channel view as a cache for WASM consumers. In wasm-native mode,
        // the runtime view may be delayed; keeping this cache prevents `listChannelsJson()` from
        // temporarily returning an empty list.
        self.channels.borrow_mut().insert(
            channel_id.clone(),
            ChannelEntry {
                temporary_channel_id: temporary_channel_id.clone(),
                data: data.clone(),
            },
        );
        if is_virtual_open {
            self.ldk_runtime.virtual_channel_session_add_from_open(
                &channel_id,
                reserved_temporary_channel_id
                    .as_deref()
                    .unwrap_or(&temporary_channel_id),
                &data.peer_pubkey,
            );
            self.register_trusted_virtual_scope_channel(&channel_id, &peer_pubkey);
            let queued_event = RuntimeTransportEvent::ChannelUsable {
                channel_id: channel_id.clone(),
            };
            let payload_json = serde_json::to_string(&queued_event).map_err(|e| {
                JsValue::from_str(&format!("failed to serialize runtime event: {e}"))
            })?;
            let payload_hex = hex::encode(payload_json.as_bytes());
            let _ = self
                .runtime_core
                .enqueue_event("channel_usable".to_string(), payload_hex);
        }
        self.ldk_runtime.record_channel_opened();
        self.persist_runtime_event_log_state();
        let channel = if self.use_runtime_state_for_ln_views() {
            self.ldk_runtime
                .list_channels()
                .into_iter()
                .find(|entry| entry.channel_id == channel_id)
                .map(Self::channel_data_from_runtime_state)
                .ok_or_else(|| JsValue::from_str(sdk_contracts::ERR_CHANNEL_NOT_FOUND_AFTER_OPEN))?
        } else {
            self.channels
                .borrow()
                .get(&channel_id)
                .map(|entry| entry.data.clone())
                .ok_or_else(|| JsValue::from_str(sdk_contracts::ERR_CHANNEL_NOT_FOUND_AFTER_OPEN))?
        };
        wasm_debug(&format!(
            "[rln-wasm-sdk openChannel] done channel_id={} status={} ready={} usable={}",
            channel.channel_id, channel.status, channel.ready, channel.is_usable
        ));
        crate::js_obj(&channel)
    }

    #[wasm_bindgen(js_name = openChannelJson)]
    pub fn open_channel_json(
        &self,
        peer_pubkey: String,
        capacity_sat: u64,
        public: bool,
        asset_id: Option<String>,
        asset_local_amount: Option<u64>,
    ) -> Result<String, JsValue> {
        let value = self.open_channel_value_with_options(
            peer_pubkey,
            capacity_sat,
            public,
            asset_id,
            asset_local_amount,
            None,
            None,
            None,
        )?;
        let parsed: serde_json::Value = crate::js_from(value)?;
        crate::js_to_json(&parsed)
    }

    #[wasm_bindgen(js_name = openChannelJsonWithOptions)]
    pub fn open_channel_json_with_options(
        &self,
        peer_pubkey: String,
        capacity_sat: u64,
        public: bool,
        asset_id: Option<String>,
        asset_local_amount: Option<u64>,
        virtual_open_mode: Option<String>,
    ) -> Result<String, JsValue> {
        let value = self.open_channel_value_with_options(
            peer_pubkey,
            capacity_sat,
            public,
            asset_id,
            asset_local_amount,
            virtual_open_mode,
            None,
            None,
        )?;
        let parsed: serde_json::Value = crate::js_from(value)?;
        crate::js_to_json(&parsed)
    }

    #[wasm_bindgen(js_name = openChannelRgbJson)]
    pub fn open_channel_rgb_json(
        &self,
        peer_pubkey: String,
        capacity_sat: u64,
        public: bool,
        asset_id: String,
        asset_local_amount: u64,
        contract_id: String,
        consignment_endpoint: String,
    ) -> Result<String, JsValue> {
        let value = self.open_channel_value_with_options(
            peer_pubkey,
            capacity_sat,
            public,
            Some(asset_id),
            Some(asset_local_amount),
            None,
            Some(contract_id),
            Some(consignment_endpoint),
        )?;
        let parsed: serde_json::Value = crate::js_from(value)?;
        crate::js_to_json(&parsed)
    }

    #[wasm_bindgen(js_name = driveRgbFundingWork)]
    pub async fn drive_rgb_funding_work(&self) -> Result<(), JsValue> {
        self.ldk_runtime.drive_rgb_funding_work_boxed().await
    }

    #[wasm_bindgen(js_name = processPendingRgbTransactions)]
    pub async fn process_pending_rgb_transactions(&self) -> Result<(), JsValue> {
        self.ldk_runtime
            .process_pending_rgb_transactions_boxed()
            .await
    }

    /// Async payments with LSP: register a fresh batch of payment hashes with the invoice-host /
    /// LSP peer (`async_order.new`). Returns the host's order acknowledgement. Mirrors the native
    /// SDK's `apay_new` / `/apay/new`.
    #[wasm_bindgen(js_name = apayNewValue)]
    pub async fn apay_new_value(&self, host_node_id: String) -> Result<JsValue, JsValue> {
        self.ensure_runtime_ready()?;
        let response = self
            .ldk_runtime
            .apay_new_boxed(host_node_id, None, None)
            .await?;
        crate::js_obj(&response)
    }

    #[wasm_bindgen(js_name = apayNewJson)]
    pub async fn apay_new_json(&self, host_node_id: String) -> Result<String, JsValue> {
        let value = self.apay_new_value(host_node_id).await?;
        let parsed: serde_json::Value = crate::js_from(value)?;
        crate::js_to_json(&parsed)
    }

    /// Like [`Self::apay_new_value`] but also attests a `username@domain` Lightning Address so the
    /// LSP can serve inbound payments to that address. Mirrors `apay_new_with_address`.
    #[wasm_bindgen(js_name = apayNewWithAddressValue)]
    pub async fn apay_new_with_address_value(
        &self,
        host_node_id: String,
        username: String,
        domain: String,
    ) -> Result<JsValue, JsValue> {
        self.ensure_runtime_ready()?;
        let response = self
            .ldk_runtime
            .apay_new_boxed(host_node_id, Some(username), Some(domain))
            .await?;
        crate::js_obj(&response)
    }

    #[wasm_bindgen(js_name = apayNewWithAddressJson)]
    pub async fn apay_new_with_address_json(
        &self,
        host_node_id: String,
        username: String,
        domain: String,
    ) -> Result<String, JsValue> {
        let value = self
            .apay_new_with_address_value(host_node_id, username, domain)
            .await?;
        let parsed: serde_json::Value = crate::js_from(value)?;
        crate::js_to_json(&parsed)
    }

    #[wasm_bindgen(js_name = closeChannel)]
    pub fn close_channel(&self, channel_id: String) -> Result<(), JsValue> {
        self.close_channel_with_options(channel_id, None, false)
    }

    #[wasm_bindgen(js_name = closeChannelWithOptions)]
    pub fn close_channel_with_options(
        &self,
        channel_id: String,
        peer_pubkey: Option<String>,
        force: bool,
    ) -> Result<(), JsValue> {
        self.ensure_runtime_ready()?;
        if channel_id.trim().is_empty() {
            return Err(JsValue::from_str(sdk_contracts::ERR_CHANNEL_ID_EMPTY));
        }
        let virtual_session = self.ldk_runtime.virtual_channel_session_get(&channel_id);
        if let Some(session) = virtual_session.as_ref() {
            let Some(peer_pubkey) = peer_pubkey.as_ref() else {
                return Err(JsValue::from_str(
                    "peer_pubkey is required for trusted virtual channel close",
                ));
            };
            let peer_pubkey = peer_pubkey.trim().to_string();
            if peer_pubkey.trim().is_empty() {
                return Err(JsValue::from_str(sdk_contracts::ERR_PEER_PUBKEY_EMPTY));
            }
            if SecpPublicKey::from_str(peer_pubkey.trim()).is_err() {
                return Err(JsValue::from_str(sdk_contracts::ERR_PEER_PUBKEY_INVALID));
            }
            if session.peer_pubkey != peer_pubkey {
                return Err(JsValue::from_str(
                    "peer pubkey does not match trusted virtual channel session",
                ));
            }
        }
        let channel = if self.use_runtime_state_for_ln_views() {
            self.ldk_runtime
                .list_channels()
                .into_iter()
                .find(|channel| channel.channel_id == channel_id)
                .map(Self::channel_data_from_runtime_state)
        } else {
            self.channels
                .borrow()
                .get(&channel_id)
                .map(|entry| entry.data.clone())
        };
        let Some(channel) = channel else {
            if let Some(session) = virtual_session.as_ref() {
                if session.status != LdkRuntimeVirtualChannelSessionStatusData::Abandoned {
                    let _ = self.ldk_runtime.virtual_channel_session_update_status(
                        &channel_id,
                        LdkRuntimeVirtualChannelSessionStatusData::Abandoned,
                    );
                }
                self.unregister_trusted_virtual_scope_channel(&channel_id);
                self.persist_runtime_event_log_state();
                return Ok(());
            }
            return Err(JsValue::from_str(sdk_contracts::ERR_CHANNEL_NOT_FOUND));
        };
        if virtual_session.is_some()
            && channel.virtual_open_mode.as_deref()
                != Some(SDK_VIRTUAL_OPEN_MODE_TRUSTED_NO_BROADCAST)
        {
            return Err(JsValue::from_str(&format!(
                "virtual channel session exists for {channel_id}, but live channel is not trusted_no_broadcast"
            )));
        }
        if let Some(ref peer_pubkey) = peer_pubkey {
            let peer_pubkey = peer_pubkey.trim().to_string();
            if peer_pubkey.trim().is_empty() {
                return Err(JsValue::from_str(sdk_contracts::ERR_PEER_PUBKEY_EMPTY));
            }
            if SecpPublicKey::from_str(peer_pubkey.trim()).is_err() {
                return Err(JsValue::from_str(sdk_contracts::ERR_PEER_PUBKEY_INVALID));
            }
            let channel_matches_peer = channel.peer_pubkey == peer_pubkey;
            if !channel_matches_peer {
                return Err(JsValue::from_str(
                    "cannot find the channel with the provided peer pubkey",
                ));
            }
        }
        let is_virtual_channel = channel.virtual_open_mode.as_deref()
            == Some(SDK_VIRTUAL_OPEN_MODE_TRUSTED_NO_BROADCAST);
        if is_virtual_channel {
            if !*self.enable_virtual_channels_v0.borrow() {
                return Err(JsValue::from_str(
                    "trusted virtual channels v0 are disabled",
                ));
            }
            let Some(peer_pubkey) = peer_pubkey.as_ref() else {
                return Err(JsValue::from_str(
                    "peer_pubkey is required for trusted virtual channel close",
                ));
            };
            let peer_pubkey = peer_pubkey.trim().to_string();
            if peer_pubkey.trim().is_empty() {
                return Err(JsValue::from_str(sdk_contracts::ERR_PEER_PUBKEY_EMPTY));
            }
            if SecpPublicKey::from_str(peer_pubkey.trim()).is_err() {
                return Err(JsValue::from_str(sdk_contracts::ERR_PEER_PUBKEY_INVALID));
            }
            if channel.peer_pubkey != peer_pubkey {
                return Err(JsValue::from_str(
                    "cannot find the channel with the provided peer pubkey",
                ));
            }
            match virtual_session.as_ref() {
                Some(session) => {
                    if session.status == LdkRuntimeVirtualChannelSessionStatusData::AbandonPending {
                        return Err(JsValue::from_str(
                            sdk_contracts::ERR_VIRTUAL_CLEANUP_IN_PROGRESS,
                        ));
                    }
                    self.ensure_virtual_cleanup_has_no_client_value(&channel, session)?;
                }
                None => {
                    // Client (accepter) path: we accepted this channel and hold no host-side session.
                    // The LSP/host abandons silently (`ErrorAction::IgnoreError`) without notifying us,
                    // so once we have drained our value we must tear down our own side. Guard on the
                    // live channel balances, then abandon locally — `abandon_virtual_channel` fires
                    // `Event::ChannelClosed`, which the authoritative reconcile propagates to our views.
                    if force {
                        return Err(JsValue::from_str(
                            "force=true is not supported for trusted virtual channels",
                        ));
                    }
                    // `peer_pubkey` was validated and unwrapped to a trimmed `String` above.
                    self.ensure_virtual_cleanup_client_no_local_value(&channel)?;
                    self.ldk_runtime
                        .virtual_channel_abandon_local(&channel_id, &peer_pubkey)?;
                    self.ldk_runtime.remove_channel(&channel_id);
                    self.unregister_trusted_virtual_scope_channel(&channel_id);
                    self.ldk_runtime.record_channel_closed();
                    self.persist_runtime_event_log_state();
                    return Ok(());
                }
            }
        }
        if force {
            if is_virtual_channel {
                return Err(JsValue::from_str(
                    "force=true is not supported for trusted virtual channels",
                ));
            }
        }
        if !is_virtual_channel {
            // Real (on-chain) channel: initiate the close on the live ChannelManager (cooperative
            // when `force` is false, force-close — broadcasting the latest commitment — when true,
            // matching the reference rgb-lightning-node SDK) and mark the cached view as closing. Do
            // NOT remove it here — the live ChannelManager removes it when it fires
            // `Event::ChannelClosed`, which `reconcile_channels_from_live` propagates (event-driven).
            // Removing optimistically on the request would report the channel as gone while it is
            // still open on-chain (funds locked) whenever the close stalls (e.g. an RGB colored-close
            // negotiation that has not produced `closing_signed` yet).
            self.ldk_runtime
                .close_live_channel(&channel_id, &channel.peer_pubkey, force)?;
            let mut closing = channel.clone();
            closing.status = if force {
                "force_closing".to_string()
            } else {
                "closing".to_string()
            };
            closing.ready = false;
            closing.is_usable = false;
            if self.use_runtime_state_for_ln_views() {
                self.ldk_runtime
                    .upsert_channel(Self::channel_runtime_state_from_data(&closing));
            } else if let Some(entry) = self.channels.borrow_mut().get_mut(&channel_id) {
                entry.data = closing;
            }
            self.ldk_runtime.record_channel_closed();
            self.persist_runtime_event_log_state();
            return Ok(());
        }

        // ---- trusted virtual channel close (host-authoritative; removal is immediate) ----
        let _ = self.ldk_runtime.virtual_channel_session_update_status(
            &channel_id,
            LdkRuntimeVirtualChannelSessionStatusData::AbandonPending,
        );
        let applied = self
            .apply_and_record_transport_event(
                RuntimeTransportEvent::ChannelClosed {
                    channel_id: channel_id.clone(),
                },
                "node_api",
            )?
            .applied;
        if !applied {
            let live_virtual_channel_still_exists = if self.use_runtime_state_for_ln_views() {
                self.ldk_runtime.list_channels().into_iter().any(|entry| {
                    entry.channel_id == channel_id
                        && entry.virtual_open_mode.as_deref()
                            == Some(SDK_VIRTUAL_OPEN_MODE_TRUSTED_NO_BROADCAST)
                })
            } else {
                self.channels.borrow().values().any(|entry| {
                    entry.data.channel_id == channel_id
                        && entry.data.virtual_open_mode.as_deref()
                            == Some(SDK_VIRTUAL_OPEN_MODE_TRUSTED_NO_BROADCAST)
                })
            };
            if live_virtual_channel_still_exists {
                let _ = self.ldk_runtime.virtual_channel_session_update_status(
                    &channel_id,
                    LdkRuntimeVirtualChannelSessionStatusData::Active,
                );
                return Err(JsValue::from_str(sdk_contracts::ERR_CHANNEL_NOT_FOUND));
            }
            let _ = self.ldk_runtime.virtual_channel_session_update_status(
                &channel_id,
                LdkRuntimeVirtualChannelSessionStatusData::Abandoned,
            );
            self.unregister_trusted_virtual_scope_channel(&channel_id);
            self.persist_runtime_event_log_state();
            return Ok(());
        }
        let _ = self.ldk_runtime.virtual_channel_session_update_status(
            &channel_id,
            LdkRuntimeVirtualChannelSessionStatusData::Abandoned,
        );
        self.unregister_trusted_virtual_scope_channel(&channel_id);
        self.ldk_runtime.record_channel_closed();
        self.persist_runtime_event_log_state();
        Ok(())
    }

    /// Guard for client-side virtual-channel cleanup (we accepted the channel; no host session).
    /// Refuse to abandon while we still hold value: RGB must be fully drained (units are valuable),
    /// and BTC must be down to sub-HTLC-minimum dust (unspendable over LN, forfeited by the abandon).
    fn ensure_virtual_cleanup_client_no_local_value(
        &self,
        channel: &RlnWasmNodeChannelData,
    ) -> Result<(), JsValue> {
        let rgb = channel.asset_local_amount.unwrap_or(0);
        if rgb > 0 {
            return Err(JsValue::from_str(&format!(
                "virtual cleanup blocked: {rgb} RGB units remain on the channel — drain them to the LSP first"
            )));
        }
        if channel.outbound_msat >= SDK_HTLC_MIN_MSAT {
            return Err(JsValue::from_str(&format!(
                "virtual cleanup blocked: {} msat of spendable BTC remains — drain it to the LSP first",
                channel.outbound_msat
            )));
        }
        Ok(())
    }

    fn ensure_virtual_cleanup_has_no_client_value(
        &self,
        channel: &RlnWasmNodeChannelData,
        session: &LdkRuntimeVirtualChannelSessionData,
    ) -> Result<(), JsValue> {
        if self.use_runtime_state_for_ln_views()
            && session.status != LdkRuntimeVirtualChannelSessionStatusData::Active
        {
            return Err(JsValue::from_str(
                "virtual cleanup requires an active host-side session",
            ));
        }
        let local_node_pubkey = self
            .local_node_pubkey_string()
            .ok_or_else(|| JsValue::from_str(sdk_contracts::ERR_NODE_IDENTITY_DERIVE_FAILED))?;
        let payments = if self.use_runtime_state_for_ln_views() {
            self.ldk_runtime
                .list_payments()
                .into_iter()
                .map(Self::payment_data_from_runtime_state)
                .collect::<Vec<_>>()
        } else {
            self.payments
                .borrow()
                .values()
                .map(|entry| entry.data.clone())
                .collect::<Vec<_>>()
        };

        let mut net_counterparty_btc_msat: i128 = 0;
        let mut net_counterparty_rgb_amount: HashMap<String, i128> = HashMap::new();
        let mut credited_payment_hashes: HashSet<String> = HashSet::new();

        for payment in payments {
            if payment.created_at < session.created_at {
                continue;
            }
            let outbound_to_counterparty =
                !payment.inbound && payment.payee_pubkey == session.peer_pubkey;
            let inbound_from_counterparty =
                payment.inbound && payment.payee_pubkey == local_node_pubkey;
            let payment_matches_channel_asset_scope = payment
                .asset_id
                .as_deref()
                .map(|asset| channel.asset_id.as_deref() == Some(asset))
                .unwrap_or(channel.asset_id.is_none());
            let payment_belongs_to_session = outbound_to_counterparty
                || (inbound_from_counterparty && payment_matches_channel_asset_scope);

            if matches!(
                payment.status.as_str(),
                "pending" | "claimable" | "claiming"
            ) && payment_belongs_to_session
            {
                return Err(JsValue::from_str(
                    "virtual cleanup is blocked while HTLCs are still in flight",
                ));
            }
            if payment.status != "succeeded" {
                continue;
            }
            if !payment_belongs_to_session {
                continue;
            }

            if outbound_to_counterparty {
                if let Some(msat) = payment.amt_msat {
                    net_counterparty_btc_msat += msat as i128;
                }
                if let (Some(asset_id), Some(asset_amount)) =
                    (payment.asset_id.as_ref(), payment.asset_amount)
                {
                    let entry = net_counterparty_rgb_amount
                        .entry(asset_id.clone())
                        .or_insert(0);
                    *entry += asset_amount as i128;
                }
            } else if inbound_from_counterparty && payment_matches_channel_asset_scope {
                if self.use_runtime_state_for_ln_views()
                    && !self.payment_has_authoritative_success_event(&payment.payment_hash)
                {
                    continue;
                }
                if let Some(msat) = payment.amt_msat {
                    net_counterparty_btc_msat -= msat as i128;
                }
                if let (Some(asset_id), Some(asset_amount)) =
                    (payment.asset_id.as_ref(), payment.asset_amount)
                {
                    let entry = net_counterparty_rgb_amount
                        .entry(asset_id.clone())
                        .or_insert(0);
                    *entry -= asset_amount as i128;
                }
                credited_payment_hashes.insert(payment.payment_hash);
            }
        }

        if self.use_runtime_state_for_ln_views() {
            for settlement in self.list_trusted_virtual_authoritative_settlements(
                session,
                channel,
                &local_node_pubkey,
            ) {
                if credited_payment_hashes.contains(&settlement.payment_hash) {
                    continue;
                }
                if let Some(msat) = settlement.amt_msat {
                    net_counterparty_btc_msat -= msat as i128;
                }
                if let (Some(asset_id), Some(asset_amount)) =
                    (settlement.asset_id.as_ref(), settlement.asset_amount)
                {
                    let entry = net_counterparty_rgb_amount
                        .entry(asset_id.clone())
                        .or_insert(0);
                    *entry -= asset_amount as i128;
                }
            }
        }

        if net_counterparty_btc_msat > 0 {
            let mut floor_sat = (net_counterparty_btc_msat / 1000) as u64;
            if floor_sat == 0 {
                floor_sat = 1;
            }
            return Err(JsValue::from_str(&format!(
                "virtual cleanup is blocked while counterparty BTC balance floor is {floor_sat} sat"
            )));
        }

        if let Some((asset_id, amount)) = net_counterparty_rgb_amount
            .iter()
            .find(|(_, amount)| **amount > 0)
            .map(|(asset_id, amount)| (asset_id.clone(), *amount as u64))
        {
            return Err(JsValue::from_str(&format!(
                "virtual cleanup is blocked while counterparty RGB balance is {amount} (asset_id={asset_id})"
            )));
        }

        Ok(())
    }

    fn payment_has_authoritative_success_event(&self, payment_hash: &str) -> bool {
        self.runtime_events.borrow().iter().rev().any(|event| {
            event.applied
                && event.payment_hash.as_deref() == Some(payment_hash)
                && event.status.as_deref() == Some("succeeded")
                && event.source != "node_api"
                && event.source != "manual_api"
        })
    }

    fn list_trusted_virtual_authoritative_settlements(
        &self,
        session: &LdkRuntimeVirtualChannelSessionData,
        channel: &RlnWasmNodeChannelData,
        _local_node_pubkey: &str,
    ) -> Vec<TrustedVirtualAuthoritativeSettlementData> {
        TRUSTED_VIRTUAL_AUTHORITATIVE_SETTLEMENT_STORAGE.with(|storage| {
            storage
                .borrow()
                .iter()
                .filter(|settlement| {
                    if settlement.created_at < session.created_at {
                        return false;
                    }
                    if settlement.from_pubkey != session.peer_pubkey {
                        return false;
                    }
                    settlement
                        .asset_id
                        .as_deref()
                        .map(|asset| channel.asset_id.as_deref() == Some(asset))
                        .unwrap_or(channel.asset_id.is_none())
                })
                .cloned()
                .collect::<Vec<_>>()
        })
    }

    #[wasm_bindgen(js_name = getChannelId)]
    pub fn get_channel_id(&self, temporary_channel_id: String) -> Result<String, JsValue> {
        self.ensure_runtime_ready()?;
        if temporary_channel_id.trim().is_empty() {
            return Err(JsValue::from_str(
                sdk_contracts::ERR_TEMPORARY_CHANNEL_ID_EMPTY,
            ));
        }
        let channel_id = if self.use_runtime_state_for_ln_views() {
            self.ldk_runtime
                .find_channel_by_temporary(&temporary_channel_id)
                .ok_or_else(|| JsValue::from_str(sdk_contracts::ERR_TEMPORARY_CHANNEL_ID_UNKNOWN))?
        } else {
            self.channels
                .borrow()
                .values()
                .find(|entry| entry.temporary_channel_id == temporary_channel_id)
                .map(|entry| entry.data.channel_id.clone())
                .ok_or_else(|| JsValue::from_str(sdk_contracts::ERR_TEMPORARY_CHANNEL_ID_UNKNOWN))?
        };
        Ok(channel_id)
    }
}

#[cfg(target_arch = "wasm32")]
async fn fetch_block_header_hex_by_height(
    indexer_url: &str,
    height: u32,
) -> Result<String, JsValue> {
    let block_hash = fetch_block_hash_by_height(indexer_url, height).await?;
    fetch_block_header_hex(indexer_url, &block_hash).await
}

#[cfg(target_arch = "wasm32")]
async fn fetch_block_hash_by_height(indexer_url: &str, height: u32) -> Result<String, JsValue> {
    let url = format!("{indexer_url}/block-height/{height}");
    let response = Request::get(&url)
        .send()
        .await
        .map_err(|e| JsValue::from_str(&e.to_string()))?;
    if !response.ok() {
        return Err(JsValue::from_str(&format!(
            "block-height query failed with status {}",
            response.status()
        )));
    }
    Ok(response
        .text()
        .await
        .map_err(|e| JsValue::from_str(&e.to_string()))?)
}

#[cfg(target_arch = "wasm32")]
async fn fetch_block_header_hex(indexer_url: &str, block_hash: &str) -> Result<String, JsValue> {
    let url = format!("{indexer_url}/block/{block_hash}/header");
    let response = Request::get(&url)
        .send()
        .await
        .map_err(|e| JsValue::from_str(&e.to_string()))?;
    if !response.ok() {
        return Err(JsValue::from_str(&format!(
            "block header query failed with status {}",
            response.status()
        )));
    }
    Ok(response
        .text()
        .await
        .map_err(|e| JsValue::from_str(&e.to_string()))?)
}

#[cfg(target_arch = "wasm32")]
async fn fetch_tx_hex(indexer_url: &str, txid: &str) -> Result<String, JsValue> {
    let url = format!("{indexer_url}/tx/{txid}/hex");
    let response = Request::get(&url)
        .send()
        .await
        .map_err(|e| JsValue::from_str(&e.to_string()))?;
    if !response.ok() {
        return Err(JsValue::from_str(&format!(
            "tx hex query failed with status {}",
            response.status()
        )));
    }
    Ok(response
        .text()
        .await
        .map_err(|e| JsValue::from_str(&e.to_string()))?)
}

#[cfg(target_arch = "wasm32")]
async fn fetch_tx_status(indexer_url: &str, txid: &str) -> Result<Option<(u32, String)>, JsValue> {
    let url = format!("{indexer_url}/tx/{txid}/status");
    let response = Request::get(&url)
        .send()
        .await
        .map_err(|e| JsValue::from_str(&e.to_string()))?;
    if !response.ok() {
        return Err(JsValue::from_str(&format!(
            "tx status query failed with status {}",
            response.status()
        )));
    }
    let value: serde_json::Value = response
        .json()
        .await
        .map_err(|e| JsValue::from_str(&e.to_string()))?;
    let confirmed = value
        .get("confirmed")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if !confirmed {
        return Ok(None);
    }
    let height = value
        .get("block_height")
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as u32;
    let block_hash = value
        .get("block_hash")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if height == 0 || block_hash.is_empty() {
        return Ok(None);
    }
    Ok(Some((height, block_hash)))
}

/// Esplora: `GET /block/:hash/txids` returns the ordered txid list for the block.
/// LDK's `transactions_confirmed` requires the transaction's index within that list (coinbase is 0).
#[cfg(target_arch = "wasm32")]
async fn fetch_tx_index_in_block(
    indexer_url: &str,
    block_hash: &str,
    txid: &str,
) -> Result<usize, JsValue> {
    let url = format!("{indexer_url}/block/{block_hash}/txids");
    let response = Request::get(&url)
        .send()
        .await
        .map_err(|e| JsValue::from_str(&e.to_string()))?;
    if !response.ok() {
        return Err(JsValue::from_str(&format!(
            "block txids query failed with status {}",
            response.status()
        )));
    }
    let value: serde_json::Value = response
        .json()
        .await
        .map_err(|e| JsValue::from_str(&e.to_string()))?;
    let Some(arr) = value.as_array() else {
        return Err(JsValue::from_str(
            "block txids response must be a JSON array",
        ));
    };
    let needle = txid.trim().to_ascii_lowercase();
    for (idx, item) in arr.iter().enumerate() {
        let Some(t) = item.as_str() else {
            continue;
        };
        if t.trim().to_ascii_lowercase() == needle {
            return Ok(idx);
        }
    }
    Err(JsValue::from_str(&format!(
        "txid {txid} not found in block {block_hash} tx list"
    )))
}

#[cfg(not(target_arch = "wasm32"))]
async fn fetch_tx_index_in_block(
    _indexer_url: &str,
    _block_hash: &str,
    _txid: &str,
) -> Result<usize, JsValue> {
    Err(JsValue::from_str(sdk_contracts::ERR_CHAIN_SYNC_WASM32_ONLY))
}

#[cfg(not(target_arch = "wasm32"))]
async fn fetch_block_header_hex_by_height(
    _indexer_url: &str,
    _height: u32,
) -> Result<String, JsValue> {
    Err(JsValue::from_str(sdk_contracts::ERR_CHAIN_SYNC_WASM32_ONLY))
}

#[cfg(not(target_arch = "wasm32"))]
async fn fetch_block_hash_by_height(_indexer_url: &str, _height: u32) -> Result<String, JsValue> {
    Err(JsValue::from_str(sdk_contracts::ERR_CHAIN_SYNC_WASM32_ONLY))
}

#[cfg(not(target_arch = "wasm32"))]
async fn fetch_block_header_hex(_indexer_url: &str, _block_hash: &str) -> Result<String, JsValue> {
    Err(JsValue::from_str(sdk_contracts::ERR_CHAIN_SYNC_WASM32_ONLY))
}

#[cfg(not(target_arch = "wasm32"))]
async fn fetch_tx_hex(_indexer_url: &str, _txid: &str) -> Result<String, JsValue> {
    Err(JsValue::from_str(sdk_contracts::ERR_CHAIN_SYNC_WASM32_ONLY))
}

#[cfg(not(target_arch = "wasm32"))]
async fn fetch_tx_status(
    _indexer_url: &str,
    _txid: &str,
) -> Result<Option<(u32, String)>, JsValue> {
    Err(JsValue::from_str(sdk_contracts::ERR_CHAIN_SYNC_WASM32_ONLY))
}

#[cfg(test)]
impl RlnWasmNode {
    #[allow(dead_code)]
    pub(crate) fn new_with_runtime_backend(
        proxy_url: String,
        runtime_backend: String,
    ) -> Result<RlnWasmNode, JsValue> {
        if runtime_backend.trim() != "wasm_native_ldk" {
            return Err(JsValue::from_str(&format!(
                "unknown runtime backend: {}",
                runtime_backend.trim()
            )));
        }
        Self::new(proxy_url)
    }

    #[allow(dead_code)]
    pub(crate) fn new_with_runtime_backend_and_id(
        proxy_url: String,
        runtime_backend: String,
        node_runtime_id: Option<String>,
    ) -> Result<RlnWasmNode, JsValue> {
        if runtime_backend.trim() != "wasm_native_ldk" {
            return Err(JsValue::from_str(&format!(
                "unknown runtime backend: {}",
                runtime_backend.trim()
            )));
        }
        Self::new_with_runtime_id_opt(proxy_url, node_runtime_id, None)
    }
}

impl RlnWasmNode {
    fn runtime_manager_key(&self) -> String {
        self.persistence_keys.ldk_manager_registry_key.clone()
    }

    fn persist_runtime_event_log_state(&self) {
        persist_runtime_event_log_state(
            &self.persistence_keys.runtime_events_storage_key,
            &self.runtime_events,
            &self.next_runtime_event_seq,
        );
    }

    fn persist_rgb_ln_transfer_state(&self) {
        persist_runtime_rgb_ln_transfer_state(
            &self.persistence_keys.rgb_ln_transfers_storage_key,
            &self.rgb_ln_transfers,
        );
    }

    fn persist_peer_session_state(&self) {
        persist_runtime_peer_session_state(
            &self.persistence_keys.peer_sessions_storage_key,
            &self.persistence_keys.runtime_scope_key,
            &self.peers,
        );
    }

    fn use_runtime_state_for_ln_views(&self) -> bool {
        let backend = self.ldk_runtime.status().backend;
        backend == "wasm_native_ldk"
    }

    fn has_any_connected_peer(&self) -> bool {
        if self.use_runtime_state_for_ln_views() {
            self.ldk_runtime.has_any_connected_peer()
        } else {
            self.peers
                .borrow()
                .values()
                .any(|entry| entry.session.is_started())
        }
    }

    fn has_connected_peer(&self, peer_pubkey: &str) -> bool {
        if self.use_runtime_state_for_ln_views() {
            self.ldk_runtime.has_connected_peer(peer_pubkey)
        } else {
            self.peers
                .borrow()
                .get(peer_pubkey)
                .map(|entry| entry.session.is_started())
                .unwrap_or(false)
        }
    }

    fn trusted_virtual_scope_key(&self) -> String {
        self.persistence_keys.runtime_scope_key.clone()
    }

    fn register_runtime_scope_for_local_pubkey(&self) {
        let Some(local_node_pubkey) = self.local_node_pubkey_string() else {
            return;
        };
        NODE_PUBKEY_RUNTIME_SCOPE_INDEX.with(|index| {
            let mut index = index.borrow_mut();
            index
                .entry(local_node_pubkey)
                .or_insert_with(HashSet::new)
                .insert(self.persistence_keys.runtime_scope_key.clone());
        });
    }

    fn trusted_virtual_link_key(local_node_pubkey: &str, peer_pubkey: &str) -> String {
        format!("{local_node_pubkey}|{peer_pubkey}")
    }

    fn local_node_pubkey_string(&self) -> Option<String> {
        if self.use_runtime_state_for_ln_views() {
            if let Ok(pubkey) = self.ldk_runtime.live_node_pubkey() {
                let trimmed = pubkey.trim().to_string();
                if !trimmed.is_empty() {
                    return Some(trimmed);
                }
            }
        }
        self.node_signing_identity()
            .ok()
            .map(|(_, pubkey)| pubkey.to_string())
    }

    fn has_trusted_virtual_link_with_peer(&self, peer_pubkey: &str) -> bool {
        let Some(local_node_pubkey) = self.local_node_pubkey_string() else {
            return false;
        };
        let key = Self::trusted_virtual_link_key(&local_node_pubkey, peer_pubkey);
        TRUSTED_VIRTUAL_PEER_LINK_STORAGE.with(|storage| storage.borrow().contains_key(&key))
    }

    fn has_any_trusted_virtual_activity_global() -> bool {
        TRUSTED_VIRTUAL_CHANNEL_SCOPE_STORAGE.with(|storage| {
            storage
                .borrow()
                .values()
                .any(|channels| !channels.is_empty())
        })
    }

    fn trusted_virtual_success_eligible(&self, payee_pubkey: &str) -> bool {
        let has_usable_trusted_virtual_channel =
            self.ldk_runtime.list_channels().into_iter().any(|entry| {
                entry.peer_pubkey == payee_pubkey
                    && entry.is_usable
                    && entry.virtual_open_mode.as_deref()
                        == Some(SDK_VIRTUAL_OPEN_MODE_TRUSTED_NO_BROADCAST)
            });
        has_usable_trusted_virtual_channel
            || self.has_trusted_virtual_link_with_peer(payee_pubkey)
            || Self::has_any_trusted_virtual_activity_global()
    }

    fn runtime_scope_keys_for_node_pubkey(node_pubkey: &str) -> Vec<String> {
        NODE_PUBKEY_RUNTIME_SCOPE_INDEX.with(|index| {
            index
                .borrow()
                .get(node_pubkey)
                .map(|keys| keys.iter().cloned().collect::<Vec<_>>())
                .unwrap_or_default()
        })
    }

    fn routed_success_eligible(&self, payment_hash: &str, payee_pubkey: &str) -> bool {
        if !self.has_any_connected_peer() {
            return false;
        }
        let has_usable_channel = self
            .ldk_runtime
            .list_channels()
            .into_iter()
            .any(|entry| entry.is_usable);
        if !has_usable_channel {
            return false;
        }

        let mut runtime_scope_keys = Self::runtime_scope_keys_for_node_pubkey(payee_pubkey);
        if runtime_scope_keys.is_empty() {
            runtime_scope_keys = KNOWN_RUNTIME_SCOPE_KEYS.with(|known_keys| {
                let mut merged = known_keys.borrow().iter().cloned().collect::<HashSet<_>>();
                for scope_key in NODE_PUBKEY_RUNTIME_SCOPE_INDEX.with(|index| {
                    index
                        .borrow()
                        .values()
                        .flat_map(|keys| keys.iter().cloned())
                        .collect::<Vec<_>>()
                }) {
                    merged.insert(scope_key);
                }
                merged.into_iter().collect::<Vec<_>>()
            });
        }
        for runtime_scope_key in runtime_scope_keys {
            let runtime_key = format!("node-runtime:{runtime_scope_key}");
            let Ok(manager) = crate::ldk_runtime::ldk_runtime_manager(runtime_key) else {
                continue;
            };
            if manager.ensure_started().is_err() {
                continue;
            }
            let Some(payment) = manager.get_payment(payment_hash) else {
                continue;
            };
            if payment.inbound
                && (payment.status == "pending"
                    || payment.status == "claimable"
                    || payment.status == "claiming")
            {
                return true;
            }
        }
        false
    }

    fn emit_runtime_payment_success_event_if_applicable(
        &self,
        payment_hash: &str,
        payee_pubkey: &str,
    ) -> Result<(), JsValue> {
        if !self.use_runtime_state_for_ln_views() {
            return Ok(());
        }
        let direct_connected = self.has_connected_peer(payee_pubkey);
        let has_usable_channel = self
            .ldk_runtime
            .list_channels()
            .into_iter()
            .any(|entry| entry.peer_pubkey == payee_pubkey && entry.is_usable);
        let virtual_eligible =
            direct_connected && self.trusted_virtual_success_eligible(payee_pubkey);
        let routed_eligible =
            !direct_connected && self.routed_success_eligible(payment_hash, payee_pubkey);
        if !(has_usable_channel || virtual_eligible || routed_eligible) {
            return Ok(());
        }
        let source = if virtual_eligible {
            "runtime_virtual_payment_engine"
        } else if routed_eligible {
            "runtime_routed_payment_engine"
        } else {
            "runtime_channel_payment_engine"
        };
        let _ = self.apply_payment_status_via_event_stream(payment_hash, "succeeded", source)?;
        if virtual_eligible {
            self.record_trusted_virtual_authoritative_settlement(payment_hash, payee_pubkey);
        }
        self.propagate_runtime_payment_status_to_payee_nodes(
            payment_hash,
            payee_pubkey,
            "succeeded",
        );
        Ok(())
    }

    fn propagate_runtime_payment_status_to_payee_nodes(
        &self,
        payment_hash: &str,
        payee_pubkey: &str,
        status: &str,
    ) {
        let runtime_scope_keys = NODE_PUBKEY_RUNTIME_SCOPE_INDEX.with(|index| {
            index
                .borrow()
                .get(payee_pubkey)
                .map(|keys| keys.iter().cloned().collect::<Vec<_>>())
                .unwrap_or_default()
        });
        if runtime_scope_keys.is_empty() {
            return;
        }

        for runtime_scope_key in runtime_scope_keys {
            let runtime_key = format!("node-runtime:{runtime_scope_key}");
            let Ok(manager) = crate::ldk_runtime::ldk_runtime_manager(runtime_key) else {
                continue;
            };
            if manager.ensure_started().is_err() {
                continue;
            }
            let Some(mut payment) = manager.get_payment(payment_hash) else {
                continue;
            };
            if !payment.inbound {
                continue;
            }
            if payment.status == status {
                continue;
            }
            if !is_valid_payment_status_transition(&payment.status, status) {
                continue;
            }
            payment.status = status.to_string();
            payment.updated_at = unix_now_secs();
            manager.upsert_payment(payment.clone());
            self.propagate_runtime_rgb_ln_transfer_status_for_scope(&runtime_scope_key, &payment);
        }
    }

    fn propagate_runtime_rgb_ln_transfer_status_for_scope(
        &self,
        runtime_scope_key: &str,
        payment: &LdkRuntimePaymentStateData,
    ) {
        let storage_key = RuntimeScopeKeys::from_runtime_scope_key(runtime_scope_key.to_string())
            .rgb_ln_transfers_storage_key;
        let mut snapshot = load_runtime_rgb_ln_transfer_snapshot(&storage_key).unwrap_or_default();
        let mut found = false;
        for entry in snapshot.transfers.iter_mut() {
            if entry.payment_hash == payment.payment_hash {
                entry.status = payment.status.clone();
                entry.updated_at = payment.updated_at;
                found = true;
            }
        }
        if !found {
            if let (Some(asset_id), Some(asset_amount)) =
                (payment.asset_id.as_ref(), payment.asset_amount)
            {
                snapshot.transfers.push(RlnWasmNodeRgbLnTransferData {
                    payment_hash: payment.payment_hash.clone(),
                    inbound: payment.inbound,
                    asset_id: asset_id.clone(),
                    asset_amount,
                    status: payment.status.clone(),
                    created_at: payment.created_at,
                    updated_at: payment.updated_at,
                });
                snapshot
                    .transfers
                    .sort_by(|a, b| a.payment_hash.cmp(&b.payment_hash));
            }
        }
        if let Ok(raw) = serde_json::to_string(&snapshot) {
            let store = browser_persistent_state_store();
            let _ = store.set(&storage_key, &raw);
        }
        RUNTIME_RGB_LN_TRANSFER_STORAGE.with(|state| {
            state.borrow_mut().insert(storage_key, snapshot);
        });
    }

    fn record_trusted_virtual_authoritative_settlement(
        &self,
        payment_hash: &str,
        payee_pubkey: &str,
    ) {
        let Some(local_node_pubkey) = self.local_node_pubkey_string() else {
            return;
        };
        let Some(payment) = self
            .ldk_runtime
            .get_payment(payment_hash)
            .map(Self::payment_data_from_runtime_state)
        else {
            return;
        };
        if payment.inbound || payment.status != "succeeded" || payment.payee_pubkey != payee_pubkey
        {
            return;
        }
        TRUSTED_VIRTUAL_AUTHORITATIVE_SETTLEMENT_STORAGE.with(|storage| {
            let mut storage = storage.borrow_mut();
            if let Some(existing) = storage.iter_mut().find(|entry| {
                entry.payment_hash == payment.payment_hash
                    && entry.from_pubkey == local_node_pubkey
                    && entry.to_pubkey == payee_pubkey
            }) {
                existing.amt_msat = payment.amt_msat;
                existing.asset_id = payment.asset_id.clone();
                existing.asset_amount = payment.asset_amount;
                existing.created_at = payment.created_at;
                return;
            }
            storage.push(TrustedVirtualAuthoritativeSettlementData {
                payment_hash: payment.payment_hash,
                from_pubkey: local_node_pubkey,
                to_pubkey: payee_pubkey.to_string(),
                amt_msat: payment.amt_msat,
                asset_id: payment.asset_id,
                asset_amount: payment.asset_amount,
                created_at: payment.created_at,
            });
        });
    }

    fn register_trusted_virtual_scope_channel(&self, channel_id: &str, peer_pubkey: &str) {
        let scope_key = self.trusted_virtual_scope_key();
        let local_node_pubkey = self.local_node_pubkey_string();
        TRUSTED_VIRTUAL_CHANNEL_SCOPE_STORAGE.with(|storage| {
            let mut storage = storage.borrow_mut();
            let channels = storage.entry(scope_key).or_insert_with(HashMap::new);
            channels.insert(
                channel_id.to_string(),
                TrustedVirtualScopeChannelData {
                    peer_pubkey: peer_pubkey.to_string(),
                    local_node_pubkey: local_node_pubkey.clone(),
                },
            );
        });
        if let Some(local_node_pubkey) = local_node_pubkey {
            TRUSTED_VIRTUAL_PEER_LINK_STORAGE.with(|storage| {
                let mut storage = storage.borrow_mut();
                let forward_key = Self::trusted_virtual_link_key(&local_node_pubkey, peer_pubkey);
                let reverse_key = Self::trusted_virtual_link_key(peer_pubkey, &local_node_pubkey);
                *storage.entry(forward_key).or_insert(0) += 1;
                *storage.entry(reverse_key).or_insert(0) += 1;
            });
        }
    }

    fn decrement_trusted_virtual_link_counter(local_node_pubkey: &str, peer_pubkey: &str) {
        TRUSTED_VIRTUAL_PEER_LINK_STORAGE.with(|storage| {
            let mut storage = storage.borrow_mut();
            let forward_key = Self::trusted_virtual_link_key(local_node_pubkey, peer_pubkey);
            let reverse_key = Self::trusted_virtual_link_key(peer_pubkey, local_node_pubkey);
            for key in [forward_key, reverse_key] {
                if let Some(counter) = storage.get_mut(&key) {
                    if *counter <= 1 {
                        storage.remove(&key);
                    } else {
                        *counter -= 1;
                    }
                }
            }
        });
    }

    fn unregister_trusted_virtual_scope_channel(&self, channel_id: &str) {
        let scope_key = self.trusted_virtual_scope_key();
        TRUSTED_VIRTUAL_CHANNEL_SCOPE_STORAGE.with(|storage| {
            let mut storage = storage.borrow_mut();
            if let Some(channels) = storage.get_mut(&scope_key) {
                if let Some(removed) = channels.remove(channel_id) {
                    if let Some(local_node_pubkey) = removed.local_node_pubkey.as_deref() {
                        Self::decrement_trusted_virtual_link_counter(
                            local_node_pubkey,
                            &removed.peer_pubkey,
                        );
                    }
                }
                if channels.is_empty() {
                    storage.remove(&scope_key);
                }
            }
        });
    }

    fn channel_runtime_state_from_data(
        data: &RlnWasmNodeChannelData,
    ) -> LdkRuntimeChannelStateData {
        LdkRuntimeChannelStateData {
            temporary_channel_id: data.temporary_channel_id.clone(),
            channel_id: data.channel_id.clone(),
            peer_pubkey: data.peer_pubkey.clone(),
            status: data.status.clone(),
            ready: data.ready,
            is_usable: data.is_usable,
            public: data.public,
            capacity_sat: data.capacity_sat,
            asset_id: data.asset_id.clone(),
            asset_local_amount: data.asset_local_amount,
            virtual_open_mode: data.virtual_open_mode.clone(),
            outbound_msat: data.outbound_msat,
            next_outbound_htlc_limit_msat: data.next_outbound_htlc_limit_msat,
        }
    }

    /// True if this node has a virtual (`trusted_no_broadcast`) channel to `dest_pubkey`.
    fn has_virtual_channel_to(&self, dest_pubkey: &str) -> bool {
        self.ldk_runtime
            .list_channels()
            .into_iter()
            .filter(|c| c.peer_pubkey == dest_pubkey)
            .any(|c| {
                self.ldk_runtime
                    .virtual_channel_session_get(&c.channel_id)
                    .is_some()
            })
    }

    /// Effective HTLC floor for a payment to `dest_pubkey`: the lower virtual floor when
    /// a virtual channel to the destination exists, otherwise the regular floor. Mirrors
    /// native's per-channel `our_htlc_minimum_msat` selection.
    fn min_htlc_msat_to_dest(&self, dest_pubkey: &str) -> u64 {
        if self.has_virtual_channel_to(dest_pubkey) {
            VIRTUAL_HTLC_MIN_MSAT
        } else {
            SDK_HTLC_MIN_MSAT
        }
    }

    fn channel_data_from_runtime_state(
        state: LdkRuntimeChannelStateData,
    ) -> RlnWasmNodeChannelData {
        RlnWasmNodeChannelData {
            temporary_channel_id: state.temporary_channel_id,
            channel_id: state.channel_id,
            peer_pubkey: state.peer_pubkey,
            status: state.status,
            ready: state.ready,
            is_usable: state.is_usable,
            public: state.public,
            capacity_sat: state.capacity_sat,
            asset_id: state.asset_id,
            asset_local_amount: state.asset_local_amount,
            virtual_open_mode: state.virtual_open_mode,
            outbound_msat: state.outbound_msat,
            next_outbound_htlc_limit_msat: state.next_outbound_htlc_limit_msat,
        }
    }

    fn merge_runtime_channels_with_local_cache(
        &self,
        runtime: Vec<RlnWasmNodeChannelData>,
    ) -> Vec<RlnWasmNodeChannelData> {
        let local_channels = self.channels.borrow();
        let mut merged = Vec::with_capacity(runtime.len().max(local_channels.len()));
        let mut seen_channel_ids = HashSet::new();
        // Track the original temporary ids that the runtime view already
        // accounts for. After a channel migrates from temp_id → final
        // channel_id (post-FundingCreated), the local `self.channels` map
        // still holds the pre-migration shadow keyed by `temp_id` with
        // `channel_id == temp_id`. Without this set we would re-emit that
        // shadow as a separate channel record, producing a phantom "opening"
        // duplicate alongside the real ready channel.
        let mut seen_temp_ids = HashSet::new();

        for mut channel in runtime {
            let local = local_channels
                .get(&channel.channel_id)
                .map(|entry| &entry.data)
                .or_else(|| {
                    local_channels
                        .values()
                        .find(|entry| {
                            entry.data.channel_id == channel.temporary_channel_id
                                || entry.data.temporary_channel_id == channel.temporary_channel_id
                                || entry.data.temporary_channel_id == channel.channel_id
                        })
                        .map(|entry| &entry.data)
                });
            if let Some(local) = local {
                if channel.temporary_channel_id.trim().is_empty() {
                    channel.temporary_channel_id = local.temporary_channel_id.clone();
                }
                if channel.peer_pubkey.trim().is_empty() {
                    channel.peer_pubkey = local.peer_pubkey.clone();
                }
                if channel.capacity_sat == 0 {
                    channel.capacity_sat = local.capacity_sat;
                }
                if channel.asset_id.is_none() {
                    channel.asset_id = local.asset_id.clone();
                }
                if channel.asset_local_amount.is_none() {
                    channel.asset_local_amount = local.asset_local_amount;
                }
                if channel.virtual_open_mode.is_none() {
                    channel.virtual_open_mode = local.virtual_open_mode.clone();
                }
            }
            seen_channel_ids.insert(channel.channel_id.clone());
            if !channel.temporary_channel_id.is_empty() {
                seen_temp_ids.insert(channel.temporary_channel_id.clone());
            }
            merged.push(channel);
        }

        for entry in local_channels.values() {
            if seen_channel_ids.contains(&entry.data.channel_id) {
                continue;
            }
            // Pre-migration shadow entries are keyed by their temp id and
            // satisfy `channel_id == temporary_channel_id`. If a runtime
            // record already covers that temp id, drop the shadow.
            if seen_temp_ids.contains(&entry.data.channel_id)
                || seen_temp_ids.contains(&entry.data.temporary_channel_id)
            {
                continue;
            }
            merged.push(entry.data.clone());
        }
        merged
    }

    fn payment_runtime_state_from_data(
        data: &RlnWasmNodePaymentData,
    ) -> LdkRuntimePaymentStateData {
        LdkRuntimePaymentStateData {
            amt_msat: data.amt_msat,
            asset_amount: data.asset_amount,
            asset_id: data.asset_id.clone(),
            payment_hash: data.payment_hash.clone(),
            inbound: data.inbound,
            status: data.status.clone(),
            invoice_type: data.invoice_type.clone(),
            preimage: data.preimage.clone(),
            created_at: data.created_at,
            updated_at: data.updated_at,
            payee_pubkey: data.payee_pubkey.clone(),
        }
    }

    fn payment_data_from_runtime_state(
        state: LdkRuntimePaymentStateData,
    ) -> RlnWasmNodePaymentData {
        RlnWasmNodePaymentData {
            amt_msat: state.amt_msat,
            asset_amount: state.asset_amount,
            asset_id: state.asset_id,
            payment_hash: state.payment_hash,
            inbound: state.inbound,
            status: state.status,
            invoice_type: state.invoice_type,
            preimage: state.preimage,
            created_at: state.created_at,
            updated_at: state.updated_at,
            payee_pubkey: state.payee_pubkey,
        }
    }

    fn register_rgb_ln_transfer_from_payment(&self, payment: &RlnWasmNodePaymentData) {
        let (Some(asset_id), Some(asset_amount)) =
            (payment.asset_id.as_ref(), payment.asset_amount)
        else {
            return;
        };
        self.rgb_ln_transfers.borrow_mut().insert(
            payment.payment_hash.clone(),
            RlnWasmNodeRgbLnTransferData {
                payment_hash: payment.payment_hash.clone(),
                inbound: payment.inbound,
                asset_id: asset_id.clone(),
                asset_amount,
                status: payment.status.clone(),
                created_at: payment.created_at,
                updated_at: payment.updated_at,
            },
        );
        self.persist_rgb_ln_transfer_state();
    }

    fn sync_rgb_ln_transfer_from_payment(&self, payment: &RlnWasmNodePaymentData) {
        let (Some(asset_id), Some(asset_amount)) =
            (payment.asset_id.as_ref(), payment.asset_amount)
        else {
            return;
        };
        let mut transfers = self.rgb_ln_transfers.borrow_mut();
        let entry = transfers
            .entry(payment.payment_hash.clone())
            .or_insert_with(|| RlnWasmNodeRgbLnTransferData {
                payment_hash: payment.payment_hash.clone(),
                inbound: payment.inbound,
                asset_id: asset_id.clone(),
                asset_amount,
                status: payment.status.clone(),
                created_at: payment.created_at,
                updated_at: payment.updated_at,
            });
        entry.status = payment.status.clone();
        entry.updated_at = payment.updated_at;
        drop(transfers);
        self.persist_rgb_ln_transfer_state();
    }

    fn apply_payment_status_via_event_stream(
        &self,
        payment_hash: &str,
        status: &str,
        source: &str,
    ) -> Result<RlnWasmNodePaymentData, JsValue> {
        let payload_hex = encode_payment_status_event_payload(payment_hash, status);
        let _ = self
            .runtime_core
            .enqueue_event("payment_status".to_string(), payload_hex.clone());
        apply_runtime_hook_payload(
            &self.ldk_runtime,
            self.use_runtime_state_for_ln_views(),
            &self.peers,
            &self.channels,
            &self.payments,
            &self.runtime_events,
            &self.next_runtime_event_seq,
            payload_hex,
            source,
        )?;
        self.persist_runtime_event_log_state();
        let payment = if self.use_runtime_state_for_ln_views() {
            self.ldk_runtime
                .get_payment(payment_hash)
                .map(Self::payment_data_from_runtime_state)
                .ok_or_else(|| JsValue::from_str(sdk_contracts::ERR_PAYMENT_NOT_FOUND))
        } else {
            self.payments
                .borrow()
                .get(payment_hash)
                .map(|entry| entry.data.clone())
                .ok_or_else(|| JsValue::from_str(sdk_contracts::ERR_PAYMENT_NOT_FOUND))
        }?;
        self.sync_rgb_ln_transfer_from_payment(&payment);
        Ok(payment)
    }

    fn apply_and_record_payment_status_event(
        &self,
        payment_hash: &str,
        status: &str,
        source: &str,
    ) -> Result<RlnWasmNodePaymentData, JsValue> {
        let payload_hex = encode_payment_status_event_payload(payment_hash, status);
        let _ = self
            .runtime_core
            .enqueue_event("payment_status".to_string(), payload_hex.clone());
        if self.use_runtime_state_for_ln_views() {
            let received_at = unix_now_secs();
            let seq = next_runtime_event_seq(&self.next_runtime_event_seq);
            let normalized = normalize_payment_status(status)?;
            let Some(mut payment) = self.ldk_runtime.get_payment(payment_hash) else {
                let error = "payment not found".to_string();
                record_runtime_event(
                    &self.runtime_events,
                    RlnWasmNodeRuntimeEventData {
                        seq,
                        source: source.to_string(),
                        event_kind: "payment_status".to_string(),
                        payload_hex: payload_hex.clone(),
                        payment_hash: Some(payment_hash.to_string()),
                        status: Some(normalized),
                        applied: false,
                        error: Some(error.clone()),
                        received_at,
                    },
                );
                self.persist_runtime_event_log_state();
                return Err(JsValue::from_str(&error));
            };
            if !is_valid_payment_status_transition(&payment.status, &normalized) {
                let error = format!(
                    "invalid payment status transition: {} -> {}",
                    payment.status, normalized
                );
                record_runtime_event(
                    &self.runtime_events,
                    RlnWasmNodeRuntimeEventData {
                        seq,
                        source: source.to_string(),
                        event_kind: "payment_status".to_string(),
                        payload_hex: payload_hex.clone(),
                        payment_hash: Some(payment_hash.to_string()),
                        status: Some(normalized),
                        applied: false,
                        error: Some(error.clone()),
                        received_at,
                    },
                );
                self.persist_runtime_event_log_state();
                return Err(JsValue::from_str(&error));
            }
            payment.status = normalized.clone();
            payment.updated_at = unix_now_secs();
            self.ldk_runtime.upsert_payment(payment.clone());
            record_runtime_event(
                &self.runtime_events,
                RlnWasmNodeRuntimeEventData {
                    seq,
                    source: source.to_string(),
                    event_kind: "payment_status".to_string(),
                    payload_hex: payload_hex.clone(),
                    payment_hash: Some(payment_hash.to_string()),
                    status: Some(normalized.clone()),
                    applied: true,
                    error: None,
                    received_at,
                },
            );
            crate::swap_runtime::apply_payment_status_update(payment_hash, &normalized);
            self.sync_rgb_ln_transfer_from_payment(&Self::payment_data_from_runtime_state(
                payment.clone(),
            ));
            self.persist_runtime_event_log_state();
            return Ok(Self::payment_data_from_runtime_state(payment));
        }
        let payment = apply_runtime_event_payload(
            &self.payments,
            &self.runtime_events,
            &self.next_runtime_event_seq,
            payload_hex,
            source,
            RuntimeEventApplyMode::StrictPaymentStatus,
        )?
        .ok_or_else(|| JsValue::from_str(sdk_contracts::ERR_PAYMENT_NOT_FOUND))?;
        crate::swap_runtime::apply_payment_status_update(&payment.payment_hash, &payment.status);
        self.sync_rgb_ln_transfer_from_payment(&payment);
        self.persist_runtime_event_log_state();
        Ok(payment)
    }

    fn apply_and_record_transport_event(
        &self,
        event: RuntimeTransportEvent,
        source: &str,
    ) -> Result<RuntimeTransportEventApplyData, JsValue> {
        let payload_hex = encode_transport_event_payload(&event);
        self.apply_and_record_transport_event_from_parsed(event, payload_hex, source)
    }

    fn apply_and_record_transport_event_from_payload_hex(
        &self,
        payload_hex: String,
        source: &str,
    ) -> Result<RuntimeTransportEventApplyData, JsValue> {
        let _ = self
            .runtime_core
            .enqueue_event("transport".to_string(), payload_hex.clone());
        let Some(event) = parse_transport_event_payload(&payload_hex) else {
            let received_at = unix_now_secs();
            let seq = next_runtime_event_seq(&self.next_runtime_event_seq);
            let error = "unrecognized transport event payload format".to_string();
            record_runtime_event(
                &self.runtime_events,
                RlnWasmNodeRuntimeEventData {
                    seq,
                    source: source.to_string(),
                    event_kind: classify_non_payment_payload_kind(&payload_hex),
                    payload_hex,
                    payment_hash: None,
                    status: None,
                    applied: false,
                    error: Some(error.clone()),
                    received_at,
                },
            );
            self.persist_runtime_event_log_state();
            return Err(JsValue::from_str(&error));
        };
        self.apply_and_record_transport_event_from_parsed(event, payload_hex, source)
    }

    fn apply_and_record_transport_event_from_parsed(
        &self,
        event: RuntimeTransportEvent,
        payload_hex: String,
        source: &str,
    ) -> Result<RuntimeTransportEventApplyData, JsValue> {
        let _ = self
            .runtime_core
            .enqueue_event(event.event_kind().to_string(), payload_hex.clone());
        let received_at = unix_now_secs();
        let seq = next_runtime_event_seq(&self.next_runtime_event_seq);
        let event_kind = event.event_kind().to_string();
        let applied = self.apply_runtime_transport_event(&event);
        record_runtime_event(
            &self.runtime_events,
            RlnWasmNodeRuntimeEventData {
                seq,
                source: source.to_string(),
                event_kind: event_kind.clone(),
                payload_hex,
                payment_hash: None,
                status: None,
                applied,
                error: if applied {
                    None
                } else {
                    Some("transport event target not found".to_string())
                },
                received_at,
            },
        );
        self.persist_runtime_event_log_state();
        Ok(RuntimeTransportEventApplyData {
            event_kind,
            applied,
        })
    }

    fn next_payment_number(&self) -> u64 {
        let mut seq = self.next_payment_seq.borrow_mut();
        *seq += 1;
        *seq
    }

    fn next_payment_identity(&self) -> (String, String) {
        let n = self.next_payment_number();
        let seed = format!(
            "{}:{}:{n}",
            self.persistence_keys.runtime_scope_key, self.node_instance_nonce
        );
        let payment_id = hex::encode(Sha256::hash(format!("payment-id:{seed}").as_bytes()));
        let payment_hash = hex::encode(Sha256::hash(format!("payment-hash:{seed}").as_bytes()));
        (payment_id, payment_hash)
    }

    fn next_invoice_payment_identity(&self) -> (Sha256, PaymentSecret) {
        let n = self.next_payment_number();
        let seed = format!(
            "{}:{}:{n}",
            self.persistence_keys.runtime_scope_key, self.node_instance_nonce
        );
        let payment_hash = Sha256::hash(format!("invoice-payment-hash:{seed}").as_bytes());
        let payment_secret_hash = Sha256::hash(format!("invoice-payment-secret:{seed}").as_bytes());
        (
            payment_hash,
            PaymentSecret(payment_secret_hash.to_byte_array()),
        )
    }

    fn node_signing_identity(&self) -> Result<(SecretKey, SecpPublicKey), JsValue> {
        let sdk_seed = crate::sdk_node_identity_seed();
        let secret_hash = Sha256::hash(
            format!(
                "node-signing-key:{}:{}:{}",
                sdk_seed.as_deref().unwrap_or("ephemeral"),
                self.proxy_url,
                self.node_runtime_id.as_deref().unwrap_or("")
            )
            .as_bytes(),
        );
        let secret_key = SecretKey::from_slice(&secret_hash.to_byte_array())
            .map_err(|e| JsValue::from_str(&format!("failed to derive node signing key: {e}")))?;
        let pubkey = SecpPublicKey::from_secret_key(&Secp256k1::new(), &secret_key);
        Ok((secret_key, pubkey))
    }

    fn sign_node_message(&self, message: &str) -> Result<String, JsValue> {
        let (secret_key, _) = self.node_signing_identity()?;
        let digest = Sha256::hash(message.as_bytes());
        let msg = SecpMessage::from_digest_slice(&digest.to_byte_array())
            .map_err(|e| JsValue::from_str(&format!("failed to hash message: {e}")))?;
        let signature = Secp256k1::new().sign_ecdsa_recoverable(&msg, &secret_key);
        let (recovery_id, compact) = signature.serialize_compact();
        let mut encoded = [0u8; 65];
        encoded[..64].copy_from_slice(&compact);
        encoded[64] = recovery_id.to_i32() as u8;
        Ok(hex::encode(encoded))
    }

    fn invoice_currency(&self) -> Result<Currency, JsValue> {
        match self.network.borrow().as_str() {
            "mainnet" => Ok(Currency::Bitcoin),
            "testnet" => Ok(Currency::BitcoinTestnet),
            "testnet4" => Ok(Currency::Regtest),
            "signet" => Ok(Currency::Signet),
            "regtest" => Ok(Currency::Regtest),
            other => Err(JsValue::from_str(&format!(
                "unsupported network for invoice creation: {other}"
            ))),
        }
    }

    fn apply_runtime_transport_event(&self, event: &RuntimeTransportEvent) -> bool {
        if self.use_runtime_state_for_ln_views() {
            return match event {
                RuntimeTransportEvent::PeerDisconnected { peer_pubkey } => {
                    let removed_peer = self.ldk_runtime.remove_peer(peer_pubkey);
                    let removed_virtual_channel_ids = self
                        .ldk_runtime
                        .list_channels()
                        .into_iter()
                        .filter(|entry| {
                            entry.peer_pubkey == *peer_pubkey
                                && entry.virtual_open_mode.as_deref()
                                    == Some(SDK_VIRTUAL_OPEN_MODE_TRUSTED_NO_BROADCAST)
                        })
                        .map(|entry| entry.channel_id)
                        .collect::<Vec<_>>();
                    let removed_channels =
                        self.ldk_runtime.remove_channels_by_peer(peer_pubkey) > 0;
                    for channel_id in removed_virtual_channel_ids {
                        self.unregister_trusted_virtual_scope_channel(&channel_id);
                    }
                    removed_peer || removed_channels
                }
                RuntimeTransportEvent::PeerReconnected { peer_pubkey } => {
                    self.ldk_runtime.has_peer(peer_pubkey)
                }
                RuntimeTransportEvent::ChannelClosed { channel_id } => {
                    let removed_runtime = self.ldk_runtime.remove_channel(channel_id);
                    let mut local_channels = self.channels.borrow_mut();
                    let local_before = local_channels.len();
                    local_channels.retain(|_, entry| {
                        entry.data.channel_id != *channel_id
                            && entry.data.temporary_channel_id != *channel_id
                    });
                    let removed_local = local_channels.len() != local_before;
                    let removed = removed_runtime || removed_local;
                    if removed {
                        self.unregister_trusted_virtual_scope_channel(channel_id);
                    }
                    removed
                }
                RuntimeTransportEvent::ChannelUsable { channel_id } => {
                    self.ldk_runtime.set_channel_usable(channel_id, true)
                }
                RuntimeTransportEvent::ChannelUnusable { channel_id } => {
                    self.ldk_runtime.set_channel_usable(channel_id, false)
                }
            };
        }
        match event {
            RuntimeTransportEvent::PeerDisconnected { peer_pubkey } => {
                let removed_peer = self.peers.borrow_mut().remove(peer_pubkey).is_some();
                let mut channels = self.channels.borrow_mut();
                let removed_channel_ids = channels
                    .values()
                    .filter(|ch| ch.data.peer_pubkey == *peer_pubkey)
                    .map(|ch| ch.data.channel_id.clone())
                    .collect::<Vec<_>>();
                let before = channels.len();
                channels.retain(|_, ch| ch.data.peer_pubkey != *peer_pubkey);
                let removed_channels = channels.len() != before;
                if removed_channels {
                    for channel_id in removed_channel_ids {
                        let _ = self.ldk_runtime.virtual_channel_session_update_status(
                            &channel_id,
                            LdkRuntimeVirtualChannelSessionStatusData::Abandoned,
                        );
                        self.unregister_trusted_virtual_scope_channel(&channel_id);
                    }
                }
                removed_peer || removed_channels
            }
            RuntimeTransportEvent::PeerReconnected { peer_pubkey } => {
                self.peers.borrow().contains_key(peer_pubkey)
            }
            RuntimeTransportEvent::ChannelClosed { channel_id } => {
                let removed = self.channels.borrow_mut().remove(channel_id).is_some();
                if removed {
                    let _ = self.ldk_runtime.virtual_channel_session_update_status(
                        channel_id,
                        LdkRuntimeVirtualChannelSessionStatusData::Abandoned,
                    );
                    self.unregister_trusted_virtual_scope_channel(channel_id);
                }
                removed
            }
            RuntimeTransportEvent::ChannelUsable { channel_id } => {
                let mut guard = self.channels.borrow_mut();
                let Some(channel) = guard.get_mut(channel_id) else {
                    return false;
                };
                channel.data.is_usable = true;
                channel.data.ready = true;
                channel.data.status = "opened".to_string();
                true
            }
            RuntimeTransportEvent::ChannelUnusable { channel_id } => {
                let mut guard = self.channels.borrow_mut();
                let Some(channel) = guard.get_mut(channel_id) else {
                    return false;
                };
                channel.data.is_usable = false;
                channel.data.ready = false;
                channel.data.status = "pending".to_string();
                true
            }
        }
    }
}

impl Drop for RlnWasmNode {
    fn drop(&mut self) {
        crate::ldk_runtime::release_runtime_manager_if_last(
            &self.runtime_manager_key(),
            &self.ldk_runtime,
        );
    }
}

fn unix_now_secs() -> u64 {
    (js_sys::Date::now() as u64) / 1000
}

/// The node-level network string (as consumed by `invoice_currency` and the chain-sync driver)
/// for a given rgb-lib WASM network. `SignetCustom` collapses to `"signet"`, matching the LDK
/// network mapping in `ldk_live_backend::rgb_network_to_bitcoin_network`.
fn rgb_network_label(network: rgb_lib_wasm::BitcoinNetwork) -> &'static str {
    match network {
        rgb_lib_wasm::BitcoinNetwork::Mainnet => "mainnet",
        rgb_lib_wasm::BitcoinNetwork::Testnet => "testnet",
        rgb_lib_wasm::BitcoinNetwork::Testnet4 => "testnet4",
        rgb_lib_wasm::BitcoinNetwork::Signet => "signet",
        rgb_lib_wasm::BitcoinNetwork::Regtest => "regtest",
        rgb_lib_wasm::BitcoinNetwork::SignetCustom => "signet",
    }
}

fn normalize_payment_status(status: &str) -> Result<String, JsValue> {
    let normalized = status.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "pending" | "claimable" | "claiming" | "succeeded" | "cancelled" | "failed" | "expired" => Ok(normalized),
        _ => Err(JsValue::from_str(
            "status must be one of: pending, claimable, claiming, succeeded, cancelled, failed, expired",
        )),
    }
}

fn fail_pending_payments_with_runtime_events(
    payments: &Rc<RefCell<HashMap<String, PaymentEntry>>>,
    runtime_events: &Rc<RefCell<Vec<RlnWasmNodeRuntimeEventData>>>,
    next_runtime_event_seq_ref: &Rc<RefCell<u64>>,
    source: &str,
    status: &str,
) -> Result<usize, JsValue> {
    let pending_hashes = payments
        .borrow()
        .iter()
        .filter(|(_, entry)| entry.data.status == "pending")
        .map(|(hash, _)| hash.clone())
        .collect::<Vec<_>>();
    let mut applied = 0usize;
    for payment_hash in pending_hashes {
        let payload_hex = encode_payment_status_event_payload(&payment_hash, status);
        let result = apply_runtime_event_payload(
            payments,
            runtime_events,
            next_runtime_event_seq_ref,
            payload_hex,
            source,
            RuntimeEventApplyMode::StrictPaymentStatus,
        )?;
        if result.is_some() {
            applied += 1;
        }
    }
    Ok(applied)
}

fn set_payment_status(
    payments: &Rc<RefCell<HashMap<String, PaymentEntry>>>,
    payment_hash: &str,
    status: &str,
) -> Result<(), JsValue> {
    let normalized = normalize_payment_status(status)?;
    let mut guard = payments.borrow_mut();
    let Some(payment) = guard.get_mut(payment_hash) else {
        return Err(JsValue::from_str(sdk_contracts::ERR_PAYMENT_NOT_FOUND));
    };
    if !is_valid_payment_status_transition(&payment.data.status, &normalized) {
        return Err(JsValue::from_str(&format!(
            "invalid payment status transition: {} -> {}",
            payment.data.status, normalized
        )));
    }
    payment.data.status = normalized;
    payment.data.updated_at = unix_now_secs();
    Ok(())
}

fn is_valid_payment_status_transition(current: &str, next: &str) -> bool {
    if current == next {
        return true;
    }
    match current {
        "pending" => matches!(next, "claimable" | "succeeded" | "failed" | "expired"),
        "claimable" => matches!(
            next,
            "claiming" | "succeeded" | "cancelled" | "failed" | "expired"
        ),
        "claiming" => matches!(next, "succeeded" | "failed"),
        "succeeded" | "cancelled" | "expired" => false,
        _ => true,
    }
}

fn next_runtime_event_seq(next_runtime_event_seq: &Rc<RefCell<u64>>) -> u64 {
    let mut guard = next_runtime_event_seq.borrow_mut();
    *guard += 1;
    *guard
}

fn normalize_node_runtime_id(node_runtime_id: Option<String>) -> Result<Option<String>, JsValue> {
    match node_runtime_id {
        Some(value) => {
            let normalized = value.trim().to_string();
            if normalized.is_empty() {
                Err(JsValue::from_str(sdk_contracts::ERR_NODE_RUNTIME_ID_EMPTY))
            } else {
                Ok(Some(normalized))
            }
        }
        None => Ok(None),
    }
}

fn runtime_scope_key(proxy_url: &str, node_runtime_id: Option<&str>) -> String {
    let proxy_url = crate::ldk_runtime::canonicalize_proxy_endpoint(proxy_url);
    match node_runtime_id {
        Some(node_runtime_id) if !node_runtime_id.trim().is_empty() => {
            format!("{proxy_url}#runtime:{}", node_runtime_id.trim())
        }
        _ => proxy_url,
    }
}

fn load_runtime_event_log_snapshot(storage_key: &str) -> Option<RuntimeEventLogSnapshot> {
    let store = browser_persistent_state_store();
    if let Ok(Some(snapshot)) = store.get_json::<RuntimeEventLogSnapshot>(storage_key) {
        RUNTIME_EVENT_LOG_STORAGE.with(|state| {
            state
                .borrow_mut()
                .insert(storage_key.to_string(), snapshot.clone());
        });
        return Some(snapshot);
    }

    RUNTIME_EVENT_LOG_STORAGE.with(|state| state.borrow().get(storage_key).cloned())
}

fn load_runtime_rgb_ln_transfer_snapshot(
    storage_key: &str,
) -> Option<RuntimeRgbLnTransferSnapshot> {
    let store = browser_persistent_state_store();
    if let Ok(Some(snapshot)) = store.get_json::<RuntimeRgbLnTransferSnapshot>(storage_key) {
        RUNTIME_RGB_LN_TRANSFER_STORAGE.with(|state| {
            state
                .borrow_mut()
                .insert(storage_key.to_string(), snapshot.clone());
        });
        return Some(snapshot);
    }
    RUNTIME_RGB_LN_TRANSFER_STORAGE.with(|state| state.borrow().get(storage_key).cloned())
}

fn load_runtime_peer_session_snapshot(storage_key: &str) -> Option<RuntimePeerSessionSnapshot> {
    let store = browser_persistent_state_store();
    if let Ok(Some(mut snapshot)) = store.get_json::<RuntimePeerSessionSnapshot>(storage_key) {
        sanitize_runtime_peer_session_snapshot(&mut snapshot);
        RUNTIME_PEER_SESSION_STORAGE.with(|state| {
            state
                .borrow_mut()
                .insert(storage_key.to_string(), snapshot.clone());
        });
        return Some(snapshot);
    }
    RUNTIME_PEER_SESSION_STORAGE.with(|state| {
        state
            .borrow()
            .get(storage_key)
            .cloned()
            .map(|mut snapshot| {
                sanitize_runtime_peer_session_snapshot(&mut snapshot);
                snapshot
            })
    })
}

fn load_virtual_channels_v0_flag(storage_key: &str) -> Option<bool> {
    let store = browser_persistent_state_store();
    store.get_json::<bool>(storage_key).ok().flatten()
}

fn persist_virtual_channels_v0_flag(storage_key: &str, enabled: bool) {
    let store = browser_persistent_state_store();
    let _ = store.set_json(storage_key, &enabled);
}

fn persist_runtime_event_log_state(
    storage_key: &str,
    runtime_events: &Rc<RefCell<Vec<RlnWasmNodeRuntimeEventData>>>,
    next_runtime_event_seq_ref: &Rc<RefCell<u64>>,
) {
    let events = {
        let guard = runtime_events.borrow();
        let start = guard.len().saturating_sub(RUNTIME_EVENT_LOG_PERSIST_WINDOW);
        guard[start..].to_vec()
    };
    let snapshot = RuntimeEventLogSnapshot {
        events,
        next_seq: *next_runtime_event_seq_ref.borrow(),
    };
    let store = browser_persistent_state_store();
    let _ = store.set_json(storage_key, &snapshot);
    RUNTIME_EVENT_LOG_STORAGE.with(|state| {
        state.borrow_mut().insert(storage_key.to_string(), snapshot);
    });
}

fn persist_runtime_rgb_ln_transfer_state(
    storage_key: &str,
    rgb_ln_transfers: &Rc<RefCell<HashMap<String, RlnWasmNodeRgbLnTransferData>>>,
) {
    let mut transfers = rgb_ln_transfers
        .borrow()
        .values()
        .cloned()
        .collect::<Vec<_>>();
    transfers.sort_by(|a, b| a.payment_hash.cmp(&b.payment_hash));
    let snapshot = RuntimeRgbLnTransferSnapshot { transfers };
    let store = browser_persistent_state_store();
    let _ = store.set_json(storage_key, &snapshot);
    RUNTIME_RGB_LN_TRANSFER_STORAGE.with(|state| {
        state.borrow_mut().insert(storage_key.to_string(), snapshot);
    });
}

fn persist_runtime_peer_session_state(
    storage_key: &str,
    runtime_scope_key: &str,
    peers: &Rc<RefCell<HashMap<String, PeerEntry>>>,
) {
    let mut sessions = peers
        .borrow()
        .iter()
        .map(|(peer_pubkey, entry)| RuntimePeerSessionEntryData {
            session_key: runtime_peer_session_key(runtime_scope_key, peer_pubkey),
            peer_pubkey: peer_pubkey.clone(),
            peer_addr: entry.peer_addr.clone(),
        })
        .collect::<Vec<_>>();
    sessions.sort_by(|a, b| a.session_key.cmp(&b.session_key));
    let snapshot = RuntimePeerSessionSnapshot { sessions };
    let store = browser_persistent_state_store();
    let _ = store.set_json(storage_key, &snapshot);
    RUNTIME_PEER_SESSION_STORAGE.with(|state| {
        state.borrow_mut().insert(storage_key.to_string(), snapshot);
    });
}

fn runtime_peer_session_key(runtime_scope_key: &str, peer_pubkey: &str) -> String {
    format!("{}::{}", runtime_scope_key.trim(), peer_pubkey.trim())
}

fn runtime_peer_session_key_legacy(
    runtime_scope_key: &str,
    peer_pubkey: &str,
    peer_addr: &str,
) -> String {
    format!(
        "{}::{}::{}",
        runtime_scope_key.trim(),
        peer_pubkey.trim(),
        peer_addr.trim().to_ascii_lowercase()
    )
}

fn sanitize_runtime_peer_session_snapshot(snapshot: &mut RuntimePeerSessionSnapshot) {
    let mut dedup = HashMap::<String, RuntimePeerSessionEntryData>::new();
    for mut entry in snapshot.sessions.drain(..) {
        let peer_pubkey = entry.peer_pubkey.trim().to_string();
        let peer_addr = entry.peer_addr.trim().to_string();
        if peer_pubkey.is_empty() || peer_addr.is_empty() {
            continue;
        }
        entry.peer_pubkey = peer_pubkey;
        entry.peer_addr = peer_addr;
        // New stable identity is by peer pubkey within a runtime scope.
        dedup.insert(entry.peer_pubkey.clone(), entry);
    }
    let mut sessions = dedup.into_values().collect::<Vec<_>>();
    sessions.sort_by(|a, b| a.peer_pubkey.cmp(&b.peer_pubkey));
    snapshot.sessions = sessions;
}

async fn reconnect_persisted_peers_once(
    proxy_url: &str,
    runtime_scope_key: &str,
    peer_session_store_key: &str,
    relay_session_auth: Option<RlnWasmNodeRelaySessionAuthData>,
    peers: &Rc<RefCell<HashMap<String, PeerEntry>>>,
    ldk_runtime: &Rc<dyn LdkRuntimeManager>,
) -> RuntimeReconnectPeersResultData {
    let snapshot = load_runtime_peer_session_snapshot(peer_session_store_key).unwrap_or_default();
    let mut sessions = snapshot.sessions;
    sessions.sort_by(|a, b| a.session_key.cmp(&b.session_key));
    let mut connected = 0usize;
    let mut failed = Vec::new();

    for entry in sessions {
        let expected_key = runtime_peer_session_key(runtime_scope_key, &entry.peer_pubkey);
        let expected_legacy = runtime_peer_session_key_legacy(
            runtime_scope_key,
            &entry.peer_pubkey,
            &entry.peer_addr,
        );
        if expected_key != entry.session_key && expected_legacy != entry.session_key {
            failed.push(format!("{}: session key mismatch", entry.session_key));
            continue;
        }
        if peers.borrow().contains_key(&entry.peer_pubkey) {
            connected = connected.saturating_add(1);
            continue;
        }
        let bridge = match RlnWasmRustPeerManagerBridge::new(None) {
            Ok(bridge) => bridge,
            Err(err) => {
                failed.push(format!(
                    "{}: bridge init failed: {}",
                    entry.session_key,
                    err.as_string().unwrap_or_else(|| "unknown".to_string())
                ));
                continue;
            }
        };
        let session = if let Some(auth) = relay_session_auth.clone() {
            let options_js = match crate::js_obj(&RlnWasmLnSocketConnectOptionsData {
                max_reconnect_attempts: Some(3),
                reconnect_initial_delay_ms: Some(250),
                reconnect_max_delay_ms: Some(4_000),
                relay_auth_token: Some(auth.relay_auth_token),
                relay_node_id: Some(auth.relay_node_id),
                replay_transport_envelope: Some(false),
                replay_session_id: None,
                replay_last_applied_seq: None,
            }) {
                Ok(options) => options,
                Err(err) => {
                    failed.push(format!(
                        "{}: relay options encode failed: {}",
                        entry.session_key,
                        err.as_string().unwrap_or_else(|| "unknown".to_string())
                    ));
                    continue;
                }
            };
            bridge
                .connect_session_with_options(
                    proxy_url.to_string(),
                    entry.peer_addr.clone(),
                    entry.peer_pubkey.clone(),
                    options_js,
                )
                .await
        } else {
            bridge
                .connect_session(
                    proxy_url.to_string(),
                    entry.peer_addr.clone(),
                    entry.peer_pubkey.clone(),
                )
                .await
        };

        let session = match session {
            Ok(session) => session,
            Err(err) => {
                failed.push(format!(
                    "{}: connect failed: {}",
                    entry.session_key,
                    err.as_string().unwrap_or_else(|| "unknown".to_string())
                ));
                continue;
            }
        };
        if let Err(err) = session.start().await {
            failed.push(format!(
                "{}: start failed: {}",
                entry.session_key,
                err.as_string().unwrap_or_else(|| "unknown".to_string())
            ));
            continue;
        }
        peers.borrow_mut().insert(
            entry.peer_pubkey.clone(),
            PeerEntry {
                peer_addr: entry.peer_addr.clone(),
                session: Rc::new(session),
            },
        );
        ldk_runtime.upsert_peer(LdkRuntimePeerStateData {
            pubkey: entry.peer_pubkey.clone(),
            peer_addr: entry.peer_addr.clone(),
            started: true,
        });
        connected = connected.saturating_add(1);
    }

    RuntimeReconnectPeersResultData {
        attempted: connected.saturating_add(failed.len()),
        connected,
        failed,
    }
}

#[cfg(target_arch = "wasm32")]
async fn sleep_ms(ms: u32) {
    let promise = js_sys::Promise::new(&mut |resolve, _reject| {
        if let Some(window) = web_sys::window() {
            let _ =
                window.set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, ms as i32);
        } else {
            let _ = resolve.call0(&JsValue::NULL);
        }
    });
    let _ = JsFuture::from(promise).await;
}

#[cfg(not(target_arch = "wasm32"))]
async fn sleep_ms(ms: u32) {
    std::thread::sleep(std::time::Duration::from_millis(ms as u64));
}

fn record_runtime_event(
    runtime_events: &Rc<RefCell<Vec<RlnWasmNodeRuntimeEventData>>>,
    entry: RlnWasmNodeRuntimeEventData,
) {
    runtime_events.borrow_mut().push(entry);
}

fn record_runtime_control_event(
    runtime_events: &Rc<RefCell<Vec<RlnWasmNodeRuntimeEventData>>>,
    next_runtime_event_seq_ref: &Rc<RefCell<u64>>,
    source: &str,
    payload_hex: String,
    error: Option<String>,
) {
    let seq = next_runtime_event_seq(next_runtime_event_seq_ref);
    record_runtime_event(
        runtime_events,
        RlnWasmNodeRuntimeEventData {
            seq,
            source: source.to_string(),
            event_kind: "control".to_string(),
            payload_hex,
            payment_hash: None,
            status: None,
            applied: false,
            error,
            received_at: unix_now_secs(),
        },
    );
}

fn apply_runtime_event_payload(
    payments: &Rc<RefCell<HashMap<String, PaymentEntry>>>,
    runtime_events: &Rc<RefCell<Vec<RlnWasmNodeRuntimeEventData>>>,
    next_runtime_event_seq_ref: &Rc<RefCell<u64>>,
    payload_hex: String,
    source: &str,
    mode: RuntimeEventApplyMode,
) -> Result<Option<RlnWasmNodePaymentData>, JsValue> {
    let received_at = unix_now_secs();
    let seq = next_runtime_event_seq(next_runtime_event_seq_ref);
    let Some(event) = parse_payment_status_event_payload(&payload_hex) else {
        let error = "unrecognized event payload format for payment status update".to_string();
        let event_kind = classify_non_payment_payload_kind(&payload_hex);
        record_runtime_event(
            runtime_events,
            RlnWasmNodeRuntimeEventData {
                seq,
                source: source.to_string(),
                event_kind,
                payload_hex,
                payment_hash: None,
                status: None,
                applied: false,
                error: Some(error.clone()),
                received_at,
            },
        );
        return match mode {
            RuntimeEventApplyMode::StrictPaymentStatus => Err(JsValue::from_str(&error)),
            RuntimeEventApplyMode::TolerantTransport => Ok(None),
        };
    };

    if let Err(err) = set_payment_status(payments, &event.payment_hash, &event.status) {
        let error = err
            .as_string()
            .unwrap_or_else(|| "failed to apply runtime event".to_string());
        record_runtime_event(
            runtime_events,
            RlnWasmNodeRuntimeEventData {
                seq,
                source: source.to_string(),
                event_kind: "payment_status".to_string(),
                payload_hex,
                payment_hash: Some(event.payment_hash.clone()),
                status: Some(event.status.clone()),
                applied: false,
                error: Some(error.clone()),
                received_at,
            },
        );
        return match mode {
            RuntimeEventApplyMode::StrictPaymentStatus => Err(JsValue::from_str(&error)),
            RuntimeEventApplyMode::TolerantTransport => Ok(None),
        };
    }

    let payment = payments
        .borrow()
        .get(&event.payment_hash)
        .map(|entry| entry.data.clone());
    record_runtime_event(
        runtime_events,
        RlnWasmNodeRuntimeEventData {
            seq,
            source: source.to_string(),
            event_kind: "payment_status".to_string(),
            payload_hex,
            payment_hash: Some(event.payment_hash),
            status: Some(event.status),
            applied: true,
            error: None,
            received_at,
        },
    );
    Ok(payment)
}

/// Apply the latest indexer chain state to the live LDK backend.
///
/// Free-function form of the chain-sync→LDK bridge so it can be driven from both the manual
/// `chainSyncTick` (`&self` wrapper) and the autonomous drive loop (cloned `Rc` handles), without
/// duplicating the confirmation-ordering logic. Behavior is identical to the previous `&self`
/// method; only `self.chain_sync`/`self.ldk_runtime` became explicit parameters.
async fn apply_chain_sync_to_live_ldk(
    chain_sync: &WasmChainSyncDriver,
    ldk_runtime: &Rc<dyn LdkRuntimeManager>,
) -> Result<(), JsValue> {
    let status = chain_sync.status();
    let Some(indexer_url) = status.indexer_url else {
        return Ok(());
    };
    let Some(tip_height) = status.latest_tip_height else {
        return Ok(());
    };
    if tip_height == 0 {
        return Ok(());
    }
    // A lower Esplora tip cannot be applied with `best_block_updated`; LDK requires explicit
    // block-disconnect notifications for a real reorg. Electrs can briefly serve an older tip
    // while indexing freshly mined regtest blocks, and applying it directly makes a locked-in
    // funding transaction appear to have zero confirmations.
    if status.tip_regressed {
        wasm_debug(&format!(
            "[rln-wasm-sdk chain-sync] indexer tip regressed to {tip_height}; skipping live LDK chain application until the tip catches up"
        ));
        return Ok(());
    }

    let relevant = ldk_runtime.chain_relevant_txids()?;
    if relevant.is_empty() {
        return Ok(());
    }

    let mut header_cache: HashMap<String, String> = HashMap::new();

    #[derive(Clone)]
    struct ConfirmedTx {
        txid: String,
        height: u32,
        block_hash: String,
    }

    let max_txids = 128usize;
    let mut confirmed: Vec<ConfirmedTx> = Vec::new();
    for txid in relevant.into_iter().take(max_txids) {
        match fetch_tx_status(&indexer_url, &txid).await? {
            None => {
                // Do NOT mark the tx unconfirmed on a single negative lookup. Esplora/electrs
                // routinely reports a freshly confirmed tx as missing for a tick or two right
                // after a block is mined (indexing lag). Feeding that transient miss to LDK as
                // `transaction_unconfirmed` force-closes the channel with
                // "Locked at 6 confs, now have 0 confs". A genuine reorg is still reflected via
                // the best-block/confirmed-tx updates below, so skipping the spurious unconfirm
                // is safe (and there are no reorgs on regtest).
                wasm_debug(&format!(
                    "[rln-wasm-sdk chain-sync] relevant tx {txid} not reported confirmed by indexer this tick; skipping unconfirm (likely indexing lag)"
                ));
            }
            Some((height, block_hash)) => {
                confirmed.push(ConfirmedTx {
                    txid,
                    height,
                    block_hash,
                });
            }
        }
    }
    // Esplora endpoints are not indexed atomically. `/tx/:txid/status` may already report a
    // confirmation at height N while `/blocks/tip/height` still reports N-1. Never advance LDK
    // to a height below a confirmation applied in this same tick, or the channel immediately
    // force-closes as "Locked at 6 confs, now have 0 confs".
    let effective_tip_height = confirmed
        .iter()
        .map(|tx| tx.height)
        .max()
        .unwrap_or(tip_height)
        .max(tip_height);
    let tip_header_hex =
        fetch_block_header_hex_by_height(&indexer_url, effective_tip_height).await?;

    // LDK requires confirmations in chain order; within a block, txs should follow block order
    // (and ideally topological order — we approximate via Esplora tx index).
    confirmed.sort_by(|a, b| {
        a.height
            .cmp(&b.height)
            .then_with(|| a.block_hash.cmp(&b.block_hash))
            .then_with(|| a.txid.cmp(&b.txid))
    });

    let mut cursor = 0usize;
    while cursor < confirmed.len() {
        let height = confirmed[cursor].height;
        let block_hash = confirmed[cursor].block_hash.clone();

        let header_hex = if let Some(cached) = header_cache.get(&block_hash) {
            cached.clone()
        } else {
            let hdr = fetch_block_header_hex(&indexer_url, &block_hash).await?;
            header_cache.insert(block_hash.clone(), hdr.clone());
            hdr
        };

        let mut end = cursor + 1;
        while end < confirmed.len()
            && confirmed[end].height == height
            && confirmed[end].block_hash == block_hash
        {
            end += 1;
        }

        let mut with_pos: Vec<(usize, String)> = Vec::with_capacity(end.saturating_sub(cursor));
        for ct in confirmed[cursor..end].iter() {
            let tx_index = fetch_tx_index_in_block(&indexer_url, &block_hash, &ct.txid).await?;
            with_pos.push((tx_index, ct.txid.clone()));
        }
        with_pos.sort_by_key(|(pos, _)| *pos);

        for (tx_index, txid) in with_pos {
            let tx_hex = fetch_tx_hex(&indexer_url, &txid).await?;
            ldk_runtime.chain_apply_confirmed_tx(height, &header_hex, tx_index, &tx_hex)?;
        }
        ldk_runtime.chain_apply_best_block(height, &header_hex)?;

        cursor = end;
    }
    // LDK expects tx confirmations to be applied before advancing the best block.
    // Advancing to the tip first can make a later historical funding confirmation fail to
    // drive the channel lock-in transition.
    ldk_runtime.chain_apply_best_block(effective_tip_height, &tip_header_hex)?;
    Ok(())
}

/// Run one full node "drive" pass: advance chain sync, apply it to the live LDK backend, drain the
/// native runtime queue and buffered peer-manager hook payloads, process peer events, and reconcile
/// the cached channel snapshot from the live `ChannelManager` *last* (so the authoritative live set
/// is the final word and pre-funding temp-id "ghost" entries are purged — PARITY_PLAN 0.1).
///
/// This is the shared body of `chainSyncTick` and the autonomous drive loop (`autoDriveStart`), so
/// both paths progress channel/payment state identically with no manual JS ticking required
/// (PARITY_PLAN 0.2).
#[allow(clippy::too_many_arguments)]
async fn node_drive_tick_once(
    chain_sync: &WasmChainSyncDriver,
    ldk_runtime: &Rc<dyn LdkRuntimeManager>,
    runtime_core: &NativeLnRuntimeCore,
    peers: &Rc<RefCell<HashMap<String, PeerEntry>>>,
    channels: &Rc<RefCell<HashMap<String, ChannelEntry>>>,
    payments: &Rc<RefCell<HashMap<String, PaymentEntry>>>,
    pending_peer_hook_events: &Rc<RefCell<Vec<PendingPeerHookEvent>>>,
    runtime_events: &Rc<RefCell<Vec<RlnWasmNodeRuntimeEventData>>>,
    next_runtime_event_seq: &Rc<RefCell<u64>>,
    runtime_events_storage_key: &str,
    label: &str,
) -> Result<(), JsValue> {
    let use_runtime_state_for_ln_views = ldk_runtime.status().backend == "wasm_native_ldk";

    chain_sync.tick().await?;
    apply_chain_sync_to_live_ldk(chain_sync, ldk_runtime).await?;

    // Drive event ingestion so channel/payment state progresses in WASM. The regular node can reach
    // lock-in and send `channel_ready`, but the WASM side only updates its channel view once we
    // drain both the native runtime queue and the buffered peer-manager hook payloads.
    let drained = runtime_core.drain_events();
    for queued in drained.iter() {
        apply_runtime_hook_payload(
            ldk_runtime,
            use_runtime_state_for_ln_views,
            peers,
            channels,
            payments,
            runtime_events,
            next_runtime_event_seq,
            queued.payload_hex.clone(),
            "native_runtime_queue",
        )?;
    }
    ldk_runtime.peer_process_events()?;
    let _ = drain_pending_peer_hook_events(
        ldk_runtime,
        use_runtime_state_for_ln_views,
        peers,
        channels,
        payments,
        pending_peer_hook_events,
        runtime_events,
        next_runtime_event_seq,
        label,
    )?;

    // Reconcile the cached channel snapshot from the live backend LAST, so the authoritative live
    // `ChannelManager` set is the final word for this pass and purges any pre-funding temporary-id
    // "ghost" entry the draining steps above may have (re-)inserted (PARITY_PLAN 0.1).
    let _ = ldk_runtime.reconcile_channels_from_live();
    persist_runtime_event_log_state(
        runtime_events_storage_key,
        runtime_events,
        next_runtime_event_seq,
    );
    Ok(())
}

fn apply_runtime_hook_payload(
    ldk_runtime: &Rc<dyn LdkRuntimeManager>,
    use_runtime_state_for_ln_views: bool,
    peers: &Rc<RefCell<HashMap<String, PeerEntry>>>,
    channels: &Rc<RefCell<HashMap<String, ChannelEntry>>>,
    payments: &Rc<RefCell<HashMap<String, PaymentEntry>>>,
    runtime_events: &Rc<RefCell<Vec<RlnWasmNodeRuntimeEventData>>>,
    next_runtime_event_seq_ref: &Rc<RefCell<u64>>,
    payload_hex: String,
    source: &str,
) -> Result<(), JsValue> {
    let received_at = unix_now_secs();
    let seq = next_runtime_event_seq(next_runtime_event_seq_ref);

    if let Some(event) = parse_payment_status_event_payload(&payload_hex) {
        let status = event.status.clone();
        if use_runtime_state_for_ln_views {
            let normalized = normalize_payment_status(&status)?;
            let Some(mut payment) = ldk_runtime.get_payment(&event.payment_hash) else {
                record_runtime_event(
                    runtime_events,
                    RlnWasmNodeRuntimeEventData {
                        seq,
                        source: source.to_string(),
                        event_kind: "payment_status".to_string(),
                        payload_hex,
                        payment_hash: Some(event.payment_hash),
                        status: Some(normalized),
                        applied: false,
                        error: Some("payment not found".to_string()),
                        received_at,
                    },
                );
                return Ok(());
            };
            if !is_valid_payment_status_transition(&payment.status, &normalized) {
                record_runtime_event(
                    runtime_events,
                    RlnWasmNodeRuntimeEventData {
                        seq,
                        source: source.to_string(),
                        event_kind: "payment_status".to_string(),
                        payload_hex,
                        payment_hash: Some(event.payment_hash),
                        status: Some(normalized.clone()),
                        applied: false,
                        error: Some(format!(
                            "invalid payment status transition: {} -> {}",
                            payment.status, normalized
                        )),
                        received_at,
                    },
                );
                return Ok(());
            }
            payment.status = normalized.clone();
            payment.updated_at = unix_now_secs();
            ldk_runtime.upsert_payment(payment);
            crate::swap_runtime::apply_payment_status_update(&event.payment_hash, &normalized);
            record_runtime_event(
                runtime_events,
                RlnWasmNodeRuntimeEventData {
                    seq,
                    source: source.to_string(),
                    event_kind: "payment_status".to_string(),
                    payload_hex,
                    payment_hash: Some(event.payment_hash),
                    status: Some(normalized),
                    applied: true,
                    error: None,
                    received_at,
                },
            );
            return Ok(());
        }

        if let Err(err) = set_payment_status(payments, &event.payment_hash, &status) {
            let error = err
                .as_string()
                .unwrap_or_else(|| "failed to apply runtime event".to_string());
            record_runtime_event(
                runtime_events,
                RlnWasmNodeRuntimeEventData {
                    seq,
                    source: source.to_string(),
                    event_kind: "payment_status".to_string(),
                    payload_hex,
                    payment_hash: Some(event.payment_hash),
                    status: Some(status),
                    applied: false,
                    error: Some(error),
                    received_at,
                },
            );
            return Ok(());
        }
        crate::swap_runtime::apply_payment_status_update(&event.payment_hash, &status);

        record_runtime_event(
            runtime_events,
            RlnWasmNodeRuntimeEventData {
                seq,
                source: source.to_string(),
                event_kind: "payment_status".to_string(),
                payload_hex,
                payment_hash: Some(event.payment_hash),
                status: Some(status),
                applied: true,
                error: None,
                received_at,
            },
        );
        return Ok(());
    }

    if let Some(event) = parse_transport_event_payload(&payload_hex) {
        let applied = apply_transport_event_to_state(
            ldk_runtime,
            use_runtime_state_for_ln_views,
            peers,
            channels,
            &event,
        );
        record_runtime_event(
            runtime_events,
            RlnWasmNodeRuntimeEventData {
                seq,
                source: source.to_string(),
                event_kind: event.event_kind().to_string(),
                payload_hex,
                payment_hash: None,
                status: None,
                applied,
                error: if applied {
                    None
                } else {
                    Some("transport event target not found".to_string())
                },
                received_at,
            },
        );
        return Ok(());
    }

    record_runtime_event(
        runtime_events,
        RlnWasmNodeRuntimeEventData {
            seq,
            source: source.to_string(),
            event_kind: classify_non_payment_payload_kind(&payload_hex),
            payload_hex,
            payment_hash: None,
            status: None,
            applied: false,
            error: Some("unrecognized event payload format for payment status update".to_string()),
            received_at,
        },
    );
    Ok(())
}

fn drain_pending_peer_hook_events(
    ldk_runtime: &Rc<dyn LdkRuntimeManager>,
    use_runtime_state_for_ln_views: bool,
    peers: &Rc<RefCell<HashMap<String, PeerEntry>>>,
    channels: &Rc<RefCell<HashMap<String, ChannelEntry>>>,
    payments: &Rc<RefCell<HashMap<String, PaymentEntry>>>,
    pending_peer_hook_events: &Rc<RefCell<Vec<PendingPeerHookEvent>>>,
    runtime_events: &Rc<RefCell<Vec<RlnWasmNodeRuntimeEventData>>>,
    next_runtime_event_seq_ref: &Rc<RefCell<u64>>,
    source: &str,
) -> Result<usize, JsValue> {
    let drained = std::mem::take(&mut *pending_peer_hook_events.borrow_mut());
    let mut applied = 0usize;
    for pending_event in drained {
        match pending_event {
            PendingPeerHookEvent::Payload(payload_hex) => {
                apply_runtime_hook_payload(
                    ldk_runtime,
                    use_runtime_state_for_ln_views,
                    peers,
                    channels,
                    payments,
                    runtime_events,
                    next_runtime_event_seq_ref,
                    payload_hex,
                    source,
                )?;
                applied += 1;
            }
            PendingPeerHookEvent::SocketDisconnected => {
                fail_pending_payments_for_hook(
                    ldk_runtime,
                    use_runtime_state_for_ln_views,
                    peers,
                    channels,
                    payments,
                    runtime_events,
                    next_runtime_event_seq_ref,
                    source,
                )?;
                record_runtime_control_event(
                    runtime_events,
                    next_runtime_event_seq_ref,
                    "peer_hook_disconnected",
                    "".to_string(),
                    Some("peer manager socket disconnected".to_string()),
                );
                applied += 1;
            }
            PendingPeerHookEvent::Error(message) => {
                fail_pending_payments_for_hook(
                    ldk_runtime,
                    use_runtime_state_for_ln_views,
                    peers,
                    channels,
                    payments,
                    runtime_events,
                    next_runtime_event_seq_ref,
                    source,
                )?;
                record_runtime_control_event(
                    runtime_events,
                    next_runtime_event_seq_ref,
                    "peer_hook_error",
                    hex::encode(message.as_bytes()),
                    Some(message),
                );
                applied += 1;
            }
        }
    }
    Ok(applied)
}

fn fail_pending_payments_for_hook(
    ldk_runtime: &Rc<dyn LdkRuntimeManager>,
    use_runtime_state_for_ln_views: bool,
    peers: &Rc<RefCell<HashMap<String, PeerEntry>>>,
    channels: &Rc<RefCell<HashMap<String, ChannelEntry>>>,
    payments: &Rc<RefCell<HashMap<String, PaymentEntry>>>,
    runtime_events: &Rc<RefCell<Vec<RlnWasmNodeRuntimeEventData>>>,
    next_runtime_event_seq_ref: &Rc<RefCell<u64>>,
    source: &str,
) -> Result<usize, JsValue> {
    if use_runtime_state_for_ln_views {
        let pending_hashes = ldk_runtime
            .list_payments()
            .into_iter()
            .filter(|p| p.status == "pending")
            .map(|p| p.payment_hash)
            .collect::<Vec<_>>();
        let mut applied = 0usize;
        for payment_hash in pending_hashes {
            let payload_hex = encode_payment_status_event_payload(&payment_hash, "failed");
            apply_runtime_hook_payload(
                ldk_runtime,
                use_runtime_state_for_ln_views,
                peers,
                channels,
                payments,
                runtime_events,
                next_runtime_event_seq_ref,
                payload_hex,
                source,
            )?;
            applied += 1;
        }
        Ok(applied)
    } else {
        fail_pending_payments_with_runtime_events(
            payments,
            runtime_events,
            next_runtime_event_seq_ref,
            source,
            "failed",
        )
    }
}

fn apply_transport_event_to_state(
    ldk_runtime: &Rc<dyn LdkRuntimeManager>,
    use_runtime_state_for_ln_views: bool,
    peers: &Rc<RefCell<HashMap<String, PeerEntry>>>,
    channels: &Rc<RefCell<HashMap<String, ChannelEntry>>>,
    event: &RuntimeTransportEvent,
) -> bool {
    if use_runtime_state_for_ln_views {
        return match event {
            RuntimeTransportEvent::PeerDisconnected { peer_pubkey } => {
                let removed_peer = ldk_runtime.remove_peer(peer_pubkey);
                let removed_channels = ldk_runtime.remove_channels_by_peer(peer_pubkey) > 0;
                removed_peer || removed_channels
            }
            RuntimeTransportEvent::PeerReconnected { peer_pubkey } => {
                ldk_runtime.set_peer_started(peer_pubkey, true)
            }
            RuntimeTransportEvent::ChannelClosed { channel_id } => {
                ldk_runtime.remove_channel(channel_id)
            }
            RuntimeTransportEvent::ChannelUsable { channel_id } => {
                ldk_runtime.set_channel_usable(channel_id, true)
            }
            RuntimeTransportEvent::ChannelUnusable { channel_id } => {
                ldk_runtime.set_channel_usable(channel_id, false)
            }
        };
    }

    match event {
        RuntimeTransportEvent::PeerDisconnected { peer_pubkey } => {
            let removed_peer = peers.borrow_mut().remove(peer_pubkey).is_some();
            let mut guard = channels.borrow_mut();
            let before = guard.len();
            guard.retain(|_, ch| ch.data.peer_pubkey != *peer_pubkey);
            removed_peer || guard.len() != before
        }
        RuntimeTransportEvent::PeerReconnected { peer_pubkey } => {
            peers.borrow().contains_key(peer_pubkey)
        }
        RuntimeTransportEvent::ChannelClosed { channel_id } => {
            channels.borrow_mut().remove(channel_id).is_some()
        }
        RuntimeTransportEvent::ChannelUsable { channel_id } => {
            let mut guard = channels.borrow_mut();
            let Some(channel) = guard.get_mut(channel_id) else {
                return false;
            };
            channel.data.is_usable = true;
            channel.data.ready = true;
            channel.data.status = "opened".to_string();
            true
        }
        RuntimeTransportEvent::ChannelUnusable { channel_id } => {
            let mut guard = channels.borrow_mut();
            let Some(channel) = guard.get_mut(channel_id) else {
                return false;
            };
            channel.data.is_usable = false;
            channel.data.ready = false;
            channel.data.status = "pending".to_string();
            true
        }
    }
}

fn classify_non_payment_payload_kind(payload_hex: &str) -> String {
    let Ok(bytes) = hex::decode(payload_hex) else {
        return "invalid_hex_payload".to_string();
    };
    let Ok(text) = std::str::from_utf8(&bytes) else {
        return "binary_payload".to_string();
    };
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return "empty_payload".to_string();
    }
    if trimmed.starts_with('{') {
        return "json_payload".to_string();
    }
    if trimmed.contains(':') {
        return "text_protocol_payload".to_string();
    }
    "text_payload".to_string()
}

fn parse_transport_event_payload(payload_hex: &str) -> Option<RuntimeTransportEvent> {
    let bytes = hex::decode(payload_hex).ok()?;
    let text = std::str::from_utf8(&bytes).ok()?.trim();
    if text.is_empty() {
        return None;
    }

    if let Ok(value) = serde_json::from_str::<RuntimeTransportEvent>(text) {
        return Some(value);
    }

    if let Ok(value) = serde_json::from_str::<serde_json::Value>(text) {
        let obj = value.as_object()?;
        let kind_raw = obj
            .get("event")
            .or_else(|| obj.get("event_name"))
            .or_else(|| obj.get("eventName"))
            .or_else(|| obj.get("kind"))
            .or_else(|| obj.get("type"))
            .and_then(|v| v.as_str())?;
        let kind = normalize_transport_event_kind(kind_raw)?;
        let id = match kind {
            "peer_disconnected" | "peer_reconnected" => obj
                .get("peer_pubkey")
                .or_else(|| obj.get("node_id"))
                .or_else(|| obj.get("id"))
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(ToString::to_string)?,
            "channel_closed" | "channel_usable" | "channel_unusable" => obj
                .get("channel_id")
                .or_else(|| obj.get("channelId"))
                .or_else(|| obj.get("id"))
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(ToString::to_string)?,
            _ => return None,
        };
        return Some(match kind {
            "peer_disconnected" => RuntimeTransportEvent::PeerDisconnected { peer_pubkey: id },
            "peer_reconnected" => RuntimeTransportEvent::PeerReconnected { peer_pubkey: id },
            "channel_closed" => RuntimeTransportEvent::ChannelClosed { channel_id: id },
            "channel_usable" => RuntimeTransportEvent::ChannelUsable { channel_id: id },
            "channel_unusable" => RuntimeTransportEvent::ChannelUnusable { channel_id: id },
            _ => return None,
        });
    }

    let (kind_raw, id_raw) = text.split_once(':')?;
    let kind = normalize_transport_event_kind(kind_raw.trim())?;
    let id = id_raw.trim().to_string();
    if id.is_empty() {
        return None;
    }
    match kind {
        "peer_disconnected" => Some(RuntimeTransportEvent::PeerDisconnected { peer_pubkey: id }),
        "peer_reconnected" => Some(RuntimeTransportEvent::PeerReconnected { peer_pubkey: id }),
        "channel_closed" => Some(RuntimeTransportEvent::ChannelClosed { channel_id: id }),
        "channel_usable" => Some(RuntimeTransportEvent::ChannelUsable { channel_id: id }),
        "channel_unusable" => Some(RuntimeTransportEvent::ChannelUnusable { channel_id: id }),
        _ => None,
    }
}

fn normalize_transport_event_kind(raw: &str) -> Option<&'static str> {
    let compact = raw
        .trim()
        .to_ascii_lowercase()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect::<String>();
    match compact.as_str() {
        "peerdisconnected" | "peeroffline" | "peerdown" => Some("peer_disconnected"),
        "peerreconnected" | "peerconnected" | "peeronline" | "peerup" => Some("peer_reconnected"),
        "channelclosed" => Some("channel_closed"),
        "channelusable" | "channelopened" | "channelready" | "channelonline" | "channelup" => {
            Some("channel_usable")
        }
        "channelunusable" | "channeldisconnected" | "channeloffline" | "channeldown" => {
            Some("channel_unusable")
        }
        _ => None,
    }
}

fn encode_transport_event_payload(event: &RuntimeTransportEvent) -> String {
    let payload = match event {
        RuntimeTransportEvent::PeerDisconnected { peer_pubkey } => {
            format!("peer_disconnected:{peer_pubkey}")
        }
        RuntimeTransportEvent::PeerReconnected { peer_pubkey } => {
            format!("peer_reconnected:{peer_pubkey}")
        }
        RuntimeTransportEvent::ChannelClosed { channel_id } => {
            format!("channel_closed:{channel_id}")
        }
        RuntimeTransportEvent::ChannelUsable { channel_id } => {
            format!("channel_usable:{channel_id}")
        }
        RuntimeTransportEvent::ChannelUnusable { channel_id } => {
            format!("channel_unusable:{channel_id}")
        }
    };
    hex::encode(payload.as_bytes())
}

fn encode_payment_status_event_payload(payment_hash: &str, status: &str) -> String {
    hex::encode(format!("payment_status:{payment_hash}:{status}").as_bytes())
}

fn validate_peer_addr_format(peer_addr: &str) -> Result<(), JsValue> {
    let trimmed = peer_addr.trim();
    let Some((host, port)) = trimmed.rsplit_once(':') else {
        return Err(JsValue::from_str(sdk_contracts::ERR_PEER_ADDR_HOST_PORT));
    };
    if host.trim().is_empty() || port.trim().is_empty() {
        return Err(JsValue::from_str(sdk_contracts::ERR_PEER_ADDR_HOST_PORT));
    }
    if !port.chars().all(|c| c.is_ascii_digit()) {
        return Err(JsValue::from_str(sdk_contracts::ERR_PEER_ADDR_PORT_NUMERIC));
    }
    let port_num = port
        .parse::<u16>()
        .map_err(|_| JsValue::from_str(sdk_contracts::ERR_PEER_ADDR_PORT_RANGE))?;
    let _ = port_num;
    Ok(())
}

fn validate_asset_id_format(asset_id: &str) -> Result<(), JsValue> {
    let trimmed = asset_id.trim();
    let is_hex64 = trimmed.len() == 64 && trimmed.chars().all(|c| c.is_ascii_hexdigit());
    let is_rgb_canonical = trimmed
        .strip_prefix("rgb:")
        .map(|rest| {
            !rest.is_empty()
                && rest
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '~'))
        })
        .unwrap_or(false);
    if !is_hex64 && !is_rgb_canonical {
        return Err(JsValue::from_str(sdk_contracts::ERR_ASSET_ID_INVALID));
    }
    Ok(())
}

fn decode_fixed_hex<const N: usize>(value: &str, error: &str) -> Result<[u8; N], JsValue> {
    let bytes = hex::decode(value).map_err(|_| JsValue::from_str(error))?;
    if bytes.len() != N {
        return Err(JsValue::from_str(error));
    }
    let mut out = [0u8; N];
    out.copy_from_slice(&bytes);
    Ok(out)
}

#[cfg(all(test, target_arch = "wasm32"))]
#[path = "tests/ln_node_tests.rs"]
mod tests;

fn parse_payment_status_event_payload(payload_hex: &str) -> Option<PaymentStatusEvent> {
    let bytes = hex::decode(payload_hex).ok()?;
    let text = std::str::from_utf8(&bytes).ok()?.trim();
    if text.is_empty() {
        return None;
    }

    if let Ok(value) = serde_json::from_str::<serde_json::Value>(text) {
        return parse_payment_status_event_json(&value);
    }

    let mut parts = text.split(':');
    match parts.next()?.trim() {
        "payment_status" => {
            let payment_hash = parts.next()?;
            let status = parts.next()?;
            if parts.next().is_some() {
                return None;
            }
            Some(PaymentStatusEvent {
                payment_hash: payment_hash.to_string(),
                status: status.to_string(),
            })
        }
        _ => None,
    }
}

fn parse_payment_status_event_json(value: &serde_json::Value) -> Option<PaymentStatusEvent> {
    let payment_hash = value
        .get("payment_hash")
        .and_then(|v| v.as_str())?
        .trim()
        .to_string();
    if payment_hash.is_empty() {
        return None;
    }

    if let Some(kind) = value.get("kind").and_then(|v| v.as_str()) {
        if kind.trim() != "payment_status" {
            return None;
        }
    }

    let status = value.get("status").and_then(|v| v.as_str())?.trim();
    if status.is_empty() {
        return None;
    }
    Some(PaymentStatusEvent {
        payment_hash,
        status: status.to_string(),
    })
}
