use super::*;

#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
pub(crate) fn reset_runtime_event_log_storage_for_tests() {
    RUNTIME_EVENT_LOG_STORAGE.with(|state| {
        state.borrow_mut().clear();
    });
    RUNTIME_RGB_LN_TRANSFER_STORAGE.with(|state| {
        state.borrow_mut().clear();
    });
    RUNTIME_PEER_SESSION_STORAGE.with(|state| {
        state.borrow_mut().clear();
    });
    TRUSTED_VIRTUAL_CHANNEL_SCOPE_STORAGE.with(|state| {
        state.borrow_mut().clear();
    });
    TRUSTED_VIRTUAL_PEER_LINK_STORAGE.with(|state| {
        state.borrow_mut().clear();
    });
    TRUSTED_VIRTUAL_AUTHORITATIVE_SETTLEMENT_STORAGE.with(|state| {
        state.borrow_mut().clear();
    });
    NODE_PUBKEY_RUNTIME_SCOPE_INDEX.with(|state| {
        state.borrow_mut().clear();
    });
    KNOWN_RUNTIME_SCOPE_KEYS.with(|state| {
        state.borrow_mut().clear();
    });
}

impl RlnWasmNode {
    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    pub(crate) fn test_upsert_runtime_peer(
        &self,
        pubkey: String,
        peer_addr: String,
        started: bool,
    ) {
        self.ldk_runtime.upsert_peer(LdkRuntimePeerStateData {
            pubkey,
            peer_addr,
            started,
        });
    }

    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    pub(crate) fn test_set_runtime_peer_started(&self, pubkey: &str, started: bool) -> bool {
        self.ldk_runtime.set_peer_started(pubkey, started)
    }
}
