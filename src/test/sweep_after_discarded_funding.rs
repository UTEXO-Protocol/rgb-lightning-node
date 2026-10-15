use super::*;

const TEST_DIR_BASE: &str = "tmp/sweep_after_discarded_funding/";

/// Recreates the bitcoind container with `extra_args` appended to its command line (see
/// `BITCOIND_EXTRA_ARGS` in compose.yaml), then waits for RPC and reloads the miner wallet.
fn recreate_bitcoind(extra_args: &str) {
    let status = Command::new("docker")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .env("BITCOIND_EXTRA_ARGS", extra_args)
        .args([
            "compose",
            "up",
            "-d",
            "--force-recreate",
            "--no-deps",
            "bitcoind",
        ])
        .status()
        .expect("failed to recreate bitcoind");
    assert!(status.success());
    let t_0 = OffsetDateTime::now_utc();
    loop {
        let ready = Command::new("docker")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .arg("compose")
            .args(bitcoin_cli())
            .arg("getindexinfo")
            .status()
            .expect("failed to call bitcoin-cli")
            .success();
        if ready {
            break;
        }
        if (OffsetDateTime::now_utc() - t_0).as_seconds_f32() > 60.0 {
            panic!("bitcoind RPC is not becoming ready");
        }
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
    let _ = Command::new("docker")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .arg("compose")
        .args(bitcoin_cli())
        .args(["loadwallet", "miner"])
        .status();
    // electrs may have exited while bitcoind was down
    let status = Command::new("docker")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .args(["compose", "up", "-d", "--no-deps", "electrs"])
        .status()
        .expect("failed to start electrs");
    assert!(status.success());
}

fn txindex_enabled() -> bool {
    bitcoind(&["getindexinfo"]).contains("txindex")
}

/// Runs bitcoind without txindex for as long as it lives, as an electrs-backed node may.
struct NoTxindex;

impl NoTxindex {
    fn start() -> Self {
        recreate_bitcoind("-txindex=0");
        assert!(!txindex_enabled());
        NoTxindex
    }
}

impl Drop for NoTxindex {
    fn drop(&mut self) {
        recreate_bitcoind("");
    }
}

#[serial_test::serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[traced_test]
async fn sweep_after_discarded_funding() {
    initialize();

    // without txindex, electrs reports a transaction that never reached the chain with bitcoind's
    // "No such mempool transaction. Use -txindex ..." error
    let _no_txindex = NoTxindex::start();

    let test_dir_node1 = format!("{TEST_DIR_BASE}node1");
    let test_dir_node2 = format!("{TEST_DIR_BASE}node2");
    let test_dir_node3 = format!("{TEST_DIR_BASE}node3");
    let (node1_addr, _) = start_node(&test_dir_node1, NODE1_PEER_PORT, false).await;
    let (node2_addr, _) = start_node(&test_dir_node2, NODE2_PEER_PORT, false).await;
    let (node3_addr, _) = start_node(&test_dir_node3, NODE3_PEER_PORT, false).await;

    fund_and_create_utxos(node1_addr, None).await;
    fund_and_create_utxos(node2_addr, None).await;
    fund_and_create_utxos(node3_addr, None).await;

    let asset_id = issue_asset_nia(node1_addr).await.asset_id;

    let node2_pubkey = node_info(node2_addr).await.pubkey;
    let node3_pubkey = node_info(node3_addr).await.pubkey;

    // a colored channel discarded before broadcast leaves its funding witness in node1's stock
    *DROP_FUNDING_SIGNED_ON_NODE.lock().unwrap() =
        Some(PublicKey::from_str(&node2_pubkey).unwrap());
    open_channel_request_raw(
        node1_addr,
        &node2_pubkey,
        Some(NODE2_PEER_PORT),
        Some(100_000),
        None,
        Some(ISSUE_AMT),
        Some(&asset_id),
        None,
        None,
        None,
        None,
        true,
        true,
    )
    .await
    .unwrap();
    let t_0 = OffsetDateTime::now_utc();
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        if list_channels(node1_addr).await.is_empty() {
            break;
        }
        if (OffsetDateTime::now_utc() - t_0).as_seconds_f32() > 10.0 {
            panic!("channel is not disappearing");
        }
    }
    *DROP_FUNDING_SIGNED_ON_NODE.lock().unwrap() = None;

    let channel = open_channel_with_retry(
        node1_addr,
        &node3_pubkey,
        Some(NODE3_PEER_PORT),
        Some(100_000),
        None,
        Some(ISSUE_AMT),
        Some(&asset_id),
        None,
        30,
    )
    .await;
    assert_eq!(asset_balance_spendable(node1_addr, &asset_id).await, 0);

    // node1's output of the closing transaction holds the whole amount; sweeping it requires
    // updating the closing witness, which must not fail because of the discarded funding witness
    close_channel(node1_addr, &channel.channel_id, &node3_pubkey, false).await;
    wait_for_balance(node1_addr, &asset_id, ISSUE_AMT).await;
}
