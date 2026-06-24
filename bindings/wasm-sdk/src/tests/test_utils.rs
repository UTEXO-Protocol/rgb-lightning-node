use super::*;
use std::sync::atomic::{AtomicU64, Ordering};

#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
fn reset_wasm_sdk_lifecycle_state_for_tests() {
    WASM_SDK_LIFECYCLE_STATE.with(|state| {
        let next = WasmSdkLifecycleState::default();
        *state.borrow_mut() = next.clone();
        sync_runtime_session_authority_from_lifecycle(&next);
    });
}

/// Puts the SDK lifecycle into an unlocked state with a deterministic test mnemonic and bootstraps
/// a default RGB wallet, mirroring the production `unlock` flow. This satisfies both prerequisites
/// `open_channel` enforces: a stable identity (`ensure_stable_identity_for_channel_operations`) and
/// an attached RGB wallet (auto-attached from `WASM_SDK_DEFAULT_WALLET`). Pair this with a node
/// constructed with an explicit `node_runtime_id` (the `*_and_runtime_id` / `*_and_id` constructors)
/// to complete the stable-runtime-id half of the identity guard.
#[cfg(feature = "wasm-browser-infra")]
pub(crate) fn set_wasm_sdk_identity_unlocked_for_tests() {
    WASM_SDK_LIFECYCLE_STATE.with(|state| {
        let next = WasmSdkLifecycleState::Unlocked {
            password: "test-password".to_string(),
            mnemonic:
                "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about"
                    .to_string(),
        };
        *state.borrow_mut() = next.clone();
        sync_runtime_session_authority_from_lifecycle(&next);
    });

    let wallet =
        crate::RlnWasmWallet::new(&test_wallet_data_json()).expect("test default rgb wallet");
    WASM_SDK_DEFAULT_WALLET.with(|slot| {
        *slot.borrow_mut() = Some(std::rc::Rc::clone(&wallet.inner));
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
    WASM_SDK_DEFAULT_RGB_PROXY_TRANSPORT.with(|slot| {
        slot.borrow_mut().take();
    });
    WASM_WALLET_RGB_PROXY_TRANSPORTS.with(|store| {
        store.borrow_mut().clear();
    });
}

#[cfg(target_arch = "wasm32")]
fn clear_wasm_media_storage() {
    let Some(window) = web_sys::window() else {
        return;
    };
    let Ok(Some(storage)) = window.local_storage() else {
        return;
    };
    let mut keys = Vec::new();
    let len = storage.length().unwrap_or(0);
    for idx in 0..len {
        if let Ok(Some(key)) = storage.key(idx) {
            if key.starts_with(MEDIA_STORAGE_PREFIX)
                || key.starts_with(WALLET_RGB_PROXY_STORAGE_PREFIX)
            {
                keys.push(key);
            }
        }
    }
    for key in keys {
        let _ = storage.remove_item(&key);
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn clear_wasm_media_storage() {}

#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
pub(crate) fn reset_wasm_runtime_state_for_tests() {
    reset_wasm_sdk_lifecycle_state_for_tests();
    crate::set_sdk_default_enable_virtual_channels_v0(false);
    crate::ldk_runtime::test_utils::reset_runtime_storage_for_tests();
    crate::ln_node::test_utils::reset_runtime_event_log_storage_for_tests();
    crate::chain_sync::test_utils::reset_chain_sync_storage_for_tests();
    crate::swap_runtime::test_utils::reset_swap_runtime_state_for_tests();
    reset_wasm_media_store_for_tests();
}

#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
pub(crate) fn test_wallet_data_json() -> String {
    static TEST_WALLET_SEQ: AtomicU64 = AtomicU64::new(0);
    let mnemonic =
        "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
    let keys = rgb_lib_wasm::restore_keys(rgb_lib_wasm::BitcoinNetwork::Regtest, mnemonic.into())
        .expect("restore keys");
    let suffix = TEST_WALLET_SEQ.fetch_add(1, Ordering::Relaxed);
    let wallet_data = serde_json::json!({
        "data_dir": crate::wasm_runtime_paths::rgb_lib_wallet_data_dir(&format!("contract{suffix}")),
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
