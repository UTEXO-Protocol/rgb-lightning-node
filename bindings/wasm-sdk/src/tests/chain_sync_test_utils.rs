use super::CHAIN_SYNC_FALLBACK_STORAGE;

pub(crate) fn reset_chain_sync_storage_for_tests() {
    CHAIN_SYNC_FALLBACK_STORAGE.with(|storage| storage.borrow_mut().clear());
}
