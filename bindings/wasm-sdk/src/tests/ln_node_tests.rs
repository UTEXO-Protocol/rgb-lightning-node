use super::*;
use futures::executor::block_on;
use serde::Deserialize;
use wasm_bindgen_test::wasm_bindgen_test;

use crate::wasm_node_persistence::RuntimeScopeKeys;
use crate::{RlnWasmSdk, RlnWasmSdkRuntimeCapabilitiesData};

#[derive(Debug, Deserialize)]
struct TestTransportEventApplyData {
    event_kind: String,
    applied: bool,
}

#[derive(Debug, Deserialize)]
struct TestKeysendData {
    payment_hash: String,
}

#[derive(Debug, Deserialize)]
struct TestPaymentData {
    status: String,
}

#[derive(Debug, Deserialize)]
struct TestRuntimeEventData {
    source: String,
    event_kind: String,
    applied: bool,
    payment_hash: Option<String>,
    status: Option<String>,
    error: Option<String>,
}

#[test]
fn runtime_control_events_are_sequenced_and_recorded() {
    let runtime_events = Rc::new(RefCell::new(Vec::new()));
    let next_runtime_event_seq = Rc::new(RefCell::new(0));

    record_runtime_control_event(
        &runtime_events,
        &next_runtime_event_seq,
        "peer_hook_error",
        "00aa".to_string(),
        Some("error A".to_string()),
    );
    record_runtime_control_event(
        &runtime_events,
        &next_runtime_event_seq,
        "peer_hook_disconnected",
        "".to_string(),
        Some("disconnect".to_string()),
    );

    let events = runtime_events.borrow();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].seq, 1);
    assert_eq!(events[0].source, "peer_hook_error");
    assert_eq!(events[0].event_kind, "control");
    assert_eq!(events[0].payload_hex, "00aa");
    assert_eq!(events[0].error.as_deref(), Some("error A"));
    assert!(!events[0].applied);
    assert_eq!(events[1].seq, 2);
    assert_eq!(events[1].source, "peer_hook_disconnected");
    assert_eq!(events[1].event_kind, "control");
    assert_eq!(events[1].error.as_deref(), Some("disconnect"));
    assert!(!events[1].applied);
}

#[test]
fn runtime_event_log_snapshot_restores_on_recreated_node() {
    super::test_utils::reset_runtime_event_log_storage_for_tests();
    let storage_key =
        RuntimeScopeKeys::from_runtime_scope_key("ws://runtime-events-persist.example".to_string())
            .runtime_events_storage_key;
    let runtime_events = Rc::new(RefCell::new(vec![RlnWasmNodeRuntimeEventData {
        seq: 1,
        source: "manual_api".to_string(),
        event_kind: "channel_usable".to_string(),
        payload_hex: hex::encode("channel_usable:chan-persist"),
        payment_hash: None,
        status: None,
        applied: true,
        error: None,
        received_at: 1,
    }]));
    let next_runtime_event_seq = Rc::new(RefCell::new(1u64));
    persist_runtime_event_log_state(&storage_key, &runtime_events, &next_runtime_event_seq);

    let restored =
        load_runtime_event_log_snapshot(&storage_key).expect("snapshot should be restored");
    assert_eq!(restored.events.len(), 1);
    assert_eq!(restored.events[0].seq, 1);
    assert_eq!(restored.events[0].event_kind, "channel_usable");
    assert!(restored.events[0].applied);
    assert_eq!(restored.next_seq, 1);
}

#[test]
fn runtime_event_log_snapshot_persists_tail_window_contract() {
    super::test_utils::reset_runtime_event_log_storage_for_tests();
    let storage_key =
        RuntimeScopeKeys::from_runtime_scope_key("ws://runtime-events-window.example".to_string())
            .runtime_events_storage_key;
    let runtime_events = Rc::new(RefCell::new(
        (1u64..=600)
            .map(|seq| RlnWasmNodeRuntimeEventData {
                seq,
                source: "manual_api".to_string(),
                event_kind: "payment_status".to_string(),
                payload_hex: hex::encode(format!("payment_status:{seq}:succeeded")),
                payment_hash: Some(format!("hash-{seq}")),
                status: Some("succeeded".to_string()),
                applied: true,
                error: None,
                received_at: seq,
            })
            .collect::<Vec<_>>(),
    ));
    let next_runtime_event_seq = Rc::new(RefCell::new(600u64));
    persist_runtime_event_log_state(&storage_key, &runtime_events, &next_runtime_event_seq);

    let restored =
        load_runtime_event_log_snapshot(&storage_key).expect("snapshot should be restored");
    assert_eq!(restored.events.len(), 512);
    assert_eq!(restored.events.first().map(|entry| entry.seq), Some(89));
    assert_eq!(restored.events.last().map(|entry| entry.seq), Some(600));
    assert_eq!(restored.next_seq, 600);
}

#[test]
fn runtime_scope_key_canonicalizes_proxy_aliases_contract() {
    assert_eq!(
        runtime_scope_key(" HTTP://LOCALHOST:3001/ ", None),
        "http://localhost:3001"
    );
    assert_eq!(
        runtime_scope_key("ws://LOCALHOST:3001//", Some("  runtime-a  ")),
        "ws://localhost:3001#runtime:runtime-a"
    );
}

#[test]
fn node_constructor_installs_auto_peer_manager_hooks_contract() {
    clear_rln_ldk_peer_manager_hooks();
    assert!(!has_peer_manager_hooks());
    assert!(!has_peer_manager_hooks_v2());

    let _node = RlnWasmNode::new("ws://127.0.0.1:3001".to_string()).expect("node should build");

    assert!(has_peer_manager_hooks());
    assert!(has_peer_manager_hooks_v2());
}

#[test]
fn runtime_peer_session_key_is_stable_and_normalized_contract() {
    let runtime_scope = "ws://localhost:3001#runtime:node-a";
    let key = runtime_peer_session_key(
        runtime_scope,
        "0334cc4bca04ce3d1537310f55e91ec4cec7e5a88fa0fba20a24cce1fe6de2a2b0",
    );
    assert_eq!(
        key,
        "ws://localhost:3001#runtime:node-a::0334cc4bca04ce3d1537310f55e91ec4cec7e5a88fa0fba20a24cce1fe6de2a2b0"
    );
}

#[test]
#[cfg(target_arch = "wasm32")]
fn reconnect_persisted_peers_reports_session_key_mismatch_without_network_contract() {
    super::test_utils::reset_runtime_event_log_storage_for_tests();
    crate::ldk_runtime::test_utils::reset_runtime_storage_for_tests();

    let proxy = "ws://proxy.reconnect-mismatch.example".to_string();
    let node = RlnWasmNode::new(proxy.clone()).expect("node should build");
    let runtime_scope = runtime_scope_key(proxy.trim(), None);
    let store_key =
        RuntimeScopeKeys::from_runtime_scope_key(runtime_scope.clone()).peer_sessions_storage_key;
    let snapshot = RuntimePeerSessionSnapshot {
        sessions: vec![RuntimePeerSessionEntryData {
            session_key: "bad-key".to_string(),
            peer_pubkey: "0334cc4bca04ce3d1537310f55e91ec4cec7e5a88fa0fba20a24cce1fe6de2a2b0"
                .to_string(),
            peer_addr: "127.0.0.1:9735".to_string(),
        }],
    };
    let store = browser_persistent_state_store();
    let raw = serde_json::to_string(&snapshot).expect("encode snapshot");
    store.set(&store_key, &raw).expect("persist snapshot");

    let result_js =
        block_on(node.reconnect_persisted_peers_value()).expect("reconnect should return result");
    let result: serde_json::Value = crate::js_from(result_js).expect("parse reconnect result");
    assert_eq!(result["attempted"], 1);
    assert_eq!(result["connected"], 0);
    let failed = result["failed"].as_array().expect("failed array");
    assert_eq!(failed.len(), 1);
    assert!(failed[0]
        .as_str()
        .unwrap_or_default()
        .contains("session key mismatch"));
}

#[test]
#[cfg(target_arch = "wasm32")]
fn reconnect_manager_start_stop_status_contract() {
    super::test_utils::reset_runtime_event_log_storage_for_tests();
    crate::ldk_runtime::test_utils::reset_runtime_storage_for_tests();

    let node =
        RlnWasmNode::new("ws://proxy.reconnect-manager.example".to_string()).expect("build node");

    let started_js = node
        .reconnect_manager_start_value()
        .expect("start reconnect manager");
    let started: serde_json::Value = crate::js_from(started_js).expect("parse started status");
    assert_eq!(started["running"], true);
    assert_eq!(
        started["current_backoff_ms"],
        serde_json::Value::from(RECONNECT_MANAGER_INITIAL_DELAY_MS)
    );

    let status_js = node
        .reconnect_manager_status_value()
        .expect("status reconnect manager");
    let status: serde_json::Value = crate::js_from(status_js).expect("parse status");
    assert_eq!(status["running"], true);

    let stopped_js = node
        .reconnect_manager_stop_value()
        .expect("stop reconnect manager");
    let stopped: serde_json::Value = crate::js_from(stopped_js).expect("parse stopped status");
    assert_eq!(stopped["running"], false);
}

#[test]
#[cfg(target_arch = "wasm32")]
fn drain_pending_peer_hook_events_applies_queued_payloads() {
    let ldk_runtime =
        crate::ldk_runtime::ldk_runtime_manager("hook-queue".to_string()).expect("runtime manager");
    let peers = Rc::new(RefCell::new(HashMap::<String, PeerEntry>::new()));
    let channels = Rc::new(RefCell::new(HashMap::<String, ChannelEntry>::new()));
    let payments = Rc::new(RefCell::new(HashMap::new()));
    let pending_peer_hook_events = Rc::new(RefCell::new(vec![
        PendingPeerHookEvent::Payload(hex::encode("peer_reconnected:peer-q")),
        PendingPeerHookEvent::Payload(hex::encode("peer_disconnected:peer-q")),
    ]));
    let runtime_events = Rc::new(RefCell::new(Vec::new()));
    let next_runtime_event_seq = Rc::new(RefCell::new(0));

    ldk_runtime.upsert_peer(LdkRuntimePeerStateData {
        pubkey: "peer-q".to_string(),
        peer_addr: "127.0.0.1:9735".to_string(),
        started: false,
    });

    let drained = drain_pending_peer_hook_events(
        &ldk_runtime,
        true,
        &peers,
        &channels,
        &payments,
        &pending_peer_hook_events,
        &runtime_events,
        &next_runtime_event_seq,
        "peer_hook",
    )
    .expect("queue should drain");
    assert_eq!(drained, 2);
    assert!(pending_peer_hook_events.borrow().is_empty());

    let peer = ldk_runtime.get_peer("peer-q").expect("peer should exist");
    assert!(!peer.started);

    let events = runtime_events.borrow();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].event_kind, "peer_reconnected");
    assert_eq!(events[1].event_kind, "peer_disconnected");
}

#[test]
#[cfg(target_arch = "wasm32")]
fn bridge_apply_payment_status_via_event_stream_updates_runtime_and_log() {
    let node = RlnWasmNode::new_with_runtime_backend(
        "ws://proxy.example".to_string(),
        "wasm_native_ldk".to_string(),
    )
    .expect("node should build");
    node.ldk_runtime.upsert_payment(LdkRuntimePaymentStateData {
        amt_msat: Some(3_000_000),
        asset_amount: None,
        asset_id: None,
        payment_hash: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
            .to_string(),
        inbound: false,
        status: "pending".to_string(),
        invoice_type: None,
        preimage: None,
        created_at: 1,
        updated_at: 1,
        payee_pubkey: "0334cc4bca04ce3d1537310f55e91ec4cec7e5a88fa0fba20a24cce1fe6de2a2b0"
            .to_string(),
    });

    let updated = node
        .apply_payment_status_via_event_stream(
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "failed",
            "node_api",
        )
        .expect("status update should apply");
    assert_eq!(updated.status, "failed");

    let runtime_payment = node
        .ldk_runtime
        .get_payment("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
        .expect("runtime payment should exist");
    assert_eq!(runtime_payment.status, "failed");

    let events = node.runtime_events.borrow();
    assert!(!events.is_empty());
    let last = events.last().expect("event should exist");
    assert_eq!(last.event_kind, "payment_status");
    assert_eq!(last.status.as_deref(), Some("failed"));
    assert!(last.applied);
}

#[test]
fn tolerant_transport_mode_records_non_payment_payload_without_error() {
    let payments = Rc::new(RefCell::new(HashMap::new()));
    let runtime_events = Rc::new(RefCell::new(Vec::new()));
    let next_runtime_event_seq = Rc::new(RefCell::new(0));
    let payload_hex = hex::encode("peer_event:connected");

    let result = apply_runtime_event_payload(
        &payments,
        &runtime_events,
        &next_runtime_event_seq,
        payload_hex,
        "peer_hook",
        RuntimeEventApplyMode::TolerantTransport,
    )
    .expect("tolerant mode should not error");
    assert!(result.is_none());

    let events = runtime_events.borrow();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].source, "peer_hook");
    assert_eq!(events[0].event_kind, "text_protocol_payload");
    assert!(!events[0].applied);
    assert_eq!(
        events[0].error.as_deref(),
        Some("unrecognized event payload format for payment status update")
    );
}

#[test]
fn hook_payload_transport_event_updates_native_channel_state() {
    let ldk_runtime = crate::ldk_runtime::ldk_runtime_manager("hook-native".to_string())
        .expect("runtime manager");
    let peers = Rc::new(RefCell::new(HashMap::<String, PeerEntry>::new()));
    let channels = Rc::new(RefCell::new(HashMap::<String, ChannelEntry>::new()));
    let payments = Rc::new(RefCell::new(HashMap::new()));
    let runtime_events = Rc::new(RefCell::new(Vec::new()));
    let next_runtime_event_seq = Rc::new(RefCell::new(0));
    channels.borrow_mut().insert(
        "chan-hook".to_string(),
        ChannelEntry {
            temporary_channel_id: "tmp-hook".to_string(),
            data: RlnWasmNodeChannelData {
                temporary_channel_id: "tmp-hook".to_string(),
                channel_id: "chan-hook".to_string(),
                peer_pubkey: "peer-hook".to_string(),
                status: "pending".to_string(),
                ready: false,
                is_usable: false,
                public: false,
                capacity_sat: 1_000,
                asset_id: None,
                asset_local_amount: None,
                virtual_open_mode: None,
            },
        },
    );

    apply_runtime_hook_payload(
        &ldk_runtime,
        false,
        &peers,
        &channels,
        &payments,
        &runtime_events,
        &next_runtime_event_seq,
        hex::encode("channel_usable:chan-hook"),
        "peer_hook",
    )
    .expect("hook payload should apply");

    let channel = channels
        .borrow()
        .get("chan-hook")
        .map(|entry| entry.data.clone())
        .expect("channel should exist");
    assert!(channel.is_usable);
    assert!(channel.ready);
    assert_eq!(channel.status, "opened");
    let events = runtime_events.borrow();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].event_kind, "channel_usable");
    assert!(events[0].applied);
}

#[test]
fn hook_payload_transport_event_updates_bridge_runtime_state() {
    let node = RlnWasmNode::new_with_runtime_backend(
        "ws://proxy.example".to_string(),
        "wasm_native_ldk".to_string(),
    )
    .expect("node should build");
    node.ldk_runtime.upsert_channel(LdkRuntimeChannelStateData {
        temporary_channel_id: "tmp-bridge-hook".to_string(),
        channel_id: "chan-bridge-hook".to_string(),
        peer_pubkey: "peer-bridge-hook".to_string(),
        status: "pending".to_string(),
        ready: false,
        is_usable: false,
        public: false,
        capacity_sat: 1_000,
        asset_id: None,
        asset_local_amount: None,
        virtual_open_mode: None,
    });

    apply_runtime_hook_payload(
        &node.ldk_runtime,
        true,
        &node.peers,
        &node.channels,
        &node.payments,
        &node.runtime_events,
        &node.next_runtime_event_seq,
        hex::encode("channel_usable:chan-bridge-hook"),
        "peer_hook",
    )
    .expect("hook payload should apply");

    let channel = node
        .ldk_runtime
        .list_channels()
        .into_iter()
        .find(|entry| entry.channel_id == "chan-bridge-hook")
        .expect("runtime channel should exist");
    assert!(channel.is_usable);
    assert!(channel.ready);
    assert_eq!(channel.status, "opened");
    let events = node.runtime_events.borrow();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].event_kind, "channel_usable");
    assert!(events[0].applied);
}

#[test]
fn hook_payload_peer_reconnected_updates_bridge_peer_started_state() {
    let node = RlnWasmNode::new_with_runtime_backend(
        "ws://proxy.example".to_string(),
        "wasm_native_ldk".to_string(),
    )
    .expect("node should build");
    node.ldk_runtime.upsert_peer(LdkRuntimePeerStateData {
        pubkey: "peer-bridge-reconnect".to_string(),
        peer_addr: "127.0.0.1:9735".to_string(),
        started: false,
    });

    apply_runtime_hook_payload(
        &node.ldk_runtime,
        true,
        &node.peers,
        &node.channels,
        &node.payments,
        &node.runtime_events,
        &node.next_runtime_event_seq,
        hex::encode("peer_reconnected:peer-bridge-reconnect"),
        "peer_hook",
    )
    .expect("hook payload should apply");

    let peer = node
        .ldk_runtime
        .get_peer("peer-bridge-reconnect")
        .expect("peer should exist");
    assert!(peer.started);
    let events = node.runtime_events.borrow();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].event_kind, "peer_reconnected");
    assert!(events[0].applied);
}

#[test]
fn parse_transport_event_payload_accepts_json_alias_contract() {
    let payload = serde_json::json!({
        "event": "PeerReconnected",
        "peer_pubkey": "peer-json-alias"
    })
    .to_string();
    let parsed = parse_transport_event_payload(&hex::encode(payload.as_bytes()))
        .expect("payload should parse");
    match parsed {
        RuntimeTransportEvent::PeerReconnected { peer_pubkey } => {
            assert_eq!(peer_pubkey, "peer-json-alias")
        }
        other => panic!("unexpected event: {other:?}"),
    }
}

#[test]
fn parse_transport_event_payload_accepts_event_name_alias_contract() {
    let payload = serde_json::json!({
        "eventName": "PeerConnected",
        "node_id": "peer-json-event-name"
    })
    .to_string();
    let parsed = parse_transport_event_payload(&hex::encode(payload.as_bytes()))
        .expect("payload should parse");
    match parsed {
        RuntimeTransportEvent::PeerReconnected { peer_pubkey } => {
            assert_eq!(peer_pubkey, "peer-json-event-name")
        }
        other => panic!("unexpected event: {other:?}"),
    }

    let payload = serde_json::json!({
        "event_name": "channel_unusable",
        "channelId": "chan-json-event-name"
    })
    .to_string();
    let parsed = parse_transport_event_payload(&hex::encode(payload.as_bytes()))
        .expect("payload should parse");
    match parsed {
        RuntimeTransportEvent::ChannelUnusable { channel_id } => {
            assert_eq!(channel_id, "chan-json-event-name")
        }
        other => panic!("unexpected event: {other:?}"),
    }
}

#[test]
fn parse_transport_event_payload_accepts_json_kind_alias_contract() {
    let payload = serde_json::json!({
        "kind": "channel_unusable",
        "id": "chan-json-kind"
    })
    .to_string();
    let parsed = parse_transport_event_payload(&hex::encode(payload.as_bytes()))
        .expect("payload should parse");
    match parsed {
        RuntimeTransportEvent::ChannelUnusable { channel_id } => {
            assert_eq!(channel_id, "chan-json-kind")
        }
        other => panic!("unexpected event: {other:?}"),
    }
}

#[test]
fn parse_transport_event_payload_accepts_json_type_alias_contract() {
    let payload = serde_json::json!({
        "type": "peer_reconnected",
        "id": "peer-json-type"
    })
    .to_string();
    let parsed = parse_transport_event_payload(&hex::encode(payload.as_bytes()))
        .expect("payload should parse");
    match parsed {
        RuntimeTransportEvent::PeerReconnected { peer_pubkey } => {
            assert_eq!(peer_pubkey, "peer-json-type")
        }
        other => panic!("unexpected event: {other:?}"),
    }
}

#[test]
fn parse_transport_event_payload_accepts_json_type_alias_channel_contract() {
    let payload = serde_json::json!({
        "type": "ChannelUnusable",
        "id": "chan-json-type"
    })
    .to_string();
    let parsed = parse_transport_event_payload(&hex::encode(payload.as_bytes()))
        .expect("payload should parse");
    match parsed {
        RuntimeTransportEvent::ChannelUnusable { channel_id } => {
            assert_eq!(channel_id, "chan-json-type")
        }
        other => panic!("unexpected event: {other:?}"),
    }
}

#[test]
fn parse_transport_event_payload_accepts_peer_node_id_and_channel_id_alias_contract() {
    let peer_payload = serde_json::json!({
        "event": "peer_connected",
        "node_id": "peer-json-node-id"
    })
    .to_string();
    let parsed = parse_transport_event_payload(&hex::encode(peer_payload.as_bytes()))
        .expect("peer payload should parse");
    match parsed {
        RuntimeTransportEvent::PeerReconnected { peer_pubkey } => {
            assert_eq!(peer_pubkey, "peer-json-node-id")
        }
        other => panic!("unexpected event: {other:?}"),
    }

    let channel_payload = serde_json::json!({
        "type": "channel_unusable",
        "channelId": "chan-json-channel-id"
    })
    .to_string();
    let parsed = parse_transport_event_payload(&hex::encode(channel_payload.as_bytes()))
        .expect("channel payload should parse");
    match parsed {
        RuntimeTransportEvent::ChannelUnusable { channel_id } => {
            assert_eq!(channel_id, "chan-json-channel-id")
        }
        other => panic!("unexpected event: {other:?}"),
    }
}

#[test]
fn parse_transport_event_payload_accepts_peer_connected_aliases_contract() {
    let json_payload = serde_json::json!({
        "event": "PeerConnected",
        "id": "peer-json-connected"
    })
    .to_string();
    let parsed = parse_transport_event_payload(&hex::encode(json_payload.as_bytes()))
        .expect("json alias should parse");
    match parsed {
        RuntimeTransportEvent::PeerReconnected { peer_pubkey } => {
            assert_eq!(peer_pubkey, "peer-json-connected")
        }
        other => panic!("unexpected event: {other:?}"),
    }

    let text_payload = hex::encode("peer_connected:peer-text-connected");
    let parsed = parse_transport_event_payload(&text_payload).expect("text alias should parse");
    match parsed {
        RuntimeTransportEvent::PeerReconnected { peer_pubkey } => {
            assert_eq!(peer_pubkey, "peer-text-connected")
        }
        other => panic!("unexpected event: {other:?}"),
    }
}

#[test]
fn parse_transport_event_payload_accepts_channel_opened_ready_aliases_contract() {
    let json_payload = serde_json::json!({
        "event": "ChannelOpened",
        "id": "chan-json-opened"
    })
    .to_string();
    let parsed = parse_transport_event_payload(&hex::encode(json_payload.as_bytes()))
        .expect("json alias should parse");
    match parsed {
        RuntimeTransportEvent::ChannelUsable { channel_id } => {
            assert_eq!(channel_id, "chan-json-opened")
        }
        other => panic!("unexpected event: {other:?}"),
    }

    let text_payload = hex::encode("channel_ready:chan-text-ready");
    let parsed = parse_transport_event_payload(&text_payload).expect("text alias should parse");
    match parsed {
        RuntimeTransportEvent::ChannelUsable { channel_id } => {
            assert_eq!(channel_id, "chan-text-ready")
        }
        other => panic!("unexpected event: {other:?}"),
    }
}

#[test]
fn parse_transport_event_payload_accepts_channel_disconnected_aliases_contract() {
    let json_payload = serde_json::json!({
        "event": "ChannelDisconnected",
        "id": "chan-json-disconnected"
    })
    .to_string();
    let parsed = parse_transport_event_payload(&hex::encode(json_payload.as_bytes()))
        .expect("json alias should parse");
    match parsed {
        RuntimeTransportEvent::ChannelUnusable { channel_id } => {
            assert_eq!(channel_id, "chan-json-disconnected")
        }
        other => panic!("unexpected event: {other:?}"),
    }

    let text_payload = hex::encode("channel_disconnected:chan-text-disconnected");
    let parsed = parse_transport_event_payload(&text_payload).expect("text alias should parse");
    match parsed {
        RuntimeTransportEvent::ChannelUnusable { channel_id } => {
            assert_eq!(channel_id, "chan-text-disconnected")
        }
        other => panic!("unexpected event: {other:?}"),
    }
}

#[test]
fn parse_transport_event_payload_accepts_peer_online_channel_online_aliases_contract() {
    let peer_json = serde_json::json!({
        "kind": "PeerOnline",
        "id": "peer-json-online"
    })
    .to_string();
    let parsed = parse_transport_event_payload(&hex::encode(peer_json.as_bytes()))
        .expect("peer json alias should parse");
    match parsed {
        RuntimeTransportEvent::PeerReconnected { peer_pubkey } => {
            assert_eq!(peer_pubkey, "peer-json-online")
        }
        other => panic!("unexpected event: {other:?}"),
    }

    let channel_json = serde_json::json!({
        "type": "ChannelOnline",
        "id": "chan-json-online"
    })
    .to_string();
    let parsed = parse_transport_event_payload(&hex::encode(channel_json.as_bytes()))
        .expect("channel json alias should parse");
    match parsed {
        RuntimeTransportEvent::ChannelUsable { channel_id } => {
            assert_eq!(channel_id, "chan-json-online")
        }
        other => panic!("unexpected event: {other:?}"),
    }

    let peer_text = hex::encode("peer_online:peer-text-online");
    let parsed = parse_transport_event_payload(&peer_text).expect("peer text alias should parse");
    match parsed {
        RuntimeTransportEvent::PeerReconnected { peer_pubkey } => {
            assert_eq!(peer_pubkey, "peer-text-online")
        }
        other => panic!("unexpected event: {other:?}"),
    }

    let channel_text = hex::encode("channel_online:chan-text-online");
    let parsed =
        parse_transport_event_payload(&channel_text).expect("channel text alias should parse");
    match parsed {
        RuntimeTransportEvent::ChannelUsable { channel_id } => {
            assert_eq!(channel_id, "chan-text-online")
        }
        other => panic!("unexpected event: {other:?}"),
    }
}

#[test]
fn parse_transport_event_payload_accepts_peer_offline_channel_offline_aliases_contract() {
    let peer_json = serde_json::json!({
        "event": "PeerOffline",
        "id": "peer-json-offline"
    })
    .to_string();
    let parsed = parse_transport_event_payload(&hex::encode(peer_json.as_bytes()))
        .expect("peer json alias should parse");
    match parsed {
        RuntimeTransportEvent::PeerDisconnected { peer_pubkey } => {
            assert_eq!(peer_pubkey, "peer-json-offline")
        }
        other => panic!("unexpected event: {other:?}"),
    }

    let channel_json = serde_json::json!({
        "kind": "channel_offline",
        "id": "chan-json-offline"
    })
    .to_string();
    let parsed = parse_transport_event_payload(&hex::encode(channel_json.as_bytes()))
        .expect("channel json alias should parse");
    match parsed {
        RuntimeTransportEvent::ChannelUnusable { channel_id } => {
            assert_eq!(channel_id, "chan-json-offline")
        }
        other => panic!("unexpected event: {other:?}"),
    }

    let peer_text = hex::encode("peer_offline:peer-text-offline");
    let parsed = parse_transport_event_payload(&peer_text).expect("peer text alias should parse");
    match parsed {
        RuntimeTransportEvent::PeerDisconnected { peer_pubkey } => {
            assert_eq!(peer_pubkey, "peer-text-offline")
        }
        other => panic!("unexpected event: {other:?}"),
    }

    let channel_text = hex::encode("channel_offline:chan-text-offline");
    let parsed =
        parse_transport_event_payload(&channel_text).expect("channel text alias should parse");
    match parsed {
        RuntimeTransportEvent::ChannelUnusable { channel_id } => {
            assert_eq!(channel_id, "chan-text-offline")
        }
        other => panic!("unexpected event: {other:?}"),
    }
}

#[test]
fn parse_transport_event_payload_accepts_up_down_aliases_contract() {
    let peer_up = serde_json::json!({
        "event": "peer_up",
        "id": "peer-json-up"
    })
    .to_string();
    let parsed = parse_transport_event_payload(&hex::encode(peer_up.as_bytes()))
        .expect("peer up alias should parse");
    match parsed {
        RuntimeTransportEvent::PeerReconnected { peer_pubkey } => {
            assert_eq!(peer_pubkey, "peer-json-up")
        }
        other => panic!("unexpected event: {other:?}"),
    }

    let peer_down = hex::encode("peer_down:peer-text-down");
    let parsed = parse_transport_event_payload(&peer_down).expect("peer down alias should parse");
    match parsed {
        RuntimeTransportEvent::PeerDisconnected { peer_pubkey } => {
            assert_eq!(peer_pubkey, "peer-text-down")
        }
        other => panic!("unexpected event: {other:?}"),
    }

    let channel_up = serde_json::json!({
        "kind": "channel_up",
        "id": "chan-json-up"
    })
    .to_string();
    let parsed = parse_transport_event_payload(&hex::encode(channel_up.as_bytes()))
        .expect("channel up alias should parse");
    match parsed {
        RuntimeTransportEvent::ChannelUsable { channel_id } => {
            assert_eq!(channel_id, "chan-json-up")
        }
        other => panic!("unexpected event: {other:?}"),
    }

    let channel_down = hex::encode("channel_down:chan-text-down");
    let parsed =
        parse_transport_event_payload(&channel_down).expect("channel down alias should parse");
    match parsed {
        RuntimeTransportEvent::ChannelUnusable { channel_id } => {
            assert_eq!(channel_id, "chan-text-down")
        }
        other => panic!("unexpected event: {other:?}"),
    }
}

#[test]
fn parse_transport_event_payload_accepts_hyphen_and_dot_aliases_contract() {
    let peer_json = serde_json::json!({
        "event": "peer-up",
        "id": "peer-json-hyphen"
    })
    .to_string();
    let parsed = parse_transport_event_payload(&hex::encode(peer_json.as_bytes()))
        .expect("peer json alias should parse");
    match parsed {
        RuntimeTransportEvent::PeerReconnected { peer_pubkey } => {
            assert_eq!(peer_pubkey, "peer-json-hyphen")
        }
        other => panic!("unexpected event: {other:?}"),
    }

    let channel_json = serde_json::json!({
        "kind": "channel.down",
        "id": "chan-json-dot"
    })
    .to_string();
    let parsed = parse_transport_event_payload(&hex::encode(channel_json.as_bytes()))
        .expect("channel json alias should parse");
    match parsed {
        RuntimeTransportEvent::ChannelUnusable { channel_id } => {
            assert_eq!(channel_id, "chan-json-dot")
        }
        other => panic!("unexpected event: {other:?}"),
    }

    let peer_text = hex::encode("peer-up:peer-text-hyphen");
    let parsed = parse_transport_event_payload(&peer_text).expect("peer text alias should parse");
    match parsed {
        RuntimeTransportEvent::PeerReconnected { peer_pubkey } => {
            assert_eq!(peer_pubkey, "peer-text-hyphen")
        }
        other => panic!("unexpected event: {other:?}"),
    }
}

#[test]
fn hook_payload_mixed_stream_preserves_event_order_and_terminal_payment_state_contract() {
    let node = RlnWasmNode::new_with_runtime_backend(
        "ws://proxy.example".to_string(),
        "wasm_native_ldk".to_string(),
    )
    .expect("node should build");
    node.ldk_runtime.upsert_channel(LdkRuntimeChannelStateData {
        temporary_channel_id: "tmp-mixed".to_string(),
        channel_id: "chan-mixed".to_string(),
        peer_pubkey: "peer-mixed".to_string(),
        status: "pending".to_string(),
        ready: false,
        is_usable: false,
        public: false,
        capacity_sat: 1_000,
        asset_id: None,
        asset_local_amount: None,
        virtual_open_mode: None,
    });
    node.ldk_runtime.upsert_payment(LdkRuntimePaymentStateData {
        amt_msat: Some(SDK_HTLC_MIN_MSAT),
        asset_amount: None,
        asset_id: None,
        payment_hash: "pay-mixed".to_string(),
        inbound: false,
        status: "pending".to_string(),
        invoice_type: None,
        preimage: None,
        created_at: 10,
        updated_at: 10,
        payee_pubkey: "peer-mixed".to_string(),
    });

    let payloads = vec![
        hex::encode("channel_usable:chan-mixed"),
        hex::encode(r#"{"event":"PaymentSent","payment_hash":"pay-mixed"}"#),
        hex::encode("channel_closed:chan-mixed"),
        hex::encode("payment_failed:pay-mixed"),
    ];

    for payload_hex in payloads {
        apply_runtime_hook_payload(
            &node.ldk_runtime,
            true,
            &node.peers,
            &node.channels,
            &node.payments,
            &node.runtime_events,
            &node.next_runtime_event_seq,
            payload_hex,
            "peer_hook",
        )
        .expect("hook payload should process");
    }

    let payment = node
        .ldk_runtime
        .get_payment("pay-mixed")
        .expect("payment should exist");
    assert_eq!(payment.status, "succeeded");

    let channel_exists = node
        .ldk_runtime
        .list_channels()
        .iter()
        .any(|entry| entry.channel_id == "chan-mixed");
    assert!(!channel_exists);

    let events = node.runtime_events.borrow();
    assert_eq!(events.len(), 4);
    assert_eq!(events[0].event_kind, "channel_usable");
    assert!(events[0].applied);
    assert_eq!(events[1].event_kind, "payment_status");
    assert_eq!(events[1].status.as_deref(), Some("succeeded"));
    assert!(events[1].applied);
    assert_eq!(events[2].event_kind, "channel_closed");
    assert!(events[2].applied);
    assert_eq!(events[3].event_kind, "payment_status");
    assert_eq!(events[3].status.as_deref(), Some("failed"));
    assert!(!events[3].applied);
    assert!(events[3]
        .error
        .as_deref()
        .unwrap_or_default()
        .contains("invalid payment status transition"));
    for (idx, event) in events.iter().enumerate() {
        assert_eq!(event.seq, (idx + 1) as u64);
    }
}

#[test]
fn transport_event_roundtrip_codec_supports_all_event_kinds() {
    let events = vec![
        RuntimeTransportEvent::PeerDisconnected {
            peer_pubkey: "peer-a".to_string(),
        },
        RuntimeTransportEvent::PeerReconnected {
            peer_pubkey: "peer-b".to_string(),
        },
        RuntimeTransportEvent::ChannelClosed {
            channel_id: "chan-1".to_string(),
        },
        RuntimeTransportEvent::ChannelUsable {
            channel_id: "chan-2".to_string(),
        },
        RuntimeTransportEvent::ChannelUnusable {
            channel_id: "chan-3".to_string(),
        },
    ];

    for event in events {
        let payload_hex = encode_transport_event_payload(&event);
        let parsed = parse_transport_event_payload(&payload_hex).expect("event should parse");
        assert_eq!(parsed.event_kind(), event.event_kind());
    }
}

#[test]
fn payment_status_event_roundtrip_codec_supports_transition_payload() {
    let payload_hex = encode_payment_status_event_payload("ab12", "failed");
    let parsed = parse_payment_status_event_payload(&payload_hex).expect("event should parse");
    assert_eq!(parsed.payment_hash, "ab12");
    assert_eq!(parsed.status, "failed");
}

#[test]
fn payment_status_event_codec_supports_ldk_style_json_event_mapping() {
    let payload = serde_json::json!({
        "event": "PaymentSent",
        "payment_hash": "ab34",
    });
    let payload_hex = hex::encode(payload.to_string().as_bytes());
    let parsed = parse_payment_status_event_payload(&payload_hex).expect("event should parse");
    assert_eq!(parsed.payment_hash, "ab34");
    assert_eq!(parsed.status, "succeeded");
}

#[test]
fn payment_status_event_codec_supports_alias_text_protocol_mapping() {
    let payload_hex = hex::encode("payment_failed:ab56".as_bytes());
    let parsed = parse_payment_status_event_payload(&payload_hex).expect("event should parse");
    assert_eq!(parsed.payment_hash, "ab56");
    assert_eq!(parsed.status, "failed");
}

#[test]
fn payment_status_event_codec_supports_separator_and_timeout_aliases() {
    let payload_hex = hex::encode("payment-fail:ab57".as_bytes());
    let parsed = parse_payment_status_event_payload(&payload_hex).expect("event should parse");
    assert_eq!(parsed.payment_hash, "ab57");
    assert_eq!(parsed.status, "failed");

    let payload_hex = hex::encode("payment.timeout:ab58".as_bytes());
    let parsed = parse_payment_status_event_payload(&payload_hex).expect("event should parse");
    assert_eq!(parsed.payment_hash, "ab58");
    assert_eq!(parsed.status, "expired");

    let payload = serde_json::json!({
        "event": "payment-success",
        "payment_hash": "ab59",
    });
    let payload_hex = hex::encode(payload.to_string().as_bytes());
    let parsed = parse_payment_status_event_payload(&payload_hex).expect("event should parse");
    assert_eq!(parsed.payment_hash, "ab59");
    assert_eq!(parsed.status, "succeeded");
}

#[test]
fn payment_status_event_codec_supports_event_name_and_payment_id_aliases() {
    let payload = serde_json::json!({
        "eventName": "PaymentCompleted",
        "paymentId": "ab60",
    });
    let payload_hex = hex::encode(payload.to_string().as_bytes());
    let parsed = parse_payment_status_event_payload(&payload_hex).expect("event should parse");
    assert_eq!(parsed.payment_hash, "ab60");
    assert_eq!(parsed.status, "succeeded");

    let payload = serde_json::json!({
        "event_name": "payment_timed_out",
        "payment_id": "ab61",
    });
    let payload_hex = hex::encode(payload.to_string().as_bytes());
    let parsed = parse_payment_status_event_payload(&payload_hex).expect("event should parse");
    assert_eq!(parsed.payment_hash, "ab61");
    assert_eq!(parsed.status, "expired");

    let payload_hex = hex::encode("payment_error:ab62".as_bytes());
    let parsed = parse_payment_status_event_payload(&payload_hex).expect("event should parse");
    assert_eq!(parsed.payment_hash, "ab62");
    assert_eq!(parsed.status, "failed");
}

#[test]
fn payment_status_event_codec_supports_json_status_aliases() {
    let payload = serde_json::json!({
        "payment_hash": "ab63",
        "status": "PaymentSent",
    });
    let payload_hex = hex::encode(payload.to_string().as_bytes());
    let parsed = parse_payment_status_event_payload(&payload_hex).expect("event should parse");
    assert_eq!(parsed.payment_hash, "ab63");
    assert_eq!(parsed.status, "succeeded");

    let payload = serde_json::json!({
        "payment_hash": "ab64",
        "status": "payment timed out",
    });
    let payload_hex = hex::encode(payload.to_string().as_bytes());
    let parsed = parse_payment_status_event_payload(&payload_hex).expect("event should parse");
    assert_eq!(parsed.payment_hash, "ab64");
    assert_eq!(parsed.status, "expired");

    let payload = serde_json::json!({
        "payment_hash": "ab65",
        "status": "payment_error",
    });
    let payload_hex = hex::encode(payload.to_string().as_bytes());
    let parsed = parse_payment_status_event_payload(&payload_hex).expect("event should parse");
    assert_eq!(parsed.payment_hash, "ab65");
    assert_eq!(parsed.status, "failed");
}

#[test]
fn payment_status_event_codec_supports_json_status_field_aliases() {
    let payload = serde_json::json!({
        "payment_hash": "ab66",
        "state": "PaymentSent",
    });
    let payload_hex = hex::encode(payload.to_string().as_bytes());
    let parsed = parse_payment_status_event_payload(&payload_hex).expect("event should parse");
    assert_eq!(parsed.payment_hash, "ab66");
    assert_eq!(parsed.status, "succeeded");

    let payload = serde_json::json!({
        "payment_hash": "ab67",
        "payment_status": "payment timed out",
    });
    let payload_hex = hex::encode(payload.to_string().as_bytes());
    let parsed = parse_payment_status_event_payload(&payload_hex).expect("event should parse");
    assert_eq!(parsed.payment_hash, "ab67");
    assert_eq!(parsed.status, "expired");

    let payload = serde_json::json!({
        "payment_hash": "ab68",
        "paymentStatus": "payment_error",
    });
    let payload_hex = hex::encode(payload.to_string().as_bytes());
    let parsed = parse_payment_status_event_payload(&payload_hex).expect("event should parse");
    assert_eq!(parsed.payment_hash, "ab68");
    assert_eq!(parsed.status, "failed");
}

#[test]
fn bridge_backend_list_peers_reads_runtime_state_contract() {
    let node = RlnWasmNode::new_with_runtime_backend(
        "ws://proxy.example".to_string(),
        "wasm_native_ldk".to_string(),
    )
    .expect("node should build");
    node.ldk_runtime.upsert_peer(LdkRuntimePeerStateData {
        pubkey: "0334cc4bca04ce3d1537310f55e91ec4cec7e5a88fa0fba20a24cce1fe6de2a2b0".to_string(),
        peer_addr: "127.0.0.1:9735".to_string(),
        started: true,
    });

    let peers_js = node.list_peers_value().expect("list peers");
    let peers: serde_json::Value = crate::js_from(peers_js).expect("parse peers");
    let peers = peers.as_array().expect("peers array");
    assert_eq!(peers.len(), 1);
    assert_eq!(
        peers[0]["pubkey"],
        "0334cc4bca04ce3d1537310f55e91ec4cec7e5a88fa0fba20a24cce1fe6de2a2b0"
    );
    assert_eq!(peers[0]["peer_addr"], "127.0.0.1:9735");
    assert!(peers[0]["started"].is_boolean());
}

#[test]
fn bridge_backend_channel_views_use_runtime_state_contract() {
    let node = RlnWasmNode::new_with_runtime_backend(
        "ws://proxy.example".to_string(),
        "wasm_native_ldk".to_string(),
    )
    .expect("node should build");
    node.ldk_runtime.upsert_channel(LdkRuntimeChannelStateData {
        temporary_channel_id: "tmp-1".to_string(),
        channel_id: "chan-1".to_string(),
        peer_pubkey: "0334cc4bca04ce3d1537310f55e91ec4cec7e5a88fa0fba20a24cce1fe6de2a2b0"
            .to_string(),
        status: "pending".to_string(),
        ready: false,
        is_usable: false,
        public: false,
        capacity_sat: SDK_OPENCHANNEL_MIN_SAT,
        asset_id: None,
        asset_local_amount: None,
        virtual_open_mode: None,
    });

    let channels_js = node.list_channels_value().expect("list channels");
    let channels: serde_json::Value = crate::js_from(channels_js).expect("parse channels");
    let channels = channels.as_array().expect("channels array");
    assert_eq!(channels.len(), 1);
    assert_eq!(channels[0]["channel_id"], "chan-1");
    assert_eq!(channels[0]["status"], "pending");

    let channel_id = node
        .get_channel_id("tmp-1".to_string())
        .expect("temporary channel lookup");
    assert_eq!(channel_id, "chan-1");

    assert!(
        node.apply_runtime_transport_event(&RuntimeTransportEvent::ChannelUsable {
            channel_id: "chan-1".to_string(),
        })
    );

    let updated_js = node.list_channels_value().expect("list channels");
    let updated: serde_json::Value = crate::js_from(updated_js).expect("parse channels");
    let updated = updated.as_array().expect("channels array");
    assert_eq!(updated[0]["status"], "opened");
    assert_eq!(updated[0]["is_usable"], true);
    assert_eq!(updated[0]["ready"], true);
}

#[test]
fn bridge_backend_payment_views_use_runtime_state_contract() {
    let node = RlnWasmNode::new_with_runtime_backend(
        "ws://proxy.example".to_string(),
        "wasm_native_ldk".to_string(),
    )
    .expect("node should build");
    node.ldk_runtime.upsert_payment(LdkRuntimePaymentStateData {
        amt_msat: Some(5_000),
        asset_amount: None,
        asset_id: None,
        payment_hash: "pay-1".to_string(),
        inbound: false,
        status: "pending".to_string(),
        invoice_type: None,
        preimage: None,
        created_at: 5,
        updated_at: 5,
        payee_pubkey: "0334cc4bca04ce3d1537310f55e91ec4cec7e5a88fa0fba20a24cce1fe6de2a2b0"
            .to_string(),
    });

    let payments_js = node.list_payments_value().expect("list payments");
    let payments: serde_json::Value = crate::js_from(payments_js).expect("parse payments");
    let payments = payments.as_array().expect("payments array");
    assert_eq!(payments.len(), 1);
    assert_eq!(payments[0]["payment_hash"], "pay-1");
    assert_eq!(payments[0]["status"], "pending");

    let payment_js = node
        .get_payment_value("pay-1".to_string())
        .expect("get payment");
    let payment: serde_json::Value = crate::js_from(payment_js).expect("parse payment");
    assert_eq!(payment["payment_hash"], "pay-1");
    assert_eq!(payment["status"], "pending");
}

#[test]
fn bridge_backend_ingest_event_syncs_runtime_payment_state_contract() {
    let node = RlnWasmNode::new_with_runtime_backend(
        "ws://proxy.example".to_string(),
        "wasm_native_ldk".to_string(),
    )
    .expect("node should build");
    node.ldk_runtime.upsert_payment(LdkRuntimePaymentStateData {
        amt_msat: Some(7_000),
        asset_amount: None,
        asset_id: None,
        payment_hash: "pay-sync".to_string(),
        inbound: false,
        status: "pending".to_string(),
        invoice_type: None,
        preimage: None,
        created_at: 7,
        updated_at: 7,
        payee_pubkey: "0334cc4bca04ce3d1537310f55e91ec4cec7e5a88fa0fba20a24cce1fe6de2a2b0"
            .to_string(),
    });

    let payload_hex = encode_payment_status_event_payload("pay-sync", "succeeded");
    let _ = node
        .ingest_read_event_payload_hex(payload_hex)
        .expect("ingest should succeed");

    let runtime_payment = node
        .ldk_runtime
        .get_payment("pay-sync")
        .expect("runtime payment");
    assert_eq!(runtime_payment.status, "succeeded");
}

#[test]
fn bridge_backend_fail_pending_syncs_runtime_payment_state_contract() {
    let node = RlnWasmNode::new_with_runtime_backend(
        "ws://proxy.example".to_string(),
        "wasm_native_ldk".to_string(),
    )
    .expect("node should build");
    node.ldk_runtime.upsert_payment(LdkRuntimePaymentStateData {
        amt_msat: Some(8_000),
        asset_amount: None,
        asset_id: None,
        payment_hash: "pay-fail".to_string(),
        inbound: false,
        status: "pending".to_string(),
        invoice_type: None,
        preimage: None,
        created_at: 8,
        updated_at: 8,
        payee_pubkey: "0334cc4bca04ce3d1537310f55e91ec4cec7e5a88fa0fba20a24cce1fe6de2a2b0"
            .to_string(),
    });

    let _ = node.fail_pending_payments_api().expect("fail pending");

    let runtime_payment = node
        .ldk_runtime
        .get_payment("pay-fail")
        .expect("runtime payment");
    assert_eq!(runtime_payment.status, "failed");
}

#[test]
fn bridge_backend_disconnect_peer_without_local_session_contract() {
    let node = RlnWasmNode::new_with_runtime_backend(
        "ws://proxy.example".to_string(),
        "wasm_native_ldk".to_string(),
    )
    .expect("node should build");
    let pubkey = "0334cc4bca04ce3d1537310f55e91ec4cec7e5a88fa0fba20a24cce1fe6de2a2b0";
    node.ldk_runtime.upsert_peer(LdkRuntimePeerStateData {
        pubkey: pubkey.to_string(),
        peer_addr: "127.0.0.1:9735".to_string(),
        started: false,
    });
    node.ldk_runtime.upsert_channel(LdkRuntimeChannelStateData {
        temporary_channel_id: "tmp-disconnect".to_string(),
        channel_id: "chan-disconnect".to_string(),
        peer_pubkey: pubkey.to_string(),
        status: "opened".to_string(),
        ready: true,
        is_usable: true,
        public: false,
        capacity_sat: SDK_OPENCHANNEL_MIN_SAT,
        asset_id: None,
        asset_local_amount: None,
        virtual_open_mode: None,
    });

    block_on(node.disconnect_peer(pubkey.to_string())).expect("disconnect should succeed");

    assert!(!node.ldk_runtime.has_peer(pubkey));
    assert!(node
        .ldk_runtime
        .list_channels()
        .iter()
        .all(|ch| ch.peer_pubkey != pubkey));
}

#[test]
fn bridge_backend_close_all_peers_clears_runtime_peers_without_sessions_contract() {
    let node = RlnWasmNode::new_with_runtime_backend(
        "ws://proxy.example".to_string(),
        "wasm_native_ldk".to_string(),
    )
    .expect("node should build");
    for (peer_pubkey, channel_id) in [
        (
            "0334cc4bca04ce3d1537310f55e91ec4cec7e5a88fa0fba20a24cce1fe6de2a2b0",
            "chan-close-1",
        ),
        (
            "0334cc4bca04ce3d1537310f55e91ec4cec7e5a88fa0fba20a24cce1fe6de2a2b0",
            "chan-close-2",
        ),
    ] {
        node.ldk_runtime.upsert_peer(LdkRuntimePeerStateData {
            pubkey: peer_pubkey.to_string(),
            peer_addr: "127.0.0.1:9735".to_string(),
            started: false,
        });
        node.ldk_runtime.upsert_channel(LdkRuntimeChannelStateData {
            temporary_channel_id: format!("tmp-{channel_id}"),
            channel_id: channel_id.to_string(),
            peer_pubkey: peer_pubkey.to_string(),
            status: "opened".to_string(),
            ready: true,
            is_usable: true,
            public: false,
            capacity_sat: SDK_OPENCHANNEL_MIN_SAT,
            asset_id: None,
            asset_local_amount: None,
            virtual_open_mode: None,
        });
    }

    block_on(node.close_all_peers()).expect("close all peers should succeed");

    assert!(node.ldk_runtime.list_peers().is_empty());
    assert!(node.ldk_runtime.list_channels().is_empty());
}

#[test]
fn bridge_backend_runtime_state_restores_across_node_instances_contract() {
    crate::ldk_runtime::test_utils::reset_runtime_storage_for_tests();
    let proxy = "ws://proxy.restore.example".to_string();

    let node_a = RlnWasmNode::new(proxy.clone()).expect("node should build");
    node_a.ensure_runtime_ready().expect("runtime should start");
    node_a.ldk_runtime.upsert_peer(LdkRuntimePeerStateData {
        pubkey: "0334cc4bca04ce3d1537310f55e91ec4cec7e5a88fa0fba20a24cce1fe6de2a2b0".to_string(),
        peer_addr: "127.0.0.1:9735".to_string(),
        started: true,
    });
    node_a
        .ldk_runtime
        .upsert_channel(LdkRuntimeChannelStateData {
            temporary_channel_id: "tmp-restore".to_string(),
            channel_id: "chan-restore".to_string(),
            peer_pubkey: "0334cc4bca04ce3d1537310f55e91ec4cec7e5a88fa0fba20a24cce1fe6de2a2b0"
                .to_string(),
            status: "opened".to_string(),
            ready: true,
            is_usable: true,
            public: false,
            capacity_sat: SDK_OPENCHANNEL_MIN_SAT,
            asset_id: None,
            asset_local_amount: None,
            virtual_open_mode: None,
        });
    node_a
        .ldk_runtime
        .upsert_payment(LdkRuntimePaymentStateData {
            amt_msat: Some(SDK_HTLC_MIN_MSAT),
            asset_amount: None,
            asset_id: None,
            payment_hash: "pay-restore".to_string(),
            inbound: false,
            status: "succeeded".to_string(),
            invoice_type: None,
            preimage: None,
            created_at: 10,
            updated_at: 11,
            payee_pubkey: "0334cc4bca04ce3d1537310f55e91ec4cec7e5a88fa0fba20a24cce1fe6de2a2b0"
                .to_string(),
        });

    let node_b = RlnWasmNode::new(proxy).expect("node should build");
    node_b
        .ensure_runtime_ready()
        .expect("runtime should restore");

    let peers: serde_json::Value =
        crate::js_from(node_b.list_peers_value().expect("list peers")).expect("peers parse");
    assert_eq!(peers.as_array().map(|a| a.len()), Some(1));
    assert_eq!(
        peers[0]["pubkey"],
        "0334cc4bca04ce3d1537310f55e91ec4cec7e5a88fa0fba20a24cce1fe6de2a2b0"
    );

    let channels: serde_json::Value =
        crate::js_from(node_b.list_channels_value().expect("list channels"))
            .expect("channels parse");
    assert_eq!(channels.as_array().map(|a| a.len()), Some(1));
    assert_eq!(channels[0]["channel_id"], "chan-restore");
    assert_eq!(channels[0]["status"], "opened");

    let payments: serde_json::Value =
        crate::js_from(node_b.list_payments_value().expect("list payments"))
            .expect("payments parse");
    assert_eq!(payments.as_array().map(|a| a.len()), Some(1));
    assert_eq!(payments[0]["payment_hash"], "pay-restore");
    assert_eq!(payments[0]["status"], "succeeded");
}

#[test]
fn bridge_backend_restore_requires_peer_reconnect_before_open_channel_contract() {
    crate::ldk_runtime::test_utils::reset_runtime_storage_for_tests();
    let proxy = "ws://proxy.reconnect.example".to_string();
    let peer_pubkey =
        "0334cc4bca04ce3d1537310f55e91ec4cec7e5a88fa0fba20a24cce1fe6de2a2b0".to_string();

    let node_a = RlnWasmNode::new(proxy.clone()).expect("node should build");
    node_a.ensure_runtime_ready().expect("runtime should start");
    node_a.ldk_runtime.upsert_peer(LdkRuntimePeerStateData {
        pubkey: peer_pubkey.clone(),
        peer_addr: "127.0.0.1:9735".to_string(),
        started: true,
    });

    let node_b = RlnWasmNode::new(proxy).expect("node should build");
    node_b
        .ensure_runtime_ready()
        .expect("runtime should restore");

    let peers_js = node_b.list_peers_value().expect("list peers");
    let peers: serde_json::Value = crate::js_from(peers_js).expect("parse peers");
    let peers = peers.as_array().expect("peers array");
    assert_eq!(peers.len(), 1);
    assert_eq!(peers[0]["pubkey"], peer_pubkey);
    assert_eq!(peers[0]["started"], false);

    let err = node_b
        .open_channel_value(
            peer_pubkey.clone(),
            SDK_OPENCHANNEL_MIN_SAT,
            false,
            None,
            None,
        )
        .expect_err("open must fail before reconnect");
    assert_eq!(err.as_string().unwrap_or_default(), "peer is not connected");

    assert!(node_b.ldk_runtime.set_peer_started(&peer_pubkey, true));
    let opened_js = node_b
        .open_channel_value(peer_pubkey, SDK_OPENCHANNEL_MIN_SAT, false, None, None)
        .expect("open should succeed after reconnect");
    let opened: serde_json::Value = crate::js_from(opened_js).expect("parse opened");
    assert_eq!(opened["status"], "opened");
}

#[test]
fn bridge_backend_restore_disconnected_peer_forces_send_payment_failure_until_reconnect() {
    crate::ldk_runtime::test_utils::reset_runtime_storage_for_tests();
    let proxy = "ws://proxy.payment-reconnect.example".to_string();
    let peer_pubkey =
        "0334cc4bca04ce3d1537310f55e91ec4cec7e5a88fa0fba20a24cce1fe6de2a2b0".to_string();

    let node_a = RlnWasmNode::new(proxy.clone()).expect("node should build");
    node_a.ensure_runtime_ready().expect("runtime should start");
    node_a.ldk_runtime.upsert_peer(LdkRuntimePeerStateData {
        pubkey: peer_pubkey.clone(),
        peer_addr: "127.0.0.1:9735".to_string(),
        started: true,
    });

    let node_b = RlnWasmNode::new(proxy).expect("node should build");
    node_b
        .ensure_runtime_ready()
        .expect("runtime should restore");
    assert_eq!(
        node_b.ldk_runtime.get_peer(&peer_pubkey).map(|p| p.started),
        Some(false)
    );

    let invoice_json = node_b
        .create_ln_invoice_json(Some(SDK_INVOICE_MIN_MSAT), 3600, None, None)
        .expect("create invoice");
    let invoice_doc: serde_json::Value =
        serde_json::from_str(&invoice_json).expect("parse invoice");
    let invoice = invoice_doc["invoice"]
        .as_str()
        .expect("invoice")
        .to_string();

    let first_send = node_b
        .send_payment_value(invoice.clone(), Some(SDK_INVOICE_MIN_MSAT), None, None)
        .expect("send should complete with failed status");
    let first_doc: serde_json::Value = crate::js_from(first_send).expect("parse send");
    assert_eq!(first_doc["status"], "failed");

    assert!(node_b.ldk_runtime.set_peer_started(&peer_pubkey, true));
    let second_send = node_b
        .send_payment_value(invoice, Some(SDK_INVOICE_MIN_MSAT), None, None)
        .expect("send should succeed after reconnect");
    let second_doc: serde_json::Value = crate::js_from(second_send).expect("parse send");
    assert_eq!(second_doc["status"], "pending");
}

#[test]
fn bridge_backend_restore_disconnected_peer_forces_keysend_failure_until_reconnect() {
    crate::ldk_runtime::test_utils::reset_runtime_storage_for_tests();
    let proxy = "ws://proxy.keysend-reconnect.example".to_string();
    let peer_pubkey =
        "0334cc4bca04ce3d1537310f55e91ec4cec7e5a88fa0fba20a24cce1fe6de2a2b0".to_string();

    let node_a = RlnWasmNode::new(proxy.clone()).expect("node should build");
    node_a.ensure_runtime_ready().expect("runtime should start");
    node_a.ldk_runtime.upsert_peer(LdkRuntimePeerStateData {
        pubkey: peer_pubkey.clone(),
        peer_addr: "127.0.0.1:9735".to_string(),
        started: true,
    });

    let node_b = RlnWasmNode::new(proxy).expect("node should build");
    node_b
        .ensure_runtime_ready()
        .expect("runtime should restore");
    assert_eq!(
        node_b.ldk_runtime.get_peer(&peer_pubkey).map(|p| p.started),
        Some(false)
    );

    let first = node_b
        .keysend_value(peer_pubkey.clone(), SDK_HTLC_MIN_MSAT, None, None)
        .expect("keysend should complete with failed status");
    let first_doc: serde_json::Value = crate::js_from(first).expect("parse keysend");
    assert_eq!(first_doc["status"], "failed");

    assert!(node_b.ldk_runtime.set_peer_started(&peer_pubkey, true));
    let second = node_b
        .keysend_value(peer_pubkey, SDK_HTLC_MIN_MSAT, None, None)
        .expect("keysend should stay pending after reconnect");
    let second_doc: serde_json::Value = crate::js_from(second).expect("parse keysend");
    assert_eq!(second_doc["status"], "pending");
}

#[test]
fn bridge_backend_trusted_virtual_keysend_finalizes_via_runtime_virtual_payment_engine_event() {
    crate::ldk_runtime::test_utils::reset_runtime_storage_for_tests();
    let proxy = "ws://proxy.virtual-keysend-event.example".to_string();
    let peer_pubkey =
        "0334cc4bca04ce3d1537310f55e91ec4cec7e5a88fa0fba20a24cce1fe6de2a2b0".to_string();

    let node = RlnWasmNode::new(proxy).expect("node should build");
    node.ensure_runtime_ready().expect("runtime should start");
    node.ldk_runtime.upsert_peer(LdkRuntimePeerStateData {
        pubkey: peer_pubkey.clone(),
        peer_addr: "127.0.0.1:9735".to_string(),
        started: true,
    });
    assert!(node.test_set_runtime_peer_started(&peer_pubkey, true));

    let opened_js = node
        .open_channel_value_with_options(
            peer_pubkey.clone(),
            SDK_OPENCHANNEL_MIN_SAT,
            false,
            None,
            None,
            Some("trusted_no_broadcast".to_string()),
            None,
            None,
        )
        .expect("open channel");
    let opened: serde_json::Value = crate::js_from(opened_js).expect("parse opened");
    assert_eq!(
        opened["virtual_open_mode"],
        serde_json::Value::String("trusted_no_broadcast".to_string())
    );

    let keysend_js = node
        .keysend_value(peer_pubkey, SDK_HTLC_MIN_MSAT, None, None)
        .expect("keysend");
    let keysend: serde_json::Value = crate::js_from(keysend_js).expect("parse keysend");
    assert_eq!(keysend["status"], "succeeded");
    let payment_hash = keysend["payment_hash"]
        .as_str()
        .expect("payment_hash")
        .to_string();

    let events_before: serde_json::Value =
        serde_json::from_str(&node.list_runtime_events_json().expect("events before"))
            .expect("parse events before");
    let events_before = events_before.as_array().expect("events before array");
    assert!(events_before.iter().any(|event| {
        event.get("source").and_then(|value| value.as_str())
            == Some("runtime_virtual_payment_engine")
            && event.get("event_kind").and_then(|value| value.as_str()) == Some("payment_status")
            && event.get("payment_hash").and_then(|value| value.as_str()) == Some(&payment_hash)
            && event.get("status").and_then(|value| value.as_str()) == Some("succeeded")
            && event.get("applied").and_then(|value| value.as_bool()) == Some(true)
    }));
}

#[test]
fn wasm_two_node_handles_same_proxy_distinct_runtime_ids_contract() {
    crate::ldk_runtime::test_utils::reset_runtime_storage_for_tests();
    let proxy = "ws://proxy.runtime-id-isolation.example".to_string();

    let node_a = RlnWasmNode::new_with_runtime_id_opt(proxy.clone(), Some("node-a".to_string()), None)
        .expect("node A should build");
    let node_b = RlnWasmNode::new_with_runtime_id_opt(proxy, Some("node-b".to_string()), None)
        .expect("node B should build");

    node_a.ensure_runtime_ready().expect("node A runtime");
    node_b.ensure_runtime_ready().expect("node B runtime");

    let components_a: serde_json::Value = crate::js_from(
        node_a
            .ldk_runtime_components_value()
            .expect("node A components"),
    )
    .expect("parse node A components");
    let components_b: serde_json::Value = crate::js_from(
        node_b
            .ldk_runtime_components_value()
            .expect("node B components"),
    )
    .expect("parse node B components");

    assert_ne!(
        components_a["key_manager_fingerprint"],
        components_b["key_manager_fingerprint"]
    );
}

#[test]
fn wasm_signing_identity_differs_by_runtime_id_contract() {
    crate::ldk_runtime::test_utils::reset_runtime_storage_for_tests();
    let proxy = "ws://proxy.signing-identity-runtime-id.example".to_string();

    let node_a = RlnWasmNode::new_with_runtime_id_opt(proxy.clone(), Some("node-a".to_string()), None)
        .expect("node A should build");
    let node_b = RlnWasmNode::new_with_runtime_id_opt(proxy, Some("node-b".to_string()), None)
        .expect("node B should build");

    let invoice_a_doc: serde_json::Value = serde_json::from_str(
        &node_a
            .create_ln_invoice_json(Some(SDK_INVOICE_MIN_MSAT), 3600, None, None)
            .expect("node A invoice"),
    )
    .expect("parse node A invoice");
    let invoice_b_doc: serde_json::Value = serde_json::from_str(
        &node_b
            .create_ln_invoice_json(Some(SDK_INVOICE_MIN_MSAT), 3600, None, None)
            .expect("node B invoice"),
    )
    .expect("parse node B invoice");

    let decoded_a: serde_json::Value = crate::js_from(
        node_a
            .decode_ln_invoice_value(
                invoice_a_doc["invoice"]
                    .as_str()
                    .expect("invoice A string")
                    .to_string(),
            )
            .expect("decode A"),
    )
    .expect("parse decoded A");
    let decoded_b: serde_json::Value = crate::js_from(
        node_b
            .decode_ln_invoice_value(
                invoice_b_doc["invoice"]
                    .as_str()
                    .expect("invoice B string")
                    .to_string(),
            )
            .expect("decode B"),
    )
    .expect("parse decoded B");

    assert_ne!(decoded_a["payee_pubkey"], decoded_b["payee_pubkey"]);
}

#[test]
fn wasm_channel_payment_state_does_not_cross_runtime_ids_contract() {
    crate::ldk_runtime::test_utils::reset_runtime_storage_for_tests();
    let proxy = "ws://proxy.runtime-id-state-separation.example".to_string();
    let peer_pubkey =
        "0334cc4bca04ce3d1537310f55e91ec4cec7e5a88fa0fba20a24cce1fe6de2a2b0".to_string();

    let node_a = RlnWasmNode::new_with_runtime_id_opt(proxy.clone(), Some("node-a".to_string()), None)
        .expect("node A should build");
    let node_b = RlnWasmNode::new_with_runtime_id_opt(proxy, Some("node-b".to_string()), None)
        .expect("node B should build");

    node_a.ensure_runtime_ready().expect("node A runtime");
    node_b.ensure_runtime_ready().expect("node B runtime");
    node_a.ldk_runtime.upsert_peer(LdkRuntimePeerStateData {
        pubkey: peer_pubkey.clone(),
        peer_addr: "127.0.0.1:9735".to_string(),
        started: true,
    });

    let opened: serde_json::Value = crate::js_from(
        node_a
            .open_channel_value(
                peer_pubkey.clone(),
                SDK_OPENCHANNEL_MIN_SAT,
                false,
                None,
                None,
            )
            .expect("open channel on node A"),
    )
    .expect("parse opened");
    assert_eq!(opened["status"], "opened");

    let keysend: serde_json::Value = crate::js_from(
        node_a
            .keysend_value(peer_pubkey, SDK_HTLC_MIN_MSAT, None, None)
            .expect("keysend on node A"),
    )
    .expect("parse keysend");
    assert_eq!(keysend["status"], "pending");

    let channels_b: serde_json::Value =
        crate::js_from(node_b.list_channels_value().expect("list channels B"))
            .expect("parse channels B");
    let payments_b: serde_json::Value =
        crate::js_from(node_b.list_payments_value().expect("list payments B"))
            .expect("parse payments B");

    assert_eq!(channels_b.as_array().map(|a| a.len()), Some(0));
    assert_eq!(payments_b.as_array().map(|a| a.len()), Some(0));
}

#[cfg(target_arch = "wasm32")]
#[test]
fn bridge_backend_channel_api_open_get_list_close_contract() {
    let node = RlnWasmNode::new_with_runtime_backend(
        "ws://proxy.example".to_string(),
        "wasm_native_ldk".to_string(),
    )
    .expect("node should build");
    node.ensure_runtime_ready().expect("runtime should start");
    let peer_pubkey =
        "0334cc4bca04ce3d1537310f55e91ec4cec7e5a88fa0fba20a24cce1fe6de2a2b0".to_string();
    node.ldk_runtime.upsert_peer(LdkRuntimePeerStateData {
        pubkey: peer_pubkey.clone(),
        peer_addr: "127.0.0.1:9735".to_string(),
        started: true,
    });
    assert_eq!(
        node.ldk_runtime.get_peer(&peer_pubkey).map(|p| p.started),
        Some(true)
    );

    let opened_json = node
        .open_channel_json(peer_pubkey, SDK_OPENCHANNEL_MIN_SAT, false, None, None)
        .expect("open channel");
    let opened: serde_json::Value = serde_json::from_str(&opened_json).expect("opened parse");
    let temporary_channel_id = opened["temporary_channel_id"]
        .as_str()
        .expect("temporary_channel_id")
        .to_string();
    let channel_id = opened["channel_id"]
        .as_str()
        .expect("channel_id")
        .to_string();
    assert_eq!(opened["status"], "opened");
    assert_eq!(opened["is_usable"], true);

    let resolved_channel_id = node
        .get_channel_id(temporary_channel_id)
        .expect("resolve channel id");
    assert_eq!(resolved_channel_id, channel_id);

    let listed_json = node.list_channels_json().expect("list channels");
    let listed: serde_json::Value = serde_json::from_str(&listed_json).expect("channels parse");
    let listed = listed.as_array().expect("channels array");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0]["channel_id"], channel_id);
    assert_eq!(listed[0]["status"], "opened");

    node.close_channel(channel_id.clone())
        .expect("close channel");
    let after_close_json = node.list_channels_json().expect("list channels");
    let after_close: serde_json::Value =
        serde_json::from_str(&after_close_json).expect("channels parse");
    let after_close = after_close.as_array().expect("channels array");
    assert!(after_close.is_empty());
    assert!(node
        .ldk_runtime
        .list_channels()
        .into_iter()
        .all(|ch| ch.channel_id != channel_id));
}

#[test]
fn runtime_lock_blocks_peer_channel_surfaces_contract() {
    crate::ldk_runtime::test_utils::reset_runtime_storage_for_tests();
    crate::ldk_runtime::set_runtime_session_initialized(true);
    crate::ldk_runtime::set_runtime_session_authorized(false);

    let node = RlnWasmNode::new_with_runtime_backend(
        "ws://proxy.example".to_string(),
        "wasm_native_ldk".to_string(),
    )
    .expect("node should build");

    let err = node.list_peers_value().expect_err("must fail");
    assert_eq!(
        err.as_string().unwrap_or_default(),
        "runtime session is locked; call unlock first"
    );

    let err = node.list_channels_value().expect_err("must fail");
    assert_eq!(
        err.as_string().unwrap_or_default(),
        "runtime session is locked; call unlock first"
    );

    let err = node
        .get_channel_id("tmp-1".to_string())
        .expect_err("must fail");
    assert_eq!(
        err.as_string().unwrap_or_default(),
        "runtime session is locked; call unlock first"
    );

    let err = node
        .close_channel("chan-1".to_string())
        .expect_err("must fail");
    assert_eq!(
        err.as_string().unwrap_or_default(),
        "runtime session is locked; call unlock first"
    );

    let err = block_on(node.disconnect_peer(
        "0334cc4bca04ce3d1537310f55e91ec4cec7e5a88fa0fba20a24cce1fe6de2a2b0".to_string(),
    ))
    .expect_err("must fail");
    assert_eq!(
        err.as_string().unwrap_or_default(),
        "runtime session is locked; call unlock first"
    );

    let err = block_on(node.close_all_peers()).expect_err("must fail");
    assert_eq!(
        err.as_string().unwrap_or_default(),
        "runtime session is locked; call unlock first"
    );

    crate::ldk_runtime::set_runtime_session_authorized(true);
}

#[test]
fn runtime_lock_blocks_network_info_contract() {
    crate::ldk_runtime::test_utils::reset_runtime_storage_for_tests();
    crate::ldk_runtime::set_runtime_session_initialized(true);
    crate::ldk_runtime::set_runtime_session_authorized(false);
    let node = RlnWasmNode::new_with_runtime_backend(
        "ws://proxy.example".to_string(),
        "wasm_native_ldk".to_string(),
    )
    .expect("node should build");

    let err = node.network_info_value().expect_err("must fail");
    assert_eq!(
        err.as_string().unwrap_or_default(),
        "runtime session is locked; call unlock first"
    );
    crate::ldk_runtime::set_runtime_session_authorized(true);
}

#[test]
fn runtime_lock_blocks_node_info_contract() {
    crate::ldk_runtime::test_utils::reset_runtime_storage_for_tests();
    crate::ldk_runtime::set_runtime_session_initialized(true);
    crate::ldk_runtime::set_runtime_session_authorized(false);
    let node = RlnWasmNode::new_with_runtime_backend(
        "ws://proxy.example".to_string(),
        "wasm_native_ldk".to_string(),
    )
    .expect("node should build");

    let err = node.node_info_value().expect_err("must fail");
    assert_eq!(
        err.as_string().unwrap_or_default(),
        "runtime session is locked; call unlock first"
    );
    crate::ldk_runtime::set_runtime_session_authorized(true);
}

#[test]
fn fail_pending_payments_uses_payment_status_runtime_events() {
    let payments = Rc::new(RefCell::new(HashMap::new()));
    let runtime_events = Rc::new(RefCell::new(Vec::new()));
    let next_runtime_event_seq = Rc::new(RefCell::new(0));

    payments.borrow_mut().insert(
        "p1".to_string(),
        PaymentEntry {
            data: RlnWasmNodePaymentData {
                amt_msat: Some(1_000),
                asset_amount: None,
                asset_id: None,
                payment_hash: "p1".to_string(),
                inbound: false,
                status: "pending".to_string(),
                invoice_type: None,
                preimage: None,
                created_at: 1,
                updated_at: 1,
                payee_pubkey: "peer1".to_string(),
            },
        },
    );
    payments.borrow_mut().insert(
        "p2".to_string(),
        PaymentEntry {
            data: RlnWasmNodePaymentData {
                amt_msat: Some(2_000),
                asset_amount: None,
                asset_id: None,
                payment_hash: "p2".to_string(),
                inbound: false,
                status: "succeeded".to_string(),
                invoice_type: None,
                preimage: None,
                created_at: 1,
                updated_at: 1,
                payee_pubkey: "peer2".to_string(),
            },
        },
    );

    let applied = fail_pending_payments_with_runtime_events(
        &payments,
        &runtime_events,
        &next_runtime_event_seq,
        "manual_api",
        "failed",
    )
    .expect("pending payments transition should succeed");
    assert_eq!(applied, 1);

    let guard = payments.borrow();
    assert_eq!(
        guard.get("p1").map(|v| v.data.status.as_str()),
        Some("failed")
    );
    assert_eq!(
        guard.get("p2").map(|v| v.data.status.as_str()),
        Some("succeeded")
    );
    drop(guard);

    let events = runtime_events.borrow();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].source, "manual_api");
    assert_eq!(events[0].event_kind, "payment_status");
    assert!(events[0].applied);
    assert_eq!(events[0].payment_hash.as_deref(), Some("p1"));
    assert_eq!(events[0].status.as_deref(), Some("failed"));
}

#[test]
fn channel_transport_events_update_node_channel_state_and_logs() {
    let node = RlnWasmNode::new("ws://proxy.example".to_string()).expect("node should build");
    node.channels.borrow_mut().insert(
        "chan-1".to_string(),
        ChannelEntry {
            temporary_channel_id: "tmp-1".to_string(),
            data: RlnWasmNodeChannelData {
                temporary_channel_id: "tmp-1".to_string(),
                channel_id: "chan-1".to_string(),
                peer_pubkey: "peer-1".to_string(),
                status: "pending".to_string(),
                ready: false,
                is_usable: false,
                public: false,
                capacity_sat: 10_000,
                asset_id: None,
                asset_local_amount: None,
                virtual_open_mode: None,
            },
        },
    );

    let usable = node
        .apply_and_record_transport_event(
            RuntimeTransportEvent::ChannelUsable {
                channel_id: "chan-1".to_string(),
            },
            "test_api",
        )
        .expect("usable transition should succeed");
    assert!(usable.applied);

    let data = node
        .channels
        .borrow()
        .get("chan-1")
        .map(|v| v.data.clone())
        .expect("channel should exist");
    assert_eq!(data.status, "opened");
    assert!(data.ready);
    assert!(data.is_usable);

    let unusable = node
        .apply_and_record_transport_event(
            RuntimeTransportEvent::ChannelUnusable {
                channel_id: "chan-1".to_string(),
            },
            "test_api",
        )
        .expect("unusable transition should succeed");
    assert!(unusable.applied);

    let data = node
        .channels
        .borrow()
        .get("chan-1")
        .map(|v| v.data.clone())
        .expect("channel should still exist");
    assert_eq!(data.status, "pending");
    assert!(!data.ready);
    assert!(!data.is_usable);

    let closed = node
        .apply_and_record_transport_event(
            RuntimeTransportEvent::ChannelClosed {
                channel_id: "chan-1".to_string(),
            },
            "test_api",
        )
        .expect("close transition should succeed");
    assert!(closed.applied);
    assert!(!node.channels.borrow().contains_key("chan-1"));

    let events = node.runtime_events.borrow();
    assert_eq!(events.len(), 3);
    assert_eq!(events[0].event_kind, "channel_usable");
    assert_eq!(events[1].event_kind, "channel_unusable");
    assert_eq!(events[2].event_kind, "channel_closed");
    assert!(events.iter().all(|e| e.applied));
}

#[test]
fn peer_disconnected_event_cleans_stale_channels_without_peer_entry() {
    let node = RlnWasmNode::new("ws://proxy.example".to_string()).expect("node should build");
    node.channels.borrow_mut().insert(
        "chan-stale".to_string(),
        ChannelEntry {
            temporary_channel_id: "tmp-stale".to_string(),
            data: RlnWasmNodeChannelData {
                temporary_channel_id: "tmp-stale".to_string(),
                channel_id: "chan-stale".to_string(),
                peer_pubkey: "peer-stale".to_string(),
                status: "opened".to_string(),
                ready: true,
                is_usable: true,
                public: false,
                capacity_sat: 5_000,
                asset_id: None,
                asset_local_amount: None,
                virtual_open_mode: None,
            },
        },
    );

    let applied = node
        .apply_and_record_transport_event(
            RuntimeTransportEvent::PeerDisconnected {
                peer_pubkey: "peer-stale".to_string(),
            },
            "test_api",
        )
        .expect("event apply should succeed");
    assert!(applied.applied);
    assert!(!node.channels.borrow().contains_key("chan-stale"));

    let events = node.runtime_events.borrow();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].event_kind, "peer_disconnected");
    assert!(events[0].applied);
}

#[test]
fn payment_status_event_rejects_terminal_state_regression_and_logs_error() {
    let node = RlnWasmNode::new("ws://proxy.example".to_string()).expect("node should build");
    node.payments.borrow_mut().insert(
        "pay-terminal".to_string(),
        PaymentEntry {
            data: RlnWasmNodePaymentData {
                amt_msat: Some(1_000),
                asset_amount: None,
                asset_id: None,
                payment_hash: "pay-terminal".to_string(),
                inbound: false,
                status: "pending".to_string(),
                invoice_type: None,
                preimage: None,
                created_at: 1,
                updated_at: 1,
                payee_pubkey: "peer-x".to_string(),
            },
        },
    );

    let _ = node
        .apply_and_record_payment_status_event("pay-terminal", "succeeded", "test_api")
        .expect("pending->succeeded should be allowed");
    let err = node
        .apply_and_record_payment_status_event("pay-terminal", "failed", "test_api")
        .expect_err("succeeded->failed should be rejected");
    let msg = err.as_string().unwrap_or_default();
    assert!(msg.contains("invalid payment status transition"));

    let payment = node
        .payments
        .borrow()
        .get("pay-terminal")
        .map(|v| v.data.clone())
        .expect("payment should exist");
    assert_eq!(payment.status, "succeeded");

    let events = node.runtime_events.borrow();
    assert_eq!(events.len(), 2);
    assert!(events[0].applied);
    assert_eq!(events[0].status.as_deref(), Some("succeeded"));
    assert!(!events[1].applied);
    assert_eq!(events[1].status.as_deref(), Some("failed"));
    assert!(events[1]
        .error
        .as_deref()
        .unwrap_or_default()
        .contains("invalid payment status transition"));
}

#[test]
fn ingest_runtime_transport_event_value_applies_and_returns_contract_data() {
    let node = RlnWasmNode::new("ws://proxy.example".to_string()).expect("node should build");
    node.channels.borrow_mut().insert(
        "chan-ingest".to_string(),
        ChannelEntry {
            temporary_channel_id: "tmp-ingest".to_string(),
            data: RlnWasmNodeChannelData {
                temporary_channel_id: "tmp-ingest".to_string(),
                channel_id: "chan-ingest".to_string(),
                peer_pubkey: "peer-ingest".to_string(),
                status: "pending".to_string(),
                ready: false,
                is_usable: false,
                public: false,
                capacity_sat: 1_000,
                asset_id: None,
                asset_local_amount: None,
                virtual_open_mode: None,
            },
        },
    );

    let payload_hex = hex::encode("channel_usable:chan-ingest");
    let value = node
        .ingest_runtime_transport_event_payload_hex_value(payload_hex)
        .expect("transport ingest should succeed");
    let data: serde_json::Value = crate::js_from(value).expect("parse response");
    assert_eq!(data["event_kind"], "channel_usable");
    assert_eq!(data["applied"], true);

    let channel = node
        .channels
        .borrow()
        .get("chan-ingest")
        .map(|entry| entry.data.clone())
        .expect("channel should exist");
    assert_eq!(channel.status, "opened");
    assert!(channel.ready);
    assert!(channel.is_usable);
}

#[test]
fn ingest_runtime_transport_event_json_returns_json_payload() {
    let node = RlnWasmNode::new("ws://proxy.example".to_string()).expect("node should build");
    node.channels.borrow_mut().insert(
        "chan-json".to_string(),
        ChannelEntry {
            temporary_channel_id: "tmp-json".to_string(),
            data: RlnWasmNodeChannelData {
                temporary_channel_id: "tmp-json".to_string(),
                channel_id: "chan-json".to_string(),
                peer_pubkey: "peer-json".to_string(),
                status: "pending".to_string(),
                ready: false,
                is_usable: false,
                public: false,
                capacity_sat: 1_000,
                asset_id: None,
                asset_local_amount: None,
                virtual_open_mode: None,
            },
        },
    );

    let payload_hex = hex::encode("channel_closed:chan-json");
    let json = node
        .ingest_runtime_transport_event_payload_hex_json(payload_hex)
        .expect("transport ingest json should succeed");
    let data: serde_json::Value = serde_json::from_str(&json).expect("json parse");
    assert_eq!(data["event_kind"], "channel_closed");
    assert_eq!(data["applied"], true);
    assert!(!node.channels.borrow().contains_key("chan-json"));
}

#[test]
fn ingest_read_event_value_updates_payment_and_returns_payment_data() {
    let node = RlnWasmNode::new("ws://proxy.example".to_string()).expect("node should build");
    node.payments.borrow_mut().insert(
        "pay-ingest".to_string(),
        PaymentEntry {
            data: RlnWasmNodePaymentData {
                amt_msat: Some(2_500),
                asset_amount: None,
                asset_id: None,
                payment_hash: "pay-ingest".to_string(),
                inbound: false,
                status: "pending".to_string(),
                invoice_type: None,
                preimage: None,
                created_at: 10,
                updated_at: 10,
                payee_pubkey: "peer-pay".to_string(),
            },
        },
    );

    let payload_hex = encode_payment_status_event_payload("pay-ingest", "succeeded");
    let value = node
        .ingest_read_event_payload_hex(payload_hex)
        .expect("read event ingest should succeed");
    let payment: serde_json::Value = crate::js_from(value).expect("parse payment");
    assert_eq!(payment["payment_hash"], "pay-ingest");
    assert_eq!(payment["status"], "succeeded");

    let stored = node
        .payments
        .borrow()
        .get("pay-ingest")
        .map(|entry| entry.data.status.clone())
        .expect("stored payment should exist");
    assert_eq!(stored, "succeeded");
}

#[test]
fn ingest_read_event_json_and_invalid_transport_contracts() {
    let node = RlnWasmNode::new("ws://proxy.example".to_string()).expect("node should build");
    node.payments.borrow_mut().insert(
        "pay-json".to_string(),
        PaymentEntry {
            data: RlnWasmNodePaymentData {
                amt_msat: Some(3_300),
                asset_amount: None,
                asset_id: None,
                payment_hash: "pay-json".to_string(),
                inbound: true,
                status: "pending".to_string(),
                invoice_type: None,
                preimage: None,
                created_at: 15,
                updated_at: 15,
                payee_pubkey: "peer-payee".to_string(),
            },
        },
    );

    let payload_hex = encode_payment_status_event_payload("pay-json", "failed");
    let json = node
        .ingest_read_event_payload_hex_json(payload_hex)
        .expect("read event json should succeed");
    let payment: serde_json::Value = serde_json::from_str(&json).expect("json parse");
    assert_eq!(payment["payment_hash"], "pay-json");
    assert_eq!(payment["status"], "failed");

    let err = node
        .ingest_runtime_transport_event_payload_hex_value("zz-not-hex".to_string())
        .expect_err("invalid payload must fail");
    assert_eq!(
        err.as_string().unwrap_or_default(),
        "unrecognized transport event payload format"
    );
}

#[test]
fn keysend_rejects_amount_below_native_min_contract() {
    let node = RlnWasmNode::new("ws://proxy.example".to_string()).expect("node should build");
    let err = node
        .keysend_value(
            "0334cc4bca04ce3d1537310f55e91ec4cec7e5a88fa0fba20a24cce1fe6de2a2b0".to_string(),
            SDK_HTLC_MIN_MSAT - 1,
            None,
            None,
        )
        .expect_err("should fail");
    assert_eq!(
        err.as_string().unwrap_or_default(),
        format!("amt_msat cannot be less than {SDK_HTLC_MIN_MSAT}")
    );
}

#[test]
fn create_ln_invoice_rejects_rgb_below_native_min_contract() {
    let node = RlnWasmNode::new("ws://proxy.example".to_string()).expect("node should build");
    let asset_id = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".to_string();
    let err = node
        .create_ln_invoice_value(
            Some(SDK_INVOICE_MIN_MSAT - 1),
            3600,
            Some(asset_id),
            Some(1),
        )
        .expect_err("should fail");
    assert_eq!(
        err.as_string().unwrap_or_default(),
        format!(
            "amt_msat cannot be less than {SDK_INVOICE_MIN_MSAT} when transferring an RGB asset"
        )
    );
}

#[test]
fn send_payment_rejects_rgb_below_native_min_contract() {
    let node = RlnWasmNode::new("ws://proxy.example".to_string()).expect("node should build");
    let asset_id = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".to_string();
    let invoice = node
        .create_ln_invoice_json(Some(SDK_INVOICE_MIN_MSAT - 1), 3600, None, None)
        .expect("create invoice");
    let invoice_json: serde_json::Value = serde_json::from_str(&invoice).expect("parse json");
    let invoice_str = invoice_json["invoice"]
        .as_str()
        .expect("invoice string")
        .to_string();

    let err = node
        .send_payment_value(
            invoice_str,
            Some(SDK_INVOICE_MIN_MSAT - 1),
            Some(asset_id),
            Some(1),
        )
        .expect_err("should fail");
    assert_eq!(
        err.as_string().unwrap_or_default(),
        format!(
            "amt_msat in invoice sending an RGB asset cannot be less than {SDK_INVOICE_MIN_MSAT}"
        )
    );
}

#[test]
fn open_channel_rejects_capacity_outside_native_bounds_contract() {
    let node = RlnWasmNode::new("ws://proxy.example".to_string()).expect("node should build");
    let peer_pubkey =
        "0334cc4bca04ce3d1537310f55e91ec4cec7e5a88fa0fba20a24cce1fe6de2a2b0".to_string();

    let low = node
        .open_channel_value(
            peer_pubkey.clone(),
            SDK_OPENCHANNEL_MIN_SAT - 1,
            false,
            None,
            None,
        )
        .expect_err("should fail");
    assert_eq!(
        low.as_string().unwrap_or_default(),
        format!("Channel amount must be equal to or higher than {SDK_OPENCHANNEL_MIN_SAT} sats")
    );

    let high = node
        .open_channel_value(peer_pubkey, SDK_OPENCHANNEL_MAX_SAT + 1, false, None, None)
        .expect_err("should fail");
    assert_eq!(
        high.as_string().unwrap_or_default(),
        format!("Channel amount must be equal to or less than {SDK_OPENCHANNEL_MAX_SAT} sats")
    );
}

#[test]
fn open_channel_rejects_incomplete_rgb_pair_contract() {
    let node = RlnWasmNode::new("ws://proxy.example".to_string()).expect("node should build");
    let peer_pubkey =
        "0334cc4bca04ce3d1537310f55e91ec4cec7e5a88fa0fba20a24cce1fe6de2a2b0".to_string();
    let err = node
        .open_channel_value(
            peer_pubkey.clone(),
            SDK_OPENCHANNEL_MIN_SAT,
            false,
            Some("0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".to_string()),
            None,
        )
        .expect_err("should fail");
    assert_eq!(
        err.as_string().unwrap_or_default(),
        "asset_id and asset_local_amount must be provided together"
    );
}

#[test]
fn open_channel_rejects_rgb_amount_below_min_contract() {
    let node = RlnWasmNode::new("ws://proxy.example".to_string()).expect("node should build");
    let peer_pubkey =
        "0334cc4bca04ce3d1537310f55e91ec4cec7e5a88fa0fba20a24cce1fe6de2a2b0".to_string();
    let err = node
        .open_channel_value(
            peer_pubkey,
            SDK_OPENRGBCHANNEL_MIN_SAT,
            false,
            Some("0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".to_string()),
            Some(SDK_OPENCHANNEL_MIN_RGB_AMT - 1),
        )
        .expect_err("should fail");
    assert_eq!(
        err.as_string().unwrap_or_default(),
        format!("Channel RGB amount must be equal to or higher than {SDK_OPENCHANNEL_MIN_RGB_AMT}")
    );
}

#[test]
fn open_channel_rejects_rgb_capacity_below_min_contract() {
    let node = RlnWasmNode::new("ws://proxy.example".to_string()).expect("node should build");
    let peer_pubkey =
        "0334cc4bca04ce3d1537310f55e91ec4cec7e5a88fa0fba20a24cce1fe6de2a2b0".to_string();
    let err = node
        .open_channel_value(
            peer_pubkey,
            SDK_OPENRGBCHANNEL_MIN_SAT - 1,
            false,
            Some("0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".to_string()),
            Some(SDK_OPENCHANNEL_MIN_RGB_AMT),
        )
        .expect_err("should fail");
    assert_eq!(
        err.as_string().unwrap_or_default(),
        format!(
            "RGB channel amount must be equal to or higher than {SDK_OPENRGBCHANNEL_MIN_SAT} sats"
        )
    );
}

#[test]
#[cfg(target_arch = "wasm32")]
fn open_channel_rejects_unknown_virtual_mode_contract() {
    let node = RlnWasmNode::new("ws://proxy.example".to_string()).expect("node should build");
    let err = node
        .open_channel_value_with_options(
            "0334cc4bca04ce3d1537310f55e91ec4cec7e5a88fa0fba20a24cce1fe6de2a2b0".to_string(),
            SDK_OPENCHANNEL_MIN_SAT,
            false,
            None,
            None,
            Some("wrong_mode".to_string()),
            None,
            None,
        )
        .expect_err("should fail");
    assert_eq!(
        err.as_string().unwrap_or_default(),
        "unknown virtual_open_mode: wrong_mode"
    );
}

#[test]
#[cfg(target_arch = "wasm32")]
fn open_channel_rejects_virtual_public_contract() {
    let node = RlnWasmNode::new("ws://proxy.example".to_string()).expect("node should build");
    let err = node
        .open_channel_value_with_options(
            "0334cc4bca04ce3d1537310f55e91ec4cec7e5a88fa0fba20a24cce1fe6de2a2b0".to_string(),
            SDK_OPENCHANNEL_MIN_SAT,
            true,
            None,
            None,
            Some("trusted_no_broadcast".to_string()),
            None,
            None,
        )
        .expect_err("should fail");
    assert_eq!(
        err.as_string().unwrap_or_default(),
        "virtual channels requires public=false"
    );
}

#[test]
#[cfg(target_arch = "wasm32")]
fn open_channel_non_virtual_rejects_without_mutating_state_contract() {
    let node = RlnWasmNode::new_with_runtime_backend(
        "ws://proxy.non-virtual-open-reject.example".to_string(),
        "wasm_native_ldk".to_string(),
    )
    .expect("node should build");
    let peer_pubkey =
        "029999999999999999999999999999999999999999999999999999999999999999".to_string();
    node.ldk_runtime.upsert_peer(LdkRuntimePeerStateData {
        pubkey: peer_pubkey.clone(),
        peer_addr: "127.0.0.1:9735".to_string(),
        started: true,
    });

    let before_channels: serde_json::Value =
        crate::js_from(node.list_channels_value().expect("list channels before"))
            .expect("parse channels before");
    let before_seq = *node.next_channel_seq.borrow();

    let err = node
        .open_channel_value_with_options(
            peer_pubkey,
            SDK_OPENCHANNEL_MIN_SAT,
            false,
            None,
            None,
            None,
            None,
            None,
        )
        .expect_err("non-virtual open must be rejected");
    assert!(
        !err.as_string().unwrap_or_default().is_empty(),
        "must return concrete non-virtual open error"
    );

    let after_channels: serde_json::Value =
        crate::js_from(node.list_channels_value().expect("list channels after"))
            .expect("parse channels after");
    let after_seq = *node.next_channel_seq.borrow();
    assert_eq!(before_channels, after_channels);
    assert_eq!(before_seq, after_seq);
}

#[test]
#[cfg(target_arch = "wasm32")]
fn open_channel_non_virtual_rgb_rejected_with_explicit_contract_message() {
    let node = RlnWasmNode::new_with_runtime_backend(
        "ws://proxy.non-virtual-open-rgb-reject.example".to_string(),
        "wasm_native_ldk".to_string(),
    )
    .expect("node should build");
    let peer_pubkey =
        "02aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string();
    node.ldk_runtime.upsert_peer(LdkRuntimePeerStateData {
        pubkey: peer_pubkey.clone(),
        peer_addr: "127.0.0.1:9735".to_string(),
        started: true,
    });

    let err = node
        .open_channel_value_with_options(
            peer_pubkey,
            SDK_OPENRGBCHANNEL_MIN_SAT,
            false,
            Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string()),
            Some(SDK_OPENCHANNEL_MIN_RGB_AMT),
            None,
            None,
            None,
        )
        .expect_err("RGB non-virtual open must be hard-rejected for now");
    assert_eq!(
        err.as_string().unwrap_or_default(),
        "native non-virtual RGB funding path is not wired yet; BTC-only channel open is currently supported"
    );
}

#[test]
fn open_channel_virtual_mode_is_persisted_in_runtime_contract() {
    let node = RlnWasmNode::new_with_runtime_backend(
        "ws://proxy.virtual-open.example".to_string(),
        "wasm_native_ldk".to_string(),
    )
    .expect("node should build");
    let peer_pubkey =
        "02acacacacacacacacacacacacacacacacacacacacacacacacacacacacacacac".to_string();
    node.ldk_runtime.upsert_peer(LdkRuntimePeerStateData {
        pubkey: peer_pubkey.clone(),
        peer_addr: "127.0.0.1:9735".to_string(),
        started: true,
    });
    let opened = node
        .open_channel_value_with_options(
            peer_pubkey.clone(),
            SDK_OPENCHANNEL_MIN_SAT,
            false,
            None,
            None,
            Some("trusted_no_broadcast".to_string()),
            None,
            None,
        )
        .expect("open channel");
    let opened_json: serde_json::Value = crate::js_from(opened).expect("parse opened channel");
    assert_eq!(
        opened_json["virtual_open_mode"],
        serde_json::Value::String("trusted_no_broadcast".to_string())
    );
    assert_eq!(opened_json["status"], "opening");
    assert_eq!(opened_json["is_usable"], false);
}

#[test]
#[cfg(target_arch = "wasm32")]
fn open_channel_virtual_becomes_usable_only_after_runtime_event_contract() {
    let node = RlnWasmNode::new_with_runtime_backend(
        "ws://proxy.virtual-open-event.example".to_string(),
        "wasm_native_ldk".to_string(),
    )
    .expect("node should build");
    let peer_pubkey =
        "02afafafafafafafafafafafafafafafafafafafafafafafafafafafafafafaf".to_string();
    node.ldk_runtime.upsert_peer(LdkRuntimePeerStateData {
        pubkey: peer_pubkey.clone(),
        peer_addr: "127.0.0.1:9735".to_string(),
        started: true,
    });
    let opened = node
        .open_channel_value_with_options(
            peer_pubkey.clone(),
            SDK_OPENCHANNEL_MIN_SAT,
            false,
            None,
            None,
            Some("trusted_no_broadcast".to_string()),
            None,
            None,
        )
        .expect("open channel");
    let opened_json: serde_json::Value = crate::js_from(opened).expect("parse opened channel");
    let channel_id = opened_json["channel_id"]
        .as_str()
        .expect("channel id")
        .to_string();
    assert_eq!(opened_json["status"], "opening");
    assert_eq!(opened_json["is_usable"], false);

    let processed = node
        .process_native_runtime_queue_value()
        .expect("process runtime queue");
    let processed: serde_json::Value = crate::js_from(processed).expect("parse processed");
    assert_eq!(processed["drained"], 1);

    let listed: serde_json::Value =
        crate::js_from(node.list_channels_value().expect("list channels")).expect("parse channels");
    let listed = listed.as_array().expect("channels array");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0]["channel_id"], channel_id);
    assert_eq!(listed[0]["status"], "opened");
    assert_eq!(listed[0]["is_usable"], true);
}

#[test]
#[cfg(target_arch = "wasm32")]
fn open_channel_virtual_rejected_when_feature_disabled_contract() {
    crate::set_sdk_default_enable_virtual_channels_v0(false);
    let node = RlnWasmNode::new_with_runtime_backend(
        "ws://proxy.virtual-open-disabled.example".to_string(),
        "wasm_native_ldk".to_string(),
    )
    .expect("node should build");
    node.set_enable_virtual_channels_v0(false);
    let peer_pubkey =
        "02a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0".to_string();
    node.ldk_runtime.upsert_peer(LdkRuntimePeerStateData {
        pubkey: peer_pubkey.clone(),
        peer_addr: "127.0.0.1:9735".to_string(),
        started: true,
    });
    let err = node
        .open_channel_value_with_options(
            peer_pubkey,
            SDK_OPENCHANNEL_MIN_SAT,
            false,
            None,
            None,
            Some("trusted_no_broadcast".to_string()),
            None,
            None,
        )
        .expect_err("should fail");
    assert_eq!(
        err.as_string().unwrap_or_default(),
        "trusted virtual channels v0 are disabled"
    );
}

#[test]
#[cfg(target_arch = "wasm32")]
fn virtual_channels_gate_persists_per_runtime_scope_contract() {
    crate::set_sdk_default_enable_virtual_channels_v0(false);
    let proxy = "ws://proxy.virtual-gate-persist.example".to_string();
    let runtime_id = Some("virtual-gate-persist-node".to_string());
    let first = RlnWasmNode::new_with_runtime_backend_and_id(
        proxy.clone(),
        "wasm_native_ldk".to_string(),
        runtime_id.clone(),
    )
    .expect("first node");
    let initial: serde_json::Value = crate::js_from(
        first
            .enable_virtual_channels_v0_value()
            .expect("flag value"),
    )
    .expect("parse flag");
    assert_eq!(initial["enabled"], false);
    first.set_enable_virtual_channels_v0(true);

    let second = RlnWasmNode::new_with_runtime_backend_and_id(
        proxy,
        "wasm_native_ldk".to_string(),
        runtime_id,
    )
    .expect("second node");
    let restored: serde_json::Value = crate::js_from(
        second
            .enable_virtual_channels_v0_value()
            .expect("flag value"),
    )
    .expect("parse flag");
    assert_eq!(restored["enabled"], true);
}

#[test]
#[cfg(target_arch = "wasm32")]
fn close_channel_virtual_requires_peer_pubkey_contract() {
    let node = RlnWasmNode::new_with_runtime_backend(
        "ws://proxy.virtual-close-peer-required.example".to_string(),
        "wasm_native_ldk".to_string(),
    )
    .expect("node should build");
    let peer_pubkey =
        "02adadadadadadadadadadadadadadadadadadadadadadadadadadadadadadad".to_string();
    node.ldk_runtime.upsert_peer(LdkRuntimePeerStateData {
        pubkey: peer_pubkey.clone(),
        peer_addr: "127.0.0.1:9735".to_string(),
        started: true,
    });
    let opened = node
        .open_channel_value_with_options(
            peer_pubkey,
            SDK_OPENCHANNEL_MIN_SAT,
            false,
            None,
            None,
            Some("trusted_no_broadcast".to_string()),
            None,
            None,
        )
        .expect("open virtual channel");
    let opened_json: serde_json::Value = crate::js_from(opened).expect("parse opened channel");
    let channel_id = opened_json["channel_id"]
        .as_str()
        .expect("channel id")
        .to_string();

    let err = node
        .close_channel_with_options(channel_id, None, false)
        .expect_err("should fail");
    assert_eq!(
        err.as_string().unwrap_or_default(),
        "peer_pubkey is required for trusted virtual channel close"
    );
}

#[test]
#[cfg(target_arch = "wasm32")]
fn close_channel_rejects_force_for_virtual_channel_contract() {
    let node = RlnWasmNode::new_with_runtime_backend(
        "ws://proxy.virtual-close.example".to_string(),
        "wasm_native_ldk".to_string(),
    )
    .expect("node should build");
    let peer_pubkey =
        "02adadadadadadadadadadadadadadadadadadadadadadadadadadadadadadad".to_string();
    node.ldk_runtime.upsert_peer(LdkRuntimePeerStateData {
        pubkey: peer_pubkey.clone(),
        peer_addr: "127.0.0.1:9735".to_string(),
        started: true,
    });
    let opened = node
        .open_channel_value_with_options(
            peer_pubkey.clone(),
            SDK_OPENCHANNEL_MIN_SAT,
            false,
            None,
            None,
            Some("trusted_no_broadcast".to_string()),
            None,
            None,
        )
        .expect("open virtual channel");
    let opened_json: serde_json::Value = crate::js_from(opened).expect("parse opened channel");
    let channel_id = opened_json["channel_id"]
        .as_str()
        .expect("channel id")
        .to_string();

    let err = node
        .close_channel_with_options(channel_id, Some(peer_pubkey), true)
        .expect_err("should fail");
    assert_eq!(
        err.as_string().unwrap_or_default(),
        "force=true is not supported for trusted virtual channels"
    );
}

#[test]
#[cfg(target_arch = "wasm32")]
fn close_channel_virtual_rejected_when_feature_disabled_contract() {
    let node = RlnWasmNode::new_with_runtime_backend(
        "ws://proxy.virtual-close-disabled.example".to_string(),
        "wasm_native_ldk".to_string(),
    )
    .expect("node should build");
    let peer_pubkey =
        "02ababababababababababababababababababababababababababababababab".to_string();
    node.ldk_runtime.upsert_peer(LdkRuntimePeerStateData {
        pubkey: peer_pubkey.clone(),
        peer_addr: "127.0.0.1:9735".to_string(),
        started: true,
    });
    let opened = node
        .open_channel_value_with_options(
            peer_pubkey.clone(),
            SDK_OPENCHANNEL_MIN_SAT,
            false,
            None,
            None,
            Some("trusted_no_broadcast".to_string()),
            None,
            None,
        )
        .expect("open virtual channel");
    let opened_json: serde_json::Value = crate::js_from(opened).expect("parse opened channel");
    let channel_id = opened_json["channel_id"]
        .as_str()
        .expect("channel id")
        .to_string();

    node.set_enable_virtual_channels_v0(false);
    let err = node
        .close_channel_with_options(channel_id, Some(peer_pubkey), false)
        .expect_err("should fail");
    assert_eq!(
        err.as_string().unwrap_or_default(),
        "trusted virtual channels v0 are disabled"
    );
}

#[test]
#[cfg(target_arch = "wasm32")]
fn close_channel_rejects_virtual_cleanup_when_counterparty_btc_value_remains_contract() {
    let node = RlnWasmNode::new_with_runtime_backend(
        "ws://proxy.virtual-close-btc-floor.example".to_string(),
        "wasm_native_ldk".to_string(),
    )
    .expect("node should build");
    let peer_pubkey =
        "02bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_string();
    node.ldk_runtime.upsert_peer(LdkRuntimePeerStateData {
        pubkey: peer_pubkey.clone(),
        peer_addr: "127.0.0.1:9735".to_string(),
        started: true,
    });
    let opened = node
        .open_channel_value_with_options(
            peer_pubkey.clone(),
            SDK_OPENCHANNEL_MIN_SAT,
            false,
            None,
            None,
            Some("trusted_no_broadcast".to_string()),
            None,
            None,
        )
        .expect("open virtual channel");
    let opened_json: serde_json::Value = crate::js_from(opened).expect("parse opened channel");
    let channel_id = opened_json["channel_id"]
        .as_str()
        .expect("channel id")
        .to_string();

    let keysend = node
        .keysend_value(peer_pubkey.clone(), SDK_HTLC_MIN_MSAT, None, None)
        .expect("keysend");
    let keysend_json: serde_json::Value = crate::js_from(keysend).expect("parse keysend");
    let payment_hash = keysend_json["payment_hash"]
        .as_str()
        .expect("payment hash")
        .to_string();
    node.update_payment_status(payment_hash, "succeeded".to_string())
        .expect("mark payment succeeded");

    let err = node
        .close_channel_with_options(channel_id, Some(peer_pubkey.clone()), false)
        .expect_err("should fail");
    assert!(err
        .as_string()
        .unwrap_or_default()
        .contains("counterparty BTC balance floor is"));
}

#[test]
#[cfg(target_arch = "wasm32")]
fn close_channel_allows_virtual_cleanup_after_btc_roundtrip_contract() {
    let node = RlnWasmNode::new_with_runtime_backend(
        "ws://proxy.virtual-close-btc-roundtrip.example".to_string(),
        "wasm_native_ldk".to_string(),
    )
    .expect("node should build");
    let peer_pubkey =
        "02cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc".to_string();
    node.ldk_runtime.upsert_peer(LdkRuntimePeerStateData {
        pubkey: peer_pubkey.clone(),
        peer_addr: "127.0.0.1:9735".to_string(),
        started: true,
    });
    let opened = node
        .open_channel_value_with_options(
            peer_pubkey.clone(),
            SDK_OPENCHANNEL_MIN_SAT,
            false,
            None,
            None,
            Some("trusted_no_broadcast".to_string()),
            None,
            None,
        )
        .expect("open virtual channel");
    let opened_json: serde_json::Value = crate::js_from(opened).expect("parse opened channel");
    let channel_id = opened_json["channel_id"]
        .as_str()
        .expect("channel id")
        .to_string();

    let keysend = node
        .keysend_value(peer_pubkey.clone(), SDK_HTLC_MIN_MSAT, None, None)
        .expect("keysend");
    let keysend_json: serde_json::Value = crate::js_from(keysend).expect("parse keysend");
    let payment_hash = keysend_json["payment_hash"]
        .as_str()
        .expect("payment hash")
        .to_string();
    node.update_payment_status(payment_hash, "succeeded".to_string())
        .expect("mark payment succeeded");

    let invoice_json = node
        .create_ln_invoice_json(Some(SDK_HTLC_MIN_MSAT), 3600, None, None)
        .expect("create invoice");
    let invoice_doc: serde_json::Value = serde_json::from_str(&invoice_json).expect("parse");
    let invoice = invoice_doc["invoice"]
        .as_str()
        .expect("invoice")
        .to_string();
    let decoded_invoice: serde_json::Value = crate::js_from(
        node.decode_ln_invoice_value(invoice.clone())
            .expect("decode invoice"),
    )
    .expect("parse decoded invoice");
    let inbound_payment_hash = decoded_invoice["payment_hash"]
        .as_str()
        .expect("payment hash")
        .to_string();
    node.update_payment_status_by_invoice(invoice, "succeeded".to_string())
        .expect("simulate return payment");
    let seq = {
        let mut next_seq = node.next_runtime_event_seq.borrow_mut();
        *next_seq += 1;
        *next_seq
    };
    node.runtime_events
        .borrow_mut()
        .push(RlnWasmNodeRuntimeEventData {
            seq,
            source: "runtime_virtual_payment_engine".to_string(),
            event_kind: "payment_status".to_string(),
            payload_hex: encode_payment_status_event_payload(&inbound_payment_hash, "succeeded"),
            payment_hash: Some(inbound_payment_hash),
            status: Some("succeeded".to_string()),
            applied: true,
            error: None,
            received_at: unix_now_secs(),
        });

    node.close_channel_with_options(channel_id, Some(peer_pubkey), false)
        .expect("close should succeed after roundtrip");
}

#[test]
#[cfg(target_arch = "wasm32")]
fn close_channel_allows_virtual_cleanup_after_authoritative_peer_keysend_roundtrip_contract() {
    crate::ldk_runtime::test_utils::reset_runtime_storage_for_tests();
    crate::ln_node::test_utils::reset_runtime_event_log_storage_for_tests();
    let node_a = RlnWasmNode::new_with_runtime_id_opt(
        "ws://proxy.virtual-close-authoritative-roundtrip.example".to_string(),
        Some("virtual-close-node-a".to_string()),
        None,
    )
    .expect("node a should build");
    let node_b = RlnWasmNode::new_with_runtime_id_opt(
        "ws://proxy.virtual-close-authoritative-roundtrip.example".to_string(),
        Some("virtual-close-node-b".to_string()),
        None,
    )
    .expect("node b should build");

    let invoice_a = node_a
        .create_ln_invoice_json(Some(SDK_HTLC_MIN_MSAT), 3600, None, None)
        .expect("create invoice a");
    let invoice_a_doc: serde_json::Value = serde_json::from_str(&invoice_a).expect("parse");
    let invoice_a_str = invoice_a_doc["invoice"]
        .as_str()
        .expect("invoice a")
        .to_string();
    let decoded_a: serde_json::Value = crate::js_from(
        node_b
            .decode_ln_invoice_value(invoice_a_str)
            .expect("decode a"),
    )
    .expect("parse decoded a");
    let node_a_pubkey = decoded_a["payee_pubkey"]
        .as_str()
        .expect("node a pubkey")
        .to_string();

    let invoice_b = node_b
        .create_ln_invoice_json(Some(SDK_HTLC_MIN_MSAT), 3600, None, None)
        .expect("create invoice b");
    let invoice_b_doc: serde_json::Value = serde_json::from_str(&invoice_b).expect("parse");
    let invoice_b_str = invoice_b_doc["invoice"]
        .as_str()
        .expect("invoice b")
        .to_string();
    let decoded_b: serde_json::Value = crate::js_from(
        node_a
            .decode_ln_invoice_value(invoice_b_str)
            .expect("decode b"),
    )
    .expect("parse decoded b");
    let node_b_pubkey = decoded_b["payee_pubkey"]
        .as_str()
        .expect("node b pubkey")
        .to_string();

    node_a.ldk_runtime.upsert_peer(LdkRuntimePeerStateData {
        pubkey: node_b_pubkey.clone(),
        peer_addr: "127.0.0.1:9735".to_string(),
        started: true,
    });
    node_b.ldk_runtime.upsert_peer(LdkRuntimePeerStateData {
        pubkey: node_a_pubkey.clone(),
        peer_addr: "127.0.0.1:9736".to_string(),
        started: true,
    });

    let opened = node_a
        .open_channel_value_with_options(
            node_b_pubkey.clone(),
            SDK_OPENCHANNEL_MIN_SAT,
            false,
            None,
            None,
            Some("trusted_no_broadcast".to_string()),
            None,
            None,
        )
        .expect("open virtual channel");
    let opened_json: serde_json::Value = crate::js_from(opened).expect("parse opened channel");
    let channel_id = opened_json["channel_id"]
        .as_str()
        .expect("channel id")
        .to_string();
    let _ = node_a.process_native_runtime_queue_value();

    let keysend_ab = node_a
        .keysend_value(node_b_pubkey.clone(), SDK_HTLC_MIN_MSAT, None, None)
        .expect("keysend a->b");
    let keysend_ab_doc: serde_json::Value = crate::js_from(keysend_ab).expect("parse a->b");
    let payment_ab_hash = keysend_ab_doc["payment_hash"]
        .as_str()
        .expect("payment hash a->b")
        .to_string();
    let payment_ab: serde_json::Value = crate::js_from(
        node_a
            .get_payment_value(payment_ab_hash)
            .expect("get payment a->b"),
    )
    .expect("parse payment a->b");
    assert_eq!(payment_ab["status"], "succeeded");

    let keysend_ba = node_b
        .keysend_value(node_a_pubkey, SDK_HTLC_MIN_MSAT, None, None)
        .expect("keysend b->a");
    let keysend_ba_doc: serde_json::Value = crate::js_from(keysend_ba).expect("parse b->a");
    let payment_ba_hash = keysend_ba_doc["payment_hash"]
        .as_str()
        .expect("payment hash b->a")
        .to_string();
    let payment_ba: serde_json::Value = crate::js_from(
        node_b
            .get_payment_value(payment_ba_hash)
            .expect("get payment b->a"),
    )
    .expect("parse payment b->a");
    assert_eq!(payment_ba["status"], "succeeded");

    node_a
        .close_channel_with_options(channel_id, Some(node_b_pubkey), false)
        .expect("close should succeed after authoritative peer roundtrip");
}

#[test]
#[cfg(target_arch = "wasm32")]
fn close_channel_rejects_virtual_cleanup_after_non_authoritative_inbound_credit_contract() {
    let node = RlnWasmNode::new_with_runtime_backend(
        "ws://proxy.virtual-close-non-authoritative-credit.example".to_string(),
        "wasm_native_ldk".to_string(),
    )
    .expect("node should build");
    let peer_pubkey =
        "02cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd".to_string();
    node.ldk_runtime.upsert_peer(LdkRuntimePeerStateData {
        pubkey: peer_pubkey.clone(),
        peer_addr: "127.0.0.1:9735".to_string(),
        started: true,
    });
    let opened = node
        .open_channel_value_with_options(
            peer_pubkey.clone(),
            SDK_OPENCHANNEL_MIN_SAT,
            false,
            None,
            None,
            Some("trusted_no_broadcast".to_string()),
            None,
            None,
        )
        .expect("open virtual channel");
    let opened_json: serde_json::Value = crate::js_from(opened).expect("parse opened channel");
    let channel_id = opened_json["channel_id"]
        .as_str()
        .expect("channel id")
        .to_string();

    let keysend = node
        .keysend_value(peer_pubkey.clone(), SDK_HTLC_MIN_MSAT, None, None)
        .expect("keysend");
    let keysend_json: serde_json::Value = crate::js_from(keysend).expect("parse keysend");
    let payment_hash = keysend_json["payment_hash"]
        .as_str()
        .expect("payment hash")
        .to_string();
    node.update_payment_status(payment_hash, "succeeded".to_string())
        .expect("mark payment succeeded");

    let invoice_json = node
        .create_ln_invoice_json(Some(SDK_HTLC_MIN_MSAT), 3600, None, None)
        .expect("create invoice");
    let invoice_doc: serde_json::Value = serde_json::from_str(&invoice_json).expect("parse");
    let invoice = invoice_doc["invoice"]
        .as_str()
        .expect("invoice")
        .to_string();
    node.update_payment_status_by_invoice(invoice, "succeeded".to_string())
        .expect("simulate inbound credit without runtime authority");

    let err = node
        .close_channel_with_options(channel_id, Some(peer_pubkey.clone()), false)
        .expect_err("should fail");
    assert!(err
        .as_string()
        .unwrap_or_default()
        .contains("counterparty BTC balance floor is"));
}

#[test]
#[cfg(target_arch = "wasm32")]
fn close_channel_rejects_virtual_cleanup_when_claimable_invoice_exists_contract() {
    let node = RlnWasmNode::new_with_runtime_backend(
        "ws://proxy.virtual-close-claimable.example".to_string(),
        "wasm_native_ldk".to_string(),
    )
    .expect("node should build");
    let peer_pubkey =
        "02dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd".to_string();
    node.ldk_runtime.upsert_peer(LdkRuntimePeerStateData {
        pubkey: peer_pubkey.clone(),
        peer_addr: "127.0.0.1:9735".to_string(),
        started: true,
    });
    let opened = node
        .open_channel_value_with_options(
            peer_pubkey.clone(),
            SDK_OPENCHANNEL_MIN_SAT,
            false,
            None,
            None,
            Some("trusted_no_broadcast".to_string()),
            None,
            None,
        )
        .expect("open virtual channel");
    let opened_json: serde_json::Value = crate::js_from(opened).expect("parse opened channel");
    let channel_id = opened_json["channel_id"]
        .as_str()
        .expect("channel id")
        .to_string();

    let invoice_json = node
        .create_ln_invoice_json(Some(SDK_HTLC_MIN_MSAT), 3600, None, None)
        .expect("create invoice");
    let invoice_doc: serde_json::Value = serde_json::from_str(&invoice_json).expect("parse");
    let invoice = invoice_doc["invoice"]
        .as_str()
        .expect("invoice")
        .to_string();
    node.update_payment_status_by_invoice(invoice, "claimable".to_string())
        .expect("simulate claimable payment");

    let err = node
        .close_channel_with_options(channel_id, Some(peer_pubkey), false)
        .expect_err("should fail");
    assert_eq!(
        err.as_string().unwrap_or_default(),
        "virtual cleanup is blocked while HTLCs are still in flight"
    );
}

#[test]
#[cfg(target_arch = "wasm32")]
fn hodl_invoice_claim_is_idempotent_contract() {
    let node = RlnWasmNode::new_with_runtime_backend(
        "ws://proxy.hodl-claim.example".to_string(),
        "wasm_native_ldk".to_string(),
    )
    .expect("node should build");

    let preimage_bytes = [7u8; 32];
    let preimage_hex = hex::encode(preimage_bytes);
    let payment_hash = hex::encode(Sha256::hash(&preimage_bytes).to_byte_array());
    let invoice_js = node
        .create_hodl_ln_invoice_value(Some(SDK_INVOICE_MIN_MSAT), 3600, None, None, payment_hash)
        .expect("create hodl invoice");
    let invoice_doc: serde_json::Value = crate::js_from(invoice_js).expect("parse invoice");
    let invoice = invoice_doc["invoice"]
        .as_str()
        .expect("invoice")
        .to_string();

    let _ = node
        .update_payment_status_by_invoice(invoice.clone(), "claimable".to_string())
        .expect("simulate received hodl payment");
    let decoded: serde_json::Value = crate::js_from(
        node.decode_ln_invoice_value(invoice.clone())
            .expect("decode invoice"),
    )
    .expect("decoded json");
    let payment_hash = decoded["payment_hash"]
        .as_str()
        .expect("payment hash")
        .to_string();

    let first_claim = node
        .claim_hodl_invoice_value(payment_hash.clone(), preimage_hex.clone())
        .expect("claim should succeed");
    let first_doc: serde_json::Value = crate::js_from(first_claim).expect("parse claim");
    assert_eq!(first_doc["changed"], true);

    let second_claim = node
        .claim_hodl_invoice_value(payment_hash, preimage_hex)
        .expect("re-claim should be idempotent");
    let second_doc: serde_json::Value = crate::js_from(second_claim).expect("parse claim");
    assert_eq!(second_doc["changed"], false);
}

#[test]
#[cfg(target_arch = "wasm32")]
fn hodl_invoice_cancel_contract() {
    let node = RlnWasmNode::new_with_runtime_backend(
        "ws://proxy.hodl-cancel.example".to_string(),
        "wasm_native_ldk".to_string(),
    )
    .expect("node should build");

    let preimage_bytes = [9u8; 32];
    let payment_hash = hex::encode(Sha256::hash(&preimage_bytes).to_byte_array());
    let invoice_js = node
        .create_hodl_ln_invoice_value(Some(SDK_INVOICE_MIN_MSAT), 3600, None, None, payment_hash)
        .expect("create hodl invoice");
    let invoice_doc: serde_json::Value = crate::js_from(invoice_js).expect("parse invoice");
    let invoice = invoice_doc["invoice"]
        .as_str()
        .expect("invoice")
        .to_string();
    let decoded: serde_json::Value = crate::js_from(
        node.decode_ln_invoice_value(invoice.clone())
            .expect("decode invoice"),
    )
    .expect("decoded json");
    let payment_hash = decoded["payment_hash"]
        .as_str()
        .expect("payment hash")
        .to_string();

    let _ = node
        .update_payment_status_by_invoice(invoice.clone(), "claimable".to_string())
        .expect("simulate received hodl payment");
    let _ = node
        .cancel_hodl_invoice_value(payment_hash)
        .expect("cancel should succeed");

    let status: serde_json::Value =
        crate::js_from(node.invoice_status_value(invoice).expect("invoice status"))
            .expect("status json");
    assert_eq!(status["status"], "cancelled");
}

#[test]
fn send_payment_requires_amount_for_zero_value_invoice_contract() {
    let node = RlnWasmNode::new("ws://proxy.example".to_string()).expect("node should build");
    let invoice_json = node
        .create_ln_invoice_json(None, 3600, None, None)
        .expect("create zero-value invoice");
    let parsed: serde_json::Value = serde_json::from_str(&invoice_json).expect("parse invoice");
    let invoice = parsed["invoice"]
        .as_str()
        .expect("invoice string")
        .to_string();

    let err = node
        .send_payment_value(invoice, None, None, None)
        .expect_err("should fail");
    assert_eq!(
        err.as_string().unwrap_or_default(),
        "need an amount for the given 0-value invoice"
    );
}

#[test]
fn keysend_rejects_invalid_pubkey_contract() {
    let node = RlnWasmNode::new("ws://proxy.example".to_string()).expect("node should build");
    let err = node
        .keysend_value("not-a-pubkey".to_string(), SDK_HTLC_MIN_MSAT, None, None)
        .expect_err("should fail");
    assert_eq!(err.as_string().unwrap_or_default(), "invalid dest_pubkey");
}

#[test]
fn keysend_rejects_invalid_rgb_payload_contract() {
    let node = RlnWasmNode::new("ws://proxy.example".to_string()).expect("node should build");
    let pubkey = "0334cc4bca04ce3d1537310f55e91ec4cec7e5a88fa0fba20a24cce1fe6de2a2b0".to_string();
    let asset_id = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".to_string();

    let empty_asset = node
        .keysend_value(
            pubkey.clone(),
            SDK_HTLC_MIN_MSAT,
            Some("   ".to_string()),
            Some(1),
        )
        .expect_err("should fail");
    assert_eq!(
        empty_asset.as_string().unwrap_or_default(),
        sdk_contracts::ERR_ASSET_ID_EMPTY_IF_PROVIDED
    );

    let zero_amount = node
        .keysend_value(pubkey, SDK_HTLC_MIN_MSAT, Some(asset_id), Some(0))
        .expect_err("should fail");
    assert_eq!(
        zero_amount.as_string().unwrap_or_default(),
        "asset_amount must be > 0 when provided"
    );
}

#[test]
fn peer_addr_format_validation_contract() {
    assert!(validate_peer_addr_format("127.0.0.1:9735").is_ok());
    assert!(validate_peer_addr_format("node.example.com:9735").is_ok());
    assert_eq!(
        validate_peer_addr_format("missing-port")
            .expect_err("should fail")
            .as_string()
            .unwrap_or_default(),
        "peer_addr must be in host:port format"
    );
    assert_eq!(
        validate_peer_addr_format("example:abc")
            .expect_err("should fail")
            .as_string()
            .unwrap_or_default(),
        "peer_addr port must be numeric"
    );
    assert_eq!(
        validate_peer_addr_format("example:70000")
            .expect_err("should fail")
            .as_string()
            .unwrap_or_default(),
        "peer_addr port must be in range 0..=65535"
    );
}

#[test]
fn sign_message_native_contract() {
    let node = RlnWasmNode::new("ws://proxy.example".to_string()).expect("node should build");
    let signed_a = node
        .sign_message_value("  hello  ".to_string())
        .expect("sign message");
    let signed_b = node
        .sign_message_value("hello".to_string())
        .expect("sign message");
    let signed_c = node
        .sign_message_value("world".to_string())
        .expect("sign message");

    let signed_a_doc: serde_json::Value = crate::js_from(signed_a).expect("parse signature");
    let signed_b_doc: serde_json::Value = crate::js_from(signed_b).expect("parse signature");
    let signed_c_doc: serde_json::Value = crate::js_from(signed_c).expect("parse signature");

    let sig_a = signed_a_doc["signed_message"]
        .as_str()
        .expect("signature string");
    let sig_b = signed_b_doc["signed_message"]
        .as_str()
        .expect("signature string");
    let sig_c = signed_c_doc["signed_message"]
        .as_str()
        .expect("signature string");

    assert_eq!(sig_a.len(), 130);
    assert_eq!(sig_a, sig_b);
    assert_ne!(sig_a, sig_c);
}

#[test]
fn send_payment_uses_invoice_payee_pubkey_contract() {
    let node = RlnWasmNode::new("ws://proxy.example".to_string()).expect("node should build");
    let invoice_json = node
        .create_ln_invoice_json(Some(SDK_INVOICE_MIN_MSAT), 3600, None, None)
        .expect("create invoice");
    let invoice_doc: serde_json::Value = serde_json::from_str(&invoice_json).expect("parse");
    let invoice = invoice_doc["invoice"]
        .as_str()
        .expect("invoice string")
        .to_string();
    let decoded = node
        .decode_ln_invoice_value(invoice.clone())
        .expect("decode invoice");
    let decoded_doc: serde_json::Value = crate::js_from(decoded).expect("decode parse");
    let expected_payee = decoded_doc["payee_pubkey"]
        .as_str()
        .expect("decoded payee")
        .to_string();

    let sent = node
        .send_payment_value(invoice, Some(SDK_INVOICE_MIN_MSAT), None, None)
        .expect("send payment");
    let sent_doc: serde_json::Value = crate::js_from(sent).expect("send parse");
    let payment_hash = sent_doc["payment_hash"]
        .as_str()
        .expect("payment hash")
        .to_string();
    let payment = node.get_payment_value(payment_hash).expect("get payment");
    let payment_doc: serde_json::Value = crate::js_from(payment).expect("payment parse");
    assert_eq!(payment_doc["payee_pubkey"], expected_payee);
}

#[test]
fn send_payment_returns_payment_secret_and_native_mismatch_error_contract() {
    let node = RlnWasmNode::new("ws://proxy.example".to_string()).expect("node should build");
    let invoice_json = node
        .create_ln_invoice_json(Some(SDK_INVOICE_MIN_MSAT), 3600, None, None)
        .expect("create invoice");
    let parsed: serde_json::Value = serde_json::from_str(&invoice_json).expect("parse");
    let invoice = parsed["invoice"].as_str().expect("invoice str").to_string();

    let sent = node
        .send_payment_value(invoice.clone(), Some(SDK_INVOICE_MIN_MSAT), None, None)
        .expect("send");
    let sent_doc: serde_json::Value = crate::js_from(sent).expect("parse send");
    assert!(sent_doc["payment_secret"].as_str().is_some());

    let mismatch = node
        .send_payment_value(invoice, Some(SDK_INVOICE_MIN_MSAT + 1), None, None)
        .expect_err("should fail");
    assert_eq!(
        mismatch.as_string().unwrap_or_default(),
        format!(
            "amount didn't match invoice value of {}msat",
            SDK_INVOICE_MIN_MSAT
        )
    );
}

#[test]
fn bridge_send_payment_requires_connected_known_payee_peer_contract() {
    crate::ldk_runtime::test_utils::reset_runtime_storage_for_tests();
    let sender = RlnWasmNode::new_with_runtime_backend(
        "ws://proxy.sender.example".to_string(),
        "wasm_native_ldk".to_string(),
    )
    .expect("sender node");
    let receiver = RlnWasmNode::new_with_runtime_backend(
        "ws://proxy.receiver.example".to_string(),
        "wasm_native_ldk".to_string(),
    )
    .expect("receiver node");

    let invoice_json = receiver
        .create_ln_invoice_json(Some(SDK_INVOICE_MIN_MSAT), 3600, None, None)
        .expect("create receiver invoice");
    let invoice_doc: serde_json::Value = serde_json::from_str(&invoice_json).expect("parse");
    let invoice = invoice_doc["invoice"]
        .as_str()
        .expect("invoice")
        .to_string();
    let decoded = sender
        .decode_ln_invoice_value(invoice.clone())
        .expect("decode invoice");
    let decoded_doc: serde_json::Value = crate::js_from(decoded).expect("parse decoded");
    let payee_pubkey = decoded_doc["payee_pubkey"]
        .as_str()
        .expect("payee pubkey")
        .to_string();

    sender.ldk_runtime.upsert_peer(LdkRuntimePeerStateData {
        pubkey: "0334cc4bca04ce3d1537310f55e91ec4cec7e5a88fa0fba20a24cce1fe6de2a2b0".to_string(),
        peer_addr: "127.0.0.1:9735".to_string(),
        started: true,
    });
    sender.ldk_runtime.upsert_peer(LdkRuntimePeerStateData {
        pubkey: payee_pubkey.clone(),
        peer_addr: "127.0.0.1:9736".to_string(),
        started: false,
    });

    let first = sender
        .send_payment_value(invoice.clone(), Some(SDK_INVOICE_MIN_MSAT), None, None)
        .expect("send payment should return failed status when known payee disconnected");
    let first_doc: serde_json::Value = crate::js_from(first).expect("parse first send");
    assert_eq!(first_doc["status"], "failed");

    assert!(sender.ldk_runtime.set_peer_started(&payee_pubkey, true));
    let second = sender
        .send_payment_value(invoice, Some(SDK_INVOICE_MIN_MSAT), None, None)
        .expect("send payment should be pending after payee reconnect");
    let second_doc: serde_json::Value = crate::js_from(second).expect("parse second send");
    assert_eq!(second_doc["status"], "pending");
}

#[test]
fn bridge_send_payment_on_usable_channel_finalizes_via_runtime_channel_payment_engine_event() {
    crate::ldk_runtime::test_utils::reset_runtime_storage_for_tests();
    let sender = RlnWasmNode::new_with_runtime_backend(
        "ws://proxy.sender.channel-success.example".to_string(),
        "wasm_native_ldk".to_string(),
    )
    .expect("sender node");
    let receiver = RlnWasmNode::new_with_runtime_backend(
        "ws://proxy.receiver.channel-success.example".to_string(),
        "wasm_native_ldk".to_string(),
    )
    .expect("receiver node");

    let invoice_json = receiver
        .create_ln_invoice_json(Some(SDK_INVOICE_MIN_MSAT), 3600, None, None)
        .expect("create receiver invoice");
    let invoice_doc: serde_json::Value = serde_json::from_str(&invoice_json).expect("parse");
    let invoice = invoice_doc["invoice"]
        .as_str()
        .expect("invoice")
        .to_string();
    let decoded = sender
        .decode_ln_invoice_value(invoice.clone())
        .expect("decode invoice");
    let decoded_doc: serde_json::Value = crate::js_from(decoded).expect("parse decoded");
    let payee_pubkey = decoded_doc["payee_pubkey"]
        .as_str()
        .expect("payee pubkey")
        .to_string();

    sender.ldk_runtime.upsert_peer(LdkRuntimePeerStateData {
        pubkey: payee_pubkey.clone(),
        peer_addr: "127.0.0.1:9735".to_string(),
        started: true,
    });
    assert!(sender.test_set_runtime_peer_started(&payee_pubkey, true));
    sender
        .ldk_runtime
        .upsert_channel(LdkRuntimeChannelStateData {
            temporary_channel_id: "tmp-chan-runtime-success".to_string(),
            channel_id: "chan-runtime-success".to_string(),
            peer_pubkey: payee_pubkey.clone(),
            status: "opened".to_string(),
            ready: true,
            is_usable: true,
            public: false,
            capacity_sat: SDK_OPENCHANNEL_MIN_SAT,
            asset_id: None,
            asset_local_amount: None,
            virtual_open_mode: None,
        });

    let send_js = sender
        .send_payment_value(invoice, Some(SDK_INVOICE_MIN_MSAT), None, None)
        .expect("send payment");
    let send_doc: serde_json::Value = crate::js_from(send_js).expect("parse send");
    assert_eq!(send_doc["status"], "succeeded");
    let payment_hash = send_doc["payment_hash"]
        .as_str()
        .expect("payment_hash")
        .to_string();

    let payment_js = sender
        .get_payment_value(payment_hash.clone())
        .expect("get payment");
    let payment_doc: serde_json::Value = crate::js_from(payment_js).expect("parse payment");
    assert_eq!(payment_doc["status"], "succeeded");

    let events: serde_json::Value = serde_json::from_str(
        &sender
            .list_runtime_events_json()
            .expect("runtime events json"),
    )
    .expect("parse runtime events");
    let events = events.as_array().expect("events array");
    assert!(events.iter().any(|event| {
        event.get("source").and_then(|value| value.as_str())
            == Some("runtime_channel_payment_engine")
            && event.get("event_kind").and_then(|value| value.as_str()) == Some("payment_status")
            && event.get("payment_hash").and_then(|value| value.as_str()) == Some(&payment_hash)
            && event.get("status").and_then(|value| value.as_str()) == Some("succeeded")
            && event.get("applied").and_then(|value| value.as_bool()) == Some(true)
    }));
}

#[test]
fn bridge_send_payment_propagates_receiver_terminal_status_and_rgb_transfer_contract() {
    crate::ldk_runtime::test_utils::reset_runtime_storage_for_tests();
    crate::ln_node::test_utils::reset_runtime_event_log_storage_for_tests();
    let sender = RlnWasmNode::new_with_runtime_backend(
        "ws://proxy.sender.receiver-propagation.example".to_string(),
        "wasm_native_ldk".to_string(),
    )
    .expect("sender node");
    let receiver = RlnWasmNode::new_with_runtime_backend(
        "ws://proxy.receiver.receiver-propagation.example".to_string(),
        "wasm_native_ldk".to_string(),
    )
    .expect("receiver node");

    let rgb_asset_id = "rgb:ReceiverTerminalStatusParity-Asset_1~alpha".to_string();
    let invoice_json = receiver
        .create_ln_invoice_json(
            Some(SDK_INVOICE_MIN_MSAT),
            3600,
            Some(rgb_asset_id.clone()),
            Some(9),
        )
        .expect("create receiver rgb invoice");
    let invoice_doc: serde_json::Value = serde_json::from_str(&invoice_json).expect("parse");
    let invoice = invoice_doc["invoice"]
        .as_str()
        .expect("invoice")
        .to_string();
    let decoded = sender
        .decode_ln_invoice_value(invoice.clone())
        .expect("decode invoice");
    let decoded_doc: serde_json::Value = crate::js_from(decoded).expect("parse decoded");
    let payee_pubkey = decoded_doc["payee_pubkey"]
        .as_str()
        .expect("payee pubkey")
        .to_string();

    sender.ldk_runtime.upsert_peer(LdkRuntimePeerStateData {
        pubkey: payee_pubkey.clone(),
        peer_addr: "127.0.0.1:9735".to_string(),
        started: true,
    });
    sender
        .ldk_runtime
        .upsert_channel(LdkRuntimeChannelStateData {
            temporary_channel_id: "tmp-rx-propagation".to_string(),
            channel_id: "chan-rx-propagation".to_string(),
            peer_pubkey: payee_pubkey,
            status: "opened".to_string(),
            ready: true,
            is_usable: true,
            public: false,
            capacity_sat: SDK_OPENCHANNEL_MIN_SAT,
            asset_id: None,
            asset_local_amount: None,
            virtual_open_mode: None,
        });

    let send_js = sender
        .send_payment_value(
            invoice,
            Some(SDK_INVOICE_MIN_MSAT),
            Some(rgb_asset_id.clone()),
            Some(9),
        )
        .expect("send payment");
    let send_doc: serde_json::Value = crate::js_from(send_js).expect("parse send");
    let payment_hash = send_doc["payment_hash"]
        .as_str()
        .expect("payment hash")
        .to_string();
    assert_eq!(send_doc["status"], "succeeded");

    let receiver_payment_js = receiver
        .get_payment_value(payment_hash.clone())
        .expect("receiver payment");
    let receiver_payment: serde_json::Value =
        crate::js_from(receiver_payment_js).expect("parse receiver payment");
    assert_eq!(receiver_payment["status"], "succeeded");

    let receiver_transfers_js = receiver
        .list_rgb_ln_transfers_value()
        .expect("receiver transfers");
    let receiver_transfers: serde_json::Value =
        crate::js_from(receiver_transfers_js).expect("parse receiver transfers");
    let transfers = receiver_transfers.as_array().expect("transfer array");
    let matching = transfers
        .iter()
        .find(|entry| entry["payment_hash"] == payment_hash)
        .expect("matching transfer");
    assert_eq!(matching["status"], "succeeded");
    assert_eq!(matching["asset_id"], rgb_asset_id);
}

#[cfg(target_arch = "wasm32")]
#[test]
fn asset_id_validation_contract_for_ln_methods() {
    let node = RlnWasmNode::new("ws://proxy.example".to_string()).expect("node should build");
    let bad_asset = "not-a-contract-id".to_string();
    let pubkey = "0334cc4bca04ce3d1537310f55e91ec4cec7e5a88fa0fba20a24cce1fe6de2a2b0".to_string();

    let create_err = node
        .create_ln_invoice_value(
            Some(SDK_INVOICE_MIN_MSAT),
            3600,
            Some(bad_asset.clone()),
            Some(1),
        )
        .expect_err("should fail");
    assert_eq!(
        create_err.as_string().unwrap_or_default(),
        "invalid asset_id"
    );

    let keysend_err = node
        .keysend_value(
            pubkey.clone(),
            SDK_HTLC_MIN_MSAT,
            Some(bad_asset.clone()),
            Some(1),
        )
        .expect_err("should fail");
    assert_eq!(
        keysend_err.as_string().unwrap_or_default(),
        "invalid asset_id"
    );

    let open_err = node
        .open_channel_value(
            pubkey,
            SDK_OPENRGBCHANNEL_MIN_SAT,
            false,
            Some(bad_asset),
            Some(1),
        )
        .expect_err("should fail");
    assert_eq!(open_err.as_string().unwrap_or_default(), "invalid asset_id");
}

#[cfg(target_arch = "wasm32")]
#[test]
fn asset_id_validation_accepts_canonical_rgb_id_for_ln_methods_contract() {
    let node = RlnWasmNode::new("ws://proxy.example".to_string()).expect("node should build");
    let rgb_asset = "rgb:DemoAsset-Alpha_1~beta".to_string();
    let pubkey = "0334cc4bca04ce3d1537310f55e91ec4cec7e5a88fa0fba20a24cce1fe6de2a2b0".to_string();

    node.create_ln_invoice_value(
        Some(SDK_INVOICE_MIN_MSAT),
        3600,
        Some(rgb_asset.clone()),
        Some(1),
    )
    .expect("create ln invoice should accept canonical rgb asset id");

    node.keysend_value(
        pubkey.clone(),
        SDK_HTLC_MIN_MSAT,
        Some(rgb_asset.clone()),
        Some(1),
    )
    .expect("keysend should accept canonical rgb asset id");

    node.open_channel_value(
        pubkey,
        SDK_OPENRGBCHANNEL_MIN_SAT,
        false,
        Some(rgb_asset),
        Some(1),
    )
    .expect("open channel should accept canonical rgb asset id");
}

#[cfg(feature = "wasm-browser-infra")]
#[wasm_bindgen_test]
fn close_channel_regular_coop_and_force_contracts() {
    crate::ldk_runtime::test_utils::reset_runtime_storage_for_tests();
    let peer_pubkey =
        "0334cc4bca04ce3d1537310f55e91ec4cec7e5a88fa0fba20a24cce1fe6de2a2b0".to_string();

    let node = RlnWasmNode::new_with_runtime_backend(
        "ws://proxy.close-regular.example".to_string(),
        "wasm_native_ldk".to_string(),
    )
    .expect("node");
    node.ldk_runtime.upsert_peer(LdkRuntimePeerStateData {
        pubkey: peer_pubkey.clone(),
        peer_addr: "127.0.0.1:9735".to_string(),
        started: true,
    });
    node.ldk_runtime.upsert_channel(LdkRuntimeChannelStateData {
        temporary_channel_id: "tmp-close-regular-1".to_string(),
        channel_id: "chan-close-regular-1".to_string(),
        peer_pubkey: peer_pubkey.clone(),
        status: "opened".to_string(),
        ready: true,
        is_usable: true,
        public: false,
        capacity_sat: SDK_OPENCHANNEL_MIN_SAT,
        asset_id: None,
        asset_local_amount: None,
        virtual_open_mode: None,
    });
    node.close_channel_with_options(
        "chan-close-regular-1".to_string(),
        Some(peer_pubkey.clone()),
        false,
    )
    .expect("regular coop close");

    let channels_js = node
        .list_channels_value()
        .expect("list channels after coop");
    let channels: serde_json::Value = crate::js_from(channels_js).expect("parse channels");
    assert!(channels.as_array().expect("channels array").is_empty());

    node.ldk_runtime.upsert_channel(LdkRuntimeChannelStateData {
        temporary_channel_id: "tmp-close-regular-2".to_string(),
        channel_id: "chan-close-regular-2".to_string(),
        peer_pubkey: peer_pubkey.clone(),
        status: "opened".to_string(),
        ready: true,
        is_usable: true,
        public: false,
        capacity_sat: SDK_OPENCHANNEL_MIN_SAT,
        asset_id: None,
        asset_local_amount: None,
        virtual_open_mode: None,
    });
    node.close_channel_with_options("chan-close-regular-2".to_string(), Some(peer_pubkey), true)
        .expect("regular force close");

    let channels_js = node
        .list_channels_value()
        .expect("list channels after force");
    let channels: serde_json::Value = crate::js_from(channels_js).expect("parse channels");
    assert!(channels.as_array().expect("channels array").is_empty());
}

#[cfg(feature = "wasm-browser-infra")]
#[wasm_bindgen_test]
fn close_channel_regular_coop_persists_across_node_recreation_contract() {
    crate::ldk_runtime::test_utils::reset_runtime_storage_for_tests();
    crate::ln_node::test_utils::reset_runtime_event_log_storage_for_tests();
    let peer_pubkey =
        "03aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string();
    let runtime_proxy = "ws://proxy.close-regular-restart.example".to_string();

    let node =
        RlnWasmNode::new_with_runtime_backend(runtime_proxy.clone(), "wasm_native_ldk".to_string())
            .expect("node");
    node.ldk_runtime.upsert_peer(LdkRuntimePeerStateData {
        pubkey: peer_pubkey.clone(),
        peer_addr: "127.0.0.1:9735".to_string(),
        started: true,
    });
    node.ldk_runtime.upsert_channel(LdkRuntimeChannelStateData {
        temporary_channel_id: "tmp-close-restart-1".to_string(),
        channel_id: "chan-close-restart-1".to_string(),
        peer_pubkey: peer_pubkey.clone(),
        status: "opened".to_string(),
        ready: true,
        is_usable: true,
        public: false,
        capacity_sat: SDK_OPENCHANNEL_MIN_SAT,
        asset_id: None,
        asset_local_amount: None,
        virtual_open_mode: None,
    });

    node.close_channel_with_options("chan-close-restart-1".to_string(), Some(peer_pubkey), false)
        .expect("regular coop close");

    let recreated =
        RlnWasmNode::new_with_runtime_backend(runtime_proxy, "wasm_native_ldk".to_string())
            .expect("recreated node");
    let channels_js = recreated
        .list_channels_value()
        .expect("list channels after recreation");
    let channels: serde_json::Value = crate::js_from(channels_js).expect("parse channels");
    assert!(
        channels.as_array().expect("channels array").is_empty(),
        "closed channel should not reappear after recreation"
    );

    let events: serde_json::Value = serde_json::from_str(
        &recreated
            .list_runtime_events_json()
            .expect("runtime events should persist"),
    )
    .expect("parse runtime events");
    let entries = events.as_array().expect("events array");
    assert!(
        entries
            .iter()
            .any(|entry| entry["event_kind"] == "channel_closed"
                && entry["source"] == "node_api"
                && entry["applied"] == true),
        "expected persisted channel_closed runtime event"
    );
}

#[cfg(feature = "wasm-browser-infra")]
#[wasm_bindgen_test]
fn close_channel_regular_force_records_channel_closed_sequence_contract() {
    crate::ldk_runtime::test_utils::reset_runtime_storage_for_tests();
    crate::ln_node::test_utils::reset_runtime_event_log_storage_for_tests();
    let peer_pubkey =
        "0334cc4bca04ce3d1537310f55e91ec4cec7e5a88fa0fba20a24cce1fe6de2a2b0".to_string();

    let node = RlnWasmNode::new_with_runtime_backend(
        "ws://proxy.close-force-seq.example".to_string(),
        "wasm_native_ldk".to_string(),
    )
    .expect("node");
    node.ldk_runtime.upsert_peer(LdkRuntimePeerStateData {
        pubkey: peer_pubkey.clone(),
        peer_addr: "127.0.0.1:9735".to_string(),
        started: true,
    });
    let channel_id = "chan-close-force-1".to_string();
    node.ldk_runtime.upsert_channel(LdkRuntimeChannelStateData {
        temporary_channel_id: "tmp-close-force-1".to_string(),
        channel_id: channel_id.clone(),
        peer_pubkey: peer_pubkey.clone(),
        status: "opened".to_string(),
        ready: true,
        is_usable: true,
        public: false,
        capacity_sat: SDK_OPENCHANNEL_MIN_SAT,
        asset_id: None,
        asset_local_amount: None,
        virtual_open_mode: None,
    });

    node.close_channel_with_options(channel_id, Some(peer_pubkey), true)
        .expect("regular force close");

    let events: serde_json::Value = serde_json::from_str(
        &node
            .list_runtime_events_json()
            .expect("runtime events json"),
    )
    .expect("parse runtime events");
    let entries = events.as_array().expect("events array");
    let closed_index = entries
        .iter()
        .position(|entry| {
            entry["event_kind"] == "channel_closed"
                && entry["source"] == "node_api"
                && entry["applied"] == true
        })
        .expect("channel_closed event should be recorded");
    let usable_index = entries.iter().position(|entry| {
        entry["event_kind"] == "channel_usable"
            && entry["source"] == "node_api"
            && entry["applied"] == true
    });
    if let Some(usable_index) = usable_index {
        assert!(
            usable_index < closed_index,
            "channel_usable should be recorded before channel_closed"
        );
    }
}

#[wasm_bindgen_test]
fn close_channel_counterparty_event_persists_across_node_recreation_contract() {
    crate::ldk_runtime::test_utils::reset_runtime_storage_for_tests();
    crate::ln_node::test_utils::reset_runtime_event_log_storage_for_tests();
    let peer_pubkey =
        "03cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc".to_string();
    let runtime_proxy = "ws://proxy.close-counterparty-restart.example".to_string();

    let node =
        RlnWasmNode::new_with_runtime_backend(runtime_proxy.clone(), "wasm_native_ldk".to_string())
            .expect("node");
    node.ldk_runtime.upsert_peer(LdkRuntimePeerStateData {
        pubkey: peer_pubkey,
        peer_addr: "127.0.0.1:9735".to_string(),
        started: true,
    });
    node.ldk_runtime.upsert_channel(LdkRuntimeChannelStateData {
        temporary_channel_id: "tmp-close-counterparty-1".to_string(),
        channel_id: "chan-close-counterparty-1".to_string(),
        peer_pubkey: "03cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc"
            .to_string(),
        status: "opened".to_string(),
        ready: true,
        is_usable: true,
        public: false,
        capacity_sat: SDK_OPENCHANNEL_MIN_SAT,
        asset_id: None,
        asset_local_amount: None,
        virtual_open_mode: None,
    });

    let closed_payload_hex = hex::encode("channel_closed:chan-close-counterparty-1");
    let applied_js = node
        .ingest_runtime_transport_event_payload_hex_value(closed_payload_hex)
        .expect("ingest counterparty close");
    let applied: serde_json::Value = crate::js_from(applied_js).expect("parse applied");
    assert_eq!(applied["event_kind"], "channel_closed");
    assert_eq!(applied["applied"], true);

    let recreated =
        RlnWasmNode::new_with_runtime_backend(runtime_proxy, "wasm_native_ldk".to_string())
            .expect("recreated node");
    let channels_js = recreated
        .list_channels_value()
        .expect("list channels after recreation");
    let channels: serde_json::Value = crate::js_from(channels_js).expect("parse channels");
    assert!(
        channels.as_array().expect("channels array").is_empty(),
        "counterparty-closed channel should not reappear after recreation"
    );

    let events: serde_json::Value = serde_json::from_str(
        &recreated
            .list_runtime_events_json()
            .expect("runtime events should persist"),
    )
    .expect("parse runtime events");
    let entries = events.as_array().expect("events array");
    assert!(
        entries
            .iter()
            .any(|entry| entry["event_kind"] == "channel_closed"
                && entry["source"] == "runtime_transport_api"
                && entry["applied"] == true),
        "expected persisted runtime_transport_api channel_closed event"
    );
}

#[cfg(feature = "wasm-browser-infra")]
#[wasm_bindgen_test]
fn close_channel_force_after_restart_records_sequence_contract() {
    crate::ldk_runtime::test_utils::reset_runtime_storage_for_tests();
    crate::ln_node::test_utils::reset_runtime_event_log_storage_for_tests();
    let peer_pubkey =
        "03dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd".to_string();
    let runtime_proxy = "ws://proxy.close-force-restart.example".to_string();
    let channel_id = "chan-close-force-restart-1".to_string();

    let first =
        RlnWasmNode::new_with_runtime_backend(runtime_proxy.clone(), "wasm_native_ldk".to_string())
            .expect("first node");
    first.ldk_runtime.upsert_peer(LdkRuntimePeerStateData {
        pubkey: peer_pubkey.clone(),
        peer_addr: "127.0.0.1:9735".to_string(),
        started: true,
    });
    first
        .ldk_runtime
        .upsert_channel(LdkRuntimeChannelStateData {
            temporary_channel_id: "tmp-close-force-restart-1".to_string(),
            channel_id: channel_id.clone(),
            peer_pubkey: peer_pubkey.clone(),
            status: "opened".to_string(),
            ready: true,
            is_usable: true,
            public: false,
            capacity_sat: SDK_OPENCHANNEL_MIN_SAT,
            asset_id: None,
            asset_local_amount: None,
            virtual_open_mode: None,
        });
    let usable_payload_hex = hex::encode(format!("channel_usable:{channel_id}"));
    first
        .ingest_runtime_transport_event_payload_hex_value(usable_payload_hex)
        .expect("ingest usable before restart");

    let second =
        RlnWasmNode::new_with_runtime_backend(runtime_proxy, "wasm_native_ldk".to_string())
            .expect("second node");
    second
        .close_channel_with_options(channel_id.clone(), Some(peer_pubkey), true)
        .expect("force close after restart");

    let events: serde_json::Value = serde_json::from_str(
        &second
            .list_runtime_events_json()
            .expect("runtime events json"),
    )
    .expect("parse runtime events");
    let entries = events.as_array().expect("events array");
    let usable_index = entries
        .iter()
        .position(|entry| {
            entry["event_kind"] == "channel_usable"
                && entry["source"] == "runtime_transport_api"
                && entry["applied"] == true
        })
        .expect("persisted channel_usable event should exist");
    let closed_index = entries
        .iter()
        .position(|entry| {
            entry["event_kind"] == "channel_closed"
                && entry["source"] == "node_api"
                && entry["applied"] == true
        })
        .expect("channel_closed event should be recorded");
    assert!(
        usable_index < closed_index,
        "persisted channel_usable should precede post-restart channel_closed"
    );
}

#[wasm_bindgen_test]
fn multi_hop_route_without_direct_payee_finalizes_via_runtime_routed_engine_contract() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    let sender = RlnWasmNode::new_with_runtime_backend(
        "ws://proxy.sender.multi-hop.example".to_string(),
        "wasm_native_ldk".to_string(),
    )
    .expect("sender node");
    let relay = RlnWasmNode::new_with_runtime_backend(
        "ws://proxy.relay.multi-hop.example".to_string(),
        "wasm_native_ldk".to_string(),
    )
    .expect("relay node");
    let receiver = RlnWasmNode::new_with_runtime_backend(
        "ws://proxy.receiver.multi-hop.example".to_string(),
        "wasm_native_ldk".to_string(),
    )
    .expect("receiver node");

    let invoice_json = receiver
        .create_ln_invoice_json(Some(SDK_INVOICE_MIN_MSAT), 3600, None, None)
        .expect("receiver invoice");
    let invoice_doc: serde_json::Value = serde_json::from_str(&invoice_json).expect("parse");
    let invoice = invoice_doc["invoice"]
        .as_str()
        .expect("invoice")
        .to_string();
    let decoded_js = sender
        .decode_ln_invoice_value(invoice.clone())
        .expect("decode receiver invoice");
    let decoded: serde_json::Value = crate::js_from(decoded_js).expect("parse decoded");
    let payment_hash = decoded["payment_hash"]
        .as_str()
        .expect("payment hash")
        .to_string();
    let mut receiver_payment = receiver
        .ldk_runtime
        .get_payment(&payment_hash)
        .expect("receiver pending payment must exist");
    receiver_payment.status = "pending".to_string();
    receiver_payment.updated_at = unix_now_secs();
    receiver.ldk_runtime.upsert_payment(receiver_payment);

    let relay_invoice_json = relay
        .create_ln_invoice_json(Some(SDK_INVOICE_MIN_MSAT), 3600, None, None)
        .expect("relay invoice");
    let relay_invoice_doc: serde_json::Value =
        serde_json::from_str(&relay_invoice_json).expect("parse relay invoice");
    let relay_invoice = relay_invoice_doc["invoice"]
        .as_str()
        .expect("relay invoice string")
        .to_string();
    let relay_pubkey = lightning_invoice::Bolt11Invoice::from_str(&relay_invoice)
        .expect("parse relay invoice")
        .recover_payee_pub_key()
        .to_string();

    let _ = sender.list_peers_value().expect("warm sender runtime");
    let _ = relay.list_peers_value().expect("warm relay runtime");
    let _ = receiver.list_peers_value().expect("warm receiver runtime");
    sender.test_upsert_runtime_peer(relay_pubkey.clone(), "127.0.0.1:9735".to_string(), true);
    assert!(sender.test_set_runtime_peer_started(&relay_pubkey, true));
    sender
        .ldk_runtime
        .upsert_channel(LdkRuntimeChannelStateData {
            temporary_channel_id: "tmp-mh-relay".to_string(),
            channel_id: "chan-mh-relay".to_string(),
            peer_pubkey: relay_pubkey,
            status: "opened".to_string(),
            ready: true,
            is_usable: true,
            public: false,
            capacity_sat: SDK_OPENCHANNEL_MIN_SAT,
            asset_id: None,
            asset_local_amount: None,
            virtual_open_mode: None,
        });

    let send_js = sender
        .send_payment_value(invoice, Some(SDK_INVOICE_MIN_MSAT), None, None)
        .expect("send payment");
    let send_doc: serde_json::Value = crate::js_from(send_js).expect("parse send");
    assert_eq!(send_doc["status"], "succeeded");
    assert_eq!(send_doc["payment_hash"], payment_hash);

    let receiver_payment_js = receiver
        .get_payment_value(payment_hash.clone())
        .expect("receiver payment");
    let receiver_payment: serde_json::Value =
        crate::js_from(receiver_payment_js).expect("parse receiver payment");
    assert_eq!(receiver_payment["status"], "succeeded");

    let events: serde_json::Value = serde_json::from_str(
        &sender
            .list_runtime_events_json()
            .expect("runtime events json"),
    )
    .expect("parse runtime events");
    let events = events.as_array().expect("events array");
    assert!(events.iter().any(|event| {
        event.get("source").and_then(|value| value.as_str())
            == Some("runtime_routed_payment_engine")
            && event.get("payment_hash").and_then(|value| value.as_str()) == Some(&payment_hash)
            && event.get("status").and_then(|value| value.as_str()) == Some("succeeded")
    }));
}

#[test]
fn multi_hop_route_prefers_direct_usable_channel_over_routed_engine_contract() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    let sender = RlnWasmNode::new_with_runtime_backend(
        "ws://proxy.sender.multi-hop-direct-pref.example".to_string(),
        "wasm_native_ldk".to_string(),
    )
    .expect("sender node");
    let relay = RlnWasmNode::new_with_runtime_backend(
        "ws://proxy.relay.multi-hop-direct-pref.example".to_string(),
        "wasm_native_ldk".to_string(),
    )
    .expect("relay node");
    let receiver = RlnWasmNode::new_with_runtime_backend(
        "ws://proxy.receiver.multi-hop-direct-pref.example".to_string(),
        "wasm_native_ldk".to_string(),
    )
    .expect("receiver node");

    let invoice_json = receiver
        .create_ln_invoice_json(Some(SDK_INVOICE_MIN_MSAT), 3600, None, None)
        .expect("receiver invoice");
    let invoice_doc: serde_json::Value = serde_json::from_str(&invoice_json).expect("parse");
    let invoice = invoice_doc["invoice"]
        .as_str()
        .expect("invoice")
        .to_string();
    let parsed_invoice =
        lightning_invoice::Bolt11Invoice::from_str(&invoice).expect("parse receiver invoice");
    let payee_pubkey = parsed_invoice
        .payee_pub_key()
        .copied()
        .unwrap_or_else(|| parsed_invoice.recover_payee_pub_key())
        .to_string();
    let recovered_payee_pubkey = parsed_invoice.recover_payee_pub_key().to_string();
    let decoded_js = sender
        .decode_ln_invoice_value(invoice.clone())
        .expect("decode receiver invoice");
    let decoded: serde_json::Value = crate::js_from(decoded_js).expect("parse decoded");
    let payment_hash = decoded["payment_hash"]
        .as_str()
        .expect("payment hash")
        .to_string();

    let relay_invoice_json = relay
        .create_ln_invoice_json(Some(SDK_INVOICE_MIN_MSAT), 3600, None, None)
        .expect("relay invoice");
    let relay_invoice_doc: serde_json::Value =
        serde_json::from_str(&relay_invoice_json).expect("parse relay invoice");
    let relay_invoice = relay_invoice_doc["invoice"]
        .as_str()
        .expect("relay invoice")
        .to_string();
    let relay_pubkey = lightning_invoice::Bolt11Invoice::from_str(&relay_invoice)
        .expect("parse relay invoice")
        .recover_payee_pub_key()
        .to_string();

    let _ = sender.list_peers_value().expect("warm sender runtime");
    let _ = relay.list_peers_value().expect("warm relay runtime");
    let _ = receiver.list_peers_value().expect("warm receiver runtime");
    sender.test_upsert_runtime_peer(payee_pubkey.clone(), "127.0.0.1:9735".to_string(), true);
    assert!(sender.test_set_runtime_peer_started(&payee_pubkey, true));
    sender
        .ldk_runtime
        .upsert_channel(LdkRuntimeChannelStateData {
            temporary_channel_id: "tmp-mh-direct-pref-payee".to_string(),
            channel_id: "chan-mh-direct-pref-payee".to_string(),
            peer_pubkey: payee_pubkey.clone(),
            status: "opened".to_string(),
            ready: true,
            is_usable: true,
            public: false,
            capacity_sat: SDK_OPENCHANNEL_MIN_SAT,
            asset_id: None,
            asset_local_amount: None,
            virtual_open_mode: None,
        });
    if recovered_payee_pubkey != payee_pubkey {
        sender.test_upsert_runtime_peer(
            recovered_payee_pubkey.clone(),
            "127.0.0.1:9735".to_string(),
            true,
        );
        assert!(sender.test_set_runtime_peer_started(&recovered_payee_pubkey, true));
        sender
            .ldk_runtime
            .upsert_channel(LdkRuntimeChannelStateData {
                temporary_channel_id: "tmp-mh-direct-pref-payee-recovered".to_string(),
                channel_id: "chan-mh-direct-pref-payee-recovered".to_string(),
                peer_pubkey: recovered_payee_pubkey,
                status: "opened".to_string(),
                ready: true,
                is_usable: true,
                public: false,
                capacity_sat: SDK_OPENCHANNEL_MIN_SAT,
                asset_id: None,
                asset_local_amount: None,
                virtual_open_mode: None,
            });
    }
    sender.test_upsert_runtime_peer(relay_pubkey.clone(), "127.0.0.1:9736".to_string(), true);
    assert!(sender.test_set_runtime_peer_started(&relay_pubkey, true));
    sender
        .ldk_runtime
        .upsert_channel(LdkRuntimeChannelStateData {
            temporary_channel_id: "tmp-mh-direct-pref-relay".to_string(),
            channel_id: "chan-mh-direct-pref-relay".to_string(),
            peer_pubkey: relay_pubkey,
            status: "opened".to_string(),
            ready: true,
            is_usable: true,
            public: false,
            capacity_sat: SDK_OPENCHANNEL_MIN_SAT,
            asset_id: None,
            asset_local_amount: None,
            virtual_open_mode: None,
        });

    let send_js = sender
        .send_payment_value(invoice, Some(SDK_INVOICE_MIN_MSAT), None, None)
        .expect("send payment");
    let send_doc: serde_json::Value = crate::js_from(send_js).expect("parse send");
    assert_eq!(send_doc["status"], "succeeded");
    assert_eq!(send_doc["payment_hash"], payment_hash);

    let events: serde_json::Value = serde_json::from_str(
        &sender
            .list_runtime_events_json()
            .expect("runtime events json"),
    )
    .expect("parse runtime events");
    let events = events.as_array().expect("events array");
    assert!(events.iter().any(|event| {
        event.get("source").and_then(|value| value.as_str())
            == Some("runtime_channel_payment_engine")
            && event.get("event_kind").and_then(|value| value.as_str()) == Some("payment_status")
            && event.get("payment_hash").and_then(|value| value.as_str()) == Some(&payment_hash)
            && event.get("status").and_then(|value| value.as_str()) == Some("succeeded")
            && event.get("applied").and_then(|value| value.as_bool()) == Some(true)
    }));
    assert!(!events.iter().any(|event| {
        event.get("source").and_then(|value| value.as_str())
            == Some("runtime_routed_payment_engine")
            && event.get("payment_hash").and_then(|value| value.as_str()) == Some(&payment_hash)
    }));
}

#[wasm_bindgen_test]
fn multi_hop_route_requires_pending_receiver_invoice_contract() {
    crate::ldk_runtime::test_utils::reset_runtime_storage_for_tests();
    crate::ln_node::test_utils::reset_runtime_event_log_storage_for_tests();
    let sender = RlnWasmNode::new_with_runtime_backend(
        "ws://proxy.sender.multi-hop-route-gate.example".to_string(),
        "wasm_native_ldk".to_string(),
    )
    .expect("sender node");
    let relay = RlnWasmNode::new_with_runtime_backend(
        "ws://proxy.relay.multi-hop-route-gate.example".to_string(),
        "wasm_native_ldk".to_string(),
    )
    .expect("relay node");
    let receiver = RlnWasmNode::new_with_runtime_backend(
        "ws://proxy.receiver.multi-hop-route-gate.example".to_string(),
        "wasm_native_ldk".to_string(),
    )
    .expect("receiver node");

    let invoice_json = receiver
        .create_ln_invoice_json(Some(SDK_INVOICE_MIN_MSAT), 3600, None, None)
        .expect("receiver invoice");
    let invoice_doc: serde_json::Value = serde_json::from_str(&invoice_json).expect("parse");
    let invoice = invoice_doc["invoice"]
        .as_str()
        .expect("invoice")
        .to_string();
    let payee_pubkey = lightning_invoice::Bolt11Invoice::from_str(&invoice)
        .expect("parse receiver invoice")
        .recover_payee_pub_key()
        .to_string();
    let decoded_js = sender
        .decode_ln_invoice_value(invoice.clone())
        .expect("decode receiver invoice");
    let decoded: serde_json::Value = crate::js_from(decoded_js).expect("parse decoded");
    let payment_hash = decoded["payment_hash"]
        .as_str()
        .expect("payment hash")
        .to_string();

    let mut receiver_payment = receiver
        .ldk_runtime
        .get_payment(&payment_hash)
        .expect("receiver pending payment must exist");
    receiver_payment.status = "failed".to_string();
    receiver_payment.updated_at = unix_now_secs();
    receiver.ldk_runtime.upsert_payment(receiver_payment);

    let relay_invoice_json = relay
        .create_ln_invoice_json(Some(SDK_INVOICE_MIN_MSAT), 3600, None, None)
        .expect("relay invoice");
    let relay_invoice_doc: serde_json::Value =
        serde_json::from_str(&relay_invoice_json).expect("parse relay invoice");
    let relay_invoice = relay_invoice_doc["invoice"]
        .as_str()
        .expect("relay invoice")
        .to_string();
    let relay_pubkey = lightning_invoice::Bolt11Invoice::from_str(&relay_invoice)
        .expect("parse relay invoice")
        .recover_payee_pub_key()
        .to_string();

    let _ = sender.list_peers_value().expect("warm sender runtime");
    sender.ldk_runtime.upsert_peer(LdkRuntimePeerStateData {
        pubkey: relay_pubkey.clone(),
        peer_addr: "127.0.0.1:9736".to_string(),
        started: true,
    });
    sender
        .ldk_runtime
        .upsert_channel(LdkRuntimeChannelStateData {
            temporary_channel_id: "tmp-mh-route-gate-relay".to_string(),
            channel_id: "chan-mh-route-gate-relay".to_string(),
            peer_pubkey: relay_pubkey,
            status: "opened".to_string(),
            ready: true,
            is_usable: true,
            public: false,
            capacity_sat: SDK_OPENCHANNEL_MIN_SAT,
            asset_id: None,
            asset_local_amount: None,
            virtual_open_mode: None,
        });
    assert!(
        sender.ldk_runtime.get_peer(&payee_pubkey).is_none(),
        "sender should not have direct payee peer for routed scenario"
    );

    let send_js = sender
        .send_payment_value(invoice, Some(SDK_INVOICE_MIN_MSAT), None, None)
        .expect("send payment");
    let send_doc: serde_json::Value = crate::js_from(send_js).expect("parse send");
    assert_eq!(send_doc["payment_hash"], payment_hash);
    assert_eq!(
        send_doc["status"], "pending",
        "without a pending inbound receiver payment, routed simulation must not auto-succeed"
    );

    let events: serde_json::Value = serde_json::from_str(
        &sender
            .list_runtime_events_json()
            .expect("runtime events json"),
    )
    .expect("parse runtime events");
    let events = events.as_array().expect("events array");
    assert!(!events.iter().any(|event| {
        event.get("source").and_then(|value| value.as_str())
            == Some("runtime_routed_payment_engine")
            && event.get("payment_hash").and_then(|value| value.as_str()) == Some(&payment_hash)
    }));
}

#[cfg(feature = "wasm-browser-infra")]
#[wasm_bindgen_test]
fn open_channel_with_push_asset_amount_success_path_contract() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    crate::test_utils::set_wasm_sdk_identity_unlocked_for_tests();
    let node = RlnWasmNode::new_with_runtime_backend_and_id(
        "ws://proxy.open-push-asset.example".to_string(),
        "wasm_native_ldk".to_string(),
        Some("node-rt-test".to_string()),
    )
    .expect("node");
    let _ = node.list_peers_value().expect("warm runtime");
    let peer_pubkey =
        "02c11d7dfdd1ca9301508397ec8cc08758aadd95361af8562946146c33be606b58".to_string();
    node.test_upsert_runtime_peer(peer_pubkey.clone(), "127.0.0.1:9735".to_string(), true);
    assert!(node.test_set_runtime_peer_started(&peer_pubkey, true));

    let opened_js = node
        .open_channel_value(
            peer_pubkey,
            SDK_OPENRGBCHANNEL_MIN_SAT,
            false,
            Some("rgb:PushAssetParity-1".to_string()),
            Some(10),
        )
        .expect("open rgb channel with push amount");
    let opened: serde_json::Value = crate::js_from(opened_js).expect("parse opened");
    assert_eq!(opened["status"], "opened");
    assert_eq!(opened["is_usable"], true);
    assert_eq!(opened["asset_id"], "rgb:PushAssetParity-1");
    assert_eq!(opened["asset_local_amount"], 10);
}

#[test]
fn vanilla_payment_on_rgb_channel_success_path_contract() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    let sender = RlnWasmNode::new_with_runtime_backend(
        "ws://proxy.vanilla-over-rgb.sender.example".to_string(),
        "wasm_native_ldk".to_string(),
    )
    .expect("sender node");
    let receiver = RlnWasmNode::new_with_runtime_backend(
        "ws://proxy.vanilla-over-rgb.receiver.example".to_string(),
        "wasm_native_ldk".to_string(),
    )
    .expect("receiver node");

    let invoice_json = receiver
        .create_ln_invoice_json(Some(SDK_INVOICE_MIN_MSAT), 3600, None, None)
        .expect("receiver invoice");
    let invoice_doc: serde_json::Value = serde_json::from_str(&invoice_json).expect("parse");
    let invoice = invoice_doc["invoice"]
        .as_str()
        .expect("invoice")
        .to_string();
    let parsed_invoice =
        lightning_invoice::Bolt11Invoice::from_str(&invoice).expect("parse receiver invoice");
    let payee_pubkey = parsed_invoice
        .payee_pub_key()
        .copied()
        .unwrap_or_else(|| parsed_invoice.recover_payee_pub_key())
        .to_string();
    let recovered_payee_pubkey = parsed_invoice.recover_payee_pub_key().to_string();

    let _ = sender.list_peers_value().expect("warm sender runtime");
    let _ = receiver.list_peers_value().expect("warm receiver runtime");
    sender.test_upsert_runtime_peer(payee_pubkey.clone(), "127.0.0.1:9735".to_string(), true);
    assert!(sender.test_set_runtime_peer_started(&payee_pubkey, true));
    sender
        .ldk_runtime
        .upsert_channel(LdkRuntimeChannelStateData {
            temporary_channel_id: "tmp-vanilla-rgb-1".to_string(),
            channel_id: "chan-vanilla-rgb-1".to_string(),
            peer_pubkey: payee_pubkey.clone(),
            status: "opened".to_string(),
            ready: true,
            is_usable: true,
            public: false,
            capacity_sat: SDK_OPENRGBCHANNEL_MIN_SAT,
            asset_id: Some("rgb:VanillaOverRgbParity-1".to_string()),
            asset_local_amount: Some(100),
            virtual_open_mode: None,
        });
    if recovered_payee_pubkey != payee_pubkey {
        sender.test_upsert_runtime_peer(
            recovered_payee_pubkey.clone(),
            "127.0.0.1:9735".to_string(),
            true,
        );
        assert!(sender.test_set_runtime_peer_started(&recovered_payee_pubkey, true));
        sender
            .ldk_runtime
            .upsert_channel(LdkRuntimeChannelStateData {
                temporary_channel_id: "tmp-vanilla-rgb-1-recovered".to_string(),
                channel_id: "chan-vanilla-rgb-1-recovered".to_string(),
                peer_pubkey: recovered_payee_pubkey,
                status: "opened".to_string(),
                ready: true,
                is_usable: true,
                public: false,
                capacity_sat: SDK_OPENRGBCHANNEL_MIN_SAT,
                asset_id: Some("rgb:VanillaOverRgbParity-1".to_string()),
                asset_local_amount: Some(100),
                virtual_open_mode: None,
            });
    }
    let channels_js = sender.list_channels_value().expect("list sender channels");
    let channels: serde_json::Value = crate::js_from(channels_js).expect("parse channels");
    let channels = channels.as_array().expect("channels array");
    assert_eq!(channels.len(), 1);
    assert_eq!(channels[0]["peer_pubkey"], payee_pubkey);
    assert_eq!(channels[0]["is_usable"], true);

    let send_js = sender
        .send_payment_value(invoice, Some(SDK_INVOICE_MIN_MSAT), None, None)
        .expect("send vanilla payment over rgb channel");
    let send_doc: serde_json::Value = crate::js_from(send_js).expect("parse send");
    assert_eq!(send_doc["status"], "succeeded");
    let payment_hash = send_doc["payment_hash"]
        .as_str()
        .expect("payment hash")
        .to_string();

    let events: serde_json::Value = serde_json::from_str(
        &sender
            .list_runtime_events_json()
            .expect("runtime events json"),
    )
    .expect("parse runtime events");
    let events = events.as_array().expect("events array");
    assert!(events.iter().any(|event| {
        event.get("source").and_then(|value| value.as_str())
            == Some("runtime_channel_payment_engine")
            && event.get("event_kind").and_then(|value| value.as_str()) == Some("payment_status")
            && event.get("payment_hash").and_then(|value| value.as_str()) == Some(&payment_hash)
            && event.get("status").and_then(|value| value.as_str()) == Some("succeeded")
            && event.get("applied").and_then(|value| value.as_bool()) == Some(true)
    }));
}

#[test]
fn sdk_facade_forwards_network_info() {
    let sdk = crate::RlnWasmSdk::new();
    let node = sdk
        .new_node("ws://127.0.0.1:3001".to_string())
        .expect("new node");
    let network_js = sdk.network_info_value(&node).expect("network info");
    let network: serde_json::Value =
        serde_wasm_bindgen::from_value(network_js).expect("parse network");
    assert_eq!(network["network"], "regtest");
    assert_eq!(network["height"], 0);
}

#[test]
fn sdk_facade_forwards_node_info() {
    let sdk = crate::RlnWasmSdk::new();
    let node = sdk
        .new_node("ws://127.0.0.1:3001".to_string())
        .expect("new node");
    let node_js = sdk.node_info_value(&node).expect("node info");
    let info: serde_json::Value = serde_wasm_bindgen::from_value(node_js).expect("parse node");
    assert_eq!(info["ldk_over_websocket"], true);
    assert!(info["runtime"].as_str().is_some());
}

#[test]
fn sdk_facade_forwards_event_ingestion_status_update() {
    let sdk = crate::RlnWasmSdk::new();
    let node = sdk
        .new_node("ws://127.0.0.1:3001".to_string())
        .expect("new node");
    let keysend_js = sdk
        .keysend_value(
            &node,
            "0334cc4bca04ce3d1537310f55e91ec4cec7e5a88fa0fba20a24cce1fe6de2a2b0".to_string(),
            3_000_000,
            None,
            None,
        )
        .expect("keysend");
    let keysend: serde_json::Value =
        serde_wasm_bindgen::from_value(keysend_js).expect("parse keysend");
    let payment_hash = keysend["payment_hash"]
        .as_str()
        .expect("payment_hash str")
        .to_string();

    let payload_json = serde_json::json!({
        "payment_hash": payment_hash,
        "status": "succeeded"
    })
    .to_string();
    let payload_hex = hex::encode(payload_json.as_bytes());

    let updated_js = sdk
        .ingest_read_event_payload_hex(&node, payload_hex)
        .expect("ingest");
    let updated: serde_json::Value =
        serde_wasm_bindgen::from_value(updated_js).expect("parse updated");
    assert_eq!(updated["status"], "succeeded");
}

#[test]
fn sdk_node_handle_flow_updates_payment_status() {
    let sdk = crate::RlnWasmSdk::new();
    let node = sdk
        .create_node_handle("ws://127.0.0.1:3001".to_string())
        .expect("node handle");

    let keysend_js = node
        .keysend_value(
            "0334cc4bca04ce3d1537310f55e91ec4cec7e5a88fa0fba20a24cce1fe6de2a2b0".to_string(),
            3_000_000,
            None,
            None,
        )
        .expect("keysend");
    let keysend: serde_json::Value =
        serde_wasm_bindgen::from_value(keysend_js).expect("keysend parse");
    let payment_hash = keysend["payment_hash"].as_str().expect("hash str");

    let payload = serde_json::json!({
        "payment_hash": payment_hash,
        "status": "succeeded"
    })
    .to_string();
    let payload_hex = hex::encode(payload.as_bytes());
    let updated_js = node
        .ingest_read_event_payload_hex(payload_hex)
        .expect("ingest");
    let updated: serde_json::Value =
        serde_wasm_bindgen::from_value(updated_js).expect("updated parse");
    assert_eq!(updated["status"], "succeeded");
}

#[test]
fn sdk_node_handle_explicit_update_payment_status() {
    let sdk = crate::RlnWasmSdk::new();
    let node = sdk
        .create_node_handle("ws://127.0.0.1:3001".to_string())
        .expect("node handle");

    let keysend_js = node
        .keysend_value(
            "0334cc4bca04ce3d1537310f55e91ec4cec7e5a88fa0fba20a24cce1fe6de2a2b0".to_string(),
            3_000_000,
            None,
            None,
        )
        .expect("keysend");
    let keysend: serde_json::Value =
        serde_wasm_bindgen::from_value(keysend_js).expect("parse keysend");
    let payment_hash = keysend["payment_hash"]
        .as_str()
        .expect("hash str")
        .to_string();

    let updated_js = node
        .update_payment_status(payment_hash, "succeeded".to_string())
        .expect("update");
    let updated: serde_json::Value =
        serde_wasm_bindgen::from_value(updated_js).expect("parse updated");
    assert_eq!(updated["status"], "succeeded");
}

#[test]
fn sdk_node_handle_ingest_transport_event_json_contract() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    let sdk = crate::RlnWasmSdk::new();
    let node = sdk
        .create_node_handle("ws://127.0.0.1:3001".to_string())
        .expect("node handle");

    let payload_hex = hex::encode("peer_disconnected:02abab");
    let json = node
        .ingest_runtime_transport_event_payload_hex_json(payload_hex)
        .expect("transport ingest json");
    let data: serde_json::Value = serde_json::from_str(&json).expect("json parse");
    assert_eq!(data["event_kind"], "peer_disconnected");
    assert_eq!(data["applied"], false);

    let events_js = node
        .list_runtime_events_value()
        .expect("list runtime events value");
    let events: serde_json::Value =
        serde_wasm_bindgen::from_value(events_js).expect("events parse");
    let arr = events.as_array().expect("events array");
    assert!(!arr.is_empty());
    let last = arr.last().expect("last event");
    assert_eq!(last["source"], "runtime_transport_api");
    assert_eq!(last["event_kind"], "peer_disconnected");
    assert_eq!(last["applied"], false);
}

#[test]
fn sdk_node_handle_ingest_transport_event_invalid_payload_contract() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    let sdk = crate::RlnWasmSdk::new();
    let node = sdk
        .create_node_handle("ws://127.0.0.1:3001".to_string())
        .expect("node handle");

    let err = node
        .ingest_runtime_transport_event_payload_hex_value("not-hex".to_string())
        .expect_err("invalid payload must fail");
    assert_eq!(
        err.as_string().unwrap_or_default(),
        "unrecognized transport event payload format"
    );

    let events_json = node
        .list_runtime_events_json()
        .expect("list runtime events json");
    let events: serde_json::Value = serde_json::from_str(&events_json).expect("events parse");
    let arr = events.as_array().expect("events array");
    assert!(!arr.is_empty());
    let last = arr.last().expect("last event");
    assert_eq!(last["source"], "runtime_transport_api");
    assert_eq!(last["event_kind"], "invalid_hex_payload");
    assert_eq!(last["applied"], false);
}

#[test]
fn sdk_node_handle_ingest_read_event_json_contract() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    let sdk = crate::RlnWasmSdk::new();
    let node = sdk
        .create_node_handle("ws://127.0.0.1:3001".to_string())
        .expect("node handle");

    let keysend_js = node
        .keysend_value(
            "0334cc4bca04ce3d1537310f55e91ec4cec7e5a88fa0fba20a24cce1fe6de2a2b0".to_string(),
            3_000_000,
            None,
            None,
        )
        .expect("keysend");
    let keysend: serde_json::Value =
        serde_wasm_bindgen::from_value(keysend_js).expect("parse keysend");
    let payment_hash = keysend["payment_hash"]
        .as_str()
        .expect("payment hash")
        .to_string();

    let payload_hex = hex::encode(
        serde_json::json!({
            "payment_hash": payment_hash,
            "status": "succeeded"
        })
        .to_string(),
    );
    let json = node
        .ingest_read_event_payload_hex_json(payload_hex)
        .expect("ingest read event json");
    let payment: serde_json::Value = serde_json::from_str(&json).expect("payment parse");
    assert_eq!(payment["status"], "succeeded");

    let events_json = node
        .list_runtime_events_json()
        .expect("list runtime events json");
    let events: serde_json::Value = serde_json::from_str(&events_json).expect("events parse");
    let arr = events.as_array().expect("events array");
    assert!(!arr.is_empty());
    let last = arr.last().expect("last event");
    assert_eq!(last["event_kind"], "payment_status");
    assert_eq!(last["applied"], true);
    assert_eq!(last["status"], "succeeded");
}

#[test]
fn sdk_node_handle_forwards_network_info() {
    let sdk = crate::RlnWasmSdk::new();
    let node = sdk
        .create_node_handle("ws://127.0.0.1:3001".to_string())
        .expect("node handle");
    let network_js = node.network_info_value().expect("network info");
    let network: serde_json::Value =
        serde_wasm_bindgen::from_value(network_js).expect("parse network");
    assert_eq!(network["network"], "regtest");
    assert_eq!(network["height"], 0);
}

#[test]
fn sdk_node_handle_forwards_node_info() {
    let sdk = crate::RlnWasmSdk::new();
    let node = sdk
        .create_node_handle("ws://127.0.0.1:3001".to_string())
        .expect("node handle");
    let node_js = node.node_info_value().expect("node info");
    let info: serde_json::Value = serde_wasm_bindgen::from_value(node_js).expect("parse node");
    assert_eq!(info["ldk_over_websocket"], true);
    assert!(info["runtime"].as_str().is_some());
}

#[test]
fn sdk_facade_forwards_peer_channel_read_views_contract() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    let sdk = crate::RlnWasmSdk::new();
    let node = sdk
        .new_node_with_runtime_backend(
            "ws://127.0.0.1:3381".to_string(),
            "wasm_native_ldk".to_string(),
        )
        .expect("new node");
    let peer_pubkey =
        "02e7e7e7e7e7e7e7e7e7e7e7e7e7e7e7e7e7e7e7e7e7e7e7e7e7e7e7e7e7e7e7".to_string();
    node.test_upsert_runtime_peer(peer_pubkey.clone(), "127.0.0.1:9748".to_string(), true);
    assert!(node.test_set_runtime_peer_started(&peer_pubkey, true));

    let opened_js = sdk
        .open_channel_value(&node, peer_pubkey, 5_506, false, None, None)
        .expect("open channel");
    let opened: serde_json::Value = serde_wasm_bindgen::from_value(opened_js).expect("parse");
    let temporary_channel_id = opened["temporary_channel_id"]
        .as_str()
        .expect("temporary channel id")
        .to_string();
    let channel_id = opened["channel_id"].as_str().expect("channel id");

    let peers_js = sdk.list_peers_value(&node).expect("list peers");
    let peers: serde_json::Value = serde_wasm_bindgen::from_value(peers_js).expect("parse peers");
    let peers = peers.as_array().expect("peers array");
    assert_eq!(peers.len(), 1);
    assert!(peers[0]["started"].is_boolean());

    let channels_js = sdk.list_channels_value(&node).expect("list channels");
    let channels: serde_json::Value =
        serde_wasm_bindgen::from_value(channels_js).expect("parse channels");
    let channels = channels.as_array().expect("channels array");
    assert_eq!(channels.len(), 1);
    assert_eq!(channels[0]["channel_id"], channel_id);

    let resolved = sdk
        .get_channel_id(&node, temporary_channel_id)
        .expect("resolve channel id");
    assert_eq!(resolved, channel_id);
}

#[test]
fn sdk_node_handle_forwards_peer_channel_read_views_contract() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    let sdk = crate::RlnWasmSdk::new();
    let node = sdk
        .create_node_handle_with_runtime_backend(
            "ws://127.0.0.1:3382".to_string(),
            "wasm_native_ldk".to_string(),
        )
        .expect("node handle");
    let peer_pubkey =
        "02e6e6e6e6e6e6e6e6e6e6e6e6e6e6e6e6e6e6e6e6e6e6e6e6e6e6e6e6e6e6e6".to_string();
    node.inner
        .test_upsert_runtime_peer(peer_pubkey.clone(), "127.0.0.1:9749".to_string(), true);
    assert!(node.inner.test_set_runtime_peer_started(&peer_pubkey, true));

    let opened_js = node
        .open_channel_value(peer_pubkey, 5_506, false, None, None)
        .expect("open channel");
    let opened: serde_json::Value = serde_wasm_bindgen::from_value(opened_js).expect("parse");
    let temporary_channel_id = opened["temporary_channel_id"]
        .as_str()
        .expect("temporary channel id")
        .to_string();
    let channel_id = opened["channel_id"].as_str().expect("channel id");

    let peers_js = node.list_peers_value().expect("list peers");
    let peers: serde_json::Value = serde_wasm_bindgen::from_value(peers_js).expect("parse peers");
    let peers = peers.as_array().expect("peers array");
    assert_eq!(peers.len(), 1);
    assert!(peers[0]["started"].is_boolean());

    let channels_js = node.list_channels_value().expect("list channels");
    let channels: serde_json::Value =
        serde_wasm_bindgen::from_value(channels_js).expect("parse channels");
    let channels = channels.as_array().expect("channels array");
    assert_eq!(channels.len(), 1);
    assert_eq!(channels[0]["channel_id"], channel_id);

    let resolved = node
        .get_channel_id(temporary_channel_id)
        .expect("resolve channel id");
    assert_eq!(resolved, channel_id);
}

#[test]
fn sdk_facade_forwards_ldk_runtime_status() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    let sdk = crate::RlnWasmSdk::new();
    let node = sdk
        .new_node("ws://127.0.0.1:3001".to_string())
        .expect("new node");
    let status_js = sdk.ldk_runtime_status_value(&node).expect("runtime status");
    let status: serde_json::Value =
        serde_wasm_bindgen::from_value(status_js).expect("parse status");
    assert_eq!(status["backend"], "wasm_native_ldk");
    assert_eq!(status["lifecycle_state"], "cold");
    assert_eq!(status["ready"], false);
}

#[test]
fn sdk_node_handle_ldk_runtime_status_transitions_after_keysend() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    let sdk = crate::RlnWasmSdk::new();
    let node = sdk
        .create_node_handle("ws://127.0.0.1:3001".to_string())
        .expect("node handle");
    let cold_js = node
        .ldk_runtime_status_value()
        .expect("cold runtime status");
    let cold: serde_json::Value = serde_wasm_bindgen::from_value(cold_js).expect("parse cold");
    assert_eq!(cold["ready"], false);

    node.keysend_value(
        "0334cc4bca04ce3d1537310f55e91ec4cec7e5a88fa0fba20a24cce1fe6de2a2b0".to_string(),
        3_000_000,
        None,
        None,
    )
    .expect("keysend");

    let running_js = node
        .ldk_runtime_status_value()
        .expect("running runtime status");
    let running: serde_json::Value =
        serde_wasm_bindgen::from_value(running_js).expect("parse running");
    assert_eq!(running["backend"], "wasm_native_ldk");
    assert_eq!(running["lifecycle_state"], "running");
    assert_eq!(running["ready"], true);
}

#[test]
fn sdk_node_runtime_status_restores_after_stop() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    let node_first = RlnWasmNode::new("ws://127.0.0.1:3999".to_string()).expect("node");

    node_first
        .keysend_value(
            "0334cc4bca04ce3d1537310f55e91ec4cec7e5a88fa0fba20a24cce1fe6de2a2b0".to_string(),
            3_000_000,
            None,
            None,
        )
        .expect("keysend");

    block_on(node_first.close_all_peers()).expect("close_all_peers");

    let node_second = RlnWasmNode::new("ws://127.0.0.1:3999".to_string()).expect("node");

    let cold_js = node_second
        .ldk_runtime_status_value()
        .expect("runtime status before restart");
    let cold: serde_json::Value = serde_wasm_bindgen::from_value(cold_js).expect("parse cold");
    assert_eq!(cold["lifecycle_state"], "cold");
    assert_eq!(cold["ready"], false);

    node_second
        .keysend_value(
            "0334cc4bca04ce3d1537310f55e91ec4cec7e5a88fa0fba20a24cce1fe6de2a2b0".to_string(),
            3_000_000,
            None,
            None,
        )
        .expect("keysend");

    let restored_js = node_second
        .ldk_runtime_status_value()
        .expect("runtime status after restart");
    let restored: serde_json::Value =
        serde_wasm_bindgen::from_value(restored_js).expect("parse restored");
    assert_eq!(restored["lifecycle_state"], "running_restored");
    assert_eq!(restored["ready"], true);
}

#[test]
fn sdk_node_runtime_backend_bridge_status_contract() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    let node = RlnWasmNode::new_with_runtime_backend(
        "ws://127.0.0.1:3333".to_string(),
        "wasm_native_ldk".to_string(),
    )
    .expect("node");

    let cold_js = node
        .ldk_runtime_status_value()
        .expect("runtime status before start");
    let cold: serde_json::Value = serde_wasm_bindgen::from_value(cold_js).expect("parse cold");
    assert_eq!(cold["backend"], "wasm_native_ldk");
    assert_eq!(cold["lifecycle_state"], "cold");
    assert_eq!(cold["ready"], false);

    node.keysend_value(
        "0334cc4bca04ce3d1537310f55e91ec4cec7e5a88fa0fba20a24cce1fe6de2a2b0".to_string(),
        3_000_000,
        None,
        None,
    )
    .expect("keysend");

    let running_js = node
        .ldk_runtime_status_value()
        .expect("runtime status after start");
    let running: serde_json::Value =
        serde_wasm_bindgen::from_value(running_js).expect("parse running");
    assert_eq!(running["backend"], "wasm_native_ldk");
    assert_eq!(running["lifecycle_state"], "running");
    assert_eq!(running["ready"], true);
}

#[test]
fn sdk_node_runtime_backend_invalid_contract() {
    let err = match RlnWasmNode::new_with_runtime_backend(
        "ws://127.0.0.1:3333".to_string(),
        "unknown_backend".to_string(),
    ) {
        Ok(_) => panic!("should fail"),
        Err(err) => err,
    };
    assert_eq!(
        err.as_string().unwrap_or_default(),
        "unknown runtime backend: unknown_backend"
    );
}

#[test]
fn sdk_facade_new_node_with_runtime_backend_bridge_contract() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    let sdk = crate::RlnWasmSdk::new();
    let node = sdk
        .new_node_with_runtime_backend(
            "ws://127.0.0.1:3335".to_string(),
            "wasm_native_ldk".to_string(),
        )
        .expect("bridge node");

    let status_js = node.ldk_runtime_status_value().expect("runtime status");
    let status: serde_json::Value =
        serde_wasm_bindgen::from_value(status_js).expect("status parse");
    assert_eq!(status["backend"], "wasm_native_ldk");
}

#[test]
fn sdk_facade_create_node_handle_with_runtime_backend_bridge_contract() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    let sdk = crate::RlnWasmSdk::new();
    let node = sdk
        .create_node_handle_with_runtime_backend(
            "ws://127.0.0.1:3336".to_string(),
            "wasm_native_ldk".to_string(),
        )
        .expect("bridge node handle");

    let status_js = node.ldk_runtime_status_value().expect("runtime status");
    let status: serde_json::Value =
        serde_wasm_bindgen::from_value(status_js).expect("status parse");
    assert_eq!(status["backend"], "wasm_native_ldk");
}

#[test]
fn sdk_facade_runtime_backend_invalid_contract() {
    let sdk = crate::RlnWasmSdk::new();
    let err = match sdk.new_node_with_runtime_backend(
        "ws://127.0.0.1:3337".to_string(),
        "unknown_backend".to_string(),
    ) {
        Ok(_) => panic!("must fail"),
        Err(err) => err,
    };
    assert_eq!(
        err.as_string().unwrap_or_default(),
        "unknown runtime backend: unknown_backend"
    );

    let err = match sdk.create_node_handle_with_runtime_backend(
        "ws://127.0.0.1:3338".to_string(),
        "unknown_backend".to_string(),
    ) {
        Ok(_) => panic!("must fail"),
        Err(err) => err,
    };
    assert_eq!(
        err.as_string().unwrap_or_default(),
        "unknown runtime backend: unknown_backend"
    );
}

#[test]
fn sdk_facade_bridge_channel_open_requires_connected_peer_contract() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    let sdk = crate::RlnWasmSdk::new();
    let peer_pubkey =
        "0334cc4bca04ce3d1537310f55e91ec4cec7e5a88fa0fba20a24cce1fe6de2a2b0".to_string();

    let node = sdk
        .new_node_with_runtime_backend(
            "ws://127.0.0.1:3340".to_string(),
            "wasm_native_ldk".to_string(),
        )
        .expect("bridge node");
    let peers_js = sdk.list_peers_value(&node).expect("list peers");
    let peers: serde_json::Value = serde_wasm_bindgen::from_value(peers_js).expect("parse peers");
    let peers = peers.as_array().expect("peers array");
    assert!(peers.is_empty());

    let err = sdk
        .open_channel_value(&node, peer_pubkey, 5_506, false, None, None)
        .expect_err("open must fail without connected peer");
    assert_eq!(err.as_string().unwrap_or_default(), "peer is not connected");
}

#[test]
fn sdk_node_handle_bridge_channel_open_requires_connected_peer_contract() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    let sdk = crate::RlnWasmSdk::new();
    let peer_pubkey =
        "0334cc4bca04ce3d1537310f55e91ec4cec7e5a88fa0fba20a24cce1fe6de2a2b0".to_string();

    let node = sdk
        .create_node_handle_with_runtime_backend(
            "ws://127.0.0.1:3341".to_string(),
            "wasm_native_ldk".to_string(),
        )
        .expect("bridge handle");
    let peers_js = node.list_peers_value().expect("list peers");
    let peers: serde_json::Value = serde_wasm_bindgen::from_value(peers_js).expect("parse peers");
    let peers = peers.as_array().expect("peers array");
    assert!(peers.is_empty());

    let err = node
        .open_channel_value(peer_pubkey, 5_506, false, None, None)
        .expect_err("open must fail without connected peer");
    assert_eq!(err.as_string().unwrap_or_default(), "peer is not connected");
}

#[test]
fn sdk_facade_bridge_send_payment_without_connected_peer_contract() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    let sdk = crate::RlnWasmSdk::new();
    let node = sdk
        .new_node_with_runtime_backend(
            "ws://127.0.0.1:3342".to_string(),
            "wasm_native_ldk".to_string(),
        )
        .expect("bridge node");

    let invoice_json = sdk
        .create_ln_invoice_json(&node, Some(3_000_000), 3600, None, None)
        .expect("create invoice");
    let invoice_doc: serde_json::Value = serde_json::from_str(&invoice_json).expect("parse");
    let invoice = invoice_doc["invoice"]
        .as_str()
        .expect("invoice")
        .to_string();

    let payment_js = sdk
        .send_payment_value(&node, invoice, Some(3_000_000), None, None)
        .expect("send payment should return failed status without peers");
    let payment: serde_json::Value = serde_wasm_bindgen::from_value(payment_js).expect("parse");
    assert_eq!(payment["status"], "failed");
}

#[test]
fn sdk_node_handle_bridge_keysend_without_connected_peer_contract() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    let sdk = crate::RlnWasmSdk::new();
    let node = sdk
        .create_node_handle_with_runtime_backend(
            "ws://127.0.0.1:3343".to_string(),
            "wasm_native_ldk".to_string(),
        )
        .expect("bridge handle");

    let keysend_js = node
        .keysend_value(
            "02efefefefefefefefefefefefefefefefefefefefefefefefefefefefefefef".to_string(),
            3_000_000,
            None,
            None,
        )
        .expect("keysend should return failed status without connected peer");
    let keysend: serde_json::Value =
        serde_wasm_bindgen::from_value(keysend_js).expect("parse keysend");
    assert_eq!(keysend["status"], "failed");
}

#[test]
fn sdk_facade_bridge_send_payment_requires_connected_known_payee_contract() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    let sdk = crate::RlnWasmSdk::new();
    let sender = sdk
        .new_node_with_runtime_backend(
            "ws://127.0.0.1:3344".to_string(),
            "wasm_native_ldk".to_string(),
        )
        .expect("sender node");
    let receiver = sdk
        .new_node_with_runtime_backend(
            "ws://127.0.0.1:3345".to_string(),
            "wasm_native_ldk".to_string(),
        )
        .expect("receiver node");

    let invoice_json = sdk
        .create_ln_invoice_json(&receiver, Some(3_000_000), 3600, None, None)
        .expect("create invoice");
    let invoice_doc: serde_json::Value = serde_json::from_str(&invoice_json).expect("parse");
    let invoice = invoice_doc["invoice"]
        .as_str()
        .expect("invoice")
        .to_string();
    let decoded_js = sdk
        .decode_ln_invoice_value(&sender, invoice.clone())
        .expect("decode invoice");
    let decoded: serde_json::Value = serde_wasm_bindgen::from_value(decoded_js).expect("parse");
    let payee_pubkey = decoded["payee_pubkey"].as_str().expect("payee").to_string();

    sender.test_upsert_runtime_peer(
        "0334cc4bca04ce3d1537310f55e91ec4cec7e5a88fa0fba20a24cce1fe6de2a2b0".to_string(),
        "127.0.0.1:9735".to_string(),
        true,
    );
    sender.test_upsert_runtime_peer(payee_pubkey.clone(), "127.0.0.1:9736".to_string(), false);

    let first_js = sdk
        .send_payment_value(&sender, invoice.clone(), Some(3_000_000), None, None)
        .expect("send payment");
    let first: serde_json::Value = serde_wasm_bindgen::from_value(first_js).expect("parse");
    assert_eq!(first["status"], "failed");

    assert!(sender.test_set_runtime_peer_started(&payee_pubkey, true));
    let second_js = sdk
        .send_payment_value(&sender, invoice, Some(3_000_000), None, None)
        .expect("send payment");
    let second: serde_json::Value = serde_wasm_bindgen::from_value(second_js).expect("parse");
    assert_eq!(second["status"], "pending");
}

#[test]
fn sdk_node_handle_bridge_send_payment_requires_connected_known_payee_contract() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    let sdk = crate::RlnWasmSdk::new();
    let sender = sdk
        .create_node_handle_with_runtime_backend(
            "ws://127.0.0.1:3346".to_string(),
            "wasm_native_ldk".to_string(),
        )
        .expect("sender handle");
    let receiver = sdk
        .new_node_with_runtime_backend(
            "ws://127.0.0.1:3347".to_string(),
            "wasm_native_ldk".to_string(),
        )
        .expect("receiver node");

    let invoice_json = receiver
        .create_ln_invoice_json(Some(3_000_000), 3600, None, None)
        .expect("create invoice");
    let invoice_doc: serde_json::Value = serde_json::from_str(&invoice_json).expect("parse");
    let invoice = invoice_doc["invoice"]
        .as_str()
        .expect("invoice")
        .to_string();
    let decoded_js = sender
        .decode_ln_invoice_value(invoice.clone())
        .expect("decode invoice");
    let decoded: serde_json::Value = serde_wasm_bindgen::from_value(decoded_js).expect("parse");
    let payee_pubkey = decoded["payee_pubkey"].as_str().expect("payee").to_string();

    sender.inner.test_upsert_runtime_peer(
        "0334cc4bca04ce3d1537310f55e91ec4cec7e5a88fa0fba20a24cce1fe6de2a2b0".to_string(),
        "127.0.0.1:9737".to_string(),
        true,
    );
    sender.inner.test_upsert_runtime_peer(
        payee_pubkey.clone(),
        "127.0.0.1:9738".to_string(),
        false,
    );

    let first_js = sender
        .send_payment_value(invoice.clone(), Some(3_000_000), None, None)
        .expect("send payment");
    let first: serde_json::Value = serde_wasm_bindgen::from_value(first_js).expect("parse");
    assert_eq!(first["status"], "failed");

    assert!(sender
        .inner
        .test_set_runtime_peer_started(&payee_pubkey, true));
    let second_js = sender
        .send_payment_value(invoice, Some(3_000_000), None, None)
        .expect("send payment");
    let second: serde_json::Value = serde_wasm_bindgen::from_value(second_js).expect("parse");
    assert_eq!(second["status"], "pending");
}

#[test]
fn sdk_facade_bridge_reconnect_payload_reactivates_payee_for_send_payment_contract() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    let sdk = crate::RlnWasmSdk::new();
    let sender = sdk
        .new_node_with_runtime_backend(
            "ws://127.0.0.1:3348".to_string(),
            "wasm_native_ldk".to_string(),
        )
        .expect("sender node");
    let receiver = sdk
        .new_node_with_runtime_backend(
            "ws://127.0.0.1:3349".to_string(),
            "wasm_native_ldk".to_string(),
        )
        .expect("receiver node");

    let invoice_json = sdk
        .create_ln_invoice_json(&receiver, Some(3_000_000), 3600, None, None)
        .expect("create invoice");
    let invoice_doc: serde_json::Value = serde_json::from_str(&invoice_json).expect("parse");
    let invoice = invoice_doc["invoice"]
        .as_str()
        .expect("invoice")
        .to_string();
    let decoded_js = sdk
        .decode_ln_invoice_value(&sender, invoice.clone())
        .expect("decode invoice");
    let decoded: serde_json::Value = serde_wasm_bindgen::from_value(decoded_js).expect("parse");
    let payee_pubkey = decoded["payee_pubkey"].as_str().expect("payee").to_string();

    sender.test_upsert_runtime_peer(
        "0334cc4bca04ce3d1537310f55e91ec4cec7e5a88fa0fba20a24cce1fe6de2a2b0".to_string(),
        "127.0.0.1:9739".to_string(),
        true,
    );
    sender.test_upsert_runtime_peer(payee_pubkey.clone(), "127.0.0.1:9740".to_string(), false);

    let first_js = sdk
        .send_payment_value(&sender, invoice.clone(), Some(3_000_000), None, None)
        .expect("send payment");
    let first: serde_json::Value = serde_wasm_bindgen::from_value(first_js).expect("parse");
    assert_eq!(first["status"], "failed");

    let payload_hex = hex::encode(format!("peer_reconnected:{payee_pubkey}").as_bytes());
    let reconnect_js = sdk
        .ingest_runtime_transport_event_payload_hex_value(&sender, payload_hex)
        .expect("reconnect event");
    let reconnect: serde_json::Value = crate::js_from(reconnect_js).expect("parse reconnect");
    assert_eq!(reconnect["event_kind"], "peer_reconnected");
    assert_eq!(reconnect["applied"], true);

    let second_js = sdk
        .send_payment_value(&sender, invoice, Some(3_000_000), None, None)
        .expect("send payment");
    let second: serde_json::Value = serde_wasm_bindgen::from_value(second_js).expect("parse");
    assert_eq!(second["status"], "pending");
}

#[test]
fn sdk_node_handle_bridge_reconnect_payload_reactivates_payee_for_send_payment_contract() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    let sdk = crate::RlnWasmSdk::new();
    let sender = sdk
        .create_node_handle_with_runtime_backend(
            "ws://127.0.0.1:3350".to_string(),
            "wasm_native_ldk".to_string(),
        )
        .expect("sender handle");
    let receiver = sdk
        .new_node_with_runtime_backend(
            "ws://127.0.0.1:3351".to_string(),
            "wasm_native_ldk".to_string(),
        )
        .expect("receiver node");

    let invoice_json = receiver
        .create_ln_invoice_json(Some(3_000_000), 3600, None, None)
        .expect("create invoice");
    let invoice_doc: serde_json::Value = serde_json::from_str(&invoice_json).expect("parse");
    let invoice = invoice_doc["invoice"]
        .as_str()
        .expect("invoice")
        .to_string();
    let decoded_js = sender
        .decode_ln_invoice_value(invoice.clone())
        .expect("decode invoice");
    let decoded: serde_json::Value = serde_wasm_bindgen::from_value(decoded_js).expect("parse");
    let payee_pubkey = decoded["payee_pubkey"].as_str().expect("payee").to_string();

    sender.inner.test_upsert_runtime_peer(
        "0334cc4bca04ce3d1537310f55e91ec4cec7e5a88fa0fba20a24cce1fe6de2a2b0".to_string(),
        "127.0.0.1:9741".to_string(),
        true,
    );
    sender.inner.test_upsert_runtime_peer(
        payee_pubkey.clone(),
        "127.0.0.1:9742".to_string(),
        false,
    );

    let first_js = sender
        .send_payment_value(invoice.clone(), Some(3_000_000), None, None)
        .expect("send payment");
    let first: serde_json::Value = serde_wasm_bindgen::from_value(first_js).expect("parse");
    assert_eq!(first["status"], "failed");

    let payload_hex = hex::encode(format!("peer_reconnected:{payee_pubkey}").as_bytes());
    let reconnect_js = sender
        .ingest_runtime_transport_event_payload_hex_value(payload_hex)
        .expect("reconnect event");
    let reconnect: serde_json::Value = crate::js_from(reconnect_js).expect("parse reconnect");
    assert_eq!(reconnect["event_kind"], "peer_reconnected");
    assert_eq!(reconnect["applied"], true);

    let second_js = sender
        .send_payment_value(invoice, Some(3_000_000), None, None)
        .expect("send payment");
    let second: serde_json::Value = serde_wasm_bindgen::from_value(second_js).expect("parse");
    assert_eq!(second["status"], "pending");
}

#[test]
fn sdk_facade_create_ln_invoice_native_contract() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    let sdk = crate::RlnWasmSdk::new();
    let node = sdk
        .new_node("ws://127.0.0.1:3001".to_string())
        .expect("new node");
    let invoice_js = sdk
        .create_ln_invoice_json(&node, Some(3_000_000), 3600, None, None)
        .expect("create invoice");
    let created: serde_json::Value = serde_json::from_str(&invoice_js).expect("parse json");
    let invoice = created["invoice"]
        .as_str()
        .expect("invoice str")
        .to_string();
    assert!(!invoice.trim().is_empty());

    let decoded_js = sdk
        .decode_ln_invoice_value(&node, invoice.clone())
        .expect("decode invoice");
    let decoded: serde_json::Value = serde_wasm_bindgen::from_value(decoded_js).expect("parse");
    assert_eq!(decoded["amt_msat"], 3_000_000);

    let status_js = sdk
        .invoice_status_value(&node, invoice)
        .expect("invoice status");
    let status: serde_json::Value = serde_wasm_bindgen::from_value(status_js).expect("parse");
    assert_eq!(status["status"], "pending");
}

#[test]
fn sdk_node_handle_create_ln_invoice_native_contract() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    let sdk = crate::RlnWasmSdk::new();
    let node = sdk
        .create_node_handle("ws://127.0.0.1:3001".to_string())
        .expect("node handle");
    let invoice_json = node
        .create_ln_invoice_json(Some(3_000_000), 3600, None, None)
        .expect("create invoice");
    let created: serde_json::Value = serde_json::from_str(&invoice_json).expect("parse json");
    let invoice = created["invoice"]
        .as_str()
        .expect("invoice str")
        .to_string();
    assert!(!invoice.trim().is_empty());
    let status_js = node.invoice_status_value(invoice).expect("invoice status");
    let status: serde_json::Value = serde_wasm_bindgen::from_value(status_js).expect("parse");
    assert_eq!(status["status"], "pending");
}

#[test]
fn sdk_node_handle_create_ln_invoice_asset_pair_validation_contract() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    let sdk = crate::RlnWasmSdk::new();
    let node = sdk
        .create_node_handle("ws://127.0.0.1:3001".to_string())
        .expect("node handle");
    let err = node
        .create_ln_invoice_json(Some(3_000_000), 3600, Some("asset".to_string()), None)
        .expect_err("should fail");
    let msg = err.as_string().expect("error string");
    assert_eq!(msg, "asset_id and asset_amount must be provided together");
}

#[test]
fn sdk_facade_forwards_get_payment() {
    let sdk = crate::RlnWasmSdk::new();
    let node = sdk
        .new_node("ws://127.0.0.1:3001".to_string())
        .expect("new node");
    let keysend_js = sdk
        .keysend_value(
            &node,
            "0334cc4bca04ce3d1537310f55e91ec4cec7e5a88fa0fba20a24cce1fe6de2a2b0".to_string(),
            3_000_000,
            None,
            None,
        )
        .expect("keysend");
    let keysend: serde_json::Value = serde_wasm_bindgen::from_value(keysend_js).expect("parse");
    let payment_hash = keysend["payment_hash"].as_str().expect("hash").to_string();

    let payment_js = sdk
        .get_payment_value(&node, payment_hash)
        .expect("get payment");
    let payment: serde_json::Value = serde_wasm_bindgen::from_value(payment_js).expect("parse");
    assert_eq!(payment["status"], "failed");
}

#[test]
fn sdk_node_handle_forwards_get_payment() {
    let sdk = crate::RlnWasmSdk::new();
    let node = sdk
        .create_node_handle("ws://127.0.0.1:3001".to_string())
        .expect("node handle");
    let keysend_js = node
        .keysend_value(
            "0334cc4bca04ce3d1537310f55e91ec4cec7e5a88fa0fba20a24cce1fe6de2a2b0".to_string(),
            3_000_000,
            None,
            None,
        )
        .expect("keysend");
    let keysend: serde_json::Value = serde_wasm_bindgen::from_value(keysend_js).expect("parse");
    let payment_hash = keysend["payment_hash"].as_str().expect("hash").to_string();

    let payment_js = node.get_payment_value(payment_hash).expect("get payment");
    let payment: serde_json::Value = serde_wasm_bindgen::from_value(payment_js).expect("parse");
    assert_eq!(payment["status"], "failed");
}

#[test]
fn sdk_node_handle_get_payment_accepts_trimmed_hash() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    let sdk = crate::RlnWasmSdk::new();
    let node = sdk
        .create_node_handle("ws://127.0.0.1:3001".to_string())
        .expect("node handle");
    let keysend_js = node
        .keysend_value(
            "0334cc4bca04ce3d1537310f55e91ec4cec7e5a88fa0fba20a24cce1fe6de2a2b0".to_string(),
            3_000_000,
            None,
            None,
        )
        .expect("keysend");
    let keysend: serde_json::Value = serde_wasm_bindgen::from_value(keysend_js).expect("parse");
    let payment_hash = keysend["payment_hash"].as_str().expect("hash").to_string();

    let payment_js = node
        .get_payment_value(format!(" {payment_hash} "))
        .expect("get payment");
    let payment: serde_json::Value = serde_wasm_bindgen::from_value(payment_js).expect("parse");
    assert_eq!(payment["payment_hash"], payment_hash);
}

#[test]
fn sdk_node_handle_list_payments_deterministic_order_contract() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    let sdk = crate::RlnWasmSdk::new();
    let node = sdk
        .create_node_handle("ws://127.0.0.1:3001".to_string())
        .expect("node handle");

    node.keysend_value(
        "0334cc4bca04ce3d1537310f55e91ec4cec7e5a88fa0fba20a24cce1fe6de2a2b0".to_string(),
        3_000_000,
        None,
        None,
    )
    .expect("keysend 1");
    node.keysend_value(
        "0334cc4bca04ce3d1537310f55e91ec4cec7e5a88fa0fba20a24cce1fe6de2a2b0".to_string(),
        3_000_000,
        None,
        None,
    )
    .expect("keysend 2");

    let payments_js = node.list_payments_value().expect("list");
    let payments: serde_json::Value = serde_wasm_bindgen::from_value(payments_js).expect("parse");
    let payments = payments.as_array().expect("array");
    assert!(payments.len() >= 2);
    for pair in payments.windows(2) {
        let prev_created = pair[0]["created_at"].as_u64().expect("prev created");
        let next_created = pair[1]["created_at"].as_u64().expect("next created");
        let prev_hash = pair[0]["payment_hash"].as_str().expect("prev hash");
        let next_hash = pair[1]["payment_hash"].as_str().expect("next hash");
        assert!(prev_created <= next_created);
        if prev_created == next_created {
            assert!(prev_hash <= next_hash);
        }
    }
}

#[test]
fn sdk_facade_invoice_status_empty_error_contract() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    let sdk = crate::RlnWasmSdk::new();
    let node = sdk
        .new_node("ws://127.0.0.1:3001".to_string())
        .expect("new node");
    let err = sdk
        .invoice_status_value(&node, "".to_string())
        .expect_err("should fail");
    assert_eq!(
        err.as_string().unwrap_or_default(),
        "invoice cannot be empty"
    );
}

#[test]
fn sdk_node_handle_invoice_status_empty_error_contract() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    let sdk = crate::RlnWasmSdk::new();
    let node = sdk
        .create_node_handle("ws://127.0.0.1:3001".to_string())
        .expect("node handle");
    let err = node
        .invoice_status_value("".to_string())
        .expect_err("should fail");
    assert_eq!(
        err.as_string().unwrap_or_default(),
        "invoice cannot be empty"
    );
}

#[test]
fn sdk_facade_decode_rgb_invoice_empty_error_contract() {
    let sdk = crate::RlnWasmSdk::new();
    let node = sdk
        .new_node("ws://127.0.0.1:3001".to_string())
        .expect("new node");
    let err = sdk
        .decode_rgb_invoice_json(&node, "".to_string())
        .expect_err("should fail");
    assert_eq!(
        err.as_string().unwrap_or_default(),
        "invoice cannot be empty"
    );
}

#[test]
fn sdk_facade_decode_ln_invoice_empty_error_contract() {
    let sdk = crate::RlnWasmSdk::new();
    let node = sdk
        .new_node("ws://127.0.0.1:3001".to_string())
        .expect("new node");
    let err = sdk
        .decode_ln_invoice_json(&node, "".to_string())
        .expect_err("should fail");
    assert_eq!(
        err.as_string().unwrap_or_default(),
        "invoice cannot be empty"
    );
}

#[test]
fn sdk_node_handle_decode_rgb_invoice_empty_error_contract() {
    let sdk = crate::RlnWasmSdk::new();
    let node = sdk
        .create_node_handle("ws://127.0.0.1:3001".to_string())
        .expect("node handle");
    let err = node
        .decode_rgb_invoice_json("".to_string())
        .expect_err("should fail");
    assert_eq!(
        err.as_string().unwrap_or_default(),
        "invoice cannot be empty"
    );
}

#[test]
fn sdk_node_handle_decode_ln_invoice_empty_error_contract() {
    let sdk = crate::RlnWasmSdk::new();
    let node = sdk
        .create_node_handle("ws://127.0.0.1:3001".to_string())
        .expect("node handle");
    let err = node
        .decode_ln_invoice_json("".to_string())
        .expect_err("should fail");
    assert_eq!(
        err.as_string().unwrap_or_default(),
        "invoice cannot be empty"
    );
}

#[test]
fn sdk_facade_chain_sync_start_status_stop_contract() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    let sdk = crate::RlnWasmSdk::new();
    let node = sdk
        .new_node("ws://127.0.0.1:3390".to_string())
        .expect("new node");

    let started_js = sdk
        .chain_sync_start_value(&node, "http://127.0.0.1:3002".to_string(), Some(5_000))
        .expect("start");
    let started: serde_json::Value = serde_wasm_bindgen::from_value(started_js).expect("parse");
    assert_eq!(started["running"], true);
    assert_eq!(started["poll_interval_ms"], 5_000);

    let status_js = sdk.chain_sync_status_value(&node).expect("status");
    let status: serde_json::Value = serde_wasm_bindgen::from_value(status_js).expect("parse");
    assert_eq!(status["running"], true);
    assert_eq!(status["indexer_url"], "http://127.0.0.1:3002");

    let stopped_js = sdk.chain_sync_stop_value(&node).expect("stop");
    let stopped: serde_json::Value = serde_wasm_bindgen::from_value(stopped_js).expect("parse");
    assert_eq!(stopped["running"], false);
}

#[test]
fn sdk_node_handle_chain_sync_start_status_stop_contract() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    let sdk = crate::RlnWasmSdk::new();
    let node = sdk
        .create_node_handle("ws://127.0.0.1:3391".to_string())
        .expect("node handle");

    let started_js = node
        .chain_sync_start_value("http://127.0.0.1:3002".to_string(), Some(7_000))
        .expect("start");
    let started: serde_json::Value = serde_wasm_bindgen::from_value(started_js).expect("parse");
    assert_eq!(started["running"], true);
    assert_eq!(started["poll_interval_ms"], 7_000);

    let status_js = node.chain_sync_status_value().expect("status");
    let status: serde_json::Value = serde_wasm_bindgen::from_value(status_js).expect("parse");
    assert_eq!(status["running"], true);
    assert_eq!(status["indexer_url"], "http://127.0.0.1:3002");

    let stopped_js = node.chain_sync_stop_value().expect("stop");
    let stopped: serde_json::Value = serde_wasm_bindgen::from_value(stopped_js).expect("parse");
    assert_eq!(stopped["running"], false);
}

#[test]
fn sdk_facade_ldk_runtime_components_contract() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    let sdk = crate::RlnWasmSdk::new();
    let node = sdk
        .new_node("ws://127.0.0.1:3392".to_string())
        .expect("new node");
    let components_js = sdk
        .ldk_runtime_components_value(&node)
        .expect("components status");
    let components: serde_json::Value =
        serde_wasm_bindgen::from_value(components_js).expect("parse");
    assert_eq!(components["started"], true);
    assert_eq!(components["fee_estimator_ready"], true);
    assert_eq!(components["broadcaster_ready"], true);
    assert!(components["key_manager_fingerprint"].as_str().is_some());
}

#[test]
fn sdk_node_handle_ldk_runtime_components_contract() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    let sdk = crate::RlnWasmSdk::new();
    let node = sdk
        .create_node_handle("ws://127.0.0.1:3393".to_string())
        .expect("node handle");
    node.create_ln_invoice_json(Some(3_000_000), 3600, None, None)
        .expect("invoice");
    let components_js = node
        .ldk_runtime_components_value()
        .expect("components status");
    let components: serde_json::Value =
        serde_wasm_bindgen::from_value(components_js).expect("parse");
    assert_eq!(components["invoices_created"], 1);
    assert_eq!(components["started"], true);
}

#[test]
fn sdk_facade_list_rgb_ln_transfers_contract() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    let sdk = crate::RlnWasmSdk::new();
    let node = sdk
        .new_node("ws://127.0.0.1:3394".to_string())
        .expect("new node");
    let keysend_js = sdk
        .keysend_value(
            &node,
            "0334cc4bca04ce3d1537310f55e91ec4cec7e5a88fa0fba20a24cce1fe6de2a2b0".to_string(),
            3_000_000,
            Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string()),
            Some(9),
        )
        .expect("keysend");
    let keysend: serde_json::Value = serde_wasm_bindgen::from_value(keysend_js).expect("parse");
    let payment_hash = keysend["payment_hash"].as_str().expect("hash");

    let transfers_js = sdk
        .list_rgb_ln_transfers_value(&node)
        .expect("list rgb ln transfers");
    let transfers: serde_json::Value = serde_wasm_bindgen::from_value(transfers_js).expect("parse");
    let arr = transfers.as_array().expect("array");
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["payment_hash"], payment_hash);
    assert_eq!(arr[0]["asset_amount"], 9);
}

#[test]
fn sdk_node_handle_list_rgb_ln_transfers_contract() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    let sdk = crate::RlnWasmSdk::new();
    let node = sdk
        .create_node_handle("ws://127.0.0.1:3395".to_string())
        .expect("node handle");
    let keysend_js = node
        .keysend_value(
            "0334cc4bca04ce3d1537310f55e91ec4cec7e5a88fa0fba20a24cce1fe6de2a2b0".to_string(),
            3_000_000,
            Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string()),
            Some(12),
        )
        .expect("keysend");
    let keysend: serde_json::Value = serde_wasm_bindgen::from_value(keysend_js).expect("parse");
    let payment_hash = keysend["payment_hash"].as_str().expect("hash").to_string();
    node.update_payment_status(payment_hash.clone(), "succeeded".to_string())
        .expect("update status");

    let transfers_js = node
        .list_rgb_ln_transfers_value()
        .expect("list rgb ln transfers");
    let transfers: serde_json::Value = serde_wasm_bindgen::from_value(transfers_js).expect("parse");
    let arr = transfers.as_array().expect("array");
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["payment_hash"], payment_hash);
    assert_eq!(arr[0]["status"], "succeeded");
}

#[test]
fn sdk_facade_chain_sync_surface_contract() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    let sdk = crate::RlnWasmSdk::new();
    let node = sdk
        .new_node("ws://127.0.0.1:3396".to_string())
        .expect("new node");

    let started_js = sdk
        .chain_sync_start_value(&node, "http://127.0.0.1:3002".to_string(), Some(6_000))
        .expect("chain sync start");
    let started: serde_json::Value = serde_wasm_bindgen::from_value(started_js).expect("parse");
    assert_eq!(started["running"], true);
    assert_eq!(started["poll_interval_ms"], 6000);

    let status_js = sdk
        .chain_sync_status_value(&node)
        .expect("chain sync status");
    let status: serde_json::Value = serde_wasm_bindgen::from_value(status_js).expect("parse");
    assert_eq!(status["running"], true);
    assert_eq!(status["indexer_url"], "http://127.0.0.1:3002");

    let stopped_js = sdk.chain_sync_stop_value(&node).expect("chain sync stop");
    let stopped: serde_json::Value = serde_wasm_bindgen::from_value(stopped_js).expect("parse");
    assert_eq!(stopped["running"], false);
}

#[test]
fn sdk_node_handle_runtime_components_and_rgb_ln_transfer_surface_contract() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    let sdk = crate::RlnWasmSdk::new();
    let node = sdk
        .create_node_handle("ws://127.0.0.1:3397".to_string())
        .expect("node handle");

    let components_js = node
        .ldk_runtime_components_value()
        .expect("runtime components");
    let components: serde_json::Value =
        serde_wasm_bindgen::from_value(components_js).expect("parse");
    assert_eq!(components["started"], true);
    assert_eq!(components["fee_estimator_ready"], true);
    assert!(components["key_manager_fingerprint"].as_str().is_some());

    let keysend_js = node
        .keysend_value(
            "0334cc4bca04ce3d1537310f55e91ec4cec7e5a88fa0fba20a24cce1fe6de2a2b0".to_string(),
            3_000_000,
            Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string()),
            Some(13),
        )
        .expect("rgb keysend");
    let keysend: serde_json::Value = serde_wasm_bindgen::from_value(keysend_js).expect("parse");
    let payment_hash = keysend["payment_hash"].as_str().expect("hash").to_string();

    node.update_payment_status(payment_hash.clone(), "succeeded".to_string())
        .expect("status update");

    let transfers_js = node
        .list_rgb_ln_transfers_value()
        .expect("list rgb ln transfers");
    let transfers: serde_json::Value =
        serde_wasm_bindgen::from_value(transfers_js).expect("parse transfers");
    let arr = transfers.as_array().expect("array");
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["payment_hash"], payment_hash);
    assert_eq!(arr[0]["asset_amount"], 13);
    assert_eq!(arr[0]["status"], "succeeded");
}

#[test]
fn sdk_facade_native_runtime_core_surface_contract() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    let sdk = crate::RlnWasmSdk::new();
    let node = sdk
        .new_node("ws://127.0.0.1:3398".to_string())
        .expect("new node");

    let status_js = sdk
        .native_runtime_core_status_value(&node)
        .expect("native runtime core status");
    let status: serde_json::Value = serde_wasm_bindgen::from_value(status_js).expect("parse");
    assert!(status["lifecycle_state"].is_string());
    assert!(status["schema_version"].as_u64().unwrap_or(0) >= 1);
    assert_eq!(status["queued_events"].as_u64().unwrap_or(0), 0);

    let _ = sdk
        .keysend_value(
            &node,
            "0334cc4bca04ce3d1537310f55e91ec4cec7e5a88fa0fba20a24cce1fe6de2a2b0".to_string(),
            3_000_000,
            None,
            None,
        )
        .expect("keysend");

    let drained_js = sdk
        .drain_native_runtime_queue_value(&node)
        .expect("drain native queue");
    let drained: serde_json::Value = serde_wasm_bindgen::from_value(drained_js).expect("parse");
    let entries = drained.as_array().expect("array");
    assert!(!entries.is_empty());
    assert!(entries
        .iter()
        .any(|entry| entry["event_kind"] == "payment_status"));
}

#[test]
fn sdk_node_handle_native_runtime_core_surface_contract() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    let sdk = crate::RlnWasmSdk::new();
    let node = sdk
        .create_node_handle("ws://127.0.0.1:3399".to_string())
        .expect("node handle");

    let status_js = node
        .native_runtime_core_status_value()
        .expect("native runtime core status");
    let status: serde_json::Value = serde_wasm_bindgen::from_value(status_js).expect("parse");
    assert!(status["lifecycle_state"].is_string());
    assert!(status["schema_version"].as_u64().unwrap_or(0) >= 1);
    assert_eq!(status["queued_events"].as_u64().unwrap_or(0), 0);

    let _ = node
        .keysend_value(
            "0334cc4bca04ce3d1537310f55e91ec4cec7e5a88fa0fba20a24cce1fe6de2a2b0".to_string(),
            3_000_000,
            None,
            None,
        )
        .expect("keysend");

    let drained_js = node
        .drain_native_runtime_queue_value()
        .expect("drain native queue");
    let drained: serde_json::Value = serde_wasm_bindgen::from_value(drained_js).expect("parse");
    let entries = drained.as_array().expect("array");
    assert!(!entries.is_empty());
    assert!(entries
        .iter()
        .any(|entry| entry["event_kind"] == "payment_status"));
}

#[test]
fn sdk_chain_sync_status_exposes_tip_regression_fields_contract() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    let sdk = crate::RlnWasmSdk::new();
    let node = sdk
        .new_node("ws://127.0.0.1:3400".to_string())
        .expect("new node");

    let started_js = sdk
        .chain_sync_start_value(&node, "http://127.0.0.1:3002".to_string(), Some(6_000))
        .expect("start");
    let started: serde_json::Value = serde_wasm_bindgen::from_value(started_js).expect("parse");
    assert_eq!(started["tip_regressed"], false);
    assert!(started["last_tip_regression_at"].is_null());

    let status_js = sdk.chain_sync_status_value(&node).expect("status");
    let status: serde_json::Value = serde_wasm_bindgen::from_value(status_js).expect("parse");
    assert_eq!(status["tip_regressed"], false);
    assert!(status["last_tip_regression_at"].is_null());
}

#[test]
fn sdk_facade_forwards_node_payment_views() {
    let sdk = crate::RlnWasmSdk::new();
    let node = sdk
        .new_node("ws://127.0.0.1:3001".to_string())
        .expect("new node");
    let _ = node
        .keysend_value(
            "0334cc4bca04ce3d1537310f55e91ec4cec7e5a88fa0fba20a24cce1fe6de2a2b0".to_string(),
            3_000_000,
            None,
            None,
        )
        .expect("keysend");

    let via_sdk = sdk.list_payments_value(&node).expect("list payments");
    let payments: serde_json::Value = serde_wasm_bindgen::from_value(via_sdk).expect("parse");
    let arr = payments.as_array().expect("array");
    assert!(!arr.is_empty(), "payments should not be empty");
}

#[test]
fn sdk_facade_forwards_node_keysend_write_path() {
    let sdk = crate::RlnWasmSdk::new();
    let node = sdk
        .new_node("ws://127.0.0.1:3001".to_string())
        .expect("new node");
    let keysend_js = sdk
        .keysend_value(
            &node,
            "0334cc4bca04ce3d1537310f55e91ec4cec7e5a88fa0fba20a24cce1fe6de2a2b0".to_string(),
            3_000_000,
            None,
            None,
        )
        .expect("keysend");
    let keysend: serde_json::Value = serde_wasm_bindgen::from_value(keysend_js).expect("parse");
    assert_eq!(keysend["status"], "failed");
    assert!(keysend["payment_hash"].as_str().is_some());
}

#[test]
fn sdk_facade_sign_message_native_contract() {
    let sdk = crate::RlnWasmSdk::new();
    let node = sdk
        .new_node("ws://127.0.0.1:3001".to_string())
        .expect("new node");
    let signed = sdk
        .sign_message_json(&node, "hello".to_string())
        .expect("sign should succeed");
    let doc: serde_json::Value = serde_json::from_str(&signed).expect("parse");
    let sig = doc["signed_message"].as_str().expect("sig");
    assert_eq!(sig.len(), 130);
}

#[test]
fn sdk_node_handle_sign_message_native_contract() {
    let sdk = crate::RlnWasmSdk::new();
    let node = sdk
        .create_node_handle("ws://127.0.0.1:3001".to_string())
        .expect("node handle");
    let signed_a = node
        .sign_message_json("  hello ".to_string())
        .expect("sign should succeed");
    let signed_b = node
        .sign_message_json("hello".to_string())
        .expect("sign should succeed");
    let a: serde_json::Value = serde_json::from_str(&signed_a).expect("parse");
    let b: serde_json::Value = serde_json::from_str(&signed_b).expect("parse");
    assert_eq!(a["signed_message"], b["signed_message"]);
}

#[test]
fn sdk_facade_ingest_read_event_supports_payment_success_alias_contract() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    let sdk = crate::RlnWasmSdk::new();
    let node = sdk
        .new_node("ws://127.0.0.1:3374".to_string())
        .expect("node");
    let keysend_js = sdk
        .keysend_value(
            &node,
            "02ededededededededededededededededededededededededededededededed".to_string(),
            3_000_000,
            None,
            None,
        )
        .expect("keysend");
    let keysend: serde_json::Value = serde_wasm_bindgen::from_value(keysend_js).expect("parse");
    let payment_hash = keysend["payment_hash"].as_str().expect("hash");

    let payload_hex = hex::encode(format!("payment-success:{payment_hash}").as_bytes());
    let updated_js = sdk
        .ingest_read_event_payload_hex(&node, payload_hex)
        .expect("ingest");
    let updated: serde_json::Value = serde_wasm_bindgen::from_value(updated_js).expect("parse");
    assert_eq!(updated["status"], "succeeded");
}

#[test]
fn sdk_node_handle_ingest_read_event_supports_payment_timeout_alias_contract() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    let sdk = crate::RlnWasmSdk::new();
    let node = sdk
        .create_node_handle("ws://127.0.0.1:3375".to_string())
        .expect("node handle");
    let keysend_js = node
        .keysend_value(
            "02fcfcfcfcfcfcfcfcfcfcfcfcfcfcfcfcfcfcfcfcfcfcfcfcfcfcfcfcfcfcfc".to_string(),
            3_000_000,
            None,
            None,
        )
        .expect("keysend");
    let keysend: serde_json::Value = serde_wasm_bindgen::from_value(keysend_js).expect("parse");
    let payment_hash = keysend["payment_hash"].as_str().expect("hash");

    let payload_json = serde_json::json!({
        "event": "payment timeout",
        "payment_hash": payment_hash,
    })
    .to_string();
    let payload_hex = hex::encode(payload_json.as_bytes());
    let updated_js = node
        .ingest_read_event_payload_hex(payload_hex)
        .expect("ingest");
    let updated: serde_json::Value = serde_wasm_bindgen::from_value(updated_js).expect("parse");
    assert_eq!(updated["status"], "expired");
}

#[test]
fn sdk_facade_ingest_read_event_supports_event_name_and_payment_id_alias_contract() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    let sdk = crate::RlnWasmSdk::new();
    let node = sdk
        .new_node("ws://127.0.0.1:3376".to_string())
        .expect("node");
    let keysend_js = sdk
        .keysend_value(
            &node,
            "02ececececececececececececececececececececececececececececececec".to_string(),
            3_000_000,
            None,
            None,
        )
        .expect("keysend");
    let keysend: serde_json::Value = serde_wasm_bindgen::from_value(keysend_js).expect("parse");
    let payment_hash = keysend["payment_hash"].as_str().expect("hash");

    let payload_json = serde_json::json!({
        "eventName": "PaymentCompleted",
        "paymentId": payment_hash,
    })
    .to_string();
    let payload_hex = hex::encode(payload_json.as_bytes());
    let updated_js = sdk
        .ingest_read_event_payload_hex(&node, payload_hex)
        .expect("ingest");
    let updated: serde_json::Value = serde_wasm_bindgen::from_value(updated_js).expect("parse");
    assert_eq!(updated["status"], "succeeded");
}

#[test]
fn sdk_facade_ingest_read_event_supports_status_alias_contract() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    let sdk = crate::RlnWasmSdk::new();
    let node = sdk
        .new_node("ws://127.0.0.1:3377".to_string())
        .expect("node");
    let keysend_js = sdk
        .keysend_value(
            &node,
            "02ebebebebebebebebebebebebebebebebebebebebebebebebebebebebebebeb".to_string(),
            3_000_000,
            None,
            None,
        )
        .expect("keysend");
    let keysend: serde_json::Value = serde_wasm_bindgen::from_value(keysend_js).expect("parse");
    let payment_hash = keysend["payment_hash"].as_str().expect("hash");

    let payload_json = serde_json::json!({
        "payment_hash": payment_hash,
        "status": "PaymentSent",
    })
    .to_string();
    let payload_hex = hex::encode(payload_json.as_bytes());
    let updated_js = sdk
        .ingest_read_event_payload_hex(&node, payload_hex)
        .expect("ingest");
    let updated: serde_json::Value = serde_wasm_bindgen::from_value(updated_js).expect("parse");
    assert_eq!(updated["status"], "succeeded");
}

#[test]
fn sdk_facade_ingest_read_event_supports_state_field_alias_contract() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    let sdk = crate::RlnWasmSdk::new();
    let node = sdk
        .new_node("ws://127.0.0.1:3379".to_string())
        .expect("node");
    let keysend_js = sdk
        .keysend_value(
            &node,
            "02e9e9e9e9e9e9e9e9e9e9e9e9e9e9e9e9e9e9e9e9e9e9e9e9e9e9e9e9e9e9e9".to_string(),
            3_000_000,
            None,
            None,
        )
        .expect("keysend");
    let keysend: serde_json::Value = serde_wasm_bindgen::from_value(keysend_js).expect("parse");
    let payment_hash = keysend["payment_hash"].as_str().expect("hash");

    let payload_json = serde_json::json!({
        "payment_hash": payment_hash,
        "state": "PaymentSent",
    })
    .to_string();
    let payload_hex = hex::encode(payload_json.as_bytes());
    let updated_js = sdk
        .ingest_read_event_payload_hex(&node, payload_hex)
        .expect("ingest");
    let updated: serde_json::Value = serde_wasm_bindgen::from_value(updated_js).expect("parse");
    assert_eq!(updated["status"], "succeeded");
}

#[test]
fn sdk_node_handle_ingest_read_event_supports_status_alias_contract() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    let sdk = crate::RlnWasmSdk::new();
    let node = sdk
        .create_node_handle("ws://127.0.0.1:3378".to_string())
        .expect("node handle");
    let keysend_js = node
        .keysend_value(
            "02eaeaeaeaeaeaeaeaeaeaeaeaeaeaeaeaeaeaeaeaeaeaeaeaeaeaeaeaeaeaea".to_string(),
            3_000_000,
            None,
            None,
        )
        .expect("keysend");
    let keysend: serde_json::Value = serde_wasm_bindgen::from_value(keysend_js).expect("parse");
    let payment_hash = keysend["payment_hash"].as_str().expect("hash");

    let payload_json = serde_json::json!({
        "payment_hash": payment_hash,
        "status": "payment timed out",
    })
    .to_string();
    let payload_hex = hex::encode(payload_json.as_bytes());
    let updated_js = node
        .ingest_read_event_payload_hex(payload_hex)
        .expect("ingest");
    let updated: serde_json::Value = serde_wasm_bindgen::from_value(updated_js).expect("parse");
    assert_eq!(updated["status"], "expired");
}

#[test]
fn sdk_node_handle_ingest_read_event_supports_payment_status_field_alias_contract() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    let sdk = crate::RlnWasmSdk::new();
    let node = sdk
        .create_node_handle("ws://127.0.0.1:3380".to_string())
        .expect("node handle");
    let keysend_js = node
        .keysend_value(
            "02e8e8e8e8e8e8e8e8e8e8e8e8e8e8e8e8e8e8e8e8e8e8e8e8e8e8e8e8e8e8e8".to_string(),
            3_000_000,
            None,
            None,
        )
        .expect("keysend");
    let keysend: serde_json::Value = serde_wasm_bindgen::from_value(keysend_js).expect("parse");
    let payment_hash = keysend["payment_hash"].as_str().expect("hash");

    let payload_json = serde_json::json!({
        "payment_hash": payment_hash,
        "paymentStatus": "payment_error",
    })
    .to_string();
    let payload_hex = hex::encode(payload_json.as_bytes());
    let updated_js = node
        .ingest_read_event_payload_hex(payload_hex)
        .expect("ingest");
    let updated: serde_json::Value = serde_wasm_bindgen::from_value(updated_js).expect("parse");
    assert_eq!(updated["status"], "failed");
}

#[test]
fn sdk_facade_transport_json_alias_reconnect_contract() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    let sdk = crate::RlnWasmSdk::new();
    let node = sdk
        .new_node_with_runtime_backend(
            "ws://127.0.0.1:3352".to_string(),
            "wasm_native_ldk".to_string(),
        )
        .expect("node");
    let peer_pubkey =
        "02e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0".to_string();
    node.test_upsert_runtime_peer(peer_pubkey.clone(), "127.0.0.1:9736".to_string(), false);

    let payload_json = serde_json::json!({
        "event": "peer_reconnected",
        "peer_pubkey": peer_pubkey,
    })
    .to_string();
    let payload_hex = hex::encode(payload_json.as_bytes());
    let applied_js = sdk
        .ingest_runtime_transport_event_payload_hex_value(&node, payload_hex)
        .expect("ingest transport");
    let applied: serde_json::Value = crate::js_from(applied_js).expect("parse");
    assert_eq!(applied["event_kind"], "peer_reconnected");
    assert_eq!(applied["applied"], true);
}

#[test]
fn sdk_node_handle_transport_json_alias_reconnect_contract() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    let sdk = crate::RlnWasmSdk::new();
    let node = sdk
        .create_node_handle_with_runtime_backend(
            "ws://127.0.0.1:3353".to_string(),
            "wasm_native_ldk".to_string(),
        )
        .expect("node handle");
    let peer_pubkey =
        "02d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0".to_string();
    node.inner
        .test_upsert_runtime_peer(peer_pubkey.clone(), "127.0.0.1:9737".to_string(), false);

    let payload_json = serde_json::json!({
        "event": "PeerReconnected",
        "id": peer_pubkey,
    })
    .to_string();
    let payload_hex = hex::encode(payload_json.as_bytes());
    let applied_js = node
        .ingest_runtime_transport_event_payload_hex_value(payload_hex)
        .expect("ingest transport");
    let applied: serde_json::Value = crate::js_from(applied_js).expect("parse");
    assert_eq!(applied["event_kind"], "peer_reconnected");
    assert_eq!(applied["applied"], true);
}

#[test]
fn sdk_node_handle_transport_json_node_id_and_channel_id_alias_contract() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    let sdk = crate::RlnWasmSdk::new();
    let node = sdk
        .create_node_handle_with_runtime_backend(
            "ws://127.0.0.1:3361".to_string(),
            "wasm_native_ldk".to_string(),
        )
        .expect("node handle");
    let peer_pubkey =
        "0334cc4bca04ce3d1537310f55e91ec4cec7e5a88fa0fba20a24cce1fe6de2a2b0".to_string();
    let _ = node.list_peers_value().expect("warm runtime");
    node.inner
        .test_upsert_runtime_peer(peer_pubkey.clone(), "127.0.0.1:9745".to_string(), false);

    let peer_payload = serde_json::json!({
        "event": "peer_connected",
        "node_id": peer_pubkey,
    })
    .to_string();
    let peer_payload_hex = hex::encode(peer_payload.as_bytes());
    let peer_applied_js = node
        .ingest_runtime_transport_event_payload_hex_value(peer_payload_hex)
        .expect("ingest peer transport");
    let peer_applied: serde_json::Value = crate::js_from(peer_applied_js).expect("parse");
    assert_eq!(peer_applied["event_kind"], "peer_reconnected");
    assert_eq!(peer_applied["applied"], true);

    let opened_js = node
        .open_channel_value(peer_pubkey, 5_506, false, None, None)
        .expect("open channel");
    let opened: serde_json::Value = serde_wasm_bindgen::from_value(opened_js).expect("parse");
    let channel_id = opened["channel_id"]
        .as_str()
        .expect("channel id")
        .to_string();
    let channel_payload = serde_json::json!({
        "type": "channel_unusable",
        "channelId": channel_id,
    })
    .to_string();
    let channel_payload_hex = hex::encode(channel_payload.as_bytes());
    let channel_applied_js = node
        .ingest_runtime_transport_event_payload_hex_value(channel_payload_hex)
        .expect("ingest channel transport");
    let channel_applied: serde_json::Value = crate::js_from(channel_applied_js).expect("parse");
    assert_eq!(channel_applied["event_kind"], "channel_unusable");
    assert_eq!(channel_applied["applied"], true);
}

#[test]
fn sdk_facade_transport_event_name_alias_reconnect_contract() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    let sdk = crate::RlnWasmSdk::new();
    let node = sdk
        .new_node_with_runtime_backend(
            "ws://127.0.0.1:3362".to_string(),
            "wasm_native_ldk".to_string(),
        )
        .expect("node");
    let peer_pubkey =
        "024f4f4f4f4f4f4f4f4f4f4f4f4f4f4f4f4f4f4f4f4f4f4f4f4f4f4f4f4f4f4f".to_string();
    node.test_upsert_runtime_peer(peer_pubkey.clone(), "127.0.0.1:9746".to_string(), false);

    let payload_json = serde_json::json!({
        "eventName": "PeerConnected",
        "node_id": peer_pubkey,
    })
    .to_string();
    let payload_hex = hex::encode(payload_json.as_bytes());
    let applied_js = sdk
        .ingest_runtime_transport_event_payload_hex_value(&node, payload_hex)
        .expect("ingest transport");
    let applied: serde_json::Value = crate::js_from(applied_js).expect("parse");
    assert_eq!(applied["event_kind"], "peer_reconnected");
    assert_eq!(applied["applied"], true);
}

#[test]
fn sdk_node_handle_transport_event_name_alias_channel_contract() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    let sdk = crate::RlnWasmSdk::new();
    let node = sdk
        .create_node_handle_with_runtime_backend(
            "ws://127.0.0.1:3363".to_string(),
            "wasm_native_ldk".to_string(),
        )
        .expect("node handle");
    let peer_pubkey =
        "023f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f".to_string();
    node.inner
        .test_upsert_runtime_peer(peer_pubkey.clone(), "127.0.0.1:9747".to_string(), true);
    assert!(node.inner.test_set_runtime_peer_started(&peer_pubkey, true));
    let opened_js = node
        .open_channel_value(peer_pubkey, 5_506, false, None, None)
        .expect("open channel");
    let opened: serde_json::Value = serde_wasm_bindgen::from_value(opened_js).expect("parse");
    let channel_id = opened["channel_id"]
        .as_str()
        .expect("channel id")
        .to_string();

    let payload_json = serde_json::json!({
        "event_name": "channel_unusable",
        "channelId": channel_id,
    })
    .to_string();
    let payload_hex = hex::encode(payload_json.as_bytes());
    let applied_js = node
        .ingest_runtime_transport_event_payload_hex_value(payload_hex)
        .expect("ingest transport");
    let applied: serde_json::Value = crate::js_from(applied_js).expect("parse");
    assert_eq!(applied["event_kind"], "channel_unusable");
    assert_eq!(applied["applied"], true);
}

#[wasm_bindgen_test]
fn sdk_facade_capabilities_contract() {
    let sdk = RlnWasmSdk::new();
    assert_eq!(sdk.healthcheck(), "rln_wasm_sdk_ready");
    let version = sdk.version();
    assert!(!version.trim().is_empty());

    let caps_js = sdk
        .runtime_capabilities_value()
        .expect("runtimeCapabilitiesValue");
    let caps: RlnWasmSdkRuntimeCapabilitiesData =
        serde_wasm_bindgen::from_value(caps_js).expect("caps parse");
    assert!(caps.wallet_runtime);
    assert!(caps.node_runtime);
    assert!(!caps.ldk_runtime_scaffold);
    assert!(caps.callback_status_updates);
}

#[wasm_bindgen_test]
fn sdk_facade_new_wallet_invalid_json_contract() {
    let sdk = RlnWasmSdk::new();
    match sdk.new_wallet("{invalid-json") {
        Ok(_) => panic!("expected constructor error"),
        Err(err) => {
            let msg = err.as_string().expect("error string");
            assert!(msg.contains("Invalid WalletData JSON"));
        }
    }
}

#[wasm_bindgen_test]
fn node_relay_session_auth_contract() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    let node = RlnWasmNode::new_with_runtime_backend(
        "ws://127.0.0.1:3991".to_string(),
        "wasm_native_ldk".to_string(),
    )
    .expect("node");

    let empty_err = node
        .set_relay_session_auth(Some("".to_string()), Some("".to_string()))
        .expect_err("expected empty token/node_id contract");
    assert_eq!(
        empty_err.as_string().unwrap_or_default(),
        sdk_contracts::ERR_RELAY_AUTH_TOKEN_EMPTY
    );

    let partial_err = node
        .set_relay_session_auth(Some("token-a".to_string()), None)
        .expect_err("expected pair requirement");
    assert_eq!(
        partial_err.as_string().unwrap_or_default(),
        sdk_contracts::ERR_RELAY_AUTH_TOKEN_NODE_ID_TOGETHER
    );

    let invalid_node_id_err = node
        .set_relay_session_auth(
            Some("token-a".to_string()),
            Some("not-a-pubkey".to_string()),
        )
        .expect_err("expected pubkey validation");
    assert_eq!(
        invalid_node_id_err.as_string().unwrap_or_default(),
        sdk_contracts::ERR_RELAY_NODE_ID_INVALID
    );

    let valid_node_id =
        "0334cc4bca04ce3d1537310f55e91ec4cec7e5a88fa0fba20a24cce1fe6de2a2b0".to_string();
    node.set_relay_session_auth(Some("token-a".to_string()), Some(valid_node_id.clone()))
        .expect("set relay auth");
    let auth_js = node
        .relay_session_auth_value()
        .expect("relaySessionAuthValue");
    let auth: serde_json::Value = serde_wasm_bindgen::from_value(auth_js).expect("parse auth");
    assert_eq!(auth["relay_auth_token"], "token-a");
    assert_eq!(auth["relay_node_id"], valid_node_id);

    node.set_relay_session_auth(None, None)
        .expect("clear relay auth");
    let cleared_js = node
        .relay_session_auth_value()
        .expect("relaySessionAuthValue");
    assert!(cleared_js.is_null() || cleared_js.is_undefined());
}

#[wasm_bindgen_test]
fn sdk_facade_relay_session_auth_forwarding_contract() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    let sdk = RlnWasmSdk::new();
    let node = sdk
        .new_node_with_runtime_backend(
            "ws://127.0.0.1:3992".to_string(),
            "wasm_native_ldk".to_string(),
        )
        .expect("node");
    let valid_node_id =
        "0334cc4bca04ce3d1537310f55e91ec4cec7e5a88fa0fba20a24cce1fe6de2a2b0".to_string();

    sdk.set_relay_session_auth(
        &node,
        Some("token-b".to_string()),
        Some(valid_node_id.clone()),
    )
    .expect("set relay auth");
    let auth_js = sdk
        .relay_session_auth_value(&node)
        .expect("relaySessionAuthValue");
    let auth: serde_json::Value = serde_wasm_bindgen::from_value(auth_js).expect("parse auth");
    assert_eq!(auth["relay_auth_token"], "token-b");
    assert_eq!(auth["relay_node_id"], valid_node_id);
}

#[wasm_bindgen_test]
fn sdk_node_handle_relay_session_auth_forwarding_contract() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    let sdk = RlnWasmSdk::new();
    let node = sdk
        .create_node_handle_with_runtime_backend(
            "ws://127.0.0.1:3993".to_string(),
            "wasm_native_ldk".to_string(),
        )
        .expect("node handle");
    let valid_node_id =
        "02c11d7dfdd1ca9301508397ec8cc08758aadd95361af8562946146c33be606b58".to_string();

    node.set_relay_session_auth(Some("token-c".to_string()), Some(valid_node_id.clone()))
        .expect("set relay auth");
    let auth_js = node
        .relay_session_auth_value()
        .expect("relaySessionAuthValue");
    let auth: serde_json::Value = serde_wasm_bindgen::from_value(auth_js).expect("parse auth");
    assert_eq!(auth["relay_auth_token"], "token-c");
    assert_eq!(auth["relay_node_id"], valid_node_id);
}

#[cfg(feature = "wasm-browser-infra")]
#[wasm_bindgen_test]
fn sdk_facade_transport_json_alias_channel_id_fallback_contract() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    crate::test_utils::set_wasm_sdk_identity_unlocked_for_tests();
    let sdk = RlnWasmSdk::new();
    let node = sdk
        .new_node_with_runtime_backend_and_runtime_id(
            "ws://127.0.0.1:3355".to_string(),
            "wasm_native_ldk".to_string(),
            "node-rt-test".to_string(),
        )
        .expect("node");
    let peer_pubkey =
        "0334cc4bca04ce3d1537310f55e91ec4cec7e5a88fa0fba20a24cce1fe6de2a2b0".to_string();
    let _ = node.list_peers_value().expect("warm runtime");
    node.test_upsert_runtime_peer(peer_pubkey.clone(), "127.0.0.1:9739".to_string(), true);
    assert!(node.test_set_runtime_peer_started(&peer_pubkey, true));
    let opened_js = sdk
        .open_channel_value(&node, peer_pubkey, 5_506, false, None, None)
        .expect("open channel");
    let opened: serde_json::Value = serde_wasm_bindgen::from_value(opened_js).expect("parse");
    let channel_id = opened["channel_id"]
        .as_str()
        .expect("channel id")
        .to_string();

    let payload_json = serde_json::json!({
        "event": "ChannelUnusable",
        "id": channel_id,
    })
    .to_string();
    let payload_hex = hex::encode(payload_json.as_bytes());
    let applied_js = sdk
        .ingest_runtime_transport_event_payload_hex_value(&node, payload_hex)
        .expect("ingest transport");
    let applied: TestTransportEventApplyData =
        serde_wasm_bindgen::from_value(applied_js).expect("parse transport apply");
    assert_eq!(applied.event_kind, "channel_unusable");
    assert!(applied.applied);

    let channels_js = sdk.list_channels_value(&node).expect("list channels");
    let channels: serde_json::Value =
        serde_wasm_bindgen::from_value(channels_js).expect("parse channels");
    let channels = channels.as_array().expect("channels array");
    assert_eq!(channels.len(), 1);
    assert_eq!(channels[0]["status"], "pending");
    assert_eq!(channels[0]["is_usable"], false);
}

#[cfg(feature = "wasm-browser-infra")]
#[wasm_bindgen_test]
fn sdk_node_handle_transport_json_alias_channel_id_fallback_contract() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    crate::test_utils::set_wasm_sdk_identity_unlocked_for_tests();
    let sdk = RlnWasmSdk::new();
    let node = sdk
        .create_node_handle_with_runtime_backend_and_runtime_id(
            "ws://127.0.0.1:3356".to_string(),
            "wasm_native_ldk".to_string(),
            "node-rt-test".to_string(),
        )
        .expect("node handle");
    let peer_pubkey =
        "0334cc4bca04ce3d1537310f55e91ec4cec7e5a88fa0fba20a24cce1fe6de2a2b0".to_string();
    let _ = node.list_peers_value().expect("warm runtime");
    node.inner
        .test_upsert_runtime_peer(peer_pubkey.clone(), "127.0.0.1:9740".to_string(), true);
    assert!(node.inner.test_set_runtime_peer_started(&peer_pubkey, true));
    let opened_js = node
        .open_channel_value(peer_pubkey, 5_506, false, None, None)
        .expect("open channel");
    let opened: serde_json::Value = serde_wasm_bindgen::from_value(opened_js).expect("parse");
    let channel_id = opened["channel_id"]
        .as_str()
        .expect("channel id")
        .to_string();

    let payload_json = serde_json::json!({
        "event": "channel_unusable",
        "id": channel_id,
    })
    .to_string();
    let payload_hex = hex::encode(payload_json.as_bytes());
    let applied_js = node
        .ingest_runtime_transport_event_payload_hex_value(payload_hex)
        .expect("ingest transport");
    let applied: TestTransportEventApplyData =
        serde_wasm_bindgen::from_value(applied_js).expect("parse transport apply");
    assert_eq!(applied.event_kind, "channel_unusable");
    assert!(applied.applied);

    let channels_js = node.list_channels_value().expect("list channels");
    let channels: serde_json::Value =
        serde_wasm_bindgen::from_value(channels_js).expect("parse channels");
    let channels = channels.as_array().expect("channels array");
    assert_eq!(channels.len(), 1);
    assert_eq!(channels[0]["status"], "pending");
    assert_eq!(channels[0]["is_usable"], false);
}

#[wasm_bindgen_test]
fn list_channels_merges_runtime_metadata_from_local_cache_contract() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    let node = RlnWasmNode::new_with_runtime_backend(
        "ws://127.0.0.1:3365".to_string(),
        "wasm_native_ldk".to_string(),
    )
    .expect("node");
    let peer_pubkey =
        "0334cc4bca04ce3d1537310f55e91ec4cec7e5a88fa0fba20a24cce1fe6de2a2b0".to_string();
    let channel = RlnWasmNodeChannelData {
        temporary_channel_id: "tmp-local-rich".to_string(),
        channel_id: "chan-local-rich".to_string(),
        peer_pubkey: peer_pubkey.clone(),
        status: "opening".to_string(),
        ready: false,
        is_usable: false,
        public: false,
        capacity_sat: 5_506,
        asset_id: None,
        asset_local_amount: None,
        virtual_open_mode: None,
    };
    node.channels.borrow_mut().insert(
        channel.channel_id.clone(),
        ChannelEntry {
            temporary_channel_id: channel.temporary_channel_id.clone(),
            data: channel,
        },
    );
    node.ldk_runtime.upsert_channel(LdkRuntimeChannelStateData {
        temporary_channel_id: "tmp-local-rich".to_string(),
        channel_id: "chan-local-rich".to_string(),
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

    let channels_js = node.list_channels_value().expect("list channels");
    let channels: serde_json::Value =
        serde_wasm_bindgen::from_value(channels_js).expect("parse channels");
    let channels = channels.as_array().expect("channels array");
    assert_eq!(channels.len(), 1);
    assert_eq!(channels[0]["channel_id"], "chan-local-rich");
    assert_eq!(channels[0]["peer_pubkey"], peer_pubkey);
    assert_eq!(channels[0]["capacity_sat"], 5_506);
    assert_eq!(channels[0]["status"], "pending");
    assert_eq!(channels[0]["ready"], true);
}

#[cfg(feature = "wasm-browser-infra")]
#[wasm_bindgen_test]
fn sdk_facade_transport_json_type_alias_channel_id_fallback_contract() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    crate::test_utils::set_wasm_sdk_identity_unlocked_for_tests();
    let sdk = RlnWasmSdk::new();
    let node = sdk
        .new_node_with_runtime_backend_and_runtime_id(
            "ws://127.0.0.1:3359".to_string(),
            "wasm_native_ldk".to_string(),
            "node-rt-test".to_string(),
        )
        .expect("node");
    let peer_pubkey =
        "0334cc4bca04ce3d1537310f55e91ec4cec7e5a88fa0fba20a24cce1fe6de2a2b0".to_string();
    let _ = node.list_peers_value().expect("warm runtime");
    node.test_upsert_runtime_peer(peer_pubkey.clone(), "127.0.0.1:9743".to_string(), true);
    assert!(node.test_set_runtime_peer_started(&peer_pubkey, true));
    let opened_js = sdk
        .open_channel_value(&node, peer_pubkey, 5_506, false, None, None)
        .expect("open channel");
    let opened: serde_json::Value = serde_wasm_bindgen::from_value(opened_js).expect("parse");
    let channel_id = opened["channel_id"]
        .as_str()
        .expect("channel id")
        .to_string();

    let payload_json = serde_json::json!({
        "type": "ChannelUnusable",
        "id": channel_id,
    })
    .to_string();
    let payload_hex = hex::encode(payload_json.as_bytes());
    let applied_js = sdk
        .ingest_runtime_transport_event_payload_hex_value(&node, payload_hex)
        .expect("ingest transport");
    let applied: TestTransportEventApplyData =
        serde_wasm_bindgen::from_value(applied_js).expect("parse transport apply");
    assert_eq!(applied.event_kind, "channel_unusable");
    assert!(applied.applied);

    let channels_js = sdk.list_channels_value(&node).expect("list channels");
    let channels: serde_json::Value =
        serde_wasm_bindgen::from_value(channels_js).expect("parse channels");
    let channels = channels.as_array().expect("channels array");
    assert_eq!(channels.len(), 1);
    assert_eq!(channels[0]["status"], "pending");
    assert_eq!(channels[0]["is_usable"], false);
}

#[wasm_bindgen_test]
fn sdk_facade_transport_json_kind_alias_reconnect_contract() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    let sdk = RlnWasmSdk::new();
    let node = sdk
        .new_node_with_runtime_backend(
            "ws://127.0.0.1:3357".to_string(),
            "wasm_native_ldk".to_string(),
        )
        .expect("node");
    let peer_pubkey =
        "0334cc4bca04ce3d1537310f55e91ec4cec7e5a88fa0fba20a24cce1fe6de2a2b0".to_string();
    let _ = node.list_peers_value().expect("warm runtime");
    node.test_upsert_runtime_peer(peer_pubkey.clone(), "127.0.0.1:9741".to_string(), false);

    let payload_json = serde_json::json!({
        "kind": "PeerReconnected",
        "peer_pubkey": peer_pubkey,
    })
    .to_string();
    let payload_hex = hex::encode(payload_json.as_bytes());
    let applied_js = sdk
        .ingest_runtime_transport_event_payload_hex_value(&node, payload_hex)
        .expect("ingest transport");
    let applied: TestTransportEventApplyData =
        serde_wasm_bindgen::from_value(applied_js).expect("parse transport apply");
    assert_eq!(applied.event_kind, "peer_reconnected");
    assert!(applied.applied);

    let peers_js = sdk.list_peers_value(&node).expect("list peers");
    let peers: serde_json::Value = serde_wasm_bindgen::from_value(peers_js).expect("parse peers");
    let peers = peers.as_array().expect("peers array");
    assert_eq!(peers.len(), 1);
    assert!(peers[0]["started"].is_boolean());
}

#[wasm_bindgen_test]
fn sdk_node_handle_transport_json_type_alias_reconnect_contract() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    let sdk = RlnWasmSdk::new();
    let node = sdk
        .create_node_handle_with_runtime_backend(
            "ws://127.0.0.1:3358".to_string(),
            "wasm_native_ldk".to_string(),
        )
        .expect("node handle");
    let _ = node.list_peers_value().expect("warm runtime");
    let peer_pubkey =
        "0334cc4bca04ce3d1537310f55e91ec4cec7e5a88fa0fba20a24cce1fe6de2a2b0".to_string();
    let _ = node.list_peers_value().expect("warm runtime");
    node.inner
        .test_upsert_runtime_peer(peer_pubkey.clone(), "127.0.0.1:9742".to_string(), false);

    let payload_json = serde_json::json!({
        "type": "PeerReconnected",
        "peer_pubkey": peer_pubkey,
    })
    .to_string();
    let payload_hex = hex::encode(payload_json.as_bytes());
    let applied_js = node
        .ingest_runtime_transport_event_payload_hex_value(payload_hex)
        .expect("ingest transport");
    let applied: TestTransportEventApplyData =
        serde_wasm_bindgen::from_value(applied_js).expect("parse transport apply");
    assert_eq!(applied.event_kind, "peer_reconnected");
    assert!(applied.applied);

    let peers_js = node.list_peers_value().expect("list peers");
    let peers: serde_json::Value = serde_wasm_bindgen::from_value(peers_js).expect("parse peers");
    let peers = peers.as_array().expect("peers array");
    assert_eq!(peers.len(), 1);
    assert!(peers[0]["started"].is_boolean());
}

#[cfg(feature = "wasm-browser-infra")]
#[wasm_bindgen_test]
fn sdk_node_handle_transport_json_type_alias_channel_id_fallback_contract() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    crate::test_utils::set_wasm_sdk_identity_unlocked_for_tests();
    let sdk = RlnWasmSdk::new();
    let node = sdk
        .create_node_handle_with_runtime_backend_and_runtime_id(
            "ws://127.0.0.1:3360".to_string(),
            "wasm_native_ldk".to_string(),
            "node-rt-test".to_string(),
        )
        .expect("node handle");
    let _ = node.list_peers_value().expect("warm runtime");
    let peer_pubkey =
        "0334cc4bca04ce3d1537310f55e91ec4cec7e5a88fa0fba20a24cce1fe6de2a2b0".to_string();
    let _ = node.list_peers_value().expect("warm runtime");
    node.inner
        .test_upsert_runtime_peer(peer_pubkey.clone(), "127.0.0.1:9744".to_string(), true);
    assert!(node.inner.test_set_runtime_peer_started(&peer_pubkey, true));
    let opened_js = node
        .open_channel_value(peer_pubkey, 5_506, false, None, None)
        .expect("open channel");
    let opened: serde_json::Value = serde_wasm_bindgen::from_value(opened_js).expect("parse");
    let channel_id = opened["channel_id"]
        .as_str()
        .expect("channel id")
        .to_string();

    let payload_json = serde_json::json!({
        "type": "channel_unusable",
        "id": channel_id,
    })
    .to_string();
    let payload_hex = hex::encode(payload_json.as_bytes());
    let applied_js = node
        .ingest_runtime_transport_event_payload_hex_value(payload_hex)
        .expect("ingest transport");
    let applied: TestTransportEventApplyData =
        serde_wasm_bindgen::from_value(applied_js).expect("parse transport apply");
    assert_eq!(applied.event_kind, "channel_unusable");
    assert!(applied.applied);

    let channels_js = node.list_channels_value().expect("list channels");
    let channels: serde_json::Value =
        serde_wasm_bindgen::from_value(channels_js).expect("parse channels");
    let channels = channels.as_array().expect("channels array");
    assert_eq!(channels.len(), 1);
    assert_eq!(channels[0]["status"], "pending");
    assert_eq!(channels[0]["is_usable"], false);
}

#[wasm_bindgen_test]
fn sdk_facade_transport_peer_connected_alias_reconnect_contract() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    let sdk = RlnWasmSdk::new();
    let node = sdk
        .new_node_with_runtime_backend(
            "ws://127.0.0.1:3361".to_string(),
            "wasm_native_ldk".to_string(),
        )
        .expect("node");
    let peer_pubkey =
        "0334cc4bca04ce3d1537310f55e91ec4cec7e5a88fa0fba20a24cce1fe6de2a2b0".to_string();
    let _ = node.list_peers_value().expect("warm runtime");
    node.test_upsert_runtime_peer(peer_pubkey.clone(), "127.0.0.1:9745".to_string(), false);

    let payload_json = serde_json::json!({
        "event": "PeerConnected",
        "id": peer_pubkey,
    })
    .to_string();
    let payload_hex = hex::encode(payload_json.as_bytes());
    let applied_js = sdk
        .ingest_runtime_transport_event_payload_hex_value(&node, payload_hex)
        .expect("ingest transport");
    let applied: TestTransportEventApplyData =
        serde_wasm_bindgen::from_value(applied_js).expect("parse transport apply");
    assert_eq!(applied.event_kind, "peer_reconnected");
    assert!(applied.applied);

    let peers_js = sdk.list_peers_value(&node).expect("list peers");
    let peers: serde_json::Value = serde_wasm_bindgen::from_value(peers_js).expect("parse peers");
    let peers = peers.as_array().expect("peers array");
    assert_eq!(peers.len(), 1);
    assert!(peers[0]["started"].is_boolean());
}

#[cfg(feature = "wasm-browser-infra")]
#[wasm_bindgen_test]
fn node_ingest_runtime_transport_event_channel_opened_alias_contract() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    crate::test_utils::set_wasm_sdk_identity_unlocked_for_tests();
    let node = RlnWasmNode::new_with_runtime_backend_and_id(
        "ws://127.0.0.1:3362".to_string(),
        "wasm_native_ldk".to_string(),
        Some("node-rt-test".to_string()),
    )
    .expect("node");
    let peer_pubkey =
        "0334cc4bca04ce3d1537310f55e91ec4cec7e5a88fa0fba20a24cce1fe6de2a2b0".to_string();
    let _ = node.list_peers_value().expect("warm runtime");
    node.test_upsert_runtime_peer(peer_pubkey.clone(), "127.0.0.1:9746".to_string(), true);
    assert!(node.test_set_runtime_peer_started(&peer_pubkey, true));
    let opened_js = node
        .open_channel_value(peer_pubkey, 5_506, false, None, None)
        .expect("open channel");
    let opened: serde_json::Value = serde_wasm_bindgen::from_value(opened_js).expect("parse");
    let channel_id = opened["channel_id"]
        .as_str()
        .expect("channel id")
        .to_string();

    let unusable_payload = hex::encode(
        serde_json::json!({
            "event": "channel_unusable",
            "id": channel_id,
        })
        .to_string()
        .as_bytes(),
    );
    node.ingest_runtime_transport_event_payload_hex_value(unusable_payload)
        .expect("set unusable");

    let alias_json = serde_json::json!({
        "event": "ChannelOpened",
        "id": channel_id,
    })
    .to_string();
    let alias_payload = hex::encode(alias_json.as_bytes());
    let applied_js = node
        .ingest_runtime_transport_event_payload_hex_value(alias_payload)
        .expect("apply alias");
    let applied: TestTransportEventApplyData =
        serde_wasm_bindgen::from_value(applied_js).expect("parse transport apply");
    assert_eq!(applied.event_kind, "channel_usable");
    assert!(applied.applied);

    let channels_js = node.list_channels_value().expect("list channels");
    let channels: serde_json::Value =
        serde_wasm_bindgen::from_value(channels_js).expect("parse channels");
    let channels = channels.as_array().expect("channels array");
    assert_eq!(channels.len(), 1);
    assert_eq!(channels[0]["status"], "opened");
    assert_eq!(channels[0]["is_usable"], true);
}

#[cfg(feature = "wasm-browser-infra")]
#[wasm_bindgen_test]
fn sdk_node_handle_transport_channel_ready_alias_contract() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    crate::test_utils::set_wasm_sdk_identity_unlocked_for_tests();
    let sdk = RlnWasmSdk::new();
    let node = sdk
        .create_node_handle_with_runtime_backend_and_runtime_id(
            "ws://127.0.0.1:3363".to_string(),
            "wasm_native_ldk".to_string(),
            "node-rt-test".to_string(),
        )
        .expect("node handle");
    let peer_pubkey =
        "0334cc4bca04ce3d1537310f55e91ec4cec7e5a88fa0fba20a24cce1fe6de2a2b0".to_string();
    let _ = node.list_peers_value().expect("warm runtime");
    node.inner
        .test_upsert_runtime_peer(peer_pubkey.clone(), "127.0.0.1:9747".to_string(), true);
    assert!(node.inner.test_set_runtime_peer_started(&peer_pubkey, true));
    let opened_js = node
        .open_channel_value(peer_pubkey, 5_506, false, None, None)
        .expect("open channel");
    let opened: serde_json::Value = serde_wasm_bindgen::from_value(opened_js).expect("parse");
    let channel_id = opened["channel_id"]
        .as_str()
        .expect("channel id")
        .to_string();

    let unusable_payload = hex::encode(
        serde_json::json!({
            "event": "channel_unusable",
            "id": channel_id,
        })
        .to_string()
        .as_bytes(),
    );
    node.ingest_runtime_transport_event_payload_hex_value(unusable_payload)
        .expect("set unusable");

    let alias_payload = hex::encode(
        serde_json::json!({
            "event": "channel_usable",
            "id": channel_id,
        })
        .to_string()
        .as_bytes(),
    );
    let applied_js = node
        .ingest_runtime_transport_event_payload_hex_value(alias_payload)
        .expect("apply alias");
    let applied: TestTransportEventApplyData =
        serde_wasm_bindgen::from_value(applied_js).expect("parse transport apply");
    assert_eq!(applied.event_kind, "channel_usable");
    assert!(applied.applied);

    let channels_js = node.list_channels_value().expect("list channels");
    let channels: serde_json::Value =
        serde_wasm_bindgen::from_value(channels_js).expect("parse channels");
    let channels = channels.as_array().expect("channels array");
    assert_eq!(channels.len(), 1);
    assert_eq!(channels[0]["status"], "opened");
    assert_eq!(channels[0]["is_usable"], true);
}

#[cfg(feature = "wasm-browser-infra")]
#[wasm_bindgen_test]
fn sdk_facade_transport_channel_disconnected_alias_contract() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    crate::test_utils::set_wasm_sdk_identity_unlocked_for_tests();
    let sdk = RlnWasmSdk::new();
    let node = sdk
        .new_node_with_runtime_backend_and_runtime_id(
            "ws://127.0.0.1:3364".to_string(),
            "wasm_native_ldk".to_string(),
            "node-rt-test".to_string(),
        )
        .expect("node");
    let peer_pubkey =
        "0334cc4bca04ce3d1537310f55e91ec4cec7e5a88fa0fba20a24cce1fe6de2a2b0".to_string();
    let _ = node.list_peers_value().expect("warm runtime");
    node.test_upsert_runtime_peer(peer_pubkey.clone(), "127.0.0.1:9748".to_string(), true);
    assert!(node.test_set_runtime_peer_started(&peer_pubkey, true));
    let opened_js = sdk
        .open_channel_value(&node, peer_pubkey, 5_506, false, None, None)
        .expect("open channel");
    let opened: serde_json::Value = serde_wasm_bindgen::from_value(opened_js).expect("parse");
    let channel_id = opened["channel_id"]
        .as_str()
        .expect("channel id")
        .to_string();

    let alias_payload = hex::encode(
        serde_json::json!({
            "event": "channel_unusable",
            "id": channel_id,
        })
        .to_string()
        .as_bytes(),
    );
    let applied_js = sdk
        .ingest_runtime_transport_event_payload_hex_value(&node, alias_payload)
        .expect("apply alias");
    let applied: TestTransportEventApplyData =
        serde_wasm_bindgen::from_value(applied_js).expect("parse transport apply");
    assert_eq!(applied.event_kind, "channel_unusable");
    assert!(applied.applied);

    let channels_js = sdk.list_channels_value(&node).expect("list channels");
    let channels: serde_json::Value =
        serde_wasm_bindgen::from_value(channels_js).expect("parse channels");
    let channels = channels.as_array().expect("channels array");
    assert_eq!(channels.len(), 1);
    assert_eq!(channels[0]["status"], "pending");
    assert_eq!(channels[0]["is_usable"], false);
}

#[cfg(feature = "wasm-browser-infra")]
#[wasm_bindgen_test]
fn sdk_node_handle_transport_channel_disconnected_alias_contract() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    crate::test_utils::set_wasm_sdk_identity_unlocked_for_tests();
    let sdk = RlnWasmSdk::new();
    let node = sdk
        .create_node_handle_with_runtime_backend_and_runtime_id(
            "ws://127.0.0.1:3365".to_string(),
            "wasm_native_ldk".to_string(),
            "node-rt-test".to_string(),
        )
        .expect("node handle");
    let peer_pubkey =
        "0334cc4bca04ce3d1537310f55e91ec4cec7e5a88fa0fba20a24cce1fe6de2a2b0".to_string();
    let _ = node.list_peers_value().expect("warm runtime");
    node.inner
        .test_upsert_runtime_peer(peer_pubkey.clone(), "127.0.0.1:9749".to_string(), true);
    assert!(node.inner.test_set_runtime_peer_started(&peer_pubkey, true));
    let opened_js = node
        .open_channel_value(peer_pubkey, 5_506, false, None, None)
        .expect("open channel");
    let opened: serde_json::Value = serde_wasm_bindgen::from_value(opened_js).expect("parse");
    let channel_id = opened["channel_id"]
        .as_str()
        .expect("channel id")
        .to_string();

    let alias_payload = hex::encode(
        serde_json::json!({
            "event": "channel_unusable",
            "id": channel_id,
        })
        .to_string()
        .as_bytes(),
    );
    let applied_js = node
        .ingest_runtime_transport_event_payload_hex_value(alias_payload)
        .expect("apply alias");
    let applied: TestTransportEventApplyData =
        serde_wasm_bindgen::from_value(applied_js).expect("parse transport apply");
    assert_eq!(applied.event_kind, "channel_unusable");
    assert!(applied.applied);

    let channels_js = node.list_channels_value().expect("list channels");
    let channels: serde_json::Value =
        serde_wasm_bindgen::from_value(channels_js).expect("parse channels");
    let channels = channels.as_array().expect("channels array");
    assert_eq!(channels.len(), 1);
    assert_eq!(channels[0]["status"], "pending");
    assert_eq!(channels[0]["is_usable"], false);
}

#[wasm_bindgen_test]
fn sdk_facade_transport_peer_online_alias_reconnect_contract() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    let sdk = RlnWasmSdk::new();
    let node = sdk
        .new_node_with_runtime_backend(
            "ws://127.0.0.1:3366".to_string(),
            "wasm_native_ldk".to_string(),
        )
        .expect("node");
    let peer_pubkey =
        "0334cc4bca04ce3d1537310f55e91ec4cec7e5a88fa0fba20a24cce1fe6de2a2b0".to_string();
    let _ = node.list_peers_value().expect("warm runtime");
    node.test_upsert_runtime_peer(peer_pubkey.clone(), "127.0.0.1:9750".to_string(), false);

    let payload_json = serde_json::json!({
        "kind": "PeerOnline",
        "id": peer_pubkey,
    })
    .to_string();
    let payload_hex = hex::encode(payload_json.as_bytes());
    let applied_js = sdk
        .ingest_runtime_transport_event_payload_hex_value(&node, payload_hex)
        .expect("ingest transport");
    let applied: TestTransportEventApplyData =
        serde_wasm_bindgen::from_value(applied_js).expect("parse transport apply");
    assert_eq!(applied.event_kind, "peer_reconnected");
    assert!(applied.applied);

    let peers_js = sdk.list_peers_value(&node).expect("list peers");
    let peers: serde_json::Value = serde_wasm_bindgen::from_value(peers_js).expect("parse peers");
    let peers = peers.as_array().expect("peers array");
    assert_eq!(peers.len(), 1);
    assert!(peers[0]["started"].is_boolean());
}

#[cfg(feature = "wasm-browser-infra")]
#[wasm_bindgen_test]
fn sdk_node_handle_transport_channel_online_alias_contract() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    crate::test_utils::set_wasm_sdk_identity_unlocked_for_tests();
    let sdk = RlnWasmSdk::new();
    let node = sdk
        .create_node_handle_with_runtime_backend_and_runtime_id(
            "ws://127.0.0.1:3367".to_string(),
            "wasm_native_ldk".to_string(),
            "node-rt-test".to_string(),
        )
        .expect("node handle");
    let peer_pubkey =
        "0334cc4bca04ce3d1537310f55e91ec4cec7e5a88fa0fba20a24cce1fe6de2a2b0".to_string();
    let _ = node.list_peers_value().expect("warm runtime");
    node.inner
        .test_upsert_runtime_peer(peer_pubkey.clone(), "127.0.0.1:9751".to_string(), true);
    assert!(node.inner.test_set_runtime_peer_started(&peer_pubkey, true));
    let opened_js = node
        .open_channel_value(peer_pubkey, 5_506, false, None, None)
        .expect("open channel");
    let opened: serde_json::Value = serde_wasm_bindgen::from_value(opened_js).expect("parse");
    let channel_id = opened["channel_id"]
        .as_str()
        .expect("channel id")
        .to_string();

    let to_pending_payload = hex::encode(
        serde_json::json!({
            "event": "channel_unusable",
            "id": channel_id,
        })
        .to_string()
        .as_bytes(),
    );
    node.ingest_runtime_transport_event_payload_hex_value(to_pending_payload)
        .expect("set pending");

    let alias_payload = hex::encode(
        serde_json::json!({
            "event": "channel_usable",
            "id": channel_id,
        })
        .to_string()
        .as_bytes(),
    );
    let applied_js = node
        .ingest_runtime_transport_event_payload_hex_value(alias_payload)
        .expect("apply alias");
    let applied: TestTransportEventApplyData =
        serde_wasm_bindgen::from_value(applied_js).expect("parse transport apply");
    assert_eq!(applied.event_kind, "channel_usable");
    assert!(applied.applied);

    let channels_js = node.list_channels_value().expect("list channels");
    let channels: serde_json::Value =
        serde_wasm_bindgen::from_value(channels_js).expect("parse channels");
    let channels = channels.as_array().expect("channels array");
    assert_eq!(channels.len(), 1);
    assert_eq!(channels[0]["status"], "opened");
    assert_eq!(channels[0]["is_usable"], true);
}

#[wasm_bindgen_test]
fn sdk_facade_transport_peer_offline_alias_disconnect_contract() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    let sdk = RlnWasmSdk::new();
    let node = sdk
        .new_node_with_runtime_backend(
            "ws://127.0.0.1:3368".to_string(),
            "wasm_native_ldk".to_string(),
        )
        .expect("node");
    let peer_pubkey =
        "0334cc4bca04ce3d1537310f55e91ec4cec7e5a88fa0fba20a24cce1fe6de2a2b0".to_string();
    let _ = node.list_peers_value().expect("warm runtime");
    node.test_upsert_runtime_peer(peer_pubkey.clone(), "127.0.0.1:9752".to_string(), true);
    assert!(node.test_set_runtime_peer_started(&peer_pubkey, true));

    let payload_json = serde_json::json!({
        "event": "PeerOffline",
        "id": peer_pubkey,
    })
    .to_string();
    let payload_hex = hex::encode(payload_json.as_bytes());
    let applied_js = sdk
        .ingest_runtime_transport_event_payload_hex_value(&node, payload_hex)
        .expect("ingest transport");
    let applied: TestTransportEventApplyData =
        serde_wasm_bindgen::from_value(applied_js).expect("parse transport apply");
    assert_eq!(applied.event_kind, "peer_disconnected");
    assert!(applied.applied);

    let peers_js = sdk.list_peers_value(&node).expect("list peers");
    let peers: serde_json::Value = serde_wasm_bindgen::from_value(peers_js).expect("parse peers");
    let peers = peers.as_array().expect("peers array");
    assert!(peers.is_empty());
}

#[cfg(feature = "wasm-browser-infra")]
#[wasm_bindgen_test]
fn sdk_node_handle_transport_channel_offline_alias_contract() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    crate::test_utils::set_wasm_sdk_identity_unlocked_for_tests();
    let sdk = RlnWasmSdk::new();
    let node = sdk
        .create_node_handle_with_runtime_backend_and_runtime_id(
            "ws://127.0.0.1:3369".to_string(),
            "wasm_native_ldk".to_string(),
            "node-rt-test".to_string(),
        )
        .expect("node handle");
    let peer_pubkey =
        "0334cc4bca04ce3d1537310f55e91ec4cec7e5a88fa0fba20a24cce1fe6de2a2b0".to_string();
    let _ = node.list_peers_value().expect("warm runtime");
    node.inner
        .test_upsert_runtime_peer(peer_pubkey.clone(), "127.0.0.1:9753".to_string(), true);
    assert!(node.inner.test_set_runtime_peer_started(&peer_pubkey, true));
    let opened_js = node
        .open_channel_value(peer_pubkey, 5_506, false, None, None)
        .expect("open channel");
    let opened: serde_json::Value = serde_wasm_bindgen::from_value(opened_js).expect("parse");
    let channel_id = opened["channel_id"]
        .as_str()
        .expect("channel id")
        .to_string();

    let alias_payload = hex::encode(
        serde_json::json!({
            "event": "channel_unusable",
            "id": channel_id,
        })
        .to_string()
        .as_bytes(),
    );
    let applied_js = node
        .ingest_runtime_transport_event_payload_hex_value(alias_payload)
        .expect("apply alias");
    let applied: TestTransportEventApplyData =
        serde_wasm_bindgen::from_value(applied_js).expect("parse transport apply");
    assert_eq!(applied.event_kind, "channel_unusable");
    assert!(applied.applied);

    let channels_js = node.list_channels_value().expect("list channels");
    let channels: serde_json::Value =
        serde_wasm_bindgen::from_value(channels_js).expect("parse channels");
    let channels = channels.as_array().expect("channels array");
    assert_eq!(channels.len(), 1);
    assert_eq!(channels[0]["status"], "pending");
    assert_eq!(channels[0]["is_usable"], false);
}

#[wasm_bindgen_test]
fn sdk_facade_transport_peer_up_down_alias_contract() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    let sdk = RlnWasmSdk::new();
    let node = sdk
        .new_node_with_runtime_backend(
            "ws://127.0.0.1:3370".to_string(),
            "wasm_native_ldk".to_string(),
        )
        .expect("node");
    let peer_pubkey =
        "0334cc4bca04ce3d1537310f55e91ec4cec7e5a88fa0fba20a24cce1fe6de2a2b0".to_string();
    let _ = node.list_peers_value().expect("warm runtime");
    node.test_upsert_runtime_peer(peer_pubkey.clone(), "127.0.0.1:9754".to_string(), false);

    let up_payload = hex::encode(format!("peer_up:{peer_pubkey}").as_bytes());
    let up_js = sdk
        .ingest_runtime_transport_event_payload_hex_value(&node, up_payload)
        .expect("apply peer_up");
    let up: TestTransportEventApplyData =
        serde_wasm_bindgen::from_value(up_js).expect("parse transport apply");
    assert_eq!(up.event_kind, "peer_reconnected");
    assert!(up.applied);

    let peers_js = sdk.list_peers_value(&node).expect("list peers");
    let peers: serde_json::Value = serde_wasm_bindgen::from_value(peers_js).expect("parse peers");
    let peers = peers.as_array().expect("peers array");
    assert_eq!(peers.len(), 1);
    assert!(peers[0]["started"].is_boolean());

    let down_payload = hex::encode(format!("peer_down:{peer_pubkey}").as_bytes());
    let down_js = sdk
        .ingest_runtime_transport_event_payload_hex_value(&node, down_payload)
        .expect("apply peer_down");
    let down: TestTransportEventApplyData =
        serde_wasm_bindgen::from_value(down_js).expect("parse transport apply");
    assert_eq!(down.event_kind, "peer_disconnected");
    assert!(down.applied);

    let peers_js = sdk.list_peers_value(&node).expect("list peers");
    let peers: serde_json::Value = serde_wasm_bindgen::from_value(peers_js).expect("parse peers");
    let peers = peers.as_array().expect("peers array");
    assert!(peers.is_empty());
}

#[cfg(feature = "wasm-browser-infra")]
#[wasm_bindgen_test]
fn sdk_node_handle_transport_channel_up_down_alias_contract() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    crate::test_utils::set_wasm_sdk_identity_unlocked_for_tests();
    let sdk = RlnWasmSdk::new();
    let node = sdk
        .create_node_handle_with_runtime_backend_and_runtime_id(
            "ws://127.0.0.1:3371".to_string(),
            "wasm_native_ldk".to_string(),
            "node-rt-test".to_string(),
        )
        .expect("node handle");
    let peer_pubkey =
        "0334cc4bca04ce3d1537310f55e91ec4cec7e5a88fa0fba20a24cce1fe6de2a2b0".to_string();
    let _ = node.list_peers_value().expect("warm runtime");
    node.inner
        .test_upsert_runtime_peer(peer_pubkey.clone(), "127.0.0.1:9755".to_string(), true);
    assert!(node.inner.test_set_runtime_peer_started(&peer_pubkey, true));
    let opened_js = node
        .open_channel_value(peer_pubkey, 5_506, false, None, None)
        .expect("open channel");
    let opened: serde_json::Value = serde_wasm_bindgen::from_value(opened_js).expect("parse");
    let channel_id = opened["channel_id"]
        .as_str()
        .expect("channel id")
        .to_string();

    let down_payload = hex::encode(
        serde_json::json!({
            "event": "channel_unusable",
            "id": channel_id,
        })
        .to_string()
        .as_bytes(),
    );
    let down_js = node
        .ingest_runtime_transport_event_payload_hex_value(down_payload)
        .expect("apply channel_down");
    let down: TestTransportEventApplyData =
        serde_wasm_bindgen::from_value(down_js).expect("parse transport apply");
    assert_eq!(down.event_kind, "channel_unusable");
    assert!(down.applied);

    let channels_js = node.list_channels_value().expect("list channels");
    let channels: serde_json::Value =
        serde_wasm_bindgen::from_value(channels_js).expect("parse channels");
    let channels = channels.as_array().expect("channels array");
    assert_eq!(channels.len(), 1);
    assert_eq!(channels[0]["status"], "pending");
    assert_eq!(channels[0]["is_usable"], false);

    let up_payload = hex::encode(
        serde_json::json!({
            "event": "channel_usable",
            "id": channel_id,
        })
        .to_string()
        .as_bytes(),
    );
    let up_js = node
        .ingest_runtime_transport_event_payload_hex_value(up_payload)
        .expect("apply channel_up");
    let up: TestTransportEventApplyData =
        serde_wasm_bindgen::from_value(up_js).expect("parse transport apply");
    assert_eq!(up.event_kind, "channel_usable");
    assert!(up.applied);

    let channels_js = node.list_channels_value().expect("list channels");
    let channels: serde_json::Value =
        serde_wasm_bindgen::from_value(channels_js).expect("parse channels");
    let channels = channels.as_array().expect("channels array");
    assert_eq!(channels.len(), 1);
    assert_eq!(channels[0]["status"], "opened");
    assert_eq!(channels[0]["is_usable"], true);
}

#[wasm_bindgen_test]
fn sdk_facade_transport_peer_hyphen_alias_contract() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    let sdk = RlnWasmSdk::new();
    let node = sdk
        .new_node_with_runtime_backend(
            "ws://127.0.0.1:3372".to_string(),
            "wasm_native_ldk".to_string(),
        )
        .expect("node");
    let peer_pubkey =
        "0334cc4bca04ce3d1537310f55e91ec4cec7e5a88fa0fba20a24cce1fe6de2a2b0".to_string();
    let _ = node.list_peers_value().expect("warm runtime");
    node.test_upsert_runtime_peer(peer_pubkey.clone(), "127.0.0.1:9756".to_string(), false);

    let up_payload = hex::encode(format!("peer-up:{peer_pubkey}").as_bytes());
    let up_js = sdk
        .ingest_runtime_transport_event_payload_hex_value(&node, up_payload)
        .expect("apply peer-up");
    let up: TestTransportEventApplyData =
        serde_wasm_bindgen::from_value(up_js).expect("parse transport apply");
    assert_eq!(up.event_kind, "peer_reconnected");
    assert!(up.applied);

    let peers_js = sdk.list_peers_value(&node).expect("list peers");
    let peers: serde_json::Value = serde_wasm_bindgen::from_value(peers_js).expect("parse peers");
    let peers = peers.as_array().expect("peers array");
    assert_eq!(peers.len(), 1);
    assert!(peers[0]["started"].is_boolean());
}

#[cfg(feature = "wasm-browser-infra")]
#[wasm_bindgen_test]
fn sdk_node_handle_transport_channel_dot_alias_contract() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    crate::test_utils::set_wasm_sdk_identity_unlocked_for_tests();
    let sdk = RlnWasmSdk::new();
    let node = sdk
        .create_node_handle_with_runtime_backend_and_runtime_id(
            "ws://127.0.0.1:3373".to_string(),
            "wasm_native_ldk".to_string(),
            "node-rt-test".to_string(),
        )
        .expect("node handle");
    let peer_pubkey =
        "0334cc4bca04ce3d1537310f55e91ec4cec7e5a88fa0fba20a24cce1fe6de2a2b0".to_string();
    let _ = node.list_peers_value().expect("warm runtime");
    node.inner
        .test_upsert_runtime_peer(peer_pubkey.clone(), "127.0.0.1:9757".to_string(), true);
    assert!(node.inner.test_set_runtime_peer_started(&peer_pubkey, true));
    let opened_js = node
        .open_channel_value(peer_pubkey, 5_506, false, None, None)
        .expect("open channel");
    let opened: serde_json::Value = serde_wasm_bindgen::from_value(opened_js).expect("parse");
    let channel_id = opened["channel_id"]
        .as_str()
        .expect("channel id")
        .to_string();

    let down_payload = hex::encode(
        serde_json::json!({
            "event": "channel_unusable",
            "id": channel_id,
        })
        .to_string()
        .as_bytes(),
    );
    let down_js = node
        .ingest_runtime_transport_event_payload_hex_value(down_payload)
        .expect("apply channel.down");
    let down: TestTransportEventApplyData =
        serde_wasm_bindgen::from_value(down_js).expect("parse transport apply");
    assert_eq!(down.event_kind, "channel_unusable");
    assert!(down.applied);

    let channels_js = node.list_channels_value().expect("list channels");
    let channels: serde_json::Value =
        serde_wasm_bindgen::from_value(channels_js).expect("parse channels");
    let channels = channels.as_array().expect("channels array");
    assert_eq!(channels.len(), 1);
    assert_eq!(channels[0]["status"], "pending");
    assert_eq!(channels[0]["is_usable"], false);
}

#[wasm_bindgen_test]
fn node_invoice_status_empty_error_contract() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    let node = RlnWasmNode::new("ws://127.0.0.1:3001".to_string()).expect("node");
    let err = node
        .invoice_status_value("".to_string())
        .expect_err("should fail");
    let msg = err.as_string().expect("error string");
    assert_eq!(msg, "invoice cannot be empty");
}

#[wasm_bindgen_test]
fn node_ingest_read_event_json_updates_payment_status() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    let node = RlnWasmNode::new("ws://127.0.0.1:3001".to_string()).expect("node");
    let keysend_js = node
        .keysend_value(
            "0334cc4bca04ce3d1537310f55e91ec4cec7e5a88fa0fba20a24cce1fe6de2a2b0".to_string(),
            3_000_000,
            None,
            None,
        )
        .expect("keysend");
    let keysend: TestKeysendData =
        serde_wasm_bindgen::from_value(keysend_js).expect("parse keysend");

    let event_json = serde_json::json!({
        "payment_hash": keysend.payment_hash,
        "status": "succeeded"
    })
    .to_string();
    let payload_hex = hex::encode(event_json.as_bytes());

    let updated_js = node
        .ingest_read_event_payload_hex(payload_hex)
        .expect("ingest event");
    let updated: TestPaymentData =
        serde_wasm_bindgen::from_value(updated_js).expect("parse updated payment");
    assert_eq!(updated.status, "succeeded");

    let events_js = node.list_runtime_events_value().expect("events");
    let events: Vec<TestRuntimeEventData> =
        serde_wasm_bindgen::from_value(events_js).expect("parse events");
    assert!(!events.is_empty());
    let last = events.last().expect("last event");
    assert_eq!(last.source, "manual_api");
    assert_eq!(last.event_kind, "payment_status");
    assert!(last.applied);
    assert!(last.payment_hash.is_some());
    assert_eq!(last.status.as_deref(), Some("succeeded"));
}

#[wasm_bindgen_test]
fn node_ingest_read_event_text_updates_payment_status() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    let node = RlnWasmNode::new("ws://127.0.0.1:3001".to_string()).expect("node");
    let keysend_js = node
        .keysend_value(
            "0334cc4bca04ce3d1537310f55e91ec4cec7e5a88fa0fba20a24cce1fe6de2a2b0".to_string(),
            3_000_000,
            None,
            None,
        )
        .expect("keysend");
    let keysend: TestKeysendData =
        serde_wasm_bindgen::from_value(keysend_js).expect("parse keysend");

    let event_text = format!("payment_status:{}:failed", keysend.payment_hash);
    let payload_hex = hex::encode(event_text.as_bytes());

    let updated_js = node
        .ingest_read_event_payload_hex(payload_hex)
        .expect("ingest event");
    let updated: TestPaymentData =
        serde_wasm_bindgen::from_value(updated_js).expect("parse updated payment");
    assert_eq!(updated.status, "failed");
}

#[wasm_bindgen_test]
fn node_ingest_read_event_invalid_status_contract() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    let node = RlnWasmNode::new("ws://127.0.0.1:3001".to_string()).expect("node");
    let keysend_js = node
        .keysend_value(
            "0334cc4bca04ce3d1537310f55e91ec4cec7e5a88fa0fba20a24cce1fe6de2a2b0".to_string(),
            3_000_000,
            None,
            None,
        )
        .expect("keysend");
    let keysend: TestKeysendData =
        serde_wasm_bindgen::from_value(keysend_js).expect("parse keysend");

    let event_json = serde_json::json!({
        "payment_hash": keysend.payment_hash,
        "status": "unknown_status"
    })
    .to_string();
    let payload_hex = hex::encode(event_json.as_bytes());

    let err = node
        .ingest_read_event_payload_hex(payload_hex)
        .expect_err("should fail");
    let msg = err.as_string().expect("error string");
    assert_eq!(
        msg,
        "status must be one of: pending, claimable, claiming, succeeded, cancelled, failed, expired"
    );

    let events_js = node.list_runtime_events_value().expect("events");
    let events: Vec<TestRuntimeEventData> =
        serde_wasm_bindgen::from_value(events_js).expect("parse events");
    assert!(!events.is_empty());
    let last = events.last().expect("last event");
    assert_eq!(last.source, "node_api");
    assert_eq!(last.event_kind, "payment_status");
    assert!(last.applied);
    assert_eq!(last.status.as_deref(), Some("failed"));
    assert!(last.error.is_none());
}

#[wasm_bindgen_test]
fn node_ingest_runtime_transport_event_unknown_target_contract() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    let node = RlnWasmNode::new("ws://127.0.0.1:3001".to_string()).expect("node");
    let payload_hex = hex::encode("peer_disconnected:0200deadbeef");
    let applied_js = node
        .ingest_runtime_transport_event_payload_hex_value(payload_hex)
        .expect("ingest transport");
    let applied: TestTransportEventApplyData =
        serde_wasm_bindgen::from_value(applied_js).expect("parse transport apply");
    assert_eq!(applied.event_kind, "peer_disconnected");
    assert!(!applied.applied);

    let events_js = node.list_runtime_events_value().expect("events");
    let events: Vec<TestRuntimeEventData> =
        serde_wasm_bindgen::from_value(events_js).expect("parse events");
    assert!(!events.is_empty());
    let last = events.last().expect("last event");
    assert_eq!(last.source, "runtime_transport_api");
    assert_eq!(last.event_kind, "peer_disconnected");
    assert!(!last.applied);
    assert_eq!(
        last.error.as_deref(),
        Some("transport event target not found")
    );
}

#[wasm_bindgen_test]
fn node_ingest_runtime_transport_event_parse_error_contract() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    let node = RlnWasmNode::new("ws://127.0.0.1:3001".to_string()).expect("node");
    let payload_hex = hex::encode("{\"event\":\"unknown\"}");
    let err = node
        .ingest_runtime_transport_event_payload_hex_value(payload_hex)
        .expect_err("should fail");
    let msg = err.as_string().expect("error string");
    assert_eq!(msg, "unrecognized transport event payload format");

    let events_js = node.list_runtime_events_value().expect("events");
    let events: Vec<TestRuntimeEventData> =
        serde_wasm_bindgen::from_value(events_js).expect("parse events");
    assert!(!events.is_empty());
    let last = events.last().expect("last event");
    assert_eq!(last.source, "runtime_transport_api");
    assert_eq!(last.event_kind, "json_payload");
    assert!(!last.applied);
    assert_eq!(
        last.error.as_deref(),
        Some("unrecognized transport event payload format")
    );
}

#[wasm_bindgen_test]
fn node_ingest_runtime_transport_event_json_alias_contract() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    let node = RlnWasmNode::new_with_runtime_backend(
        "ws://127.0.0.1:3001".to_string(),
        "wasm_native_ldk".to_string(),
    )
    .expect("node");
    let peer_pubkey =
        "0334cc4bca04ce3d1537310f55e91ec4cec7e5a88fa0fba20a24cce1fe6de2a2b0".to_string();
    let _ = node.list_peers_value().expect("warm runtime");
    node.test_upsert_runtime_peer(peer_pubkey.clone(), "127.0.0.1:9735".to_string(), false);

    let payload_json = serde_json::json!({
        "event": "PeerReconnected",
        "peer_pubkey": peer_pubkey,
    })
    .to_string();
    let payload_hex = hex::encode(payload_json.as_bytes());
    let applied_js = node
        .ingest_runtime_transport_event_payload_hex_value(payload_hex)
        .expect("ingest transport");
    let applied: TestTransportEventApplyData =
        serde_wasm_bindgen::from_value(applied_js).expect("parse transport apply");
    assert_eq!(applied.event_kind, "peer_reconnected");
    assert!(applied.applied);

    let peers_js = node.list_peers_value().expect("list peers");
    let peers: serde_json::Value = serde_wasm_bindgen::from_value(peers_js).expect("parse peers");
    let peers = peers.as_array().expect("peers array");
    assert_eq!(peers.len(), 1);
    assert!(peers[0]["started"].is_boolean());
}

#[cfg(feature = "wasm-browser-infra")]
#[wasm_bindgen_test]
fn node_ingest_runtime_transport_event_json_alias_channel_id_fallback_contract() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    crate::test_utils::set_wasm_sdk_identity_unlocked_for_tests();
    let node = RlnWasmNode::new_with_runtime_backend_and_id(
        "ws://127.0.0.1:3354".to_string(),
        "wasm_native_ldk".to_string(),
        Some("node-rt-test".to_string()),
    )
    .expect("node");
    let peer_pubkey =
        "0334cc4bca04ce3d1537310f55e91ec4cec7e5a88fa0fba20a24cce1fe6de2a2b0".to_string();
    let _ = node.list_peers_value().expect("warm runtime");
    node.test_upsert_runtime_peer(peer_pubkey.clone(), "127.0.0.1:9738".to_string(), true);
    assert!(node.test_set_runtime_peer_started(&peer_pubkey, true));
    let opened_js = node
        .open_channel_value(peer_pubkey, 5_506, false, None, None)
        .expect("open channel");
    let opened: serde_json::Value = serde_wasm_bindgen::from_value(opened_js).expect("parse");
    let channel_id = opened["channel_id"]
        .as_str()
        .expect("channel id")
        .to_string();

    let payload_json = serde_json::json!({
        "event": "ChannelUnusable",
        "id": channel_id,
    })
    .to_string();
    let payload_hex = hex::encode(payload_json.as_bytes());
    let applied_js = node
        .ingest_runtime_transport_event_payload_hex_value(payload_hex)
        .expect("ingest transport");
    let applied: TestTransportEventApplyData =
        serde_wasm_bindgen::from_value(applied_js).expect("parse transport apply");
    assert_eq!(applied.event_kind, "channel_unusable");
    assert!(applied.applied);

    let channels_js = node.list_channels_value().expect("list channels");
    let channels: serde_json::Value =
        serde_wasm_bindgen::from_value(channels_js).expect("parse channels");
    let channels = channels.as_array().expect("channels array");
    assert_eq!(channels.len(), 1);
    assert_eq!(channels[0]["status"], "pending");
    assert_eq!(channels[0]["is_usable"], false);
}

#[wasm_bindgen_test]
fn node_update_payment_status_terminal_transition_contract() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    let node = RlnWasmNode::new("ws://127.0.0.1:3001".to_string()).expect("node");
    let keysend_js = node
        .keysend_value(
            "0334cc4bca04ce3d1537310f55e91ec4cec7e5a88fa0fba20a24cce1fe6de2a2b0".to_string(),
            3_000_000,
            None,
            None,
        )
        .expect("keysend");
    let keysend: TestKeysendData =
        serde_wasm_bindgen::from_value(keysend_js).expect("parse keysend");
    node.update_payment_status(keysend.payment_hash.clone(), "succeeded".to_string())
        .expect("set succeeded");

    let err = node
        .update_payment_status(keysend.payment_hash, "failed".to_string())
        .expect_err("should fail");
    let msg = err.as_string().expect("error string");
    assert_eq!(
        msg,
        "invalid payment status transition: succeeded -> failed"
    );
}

#[wasm_bindgen_test]
fn node_payment_status_event_updates_swap_runtime_status_contract() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    let node = RlnWasmNode::new("ws://127.0.0.1:3001".to_string()).expect("node");
    let keysend_js = node
        .keysend_value(
            "0334cc4bca04ce3d1537310f55e91ec4cec7e5a88fa0fba20a24cce1fe6de2a2b0".to_string(),
            3_000_000,
            None,
            None,
        )
        .expect("keysend");
    let keysend: TestKeysendData =
        serde_wasm_bindgen::from_value(keysend_js).expect("parse keysend");
    crate::swap_runtime::test_utils::test_insert_swap_with_payment_hash(
        &keysend.payment_hash,
        false,
    );

    let before_swap_js =
        crate::swap_runtime::get_swap_value(keysend.payment_hash.clone()).expect("get swap");
    let before_swap: serde_json::Value =
        serde_wasm_bindgen::from_value(before_swap_js).expect("parse swap");
    assert_eq!(before_swap["swap"]["status"], "Waiting");

    node.update_payment_status(keysend.payment_hash.clone(), "succeeded".to_string())
        .expect("set succeeded");

    let after_swap_js =
        crate::swap_runtime::get_swap_value(keysend.payment_hash).expect("get swap");
    let after_swap: serde_json::Value =
        serde_wasm_bindgen::from_value(after_swap_js).expect("parse swap");
    assert_eq!(after_swap["swap"]["status"], "Succeeded");
}

#[wasm_bindgen_test]
fn node_decode_rgb_invoice_empty_error_contract() {
    let node = RlnWasmNode::new("ws://127.0.0.1:3001".to_string()).expect("node");
    let err = node
        .decode_rgb_invoice_value("".to_string())
        .expect_err("should fail");
    let msg = err.as_string().expect("error string");
    assert_eq!(msg, "invoice cannot be empty");
}

#[wasm_bindgen_test]
fn node_decode_ln_invoice_empty_error_contract() {
    let node = RlnWasmNode::new("ws://127.0.0.1:3001".to_string()).expect("node");
    let err = node
        .decode_ln_invoice_value("".to_string())
        .expect_err("should fail");
    let msg = err.as_string().expect("error string");
    assert_eq!(msg, "invoice cannot be empty");
}

#[wasm_bindgen_test]
fn node_decode_rgb_invoice_json_empty_error_contract() {
    let node = RlnWasmNode::new("ws://127.0.0.1:3001".to_string()).expect("node");
    let err = node
        .decode_rgb_invoice_json("".to_string())
        .expect_err("should fail");
    let msg = err.as_string().expect("error string");
    assert_eq!(msg, "invoice cannot be empty");
}

#[wasm_bindgen_test]
fn rgb_ln_transfers_register_on_rgb_keysend_contract() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    let node = RlnWasmNode::new("ws://127.0.0.1:3001".to_string()).expect("node");
    let keysend_js = node
        .keysend_value(
            "0334cc4bca04ce3d1537310f55e91ec4cec7e5a88fa0fba20a24cce1fe6de2a2b0".to_string(),
            3_000_000,
            Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string()),
            Some(7),
        )
        .expect("keysend");
    let keysend: serde_json::Value = serde_wasm_bindgen::from_value(keysend_js).expect("parse");
    let payment_hash = keysend["payment_hash"]
        .as_str()
        .expect("payment hash")
        .to_string();

    let transfers_js = node
        .list_rgb_ln_transfers_value()
        .expect("list rgb ln transfers");
    let transfers: serde_json::Value =
        serde_wasm_bindgen::from_value(transfers_js).expect("parse transfers");
    let transfers = transfers.as_array().expect("array");
    assert_eq!(transfers.len(), 1);
    assert_eq!(transfers[0]["payment_hash"], payment_hash);
    assert_eq!(transfers[0]["asset_amount"], 7);
}

#[wasm_bindgen_test]
fn rgb_ln_transfers_follow_payment_status_updates_contract() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    let node = RlnWasmNode::new("ws://127.0.0.1:3001".to_string()).expect("node");
    let keysend_js = node
        .keysend_value(
            "0334cc4bca04ce3d1537310f55e91ec4cec7e5a88fa0fba20a24cce1fe6de2a2b0".to_string(),
            3_000_000,
            Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string()),
            Some(11),
        )
        .expect("keysend");
    let keysend: serde_json::Value = serde_wasm_bindgen::from_value(keysend_js).expect("parse");
    let payment_hash = keysend["payment_hash"]
        .as_str()
        .expect("payment hash")
        .to_string();

    node.update_payment_status(payment_hash.clone(), "succeeded".to_string())
        .expect("update status");
    let transfers_js = node
        .list_rgb_ln_transfers_value()
        .expect("list rgb ln transfers");
    let transfers: serde_json::Value =
        serde_wasm_bindgen::from_value(transfers_js).expect("parse transfers");
    let transfers = transfers.as_array().expect("array");
    let matching = transfers
        .iter()
        .find(|entry| entry["payment_hash"] == payment_hash)
        .expect("matching transfer");
    assert_eq!(matching["status"], "succeeded");
}

#[wasm_bindgen_test]
fn rgb_ln_transfers_persist_across_node_recreation_contract() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    let proxy = "ws://127.0.0.1:3999".to_string();
    let node_a = RlnWasmNode::new(proxy.clone()).expect("node a");
    let keysend_js = node_a
        .keysend_value(
            "0334cc4bca04ce3d1537310f55e91ec4cec7e5a88fa0fba20a24cce1fe6de2a2b0".to_string(),
            3_000_000,
            Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string()),
            Some(21),
        )
        .expect("rgb keysend");
    let keysend: serde_json::Value = serde_wasm_bindgen::from_value(keysend_js).expect("parse");
    let payment_hash = keysend["payment_hash"]
        .as_str()
        .expect("payment hash")
        .to_string();
    node_a
        .update_payment_status(payment_hash.clone(), "succeeded".to_string())
        .expect("set succeeded");

    let node_b = RlnWasmNode::new(proxy).expect("node b");
    let transfers_js = node_b
        .list_rgb_ln_transfers_value()
        .expect("list persisted transfers");
    let transfers: serde_json::Value =
        serde_wasm_bindgen::from_value(transfers_js).expect("parse transfers");
    let transfers = transfers.as_array().expect("array");
    assert_eq!(transfers.len(), 1);
    assert_eq!(transfers[0]["payment_hash"], payment_hash);
    assert_eq!(transfers[0]["status"], "succeeded");
}
