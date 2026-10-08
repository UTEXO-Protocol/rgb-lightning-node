use super::*;
use crate::wasm_node_persistence::RuntimeScopeKeys;
use crate::{RlnWasmNode, RlnWasmSdk, RlnWasmWallet};
use std::collections::BTreeMap;
use wasm_bindgen_test::wasm_bindgen_test;

#[derive(Debug, PartialEq)]
struct StoredState {
    local: BTreeMap<String, String>,
    durable: BTreeMap<String, String>,
}

async fn scoped_state(scope: &str) -> Result<StoredState, JsValue> {
    let storage = web_sys::window().unwrap().local_storage()?.unwrap();
    let mut local = BTreeMap::new();
    for index in 0..storage.length()? {
        if let Some(key) = storage.key(index)? {
            if key.contains(scope) {
                if let Some(value) = storage.get_item(&key)? {
                    local.insert(key, value);
                }
            }
        }
    }
    let mut durable = BTreeMap::new();
    for entry in indexed_db_list_entries().await? {
        let pair = Array::from(&entry);
        if let (Some(key), Some(value)) = (pair.get(0).as_string(), pair.get(1).as_string()) {
            if key.contains(scope) {
                durable.insert(key, value);
            }
        }
    }
    Ok(StoredState { local, durable })
}

async fn remove_fixture(scope: &str) -> Result<(), JsValue> {
    let state = scoped_state(scope).await?;
    for key in state.local.keys().chain(state.durable.keys()) {
        local_storage_remove_item(key)?;
        indexed_db_delete_item(key).await?;
    }
    Ok(())
}

async fn seed_legacy_records(
    keys: &RuntimeScopeKeys,
    saved_running: bool,
) -> Result<StoredState, JsValue> {
    // Capture actual serialized empty/stopped driver state, as older wallet-only starts could
    // persist it. Construct this fixture before measuring production lifecycle startup calls.
    let driver = crate::chain_sync::WasmChainSyncDriver::new(
        keys.ldk_manager_registry_key.clone(),
        "mainnet".into(),
    )?;
    let stopped = local_storage_get_item(&keys.chain_sync_storage_key)?.unwrap();
    drop(driver);
    indexed_db_set_item(&keys.chain_sync_storage_key, &stopped).await?;
    let saved_chain = if saved_running {
        // Use the same real snapshot schema without ever starting its timer or indexer.
        let mut running: serde_json::Value = serde_json::from_str(&stopped).unwrap();
        running["running"] = serde_json::json!(true);
        running["indexer_url"] = serde_json::json!("https://must-not-resume.invalid");
        running.to_string()
    } else {
        stopped
    };

    let protected = [
        keys.chain_sync_storage_key.clone(),
        keys.ldk_runtime_committed_storage_key.clone(),
        keys.ldk_runtime_pending_storage_key.clone(),
        format!("{}:channel-manager", keys.ldk_runtime_committed_storage_key),
        format!(
            "{}:channel-manager:pending",
            keys.ldk_runtime_committed_storage_key
        ),
        format!("{}:network-graph", keys.ldk_runtime_committed_storage_key),
        format!(
            "{}:network-graph:pending",
            keys.ldk_runtime_committed_storage_key
        ),
        format!("{}:scorer", keys.ldk_runtime_committed_storage_key),
        format!("{}:scorer:pending", keys.ldk_runtime_committed_storage_key),
        format!("{}:committed", keys.native_ln_runtime_core_storage_base),
        format!("{}:pending", keys.native_ln_runtime_core_storage_base),
        keys.runtime_events_storage_key.clone(),
        keys.rgb_ln_transfers_storage_key.clone(),
        keys.peer_sessions_storage_key.clone(),
        format!("rln:wasm:ldk-monitors:{}", keys.ldk_manager_registry_key),
        format!(
            "rln:wasm:ldk-monitors:{}:pending",
            keys.ldk_manager_registry_key
        ),
        format!(
            "rln:wasm:ldk-monitors:{}:index",
            keys.ldk_manager_registry_key
        ),
        format!(
            "rln:wasm:ldk-monitors:{}:monitor:old-channel",
            keys.ldk_manager_registry_key
        ),
        format!(
            "rln:wasm:ldk-broadcast-queue:{}",
            keys.ldk_manager_registry_key
        ),
        format!("rln:wasm:ldk-sweeps:{}", keys.ldk_manager_registry_key),
        format!("rln:ldk-kv:{}:::manager", keys.ldk_manager_registry_key),
        format!(
            "rln:ldk-kv:{}:monitors::old-channel",
            keys.ldk_manager_registry_key
        ),
        format!("rln:wasm:swap-runtime:{}", keys.runtime_scope_key),
    ];
    for (index, key) in protected.iter().enumerate() {
        // Divergent caches, durable-only and cache-only records all need preservation. Invalid
        // JSON is deliberate: successful on-chain lifecycle must not decode these payloads.
        let local = if index == 0 {
            let mut divergent: serde_json::Value = serde_json::from_str(&saved_chain).unwrap();
            divergent["running"] = serde_json::json!(!saved_running);
            divergent["indexer_url"] = serde_json::json!("https://must-not-resume.invalid");
            divergent["latest_tip_height"] = serde_json::json!(42);
            divergent.to_string()
        } else {
            format!("opaque local record {index}: {{\u{0}unfinished")
        };
        let durable = if index == 0 {
            saved_chain.clone()
        } else {
            format!("opaque durable record {index}: [\u{0}unfinished")
        };
        match index % 3 {
            0 => {
                local_storage_set_item(key, &local)?;
                indexed_db_set_item(key, &durable).await?;
            }
            1 => {
                local_storage_remove_item(key)?;
                indexed_db_set_item(key, &durable).await?;
            }
            _ => {
                local_storage_set_item(key, &local)?;
                indexed_db_delete_item(key).await?;
            }
        }
    }
    scoped_state(&keys.runtime_scope_key).await
}

fn mainnet_wallet_data() -> serde_json::Value {
    let mut data: serde_json::Value =
        serde_json::from_str(&crate::test_utils::test_wallet_data_json()).unwrap();
    let keys = rgb_lib_wasm::restore_keys(
        rgb_lib_wasm::BitcoinNetwork::Mainnet,
        data["mnemonic"].as_str().unwrap().to_owned(),
    )
    .unwrap();
    data["bitcoin_network"] = serde_json::json!("Mainnet");
    data["supported_schemas"] = serde_json::json!(["Nia"]);
    data["account_xpub_vanilla"] = serde_json::json!(keys.account_xpub_vanilla);
    data["account_xpub_colored"] = serde_json::json!(keys.account_xpub_colored);
    data
}

async fn exercise_wallet_lifecycle(
    proxy: &str,
    runtime_id: &str,
    explicit: bool,
    scope: &str,
) -> Result<Vec<StoredState>, JsValue> {
    let sdk = RlnWasmSdk::new();
    let wallet_data = mainnet_wallet_data();
    sdk.init_json(
        "preservation-password".into(),
        Some(wallet_data["mnemonic"].as_str().unwrap().into()),
    )
    .await?;
    let mut observed = vec![scoped_state(scope).await?];
    for _ in 0..2 {
        sdk.unlock(r#"{"password":"preservation-password"}"#.into())
            .await?;
        sdk.preload_persistent_runtime_state().await?;
        observed.push(scoped_state(scope).await?);
        let wallet = RlnWasmWallet::create(&wallet_data.to_string()).await?;
        let node = RlnWasmNode::new_with_runtime_id_opt(
            proxy.into(),
            Some(runtime_id.into()),
            explicit.then_some(crate::WasmRlnNetwork::Mainnet),
        )?;
        // Bare construction precedes network adoption: it must not restore old Lightning views.
        observed.push(scoped_state(scope).await?);
        node.attach_wallet(&wallet)?;
        if !wallet.get_address()?.starts_with("bc1") {
            return Err(JsValue::from_str(
                "mainnet wallet did not return a bc1 address",
            ));
        }
        node.sign_message_json("inactive legacy wallet".into())?;
        node.node_pubkey_json()?;
        node.chain_sync_status_json()?;
        node.ldk_runtime_status_json()?;
        node.native_runtime_core_status_json()?;
        let unavailable = node.network_info_json().err().and_then(|e| e.as_string());
        if !unavailable.is_some_and(|e| e.starts_with("NetworkInfoUnavailable:")) {
            return Err(JsValue::from_str(
                "mainnet must not expose an old Lightning chain tip",
            ));
        }
        let rejected = node.list_channels_json().err().and_then(|e| e.as_string());
        if rejected.as_deref() != Some("LightningUnsupportedOnMainnet: RLN on mainnet currently supports only on-chain methods. Lightning APIs are not supported.") {
            return Err(JsValue::from_str("mainnet Lightning call did not retain its error"));
        }
        let shared = RlnWasmNode::new_with_node_runtime_id(
            proxy.into(),
            runtime_id.into(),
            "mainnet".into(),
        )?;
        shared.node_pubkey_json()?;
        shared.chain_sync_status_json()?;
        observed.push(scoped_state(scope).await?);
        sdk.lock().await?;
        drop(shared);
        drop(node);
        drop(wallet);
        // Model another page session reading durable storage again after all node handles drop.
        reset_preload_readiness_for_tests();
        sdk.preload_persistent_runtime_state().await?;
        observed.push(scoped_state(scope).await?);
    }
    Ok(observed)
}

#[wasm_bindgen_test(async)]
async fn mainnet_sdk_lifecycle_preserves_divergent_and_asymmetric_legacy_storage() {
    for explicit in [true, false] {
        crate::test_utils::reset_wasm_runtime_state_for_tests();
        crate::peer_session::clear_rln_ldk_peer_manager_hooks();
        let proxy = "ws://mainnet-preservation-lifecycle.invalid";
        let runtime_id = if explicit { "explicit" } else { "adopted" };
        let keys =
            RuntimeScopeKeys::from_runtime_scope_key(format!("{proxy}#runtime:{runtime_id}"));
        remove_fixture(&keys.runtime_scope_key).await.unwrap();
        let expected = seed_legacy_records(&keys, !explicit).await.unwrap();
        reset_preload_readiness_for_tests();
        let before = crate::ln_node::test_utils::startup_calls();
        let result =
            exercise_wallet_lifecycle(proxy, runtime_id, explicit, &keys.runtime_scope_key).await;
        let after = crate::ln_node::test_utils::startup_calls();
        let hooks = crate::peer_session::has_peer_manager_hooks();
        // Remove every scoped record, including any unexpected writes, before assertions so a
        // failure cannot poison later tests or be rehydrated by another SDK initialization.
        remove_fixture(&keys.runtime_scope_key).await.unwrap();
        reset_preload_readiness_for_tests();
        crate::test_utils::reset_wasm_runtime_state_for_tests();
        for observed in result.expect("mainnet on-chain lifecycle with inactive legacy state") {
            assert_eq!(observed, expected, "explicit mainnet = {explicit}");
        }
        assert_eq!(after, before, "no Lightning factories may run");
        assert!(!hooks, "no global peer hooks may be installed");
    }
}
