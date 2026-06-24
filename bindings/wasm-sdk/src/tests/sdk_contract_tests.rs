use crate::test_utils::test_wallet_data_json;
use crate::*;
use wasm_bindgen::JsValue;
use wasm_bindgen_test::*;

wasm_bindgen_test_configure!(run_in_browser);

#[wasm_bindgen_test(async)]
async fn sdk_lifecycle_init_success_contract() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    let sdk = RlnWasmSdk::new();
    let result = sdk
        .init_json("test-password".to_string(), None)
        .await
        .expect("init json");
    let parsed: RlnWasmInitData = serde_json::from_str(&result).expect("parse init json");
    assert!(!parsed.mnemonic.trim().is_empty());
}

#[wasm_bindgen_test(async)]
async fn sdk_lifecycle_unlock_requires_init_contract() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    let sdk = RlnWasmSdk::new();
    let err = sdk
        .unlock("{\"password\":\"x\"}".to_string())
        .await
        .expect_err("should fail");
    let msg = err.as_string().expect("error string");
    assert_eq!(msg, "sdk is not initialized");
}

#[wasm_bindgen_test(async)]
async fn sdk_lifecycle_unlock_lock_success_contract() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    let sdk = RlnWasmSdk::new();
    sdk.init_json("phase-next-password".to_string(), None)
        .await
        .expect("init");
    sdk.unlock("{\"password\":\"phase-next-password\"}".to_string())
        .await
        .expect("unlock");
    sdk.lock().await.expect("lock");
}

#[wasm_bindgen_test(async)]
async fn sdk_lifecycle_locked_blocks_node_handle_creation_contract() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    let sdk = RlnWasmSdk::new();
    sdk.init_json("phase-node-lock".to_string(), None)
        .await
        .expect("init");
    match sdk.create_node_handle("ws://127.0.0.1:3001".to_string()) {
        Ok(_) => panic!("expected locked runtime error"),
        Err(err) => {
            let msg = err.as_string().expect("error string");
            assert_eq!(msg, "sdk node runtime is locked; call unlock first");
        }
    }
}

#[wasm_bindgen_test(async)]
async fn sdk_lifecycle_lock_blocks_existing_node_runtime_calls_contract() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    let sdk = RlnWasmSdk::new();
    sdk.init_json("phase-node-runtime".to_string(), None)
        .await
        .expect("init");
    sdk.unlock("{\"password\":\"phase-node-runtime\"}".to_string())
        .await
        .expect("unlock");
    let node = sdk
        .create_node_handle("ws://127.0.0.1:3001".to_string())
        .expect("node handle");

    node.keysend_value(
        "0334cc4bca04ce3d1537310f55e91ec4cec7e5a88fa0fba20a24cce1fe6de2a2b0".to_string(),
        3_000_000,
        None,
        None,
    )
    .expect("keysend unlocked");

    sdk.lock().await.expect("lock");
    let err = node
        .keysend_value(
            "0334cc4bca04ce3d1537310f55e91ec4cec7e5a88fa0fba20a24cce1fe6de2a2b0".to_string(),
            3_000_000,
            None,
            None,
        )
        .expect_err("should fail");
    let msg = err.as_string().expect("error string");
    assert_eq!(msg, "sdk node runtime is locked; call unlock first");

    sdk.unlock("{\"password\":\"phase-node-runtime\"}".to_string())
        .await
        .expect("unlock again");
    node.keysend_value(
        "0334cc4bca04ce3d1537310f55e91ec4cec7e5a88fa0fba20a24cce1fe6de2a2b0".to_string(),
        3_000_000,
        None,
        None,
    )
    .expect("keysend after unlock");
}

#[wasm_bindgen_test(async)]
async fn sdk_default_virtual_channels_gate_contract() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    let sdk = RlnWasmSdk::new();
    sdk.init_json("phase-virtual-default".to_string(), None)
        .await
        .expect("init");
    sdk.unlock("{\"password\":\"phase-virtual-default\"}".to_string())
        .await
        .expect("unlock");

    let default_flag: serde_json::Value = serde_wasm_bindgen::from_value(
        sdk.default_enable_virtual_channels_v0_value()
            .expect("default flag value"),
    )
    .expect("parse default flag");
    assert_eq!(default_flag["enabled"], false);

    let node_default = sdk
        .create_node_handle("ws://127.0.0.1:3191".to_string())
        .expect("node default");
    let node_default_flag: serde_json::Value = serde_wasm_bindgen::from_value(
        node_default
            .enable_virtual_channels_v0_value()
            .expect("node default flag"),
    )
    .expect("parse node default flag");
    assert_eq!(node_default_flag["enabled"], false);

    sdk.set_default_enable_virtual_channels_v0_api(true);
    let node_enabled = sdk
        .create_node_handle("ws://127.0.0.1:3192".to_string())
        .expect("node enabled");
    let node_enabled_flag: serde_json::Value = serde_wasm_bindgen::from_value(
        node_enabled
            .enable_virtual_channels_v0_value()
            .expect("node enabled flag"),
    )
    .expect("parse node enabled flag");
    assert_eq!(node_enabled_flag["enabled"], true);
}

#[wasm_bindgen_test(async)]
async fn sdk_node_handle_connect_peer_invalid_pubkey_contract() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    let sdk = RlnWasmSdk::new();
    sdk.init_json("phase-connect-peer".to_string(), None)
        .await
        .expect("init");
    sdk.unlock("{\"password\":\"phase-connect-peer\"}".to_string())
        .await
        .expect("unlock");
    let node = sdk
        .create_node_handle_with_runtime_id(
            "ws://127.0.0.1:3001".to_string(),
            "node-rt-test".to_string(),
        )
        .expect("node handle");

    let err = node
        .connect_peer("127.0.0.1:9735".to_string(), "bad-pubkey".to_string())
        .await
        .expect_err("should fail");
    assert_eq!(
        err.as_string().unwrap_or_default(),
        sdk_contracts::ERR_PEER_PUBKEY_INVALID
    );
}

#[wasm_bindgen_test(async)]
async fn sdk_node_handle_connect_peer_invalid_addr_contract() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    let sdk = RlnWasmSdk::new();
    sdk.init_json("phase-connect-peer-addr".to_string(), None)
        .await
        .expect("init");
    sdk.unlock("{\"password\":\"phase-connect-peer-addr\"}".to_string())
        .await
        .expect("unlock");
    let node = sdk
        .create_node_handle_with_runtime_id(
            "ws://127.0.0.1:3001".to_string(),
            "node-rt-test".to_string(),
        )
        .expect("node handle");

    let err = node
        .connect_peer(
            "peer-without-port".to_string(),
            "0334cc4bca04ce3d1537310f55e91ec4cec7e5a88fa0fba20a24cce1fe6de2a2b0".to_string(),
        )
        .await
        .expect_err("should fail");
    assert_eq!(
        err.as_string().unwrap_or_default(),
        "peer_addr must be in host:port format"
    );
}

#[wasm_bindgen_test(async)]
async fn sdk_lifecycle_lock_requires_init_contract() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    let sdk = RlnWasmSdk::new();
    let err = sdk.lock().await.expect_err("should fail");
    let msg = err.as_string().expect("error string");
    assert_eq!(msg, "sdk is not initialized");
}

#[wasm_bindgen_test(async)]
async fn sdk_lifecycle_unlock_invalid_password_contract() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    let sdk = RlnWasmSdk::new();
    sdk.init_json("phase-pass-a".to_string(), None)
        .await
        .expect("init");
    let err = sdk
        .unlock("{\"password\":\"phase-pass-b\"}".to_string())
        .await
        .expect_err("should fail");
    let msg = err.as_string().expect("error string");
    assert_eq!(msg, "invalid password");
}

#[wasm_bindgen_test(async)]
async fn sdk_send_rgb_from_groups_unsupported_contract() {
    let sdk = RlnWasmSdk::new();
    let err = sdk
        .send_rgb_from_groups_json("{}".to_string())
        .await
        .expect_err("should fail");
    let msg = err.as_string().expect("error string");
    assert_eq!(
        msg,
        "send_rgb_from_groups is not supported in wasm runtime: grouped SDK transfer adapter is unavailable"
    );
}

#[wasm_bindgen_test(async)]
async fn sdk_swap_onion_native_parity_error_contracts() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    let sdk = RlnWasmSdk::new();
    sdk.init_json("phase-native-parity".to_string(), None)
        .await
        .expect("init");
    sdk.unlock("{\"password\":\"phase-native-parity\"}".to_string())
        .await
        .expect("unlock");

    struct Case {
        name: &'static str,
        op: &'static str,
        input: String,
        expected_error: &'static str,
    }

    let cases = vec![
        Case {
            name: "swap btc-btc",
            op: "maker_init",
            input: "{\"qty_from\":100,\"qty_to\":50,\"from_asset\":null,\"to_asset\":null,\"timeout_sec\":120}".to_string(),
            expected_error: "cannot swap BTC for BTC",
        },
        Case {
            name: "swap same asset",
            op: "maker_init",
            input: "{\"qty_from\":100,\"qty_to\":50,\"from_asset\":\"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\",\"to_asset\":\"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\",\"timeout_sec\":120}".to_string(),
            expected_error: "cannot swap the same asset",
        },
        Case {
            name: "onion empty path",
            op: "send_onion_message",
            input: "{\"node_ids\":[],\"tlv_type\":64,\"data\":\"aa\"}".to_string(),
            expected_error: "SendOnionMessage requires at least one node id for the path",
        },
        Case {
            name: "onion bad pubkey",
            op: "send_onion_message",
            input: "{\"node_ids\":[\"bad\"],\"tlv_type\":64,\"data\":\"aa\"}".to_string(),
            expected_error: "Couldn't parse peer_pubkey 'bad'",
        },
        Case {
            name: "onion bad tlv",
            op: "send_onion_message",
            input: "{\"node_ids\":[\"0334cc4bca04ce3d1537310f55e91ec4cec7e5a88fa0fba20a24cce1fe6de2a2b0\"],\"tlv_type\":63,\"data\":\"aa\"}".to_string(),
            expected_error: "need an integral message type above 64",
        },
        Case {
            name: "onion bad payload hex",
            op: "send_onion_message",
            input: "{\"node_ids\":[\"0334cc4bca04ce3d1537310f55e91ec4cec7e5a88fa0fba20a24cce1fe6de2a2b0\"],\"tlv_type\":64,\"data\":\"zz\"}".to_string(),
            expected_error: "need a hex data string",
        },
    ];

    for case in cases {
        let err = match case.op {
            "maker_init" => sdk
                .maker_init_json(case.input.clone())
                .await
                .expect_err(case.name),
            "send_onion_message" => sdk
                .send_onion_message(case.input.clone())
                .await
                .expect_err(case.name),
            _ => panic!("unknown case op"),
        };
        assert_eq!(
            err.as_string().unwrap_or_default(),
            case.expected_error,
            "native parity mismatch in case: {}",
            case.name
        );
    }
}

#[wasm_bindgen_test(async)]
async fn sdk_send_onion_message_runtime_contract() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    let sdk = RlnWasmSdk::new();
    sdk.init_json("phase-onion".to_string(), None)
        .await
        .expect("init");
    sdk.unlock("{\"password\":\"phase-onion\"}".to_string())
        .await
        .expect("unlock");

    let request = serde_json::json!({
        "node_ids": ["0334cc4bca04ce3d1537310f55e91ec4cec7e5a88fa0fba20a24cce1fe6de2a2b0"],
        "tlv_type": 64,
        "data": "00ff"
    });
    sdk.send_onion_message(request.to_string())
        .await
        .expect("send onion");
}

#[wasm_bindgen_test(async)]
async fn sdk_send_onion_message_validation_contracts() {
    let sdk = RlnWasmSdk::new();

    let err = sdk
        .send_onion_message("{\"node_ids\":[],\"tlv_type\":64,\"data\":\"aa\"}".to_string())
        .await
        .expect_err("empty path should fail");
    assert_eq!(
        err.as_string().unwrap_or_default(),
        "SendOnionMessage requires at least one node id for the path"
    );

    let err = sdk
        .send_onion_message("{\"node_ids\":[\"bad\"],\"tlv_type\":64,\"data\":\"aa\"}".to_string())
        .await
        .expect_err("bad pubkey should fail");
    assert_eq!(
        err.as_string().unwrap_or_default(),
        "Couldn't parse peer_pubkey 'bad'"
    );

    let err = sdk
        .send_onion_message("{\"node_ids\":[\"0334cc4bca04ce3d1537310f55e91ec4cec7e5a88fa0fba20a24cce1fe6de2a2b0\"],\"tlv_type\":63,\"data\":\"aa\"}".to_string())
        .await
        .expect_err("bad tlv type should fail");
    assert_eq!(
        err.as_string().unwrap_or_default(),
        "need an integral message type above 64"
    );

    let err = sdk
        .send_onion_message("{\"node_ids\":[\"0334cc4bca04ce3d1537310f55e91ec4cec7e5a88fa0fba20a24cce1fe6de2a2b0\"],\"tlv_type\":64,\"data\":\"zz\"}".to_string())
        .await
        .expect_err("bad data should fail");
    assert_eq!(
        err.as_string().unwrap_or_default(),
        "need a hex data string"
    );
}

#[cfg(feature = "wasm-browser-infra")]
#[wasm_bindgen_test(async)]
async fn sdk_issue_asset_nia_uses_unlock_bootstrapped_wallet_contract() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    let sdk = RlnWasmSdk::new();
    sdk.init_json("phase-issue-asset-nia".to_string(), None)
        .await
        .expect("init");
    sdk.unlock("{\"password\":\"phase-issue-asset-nia\"}".to_string())
        .await
        .expect("unlock");
    let node = sdk
        .create_node_handle("ws://127.0.0.1:3001".to_string())
        .expect("node handle");

    let err = node
        .issue_asset_nia_value(JsValue::from_str("not-an-object"))
        .expect_err("should fail");
    let msg = err.as_string().expect("error string");
    assert!(msg.contains("Invalid issue_asset_nia request"));
}

#[wasm_bindgen_test(async)]
async fn sdk_issue_asset_nia_invalid_request_contract() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    let sdk = RlnWasmSdk::new();
    sdk.init_json("phase-issue-asset-nia-invalid".to_string(), None)
        .await
        .expect("init");
    sdk.unlock("{\"password\":\"phase-issue-asset-nia-invalid\"}".to_string())
        .await
        .expect("unlock");
    let node = sdk
        .create_node_handle("ws://127.0.0.1:3001".to_string())
        .expect("node handle");
    let err = node
        .issue_asset_nia_value(JsValue::from_str("not-an-object"))
        .expect_err("should fail");
    let msg = err.as_string().expect("error string");
    assert!(msg.contains("Invalid issue_asset_nia request"));
}

#[wasm_bindgen_test(async)]
async fn sdk_issue_asset_cfa_invalid_request_contract() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    let sdk = RlnWasmSdk::new();
    sdk.init_json("phase-issue-asset-cfa-invalid".to_string(), None)
        .await
        .expect("init");
    sdk.unlock("{\"password\":\"phase-issue-asset-cfa-invalid\"}".to_string())
        .await
        .expect("unlock");
    let node = sdk
        .create_node_handle("ws://127.0.0.1:3001".to_string())
        .expect("node handle");
    let err = node
        .issue_asset_cfa_value(JsValue::from_str("not-an-object"))
        .expect_err("should fail");
    let msg = err.as_string().expect("error string");
    assert!(msg.contains("Invalid issue_asset_cfa request"));
}

#[wasm_bindgen_test(async)]
async fn sdk_issue_asset_cfa_empty_amounts_contract() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    let sdk = RlnWasmSdk::new();
    sdk.init_json("phase-issue-asset-cfa-empty".to_string(), None)
        .await
        .expect("init");
    sdk.unlock("{\"password\":\"phase-issue-asset-cfa-empty\"}".to_string())
        .await
        .expect("unlock");
    let node = sdk
        .create_node_handle("ws://127.0.0.1:3001".to_string())
        .expect("node handle");
    let request = serde_wasm_bindgen::to_value(&WasmIssueAssetCfaRequest {
        amounts: vec![],
        name: "Collectible".to_string(),
        details: Some("demo".to_string()),
        precision: 0,
        file_digest: None,
    })
    .expect("request");
    let err = node
        .issue_asset_cfa_value(request)
        .expect_err("should fail");
    let msg = err.as_string().expect("error string");
    assert_eq!(msg, "amounts cannot be empty");
}

#[cfg(feature = "wasm-browser-infra")]
#[wasm_bindgen_test(async)]
async fn sdk_issue_asset_nia_node_created_before_unlock_uses_default_wallet_contract() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    let sdk = RlnWasmSdk::new();
    sdk.init_json("phase-issue-asset-nia-late-unlock".to_string(), None)
        .await
        .expect("init");

    let node = sdk
        .create_node_handle("ws://127.0.0.1:3001".to_string())
        .expect_err("node should be locked before unlock");
    let msg = node.as_string().expect("error string");
    assert_eq!(msg, "sdk node runtime is locked; call unlock first");

    sdk.unlock("{\"password\":\"phase-issue-asset-nia-late-unlock\"}".to_string())
        .await
        .expect("unlock");

    let node = sdk
        .create_node_handle("ws://127.0.0.1:3001".to_string())
        .expect("node handle after unlock");
    let err = node
        .issue_asset_nia_value(JsValue::from_str("not-an-object"))
        .expect_err("should fail on validation, not missing wallet");
    let msg = err.as_string().expect("error string");
    assert!(msg.contains("Invalid issue_asset_nia request"));
}

#[wasm_bindgen_test]
fn sdk_wallet_handle_issue_asset_uda_invalid_request_contract() {
    let sdk = RlnWasmSdk::new();
    let wallet_json = test_wallet_data_json();
    let wallet = sdk
        .create_wallet_handle(&wallet_json)
        .expect("wallet handle");
    let err = wallet
        .issue_asset_uda_value(JsValue::from_str("not-an-object"))
        .expect_err("should fail");
    let msg = err.as_string().expect("error string");
    assert!(msg.contains("Invalid issue_asset_uda request"));
}

#[wasm_bindgen_test]
fn sdk_wallet_handle_issue_asset_uda_empty_ticker_contract() {
    let sdk = RlnWasmSdk::new();
    let wallet_json = test_wallet_data_json();
    let wallet = sdk
        .create_wallet_handle(&wallet_json)
        .expect("wallet handle");
    let request = serde_wasm_bindgen::to_value(&WasmIssueAssetUdaRequest {
        ticker: "".to_string(),
        name: "Collectible".to_string(),
        details: Some("demo".to_string()),
        precision: 0,
        media_file_digest: None,
        attachments_file_digests: vec![],
    })
    .expect("request");
    let err = wallet
        .issue_asset_uda_value(request)
        .expect_err("should fail");
    let msg = err.as_string().expect("error string");
    assert_eq!(msg, "ticker cannot be empty");
}

#[wasm_bindgen_test(async)]
async fn sdk_wallet_handle_send_rgb_from_groups_invalid_request_contract() {
    let sdk = RlnWasmSdk::new();
    let wallet_json = test_wallet_data_json();
    let wallet = sdk
        .create_wallet_handle(&wallet_json)
        .expect("wallet handle");
    let err = wallet
        .send_rgb_from_groups_value(JsValue::from_str("not-an-object"))
        .await
        .expect_err("should fail");
    let msg = err.as_string().expect("error string");
    assert!(msg.contains("Invalid send_rgb_from_groups request"));
}

#[wasm_bindgen_test(async)]
async fn sdk_wallet_handle_send_rgb_from_groups_empty_groups_contract() {
    let sdk = RlnWasmSdk::new();
    let wallet_json = test_wallet_data_json();
    let wallet = sdk
        .create_wallet_handle(&wallet_json)
        .expect("wallet handle");
    let request = serde_wasm_bindgen::to_value(&WasmSendRgbFromGroupsRequest {
        online: rgb_lib_wasm::wallet::Online {
            id: 1,
            indexer_url: "https://indexer.example.com".to_string(),
        },
        donation: false,
        fee_rate: 1,
        min_confirmations: 1,
        recipient_groups: vec![],
    })
    .expect("request");
    let err = wallet
        .send_rgb_from_groups_json(request)
        .await
        .expect_err("should fail");
    let msg = err.as_string().expect("error string");
    assert_eq!(msg, "recipient_groups cannot be empty");
}

#[wasm_bindgen_test(async)]
async fn sdk_wallet_facade_send_rgb_from_groups_empty_groups_contract() {
    let sdk = RlnWasmSdk::new();
    let wallet_json = test_wallet_data_json();
    let wallet = sdk.new_wallet(&wallet_json).expect("wallet");
    let request = serde_wasm_bindgen::to_value(&WasmSendRgbFromGroupsRequest {
        online: rgb_lib_wasm::wallet::Online {
            id: 1,
            indexer_url: "https://indexer.example.com".to_string(),
        },
        donation: false,
        fee_rate: 1,
        min_confirmations: 1,
        recipient_groups: vec![],
    })
    .expect("request");
    let err = sdk
        .wallet_send_rgb_from_groups_json(&wallet, request)
        .await
        .expect_err("should fail");
    let msg = err.as_string().expect("error string");
    assert_eq!(msg, "recipient_groups cannot be empty");
}

#[wasm_bindgen_test]
fn send_rgb_from_groups_recipient_map_shape_contract() {
    let recipient = rgb_lib_wasm::wallet::Recipient {
        recipient_id: "rgb1qqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqq".to_string(),
        witness_data: None,
        assignment: rgb_lib_wasm::Assignment::Fungible(1),
        transport_endpoints: vec!["rpc://127.0.0.1:3000/json-rpc".to_string()],
    };
    let groups = vec![WasmSendRgbAssetRecipientsInput {
        asset_id: "rgb:asset:demo".to_string(),
        recipients: vec![recipient],
    }];

    let recipient_map = recipient_map_from_groups(groups).expect("recipient map");
    assert_eq!(recipient_map.len(), 1);
    let recipients = recipient_map
        .get("rgb:asset:demo")
        .expect("asset recipients");
    assert_eq!(recipients.len(), 1);
}

#[wasm_bindgen_test(async)]
async fn sdk_issue_asset_uda_unsupported_contract() {
    let sdk = RlnWasmSdk::new();
    let err = sdk
        .issue_asset_uda_json("{}".to_string())
        .await
        .expect_err("should fail");
    let msg = err.as_string().expect("error string");
    assert_eq!(
        msg,
        "issue_asset_uda is not supported in wasm runtime: RLN issuance adapter is unavailable"
    );
}

#[wasm_bindgen_test]
fn sdk_wallet_facade_issue_asset_uda_unsupported_contract() {
    let sdk = RlnWasmSdk::new();
    let wallet_json = test_wallet_data_json();
    let wallet = sdk.new_wallet(&wallet_json).expect("wallet");
    let request = serde_wasm_bindgen::to_value(&WasmIssueAssetUdaRequest {
        ticker: "TOK".to_string(),
        name: "Collectible".to_string(),
        details: Some("demo".to_string()),
        precision: 0,
        media_file_digest: None,
        attachments_file_digests: vec![],
    })
    .expect("request");
    let err = sdk
        .wallet_issue_asset_uda_json(&wallet, request)
        .expect_err("should fail");
    let msg = err.as_string().expect("error string");
    assert_eq!(
        msg,
        "issue_asset_uda is not supported in wasm runtime: rgb-lib-wasm does not expose a UDA issuance primitive"
    );
}

#[wasm_bindgen_test(async)]
async fn sdk_post_asset_media_wasm_runtime_contract() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    let sdk = RlnWasmSdk::new();
    let posted = sdk
        .post_asset_media_json("image/png".to_string(), "00ff".to_string())
        .await
        .expect("should succeed");
    let doc: serde_json::Value = serde_json::from_str(&posted).expect("parse posted");
    let digest = doc["digest"].as_str().expect("digest string");
    assert_eq!(digest.len(), 64);
}

#[wasm_bindgen_test]
fn runtime_layer_lock_authority_contract() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    crate::ldk_runtime::set_runtime_session_initialized(true);
    crate::ldk_runtime::set_runtime_session_authorized(false);

    let node = RlnWasmNode::new("ws://127.0.0.1:3334".to_string()).expect("node");

    let err = node
        .keysend_value(
            "0334cc4bca04ce3d1537310f55e91ec4cec7e5a88fa0fba20a24cce1fe6de2a2b0".to_string(),
            3_000_000,
            None,
            None,
        )
        .expect_err("locked runtime session should fail");
    assert_eq!(
        err.as_string().unwrap_or_default(),
        "runtime session is locked; call unlock first"
    );

    crate::ldk_runtime::set_runtime_session_authorized(true);
    node.keysend_value(
        "0334cc4bca04ce3d1537310f55e91ec4cec7e5a88fa0fba20a24cce1fe6de2a2b0".to_string(),
        3_000_000,
        None,
        None,
    )
    .expect("authorized runtime session should succeed");
}

#[wasm_bindgen_test]
fn sdk_wallet_handle_invalid_json_contract() {
    let sdk = RlnWasmSdk::new();
    match sdk.create_wallet_handle("{invalid-json") {
        Ok(_) => panic!("expected constructor error"),
        Err(err) => {
            let msg = err.as_string().expect("error string");
            assert!(msg.contains("Invalid WalletData JSON"));
        }
    }
}

#[wasm_bindgen_test]
fn sdk_default_rgb_proxy_transport_set_get_clear_contract() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    let sdk = RlnWasmSdk::new();

    sdk.set_default_rgb_proxy_transport(
        "https://proxy.example.com/rgb/json-rpc".to_string(),
        Some("token-1".to_string()),
        Some("0334cc4bca04ce3d1537310f55e91ec4cec7e5a88fa0fba20a24cce1fe6de2a2b0".to_string()),
    )
    .expect("set default config");

    let value = sdk
        .default_rgb_proxy_transport_value()
        .expect("default config value");
    let parsed: RlnWasmRgbProxyTransportConfigData =
        serde_wasm_bindgen::from_value(value).expect("parse default config");
    assert_eq!(parsed.endpoint, "https://proxy.example.com/rgb/json-rpc");
    assert_eq!(parsed.auth_token.as_deref(), Some("token-1"));
    assert_eq!(
        parsed.node_id.as_deref(),
        Some("0334cc4bca04ce3d1537310f55e91ec4cec7e5a88fa0fba20a24cce1fe6de2a2b0")
    );

    sdk.clear_default_rgb_proxy_transport();
    let cleared = sdk
        .default_rgb_proxy_transport_value()
        .expect("cleared default config");
    assert!(cleared.is_null(), "expected null after clear");
}

#[wasm_bindgen_test]
fn sdk_default_rgb_proxy_transport_applies_to_new_wallet_contract() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    let sdk = RlnWasmSdk::new();
    sdk.set_default_rgb_proxy_transport(
        "http://127.0.0.1:3000/rgb/json-rpc".to_string(),
        None,
        None,
    )
    .expect("set default config");

    let wallet = sdk
        .new_wallet(&test_wallet_data_json())
        .expect("wallet with default transport");
    let value = wallet
        .rgb_proxy_transport_value()
        .expect("wallet proxy transport value");
    let parsed: RlnWasmRgbProxyTransportConfigData =
        serde_wasm_bindgen::from_value(value).expect("parse wallet config");
    assert_eq!(parsed.endpoint, "http://127.0.0.1:3000/rgb/json-rpc");
    assert!(parsed.auth_token.is_none());
    assert!(parsed.node_id.is_none());
}

#[wasm_bindgen_test]
fn sdk_default_rgb_proxy_transport_applies_to_wallet_handle_contract() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    let sdk = RlnWasmSdk::new();
    sdk.set_default_rgb_proxy_transport(
        "http://127.0.0.1:3000/rgb/json-rpc".to_string(),
        Some("token-2".to_string()),
        Some("0334cc4bca04ce3d1537310f55e91ec4cec7e5a88fa0fba20a24cce1fe6de2a2b0".to_string()),
    )
    .expect("set default config");

    let wallet_handle = sdk
        .create_wallet_handle(&test_wallet_data_json())
        .expect("wallet handle with default transport");
    let value = wallet_handle
        .rgb_proxy_transport_value()
        .expect("wallet handle proxy transport value");
    let parsed: RlnWasmRgbProxyTransportConfigData =
        serde_wasm_bindgen::from_value(value).expect("parse wallet handle config");
    assert_eq!(parsed.endpoint, "http://127.0.0.1:3000/rgb/json-rpc");
    assert_eq!(parsed.auth_token.as_deref(), Some("token-2"));
    assert_eq!(
        parsed.node_id.as_deref(),
        Some("0334cc4bca04ce3d1537310f55e91ec4cec7e5a88fa0fba20a24cce1fe6de2a2b0")
    );
}

#[wasm_bindgen_test(async)]
async fn sdk_default_rgb_proxy_transport_applies_to_wallet_handle_async_contract() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    let sdk = RlnWasmSdk::new();
    sdk.set_default_rgb_proxy_transport(
        "http://127.0.0.1:3000/rgb/json-rpc".to_string(),
        None,
        None,
    )
    .expect("set default config");

    let wallet_handle = sdk
        .create_wallet_handle_async(&test_wallet_data_json())
        .await
        .expect("wallet handle async with default transport");
    let value = wallet_handle
        .rgb_proxy_transport_value()
        .expect("wallet handle proxy transport value");
    let parsed: RlnWasmRgbProxyTransportConfigData =
        serde_wasm_bindgen::from_value(value).expect("parse wallet handle config");
    assert_eq!(parsed.endpoint, "http://127.0.0.1:3000/rgb/json-rpc");
    assert!(parsed.auth_token.is_none());
    assert!(parsed.node_id.is_none());
}

#[wasm_bindgen_test]
fn sdk_default_rgb_proxy_transport_clear_stops_auto_apply_contract() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    let sdk = RlnWasmSdk::new();
    sdk.set_default_rgb_proxy_transport(
        "http://127.0.0.1:3000/rgb/json-rpc".to_string(),
        None,
        None,
    )
    .expect("set default config");
    sdk.clear_default_rgb_proxy_transport();

    let wallet = sdk
        .new_wallet(&test_wallet_data_json())
        .expect("wallet without default transport");
    let value = wallet
        .rgb_proxy_transport_value()
        .expect("wallet proxy transport value");
    assert!(value.is_null(), "expected null after sdk default clear");
}

#[wasm_bindgen_test]
fn sdk_default_rgb_proxy_transport_backfills_existing_wallet_contract() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    let sdk = RlnWasmSdk::new();
    let wallet = sdk.new_wallet(&test_wallet_data_json()).expect("wallet");

    let before = wallet
        .rgb_proxy_transport_value()
        .expect("wallet proxy transport before default");
    assert!(before.is_null(), "expected null before default setup");

    sdk.set_default_rgb_proxy_transport(
        "http://127.0.0.1:3000/rgb/json-rpc".to_string(),
        None,
        None,
    )
    .expect("set sdk default");

    let after = wallet
        .rgb_proxy_transport_value()
        .expect("wallet proxy transport after default");
    let parsed: RlnWasmRgbProxyTransportConfigData =
        serde_wasm_bindgen::from_value(after).expect("parse wallet config");
    assert_eq!(parsed.endpoint, "http://127.0.0.1:3000/rgb/json-rpc");
    assert!(parsed.auth_token.is_none());
    assert!(parsed.node_id.is_none());
}

#[wasm_bindgen_test]
fn sdk_default_rgb_proxy_transport_clear_does_not_wipe_backfilled_wallet_contract() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    let sdk = RlnWasmSdk::new();
    let wallet = sdk.new_wallet(&test_wallet_data_json()).expect("wallet");

    sdk.set_default_rgb_proxy_transport(
        "http://127.0.0.1:3000/rgb/json-rpc".to_string(),
        None,
        None,
    )
    .expect("set sdk default");

    let _ = wallet
        .rgb_proxy_transport_value()
        .expect("trigger backfill from sdk default");

    sdk.clear_default_rgb_proxy_transport();

    let value = wallet
        .rgb_proxy_transport_value()
        .expect("wallet config should remain after sdk default clear");
    let parsed: RlnWasmRgbProxyTransportConfigData =
        serde_wasm_bindgen::from_value(value).expect("parse wallet config");
    assert_eq!(parsed.endpoint, "http://127.0.0.1:3000/rgb/json-rpc");
}

#[wasm_bindgen_test]
fn wallet_transport_overrides_sdk_default_contract() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    let sdk = RlnWasmSdk::new();
    let wallet = sdk.new_wallet(&test_wallet_data_json()).expect("wallet");

    sdk.set_default_rgb_proxy_transport(
        "http://127.0.0.1:3000/rgb/json-rpc".to_string(),
        None,
        None,
    )
    .expect("set sdk default");

    wallet
        .set_rgb_proxy_transport(
            "https://proxy.example.com/rgb/json-rpc".to_string(),
            None,
            None,
        )
        .expect("set wallet override");

    let value = wallet
        .rgb_proxy_transport_value()
        .expect("wallet transport value");
    let parsed: RlnWasmRgbProxyTransportConfigData =
        serde_wasm_bindgen::from_value(value).expect("parse config");
    assert_eq!(parsed.endpoint, "https://proxy.example.com/rgb/json-rpc");
}

#[wasm_bindgen_test]
fn clearing_wallet_transport_reverts_to_sdk_default_backfill_contract() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    let sdk = RlnWasmSdk::new();
    let wallet = sdk.new_wallet(&test_wallet_data_json()).expect("wallet");

    sdk.set_default_rgb_proxy_transport(
        "http://127.0.0.1:3000/rgb/json-rpc".to_string(),
        None,
        None,
    )
    .expect("set sdk default");

    wallet
        .set_rgb_proxy_transport(
            "https://proxy.example.com/rgb/json-rpc".to_string(),
            None,
            None,
        )
        .expect("set wallet override");
    wallet.clear_rgb_proxy_transport();

    let value = wallet
        .rgb_proxy_transport_value()
        .expect("wallet transport value after clear");
    let parsed: RlnWasmRgbProxyTransportConfigData =
        serde_wasm_bindgen::from_value(value).expect("parse config");
    assert_eq!(parsed.endpoint, "http://127.0.0.1:3000/rgb/json-rpc");
}

#[wasm_bindgen_test]
fn sdk_default_rgb_proxy_transport_empty_endpoint_validation_contract() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    let sdk = RlnWasmSdk::new();
    let err = sdk
        .set_default_rgb_proxy_transport("".to_string(), None, None)
        .expect_err("empty endpoint should fail");
    assert_eq!(
        err.as_string().unwrap_or_default(),
        sdk_contracts::ERR_RGB_PROXY_ENDPOINT_EMPTY
    );
}

#[wasm_bindgen_test]
fn sdk_default_rgb_proxy_transport_scheme_validation_contract() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    let sdk = RlnWasmSdk::new();
    let err = sdk
        .set_default_rgb_proxy_transport("rpc://127.0.0.1:3000/json-rpc".to_string(), None, None)
        .expect_err("invalid scheme should fail");
    assert_eq!(
        err.as_string().unwrap_or_default(),
        sdk_contracts::ERR_RGB_PROXY_ENDPOINT_SCHEME
    );
}

#[wasm_bindgen_test]
fn sdk_default_rgb_proxy_transport_auth_node_pair_validation_contract() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    let sdk = RlnWasmSdk::new();
    let err = sdk
        .set_default_rgb_proxy_transport(
            "http://127.0.0.1:3000/rgb/json-rpc".to_string(),
            Some("token".to_string()),
            None,
        )
        .expect_err("missing node_id should fail");
    assert_eq!(
        err.as_string().unwrap_or_default(),
        sdk_contracts::ERR_RGB_PROXY_AUTH_TOKEN_NODE_ID_TOGETHER
    );
}

#[wasm_bindgen_test]
fn sdk_default_rgb_proxy_transport_invalid_node_id_validation_contract() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    let sdk = RlnWasmSdk::new();
    let err = sdk
        .set_default_rgb_proxy_transport(
            "http://127.0.0.1:3000/rgb/json-rpc".to_string(),
            Some("token".to_string()),
            Some("bad-node-id".to_string()),
        )
        .expect_err("invalid node_id should fail");
    assert_eq!(
        err.as_string().unwrap_or_default(),
        sdk_contracts::ERR_RGB_PROXY_NODE_ID_INVALID
    );
}
