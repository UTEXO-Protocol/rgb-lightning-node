use super::*;

const TEST_DIR_BASE: &str = "tmp/private_multihop/";

/// Regression test: a multihop BTC payment through a private channel succeeds when
/// the intermediate node has `accept_forwards_to_priv_channels = true`.
///
/// Topology:
///   Node 1 --(public channel)--> Node 2 --(private channel)--> Node 3
///
/// Node 2 is the intermediate hop. LDK defaults `accept_forwards_to_priv_channels`
/// to `false` in `UserConfig`, which causes it to reject any HTLC forwarded into a
/// private channel with `LocalHTLCFailureReason::PrivateChannelForward`.
///
/// In `src/ldk.rs` this LDK flag is read from `Config::channels::accept_forwards_to_priv_channels`
/// (also OR-ed with `enable_virtual_channels_v0`). This test starts Node 2 with
/// `config.channels.accept_forwards_to_priv_channels = true` directly, proving that
/// the config path is wired correctly and the payment succeeds.
///
/// The tracked fix is: set `accept_forwards_to_priv_channels = true` as the default
/// in `src/config/mod.rs` (or unconditionally in `src/ldk.rs`) so that forwarding
/// through private channels works out of the box without explicit configuration.
#[serial_test::serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn private_multihop() {
    initialize();

    let test_dir_node1 = format!("{TEST_DIR_BASE}node1");
    let test_dir_node2 = format!("{TEST_DIR_BASE}node2");
    let test_dir_node3 = format!("{TEST_DIR_BASE}node3");

    // Use dynamically allocated ports to avoid collisions with ports from other
    // tests that may still be in teardown when this test starts.
    let port1 = next_peer_port();
    let port2 = next_peer_port();
    let port3 = next_peer_port();

    let (node1_addr, _) = start_node(&test_dir_node1, port1, false).await;

    // Node 2 is started with config.channels.accept_forwards_to_priv_channels = true
    // so that LDK's UserConfig::accept_forwards_to_priv_channels is set to true on its
    // ChannelManager. Without this, Node 2 would reject the HTLC forward destined for
    // the private channel to Node 3 with PrivateChannelForward, causing Node 1 to see
    // a RouteNotFound failure.
    let (node2_addr, _) = start_node_with_accept_forwards_to_priv(&test_dir_node2, port2).await;

    let (node3_addr, _) = start_node(&test_dir_node3, port3, false).await;

    fund_and_create_utxos(node1_addr, None).await;
    fund_and_create_utxos(node2_addr, None).await;
    fund_and_create_utxos(node3_addr, None).await;

    let node2_info = node_info(node2_addr).await;
    let node3_info = node_info(node3_addr).await;
    let node2_pubkey = node2_info.pubkey;
    let node3_pubkey = node3_info.pubkey;

    // Open a PRIVATE channel from Node 2 to Node 3 (B -> C).
    // Because the channel is private, the invoice from Node 3 carries route hints
    // so Node 1 can construct a path through Node 2.
    println!("Opening private channel: Node 2 -> Node 3");
    open_channel_funded_raw(
        node2_addr,
        &node3_pubkey,
        Some(port3),
        Some(500000),
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        true,  // with_anchors
        false, // public = false (private channel)
    )
    .await
    .expect("private channel Node2->Node3 should open");

    // Open a PUBLIC channel from Node 1 to Node 2 (A -> B).
    println!("Opening public channel: Node 1 -> Node 2");
    open_channel_funded_raw(
        node1_addr,
        &node2_pubkey,
        Some(port2),
        Some(500000),
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        true, // with_anchors
        true, // public = true
    )
    .await
    .expect("public channel Node1->Node2 should open");

    // Node 3 generates a BTC-only invoice. Because the B->C channel is private,
    // LDK automatically embeds route hints so Node 1 can construct the full path.
    let LNInvoiceResponse { invoice } =
        ln_invoice(node3_addr, Some(3000000), None, None, 900).await;

    // Node 1 pays Node 3 via Node 2. This succeeds because Node 2 was started with
    // accept_forwards_to_priv_channels = true in its Config.
    // send_payment waits for HTLCStatus::Succeeded and panics if the payment fails.
    println!("Sending payment: Node 1 -> Node 2 -> Node 3");
    send_payment(node1_addr, invoice).await;

    println!("Private multihop payment settled successfully.");

    shutdown(&[node1_addr, node2_addr, node3_addr]).await;
}

/// Start a node with `config.channels.accept_forwards_to_priv_channels = true`.
///
/// This exercises the `[channels] accept_forwards_to_priv_channels` configuration
/// path in `src/config/mod.rs`, which is consumed in `src/ldk.rs` to set
/// `UserConfig::accept_forwards_to_priv_channels` on the LDK ChannelManager.
async fn start_node_with_accept_forwards_to_priv(
    node_test_dir: &str,
    node_peer_port: u16,
) -> (SocketAddr, String) {
    println!("starting node with peer port {node_peer_port} (accept_forwards_to_priv_channels=true)");

    if Path::new(node_test_dir).is_dir() {
        std::fs::remove_dir_all(node_test_dir).unwrap();
    }
    let listener = TcpListener::bind("0.0.0.0:0").await.unwrap();
    let node_address = listener.local_addr().unwrap();
    std::fs::create_dir_all(node_test_dir).unwrap();

    let mut config = crate::config::Config::default();
    config.channels.accept_forwards_to_priv_channels = true;

    let args = UserArgs {
        storage_dir_path: node_test_dir.into(),
        ldk_peer_listening_port: node_peer_port,
        config,
        ..Default::default()
    };

    let (router, app_state) = app(args).await.unwrap();
    register_app_state(node_address, Arc::clone(&app_state));
    tokio::spawn(async move {
        axum::serve(listener, router)
            .with_graceful_shutdown(shutdown_signal(app_state))
            .await
            .unwrap();
    });

    let password = format!("{node_test_dir}.{node_peer_port}");
    init(node_address, &password, None).await;
    unlock(node_address, &password).await;
    wait_for_peer_port_ready(node_peer_port).await;

    println!("node on peer port {node_peer_port} started with address {node_address:?}");
    (node_address, password)
}
