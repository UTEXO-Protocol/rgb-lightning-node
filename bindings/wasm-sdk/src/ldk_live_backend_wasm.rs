use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet, VecDeque};
use std::rc::Rc;

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

pub struct WasmLdkLiveBackend {
    _runtime_key: String,
    connected_peers: RefCell<HashSet<String>>,
    active_peer_pubkey: RefCell<Option<String>>,
    inbound_frames: RefCell<VecDeque<String>>,
    outbound_frames: RefCell<VecDeque<String>>,
    disconnected: Cell<bool>,
    pending_funding_requests: RefCell<HashMap<String, LdkRuntimeFundingRequestData>>,
}

impl WasmLdkLiveBackend {
    fn new(runtime_key: String) -> Self {
        Self {
            _runtime_key: runtime_key,
            connected_peers: RefCell::new(HashSet::new()),
            active_peer_pubkey: RefCell::new(None),
            inbound_frames: RefCell::new(VecDeque::new()),
            outbound_frames: RefCell::new(VecDeque::new()),
            disconnected: Cell::new(false),
            pending_funding_requests: RefCell::new(HashMap::new()),
        }
    }
}

impl LdkLiveBackend for WasmLdkLiveBackend {
    fn new_outbound_connection(&self, peer_pubkey: &str) -> Result<String, JsValue> {
        let peer_pubkey = peer_pubkey.trim();
        if peer_pubkey.is_empty() {
            return Err(JsValue::from_str(sdk_contracts::ERR_PEER_PUBKEY_INVALID));
        }
        let bytes = hex::decode(peer_pubkey)
            .map_err(|_| JsValue::from_str(sdk_contracts::ERR_PEER_PUBKEY_INVALID))?;
        SecpPublicKey::from_slice(&bytes)
            .map_err(|_| JsValue::from_str(sdk_contracts::ERR_PEER_PUBKEY_INVALID))?;

        self.disconnected.set(false);
        self.connected_peers.borrow_mut().insert(peer_pubkey.to_string());
        self.active_peer_pubkey
            .borrow_mut()
            .replace(peer_pubkey.to_string());

        Ok(String::new())
    }

    fn read_event(&self, payload_hex: &str) -> Result<(), JsValue> {
        if self.disconnected.get() {
            return Err(JsValue::from_str(sdk_contracts::ERR_PEER_TRANSPORT_DISCONNECTED));
        }
        let payload_hex = payload_hex.trim();
        if payload_hex.is_empty() {
            return Err(JsValue::from_str(sdk_contracts::ERR_PAYLOAD_HEX_EMPTY));
        }
        let _ = hex::decode(payload_hex)
            .map_err(|e| JsValue::from_str(&format!("invalid payload_hex: {e}")))?;
        self.inbound_frames
            .borrow_mut()
            .push_back(payload_hex.to_string());
        Ok(())
    }

    fn process_events(&self) -> Result<(), JsValue> {
        if self.disconnected.get() {
            return Err(JsValue::from_str(sdk_contracts::ERR_PEER_TRANSPORT_DISCONNECTED));
        }
        Ok(())
    }

    fn take_outbound_frames(&self) -> Result<Vec<String>, JsValue> {
        if self.disconnected.get() {
            return Ok(Vec::new());
        }
        let mut q = self.outbound_frames.borrow_mut();
        let mut out = Vec::new();
        while let Some(frame) = q.pop_front() {
            out.push(frame);
        }
        Ok(out)
    }

    fn socket_disconnected(&self) -> Result<(), JsValue> {
        self.disconnected.set(true);
        self.active_peer_pubkey.borrow_mut().take();
        self.inbound_frames.borrow_mut().clear();
        self.outbound_frames.borrow_mut().clear();
        Ok(())
    }

    fn is_peer_handshake_complete(&self, peer_pubkey: &str) -> Result<bool, JsValue> {
        Ok(self.connected_peers.borrow().contains(peer_pubkey.trim()))
    }

    fn local_node_pubkey(&self) -> Result<String, JsValue> {
        Err(JsValue::from_str(
            "local_node_pubkey requires non-wasm live LDK backend build profile",
        ))
    }

    fn open_channel_non_virtual(
        &self,
        _request: LdkRuntimeOpenChannelRequestData,
    ) -> Result<LdkRuntimeOpenChannelResultData, JsValue> {
        Err(JsValue::from_str(
            "open_channel_non_virtual requires non-wasm live LDK backend build profile",
        ))
    }

    fn list_pending_funding_requests(&self) -> Result<Vec<LdkRuntimeFundingRequestData>, JsValue> {
        Ok(self.pending_funding_requests.borrow().values().cloned().collect())
    }

    fn submit_funding_transaction(
        &self,
        _request: LdkRuntimeFundingTxSubmissionData,
    ) -> Result<(), JsValue> {
        Err(JsValue::from_str(
            "submit_funding_transaction requires non-wasm live LDK backend build profile",
        ))
    }

    fn list_live_channels(&self) -> Result<Vec<LdkRuntimeOpenChannelResultData>, JsValue> {
        Ok(Vec::new())
    }
}

pub fn create_wasm_ldk_live_backend(
    runtime_key: String,
    _node_seed32: Option<[u8; 32]>,
) -> Result<Rc<dyn LdkLiveBackend>, JsValue> {
    Ok(Rc::new(WasmLdkLiveBackend::new(runtime_key)))
}
