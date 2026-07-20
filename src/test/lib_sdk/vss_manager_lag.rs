//! Reproduces the 2026-07 field incident: the channel-manager key on VSS lags
//! behind the (remote-first) channel monitors, so a fresh-device restore loads
//! a manager that does not know the channel and LDK force-closes it
//! (`OutdatedChannelManager`) — `listchannels` comes back empty even though
//! the restore itself "succeeded".
//!
//! The lag is driven through the production write path: an HTTP proxy in
//! front of VSS rejects only PUTs of the `_/_/manager` key (a transient-style
//! 502), which the best-effort `SyncedKvStore` replication absorbs into its
//! in-memory pending queue. Monitor writes (RemoteFirstKvStore) pass through
//! untouched. A graceful shutdown then discards the queue silently.
//!
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

fn vss_server_available() -> bool {
    std::net::TcpStream::connect_timeout(&VSS_SERVER_ADDR.parse().unwrap(), Duration::from_secs(2))
        .is_ok()
}

/// HTTP proxy for the VSS server that can reject writes of the
/// channel-manager key while passing everything else through — most
/// importantly all channel-monitor writes.
struct ManagerFilterProxy {
    port: u16,
    filter: Arc<AtomicBool>,
}

impl ManagerFilterProxy {
    fn start() -> Self {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let filter = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&filter);
        std::thread::spawn(move || {
            for conn in listener.incoming() {
                let Ok(stream) = conn else { break };
                let flag = Arc::clone(&flag);
                std::thread::spawn(move || {
                    let _ = handle_conn(stream, flag);
                });
            }
        });
        Self { port, filter }
    }

    fn url(&self) -> String {
        format!("http://127.0.0.1:{}/vss", self.port)
    }

    fn block_manager_writes(&self) {
        self.filter.store(true, Ordering::SeqCst);
    }

    fn allow_all(&self) {
        self.filter.store(false, Ordering::SeqCst);
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

fn handle_conn(mut client: std::net::TcpStream, filter: Arc<AtomicBool>) -> std::io::Result<()> {
    while let Some((head, body)) = read_http_request(&mut client)? {
        let head_str = String::from_utf8_lossy(&head).to_string();
        let is_put = head_str.lines().next().is_some_and(|l| l.contains("putObject"));
        let has_manager_key = body
            .windows(MANAGER_VSS_KEY.len())
            .any(|w| w == MANAGER_VSS_KEY);
        if filter.load(Ordering::SeqCst) && is_put && has_manager_key {
            client.write_all(
                b"HTTP/1.1 502 Bad Gateway\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
            )?;
            return Ok(());
        }
        // Forward on a fresh upstream connection with `Connection: close` so
        // the response is delimited by EOF and needs no framing of its own.
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
        return Ok(());
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
        } else if path.file_name().is_some_and(|n| n == "logs.txt") {
            if fs::read_to_string(&path).is_ok_and(|c| c.contains(needle)) {
                return true;
            }
        }
    }
    false
}

#[test]
#[serial]
fn restore_keeps_channel_when_manager_replication_lagged() {
    ensure_regtest_available();
    if !vss_server_available() {
        eprintln!("SKIP: VSS server not available at {VSS_SERVER_ADDR}");
        return;
    }

    let test_dir = test_dir("vss_manager_lag");
    if test_dir.exists() {
        fs::remove_dir_all(&test_dir).expect("clean previous test dir");
    }
    fs::create_dir_all(&test_dir).expect("create test dir");
    let node_a_dir = test_dir.join("node_a");
    let node_b_dir = test_dir.join("node_b");

    let proxy = ManagerFilterProxy::start();

    // --- Phase 1: node A (VSS via proxy) + peer B, channel while manager
    // replication silently fails. ---
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

    // From here on the manager key never reaches VSS again (transient-style
    // 502s absorbed by the best-effort replication queue); monitors do.
    proxy.block_manager_writes();

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
            asset_id: Some(asset_id.clone()),
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

    // --- Phase 2: graceful shutdown (drops the pending replication queue),
    // outage "ends", node A device is wiped. ---
    node_a.shutdown();
    drop(node_a);
    proxy.allow_all();

    fs::remove_dir_all(&node_a_dir).expect("wipe node A storage");

    // --- Phase 3: fresh device, same mnemonic, by-the-book restore. ---
    let node_a = make_node_with_vss(
        &node_a_dir,
        NODE_A_DAEMON_PORT + NODE_A_PORT_OFFSET,
        NODE_A_PEER_PORT + NODE_A_PORT_OFFSET,
        &proxy.url(),
    );
    let mnemonic_returned = node_a
        .init(PASSWORD_A.to_string(), Some(mnemonic_a.clone()))
        .expect("node A re-init");
    assert_eq!(mnemonic_returned, mnemonic_a);
    node_a
        .vss_clear_fence(SdkVssClearFenceRequest {
            password: PASSWORD_A.to_string(),
        })
        .expect("node A vss_clear_fence");
    node_a
        .unlock(unlock_request(PASSWORD_A))
        .expect("node A unlock after restore");

    // --- Phase 4: the channel must have survived the restore. ---
    let channels = node_a.list_channels().expect("node A list_channels");
    let channel_ids: Vec<_> = channels.iter().map(|c| c.channel_id).collect();
    let force_closed_evidence =
        node_ldk_log_contains(&node_a_dir, "hen we loaded") // "...was not found... when we loaded"
            || node_ldk_log_contains(&node_a_dir, "Force-closing");
    assert!(
        channel_ids.contains(&channel_id),
        "channel {channel_id} must survive a VSS restore even when manager replication lagged \
         behind monitor replication; got channels: {channel_ids:?} \
         (force-close evidence in LDK log: {force_closed_evidence})"
    );

    // Cleanup
    node_a.shutdown();
    node_b.shutdown();
}
