use super::*;
use std::rc::Rc;

fn sample_snapshot() -> LdkRuntimeSnapshot {
    LdkRuntimeSnapshot {
        lifecycle_state: LdkRuntimeLifecycleState::Running,
        storage_initialized: true,
        schema_version: 1,
        transport_session_initialized: true,
        peers: vec![LdkRuntimePeerStateData {
            pubkey: "peer-a".to_string(),
            peer_addr: "127.0.0.1:9735".to_string(),
            started: true,
        }],
        channels: vec![LdkRuntimeChannelStateData {
            temporary_channel_id: "tmp-1".to_string(),
            channel_id: "chan-1".to_string(),
            peer_pubkey: "peer-a".to_string(),
            status: "opened".to_string(),
            ready: true,
            is_usable: true,
            public: false,
            capacity_sat: 5_506,
            asset_id: None,
            asset_local_amount: None,
            virtual_open_mode: None,
        }],
        payments: vec![LdkRuntimePaymentStateData {
            amt_msat: Some(3_000_000),
            asset_amount: None,
            asset_id: None,
            payment_hash: "pay-1".to_string(),
            inbound: false,
            status: "pending".to_string(),
            invoice_type: None,
            preimage: None,
            created_at: 1,
            updated_at: 1,
            payee_pubkey: "peer-a".to_string(),
        }],
        virtual_channel_drafts: vec![],
        virtual_channel_sessions: vec![],
        counters: LdkRuntimeLifecycleCounters::default(),
        key_manager_fingerprint: "deadbeef".to_string(),
    }
}

#[test]
fn checkpoint_envelope_roundtrip_contract() {
    let snapshot = sample_snapshot();
    let raw = BrowserBackedLdkRuntimeStorage::encode_checkpoint(&snapshot).expect("encode");
    let decoded = BrowserBackedLdkRuntimeStorage::decode_snapshot(&raw).expect("decode");
    assert_eq!(decoded.schema_version, snapshot.schema_version);
    assert_eq!(decoded.peers.len(), 1);
    assert_eq!(decoded.channels.len(), 1);
    assert_eq!(decoded.payments.len(), 1);
}

#[test]
fn checkpoint_restore_uses_pending_when_commit_missing_contract() {
    let snapshot = sample_snapshot();
    let pending_raw = BrowserBackedLdkRuntimeStorage::encode_checkpoint(&snapshot).expect("encode");
    let restored =
        BrowserBackedLdkRuntimeStorage::select_snapshot_for_restore(None, Some(pending_raw))
            .expect("restore from pending");
    assert_eq!(restored.channels[0].channel_id, "chan-1");
}

#[test]
fn checkpoint_restore_prefers_committed_over_pending_contract() {
    let mut committed = sample_snapshot();
    committed.channels[0].channel_id = "chan-committed".to_string();
    let committed_raw =
        BrowserBackedLdkRuntimeStorage::encode_checkpoint(&committed).expect("encode committed");

    let mut pending = sample_snapshot();
    pending.channels[0].channel_id = "chan-pending".to_string();
    let pending_raw =
        BrowserBackedLdkRuntimeStorage::encode_checkpoint(&pending).expect("encode pending");

    let restored = BrowserBackedLdkRuntimeStorage::select_snapshot_for_restore(
        Some(committed_raw),
        Some(pending_raw),
    )
    .expect("restore");
    assert_eq!(restored.channels[0].channel_id, "chan-committed");
}

#[test]
fn checkpoint_restore_rejects_legacy_payload_contract() {
    let snapshot = sample_snapshot();
    let legacy_raw = serde_json::to_string(&snapshot).expect("legacy encode");
    let restored =
        BrowserBackedLdkRuntimeStorage::select_snapshot_for_restore(Some(legacy_raw), None);
    assert!(restored.is_none());
}

#[test]
fn connected_peer_helpers_track_started_state_contract() {
    super::test_utils::reset_runtime_storage_for_tests();
    let manager =
        ldk_runtime_manager("runtime-connected-peer-test".to_string()).expect("runtime manager");
    manager
        .ensure_started()
        .expect("runtime should start in default unlocked test session");

    manager.upsert_peer(LdkRuntimePeerStateData {
        pubkey: "peer-a".to_string(),
        peer_addr: "127.0.0.1:9735".to_string(),
        started: false,
    });
    manager.upsert_peer(LdkRuntimePeerStateData {
        pubkey: "peer-b".to_string(),
        peer_addr: "127.0.0.1:9736".to_string(),
        started: true,
    });

    assert!(!manager.has_connected_peer("peer-a"));
    assert!(manager.has_connected_peer("peer-b"));
    assert!(manager.has_any_connected_peer());

    assert!(manager.set_peer_started("peer-b", false));
    assert!(!manager.has_connected_peer("peer-b"));
    assert!(!manager.has_any_connected_peer());
}

#[test]
fn virtual_channel_intent_session_and_reconcile_contract() {
    super::test_utils::reset_runtime_storage_for_tests();
    let manager =
        ldk_runtime_manager("runtime-virtual-channel-test".to_string()).expect("runtime manager");
    manager
        .ensure_started()
        .expect("runtime should start in default unlocked test session");

    let temp_id = manager
        .virtual_channel_add_intent("peer-v", Some("tmp-v-1".to_string()))
        .expect("intent should reserve temp id");
    assert_eq!(temp_id, "tmp-v-1");
    assert!(manager.virtual_channel_draft_get("tmp-v-1").is_some());

    manager.virtual_channel_session_add_from_open("chan-v-1", "tmp-v-1", "peer-v");
    assert!(manager.virtual_channel_draft_get("tmp-v-1").is_none());
    let session = manager
        .virtual_channel_session_get("chan-v-1")
        .expect("session should exist");
    assert_eq!(session.peer_pubkey, "peer-v");
    assert_eq!(
        session.status,
        LdkRuntimeVirtualChannelSessionStatusData::Active
    );

    manager.upsert_channel(LdkRuntimeChannelStateData {
        temporary_channel_id: "tmp-v-1".to_string(),
        channel_id: "chan-v-1".to_string(),
        peer_pubkey: "peer-v".to_string(),
        status: "opened".to_string(),
        ready: true,
        is_usable: true,
        public: false,
        capacity_sat: 5_506,
        asset_id: None,
        asset_local_amount: None,
        virtual_open_mode: Some("trusted_no_broadcast".to_string()),
    });
    assert!(manager.remove_channel("chan-v-1"));
    let session = manager
        .virtual_channel_session_get("chan-v-1")
        .expect("session should still exist");
    assert_eq!(
        session.status,
        LdkRuntimeVirtualChannelSessionStatusData::Abandoned
    );
}

#[test]
fn upsert_channel_preserves_metadata_when_incoming_live_snapshot_is_partial() {
    super::test_utils::reset_runtime_storage_for_tests();
    let manager = ldk_runtime_manager("runtime-channel-metadata-merge-test".to_string())
        .expect("runtime manager");
    manager
        .ensure_started()
        .expect("runtime should start in default unlocked test session");

    manager.upsert_channel(LdkRuntimeChannelStateData {
        temporary_channel_id: "tmp-rich".to_string(),
        channel_id: "chan-rich".to_string(),
        peer_pubkey: "peer-rich".to_string(),
        status: "opening".to_string(),
        ready: false,
        is_usable: false,
        public: false,
        capacity_sat: 5_506,
        asset_id: Some("asset-rich".to_string()),
        asset_local_amount: Some(42),
        virtual_open_mode: Some("trusted_no_broadcast".to_string()),
    });

    manager.upsert_channel(LdkRuntimeChannelStateData {
        temporary_channel_id: "tmp-rich".to_string(),
        channel_id: "chan-rich".to_string(),
        peer_pubkey: String::new(),
        status: "pending".to_string(),
        ready: true,
        is_usable: false,
        public: false,
        capacity_sat: 0,
        asset_id: None,
        asset_local_amount: None,
        virtual_open_mode: None,
    });

    let channel = manager
        .list_channels()
        .into_iter()
        .find(|channel| channel.channel_id == "chan-rich")
        .expect("channel should exist");
    assert_eq!(channel.peer_pubkey, "peer-rich");
    assert_eq!(channel.capacity_sat, 5_506);
    assert_eq!(channel.asset_id.as_deref(), Some("asset-rich"));
    assert_eq!(channel.asset_local_amount, Some(42));
    assert_eq!(
        channel.virtual_open_mode.as_deref(),
        Some("trusted_no_broadcast")
    );
    assert_eq!(channel.status, "pending");
    assert!(channel.ready);
}

#[test]
fn runtime_component_status_tracks_lifecycle_counters_contract() {
    super::test_utils::reset_runtime_storage_for_tests();
    let manager =
        ldk_runtime_manager("runtime-components-test".to_string()).expect("runtime manager");
    manager
        .ensure_started()
        .expect("runtime should start in default unlocked test session");

    manager.record_invoice_created();
    manager.record_payment_initiated();
    manager.record_keysend_initiated();
    manager.record_channel_opened();
    manager.record_channel_closed();

    let status = manager.component_status();
    assert!(status.started);
    assert!(status.fee_estimator_ready);
    assert!(status.broadcaster_ready);
    assert!(status.logger_ready);
    assert!(status.persister_ready);
    assert!(status.key_manager_ready);
    assert!(status.payment_engine_ready);
    assert!(status.channel_engine_ready);
    assert_eq!(status.invoices_created, 1);
    assert_eq!(status.payments_initiated, 1);
    assert_eq!(status.keysends_initiated, 1);
    assert_eq!(status.channels_opened, 1);
    assert_eq!(status.channels_closed, 1);
    assert!(!status.key_manager_fingerprint.is_empty());
}

#[test]
fn runtime_key_canonicalization_normalizes_equivalent_proxy_shapes_contract() {
    assert_eq!(
        canonicalize_runtime_key(" node-runtime:HTTP://LOCALHOST:3001/ "),
        "node-runtime:http://localhost:3001"
    );
    assert_eq!(
        canonicalize_runtime_key("node-runtime:ws://LOCALHOST:3001//"),
        "node-runtime:ws://localhost:3001"
    );
    assert_eq!(
        canonicalize_runtime_key("node-runtime:http://localhost:3001/#runtime:  abc  "),
        "node-runtime:http://localhost:3001#runtime:abc"
    );
}

#[test]
fn runtime_manager_registry_reuses_alias_runtime_keys_contract() {
    super::test_utils::reset_runtime_storage_for_tests();

    let manager_a = ldk_runtime_manager("node-runtime:HTTP://LOCALHOST:3001/".to_string())
        .expect("runtime manager a");
    let manager_b = ldk_runtime_manager(" node-runtime:http://localhost:3001 ".to_string())
        .expect("runtime manager b");

    assert!(Rc::ptr_eq(&manager_a, &manager_b));
}
