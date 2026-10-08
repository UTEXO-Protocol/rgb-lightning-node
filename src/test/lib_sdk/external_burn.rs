//! Funded, strict-signer burn tests. Uses only a local regtest chain; never resets the stack.
use bitcoin::psbt::Psbt;
use hex::DisplayHex;
use rgb_lightning_node::signer_integration_wire::{
    decode_signer_request_wire, encode_signer_response_wire, SignerRequest, SignerResponse,
};
use rgb_lightning_node::*;
use serde_json::{json, Value};
use serial_test::serial;
use std::path::Path;
use std::str::FromStr;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

fn setting(name: &str, default: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| default.into())
}

fn indexer() -> String {
    setting("RLN_BURN_INDEXER", "tcp://127.0.0.1:50001")
}

fn proxy() -> String {
    setting("RLN_BURN_PROXY", "rpc://127.0.0.1:3000/json-rpc")
}

fn rpc(method: &str, params: Value) -> Value {
    let url = setting("RLN_BURN_BITCOIND", "http://127.0.0.1:18443/wallet/miner");
    let parsed = reqwest::Url::parse(&url).unwrap();
    assert!(matches!(
        parsed.host_str(),
        Some("127.0.0.1" | "localhost" | "[::1]")
    ));
    let rt = tokio::runtime::Runtime::new().unwrap();
    let result: Value = rt.block_on(async {
        reqwest::Client::new()
            .post(url)
            .basic_auth(
                setting("RLN_BURN_RPC_USER", "user"),
                Some(setting("RLN_BURN_RPC_PASSWORD", "password")),
            )
            .timeout(Duration::from_secs(30))
            .json(&json!({"jsonrpc":"2.0", "id":"burn-test", "method":method, "params":params}))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap()
    });
    assert!(result["error"].is_null(), "{method}: {result}");
    result["result"].clone()
}

fn mine() {
    let address = rpc("getnewaddress", json!([]));
    rpc("generatetoaddress", json!([6, address]));
    std::thread::sleep(Duration::from_secs(2));
}

fn wait(mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(90);
    while !ready() {
        assert!(Instant::now() < deadline, "burn test timed out");
        std::thread::sleep(Duration::from_millis(500));
    }
}

struct Wallet(SdkNode);
impl Drop for Wallet {
    fn drop(&mut self) {
        self.0.shutdown();
    }
}

fn node(path: &Path) -> Wallet {
    std::fs::create_dir_all(path).unwrap();
    Wallet(
        SdkNode::create(SdkInitRequest {
            storage_dir_path: path.display().to_string(),
            daemon_listening_port: 0,
            ldk_peer_listening_port: 0,
            network: "regtest".into(),
            max_media_upload_size_mb: 5,
            enable_virtual_channels_v0: Some(false),
            virtual_peer_pubkeys: None,
            lsp_base_url: None,
            lsp_bearer_token: None,
            vss_url: None,
            vss_allow_http: false,
            vss_allow_empty_restore: false,
            reuse_addresses: false,
        })
        .unwrap(),
    )
}

fn unlock_request() -> SdkExternalUnlockRequest {
    SdkExternalUnlockRequest {
        ldk_chain_sync: SdkLdkChainSync::TransactionSync {
            indexer_url: indexer(),
        },
        indexer_url: Some(indexer()),
        proxy_endpoint: Some(proxy()),
        announce_addresses: vec![],
        announce_alias: None,
        eth_rpc_url: None,
    }
}

fn fund(node: &SdkNode) {
    rpc(
        "sendtoaddress",
        json!([node.address().unwrap().address.to_string(), 0.02]),
    );
    mine();
    wait(|| node.btc_balance(false).unwrap().vanilla.spendable >= 1_900_000);
    node.createutxos(SdkCreateUtxosRequest {
        up_to: false,
        num: Some(8),
        size: Some(100_000),
        fee_rate: 2,
        skip_sync: false,
    })
    .unwrap();
    mine();
    node.sync().unwrap();
}

fn native_signer() -> Arc<NativeExternalSigner> {
    let seed: [u8; 32] = rand::random();
    NativeExternalSigner::new(seed.as_hex().to_string(), "regtest".into(), Some(false)).unwrap()
}

fn balance(node: &SdkNode, asset: ContractId) -> u64 {
    node.refreshtransfers(SdkRefreshTransfersRequest { skip_sync: false })
        .unwrap();
    node.asset_balance(asset).unwrap().spendable
}

fn burn_request(asset: ContractId, amount: u64) -> BurnRequest {
    BurnRequest {
        asset_id: asset,
        amount,
        burn_recipient: None,
        fee_rate: 2,
        min_confirmations: 1,
    }
}

// 0: normal, 1: refuse signing, 2: alter transaction, 3: unsigned response,
// 4: sign successfully but lose the response. Only test transactions are affected.
struct BurnSigner {
    inner: Arc<NativeExternalSigner>,
    fault: AtomicU8,
    prepared_txid: Mutex<Option<String>>,
}

impl ExternalSignerHost for BurnSigner {
    fn call(&self, request: Vec<u8>) -> Result<Vec<u8>, RlnError> {
        let decoded = decode_signer_request_wire(&request).map_err(RlnError::Internal)?;
        if let SignerRequest::SignRgbPsbt { psbt, .. } = decoded {
            let mut prepared = Psbt::from_str(&psbt).unwrap();
            *self.prepared_txid.lock().unwrap() =
                Some(prepared.unsigned_tx.compute_txid().to_string());
            match self.fault.load(Ordering::SeqCst) {
                1 => return Err(RlnError::Internal("burn signer unavailable".into())),
                2 | 3 => {
                    if self.fault.load(Ordering::SeqCst) == 2 {
                        prepared.unsigned_tx.output[0].value = bitcoin::Amount::from_sat(1);
                    }
                    return encode_signer_response_wire(&SignerResponse::SignedPsbt {
                        psbt: prepared.to_string(),
                    })
                    .map_err(RlnError::Internal);
                }
                4 => {
                    self.inner.call(request)?;
                    return Err(RlnError::Internal("burn signed response lost".into()));
                }
                _ => {}
            }
        }
        self.inner.call(request)
    }
}

fn receive(issuer: &SdkNode, receiver: &SdkNode, asset: ContractId, amount: u64) {
    let invoice = receiver
        .rgbinvoice(SdkRgbInvoiceRequest {
            asset_id: None,
            assignment_kind: Some(AssignmentKind::Fungible),
            assignment_amount: Some(amount),
            duration_seconds: Some(3600),
            min_confirmations: 1,
            witness: false,
        })
        .unwrap();
    issuer
        .send_rgb(SendRgbRequest {
            donation: true,
            fee_rate: 2,
            min_confirmations: 1,
            recipient_groups: vec![AssetRecipients {
                asset_id: asset,
                recipients: vec![RgbRecipient {
                    recipient_id: invoice.recipient_id,
                    witness_data: None,
                    assignment_kind: AssignmentKind::Fungible,
                    assignment_amount: Some(amount),
                    transport_endpoints: vec![TransportEndpoint(proxy())],
                }],
            }],
        })
        .unwrap();
    mine();
    wait(|| {
        receiver
            .refreshtransfers(SdkRefreshTransfersRequest { skip_sync: false })
            .unwrap();
        receiver
            .asset_balance(asset)
            .is_ok_and(|b| b.spendable == amount)
    });
    issuer
        .refreshtransfers(SdkRefreshTransfersRequest { skip_sync: false })
        .unwrap();
}

#[test]
#[serial]
fn external_burn_funded_and_recovery() {
    assert_eq!(rpc("getblockchaininfo", json!([]))["chain"], "regtest");
    let root = tempfile::Builder::new()
        .prefix("rln-external-burn-")
        .tempdir()
        .unwrap()
        .keep();
    eprintln!("burn test evidence: {}", root.display());
    let issuer = node(&root.join("issuer"));
    issuer.0.init("burn-test-password".into(), None).unwrap();
    issuer
        .0
        .unlock(SdkUnlockRequest {
            password: "burn-test-password".into(),
            ldk_chain_sync: SdkLdkChainSync::TransactionSync {
                indexer_url: indexer(),
            },
            indexer_url: Some(indexer()),
            eth_rpc_url: None,
            proxy_endpoint: Some(proxy()),
            announce_addresses: vec![],
            announce_alias: None,
            gossip_rgs_server_url: None,
        })
        .unwrap();
    fund(&issuer.0);
    let asset = issuer
        .0
        .issueassetifa(SdkIssueAssetIfaRequest {
            amounts: vec![2000],
            inflation_amounts: vec![],
            ticker: "BURN".into(),
            name: "Burn regression".into(),
            precision: 0,
            reject_list_url: None,
            issuance_type: None,
        })
        .unwrap()
        .asset_id;

    let native = native_signer();
    let wallet_path = root.join("external");
    let wallet = node(&wallet_path);
    wallet
        .0
        .init_with_native_external_signer(native.clone())
        .unwrap();
    wallet
        .0
        .unlock_with_native_external_signer_request(native.clone(), unlock_request())
        .unwrap();
    fund(&wallet.0);
    receive(&issuer.0, &wallet.0, asset, 1000);

    // Validate parameters before reserving any inputs; the valid request must still work.
    assert!(wallet.0.burn(burn_request(asset, 0)).is_err());
    assert!(wallet.0.burn(burn_request(asset, 1001)).is_err());
    let mut invalid = burn_request(asset, 10);
    invalid.burn_recipient = Some(vec![1; 31]);
    assert!(wallet.0.burn(invalid).is_err());
    assert_eq!(balance(&wallet.0, asset), 1000);

    let burned = wallet.0.burn(burn_request(asset, 100)).unwrap();
    assert!(burned.batch_transfer_idx > 0);
    rpc("getrawtransaction", json!([burned.txid.to_string(), true]));
    mine();
    wait(|| balance(&wallet.0, asset) == 900);
    assert!(!wallet
        .0
        .get_consignment(asset, burned.txid)
        .unwrap()
        .is_empty());
    assert!(wallet
        .0
        .list_transfers(Some(asset), Some(burned.txid.to_string()))
        .unwrap()
        .iter()
        .any(|t| t.kind == "Burn" && t.status == "Settled"));

    // Exercise the attached-host surface and ensure pending reservations survive reopening.
    drop(wallet);
    let mut wallet = node(&wallet_path);
    let host = Arc::new(BurnSigner {
        inner: native.clone(),
        fault: AtomicU8::new(0),
        prepared_txid: Mutex::new(None),
    });
    wallet
        .0
        .attach_external_signer(host.clone(), native.bootstrap().unwrap())
        .unwrap();
    wallet
        .0
        .unlock_with_attached_external_signer_request(unlock_request())
        .unwrap();
    assert_eq!(balance(&wallet.0, asset), 900);
    for fault in 1..=4 {
        host.fault.store(fault, Ordering::SeqCst);
        assert!(wallet.0.burn(burn_request(asset, 10)).is_err());
        let detail = take_last_api_error_detail().expect("burn error detail");
        assert!(
            detail.contains("reconcile the transfer before retrying"),
            "{detail}"
        );
        let batch: i32 = detail
            .split("batch_transfer_idx=Some(")
            .nth(1)
            .unwrap()
            .split(')')
            .next()
            .unwrap()
            .parse()
            .unwrap();
        let txid = host.prepared_txid.lock().unwrap().clone().unwrap();
        assert!(!rpc("getrawmempool", json!([]))
            .as_array()
            .unwrap()
            .contains(&json!(txid)));
        let pending = wallet.0.list_transfers(Some(asset), Some(txid)).unwrap();
        assert!(pending
            .iter()
            .any(|t| t.kind == "Burn" && t.status == "Initiated"));
        assert_eq!(wallet.0.asset_balance(asset).unwrap().spendable, 0);
        if fault == 4 {
            drop(wallet);
            wallet = node(&wallet_path);
            wallet
                .0
                .attach_external_signer(host.clone(), native.bootstrap().unwrap())
                .unwrap();
            wallet
                .0
                .unlock_with_attached_external_signer_request(unlock_request())
                .unwrap();
            assert_eq!(wallet.0.asset_balance(asset).unwrap().spendable, 0);
            assert!(wallet
                .0
                .list_transfers(Some(asset), None)
                .unwrap()
                .iter()
                .any(|t| t.kind == "Burn" && t.status == "Initiated"));
        }
        host.fault.store(0, Ordering::SeqCst);
        // This fixture knows it never broadcast. A real ambiguous failure needs independent reconciliation.
        assert!(
            wallet
                .0
                .failtransfers(SdkFailTransfersRequest {
                    batch_transfer_idx: Some(batch),
                    no_asset_only: false,
                    skip_sync: false,
                })
                .unwrap()
                .transfers_changed
        );
        assert_eq!(balance(&wallet.0, asset), 900);
    }
    let burned = wallet.0.burn(burn_request(asset, 50)).unwrap();
    mine();
    wait(|| balance(&wallet.0, asset) == 850);
    assert!(!wallet
        .0
        .get_consignment(asset, burned.txid)
        .unwrap()
        .is_empty());

    // Both requests contend for the same RGB input; at most one may spend it.
    let attempts = std::thread::scope(|scope| {
        let first = scope.spawn(|| wallet.0.burn(burn_request(asset, 25)));
        let second = scope.spawn(|| wallet.0.burn(burn_request(asset, 25)));
        [first.join().unwrap(), second.join().unwrap()]
    });
    assert_eq!(attempts.iter().filter(|result| result.is_ok()).count(), 1);
    mine();
    wait(|| balance(&wallet.0, asset) == 825);

    // Password-signer behavior is unchanged.
    let burned = issuer.0.burn(burn_request(asset, 25)).unwrap();
    mine();
    wait(|| balance(&issuer.0, asset) == 975);
    assert!(!issuer
        .0
        .get_consignment(asset, burned.txid)
        .unwrap()
        .is_empty());
}

/// Requires a local Anvil BaseBridge funded/approved for the standard test account.
#[test]
#[ignore = "requires a local funded Anvil BaseBridge; see external-burn.md"]
#[serial]
fn external_burn_bfa_proof() {
    use rgb_lib::keys::{generate_keys, WitnessVersion};
    use rgb_lib::wallet::{
        DatabaseType, OnlineOptions, Recipient, RgbWalletOpsOffline, SinglesigKeys,
        Wallet as RgbWallet, WalletData,
    };
    use rgb_lib::{AssetSchema, BitcoinNetwork};

    assert_eq!(rpc("getblockchaininfo", json!([]))["chain"], "regtest");
    let eth_url = std::env::var("RLN_BURN_ETH_RPC").expect("RLN_BURN_ETH_RPC required");
    assert!(matches!(
        reqwest::Url::parse(&eth_url).unwrap().host_str(),
        Some("localhost" | "127.0.0.1")
    ));
    let bridge = std::env::var("RLN_BURN_BRIDGE").expect("RLN_BURN_BRIDGE required");
    let container =
        std::env::var("RLN_BURN_ANVIL_CONTAINER").expect("RLN_BURN_ANVIL_CONTAINER required");
    let rt = tokio::runtime::Runtime::new().unwrap();
    let eth_rpc = |method: &str, params: Value| -> Value {
        let response: Value = rt.block_on(async {
            reqwest::Client::new()
                .post(&eth_url)
                .timeout(Duration::from_secs(30))
                .json(&json!({"jsonrpc":"2.0","id":1,"method":method,"params":params}))
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap()
        });
        assert!(response["error"].is_null(), "{response}");
        response["result"].clone()
    };
    assert_eq!(eth_rpc("eth_chainId", json!([])), "0x7a69");
    assert_ne!(eth_rpc("eth_getCode", json!([bridge, "latest"])), "0x");

    let root = tempfile::Builder::new()
        .prefix("rln-bfa-burn-")
        .tempdir()
        .unwrap()
        .keep();
    eprintln!("BFA burn evidence: {}", root.display());
    let issuer_dir = root.join("issuer");
    std::fs::create_dir_all(&issuer_dir).unwrap();
    let keys = generate_keys(BitcoinNetwork::Regtest, WitnessVersion::Taproot);
    let mut issuer = RgbWallet::new(
        WalletData {
            data_dir: issuer_dir.display().to_string(),
            bitcoin_network: BitcoinNetwork::Regtest,
            database_type: DatabaseType::Sqlite,
            max_allocations_per_utxo: 5,
            supported_schemas: vec![AssetSchema::Bfa],
            reuse_addresses: false,
        },
        SinglesigKeys::from_keys(&keys, None),
    )
    .unwrap();
    let online = issuer
        .go_online(OnlineOptions {
            indexer_url: indexer(),
            skip_consistency_check: false,
            vanilla_sync_lookback: 20,
            eth_rpc_url: Some(eth_url.clone()),
        })
        .unwrap();
    rpc(
        "sendtoaddress",
        json!([issuer.get_address().unwrap(), 0.02]),
    );
    mine();
    wait(|| {
        issuer
            .get_btc_balance(Some(online), false)
            .unwrap()
            .vanilla
            .spendable
            >= 1_900_000
    });
    issuer
        .create_utxos(online, false, Some(8), Some(100_000), 2, false)
        .unwrap();
    mine();
    let asset = issuer
        .issue_asset_bfa(
            "BURNBFA".into(),
            "Burn BFA".into(),
            6,
            1,
            bridge.clone(),
            None,
        )
        .unwrap();
    let asset_id = ContractId::from_str(&asset.asset_id).unwrap();
    let signer = native_signer();
    let wallet = node(&root.join("external"));
    wallet
        .0
        .init_with_native_external_signer(signer.clone())
        .unwrap();
    let mut unlock = unlock_request();
    unlock.eth_rpc_url = Some(eth_url.clone());
    wallet
        .0
        .unlock_with_native_external_signer_request(signer, unlock)
        .unwrap();
    fund(&wallet.0);
    let invoice = wallet
        .0
        .rgbinvoice(SdkRgbInvoiceRequest {
            asset_id: None,
            assignment_kind: Some(AssignmentKind::Fungible),
            assignment_amount: Some(1000),
            duration_seconds: Some(3600),
            min_confirmations: 1,
            witness: false,
        })
        .unwrap();
    let prepared = issuer
        .bridge_begin(
            online,
            asset.asset_id.clone(),
            Recipient {
                recipient_id: invoice.recipient_id.0,
                witness_data: None,
                assignment: rgb_lib::Assignment::Fungible(1000),
                transport_endpoints: vec![proxy()],
            },
            2,
            1,
        )
        .unwrap();
    let lock = std::process::Command::new("docker")
        .args([
            "exec",
            &container,
            "cast",
            "send",
            &bridge,
            "fundsIn(uint256,uint256)",
            "1000",
            &format!("0x{}", prepared.details.opid),
            "--unlocked",
            "--from",
            "0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266",
            "--rpc-url",
            "http://127.0.0.1:8545",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(
        lock.status.success(),
        "{}",
        String::from_utf8_lossy(&lock.stderr)
    );
    let lock: Value = serde_json::from_slice(&lock.stdout).unwrap();
    assert_eq!(lock["status"], "0x1");
    assert_eq!(
        eth_rpc(
            "eth_getTransactionReceipt",
            json!([lock["transactionHash"]])
        )["status"],
        "0x1"
    );
    let signed = issuer.sign_psbt(prepared.psbt, None).unwrap();
    issuer.bridge_end(online, signed).unwrap();
    mine();
    wait(|| {
        wallet
            .0
            .refreshtransfers(SdkRefreshTransfersRequest { skip_sync: false })
            .unwrap();
        wallet
            .0
            .asset_balance(asset_id)
            .is_ok_and(|b| b.spendable == 1000)
    });

    // Missing or malformed payout recipients must not consume the received funds.
    assert!(wallet.0.burn(burn_request(asset_id, 100)).is_err());
    let mut invalid = burn_request(asset_id, 100);
    invalid.burn_recipient = Some(vec![0; 31]);
    assert!(wallet.0.burn(invalid).is_err());
    assert_eq!(balance(&wallet.0, asset_id), 1000);
    for (amount, remaining, recipient) in [(100, 900, vec![0x11; 32]), (200, 700, vec![0x22; 32])] {
        let mut request = burn_request(asset_id, amount);
        request.burn_recipient = Some(recipient.clone());
        let burned = wallet.0.burn(request).unwrap();
        rpc("getrawtransaction", json!([burned.txid.to_string(), true]));
        mine();
        wait(|| balance(&wallet.0, asset_id) == remaining);
        let proof_path = wallet
            .0
            .get_consignment_path(asset_id, burned.txid)
            .unwrap();
        let proof = wallet.0.get_consignment(asset_id, burned.txid).unwrap();
        assert!(!proof.is_empty());
        assert_eq!(std::fs::read(&proof_path).unwrap(), proof);
        assert_eq!(issuer.get_burn_recipient(proof_path).unwrap(), recipient);
    }
}
