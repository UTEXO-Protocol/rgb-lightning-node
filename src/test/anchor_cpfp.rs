use super::*;

const NODE1_PEER_PORT: u16 = 9811;
const NODE2_PEER_PORT: u16 = 9812;
const TEST_DIR_BASE: &str = "tmp/anchor_cpfp/";

#[serial_test::serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[traced_test]
async fn anchor_cpfp_uses_bitcoind_package_submission() {
    initialize();
    let packages = Arc::new(Mutex::new(Vec::<Vec<String>>::new()));
    let reject = Arc::new(AtomicBool::new(true));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let rpc = axum::Router::new().route("/", axum::routing::post({
        let packages = packages.clone();
        let reject = reject.clone();
        move |axum::Json(request): axum::Json<serde_json::Value>| {
            let packages = packages.clone();
            let reject = reject.clone();
            async move {
                if request["method"] == "submitpackage" {
                    let txs: Vec<String> = serde_json::from_value(request["params"][0].clone()).unwrap();
                    let child: bitcoin::Transaction = deserialize_hex(txs.last().unwrap()).unwrap();
                    packages.lock().unwrap().push(txs);
                    if reject.load(Ordering::SeqCst) {
                        return axum::Json(serde_json::json!({
                            "id": request["id"], "error": null,
                            "result": {"package_msg": "transaction failed", "tx-results": {
                                child.compute_wtxid().to_string(): {"txid": child.compute_txid().to_string(), "error": "injected package rejection"}
                            }}
                        }));
                    }
                }
                let response = reqwest::Client::new()
                    .post("http://127.0.0.1:18443")
                    .basic_auth("user", Some("password"))
                    .json(&request)
                    .send()
                    .await;
                let response = match response {
                    Ok(response) => response.json::<serde_json::Value>().await,
                    Err(error) => Err(error),
                };
                axum::Json(match response {
                    Ok(result) => result,
                    Err(error) => serde_json::json!({
                        "id": request["id"],
                        "result": null,
                        "error": {"code": -1, "message": error.to_string()}
                    }),
                })
            }
        }
    }));
    let proxy = tokio::spawn(async move { axum::serve(listener, rpc).await.unwrap() });
    let mut nodes = Vec::new();
    let mut paused = false;
    let result = AssertUnwindSafe(async {
        let (node1, _) = start_node_with(
            &format!("{TEST_DIR_BASE}node1"),
            NODE1_PEER_PORT,
            false,
            LdkChainSync::BlockSync {
                bitcoind_rpc_username: "user".to_string(),
                bitcoind_rpc_password: "password".to_string(),
                bitcoind_rpc_host: "127.0.0.1".to_string(),
                bitcoind_rpc_port: port,
            },
        )
        .await;
        nodes.push(node1);
        let (node2, _) = start_node_with(
            &format!("{TEST_DIR_BASE}node2"),
            NODE2_PEER_PORT,
            false,
            LdkChainSync::BlockSync {
                bitcoind_rpc_username: "user".to_string(),
                bitcoind_rpc_password: "password".to_string(),
                bitcoind_rpc_host: "127.0.0.1".to_string(),
                bitcoind_rpc_port: 18443,
            },
        )
        .await;
        nodes.push(node2);
        fund_and_create_utxos(node1, None).await;
        fund_and_create_utxos(node2, None).await;
        let peer = node_info(node2).await.pubkey;
        let channel = open_channel(
            node1,
            &peer,
            Some(NODE2_PEER_PORT),
            Some(100_000),
            None,
            None,
            None,
        )
        .await;
        let channel_id = ChannelId(
            hex_str_to_vec(&channel.channel_id)
                .unwrap()
                .try_into()
                .unwrap(),
        );
        stop_mining();
        paused = true;
        check_response_is_ok(
            reqwest::Client::new()
                .post(format!("http://{node1}/closechannel"))
                .json(&CloseChannelRequest {
                    channel_id: channel.channel_id.clone(),
                    peer_pubkey: peer.clone(),
                    force: true,
                })
                .send()
                .await
                .unwrap(),
        )
        .await;
        let app = test_get_app_state(node1);
        let unlocked = app.get_unlocked_app_state().await.as_ref().unwrap().clone();
        let event = tokio::time::timeout(std::time::Duration::from_secs(60), async {
            loop {
                if let Some(event) = unlocked.cpfp_state.event_for(&channel_id, &peer) {
                    break event;
                }
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
        })
        .await
        .expect("LDK did not emit an anchor CPFP event");
        let BumpTransactionEvent::ChannelClose {
            commitment_tx,
            anchor_descriptor,
            ..
        } = &event
        else {
            unreachable!()
        };
        let parent = commitment_tx.clone();
        let parent_id = parent.compute_txid().to_string();
        let anchor = anchor_descriptor.outpoint;
        assert_eq!(anchor.txid.to_string(), parent_id);
        let mempool: Vec<String> = serde_json::from_str(&bitcoind(&["getrawmempool"])).unwrap();
        if !mempool.contains(&parent_id) {
            bitcoind(&["sendrawtransaction", &serialize_hex(&parent)]);
        }
        let stuck: Vec<String> = serde_json::from_str(&bitcoind(&["getrawmempool"])).unwrap();
        assert!(
            stuck.contains(&parent_id),
            "parent should remain unconfirmed while mining is stopped"
        );
        tokio::time::timeout(std::time::Duration::from_secs(60), async {
            loop {
                let automatic_attempt_finished = unlocked
                    .cpfp_state
                    .record_for(&channel_id)
                    .is_some_and(|record| {
                        matches!(record.status.as_str(), "accepted" | "retryable_failure")
                    });
                if automatic_attempt_finished {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
        })
        .await
        .expect("automatic parent broadcast did not finish");
        let mut event = event;
        let BumpTransactionEvent::ChannelClose {
            package_target_feerate_sat_per_1000_weight,
            ..
        } = &mut event
        else {
            unreachable!()
        };
        *package_target_feerate_sat_per_1000_weight = 5_000;
        unlocked.cpfp_state.register_event(&event).unwrap();
        let bump = || async {
            check_response_is_ok(
                reqwest::Client::new()
                    .post(format!("http://{node1}/bumpforceclosefee"))
                    .json(&BumpForceCloseFeeRequest {
                        channel_id: channel.channel_id.clone(),
                        peer_pubkey: peer.clone(),
                    })
                    .send()
                    .await
                    .unwrap(),
            )
            .await
            .json::<BumpForceCloseFeeResponse>()
            .await
            .unwrap()
        };
        let failed = bump().await;
        assert_eq!(failed.status, "retryable_failure");
        assert!(failed
            .last_error
            .unwrap()
            .contains("injected package rejection"));
        assert!(failed.child_txid.is_some());
        let first_package = packages.lock().unwrap().last().unwrap().clone();
        let first_child: bitcoin::Transaction =
            deserialize_hex(first_package.last().unwrap()).unwrap();
        let package_parent: bitcoin::Transaction = deserialize_hex(&first_package[0]).unwrap();
        assert_eq!(package_parent, parent);
        assert_eq!(first_child.input[0].previous_output, anchor);
        reject.store(false, Ordering::SeqCst);
        let accepted = bump().await;
        assert_eq!(accepted.status, "accepted", "{:?}", accepted.last_error);
        assert_eq!(accepted.backend, "bitcoind");
        assert_eq!(accepted.commitment_txid, parent_id);
        assert!(accepted.last_error.is_none());
        let child_id = accepted.child_txid.unwrap();
        let submitted = packages.lock().unwrap().last().unwrap().clone();
        assert_eq!(submitted.len(), 2);
        assert_eq!(submitted[0], serialize_hex(&parent));
        let child: bitcoin::Transaction = deserialize_hex(&submitted[1]).unwrap();
        assert_eq!(child.compute_txid().to_string(), child_id);
        assert!(child
            .input
            .iter()
            .any(|input| input.previous_output == anchor));
        let parent_entry: serde_json::Value =
            serde_json::from_str(&bitcoind(&["getmempoolentry", &parent_id])).unwrap();
        let child_entry: serde_json::Value =
            serde_json::from_str(&bitcoind(&["getmempoolentry", &child_id])).unwrap();
        let fee = |entry: &serde_json::Value| {
            Amount::from_btc(entry["fees"]["base"].as_f64().unwrap())
                .unwrap()
                .to_sat()
        };
        assert!(
            (fee(&parent_entry) + fee(&child_entry)) * 1_000
                >= 5_000 * (parent.weight().to_wu() + child.weight().to_wu())
        );
        let records: HashMap<String, crate::cpfp::ChannelCloseBumpRecord> =
            serde_json::from_slice(&unlocked.kv_store.read("cpfp", "", "channel_close").unwrap())
                .unwrap();
        let record = records
            .values()
            .find(|record| {
                record.channel_id == channel.channel_id && record.commitment_txid == parent_id
            })
            .expect("persisted CPFP record");
        assert_eq!(record.status, "accepted");
        assert_eq!(record.child_txid.as_deref(), Some(child_id.as_str()));
        let generated: serde_json::Value =
            serde_json::from_str(&bitcoind(&["-generate", "1"])).unwrap();
        let blocks = generated["blocks"].as_array().unwrap();
        let block: serde_json::Value =
            serde_json::from_str(&bitcoind(&["getblock", blocks[0].as_str().unwrap()])).unwrap();
        let txids = block["tx"].as_array().unwrap();
        assert!(txids.contains(&serde_json::json!(parent_id)));
        assert!(txids.contains(&serde_json::json!(child_id)));
    })
    .catch_unwind()
    .await;
    if paused {
        resume_mining();
    }
    let cleanup = AssertUnwindSafe(async {
        if !nodes.is_empty() {
            shutdown(&nodes).await;
        }
    })
    .catch_unwind()
    .await;
    proxy.abort();
    result.unwrap_or_else(|error| std::panic::resume_unwind(error));
    cleanup.unwrap_or_else(|error| std::panic::resume_unwind(error));
}
