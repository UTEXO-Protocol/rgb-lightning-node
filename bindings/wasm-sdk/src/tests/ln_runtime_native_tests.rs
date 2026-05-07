use super::test_utils::{inject_for_test, reset_native_runtime_core_state_for_tests};
use super::*;
use crate::wasm_node_persistence::WASM_LN_RUNTIME_CORE_STORAGE_PREFIX;

#[test]
fn core_lifecycle_and_queue_are_deterministic() {
    reset_native_runtime_core_state_for_tests();
    let runtime_key = "test-runtime-native-core".to_string();
    let core = NativeLnRuntimeCore::new(runtime_key.clone());
    assert_eq!(core.status().lifecycle_state, "cold");

    core.ensure_started();
    assert!(core.status().ready);

    let e0 = core.enqueue_event("payment_status".to_string(), "aa".to_string());
    let e1 = core.enqueue_event("transport".to_string(), "bb".to_string());
    assert_eq!(e0.seq, 0);
    assert_eq!(e1.seq, 1);

    let drained = core.drain_events();
    assert_eq!(drained.len(), 2);
    assert_eq!(drained[0].seq, 0);
    assert_eq!(drained[1].seq, 1);

    core.stop();
    assert!(
        core.status().lifecycle_state == "stopped"
            || core.status().lifecycle_state == "restored_stopped"
    );

    let restored = NativeLnRuntimeCore::new(runtime_key);
    let status = restored.status();
    assert!(status.schema_version >= 1);
    assert_eq!(status.queued_events, 0);
}

#[test]
fn recovery_prefers_newest_pending_snapshot() {
    reset_native_runtime_core_state_for_tests();
    let base = format!("{WASM_LN_RUNTIME_CORE_STORAGE_PREFIX}recovery-prefers-pending");
    let committed = NativeLnRuntimeCoreSnapshot {
        revision: 3,
        lifecycle_state: NativeLnRuntimeLifecycleState::Stopped,
        storage_initialized: true,
        schema_version: 1,
        queued_events: Vec::new(),
        next_event_seq: 1,
    };
    let pending = NativeLnRuntimeCoreSnapshot {
        revision: 4,
        lifecycle_state: NativeLnRuntimeLifecycleState::RunningRestored,
        storage_initialized: true,
        schema_version: 1,
        queued_events: vec![NativeLnRuntimeQueuedEventData {
            seq: 1,
            event_kind: "payment_status".to_string(),
            payload_hex: "aa".to_string(),
            received_at: 1,
        }],
        next_event_seq: 2,
    };
    inject_for_test(&base, Some(pending), Some(committed));

    let core = NativeLnRuntimeCore::new("recovery-prefers-pending".to_string());
    let status = core.status();
    assert!(status.ready);
    assert_eq!(status.queued_events, 1);
}

#[test]
fn recovery_falls_back_to_committed_when_newer() {
    reset_native_runtime_core_state_for_tests();
    let base = format!("{WASM_LN_RUNTIME_CORE_STORAGE_PREFIX}recovery-prefers-committed");
    let committed = NativeLnRuntimeCoreSnapshot {
        revision: 7,
        lifecycle_state: NativeLnRuntimeLifecycleState::Running,
        storage_initialized: true,
        schema_version: 1,
        queued_events: vec![NativeLnRuntimeQueuedEventData {
            seq: 5,
            event_kind: "transport".to_string(),
            payload_hex: "bb".to_string(),
            received_at: 1,
        }],
        next_event_seq: 6,
    };
    let pending = NativeLnRuntimeCoreSnapshot {
        revision: 6,
        lifecycle_state: NativeLnRuntimeLifecycleState::Stopped,
        storage_initialized: true,
        schema_version: 1,
        queued_events: Vec::new(),
        next_event_seq: 6,
    };
    inject_for_test(&base, Some(pending), Some(committed));

    let core = NativeLnRuntimeCore::new("recovery-prefers-committed".to_string());
    let status = core.status();
    assert!(status.ready);
    assert_eq!(status.queued_events, 1);
}
