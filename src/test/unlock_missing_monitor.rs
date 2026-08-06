use super::*;

const TEST_DIR_BASE: &str = "tmp/unlock_missing_monitor/";

async fn delete_monitor_rows(node_test_dir: &str) -> u64 {
    use sea_orm::{ConnectionTrait, Database, Statement};

    let db = Database::connect(format!("sqlite:{node_test_dir}/rln_db?mode=rw"))
        .await
        .expect("open node db");
    let res = db
        .execute(Statement::from_string(
            sea_orm::DatabaseBackend::Sqlite,
            "DELETE FROM kv_store WHERE primary_namespace IN ('monitors', 'monitor_updates')",
        ))
        .await
        .expect("delete monitor rows");
    res.rows_affected()
}

/// A channel manager whose channel monitors are missing from storage (e.g. an
/// incomplete backup) must fail unlock with a clear error and leave the node
/// serving, instead of panicking and dropping the connection.
#[serial_test::serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[traced_test]
async fn unlock_with_missing_monitor_fails_cleanly() {
    tokio::time::timeout(
        std::time::Duration::from_secs(300),
        unlock_with_missing_monitor_fails_cleanly_inner(),
    )
    .await
    .expect("unlock_with_missing_monitor_fails_cleanly timed out");
}

async fn unlock_with_missing_monitor_fails_cleanly_inner() {
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

    let deleted = delete_monitor_rows(&test_dir_node1).await;
    assert!(deleted > 0, "monitors must exist before deletion");

    let node1_addr =
        start_daemon_with_vss(&test_dir_node1, NODE1_PEER_PORT, true, None, false).await;

    // Two attempts: the failure must be a clean, repeatable error, not a
    // panicked task and a dropped connection.
    for attempt in 1..=2 {
        let res = reqwest::Client::new()
            .post(format!("http://{node1_addr}/unlock"))
            .json(&unlock_req(&node1_password))
            .send()
            .await
            .unwrap_or_else(|e| panic!("attempt {attempt}: unlock must answer, got: {e}"));
        assert_eq!(
            res.status(),
            reqwest::StatusCode::INTERNAL_SERVER_ERROR,
            "attempt {attempt}"
        );
        let body = res.json::<APIErrorResponse>().await.unwrap();
        assert_eq!(body.name, "FailedLoadingChannelState", "attempt {attempt}");
        assert!(
            body.error.contains("channel monitor"),
            "attempt {attempt}: error must explain the cause: {}",
            body.error
        );
    }

    // The node must keep serving after the failed unlocks.
    let res = reqwest::Client::new()
        .get(format!("http://{node1_addr}/nodeinfo"))
        .send()
        .await
        .expect("nodeinfo must answer");
    assert_eq!(res.status(), reqwest::StatusCode::FORBIDDEN);

    shutdown(&[node1_addr]).await;
}
