//! A restore whose channel manager lags the (remote-first) channel monitors
//! must refuse to unlock or keep the channel — never silently force-close.
//! Requires the regtest stack (`./regtest.sh start`) and the VSS server
//! (`docker compose --profile vss up -d`).

use crate::helpers::*;
use serial_test::serial;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::{fs, time::Duration};

const VSS_SERVER_ADDR: &str = "127.0.0.1:8081";
const NODE_A_PORT_OFFSET: u16 = 110;
const NODE_B_PORT_OFFSET: u16 = 110;
const PASSWORD_A: &str = "nodeApass";
const PASSWORD_B: &str = "nodeBpass";
const MANAGER_VSS_KEY: &[u8] = b"_/_/manager";

pub(crate) fn vss_server_available() -> bool {
    std::net::TcpStream::connect_timeout(&VSS_SERVER_ADDR.parse().unwrap(), Duration::from_secs(2))
        .is_ok()
}

/// VSS proxy that can reject channel-manager-key and/or RGB-backup writes,
/// passing all else through.
pub(crate) struct ManagerFilterProxy {
    port: u16,
    filter_manager: Arc<AtomicBool>,
    filter_rgb_backup: Arc<AtomicBool>,
    blocked: Arc<std::sync::atomic::AtomicUsize>,
}

impl ManagerFilterProxy {
    pub(crate) fn start() -> Self {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let filter_manager = Arc::new(AtomicBool::new(false));
        let filter_rgb_backup = Arc::new(AtomicBool::new(false));
        let blocked = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let manager_flag = Arc::clone(&filter_manager);
        let rgb_flag = Arc::clone(&filter_rgb_backup);
        let hits = Arc::clone(&blocked);
        std::thread::spawn(move || {
            for conn in listener.incoming() {
                let Ok(stream) = conn else { break };
                let manager_flag = Arc::clone(&manager_flag);
                let rgb_flag = Arc::clone(&rgb_flag);
                let hits = Arc::clone(&hits);
                std::thread::spawn(move || {
                    let _ = handle_conn(stream, manager_flag, rgb_flag, hits);
                });
            }
        });
        Self {
            port,
            filter_manager,
            filter_rgb_backup,
            blocked,
        }
    }

    fn blocked_count(&self) -> usize {
        self.blocked.load(Ordering::SeqCst)
    }

    pub(crate) fn url(&self) -> String {
        format!("http://127.0.0.1:{}/vss", self.port)
    }

    pub(crate) fn block_manager_writes(&self) {
        self.filter_manager.store(true, Ordering::SeqCst);
    }

    pub(crate) fn block_rgb_backup_writes(&self) {
        self.filter_rgb_backup.store(true, Ordering::SeqCst);
    }

    pub(crate) fn allow_all(&self) {
        self.filter_manager.store(false, Ordering::SeqCst);
        self.filter_rgb_backup.store(false, Ordering::SeqCst);
    }
}

/// Reads one HTTP/1.1 request; `None` on a clean close between requests.
fn read_http_request(
    stream: &mut std::net::TcpStream,
) -> std::io::Result<Option<(Vec<u8>, Vec<u8>)>> {
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        if stream.read(&mut byte)? == 0 {
            return Ok(None);
        }
        head.push(byte[0]);
        if head.ends_with(b"\r\n\r\n") {
            break;
        }
        if head.len() > 64 * 1024 {
            return Ok(None);
        }
    }
    let head_str = String::from_utf8_lossy(&head).to_string();
    let content_length = head_str
        .lines()
        .find_map(|l| {
            let (name, value) = l.split_once(':')?;
            if name.eq_ignore_ascii_case("content-length") {
                value.trim().parse::<usize>().ok()
            } else {
                None
            }
        })
        .unwrap_or(0);
    let mut body = vec![0u8; content_length];
    stream.read_exact(&mut body)?;
    Ok(Some((head, body)))
}

fn body_contains(body: &[u8], needle: &[u8]) -> bool {
    body.windows(needle.len()).any(|w| w == needle)
}

fn handle_conn(
    mut client: std::net::TcpStream,
    filter_manager: Arc<AtomicBool>,
    filter_rgb_backup: Arc<AtomicBool>,
    blocked: Arc<std::sync::atomic::AtomicUsize>,
) -> std::io::Result<()> {
    while let Some((head, body)) = read_http_request(&mut client)? {
        let head_str = String::from_utf8_lossy(&head).to_string();
        let is_put = head_str
            .lines()
            .next()
            .is_some_and(|l| l.contains("putObject"));
        let is_blocked = is_put
            && ((filter_manager.load(Ordering::SeqCst) && body_contains(&body, MANAGER_VSS_KEY))
                || (filter_rgb_backup.load(Ordering::SeqCst) && body_contains(&body, b"backup/")));
        if is_blocked {
            blocked.fetch_add(1, Ordering::SeqCst);
            client.write_all(
                b"HTTP/1.1 502 Bad Gateway\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
            )?;
            return Ok(());
        }
        // Fresh upstream connection with `Connection: close`: response is EOF-delimited.
        let mut upstream = std::net::TcpStream::connect(VSS_SERVER_ADDR)?;
        let mut new_head = String::new();
        for line in head_str.split("\r\n") {
            if line.is_empty() {
                continue;
            }
            if let Some((name, _)) = line.split_once(':') {
                if name.eq_ignore_ascii_case("connection") {
                    continue;
                }
            }
            new_head.push_str(line);
            new_head.push_str("\r\n");
        }
        new_head.push_str("connection: close\r\n\r\n");
        upstream.write_all(new_head.as_bytes())?;
        upstream.write_all(&body)?;
        std::io::copy(&mut upstream, &mut client)?;
    }
    Ok(())
}

/// Recursively searches a node dir for LDK `logs.txt` files containing `needle`.
fn node_ldk_log_contains(dir: &std::path::Path, needle: &str) -> bool {
    let Ok(entries) = fs::read_dir(dir) else {
        return false;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if node_ldk_log_contains(&path, needle) {
                return true;
            }
        } else if path.file_name().is_some_and(|n| n == "logs.txt")
            && fs::read_to_string(&path).is_ok_and(|c| c.contains(needle))
        {
            return true;
        }
    }
    false
}

struct LagSetup {
    proxy: ManagerFilterProxy,
    node_a: SdkNode,
    node_b: SdkNode,
    node_a_dir: std::path::PathBuf,
    mnemonic_a: String,
    node_b_pubkey: bitcoin::secp256k1::PublicKey,
    peer_uri: String,
    channel_id: lightning::ln::types::ChannelId,
}

/// Init both nodes and open an RGB channel with full VSS replication.
fn setup_with_open_channel(test_name: &str) -> LagSetup {
    let test_dir = test_dir(test_name);
    if test_dir.exists() {
        fs::remove_dir_all(&test_dir).expect("clean previous test dir");
    }
    fs::create_dir_all(&test_dir).expect("create test dir");
    let node_a_dir = test_dir.join("node_a");
    let node_b_dir = test_dir.join("node_b");

    let proxy = ManagerFilterProxy::start();

    let node_a = make_node_with_vss(
        &node_a_dir,
        NODE_A_DAEMON_PORT + NODE_A_PORT_OFFSET,
        NODE_A_PEER_PORT + NODE_A_PORT_OFFSET,
        &proxy.url(),
    );
    let node_b = make_node(
        &node_b_dir,
        NODE_B_DAEMON_PORT + NODE_B_PORT_OFFSET,
        NODE_B_PEER_PORT + NODE_B_PORT_OFFSET,
    );

    let mnemonic_a = node_a
        .init(PASSWORD_A.to_string(), None)
        .expect("node A init");
    node_b
        .init(PASSWORD_B.to_string(), None)
        .expect("node B init");
    node_a
        .unlock(unlock_request(PASSWORD_A))
        .expect("node A initial unlock");
    node_b
        .unlock(unlock_request(PASSWORD_B))
        .expect("node B initial unlock");

    fund_and_create_utxos(&node_a, "node A");
    fund_and_create_utxos(&node_b, "node B");

    let asset = node_a
        .issueassetnia(SdkIssueAssetNiaRequest {
            amounts: vec![1_000],
            ticker: "USDT".to_string(),
            name: "Tether".to_string(),
            precision: 0,
        })
        .expect("node A issueassetnia");
    let asset_id = asset.asset_id;

    let node_b_pubkey = node_b.node_info().expect("node B node_info").pubkey;
    let peer_uri = format!(
        "{node_b_pubkey}@127.0.0.1:{}",
        NODE_B_PEER_PORT + NODE_B_PORT_OFFSET
    );
    node_a
        .connectpeer(peer_uri.clone())
        .expect("node A connectpeer");

    let open_channel = node_a
        .openchannel(SdkOpenChannelRequest {
            peer_pubkey_and_opt_addr: peer_uri.clone(),
            capacity_sat: OPEN_CHANNEL_CAPACITY_SAT,
            push_msat: 0,
            public: true,
            with_anchors: true,
            fee_base_msat: None,
            fee_proportional_millionths: None,
            temporary_channel_id: None,
            asset_id: Some(asset_id),
            asset_amount: Some(600),
            push_asset_amount: None,
            virtual_open_mode: None,
        })
        .expect("node A openchannel");
    wait_for_channel_funding_tx(&node_a, &node_b, &asset_id, Duration::from_secs(120));
    mine(OPEN_CHANNEL_CONFIRM_BLOCKS);
    wait_for_usable_channel(&node_a, &node_b, &asset_id, Duration::from_secs(300));
    let channel_id = node_a
        .get_channel_id(open_channel.temporary_channel_id)
        .expect("node A get_channel_id");

    LagSetup {
        proxy,
        node_a,
        node_b,
        node_a_dir,
        mnemonic_a,
        node_b_pubkey,
        peer_uri,
        channel_id,
    }
}

fn keysend_raw(node: &SdkNode, dest: bitcoin::secp256k1::PublicKey) -> PaymentHash {
    node.keysend(SdkKeysendRequest {
        dest_pubkey: dest,
        amt_msat: 3_000_000,
        asset_id: None,
        asset_amount: None,
    })
    .expect("keysend")
    .payment_hash
}

fn payment_is_succeeded(node: &SdkNode, hash: &PaymentHash) -> bool {
    node.list_payments()
        .expect("list_payments")
        .into_iter()
        .any(|p| p.payment_hash == *hash && matches!(p.status, HtlcStatus::Succeeded))
}

fn restore_node_a(setup: &LagSetup) -> (SdkNode, Result<(), rgb_lightning_node::RlnError>) {
    fs::remove_dir_all(&setup.node_a_dir).expect("wipe node A storage");
    let node_a = make_node_with_vss(
        &setup.node_a_dir,
        NODE_A_DAEMON_PORT + NODE_A_PORT_OFFSET,
        NODE_A_PEER_PORT + NODE_A_PORT_OFFSET,
        &setup.proxy.url(),
    );
    let mnemonic_returned = node_a
        .init(PASSWORD_A.to_string(), Some(setup.mnemonic_a.clone()))
        .expect("node A re-init");
    assert_eq!(mnemonic_returned, setup.mnemonic_a);
    node_a
        .vss_clear_fence(SdkVssClearFenceRequest {
            password: PASSWORD_A.to_string(),
        })
        .expect("node A vss_clear_fence");
    let unlock_res = node_a.unlock(unlock_request(PASSWORD_A));
    (node_a, unlock_res)
}

/// A VSS outage pauses event processing (manager persists are remote-first)
/// instead of letting the backup lag: after recovery and a graceful shutdown,
/// a wipe-and-restore keeps the channel.
#[test]
#[serial]
fn manager_replication_outage_cannot_poison_restore() {
    ensure_regtest_available();
    if !vss_server_available() {
        eprintln!("SKIP: VSS server not available at {VSS_SERVER_ADDR}");
        return;
    }

    let setup = setup_with_open_channel("vss_manager_lag_recovers");

    setup.proxy.block_manager_writes();

    // First payment's events may process before its manager persist blocks;
    // the second payment's events sit behind the blocked persist.
    let hash1 = keysend_raw(&setup.node_a, setup.node_b_pubkey);
    std::thread::sleep(Duration::from_secs(5));
    let hash2 = keysend_raw(&setup.node_a, setup.node_b_pubkey);
    std::thread::sleep(Duration::from_secs(15));
    assert!(
        setup.proxy.blocked_count() > 0,
        "the proxy must have rejected manager puts, or this test exercises nothing"
    );
    assert!(
        !payment_is_succeeded(&setup.node_a, &hash2),
        "second payment must stay pending while the manager cannot reach VSS"
    );

    setup.proxy.allow_all();
    wait_for_succeeded_payment_in_list(&setup.node_a, &hash1, Duration::from_secs(60));
    wait_for_succeeded_payment_in_list(&setup.node_a, &hash2, Duration::from_secs(60));

    setup.node_a.shutdown();

    let (node_a, unlock_res) = restore_node_a(&setup);
    unlock_res.expect("restore after a recovered outage must unlock");
    let channels = node_a.list_channels().expect("node A list_channels");
    assert!(
        channels.iter().any(|c| c.channel_id == setup.channel_id),
        "channel must survive the restore"
    );
    assert!(
        payment_is_succeeded(&node_a, &hash1) && payment_is_succeeded(&node_a, &hash2),
        "restored node must know both payments as succeeded"
    );

    // The channel must be functional, not just listed.
    node_a
        .connectpeer(setup.peer_uri.clone())
        .expect("node A reconnect");
    wait_for_channel_ready(&node_a, setup.channel_id, Duration::from_secs(60));

    node_a.shutdown();
    setup.node_b.shutdown();
}

/// If the node stops while VSS is still down, the bounded shutdown flush gives
/// up and the backup keeps the stale manager; the restore must then refuse to
/// unlock instead of silently force-closing.
#[test]
#[serial]
fn restore_refuses_when_final_flush_fails() {
    ensure_regtest_available();
    if !vss_server_available() {
        eprintln!("SKIP: VSS server not available at {VSS_SERVER_ADDR}");
        return;
    }

    let setup = setup_with_open_channel("vss_manager_lag_refuses");

    setup.proxy.block_manager_writes();
    let _hash = keysend_raw(&setup.node_a, setup.node_b_pubkey);
    std::thread::sleep(Duration::from_secs(10));
    assert!(
        setup.proxy.blocked_count() > 0,
        "the proxy must have rejected manager puts, or this test exercises nothing"
    );

    // Monitors advanced past the last manager that reached VSS; the bounded
    // flush gives up on the newer manager while the proxy still blocks it.
    let shutdown_started = std::time::Instant::now();
    setup.node_a.shutdown();
    let shutdown_elapsed = shutdown_started.elapsed();
    assert!(
        shutdown_elapsed >= Duration::from_secs(25),
        "shutdown must have waited for the bounded flush (took {shutdown_elapsed:?})"
    );
    setup.proxy.allow_all();

    let (node_a, unlock_res) = restore_node_a(&setup);
    assert!(
        unlock_res.is_err(),
        "restore with a stale manager must refuse to unlock"
    );
    // A retry must hit the guard again, not skip the restore and force-close.
    assert!(
        node_a.unlock(unlock_request(PASSWORD_A)).is_err(),
        "second unlock attempt must be refused again"
    );
    assert!(
        !node_ldk_log_contains(&setup.node_a_dir, "force closed, should broadcast: true"),
        "a refused unlock must not have broadcast a force-close"
    );
    // Nothing was broadcast: node B must still see the channel open on-chain.
    mine(6);
    std::thread::sleep(Duration::from_secs(5));
    assert!(
        setup
            .node_b
            .list_channels()
            .expect("node B list_channels")
            .iter()
            .any(|c| c.channel_id == setup.channel_id),
        "counterparty must still see the channel open after a refused restore"
    );
    node_a.shutdown();
    drop(node_a);

    // The explicit override accepts the force-close: unlock proceeds and the
    // channel is gone, proving the refusal above was the consistency guard.
    fs::remove_dir_all(&setup.node_a_dir).expect("wipe refused node dir");
    let node_a = make_node_with_vss_allow_empty(
        &setup.node_a_dir,
        NODE_A_DAEMON_PORT + NODE_A_PORT_OFFSET,
        NODE_A_PEER_PORT + NODE_A_PORT_OFFSET,
        &setup.proxy.url(),
    );
    node_a
        .init(PASSWORD_A.to_string(), Some(setup.mnemonic_a.clone()))
        .expect("override re-init");
    node_a
        .vss_clear_fence(SdkVssClearFenceRequest {
            password: PASSWORD_A.to_string(),
        })
        .expect("override clear fence");
    node_a
        .unlock(unlock_request(PASSWORD_A))
        .expect("unlock with --vss-allow-empty-restore must proceed");
    assert!(
        node_a
            .list_channels()
            .expect("node A list_channels")
            .is_empty(),
        "override accepts the force-close: the stale-manager channel is gone"
    );

    node_a.shutdown();
    setup.node_b.shutdown();
}
