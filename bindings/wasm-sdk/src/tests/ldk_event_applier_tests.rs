use super::*;

#[test]
fn manual_status_update_allowed_when_not_using_runtime_pipeline() {
    assert!(ensure_manual_status_update_allowed(false).is_ok());
}

#[test]
fn manual_event_ingestion_allowed_when_not_using_runtime_pipeline() {
    assert!(ensure_manual_event_ingestion_allowed(false).is_ok());
}

#[test]
fn manual_update_paths_are_enabled_in_test_mode_for_runtime() {
    assert!(ensure_manual_status_update_allowed(true).is_ok());
    assert!(ensure_manual_event_ingestion_allowed(true).is_ok());
}
