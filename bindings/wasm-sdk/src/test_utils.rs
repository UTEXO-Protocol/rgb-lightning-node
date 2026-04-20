use super::*;

#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
fn reset_wasm_sdk_lifecycle_state_for_tests() {
    WASM_SDK_LIFECYCLE_STATE.with(|state| {
        let next = WasmSdkLifecycleState::default();
        *state.borrow_mut() = next.clone();
        sync_runtime_session_authority_from_lifecycle(&next);
    });
}

#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
fn reset_wasm_media_store_for_tests() {
    WASM_MEDIA_STORE.with(|store| {
        store.borrow_mut().clear();
    });
    clear_wasm_media_storage();
    WASM_SDK_DEFAULT_WALLET.with(|slot| {
        *slot.borrow_mut() = None;
    });
}

#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
pub(crate) fn reset_wasm_runtime_state_for_tests() {
    reset_wasm_sdk_lifecycle_state_for_tests();
    crate::ldk_runtime::reset_scaffold_runtime_storage_for_tests();
    crate::ln_node::reset_runtime_event_log_storage_for_tests();
    crate::swap_runtime::reset_swap_runtime_state_for_tests();
    reset_wasm_media_store_for_tests();
}

#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
pub(crate) fn test_wallet_data_json() -> String {
    let mnemonic =
        "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
    let keys = rgb_lib_wasm::restore_keys(rgb_lib_wasm::BitcoinNetwork::Regtest, mnemonic.into())
        .expect("restore keys");
    let wallet_data = serde_json::json!({
        "data_dir": "/tmp/rln_wasm_contract_wallet",
        "bitcoin_network": "Regtest",
        "database_type": "Sqlite",
        "max_allocations_per_utxo": 5,
        "account_xpub_vanilla": keys.account_xpub_vanilla,
        "account_xpub_colored": keys.account_xpub_colored,
        "mnemonic": keys.mnemonic,
        "master_fingerprint": keys.master_fingerprint,
        "vanilla_keychain": serde_json::Value::Null,
        "supported_schemas": ["Nia", "Ifa"],
    });
    wallet_data.to_string()
}
