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
    fn test_lightning_runtime(&self) -> Rc<NodeLightningRuntime> {
        self.ensure_runtime_ready().expect("test Lightning runtime");
        self.lightning_runtime().unwrap()
    }

    pub(super) fn test_ldk(&self) -> Rc<dyn LdkRuntimeManager> {
        Rc::clone(&self.test_lightning_runtime().ldk_runtime)
    }

    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    pub(crate) fn test_upsert_runtime_peer(
        &self,
        pubkey: String,
        peer_addr: String,
        started: bool,
    ) {
        self.test_lightning_runtime()
            .ldk_runtime
            .upsert_peer(LdkRuntimePeerStateData {
                pubkey,
                peer_addr,
                started,
            });
    }

    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    pub(crate) fn test_set_runtime_peer_started(&self, pubkey: &str, started: bool) -> bool {
        self.test_lightning_runtime()
            .ldk_runtime
            .set_peer_started(pubkey, started)
    }
}

thread_local! {
    static STARTUP_CALLS: RefCell<HashMap<&'static str, usize>> = RefCell::new(HashMap::new());
}

pub(crate) fn record_startup_call(component: &'static str) {
    STARTUP_CALLS.with(|calls| *calls.borrow_mut().entry(component).or_default() += 1);
}

pub(crate) fn startup_calls() -> HashMap<&'static str, usize> {
    STARTUP_CALLS.with(|calls| calls.borrow().clone())
}
