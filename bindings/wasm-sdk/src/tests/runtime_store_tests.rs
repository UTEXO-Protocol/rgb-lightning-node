use super::RUNTIME_STATE_HYDRATE_PREFIXES;

#[test]
fn hydrate_prefixes_cover_runtime_state_domains() {
    let must_include = [
        crate::wasm_node_persistence::WASM_LDK_RUNTIME_STORAGE_PREFIX,
        "rln:wasm:swap-runtime:",
        "rln:wasm:media:",
        "rln:wasm:wallet-rgb-proxy:",
        crate::wasm_node_persistence::WASM_LN_RUNTIME_CORE_STORAGE_PREFIX,
        crate::wasm_node_persistence::WASM_CHAIN_SYNC_STORAGE_PREFIX,
        crate::wasm_node_persistence::WASM_LDK_BROADCAST_QUEUE_STORAGE_PREFIX,
        crate::wasm_node_persistence::WASM_LDK_MONITORS_STORAGE_PREFIX,
        crate::wasm_node_persistence::WASM_RUNTIME_EVENTS_STORAGE_PREFIX,
        crate::wasm_node_persistence::WASM_RGB_LN_TRANSFERS_STORAGE_PREFIX,
        crate::wasm_node_persistence::WASM_VIRTUAL_CHANNELS_V0_STORAGE_PREFIX,
        crate::wasm_node_persistence::WASM_PEER_SESSIONS_STORAGE_PREFIX,
    ];
    assert_eq!(RUNTIME_STATE_HYDRATE_PREFIXES, must_include);
    for prefix in must_include {
        assert!(
            RUNTIME_STATE_HYDRATE_PREFIXES.contains(&prefix),
            "missing hydrate prefix: {prefix}"
        );
    }
}

#[cfg(target_arch = "wasm32")]
mod browser {
    use super::super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    fn entries(values: &[(&str, &str)]) -> Vec<JsValue> {
        values
            .iter()
            .map(|(key, value)| {
                let pair = Array::new();
                pair.push(&JsValue::from_str(key));
                pair.push(&JsValue::from_str(value));
                pair.into()
            })
            .collect()
    }

    #[wasm_bindgen_test(async)]
    async fn preload_preserves_inactive_lightning_cache_and_durable_bytes() {
        let key = "rln:wasm:chain-sync:node-runtime:preserved-upgrade";
        let storage = web_sys::window().unwrap().local_storage().unwrap().unwrap();
        indexed_db_set_item(key, "durable Lightning bytes")
            .await
            .unwrap();
        storage
            .set_item(key, "different local Lightning bytes")
            .unwrap();
        reset_preload_readiness_for_tests();
        preload_runtime_state_from_persistent_store().await.unwrap();
        let actual = storage.get_item(key).unwrap();
        let durable = indexed_db_list_entries().await.unwrap();
        storage.remove_item(key).unwrap();
        indexed_db_delete_item(key).await.unwrap();
        assert_eq!(actual.as_deref(), Some("different local Lightning bytes"));
        assert!(durable.iter().any(|entry| {
            let pair = Array::from(entry);
            pair.get(0).as_string().as_deref() == Some(key)
                && pair.get(1).as_string().as_deref() == Some("durable Lightning bytes")
        }));
    }

    #[wasm_bindgen_test]
    fn preload_defers_lightning_per_key_and_retains_common_best_effort_copy() {
        reset_preload_readiness_for_tests();
        let storage = web_sys::window().unwrap().local_storage().unwrap().unwrap();
        let supported = "rln:wasm:runtime-events:supported-hydration";
        let mainnet = "rln:wasm:runtime-events:mainnet-hydration";
        let swap = "rln:wasm:swap-runtime:inactive-hydration";
        let common = "rln:wasm:media:common-hydration";
        for key in [supported, mainnet, swap] {
            storage.set_item(key, "local").unwrap();
        }
        let mut copies = Vec::new();
        finish_preload(
            entries(&[
                (supported, "durable"),
                (mainnet, "durable"),
                (swap, "durable"),
                (common, "media"),
            ]),
            RUNTIME_STATE_HYDRATE_PREFIXES,
            0,
            |key, _| {
                copies.push(key.to_string());
                Err(JsValue::from_str("simulated quota error"))
            },
        );
        assert_eq!(copies, [common]);
        assert_eq!(
            storage.get_item(supported).unwrap().as_deref(),
            Some("local")
        );
        assert_eq!(
            browser_persistent_state_store()
                .get(supported)
                .unwrap()
                .as_deref(),
            Some("durable")
        );
        assert_eq!(storage.get_item(mainnet).unwrap().as_deref(), Some("local"));
        assert_eq!(storage.get_item(swap).unwrap().as_deref(), Some("local"));
        storage.set_item(supported, "new local").unwrap();
        assert_eq!(
            browser_persistent_state_store()
                .get(supported)
                .unwrap()
                .as_deref(),
            Some("new local")
        );
        finish_preload(
            entries(&[(supported, "obsolete")]),
            RUNTIME_STATE_HYDRATE_PREFIXES,
            0,
            |_, _| panic!("one-shot preload"),
        );
        assert_eq!(
            browser_persistent_state_store()
                .get(supported)
                .unwrap()
                .as_deref(),
            Some("new local")
        );
        for key in [supported, mainnet, swap] {
            storage.remove_item(key).unwrap();
        }
        reset_preload_readiness_for_tests();
    }

    #[wasm_bindgen_test(async)]
    async fn deferred_hydration_never_resurrects_written_or_deleted_values() {
        reset_preload_readiness_for_tests();
        let changed = "rln:wasm:ldk-runtime:deferred-write";
        let deleted = "rln:wasm:ldk-runtime:deferred-delete";
        finish_preload(
            entries(&[(changed, "old"), (deleted, "old")]),
            RUNTIME_STATE_HYDRATE_PREFIXES,
            0,
            |_, _| Ok(()),
        );
        let store = browser_persistent_state_store();
        store.set(changed, "new").unwrap();
        store.delete(deleted).unwrap();
        assert_eq!(store.get(changed).unwrap().as_deref(), Some("new"));
        assert_eq!(store.get(deleted).unwrap(), None);
        // Drain prior background mutations before removing the fixture from both stores.
        indexed_db_set_item(changed, "new").await.unwrap();
        indexed_db_delete_item(changed).await.unwrap();
        indexed_db_delete_item(deleted).await.unwrap();
        local_storage_remove_item(changed).unwrap();
    }

    #[wasm_bindgen_test(async)]
    async fn mutations_during_overlapping_preloads_cannot_stage_stale_values() {
        reset_preload_readiness_for_tests();
        let changed = "rln:wasm:ldk-runtime:inflight-write";
        let deleted = "rln:wasm:ldk-runtime:inflight-delete";
        let read = "rln:wasm:ldk-runtime:inflight-read";
        let durable_write = "rln:wasm:ldk-runtime:inflight-durable-write";
        let durable_delete = "rln:wasm:ldk-runtime:inflight-durable-delete";
        let first = PreloadRead::begin();
        let second = PreloadRead::begin();
        let store = browser_persistent_state_store();
        store.set(changed, "current").unwrap();
        store.delete(deleted).unwrap();
        local_storage_set_item(read, "already restored").unwrap();
        assert_eq!(
            store.get(read).unwrap().as_deref(),
            Some("already restored")
        );
        local_storage_set_item(durable_write, "current cache").unwrap();
        local_storage_remove_item(durable_delete).unwrap();
        indexed_db_set_durable(durable_write.into(), "committed".into())
            .await
            .unwrap();
        indexed_db_delete_item(durable_delete).await.unwrap();
        let snapshot = || {
            entries(&[
                (changed, "old"),
                (deleted, "old"),
                (read, "old"),
                (durable_write, "old"),
                (durable_delete, "old"),
            ])
        };
        finish_preload(
            snapshot(),
            RUNTIME_STATE_HYDRATE_PREFIXES,
            first.revision,
            |_, _| Ok(()),
        );
        drop(first);
        finish_preload(
            snapshot(),
            RUNTIME_STATE_HYDRATE_PREFIXES,
            second.revision,
            |_, _| panic!("late preload must not republish"),
        );
        drop(second);
        assert_eq!(store.get(changed).unwrap().as_deref(), Some("current"));
        assert_eq!(store.get(deleted).unwrap(), None);
        assert_eq!(
            store.get(read).unwrap().as_deref(),
            Some("already restored")
        );
        assert_eq!(
            store.get(durable_write).unwrap().as_deref(),
            Some("current cache")
        );
        assert_eq!(store.get(durable_delete).unwrap(), None);
        DEFERRED_RUNTIME_STATE.with(|state| {
            assert!(state.borrow().touched.is_empty());
            assert_eq!(state.borrow().reads_in_flight, 0);
        });
        indexed_db_set_item(changed, "current").await.unwrap();
        for key in [changed, deleted, read, durable_write, durable_delete] {
            indexed_db_delete_item(key).await.unwrap();
            local_storage_remove_item(key).unwrap();
        }
    }

    #[wasm_bindgen_test]
    fn standalone_chain_driver_and_runtime_core_restore_deferred_snapshots() {
        crate::test_utils::reset_wasm_runtime_state_for_tests();
        reset_preload_readiness_for_tests();
        let key = "standalone-deferred";
        let chain_key = format!("rln:wasm:chain-sync:{key}");
        let core_key = format!("rln:wasm:ln-runtime-core:{key}:committed");
        let chain = serde_json::json!({"schema_version":1,"network":"regtest","indexer_url":null,
            "running":false,"poll_interval_ms":1000,"latest_tip_height":42,"last_tip_at":null,
            "last_tick_at":null,"last_error":null,"rebroadcast_queue":[]})
        .to_string();
        let core = serde_json::json!({"revision":1,"schema_version":1,"lifecycle_state":"stopped",
            "storage_initialized":true,"queued_events":[{"seq":7,"event_kind":"saved","payload_hex":"00","received_at":1}],
            "next_event_seq":8}).to_string();
        local_storage_set_item(&chain_key, "invalid older cache").unwrap();
        local_storage_set_item(&core_key, "invalid older cache").unwrap();
        finish_preload(
            entries(&[(&chain_key, &chain), (&core_key, &core)]),
            RUNTIME_STATE_HYDRATE_PREFIXES,
            0,
            |_, _| Ok(()),
        );
        let driver = crate::chain_sync::WasmChainSyncDriver::new_without_resume(
            key.into(),
            "regtest".into(),
        )
        .unwrap();
        assert_eq!(driver.latest_tip_height(), Some(42));
        let restored = crate::NativeLnRuntimeCore::new(key.into());
        assert_eq!(restored.status().queued_events, 1);
        assert_eq!(restored.status().lifecycle_state, "stopped");
        assert_eq!(
            local_storage_get_item(&chain_key).unwrap().as_deref(),
            Some(chain.as_str())
        );
        local_storage_remove_item(&chain_key).unwrap();
        local_storage_remove_item(&core_key).unwrap();
    }
    #[wasm_bindgen_test(async)]
    async fn cold_supported_handles_restore_deferred_views_when_each_activates() {
        crate::test_utils::reset_wasm_runtime_state_for_tests();
        reset_preload_readiness_for_tests();
        let proxy = "ws://deferred-node-views.invalid";
        let id = "supported";
        let keys = crate::wasm_node_persistence::RuntimeScopeKeys::from_runtime_scope_key(format!(
            "{proxy}#runtime:{id}"
        ));
        let events = serde_json::json!({"events":[{"seq":7,"source":"saved","event_kind":"saved",
            "payload_hex":"00","payment_hash":null,"status":null,"applied":true,"error":null,"received_at":1}],"next_seq":8}).to_string();
        let transfers = serde_json::json!({"transfers":[{"payment_hash":"saved-payment","inbound":true,
            "asset_id":"saved-asset","asset_amount":3,"status":"succeeded","created_at":1,"updated_at":2}]}).to_string();
        local_storage_set_item(&keys.runtime_events_storage_key, "invalid old cache").unwrap();
        local_storage_set_item(&keys.rgb_ln_transfers_storage_key, "invalid old cache").unwrap();
        finish_preload(
            entries(&[
                (&keys.runtime_events_storage_key, &events),
                (&keys.rgb_ln_transfers_storage_key, &transfers),
            ]),
            RUNTIME_STATE_HYDRATE_PREFIXES,
            0,
            |_, _| Ok(()),
        );
        let first =
            crate::RlnWasmNode::new_with_runtime_id_opt(proxy.into(), Some(id.into()), None)
                .unwrap();
        let second =
            crate::RlnWasmNode::new_with_runtime_id_opt(proxy.into(), Some(id.into()), None)
                .unwrap();
        assert_eq!(first.list_runtime_events_json().unwrap(), "[]");
        assert_eq!(
            local_storage_get_item(&keys.runtime_events_storage_key)
                .unwrap()
                .as_deref(),
            Some("invalid old cache")
        );
        let wallet = crate::RlnWasmWallet::create(&crate::test_utils::test_wallet_data_json())
            .await
            .unwrap();
        let before = crate::ln_node::test_utils::startup_calls();
        first.attach_wallet(&wallet).unwrap();
        let passive_events: serde_json::Value =
            serde_json::from_str(&second.list_runtime_events_json().unwrap()).unwrap();
        assert_eq!(passive_events[0]["seq"], 7);
        assert_eq!(
            first.list_runtime_events_json().unwrap(),
            second.list_runtime_events_json().unwrap()
        );
        assert_eq!(crate::ln_node::test_utils::startup_calls(), before);
        let restored: serde_json::Value =
            serde_json::from_str(&second.list_rgb_ln_transfers_json().unwrap()).unwrap();
        assert_eq!(restored[0]["payment_hash"], "saved-payment");
        let logs: serde_json::Value =
            serde_json::from_str(&second.list_runtime_events_json().unwrap()).unwrap();
        assert_eq!(logs[0]["seq"], 7);
        assert_eq!(
            first.list_rgb_ln_transfers_json().unwrap(),
            second.list_rgb_ln_transfers_json().unwrap()
        );
        assert_eq!(
            first.list_runtime_events_json().unwrap(),
            second.list_runtime_events_json().unwrap()
        );
        drop(first);
        drop(second);
        local_storage_remove_item(&keys.runtime_events_storage_key).unwrap();
        local_storage_remove_item(&keys.rgb_ln_transfers_storage_key).unwrap();
    }
}
