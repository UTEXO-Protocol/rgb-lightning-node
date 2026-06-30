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
    fund_and_create_utxos(node_addr, None).await;

    let linked_asset_id = issue_asset_nia(node_addr).await.asset_id;
    let asset_id = issue_asset_nia(node_addr).await.asset_id;
    let issuer_pubkey = node_info(node_addr).await.pubkey;

    let asset_record = read_asset_link_record(&test_dir_node, &asset_id);
    assert_eq!(asset_record.asset_id, asset_id);
    assert_eq!(asset_record.linked_asset_id, None);
    assert_eq!(asset_record.issuer_pubkey, issuer_pubkey);
    assert_eq!(asset_record.signature, None);
    assert_eq!(asset_record.created_at, None);

    let linked_asset_record = read_asset_link_record(&test_dir_node, &linked_asset_id);
    assert_eq!(linked_asset_record.asset_id, linked_asset_id);
    assert_eq!(linked_asset_record.linked_asset_id, None);
    assert_eq!(linked_asset_record.issuer_pubkey, issuer_pubkey);
    assert_eq!(linked_asset_record.signature, None);
    assert_eq!(linked_asset_record.created_at, None);

    let asset_link = asset_link_create(node_addr, &asset_id, &linked_asset_id).await;
    assert_eq!(asset_link.asset_id, asset_id);
    assert_eq!(asset_link.linked_asset_id, Some(linked_asset_id.clone()));
    assert_eq!(asset_link.issuer_pubkey, issuer_pubkey);
    assert!(asset_link.signature.is_some());
    assert!(!asset_link.signature.as_deref().unwrap().is_empty());
    assert!(asset_link.created_at.is_some());

    let persisted_asset_link = read_asset_link_record(&test_dir_node, &asset_id);
    assert_eq!(persisted_asset_link, asset_link);

    let duplicate = asset_link_create(
        node_addr,
        &asset_link.asset_id,
        asset_link.linked_asset_id.as_deref().unwrap(),
    )
    .await;
    assert_eq!(duplicate, asset_link);

    let conflicting_linked_asset_id = issue_asset_nia(node_addr).await.asset_id;
    let conflict_payload = AssetLinkCreateRequest {
        asset_id: asset_link.asset_id.clone(),
        linked_asset_id: conflicting_linked_asset_id,
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
        asset_link.linked_asset_id.as_deref().unwrap(),
    )
    .await;
    assert_eq!(after_restart, asset_link);
}
