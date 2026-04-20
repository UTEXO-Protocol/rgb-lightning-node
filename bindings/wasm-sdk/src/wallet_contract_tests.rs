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
    assert_eq!(msg, "indexer_url cannot be empty");
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
    assert_eq!(msg, "indexer_url cannot be empty");
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
    assert_eq!(msg, "asset_id cannot be empty if provided");
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
    assert_eq!(msg, "address cannot be empty");
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
    assert_eq!(msg, "asset_id cannot be empty");
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
async fn sdk_wallet_handle_get_asset_media_scaffold_contract() {
    reset_wasm_runtime_state_for_tests();
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
async fn sdk_facade_wallet_get_asset_media_scaffold_contract() {
    reset_wasm_runtime_state_for_tests();
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
    reset_wasm_runtime_state_for_tests();
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
