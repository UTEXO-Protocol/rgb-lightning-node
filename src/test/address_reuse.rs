//! rgb-lib-bfa uses fresh addresses. Pinned-address reuse is explicitly unsupported.
use super::*;

const TEST_DIR_BASE: &str = "tmp/address_reuse/";
const FUND_SATS: u64 = 100_000_000;
#[cfg(feature = "vss")]
const VSS_URL: &str = "http://localhost:8081/vss";

async fn rotate_address(node: SocketAddr) -> String {
    let response = reqwest::Client::new()
        .post(format!("http://{node}/rotateaddress"))
        .send()
        .await
        .unwrap();
    check_response_is_ok(response)
        .await
        .json::<AddressResponse>()
        .await
        .unwrap()
        .address
}
fn fund_and_mine(address: String, sats: u64) {
    fund_wallet(address, sats);
    mine(false);
}
async fn settled_btc(node: SocketAddr) -> u64 {
    btc_balance(node).await.vanilla.settled
}

#[tokio::test]
async fn reuse_configuration_is_rejected_before_rest_wallet_creation() {
    let temporary = tempfile::tempdir().unwrap();
    let wallet = temporary.path().join("not-created");
    let result = app(UserArgs {
        storage_dir_path: wallet.clone(),
        reuse_addresses: true,
        ..Default::default()
    })
    .await;
    assert!(
        matches!(result,Err(crate::error::AppError::InvalidConfig(message)) if message.contains("Address reuse is not supported"))
    );
    assert!(
        !wallet.exists(),
        "unsupported reuse must not initialize logs, DB or wallet state"
    );
}

#[serial_test::serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[traced_test]
async fn default_address_calls_generate_fresh_addresses() {
    initialize();
    let (node, _) = start_node(&format!("{TEST_DIR_BASE}default"), NODE1_PEER_PORT, false).await;
    let mut seen = std::collections::HashSet::new();
    for _ in 0..3 {
        assert!(seen.insert(address(node).await));
    }
    shutdown(&[node]).await;
}

#[serial_test::serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[traced_test]
async fn explicit_reuse_false_generates_fresh_addresses() {
    initialize();
    let (node, _, _) = start_node_with_reuse_addresses(
        &format!("{TEST_DIR_BASE}explicit_false"),
        NODE1_PEER_PORT,
        false,
        false,
        None,
    )
    .await;
    assert_ne!(address(node).await, address(node).await);
    shutdown(&[node]).await;
}

#[serial_test::serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[traced_test]
async fn rotation_generates_a_fresh_address_and_both_addresses_receive_funds() {
    initialize();
    let (node, _) = start_node(&format!("{TEST_DIR_BASE}rotation"), NODE1_PEER_PORT, false).await;
    let original = address(node).await;
    let rotated = rotate_address(node).await;
    assert_ne!(original, rotated);
    let next = address(node).await;
    assert_ne!(next, original);
    assert_ne!(next, rotated);
    fund_and_mine(original, FUND_SATS);
    fund_and_mine(rotated, FUND_SATS);
    assert!(settled_btc(node).await >= 2 * FUND_SATS);
    shutdown(&[node]).await;
}

#[serial_test::serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[traced_test]
async fn fresh_address_funds_survive_restart() {
    initialize();
    let directory = format!("{TEST_DIR_BASE}restart");
    let (node, _, _) =
        start_node_with_reuse_addresses(&directory, NODE1_PEER_PORT, false, false, None).await;
    fund_and_mine(address(node).await, FUND_SATS);
    fund_and_mine(rotate_address(node).await, FUND_SATS);
    let before = settled_btc(node).await;
    assert!(before >= 2 * FUND_SATS);
    shutdown(&[node]).await;
    let (node, _, _) =
        start_node_with_reuse_addresses(&directory, NODE1_PEER_PORT, false, true, None).await;
    assert!(settled_btc(node).await >= before);
    shutdown(&[node]).await;
}

#[serial_test::serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[traced_test]
async fn fresh_address_funds_recover_from_same_seed() {
    initialize();
    let directory = format!("{TEST_DIR_BASE}seed");
    let (node, _, seed) =
        start_node_with_reuse_addresses(&directory, NODE1_PEER_PORT, false, false, None).await;
    fund_and_mine(address(node).await, FUND_SATS);
    fund_and_mine(rotate_address(node).await, FUND_SATS);
    let before = settled_btc(node).await;
    assert!(before >= 2 * FUND_SATS);
    shutdown(&[node]).await;
    let (node, _, _) =
        start_node_with_reuse_addresses(&directory, NODE1_PEER_PORT, false, false, Some(&seed))
            .await;
    assert!(
        settled_btc(node).await >= before,
        "same-seed scan must recover confirmed BTC at all generated fixture addresses"
    );
    shutdown(&[node]).await;
}

#[serial_test::serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[traced_test]
async fn revealed_address_index_persists_across_restart() {
    initialize();
    let directory = format!("{TEST_DIR_BASE}index");
    let (node, _, _) =
        start_node_with_reuse_addresses(&directory, NODE1_PEER_PORT, false, false, None).await;
    let revealed = std::collections::HashSet::from([
        address(node).await,
        rotate_address(node).await,
        address(node).await,
    ]);
    assert_eq!(revealed.len(), 3);
    shutdown(&[node]).await;
    let (node, _, _) =
        start_node_with_reuse_addresses(&directory, NODE1_PEER_PORT, false, true, None).await;
    assert!(!revealed.contains(&address(node).await));
    assert!(!revealed.contains(&rotate_address(node).await));
    shutdown(&[node]).await;
}

#[cfg(feature = "vss")]
#[serial_test::serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[traced_test]
async fn fresh_address_and_assets_recover_via_vss() {
    assert!(
        std::net::TcpStream::connect_timeout(
            &"127.0.0.1:8081".parse().unwrap(),
            std::time::Duration::from_secs(2)
        )
        .is_ok(),
        "required VSS fixture is unavailable"
    );
    initialize();
    let directory = format!("{TEST_DIR_BASE}vss");
    let (node, _, seed) =
        start_node_with_vss(&directory, NODE1_PEER_PORT, false, VSS_URL, None, false).await;
    let identity = node_info(node).await.pubkey;
    let revealed =
        std::collections::HashSet::from([address(node).await, rotate_address(node).await]);
    fund_and_create_utxos(node, None).await;
    let asset = issue_asset_nia(node).await.asset_id;
    assert_eq!(asset_balance_spendable(node, &asset).await, ISSUE_AMT);
    assert_eq!(
        vss_backup_info(node).await["backup_exists"],
        serde_json::json!(true)
    );
    let before = settled_btc(node).await;
    wipe_node_dir(node, &directory).await;
    let (node, _, _) = start_node_with_vss(
        &directory,
        NODE1_PEER_PORT,
        true,
        VSS_URL,
        Some(&seed),
        false,
    )
    .await;
    assert_eq!(
        node_info(node).await.pubkey,
        identity,
        "restored runtime identity must match its seed-derived cloud store"
    );
    assert!(
        !revealed.contains(&address(node).await),
        "restored BDK address indices must retain the fresh-only policy"
    );
    assert!(settled_btc(node).await >= before);
    assert_eq!(asset_balance_spendable(node, &asset).await, ISSUE_AMT);
    assert_eq!(
        vss_backup_info(node).await["backup_exists"],
        serde_json::json!(true)
    );
    shutdown(&[node]).await;
}
