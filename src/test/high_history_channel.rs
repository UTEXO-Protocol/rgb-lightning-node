use super::*;

use std::{fs, time::Instant};

use bitcoin::io;

const TEST_DIR_BASE: &str = "tmp/high_history_channel/";
const DEFAULT_PRE_CHANNEL_TRANSFERS: usize = 87;
const ASSET_AMOUNT: u64 = 1_000;

#[derive(Default)]
struct MemoryStore(std::sync::Mutex<HashMap<(String, String, String), Vec<u8>>>);

impl lightning::util::persist::KVStoreSync for MemoryStore {
    fn read(
        &self,
        primary_namespace: &str,
        secondary_namespace: &str,
        key: &str,
    ) -> Result<Vec<u8>, io::Error> {
        self.0
            .lock()
            .unwrap()
            .get(&(
                primary_namespace.to_owned(),
                secondary_namespace.to_owned(),
                key.to_owned(),
            ))
            .cloned()
            .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))
    }

    fn write(
        &self,
        primary_namespace: &str,
        secondary_namespace: &str,
        key: &str,
        buf: Vec<u8>,
    ) -> Result<(), io::Error> {
        self.0.lock().unwrap().insert(
            (
                primary_namespace.to_owned(),
                secondary_namespace.to_owned(),
                key.to_owned(),
            ),
            buf,
        );
        Ok(())
    }

    fn remove(
        &self,
        primary_namespace: &str,
        secondary_namespace: &str,
        key: &str,
        _lazy: bool,
    ) -> Result<(), io::Error> {
        self.0.lock().unwrap().remove(&(
            primary_namespace.to_owned(),
            secondary_namespace.to_owned(),
            key.to_owned(),
        ));
        Ok(())
    }

    fn list(
        &self,
        primary_namespace: &str,
        secondary_namespace: &str,
    ) -> Result<Vec<String>, io::Error> {
        Ok(self
            .0
            .lock()
            .unwrap()
            .keys()
            .filter(|(primary, secondary, _)| {
                primary == primary_namespace && secondary == secondary_namespace
            })
            .map(|(_, _, key)| key.clone())
            .collect())
    }
}

#[test]
fn funding_acceptance_journal_round_trips() {
    use lightning::rgb_utils::{
        read_pending_funding_acceptance, write_pending_funding_acceptance, FundingAcceptanceStage,
        PendingFundingAcceptance,
    };

    let store = MemoryStore::default();
    let record = PendingFundingAcceptance {
        version: 2,
        temporary_channel_id: "01".repeat(32),
        counterparty_node_id: "02".repeat(33),
        funding_txid: "03".repeat(32),
        funding_output_index: 0,
        consignment_endpoint: "rpc://127.0.0.1:3000/json-rpc".to_owned(),
        push_asset_amount: Some(500),
        stage: FundingAcceptanceStage::Validating,
        consignment: None,
        rgb_info: None,
    };
    write_pending_funding_acceptance(&record, &store).unwrap();

    assert_eq!(
        read_pending_funding_acceptance(record.key(), &store).unwrap(),
        record
    );

    let mut malformed = record;
    malformed.funding_txid = "zz".repeat(32);
    assert!(write_pending_funding_acceptance(&malformed, &store).is_err());
}

#[serial_test::serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn peer_connection_epoch_advances_on_live_reconnect() {
    initialize();
    let node_a_dir = format!("{TEST_DIR_BASE}peer_epoch_a");
    let node_b_dir = format!("{TEST_DIR_BASE}peer_epoch_b");
    let node_a_peer = next_peer_port();
    let node_b_peer = next_peer_port();
    let (node_a, _, _) =
        start_node_with_reuse_addresses(&node_a_dir, node_a_peer, true, false, None).await;
    let (node_b, _, _) =
        start_node_with_reuse_addresses(&node_b_dir, node_b_peer, true, false, None).await;

    let node_b_pubkey = node_info(node_b).await.pubkey;
    let node_b_peer_address = format!("127.0.0.1:{node_b_peer}");
    connect_peer(node_a, &node_b_pubkey, &node_b_peer_address).await;
    let peer_id = PublicKey::from_str(&node_b_pubkey).expect("valid peer public key");
    let initial_epoch = {
        let app_state = test_get_app_state(node_a);
        let unlocked = app_state
            .get_unlocked_app_state()
            .await
            .clone()
            .expect("node A unlocked");
        unlocked
            .channel_manager
            .peer_connection_epoch_for_test(&peer_id)
            .expect("peer connected")
    };

    disconnect_peer(node_a, &node_b_pubkey).await;
    connect_peer(node_a, &node_b_pubkey, &node_b_peer_address).await;
    let reconnected_epoch = {
        let app_state = test_get_app_state(node_a);
        let unlocked = app_state
            .get_unlocked_app_state()
            .await
            .clone()
            .expect("node A unlocked");
        unlocked
            .channel_manager
            .peer_connection_epoch_for_test(&peer_id)
            .expect("peer reconnected")
    };

    assert_eq!(reconnected_epoch, initial_epoch + 1);
    shutdown(&[node_a, node_b]).await;
}

#[serial_test::serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unresolved_rgb_funding_quarantines_http_mutations_and_allows_reads() {
    initialize();
    let node_dir = format!("{TEST_DIR_BASE}recovery_quarantine");
    let node_peer = next_peer_port();
    let (node, _, _) =
        start_node_with_reuse_addresses(&node_dir, node_peer, true, false, None).await;
    let state = test_get_app_state(node);
    let recovery = crate::ldk::RgbFundingRecovery {
        role: crate::ldk::RgbFundingRecoveryRole::Sender,
        funding_txid: "03".repeat(32),
        temporary_channel_id: "01".repeat(32),
        final_channel_id: Some("02".repeat(32)),
        stage: crate::ldk::RgbSenderFundingStage::Broadcasting,
        channel_is_durable: false,
        transaction_is_known: None,
        observation_error: Some("indexer unavailable".to_owned()),
        required_action: crate::ldk::RgbFundingRecoveryRequiredAction::RetryChainObservation,
    };
    let unlocked = state
        .get_unlocked_app_state()
        .await
        .clone()
        .expect("node unlocked");
    unlocked.rgb_funding_recovery_guard.replace(&[recovery]);

    let response = reqwest::Client::new()
        .post(format!("http://{node}/createutxos"))
        .json(&crate::routes::CreateUtxosRequest {
            up_to: false,
            num: Some(1),
            size: Some(32_000),
            fee_rate: FEE_RATE,
            skip_sync: true,
        })
        .send()
        .await
        .expect("HTTP mutation response");
    assert_eq!(response.status(), reqwest::StatusCode::FORBIDDEN);
    let error = response
        .json::<crate::error::APIErrorResponse>()
        .await
        .expect("typed HTTP error");
    assert_eq!(error.name, "RgbFundingRecoveryRequired");

    assert!(list_channels(node).await.is_empty());
    unlocked.rgb_funding_recovery_guard.replace(&[]);
    shutdown(&[node]).await;
}

#[serial_test::serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn finalized_receiver_funding_is_quarantined_after_restart() {
    use lightning::rgb_utils::{
        write_pending_funding_acceptance, FundingAcceptanceStage, PendingFundingAcceptance,
    };

    initialize();
    let node_dir = format!("{TEST_DIR_BASE}receiver_recovery_restart");
    let node_peer = next_peer_port();
    let (node, password, _) =
        start_node_with_reuse_addresses(&node_dir, node_peer, true, false, None).await;
    let funding_txid = "03".repeat(32);
    let temporary_channel_id = "01".repeat(32);
    let state = test_get_app_state(node);
    let unlocked = state
        .get_unlocked_app_state()
        .await
        .clone()
        .expect("node unlocked");
    let record = PendingFundingAcceptance {
        version: 2,
        temporary_channel_id: temporary_channel_id.clone(),
        counterparty_node_id: format!("02{}", "02".repeat(32)),
        funding_txid: funding_txid.clone(),
        funding_output_index: 0,
        consignment_endpoint: PROXY_ENDPOINT_LOCAL.to_owned(),
        push_asset_amount: Some(1),
        stage: FundingAcceptanceStage::Finalized,
        consignment: Some(vec![1]),
        rgb_info: None,
    };
    write_pending_funding_acceptance(&record, unlocked.kv_store.as_ref())
        .expect("persist finalized receiver journal");
    drop(unlocked);
    drop(state);
    shutdown(&[node]).await;

    let (restarted, restarted_password, _) =
        start_node_with_reuse_addresses(&node_dir, node_peer, true, true, None).await;
    assert_eq!(restarted_password, password);

    let response = reqwest::Client::new()
        .get(format!("http://{restarted}/rgbfundingrecoveries"))
        .send()
        .await
        .expect("list receiver recoveries");
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let recoveries = response
        .json::<Vec<crate::ldk::RgbFundingRecovery>>()
        .await
        .expect("typed recovery response");
    let recovery = recoveries
        .iter()
        .find(|recovery| recovery.funding_txid == funding_txid)
        .expect("receiver recovery survives restart");
    assert_eq!(recovery.role, crate::ldk::RgbFundingRecoveryRole::Receiver);
    assert_eq!(recovery.temporary_channel_id, temporary_channel_id);
    assert!(!recovery.channel_is_durable);
    assert_eq!(
        recovery.required_action,
        crate::ldk::RgbFundingRecoveryRequiredAction::ManualChannelStateRecovery
    );

    let mutation = reqwest::Client::new()
        .post(format!("http://{restarted}/createutxos"))
        .json(&crate::routes::CreateUtxosRequest {
            up_to: false,
            num: Some(1),
            size: Some(32_000),
            fee_rate: FEE_RATE,
            skip_sync: true,
        })
        .send()
        .await
        .expect("quarantined mutation response");
    assert_eq!(mutation.status(), reqwest::StatusCode::FORBIDDEN);
    let error = mutation
        .json::<crate::error::APIErrorResponse>()
        .await
        .expect("typed quarantine error");
    assert_eq!(error.name, "RgbFundingRecoveryRequired");

    let unsafe_resolution = reqwest::Client::new()
        .post(format!("http://{restarted}/resolvergbfundingrecovery"))
        .json(&serde_json::json!({
            "funding_txid": funding_txid,
            "action": "resume_broadcast",
        }))
        .send()
        .await
        .expect("receiver broadcast resolution response");
    assert!(!unsafe_resolution.status().is_success());

    let recoveries = reqwest::Client::new()
        .get(format!("http://{restarted}/rgbfundingrecoveries"))
        .send()
        .await
        .expect("receiver recovery remains visible")
        .json::<Vec<crate::ldk::RgbFundingRecovery>>()
        .await
        .expect("typed recovery response");
    assert!(recoveries.iter().any(|recovery| {
        recovery.role == crate::ldk::RgbFundingRecoveryRole::Receiver
            && recovery.funding_txid == funding_txid
    }));

    shutdown(&[restarted]).await;
}

async fn send_full_balance(sender: SocketAddr, receiver: SocketAddr, asset_id: &str) {
    let receive = rgb_invoice_with_assignment(
        receiver,
        None,
        Some(Assignment::Fungible(ASSET_AMOUNT)),
        true,
    )
    .await;
    let decoded = decode_rgb_invoice(sender, &receive.invoice).await;
    assert_eq!(decoded.recipient_id, receive.recipient_id);
    assert_eq!(decoded.assignment, Assignment::Fungible(ASSET_AMOUNT));
    assert!(!decoded.transport_endpoints.is_empty());

    let recipients = HashMap::from([(
        asset_id.to_string(),
        vec![Recipient {
            recipient_id: decoded.recipient_id,
            witness_data: Some(WitnessData {
                amount_sat: 1_000,
                blinding: None,
            }),
            assignment: Assignment::Fungible(ASSET_AMOUNT),
            transport_endpoints: decoded.transport_endpoints,
        }],
    )]);
    send_assets(sender, recipients, true).await;
    mine(false);

    let deadline = Instant::now() + std::time::Duration::from_secs(90);
    loop {
        refresh_transfers(receiver).await;
        refresh_transfers(sender).await;
        let receiver_balance = asset_balance_spendable(receiver, asset_id).await;
        let sender_balance = asset_balance_spendable(sender, asset_id).await;
        if receiver_balance == ASSET_AMOUNT && sender_balance == 0 {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "transfer did not settle: sender={sender_balance}, receiver={receiver_balance}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
}

async fn assert_completed_funding_is_clean(
    sender: SocketAddr,
    receiver: SocketAddr,
    temporary_channel_id: &str,
    final_channel_id: &str,
    funding_txid: &str,
) {
    use lightning::rgb_utils::{RGB_CONSIGNMENT_NS, RGB_FUNDING_ACCEPTANCE_NS, RGB_PRIMARY_NS};

    let deadline = Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let sender_state = test_get_app_state(sender);
        let sender_unlocked = sender_state
            .get_unlocked_app_state()
            .await
            .clone()
            .expect("sender unlocked");
        let receiver_state = test_get_app_state(receiver);
        let receiver_unlocked = receiver_state
            .get_unlocked_app_state()
            .await
            .clone()
            .expect("receiver unlocked");

        let sender_is_clean = sender_unlocked
            .kv_store
            .read(crate::ldk::RGB_SENDER_FUNDING_NAMESPACE, "", funding_txid)
            .is_err()
            && sender_unlocked
                .kv_store
                .read(crate::ldk::PSBT_NAMESPACE, "", funding_txid)
                .is_err()
            && sender_unlocked
                .kv_store
                .read(crate::ldk::PENDING_FUNDING_NAMESPACE, "", final_channel_id)
                .is_err()
            && crate::ldk::list_rgb_funding_recoveries(
                sender_unlocked.channel_manager.as_ref(),
                sender_unlocked.rgb_wallet_wrapper.as_ref(),
                sender_unlocked.kv_store.as_ref(),
            )
            .expect("sender recovery inspection")
            .is_empty();

        let receiver_is_clean = receiver_unlocked
            .kv_store
            .read(
                RGB_PRIMARY_NS,
                RGB_FUNDING_ACCEPTANCE_NS,
                temporary_channel_id,
            )
            .is_err()
            && receiver_unlocked
                .kv_store
                .read(RGB_PRIMARY_NS, RGB_CONSIGNMENT_NS, temporary_channel_id)
                .is_err()
            && receiver_unlocked
                .kv_store
                .read(RGB_PRIMARY_NS, RGB_CONSIGNMENT_NS, funding_txid)
                .is_err();

        if sender_is_clean && receiver_is_clean {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "completed funding left sender or receiver recovery artifacts"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

#[serial_test::serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[traced_test]
async fn durable_rgb_channel_auto_resumes_a_lost_broadcast_safe_event_after_restart() {
    initialize();

    let sender_dir = format!("{TEST_DIR_BASE}resume_broadcast/sender");
    let receiver_dir = format!("{TEST_DIR_BASE}resume_broadcast/receiver");
    let sender_peer_port = next_peer_port();
    let receiver_peer_port = next_peer_port();
    let (mut sender, _) = start_node(&sender_dir, sender_peer_port, false).await;
    let (receiver, _) = start_node(&receiver_dir, receiver_peer_port, false).await;
    fund_and_create_utxos(sender, Some(20)).await;
    fund_and_create_utxos(receiver, Some(20)).await;

    let asset_id = issue_asset_nia_with_amounts(sender, vec![ASSET_AMOUNT])
        .await
        .asset_id;
    let receiver_pubkey = node_info(receiver).await.pubkey;
    crate::ldk::ACK_NEXT_RGB_FUNDING_BROADCAST_SAFE_WITHOUT_PROCESSING
        .store(true, std::sync::atomic::Ordering::Release);

    let open = open_channel_request_raw(
        sender,
        &receiver_pubkey,
        Some(receiver_peer_port),
        Some(100_000),
        Some(0),
        Some(600),
        Some(&asset_id),
        None,
        None,
        None,
        None,
        true,
        true,
    )
    .await
    .expect("RGB channel request with interrupted broadcast-safe event");

    let recovery_deadline = Instant::now() + std::time::Duration::from_secs(60);
    let recovery = loop {
        let response = reqwest::Client::new()
            .get(format!("http://{sender}/rgbfundingrecoveries"))
            .send()
            .await
            .expect("request sender funding recoveries");
        let recoveries = check_response_is_ok(response)
            .await
            .json::<Vec<crate::ldk::RgbFundingRecovery>>()
            .await
            .expect("decode sender funding recoveries");
        if let Some(recovery) = recoveries.into_iter().find(|recovery| {
            recovery.temporary_channel_id == open.temporary_channel_id
                && recovery.channel_is_durable
                && recovery.stage == crate::ldk::RgbSenderFundingStage::BroadcastSafeObserved
        }) {
            break recovery;
        }
        assert!(
            Instant::now() < recovery_deadline,
            "lost broadcast-safe event did not produce a durable recovery"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    };

    assert_eq!(
        recovery.stage,
        crate::ldk::RgbSenderFundingStage::BroadcastSafeObserved
    );
    assert_eq!(
        recovery.required_action,
        crate::ldk::RgbFundingRecoveryRequiredAction::ResumeBroadcast
    );
    assert!(
        get_txout(&recovery.funding_txid).is_empty(),
        "funding transaction was published before explicit recovery"
    );

    let final_channel_id = recovery
        .final_channel_id
        .clone()
        .expect("durable recovery has final channel ID");
    shutdown(&[sender]).await;
    sender = start_node(&sender_dir, sender_peer_port, true).await.0;

    let remaining = check_response_is_ok(
        reqwest::Client::new()
            .get(format!("http://{sender}/rgbfundingrecoveries"))
            .send()
            .await
            .expect("inspect automatic funding recovery after restart"),
    )
    .await
    .json::<Vec<crate::ldk::RgbFundingRecovery>>()
    .await
    .expect("decode automatic funding recovery response");
    assert!(remaining.is_empty(), "startup left RGB funding quarantined");

    let broadcast_deadline = Instant::now() + std::time::Duration::from_secs(30);
    loop {
        if !get_txout(&recovery.funding_txid).is_empty() {
            break;
        }
        assert!(
            Instant::now() < broadcast_deadline,
            "resumed funding transaction was not broadcast"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    connect_peer(
        sender,
        &receiver_pubkey,
        &format!("127.0.0.1:{receiver_peer_port}"),
    )
    .await;
    mine_n_blocks(false, 6);
    let ready_deadline = Instant::now() + std::time::Duration::from_secs(120);
    loop {
        let sender_ready = list_channels(sender)
            .await
            .iter()
            .any(|channel| channel.channel_id == final_channel_id && channel.ready);
        let receiver_ready = list_channels(receiver)
            .await
            .iter()
            .any(|channel| channel.channel_id == final_channel_id && channel.ready);
        if sender_ready && receiver_ready {
            break;
        }
        assert!(
            Instant::now() < ready_deadline,
            "recovered RGB channel did not become ready"
        );
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }

    let receiver_assets = list_assets(receiver).await;
    assert!(
        receiver_assets
            .nia
            .expect("receiver NIA assets")
            .iter()
            .any(|asset| asset.asset_id == asset_id),
        "receiver metadata was not persisted from the validated funding transfer"
    );
    assert_completed_funding_is_clean(
        sender,
        receiver,
        &open.temporary_channel_id,
        &final_channel_id,
        &recovery.funding_txid,
    )
    .await;
    shutdown(&[sender, receiver]).await;

    let receiver_log_path = Path::new(&receiver_dir).join(".ldk/logs/logs.txt");
    let receiver_log = fs::read_to_string(&receiver_log_path).unwrap_or_else(|error| {
        panic!(
            "cannot read receiver log {}: {error}",
            receiver_log_path.display()
        )
    });
    assert!(
        !receiver_log.contains("Saving new asset"),
        "receiver used the legacy metadata path and revalidated the transfer"
    );
}

#[serial_test::serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[traced_test]
#[ignore = "expensive 88-transfer protocol performance experiment"]
async fn reuse_address_history_through_live_channel_handshake() {
    initialize();
    let reuse_addresses = std::env::var("RGB_PERF_REUSE_ADDRESSES").as_deref() != Ok("0");
    let pre_channel_transfers = std::env::var("RGB_PERF_HISTORY_TRANSFERS")
        .ok()
        .map(|value| {
            value
                .parse::<usize>()
                .expect("valid RGB_PERF_HISTORY_TRANSFERS")
        })
        .unwrap_or(DEFAULT_PRE_CHANNEL_TRANSFERS);

    let node_a_dir = format!("{TEST_DIR_BASE}node_a");
    let node_b_dir = format!("{TEST_DIR_BASE}node_b");
    let node_c_dir = format!("{TEST_DIR_BASE}node_c");
    let node_a_peer = next_peer_port();
    let node_b_peer = next_peer_port();
    let node_c_peer = next_peer_port();

    let (node_a, _, _) =
        start_node_with_reuse_addresses(&node_a_dir, node_a_peer, reuse_addresses, false, None)
            .await;
    let (node_b, _, _) =
        start_node_with_reuse_addresses(&node_b_dir, node_b_peer, reuse_addresses, false, None)
            .await;
    fund_and_create_utxos(node_a, Some(20)).await;
    fund_and_create_utxos(node_b, Some(20)).await;

    let asset_id = issue_asset_nia_with_amounts(node_a, vec![ASSET_AMOUNT])
        .await
        .asset_id;
    let history_started = Instant::now();
    for index in 0..pre_channel_transfers {
        let step_started = Instant::now();
        let (sender, receiver) = if index % 2 == 0 {
            (node_a, node_b)
        } else {
            (node_b, node_a)
        };
        send_full_balance(sender, receiver, &asset_id).await;
        println!(
            "RLN_PERF_HISTORY step={} elapsed_ms={}",
            index + 1,
            step_started.elapsed().as_millis()
        );
    }
    let mut opener = if pre_channel_transfers.is_multiple_of(2) {
        node_a
    } else {
        node_b
    };
    let non_holder = if opener == node_a { node_b } else { node_a };
    assert_eq!(
        asset_balance_spendable(opener, &asset_id).await,
        ASSET_AMOUNT
    );
    if pre_channel_transfers > 0 {
        assert_eq!(asset_balance_spendable(non_holder, &asset_id).await, 0);
    }

    let acceptor_indexer = std::env::var("RGB_PERF_ACCEPTOR_INDEXER")
        .unwrap_or_else(|_| crate::utils::ESPLORA_URL_REGTEST.to_string());
    let (mut node_c, _) = start_node_with_reuse_addresses_and_indexer(
        &node_c_dir,
        node_c_peer,
        &acceptor_indexer,
        reuse_addresses,
        false,
    )
    .await;

    let node_c_pubkey = node_info(node_c).await.pubkey;
    let opener_pubkey = node_info(opener).await.pubkey;
    let abort_after_prepare = std::env::var("RGB_PERF_ABORT_AFTER_PREPARE").as_deref() == Ok("1");
    if abort_after_prepare {
        lightning::ln::channelmanager::ABORT_NEXT_RGB_FUNDING_AFTER_PREPARE
            .store(true, std::sync::atomic::Ordering::Release);
    }
    let handshake_started = Instant::now();
    let open = open_channel_request_raw(
        opener,
        &node_c_pubkey,
        Some(node_c_peer),
        Some(100_000),
        Some(0),
        Some(600),
        Some(&asset_id),
        None,
        None,
        None,
        None,
        true,
        true,
    )
    .await
    .expect("live high-history channel request");

    let pending_key = open.temporary_channel_id.clone();
    let acceptance_started_deadline = Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let app_state = test_get_app_state(node_c);
        let unlocked = app_state
            .get_unlocked_app_state()
            .await
            .clone()
            .expect("acceptor unlocked");
        if unlocked
            .kv_store
            .read(
                lightning::rgb_utils::RGB_PRIMARY_NS,
                lightning::rgb_utils::RGB_FUNDING_ACCEPTANCE_NS,
                &pending_key,
            )
            .is_ok()
        {
            break;
        }
        assert!(
            Instant::now() < acceptance_started_deadline,
            "RGB funding acceptance did not start"
        );
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    let list_started = Instant::now();
    let receiver_channels = list_channels(node_c).await;
    let list_elapsed = list_started.elapsed();

    if abort_after_prepare {
        use lightning::rgb_utils::{
            read_pending_funding_acceptance, FundingAcceptanceStage, RGB_CHANNEL_INFO_NS,
            RGB_CHANNEL_INFO_PENDING_NS, RGB_CONSIGNMENT_NS, RGB_FUNDING_ACCEPTANCE_NS,
            RGB_PRIMARY_NS,
        };

        let fault_started = Instant::now();

        let retry_deadline = Instant::now() + std::time::Duration::from_secs(600);
        let retry_record = loop {
            let app_state = test_get_app_state(node_c);
            let unlocked = app_state
                .get_unlocked_app_state()
                .await
                .clone()
                .expect("acceptor remains unlocked");
            if let Ok(record) =
                read_pending_funding_acceptance(&pending_key, unlocked.kv_store.as_ref())
            {
                if record.stage == FundingAcceptanceStage::RetryRequired {
                    break record;
                }
            }
            assert!(
                Instant::now() < retry_deadline,
                "interrupted RGB funding did not become retryable"
            );
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        };
        let abandoned_funding_txid = retry_record.funding_txid.clone();
        let rollback_elapsed = fault_started.elapsed();

        assert!(
            retry_record.consignment.is_none() && retry_record.rgb_info.is_none(),
            "retry evidence must not retain rolled-back RGB payloads"
        );

        let app_state = test_get_app_state(node_c);
        let unlocked = app_state
            .get_unlocked_app_state()
            .await
            .clone()
            .expect("acceptor remains unlocked");
        for namespace in [RGB_CHANNEL_INFO_NS, RGB_CHANNEL_INFO_PENDING_NS] {
            assert!(
                unlocked
                    .kv_store
                    .read(RGB_PRIMARY_NS, namespace, &pending_key)
                    .is_err(),
                "retry cleanup left temporary RGB channel metadata"
            );
        }
        for key in [&pending_key, &retry_record.funding_txid] {
            assert!(
                unlocked
                    .kv_store
                    .read(RGB_PRIMARY_NS, RGB_CONSIGNMENT_NS, key)
                    .is_err(),
                "retry cleanup left a staged RGB consignment"
            );
        }
        assert!(
            unlocked
                .kv_store
                .read(RGB_PRIMARY_NS, RGB_FUNDING_ACCEPTANCE_NS, &pending_key)
                .is_ok_and(|evidence| evidence.len() < 1_024),
            "retry evidence must remain durable"
        );
        assert!(
            get_txout(&abandoned_funding_txid).is_empty(),
            "interrupted funding transaction must not be broadcast"
        );

        let balance_deadline = Instant::now() + std::time::Duration::from_secs(60);
        loop {
            if asset_balance_spendable(opener, &asset_id).await == ASSET_AMOUNT {
                break;
            }
            assert!(
                Instant::now() < balance_deadline,
                "initiator RGB allocation was not released for retry"
            );
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }

        let restart_before_retry =
            std::env::var("RGB_PERF_RESTART_BEFORE_RETRY").as_deref() == Ok("1");
        let restart_elapsed = if restart_before_retry {
            let restart_started = Instant::now();
            shutdown(&[opener, node_c]).await;
            let opener_dir = if opener == node_a {
                &node_a_dir
            } else {
                &node_b_dir
            };
            let opener_peer = if opener == node_a {
                node_a_peer
            } else {
                node_b_peer
            };
            opener = start_node_with_reuse_addresses(
                opener_dir,
                opener_peer,
                reuse_addresses,
                true,
                None,
            )
            .await
            .0;
            node_c = start_node_with_reuse_addresses_and_indexer(
                &node_c_dir,
                node_c_peer,
                &acceptor_indexer,
                reuse_addresses,
                true,
            )
            .await
            .0;
            assert_eq!(
                asset_balance_spendable(opener, &asset_id).await,
                ASSET_AMOUNT,
                "restart recovery changed the sender RGB allocation"
            );
            restart_started.elapsed()
        } else {
            std::time::Duration::ZERO
        };

        let retry_started = Instant::now();
        let retry = open_channel_request_raw(
            opener,
            &node_c_pubkey,
            Some(node_c_peer),
            Some(100_000),
            Some(0),
            Some(600),
            Some(&asset_id),
            None,
            None,
            None,
            None,
            true,
            true,
        )
        .await
        .expect("fresh RGB funding retry");

        let retry_funding_deadline = Instant::now() + std::time::Duration::from_secs(600);
        let (retry_funding_txid, retry_channel_id) = loop {
            let channels = list_channels(node_c).await;
            if let Some(channel) = channels
                .iter()
                .find(|channel| channel.asset_id.as_deref() == Some(&asset_id))
            {
                if let Some(funding_txid) = channel.funding_txid.clone() {
                    break (funding_txid, channel.channel_id.clone());
                }
            }
            assert!(
                Instant::now() < retry_funding_deadline,
                "fresh RGB funding retry did not complete"
            );
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        };
        let retry_broadcast_deadline = Instant::now() + std::time::Duration::from_secs(30);
        loop {
            if !get_txout(&retry_funding_txid).is_empty() {
                break;
            }
            assert!(
                Instant::now() < retry_broadcast_deadline,
                "fresh retry funding transaction was not broadcast"
            );
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        mine_n_blocks(false, 6);
        let ready_deadline = Instant::now() + std::time::Duration::from_secs(120);
        loop {
            if list_channels(opener)
                .await
                .iter()
                .any(|channel| channel.channel_id == retry_channel_id && channel.ready)
            {
                break;
            }
            assert!(
                Instant::now() < ready_deadline,
                "retried channel did not become ready"
            );
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }

        assert_completed_funding_is_clean(
            opener,
            node_c,
            &retry.temporary_channel_id,
            &retry_channel_id,
            &retry_funding_txid,
        )
        .await;

        println!(
            "RLN_SAFETY_RETRY_COMPLETE pre_channel_transfers={} reuse_addresses={} acceptor_indexer={} concurrent_list_channels_ms={} rollback_ms={} restart_before_retry={} restart_ms={} retry_channel_ms={} abandoned_funding_txid={} retry_funding_txid={} first_temporary_channel_id={} retry_temporary_channel_id={} retry_channel_id={}",
            pre_channel_transfers,
            reuse_addresses,
            acceptor_indexer,
            list_elapsed.as_millis(),
            rollback_elapsed.as_millis(),
            restart_before_retry,
            restart_elapsed.as_millis(),
            retry_started.elapsed().as_millis(),
            abandoned_funding_txid,
            retry_funding_txid,
            open.temporary_channel_id,
            retry.temporary_channel_id,
            retry_channel_id,
        );
        shutdown(&[opener, non_holder, node_c]).await;
        return;
    }

    let funding_deadline = Instant::now() + std::time::Duration::from_secs(600);
    let (funding_txid, channel_id) = loop {
        let channels = list_channels(node_c).await;
        if let Some(channel) = channels.iter().find(|channel| {
            channel.peer_pubkey == opener_pubkey && channel.asset_id.as_deref() == Some(&asset_id)
        }) {
            if let Some(funding_txid) = channel.funding_txid.clone() {
                break (funding_txid, channel.channel_id.clone());
            }
        }
        assert!(
            Instant::now() < funding_deadline,
            "funding transaction was not created"
        );
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    };
    let funding_elapsed = handshake_started.elapsed();

    let broadcast_deadline = Instant::now() + std::time::Duration::from_secs(30);
    loop {
        if !get_txout(&funding_txid).is_empty() {
            break;
        }
        assert!(
            Instant::now() < broadcast_deadline,
            "funding transaction was not broadcast"
        );
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    mine_n_blocks(false, 6);
    let ready_deadline = Instant::now() + std::time::Duration::from_secs(120);
    loop {
        let channels = list_channels(opener).await;
        if channels
            .iter()
            .any(|channel| channel.channel_id == channel_id && channel.ready)
        {
            break;
        }
        assert!(
            Instant::now() < ready_deadline,
            "channel did not become ready"
        );
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }

    assert_completed_funding_is_clean(
        opener,
        node_c,
        &open.temporary_channel_id,
        &channel_id,
        &funding_txid,
    )
    .await;

    println!(
        "RLN_PERF_COMPLETE pre_channel_transfers={} expected_funding_history={} reuse_addresses={} asset_id={} acceptor_indexer={} history_ms={} funding_created_ms={} concurrent_list_channels_ms={} receiver_channels_seen={} funding_txid={} temporary_channel_id={} channel_id={}",
        pre_channel_transfers,
        pre_channel_transfers + 1,
        reuse_addresses,
        asset_id,
        acceptor_indexer,
        history_started.elapsed().as_millis(),
        funding_elapsed.as_millis(),
        list_elapsed.as_millis(),
        receiver_channels.len(),
        funding_txid,
        open.temporary_channel_id,
        channel_id,
    );

    shutdown(&[node_a, node_b, node_c]).await;
}
