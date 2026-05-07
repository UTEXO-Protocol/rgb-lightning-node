use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::collections::VecDeque;
use std::rc::Rc;

use serde::Serialize;
use wasm_bindgen::prelude::JsValue;

#[derive(Clone, Debug, Serialize)]
pub struct WasmLdkPeerEngineStatusData {
    pub runtime_scope_key: String,
    pub initialized: bool,
    pub active_descriptors: usize,
}

#[derive(Clone, Debug, Serialize)]
pub struct WasmLdkPeerDescriptorData {
    pub descriptor_id: u64,
    pub session_key: String,
    pub peer_pubkey: String,
    pub peer_addr: String,
    pub connected_at: u64,
    pub disconnected_at: Option<u64>,
    pub last_read_at: Option<u64>,
    pub last_process_at: Option<u64>,
    pub last_error: Option<String>,
}

#[derive(Clone)]
pub struct WasmLdkPeerEngine {
    runtime_scope_key: String,
    initialized: Rc<RefCell<bool>>,
    descriptors: Rc<RefCell<HashMap<u64, WasmLdkPeerDescriptorData>>>,
    peer_index: Rc<RefCell<HashMap<String, u64>>>,
    outbound_frames: Rc<RefCell<VecDeque<String>>>,
    runtime_event_payload_hex: Rc<RefCell<VecDeque<String>>>,
    next_descriptor_id: Rc<Cell<u64>>,
}

impl WasmLdkPeerEngine {
    pub fn new(runtime_scope_key: String) -> Result<Self, JsValue> {
        if runtime_scope_key.trim().is_empty() {
            return Err(JsValue::from_str(sdk_contracts::ERR_RUNTIME_SCOPE_KEY_EMPTY));
        }
        Ok(Self {
            runtime_scope_key,
            initialized: Rc::new(RefCell::new(false)),
            descriptors: Rc::new(RefCell::new(HashMap::new())),
            peer_index: Rc::new(RefCell::new(HashMap::new())),
            outbound_frames: Rc::new(RefCell::new(VecDeque::new())),
            runtime_event_payload_hex: Rc::new(RefCell::new(VecDeque::new())),
            next_descriptor_id: Rc::new(Cell::new(1)),
        })
    }

    pub fn ensure_initialized(&self) {
        *self.initialized.borrow_mut() = true;
    }

    pub fn status(&self) -> WasmLdkPeerEngineStatusData {
        WasmLdkPeerEngineStatusData {
            runtime_scope_key: self.runtime_scope_key.clone(),
            initialized: *self.initialized.borrow(),
            active_descriptors: self.peer_index.borrow().len(),
        }
    }

    pub fn register_descriptor(
        &self,
        session_key: &str,
        peer_pubkey: &str,
        peer_addr: &str,
    ) -> Result<u64, JsValue> {
        let session_key = session_key.trim();
        let peer_pubkey = peer_pubkey.trim();
        let peer_addr = peer_addr.trim();
        if session_key.is_empty() {
            return Err(JsValue::from_str(sdk_contracts::ERR_SESSION_KEY_EMPTY));
        }
        if peer_pubkey.is_empty() {
            return Err(JsValue::from_str(sdk_contracts::ERR_PEER_PUBKEY_INVALID));
        }
        if peer_addr.is_empty() {
            return Err(JsValue::from_str(sdk_contracts::ERR_PEER_ADDR_EMPTY));
        }
        let descriptor_id = self.next_descriptor_id.get();
        self.next_descriptor_id
            .set(descriptor_id.saturating_add(1).max(1));
        let descriptor = WasmLdkPeerDescriptorData {
            descriptor_id,
            session_key: session_key.to_string(),
            peer_pubkey: peer_pubkey.to_string(),
            peer_addr: peer_addr.to_string(),
            connected_at: unix_now_secs(),
            disconnected_at: None,
            last_read_at: None,
            last_process_at: None,
            last_error: None,
        };
        self.descriptors
            .borrow_mut()
            .insert(descriptor_id, descriptor.clone());
        self.peer_index
            .borrow_mut()
            .insert(descriptor.peer_pubkey, descriptor_id);
        self.enqueue_runtime_transport_event("peer_reconnected", peer_pubkey);
        Ok(descriptor_id)
    }

    pub fn mark_disconnected(&self, peer_pubkey: &str, reason: Option<&str>) -> Result<(), JsValue> {
        let peer_pubkey = peer_pubkey.trim();
        if peer_pubkey.is_empty() {
            return Ok(());
        }
        let Some(descriptor_id) = self.peer_index.borrow().get(peer_pubkey).copied() else {
            return Ok(());
        };
        if let Some(descriptor) = self.descriptors.borrow_mut().get_mut(&descriptor_id) {
            descriptor.disconnected_at = Some(unix_now_secs());
            if let Some(reason) = reason {
                let trimmed = reason.trim();
                if !trimmed.is_empty() {
                    descriptor.last_error = Some(trimmed.to_string());
                }
            }
        }
        self.peer_index.borrow_mut().remove(peer_pubkey);
        self.enqueue_runtime_transport_event("peer_disconnected", peer_pubkey);
        Ok(())
    }

    pub fn new_outbound_connection(&self, _peer_pubkey: &str) -> Result<String, JsValue> {
        Err(JsValue::from_str(
            "real LDK peer-manager outbound handshake is not implemented yet",
        ))
    }

    pub fn read_event(&self, payload_hex: &str) -> Result<(), JsValue> {
        let payload_hex = payload_hex.trim();
        if payload_hex.is_empty() {
            return Err(JsValue::from_str(sdk_contracts::ERR_PAYLOAD_HEX_EMPTY));
        }
        if self.peer_index.borrow().is_empty() {
            return Err(JsValue::from_str(
                "no active peer descriptor registered for read_event",
            ));
        }
        let now = unix_now_secs();
        for descriptor_id in self.peer_index.borrow().values() {
            if let Some(descriptor) = self.descriptors.borrow_mut().get_mut(descriptor_id) {
                descriptor.last_read_at = Some(now);
            }
        }
        Err(JsValue::from_str(
            "real LDK peer-manager read_event path is not implemented yet",
        ))
    }

    pub fn process_events(&self) -> Result<(), JsValue> {
        let now = unix_now_secs();
        for descriptor_id in self.peer_index.borrow().values() {
            if let Some(descriptor) = self.descriptors.borrow_mut().get_mut(descriptor_id) {
                descriptor.last_process_at = Some(now);
            }
        }
        Err(JsValue::from_str(
            "real LDK peer-manager process_events path is not implemented yet",
        ))
    }

    pub fn socket_disconnected(&self) -> Result<(), JsValue> {
        let now = unix_now_secs();
        let active = self
            .peer_index
            .borrow()
            .values()
            .copied()
            .collect::<Vec<_>>();
        for descriptor_id in active {
            if let Some(descriptor) = self.descriptors.borrow_mut().get_mut(&descriptor_id) {
                descriptor.disconnected_at = Some(now);
            }
        }
        self.peer_index.borrow_mut().clear();
        Err(JsValue::from_str(
            "real LDK peer-manager disconnect path is not implemented yet",
        ))
    }

    pub fn take_outbound_frames(&self) -> Vec<String> {
        let mut out = Vec::new();
        while let Some(frame) = self.outbound_frames.borrow_mut().pop_front() {
            out.push(frame);
        }
        out
    }

    pub fn drain_runtime_event_payload_hex(&self) -> Vec<String> {
        let mut out = Vec::new();
        while let Some(payload) = self.runtime_event_payload_hex.borrow_mut().pop_front() {
            out.push(payload);
        }
        out
    }

    fn enqueue_runtime_transport_event(&self, event_kind: &str, id: &str) {
        let payload = if event_kind.starts_with("peer_") {
            serde_json::json!({
                "event": event_kind,
                "peer_pubkey": id,
            })
        } else {
            serde_json::json!({
                "event": event_kind,
                "channel_id": id,
            })
        };
        if let Ok(raw) = serde_json::to_string(&payload) {
            self.runtime_event_payload_hex
                .borrow_mut()
                .push_back(hex::encode(raw.as_bytes()));
        }
    }
}

#[cfg(all(test, target_arch = "wasm32"))]
mod tests {
    use super::*;

    #[test]
    fn register_descriptor_tracks_active_peer_index() {
        let engine = WasmLdkPeerEngine::new("scope-1".to_string()).expect("engine");
        engine.ensure_initialized();
        let id = engine
            .register_descriptor("sess-1", "02aa", "127.0.0.1:9735")
            .expect("register");
        assert!(id > 0);
        assert_eq!(engine.status().active_descriptors, 1);

        engine
            .mark_disconnected("02aa", Some("test disconnect"))
            .expect("disconnect");
        assert_eq!(engine.status().active_descriptors, 0);
    }

    #[test]
    fn read_event_requires_active_descriptor() {
        let engine = WasmLdkPeerEngine::new("scope-2".to_string()).expect("engine");
        let err = engine
            .read_event("00")
            .expect_err("expected missing descriptor error");
        assert!(
            err.as_string()
                .unwrap_or_default()
                .contains("no active peer descriptor")
        );
    }

    #[test]
    fn runtime_transport_events_are_enqueued_and_drained() {
        let engine = WasmLdkPeerEngine::new("scope-3".to_string()).expect("engine");
        engine
            .register_descriptor("sess-3", "02bb", "127.0.0.1:9736")
            .expect("register");
        engine
            .mark_disconnected("02bb", Some("test disconnected"))
            .expect("disconnect");

        let drained = engine.drain_runtime_event_payload_hex();
        assert_eq!(drained.len(), 2);
        let raw0 = hex::decode(drained[0].trim()).expect("hex decode #1");
        let raw1 = hex::decode(drained[1].trim()).expect("hex decode #2");
        let evt0: serde_json::Value = serde_json::from_slice(&raw0).expect("json #1");
        let evt1: serde_json::Value = serde_json::from_slice(&raw1).expect("json #2");
        assert_eq!(evt0["event"], "peer_reconnected");
        assert_eq!(evt0["peer_pubkey"], "02bb");
        assert_eq!(evt1["event"], "peer_disconnected");
        assert_eq!(evt1["peer_pubkey"], "02bb");

        let empty = engine.drain_runtime_event_payload_hex();
        assert!(empty.is_empty());
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
