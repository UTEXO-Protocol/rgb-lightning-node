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
