use std::cell::RefCell;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::rc::{Rc, Weak};

use bitcoin_hashes::sha256::Hash as Sha256;
use bitcoin_hashes::Hash as _;
use serde::{Deserialize, Serialize};
use wasm_bindgen::prelude::JsValue;

use crate::ldk_live_backend::{create_wasm_ldk_live_backend, LdkLiveBackend};
use crate::runtime_store::{browser_persistent_state_store, RuntimeStateStore};
use crate::wasm_node_persistence::WASM_LDK_RUNTIME_STORAGE_PREFIX;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum LdkRuntimeLifecycleState {
    Cold,
    Starting,
    Running,
    RunningRestored,
    Restoring,
    Stopping,
    Stopped,
    RestoredStopped,
}

impl LdkRuntimeLifecycleState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Cold => "cold",
            Self::Starting => "starting",
            Self::Running => "running",
            Self::RunningRestored => "running_restored",
            Self::Restoring => "restoring",
            Self::Stopping => "stopping",
            Self::Stopped => "stopped",
            Self::RestoredStopped => "restored_stopped",
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct LdkRuntimeStatusData {
    pub backend: String,
    pub lifecycle_state: String,
    pub ready: bool,
    pub identity_stable: bool,
    pub channel_manager_restored: bool,
    pub monitors_restored: bool,
    pub storage_initialized: bool,
    pub schema_version: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
struct LdkRuntimeLifecycleCounters {
    invoices_created: u64,
    payments_initiated: u64,
    keysends_initiated: u64,
    channels_opened: u64,
    channels_closed: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct LdkRuntimeComponentsStatusData {
    pub backend: String,
    pub started: bool,
    pub fee_estimator_ready: bool,
    pub broadcaster_ready: bool,
    pub logger_ready: bool,
    pub persister_ready: bool,
    pub key_manager_ready: bool,
    pub payment_engine_ready: bool,
    pub channel_engine_ready: bool,
    pub key_manager_fingerprint: String,
    pub invoices_created: u64,
    pub payments_initiated: u64,
    pub keysends_initiated: u64,
    pub channels_opened: u64,
    pub channels_closed: u64,
}

#[derive(Clone, Copy, Debug, Default)]
struct RuntimeSessionAuthorityState {
    initialized: bool,
    authorized: bool,
}

thread_local! {
    static RUNTIME_SESSION_AUTHORITY_STATE: RefCell<RuntimeSessionAuthorityState> =
        RefCell::new(RuntimeSessionAuthorityState::default());
}

pub fn set_runtime_session_initialized(initialized: bool) {
    RUNTIME_SESSION_AUTHORITY_STATE.with(|state| {
        let mut state = state.borrow_mut();
        state.initialized = initialized;
        if !initialized {
            state.authorized = false;
        }
    });
}

pub fn set_runtime_session_authorized(authorized: bool) {
    RUNTIME_SESSION_AUTHORITY_STATE.with(|state| {
        let mut state = state.borrow_mut();
        state.authorized = authorized;
    });
}

fn ensure_runtime_session_authorized() -> Result<(), JsValue> {
    RUNTIME_SESSION_AUTHORITY_STATE.with(|state| {
        let state = state.borrow();
        if state.initialized && !state.authorized {
            return Err(JsValue::from_str(
                "runtime session is locked; call unlock first",
            ));
        }
        Ok(())
    })
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

fn reconcile_virtual_channel_state(
    channels: &HashMap<String, LdkRuntimeChannelStateData>,
    drafts: &mut HashMap<String, LdkRuntimeVirtualChannelDraftData>,
    sessions: &mut HashMap<String, LdkRuntimeVirtualChannelSessionData>,
) {
    drafts.retain(|_, draft| {
        !sessions
            .values()
            .any(|session| session.peer_pubkey == draft.peer_pubkey)
    });

    let mut needs_touch = false;
    for session in sessions.values_mut() {
        let live_channel = channels.get(&session.channel_id);
        match live_channel {
            Some(channel) => {
                if channel.virtual_open_mode.is_none()
                    && session.status != LdkRuntimeVirtualChannelSessionStatusData::Abandoned
                {
                    session.status = LdkRuntimeVirtualChannelSessionStatusData::Abandoned;
                    session.updated_at = unix_now_secs();
                    needs_touch = true;
                }
            }
            None => {
                if session.status != LdkRuntimeVirtualChannelSessionStatusData::Abandoned {
                    session.status = LdkRuntimeVirtualChannelSessionStatusData::Abandoned;
                    session.updated_at = unix_now_secs();
                    needs_touch = true;
                }
            }
        }
    }
    if needs_touch {
        // updates already applied in-place
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LdkRuntimePeerStateData {
    pub pubkey: String,
    pub peer_addr: String,
    pub started: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LdkRuntimeChannelStateData {
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
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LdkRuntimeOpenChannelRequestData {
    pub peer_pubkey: String,
    pub capacity_sat: u64,
    pub public: bool,
    pub asset_id: Option<String>,
    pub asset_local_amount: Option<u64>,
    /// baid58-encoded RGB `ContractId`. Required when `asset_id` is `Some`.
    #[serde(default)]
    pub contract_id: Option<String>,
    /// RGB transport endpoint string (e.g. `rpc://…`). Required when `asset_id` is `Some`.
    #[serde(default)]
    pub consignment_endpoint: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LdkRuntimeOpenChannelResultData {
    pub temporary_channel_id: String,
    pub channel_id: String,
    #[serde(default)]
    pub peer_pubkey: String,
    #[serde(default)]
    pub capacity_sat: u64,
    pub status: String,
    pub ready: bool,
    pub is_usable: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LdkRuntimeVirtualChannelSessionStatusData {
    Active,
    AbandonPending,
    Abandoned,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LdkRuntimeVirtualChannelDraftData {
    pub temporary_channel_id: String,
    pub peer_pubkey: String,
    pub created_at: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LdkRuntimeVirtualChannelSessionData {
    pub channel_id: String,
    pub former_temporary_channel_id: String,
    pub peer_pubkey: String,
    pub status: LdkRuntimeVirtualChannelSessionStatusData,
    pub created_at: u64,
    pub updated_at: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LdkRuntimePaymentStateData {
    pub amt_msat: Option<u64>,
    pub asset_amount: Option<u64>,
    pub asset_id: Option<String>,
    pub payment_hash: String,
    pub inbound: bool,
    pub status: String,
    #[serde(default)]
    pub invoice_type: Option<String>,
    #[serde(default)]
    pub preimage: Option<String>,
    pub created_at: u64,
    pub updated_at: u64,
    pub payee_pubkey: String,
}

/// A real (live-LDK) payment as tracked by the wasm `ChannelManager`.
///
/// Unlike [`LdkRuntimePaymentStateData`], whose status is driven by the event-stream parity model,
/// the `status` here is updated strictly from real LDK `PaymentSent`/`PaymentFailed`/`PaymentClaimed`
/// events, so it reflects an actual settled HTLC over the wire.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LdkRuntimeLivePaymentData {
    pub payment_hash: String,
    /// `pending` | `succeeded` | `failed`
    pub status: String,
    pub amt_msat: Option<u64>,
    pub asset_id: Option<String>,
    pub asset_amount: Option<u64>,
    pub inbound: bool,
    #[serde(default)]
    pub preimage: Option<String>,
    pub created_at: u64,
    pub updated_at: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LdkRuntimeFundingRequestData {
    pub temporary_channel_id: String,
    pub counterparty_node_id: String,
    pub channel_value_satoshis: u64,
    pub output_script_hex: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LdkRuntimeFundingTxSubmissionData {
    pub temporary_channel_id: String,
    pub counterparty_node_id: String,
    pub funding_tx_hex: String,
}

pub trait LdkRuntimeManager {
    fn status(&self) -> LdkRuntimeStatusData;
    fn start(&self) -> Result<(), JsValue>;
    fn restore(&self) -> Result<(), JsValue>;
    fn ensure_started(&self) -> Result<(), JsValue>;
    fn stop(&self) -> Result<(), JsValue>;
    fn has_peer(&self, peer_pubkey: &str) -> bool;
    fn has_connected_peer(&self, peer_pubkey: &str) -> bool;
    fn has_any_connected_peer(&self) -> bool;
    fn get_peer(&self, peer_pubkey: &str) -> Option<LdkRuntimePeerStateData>;
    fn upsert_peer(&self, peer: LdkRuntimePeerStateData);
    fn set_peer_started(&self, peer_pubkey: &str, started: bool) -> bool;
    fn remove_peer(&self, peer_pubkey: &str) -> bool;
    fn list_peers(&self) -> Vec<LdkRuntimePeerStateData>;
    fn peer_new_outbound_connection(&self, peer_pubkey: &str) -> Result<String, JsValue>;
    fn peer_read_event(&self, payload_hex: &str) -> Result<(), JsValue>;
    fn peer_read_event_for_peer(
        &self,
        _peer_pubkey: &str,
        payload_hex: &str,
    ) -> Result<(), JsValue> {
        self.peer_read_event(payload_hex)
    }
    fn peer_process_events(&self) -> Result<(), JsValue>;
    fn peer_take_outbound_frames(&self) -> Result<Vec<String>, JsValue>;
    fn peer_take_outbound_frames_for_peer(
        &self,
        _peer_pubkey: &str,
    ) -> Result<Vec<String>, JsValue> {
        self.peer_take_outbound_frames()
    }
    fn peer_socket_disconnected(&self) -> Result<(), JsValue>;
    fn peer_socket_disconnected_for_peer(&self, _peer_pubkey: &str) -> Result<(), JsValue> {
        self.peer_socket_disconnected()
    }
    fn peer_is_handshake_complete(&self, peer_pubkey: &str) -> Result<bool, JsValue>;
    fn set_live_node_seed_hex(&self, seed_hex: String) -> Result<(), JsValue>;
    fn set_identity_stable(&self, stable: bool);
    fn live_node_pubkey(&self) -> Result<String, JsValue>;
    fn persist_live_state(&self) -> Result<(), JsValue>;
    fn close_live_channel(
        &self,
        channel_id: &str,
        peer_pubkey: &str,
        force: bool,
    ) -> Result<(), JsValue>;
    fn upsert_channel(&self, channel: LdkRuntimeChannelStateData);
    fn remove_channel(&self, channel_id: &str) -> bool;
    fn remove_channels_by_peer(&self, peer_pubkey: &str) -> usize;
    fn set_channel_usable(&self, channel_id: &str, is_usable: bool) -> bool;
    fn find_channel_by_temporary(&self, temporary_channel_id: &str) -> Option<String>;
    fn list_channels(&self) -> Vec<LdkRuntimeChannelStateData>;
    fn open_channel_non_virtual(
        &self,
        request: LdkRuntimeOpenChannelRequestData,
    ) -> Result<LdkRuntimeOpenChannelResultData, JsValue>;
    fn list_pending_funding_requests(&self) -> Result<Vec<LdkRuntimeFundingRequestData>, JsValue>;
    fn submit_funding_transaction(
        &self,
        request: LdkRuntimeFundingTxSubmissionData,
    ) -> Result<(), JsValue>;
    fn reconcile_channels_from_live(&self) -> Result<usize, JsValue>;
    fn upsert_payment(&self, payment: LdkRuntimePaymentStateData);
    fn get_payment(&self, payment_hash: &str) -> Option<LdkRuntimePaymentStateData>;
    fn list_payments(&self) -> Vec<LdkRuntimePaymentStateData>;
    /// Send a real keysend (spontaneous) payment over a live channel via the wasm `ChannelManager`.
    /// When `contract_id`/`asset_amount` are set, an RGB amount rides the HTLC.
    fn keysend_live(
        &self,
        dest_pubkey: &str,
        amt_msat: u64,
        contract_id: Option<String>,
        asset_amount: Option<u64>,
    ) -> Result<LdkRuntimeLivePaymentData, JsValue>;
    fn send_bolt11_live(
        &self,
        invoice: &str,
        amt_msat: Option<u64>,
        contract_id: Option<String>,
        asset_amount: Option<u64>,
    ) -> Result<LdkRuntimeLivePaymentData, JsValue>;
    fn create_bolt11_invoice_live(
        &self,
        amt_msat: Option<u64>,
        expiry_sec: u32,
        contract_id: Option<String>,
        asset_amount: Option<u64>,
    ) -> Result<String, JsValue>;
    fn create_hodl_bolt11_invoice_live(
        &self,
        amt_msat: Option<u64>,
        expiry_sec: u32,
        contract_id: Option<String>,
        asset_amount: Option<u64>,
        payment_hash: &str,
    ) -> Result<String, JsValue>;
    fn claim_hodl_invoice_live(
        &self,
        payment_hash: &str,
        payment_preimage: &str,
    ) -> Result<bool, JsValue>;
    fn cancel_hodl_invoice_live(&self, payment_hash: &str) -> Result<(), JsValue>;
    /// Snapshot of all real payments tracked by the live `ChannelManager` event stream.
    fn live_payments(&self) -> Vec<LdkRuntimeLivePaymentData>;
    /// Status of a single real payment by hex payment hash, if known.
    fn live_payment(&self, payment_hash: &str) -> Option<LdkRuntimeLivePaymentData>;
    fn virtual_channel_add_intent(
        &self,
        peer_pubkey: &str,
        temporary_channel_id: Option<String>,
    ) -> Result<String, String>;
    fn virtual_channel_draft_delete(&self, temporary_channel_id: &str) -> bool;
    fn virtual_channel_draft_get(
        &self,
        temporary_channel_id: &str,
    ) -> Option<LdkRuntimeVirtualChannelDraftData>;
    fn virtual_channel_draft_store(&self) -> Vec<LdkRuntimeVirtualChannelDraftData>;
    fn virtual_channel_session_add_from_open(
        &self,
        channel_id: &str,
        temporary_channel_id: &str,
        peer_pubkey: &str,
    );
    fn virtual_channel_session_get(
        &self,
        channel_id: &str,
    ) -> Option<LdkRuntimeVirtualChannelSessionData>;
    fn virtual_channel_session_get_by_peer(
        &self,
        peer_pubkey: &str,
    ) -> Option<LdkRuntimeVirtualChannelSessionData>;
    fn virtual_channel_session_update_status(
        &self,
        channel_id: &str,
        status: LdkRuntimeVirtualChannelSessionStatusData,
    ) -> bool;
    fn virtual_channel_session_store(&self) -> Vec<LdkRuntimeVirtualChannelSessionData>;
    fn virtual_channel_reconcile_sessions(&self);
    fn component_status(&self) -> LdkRuntimeComponentsStatusData;
    fn record_invoice_created(&self);
    fn record_payment_initiated(&self);
    fn record_keysend_initiated(&self);
    fn record_channel_opened(&self);
    fn record_channel_closed(&self);

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
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct LdkRuntimeSnapshot {
    lifecycle_state: LdkRuntimeLifecycleState,
    storage_initialized: bool,
    schema_version: u32,
    #[serde(default)]
    transport_session_initialized: bool,
    #[serde(default)]
    peers: Vec<LdkRuntimePeerStateData>,
    #[serde(default)]
    channels: Vec<LdkRuntimeChannelStateData>,
    #[serde(default)]
    payments: Vec<LdkRuntimePaymentStateData>,
    #[serde(default)]
    virtual_channel_drafts: Vec<LdkRuntimeVirtualChannelDraftData>,
    #[serde(default)]
    virtual_channel_sessions: Vec<LdkRuntimeVirtualChannelSessionData>,
    #[serde(default)]
    counters: LdkRuntimeLifecycleCounters,
    #[serde(default)]
    key_manager_fingerprint: String,
}

trait LdkRuntimeStorage {
    fn load(&self, key: &str) -> Result<Option<LdkRuntimeSnapshot>, JsValue>;
    fn save(&self, key: &str, snapshot: &LdkRuntimeSnapshot) -> Result<(), JsValue>;
}

struct InMemoryLdkRuntimeStorage;
struct BrowserBackedLdkRuntimeStorage;

const LDK_RUNTIME_CHECKPOINT_SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Debug, Serialize, Deserialize)]
struct LdkRuntimeCheckpointEnvelope {
    schema_version: u32,
    snapshot: LdkRuntimeSnapshot,
}

thread_local! {
    static FALLBACK_RUNTIME_STORAGE: RefCell<HashMap<String, LdkRuntimeSnapshot>> =
        RefCell::new(HashMap::new());
    static RUNTIME_MANAGER_REGISTRY: RefCell<HashMap<String, Weak<dyn LdkRuntimeManager>>> =
        RefCell::new(HashMap::new());
}

impl LdkRuntimeStorage for InMemoryLdkRuntimeStorage {
    fn load(&self, key: &str) -> Result<Option<LdkRuntimeSnapshot>, JsValue> {
        Ok(FALLBACK_RUNTIME_STORAGE.with(|storage| storage.borrow().get(key).cloned()))
    }

    fn save(&self, key: &str, snapshot: &LdkRuntimeSnapshot) -> Result<(), JsValue> {
        FALLBACK_RUNTIME_STORAGE.with(|storage| {
            storage
                .borrow_mut()
                .insert(key.to_string(), snapshot.clone());
        });
        Ok(())
    }
}

impl BrowserBackedLdkRuntimeStorage {
    fn scoped_key(key: &str) -> String {
        format!("{WASM_LDK_RUNTIME_STORAGE_PREFIX}{key}")
    }

    fn pending_scoped_key(key: &str) -> String {
        format!("{WASM_LDK_RUNTIME_STORAGE_PREFIX}{key}:pending")
    }

    fn encode_checkpoint(snapshot: &LdkRuntimeSnapshot) -> Result<String, JsValue> {
        let envelope = LdkRuntimeCheckpointEnvelope {
            schema_version: LDK_RUNTIME_CHECKPOINT_SCHEMA_VERSION,
            snapshot: snapshot.clone(),
        };
        serde_json::to_string(&envelope).map_err(|e| JsValue::from_str(&e.to_string()))
    }

    fn decode_snapshot(raw: &str) -> Option<LdkRuntimeSnapshot> {
        if let Ok(envelope) = serde_json::from_str::<LdkRuntimeCheckpointEnvelope>(raw) {
            if envelope.schema_version <= LDK_RUNTIME_CHECKPOINT_SCHEMA_VERSION {
                return Some(envelope.snapshot);
            }
        }
        None
    }

    fn select_snapshot_for_restore(
        committed_raw: Option<String>,
        pending_raw: Option<String>,
    ) -> Option<LdkRuntimeSnapshot> {
        if let Some(raw) = committed_raw.as_deref() {
            if let Some(snapshot) = Self::decode_snapshot(raw) {
                return Some(snapshot);
            }
        }
        if let Some(raw) = pending_raw.as_deref() {
            if let Some(snapshot) = Self::decode_snapshot(raw) {
                return Some(snapshot);
            }
        }
        None
    }
}

impl LdkRuntimeStorage for BrowserBackedLdkRuntimeStorage {
    fn load(&self, key: &str) -> Result<Option<LdkRuntimeSnapshot>, JsValue> {
        let scoped_key = Self::scoped_key(key);
        let pending_key = Self::pending_scoped_key(key);
        let store = browser_persistent_state_store();
        let committed_raw = store.get(&scoped_key)?;
        let pending_raw = store.get(&pending_key)?;
        if let Some(snapshot) = Self::select_snapshot_for_restore(committed_raw, pending_raw) {
            return Ok(Some(snapshot));
        }
        InMemoryLdkRuntimeStorage.load(key)
    }

    fn save(&self, key: &str, snapshot: &LdkRuntimeSnapshot) -> Result<(), JsValue> {
        let scoped_key = Self::scoped_key(key);
        let pending_key = Self::pending_scoped_key(key);
        let raw = Self::encode_checkpoint(snapshot)?;
        let store = browser_persistent_state_store();
        // Crash-safe checkpoint protocol:
        // 1) stage pending payload
        // 2) commit primary payload
        // 3) clear pending marker
        let _ = store.set(&pending_key, &raw);
        let _ = store.set(&scoped_key, &raw);
        let _ = store.delete(&pending_key);
        InMemoryLdkRuntimeStorage.save(key, snapshot)
    }
}

struct WasmNativeRuntimeManager {
    runtime_key: String,
    backend_label: String,
    storage: Rc<dyn LdkRuntimeStorage>,
    started: RefCell<bool>,
    restored: RefCell<bool>,
    lifecycle_state: RefCell<LdkRuntimeLifecycleState>,
    storage_initialized: RefCell<bool>,
    transport_session_initialized: RefCell<bool>,
    peers: RefCell<HashMap<String, LdkRuntimePeerStateData>>,
    channels: RefCell<HashMap<String, LdkRuntimeChannelStateData>>,
    payments: RefCell<HashMap<String, LdkRuntimePaymentStateData>>,
    virtual_channel_drafts: RefCell<HashMap<String, LdkRuntimeVirtualChannelDraftData>>,
    virtual_channel_sessions: RefCell<HashMap<String, LdkRuntimeVirtualChannelSessionData>>,
    counters: RefCell<LdkRuntimeLifecycleCounters>,
    key_manager_fingerprint: String,
    live_backend: RefCell<Option<Rc<dyn LdkLiveBackend>>>,
    live_node_seed: RefCell<Option<[u8; 32]>>,
    identity_stable: RefCell<bool>,
}

impl WasmNativeRuntimeManager {
    fn new(runtime_key: String, backend_label: String, storage: Rc<dyn LdkRuntimeStorage>) -> Self {
        let key_manager_fingerprint = runtime_key_fingerprint(&runtime_key);
        Self {
            runtime_key,
            backend_label,
            storage,
            started: RefCell::new(false),
            restored: RefCell::new(false),
            lifecycle_state: RefCell::new(LdkRuntimeLifecycleState::Cold),
            storage_initialized: RefCell::new(false),
            transport_session_initialized: RefCell::new(false),
            peers: RefCell::new(HashMap::new()),
            channels: RefCell::new(HashMap::new()),
            payments: RefCell::new(HashMap::new()),
            virtual_channel_drafts: RefCell::new(HashMap::new()),
            virtual_channel_sessions: RefCell::new(HashMap::new()),
            counters: RefCell::new(LdkRuntimeLifecycleCounters::default()),
            key_manager_fingerprint,
            live_backend: RefCell::new(None),
            live_node_seed: RefCell::new(None),
            identity_stable: RefCell::new(false),
        }
    }

    fn snapshot(&self) -> LdkRuntimeSnapshot {
        LdkRuntimeSnapshot {
            lifecycle_state: *self.lifecycle_state.borrow(),
            storage_initialized: *self.storage_initialized.borrow(),
            schema_version: 1,
            transport_session_initialized: *self.transport_session_initialized.borrow(),
            peers: self.peers.borrow().values().cloned().collect(),
            channels: self.channels.borrow().values().cloned().collect(),
            payments: self.payments.borrow().values().cloned().collect(),
            virtual_channel_drafts: self
                .virtual_channel_drafts
                .borrow()
                .values()
                .cloned()
                .collect(),
            virtual_channel_sessions: self
                .virtual_channel_sessions
                .borrow()
                .values()
                .cloned()
                .collect(),
            counters: self.counters.borrow().clone(),
            key_manager_fingerprint: self.key_manager_fingerprint.clone(),
        }
    }

    fn persist_state(&self) {
        let _ = self.storage.save(&self.runtime_key, &self.snapshot());
    }

    fn ensure_live_backend(&self) -> Result<Rc<dyn LdkLiveBackend>, JsValue> {
        if self.live_backend.borrow().is_none() {
            let backend = create_wasm_ldk_live_backend(
                self.runtime_key.clone(),
                *self.live_node_seed.borrow(),
            )?;
            self.live_backend.borrow_mut().replace(backend);
        }
        self.live_backend
            .borrow()
            .as_ref()
            .cloned()
            .ok_or_else(|| JsValue::from_str("failed to initialize ldk live backend"))
    }
}

impl LdkRuntimeManager for WasmNativeRuntimeManager {
    fn status(&self) -> LdkRuntimeStatusData {
        let storage_initialized = *self.storage_initialized.borrow();
        let transport_session_initialized = *self.transport_session_initialized.borrow();
        let lifecycle_state = *self.lifecycle_state.borrow();
        let restore_status = self
            .live_backend
            .borrow()
            .as_ref()
            .map(|backend| backend.restore_status())
            .unwrap_or_default();
        LdkRuntimeStatusData {
            backend: self.backend_label.clone(),
            lifecycle_state: lifecycle_state.as_str().to_string(),
            ready: *self.started.borrow() && transport_session_initialized,
            identity_stable: *self.identity_stable.borrow(),
            channel_manager_restored: restore_status.channel_manager_restored,
            monitors_restored: restore_status.monitors_restored,
            storage_initialized,
            schema_version: 1,
        }
    }

    fn start(&self) -> Result<(), JsValue> {
        *self.storage_initialized.borrow_mut() = true;
        *self.lifecycle_state.borrow_mut() = LdkRuntimeLifecycleState::Starting;
        *self.transport_session_initialized.borrow_mut() = true;
        *self.started.borrow_mut() = true;
        *self.lifecycle_state.borrow_mut() = if *self.restored.borrow() {
            LdkRuntimeLifecycleState::RunningRestored
        } else {
            LdkRuntimeLifecycleState::Running
        };
        self.storage.save(&self.runtime_key, &self.snapshot())?;
        Ok(())
    }

    fn restore(&self) -> Result<(), JsValue> {
        *self.lifecycle_state.borrow_mut() = LdkRuntimeLifecycleState::Restoring;
        if let Some(snapshot) = self.storage.load(&self.runtime_key)? {
            *self.storage_initialized.borrow_mut() = snapshot.storage_initialized;
            *self.transport_session_initialized.borrow_mut() =
                snapshot.transport_session_initialized;
            *self.restored.borrow_mut() = true;
            self.peers.borrow_mut().clear();
            self.channels.borrow_mut().clear();
            self.payments.borrow_mut().clear();
            self.virtual_channel_drafts.borrow_mut().clear();
            self.virtual_channel_sessions.borrow_mut().clear();
            for mut peer in snapshot.peers {
                peer.started = false;
                self.peers.borrow_mut().insert(peer.pubkey.clone(), peer);
            }
            for channel in snapshot.channels {
                self.channels
                    .borrow_mut()
                    .insert(channel.channel_id.clone(), channel);
            }
            for payment in snapshot.payments {
                self.payments
                    .borrow_mut()
                    .insert(payment.payment_hash.clone(), payment);
            }
            for draft in snapshot.virtual_channel_drafts {
                self.virtual_channel_drafts
                    .borrow_mut()
                    .insert(draft.temporary_channel_id.clone(), draft);
            }
            for session in snapshot.virtual_channel_sessions {
                self.virtual_channel_sessions
                    .borrow_mut()
                    .insert(session.channel_id.clone(), session);
            }
            *self.counters.borrow_mut() = snapshot.counters;
            {
                let channels = self.channels.borrow();
                let mut drafts = self.virtual_channel_drafts.borrow_mut();
                let mut sessions = self.virtual_channel_sessions.borrow_mut();
                reconcile_virtual_channel_state(&channels, &mut drafts, &mut sessions);
            }
            *self.lifecycle_state.borrow_mut() = LdkRuntimeLifecycleState::RestoredStopped;
        } else {
            *self.storage_initialized.borrow_mut() = true;
            *self.transport_session_initialized.borrow_mut() = false;
            *self.restored.borrow_mut() = false;
            *self.counters.borrow_mut() = LdkRuntimeLifecycleCounters::default();
            *self.lifecycle_state.borrow_mut() = LdkRuntimeLifecycleState::Cold;
        }
        Ok(())
    }

    fn ensure_started(&self) -> Result<(), JsValue> {
        ensure_runtime_session_authorized()?;
        if !*self.started.borrow() {
            self.restore()?;
            self.start()?;
        }
        Ok(())
    }

    fn stop(&self) -> Result<(), JsValue> {
        *self.lifecycle_state.borrow_mut() = LdkRuntimeLifecycleState::Stopping;
        *self.started.borrow_mut() = false;
        *self.transport_session_initialized.borrow_mut() = false;
        *self.lifecycle_state.borrow_mut() = LdkRuntimeLifecycleState::Stopped;
        self.storage.save(&self.runtime_key, &self.snapshot())?;
        Ok(())
    }

    fn has_peer(&self, peer_pubkey: &str) -> bool {
        self.peers.borrow().contains_key(peer_pubkey)
    }

    fn has_connected_peer(&self, peer_pubkey: &str) -> bool {
        if let Some(backend) = self.live_backend.borrow().as_ref() {
            if let Ok(connected) = backend.is_peer_handshake_complete(peer_pubkey) {
                return connected;
            }
        }
        self.peers
            .borrow()
            .get(peer_pubkey)
            .map(|peer| peer.started)
            .unwrap_or(false)
    }

    fn has_any_connected_peer(&self) -> bool {
        if let Some(backend) = self.live_backend.borrow().as_ref() {
            for peer in self.peers.borrow().keys() {
                if backend.is_peer_handshake_complete(peer).unwrap_or(false) {
                    return true;
                }
            }
        }
        self.peers.borrow().values().any(|peer| peer.started)
    }

    fn get_peer(&self, peer_pubkey: &str) -> Option<LdkRuntimePeerStateData> {
        self.peers.borrow().get(peer_pubkey).cloned()
    }

    fn upsert_peer(&self, peer: LdkRuntimePeerStateData) {
        self.peers.borrow_mut().insert(peer.pubkey.clone(), peer);
        self.persist_state();
    }

    fn set_peer_started(&self, peer_pubkey: &str, started: bool) -> bool {
        let mut peers = self.peers.borrow_mut();
        let Some(peer) = peers.get_mut(peer_pubkey) else {
            return false;
        };
        peer.started = started;
        drop(peers);
        self.persist_state();
        true
    }

    fn remove_peer(&self, peer_pubkey: &str) -> bool {
        let removed = self.peers.borrow_mut().remove(peer_pubkey).is_some();
        if removed {
            self.persist_state();
        }
        removed
    }

    fn list_peers(&self) -> Vec<LdkRuntimePeerStateData> {
        self.peers.borrow().values().cloned().collect()
    }

    fn peer_new_outbound_connection(&self, peer_pubkey: &str) -> Result<String, JsValue> {
        if self.live_backend.borrow().is_none() {
            let backend = create_wasm_ldk_live_backend(
                self.runtime_key.clone(),
                *self.live_node_seed.borrow(),
            )?;
            self.live_backend.borrow_mut().replace(backend);
        }
        let backend = self
            .live_backend
            .borrow()
            .as_ref()
            .cloned()
            .ok_or_else(|| JsValue::from_str("failed to initialize ldk live backend"))?;
        backend.new_outbound_connection(peer_pubkey)
    }

    fn peer_read_event(&self, payload_hex: &str) -> Result<(), JsValue> {
        if self.live_backend.borrow().is_none() {
            let backend = create_wasm_ldk_live_backend(
                self.runtime_key.clone(),
                *self.live_node_seed.borrow(),
            )?;
            self.live_backend.borrow_mut().replace(backend);
        }
        let backend = self
            .live_backend
            .borrow()
            .as_ref()
            .cloned()
            .ok_or_else(|| JsValue::from_str("failed to initialize ldk live backend"))?;
        backend.read_event(payload_hex)
    }

    fn peer_read_event_for_peer(
        &self,
        peer_pubkey: &str,
        payload_hex: &str,
    ) -> Result<(), JsValue> {
        if self.live_backend.borrow().is_none() {
            let backend = create_wasm_ldk_live_backend(
                self.runtime_key.clone(),
                *self.live_node_seed.borrow(),
            )?;
            self.live_backend.borrow_mut().replace(backend);
        }
        let backend = self
            .live_backend
            .borrow()
            .as_ref()
            .cloned()
            .ok_or_else(|| JsValue::from_str("failed to initialize ldk live backend"))?;
        backend.read_event_for_peer(peer_pubkey, payload_hex)
    }

    fn peer_process_events(&self) -> Result<(), JsValue> {
        if self.live_backend.borrow().is_none() {
            let backend = create_wasm_ldk_live_backend(
                self.runtime_key.clone(),
                *self.live_node_seed.borrow(),
            )?;
            self.live_backend.borrow_mut().replace(backend);
        }
        let backend = self
            .live_backend
            .borrow()
            .as_ref()
            .cloned()
            .ok_or_else(|| JsValue::from_str("failed to initialize ldk live backend"))?;
        backend.process_events()?;
        let _ = self.reconcile_channels_from_live();
        Ok(())
    }

    fn peer_take_outbound_frames(&self) -> Result<Vec<String>, JsValue> {
        if self.live_backend.borrow().is_none() {
            let backend = create_wasm_ldk_live_backend(
                self.runtime_key.clone(),
                *self.live_node_seed.borrow(),
            )?;
            self.live_backend.borrow_mut().replace(backend);
        }
        let backend = self
            .live_backend
            .borrow()
            .as_ref()
            .cloned()
            .ok_or_else(|| JsValue::from_str("failed to initialize ldk live backend"))?;
        backend.take_outbound_frames()
    }

    fn peer_take_outbound_frames_for_peer(
        &self,
        peer_pubkey: &str,
    ) -> Result<Vec<String>, JsValue> {
        if self.live_backend.borrow().is_none() {
            let backend = create_wasm_ldk_live_backend(
                self.runtime_key.clone(),
                *self.live_node_seed.borrow(),
            )?;
            self.live_backend.borrow_mut().replace(backend);
        }
        let backend = self
            .live_backend
            .borrow()
            .as_ref()
            .cloned()
            .ok_or_else(|| JsValue::from_str("failed to initialize ldk live backend"))?;
        backend.take_outbound_frames_for_peer(peer_pubkey)
    }

    fn peer_socket_disconnected(&self) -> Result<(), JsValue> {
        if self.live_backend.borrow().is_none() {
            let backend = create_wasm_ldk_live_backend(
                self.runtime_key.clone(),
                *self.live_node_seed.borrow(),
            )?;
            self.live_backend.borrow_mut().replace(backend);
        }
        let backend = self
            .live_backend
            .borrow()
            .as_ref()
            .cloned()
            .ok_or_else(|| JsValue::from_str("failed to initialize ldk live backend"))?;
        backend.socket_disconnected()
    }

    fn peer_socket_disconnected_for_peer(&self, peer_pubkey: &str) -> Result<(), JsValue> {
        if self.live_backend.borrow().is_none() {
            let backend = create_wasm_ldk_live_backend(
                self.runtime_key.clone(),
                *self.live_node_seed.borrow(),
            )?;
            self.live_backend.borrow_mut().replace(backend);
        }
        let backend = self
            .live_backend
            .borrow()
            .as_ref()
            .cloned()
            .ok_or_else(|| JsValue::from_str("failed to initialize ldk live backend"))?;
        backend.socket_disconnected_for_peer(peer_pubkey)
    }

    fn peer_is_handshake_complete(&self, peer_pubkey: &str) -> Result<bool, JsValue> {
        if self.live_backend.borrow().is_none() {
            let backend = create_wasm_ldk_live_backend(
                self.runtime_key.clone(),
                *self.live_node_seed.borrow(),
            )?;
            self.live_backend.borrow_mut().replace(backend);
        }
        let backend = self
            .live_backend
            .borrow()
            .as_ref()
            .cloned()
            .ok_or_else(|| JsValue::from_str("failed to initialize ldk live backend"))?;
        backend.is_peer_handshake_complete(peer_pubkey)
    }

    fn set_live_node_seed_hex(&self, seed_hex: String) -> Result<(), JsValue> {
        let seed_hex = seed_hex.trim();
        if seed_hex.is_empty() {
            return Err(JsValue::from_str("seed_hex cannot be empty"));
        }
        let bytes = hex::decode(seed_hex).map_err(|_| JsValue::from_str("invalid seed_hex"))?;
        if bytes.len() != 32 {
            return Err(JsValue::from_str("seed_hex must be 32 bytes hex"));
        }
        let mut seed = [0u8; 32];
        seed.copy_from_slice(&bytes);
        *self.live_node_seed.borrow_mut() = Some(seed);
        // Recreate backend with new identity on next operation.
        self.live_backend.borrow_mut().take();
        Ok(())
    }

    fn set_identity_stable(&self, stable: bool) {
        *self.identity_stable.borrow_mut() = stable;
    }

    fn live_node_pubkey(&self) -> Result<String, JsValue> {
        if self.live_backend.borrow().is_none() {
            let backend = create_wasm_ldk_live_backend(
                self.runtime_key.clone(),
                *self.live_node_seed.borrow(),
            )?;
            self.live_backend.borrow_mut().replace(backend);
        }
        let backend = self
            .live_backend
            .borrow()
            .as_ref()
            .cloned()
            .ok_or_else(|| JsValue::from_str("failed to initialize ldk live backend"))?;
        backend.local_node_pubkey()
    }

    fn persist_live_state(&self) -> Result<(), JsValue> {
        if self.live_backend.borrow().is_none() {
            return Ok(());
        }
        let backend = self
            .live_backend
            .borrow()
            .as_ref()
            .cloned()
            .ok_or_else(|| JsValue::from_str("failed to initialize ldk live backend"))?;
        backend.persist_state()
    }

    fn close_live_channel(
        &self,
        channel_id: &str,
        peer_pubkey: &str,
        force: bool,
    ) -> Result<(), JsValue> {
        let backend = self.ensure_live_backend()?;
        backend.close_live_channel(channel_id, peer_pubkey, force)
    }

    fn chain_relevant_txids(&self) -> Result<Vec<String>, JsValue> {
        let backend = self.ensure_live_backend()?;
        backend.chain_relevant_txids()
    }

    fn chain_apply_best_block(&self, height: u32, header_hex: &str) -> Result<(), JsValue> {
        let backend = self.ensure_live_backend()?;
        backend.chain_apply_best_block(height, header_hex)
    }

    fn chain_apply_confirmed_tx(
        &self,
        height: u32,
        header_hex: &str,
        tx_index: usize,
        tx_hex: &str,
    ) -> Result<(), JsValue> {
        let backend = self.ensure_live_backend()?;
        backend.chain_apply_confirmed_tx(height, header_hex, tx_index, tx_hex)
    }

    fn chain_apply_unconfirmed_tx(&self, txid: &str) -> Result<(), JsValue> {
        let backend = self.ensure_live_backend()?;
        backend.chain_apply_unconfirmed_tx(txid)
    }

    fn drive_rgb_funding_work_boxed(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<(), JsValue>> + 'static>> {
        let maybe_backend = self.live_backend.borrow().as_ref().map(Rc::clone);
        Box::pin(async move {
            if let Some(backend) = maybe_backend {
                backend.drive_rgb_funding_work_boxed().await?;
            }
            Ok(())
        })
    }

    fn process_pending_rgb_transactions_boxed(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<(), JsValue>> + 'static>> {
        let maybe_backend = self.live_backend.borrow().as_ref().map(Rc::clone);
        Box::pin(async move {
            if let Some(backend) = maybe_backend {
                backend.process_pending_rgb_transactions_boxed().await?;
            }
            Ok(())
        })
    }

    fn upsert_channel(&self, channel: LdkRuntimeChannelStateData) {
        let mut channels = self.channels.borrow_mut();
        let incoming_id = channel.channel_id.clone();
        let incoming_temp = channel.temporary_channel_id.clone();

        // If the same temporary channel migrated to a new final channel_id,
        // remove the stale key first so runtime keeps exactly one canonical entry.
        let stale_key = channels.iter().find_map(|(key, ch)| {
            if ch.temporary_channel_id == incoming_temp && *key != incoming_id {
                Some(key.clone())
            } else {
                None
            }
        });
        if let Some(stale_key) = stale_key {
            channels.remove(&stale_key);
            let mut sessions = self.virtual_channel_sessions.borrow_mut();
            if let Some(mut session) = sessions.remove(&stale_key) {
                session.channel_id = incoming_id.clone();
                session.updated_at = unix_now_secs();
                sessions.insert(incoming_id.clone(), session);
            }
        }

        channels.insert(incoming_id, channel);
        drop(channels);
        self.persist_state();
    }

    fn remove_channel(&self, channel_id: &str) -> bool {
        let removed = self.channels.borrow_mut().remove(channel_id).is_some();
        if removed {
            let mut sessions = self.virtual_channel_sessions.borrow_mut();
            if let Some(session) = sessions.get_mut(channel_id) {
                session.status = LdkRuntimeVirtualChannelSessionStatusData::Abandoned;
                session.updated_at = unix_now_secs();
            }
        }
        if removed {
            self.persist_state();
        }
        removed
    }

    fn remove_channels_by_peer(&self, peer_pubkey: &str) -> usize {
        let mut channels = self.channels.borrow_mut();
        let removed_channel_ids = channels
            .values()
            .filter(|ch| ch.peer_pubkey == peer_pubkey)
            .map(|ch| ch.channel_id.clone())
            .collect::<Vec<_>>();
        let before = channels.len();
        channels.retain(|_, ch| ch.peer_pubkey != peer_pubkey);
        let removed = before.saturating_sub(channels.len());
        drop(channels);
        if removed > 0 {
            let mut sessions = self.virtual_channel_sessions.borrow_mut();
            for channel_id in removed_channel_ids {
                if let Some(session) = sessions.get_mut(&channel_id) {
                    session.status = LdkRuntimeVirtualChannelSessionStatusData::Abandoned;
                    session.updated_at = unix_now_secs();
                }
            }
        }
        if removed > 0 {
            self.persist_state();
        }
        removed
    }

    fn set_channel_usable(&self, channel_id: &str, is_usable: bool) -> bool {
        let mut channels = self.channels.borrow_mut();
        let resolved_key = if channels.contains_key(channel_id) {
            channel_id.to_string()
        } else {
            channels
                .iter()
                .find_map(|(key, ch)| {
                    if ch.temporary_channel_id == channel_id {
                        Some(key.clone())
                    } else {
                        None
                    }
                })
                .unwrap_or_default()
        };
        if resolved_key.is_empty() {
            return false;
        }
        let Some(ch) = channels.get_mut(&resolved_key) else {
            return false;
        };
        ch.is_usable = is_usable;
        ch.ready = is_usable;
        ch.status = if is_usable {
            "opened".to_string()
        } else {
            "pending".to_string()
        };
        drop(channels);
        self.persist_state();
        true
    }

    fn find_channel_by_temporary(&self, temporary_channel_id: &str) -> Option<String> {
        self.channels
            .borrow()
            .values()
            .find(|ch| ch.temporary_channel_id == temporary_channel_id)
            .map(|ch| ch.channel_id.clone())
    }

    fn list_channels(&self) -> Vec<LdkRuntimeChannelStateData> {
        self.channels.borrow().values().cloned().collect()
    }

    fn open_channel_non_virtual(
        &self,
        request: LdkRuntimeOpenChannelRequestData,
    ) -> Result<LdkRuntimeOpenChannelResultData, JsValue> {
        if self.live_backend.borrow().is_none() {
            let backend = create_wasm_ldk_live_backend(
                self.runtime_key.clone(),
                *self.live_node_seed.borrow(),
            )?;
            self.live_backend.borrow_mut().replace(backend);
        }
        let backend = self
            .live_backend
            .borrow()
            .as_ref()
            .cloned()
            .ok_or_else(|| JsValue::from_str("failed to initialize ldk live backend"))?;
        backend.open_channel_non_virtual(request)
    }

    fn list_pending_funding_requests(&self) -> Result<Vec<LdkRuntimeFundingRequestData>, JsValue> {
        self.ensure_started()?;
        let backend = self
            .live_backend
            .borrow()
            .as_ref()
            .ok_or_else(|| JsValue::from_str("ldk live backend is not initialized"))?
            .clone();
        backend.list_pending_funding_requests()
    }

    fn submit_funding_transaction(
        &self,
        request: LdkRuntimeFundingTxSubmissionData,
    ) -> Result<(), JsValue> {
        self.ensure_started()?;
        let backend = self
            .live_backend
            .borrow()
            .as_ref()
            .ok_or_else(|| JsValue::from_str("ldk live backend is not initialized"))?
            .clone();
        backend.submit_funding_transaction(request)
    }

    fn reconcile_channels_from_live(&self) -> Result<usize, JsValue> {
        self.ensure_started()?;
        if self.live_backend.borrow().is_none() {
            let backend = create_wasm_ldk_live_backend(
                self.runtime_key.clone(),
                *self.live_node_seed.borrow(),
            )?;
            self.live_backend.borrow_mut().replace(backend);
        }
        let backend = self
            .live_backend
            .borrow()
            .as_ref()
            .cloned()
            .ok_or_else(|| JsValue::from_str("failed to initialize ldk live backend"))?;

        let live_channels = backend.list_live_channels()?;
        // Snapshot the set of live (funding-derived) channel ids before consuming the vec, so we
        // can purge pre-migration "ghost" entries below.
        let live_ids: Vec<String> = live_channels.iter().map(|c| c.channel_id.clone()).collect();
        let mut updated = 0usize;
        for ch in live_channels.into_iter() {
            // Find the cache entry this live channel should supersede.
            //
            // Primary key match: same `channel_id` (covers re-runs of reconcile
            // after the channel has already migrated to its final id).
            //
            // Secondary, peer+temp match: LDK's `ChannelDetails` does *not*
            // expose the original temporary_channel_id once a channel migrates
            // to its final id, so `list_live_channels` is forced to set
            // `temporary_channel_id = channel_id`. Without this fallback, the
            // pre-migration cache entry (keyed and temp-tagged by the real
            // temp id) would be invisible to reconcile, leaving two records
            // for the same logical channel after FundingCreated.
            let existing = {
                let channels = self.channels.borrow();
                channels.get(&ch.channel_id).cloned().or_else(|| {
                    channels
                        .values()
                        .find(|e| {
                            e.channel_id != ch.channel_id
                                && !ch.peer_pubkey.trim().is_empty()
                                && e.peer_pubkey == ch.peer_pubkey
                                && e.channel_id == e.temporary_channel_id
                                && matches!(e.status.as_str(), "opening" | "pending" | "")
                        })
                        .cloned()
                })
            };
            // Inherit the original temporary id from the pre-migration record
            // so `upsert_channel`'s temp→final migration cleanup matches and
            // removes the stale temp-keyed entry.
            let temporary_channel_id = existing
                .as_ref()
                .map(|e| e.temporary_channel_id.clone())
                .filter(|t| !t.is_empty())
                .unwrap_or_else(|| ch.temporary_channel_id.clone());
            let state = LdkRuntimeChannelStateData {
                temporary_channel_id,
                channel_id: ch.channel_id.clone(),
                peer_pubkey: if !ch.peer_pubkey.trim().is_empty() {
                    ch.peer_pubkey.clone()
                } else {
                    existing
                        .as_ref()
                        .map(|e| e.peer_pubkey.clone())
                        .unwrap_or_default()
                },
                status: ch.status.clone(),
                ready: ch.ready,
                is_usable: ch.is_usable,
                public: existing.as_ref().map(|e| e.public).unwrap_or(false),
                capacity_sat: if ch.capacity_sat != 0 {
                    ch.capacity_sat
                } else {
                    existing.as_ref().map(|e| e.capacity_sat).unwrap_or(0)
                },
                asset_id: existing.as_ref().and_then(|e| e.asset_id.clone()),
                asset_local_amount: existing.as_ref().and_then(|e| e.asset_local_amount),
                virtual_open_mode: existing.as_ref().and_then(|e| e.virtual_open_mode.clone()),
            };
            self.upsert_channel(state);
            updated += 1;
        }
        // Authoritative reconcile: the live `ChannelManager` is the source of truth for which real
        // (non-virtual) channels exist. Remove any non-virtual cached channel that is not in the
        // live set. This purges, in one rule:
        //   - closed channels (gone from the manager once it fires Event::ChannelClosed), so the
        //     close API never has to remove optimistically (which would report a still-open channel
        //     as gone — a funds-locked hazard — if the cooperative close stalls), and
        //   - pre-migration temp-id "ghosts" (the live channel migrated to its funding-derived id,
        //     so its temporary id is no longer in the live set).
        // Virtual (off-ledger) channels are not tracked by the ChannelManager, so they are kept.
        // A real channel is added to the live manager synchronously at open (`create_channel`), so a
        // non-virtual cached channel absent from the live set is always stale, never merely "not yet
        // opened".
        let stale_ids: Vec<String> = self
            .channels
            .borrow()
            .iter()
            .filter(|(id, ch)| {
                ch.virtual_open_mode.is_none() && !live_ids.iter().any(|live| live == *id)
            })
            .map(|(id, _)| id.clone())
            .collect();
        for stale_id in stale_ids {
            if self.remove_channel(&stale_id) {
                updated += 1;
            }
        }
        // Drained for completeness; removal is handled authoritatively by the live-set diff above.
        let _ = backend.take_closed_live_channels();
        Ok(updated)
    }

    fn upsert_payment(&self, payment: LdkRuntimePaymentStateData) {
        self.payments
            .borrow_mut()
            .insert(payment.payment_hash.clone(), payment);
        self.persist_state();
    }

    fn get_payment(&self, payment_hash: &str) -> Option<LdkRuntimePaymentStateData> {
        self.payments.borrow().get(payment_hash).cloned()
    }

    fn list_payments(&self) -> Vec<LdkRuntimePaymentStateData> {
        self.payments.borrow().values().cloned().collect()
    }

    fn keysend_live(
        &self,
        dest_pubkey: &str,
        amt_msat: u64,
        contract_id: Option<String>,
        asset_amount: Option<u64>,
    ) -> Result<LdkRuntimeLivePaymentData, JsValue> {
        self.ensure_started()?;
        let backend = self.ensure_live_backend()?;
        backend.keysend_live(dest_pubkey, amt_msat, contract_id, asset_amount)
    }

    fn send_bolt11_live(
        &self,
        invoice: &str,
        amt_msat: Option<u64>,
        contract_id: Option<String>,
        asset_amount: Option<u64>,
    ) -> Result<LdkRuntimeLivePaymentData, JsValue> {
        self.ensure_started()?;
        self.ensure_live_backend()?
            .send_bolt11_live(invoice, amt_msat, contract_id, asset_amount)
    }

    fn create_bolt11_invoice_live(
        &self,
        amt_msat: Option<u64>,
        expiry_sec: u32,
        contract_id: Option<String>,
        asset_amount: Option<u64>,
    ) -> Result<String, JsValue> {
        self.ensure_started()?;
        self.ensure_live_backend()?.create_bolt11_invoice_live(
            amt_msat,
            expiry_sec,
            contract_id,
            asset_amount,
        )
    }

    fn create_hodl_bolt11_invoice_live(
        &self,
        amt_msat: Option<u64>,
        expiry_sec: u32,
        contract_id: Option<String>,
        asset_amount: Option<u64>,
        payment_hash: &str,
    ) -> Result<String, JsValue> {
        self.ensure_started()?;
        self.ensure_live_backend()?.create_hodl_bolt11_invoice_live(
            amt_msat,
            expiry_sec,
            contract_id,
            asset_amount,
            payment_hash,
        )
    }

    fn claim_hodl_invoice_live(
        &self,
        payment_hash: &str,
        payment_preimage: &str,
    ) -> Result<bool, JsValue> {
        self.ensure_started()?;
        self.ensure_live_backend()?
            .claim_hodl_invoice_live(payment_hash, payment_preimage)
    }

    fn cancel_hodl_invoice_live(&self, payment_hash: &str) -> Result<(), JsValue> {
        self.ensure_started()?;
        self.ensure_live_backend()?
            .cancel_hodl_invoice_live(payment_hash)
    }

    fn live_payments(&self) -> Vec<LdkRuntimeLivePaymentData> {
        match self.live_backend.borrow().as_ref() {
            Some(backend) => backend.live_payments(),
            None => Vec::new(),
        }
    }

    fn live_payment(&self, payment_hash: &str) -> Option<LdkRuntimeLivePaymentData> {
        self.live_backend
            .borrow()
            .as_ref()
            .and_then(|backend| backend.live_payment(payment_hash))
    }

    fn virtual_channel_add_intent(
        &self,
        peer_pubkey: &str,
        temporary_channel_id: Option<String>,
    ) -> Result<String, String> {
        let duplicate_virtual_draft = self
            .virtual_channel_drafts
            .borrow()
            .values()
            .any(|draft| draft.peer_pubkey == peer_pubkey);
        if duplicate_virtual_draft {
            return Err("virtual channel draft already exists for this peer pair".to_string());
        }

        let duplicate_virtual_session =
            self.virtual_channel_sessions
                .borrow()
                .values()
                .any(|session| {
                    session.peer_pubkey == peer_pubkey
                        && session.status != LdkRuntimeVirtualChannelSessionStatusData::Abandoned
                });
        if duplicate_virtual_session {
            return Err("virtual channel session already exists for this peer pair".to_string());
        }

        let temporary_channel_id = temporary_channel_id
            .filter(|id| !id.trim().is_empty())
            .ok_or_else(|| "temporary_channel_id is required".to_string())?;
        if self.channels.borrow().contains_key(&temporary_channel_id)
            || self
                .virtual_channel_drafts
                .borrow()
                .contains_key(&temporary_channel_id)
        {
            return Err("temporary_channel_id already used".to_string());
        }

        self.virtual_channel_drafts.borrow_mut().insert(
            temporary_channel_id.clone(),
            LdkRuntimeVirtualChannelDraftData {
                temporary_channel_id: temporary_channel_id.clone(),
                peer_pubkey: peer_pubkey.to_string(),
                created_at: unix_now_secs(),
            },
        );
        self.persist_state();
        Ok(temporary_channel_id)
    }

    fn virtual_channel_draft_delete(&self, temporary_channel_id: &str) -> bool {
        let removed = self
            .virtual_channel_drafts
            .borrow_mut()
            .remove(temporary_channel_id)
            .is_some();
        if removed {
            self.persist_state();
        }
        removed
    }

    fn virtual_channel_draft_get(
        &self,
        temporary_channel_id: &str,
    ) -> Option<LdkRuntimeVirtualChannelDraftData> {
        self.virtual_channel_drafts
            .borrow()
            .get(temporary_channel_id)
            .cloned()
    }

    fn virtual_channel_draft_store(&self) -> Vec<LdkRuntimeVirtualChannelDraftData> {
        self.virtual_channel_drafts
            .borrow()
            .values()
            .cloned()
            .collect()
    }

    fn virtual_channel_session_add_from_open(
        &self,
        channel_id: &str,
        temporary_channel_id: &str,
        peer_pubkey: &str,
    ) {
        let now = unix_now_secs();
        self.virtual_channel_sessions.borrow_mut().insert(
            channel_id.to_string(),
            LdkRuntimeVirtualChannelSessionData {
                channel_id: channel_id.to_string(),
                former_temporary_channel_id: temporary_channel_id.to_string(),
                peer_pubkey: peer_pubkey.to_string(),
                status: LdkRuntimeVirtualChannelSessionStatusData::Active,
                created_at: now,
                updated_at: now,
            },
        );
        self.virtual_channel_drafts
            .borrow_mut()
            .remove(temporary_channel_id);
        self.persist_state();
    }

    fn virtual_channel_session_get(
        &self,
        channel_id: &str,
    ) -> Option<LdkRuntimeVirtualChannelSessionData> {
        self.virtual_channel_sessions
            .borrow()
            .get(channel_id)
            .cloned()
    }

    fn virtual_channel_session_get_by_peer(
        &self,
        peer_pubkey: &str,
    ) -> Option<LdkRuntimeVirtualChannelSessionData> {
        self.virtual_channel_sessions
            .borrow()
            .values()
            .find(|session| session.peer_pubkey == peer_pubkey)
            .cloned()
    }

    fn virtual_channel_session_update_status(
        &self,
        channel_id: &str,
        status: LdkRuntimeVirtualChannelSessionStatusData,
    ) -> bool {
        let mut sessions = self.virtual_channel_sessions.borrow_mut();
        let Some(session) = sessions.get_mut(channel_id) else {
            return false;
        };
        session.status = status;
        session.updated_at = unix_now_secs();
        drop(sessions);
        self.persist_state();
        true
    }

    fn virtual_channel_session_store(&self) -> Vec<LdkRuntimeVirtualChannelSessionData> {
        self.virtual_channel_sessions
            .borrow()
            .values()
            .cloned()
            .collect()
    }

    fn virtual_channel_reconcile_sessions(&self) {
        let channels = self.channels.borrow();
        let mut drafts = self.virtual_channel_drafts.borrow_mut();
        let mut sessions = self.virtual_channel_sessions.borrow_mut();
        reconcile_virtual_channel_state(&channels, &mut drafts, &mut sessions);
        drop(sessions);
        drop(drafts);
        drop(channels);
        self.persist_state();
    }

    fn component_status(&self) -> LdkRuntimeComponentsStatusData {
        let started = *self.started.borrow();
        let counters = self.counters.borrow().clone();
        LdkRuntimeComponentsStatusData {
            backend: self.backend_label.clone(),
            started,
            fee_estimator_ready: started,
            broadcaster_ready: started,
            logger_ready: started,
            persister_ready: started,
            key_manager_ready: started,
            payment_engine_ready: started,
            channel_engine_ready: started,
            key_manager_fingerprint: self.key_manager_fingerprint.clone(),
            invoices_created: counters.invoices_created,
            payments_initiated: counters.payments_initiated,
            keysends_initiated: counters.keysends_initiated,
            channels_opened: counters.channels_opened,
            channels_closed: counters.channels_closed,
        }
    }

    fn record_invoice_created(&self) {
        let mut counters = self.counters.borrow_mut();
        counters.invoices_created = counters.invoices_created.saturating_add(1);
        drop(counters);
        self.persist_state();
    }

    fn record_payment_initiated(&self) {
        let mut counters = self.counters.borrow_mut();
        counters.payments_initiated = counters.payments_initiated.saturating_add(1);
        drop(counters);
        self.persist_state();
    }

    fn record_keysend_initiated(&self) {
        let mut counters = self.counters.borrow_mut();
        counters.keysends_initiated = counters.keysends_initiated.saturating_add(1);
        drop(counters);
        self.persist_state();
    }

    fn record_channel_opened(&self) {
        let mut counters = self.counters.borrow_mut();
        counters.channels_opened = counters.channels_opened.saturating_add(1);
        drop(counters);
        self.persist_state();
    }

    fn record_channel_closed(&self) {
        let mut counters = self.counters.borrow_mut();
        counters.channels_closed = counters.channels_closed.saturating_add(1);
        drop(counters);
        self.persist_state();
    }
}

fn runtime_key_fingerprint(runtime_key: &str) -> String {
    let hash = Sha256::hash(runtime_key.as_bytes()).to_byte_array();
    hex::encode(&hash[..8])
}

const WASM_NATIVE_LDK_BACKEND_LABEL: &str = "wasm_native_ldk";

pub fn ldk_runtime_manager(runtime_key: String) -> Result<Rc<dyn LdkRuntimeManager>, JsValue> {
    let runtime_key = canonicalize_runtime_key(&runtime_key);
    let manager_registry_key = runtime_key.clone();
    if let Some(manager) = RUNTIME_MANAGER_REGISTRY.with(|registry| {
        let mut registry = registry.borrow_mut();
        registry.retain(|_, weak| weak.strong_count() > 0);
        registry.get(&manager_registry_key).and_then(Weak::upgrade)
    }) {
        return Ok(manager);
    }
    let storage: Rc<dyn LdkRuntimeStorage> = Rc::new(BrowserBackedLdkRuntimeStorage);
    let manager: Rc<dyn LdkRuntimeManager> = Rc::new(WasmNativeRuntimeManager::new(
        runtime_key,
        WASM_NATIVE_LDK_BACKEND_LABEL.to_string(),
        storage,
    ));
    RUNTIME_MANAGER_REGISTRY.with(|registry| {
        registry
            .borrow_mut()
            .insert(manager_registry_key, Rc::downgrade(&manager));
    });
    Ok(manager)
}

pub fn release_runtime_manager_if_last(runtime_key: &str, manager: &Rc<dyn LdkRuntimeManager>) {
    let runtime_key = canonicalize_runtime_key(runtime_key);
    RUNTIME_MANAGER_REGISTRY.with(|registry| {
        let mut registry = registry.borrow_mut();
        let should_remove = registry
            .get(&runtime_key)
            .and_then(Weak::upgrade)
            .map(|existing| Rc::ptr_eq(&existing, manager) && Rc::strong_count(&existing) <= 1)
            .unwrap_or(true);
        if should_remove {
            registry.remove(&runtime_key);
        }
        registry.retain(|_, weak| weak.strong_count() > 0);
    });
}

fn canonicalize_runtime_key(runtime_key: &str) -> String {
    let trimmed = runtime_key.trim();
    let Some(scope) = trimmed.strip_prefix("node-runtime:") else {
        return trimmed.to_string();
    };
    format!("node-runtime:{}", canonicalize_runtime_scope(scope))
}

fn canonicalize_runtime_scope(scope: &str) -> String {
    let scope = scope.trim();
    if let Some((proxy, runtime_id)) = scope.split_once("#runtime:") {
        let proxy = canonicalize_proxy_endpoint(proxy);
        let runtime_id = runtime_id.trim();
        if runtime_id.is_empty() {
            proxy
        } else {
            format!("{proxy}#runtime:{runtime_id}")
        }
    } else {
        canonicalize_proxy_endpoint(scope)
    }
}

pub(crate) fn canonicalize_proxy_endpoint(value: &str) -> String {
    let mut value = value.trim().to_string();
    if let Some(scheme_sep) = value.find("://") {
        let scheme = value[..scheme_sep].to_ascii_lowercase();
        let rest = &value[scheme_sep + 3..];
        let boundary = rest.find(['/', '?', '#']).unwrap_or(rest.len());
        let authority = rest[..boundary].to_ascii_lowercase();
        let suffix = &rest[boundary..];
        value = format!("{scheme}://{authority}{suffix}");
    }
    while value.ends_with('/') {
        value.pop();
    }
    value
}

#[cfg(test)]
#[path = "tests/ldk_runtime_test_utils.rs"]
pub(crate) mod test_utils;

#[cfg(test)]
#[path = "tests/ldk_runtime_tests.rs"]
mod tests;
