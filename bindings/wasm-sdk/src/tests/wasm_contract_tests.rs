use crate::*;
use wasm_bindgen_test::*;

wasm_bindgen_test_configure!(run_in_browser);

#[wasm_bindgen_test]
fn keys_json_value_parity() {
    let value = rgb_generate_keys_value("regtest".to_string()).expect("value");
    let json = rgb_generate_keys_json("regtest".to_string()).expect("json");

    let value_data: RlnRgbKeysData = serde_wasm_bindgen::from_value(value).expect("value parse");
    let json_data: RlnRgbKeysData = serde_json::from_str(&json).expect("json parse");

    assert!(value_data.xpub.starts_with("tpub"));
    assert!(json_data.xpub.starts_with("tpub"));
    assert!(value_data.account_xpub_vanilla.starts_with("tpub"));
    assert!(json_data.account_xpub_vanilla.starts_with("tpub"));
    assert!(value_data.account_xpub_colored.starts_with("tpub"));
    assert!(json_data.account_xpub_colored.starts_with("tpub"));
    assert_eq!(value_data.master_fingerprint.len(), 8);
    assert_eq!(json_data.master_fingerprint.len(), 8);
    assert!(!value_data.mnemonic.is_empty());
    assert!(!json_data.mnemonic.is_empty());
}

#[wasm_bindgen_test]
fn invalid_network_error_contract() {
    let err = rgb_generate_keys_json("badnet".to_string()).expect_err("should fail");
    let msg = err.as_string().expect("error string");
    assert!(msg.contains("unsupported network"));
}

#[wasm_bindgen_test]
fn wallet_constructor_invalid_json_error_contract() {
    match RlnWasmWallet::new("{invalid-json") {
        Ok(_) => panic!("expected constructor error"),
        Err(err) => {
            let msg = err.as_string().expect("error string");
            assert!(msg.contains("Invalid WalletData JSON"));
        }
    }
}

#[wasm_bindgen_test(async)]
async fn proxy_empty_error_contract() {
    let err = check_proxy_url("".to_string())
        .await
        .expect_err("should fail");
    let msg = err.as_string().expect("error string");
    assert_eq!(msg, sdk_contracts::ERR_PROXY_URL_EMPTY);
}

#[wasm_bindgen_test(async)]
async fn ln_peer_websocket_empty_proxy_contract() {
    let err = check_ln_peer_websocket_value("".to_string(), "127.0.0.1:9735".to_string())
        .await
        .expect_err("should fail");
    let msg = err.as_string().expect("error string");
    assert_eq!(msg, sdk_contracts::ERR_PROXY_URL_EMPTY);
}

#[wasm_bindgen_test(async)]
async fn ln_peer_websocket_bad_peer_addr_contract() {
    let err = check_ln_peer_websocket_value("ws://127.0.0.1:3001".to_string(), "bad".to_string())
        .await
        .expect_err("should fail");
    let msg = err.as_string().expect("error string");
    assert_eq!(msg, "peer_addr must be in host:port format");
}

#[wasm_bindgen_test]
fn proxy_url_mapping_contract() {
    let ws_url = proxy_url_for_peer("ws://127.0.0.1:3001", "3.33.236.230:9735").expect("url");
    assert_eq!(ws_url, "ws://127.0.0.1:3001/v1/3_33_236_230/9735");
}

#[wasm_bindgen_test(async)]
async fn peer_session_empty_pubkey_contract() {
    let noop = js_sys::Function::new_no_args("return '';");
    let res = peer_session_connect(
        "ws://127.0.0.1:3001".to_string(),
        "127.0.0.1:9735".to_string(),
        "".to_string(),
        noop.clone(),
        noop.clone(),
        noop.clone(),
        noop.clone(),
    )
    .await;
    match res {
        Ok(_) => panic!("should fail"),
        Err(err) => {
            let msg = err.as_string().expect("error string");
            assert_eq!(msg, "peer_pubkey cannot be empty");
        }
    }
}

#[wasm_bindgen_test]
fn rust_peer_manager_invalid_initial_hex_contract() {
    match RlnWasmRustPeerManagerBridge::new(Some("zz".to_string())) {
        Ok(_) => panic!("should fail"),
        Err(err) => {
            let msg = err.as_string().expect("error string");
            assert!(msg.contains("invalid initial_outbound_hex"));
        }
    }
}

#[wasm_bindgen_test]
fn peer_manager_hooks_install_clear_contract() {
    clear_peer_manager_hooks();
    assert!(!has_peer_manager_hooks());

    let new_outbound = js_sys::Function::new_no_args("return '';");
    let read_event = js_sys::Function::new_no_args("return undefined;");
    let process_events = js_sys::Function::new_no_args("return undefined;");
    let disconnected = js_sys::Function::new_no_args("return undefined;");
    let report_error = js_sys::Function::new_no_args("return undefined;");

    install_peer_manager_hooks_from_js(
        new_outbound,
        read_event,
        process_events,
        disconnected,
        report_error,
    );
    assert!(has_peer_manager_hooks());

    clear_peer_manager_hooks();
    assert!(!has_peer_manager_hooks());
}

#[wasm_bindgen_test]
fn invoice_empty_error_contract() {
    match RlnWasmInvoice::new("".to_string()) {
        Ok(_) => panic!("expected invoice parse error"),
        Err(err) => {
            let msg = err.as_string().expect("error string");
            assert_eq!(msg, sdk_contracts::ERR_INVOICE_STRING_EMPTY);
        }
    }
}
