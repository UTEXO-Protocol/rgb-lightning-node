use super::*;

const TEST_DIR_BASE: &str = "tmp/colored_channel_electrum/";

// Start, init and unlock a node using only the Electrum indexer (no bitcoind RPC),
// matching the indexer-only deployment from the bug report.
async fn start_node_electrum_only(node_test_dir: &str, node_peer_port: u16) -> SocketAddr {
    let node_address = start_daemon(node_test_dir, node_peer_port, None, false).await;
    let password = format!("{node_test_dir}.{node_peer_port}");
    init(node_address, &password, None).await;

    let payload = UnlockRequest {
        password,
        bitcoind_rpc_username: None,
        bitcoind_rpc_password: None,
        bitcoind_rpc_host: None,
        bitcoind_rpc_port: None,
        indexer_url: Some(ELECTRUM_URL_REGTEST.to_string()),
        proxy_endpoint: Some(PROXY_ENDPOINT_LOCAL.to_string()),
        announce_addresses: vec![],
        announce_alias: None,
        gossip_source: None,
    };
    let res = reqwest::Client::new()
        .post(format!("http://{node_address}/unlock"))
        .json(&payload)
        .send()
        .await
        .unwrap();
    assert_eq!(
        res.status(),
        reqwest::StatusCode::OK,
        "electrum-only unlock failed: {:?}",
        res.text().await
    );
    wait_for_peer_port_ready(node_peer_port).await;
    node_address
}

// Regression: a colored funding tx carries an OP_RETURN (RGB commitment) at vout 0.
// The Electrum tx-sync client derived the funding confirmation from the first
// output's script history, which Electrum servers don't index for OP_RETURN
// scripts, so the funding never confirmed and the channel never became ready.
#[serial_test::serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[traced_test]
async fn colored_channel_becomes_usable_with_electrum_only_sync() {
    initialize();

    let test_dir_node1 = format!("{TEST_DIR_BASE}node1");
    let test_dir_node2 = format!("{TEST_DIR_BASE}node2");
    let node1_addr = start_node_electrum_only(&test_dir_node1, NODE1_PEER_PORT).await;
    let node2_addr = start_node_electrum_only(&test_dir_node2, NODE2_PEER_PORT).await;

    fund_and_create_utxos(node1_addr, None).await;
    fund_and_create_utxos(node2_addr, None).await;

    let asset_id = issue_asset_nia(node1_addr).await.asset_id;
    let node2_pubkey = node_info(node2_addr).await.pubkey;

    let channel = open_channel(
        node1_addr,
        &node2_pubkey,
        Some(NODE2_PEER_PORT),
        None,
        Some(3500000),
        Some(600),
        Some(&asset_id),
    )
    .await;

    assert!(
        channel.ready && channel.is_usable,
        "colored channel never became usable over electrum-only sync (ready={}, usable={})",
        channel.ready,
        channel.is_usable,
    );
}
