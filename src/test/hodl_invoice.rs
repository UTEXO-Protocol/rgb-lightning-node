use super::*;

const TEST_DIR_BASE: &str = "tmp/hodl_invoice/";

#[serial_test::serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[traced_test]
async fn auto_claim_invoice_regression() {
    initialize();

    let (node1_addr, node2_addr, _test_dir_node1, _test_dir_node2) =
        setup_two_nodes_with_channel("autoclaim", 50).await;

    let LNInvoiceResponse { invoice } = ln_invoice(node2_addr, Some(25_000), None, None, 120).await;
    let decoded = decode_ln_invoice(node1_addr, &invoice).await;

    let _ = send_payment_with_status(node1_addr, invoice.clone(), HTLCStatus::Succeeded).await;
    let payer_payment =
        wait_for_ln_payment(node1_addr, &decoded.payment_hash, HTLCStatus::Succeeded).await;
    assert_eq!(payer_payment.status, HTLCStatus::Succeeded);
    let payee_payment =
        wait_for_ln_payment(node2_addr, &decoded.payment_hash, HTLCStatus::Succeeded).await;
    assert_eq!(payee_payment.status, HTLCStatus::Succeeded);
    assert!(matches!(
        invoice_status(node2_addr, &invoice).await,
        InvoiceStatus::Succeeded
    ));
}
#[serial_test::serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[traced_test]
async fn cancel_hodl_invoice() {
    initialize();

    let (node1_addr, node2_addr, _test_dir_node1, test_dir_node2) =
        setup_two_nodes_with_channel("cancel", 10).await;

    let (_preimage_hex, payment_hash_hex) = random_preimage_and_hash();
    let InvoiceHodlResponse { invoice, .. } = invoice_hodl(
        node2_addr,
        Some(40_000),
        120,
        payment_hash_hex.clone(),
        None,
        None,
    )
    .await;
    let decoded = decode_ln_invoice(node1_addr, &invoice).await;
    assert_eq!(decoded.payment_hash, payment_hash_hex);

    let _ = send_payment_with_status(node1_addr, invoice.clone(), HTLCStatus::Pending).await;
    assert!(matches!(
        invoice_status(node2_addr, &invoice).await,
        InvoiceStatus::Pending
    ));
    expect_api_ok(
        wait_for_claimable_state(&test_dir_node2, &payment_hash_hex, true).await,
        "wait for claimable entry to appear",
    );
    let payee_payment =
        wait_for_ln_payment(node2_addr, &decoded.payment_hash, HTLCStatus::Claimable).await;
    assert_eq!(payee_payment.status, HTLCStatus::Claimable);

    invoice_cancel(node2_addr, payment_hash_hex.clone()).await;

    let payer_payment =
        wait_for_ln_payment(node1_addr, &decoded.payment_hash, HTLCStatus::Failed).await;
    assert_eq!(payer_payment.status, HTLCStatus::Failed);
    let payee_payment =
        wait_for_ln_payment(node2_addr, &decoded.payment_hash, HTLCStatus::Cancelled).await;
    assert_eq!(payee_payment.status, HTLCStatus::Cancelled);
    assert!(matches!(
        invoice_status(node2_addr, &invoice).await,
        InvoiceStatus::Cancelled
    ));
    expect_api_ok(
        wait_for_claimable_state(&test_dir_node2, &payment_hash_hex, false).await,
        "wait for claimable entry to be removed",
    );

    invoice_cancel_expect_error(
        node2_addr,
        payment_hash_hex,
        StatusCode::NOT_FOUND,
        "No claimable HTLC found for this invoice",
        "InvoiceNotClaimable",
    )
    .await;
}

#[serial_test::serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[traced_test]
async fn cancel_hodl_invoice_rgb() {
    initialize();

    let asset_channel_amount = 100;
    let asset_payment_amount = 10;
    let (node1_addr, node2_addr, _test_dir_node1, test_dir_node2, asset_id) =
        setup_two_nodes_with_asset_channel("cancel-rgb", 61, asset_channel_amount).await;

    let initial_ln_rgb_balance_node1 = asset_balance_offchain_outbound(node1_addr, &asset_id).await;
    let initial_ln_rgb_balance_node2 = asset_balance_offchain_outbound(node2_addr, &asset_id).await;

    let (_preimage_hex, payment_hash_hex) = random_preimage_and_hash();
    let InvoiceHodlResponse { invoice, .. } = invoice_hodl(
        node2_addr,
        Some(HTLC_MIN_MSAT),
        120,
        payment_hash_hex.clone(),
        Some(&asset_id),
        Some(asset_payment_amount),
    )
    .await;
    let decoded = decode_ln_invoice(node1_addr, &invoice).await;
    assert_eq!(decoded.payment_hash, payment_hash_hex);
    assert_eq!(decoded.amt_msat, Some(3_000_000));
    assert_eq!(decoded.asset_id, Some(asset_id.to_string()));
    assert_eq!(decoded.asset_amount, Some(asset_payment_amount));

    let _ = send_payment_with_status(node1_addr, invoice.clone(), HTLCStatus::Pending).await;
    assert!(matches!(
        invoice_status(node2_addr, &invoice).await,
        InvoiceStatus::Pending
    ));
    expect_api_ok(
        wait_for_claimable_state(&test_dir_node2, &payment_hash_hex, true).await,
        "wait for claimable entry to appear",
    );
    let payee_payment =
        wait_for_ln_payment(node2_addr, &decoded.payment_hash, HTLCStatus::Claimable).await;
    assert_eq!(payee_payment.status, HTLCStatus::Claimable);
    assert_eq!(payee_payment.asset_id, Some(asset_id.to_string()));
    assert_eq!(payee_payment.asset_amount, Some(asset_payment_amount));

    invoice_cancel(node2_addr, payment_hash_hex.clone()).await;

    let payer_payment =
        wait_for_ln_payment(node1_addr, &decoded.payment_hash, HTLCStatus::Failed).await;
    assert_eq!(payer_payment.status, HTLCStatus::Failed);
    assert_eq!(payer_payment.asset_id, Some(asset_id.to_string()));
    assert_eq!(payer_payment.asset_amount, Some(asset_payment_amount));
    let payee_payment =
        wait_for_ln_payment(node2_addr, &decoded.payment_hash, HTLCStatus::Cancelled).await;
    assert_eq!(payee_payment.status, HTLCStatus::Cancelled);
    assert_eq!(payee_payment.asset_id, Some(asset_id.to_string()));
    assert_eq!(payee_payment.asset_amount, Some(asset_payment_amount));
    assert!(matches!(
        invoice_status(node2_addr, &invoice).await,
        InvoiceStatus::Cancelled
    ));
    expect_api_ok(
        wait_for_claimable_state(&test_dir_node2, &payment_hash_hex, false).await,
        "wait for claimable entry to be removed",
    );

    wait_for_ln_balance(node1_addr, &asset_id, initial_ln_rgb_balance_node1).await;
    wait_for_ln_balance(node2_addr, &asset_id, initial_ln_rgb_balance_node2).await;

    invoice_cancel_expect_error(
        node2_addr,
        payment_hash_hex,
        StatusCode::NOT_FOUND,
        "No claimable HTLC found for this invoice",
        "InvoiceNotClaimable",
    )
    .await;
}

#[serial_test::serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[traced_test]
async fn cancel_then_settle_fails() {
    initialize();

    let (node1_addr, node2_addr, _test_dir_node1, test_dir_node2) =
        setup_two_nodes_with_channel("cancel-settle", 11).await;

    let (preimage_hex, payment_hash_hex) = random_preimage_and_hash();
    let InvoiceHodlResponse { invoice, .. } = invoice_hodl(
        node2_addr,
        Some(40_000),
        120,
        payment_hash_hex.clone(),
        None,
        None,
    )
    .await;
    let decoded = decode_ln_invoice(node1_addr, &invoice).await;

    let _ = send_payment_with_status(node1_addr, invoice.clone(), HTLCStatus::Pending).await;
    expect_api_ok(
        wait_for_claimable_state(&test_dir_node2, &payment_hash_hex, true).await,
        "claimable entry should appear",
    );
    let payee_payment =
        wait_for_ln_payment(node2_addr, &decoded.payment_hash, HTLCStatus::Claimable).await;
    assert_eq!(payee_payment.status, HTLCStatus::Claimable);

    invoice_cancel(node2_addr, payment_hash_hex.clone()).await;

    invoice_settle_expect_error(
        node2_addr,
        payment_hash_hex.clone(),
        preimage_hex,
        StatusCode::NOT_FOUND,
        "No claimable HTLC found for this invoice",
        "InvoiceNotClaimable",
    )
    .await;

    let payee_payment =
        wait_for_ln_payment(node2_addr, &decoded.payment_hash, HTLCStatus::Cancelled).await;
    assert_eq!(payee_payment.status, HTLCStatus::Cancelled);
}

#[serial_test::serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[traced_test]
async fn cancel_unpaid_invoice_fails() {
    initialize();

    let test_dir_base = format!("{TEST_DIR_BASE}cancel_unpaid/");
    let (node1_addr, _test_dir_node1) =
        setup_single_node(&test_dir_base, "node1", NODE1_PEER_PORT + 41).await;

    let (_preimage_hex, payment_hash_hex) = random_preimage_and_hash();
    let InvoiceHodlResponse { invoice, .. } = invoice_hodl(
        node1_addr,
        Some(20_000),
        120,
        payment_hash_hex.clone(),
        None,
        None,
    )
    .await;

    assert!(matches!(
        invoice_status(node1_addr, &invoice).await,
        InvoiceStatus::Pending
    ));
}

#[serial_test::serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[traced_test]
async fn cancel_while_settling_fails() {
    initialize();

    let (node1_addr, node2_addr, _test_dir_node1, test_dir_node2) =
        setup_two_nodes_with_channel("cancel-while-settling", 13).await;

    let (preimage_hex, payment_hash_hex) = random_preimage_and_hash();
    let InvoiceHodlResponse { invoice, .. } = invoice_hodl(
        node2_addr,
        Some(42_000),
        120,
        payment_hash_hex.clone(),
        None,
        None,
    )
    .await;
    let decoded = decode_ln_invoice(node1_addr, &invoice).await;

    let _ = send_payment_with_status(node1_addr, invoice.clone(), HTLCStatus::Pending).await;
    expect_api_ok(
        wait_for_claimable_state(&test_dir_node2, &payment_hash_hex, true).await,
        "claimable entry should appear",
    );
    let payee_payment =
        wait_for_ln_payment(node2_addr, &decoded.payment_hash, HTLCStatus::Claimable).await;
    assert_eq!(payee_payment.status, HTLCStatus::Claimable);

    invoice_settle(node2_addr, payment_hash_hex.clone(), preimage_hex).await;
    expect_api_ok(
        wait_for_claimable_settling(&test_dir_node2, &payment_hash_hex).await,
        "claimable entry should be marked settling",
    );

    let payload = InvoiceCancelRequest {
        payment_hash: payment_hash_hex.clone(),
    };
    let res = reqwest::Client::new()
        .post(format!("http://{node2_addr}/cancelhodlinvoice"))
        .json(&payload)
        .send()
        .await
        .unwrap();

    if res.status() == StatusCode::FORBIDDEN {
        check_response_is_nok(
            res,
            StatusCode::FORBIDDEN,
            "Invoice settlement is in progress",
            "InvoiceSettlingInProgress",
        )
        .await;
    } else if res.status() == StatusCode::CONFLICT {
        check_response_is_nok(
            res,
            StatusCode::CONFLICT,
            "Invoice is already settled",
            "InvoiceAlreadySettled",
        )
        .await;
    } else {
        let status = res.status();
        let body = res.text().await.unwrap_or_default();
        panic!("expected 403 settling-in-progress or 409 already settled, got {status}: {body}");
    }

    let payee_payment =
        wait_for_ln_payment(node2_addr, &decoded.payment_hash, HTLCStatus::Succeeded).await;
    assert_eq!(payee_payment.status, HTLCStatus::Succeeded);
}

fn claimable_exists(node_test_dir: &str, payment_hash_hex: &str) -> Result<bool, APIError> {
    let claimable_path = Path::new(node_test_dir)
        .join(LDK_DIR)
        .join(CLAIMABLE_HTLCS_FNAME);
    let storage = read_claimable_htlcs(&claimable_path);
    let hash = validate_and_parse_payment_hash(payment_hash_hex)?;
    Ok(storage.payments.contains_key(&hash))
}

fn claimable_is_settling(node_test_dir: &str, payment_hash_hex: &str) -> Result<bool, APIError> {
    let claimable_path = Path::new(node_test_dir)
        .join(LDK_DIR)
        .join(CLAIMABLE_HTLCS_FNAME);
    let storage = read_claimable_htlcs(&claimable_path);
    let hash = validate_and_parse_payment_hash(payment_hash_hex)?;
    Ok(storage
        .payments
        .get(&hash)
        .and_then(|c| c.settling)
        .unwrap_or(false))
}

fn expect_api_ok<T>(result: Result<T, APIError>, context: &str) -> T {
    match result {
        Ok(value) => value,
        Err(err) => panic!("{context}: {err}"),
    }
}

#[serial_test::serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[traced_test]
async fn expire_hodl_invoice() {
    initialize();

    let (node1_addr, node2_addr, _test_dir_node1, test_dir_node2) =
        setup_two_nodes_with_channel("expiry", 20).await;

    let (_preimage_hex, payment_hash_hex) = random_preimage_and_hash();
    let InvoiceHodlResponse { invoice, .. } = invoice_hodl(
        node2_addr,
        Some(30_000),
        20,
        payment_hash_hex.clone(),
        None,
        None,
    )
    .await;
    let decoded = decode_ln_invoice(node1_addr, &invoice).await;
    assert_eq!(decoded.payment_hash, payment_hash_hex);

    let _ = send_payment_with_status(node1_addr, invoice.clone(), HTLCStatus::Pending).await;
    expect_api_ok(
        wait_for_claimable_state(&test_dir_node2, &payment_hash_hex, true).await,
        "wait for claimable entry to appear",
    );
    let payee_payment =
        wait_for_ln_payment(node2_addr, &decoded.payment_hash, HTLCStatus::Claimable).await;
    assert_eq!(payee_payment.status, HTLCStatus::Claimable);

    let payer_payment =
        wait_for_ln_payment(node1_addr, &decoded.payment_hash, HTLCStatus::Failed).await;
    assert_eq!(payer_payment.status, HTLCStatus::Failed);
    let payee_payment =
        wait_for_ln_payment(node2_addr, &decoded.payment_hash, HTLCStatus::Failed).await;
    assert_eq!(payee_payment.status, HTLCStatus::Failed);
    assert!(matches!(
        invoice_status(node2_addr, &invoice).await,
        InvoiceStatus::Failed
    ));
    expect_api_ok(
        wait_for_claimable_state(&test_dir_node2, &payment_hash_hex, false).await,
        "wait for claimable entry to be removed",
    );

    invoice_settle_expect_error(
        node2_addr,
        payment_hash_hex.clone(),
        _preimage_hex,
        StatusCode::NOT_FOUND,
        "No claimable HTLC found for this invoice",
        "InvoiceNotClaimable",
    )
    .await;
    invoice_cancel_expect_error(
        node2_addr,
        payment_hash_hex,
        StatusCode::NOT_FOUND,
        "No claimable HTLC found for this invoice",
        "InvoiceNotClaimable",
    )
    .await;
}

#[serial_test::serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[traced_test]
async fn expire_hodl_invoice_by_blocks() {
    initialize();

    let (node1_addr, node2_addr, _test_dir_node1, test_dir_node2) =
        setup_two_nodes_with_channel("expiry-blocks", 25).await;

    let (_preimage_hex, payment_hash_hex) = random_preimage_and_hash();
    let InvoiceHodlResponse { invoice, .. } = invoice_hodl(
        node2_addr,
        Some(30_000),
        900,
        payment_hash_hex.clone(),
        None,
        None,
    )
    .await;
    let decoded = decode_ln_invoice(node1_addr, &invoice).await;
    assert_eq!(decoded.payment_hash, payment_hash_hex);

    let _ = send_payment_with_status(node1_addr, invoice.clone(), HTLCStatus::Pending).await;
    expect_api_ok(
        wait_for_claimable_state(&test_dir_node2, &payment_hash_hex, true).await,
        "wait for claimable entry to appear",
    );
    let payee_payment =
        wait_for_ln_payment(node2_addr, &decoded.payment_hash, HTLCStatus::Claimable).await;
    assert_eq!(payee_payment.status, HTLCStatus::Claimable);

    let claimable_path = Path::new(&test_dir_node2)
        .join(LDK_DIR)
        .join(CLAIMABLE_HTLCS_FNAME);
    let storage = read_claimable_htlcs(&claimable_path);
    let hash = validate_and_parse_payment_hash(&payment_hash_hex).unwrap();
    let deadline_height = storage
        .payments
        .get(&hash)
        .and_then(|c| c.claim_deadline_height)
        .unwrap_or(0);

    let current_height = super::get_block_count();
    let blocks_to_mine = deadline_height.saturating_sub(current_height) + 2;
    super::mine_n_blocks(false, blocks_to_mine as u16);

    let payer_payment = expect_api_ok(
        wait_for_ln_payment_with_timeout(
            node1_addr,
            &decoded.payment_hash,
            HTLCStatus::Failed,
            std::time::Duration::from_secs(60),
        )
        .await,
        "wait for payer payment to fail after block-based expiry",
    );
    assert_eq!(payer_payment.status, HTLCStatus::Failed);
    let payee_payment = expect_api_ok(
        wait_for_ln_payment_with_timeout(
            node2_addr,
            &decoded.payment_hash,
            HTLCStatus::Failed,
            std::time::Duration::from_secs(60),
        )
        .await,
        "wait for payee payment to fail after block-based expiry",
    );
    assert_eq!(payee_payment.status, HTLCStatus::Failed);
    assert!(matches!(
        invoice_status(node2_addr, &invoice).await,
        InvoiceStatus::Failed
    ));
    expect_api_ok(
        wait_for_claimable_state(&test_dir_node2, &payment_hash_hex, false).await,
        "wait for claimable entry to be removed",
    );

    invoice_settle_expect_error(
        node2_addr,
        payment_hash_hex.clone(),
        _preimage_hex,
        StatusCode::NOT_FOUND,
        "No claimable HTLC found for this invoice",
        "InvoiceNotClaimable",
    )
    .await;
    invoice_cancel_expect_error(
        node2_addr,
        payment_hash_hex,
        StatusCode::NOT_FOUND,
        "No claimable HTLC found for this invoice",
        "InvoiceNotClaimable",
    )
    .await;
}

async fn invoice_cancel_expect_error(
    node_address: SocketAddr,
    payment_hash: String,
    expected_status: StatusCode,
    expected_message: &str,
    expected_name: &str,
) {
    println!("cancelling HODL invoice {payment_hash} on node {node_address}");
    let payload = InvoiceCancelRequest { payment_hash };
    post_and_check_error_response(
        node_address,
        "/cancelhodlinvoice",
        &payload,
        expected_status,
        expected_message,
        expected_name,
    )
    .await;
}

async fn invoice_settle_expect_error(
    node_address: SocketAddr,
    payment_hash: String,
    payment_preimage: String,
    expected_status: StatusCode,
    expected_message: &str,
    expected_name: &str,
) {
    println!("settling HODL invoice {payment_hash} on node {node_address}");
    let payload = InvoiceSettleRequest {
        payment_hash,
        payment_preimage,
    };
    post_and_check_error_response(
        node_address,
        "/settlehodlinvoice",
        &payload,
        expected_status,
        expected_message,
        expected_name,
    )
    .await;
}

#[serial_test::serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[traced_test]
async fn reject_duplicate_hodl_payment_hash() {
    initialize();

    let test_dir_base = format!("{TEST_DIR_BASE}duplicate_hash/");
    let (node1_addr, _test_dir_node1) =
        setup_single_node(&test_dir_base, "node1", NODE1_PEER_PORT + 40).await;

    let (_preimage_hex, payment_hash_hex) = random_preimage_and_hash();
    let InvoiceHodlResponse { invoice, .. } = invoice_hodl(
        node1_addr,
        Some(20_000),
        120,
        payment_hash_hex.clone(),
        None,
        None,
    )
    .await;

    let payload = InvoiceHodlRequest {
        amt_msat: Some(10_000),
        expiry_sec: 60,
        asset_id: None,
        asset_amount: None,
        payment_hash: payment_hash_hex.clone(),
        external_ref: None,
    };
    post_and_check_error_response(
        node1_addr,
        "/hodlinvoice",
        &payload,
        StatusCode::BAD_REQUEST,
        "Payment hash already used",
        "PaymentHashAlreadyUsed",
    )
    .await;

    assert!(matches!(
        invoice_status(node1_addr, &invoice).await,
        InvoiceStatus::Pending
    ));
}

#[serial_test::serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[traced_test]
async fn reject_wrong_preimage_settle() {
    initialize();

    let (node1_addr, node2_addr, _test_dir_node1, test_dir_node2) =
        setup_two_nodes_with_channel("wrong_preimage", 30).await;

    let (good_preimage_hex, payment_hash_hex) = random_preimage_and_hash();
    let InvoiceHodlResponse { invoice, .. } = invoice_hodl(
        node2_addr,
        Some(35_000),
        120,
        payment_hash_hex.clone(),
        None,
        None,
    )
    .await;
    let decoded = decode_ln_invoice(node1_addr, &invoice).await;
    assert_eq!(decoded.payment_hash, payment_hash_hex);

    let _ = send_payment_with_status(node1_addr, invoice.clone(), HTLCStatus::Pending).await;
    expect_api_ok(
        wait_for_claimable_state(&test_dir_node2, &payment_hash_hex, true).await,
        "wait for claimable entry to appear",
    );
    let payee_payment =
        wait_for_ln_payment(node2_addr, &decoded.payment_hash, HTLCStatus::Claimable).await;
    assert_eq!(payee_payment.status, HTLCStatus::Claimable);

    let (wrong_preimage_hex, _) = random_preimage_and_hash();
    invoice_settle_expect_error(
        node2_addr,
        payment_hash_hex.clone(),
        wrong_preimage_hex,
        StatusCode::BAD_REQUEST,
        "Invalid payment preimage",
        "InvalidPaymentPreimage",
    )
    .await;

    assert!(matches!(
        invoice_status(node2_addr, &invoice).await,
        InvoiceStatus::Pending
    ));
    expect_api_ok(
        wait_for_claimable_state(&test_dir_node2, &payment_hash_hex, true).await,
        "wait for claimable entry to remain",
    );
    let payee_payment =
        wait_for_ln_payment(node2_addr, &decoded.payment_hash, HTLCStatus::Claimable).await;
    assert_eq!(payee_payment.status, HTLCStatus::Claimable);

    invoice_settle(node2_addr, payment_hash_hex.clone(), good_preimage_hex).await;
    let _ = wait_for_ln_payment(node2_addr, &decoded.payment_hash, HTLCStatus::Succeeded).await;
    assert!(matches!(
        invoice_status(node2_addr, &invoice).await,
        InvoiceStatus::Succeeded
    ));
}

#[serial_test::serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[traced_test]
async fn settle_after_expiry_idempotent_succeeds() {
    initialize();

    let (node1_addr, node2_addr, _test_dir_node1, test_dir_node2) =
        setup_two_nodes_with_channel("settle-after-expiry", 7).await;

    let (preimage_hex, payment_hash_hex) = random_preimage_and_hash();
    let InvoiceHodlResponse { invoice, .. } = invoice_hodl(
        node2_addr,
        Some(45_000),
        10,
        payment_hash_hex.clone(),
        None,
        None,
    )
    .await;
    let decoded = decode_ln_invoice(node1_addr, &invoice).await;

    let _ = send_payment_with_status(node1_addr, invoice.clone(), HTLCStatus::Pending).await;
    expect_api_ok(
        wait_for_claimable_state(&test_dir_node2, &payment_hash_hex, true).await,
        "wait for claimable entry to appear",
    );
    let payee_payment =
        wait_for_ln_payment(node2_addr, &decoded.payment_hash, HTLCStatus::Claimable).await;
    assert_eq!(payee_payment.status, HTLCStatus::Claimable);

    invoice_settle(node2_addr, payment_hash_hex.clone(), preimage_hex.clone()).await;
    let _ = wait_for_ln_payment(node2_addr, &decoded.payment_hash, HTLCStatus::Succeeded).await;

    let invoice_expiry_ts = decoded
        .timestamp
        .saturating_add(decoded.expiry_sec)
        .saturating_add(1);
    let wait_timeout = std::time::Duration::from_secs(decoded.expiry_sec.saturating_add(30));
    assert!(
        wait_timeout > std::time::Duration::ZERO,
        "invoice expiry wait timeout must be > 0"
    );
    let target_ts = i128::from(invoice_expiry_ts);
    let t_0 = std::time::Instant::now();
    loop {
        let now_ts = i128::from(OffsetDateTime::now_utc().unix_timestamp());
        if now_ts >= target_ts {
            break;
        }
        if t_0.elapsed() > wait_timeout {
            panic!("invoice expiry did not pass in time (target: {target_ts}, current: {now_ts})");
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }

    invoice_settle(node2_addr, payment_hash_hex.clone(), preimage_hex.clone()).await;
    let _ = wait_for_ln_payment(node2_addr, &decoded.payment_hash, HTLCStatus::Succeeded).await;
    assert!(matches!(
        invoice_status(node2_addr, &invoice).await,
        InvoiceStatus::Succeeded
    ));
}

#[serial_test::serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[traced_test]
async fn settle_hodl_invoice() {
    initialize();

    let (node1_addr, node2_addr, _test_dir_node1, test_dir_node2) =
        setup_two_nodes_with_channel("settle", 0).await;

    let (preimage_hex, payment_hash_hex) = random_preimage_and_hash();
    let InvoiceHodlResponse { invoice, .. } = invoice_hodl(
        node2_addr,
        Some(50_000),
        120,
        payment_hash_hex.clone(),
        None,
        None,
    )
    .await;
    let decoded = decode_ln_invoice(node1_addr, &invoice).await;
    assert_eq!(decoded.payment_hash, payment_hash_hex);

    let _ = send_payment_with_status(node1_addr, invoice.clone(), HTLCStatus::Pending).await;
    assert!(matches!(
        invoice_status(node2_addr, &invoice).await,
        InvoiceStatus::Pending
    ));
    expect_api_ok(
        wait_for_claimable_state(&test_dir_node2, &payment_hash_hex, true).await,
        "wait for claimable entry to appear",
    );
    let payee_payment =
        wait_for_ln_payment(node2_addr, &decoded.payment_hash, HTLCStatus::Claimable).await;
    assert_eq!(payee_payment.status, HTLCStatus::Claimable);

    invoice_settle(node2_addr, payment_hash_hex.clone(), preimage_hex.clone()).await;

    let payer_payment =
        wait_for_ln_payment(node1_addr, &decoded.payment_hash, HTLCStatus::Succeeded).await;
    assert_eq!(payer_payment.status, HTLCStatus::Succeeded);
    let payee_payment =
        wait_for_ln_payment(node2_addr, &decoded.payment_hash, HTLCStatus::Succeeded).await;
    assert_eq!(payee_payment.status, HTLCStatus::Succeeded);
    assert!(matches!(
        invoice_status(node2_addr, &invoice).await,
        InvoiceStatus::Succeeded
    ));
    expect_api_ok(
        wait_for_claimable_state(&test_dir_node2, &payment_hash_hex, false).await,
        "wait for claimable entry to be removed",
    );

    let preimage_resp = expect_api_ok(
        wait_for_payment_preimage(node1_addr, &payment_hash_hex).await,
        "wait for payment preimage to be available",
    );
    assert_eq!(preimage_resp.status, HTLCStatus::Succeeded);
    assert_eq!(preimage_resp.preimage, Some(preimage_hex));
}

#[serial_test::serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[traced_test]
async fn settle_hodl_invoice_rgb() {
    initialize();

    let asset_channel_amount = 100;
    let asset_payment_amount = 10;
    let (node1_addr, node2_addr, _test_dir_node1, test_dir_node2, asset_id) =
        setup_two_nodes_with_asset_channel("settle-rgb", 60, asset_channel_amount).await;

    let initial_ln_balance_node1 = asset_balance_offchain_outbound(node1_addr, &asset_id).await;
    let initial_ln_balance_node2 = asset_balance_offchain_outbound(node2_addr, &asset_id).await;

    let (preimage_hex, payment_hash_hex) = random_preimage_and_hash();
    let InvoiceHodlResponse { invoice, .. } = invoice_hodl(
        node2_addr,
        Some(HTLC_MIN_MSAT),
        120,
        payment_hash_hex.clone(),
        Some(&asset_id),
        Some(asset_payment_amount),
    )
    .await;
    let decoded = decode_ln_invoice(node1_addr, &invoice).await;
    assert_eq!(decoded.payment_hash, payment_hash_hex);
    assert_eq!(decoded.amt_msat, Some(3_000_000));
    assert_eq!(decoded.asset_id, Some(asset_id.to_string()));
    assert_eq!(decoded.asset_amount, Some(asset_payment_amount));

    let _ = send_payment_with_status(node1_addr, invoice.clone(), HTLCStatus::Pending).await;
    expect_api_ok(
        wait_for_claimable_state(&test_dir_node2, &payment_hash_hex, true).await,
        "wait for claimable entry to appear",
    );
    let payee_payment =
        wait_for_ln_payment(node2_addr, &decoded.payment_hash, HTLCStatus::Claimable).await;
    assert_eq!(payee_payment.status, HTLCStatus::Claimable);
    assert_eq!(payee_payment.asset_id, Some(asset_id.to_string()));
    assert_eq!(payee_payment.asset_amount, Some(asset_payment_amount));

    invoice_settle(node2_addr, payment_hash_hex.clone(), preimage_hex.clone()).await;

    let payer_payment =
        wait_for_ln_payment(node1_addr, &decoded.payment_hash, HTLCStatus::Succeeded).await;
    assert_eq!(payer_payment.status, HTLCStatus::Succeeded);
    assert_eq!(payer_payment.asset_id, Some(asset_id.to_string()));
    assert_eq!(payer_payment.asset_amount, Some(asset_payment_amount));
    let payee_payment =
        wait_for_ln_payment(node2_addr, &decoded.payment_hash, HTLCStatus::Succeeded).await;
    assert_eq!(payee_payment.status, HTLCStatus::Succeeded);
    assert_eq!(payee_payment.asset_id, Some(asset_id.to_string()));
    assert_eq!(payee_payment.asset_amount, Some(asset_payment_amount));
    expect_api_ok(
        wait_for_claimable_state(&test_dir_node2, &payment_hash_hex, false).await,
        "wait for claimable entry to be removed",
    );

    wait_for_ln_balance(
        node1_addr,
        &asset_id,
        initial_ln_balance_node1 - asset_payment_amount,
    )
    .await;
    wait_for_ln_balance(
        node2_addr,
        &asset_id,
        initial_ln_balance_node2 + asset_payment_amount,
    )
    .await;
}

#[serial_test::serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[traced_test]
async fn settle_then_cancel_fails() {
    initialize();

    let (node1_addr, node2_addr, _test_dir_node1, test_dir_node2) =
        setup_two_nodes_with_channel("settle-cancel", 12).await;

    let (preimage_hex, payment_hash_hex) = random_preimage_and_hash();
    let InvoiceHodlResponse { invoice, .. } = invoice_hodl(
        node2_addr,
        Some(42_000),
        120,
        payment_hash_hex.clone(),
        None,
        None,
    )
    .await;
    let decoded = decode_ln_invoice(node1_addr, &invoice).await;

    let _ = send_payment_with_status(node1_addr, invoice.clone(), HTLCStatus::Pending).await;
    expect_api_ok(
        wait_for_claimable_state(&test_dir_node2, &payment_hash_hex, true).await,
        "claimable entry should appear",
    );
    let payee_payment =
        wait_for_ln_payment(node2_addr, &decoded.payment_hash, HTLCStatus::Claimable).await;
    assert_eq!(payee_payment.status, HTLCStatus::Claimable);

    invoice_settle(node2_addr, payment_hash_hex.clone(), preimage_hex).await;
    let payee_payment =
        wait_for_ln_payment(node2_addr, &decoded.payment_hash, HTLCStatus::Succeeded).await;
    assert_eq!(payee_payment.status, HTLCStatus::Succeeded);

    invoice_cancel_expect_error(
        node2_addr,
        payment_hash_hex.clone(),
        StatusCode::CONFLICT,
        "Invoice is already settled",
        "InvoiceAlreadySettled",
    )
    .await;
}

#[serial_test::serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[traced_test]
async fn settle_twice_succeeds() {
    initialize();

    let (node1_addr, node2_addr, _test_dir_node1, test_dir_node2) =
        setup_two_nodes_with_channel("settle-twice", 5).await;

    let (preimage_hex, payment_hash_hex) = random_preimage_and_hash();
    let InvoiceHodlResponse { invoice, .. } = invoice_hodl(
        node2_addr,
        Some(45_000),
        120,
        payment_hash_hex.clone(),
        None,
        None,
    )
    .await;
    let decoded = decode_ln_invoice(node1_addr, &invoice).await;
    assert_eq!(decoded.payment_hash, payment_hash_hex);

    let _ = send_payment_with_status(node1_addr, invoice.clone(), HTLCStatus::Pending).await;
    expect_api_ok(
        wait_for_claimable_state(&test_dir_node2, &payment_hash_hex, true).await,
        "wait for claimable entry to appear",
    );
    let payee_payment =
        wait_for_ln_payment(node2_addr, &decoded.payment_hash, HTLCStatus::Claimable).await;
    assert_eq!(payee_payment.status, HTLCStatus::Claimable);

    invoice_settle(node2_addr, payment_hash_hex.clone(), preimage_hex.clone()).await;

    let payer_payment =
        wait_for_ln_payment(node1_addr, &decoded.payment_hash, HTLCStatus::Succeeded).await;
    assert_eq!(payer_payment.status, HTLCStatus::Succeeded);
    let payee_payment =
        wait_for_ln_payment(node2_addr, &decoded.payment_hash, HTLCStatus::Succeeded).await;
    assert_eq!(payee_payment.status, HTLCStatus::Succeeded);
    assert!(matches!(
        invoice_status(node2_addr, &invoice).await,
        InvoiceStatus::Succeeded
    ));

    invoice_settle(node2_addr, payment_hash_hex.clone(), preimage_hex.clone()).await;
    let _ = wait_for_ln_payment(node2_addr, &decoded.payment_hash, HTLCStatus::Succeeded).await;
}

#[serial_test::serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[traced_test]
async fn settle_twice_wrong_preimage_fails() {
    initialize();

    let (node1_addr, node2_addr, _test_dir_node1, test_dir_node2) =
        setup_two_nodes_with_channel("settle-twice-wrong", 6).await;

    let (preimage_hex, payment_hash_hex) = random_preimage_and_hash();
    let InvoiceHodlResponse { invoice, .. } = invoice_hodl(
        node2_addr,
        Some(45_000),
        120,
        payment_hash_hex.clone(),
        None,
        None,
    )
    .await;
    let decoded = decode_ln_invoice(node1_addr, &invoice).await;

    let _ = send_payment_with_status(node1_addr, invoice.clone(), HTLCStatus::Pending).await;
    expect_api_ok(
        wait_for_claimable_state(&test_dir_node2, &payment_hash_hex, true).await,
        "wait for claimable entry to appear",
    );
    let payee_payment =
        wait_for_ln_payment(node2_addr, &decoded.payment_hash, HTLCStatus::Claimable).await;
    assert_eq!(payee_payment.status, HTLCStatus::Claimable);

    invoice_settle(node2_addr, payment_hash_hex.clone(), preimage_hex.clone()).await;
    let _ = wait_for_ln_payment(node2_addr, &decoded.payment_hash, HTLCStatus::Succeeded).await;

    let (wrong_preimage_hex, _) = random_preimage_and_hash();
    invoice_settle_expect_error(
        node2_addr,
        payment_hash_hex.clone(),
        wrong_preimage_hex,
        StatusCode::BAD_REQUEST,
        "Invalid payment preimage",
        "InvalidPaymentPreimage",
    )
    .await;
    assert!(matches!(
        invoice_status(node2_addr, &invoice).await,
        InvoiceStatus::Succeeded
    ));
}

#[serial_test::serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[traced_test]
async fn settling_while_settling_fails() {
    initialize();

    let (node1_addr, node2_addr, _test_dir_node1, test_dir_node2) =
        setup_two_nodes_with_channel("settling-while-settling", 14).await;

    let (preimage_hex, payment_hash_hex) = random_preimage_and_hash();
    let InvoiceHodlResponse { invoice, .. } = invoice_hodl(
        node2_addr,
        Some(42_000),
        120,
        payment_hash_hex.clone(),
        None,
        None,
    )
    .await;
    let decoded = decode_ln_invoice(node1_addr, &invoice).await;

    let _ = send_payment_with_status(node1_addr, invoice.clone(), HTLCStatus::Pending).await;
    expect_api_ok(
        wait_for_claimable_state(&test_dir_node2, &payment_hash_hex, true).await,
        "claimable entry should appear",
    );
    let payee_payment =
        wait_for_ln_payment(node2_addr, &decoded.payment_hash, HTLCStatus::Claimable).await;
    assert_eq!(payee_payment.status, HTLCStatus::Claimable);

    invoice_settle(node2_addr, payment_hash_hex.clone(), preimage_hex.clone()).await;
    expect_api_ok(
        wait_for_claimable_settling(&test_dir_node2, &payment_hash_hex).await,
        "claimable entry should be marked settling",
    );

    let payload = InvoiceSettleRequest {
        payment_hash: payment_hash_hex.clone(),
        payment_preimage: preimage_hex,
    };
    let res = reqwest::Client::new()
        .post(format!("http://{node2_addr}/settlehodlinvoice"))
        .json(&payload)
        .send()
        .await
        .unwrap();
    if res.status() == StatusCode::FORBIDDEN {
        check_response_is_nok(
            res,
            StatusCode::FORBIDDEN,
            "Invoice settlement is in progress",
            "InvoiceSettlingInProgress",
        )
        .await;
    } else if res.status() == StatusCode::OK {
        let _ = _check_response_is_ok(res).await;
    } else {
        let status = res.status();
        let body = res.text().await.unwrap_or_default();
        panic!("expected 403 settling-in-progress or 200 already settled, got {status}: {body}");
    }

    let payee_payment =
        wait_for_ln_payment(node2_addr, &decoded.payment_hash, HTLCStatus::Succeeded).await;
    assert_eq!(payee_payment.status, HTLCStatus::Succeeded);
}

async fn setup_two_nodes_with_asset_channel(
    test_dir_suffix: &str,
    port_offset: u16,
    asset_channel_amount: u64,
) -> (SocketAddr, SocketAddr, String, String, String) {
    let test_dir_base = format!("{TEST_DIR_BASE}{test_dir_suffix}/");
    let test_dir_node1 = format!("{test_dir_base}node1");
    let test_dir_node2 = format!("{test_dir_base}node2");
    let node1_port = NODE1_PEER_PORT + port_offset;
    let node2_port = NODE2_PEER_PORT + port_offset;
    let (node1_addr, _) = start_node(&test_dir_node1, node1_port, false).await;
    let (node2_addr, _) = start_node(&test_dir_node2, node2_port, false).await;

    fund_and_create_utxos(node1_addr, None).await;
    fund_and_create_utxos(node2_addr, None).await;

    let asset_id = issue_asset_nia(node1_addr).await.asset_id;
    fund_and_create_utxos(node1_addr, None).await;

    let node2_pubkey = node_info(node2_addr).await.pubkey;
    let _channel = open_channel(
        node1_addr,
        &node2_pubkey,
        Some(node2_port),
        Some(500000),
        Some(0),
        Some(asset_channel_amount),
        Some(&asset_id),
    )
    .await;

    (
        node1_addr,
        node2_addr,
        test_dir_node1,
        test_dir_node2,
        asset_id,
    )
}

async fn setup_two_nodes_with_channel(
    test_dir_suffix: &str,
    port_offset: u16,
) -> (SocketAddr, SocketAddr, String, String) {
    let test_dir_base = format!("{TEST_DIR_BASE}{test_dir_suffix}/");
    let test_dir_node1 = format!("{test_dir_base}node1");
    let test_dir_node2 = format!("{test_dir_base}node2");
    let node1_port = NODE1_PEER_PORT + port_offset;
    let node2_port = NODE2_PEER_PORT + port_offset;
    let (node1_addr, _) = start_node(&test_dir_node1, node1_port, false).await;
    let (node2_addr, _) = start_node(&test_dir_node2, node2_port, false).await;

    fund_and_create_utxos(node1_addr, None).await;
    fund_and_create_utxos(node2_addr, None).await;

    let node2_pubkey = node_info(node2_addr).await.pubkey;
    let _channel = open_channel(
        node1_addr,
        &node2_pubkey,
        Some(node2_port),
        Some(500000),
        Some(0),
        None,
        None,
    )
    .await;

    (node1_addr, node2_addr, test_dir_node1, test_dir_node2)
}

async fn wait_for_claimable_settling(
    node_test_dir: &str,
    payment_hash_hex: &str,
) -> Result<(), APIError> {
    let t_0 = OffsetDateTime::now_utc();
    loop {
        if claimable_is_settling(node_test_dir, payment_hash_hex)? {
            return Ok(());
        }
        if (OffsetDateTime::now_utc() - t_0).as_seconds_f32() > 10.0 {
            return Err(APIError::Unexpected(format!(
                "claimable entry for {payment_hash_hex} was not marked settling in time"
            )));
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
}

async fn wait_for_claimable_state(
    node_test_dir: &str,
    payment_hash_hex: &str,
    expected: bool,
) -> Result<(), APIError> {
    let t_0 = OffsetDateTime::now_utc();
    loop {
        if claimable_exists(node_test_dir, payment_hash_hex)? == expected {
            return Ok(());
        }
        if (OffsetDateTime::now_utc() - t_0).as_seconds_f32() > 20.0 {
            return Err(APIError::Unexpected(format!(
                "claimable entry for {payment_hash_hex} did not reach state {expected}"
            )));
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
}

async fn wait_for_ln_payment_with_timeout(
    node_address: SocketAddr,
    payment_hash: &str,
    expected_status: HTLCStatus,
    timeout: std::time::Duration,
) -> Result<Payment, APIError> {
    assert!(timeout > std::time::Duration::ZERO, "timeout must be > 0");
    let t_0 = std::time::Instant::now();
    loop {
        if let Some(payment) =
            check_payment_status(node_address, payment_hash, expected_status).await
        {
            return Ok(payment);
        }
        if t_0.elapsed() > timeout {
            return Err(APIError::Unexpected(format!(
                "payment {payment_hash} on {node_address} did not reach status \
                {expected_status:?} in {timeout:?}"
            )));
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
}

async fn wait_for_payment_preimage(
    node_address: SocketAddr,
    payment_hash_hex: &str,
) -> Result<GetPaymentPreimageResponse, APIError> {
    let t_0 = OffsetDateTime::now_utc();
    loop {
        let resp = get_payment_preimage(node_address, payment_hash_hex).await;
        if matches!(resp.status, HTLCStatus::Succeeded) && resp.preimage.is_some() {
            return Ok(resp);
        }
        if (OffsetDateTime::now_utc() - t_0).as_seconds_f32() > 20.0 {
            return Err(APIError::Unexpected(format!(
                "preimage for {payment_hash_hex} was not available in time"
            )));
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
}
