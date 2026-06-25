use super::*;

const TEST_DIR_BASE: &str = "tmp/remote_first_recovery/";
const VSS_URL: &str = "http://localhost:8081/vss";

/// End-to-end recovery-from-seed: a node with VSS issues an asset, opens a
/// colored channel and sends a payment; its storage dir is wiped; it restarts
/// with the same seed + VSS URL and must recover asset balances, the channel
/// and the RGB stash from VSS alone.
// Mirror the node's multi-threaded runtime (`#[tokio::main]`); synchronous VSS
// calls need more than one worker thread.
#[cfg(feature = "vss")]
#[serial_test::serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[traced_test]
async fn remote_first_recovery() {
    // Bound the run so a hang surfaces as a failure rather than blocking forever.
    tokio::time::timeout(
        std::time::Duration::from_secs(240),
        remote_first_recovery_inner(),
    )
    .await
    .expect("remote_first_recovery timed out");
}

#[cfg(feature = "vss")]
async fn remote_first_recovery_inner() {
    initialize();

    let test_dir_node1 = format!("{TEST_DIR_BASE}node1");
    let test_dir_node2 = format!("{TEST_DIR_BASE}node2");

    // Fresh per-run seeds (returned by init) so VSS state never leaks between
    // runs; node1's seed is reused on the post-wipe restart.
    let (node1_addr, _, mnemonic_1) = start_node_with_vss(
        &test_dir_node1,
        NODE1_PEER_PORT,
        false,
        VSS_URL,
        None,
        false,
    )
    .await;
    let (node2_addr, _, _) = start_node_with_vss(
        &test_dir_node2,
        NODE2_PEER_PORT,
        false,
        VSS_URL,
        None,
        false,
    )
    .await;

    fund_and_create_utxos(node1_addr, None).await;
    fund_and_create_utxos(node2_addr, None).await;

    let asset_id = issue_asset_nia(node1_addr).await.asset_id;
    assert_eq!(
        asset_balance_spendable(node1_addr, &asset_id).await,
        ISSUE_AMT
    );

    // Blocking RGB backup: the issue op must not return until its VSS backup is
    // durable, so the backup exists and is not stale once the call completes.
    let info = vss_backup_info(node1_addr).await;
    assert_eq!(
        info["backup_exists"],
        serde_json::json!(true),
        "rgb backup must exist after issue"
    );
    assert_eq!(
        info["backup_required"],
        serde_json::json!(false),
        "rgb backup must be current (not lagging) after a blocking-backup op"
    );

    let node2_pubkey = node_info(node2_addr).await.pubkey;
    connect_peer(
        node1_addr,
        &node2_pubkey,
        &format!("127.0.0.1:{NODE2_PEER_PORT}"),
    )
    .await;

    let channel = open_channel(
        node1_addr,
        &node2_pubkey,
        Some(NODE2_PEER_PORT),
        None,
        None,
        Some(600),
        Some(&asset_id),
    )
    .await;
    assert_eq!(asset_balance_spendable(node1_addr, &asset_id).await, 400);

    let channel_id = channel.channel_id.clone();
    keysend(node1_addr, &node2_pubkey, None, Some(&asset_id), Some(100)).await;
    assert_eq!(
        channel_rgb_amounts(node1_addr, &channel_id).await,
        (Some(500), Some(100)),
        "keysend must move 100 of the asset over the channel"
    );

    let pre_wipe_balance = asset_balance_spendable(node1_addr, &asset_id).await;

    wipe_node_dir(node1_addr, &test_dir_node1).await;

    // Restart from the same seed + VSS URL; recovery must come from VSS alone.
    let (node1_addr, _, _) = start_node_with_vss(
        &test_dir_node1,
        NODE1_PEER_PORT,
        true,
        VSS_URL,
        Some(&mnemonic_1),
        false,
    )
    .await;

    // On-chain asset balance recovered.
    assert_eq!(
        asset_balance_spendable(node1_addr, &asset_id).await,
        pre_wipe_balance,
        "asset balance must match pre-wipe value after recovery"
    );

    // Issued asset listed again and its stash file is present.
    assert!(
        list_assets(node1_addr)
            .await
            .nia
            .unwrap_or_default()
            .iter()
            .any(|a| a.asset_id == asset_id),
        "issued asset must be listed after recovery"
    );
    assert!(
        glob_stash_dat(&test_dir_node1),
        "rgb/stash.dat must exist after recovery"
    );

    // Channel recovered with its post-keysend balance: the latest state, not a
    // stale snapshot.
    assert_eq!(
        channel_rgb_amounts(node1_addr, &channel_id).await,
        (Some(500), Some(100)),
        "recovered channel must reflect the keysend made before the wipe"
    );

    // The recovered channel and RGB wallet are operational: reconnect and route a
    // fresh payment over the same channel, then check the balance moved.
    connect_peer(
        node1_addr,
        &node2_pubkey,
        &format!("127.0.0.1:{NODE2_PEER_PORT}"),
    )
    .await;
    wait_for_channel_usable(node1_addr, &channel_id).await;
    keysend(node1_addr, &node2_pubkey, None, Some(&asset_id), Some(50)).await;
    assert_eq!(
        channel_rgb_amounts(node1_addr, &channel_id).await,
        (Some(450), Some(150)),
        "a payment over the recovered channel must move the asset"
    );
}

#[cfg(feature = "vss")]
async fn channel_rgb_amounts(
    node_address: SocketAddr,
    channel_id: &str,
) -> (Option<u64>, Option<u64>) {
    let channel = list_channels(node_address)
        .await
        .into_iter()
        .find(|c| c.channel_id == channel_id)
        .unwrap_or_else(|| panic!("channel {channel_id} not found on {node_address}"));
    (channel.asset_local_amount, channel.asset_remote_amount)
}

#[cfg(feature = "vss")]
async fn wait_for_channel_usable(node_address: SocketAddr, channel_id: &str) {
    let t_0 = OffsetDateTime::now_utc();
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        if list_channels(node_address)
            .await
            .iter()
            .any(|c| c.channel_id == channel_id && c.ready && c.is_usable)
        {
            return;
        }
        if (OffsetDateTime::now_utc() - t_0).as_seconds_f32() > 30.0 {
            panic!("channel {channel_id} did not become usable on {node_address}");
        }
    }
}

#[cfg(feature = "vss")]
fn glob_stash_dat(node_test_dir: &str) -> bool {
    let Ok(entries) = std::fs::read_dir(node_test_dir) else {
        return false;
    };
    for entry in entries.flatten() {
        let stash = entry.path().join("rgb").join("stash.dat");
        if stash.is_file() {
            return true;
        }
    }
    false
}
