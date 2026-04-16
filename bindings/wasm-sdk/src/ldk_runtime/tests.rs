use super::*;

#[test]
fn connected_peer_helpers_track_started_state_contract() {
    reset_scaffold_runtime_storage_for_tests();
    let manager = ldk_runtime_manager(
        "runtime-connected-peer-test".to_string(),
        Some("ldk_bridge".to_string()),
    )
    .expect("runtime manager");
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
    reset_scaffold_runtime_storage_for_tests();
    let manager = ldk_runtime_manager(
        "runtime-virtual-channel-test".to_string(),
        Some("ldk_bridge".to_string()),
    )
    .expect("runtime manager");
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
