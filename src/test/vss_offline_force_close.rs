use super::*;
use axum::response::IntoResponse;

const TEST_DIR_BASE: &str = "tmp/vss_offline_force_close/";
const VSS_SERVER_URL: &str = "http://127.0.0.1:8081";

#[derive(Clone)]
struct VssProxyState {
    online: Arc<std::sync::atomic::AtomicBool>,
    client: reqwest::Client,
}

/// Reverse proxy in front of the VSS server that can go offline and back
/// online for one node without changing the shared test service.
pub(super) struct VssProxy {
    port: u16,
    online: Arc<std::sync::atomic::AtomicBool>,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
}

impl VssProxy {
    pub(super) fn start() -> Self {
        let std_listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        std_listener.set_nonblocking(true).unwrap();
        let port = std_listener.local_addr().unwrap().port();
        let online = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let state = VssProxyState {
            online: Arc::clone(&online),
            client: reqwest::Client::builder()
                .no_proxy()
                .build()
                .expect("VSS fault proxy client must be constructible"),
        };
        let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(1);
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();

        // Synchronous KVStore calls can stall the shared test runtime, so the
        // fault proxy owns a dedicated runtime.
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async move {
                let listener = TcpListener::from_std(std_listener).unwrap();
                let app = axum::Router::new()
                    .fallback(axum::routing::any(forward_vss_request))
                    .with_state(state);
                ready_tx.send(()).unwrap();
                axum::serve(listener, app)
                    .with_graceful_shutdown(async {
                        let _ = shutdown_rx.await;
                    })
                    .await
                    .unwrap();
            });
        });
        ready_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("VSS fault proxy did not become ready");

        Self {
            port,
            online,
            shutdown: Some(shutdown_tx),
        }
    }

    pub(super) fn url(&self) -> String {
        format!("http://127.0.0.1:{}/vss", self.port)
    }

    pub(super) fn go_offline(&self) {
        self.online
            .store(false, std::sync::atomic::Ordering::Release);
    }

    pub(super) fn go_online(&self) {
        self.online
            .store(true, std::sync::atomic::Ordering::Release);
    }
}

impl Drop for VssProxy {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
    }
}

async fn forward_vss_request(
    axum::extract::State(state): axum::extract::State<VssProxyState>,
    method: axum::http::Method,
    uri: axum::http::Uri,
    mut headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> axum::response::Response {
    if !state.online.load(std::sync::atomic::Ordering::Acquire) {
        return (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            "VSS fault proxy is offline",
        )
            .into_response();
    }

    headers.remove(axum::http::header::HOST);
    headers.remove(axum::http::header::CONTENT_LENGTH);
    headers.remove(axum::http::header::CONNECTION);
    let path_and_query = uri
        .path_and_query()
        .map(|value| value.as_str())
        .unwrap_or("/");
    let upstream = state
        .client
        .request(method, format!("{VSS_SERVER_URL}{path_and_query}"))
        .headers(headers)
        .body(body)
        .send()
        .await;

    let response = match upstream {
        Ok(response) => response,
        Err(error) => {
            eprintln!("VSS fault proxy upstream request failed: {error}");
            return (
                axum::http::StatusCode::BAD_GATEWAY,
                format!("VSS upstream request failed: {error}"),
            )
                .into_response();
        }
    };
    let status = response.status();
    let mut response_headers = response.headers().clone();
    response_headers.remove(axum::http::header::CONTENT_LENGTH);
    response_headers.remove(axum::http::header::CONNECTION);
    response_headers.remove(axum::http::header::TRANSFER_ENCODING);
    match response.bytes().await {
        Ok(body) => {
            let mut downstream = (status, body).into_response();
            downstream.headers_mut().extend(response_headers);
            downstream
        }
        Err(error) => (
            axum::http::StatusCode::BAD_GATEWAY,
            format!("VSS upstream response failed: {error}"),
        )
            .into_response(),
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
