use crate::test_utils::test_wallet_data_json;
use crate::*;
use rgb_lib_wasm::AssetSchema;
use wasm_bindgen::JsValue;
use wasm_bindgen_test::*;

wasm_bindgen_test_configure!(run_in_browser);

#[wasm_bindgen_test]
fn check_indexer_url_value_success_contract() {
    let value = check_indexer_url_value(
        "regtest".to_string(),
        "https://indexer.example.com".to_string(),
    )
    .expect("check indexer");
    let parsed: CheckIndexerUrlData = serde_wasm_bindgen::from_value(value).expect("parse");
    assert_eq!(parsed.indexer_protocol, "esplora");
}

#[wasm_bindgen_test]
fn check_indexer_url_json_success_contract() {
    let json = check_indexer_url_json(
        "regtest".to_string(),
        "https://indexer.example.com".to_string(),
    )
    .expect("check indexer json");
    let parsed: CheckIndexerUrlData = serde_json::from_str(&json).expect("parse json");
    assert_eq!(parsed.indexer_protocol, "esplora");
}

#[wasm_bindgen_test]
fn check_indexer_url_value_empty_error_contract() {
    let err =
        check_indexer_url_value("regtest".to_string(), "".to_string()).expect_err("should fail");
    let msg = err.as_string().expect("error string");
    assert_eq!(msg, sdk_contracts::ERR_INDEXER_URL_EMPTY);
}

#[wasm_bindgen_test]
fn check_indexer_url_value_invalid_network_contract() {
    let err = check_indexer_url_value(
        "invalid-network".to_string(),
        "https://indexer.example.com".to_string(),
    )
    .expect_err("should fail");
    let msg = err.as_string().expect("error string");
    assert!(msg.contains("unsupported network"));
}

#[wasm_bindgen_test]
fn check_indexer_url_value_unsupported_electrum_contract() {
    let err = check_indexer_url_value(
        "regtest".to_string(),
        "ssl://electrum.example.com:50002".to_string(),
    )
    .expect_err("should fail");
    let msg = err.as_string().expect("error string");
    assert_eq!(msg, "electrum indexer URLs are not supported in wasm build");
}

#[wasm_bindgen_test]
fn check_indexer_url_value_invalid_http_format_contract() {
    let err = check_indexer_url_value("regtest".to_string(), "https:///api/v1/".to_string())
        .expect_err("should fail");
    let msg = err.as_string().expect("error string");
    assert_eq!(msg, "invalid indexer_url format");
}

#[wasm_bindgen_test(async)]
async fn sdk_wallet_handle_go_online_empty_indexer_contract() {
    let sdk = RlnWasmSdk::new();
    let wallet_json = test_wallet_data_json();
    let wallet = sdk
        .create_wallet_handle(&wallet_json)
        .expect("wallet handle");
    let err = wallet
        .go_online_value(false, "".to_string())
        .await
        .expect_err("should fail");
    let msg = err.as_string().expect("error string");
    assert_eq!(msg, sdk_contracts::ERR_INDEXER_URL_EMPTY);
}

#[wasm_bindgen_test(async)]
async fn sdk_wallet_handle_refresh_empty_asset_id_contract() {
    let sdk = RlnWasmSdk::new();
    let wallet_json = test_wallet_data_json();
    let wallet = sdk
        .create_wallet_handle(&wallet_json)
        .expect("wallet handle");
    let err = wallet
        .refresh_value(JsValue::NULL, Some("".to_string()), JsValue::NULL, false)
        .await
        .expect_err("should fail");
    let msg = err.as_string().expect("error string");
    assert_eq!(msg, sdk_contracts::ERR_ASSET_ID_EMPTY_IF_PROVIDED);
}

#[wasm_bindgen_test(async)]
async fn sdk_wallet_handle_send_btc_begin_empty_address_contract() {
    let sdk = RlnWasmSdk::new();
    let wallet_json = test_wallet_data_json();
    let wallet = sdk
        .create_wallet_handle(&wallet_json)
        .expect("wallet handle");
    let err = wallet
        .send_btc_begin(JsValue::NULL, "".to_string(), 1, 1, true)
        .await
        .expect_err("should fail");
    let msg = err.as_string().expect("error string");
    assert_eq!(msg, sdk_contracts::ERR_ADDRESS_EMPTY);
}

#[wasm_bindgen_test]
fn sdk_wallet_handle_get_address_success_contract() {
    let sdk = RlnWasmSdk::new();
    let wallet_json = test_wallet_data_json();
    let wallet = sdk
        .create_wallet_handle(&wallet_json)
        .expect("wallet handle");
    let address = wallet.get_address().expect("address");
    assert!(
        address.starts_with("bcrt1"),
        "unexpected address: {address}"
    );
}

#[wasm_bindgen_test]
fn sdk_wallet_handle_list_transactions_empty_contract() {
    let sdk = RlnWasmSdk::new();
    let wallet_json = test_wallet_data_json();
    let wallet = sdk
        .create_wallet_handle(&wallet_json)
        .expect("wallet handle");
    let txs_js = wallet.list_transactions_value().expect("list txs");
    let txs: serde_json::Value = serde_wasm_bindgen::from_value(txs_js).expect("parse txs");
    let arr = txs.as_array().expect("txs array");
    assert!(arr.is_empty(), "fresh wallet should have no txs");
}

#[wasm_bindgen_test]
fn sdk_wallet_handle_list_assets_empty_contract() {
    let sdk = RlnWasmSdk::new();
    let wallet_json = test_wallet_data_json();
    let wallet = sdk
        .create_wallet_handle(&wallet_json)
        .expect("wallet handle");
    let schemas_js = serde_wasm_bindgen::to_value(&vec![AssetSchema::Nia]).expect("schemas");
    let assets_js = wallet.list_assets_value(schemas_js).expect("list assets");
    let assets: serde_json::Value =
        serde_wasm_bindgen::from_value(assets_js).expect("parse assets");
    let nia = assets["nia"].as_array().expect("nia array");
    assert!(nia.is_empty(), "fresh wallet should have no NIA assets");
}

#[wasm_bindgen_test]
fn sdk_wallet_handle_get_asset_media_empty_asset_id_contract() {
    let sdk = RlnWasmSdk::new();
    let wallet_json = test_wallet_data_json();
    let wallet = sdk
        .create_wallet_handle(&wallet_json)
        .expect("wallet handle");
    let err = wallet
        .get_asset_media_value("".to_string())
        .expect_err("should fail");
    let msg = err.as_string().expect("error string");
    assert_eq!(msg, sdk_contracts::ERR_ASSET_ID_EMPTY);
}

#[wasm_bindgen_test]
fn sdk_wallet_handle_get_asset_media_invalid_digest_contract() {
    let sdk = RlnWasmSdk::new();
    let wallet_json = test_wallet_data_json();
    let wallet = sdk
        .create_wallet_handle(&wallet_json)
        .expect("wallet handle");
    let err = wallet
        .get_asset_media_json("not-a-digest".to_string())
        .expect_err("should fail");
    let msg = err.as_string().expect("error string");
    assert_eq!(msg, "invalid media digest");
}

#[wasm_bindgen_test(async)]
async fn sdk_wallet_handle_get_asset_media_wasm_runtime_contract() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    let sdk = RlnWasmSdk::new();
    let wallet_json = test_wallet_data_json();
    let wallet = sdk
        .create_wallet_handle(&wallet_json)
        .expect("wallet handle");
    let posted = sdk
        .post_asset_media_json("image/png".to_string(), "00ff".to_string())
        .await
        .expect("post media");
    let posted_doc: serde_json::Value = serde_json::from_str(&posted).expect("parse posted");
    let digest = posted_doc["digest"].as_str().expect("digest").to_string();
    let media_json = wallet
        .get_asset_media_json(digest)
        .expect("media should exist");
    let media_doc: serde_json::Value = serde_json::from_str(&media_json).expect("parse media");
    assert_eq!(media_doc["bytes_hex"], "00ff");
}

#[wasm_bindgen_test(async)]
async fn sdk_facade_wallet_get_asset_media_wasm_runtime_contract() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    let sdk = RlnWasmSdk::new();
    let wallet_json = test_wallet_data_json();
    let wallet = sdk.new_wallet(&wallet_json).expect("wallet");
    let posted = sdk
        .post_asset_media_json("image/jpeg".to_string(), "ab12".to_string())
        .await
        .expect("post media");
    let posted_doc: serde_json::Value = serde_json::from_str(&posted).expect("parse posted");
    let digest = posted_doc["digest"].as_str().expect("digest").to_string();
    let media_json = sdk
        .wallet_get_asset_media_json(&wallet, digest)
        .expect("media should exist");
    let media_doc: serde_json::Value = serde_json::from_str(&media_json).expect("parse media");
    assert_eq!(media_doc["bytes_hex"], "ab12");
}

#[wasm_bindgen_test(async)]
async fn sdk_wallet_get_asset_media_persists_across_memory_reset_contract() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    let sdk = RlnWasmSdk::new();
    let wallet_json = test_wallet_data_json();
    let wallet = sdk.new_wallet(&wallet_json).expect("wallet");
    let posted = sdk
        .post_asset_media_json("image/webp".to_string(), "beef".to_string())
        .await
        .expect("post media");
    let posted_doc: serde_json::Value = serde_json::from_str(&posted).expect("parse posted");
    let digest = posted_doc["digest"].as_str().expect("digest").to_string();

    WASM_MEDIA_STORE.with(|store| {
        store.borrow_mut().clear();
    });

    let media_json = sdk
        .wallet_get_asset_media_json(&wallet, digest)
        .expect("media should load from persistent storage");
    let media_doc: serde_json::Value = serde_json::from_str(&media_json).expect("parse media");
    assert_eq!(media_doc["bytes_hex"], "beef");
}

#[wasm_bindgen_test(async)]
async fn sdk_create_wallet_handle_async_success_contract() {
    let sdk = RlnWasmSdk::new();
    let wallet_json = test_wallet_data_json();
    let wallet = sdk
        .create_wallet_handle_async(&wallet_json)
        .await
        .expect("wallet handle async");
    let address = wallet.get_address().expect("address");
    assert!(
        address.starts_with("bcrt1"),
        "unexpected address: {address}"
    );
}

#[wasm_bindgen_test(async)]
async fn sdk_create_wallet_handle_async_list_transactions_empty_contract() {
    let sdk = RlnWasmSdk::new();
    let wallet_json = test_wallet_data_json();
    let wallet = sdk
        .create_wallet_handle_async(&wallet_json)
        .await
        .expect("wallet handle async");
    let txs_js = wallet.list_transactions_value().expect("list txs");
    let txs: serde_json::Value = serde_wasm_bindgen::from_value(txs_js).expect("parse txs");
    let arr = txs.as_array().expect("txs array");
    assert!(arr.is_empty(), "fresh wallet should have no txs");
}

#[wasm_bindgen_test]
fn sdk_wallet_handle_rgb_proxy_transport_set_get_clear_contract() {
    let sdk = RlnWasmSdk::new();
    let wallet_json = test_wallet_data_json();
    let wallet = sdk
        .create_wallet_handle(&wallet_json)
        .expect("wallet handle");

    wallet
        .set_rgb_proxy_transport("http://127.0.0.1:3000/json-rpc".to_string(), None, None)
        .expect("set proxy transport");

    let value = wallet
        .rgb_proxy_transport_value()
        .expect("proxy transport value");
    let parsed: RlnWasmRgbProxyTransportConfigData =
        serde_wasm_bindgen::from_value(value).expect("parse proxy transport");
    assert_eq!(parsed.endpoint, "http://127.0.0.1:3000/json-rpc");
    assert_eq!(parsed.auth_token, None);
    assert_eq!(parsed.node_id, None);

    wallet.clear_rgb_proxy_transport();
    let cleared = wallet
        .rgb_proxy_transport_value()
        .expect("cleared proxy transport value");
    assert!(cleared.is_null(), "expected null after clear");
}

#[wasm_bindgen_test]
fn sdk_wallet_handle_rgb_proxy_transport_pair_validation_contract() {
    let sdk = RlnWasmSdk::new();
    let wallet_json = test_wallet_data_json();
    let wallet = sdk
        .create_wallet_handle(&wallet_json)
        .expect("wallet handle");

    let err = wallet
        .set_rgb_proxy_transport(
            "http://127.0.0.1:3000/json-rpc".to_string(),
            Some("token".to_string()),
            None,
        )
        .expect_err("should require node_id with auth token");
    let msg = err.as_string().expect("error string");
    assert_eq!(
        msg,
        sdk_contracts::ERR_RGB_PROXY_AUTH_TOKEN_NODE_ID_TOGETHER
    );
}

#[wasm_bindgen_test]
fn sdk_wallet_handle_blind_receive_requires_endpoints_or_proxy_config_contract() {
    let sdk = RlnWasmSdk::new();
    let wallet_json = test_wallet_data_json();
    let wallet = sdk
        .create_wallet_handle(&wallet_json)
        .expect("wallet handle");

    let assignment_js =
        serde_wasm_bindgen::to_value(&rgb_lib_wasm::Assignment::Any).expect("assignment");
    let err = wallet
        .blind_receive_value(None, assignment_js, None, JsValue::NULL, 1)
        .expect_err("should require endpoints or configured proxy");
    let msg = err.as_string().expect("error string");
    assert_eq!(msg, sdk_contracts::ERR_TRANSPORT_ENDPOINTS_MISSING);
}

#[wasm_bindgen_test]
fn sdk_wallet_facade_rgb_proxy_transport_contract() {
    let sdk = RlnWasmSdk::new();
    let wallet_json = test_wallet_data_json();
    let wallet = sdk.new_wallet(&wallet_json).expect("wallet");

    sdk.wallet_set_rgb_proxy_transport(
        &wallet,
        "https://proxy.example.com/json-rpc".to_string(),
        Some("auth-token".to_string()),
        Some("0334cc4bca04ce3d1537310f55e91ec4cec7e5a88fa0fba20a24cce1fe6de2a2b0".to_string()),
    )
    .expect("set proxy transport via facade");

    let value = sdk
        .wallet_rgb_proxy_transport_value(&wallet)
        .expect("value via facade");
    let parsed: RlnWasmRgbProxyTransportConfigData =
        serde_wasm_bindgen::from_value(value).expect("parse");
    assert_eq!(parsed.endpoint, "https://proxy.example.com/json-rpc");
    assert_eq!(parsed.auth_token.as_deref(), Some("auth-token"));
    assert_eq!(
        parsed.node_id.as_deref(),
        Some("0334cc4bca04ce3d1537310f55e91ec4cec7e5a88fa0fba20a24cce1fe6de2a2b0")
    );

    sdk.wallet_clear_rgb_proxy_transport(&wallet);
    let cleared = sdk
        .wallet_rgb_proxy_transport_value(&wallet)
        .expect("cleared value via facade");
    assert!(cleared.is_null(), "expected null after facade clear");
}

#[wasm_bindgen_test]
fn wallet_rgb_proxy_transport_persists_across_wrappers_contract() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    let sdk = RlnWasmSdk::new();
    let wallet_json = test_wallet_data_json();

    let wallet_a = sdk.new_wallet(&wallet_json).expect("wallet a");
    wallet_a
        .set_rgb_proxy_transport("http://127.0.0.1:3000/rgb/json-rpc".to_string(), None, None)
        .expect("set transport on wallet a");

    let wallet_b = sdk.new_wallet(&wallet_json).expect("wallet b");
    let value_b = wallet_b
        .rgb_proxy_transport_value()
        .expect("read transport from wallet b");
    let parsed_b: RlnWasmRgbProxyTransportConfigData =
        serde_wasm_bindgen::from_value(value_b).expect("parse wallet b transport");
    assert_eq!(parsed_b.endpoint, "http://127.0.0.1:3000/rgb/json-rpc");

    wallet_b.clear_rgb_proxy_transport();
    let value_a = wallet_a
        .rgb_proxy_transport_value()
        .expect("read transport from wallet a after clear");
    assert!(
        value_a.is_null(),
        "expected clear on wallet b to affect shared wallet config"
    );
}

#[wasm_bindgen_test]
fn wallet_rgb_proxy_transport_restores_from_local_storage_contract() {
    crate::test_utils::reset_wasm_runtime_state_for_tests();
    let sdk = RlnWasmSdk::new();
    let wallet_json = test_wallet_data_json();

    let wallet_a = sdk.new_wallet(&wallet_json).expect("wallet a");
    wallet_a
        .set_rgb_proxy_transport("http://127.0.0.1:3000/rgb/json-rpc".to_string(), None, None)
        .expect("set transport on wallet a");

    WASM_WALLET_RGB_PROXY_TRANSPORTS.with(|store| {
        store.borrow_mut().clear();
    });

    let wallet_b = sdk.new_wallet(&wallet_json).expect("wallet b");
    let value_b = wallet_b
        .rgb_proxy_transport_value()
        .expect("read transport from wallet b");
    let parsed_b: RlnWasmRgbProxyTransportConfigData =
        serde_wasm_bindgen::from_value(value_b).expect("parse wallet b transport");
    assert_eq!(parsed_b.endpoint, "http://127.0.0.1:3000/rgb/json-rpc");
}
