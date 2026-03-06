use rgb_lightning_node::test_utils::error_mapping_snapshot_for_tests;
use rgb_lightning_node::RlnError;

#[test]
fn api_error_mapping_remains_stable() {
    let snapshot = error_mapping_snapshot_for_tests();

    assert!(matches!(snapshot.locked_node, RlnError::NotInitialized));
    assert!(matches!(snapshot.payment_not_found, RlnError::InvalidRequest));
    assert!(matches!(snapshot.io_error, RlnError::Internal));
}
