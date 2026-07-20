use super::*;

const TEST_DIR_BASE: &str = "tmp/vss_offline_force_close/";
const VSS_SERVER_ADDR: &str = "127.0.0.1:8081";

/// TCP proxy in front of the VSS server that can go offline and back online,
/// simulating a VSS outage for a single node without touching the shared
/// service. Offline: established connections are cut and new ones are closed
/// on accept.
pub(super) struct VssProxy {
    port: u16,
    online: tokio::sync::watch::Sender<bool>,
}

impl VssProxy {
    // The proxy gets a dedicated thread + runtime: the shared test runtime can
    // stall for seconds on synchronous KVStore calls and would starve it.
    pub(super) fn start() -> Self {
        let std_listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        std_listener.set_nonblocking(true).unwrap();
        let port = std_listener.local_addr().unwrap().port();
        let (online, rx) = tokio::sync::watch::channel(true);
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async move {
                let listener = TcpListener::from_std(std_listener).unwrap();
                loop {
                    let Ok((mut inbound, _)) = listener.accept().await else {
                        break;
                    };
                    if !*rx.borrow() {
                        continue; // offline: drop the connection immediately
                    }
                    let mut rx_conn = rx.clone();
                    tokio::spawn(async move {
                        let Ok(mut outbound) = TcpStream::connect(VSS_SERVER_ADDR).await else {
                            return;
                        };
                        tokio::select! {
                            _ = tokio::io::copy_bidirectional(&mut inbound, &mut outbound) => {}
                            _ = rx_conn.wait_for(|online| !online) => {}
                        }
                    });
                }
            });
        });
        Self { port, online }
    }

    pub(super) fn url(&self) -> String {
        format!("http://127.0.0.1:{}/vss", self.port)
    }

    pub(super) fn go_offline(&self) {
        self.online.send(false).unwrap();
    }

    pub(super) fn go_online(&self) {
        self.online.send(true).unwrap();
    }
}

fn node_log_contains(node_test_dir: &str, needle: &str) -> bool {
    let Ok(entries) = std::fs::read_dir(node_test_dir) else {
        return false;
    };
    for entry in entries.flatten() {
        let log_file = entry.path().join(LOGS_DIR).join(LDK_LOGS_FILE);
        if let Ok(content) = std::fs::read_to_string(&log_file) {
            if content.contains(needle) {
                return true;
            }
        }
    }
    false
}

/// A VSS outage while an HTLC is in flight must pause the channel, not wedge
/// it: node2 (payee, VSS-replicated) cannot persist monitor updates while VSS
/// is down, so the payment from node1 (no VSS) hangs Pending; once VSS is back
/// the pending writes go through, the payment settles and the channel stays
/// open and usable — no force close, no restart. Regression test for the bug
/// where a failed monitor persist was never retried, permanently stalling the
/// channel until node1 force-closed at HTLC expiry.
#[serial_test::serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[traced_test]
async fn vss_outage_payment_recovers_without_force_close() {
    tokio::time::timeout(
        std::time::Duration::from_secs(300),
        vss_outage_payment_recovers_without_force_close_inner(),
    )
    .await
    .expect("vss_outage_payment_recovers_without_force_close timed out");
}

async fn vss_outage_payment_recovers_without_force_close_inner() {
    initialize();

    let test_dir_node1 = format!("{TEST_DIR_BASE}node1");
    let test_dir_node2 = format!("{TEST_DIR_BASE}node2");

    let proxy = VssProxy::start();

    let (node1_addr, _) = start_node(&test_dir_node1, NODE1_PEER_PORT, false).await;
    let (node2_addr, _, _) = start_node_with_vss(
        &test_dir_node2,
        NODE2_PEER_PORT,
        false,
        &proxy.url(),
        None,
        false,
    )
    .await;

    fund_and_create_utxos(node1_addr, None).await;
    fund_and_create_utxos(node2_addr, None).await;

    let node2_pubkey = node_info(node2_addr).await.pubkey;
    connect_peer(
        node1_addr,
        &node2_pubkey,
        &format!("127.0.0.1:{NODE2_PEER_PORT}"),
    )
    .await;

    let channel = open_channel(
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
    wait_for_usable_channels(node2_addr, 1).await;

    let LNInvoiceResponse { invoice } =
        ln_invoice(node2_addr, Some(50_000_000), None, None, 86400).await;

    proxy.go_offline();

    let send = send_payment_raw(node1_addr, invoice.clone()).await;
    let payment_hash = send.payment_hash.unwrap();

    // Long enough for node2 to exhaust the VSS client retries and enter the
    // outage retry loop.
    tokio::time::sleep(std::time::Duration::from_secs(25)).await;

    assert!(
        check_payment_status_by_type(
            node1_addr,
            &payment_hash,
            PaymentType::Outbound,
            HTLCStatus::Pending,
        )
        .await
        .is_some(),
        "payment must be Pending while node2 cannot persist to VSS"
    );
    assert!(
        list_channels(node1_addr)
            .await
            .iter()
            .any(|c| c.channel_id == channel.channel_id),
        "channel must stay open during the outage"
    );
    assert!(
        !node_log_contains(&test_dir_node2, "Failed to persist new ChannelMonitor"),
        "monitor persistence must never fail permanently during an outage"
    );

    proxy.go_online();

    // Pending writes go through and the stalled commitment dance completes.
    wait_for_ln_payment(node1_addr, &payment_hash, HTLCStatus::Succeeded).await;

    assert!(
        !node_log_contains(&test_dir_node1, "Force-closing channel"),
        "node1 must not force-close once VSS is back"
    );
    assert!(
        !node_log_contains(&test_dir_node2, "Failed to persist new ChannelMonitor"),
        "monitor persistence must have recovered, not failed"
    );

    // The channel is still open and fully usable: route a second payment.
    let LNInvoiceResponse { invoice } =
        ln_invoice(node2_addr, Some(10_000_000), None, None, 86400).await;
    send_payment(node1_addr, invoice).await;
}

async fn spendable_sats(node_address: SocketAddr) -> u64 {
    let balance = btc_balance(node_address).await;
    balance.vanilla.spendable + balance.colored.spendable
}

// Generous budget: maturity needs 144+ blocks, then the monitor holds the
// output for ANTI_REORG_DELAY, the sweeper acts on ~30s background ticks and
// its tx is locktimed to broadcast-height + 1.
async fn wait_for_spendable_above(node_address: SocketAddr, threshold: u64, what: &str) {
    for _ in 0..70 {
        mine_n_blocks(false, 20);
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        let spendable = spendable_sats(node_address).await;
        if spendable > threshold {
            return;
        }
        println!("{what}: spendable {spendable} <= {threshold}, waiting");
    }
    panic!("{what}: spendable did not exceed {threshold}");
}

/// If the outage outlasts the HTLC expiry the payer force-closes anyway (the
/// protocol backstop). Funds must still be recovered afterwards: node1 sweeps
/// its commitment output back to spendable balance once the to_self_delay
/// (144 blocks) matures, and node2 claims its pushed balance once VSS is back.
#[serial_test::serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[traced_test]
async fn vss_outage_force_close_funds_are_swept() {
    tokio::time::timeout(
        std::time::Duration::from_secs(900),
        vss_outage_force_close_funds_are_swept_inner(),
    )
    .await
    .expect("vss_outage_force_close_funds_are_swept timed out");
}

async fn vss_outage_force_close_funds_are_swept_inner() {
    initialize();

    let test_dir_node1 = format!("{TEST_DIR_BASE}fc_node1");
    let test_dir_node2 = format!("{TEST_DIR_BASE}fc_node2");

    let proxy = VssProxy::start();

    let (node1_addr, _) = start_node(&test_dir_node1, NODE1_PEER_PORT, false).await;
    let (node2_addr, _, _) = start_node_with_vss(
        &test_dir_node2,
        NODE2_PEER_PORT,
        false,
        &proxy.url(),
        None,
        false,
    )
    .await;

    fund_and_create_utxos(node1_addr, None).await;
    fund_and_create_utxos(node2_addr, None).await;

    let node2_pubkey = node_info(node2_addr).await.pubkey;
    connect_peer(
        node1_addr,
        &node2_pubkey,
        &format!("127.0.0.1:{NODE2_PEER_PORT}"),
    )
    .await;

    let channel = open_channel(
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
    wait_for_usable_channels(node2_addr, 1).await;

    let LNInvoiceResponse { invoice } =
        ln_invoice(node2_addr, Some(50_000_000), None, None, 86400).await;

    // VSS goes offline and never comes back before the HTLC expires.
    proxy.go_offline();
    send_payment_raw(node1_addr, invoice).await;
    tokio::time::sleep(std::time::Duration::from_secs(20)).await;

    let node1_spendable_stuck = spendable_sats(node1_addr).await;
    let node2_spendable_stuck = spendable_sats(node2_addr).await;

    // Mine past the HTLC expiry; node1 force-closes.
    let mut force_closed = false;
    for _ in 0..30 {
        mine_n_blocks(false, 20);
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        let still_open = list_channels(node1_addr)
            .await
            .iter()
            .any(|c| c.channel_id == channel.channel_id);
        if !still_open {
            force_closed = true;
            break;
        }
    }
    assert!(force_closed, "node1 must force-close once the HTLC expires");
    assert!(
        node_log_contains(&test_dir_node1, "Force-closing channel"),
        "node1 must have logged the force close"
    );

    // node1 (no VSS) must sweep its commitment output back to balance once the
    // 144-block to_self_delay matures: ~500k sat (600k capacity - 100k pushed -
    // commitment/sweep fees).
    wait_for_spendable_above(
        node1_addr,
        node1_spendable_stuck + 400_000,
        "node1 post-force-close sweep",
    )
    .await;

    // node2 recovers its pushed balance too (VSS back on; whether the claim
    // already happened during the outage is not part of the contract).
    proxy.go_online();
    wait_for_spendable_above(
        node2_addr,
        node2_spendable_stuck + 50_000,
        "node2 pushed-balance claim",
    )
    .await;
}
