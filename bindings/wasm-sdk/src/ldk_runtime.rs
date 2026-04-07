use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use serde::{Deserialize, Serialize};
use wasm_bindgen::prelude::JsValue;

use crate::runtime_store::{browser_persistent_state_store, RuntimeStateStore};

#[derive(Clone, Debug, Serialize)]
pub struct LdkRuntimeStatusData {
    pub backend: String,
    pub lifecycle_state: String,
    pub ready: bool,
    pub storage_initialized: bool,
    pub schema_version: u32,
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
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LdkRuntimePaymentStateData {
    pub amt_msat: Option<u64>,
    pub asset_amount: Option<u64>,
    pub asset_id: Option<String>,
    pub payment_hash: String,
    pub inbound: bool,
    pub status: String,
    pub created_at: u64,
    pub updated_at: u64,
    pub payee_pubkey: String,
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
    fn upsert_channel(&self, channel: LdkRuntimeChannelStateData);
    fn remove_channel(&self, channel_id: &str) -> bool;
    fn remove_channels_by_peer(&self, peer_pubkey: &str) -> usize;
    fn set_channel_usable(&self, channel_id: &str, is_usable: bool) -> bool;
    fn find_channel_by_temporary(&self, temporary_channel_id: &str) -> Option<String>;
    fn list_channels(&self) -> Vec<LdkRuntimeChannelStateData>;
    fn upsert_payment(&self, payment: LdkRuntimePaymentStateData);
    fn get_payment(&self, payment_hash: &str) -> Option<LdkRuntimePaymentStateData>;
    fn list_payments(&self) -> Vec<LdkRuntimePaymentStateData>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LdkRuntimeBackendKind {
    Scaffold,
    LdkBridge,
}

impl LdkRuntimeBackendKind {
    fn parse(value: &str) -> Result<Self, JsValue> {
        match value.trim().to_ascii_lowercase().as_str() {
            "" | "scaffold" => Ok(Self::Scaffold),
            "ldk_bridge" | "bridge" => Ok(Self::LdkBridge),
            other => Err(JsValue::from_str(&format!(
                "unknown runtime backend: {other}"
            ))),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct LdkRuntimeSnapshot {
    started: bool,
    restored: bool,
    storage_initialized: bool,
    schema_version: u32,
    #[serde(default)]
    bridge_session_initialized: bool,
    #[serde(default)]
    peers: Vec<LdkRuntimePeerStateData>,
    #[serde(default)]
    channels: Vec<LdkRuntimeChannelStateData>,
    #[serde(default)]
    payments: Vec<LdkRuntimePaymentStateData>,
}

trait LdkRuntimeStorage {
    fn load(&self, key: &str) -> Result<Option<LdkRuntimeSnapshot>, JsValue>;
    fn save(&self, key: &str, snapshot: &LdkRuntimeSnapshot) -> Result<(), JsValue>;
}

struct InMemoryLdkRuntimeStorage;
struct BrowserBackedLdkRuntimeStorage;

const RUNTIME_STORAGE_PREFIX: &str = "rln:wasm:ldk-runtime:";

thread_local! {
    static SCAFFOLD_RUNTIME_STORAGE: RefCell<HashMap<String, LdkRuntimeSnapshot>> =
        RefCell::new(HashMap::new());
}

impl LdkRuntimeStorage for InMemoryLdkRuntimeStorage {
    fn load(&self, key: &str) -> Result<Option<LdkRuntimeSnapshot>, JsValue> {
        Ok(SCAFFOLD_RUNTIME_STORAGE.with(|storage| storage.borrow().get(key).cloned()))
    }

    fn save(&self, key: &str, snapshot: &LdkRuntimeSnapshot) -> Result<(), JsValue> {
        SCAFFOLD_RUNTIME_STORAGE.with(|storage| {
            storage
                .borrow_mut()
                .insert(key.to_string(), snapshot.clone());
        });
        Ok(())
    }
}

impl BrowserBackedLdkRuntimeStorage {
    fn scoped_key(key: &str) -> String {
        format!("{RUNTIME_STORAGE_PREFIX}{key}")
    }
}

impl LdkRuntimeStorage for BrowserBackedLdkRuntimeStorage {
    fn load(&self, key: &str) -> Result<Option<LdkRuntimeSnapshot>, JsValue> {
        let scoped_key = Self::scoped_key(key);
        let store = browser_persistent_state_store();
        if let Some(raw) = store.get(&scoped_key)? {
            if let Ok(snapshot) = serde_json::from_str::<LdkRuntimeSnapshot>(&raw) {
                return Ok(Some(snapshot));
            }
        }

        InMemoryLdkRuntimeStorage.load(key)
    }

    fn save(&self, key: &str, snapshot: &LdkRuntimeSnapshot) -> Result<(), JsValue> {
        let scoped_key = Self::scoped_key(key);
        let raw = serde_json::to_string(snapshot).map_err(|e| JsValue::from_str(&e.to_string()))?;
        let store = browser_persistent_state_store();
        let _ = store.set(&scoped_key, &raw);
        InMemoryLdkRuntimeStorage.save(key, snapshot)
    }
}

struct ScaffoldLdkRuntimeManager {
    runtime_key: String,
    storage: Rc<dyn LdkRuntimeStorage>,
    started: RefCell<bool>,
    restored: RefCell<bool>,
    storage_initialized: RefCell<bool>,
    peers: RefCell<HashMap<String, LdkRuntimePeerStateData>>,
    channels: RefCell<HashMap<String, LdkRuntimeChannelStateData>>,
    payments: RefCell<HashMap<String, LdkRuntimePaymentStateData>>,
}

struct LdkBridgeRuntimeManager {
    runtime_key: String,
    storage: Rc<dyn LdkRuntimeStorage>,
    started: RefCell<bool>,
    restored: RefCell<bool>,
    storage_initialized: RefCell<bool>,
    bridge_session_initialized: RefCell<bool>,
    peers: RefCell<HashMap<String, LdkRuntimePeerStateData>>,
    channels: RefCell<HashMap<String, LdkRuntimeChannelStateData>>,
    payments: RefCell<HashMap<String, LdkRuntimePaymentStateData>>,
}

impl ScaffoldLdkRuntimeManager {
    fn new(runtime_key: String, storage: Rc<dyn LdkRuntimeStorage>) -> Self {
        Self {
            runtime_key,
            storage,
            started: RefCell::new(false),
            restored: RefCell::new(false),
            storage_initialized: RefCell::new(false),
            peers: RefCell::new(HashMap::new()),
            channels: RefCell::new(HashMap::new()),
            payments: RefCell::new(HashMap::new()),
        }
    }

    fn snapshot(&self) -> LdkRuntimeSnapshot {
        LdkRuntimeSnapshot {
            started: *self.started.borrow(),
            restored: *self.restored.borrow(),
            storage_initialized: *self.storage_initialized.borrow(),
            schema_version: 1,
            bridge_session_initialized: false,
            peers: self.peers.borrow().values().cloned().collect(),
            channels: self.channels.borrow().values().cloned().collect(),
            payments: self.payments.borrow().values().cloned().collect(),
        }
    }

    fn persist_state(&self) {
        let _ = self.storage.save(&self.runtime_key, &self.snapshot());
    }
}

impl LdkBridgeRuntimeManager {
    fn new(runtime_key: String, storage: Rc<dyn LdkRuntimeStorage>) -> Self {
        Self {
            runtime_key,
            storage,
            started: RefCell::new(false),
            restored: RefCell::new(false),
            storage_initialized: RefCell::new(false),
            bridge_session_initialized: RefCell::new(false),
            peers: RefCell::new(HashMap::new()),
            channels: RefCell::new(HashMap::new()),
            payments: RefCell::new(HashMap::new()),
        }
    }

    fn snapshot(&self) -> LdkRuntimeSnapshot {
        LdkRuntimeSnapshot {
            started: *self.started.borrow(),
            restored: *self.restored.borrow(),
            storage_initialized: *self.storage_initialized.borrow(),
            schema_version: 1,
            bridge_session_initialized: *self.bridge_session_initialized.borrow(),
            peers: self.peers.borrow().values().cloned().collect(),
            channels: self.channels.borrow().values().cloned().collect(),
            payments: self.payments.borrow().values().cloned().collect(),
        }
    }

    fn persist_state(&self) {
        let _ = self.storage.save(&self.runtime_key, &self.snapshot());
    }
}

impl LdkRuntimeManager for ScaffoldLdkRuntimeManager {
    fn status(&self) -> LdkRuntimeStatusData {
        let started = *self.started.borrow();
        let restored = *self.restored.borrow();
        let storage_initialized = *self.storage_initialized.borrow();
        let lifecycle_state = if started {
            if restored {
                "running_restored"
            } else {
                "running"
            }
        } else if restored {
            "restored_stopped"
        } else {
            "cold"
        };
        LdkRuntimeStatusData {
            backend: "scaffold".to_string(),
            lifecycle_state: lifecycle_state.to_string(),
            ready: started,
            storage_initialized,
            schema_version: 1,
        }
    }

    fn start(&self) -> Result<(), JsValue> {
        *self.storage_initialized.borrow_mut() = true;
        *self.started.borrow_mut() = true;
        self.storage.save(&self.runtime_key, &self.snapshot())?;
        Ok(())
    }

    fn restore(&self) -> Result<(), JsValue> {
        if let Some(snapshot) = self.storage.load(&self.runtime_key)? {
            *self.storage_initialized.borrow_mut() = snapshot.storage_initialized;
            *self.restored.borrow_mut() = true;
            self.peers.borrow_mut().clear();
            self.channels.borrow_mut().clear();
            self.payments.borrow_mut().clear();
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
        } else {
            *self.storage_initialized.borrow_mut() = true;
            *self.restored.borrow_mut() = false;
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
        *self.started.borrow_mut() = false;
        self.storage.save(&self.runtime_key, &self.snapshot())?;
        Ok(())
    }

    fn has_peer(&self, peer_pubkey: &str) -> bool {
        self.peers.borrow().contains_key(peer_pubkey)
    }

    fn has_connected_peer(&self, peer_pubkey: &str) -> bool {
        self.peers
            .borrow()
            .get(peer_pubkey)
            .map(|peer| peer.started)
            .unwrap_or(false)
    }

    fn has_any_connected_peer(&self) -> bool {
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

    fn upsert_channel(&self, channel: LdkRuntimeChannelStateData) {
        self.channels
            .borrow_mut()
            .insert(channel.channel_id.clone(), channel);
        self.persist_state();
    }

    fn remove_channel(&self, channel_id: &str) -> bool {
        let removed = self.channels.borrow_mut().remove(channel_id).is_some();
        if removed {
            self.persist_state();
        }
        removed
    }

    fn remove_channels_by_peer(&self, peer_pubkey: &str) -> usize {
        let mut channels = self.channels.borrow_mut();
        let before = channels.len();
        channels.retain(|_, ch| ch.peer_pubkey != peer_pubkey);
        let removed = before.saturating_sub(channels.len());
        drop(channels);
        if removed > 0 {
            self.persist_state();
        }
        removed
    }

    fn set_channel_usable(&self, channel_id: &str, is_usable: bool) -> bool {
        let mut channels = self.channels.borrow_mut();
        let Some(ch) = channels.get_mut(channel_id) else {
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
}

impl LdkRuntimeManager for LdkBridgeRuntimeManager {
    fn status(&self) -> LdkRuntimeStatusData {
        let started = *self.started.borrow();
        let restored = *self.restored.borrow();
        let storage_initialized = *self.storage_initialized.borrow();
        let bridge_session_initialized = *self.bridge_session_initialized.borrow();
        let lifecycle_state = if started {
            if restored {
                "running_restored"
            } else {
                "running"
            }
        } else if restored {
            "restored_stopped"
        } else {
            "cold"
        };
        LdkRuntimeStatusData {
            backend: "ldk_bridge".to_string(),
            lifecycle_state: lifecycle_state.to_string(),
            ready: started && bridge_session_initialized,
            storage_initialized,
            schema_version: 1,
        }
    }

    fn start(&self) -> Result<(), JsValue> {
        *self.storage_initialized.borrow_mut() = true;
        *self.bridge_session_initialized.borrow_mut() = true;
        *self.started.borrow_mut() = true;
        self.storage.save(&self.runtime_key, &self.snapshot())?;
        Ok(())
    }

    fn restore(&self) -> Result<(), JsValue> {
        if let Some(snapshot) = self.storage.load(&self.runtime_key)? {
            *self.storage_initialized.borrow_mut() = snapshot.storage_initialized;
            *self.bridge_session_initialized.borrow_mut() = snapshot.bridge_session_initialized;
            *self.restored.borrow_mut() = true;
            self.peers.borrow_mut().clear();
            self.channels.borrow_mut().clear();
            self.payments.borrow_mut().clear();
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
        } else {
            *self.storage_initialized.borrow_mut() = true;
            *self.bridge_session_initialized.borrow_mut() = false;
            *self.restored.borrow_mut() = false;
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
        *self.started.borrow_mut() = false;
        *self.bridge_session_initialized.borrow_mut() = false;
        self.storage.save(&self.runtime_key, &self.snapshot())?;
        Ok(())
    }

    fn has_peer(&self, peer_pubkey: &str) -> bool {
        self.peers.borrow().contains_key(peer_pubkey)
    }

    fn has_connected_peer(&self, peer_pubkey: &str) -> bool {
        self.peers
            .borrow()
            .get(peer_pubkey)
            .map(|peer| peer.started)
            .unwrap_or(false)
    }

    fn has_any_connected_peer(&self) -> bool {
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

    fn upsert_channel(&self, channel: LdkRuntimeChannelStateData) {
        self.channels
            .borrow_mut()
            .insert(channel.channel_id.clone(), channel);
        self.persist_state();
    }

    fn remove_channel(&self, channel_id: &str) -> bool {
        let removed = self.channels.borrow_mut().remove(channel_id).is_some();
        if removed {
            self.persist_state();
        }
        removed
    }

    fn remove_channels_by_peer(&self, peer_pubkey: &str) -> usize {
        let mut channels = self.channels.borrow_mut();
        let before = channels.len();
        channels.retain(|_, ch| ch.peer_pubkey != peer_pubkey);
        let removed = before.saturating_sub(channels.len());
        drop(channels);
        if removed > 0 {
            self.persist_state();
        }
        removed
    }

    fn set_channel_usable(&self, channel_id: &str, is_usable: bool) -> bool {
        let mut channels = self.channels.borrow_mut();
        let Some(ch) = channels.get_mut(channel_id) else {
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
}

pub fn ldk_runtime_manager(
    runtime_key: String,
    backend_hint: Option<String>,
) -> Result<Rc<dyn LdkRuntimeManager>, JsValue> {
    let backend_kind = LdkRuntimeBackendKind::parse(backend_hint.as_deref().unwrap_or("scaffold"))?;
    let storage: Rc<dyn LdkRuntimeStorage> = Rc::new(BrowserBackedLdkRuntimeStorage);
    let manager: Rc<dyn LdkRuntimeManager> = match backend_kind {
        LdkRuntimeBackendKind::Scaffold => {
            Rc::new(ScaffoldLdkRuntimeManager::new(runtime_key, storage))
        }
        LdkRuntimeBackendKind::LdkBridge => {
            Rc::new(LdkBridgeRuntimeManager::new(runtime_key, storage))
        }
    };
    Ok(manager)
}

pub fn scaffold_ldk_runtime_manager(runtime_key: String) -> Rc<dyn LdkRuntimeManager> {
    // Backward-compatible helper kept for existing call sites.
    ldk_runtime_manager(runtime_key, Some("scaffold".to_string()))
        .expect("scaffold runtime manager must be constructible")
}

#[cfg(test)]
pub fn reset_scaffold_runtime_storage_for_tests() {
    SCAFFOLD_RUNTIME_STORAGE.with(|storage| storage.borrow_mut().clear());
    RUNTIME_SESSION_AUTHORITY_STATE.with(|state| {
        *state.borrow_mut() = RuntimeSessionAuthorityState::default();
    });
}

#[cfg(test)]
mod tests;
