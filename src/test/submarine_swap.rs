use super::*;
use crate::disk::read_htlc_tracker;
use crate::ldk::{HtlcTrackerStorage, HtlcUtxoKind};
use bitcoin::{Address, Network, ScriptBuf};
use lightning::rgb_utils::STATIC_BLINDING;
use lightning::util::ser::Readable;
use std::path::Path;

const TEST_DIR_BASE: &str = "tmp/submarine_swap/";

fn fund_htlc_address(htlc_spk_hex: &str) {
    let script_bytes = hex_str_to_vec(htlc_spk_hex).expect("htlc_p2tr_script_pubkey hex");
    let script_buf = ScriptBuf::from_bytes(script_bytes);
    let htlc_address = Address::from_script(&script_buf, Network::Regtest).expect("htlc address");
    _fund_wallet(htlc_address.to_string());
    mine(false);
}

async fn setup_single_node(
    test_dir_base: &str,
    node_dir: &str,
    node_peer_port: u16,
) -> (SocketAddr, String) {
    let test_dir_node = format!("{test_dir_base}{node_dir}");
    let (node_addr, _password) = start_node(&test_dir_node, node_peer_port, false).await;
    fund_and_create_utxos(node_addr, None).await;
    (node_addr, test_dir_node)
}

#[serial_test::serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[traced_test]
async fn htlc_claim_is_idempotent_with_same_preimage() {
    initialize();

    let test_dir_base = format!("{TEST_DIR_BASE}htlc_claim_idempotent/");
    let (node1_addr, test_dir_node1) =
        setup_single_node(&test_dir_base, "node1", NODE1_PEER_PORT).await;
    let test_dir_node1 = Path::new(&test_dir_node1);

    let asset_id = issue_asset_nia(node1_addr).await.asset_id;
    let (preimage_hex, payment_hash_hex) = random_preimage_and_hash();
    let response = rgb_invoice_htlc_with_random_user(
        node1_addr,
        Some(asset_id.clone()),
        Some(Assignment::Fungible(1)),
        Some(3600),
        payment_hash_hex.clone(),
        210,
    )
    .await;

    send_asset(
        node1_addr,
        &asset_id,
        Assignment::Fungible(1),
        response.recipient_id.clone(),
        Some(WitnessData {
            amount_sat: 1000,
            blinding: Some(STATIC_BLINDING),
        }),
    )
    .await;
    mine(false);
    refresh_transfers(node1_addr).await;

    fund_htlc_address(response.htlc_p2tr_script_pubkey.as_str());

    htlc_claim(node1_addr, payment_hash_hex.clone(), preimage_hex.clone()).await;
    htlc_claim(node1_addr, payment_hash_hex.clone(), preimage_hex.clone()).await;

    let tracker_path = test_dir_node1.join(LDK_DIR);
    let tracker_file =
        std::fs::File::open(tracker_path.join("htlc_tracker.json")).expect("open htlc tracker");
    let tracker = HtlcTrackerStorage::read(&mut std::io::BufReader::new(tracker_file))
        .expect("decode htlc tracker");
    let entry = tracker
        .entries
        .iter()
        .find(|(k, _)| hex_str(&k.0).eq_ignore_ascii_case(&payment_hash_hex))
        .map(|(_, v)| v)
        .expect("htlc tracker entry");
    assert_eq!(entry.status, "ClaimRequested");
}

#[serial_test::serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[traced_test]
async fn htlc_claim_sets_dest_scripts_for_vanilla_and_colored() {
    initialize();

    let test_dir_base = format!("{TEST_DIR_BASE}htlc_claim_dest_scripts/");
    let (node1_addr, test_dir_node1) =
        setup_single_node(&test_dir_base, "node1", NODE1_PEER_PORT).await;
    let test_dir_node1 = Path::new(&test_dir_node1);

    let asset_id = issue_asset_nia(node1_addr).await.asset_id;

    let (preimage_hex, payment_hash_hex) = random_preimage_and_hash();
    let response = rgb_invoice_htlc_with_random_user(
        node1_addr,
        Some(asset_id.clone()),
        Some(Assignment::Fungible(1)),
        Some(3600),
        payment_hash_hex.clone(),
        210,
    )
    .await;

    let htlc_spk_hex = response.htlc_p2tr_script_pubkey.as_str();
    fund_htlc_address(htlc_spk_hex);

    send_asset(
        node1_addr,
        &asset_id,
        Assignment::Fungible(1),
        response.recipient_id.clone(),
        Some(WitnessData {
            amount_sat: 1000,
            blinding: None,
        }),
    )
    .await;
    mine(false);
    refresh_transfers(node1_addr).await;

    htlc_claim(node1_addr, payment_hash_hex.clone(), preimage_hex.clone()).await;

    let tracker_path = test_dir_node1.join(LDK_DIR);
    let tracker_file =
        std::fs::File::open(tracker_path.join("htlc_tracker.json")).expect("open htlc tracker");
    let tracker = HtlcTrackerStorage::read(&mut std::io::BufReader::new(tracker_file))
        .expect("decode htlc tracker");
    let entry = tracker
        .entries
        .iter()
        .find(|(k, _)| hex_str(&k.0).eq_ignore_ascii_case(&payment_hash_hex))
        .map(|(_, v)| v)
        .expect("htlc tracker entry");

    let btc_dest_hex = entry
        .btc_destination_script_hex
        .as_ref()
        .expect("btc destination script missing");
    let btc_script_bytes = hex_str_to_vec(btc_dest_hex).expect("btc destination script hex");
    let btc_script = ScriptBuf::from_bytes(btc_script_bytes);
    Address::from_script(&btc_script, Network::Regtest).expect("btc destination script invalid");

    let rgb_dest_hex = entry
        .rgb_destination_script_hex
        .as_ref()
        .expect("rgb destination script missing");
    let rgb_script_bytes = hex_str_to_vec(rgb_dest_hex).expect("rgb destination script hex");
    let rgb_script = ScriptBuf::from_bytes(rgb_script_bytes);
    Address::from_script(&rgb_script, Network::Regtest).expect("rgb destination script invalid");
    assert_ne!(
        btc_dest_hex, rgb_dest_hex,
        "BTC and RGB destination scripts should be distinct"
    );
}

#[serial_test::serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[traced_test]
async fn htlc_claim_updates_tracker_for_mixed_utxos() {
    initialize();

    let test_dir_base = format!("{TEST_DIR_BASE}htlc_claim_mixed/");
    let (node1_addr, test_dir_node1) =
        setup_single_node(&test_dir_base, "node1", NODE1_PEER_PORT).await;
    let test_dir_node1 = Path::new(&test_dir_node1);

    let asset_id = issue_asset_nia(node1_addr).await.asset_id;
    let (preimage_hex, payment_hash_hex) = random_preimage_and_hash();
    let response = rgb_invoice_htlc_with_random_user(
        node1_addr,
        Some(asset_id.clone()),
        Some(Assignment::Fungible(1)),
        Some(3600),
        payment_hash_hex.clone(),
        210,
    )
    .await;

    send_asset(
        node1_addr,
        &asset_id,
        Assignment::Fungible(1),
        response.recipient_id.clone(),
        Some(WitnessData {
            amount_sat: 1000,
            blinding: Some(STATIC_BLINDING),
        }),
    )
    .await;
    mine(false);
    refresh_transfers(node1_addr).await;

    fund_htlc_address(response.htlc_p2tr_script_pubkey.as_str());

    htlc_claim(node1_addr, payment_hash_hex.clone(), preimage_hex.clone()).await;

    let tracker_path = test_dir_node1.join(LDK_DIR);
    assert!(
        tracker_path.join("htlc_tracker.json").exists(),
        "htlc tracker file missing at {}",
        tracker_path.display()
    );
    let tracker_file =
        std::fs::File::open(tracker_path.join("htlc_tracker.json")).expect("open htlc tracker");
    let tracker = HtlcTrackerStorage::read(&mut std::io::BufReader::new(tracker_file))
        .expect("decode htlc tracker");
    assert!(
        !tracker.entries.is_empty(),
        "htlc tracker empty at {}",
        tracker_path.display()
    );
    let entry = tracker
        .entries
        .iter()
        .find(|(k, _)| hex_str(&k.0).eq_ignore_ascii_case(&payment_hash_hex))
        .map(|(_, v)| v)
        .expect("htlc tracker entry");
    assert_eq!(entry.status, "ClaimRequested");
    assert!(entry.funding.len() == 2);
    assert!(entry
        .funding
        .iter()
        .any(|d| d.utxo_kind() == HtlcUtxoKind::Vanilla));
    assert!(entry
        .funding
        .iter()
        .any(|d| d.utxo_kind() == HtlcUtxoKind::Colored));
    assert!(entry
        .funding
        .iter()
        .any(|d| d.assignment() == Some(Assignment::Fungible(1))));
}

#[serial_test::serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[traced_test]
async fn htlc_scan_marks_underfunded() {
    initialize();

    let test_dir_base = format!("{TEST_DIR_BASE}htlc_scan_underfunded/");
    let (node1_addr, test_dir_node1) =
        setup_single_node(&test_dir_base, "node1", NODE1_PEER_PORT).await;
    let test_dir_node1 = Path::new(&test_dir_node1);

    let asset_id = issue_asset_nia(node1_addr).await.asset_id;
    let (_preimage_hex, payment_hash_hex) = random_preimage_and_hash();
    let response = rgb_invoice_htlc_with_random_user(
        node1_addr,
        Some(asset_id.clone()),
        Some(Assignment::Fungible(2)),
        Some(3600),
        payment_hash_hex.clone(),
        210,
    )
    .await;

    fund_htlc_address(response.htlc_p2tr_script_pubkey.as_str());

    htlc_scan(node1_addr, payment_hash_hex.clone()).await;

    let tracker_path = test_dir_node1.join(LDK_DIR);
    let tracker_file =
        std::fs::File::open(tracker_path.join("htlc_tracker.json")).expect("open htlc tracker");
    let tracker = HtlcTrackerStorage::read(&mut std::io::BufReader::new(tracker_file))
        .expect("decode htlc tracker");
    let entry = tracker
        .entries
        .iter()
        .find(|(k, _)| hex_str(&k.0).eq_ignore_ascii_case(&payment_hash_hex))
        .map(|(_, v)| v)
        .expect("htlc tracker entry");

    assert_eq!(entry.status, "Underfunded");
    assert!(entry
        .funding
        .iter()
        .any(|d| d.utxo_kind() == HtlcUtxoKind::Vanilla));
    assert!(!entry
        .funding
        .iter()
        .any(|d| d.utxo_kind() == HtlcUtxoKind::Colored));
}

#[serial_test::serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[traced_test]
async fn htlc_scan_updates_tracker_for_mixed_utxos() {
    initialize();

    let test_dir_base = format!("{TEST_DIR_BASE}htlc_scan/");
    let (node1_addr, test_dir_node1) =
        setup_single_node(&test_dir_base, "node1", NODE1_PEER_PORT).await;
    let test_dir_node1 = Path::new(&test_dir_node1);

    let asset_id = issue_asset_nia(node1_addr).await.asset_id;
    let (_preimage_hex, payment_hash_hex) = random_preimage_and_hash();
    let response = rgb_invoice_htlc_with_random_user(
        node1_addr,
        Some(asset_id.clone()),
        Some(Assignment::Fungible(1)),
        Some(3600),
        payment_hash_hex.clone(),
        210,
    )
    .await;

    fund_htlc_address(response.htlc_p2tr_script_pubkey.as_str());

    send_asset(
        node1_addr,
        &asset_id,
        Assignment::Fungible(1),
        response.recipient_id.clone(),
        Some(WitnessData {
            amount_sat: 1000,
            blinding: None,
        }),
    )
    .await;
    mine(false);
    refresh_transfers(node1_addr).await;

    htlc_scan(node1_addr, payment_hash_hex.clone()).await;

    let tracker_path = test_dir_node1.join(LDK_DIR);
    let tracker_file =
        std::fs::File::open(tracker_path.join("htlc_tracker.json")).expect("open htlc tracker");
    let tracker = HtlcTrackerStorage::read(&mut std::io::BufReader::new(tracker_file))
        .expect("decode htlc tracker");
    let entry = tracker
        .entries
        .iter()
        .find(|(k, _)| hex_str(&k.0).eq_ignore_ascii_case(&payment_hash_hex))
        .map(|(_, v)| v)
        .expect("htlc tracker entry");

    assert_eq!(entry.status, "FundingDetected");
    let vanilla_count = entry
        .funding
        .iter()
        .filter(|d| d.utxo_kind() == HtlcUtxoKind::Vanilla)
        .count();
    let colored_count = entry
        .funding
        .iter()
        .filter(|d| d.utxo_kind() == HtlcUtxoKind::Colored)
        .count();
    assert_eq!(vanilla_count, 1);
    assert_eq!(colored_count, 1);
}

#[serial_test::serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[traced_test]
async fn htlc_tracker_endpoint_returns_entry() {
    initialize();

    let test_dir_base = format!("{TEST_DIR_BASE}htlc_tracker_endpoint/");
    let (node1_addr, _test_dir_node1) =
        setup_single_node(&test_dir_base, "node1", NODE1_PEER_PORT).await;

    let asset_id = issue_asset_nia(node1_addr).await.asset_id;
    let (_preimage_hex, payment_hash_hex) = random_preimage_and_hash();
    let response = rgb_invoice_htlc_with_random_user(
        node1_addr,
        Some(asset_id.clone()),
        Some(Assignment::Fungible(1)),
        Some(3600),
        payment_hash_hex.clone(),
        210,
    )
    .await;

    fund_htlc_address(response.htlc_p2tr_script_pubkey.as_str());

    send_asset(
        node1_addr,
        &asset_id,
        Assignment::Fungible(1),
        response.recipient_id.clone(),
        Some(WitnessData {
            amount_sat: 1000,
            blinding: Some(STATIC_BLINDING),
        }),
    )
    .await;
    mine(false);
    refresh_transfers(node1_addr).await;

    htlc_scan(node1_addr, payment_hash_hex.clone()).await;

    let tracker = htlc_tracker(node1_addr, payment_hash_hex.clone()).await;
    let entry = tracker.entry.expect("htlc tracker entry");

    assert!(entry.payment_hash.eq_ignore_ascii_case(&payment_hash_hex));
    assert_eq!(entry.htlc_script_pubkey, response.htlc_p2tr_script_pubkey);
    assert_eq!(entry.status, "FundingDetected");
    let vanilla_count = entry
        .funding
        .iter()
        .filter(|d| d.utxo_kind == "Vanilla")
        .count();
    let colored_count = entry
        .funding
        .iter()
        .filter(|d| d.utxo_kind == "Colored")
        .count();
    assert_eq!(vanilla_count, 1);
    assert_eq!(colored_count, 1);
}

fn random_user_pubkey() -> bitcoin::PublicKey {
    let secp = Secp256k1::new();
    let mut rng = OsRng;
    let keypair = SecpKeyPair::new(&secp, &mut rng);
    bitcoin::PublicKey::new(keypair.public_key())
}

#[serial_test::serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[traced_test]
async fn rgb_invoice_htlc_binds_to_p2tr_script() {
    initialize();

    let test_dir_base = format!("{TEST_DIR_BASE}invoice_happy_path/");
    let (node1_addr, test_dir_node1) =
        setup_single_node(&test_dir_base, "node1", NODE1_PEER_PORT).await;
    let asset_id = issue_asset_nia(node1_addr).await.asset_id;

    let (_, payment_hash_hex) = random_preimage_and_hash();

    let current_height = get_block_count();
    let csv = 210;
    let response = rgb_invoice_htlc_with_random_user(
        node1_addr,
        Some(asset_id.clone()),
        Some(Assignment::Fungible(1)),
        Some(3600),
        payment_hash_hex.clone(),
        csv,
    )
    .await;

    println!("rgbinvoicehtlc response: {}", response.invoice);

    assert_eq!(response.batch_transfer_idx, 0);
    assert!(response.expiration_timestamp.is_some());

    let decoded = decode_rgb_invoice(node1_addr, &response.invoice).await;
    assert_eq!(decoded.recipient_type, RecipientType::Witness);
    assert_eq!(decoded.network, ApiBitcoinNetwork::Regtest);
    assert_eq!(decoded.asset_id.as_deref(), Some(asset_id.as_str()));
    assert_eq!(decoded.assignment, Assignment::Fungible(1));
    assert_eq!(decoded.transport_endpoints, vec![PROXY_ENDPOINT_LOCAL]);

    let tracker_path = Path::new(&test_dir_node1).join(LDK_DIR);
    let tracker = read_htlc_tracker(tracker_path.as_path());
    let payment_hash =
        validate_and_parse_payment_hash(payment_hash_hex.as_str()).expect("payment_hash parse");
    let entry = tracker
        .entries
        .get(&payment_hash)
        .expect("htlc tracker entry");
    assert_eq!(entry.status, "Created");
    assert_eq!(entry.recipient_id, response.recipient_id);
    assert_eq!(entry.rgb_invoice, response.invoice);
    assert_eq!(entry.htlc_script_pubkey, response.htlc_p2tr_script_pubkey);
    assert_eq!(entry.t_lock, current_height + csv);
    assert_eq!(entry.min_confirmations, 1);
    assert!(entry.preimage.is_none());
    assert!(entry.funding.is_empty());
    assert!(entry.claim_tapscript_hex.is_some());
    assert!(entry.refund_tapscript_hex.is_some());
    assert!(entry.tapleaf_version.is_some());
    assert!(entry.control_block_hex.is_some());
}

#[serial_test::serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[traced_test]
async fn rgb_invoice_htlc_rejects_invalid_params() {
    initialize();

    let test_dir_base = format!("{TEST_DIR_BASE}invalid_params/");
    let (node1_addr, _test_dir_node1) =
        setup_single_node(&test_dir_base, "node1", NODE1_PEER_PORT).await;

    let user_pubkey = random_user_pubkey();

    let short_payload = RgbInvoiceHtlcRequest {
        asset_id: None,
        assignment: None,
        duration_seconds: Some(0),
        min_confirmations: 1,
        payment_hash: "51".to_string(),
        user_pubkey: user_pubkey.to_string(),
        csv: 100,
    };
    let res = reqwest::Client::new()
        .post(format!("http://{node1_addr}/rgbinvoicehtlc"))
        .json(&short_payload)
        .send()
        .await
        .unwrap();
    check_response_is_nok(
        res,
        StatusCode::BAD_REQUEST,
        "Invalid payment hash",
        "InvalidPaymentHash",
    )
    .await;

    let (_, payment_hash_hex) = random_preimage_and_hash();

    let bad_prefix_payload = RgbInvoiceHtlcRequest {
        asset_id: None,
        assignment: None,
        duration_seconds: Some(0),
        min_confirmations: 1,
        payment_hash: payment_hash_hex,
        user_pubkey: "02".to_string(),
        csv: 100,
    };
    let res = reqwest::Client::new()
        .post(format!("http://{node1_addr}/rgbinvoicehtlc"))
        .json(&bad_prefix_payload)
        .send()
        .await
        .unwrap();
    check_response_is_nok(
        res,
        StatusCode::BAD_REQUEST,
        "Invalid user compressed pubkey",
        "InvalidHtlcParams",
    )
    .await;
}

async fn rgb_invoice_htlc_with_random_user(
    node_address: SocketAddr,
    asset_id: Option<String>,
    assignment: Option<Assignment>,
    duration_seconds: Option<u32>,
    payment_hash: String,
    csv: u32,
) -> RgbInvoiceHtlcResponse {
    let user_pubkey = random_user_pubkey();
    rgb_invoice_htlc(
        node_address,
        asset_id,
        assignment,
        duration_seconds,
        payment_hash,
        user_pubkey.to_string(),
        csv,
    )
    .await
}

#[serial_test::serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[traced_test]
async fn submarine_swap_sweeper_broadcasts_mixed_claim() {
    initialize();

    let test_dir_base = format!("{TEST_DIR_BASE}sweeper_full_lifecycle/");
    let (node1_addr, test_dir_node1, app_state) =
        setup_single_node_with_state(&test_dir_base, "node1", NODE1_PEER_PORT).await;
    let test_dir_node1 = Path::new(&test_dir_node1);

    let asset_id = issue_asset_nia(node1_addr).await.asset_id;
    let (preimage_hex, payment_hash_hex) = random_preimage_and_hash();
    let response = rgb_invoice_htlc_with_random_user(
        node1_addr,
        Some(asset_id.clone()),
        Some(Assignment::Fungible(1)),
        Some(3600),
        payment_hash_hex.clone(),
        210,
    )
    .await;

    send_asset(
        node1_addr,
        &asset_id,
        Assignment::Fungible(1),
        response.recipient_id.clone(),
        Some(WitnessData {
            amount_sat: 1000,
            blinding: Some(STATIC_BLINDING),
        }),
    )
    .await;
    mine(false);
    refresh_transfers(node1_addr).await;

    fund_htlc_address(response.htlc_p2tr_script_pubkey.as_str());

    htlc_claim(node1_addr, payment_hash_hex.clone(), preimage_hex.clone()).await;

    let unlocked_guard = app_state.get_unlocked_app_state().await;
    let unlocked_state = unlocked_guard.as_ref().expect("unlocked state").clone();
    drop(unlocked_guard);
    let spender = unlocked_state.htlc_output_spender.clone();
    let (sender, receiver) = tokio::sync::oneshot::channel();
    std::thread::spawn(move || {
        spender.sweep_htlc_tracker_for_tests();
        let _ = sender.send(());
    });
    receiver.await.expect("sweeper thread failed");

    let tracker_path = test_dir_node1.join(LDK_DIR);
    let tracker_file =
        std::fs::File::open(tracker_path.join("htlc_tracker.json")).expect("open htlc tracker");
    let tracker = HtlcTrackerStorage::read(&mut std::io::BufReader::new(tracker_file))
        .expect("decode htlc tracker");
    let entry = tracker
        .entries
        .iter()
        .find(|(k, _)| hex_str(&k.0).eq_ignore_ascii_case(&payment_hash_hex))
        .map(|(_, v)| v)
        .expect("htlc tracker entry");
    assert!(matches!(
        entry.status.as_str(),
        "SweepBroadcast" | "ClaimConfirmed"
    ));
}
