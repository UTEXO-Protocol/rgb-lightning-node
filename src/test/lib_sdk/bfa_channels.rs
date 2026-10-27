//! Real BFA bridge -> Lightning -> on-chain lifecycle. See test/bfa-lightning.md.
use crate::helpers::*;
use bitcoin::hex::FromHex;
use rgb_lib::keys::{generate_keys, WitnessVersion};
use rgb_lib::wallet::{DatabaseType, OnlineOptions, SinglesigKeys, Wallet, WalletData};
use rgb_lib::{AssetSchema, BitcoinNetwork};
use rgb_lightning_node::BurnRequest;
use serde_json::{json, Value};
use serial_test::serial;
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::str::FromStr;
use std::time::{Duration, Instant};

const TIMEOUT: Duration = Duration::from_secs(120);
const PASSWORD: &str = "bfa-ln-test";
const INDEXER: &str = "tcp://127.0.0.1:50001";

fn proxy() -> String {
    std::env::var("RLN_BFA_PROXY").unwrap_or_else(|_| PROXY_ENDPOINT_LOCAL.into())
}

fn wait_for_received(node: &SdkNode, asset: &ContractId, amount: u64) {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        refresh_transfers(node);
        if node
            .asset_balance(*asset)
            .is_ok_and(|balance| balance.spendable == amount)
        {
            return;
        }
        assert!(Instant::now() < deadline, "BFA receipt did not settle");
        std::thread::sleep(Duration::from_secs(1));
    }
}

struct Node {
    inner: SdkNode,
    dir: PathBuf,
    port: u16,
    eth_rpc: String,
}

impl Node {
    fn new(dir: PathBuf, port: u16, eth_rpc: &str) -> Self {
        let inner = make_node(&dir, 0, port);
        inner.init(PASSWORD.into(), None).unwrap();
        let node = Self {
            inner,
            dir,
            port,
            eth_rpc: eth_rpc.into(),
        };
        node.unlock_bfa();
        fund_and_create_utxos(&node, "BFA node");
        node
    }

    fn unlock_bfa(&self) {
        let mut request = unlock_request(PASSWORD);
        if let SdkLdkChainSync::BlockSync {
            bitcoind_rpc_host, ..
        } = &mut request.ldk_chain_sync
        {
            *bitcoind_rpc_host = "127.0.0.1".into();
        }
        request.eth_rpc_url = Some(self.eth_rpc.clone());
        request.proxy_endpoint = Some(proxy());
        self.inner.unlock(request).unwrap();
        rgb_lightning_node::test_utils::enable_private_forwarding_for_tests(&self.inner);
    }

    fn restart(&mut self) {
        self.inner.shutdown();
        self.inner = make_node(&self.dir, 0, self.port);
        self.unlock_bfa();
    }
}

impl Deref for Node {
    type Target = SdkNode;
    fn deref(&self) -> &SdkNode {
        &self.inner
    }
}

impl Drop for Node {
    fn drop(&mut self) {
        self.inner.shutdown();
    }
}

fn eth_rpc(url: &str, method: &str, params: Value) -> Value {
    let url = reqwest::Url::parse(url).unwrap();
    assert!(matches!(url.host_str(), Some("localhost" | "127.0.0.1")));
    let rt = tokio::runtime::Runtime::new().unwrap();
    let result: Value = rt.block_on(async {
        reqwest::Client::new()
            .post(url)
            .timeout(Duration::from_secs(30))
            .json(&json!({"jsonrpc":"2.0", "id":1, "method":method, "params":params}))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap()
    });
    assert!(result["error"].is_null(), "{result}");
    result["result"].clone()
}

fn mint_bfa(dir: &Path, node: &Node, bridge: &str, container: &str) -> (Wallet, ContractId) {
    assert_eq!(eth_rpc(&node.eth_rpc, "eth_chainId", json!([])), "0x7a69");
    assert_ne!(
        eth_rpc(&node.eth_rpc, "eth_getCode", json!([bridge, "latest"])),
        "0x"
    );
    std::fs::create_dir_all(dir).unwrap();
    let keys = generate_keys(BitcoinNetwork::Regtest, WitnessVersion::Taproot);
    let mut issuer = Wallet::new(
        WalletData {
            data_dir: dir.display().to_string(),
            bitcoin_network: BitcoinNetwork::Regtest,
            database_type: DatabaseType::Sqlite,
            max_allocations_per_utxo: 5,
            supported_schemas: vec![AssetSchema::Bfa],
        },
        SinglesigKeys::from_keys(&keys, None),
    )
    .unwrap();
    let online = issuer
        .go_online(OnlineOptions {
            indexer_config: rgb_lib::wallet::IndexerConfig::Electrum {
                url: INDEXER.into(),
            },
            skip_consistency_check: false,
            vanilla_sync_lookback: 20,
            failed_witness_receive_grace_secs: 3600,
            proxy_auth_keys: std::collections::HashMap::new(),
            evm_rpc_urls: std::collections::HashMap::from([(31337, node.eth_rpc.clone())]),
        })
        .unwrap();
    let funded = Command::new("./regtest.sh")
        .args(["sendtoaddress", &issuer.get_address().unwrap(), "0.02"])
        .output()
        .unwrap();
    assert!(
        funded.status.success(),
        "{}",
        String::from_utf8_lossy(&funded.stderr)
    );
    mine(6);
    issuer
        .create_utxos(online, false, Some(8), Some(100_000), 2, false)
        .unwrap();
    mine(6);
    let asset = issuer
        .issue_asset_bfa(
            "BFALN".into(),
            "BFA Lightning".into(),
            6,
            1,
            31337,
            bridge.into(),
            None,
        )
        .unwrap();
    let asset_id = ContractId::from_str(&asset.asset_id).unwrap();
    let receive = crate::helpers::rgb_invoice(
        &node,
        SdkRgbInvoiceRequest {
            asset_id: None,
            assignment_kind: Some(AssignmentKind::Fungible),
            assignment_amount: Some(1000),
            duration_seconds: Some(3600),
            min_confirmations: 1,
            witness: false,
        },
    )
    .unwrap();
    let prepared = issuer
        .mint_begin(
            online,
            asset.asset_id,
            crate::helpers::native_recipient_from_invoice(&receive.invoice, 1000),
            2,
            1,
            proxy(),
        )
        .unwrap();
    let locked = Command::new("docker")
        .args([
            "exec",
            container,
            "cast",
            "send",
            bridge,
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
        locked.status.success(),
        "{}",
        String::from_utf8_lossy(&locked.stderr)
    );
    let receipt: Value = serde_json::from_slice(&locked.stdout).unwrap();
    assert_eq!(receipt["status"], "0x1");
    let signed = issuer.sign_psbt(prepared.psbt, None).unwrap();
    issuer.mint_end(online, signed).unwrap();
    mine(6);
    wait_for_received(node, &asset_id, 1000);
    (issuer, asset_id)
}

fn open_bfa(
    sender: &Node,
    receiver: &Node,
    asset: &ContractId,
    amount: u64,
    push: u64,
    via_rest: bool,
) -> lightning::ln::types::ChannelId {
    let peer = receiver.node_info().unwrap().pubkey;
    let temporary_channel_id = if via_rest {
        let request = json!({
            "peer_pubkey_and_opt_addr": format!("{peer}@127.0.0.1:{}", receiver.port),
            "capacity_sat": 100_000, "push_msat": 20_000_000,
            "public": false, "with_anchors": true,
            "fee_base_msat": 0, "fee_proportional_millionths": 0,
            "asset_id": asset.to_string(), "asset_amount": amount, "push_asset_amount": push,
        });
        let id = rgb_lightning_node::test_utils::open_channel_via_rest_for_tests(
            sender,
            &request.to_string(),
        )
        .unwrap();
        lightning::ln::types::ChannelId::from_bytes(<[u8; 32]>::from_hex(&id).unwrap())
    } else {
        sender
            .openchannel(SdkOpenChannelRequest {
                peer_pubkey_and_opt_addr: format!("{peer}@127.0.0.1:{}", receiver.port),
                capacity_sat: 100_000,
                push_msat: 20_000_000,
                // Private channels keep the two-hop test independent of gossip propagation
                // after restart: invoices always carry the final hop's route hint.
                public: false,
                with_anchors: true,
                fee_base_msat: Some(0),
                fee_proportional_millionths: Some(0),
                temporary_channel_id: None,
                asset_id: Some(*asset),
                asset_amount: Some(amount),
                push_asset_amount: Some(push),
                virtual_open_mode: None,
            })
            .unwrap()
            .temporary_channel_id
    };
    let channel_id = wait_for_channel_open(
        sender,
        |channel| channel.peer_pubkey == peer && channel.asset_id.as_ref() == Some(asset),
        TIMEOUT,
    );
    wait_for_channel_ready(receiver, channel_id, TIMEOUT);
    assert_eq!(
        sender.get_channel_id(temporary_channel_id).unwrap(),
        channel_id
    );
    channel_id
}

fn pay(
    sender: &Node,
    receiver: &Node,
    asset: &ContractId,
    amount: u64,
    sender_balance: u64,
    receiver_balance: u64,
) {
    let invoice = receiver
        .ln_invoice(LnInvoiceRequest {
            amt_msat: Some(PAYMENT_MSAT),
            expiry_sec: 900,
            asset_id: Some(*asset),
            asset_amount: Some(amount),
            payment_hash: None,
            description: None,
            description_hash: None,
            min_final_cltv_expiry_delta: None,
        })
        .unwrap()
        .invoice;
    send_payment_with_ln_balance(
        sender,
        receiver,
        invoice,
        asset,
        amount,
        sender_balance,
        receiver_balance,
    );
}

#[test]
#[serial]
#[ignore = "requires local regtest and an Anvil BFA bridge; see test/bfa-lightning.md"]
fn bfa_channels_multihop_restart_close_and_burn() {
    ensure_regtest_available();
    let eth = std::env::var("RLN_BFA_ETH_RPC").expect("RLN_BFA_ETH_RPC required");
    let bridge = std::env::var("RLN_BFA_BRIDGE").expect("RLN_BFA_BRIDGE required");
    let container =
        std::env::var("RLN_BFA_ANVIL_CONTAINER").expect("RLN_BFA_ANVIL_CONTAINER required");
    let root = tempfile::Builder::new()
        .prefix("rln-bfa-ln-")
        .tempdir()
        .unwrap()
        .keep();
    eprintln!("BFA LN evidence: {}", root.display());
    let mut a = Node::new(root.join("a"), NODE_A_PEER_PORT + 200, &eth);
    let mut b = Node::new(root.join("b"), NODE_B_PEER_PORT + 200, &eth);
    let mut c = Node::new(root.join("c"), NODE_C_PEER_PORT + 200, &eth);
    let (_issuer, asset) = mint_bfa(&root.join("issuer"), &a, &bridge, &container);
    eprintln!("BFA minted: {asset}");

    let receive = crate::helpers::rgb_invoice(
        &b,
        SdkRgbInvoiceRequest {
            asset_id: None,
            assignment_kind: Some(AssignmentKind::Fungible),
            assignment_amount: Some(400),
            duration_seconds: Some(3600),
            min_confirmations: 1,
            witness: false,
        },
    )
    .unwrap();
    crate::helpers::send_rgb(
        &a,
        SendRgbRequest {
            donation: true,
            fee_rate: 2,
            min_confirmations: 1,
            recipient_groups: vec![AssetRecipients {
                asset_id: asset,
                recipients: vec![RgbRecipient {
                    recipient_id: receive.recipient_id,
                    witness_data: None,
                    assignment_kind: AssignmentKind::Fungible,
                    assignment_amount: Some(400),
                    transport_endpoints: vec![TransportEndpoint(proxy())],
                }],
            }],
        },
    )
    .unwrap();
    mine(6);
    wait_for_received(&b, &asset, 400);
    wait_for_balance(&a, &asset, 600, TIMEOUT);

    let ab = open_bfa(&a, &b, &asset, 500, 50, false);
    eprintln!("A-B SDK channel ready: {ab}");
    // C learns the BFA contract via funding, without a preceding on-chain receipt.
    let bc = open_bfa(&b, &c, &asset, 300, 20, true);
    eprintln!("B-C REST channel ready: {bc}");
    let metadata = c.asset_metadata(asset).unwrap();
    assert_eq!(metadata.asset_schema, "Bfa");
    assert_eq!(metadata.known_circulating_supply, 1000);
    wait_for_ln_balance(&a, &asset, 450, TIMEOUT);
    wait_for_ln_balance(&b, &asset, 330, TIMEOUT);
    wait_for_ln_balance(&c, &asset, 20, TIMEOUT);
    pay(&a, &c, &asset, 100, 450, 20);
    eprintln!("A -> B -> C payment complete");
    wait_for_ln_balance(&b, &asset, 330, TIMEOUT);

    a.restart();
    b.restart();
    c.restart();
    wait_for_usable_channels(&a, 1, TIMEOUT);
    wait_for_usable_channels(&b, 2, TIMEOUT);
    wait_for_usable_channels(&c, 1, TIMEOUT);
    pay(&c, &a, &asset, 30, 120, 350);
    eprintln!("Restart and C -> B -> A payment complete");

    let closed_ab = close_channel(&a, ab, b.node_info().unwrap().pubkey);
    wait_for_closed_channel_balance(&a, &asset, 480, TIMEOUT, &closed_ab);
    wait_for_closed_channel_balance(&b, &asset, 220, TIMEOUT, &closed_ab);
    eprintln!("A-B cooperative close recovered assets");
    let closed_bc = close_channel_with_force(&b, bc, c.node_info().unwrap().pubkey, true);
    // Confirm only observed descendants after the commitment reaches CSV maturity.
    wait_for_closed_channel_balance(&b, &asset, 430, TIMEOUT, &closed_bc);
    wait_for_closed_channel_balance(&c, &asset, 90, TIMEOUT, &closed_bc);
    eprintln!("B-C force close recovered assets");

    // The recovered assets must remain valid bridge assets, including a subsequent burn proof.
    let recipient = vec![0x42; 32];
    let burned = c
        .burn(BurnRequest {
            asset_id: asset,
            amount: 10,
            burn_recipient: Some(recipient.clone()),
            fee_rate: 2,
            min_confirmations: 1,
        })
        .unwrap();
    mine(6);
    wait_for_balance(&c, &asset, 80, TIMEOUT);
    let proof = c.get_consignment_path(asset, burned.txid).unwrap();
    crate::helpers::assert_burn_proof_destination(proof, &asset, &recipient);
}
