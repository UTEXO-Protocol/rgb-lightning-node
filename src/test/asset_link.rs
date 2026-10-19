use super::*;

const TEST_DIR_BASE: &str = "tmp/asset_link/";

async fn wait_for_link_transfer_settled(node_addr: SocketAddr, asset_id: &str) {
    let t_0 = OffsetDateTime::now_utc();
    loop {
        refresh_transfers(node_addr).await;
        let transfers = list_transfers(node_addr, asset_id).await;
        if transfers.iter().any(|transfer| {
            transfer.kind == TransferKind::Link && transfer.status == TransferStatus::Settled
        }) {
            break;
        }
        if (OffsetDateTime::now_utc() - t_0).as_seconds_f32() > 70.0 {
            panic!("link transfer for asset {asset_id} did not settle");
        }
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
}

#[serial_test::serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[traced_test]
async fn asset_link_create_uses_rgb_link_state_and_is_idempotent() {
    initialize();

    let test_dir_node = format!("{TEST_DIR_BASE}node");
    let (node_addr, _) = start_node(&test_dir_node, NODE1_PEER_PORT, false).await;
    fund_and_create_utxos(node_addr, Some(12)).await;

    let legacy_asset = issue_asset_ifa(node_addr).await;
    assert_eq!(legacy_asset.issuance_link_right_outpoint, None);
    let legacy_asset_id = legacy_asset.asset_id;

    let parent_asset =
        issue_asset_ifa_with_type(node_addr, Some(IfaIssuanceType::LinkRightOnly)).await;
    let parent_asset_id = parent_asset.asset_id;
    let issuance_link_right_outpoint = parent_asset
        .issuance_link_right_outpoint
        .expect("link-right outpoint");
    let child_asset = issue_asset_ifa_with_type(
        node_addr,
        Some(IfaIssuanceType::LinkedFromParent {
            contract_id: parent_asset_id.clone(),
            request_link_right: false,
        }),
    )
    .await;
    assert_eq!(child_asset.issuance_link_right_outpoint, None);
    let child_asset_id = child_asset.asset_id;

    let parent_metadata = asset_metadata(node_addr, &parent_asset_id).await;
    assert_eq!(
        parent_metadata.unspent_link_right_outpoint,
        Some(issuance_link_right_outpoint)
    );
    assert_eq!(parent_metadata.linked_from_asset_id, None);
    assert_eq!(parent_metadata.linked_to_asset_id, None);
    let child_metadata = asset_metadata(node_addr, &child_asset_id).await;
    assert_eq!(
        child_metadata.linked_from_asset_id,
        Some(parent_asset_id.clone())
    );
    assert_eq!(child_metadata.linked_to_asset_id, None);
    assert_eq!(child_metadata.unspent_link_right_outpoint, None);

    let invalid_link_payload = AssetLinkRequest {
        parent_asset_id: parent_asset_id.clone(),
        child_asset_id: legacy_asset_id.clone(),
        min_confirmations: 1,
    };
    let invalid_link_res = reqwest::Client::new()
        .post(format!("http://{node_addr}/assetlink"))
        .json(&invalid_link_payload)
        .send()
        .await
        .unwrap();
    check_response_is_nok(
        invalid_link_res,
        StatusCode::BAD_REQUEST,
        "Invalid contract link",
        "InvalidContractLink",
    )
    .await;

    let missing_link_right_child = issue_asset_ifa_with_type(
        node_addr,
        Some(IfaIssuanceType::LinkedFromParent {
            contract_id: legacy_asset_id.clone(),
            request_link_right: false,
        }),
    )
    .await;
    let missing_link_right_payload = AssetLinkRequest {
        parent_asset_id: legacy_asset_id.clone(),
        child_asset_id: missing_link_right_child.asset_id.clone(),
        min_confirmations: 1,
    };
    let missing_link_right_res = reqwest::Client::new()
        .post(format!("http://{node_addr}/assetlink"))
        .json(&missing_link_right_payload)
        .send()
        .await
        .unwrap();
    check_response_is_nok(
        missing_link_right_res,
        StatusCode::BAD_REQUEST,
        "missing unspent_link_right_outpoint",
        "InvalidRequest",
    )
    .await;

    let asset_link = asset_link_create(node_addr, &parent_asset_id, &child_asset_id).await;
    assert_eq!(asset_link.parent_asset_id, parent_asset_id);
    assert_eq!(asset_link.child_asset_id, Some(child_asset_id.clone()));
    assert!(asset_link
        .txid
        .as_deref()
        .is_some_and(|txid| !txid.is_empty()));
    assert!(asset_link.created_at.is_some());

    // The parent was issued with LinkRightOnly. Its sole allocation must be
    // absent after the link transaction consumes it.
    mine(false);
    wait_for_link_transfer_settled(node_addr, &parent_asset_id).await;
    let unspents = list_settled_unspents(node_addr).await;
    assert!(!unspents.iter().any(|unspent| {
        unspent
            .rgb_allocations
            .iter()
            .any(|allocation| allocation.asset_id.as_deref() == Some(parent_asset_id.as_str()))
    }));
    assert_eq!(
        asset_metadata(node_addr, &parent_asset_id)
            .await
            .unspent_link_right_outpoint,
        None
    );

    // the link transition should be idempotent
    let duplicate = asset_link_create(
        node_addr,
        &asset_link.parent_asset_id,
        asset_link
            .child_asset_id
            .as_deref()
            .expect("child asset id"),
    )
    .await;
    assert_eq!(duplicate.parent_asset_id, asset_link.parent_asset_id);
    assert_eq!(duplicate.child_asset_id, asset_link.child_asset_id);
    assert!(duplicate
        .txid
        .as_deref()
        .is_some_and(|txid| !txid.is_empty()));
    assert!(duplicate.created_at.is_some());

    let conflicting_linked_asset_id = issue_asset_ifa_with_type(
        node_addr,
        Some(IfaIssuanceType::LinkedFromParent {
            contract_id: parent_asset_id.clone(),
            request_link_right: false,
        }),
    )
    .await
    .asset_id;
    let conflict_payload = AssetLinkRequest {
        parent_asset_id: asset_link.parent_asset_id.clone(),
        child_asset_id: conflicting_linked_asset_id,
        min_confirmations: 1,
    };
    let conflict_res = reqwest::Client::new()
        .post(format!("http://{node_addr}/assetlink"))
        .json(&conflict_payload)
        .send()
        .await
        .unwrap();
    check_response_is_nok(
        conflict_res,
        StatusCode::BAD_REQUEST,
        "already linked to",
        "InvalidContractLink",
    )
    .await;

    shutdown(&[node_addr]).await;
    let (node_addr, _) = start_node(&test_dir_node, NODE1_PEER_PORT, true).await;

    let after_restart = asset_link_create(
        node_addr,
        &asset_link.parent_asset_id,
        asset_link
            .child_asset_id
            .as_deref()
            .expect("child asset id"),
    )
    .await;
    assert_eq!(after_restart.parent_asset_id, asset_link.parent_asset_id);
    assert_eq!(after_restart.child_asset_id, asset_link.child_asset_id);
    assert!(after_restart
        .txid
        .as_deref()
        .is_some_and(|txid| !txid.is_empty()));
    assert!(after_restart.created_at.is_some());

    let restarted_parent_metadata = asset_metadata(node_addr, &parent_asset_id).await;
    assert_eq!(
        restarted_parent_metadata.linked_to_asset_id,
        Some(child_asset_id.clone())
    );
    let restarted_child_metadata = asset_metadata(node_addr, &child_asset_id).await;
    assert_eq!(
        restarted_child_metadata.linked_from_asset_id,
        Some(parent_asset_id.clone())
    );
    assert_eq!(restarted_child_metadata.linked_to_asset_id, None);

    let assets = list_assets(node_addr).await;
    let listed_parent = assets
        .ifa
        .as_ref()
        .and_then(|ifa| ifa.iter().find(|asset| asset.asset_id == parent_asset_id))
        .expect("listed parent");
    assert_eq!(
        listed_parent.linked_to_asset_id,
        Some(child_asset_id.clone())
    );
    let listed_child = assets
        .ifa
        .as_ref()
        .and_then(|ifa| ifa.iter().find(|asset| asset.asset_id == child_asset_id))
        .expect("listed child");
    assert_eq!(
        listed_child.linked_from_asset_id,
        Some(parent_asset_id.clone())
    );
    assert_eq!(listed_child.linked_to_asset_id, None);
}

#[serial_test::serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[traced_test]
async fn asset_link_send_payment_uses_linked_asset_when_invoice_asset_liquidity_is_insufficient() {
    initialize();

    const LINK_AMT: u64 = 100;
    const CHANNEL_AMT: u64 = 300;

    let test_dir_host = format!("{TEST_DIR_BASE}host");
    let test_dir_payer = format!("{TEST_DIR_BASE}payer");
    let test_dir_receiver = format!("{TEST_DIR_BASE}receiver");

    let host_peer_port = next_peer_port();
    let payer_peer_port = next_peer_port();
    let receiver_peer_port = next_peer_port();

    let (host_addr, _) =
        start_node_with_virtual_options(&test_dir_host, host_peer_port, false, true, vec![]).await;
    let host_info = node_info(host_addr).await;
    let (payer_addr, _) = start_node_with_virtual_options(
        &test_dir_payer,
        payer_peer_port,
        false,
        true,
        vec![PublicKey::from_str(&host_info.pubkey).unwrap()],
    )
    .await;
    let (receiver_addr, _) = start_node(&test_dir_receiver, receiver_peer_port, false).await;

    fund_and_create_utxos(host_addr, None).await;
    fund_and_create_utxos(receiver_addr, None).await;

    let asset_r_data =
        issue_asset_ifa_with_type(host_addr, Some(IfaIssuanceType::LinkRightOnly)).await;
    let asset_r = asset_r_data.asset_id;
    let _link_right_outpoint = asset_r_data
        .issuance_link_right_outpoint
        .expect("link-right outpoint");
    let asset_v = issue_asset_ifa_with_type(
        host_addr,
        Some(IfaIssuanceType::LinkedFromParent {
            contract_id: asset_r.clone(),
            request_link_right: false,
        }),
    )
    .await
    .asset_id;
    asset_link_create(host_addr, &asset_r, &asset_v).await;
    mine(false);
    wait_for_link_transfer_settled(host_addr, &asset_r).await;

    let payer_info = node_info(payer_addr).await;
    open_virtual_channel(
        host_addr,
        &payer_info.pubkey,
        Some(payer_peer_port),
        Some(100_000),
        Some(10_000_000),
        Some(CHANNEL_AMT),
        Some(&asset_v),
        Some(CHANNEL_AMT - LINK_AMT),
    )
    .await;

    let receiver_info = node_info(receiver_addr).await;
    open_channel_raw(
        host_addr,
        &receiver_info.pubkey,
        Some(receiver_peer_port),
        Some(100_000),
        Some(10_000_000),
        Some(CHANNEL_AMT),
        Some(&asset_r),
        Some(CHANNEL_AMT - LINK_AMT),
        None,
        None,
        None,
        true,
        false,
        None,
    )
    .await
    .unwrap();

    let invoice = ln_invoice(
        receiver_addr,
        Some(3_000_000),
        Some(&asset_r),
        Some(LINK_AMT),
        3600,
    )
    .await
    .invoice;

    let payment = send_payment(payer_addr, invoice).await;
    let payment_hash = payment.payment_hash.clone();
    check_preimage_matches_hash(&payment, &payment_hash);
    assert_eq!(payment.asset_id.as_deref(), Some(asset_v.as_str()));
    assert_eq!(payment.asset_amount, Some(LINK_AMT));

    wait_for_ln_balance(receiver_addr, &asset_r, CHANNEL_AMT).await;
    wait_for_ln_balance(payer_addr, &asset_v, CHANNEL_AMT - LINK_AMT * 2).await;
    wait_for_ln_balance(host_addr, &asset_v, LINK_AMT * 2).await;
    wait_for_ln_balance(host_addr, &asset_r, 0).await;

    let receiver_payment = wait_for_ln_payment_by_type(
        receiver_addr,
        &payment_hash,
        PaymentType::InboundAutoClaim,
        HTLCStatus::Succeeded,
    )
    .await;
    assert_eq!(receiver_payment.asset_id.as_deref(), Some(asset_r.as_str()));
    assert_eq!(receiver_payment.asset_amount, Some(LINK_AMT));

    let host_swaps = list_swaps(host_addr).await;
    let linked_swap = host_swaps
        .taker
        .iter()
        .find(|swap| swap.payment_hash == payment_hash)
        .expect("linked swap whitelist entry on the host");
    assert_eq!(linked_swap.from_asset.as_deref(), Some(asset_r.as_str()));
    assert_eq!(linked_swap.to_asset.as_deref(), Some(asset_v.as_str()));
    assert_eq!(linked_swap.qty_from, LINK_AMT);
    assert_eq!(linked_swap.qty_to, LINK_AMT);

    wait_for_ln_balance(receiver_addr, &asset_r, CHANNEL_AMT).await;
    wait_for_ln_balance(host_addr, &asset_r, 0).await;

    shutdown(&[payer_addr, host_addr, receiver_addr]).await;
}
