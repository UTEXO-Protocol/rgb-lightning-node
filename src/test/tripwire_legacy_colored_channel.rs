use super::*;

use lightning::util::ser::{BigSize, Readable, Writeable};

const TEST_DIR_BASE: &str = "tmp/tripwire_legacy_colored_channel/";

fn read_bigsize(data: &[u8]) -> Option<(u64, usize)> {
    let mut cursor = data;
    let before = cursor.len();
    let v = BigSize::read(&mut cursor).ok()?;
    Some((v.0, before - cursor.len()))
}

// (type, start offset, end offset) of a TLV record within a stream's records region.
type Record = (u64, usize, usize);

fn parse_records(data: &[u8]) -> Option<Vec<Record>> {
    let mut records = Vec::new();
    let mut pos = 0usize;
    let mut last_type: Option<u64> = None;
    while pos < data.len() {
        let start = pos;
        let (typ, tn) = read_bigsize(&data[pos..])?;
        pos += tn;
        let (len, ln) = read_bigsize(&data[pos..])?;
        pos += ln;
        pos = pos.checked_add(len as usize)?;
        if pos > data.len() || typ > 255 {
            return None;
        }
        if let Some(lt) = last_type {
            if typ <= lt {
                return None;
            }
        }
        last_type = Some(typ);
        records.push((typ, start, pos));
    }
    (pos == data.len()).then_some(records)
}

fn bigsize_bytes(v: u64) -> Vec<u8> {
    let mut out = Vec::new();
    BigSize(v).write(&mut out).unwrap();
    out
}

/// Rewrite the persisted `ChannelManager` blob so its single channel carries the pre-sync
/// colored-channel marker: a legacy TLV record at type 71 in the `ChannelContext` stream, with no
/// `rgb_asset` (type 73). This reproduces exactly what a pre-sync build wrote for a colored
/// channel; reading it must now hit the tripwire (`DangerousValue`) instead of silently dropping
/// the asset. The base channel is non-colored, so it has neither 71 nor 73 to begin with.
fn splice_legacy_marker(blob: &[u8], counterparty_pubkey: &[u8]) -> Vec<u8> {
    // Manager layout up to the channels: [ver:2][chain_hash:32][height:4][block_hash:32]
    // [num_channels:u64:8]. First (only) channel begins at 78.
    const FIRST_CHANNEL: usize = 78;

    // After the channels come forward_htlcs(u64=0), claimable_payments(u64=0),
    // serializable_peer_count(u64=1), then the peer's node_id (33 bytes). Anchor on that whole
    // pattern; its start is the end of the channel serialization.
    let mut anchor = vec![0u8; 23];
    anchor.push(1);
    anchor.extend_from_slice(counterparty_pubkey);
    let channel_end = (FIRST_CHANNEL..=blob.len().saturating_sub(anchor.len()))
        .find(|&i| &blob[i..i + anchor.len()] == anchor.as_slice())
        .unwrap_or_else(|| {
            panic!("post-channel anchor (fwd=0,claimable=0,peers=1,node_id) not found in manager")
        });

    // Locate the ChannelContext TLV stream: the suffix [s..channel_end] framed as
    // BigSize(len) + len bytes of ascending TLV records, ending exactly at channel_end. Any
    // record-boundary suffix of an ascending stream is itself a valid framing, so pick the
    // outermost one (the most records) — that is the real stream start.
    let mut found: Option<(usize, Vec<Record>)> = None;
    for s in FIRST_CHANNEL..channel_end {
        let (len, plen) = match read_bigsize(&blob[s..channel_end]) {
            Some(v) => v,
            None => continue,
        };
        if s + plen + len as usize != channel_end {
            continue;
        }
        if let Some(records) = parse_records(&blob[s + plen..channel_end]) {
            let better = found
                .as_ref()
                .map(|(_, r)| records.len() > r.len())
                .unwrap_or(true);
            if better {
                found = Some((s, records));
            }
        }
    }
    let (stream_start, records) =
        found.expect("ChannelContext TLV stream not located in manager blob");
    let (len, plen) = read_bigsize(&blob[stream_start..channel_end]).unwrap();
    let records_bytes = blob[stream_start + plen..channel_end].to_vec();

    assert!(
        records.iter().all(|&(t, _, _)| t != 71 && t != 73),
        "base channel must be non-colored (no legacy marker, no rgb_asset)"
    );

    // Legacy marker record: type 71, value is a u16-length-prefixed UTF-8 endpoint, matching how
    // the pre-sync `RgbTransport` serialized `consignment_endpoint`.
    let endpoint = b"rpc://127.0.0.1:3000/json-rpc";
    let mut value = Vec::new();
    (endpoint.len() as u16).write(&mut value).unwrap();
    value.extend_from_slice(endpoint);
    let mut record = bigsize_bytes(71);
    record.extend_from_slice(&bigsize_bytes(value.len() as u64));
    record.extend_from_slice(&value);

    // Insert in ascending type order (before the first record of type > 71, else append).
    let insert_at = records
        .iter()
        .find(|&&(t, _, _)| t > 71)
        .map(|&(_, start, _)| start)
        .unwrap_or(records_bytes.len());

    let mut new_records = Vec::with_capacity(records_bytes.len() + record.len());
    new_records.extend_from_slice(&records_bytes[..insert_at]);
    new_records.extend_from_slice(&record);
    new_records.extend_from_slice(&records_bytes[insert_at..]);

    let new_len = len as usize + record.len();
    let mut out = Vec::with_capacity(blob.len() + record.len() + 4);
    out.extend_from_slice(&blob[..stream_start]);
    out.extend_from_slice(&bigsize_bytes(new_len as u64));
    out.extend_from_slice(&new_records);
    out.extend_from_slice(&blob[channel_end..]);
    out
}

async fn rewrite_manager_with_legacy_marker(node_test_dir: &str, counterparty_pubkey: &[u8]) {
    use sea_orm::{ConnectionTrait, Database, Statement};

    let db = Database::connect(format!("sqlite:{node_test_dir}/rln_db?mode=rw"))
        .await
        .expect("open node db");
    let row = db
        .query_one(Statement::from_string(
            sea_orm::DatabaseBackend::Sqlite,
            "SELECT value FROM kv_store WHERE primary_namespace = '' AND \
             secondary_namespace = '' AND key = 'manager'",
        ))
        .await
        .expect("query manager row")
        .expect("manager row must exist");
    let blob: Vec<u8> = row.try_get("", "value").expect("read manager value");

    let spliced = splice_legacy_marker(&blob, counterparty_pubkey);
    assert!(spliced.len() > blob.len(), "splice must grow the blob");

    db.execute(Statement::from_sql_and_values(
        sea_orm::DatabaseBackend::Sqlite,
        "UPDATE kv_store SET value = ? WHERE primary_namespace = '' AND \
         secondary_namespace = '' AND key = 'manager'",
        [spliced.into()],
    ))
    .await
    .expect("rewrite manager row");
}

/// A channel persisted by a pre-sync build (colored marker at TLV 71, no `rgb_asset` at 73) must
/// refuse to deserialize on unlock, surfacing the `DangerousValue` tripwire, instead of silently
/// downgrading to a non-colored channel and stranding the RGB asset.
#[serial_test::serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[traced_test]
async fn pre_sync_colored_channel_unlock_is_refused() {
    tokio::time::timeout(
        std::time::Duration::from_secs(300),
        pre_sync_colored_channel_unlock_is_refused_inner(),
    )
    .await
    .expect("pre_sync_colored_channel_unlock_is_refused timed out");
}

async fn pre_sync_colored_channel_unlock_is_refused_inner() {
    initialize();

    let test_dir_node1 = format!("{TEST_DIR_BASE}node1");
    let test_dir_node2 = format!("{TEST_DIR_BASE}node2");

    let (node1_addr, node1_password) = start_node(&test_dir_node1, NODE1_PEER_PORT, false).await;
    let (node2_addr, _) = start_node(&test_dir_node2, NODE2_PEER_PORT, false).await;

    fund_and_create_utxos(node1_addr, None).await;
    fund_and_create_utxos(node2_addr, None).await;

    let node2_pubkey = node_info(node2_addr).await.pubkey;
    connect_peer(
        node1_addr,
        &node2_pubkey,
        &format!("127.0.0.1:{NODE2_PEER_PORT}"),
    )
    .await;

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

    shutdown(&[node1_addr, node2_addr]).await;

    let counterparty_pubkey = PublicKey::from_str(&node2_pubkey).unwrap().serialize();
    rewrite_manager_with_legacy_marker(&test_dir_node1, &counterparty_pubkey).await;

    let node1_addr = start_daemon_with_virtual_options(
        &test_dir_node1,
        NODE1_PEER_PORT,
        None,
        true,
        false,
        vec![],
    )
    .await;

    let res = reqwest::Client::new()
        .post(format!("http://{node1_addr}/unlock"))
        .json(&unlock_req(&node1_password))
        .send()
        .await
        .expect("unlock must answer");
    assert_eq!(res.status(), reqwest::StatusCode::INTERNAL_SERVER_ERROR);
    let body = res.json::<APIErrorResponse>().await.unwrap();
    assert_eq!(body.name, "FailedLoadingChannelState");
    assert!(
        body.error.contains("DangerousValue"),
        "error must trace to the tripwire: {}",
        body.error
    );

    shutdown(&[node1_addr]).await;
}
