use super::*;

use crate::ldk::{
    FUNDING_CHECKPOINT_AFTER_COLOR, FUNDING_CHECKPOINT_BROADCASTING,
    FUNDING_CHECKPOINT_BROADCAST_COMMITTED, FUNDING_CHECKPOINT_BROADCAST_SAFE,
    FUNDING_CHECKPOINT_DURABLY_COMPLETED, FUNDING_CHECKPOINT_FINALIZED,
    FUNDING_CHECKPOINT_HANDED_TO_LDK, FUNDING_CHECKPOINT_HANDOFF_READY,
};

const TEST_DIR_BASE: &str = "tmp/funding_crash_sender/";

/// Real daemon subprocess so the test can SIGKILL it at a funding checkpoint
/// (in-process nodes cannot model an OS crash). A debug build is required: the
/// crash checkpoint is compiled out under `--release`.
struct DaemonProcess {
    child: std::process::Child,
    address: SocketAddr,
}

impl DaemonProcess {
    fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for DaemonProcess {
    fn drop(&mut self) {
        self.kill();
    }
}

fn daemon_binary() -> PathBuf {
    let target_dir = std::env::var("CARGO_TARGET_DIR")
        .unwrap_or_else(|_| format!("{}/target", env!("CARGO_MANIFEST_DIR")));
    let bin = PathBuf::from(target_dir)
        .join("debug")
        .join("rgb-lightning-node");
    assert!(
        bin.exists(),
        "daemon binary not found at {}; run `cargo build` first (a debug build is required)",
        bin.display()
    );
    bin
}

async fn start_daemon_process(
    node_test_dir: &str,
    peer_port: u16,
    envs: &[(&str, &str)],
) -> DaemonProcess {
    let daemon_port = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().port()
    };
    std::fs::create_dir_all(node_test_dir).unwrap();
    let log = std::fs::File::create(format!("{node_test_dir}/daemon.log")).unwrap();
    let mut cmd = std::process::Command::new(daemon_binary());
    cmd.arg(node_test_dir)
        .arg("--daemon-listening-port")
        .arg(daemon_port.to_string())
        .arg("--ldk-peer-listening-port")
        .arg(peer_port.to_string())
        .arg("--network")
        .arg("regtest")
        .arg("--disable-authentication")
        .stdout(std::process::Stdio::from(log.try_clone().unwrap()))
        .stderr(std::process::Stdio::from(log));
    for (key, value) in envs {
        cmd.env(key, value);
    }
    let child = cmd.spawn().expect("spawn daemon");
    let address: SocketAddr = format!("127.0.0.1:{daemon_port}").parse().unwrap();

    let t_0 = OffsetDateTime::now_utc();
    loop {
        if std::net::TcpStream::connect_timeout(&address, std::time::Duration::from_millis(200))
            .is_ok()
        {
            break;
        }
        if (OffsetDateTime::now_utc() - t_0).as_seconds_f32() > 30.0 {
            panic!("daemon did not come up on {address}");
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
    DaemonProcess { child, address }
}

/// Wait for the daemon to signal the checkpoint by writing the ready file.
/// Fails fast (rather than after the full timeout) if the daemon exits before
/// the checkpoint, so an unrelated crash reports its real cause.
async fn wait_for_checkpoint(daemon: &mut DaemonProcess, ready_path: &str, timeout_secs: f32) {
    let t_0 = OffsetDateTime::now_utc();
    loop {
        if Path::new(ready_path).exists() {
            return;
        }
        if let Some(status) = daemon.child.try_wait().expect("poll daemon status") {
            panic!("daemon exited ({status}) before reaching the funding checkpoint");
        }
        if (OffsetDateTime::now_utc() - t_0).as_seconds_f32() > timeout_secs {
            panic!("timeout waiting for the funding checkpoint at {ready_path}");
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
}

#[derive(Clone, Copy)]
enum RecoveryExpectation {
    RolledBack,
    Funded,
    EitherSafeOutcome,
}

struct CrashCase {
    checkpoint: &'static str,
    expectation: RecoveryExpectation,
}

async fn wait_for_recovered_balance_conservation(
    node_address: SocketAddr,
    asset_id: &str,
    initial_supply: u64,
    expected_offchain_outbound: u64,
) {
    let client = reqwest::Client::new();
    let payload = AssetBalanceRequest {
        asset_id: asset_id.to_owned(),
    };
    let started = OffsetDateTime::now_utc();
    loop {
        let response = client
            .post(format!("http://{node_address}/assetbalance"))
            .json(&payload)
            .send()
            .await
            .expect("request recovered asset balance");
        if response.status().is_success() {
            let balance = response
                .json::<AssetBalanceResponse>()
                .await
                .expect("decode recovered asset balance");
            if balance.offchain_outbound == expected_offchain_outbound
                && balance.future.checked_add(balance.offchain_outbound) == Some(initial_supply)
            {
                return;
            }

            let _ = client
                .post(format!("http://{node_address}/refreshtransfers"))
                .send()
                .await;
        } else {
            assert_eq!(
                response.status(),
                reqwest::StatusCode::FORBIDDEN,
                "unexpected balance response while funding recovery is active"
            );
        }
        assert!(
            (OffsetDateTime::now_utc() - started).as_seconds_f32() <= 120.0,
            "recovered RGB balances did not conserve the original supply"
        );
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
}

async fn assert_recovered_funding_state(
    node_address: SocketAddr,
    asset_id: &str,
    initial_spendable: u64,
    channel_amount: u64,
    funded_before: usize,
    expectation: RecoveryExpectation,
) -> usize {
    let channels = list_channels(node_address).await;
    let funded_after = channels
        .iter()
        .filter(|channel| channel.asset_id.as_deref() == Some(asset_id))
        .count();
    let expected_if_funded = funded_before + 1;

    match expectation {
        RecoveryExpectation::RolledBack => assert_eq!(
            funded_after, funded_before,
            "a pre-handoff crash must not restore a phantom RGB channel: {channels:?}"
        ),
        RecoveryExpectation::Funded => assert_eq!(
            funded_after, expected_if_funded,
            "a post-durable-handoff crash must restore exactly one RGB channel: {channels:?}"
        ),
        RecoveryExpectation::EitherSafeOutcome => assert!(
            funded_after == funded_before || funded_after == expected_if_funded,
            "the handoff boundary must either roll back or restore exactly one channel: {channels:?}"
        ),
    }

    wait_for_recovered_balance_conservation(
        node_address,
        asset_id,
        initial_spendable,
        channel_amount * funded_after as u64,
    )
    .await;
    for channel in channels
        .iter()
        .filter(|channel| channel.asset_id.as_deref() == Some(asset_id))
    {
        assert_eq!(channel.asset_local_amount, Some(channel_amount));
        assert_eq!(channel.asset_remote_amount, Some(0));
    }
    funded_after
}

async fn open_channel_after_recovery(
    node_address: SocketAddr,
    node2_pubkey: &str,
    asset_id: &str,
    channel_amount: u64,
) {
    let started = OffsetDateTime::now_utc();
    loop {
        match open_channel_request_raw(
            node_address,
            node2_pubkey,
            Some(NODE2_PEER_PORT),
            None,
            None,
            Some(channel_amount),
            Some(asset_id),
            None,
            None,
            None,
            None,
            true,
            true,
        )
        .await
        {
            Ok(_) => return,
            Err(response) => {
                let status = response.status();
                let body = response
                    .text()
                    .await
                    .unwrap_or_else(|error| format!("cannot read response body: {error}"));
                if status == reqwest::StatusCode::FORBIDDEN
                    && body.contains("\"name\":\"ChangingState\"")
                {
                    assert!(
                        (OffsetDateTime::now_utc() - started).as_seconds_f32() <= 30.0,
                        "funding recovery did not release financial-operation admission"
                    );
                    tokio::time::sleep(std::time::Duration::from_millis(250)).await;
                    continue;
                }
                panic!("unexpected open-channel response after funding recovery: {status} {body}");
            }
        }
    }
}

/// Exercise every sender funding persistence boundary in real daemon processes. Each child is
/// SIGKILLed immediately after the selected journal checkpoint, then restarted over the same data
/// directory. Recovery must preserve exact asset conservation and either roll back a pre-durable
/// handoff or resume/finalize the exact transaction once the LDK channel is durable.
#[serial_test::serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sender_funding_crash_matrix_preserves_assets_and_channels() {
    initialize();

    let test_dir_node1 = format!("{TEST_DIR_BASE}sender_node1");
    let test_dir_node2 = format!("{TEST_DIR_BASE}sender_node2");
    let ready_path = format!("{TEST_DIR_BASE}sender_kill_ready");
    let _ = std::fs::remove_dir_all(&test_dir_node1);
    let _ = std::fs::remove_dir_all(&test_dir_node2);
    let _ = std::fs::remove_file(&ready_path);

    let password = "funding_crash_sender";
    let mut node1 = start_daemon_process(&test_dir_node1, NODE1_PEER_PORT, &[]).await;
    init(node1.address, password, None).await;
    unlock(node1.address, password).await;

    let (node2_addr, _) = start_node(&test_dir_node2, NODE2_PEER_PORT, false).await;

    fund_and_create_utxos(node1.address, None).await;
    fund_and_create_utxos(node2_addr, None).await;

    let channel_amount = 100;
    let asset = issue_asset_nia_with_amounts(node1.address, vec![channel_amount; 10]).await;
    create_utxos(node1.address, false, Some(20), Some(32_000)).await;
    let initial_spendable = asset_balance_spendable(node1.address, &asset.asset_id).await;
    assert!(
        initial_spendable > 0,
        "test setup must start with spendable RGB assets"
    );
    let node2_pubkey = node_info(node2_addr).await.pubkey;
    let cases = [
        CrashCase {
            checkpoint: FUNDING_CHECKPOINT_AFTER_COLOR,
            expectation: RecoveryExpectation::RolledBack,
        },
        CrashCase {
            checkpoint: FUNDING_CHECKPOINT_HANDOFF_READY,
            expectation: RecoveryExpectation::RolledBack,
        },
        CrashCase {
            checkpoint: FUNDING_CHECKPOINT_HANDED_TO_LDK,
            expectation: RecoveryExpectation::EitherSafeOutcome,
        },
        CrashCase {
            checkpoint: FUNDING_CHECKPOINT_BROADCAST_SAFE,
            expectation: RecoveryExpectation::Funded,
        },
        CrashCase {
            checkpoint: FUNDING_CHECKPOINT_BROADCASTING,
            expectation: RecoveryExpectation::Funded,
        },
        CrashCase {
            checkpoint: FUNDING_CHECKPOINT_BROADCAST_COMMITTED,
            expectation: RecoveryExpectation::Funded,
        },
        CrashCase {
            checkpoint: FUNDING_CHECKPOINT_FINALIZED,
            expectation: RecoveryExpectation::Funded,
        },
        CrashCase {
            checkpoint: FUNDING_CHECKPOINT_DURABLY_COMPLETED,
            expectation: RecoveryExpectation::Funded,
        },
    ];
    let mut funded_channels = 0;

    for case in cases {
        node1.kill();
        let _ = std::fs::remove_file(&ready_path);
        node1 = start_daemon_process(
            &test_dir_node1,
            NODE1_PEER_PORT,
            &[
                ("RLN_FUNDING_KILL_AT", case.checkpoint),
                ("RLN_FUNDING_KILL_READY_PATH", &ready_path),
            ],
        )
        .await;
        unlock(node1.address, password).await;

        open_channel_after_recovery(
            node1.address,
            &node2_pubkey,
            &asset.asset_id,
            channel_amount,
        )
        .await;

        wait_for_checkpoint(&mut node1, &ready_path, 120.0).await;
        assert_eq!(
            std::fs::read_to_string(&ready_path).expect("read ready file"),
            case.checkpoint,
            "the crash must fire at the requested funding boundary"
        );
        node1.kill();

        node1 = start_daemon_process(&test_dir_node1, NODE1_PEER_PORT, &[]).await;
        unlock(node1.address, password).await;
        funded_channels = assert_recovered_funding_state(
            node1.address,
            &asset.asset_id,
            initial_spendable,
            channel_amount,
            funded_channels,
            case.expectation,
        )
        .await;
        if funded_channels > 0 {
            mine_n_blocks(false, 6);
            wait_for_usable_channels(node1.address, funded_channels).await;
            wait_for_usable_channels(node2_addr, funded_channels).await;
        }
    }

    shutdown(&[node1.address, node2_addr]).await;
}
