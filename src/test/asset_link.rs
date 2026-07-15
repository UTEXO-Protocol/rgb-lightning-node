use super::*;

const TEST_DIR_BASE: &str = "tmp/asset_link/";

fn read_asset_link_record(test_dir: &str, asset_id: &str) -> AssetLink {
    let db_path = get_db_path(&std::path::PathBuf::from(test_dir));
    let connection_string = format!("sqlite:{}?mode=rwc", db_path.display());
    let mut opt = ConnectOptions::new(connection_string);
    opt.max_connections(1);
    let db = crate::runtime::block_on(Database::connect(opt)).expect("connect to test db");
    let kv_store = SeaOrmKvStore::from_connection(Arc::new(db));
    let bytes = kv_store
        .read("asset_link", "", asset_id)
        .expect("asset link record");
    serde_json::from_slice(&bytes).expect("parse asset link")
}

#[serial_test::serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[traced_test]
async fn asset_link_create_is_persisted_and_idempotent() {
    initialize();

    let test_dir_node = format!("{TEST_DIR_BASE}node");
    let (node_addr, _) = start_node(&test_dir_node, NODE1_PEER_PORT, false).await;
    fund_and_create_utxos(node_addr, Some(12)).await;

    let legacy_asset = issue_asset_ifa(node_addr).await;
    assert_eq!(legacy_asset.link_right_outpoint, None);
    let legacy_asset_id = legacy_asset.asset_id;

    let asset = issue_asset_ifa_with_type(node_addr, Some(IfaIssuanceType::LinkRightOnly)).await;
    let asset_id = asset.asset_id;
    let _link_right_outpoint = asset.link_right_outpoint.expect("link-right outpoint");
    let linked_asset = issue_asset_ifa_with_type(
        node_addr,
        Some(IfaIssuanceType::LinkedFromParent {
            contract_id: asset_id.clone(),
            request_link_right: false,
        }),
    )
    .await;
    assert_eq!(linked_asset.link_right_outpoint, None);
    let linked_asset_id = linked_asset.asset_id;

    let invalid_link_payload = AssetLinkCreateRequest {
        asset_id: asset_id.clone(),
        linked_asset_id: legacy_asset_id.clone(),
        fee_rate: FEE_RATE,
        min_confirmations: 1,
    };
    let invalid_link_res = reqwest::Client::new()
        .post(format!("http://{node_addr}/assetlink/create"))
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
    let missing_link_right_payload = AssetLinkCreateRequest {
        asset_id: legacy_asset_id.clone(),
        linked_asset_id: missing_link_right_child.asset_id.clone(),
        fee_rate: FEE_RATE,
        min_confirmations: 1,
    };
    let missing_link_right_res = reqwest::Client::new()
        .post(format!("http://{node_addr}/assetlink/create"))
        .json(&missing_link_right_payload)
        .send()
        .await
        .unwrap();
    check_response_is_nok(
        missing_link_right_res,
        StatusCode::BAD_REQUEST,
        "missing link_right_outpoint",
        "InvalidRequest",
    )
    .await;

    let asset_link = asset_link_create(node_addr, &asset_id, &linked_asset_id).await;
    assert_eq!(asset_link.asset_id, asset_id);
    assert_eq!(asset_link.linked_asset_id, Some(linked_asset_id));
    assert!(asset_link
        .txid
        .as_deref()
        .is_some_and(|txid| !txid.is_empty()));
    assert!(asset_link.created_at.is_some());

    let persisted_asset_link = read_asset_link_record(&test_dir_node, &asset_id);
    assert_eq!(persisted_asset_link, asset_link);

    let duplicate = asset_link_create(
        node_addr,
        &asset_link.asset_id,
        asset_link
            .linked_asset_id
            .as_deref()
            .expect("linked asset id"),
    )
    .await;
    assert_eq!(duplicate, asset_link);

    let conflicting_linked_asset_id = issue_asset_ifa_with_type(
        node_addr,
        Some(IfaIssuanceType::LinkedFromParent {
            contract_id: asset_id.clone(),
            request_link_right: false,
        }),
    )
    .await
    .asset_id;
    let conflict_payload = AssetLinkCreateRequest {
        asset_id: asset_link.asset_id.clone(),
        linked_asset_id: conflicting_linked_asset_id,
        fee_rate: FEE_RATE,
        min_confirmations: 1,
    };
    let conflict_res = reqwest::Client::new()
        .post(format!("http://{node_addr}/assetlink/create"))
        .json(&conflict_payload)
        .send()
        .await
        .unwrap();
    check_response_is_nok(
        conflict_res,
        StatusCode::BAD_REQUEST,
        "asset link already exists",
        "InvalidRequest",
    )
    .await;

    shutdown(&[node_addr]).await;
    let (node_addr, _) = start_node(&test_dir_node, NODE1_PEER_PORT, true).await;

    let after_restart = asset_link_create(
        node_addr,
        &asset_link.asset_id,
        asset_link
            .linked_asset_id
            .as_deref()
            .expect("linked asset id"),
    )
    .await;
    assert_eq!(after_restart, asset_link);
}

#[serial_test::serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[traced_test]
async fn asset_link_send_payment_swaps_virtual_to_reserve_asset() {
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
        .link_right_outpoint
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

    let payment = asset_link_send_payment(payer_addr, &invoice, &asset_v, &host_info.pubkey).await;
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

    let asset_unlinked = issue_asset_ifa(host_addr).await.asset_id;
    let r_invoice_unlinked = ln_invoice(
        receiver_addr,
        Some(3_000_000),
        Some(&asset_r),
        Some(1),
        3600,
    )
    .await
    .invoice;
    let res = asset_link_send_payment_raw(
        payer_addr,
        &r_invoice_unlinked,
        &asset_unlinked,
        &host_info.pubkey,
    )
    .await;
    check_response_is_nok(res, StatusCode::FORBIDDEN, "No route found", "NoRoute").await;
    wait_for_ln_balance(receiver_addr, &asset_r, CHANNEL_AMT).await;
    wait_for_ln_balance(host_addr, &asset_r, 0).await;

    shutdown(&[payer_addr, host_addr, receiver_addr]).await;
}
