use crate::helpers::*;
use serial_test::serial;
use std::{
    fs,
    thread::sleep,
    time::{Duration, Instant},
};

// This fixture intentionally closes a fully settled bidirectional-payment channel.
// PaymentSent alone does not establish reverse liquidity or the final commitment state.
pub(super) fn settled_keysend(
    sender: &SdkNode,
    receiver: &SdkNode,
    destination: bitcoin::secp256k1::PublicKey,
    channel_id: lightning::ln::types::ChannelId,
    asset_id: &ContractId,
    asset_amount: u64,
    expected_after: (u64, u64),
) {
    let deadline = Instant::now() + Duration::from_secs(60);
    let expected_before = (
        expected_after.0.checked_add(asset_amount).unwrap(),
        expected_after.1.checked_sub(asset_amount).unwrap(),
    );
    let channel_state = |node: &SdkNode| {
        let channel = node
            .list_channels()
            .expect("settled keysend list_channels")
            .into_iter()
            .find(|channel| channel.channel_id == channel_id)
            .expect("settled keysend exact channel must exist");
        let inflight =
            rgb_lightning_node::test_utils::channel_has_inflight_htlcs_for_tests(node, channel_id)
                .expect("settled keysend exact HTLC state");
        (channel, inflight)
    };
    let mut payment_hash = None;
    loop {
        sender.sync().expect("settled keysend sender sync");
        receiver.sync().expect("settled keysend receiver sync");
        let (sender_channel, sender_inflight) = channel_state(sender);
        let (receiver_channel, receiver_inflight) = channel_state(receiver);
        let both_ready = sender_channel.ready
            && sender_channel.is_usable
            && receiver_channel.ready
            && receiver_channel.is_usable;
        let quiescent = !sender_inflight && !receiver_inflight;
        let balances_match = |expected: (u64, u64)| {
            sender_channel.asset_local_amount == Some(expected.0)
                && sender_channel.asset_remote_amount == Some(expected.1)
                && receiver_channel.asset_local_amount == Some(expected.1)
                && receiver_channel.asset_remote_amount == Some(expected.0)
        };
        let diagnostic = || {
            format!(
                "asset_amount={asset_amount}, sent={}, ready={both_ready}, next_min={}, next_limit={}, sender_rgb={:?}, receiver_rgb={:?}, inflight={sender_inflight}/{receiver_inflight}",
                payment_hash.is_some(),
                sender_channel.next_outbound_htlc_minimum_msat,
                sender_channel.next_outbound_htlc_limit_msat,
                sender_channel.asset_local_amount,
                receiver_channel.asset_local_amount,
            )
        };
        assert!(
            Instant::now() < deadline,
            "settled keysend timed out: {}",
            diagnostic()
        );
        if let Some(hash) = payment_hash.as_ref() {
            let payments = [sender, receiver].map(|node| {
                node.list_payments()
                    .expect("settled keysend list_payments")
                    .into_iter()
                    .find(|payment| payment.payment_hash == *hash)
            });
            let statuses = payments.each_ref().map(|payment| {
                match payment.as_ref().map(|payment| &payment.status) {
                    None => "absent",
                    Some(HtlcStatus::Pending) => "pending",
                    Some(HtlcStatus::Claimable) => "claimable",
                    Some(HtlcStatus::Claiming) => "claiming",
                    Some(HtlcStatus::Succeeded) => "succeeded",
                    Some(HtlcStatus::Cancelled) => "cancelled",
                    Some(HtlcStatus::Failed) => "failed",
                }
            });
            assert!(
                !payments
                    .iter()
                    .any(|payment| payment.as_ref().is_some_and(|payment| matches!(
                        payment.status,
                        HtlcStatus::Failed | HtlcStatus::Cancelled
                    ))),
                "settled keysend reached a terminal failure: sender_status={}, receiver_status={}, {}",
                statuses[0],
                statuses[1],
                diagnostic()
            );
            if both_ready
                && quiescent
                && balances_match(expected_after)
                && payments.iter().all(|payment| {
                    payment
                        .as_ref()
                        .is_some_and(|payment| matches!(payment.status, HtlcStatus::Succeeded))
                })
            {
                assert!(
                    Instant::now() < deadline,
                    "settled keysend completed after deadline"
                );
                return;
            }
        } else if both_ready
            && quiescent
            && balances_match(expected_before)
            && sender_channel.next_outbound_htlc_minimum_msat <= PAYMENT_MSAT
            && sender_channel.next_outbound_htlc_limit_msat >= PAYMENT_MSAT
        {
            let sent = sender
                .keysend(SdkKeysendRequest {
                    dest_pubkey: destination,
                    amt_msat: PAYMENT_MSAT,
                    asset_id: Some(*asset_id),
                    asset_amount: Some(asset_amount),
                })
                .expect("settled keysend one-shot submission");
            assert!(
                matches!(sent.status, HtlcStatus::Pending | HtlcStatus::Succeeded),
                "settled keysend submission failed: {}",
                diagnostic()
            );
            payment_hash = Some(sent.payment_hash);
        }
        sleep(Duration::from_secs(1));
    }
}

#[test]
#[serial]
fn close_force_standard() {
    ensure_regtest_available();

    let test_dir = test_dir("sdk_close_force_standard");
    if test_dir.exists() {
        fs::remove_dir_all(&test_dir).expect("remove previous lib_sdk test dir");
    }
    fs::create_dir_all(&test_dir).expect("create lib_sdk test dir");
    let node_a_dir = test_dir.join("node_a");
    let node_b_dir = test_dir.join("node_b");
    let node_c_dir = test_dir.join("node_c");

    let node_a = make_node(&node_a_dir, NODE_A_DAEMON_PORT + 30, NODE_A_PEER_PORT + 30);
    let node_b = make_node(&node_b_dir, NODE_B_DAEMON_PORT + 30, NODE_B_PEER_PORT + 30);
    let node_c = make_node(&node_c_dir, NODE_C_DAEMON_PORT + 30, NODE_C_PEER_PORT + 30);

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        node_a
            .init("nodeApass".to_string(), None)
            .expect("node A init");
        node_b
            .init("nodeBpass".to_string(), None)
            .expect("node B init");
        node_c
            .init("nodeCpass".to_string(), None)
            .expect("node C init");

        node_a
            .unlock(unlock_request("nodeApass"))
            .expect("node A unlock");
        node_b
            .unlock(unlock_request("nodeBpass"))
            .expect("node B unlock");
        node_c
            .unlock(unlock_request("nodeCpass"))
            .expect("node C unlock");

        fund_and_create_utxos(&node_a, "node A");
        fund_and_create_utxos(&node_b, "node B");
        fund_and_create_utxos(&node_c, "node C");

        let asset_id = node_a
            .issueassetnia(SdkIssueAssetNiaRequest {
                amounts: vec![1_000],
                ticker: "USDT".to_string(),
                name: "Tether".to_string(),
                precision: 0,
            })
            .expect("node A issueassetnia")
            .asset_id;

        let node_a_pubkey = node_a.node_info().expect("node A node_info").pubkey;
        let node_b_pubkey = node_b.node_info().expect("node B node_info").pubkey;

        let peer_uri = format!("{node_b_pubkey}@127.0.0.1:{}", NODE_B_PEER_PORT + 30);
        node_a
            .connectpeer(peer_uri.clone())
            .expect("node A connectpeer");

        let open_channel = node_a
            .openchannel(SdkOpenChannelRequest {
                peer_pubkey_and_opt_addr: peer_uri,
                capacity_sat: OPEN_CHANNEL_CAPACITY_SAT,
                push_msat: PAYMENT_MSAT,
                public: true,
                with_anchors: true,
                fee_base_msat: None,
                fee_proportional_millionths: None,
                temporary_channel_id: None,
                asset_id: Some(asset_id.clone()),
                asset_amount: Some(600),
                push_asset_amount: None,
                virtual_open_mode: None,
            })
            .expect("node A openchannel");

        wait_for_channel_funding_tx(&node_a, &node_b, &asset_id, Duration::from_secs(120));
        mine(OPEN_CHANNEL_CONFIRM_BLOCKS);
        wait_for_usable_channel(&node_a, &node_b, &asset_id, Duration::from_secs(300));
        wait_for_balance(&node_a, &asset_id, 400, Duration::from_secs(70));

        let channel_id = node_a
            .get_channel_id(open_channel.temporary_channel_id)
            .expect("node A get_channel_id");

        settled_keysend(
            &node_a,
            &node_b,
            node_b_pubkey,
            channel_id,
            &asset_id,
            150,
            (450, 150),
        );
        settled_keysend(
            &node_b,
            &node_a,
            node_a_pubkey,
            channel_id,
            &asset_id,
            50,
            (100, 500),
        );

        let closed = close_channel_with_force(&node_a, channel_id, node_b_pubkey, true);
        wait_for_closed_channel_balance(&node_a, &asset_id, 900, Duration::from_secs(70), &closed);
        wait_for_closed_channel_balance(&node_b, &asset_id, 100, Duration::from_secs(70), &closed);

        let recipient_id = crate::helpers::rgb_invoice(
            &node_c,
            SdkRgbInvoiceRequest {
                asset_id: None,
                assignment_kind: None,
                assignment_amount: None,
                duration_seconds: None,
                min_confirmations: 1,
                witness: false,
            },
        )
        .expect("node C rgbinvoice first")
        .recipient_id;
        crate::helpers::send_rgb(
            &node_a,
            SendRgbRequest {
                donation: true,
                fee_rate: CREATE_UTXOS_FEE_RATE,
                min_confirmations: 1,
                recipient_groups: vec![AssetRecipients {
                    asset_id: asset_id.clone(),
                    recipients: vec![RgbRecipient {
                        recipient_id: RecipientId(recipient_id.0),
                        witness_data: None,
                        assignment_kind: AssignmentKind::Fungible,
                        assignment_amount: Some(700),
                        transport_endpoints: vec![TransportEndpoint(
                            PROXY_ENDPOINT_LOCAL.to_string(),
                        )],
                    }],
                }],
            },
        )
        .expect("node A send_rgb");
        mine(1);
        refresh_transfers(&node_c);
        refresh_transfers(&node_c);
        refresh_transfers(&node_a);

        let recipient_id = crate::helpers::rgb_invoice(
            &node_c,
            SdkRgbInvoiceRequest {
                asset_id: None,
                assignment_kind: None,
                assignment_amount: None,
                duration_seconds: None,
                min_confirmations: 1,
                witness: false,
            },
        )
        .expect("node C rgbinvoice second")
        .recipient_id;
        crate::helpers::send_rgb(
            &node_b,
            SendRgbRequest {
                donation: true,
                fee_rate: CREATE_UTXOS_FEE_RATE,
                min_confirmations: 1,
                recipient_groups: vec![AssetRecipients {
                    asset_id: asset_id.clone(),
                    recipients: vec![RgbRecipient {
                        recipient_id: RecipientId(recipient_id.0),
                        witness_data: None,
                        assignment_kind: AssignmentKind::Fungible,
                        assignment_amount: Some(50),
                        transport_endpoints: vec![TransportEndpoint(
                            PROXY_ENDPOINT_LOCAL.to_string(),
                        )],
                    }],
                }],
            },
        )
        .expect("node B send_rgb");
        mine(1);
        refresh_transfers(&node_c);
        refresh_transfers(&node_c);
        refresh_transfers(&node_b);

        assert_eq!(asset_balance_spendable(&node_a, &asset_id), 200);
        assert_eq!(asset_balance_spendable(&node_b, &asset_id), 50);
        assert_eq!(asset_balance_spendable(&node_c, &asset_id), 750);
    }));

    node_a.shutdown();
    node_b.shutdown();
    node_c.shutdown();

    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}
