use super::*;
use crate::gossip::GossipSourceConfig;
use tokio::io::AsyncWriteExt;

const TEST_DIR_BASE: &str = "tmp/gossip_rgs/";

// A known-valid Rapid Gossip Sync v1 snapshot (1 channel announcement + updates).
// The chain hash (bytes 4..36) and the snapshot timestamp (bytes 36..40) are
// patched at runtime so the snapshot applies to the regtest network graph and
// passes the freshness check.
const RGS_SNAPSHOT_TEMPLATE: [u8; 300] = [
    76, 68, 75, 1, 111, 226, 140, 10, 182, 241, 179, 114, 193, 166, 162, 70, 174, 99, 247, 79, 147,
    30, 131, 101, 225, 90, 8, 156, 104, 214, 25, 0, 0, 0, 0, 0, 97, 227, 98, 218, 0, 0, 0, 4, 2,
    22, 7, 207, 206, 25, 164, 197, 231, 230, 231, 56, 102, 61, 250, 251, 187, 172, 38, 46, 79, 247,
    108, 44, 155, 48, 219, 238, 252, 53, 192, 6, 67, 2, 36, 125, 157, 176, 223, 175, 234, 116, 94,
    248, 201, 225, 97, 235, 50, 47, 115, 172, 63, 136, 88, 216, 115, 11, 111, 217, 114, 84, 116,
    124, 231, 107, 2, 158, 1, 242, 121, 152, 106, 204, 131, 186, 35, 93, 70, 216, 10, 237, 224,
    183, 89, 95, 65, 3, 83, 185, 58, 138, 181, 64, 187, 103, 127, 68, 50, 2, 201, 19, 17, 138, 136,
    149, 185, 226, 156, 137, 175, 110, 32, 237, 0, 217, 90, 31, 100, 228, 149, 46, 219, 175, 168,
    77, 4, 143, 38, 128, 76, 97, 0, 0, 0, 2, 0, 0, 255, 8, 153, 192, 0, 2, 27, 0, 0, 0, 1, 0, 0,
    255, 2, 68, 226, 0, 6, 11, 0, 1, 2, 3, 0, 0, 0, 4, 0, 40, 0, 0, 0, 0, 0, 0, 3, 232, 0, 0, 3,
    232, 0, 0, 0, 1, 0, 0, 0, 0, 29, 129, 25, 192, 255, 8, 153, 192, 0, 2, 27, 0, 0, 60, 0, 0, 0,
    0, 0, 0, 0, 1, 0, 0, 0, 100, 0, 0, 2, 224, 0, 0, 0, 0, 58, 85, 116, 216, 0, 29, 0, 0, 0, 1, 0,
    0, 0, 125, 0, 0, 0, 0, 58, 85, 116, 216, 255, 2, 68, 226, 0, 6, 11, 0, 1, 0, 0, 1,
];

fn regtest_rgs_snapshot() -> Vec<u8> {
    let mut data = RGS_SNAPSHOT_TEMPLATE.to_vec();
    let chain_hash = bitcoin::constants::ChainHash::using_genesis_block(bitcoin::Network::Regtest);
    data[4..36].copy_from_slice(chain_hash.as_bytes());
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as u32;
    data[36..40].copy_from_slice(&now.to_be_bytes());
    data
}

// Serve the given snapshot bytes over HTTP on an ephemeral port, for any GET path.
async fn serve_rgs_snapshot(snapshot: Vec<u8>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            let body = snapshot.clone();
            tokio::spawn(async move {
                let mut buf = [0u8; 1024];
                let _ = sock.read(&mut buf).await;
                let header = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = sock.write_all(header.as_bytes()).await;
                let _ = sock.write_all(&body).await;
                let _ = sock.flush().await;
            });
        }
    });
    format!("http://{addr}")
}

#[serial_test::serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[traced_test]
async fn rgs_mode_syncs_snapshot() {
    initialize();

    let rgs_url = serve_rgs_snapshot(regtest_rgs_snapshot()).await;

    let test_dir_node1 = format!("{TEST_DIR_BASE}node1");
    let password = format!("{test_dir_node1}.{NODE1_PEER_PORT}");
    let node_addr = start_daemon(&test_dir_node1, NODE1_PEER_PORT, None, false).await;
    init(node_addr, &password, None).await;
    unlock_with_gossip_source(
        node_addr,
        &password,
        Some(GossipSourceConfig::RapidGossipSync {
            server_url: rgs_url,
        }),
    )
    .await;

    // The background task fires its first tick immediately, so the snapshot is
    // fetched and applied shortly after unlock.
    let t_0 = OffsetDateTime::now_utc();
    loop {
        if let Some(ts) = node_info(node_addr).await.latest_rgs_snapshot_timestamp {
            assert!(ts > 0, "RGS snapshot timestamp should be non-zero");
            break;
        }
        if (OffsetDateTime::now_utc() - t_0).as_seconds_f32() > 15.0 {
            panic!("RGS snapshot timestamp did not become Some within timeout");
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
}
