use super::*;

const TEST_DIR_BASE: &str = "tmp/ifa_channel/";

#[serial_test::serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[traced_test]
async fn open_pay_close() {
    initialize();

    let test_dir_base = format!("{TEST_DIR_BASE}open_pay_close/");
    let test_dir_node1 = format!("{test_dir_base}node1");
    let test_dir_node2 = format!("{test_dir_base}node2");
    let (node1_addr, _) = start_node(&test_dir_node1, NODE1_PEER_PORT, false).await;
    let (node2_addr, _) = start_node(&test_dir_node2, NODE2_PEER_PORT, false).await;

    fund_and_create_utxos(node1_addr, None).await;
    fund_and_create_utxos(node2_addr, None).await;

    let asset_id = issue_asset_ifa(node1_addr).await.asset_id;

    let node2_pubkey = node_info(node2_addr).await.pubkey;
    let channel = open_channel(
        node1_addr,
        &node2_pubkey,
        Some(NODE2_PEER_PORT),
        None,
        Some(3500000),
        Some(600),
        Some(&asset_id),
    )
    .await;
    assert_eq!(asset_balance_spendable(node1_addr, &asset_id).await, 400);

    let asset_amount = Some(100);
    let LNInvoiceResponse { invoice } =
        ln_invoice(node2_addr, None, Some(&asset_id), asset_amount, 900).await;
    send_payment(node1_addr, invoice.clone()).await;

    let decoded = decode_ln_invoice(node1_addr, &invoice).await;
    let payment = get_payment(node1_addr, &decoded.payment_hash, PaymentType::Outbound).await;
    assert_eq!(payment.asset_id, Some(asset_id.clone()));
    assert_eq!(payment.asset_amount, asset_amount);
    assert_eq!(payment.status, HTLCStatus::Succeeded);
    let payment = get_payment(
        node2_addr,
        &decoded.payment_hash,
        PaymentType::InboundAutoClaim,
    )
    .await;
    assert_eq!(payment.asset_id, Some(asset_id.clone()));
    assert_eq!(payment.asset_amount, asset_amount);
    assert_eq!(payment.status, HTLCStatus::Succeeded);

    close_channel(node1_addr, &channel.channel_id, &node2_pubkey, false).await;
    wait_for_balance(node1_addr, &asset_id, 900).await;
    wait_for_balance(node2_addr, &asset_id, 100).await;
}

#[serial_test::serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[traced_test]
async fn virtual_open_pay_close() {
    initialize();

    let test_dir_base = format!("{TEST_DIR_BASE}virtual_open_pay_close/");
    let host_peer_port = next_peer_port();
    let client_peer_port = next_peer_port();

    let (host_addr, _) = start_node_with_virtual_options(
        &format!("{test_dir_base}host_node"),
        host_peer_port,
        false,
        true,
        vec![],
    )
    .await;
    let host_pubkey = node_info(host_addr).await.pubkey;

    fund_and_create_utxos(host_addr, None).await;
    let asset_id = issue_asset_ifa(host_addr).await.asset_id;

    let (client_addr, _) = start_node_with_virtual_options(
        &format!("{test_dir_base}client_node"),
        client_peer_port,
        false,
        true,
        vec![bitcoin::secp256k1::PublicKey::from_str(&host_pubkey).unwrap()],
    )
    .await;
    let client_pubkey = node_info(client_addr).await.pubkey;

    let channel = open_virtual_channel(
        host_addr,
        &client_pubkey,
        Some(client_peer_port),
        Some(100_000),
        Some(0),
        Some(600),
        Some(&asset_id),
        None,
    )
    .await;
    assert_eq!(channel.asset_id.as_deref(), Some(asset_id.as_str()));

    let asset_amount = Some(100);
    let LNInvoiceResponse { invoice } =
        ln_invoice(client_addr, None, Some(&asset_id), asset_amount, 900).await;
    let payment = send_payment(host_addr, invoice).await;
    assert_eq!(payment.asset_id, Some(asset_id.clone()));
    assert_eq!(payment.asset_amount, asset_amount);
    assert_eq!(payment.status, HTLCStatus::Succeeded);
    wait_for_ln_balance(host_addr, &asset_id, 500).await;
    wait_for_ln_balance(client_addr, &asset_id, 100).await;

    // virtual close requires the counterparty balance back at zero
    let LNInvoiceResponse { invoice } = ln_invoice(
        host_addr,
        Some(3_000_000),
        Some(&asset_id),
        asset_amount,
        900,
    )
    .await;
    let payment = send_payment(client_addr, invoice).await;
    assert_eq!(payment.status, HTLCStatus::Succeeded);
    wait_for_ln_balance(host_addr, &asset_id, 600).await;
    wait_for_ln_balance(client_addr, &asset_id, 0).await;

    close_channel(host_addr, &channel.channel_id, &client_pubkey, false).await;

    shutdown(&[host_addr, client_addr]).await;
}
