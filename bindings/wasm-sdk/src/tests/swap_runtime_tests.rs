use super::*;
use futures::executor::block_on;

fn make_sdk_with_identity(password: &str, mnemonic: &str) -> crate::RlnWasmSdk {
    let sdk = crate::RlnWasmSdk::new();
    block_on(sdk.init_json(password.to_string(), Some(mnemonic.to_string()))).expect("init");
    block_on(sdk.unlock(format!("{{\"password\":\"{password}\"}}"))).expect("unlock");
    sdk
}

#[test]
fn swap_state_storage_key_is_identity_scoped() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    let mnemonic_a = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
    let _sdk_a = make_sdk_with_identity("scope-a", mnemonic_a);
    let key_a = swap_state_storage_key();

    crate::test_utils::reset_wasm_runtime_state_for_tests();
    let mnemonic_b = "legal winner thank year wave sausage worth useful legal winner thank yellow";
    let _sdk_b = make_sdk_with_identity("scope-b", mnemonic_b);
    let key_b = swap_state_storage_key();

    assert_ne!(key_a, key_b);
    assert!(key_a.starts_with(SWAP_STATE_STORAGE_KEY_PREFIX));
    assert!(key_b.starts_with(SWAP_STATE_STORAGE_KEY_PREFIX));
}

#[test]
fn sdk_maker_init_runtime_contract() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    let sdk = crate::RlnWasmSdk::new();
    block_on(sdk.init_json("phase-swap".to_string(), None)).expect("init");
    block_on(sdk.unlock("{\"password\":\"phase-swap\"}".to_string())).expect("unlock");
    let asset_id = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    let response = block_on(sdk.maker_init_json(format!(
        "{{\"qty_from\":1500,\"qty_to\":25,\"from_asset\":null,\"to_asset\":\"{asset_id}\",\"timeout_sec\":300}}"
    )))
    .expect("maker_init");
    let parsed: serde_json::Value = serde_json::from_str(&response).expect("valid json");
    assert!(parsed["payment_hash"].as_str().unwrap_or_default().len() == 64);
    assert!(parsed["payment_secret"].as_str().unwrap_or_default().len() == 64);
    assert!(parsed["swapstring"]
        .as_str()
        .unwrap_or_default()
        .starts_with("rln-wasm-swap:"));
}

#[test]
fn sdk_swap_runtime_roundtrip_contract() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    let sdk = crate::RlnWasmSdk::new();
    block_on(sdk.init_json("phase-swap-roundtrip".to_string(), None)).expect("init");
    block_on(sdk.unlock("{\"password\":\"phase-swap-roundtrip\"}".to_string())).expect("unlock");
    let asset_id = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    let maker_init_raw = block_on(sdk.maker_init_json(format!(
        "{{\"qty_from\":3000,\"qty_to\":40,\"from_asset\":null,\"to_asset\":\"{asset_id}\",\"timeout_sec\":300}}"
    )))
    .expect("maker_init");
    let maker_init: serde_json::Value =
        serde_json::from_str(&maker_init_raw).expect("valid maker_init json");
    let swapstring = maker_init["swapstring"]
        .as_str()
        .expect("swapstring")
        .to_string();
    let payment_hash = maker_init["payment_hash"]
        .as_str()
        .expect("payment_hash")
        .to_string();
    let payment_secret = maker_init["payment_secret"]
        .as_str()
        .expect("payment_secret")
        .to_string();

    let before_taker_raw = block_on(sdk.list_swaps_json()).expect("list_swaps");
    let before_taker: serde_json::Value =
        serde_json::from_str(&before_taker_raw).expect("valid list_swaps json");
    assert_eq!(before_taker["maker"].as_array().map_or(0, |a| a.len()), 1);
    assert_eq!(before_taker["taker"].as_array().map_or(0, |a| a.len()), 0);

    let taker_request = serde_json::json!({ "swapstring": swapstring });
    block_on(sdk.taker(taker_request.to_string())).expect("taker");
    let after_taker_raw = block_on(sdk.list_swaps_json()).expect("list_swaps");
    let after_taker: serde_json::Value =
        serde_json::from_str(&after_taker_raw).expect("valid list_swaps json");
    assert_eq!(after_taker["maker"].as_array().map_or(0, |a| a.len()), 1);
    assert_eq!(after_taker["taker"].as_array().map_or(0, |a| a.len()), 1);
    assert_eq!(after_taker["maker"][0]["status"], "Waiting");
    assert_eq!(after_taker["taker"][0]["status"], "Waiting");

    let execute_request = serde_json::json!({
        "swapstring": swapstring.clone(),
        "payment_secret": payment_secret,
        "taker_pubkey": "0334cc4bca04ce3d1537310f55e91ec4cec7e5a88fa0fba20a24cce1fe6de2a2b0",
    });
    let execute_raw =
        block_on(sdk.maker_execute_json(execute_request.to_string())).expect("maker_execute");
    let execute_swap: serde_json::Value =
        serde_json::from_str(&execute_raw).expect("valid maker_execute json");
    assert_eq!(execute_swap["status"], "Pending");

    let get_swap_raw = block_on(sdk.get_swap_json(payment_hash)).expect("get_swap");
    let get_swap: serde_json::Value =
        serde_json::from_str(&get_swap_raw).expect("valid get_swap json");
    assert_eq!(get_swap["swap"]["status"], "Pending");
}

#[test]
fn sdk_swap_runtime_validation_contracts() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    let sdk = crate::RlnWasmSdk::new();
    block_on(sdk.init_json("phase-swap-validation".to_string(), None)).expect("init");
    block_on(sdk.unlock("{\"password\":\"phase-swap-validation\"}".to_string())).expect("unlock");

    let err = block_on(sdk.maker_init_json(
        "{\"qty_from\":100,\"qty_to\":50,\"from_asset\":null,\"to_asset\":null,\"timeout_sec\":120}".to_string(),
    ))
    .expect_err("btc-btc swap should fail");
    assert_eq!(
        err.as_string().unwrap_or_default(),
        "cannot swap BTC for BTC"
    );

    let err = block_on(sdk.maker_init_json(
        "{\"qty_from\":100,\"qty_to\":50,\"from_asset\":\"deadbeef\",\"to_asset\":null,\"timeout_sec\":120}".to_string(),
    ))
    .expect_err("invalid asset should fail");
    assert_eq!(err.as_string().unwrap_or_default(), "invalid asset_id");
}

#[test]
fn sdk_swap_runtime_get_swap_request_shape_contract() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    let sdk = crate::RlnWasmSdk::new();
    block_on(sdk.init_json("phase-swap-get-shape".to_string(), None)).expect("init");
    block_on(sdk.unlock("{\"password\":\"phase-swap-get-shape\"}".to_string())).expect("unlock");
    let asset_id = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";

    let maker_init_raw = block_on(sdk.maker_init_json(format!(
        "{{\"qty_from\":700,\"qty_to\":20,\"from_asset\":null,\"to_asset\":\"{asset_id}\",\"timeout_sec\":300}}"
    )))
    .expect("maker_init");
    let maker_init: serde_json::Value =
        serde_json::from_str(&maker_init_raw).expect("valid maker_init json");
    let payment_hash = maker_init["payment_hash"]
        .as_str()
        .expect("payment_hash")
        .to_string();

    let request = serde_json::json!({
        "payment_hash": payment_hash,
        "taker": false
    });
    let get_swap_raw = block_on(sdk.get_swap_json(request.to_string())).expect("get_swap");
    let get_swap: serde_json::Value =
        serde_json::from_str(&get_swap_raw).expect("valid get_swap json");
    assert!(get_swap.get("swap").is_some());
    assert_eq!(get_swap["swap"]["status"], "Waiting");
}

#[test]
fn sdk_swap_runtime_maker_execute_validation_contract() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    let sdk = crate::RlnWasmSdk::new();
    block_on(sdk.init_json("phase-swap-maker-exec-validation".to_string(), None)).expect("init");
    block_on(sdk.unlock("{\"password\":\"phase-swap-maker-exec-validation\"}".to_string()))
        .expect("unlock");

    let err = block_on(sdk.maker_execute_json(
        "{\"swapstring\":\"rln-wasm-swap:{}\",\"payment_secret\":\"00\",\"taker_pubkey\":\"bad\"}".to_string(),
    ))
    .expect_err("invalid payment_secret should fail");
    assert_eq!(
        err.as_string().unwrap_or_default(),
        "invalid payment_secret"
    );
}
