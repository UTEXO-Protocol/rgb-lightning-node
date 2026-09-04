use super::*;

use std::collections::BTreeMap;
use std::net::{SocketAddrV4, TcpListener as StdTcpListener};
use std::process::Child;

const TEST_DIR_BASE: &str = "tmp/funding_crash_recovery";
const CHILD_MODE_ENV: &str = "RLN_TEST_DAEMON_CHILD";
const CHILD_STORAGE_ENV: &str = "RLN_TEST_DAEMON_STORAGE";
const CHILD_DAEMON_PORT_ENV: &str = "RLN_TEST_DAEMON_PORT";
const CHILD_PEER_PORT_ENV: &str = "RLN_TEST_DAEMON_PEER_PORT";
const PREPARED_CHECKPOINT_ENV: &str = "RLN_TEST_RGB_FUNDING_PREPARED_CHECKPOINT";
const PROMOTED_CHECKPOINT_ENV: &str = "RLN_TEST_RGB_FUNDING_PROMOTED_CHECKPOINT";

#[serial_test::serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[traced_test]
async fn promoted_funding_crash_is_quarantined_without_mutating_stock() {
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

    let mut node2_child = spawn_child_daemon(
        &test_dir_node2,
        NODE2_PEER_PORT,
        Some((PROMOTED_CHECKPOINT_ENV, checkpoint_addr)),
    );
    wait_for_api(node2_addr).await;

    let node2_password = format!("{test_dir_node2}.{NODE2_PEER_PORT}");
    init(node2_addr, &node2_password, None).await;
    unlock(node2_addr, &node2_password).await;

    fund_and_create_utxos(node1_addr, None).await;
    fund_and_create_utxos(node2_addr, None).await;

    let asset_id = issue_asset_nia(node1_addr).await.asset_id;
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

    let receiver_rgb_at_crash = rgb_stock_snapshot(&test_dir_node2);
    assert_ne!(
        receiver_rgb_before, receiver_rgb_at_crash,
        "the promoted checkpoint must expose the accepted RGB stock"
    );

    let mut node2_child = spawn_child_daemon(&test_dir_node2, NODE2_PEER_PORT, None);
    wait_for_api(node2_addr).await;
    unlock(node2_addr, &node2_password).await;

    let records = receiver_funding_records(&test_dir_node2);
    assert_eq!(records.len(), 1, "the crash journal must remain durable");
    assert_eq!(records[0].stage, FundingAcceptanceStage::Promoted);

    let create_utxos = reqwest::Client::new()
        .post(format!("http://{node2_addr}/createutxos"))
        .json(&CreateUtxosRequest {
            up_to: true,
            num: Some(1),
            size: Some(1_000),
            fee_rate: FEE_RATE,
            skip_sync: true,
        })
        .send()
        .await
        .unwrap();
    check_response_is_nok(
        create_utxos,
        reqwest::StatusCode::FORBIDDEN,
        "RGB funding recovery is required",
        "RgbFundingRecoveryRequired",
    )
    .await;

    let receiver_rgb_after = rgb_stock_snapshot(&test_dir_node2);

    shutdown(&[node1_addr, node2_addr]).await;
    let _ = node2_child.wait();

    assert_eq!(
        receiver_rgb_at_crash, receiver_rgb_after,
        "ambiguous promoted-state recovery must not mutate the RGB stock"
    );
}

#[serial_test::serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[traced_test]
async fn prepared_funding_crash_rolls_back_on_restart() {
    initialize();

    let test_dir_node1 = format!("{TEST_DIR_BASE}_prepared/node1");
    let test_dir_node2 = format!("{TEST_DIR_BASE}_prepared/node2");
    for test_dir in [&test_dir_node1, &test_dir_node2] {
        if Path::new(test_dir).is_dir() {
            std::fs::remove_dir_all(test_dir).unwrap();
        }
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

    let mut node2_child = spawn_child_daemon(
        &test_dir_node2,
        NODE2_PEER_PORT,
        Some((PREPARED_CHECKPOINT_ENV, checkpoint_addr)),
    );
    wait_for_api(node2_addr).await;

    let node2_password = format!("{test_dir_node2}.{NODE2_PEER_PORT}");
    init(node2_addr, &node2_password, None).await;
    unlock(node2_addr, &node2_password).await;

    fund_and_create_utxos(node1_addr, None).await;
    fund_and_create_utxos(node2_addr, None).await;

    let asset_id = issue_asset_nia(node1_addr).await.asset_id;
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
            .expect("receiver should report the post-preparation checkpoint")
            .expect("checkpoint task should complete");
    assert!(
        checkpoint_line.contains(' '),
        "checkpoint should include temporary channel id and funding txid"
    );

    node2_child.kill().expect("receiver child should be killed");
    let _ = node2_child.wait();
    open_channel_task.abort();
    let _ = open_channel_task.await;

    let mut node2_child = spawn_child_daemon(&test_dir_node2, NODE2_PEER_PORT, None);
    wait_for_api(node2_addr).await;
    unlock(node2_addr, &node2_password).await;

    assert!(
        receiver_funding_records(&test_dir_node2).is_empty(),
        "startup reconciliation must remove the rolled-back prepared journal"
    );
    assert!(
        list_channels(node2_addr).await.is_empty(),
        "a channel must not survive a crash before RGB stock promotion"
    );

    shutdown(&[node1_addr, node2_addr]).await;
    let _ = node2_child.wait();

    assert_eq!(
        receiver_rgb_before,
        rgb_stock_snapshot(&test_dir_node2),
        "prepared-state recovery must restore the receiver's exact pre-funding RGB stock"
    );
}

fn receiver_funding_records(storage_dir: &str) -> Vec<PendingFundingAcceptance> {
    let db_path = get_db_path(Path::new(storage_dir));
    let connection_string = format!("sqlite:{}?mode=rw", db_path.display());
    let mut options = ConnectOptions::new(connection_string);
    options.max_connections(1);
    let database = crate::runtime::block_on(Database::connect(options))
        .expect("connect to receiver recovery database");
    let kv_store = SeaOrmKvStore::from_connection(Arc::new(database));
    kv_store
        .list(RGB_PRIMARY_NS, RGB_FUNDING_ACCEPTANCE_NS)
        .expect("list receiver funding journals")
        .into_iter()
        .map(|key| {
            read_pending_funding_acceptance(&key, &kv_store).expect("read receiver funding journal")
        })
        .collect()
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

fn child_daemon_addr() -> SocketAddr {
    SocketAddr::V4(SocketAddrV4::new([127, 0, 0, 1].into(), 31_202))
}

fn spawn_child_daemon(
    storage_dir: &str,
    peer_port: u16,
    checkpoint: Option<(&'static str, String)>,
) -> Child {
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
    if let Some((checkpoint_env, checkpoint_addr)) = checkpoint {
        command.env(checkpoint_env, checkpoint_addr);
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
