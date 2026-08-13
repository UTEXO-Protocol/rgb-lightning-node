use super::*;

use std::collections::BTreeMap;
use std::net::{SocketAddrV4, TcpListener as StdTcpListener};
use std::process::Child;

const TEST_DIR_BASE: &str = "tmp/funding_crash_recovery";
const CHILD_MODE_ENV: &str = "RLN_TEST_DAEMON_CHILD";
const CHILD_STORAGE_ENV: &str = "RLN_TEST_DAEMON_STORAGE";
const CHILD_DAEMON_PORT_ENV: &str = "RLN_TEST_DAEMON_PORT";
const CHILD_PEER_PORT_ENV: &str = "RLN_TEST_DAEMON_PEER_PORT";
const CHECKPOINT_ENV: &str = "RLN_TEST_RGB_FUNDING_PROMOTED_CHECKPOINT";

#[serial_test::serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[traced_test]
async fn promoted_funding_crash_recovers_without_untracked_rgb_state() {
    initialize();

    let test_dir_node1 = format!("{TEST_DIR_BASE}/node1");
    let test_dir_node2 = format!("{TEST_DIR_BASE}/node2");
    if Path::new(&test_dir_node2).is_dir() {
        std::fs::remove_dir_all(&test_dir_node2).unwrap();
    }
    let (node1_addr, _) = start_node(&test_dir_node1, NODE1_PEER_PORT, false).await;
    let node2_addr = child_daemon_addr();

    let checkpoint_listener = StdTcpListener::bind("127.0.0.1:0").unwrap();
    let checkpoint_addr = checkpoint_listener.local_addr().unwrap().to_string();
    let checkpoint = tokio::task::spawn_blocking(move || {
        let (stream, _) = checkpoint_listener.accept().unwrap();
        let mut line = String::new();
        BufReader::new(stream.try_clone().unwrap())
            .read_line(&mut line)
            .unwrap();
        (line, stream)
    });

    let mut node2_child = spawn_child_daemon(&test_dir_node2, NODE2_PEER_PORT, checkpoint_addr);
    wait_for_api(node2_addr).await;

    let node2_password = format!("{test_dir_node2}.{NODE2_PEER_PORT}");
    init(node2_addr, &node2_password, None).await;
    unlock(node2_addr, &node2_password).await;

    fund_and_create_utxos(node1_addr, None).await;
    fund_and_create_utxos(node2_addr, None).await;

    let asset_id = issue_asset_nia(node1_addr).await.asset_id;
    let node1_pubkey = node_info(node1_addr).await.pubkey;
    let node2_pubkey = node_info(node2_addr).await.pubkey;
    let receiver_rgb_before = rgb_stock_snapshot(&test_dir_node2);

    let open_node2_pubkey = node2_pubkey.clone();
    let open_asset_id = asset_id.clone();
    let open_channel_task = tokio::spawn(async move {
        open_channel_raw(
            node1_addr,
            &open_node2_pubkey,
            Some(NODE2_PEER_PORT),
            Some(100_000),
            None,
            Some(600),
            Some(&open_asset_id),
            Some(250),
            None,
            None,
            None,
            true,
            true,
            None,
        )
        .await
    });

    let (checkpoint_line, _checkpoint_stream) =
        tokio::time::timeout(std::time::Duration::from_secs(30), checkpoint)
            .await
            .expect("receiver should report the post-promotion checkpoint")
            .expect("checkpoint task should complete");
    assert!(
        checkpoint_line.contains(' '),
        "checkpoint should include temporary channel id and funding txid"
    );

    node2_child.kill().expect("receiver child should be killed");
    let _ = node2_child.wait();
    open_channel_task.abort();
    let _ = open_channel_task.await;

    let mut node2_child = spawn_child_daemon(&test_dir_node2, NODE2_PEER_PORT, String::new());
    wait_for_api(node2_addr).await;
    unlock(node2_addr, &node2_password).await;

    connect_peer(
        node2_addr,
        &node1_pubkey,
        &format!("127.0.0.1:{NODE1_PEER_PORT}"),
    )
    .await;
    let (node1_channels, node2_channels) =
        wait_for_peer_channel_convergence(node1_addr, node2_addr, &asset_id).await;
    let receiver_has_unmatched_rgb_channel =
        has_unmatched_receiver_rgb_channel(&node1_channels, &node2_channels, &asset_id);
    let receiver_has_matched_rgb_channel =
        has_matched_receiver_rgb_channel(&node1_channels, &node2_channels, &asset_id);
    let accepted_balance = asset_balance_optional(node2_addr, &asset_id).await;
    let has_accepted_rgb_state = accepted_balance.as_ref().is_some_and(|balance| {
        balance.settled
            + balance.future
            + balance.spendable
            + balance.offchain_outbound
            + balance.offchain_inbound
            > 0
    });
    let receiver_rgb_after = rgb_stock_snapshot(&test_dir_node2);
    let receiver_stock_changed = receiver_rgb_before != receiver_rgb_after;

    shutdown(&[node1_addr, node2_addr]).await;
    let _ = node2_child.wait();

    assert!(
        !receiver_has_unmatched_rgb_channel
            && (receiver_has_matched_rgb_channel
                || (!has_accepted_rgb_state && !receiver_stock_changed)),
        "crash recovery left the receiver with unmatched RGB funding state: node1_channels={node1_channels:?}, node2_channels={node2_channels:?}, balance={}, receiver_stock_changed={receiver_stock_changed}",
        format_balance(accepted_balance.as_ref())
    );
}

fn rgb_stock_snapshot(storage_dir: &str) -> BTreeMap<String, Vec<u8>> {
    let mut snapshot = BTreeMap::new();
    let entries = std::fs::read_dir(storage_dir).expect("receiver storage must be readable");
    for entry in entries {
        let entry = entry.expect("receiver storage entry must be readable");
        if !entry
            .file_type()
            .expect("receiver entry type must be readable")
            .is_dir()
            || entry.file_name() == ".ldk"
        {
            continue;
        }
        let rgb_dir = entry.path().join("rgb");
        if !rgb_dir.is_dir() {
            continue;
        }
        for file in std::fs::read_dir(&rgb_dir).expect("RGB stock directory must be readable") {
            let file = file.expect("RGB stock entry must be readable");
            if file
                .file_type()
                .expect("RGB stock entry type must be readable")
                .is_file()
            {
                let name = file.file_name().to_string_lossy().into_owned();
                let bytes = std::fs::read(file.path()).expect("RGB stock file must be readable");
                snapshot.insert(name, bytes);
            }
        }
    }
    assert!(
        !snapshot.is_empty(),
        "receiver RGB stock must exist before funding"
    );
    snapshot
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn daemon_child_process() {
    if std::env::var(CHILD_MODE_ENV).is_err() {
        return;
    }

    let storage_dir_path = PathBuf::from(std::env::var(CHILD_STORAGE_ENV).unwrap());
    let daemon_port = std::env::var(CHILD_DAEMON_PORT_ENV)
        .unwrap()
        .parse::<u16>()
        .unwrap();
    let peer_port = std::env::var(CHILD_PEER_PORT_ENV)
        .unwrap()
        .parse::<u16>()
        .unwrap();

    let args = UserArgs {
        storage_dir_path,
        daemon_listening_port: daemon_port,
        ldk_peer_listening_port: peer_port,
        ..Default::default()
    };
    let (router, app_state) = app(args).await.unwrap();
    let listener = tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], daemon_port)))
        .await
        .unwrap();
    axum::serve(listener, router)
        .with_graceful_shutdown(shutdown_signal(app_state))
        .await
        .unwrap();
}

async fn asset_balance_optional(
    node_address: SocketAddr,
    asset_id: &str,
) -> Option<AssetBalanceResponse> {
    let payload = AssetBalanceRequest {
        asset_id: asset_id.to_string(),
    };
    let res = reqwest::Client::new()
        .post(format!("http://{node_address}/assetbalance"))
        .json(&payload)
        .send()
        .await
        .unwrap();
    if res.status() != reqwest::StatusCode::OK {
        return None;
    }
    Some(res.json::<AssetBalanceResponse>().await.unwrap())
}

fn child_daemon_addr() -> SocketAddr {
    SocketAddr::V4(SocketAddrV4::new([127, 0, 0, 1].into(), 31_202))
}

fn format_balance(balance: Option<&AssetBalanceResponse>) -> String {
    match balance {
        None => "not imported".to_string(),
        Some(balance) => format!(
            "settled={}, future={}, spendable={}, offchain_outbound={}, offchain_inbound={}",
            balance.settled,
            balance.future,
            balance.spendable,
            balance.offchain_outbound,
            balance.offchain_inbound
        ),
    }
}

async fn wait_for_peer_channel_convergence(
    node1_addr: SocketAddr,
    node2_addr: SocketAddr,
    asset_id: &str,
) -> (Vec<Channel>, Vec<Channel>) {
    let started_at = OffsetDateTime::now_utc();
    loop {
        let node1_channels = list_channels(node1_addr).await;
        let node2_channels = list_channels(node2_addr).await;
        if !has_unmatched_receiver_rgb_channel(&node1_channels, &node2_channels, asset_id) {
            return (node1_channels, node2_channels);
        }
        if (OffsetDateTime::now_utc() - started_at).as_seconds_f32() > 10.0 {
            return (node1_channels, node2_channels);
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
}

fn has_unmatched_receiver_rgb_channel(
    node1_channels: &[Channel],
    node2_channels: &[Channel],
    asset_id: &str,
) -> bool {
    node2_channels
        .iter()
        .filter(|channel| channel.asset_id.as_deref() == Some(asset_id))
        .any(|receiver_channel| {
            !node1_channels.iter().any(|opener_channel| {
                opener_channel.asset_id.as_deref() == Some(asset_id)
                    && opener_channel.channel_id == receiver_channel.channel_id
            })
        })
}

fn has_matched_receiver_rgb_channel(
    node1_channels: &[Channel],
    node2_channels: &[Channel],
    asset_id: &str,
) -> bool {
    node2_channels
        .iter()
        .filter(|channel| channel.asset_id.as_deref() == Some(asset_id))
        .any(|receiver_channel| {
            node1_channels.iter().any(|opener_channel| {
                opener_channel.asset_id.as_deref() == Some(asset_id)
                    && opener_channel.channel_id == receiver_channel.channel_id
            })
        })
}

fn spawn_child_daemon(storage_dir: &str, peer_port: u16, checkpoint_addr: String) -> Child {
    let exe = std::env::current_exe().unwrap();
    let mut command = Command::new(exe);
    command
        .arg("--exact")
        .arg("test::funding_crash_recovery::daemon_child_process")
        .arg("--nocapture")
        .env(CHILD_MODE_ENV, "1")
        .env(CHILD_STORAGE_ENV, storage_dir)
        .env(
            CHILD_DAEMON_PORT_ENV,
            child_daemon_addr().port().to_string(),
        )
        .env(CHILD_PEER_PORT_ENV, peer_port.to_string())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    if !checkpoint_addr.is_empty() {
        command.env(CHECKPOINT_ENV, checkpoint_addr);
    }
    command.spawn().expect("child daemon should spawn")
}

async fn wait_for_api(node_address: SocketAddr) {
    let started_at = OffsetDateTime::now_utc();
    loop {
        if tokio::net::TcpStream::connect(node_address).await.is_ok() {
            return;
        }
        if (OffsetDateTime::now_utc() - started_at).as_seconds_f32() > 20.0 {
            panic!("child daemon did not bind {node_address}");
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
}
