use super::*;

const TEST_DIR_BASE: &str = "tmp/vss_unreachable_openchannel/";

async fn unlock_electrum_only(node_address: SocketAddr, password: &str) {
    let payload = UnlockRequest {
        password: password.to_string(),
        ldk_chain_sync: LdkChainSync::TransactionSync {
            indexer_url: ELECTRUM_URL_REGTEST.to_string(),
        },
        indexer_url: Some(ELECTRUM_URL_REGTEST.to_string()),
        proxy_endpoint: Some(PROXY_ENDPOINT_LOCAL.to_string()),
        announce_addresses: vec![],
        announce_alias: Some(s!("RLN_alias")),
        gossip_source: None,
    };
    let res = reqwest::Client::new()
        .post(format!("http://{node_address}/unlock"))
        .json(&payload)
        .send()
        .await
        .unwrap();
    check_response_is_ok(res)
        .await
        .json::<EmptyResponse>()
        .await
        .unwrap();
}

async fn start_node_electrum_only(
    node_test_dir: &str,
    node_peer_port: u16,
) -> (SocketAddr, String) {
    let node_address = start_daemon(node_test_dir, node_peer_port, None, false).await;
    let password = format!("{node_test_dir}.{node_peer_port}");
    init(node_address, &password, None).await;
    unlock_electrum_only(node_address, &password).await;
    wait_for_peer_port_ready(node_peer_port).await;
    (node_address, password)
}

async fn start_vss_node_electrum_only(
    node_test_dir: &str,
    node_peer_port: u16,
    vss_url: &str,
) -> (SocketAddr, String) {
    let node_address = start_daemon_with_vss(
        node_test_dir,
        node_peer_port,
        false,
        Some(vss_url.into()),
        false,
    )
    .await;
    let password = format!("{node_test_dir}.{node_peer_port}");
    init(node_address, &password, None).await;
    clear_vss_fence(node_address, &password).await;
    unlock_electrum_only(node_address, &password).await;
    wait_for_peer_port_ready(node_peer_port).await;
    (node_address, password)
}

/// A node whose configured VSS server is unreachable must refuse a channel
/// open with a clear error instead of accepting it and leaving the channel
/// silently stuck in `Opening`; once VSS is reachable again the same open
/// must succeed.
#[serial_test::serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[traced_test]
async fn openchannel_refused_while_vss_unreachable() {
    tokio::time::timeout(
        std::time::Duration::from_secs(300),
        openchannel_refused_while_vss_unreachable_inner(),
    )
    .await
    .expect("openchannel_refused_while_vss_unreachable timed out");
}

async fn openchannel_refused_while_vss_unreachable_inner() {
    initialize();

    let test_dir_node1 = format!("{TEST_DIR_BASE}node1");
    let test_dir_node2 = format!("{TEST_DIR_BASE}node2");

    let proxy = super::vss_offline_force_close::VssProxy::start();
    let (node1_addr, _) =
        start_vss_node_electrum_only(&test_dir_node1, NODE1_PEER_PORT, &proxy.url()).await;
    let (node2_addr, _) = start_node_electrum_only(&test_dir_node2, NODE2_PEER_PORT).await;

    fund_and_create_utxos(node1_addr, None).await;
    fund_and_create_utxos(node2_addr, None).await;

    let node2_pubkey = node_info(node2_addr).await.pubkey;
    connect_peer(
        node1_addr,
        &node2_pubkey,
        &format!("127.0.0.1:{NODE2_PEER_PORT}"),
    )
    .await;

    proxy.go_offline();
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    let res = open_channel_request_raw(
        node1_addr,
        &node2_pubkey,
        Some(NODE2_PEER_PORT),
        Some(600_000),
        Some(100_000_000),
        None,
        None,
        None,
        None,
        None,
        None,
        true,
        true,
    )
    .await
    .expect_err("openchannel must be refused while VSS is unreachable");
    check_response_is_nok(
        res,
        reqwest::StatusCode::SERVICE_UNAVAILABLE,
        "VSS server is unreachable",
        "VssUnreachable",
    )
    .await;
    assert!(
        list_channels(node1_addr).await.is_empty(),
        "a refused open must not leave a channel behind"
    );

    // Once VSS is reachable again the same open must go through to a ready,
    // usable channel.
    proxy.go_online();
    open_channel(
        node1_addr,
        &node2_pubkey,
        Some(NODE2_PEER_PORT),
        Some(600_000),
        Some(100_000_000),
        None,
        None,
    )
    .await;
    wait_for_usable_channels(node1_addr, 1).await;
    wait_for_usable_channels(node2_addr, 1).await;

    shutdown(&[node1_addr, node2_addr]).await;
}
