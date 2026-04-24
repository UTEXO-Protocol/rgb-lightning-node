use super::{storage_keys, NativeLnRuntimeCoreSnapshot, NATIVE_RUNTIME_CORE_MEMORY_SNAPSHOTS};

pub(crate) fn reset_native_runtime_core_state_for_tests() {
    NATIVE_RUNTIME_CORE_MEMORY_SNAPSHOTS.with(|state| state.borrow_mut().clear());
}

pub(crate) fn inject_for_test(
    base: &str,
    pending: Option<NativeLnRuntimeCoreSnapshot>,
    committed: Option<NativeLnRuntimeCoreSnapshot>,
) {
    let (pending_key, committed_key) = storage_keys(base);
    NATIVE_RUNTIME_CORE_MEMORY_SNAPSHOTS.with(|state| {
        let mut state = state.borrow_mut();
        state.remove(&pending_key);
        state.remove(&committed_key);
        if let Some(p) = pending {
            state.insert(pending_key, p);
        }
        if let Some(c) = committed {
            state.insert(committed_key, c);
        }
    });
}
