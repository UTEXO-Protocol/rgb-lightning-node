use super::*;

pub(crate) fn reset_runtime_storage_for_tests() {
    FALLBACK_RUNTIME_STORAGE.with(|storage| storage.borrow_mut().clear());
    RUNTIME_SESSION_AUTHORITY_STATE.with(|state| {
        *state.borrow_mut() = RuntimeSessionAuthorityState::default();
    });
}
