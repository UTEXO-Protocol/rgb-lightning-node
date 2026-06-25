use super::*;

const TEST_DIR_BASE: &str = "tmp/address_reuse/";
const FUND_SATS: u64 = 100_000_000;
#[cfg(feature = "vss")]
const VSS_URL: &str = "http://localhost:8081/vss";

async fn rotate_address_res(node_address: SocketAddr) -> Response {
    reqwest::Client::new()
        .post(format!("http://{node_address}/rotateaddress"))
        .send()
        .await
        .unwrap()
}

async fn rotate_address(node_address: SocketAddr) -> String {
    let res = rotate_address_res(node_address).await;
    check_response_is_ok(res)
        .await
        .json::<AddressResponse>()
        .await
        .unwrap()
        .address
}

fn fund_and_mine(address: String, sats: u64) {
    fund_wallet(address, sats);
    mine(false);
}

async fn settled_btc(node_address: SocketAddr) -> u64 {
    btc_balance(node_address).await.vanilla.settled
}

/// With reuse on, `/address` pins a single address and BTC sent to it
/// repeatedly accumulates on that same address.
#[serial_test::serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[traced_test]
async fn reuse_enabled_pins_address_and_accumulates_funds() {
    initialize();

    let test_dir = format!("{TEST_DIR_BASE}reuse_on");
    let (node_addr, _, _) =
        start_node_with_reuse_addresses(&test_dir, NODE1_PEER_PORT, true, false, None).await;

    let pinned = address(node_addr).await;
    assert_eq!(pinned, address(node_addr).await, "address must stay pinned");

    fund_and_mine(pinned.clone(), FUND_SATS);
    assert!(settled_btc(node_addr).await >= FUND_SATS);

    // The pin is unchanged after receiving, and a second payment to the same
    // address accumulates rather than landing on a fresh one.
    assert_eq!(address(node_addr).await, pinned);
    fund_and_mine(pinned.clone(), FUND_SATS);
    assert!(
        settled_btc(node_addr).await >= 2 * FUND_SATS,
        "funds sent to the reused address must accumulate"
    );
}

/// With reuse off (default), `/address` returns a fresh address each call.
#[serial_test::serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[traced_test]
async fn reuse_disabled_returns_fresh_addresses() {
    initialize();

    let test_dir = format!("{TEST_DIR_BASE}reuse_off");
    let (node_addr, _, _) =
        start_node_with_reuse_addresses(&test_dir, NODE1_PEER_PORT, false, false, None).await;

    assert_ne!(address(node_addr).await, address(node_addr).await);
}

/// `/rotateaddress` advances the pin to a new address that subsequent
/// `/address` calls return, and which can receive funds.
#[serial_test::serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[traced_test]
async fn rotate_advances_pin_and_new_address_receives_funds() {
    initialize();

    let test_dir = format!("{TEST_DIR_BASE}rotate_on");
    let (node_addr, _, _) =
        start_node_with_reuse_addresses(&test_dir, NODE1_PEER_PORT, true, false, None).await;

    let pinned = address(node_addr).await;
    fund_and_mine(pinned.clone(), FUND_SATS);
    assert!(settled_btc(node_addr).await >= FUND_SATS);

    let rotated = rotate_address(node_addr).await;
    assert_ne!(rotated, pinned);
    assert_eq!(
        rotated,
        address(node_addr).await,
        "the pin must move to the rotated address"
    );

    fund_and_mine(rotated, FUND_SATS);
    assert!(
        settled_btc(node_addr).await >= 2 * FUND_SATS,
        "funds sent to the rotated address must be spendable"
    );
}

/// `/rotateaddress` is rejected when reuse is disabled.
#[serial_test::serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[traced_test]
async fn rotate_disabled_errors() {
    initialize();

    let test_dir = format!("{TEST_DIR_BASE}rotate_off");
    let (node_addr, _, _) =
        start_node_with_reuse_addresses(&test_dir, NODE1_PEER_PORT, false, false, None).await;

    let res = rotate_address_res(node_addr).await;
    check_response_is_nok(
        res,
        reqwest::StatusCode::BAD_REQUEST,
        "Address reuse is disabled",
        "AddressReuseDisabled",
    )
    .await;
}

/// The pinned address and its balance survive a restart that keeps local state.
#[serial_test::serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[traced_test]
async fn reuse_pinned_address_and_balance_survive_restart() {
    initialize();

    let test_dir = format!("{TEST_DIR_BASE}restart");
    let (node_addr, _, _) =
        start_node_with_reuse_addresses(&test_dir, NODE1_PEER_PORT, true, false, None).await;

    let pinned = address(node_addr).await;
    fund_and_mine(pinned.clone(), FUND_SATS);
    let balance = settled_btc(node_addr).await;
    assert!(balance >= FUND_SATS);

    shutdown(&[node_addr]).await;
    let (node_addr, _, _) =
        start_node_with_reuse_addresses(&test_dir, NODE1_PEER_PORT, true, true, None).await;

    assert_eq!(
        address(node_addr).await,
        pinned,
        "pinned address must be stable across a restart"
    );
    assert!(
        settled_btc(node_addr).await >= balance,
        "balance must be retained across a restart"
    );
}

/// The pinned address is deterministic from the seed: after wiping all local
/// state and re-initializing from the same mnemonic, `/address` returns the
/// same pinned address and the on-chain funds are recovered.
#[serial_test::serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[traced_test]
async fn reuse_pinned_address_recovers_from_seed() {
    initialize();

    let test_dir = format!("{TEST_DIR_BASE}seed_recovery");
    let (node_addr, _, mnemonic) =
        start_node_with_reuse_addresses(&test_dir, NODE1_PEER_PORT, true, false, None).await;

    let pinned = address(node_addr).await;
    fund_and_mine(pinned.clone(), FUND_SATS);
    let balance = settled_btc(node_addr).await;
    assert!(balance >= FUND_SATS);

    // Wipe local state and recover from the seed alone.
    shutdown(&[node_addr]).await;
    let (node_addr, _, _) =
        start_node_with_reuse_addresses(&test_dir, NODE1_PEER_PORT, true, false, Some(&mnemonic))
            .await;

    assert_eq!(
        address(node_addr).await,
        pinned,
        "pinned address must be re-derived from the seed after a wipe"
    );
    assert!(
        settled_btc(node_addr).await >= balance,
        "on-chain funds must be recovered from the seed after a wipe"
    );
}

/// The rotated pin is persisted and survives a restart that keeps local state.
#[serial_test::serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[traced_test]
async fn rotate_persists_across_restart() {
    initialize();

    let test_dir = format!("{TEST_DIR_BASE}rotate_restart");
    let (node_addr, _, _) =
        start_node_with_reuse_addresses(&test_dir, NODE1_PEER_PORT, true, false, None).await;

    let base = address(node_addr).await;
    let rotated = rotate_address(node_addr).await;
    assert_ne!(rotated, base);
    assert_eq!(rotated, address(node_addr).await);

    shutdown(&[node_addr]).await;
    let (node_addr, _, _) =
        start_node_with_reuse_addresses(&test_dir, NODE1_PEER_PORT, true, true, None).await;

    assert_eq!(
        address(node_addr).await,
        rotated,
        "rotated pin must persist across a restart"
    );
}

/// End-to-end VSS recovery with reuse enabled: a reuse node issues an asset,
/// is wiped, and restarts from the same seed + VSS. The pinned address, the
/// on-chain funds and the RGB asset must all come back.
#[cfg(feature = "vss")]
#[serial_test::serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[traced_test]
async fn reuse_address_and_assets_recover_via_vss() {
    if !vss_server_available() {
        eprintln!("SKIP: VSS server not available at {VSS_URL}");
        return;
    }
    initialize();

    let test_dir = format!("{TEST_DIR_BASE}vss");
    let (node_addr, _, mnemonic) =
        start_node_with_vss(&test_dir, NODE1_PEER_PORT, false, VSS_URL, None, true).await;

    // Rotate first so recovery has to restore a non-default pin from VSS.
    let base = address(node_addr).await;
    let pinned = rotate_address(node_addr).await;
    assert_ne!(pinned, base);
    fund_and_create_utxos(node_addr, None).await;

    let asset_id = issue_asset_nia(node_addr).await.asset_id;
    assert_eq!(
        asset_balance_spendable(node_addr, &asset_id).await,
        ISSUE_AMT
    );

    // Blocking RGB backup: the asset's VSS backup is durable once issue returns.
    assert_eq!(
        vss_backup_info(node_addr).await["backup_exists"],
        serde_json::json!(true),
        "rgb backup must exist after issue"
    );

    let pre_wipe_btc = settled_btc(node_addr).await;

    wipe_node_dir(node_addr, &test_dir).await;
    let (node_addr, _, _) = start_node_with_vss(
        &test_dir,
        NODE1_PEER_PORT,
        true,
        VSS_URL,
        Some(&mnemonic),
        true,
    )
    .await;

    assert_eq!(
        address(node_addr).await,
        pinned,
        "rotated pin must be recovered with reuse + VSS"
    );
    assert!(
        settled_btc(node_addr).await >= pre_wipe_btc,
        "on-chain funds must be recovered after VSS restore"
    );
    assert_eq!(
        asset_balance_spendable(node_addr, &asset_id).await,
        ISSUE_AMT,
        "asset balance must be recovered from VSS"
    );
}

#[cfg(feature = "vss")]
fn vss_server_available() -> bool {
    std::net::TcpStream::connect_timeout(
        &"127.0.0.1:8081".parse().unwrap(),
        std::time::Duration::from_secs(2),
    )
    .is_ok()
}
