use super::*;

pub fn reset_scaffold_runtime_storage_for_tests() {
    SCAFFOLD_RUNTIME_STORAGE.with(|storage| storage.borrow_mut().clear());
    RUNTIME_SESSION_AUTHORITY_STATE.with(|state| {
        *state.borrow_mut() = RuntimeSessionAuthorityState::default();
    });
}
