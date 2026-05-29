use super::*;

const TEST_DIR_BASE: &str = "tmp/init_esplora/";

// To run locally:
//   docker compose --profile esplora up -d
//   cargo test --no-fail-fast init_esplora -- --ignored --test-threads=1
//   docker compose --profile esplora down
#[serial_test::serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[traced_test]
#[ignore = "requires docker compose --profile esplora up"]
async fn init_esplora_path_unlocks_without_bitcoind() {
    initialize();

    let test_dir_node1 = format!("{TEST_DIR_BASE}node1");
    let node1_addr = start_daemon(&test_dir_node1, NODE1_PEER_PORT, None, false).await;

    let mnemonic = s!(
        "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about"
    );
    let init_1 = init(node1_addr, "password123", Some(mnemonic.clone())).await;
    assert_eq!(init_1.mnemonic, mnemonic);

    let payload = UnlockRequest {
        password: s!("password123"),
        bitcoind_rpc_username: None,
        bitcoind_rpc_password: None,
        bitcoind_rpc_host: None,
        bitcoind_rpc_port: None,
        indexer_url: Some(crate::utils::ESPLORA_URL_REGTEST.to_string()),
        proxy_endpoint: Some(PROXY_ENDPOINT_LOCAL.to_string()),
        announce_addresses: vec![],
        announce_alias: None,
    };
    let res = reqwest::Client::new()
        .post(format!("http://{node1_addr}/unlock"))
        .json(&payload)
        .send()
        .await
        .unwrap();
    assert_eq!(
        res.status(),
        reqwest::StatusCode::OK,
        "esplora unlock failed: {:?}",
        res.text().await
    );

    let info = node_info(node1_addr).await;
    assert!(
        !info.pubkey.is_empty(),
        "node has no pubkey after esplora unlock"
    );
}
