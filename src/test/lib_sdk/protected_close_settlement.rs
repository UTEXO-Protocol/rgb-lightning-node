//! Explicitly admitted host regression: a confirmed close is not a confirmed RGB sweep.
//! Never run this against the checkout's default regtest project or a shared wallet.

use bitcoin::secp256k1::PublicKey;
use rgb_lightning_node::{
    ContractId, HtlcStatus, SdkCloseChannelRequest, SdkCreateUtxosRequest, SdkInitRequest,
    SdkIssueAssetNiaRequest, SdkKeysendRequest, SdkLdkChainSync, SdkNode, SdkOpenChannelRequest,
    SdkRefreshTransfersRequest, SdkUnlockRequest,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    collections::HashSet,
    env, fs,
    net::TcpListener,
    path::{Path, PathBuf},
    process::Command,
    str::FromStr,
    thread::sleep,
    time::{Duration, Instant},
};

const PROJECT: &str = "iris_rgb_output_20261026";
const CONTAINER: &str = "iris_rgb_output_20261026-bitcoind-1";
const TREASURY: &str = "iris-output-tests";
const PASSWORD: &str = "owned-close-regression";

struct Fixture {
    compose: PathBuf,
}

impl Fixture {
    fn admit() -> Self {
        assert_eq!(env::var("COMPOSE_PROJECT_NAME").unwrap(), PROJECT);
        let compose = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("qualification-consumers/rgb-output-native-fixture-20261026/compose.yaml")
            .canonicalize()
            .unwrap();
        assert_eq!(
            PathBuf::from(env::var("COMPOSE_FILE").unwrap())
                .canonicalize()
                .unwrap(),
            compose
        );
        let fixture = Self { compose };
        fixture.assert_owner();
        assert_eq!(
            fixture.rpc(false, "getblockchaininfo", &[])["chain"],
            "regtest"
        );
        assert_eq!(fixture.rpc(false, "getmempoolinfo", &[])["size"], 0);
        assert!(fixture.rpc(true, "getbalance", &[]).as_f64().unwrap() >= 0.1);
        let info: Value = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                reqwest::Client::builder()
                    .timeout(Duration::from_secs(10))
                    .build()
                    .unwrap()
                    .post("http://127.0.0.1:13004/json-rpc")
                    .json(&json!({"jsonrpc":"2.0", "id":1, "method":"server.info"}))
                    .send()
                    .await
                    .unwrap()
                    .error_for_status()
                    .unwrap()
                    .json()
                    .await
                    .unwrap()
            });
        assert_eq!(info["result"]["protocol_version"], "1.0");
        for port in [13411, 13412, 15111, 15112] {
            drop(TcpListener::bind(("127.0.0.1", port)).expect("owned test port must be free"));
        }
        fixture
    }

    fn assert_owner(&self) {
        assert_eq!(env::var("COMPOSE_PROJECT_NAME").unwrap(), PROJECT);
        assert_eq!(
            PathBuf::from(env::var("COMPOSE_FILE").unwrap())
                .canonicalize()
                .unwrap(),
            self.compose
        );
        let output = Command::new("docker")
            .args(["inspect", CONTAINER, "--format", "{{json .Config.Labels}}"])
            .output()
            .unwrap();
        assert!(output.status.success());
        let labels: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(labels["com.docker.compose.project"], PROJECT);
        assert_eq!(
            Path::new(
                labels["com.docker.compose.project.config_files"]
                    .as_str()
                    .unwrap()
            )
            .canonicalize()
            .unwrap(),
            self.compose
        );
    }

    fn rpc(&self, wallet: bool, method: &str, arguments: &[Value]) -> Value {
        // Re-admit before every command, including each funding/mining mutation.
        self.assert_owner();
        let mut command = Command::new("docker");
        command.args([
            "exec",
            "-u",
            "blits",
            CONTAINER,
            "bitcoin-cli",
            "-regtest",
            "-rpcuser=user",
            "-rpcpassword=password",
        ]);
        if wallet {
            command.arg(format!("-rpcwallet={TREASURY}"));
        }
        command.arg(method);
        for argument in arguments {
            command.arg(
                argument
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| argument.to_string()),
            );
        }
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "owned RPC {method}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        match method {
            // bitcoin-cli prints these successful string results without JSON quotes.
            "getnewaddress" => {
                let raw = std::str::from_utf8(&output.stdout).unwrap().trim();
                bitcoin::Address::from_str(raw)
                    .unwrap()
                    .require_network(bitcoin::Network::Regtest)
                    .unwrap();
                json!(raw)
            }
            "sendtoaddress" => {
                let raw = std::str::from_utf8(&output.stdout).unwrap().trim();
                let txid = bitcoin::Txid::from_str(raw).unwrap();
                assert_eq!(txid.to_string(), raw);
                json!(raw)
            }
            _ => serde_json::from_slice(&output.stdout).unwrap(),
        }
    }

    fn mine(&self, count: u64) {
        let address = self.rpc(true, "getnewaddress", &[]);
        self.rpc(false, "generatetoaddress", &[json!(count), address]);
    }

    fn transaction(&self, txid: &str) -> Value {
        self.rpc(false, "getrawtransaction", &[json!(txid), json!(true)])
    }

    fn mempool(&self) -> Vec<String> {
        serde_json::from_value(self.rpc(false, "getrawmempool", &[])).unwrap()
    }
}

fn wait<T>(label: &str, mut poll: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + Duration::from_secs(180);
    loop {
        if let Some(value) = poll() {
            return value;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {label}");
        sleep(Duration::from_millis(500));
    }
}

struct OwnedNode(SdkNode);

impl std::ops::Deref for OwnedNode {
    type Target = SdkNode;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl Drop for OwnedNode {
    fn drop(&mut self) {
        self.0.shutdown();
    }
}

struct Nodes {
    a: OwnedNode,
    b: OwnedNode,
}

struct WalletDirectories(Option<tempfile::TempDir>);

impl WalletDirectories {
    fn path(&self) -> &Path {
        self.0.as_ref().unwrap().path()
    }
}

impl Drop for WalletDirectories {
    fn drop(&mut self) {
        if std::thread::panicking() {
            let path = self.0.take().unwrap().keep();
            eprintln!("retained owned fixture diagnostics at {}", path.display());
        }
    }
}

fn node(directory: &Path, daemon: u16, peer: u16) -> OwnedNode {
    fs::create_dir_all(directory).unwrap();
    let node = OwnedNode(
        SdkNode::create(SdkInitRequest {
            storage_dir_path: directory.display().to_string(),
            daemon_listening_port: daemon,
            ldk_peer_listening_port: peer,
            network: "regtest".into(),
            max_media_upload_size_mb: 20,
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
    );
    node.init(PASSWORD.into(), None).unwrap();
    node.unlock(SdkUnlockRequest {
        password: PASSWORD.into(),
        ldk_chain_sync: SdkLdkChainSync::BlockSync {
            bitcoind_rpc_username: "user".into(),
            bitcoind_rpc_password: "password".into(),
            bitcoind_rpc_host: "127.0.0.1".into(),
            bitcoind_rpc_port: 18443,
        },
        // This owned fixture's Electrum endpoint lacks verbose transactions. Its
        // Esplora endpoint exposes the same chain with the resolver data required.
        indexer_url: Some("http://127.0.0.1:3002".into()),
        eth_rpc_url: None,
        proxy_endpoint: Some("rpc://127.0.0.1:13004/json-rpc".into()),
        announce_addresses: vec![],
        announce_alias: None,
        gossip_rgs_server_url: None,
    })
    .unwrap();
    node
}

fn refresh(node: &SdkNode) {
    node.refreshtransfers(SdkRefreshTransfersRequest { skip_sync: false })
        .unwrap();
}

fn balance(node: &SdkNode, asset: &ContractId) -> u64 {
    node.asset_balance(asset.clone()).unwrap().spendable
}

fn settled_balance(node: &SdkNode, asset: &ContractId, expected: u64) {
    wait("settled RGB balance", || {
        refresh(node);
        let value = node.asset_balance(asset.clone()).unwrap();
        (value.spendable == expected && value.settled == expected).then_some(())
    });
}

fn payment(
    node: &SdkNode,
    receiver: &SdkNode,
    recipient: PublicKey,
    asset: Option<ContractId>,
    amount: Option<u64>,
    sats_msat: u64,
) {
    // PaymentSent can precede the final revoke that releases the HTLC slot.
    // Admit the next genuine amount only after LDK advertises sufficient capacity.
    wait("next outbound HTLC capacity", || {
        node.sync().unwrap();
        node.list_channels()
            .unwrap()
            .iter()
            .any(|channel| {
                channel.peer_pubkey == recipient
                    && channel.ready
                    && channel.is_usable
                    && channel.next_outbound_htlc_minimum_msat <= sats_msat
                    && channel.next_outbound_htlc_limit_msat >= sats_msat
            })
            .then_some(())
    });
    println!("protected-close payment: amount_msat={sats_msat} asset_amount={amount:?}");
    let sent = node
        .keysend(SdkKeysendRequest {
            dest_pubkey: recipient,
            amt_msat: sats_msat,
            asset_id: asset,
            asset_amount: amount,
        })
        .unwrap();
    wait("succeeded payment", || {
        [node, receiver]
            .iter()
            .all(|peer| {
                peer.sync().unwrap();
                peer.list_payments().unwrap().into_iter().any(|payment| {
                    payment.payment_hash == sent.payment_hash
                        && matches!(payment.status, HtlcStatus::Succeeded)
                })
            })
            .then_some(())
    });
}

#[derive(Debug, Deserialize)]
struct Preparation {
    batch_transfer_idx: i32,
    txid: String,
    phase: String,
}

fn preparations(root: &Path, storage: &Path) -> Vec<Preparation> {
    let storage = storage.canonicalize().unwrap();
    assert!(storage.starts_with(root.canonicalize().unwrap()));
    let databases: Vec<_> = fs::read_dir(&storage)
        .unwrap()
        .flatten()
        .map(|entry| entry.path().join("rgb_lib_db"))
        .filter(|path| path.is_file())
        .collect();
    assert_eq!(databases.len(), 1, "one native wallet per owned SDK node");
    assert!(databases[0]
        .canonicalize()
        .unwrap()
        .starts_with(root.canonicalize().unwrap()));
    let output = Command::new("python3").args([
        "-c",
        "import json,sqlite3,sys; c=sqlite3.connect('file:'+sys.argv[1]+'?mode=ro',uri=True,timeout=5); c.row_factory=sqlite3.Row; print(json.dumps([dict(r) for r in c.execute('SELECT batch_transfer_idx,txid,phase FROM prepared_rgb_sweep ORDER BY batch_transfer_idx')]))",
    ]).arg(&databases[0]).output().unwrap();
    assert!(
        output.status.success(),
        "read own native journal: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
#[ignore = "requires exactly admitted owned regtest fixture and isolated protocol-1 proxy"]
#[serial_test::serial]
fn protected_close_requires_observed_sweep_confirmation_before_credit() {
    run_protected_close(false);
}

#[test]
#[ignore = "requires exactly admitted owned regtest fixture and isolated protocol-1 proxy"]
#[serial_test::serial]
fn protected_force_close_requires_observed_sweep_confirmation_before_credit() {
    run_protected_close(true);
}

fn run_protected_close(force: bool) {
    let fixture = Fixture::admit(); // Before creating wallets or funding/mining anything.
    let directory = WalletDirectories(Some(
        tempfile::Builder::new()
            .prefix(if force {
                "rln-protected-force-close-"
            } else {
                "rln-protected-close-"
            })
            .tempdir()
            .unwrap(),
    ));
    let a_dir = directory.path().join("a");
    let b_dir = directory.path().join("b");
    let nodes = Nodes {
        a: node(&a_dir, 13411, 15111),
        b: node(&b_dir, 13412, 15112),
    };
    for node in [&nodes.a, &nodes.b] {
        fixture.rpc(
            true,
            "sendtoaddress",
            &[json!(node.address().unwrap().address), json!(0.02)],
        );
    }
    fixture.mine(1);
    for node in [&nodes.a, &nodes.b] {
        wait("confirmed funding", || {
            (node.btc_balance(false).unwrap().vanilla.spendable >= 1_500_000).then_some(())
        });
        node.createutxos(SdkCreateUtxosRequest {
            up_to: false,
            num: Some(10),
            size: Some(100_000),
            fee_rate: 7,
            skip_sync: false,
        })
        .unwrap();
    }
    fixture.mine(1);
    for node in [&nodes.a, &nodes.b] {
        node.sync().unwrap();
    }
    let asset = nodes
        .a
        .issueassetnia(SdkIssueAssetNiaRequest {
            amounts: vec![1000],
            ticker: "UTEST".into(),
            name: "Owned close test".into(),
            precision: 0,
        })
        .unwrap()
        .asset_id;
    let a_pub = nodes.a.node_info().unwrap().pubkey;
    let b_pub = nodes.b.node_info().unwrap().pubkey;
    nodes
        .a
        .connectpeer(format!("{b_pub}@127.0.0.1:15112"))
        .unwrap();
    nodes
        .a
        .openchannel(SdkOpenChannelRequest {
            peer_pubkey_and_opt_addr: format!("{b_pub}@127.0.0.1:15112"),
            capacity_sat: 100_000,
            push_msat: 0,
            public: true,
            with_anchors: true,
            fee_base_msat: None,
            fee_proportional_millionths: None,
            temporary_channel_id: None,
            asset_id: Some(asset.clone()),
            asset_amount: Some(600),
            push_asset_amount: Some(250),
            virtual_open_mode: None,
        })
        .unwrap();
    let funding = wait("channel funding transaction", || {
        nodes
            .a
            .list_channels()
            .unwrap()
            .into_iter()
            .find_map(|c| c.funding_txid)
    });
    wait("funding broadcast", || {
        fixture
            .mempool()
            .contains(&funding.to_string())
            .then_some(())
    });
    fixture.mine(6);
    wait("both usable channels", || {
        [&nodes.a, &nodes.b]
            .iter()
            .all(|node| {
                node.sync().unwrap();
                node.list_channels()
                    .unwrap()
                    .iter()
                    .any(|c| c.ready && c.is_usable)
            })
            .then_some(())
    });
    settled_balance(&nodes.a, &asset, 400); // Readiness can precede native funding-change refresh.
    payment(
        &nodes.a,
        &nodes.b,
        b_pub,
        Some(asset.clone()),
        Some(100),
        3_000_000,
    );
    payment(&nodes.a, &nodes.b, b_pub, None, None, 10_000_000);
    payment(
        &nodes.b,
        &nodes.a,
        a_pub,
        Some(asset.clone()),
        Some(50),
        3_000_000,
    );
    wait("RGB channel balances 300/300", || {
        [&nodes.a, &nodes.b]
            .iter()
            .all(|node| {
                node.list_channels().unwrap().iter().any(|c| {
                    c.asset_local_amount == Some(300) && c.asset_remote_amount == Some(300)
                })
            })
            .then_some(())
    });
    let channel_id = nodes.a.list_channels().unwrap()[0].channel_id;
    // Succeeded payment events can precede the final revoke. This fixture closes a fully
    // settled channel. This fixture does not qualify recovery with pending HTLCs.
    wait("both peers' exact LDK HTLC state is quiescent", || {
        [&nodes.a, &nodes.b]
            .iter()
            .all(|node| {
                !rgb_lightning_node::test_utils::channel_has_inflight_htlcs_for_tests(
                    node, channel_id,
                )
                .unwrap()
            })
            .then_some(())
    });
    let funding_tx = fixture.transaction(&funding.to_string());
    let funding_vouts: Vec<_> = funding_tx["vout"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|v| v["scriptPubKey"]["type"] == "witness_v0_scripthash")
        .map(|v| v["n"].as_u64().unwrap())
        .collect();
    assert_eq!(funding_vouts.len(), 1);
    nodes
        .a
        .closechannel(SdkCloseChannelRequest {
            channel_id,
            peer_pubkey: b_pub,
            force,
        })
        .unwrap();
    let closing = wait(
        "closing transaction spending exact channel outpoint",
        || {
            fixture.mempool().into_iter().find(|txid| {
                fixture.transaction(txid)["vin"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|i| i["txid"] == funding.to_string() && i["vout"] == funding_vouts[0])
            })
        },
    );
    let closing_confirmations = if force { 144 } else { 6 };
    // Observe the exact closing broadcast first, then retain the original maturity batch.
    // No sweep-confirming block is mined until both preparations are actually observed.
    fixture.mine(closing_confirmations);
    assert_eq!(
        fixture.transaction(&closing)["confirmations"],
        closing_confirmations
    );
    let prepared = wait("both protected sweep broadcasts", || {
        nodes.a.sync().unwrap();
        nodes.b.sync().unwrap();
        let a = preparations(directory.path(), &a_dir);
        let b = preparations(directory.path(), &b_dir);
        if a.len() != 1 || b.len() != 1 || a[0].phase != "handed_off" || b[0].phase != "handed_off"
        {
            return None;
        }
        let mempool: HashSet<_> = fixture.mempool().into_iter().collect();
        a.iter()
            .chain(&b)
            .all(|p| mempool.contains(&p.txid))
            .then_some((a, b))
    });
    let sweep_txids: HashSet<_> = prepared
        .0
        .iter()
        .chain(&prepared.1)
        .map(|p| p.txid.clone())
        .collect();
    assert_eq!(sweep_txids.len(), 2);
    for txid in &sweep_txids {
        let tx = fixture.transaction(txid);
        assert!(tx["confirmations"].as_u64().unwrap_or(0) == 0);
        assert!(tx["vin"]
            .as_array()
            .unwrap()
            .iter()
            .any(|input| input["txid"] == closing));
    }
    for _ in 0..3 {
        refresh(&nodes.a);
        refresh(&nodes.b);
    }
    assert_eq!(
        (balance(&nodes.a, &asset), balance(&nodes.b, &asset)),
        (400, 0)
    );
    println!("protected-close before mine: force={force} close={closing} closing_confirmations={closing_confirmations} preparations={prepared:?} balances=400/0");
    assert_eq!(
        fixture.mempool().into_iter().collect::<HashSet<_>>(),
        sweep_txids,
        "only observed owned sweeps may be confirmed"
    );
    // Exercise the shared close-fixture detector against genuine native preparations.
    // Its exact funding-outpoint binding must identify both peers' pending sweeps.
    fixture.assert_owner();
    let detected = Command::new("python3")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("test/channel_sweep_candidates.py"))
        .args([funding.to_string(), channel_id.to_string()])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .unwrap();
    assert!(
        detected.status.success(),
        "owned exact-channel sweep detector: {}",
        String::from_utf8_lossy(&detected.stderr)
    );
    let detected: HashSet<String> = serde_json::from_slice(&detected.stdout).unwrap();
    assert_eq!(detected, sweep_txids);
    fixture.mine(1);
    settled_balance(&nodes.a, &asset, 700);
    settled_balance(&nodes.b, &asset, 300);
    for (node, storage, preparation) in [
        (&nodes.a, &a_dir, &prepared.0[0]),
        (&nodes.b, &b_dir, &prepared.1[0]),
    ] {
        assert!(
            fixture.transaction(&preparation.txid)["confirmations"]
                .as_u64()
                .unwrap()
                >= 1
        );
        let rows = preparations(directory.path(), storage);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].batch_transfer_idx, preparation.batch_transfer_idx);
        assert_eq!(rows[0].phase, "consumed");
        let transfers = node
            .list_transfers(Some(asset.clone()), Some(preparation.txid.clone()))
            .unwrap();
        assert_eq!(
            transfers
                .iter()
                .filter(|t| t.kind == "ReceiveWitness" && t.status == "Settled")
                .count(),
            1
        );
        refresh(node);
    }
    assert_eq!(
        (balance(&nodes.a, &asset), balance(&nodes.b, &asset)),
        (700, 300)
    );
    println!("protected-close after observed sweep confirmation: balances=700/300; journals=consumed; idempotent refresh passed");
}
