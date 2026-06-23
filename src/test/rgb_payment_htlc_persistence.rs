use super::*;

const TEST_DIR_BASE: &str = "tmp/rgb_payment_htlc_persistence/";

// A channel can hold an RGB HTLC in flight, and that state must survive a node
// restart. This pays a hodl invoice to keep an RGB HTLC pending across a restart
// of both nodes — the receiver holds a pending inbound HTLC and the sender a
// pending outbound one — then checks the channel re-establishes and the held
// payment can still be settled.
#[serial_test::serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[traced_test]
async fn restart_with_pending_rgb_htlc_recovers_channel() {
    initialize();

    let test_dir_node1 = format!("{TEST_DIR_BASE}node1");
    let test_dir_node2 = format!("{TEST_DIR_BASE}node2");

    let (node1_addr, _) = start_node(&test_dir_node1, NODE1_PEER_PORT, false).await;
    let (node2_addr, _) = start_node(&test_dir_node2, NODE2_PEER_PORT, false).await;

    fund_and_create_utxos(node1_addr, None).await;
    fund_and_create_utxos(node2_addr, None).await;

    let asset_id = issue_asset_nia(node1_addr).await.asset_id;
    fund_and_create_utxos(node1_addr, None).await;

    let node2_pubkey = node_info(node2_addr).await.pubkey;
    let channel = open_channel_with_retry(
        node1_addr,
        &node2_pubkey,
        Some(NODE2_PEER_PORT),
        Some(500000),
        Some(0),
        Some(100),
        Some(&asset_id),
        None,
        5,
    )
    .await;
    let channel_id = channel.channel_id.clone();
    let node1_balance = asset_balance_offchain_outbound(node1_addr, &asset_id).await;
    let node2_balance = asset_balance_offchain_outbound(node2_addr, &asset_id).await;

    // Pay a hodl invoice so an RGB HTLC stays pending: receiver holds it
    // (Claimable) and sender keeps it in flight (Pending).
    let (preimage, payment_hash) = random_preimage_and_hash();
    let LNInvoiceResponse { invoice } = ln_invoice_hodl(
        node2_addr,
        Some(HTLC_MIN_MSAT),
        Some(&asset_id),
        Some(10),
        600,
        Some(payment_hash.clone()),
    )
    .await;
    send_payment_with_status(node1_addr, invoice, HTLCStatus::Pending).await;
    wait_for_ln_payment(node2_addr, &payment_hash, HTLCStatus::Claimable).await;

    // Restart both nodes: each ChannelManager is serialized with a pending HTLC
    // in this channel and must deserialize on unlock.
    shutdown(&[node1_addr, node2_addr]).await;
    let (node1_addr, _) = start_node(&test_dir_node1, NODE1_PEER_PORT, true).await;
    let (node2_addr, _) = start_node(&test_dir_node2, NODE2_PEER_PORT, true).await;

    wait_for_channel_ready(node1_addr, &channel_id).await;
    wait_for_channel_ready(node2_addr, &channel_id).await;

    // The held HTLC survived the restart; settling it completes the payment and
    // moves the asset across the recovered channel.
    wait_for_ln_payment(node2_addr, &payment_hash, HTLCStatus::Claimable).await;
    claim_hodl_invoice(node2_addr, payment_hash.clone(), preimage).await;
    wait_for_ln_payment(node1_addr, &payment_hash, HTLCStatus::Succeeded).await;
    wait_for_ln_payment(node2_addr, &payment_hash, HTLCStatus::Succeeded).await;
    wait_for_ln_balance(node1_addr, &asset_id, node1_balance - 10).await;
    wait_for_ln_balance(node2_addr, &asset_id, node2_balance + 10).await;

    shutdown(&[node1_addr, node2_addr]).await;
}

async fn wait_for_channel_ready(node_address: SocketAddr, channel_id: &str) {
    let t_0 = OffsetDateTime::now_utc();
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        if list_channels(node_address)
            .await
            .iter()
            .any(|c| c.channel_id == channel_id && c.ready)
        {
            return;
        }
        if (OffsetDateTime::now_utc() - t_0).as_seconds_f32() > 30.0 {
            panic!("channel {channel_id} did not become ready on {node_address}");
        }
    }
}
