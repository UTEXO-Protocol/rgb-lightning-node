//! rgb-lib-bfa has no IFA link-right implementation. These regressions deliberately prove
//! truthful rejection and unchanged wallet state, rather than claiming legacy link parity.
use super::*;

const TEST_DIR_BASE: &str = "tmp/asset_link/";

#[serial_test::serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[traced_test]
async fn asset_link_unavailable_does_not_mutate_wallet_or_broadcast() {
    initialize();
    let test_dir = format!("{TEST_DIR_BASE}unavailable");
    let (node_addr, _) = start_node(&test_dir, NODE1_PEER_PORT, false).await;
    fund_and_create_utxos(node_addr, None).await;
    let before_assets = serde_json::to_value(list_assets(node_addr).await).unwrap();
    let before_unspents = serde_json::to_value(list_unspents(node_addr).await).unwrap();
    let before_transactions = serde_json::to_value(list_transactions(node_addr).await).unwrap();

    for (parent, child) in [("parent", "child"), ("same", "same")] {
        let res = reqwest::Client::new()
            .post(format!("http://{node_addr}/assetlink"))
            .json(&AssetLinkRequest {
                parent_asset_id: parent.into(),
                child_asset_id: child.into(),
                min_confirmations: 1,
            })
            .send()
            .await
            .unwrap();
        check_response_is_nok(
            res,
            StatusCode::BAD_REQUEST,
            "IFA linking is unavailable in the local BFA build",
            "InvalidRequest",
        )
        .await;
    }
    assert_eq!(
        serde_json::to_value(list_assets(node_addr).await).unwrap(),
        before_assets
    );
    assert_eq!(
        serde_json::to_value(list_unspents(node_addr).await).unwrap(),
        before_unspents
    );
    assert_eq!(
        serde_json::to_value(list_transactions(node_addr).await).unwrap(),
        before_transactions
    );
    shutdown(&[node_addr]).await;
}

#[serial_test::serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[traced_test]
async fn ifa_link_right_issuance_unavailable_does_not_mutate_wallet_or_broadcast() {
    initialize();
    let test_dir = format!("{TEST_DIR_BASE}issuance");
    let (node_addr, _) = start_node(&test_dir, NODE1_PEER_PORT, false).await;
    fund_and_create_utxos(node_addr, None).await;
    let before_assets = serde_json::to_value(list_assets(node_addr).await).unwrap();
    let before_transactions = serde_json::to_value(list_transactions(node_addr).await).unwrap();
    for issuance_type in [
        IfaIssuanceType::LinkRightOnly,
        IfaIssuanceType::LinkedFromParent {
            contract_id: "parent".into(),
            request_link_right: false,
        },
    ] {
        let res = reqwest::Client::new()
            .post(format!("http://{node_addr}/issueassetifa"))
            .json(&IssueAssetIFARequest {
                amounts: vec![1000],
                ticker: "TICKER".into(),
                name: "Test asset".into(),
                precision: 0,
                inflation_amounts: vec![1000],
                reject_list_url: None,
                issuance_type: Some(issuance_type),
            })
            .send()
            .await
            .unwrap();
        check_response_is_nok(
            res,
            StatusCode::BAD_REQUEST,
            "IFA link rights are unavailable",
            "InvalidRequest",
        )
        .await;
    }
    assert_eq!(
        serde_json::to_value(list_assets(node_addr).await).unwrap(),
        before_assets
    );
    assert_eq!(
        serde_json::to_value(list_transactions(node_addr).await).unwrap(),
        before_transactions
    );
    shutdown(&[node_addr]).await;
}
