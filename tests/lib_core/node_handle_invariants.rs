use serial_test::serial;

use rgb_lightning_node::test_utils::{
    clear_uniffi_state_for_tests, mock_locked_app_state, node_handle_from_mock_state_for_tests,
};
use rgb_lightning_node::uniffi_is_initialized;

#[test]
#[serial(uniffi_state)]
fn node_handle_register_unregister_controls_uniffi_state() {
    clear_uniffi_state_for_tests();

    let state = mock_locked_app_state();
    let handle = node_handle_from_mock_state_for_tests(&state);

    handle.register_for_uniffi();
    assert!(uniffi_is_initialized());

    handle.unregister_for_uniffi();
    assert!(!uniffi_is_initialized());
}

#[test]
#[serial(uniffi_state)]
fn repeated_register_unregister_is_stable() {
    clear_uniffi_state_for_tests();

    let state = mock_locked_app_state();
    let handle = node_handle_from_mock_state_for_tests(&state);

    handle.register_for_uniffi();
    handle.register_for_uniffi();
    assert!(uniffi_is_initialized());

    handle.unregister_for_uniffi();
    handle.unregister_for_uniffi();
    assert!(!uniffi_is_initialized());
}
