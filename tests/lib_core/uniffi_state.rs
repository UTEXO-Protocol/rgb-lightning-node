use serial_test::serial;

use rgb_lightning_node::test_utils::{
    clear_uniffi_state_for_tests, mock_locked_app_state, register_uniffi_state_for_tests,
};
use rgb_lightning_node::{
    sdk_get_channel_id, sdk_get_payment, sdk_get_swap, sdk_ln_invoice, sdk_node_info, sdk_send_rgb,
    uniffi_is_initialized, LnInvoiceRequestV1, RlnError, SendRgbRequestV1,
};

#[test]
#[serial(uniffi_state)]
fn uniffi_entrypoints_require_initialized_state() {
    clear_uniffi_state_for_tests();

    assert!(!uniffi_is_initialized());
    assert!(matches!(sdk_node_info(), Err(RlnError::NotInitialized)));
    assert!(matches!(
        sdk_get_channel_id(lightning::ln::types::ChannelId([0u8; 32])),
        Err(RlnError::NotInitialized)
    ));
    assert!(matches!(
        sdk_get_payment(lightning::types::payment::PaymentHash([0u8; 32])),
        Err(RlnError::NotInitialized)
    ));
    assert!(matches!(
        sdk_get_swap(lightning::types::payment::PaymentHash([0u8; 32]), true),
        Err(RlnError::NotInitialized)
    ));
}

#[test]
#[serial(uniffi_state)]
fn register_and_clear_uniffi_state_transitions() {
    let state = mock_locked_app_state();
    register_uniffi_state_for_tests(&state);

    assert!(uniffi_is_initialized());
    assert!(matches!(sdk_node_info(), Err(RlnError::NotInitialized)));

    clear_uniffi_state_for_tests();
    assert!(!uniffi_is_initialized());
}

#[test]
#[serial(uniffi_state)]
fn locked_state_does_not_bypass_unlock_guards() {
    let state = mock_locked_app_state();
    register_uniffi_state_for_tests(&state);

    let invoice = sdk_ln_invoice(LnInvoiceRequestV1 {
        amt_msat: Some(1000),
        expiry_sec: 3600,
        asset_id: None,
        asset_amount: None,
    });
    assert!(matches!(invoice, Err(RlnError::NotInitialized)));

    let send_rgb = sdk_send_rgb(SendRgbRequestV1 {
        donation: false,
        fee_rate: 1,
        min_confirmations: 1,
        skip_sync: true,
        recipient_groups: vec![],
    });
    assert!(matches!(send_rgb, Err(RlnError::InvalidRequest)));

    clear_uniffi_state_for_tests();
}
