use super::*;

use std::str::FromStr;

use bitcoin::secp256k1::PublicKey;
use bitcoin::Address;

#[cfg(feature = "interop-ldk")]
use std::io::{BufRead, BufReader, Write};
#[cfg(feature = "interop-ldk")]
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

const CHANNEL_CAPACITY_SAT: u64 = 600_000;
const FORWARD_PAYMENT_MSAT: u64 = 20_000_000;
const RETURN_PAYMENT_MSAT: u64 = 10_000_000;
// the two payments settle on-chain balances only at close, so channel balances are compared with a
// tolerance covering the commitment transaction fees
const FEE_TOLERANCE_SAT: u64 = 2_000;

/// Selects which stock Lightning implementation the interoperability tests drive, via the
/// `INTEROP_BACKEND` env var (`ldk` or `lnd`; defaults to `ldk`).
fn interop_backend() -> String {
    std::env::var("INTEROP_BACKEND").unwrap_or_else(|_| "ldk".to_string())
}

/// A stock Lightning node used as an out-of-band counterparty in the interoperability scenarios.
///
/// Each backend (the ldk-node fixture, a stock `lnd`) is driven through its own native interface:
/// the ldk-node fixture over its stdin/stdout protocol, lnd through its `lncli` client.
trait ExternalLightningNode {
    fn pubkey(&mut self) -> PublicKey;
    fn onchain_address(&mut self) -> Address;
    fn open_channel(&mut self, peer_pubkey: &PublicKey, peer_addr: &SocketAddr, capacity_sat: u64);
    fn create_invoice(&mut self, amount_msat: u64) -> String;
    fn pay_invoice(&mut self, invoice: &str) -> Result<(), String>;
    /// (outbound_msat, inbound_msat) summed over the node's open channels
    fn channel_balances(&mut self) -> (u64, u64);
    /// (total channel count, ready channel count)
    fn channels(&mut self) -> (usize, usize);
    fn onchain_spendable(&mut self) -> u64;
    fn stop(&mut self);
}

#[cfg(feature = "interop-ldk")]
#[derive(Debug, PartialEq)]
struct ChannelBalances {
    outbound_msat: u64,
    inbound_msat: u64,
}

/// A stock ldk-node fixture, driven over its stdin/stdout with one line per command.
///
/// It is a separate Cargo crate so RLN's Cargo patches cannot replace the stock LDK dependencies
/// it is built against. It only reports state, so that mining stays under the test's control.
#[cfg(feature = "interop-ldk")]
struct StockLdkNode {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    node_id: String,
    address: Address,
}

#[cfg(feature = "interop-ldk")]
impl StockLdkNode {
    fn start(test_dir: &str, listening_port: u16) -> Self {
        if Path::new(test_dir).exists() {
            std::fs::remove_dir_all(test_dir).unwrap();
        }
        std::fs::create_dir_all(test_dir).unwrap();

        let manifest = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src/test/interoperability/ldk-node/Cargo.toml");
        let target_dir =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("target/interoperability/ldk-node");
        // the fixture is built and run outside of the instrumentation of the test run, so that its
        // dependencies don't end up in the coverage report
        let status = Command::new(env!("CARGO"))
            .args(["build", "--manifest-path"])
            .arg(&manifest)
            .env("CARGO_TARGET_DIR", &target_dir)
            .env_remove("RUSTFLAGS")
            .env_remove("CARGO_ENCODED_RUSTFLAGS")
            .env_remove("LLVM_PROFILE_FILE")
            .status()
            .expect("failed to build stock ldk-node fixture");
        assert!(status.success(), "failed to build stock ldk-node fixture");

        let stderr_log = std::fs::File::create(format!("{test_dir}/stock-ldk-node.log"))
            .expect("failed to create stock-ldk-node log file");
        let mut child = Command::new(target_dir.join("debug/stock-ldk-node"))
            .args([test_dir, &format!("127.0.0.1:{listening_port}")])
            .env_remove("LLVM_PROFILE_FILE")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(stderr_log)
            .spawn()
            .expect("failed to start stock ldk-node fixture");
        let stdin = child.stdin.take().unwrap();
        let mut stdout = BufReader::new(child.stdout.take().unwrap());
        let mut line = String::new();
        stdout.read_line(&mut line).unwrap();
        let mut parts = line.split_whitespace();
        assert_eq!(parts.next(), Some("NODE"));
        let node_id = parts.next().unwrap().to_string();
        let address = parts.next().unwrap().to_string();
        Self {
            child,
            stdin,
            stdout,
            node_id,
            address: Address::from_str(&address)
                .expect("stock ldk-node fixture returned an invalid onchain address")
                .assume_checked(),
        }
    }

    fn command(&mut self, command: &str) -> String {
        writeln!(self.stdin, "{command}").unwrap();
        self.stdin.flush().unwrap();
        let mut response = String::new();
        self.stdout.read_line(&mut response).unwrap();
        assert!(
            !response.is_empty(),
            "stock ldk-node exited during {command}"
        );
        response.trim().to_string()
    }

    fn open_channel(&mut self, peer_pubkey: &str, peer_port: u16) {
        let command = format!("open {peer_pubkey} 127.0.0.1:{peer_port}");
        assert_eq!(self.command(&command), "OPENING");
    }

    /// Number of channels the stock node has, and how many of them are ready
    fn channels(&mut self) -> (usize, usize) {
        let response = self.command("channels");
        let mut parts = response.split_whitespace();
        assert_eq!(parts.next(), Some("CHANNELS"));
        (
            parts.next().unwrap().parse().unwrap(),
            parts.next().unwrap().parse().unwrap(),
        )
    }

    fn balances(&mut self) -> ChannelBalances {
        let response = self.command("balances");
        let mut parts = response.split_whitespace();
        assert_eq!(parts.next(), Some("BALANCES"));
        ChannelBalances {
            outbound_msat: parts.next().unwrap().parse().unwrap(),
            inbound_msat: parts.next().unwrap().parse().unwrap(),
        }
    }

    fn invoice(&mut self, amount_msat: u64) -> String {
        self.command(&format!("invoice {amount_msat}"))
            .strip_prefix("INVOICE ")
            .unwrap()
            .to_string()
    }

    fn pay(&mut self, invoice: &str) {
        assert_eq!(self.command(&format!("pay {invoice}")), "PAID");
    }

    fn onchain_spendable(&mut self) -> u64 {
        let response = self.command("onchain");
        let mut parts = response.split_whitespace();
        assert_eq!(parts.next(), Some("ONCHAIN"));
        parts.next().unwrap().parse().unwrap()
    }
}

#[cfg(feature = "interop-ldk")]
impl ExternalLightningNode for StockLdkNode {
    fn pubkey(&mut self) -> PublicKey {
        PublicKey::from_str(&self.node_id).expect("invalid stock ldk-node node id")
    }

    fn onchain_address(&mut self) -> Address {
        self.address.clone()
    }

    fn open_channel(&mut self, peer_pubkey: &PublicKey, peer_addr: &SocketAddr, _capacity_sat: u64) {
        // the fixture opens channels at its own fixed capacity
        Self::open_channel(self, &peer_pubkey.to_string(), peer_addr.port())
    }

    fn create_invoice(&mut self, amount_msat: u64) -> String {
        self.invoice(amount_msat)
    }

    fn pay_invoice(&mut self, invoice: &str) -> Result<(), String> {
        Self::pay(self, invoice);
        Ok(())
    }

    fn channel_balances(&mut self) -> (u64, u64) {
        let ChannelBalances {
            outbound_msat,
            inbound_msat,
        } = self.balances();
        (outbound_msat, inbound_msat)
    }

    fn channels(&mut self) -> (usize, usize) {
        Self::channels(self)
    }

    fn onchain_spendable(&mut self) -> u64 {
        Self::onchain_spendable(self)
    }

    fn stop(&mut self) {
        // shutdown is handled by Drop
    }
}

#[cfg(feature = "interop-ldk")]
impl Drop for StockLdkNode {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = writeln!(self.stdin, "stop");
            let _ = self.stdin.flush();
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Wait until the stock node has completed its initial chain sync by polling
/// until `channels()` returns successfully and the node is responsive.
/// Also mines blocks to advance the chain, giving the node something to sync to.
async fn wait_for_stock_synced(stock: &mut dyn ExternalLightningNode) {
    let t_0 = OffsetDateTime::now_utc();
    loop {
        mine(false);
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        // channels() will panic if the node process died, but returns (0, 0) if alive and synced
        let _ = stock.channels();
        // give it a few more seconds after first response to finish internal setup
        if (OffsetDateTime::now_utc() - t_0).as_seconds_f32() > 10.0 {
            break;
        }
        if (OffsetDateTime::now_utc() - t_0).as_seconds_f32() > 120.0 {
            panic!("stock node is taking too long to sync");
        }
    }
}

async fn wait_for_stock_funds(stock: &mut dyn ExternalLightningNode) {
    let t_0 = OffsetDateTime::now_utc();
    loop {
        if stock.onchain_spendable() > 0 {
            break;
        }
        if (OffsetDateTime::now_utc() - t_0).as_seconds_f32() > 60.0 {
            panic!("stock wallet is taking too long to be funded");
        }
        mine(false);
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
}

async fn wait_for_stock_channel_ready(stock: &mut dyn ExternalLightningNode) {
    let t_0 = OffsetDateTime::now_utc();
    loop {
        if stock.channels().1 > 0 {
            break;
        }
        if (OffsetDateTime::now_utc() - t_0).as_seconds_f32() > 180.0 {
            panic!("stock channel is taking too long to be ready");
        }
        mine(false);
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
}

async fn wait_for_stock_channel_closed(stock: &mut dyn ExternalLightningNode, initial_spendable: u64) {
    let t_0 = OffsetDateTime::now_utc();
    loop {
        if stock.channels().0 == 0 && stock.onchain_spendable() > initial_spendable {
            break;
        }
        if (OffsetDateTime::now_utc() - t_0).as_seconds_f32() > 180.0 {
            panic!("stock node is taking too long to settle the cooperative close");
        }
        mine(false);
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
}

/// Pay in both directions over a ready channel between RLN and the stock node,
/// then close it cooperatively.
async fn exercise_channel_stock(
    rln_addr: SocketAddr,
    stock: &mut dyn ExternalLightningNode,
    rln_funded: bool,
) {
    wait_for_usable_channels(rln_addr, 1).await;
    let stock_pubkey = stock.pubkey().to_string();
    let rln_channel = list_channels(rln_addr)
        .await
        .into_iter()
        .find(|channel| channel.peer_pubkey == stock_pubkey && channel.ready)
        .unwrap();
    let (initial_outbound_msat, initial_inbound_msat) = stock.channel_balances();
    let initial_stock_spendable = stock.onchain_spendable();

    if rln_funded {
        let invoice = stock.create_invoice(FORWARD_PAYMENT_MSAT);
        send_payment(rln_addr, invoice).await;
        let invoice = ln_invoice(rln_addr, Some(RETURN_PAYMENT_MSAT), None, None, 900)
            .await
            .invoice;
        stock.pay_invoice(&invoice).unwrap();
    } else {
        let invoice = ln_invoice(rln_addr, Some(FORWARD_PAYMENT_MSAT), None, None, 900)
            .await
            .invoice;
        stock.pay_invoice(&invoice).unwrap();
        let invoice = stock.create_invoice(RETURN_PAYMENT_MSAT);
        send_payment(rln_addr, invoice).await;
    }

    let (final_outbound_msat, final_inbound_msat) = stock.channel_balances();
    let final_rln = list_channels(rln_addr)
        .await
        .into_iter()
        .find(|channel| channel.channel_id == rln_channel.channel_id)
        .unwrap();
    let net_sat = (FORWARD_PAYMENT_MSAT - RETURN_PAYMENT_MSAT) / 1000;
    if rln_funded {
        assert!(final_outbound_msat > initial_outbound_msat);
        assert!(final_inbound_msat < initial_inbound_msat);
        let spent_sat = rln_channel.local_balance_sat - final_rln.local_balance_sat;
        assert!((net_sat..=net_sat + FEE_TOLERANCE_SAT).contains(&spent_sat));
    } else {
        assert!(final_outbound_msat < initial_outbound_msat);
        assert!(final_inbound_msat > initial_inbound_msat);
        let received_sat = final_rln.local_balance_sat - rln_channel.local_balance_sat;
        assert!((net_sat - FEE_TOLERANCE_SAT..=net_sat).contains(&received_sat));
    }

    close_channel(
        rln_addr,
        &rln_channel.channel_id,
        &stock_pubkey,
        false,
    )
    .await;
    wait_for_stock_channel_closed(stock, initial_stock_spendable).await;
    assert!(list_channels(rln_addr).await.is_empty());
}

/// The stock node opens a channel to RLN, both pay each other over it, then close cooperatively.
async fn scenario_stock_opens_pays_both_ways_and_closes(
    rln_addr: SocketAddr,
    stock: &mut dyn ExternalLightningNode,
) {
    wait_for_stock_synced(stock).await;
    let rln_pubkey = node_info(rln_addr).await.pubkey;
    let rln_pubkey = PublicKey::from_str(&rln_pubkey).expect("invalid RLN node pubkey");
    stock.open_channel(
        &rln_pubkey,
        &SocketAddr::from(([127, 0, 0, 1], NODE1_PEER_PORT)),
        CHANNEL_CAPACITY_SAT,
    );
    wait_for_stock_channel_ready(stock).await;
    exercise_channel_stock(rln_addr, stock, false).await;
}

/// RLN opens a channel to the stock node, both pay each other over it, then close cooperatively.
async fn scenario_rln_opens_pays_both_ways_and_closes(
    rln_addr: SocketAddr,
    stock: &mut dyn ExternalLightningNode,
) {
    // MUST sync the stock node so it has reserves for anchor channels!
    wait_for_stock_synced(stock).await;
    open_channel_funded_raw(
        rln_addr,
        &stock.pubkey().to_string(),
        Some(NODE2_PEER_PORT),
        Some(CHANNEL_CAPACITY_SAT),
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        true,
        false,
    )
    .await
    .unwrap();
    wait_for_stock_channel_ready(stock).await;
    exercise_channel_stock(rln_addr, stock, true).await;
}

/// A RGB channel opened to a stock node is rejected by the stock node and never becomes ready.
async fn scenario_rgb_channel_to_stock_fails(
    rln_addr: SocketAddr,
    stock: &mut dyn ExternalLightningNode,
) {
    wait_for_stock_synced(stock).await;
    let asset_id = issue_asset_nia(rln_addr).await.asset_id;
    open_channel_request_raw(
        rln_addr,
        &stock.pubkey().to_string(),
        Some(NODE2_PEER_PORT),
        Some(100_000),
        None,
        Some(100),
        Some(&asset_id),
        None,
        None,
        None,
        None,
        true,
        false,
    )
    .await
    .expect("RGB channel attempt should pass RLN's local validation");

    // the stock node rejects the colored funding, so while it may hold the channel as pending for a
    // while, the channel never becomes ready on either side
    let t_0 = OffsetDateTime::now_utc();
    while (OffsetDateTime::now_utc() - t_0).as_seconds_f32() < 10.0 {
        assert!(list_channels(rln_addr).await.iter().all(|c| !c.ready));
        assert_eq!(stock.channels().1, 0);
        mine(false);
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
}

#[cfg(feature = "interop-ldk")]
mod ldk;
#[cfg(feature = "interop-lnd")]
mod lnd;
#[cfg(feature = "interop-lnd")]
mod lnd_node;
#[cfg(feature = "interop-lnd")]
pub(crate) use lnd_node::StockLndNode;