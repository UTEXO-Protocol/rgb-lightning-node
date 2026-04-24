use super::test_utils::reset_chain_sync_storage_for_tests;
use super::*;
#[cfg(target_arch = "wasm32")]
use futures::executor::block_on;

#[test]
fn start_persists_configuration() {
    reset_chain_sync_storage_for_tests();
    let driver =
        WasmChainSyncDriver::new("test-start".to_string(), "regtest".to_string()).expect("driver");
    driver
        .start("http://127.0.0.1:3002".to_string(), Some(5_000))
        .expect("start");
    let status = driver.status();
    assert!(status.running);
    assert_eq!(status.poll_interval_ms, 5_000);
    assert_eq!(status.indexer_url.as_deref(), Some("http://127.0.0.1:3002"));
}

#[test]
fn enqueue_rebroadcast_tracks_queue() {
    reset_chain_sync_storage_for_tests();
    let driver =
        WasmChainSyncDriver::new("test-queue".to_string(), "regtest".to_string()).expect("driver");
    driver
        .enqueue_rebroadcast_tx(
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
            "020000000001".to_string(),
        )
        .expect("enqueue");
    let status = driver.status();
    assert_eq!(status.rebroadcast_pending, 1);
    assert_eq!(status.rebroadcast_confirmed, 0);
}

#[test]
fn chain_sync_tip_regression_tracking_contract() {
    reset_chain_sync_storage_for_tests();
    let runtime_key = format!(
        "chain-sync-tip-regression-test-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    );
    let driver = WasmChainSyncDriver::new(runtime_key, "regtest".to_string()).expect("driver");

    driver.apply_tip_height(100, 1);
    let s1 = driver.status();
    assert_eq!(s1.latest_tip_height, Some(100));
    assert!(!s1.tip_regressed);
    assert_eq!(s1.last_tip_regression_at, None);

    driver.apply_tip_height(101, 2);
    let s2 = driver.status();
    assert_eq!(s2.latest_tip_height, Some(101));
    assert!(!s2.tip_regressed);
    assert_eq!(s2.last_tip_regression_at, None);

    driver.apply_tip_height(99, 3);
    let s3 = driver.status();
    assert_eq!(s3.latest_tip_height, Some(99));
    assert!(s3.tip_regressed);
    assert_eq!(s3.last_tip_regression_at, Some(3));

    driver.apply_tip_height(100, 4);
    let s4 = driver.status();
    assert_eq!(s4.latest_tip_height, Some(100));
    assert!(!s4.tip_regressed);
    assert_eq!(s4.last_tip_regression_at, Some(3));
}

#[test]
fn restored_driver_reads_previous_state() {
    reset_chain_sync_storage_for_tests();
    let key = "test-restore".to_string();
    let driver =
        WasmChainSyncDriver::new(key.clone(), "regtest".to_string()).expect("driver create");
    driver
        .start("http://127.0.0.1:3002".to_string(), Some(7_000))
        .expect("start");
    let restored = WasmChainSyncDriver::new(key, "regtest".to_string()).expect("restore");
    let status = restored.status();
    assert!(status.running);
    assert_eq!(status.poll_interval_ms, 7_000);
    assert_eq!(status.indexer_url.as_deref(), Some("http://127.0.0.1:3002"));
}

#[cfg(target_arch = "wasm32")]
#[test]
fn start_rejects_invalid_indexer_scheme_contract() {
    reset_chain_sync_storage_for_tests();
    let driver =
        WasmChainSyncDriver::new("test-invalid-indexer".to_string(), "regtest".to_string())
            .expect("driver");
    let err = driver
        .start("esplora://127.0.0.1:3002".to_string(), Some(5_000))
        .expect_err("invalid indexer should fail");
    assert_eq!(
        err.as_string().unwrap_or_default(),
        "indexer_url must start with http:// or https://"
    );
}

#[test]
fn start_clamps_poll_interval_contract() {
    reset_chain_sync_storage_for_tests();
    let driver =
        WasmChainSyncDriver::new("test-clamp".to_string(), "regtest".to_string()).expect("driver");
    driver
        .start("http://127.0.0.1:3002".to_string(), Some(10))
        .expect("start");
    let status = driver.status();
    assert_eq!(status.poll_interval_ms, CHAIN_SYNC_MIN_POLL_INTERVAL_MS);

    driver
        .start("http://127.0.0.1:3002".to_string(), Some(120_000))
        .expect("start");
    let status = driver.status();
    assert_eq!(status.poll_interval_ms, CHAIN_SYNC_MAX_POLL_INTERVAL_MS);
}

#[cfg(target_arch = "wasm32")]
#[test]
fn tick_requires_running_contract() {
    reset_chain_sync_storage_for_tests();
    let driver = WasmChainSyncDriver::new("test-tick-running".to_string(), "regtest".to_string())
        .expect("driver");
    let err = block_on(driver.tick()).expect_err("tick should fail when stopped");
    assert_eq!(
        err.as_string().unwrap_or_default(),
        "chain sync is not running; call chainSyncStart first"
    );
}

#[cfg(target_arch = "wasm32")]
#[test]
fn enqueue_rebroadcast_rejects_invalid_inputs_contract() {
    reset_chain_sync_storage_for_tests();
    let driver =
        WasmChainSyncDriver::new("test-enqueue-invalid".to_string(), "regtest".to_string())
            .expect("driver");

    let err = driver
        .enqueue_rebroadcast_tx("abcd".to_string(), "00".to_string())
        .expect_err("invalid txid should fail");
    assert_eq!(
        err.as_string().unwrap_or_default(),
        "txid must be 64 hex chars"
    );

    let err = driver
        .enqueue_rebroadcast_tx(
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
            "zzzz".to_string(),
        )
        .expect_err("invalid tx hex should fail");
    assert_eq!(
        err.as_string().unwrap_or_default(),
        "tx_hex must be non-empty hex"
    );
}
