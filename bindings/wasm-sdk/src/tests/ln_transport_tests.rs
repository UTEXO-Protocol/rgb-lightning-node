use super::*;

fn default_options() -> RlnWasmLnSocketConnectOptionsData {
    RlnWasmLnSocketConnectOptionsData {
        max_reconnect_attempts: Some(3),
        reconnect_initial_delay_ms: Some(250),
        reconnect_max_delay_ms: Some(4_000),
        relay_auth_token: None,
        relay_node_id: None,
        replay_transport_envelope: Some(false),
        replay_session_id: None,
        replay_last_applied_seq: None,
    }
}

#[test]
fn validate_connect_options_contract() {
    let mut options = default_options();
    assert!(validate_connect_options(&options).is_ok());

    options.reconnect_initial_delay_ms = Some(0);
    assert_eq!(
        validate_connect_options(&options)
            .unwrap_err()
            .as_string()
            .unwrap_or_default(),
        "reconnect delays must be > 0"
    );

    let mut options = default_options();
    options.reconnect_initial_delay_ms = Some(500);
    options.reconnect_max_delay_ms = Some(100);
    assert_eq!(
        validate_connect_options(&options)
            .unwrap_err()
            .as_string()
            .unwrap_or_default(),
        "reconnect_initial_delay_ms cannot be greater than reconnect_max_delay_ms"
    );

    let mut options = default_options();
    options.relay_auth_token = Some("   ".to_string());
    assert_eq!(
        validate_connect_options(&options)
            .unwrap_err()
            .as_string()
            .unwrap_or_default(),
        sdk_contracts::ERR_RELAY_AUTH_TOKEN_EMPTY
    );

    let mut options = default_options();
    options.relay_node_id = Some("   ".to_string());
    assert_eq!(
        validate_connect_options(&options)
            .unwrap_err()
            .as_string()
            .unwrap_or_default(),
        sdk_contracts::ERR_RELAY_NODE_ID_EMPTY
    );

    let mut options = default_options();
    options.replay_session_id = Some("   ".to_string());
    assert_eq!(
        validate_connect_options(&options)
            .unwrap_err()
            .as_string()
            .unwrap_or_default(),
        "replay_session_id cannot be empty"
    );
}

#[test]
fn websocket_url_includes_relay_binding_query_contract() {
    let options = RlnWasmLnSocketConnectOptionsData {
        relay_auth_token: Some("token-1".to_string()),
        relay_node_id: Some(
            "0334cc4bca04ce3d1537310f55e91ec4cec7e5a88fa0fba20a24cce1fe6de2a2b0".to_string(),
        ),
        ..default_options()
    };
    let url = proxy_url_for_peer_with_options("ws://127.0.0.1:3000", "127.0.0.1:9735", &options)
        .expect("url");
    assert!(url.contains("auth_token=token-1"));
    assert!(
        url.contains("node_id=0334cc4bca04ce3d1537310f55e91ec4cec7e5a88fa0fba20a24cce1fe6de2a2b0")
    );
}

#[test]
fn websocket_url_encodes_relay_query_values() {
    let options = RlnWasmLnSocketConnectOptionsData {
        relay_auth_token: Some("token with spaces".to_string()),
        relay_node_id: Some("node/id+value".to_string()),
        ..default_options()
    };
    let url = proxy_url_for_peer_with_options("ws://127.0.0.1:3000", "127.0.0.1:9735", &options)
        .expect("url");
    assert!(url.contains("auth_token=token%20with%20spaces"));
    assert!(url.contains("node_id=node%2Fid%2Bvalue"));
}

#[test]
fn websocket_url_includes_replay_query_contract() {
    let options = RlnWasmLnSocketConnectOptionsData {
        replay_transport_envelope: Some(true),
        replay_session_id: Some("session-abc".to_string()),
        replay_last_applied_seq: Some(42),
        ..default_options()
    };
    let url = proxy_url_for_peer_with_options("ws://127.0.0.1:3000", "127.0.0.1:9735", &options)
        .expect("url");
    assert!(url.contains("replay=1"));
    assert!(url.contains("session_id=session-abc"));
    assert!(url.contains("last_applied_seq=42"));
}

#[test]
fn websocket_url_omits_last_seq_when_not_available_contract() {
    let options = RlnWasmLnSocketConnectOptionsData {
        replay_transport_envelope: Some(true),
        replay_session_id: Some("session-no-seq".to_string()),
        replay_last_applied_seq: None,
        ..default_options()
    };
    let url = proxy_url_for_peer_with_options("ws://127.0.0.1:3000", "127.0.0.1:9735", &options)
        .expect("url");
    assert!(url.contains("replay=1"));
    assert!(url.contains("session_id=session-no-seq"));
    assert!(!url.contains("last_applied_seq="));
}
