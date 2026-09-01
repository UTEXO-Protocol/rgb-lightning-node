use super::super::*;
use super::*;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::str::FromStr;

use bitcoin::secp256k1::PublicKey;
use bitcoin::Address;

const WALLET_PASSWORD: &str = "rln-interop-wallet-password-4f82b1c9";
const NEW_WALLET_PASSWORD: &str = "rln-interop-new-wallet-password-3f9c57d1";

/// Resolve a lnd binary from an explicit env var, or from PATH.
fn find_bin(env_name: &str, bin_name: &str) -> PathBuf {
    let probe = |path: &Path| {
        Command::new(path)
            .arg("--version")
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false)
    };
    if let Some(env_path) = std::env::var_os(env_name) {
        let candidate = PathBuf::from(env_path);
        assert!(
            probe(&candidate),
            "`{env_name}` set to {candidate:?} but `{bin_name} --version` did not succeed"
        );
        return candidate;
    }
    let bin_name = if cfg!(windows) {
        format!("{bin_name}.exe")
    } else {
        bin_name.to_string()
    };
    for dir in std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()) {
        let candidate = dir.join(&bin_name);
        if probe(&candidate) {
            return candidate;
        }
    }
    panic!(
        "could not find `{bin_name}` on PATH; install lnd or set the `{env_name}` env var"
    );
}

/// Parse a JSON uint64 field that lncli serializes as a string.
fn json_sats(value: &serde_json::Value) -> u64 {
    value.as_str().and_then(|s| s.parse().ok()).unwrap_or(0)
}

/// A stock `lnd` node, started with its own ephemeral data directory and driven through
/// `lncli`. It only reports state, so that mining stays under the test's control.
pub(crate) struct StockLndNode {
    test_dir: String,
    lnd_bin: PathBuf,
    lncli_bin: PathBuf,
    lnd_dir: String,
    rpc_server: String,
    rpc_port: u16,
    rest_port: u16,
    listening_port: u16,
    child: Child,
}

/// Start a `lnd` daemon over the given data directory, logging both its stdout and
/// stderr to `<test_dir>/stock-lnd.log` (lnd 0.21 writes its journal to stdout).
fn spawn_daemon(
    lnd_bin: &Path,
    test_dir: &str,
    listening_port: u16,
    rpc_port: u16,
    rest_port: u16,
) -> Child {
    let lnd_dir = format!("{test_dir}/lnd");
    let log_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(format!("{test_dir}/stock-lnd.log"))
        .expect("failed to open stock-lnd log file");
    let stdout_log = log_file
        .try_clone()
        .expect("failed to clone stock-lnd log file");
    Command::new(lnd_bin)
        .arg(format!("--lnddir={lnd_dir}"))
        .arg(format!("--listen=127.0.0.1:{listening_port}"))
        .arg(format!("--rpclisten=127.0.0.1:{rpc_port}"))
        .arg(format!("--restlisten=127.0.0.1:{rest_port}"))
        .arg("--bitcoin.active")
        .arg("--bitcoin.regtest")
        .arg("--bitcoin.node=bitcoind")
        .arg("--bitcoind.rpchost=127.0.0.1:18443")
        .arg("--bitcoind.rpcuser=user")
        .arg("--bitcoind.rpcpass=password")
        // the test bitcoind publishes no ZMQ streams, so RPC polling is mandatory
        .arg("--bitcoind.rpcpolling")
        .arg("--bitcoind.blockpollinginterval=1s")
        .arg("--bitcoind.txpollinginterval=1s")
        .arg("--nobootstrap")
        .arg("--alias=StockLndNode")
        .arg("--debuglevel=info")
        .env_remove("RUSTFLAGS")
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .env_remove("LLVM_PROFILE_FILE")
        .stdout(Stdio::from(stdout_log))
        .stderr(Stdio::from(log_file))
        .spawn()
        .expect("failed to start lnd")
}

impl StockLndNode {
    pub(crate) fn start(test_dir: &str, listening_port: u16) -> Self {
        if Path::new(test_dir).exists() {
            std::fs::remove_dir_all(test_dir).unwrap();
        }
        std::fs::create_dir_all(test_dir).unwrap();

        let lnd_bin = find_bin("LND_BIN_PATH", "lnd");
        let lncli_bin = find_bin("LNCLI_BIN_PATH", "lncli");
        let rpc_port = next_peer_port();
        let rest_port = next_peer_port();
        let lnd_dir = format!("{test_dir}/lnd");

        let child = spawn_daemon(&lnd_bin, test_dir, listening_port, rpc_port, rest_port);
        let mut node = Self {
            test_dir: test_dir.to_string(),
            lnd_bin,
            lncli_bin,
            lnd_dir,
            rpc_server: format!("127.0.0.1:{rpc_port}"),
            rpc_port,
            rest_port,
            listening_port,
            child,
        };
        node.initialize_wallet();
        node.wait_until_ready();
        node
    }

    /// Run `lncli`, returning (success, parsed JSON response, stderr).
    fn lncli(&self, args: &[&str]) -> (bool, serde_json::Value, String) {
        let output = Command::new(&self.lncli_bin)
            .arg("--lnddir")
            .arg(&self.lnd_dir)
            .arg("--network=regtest")
            .arg("--rpcserver")
            .arg(&self.rpc_server)
            .args(args)
            .output()
            .expect("failed to run lncli");
        if !output.status.success() {
            return (
                false,
                serde_json::Value::Null,
                String::from_utf8_lossy(&output.stderr).to_string(),
            );
        }
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        // `lncli payinvoice` streams one JSON object per state transition (one per line), so
        // the whole stdout is not a single JSON value. Take the last parseable JSON object to
        // reflect the final state of the command rather than a mid-flight one.
        for line in stdout.lines().rev() {
            if line.trim().is_empty() {
                continue;
            }
            if let Ok(value) = serde_json::from_str(line) {
                return (true, value, String::new());
            }
        }
        match serde_json::from_str(&stdout) {
            Ok(value) => (true, value, String::new()),
            Err(_) => (true, serde_json::Value::Null, stdout),
        }
    }

    /// Wait until the daemon's RPC port answers, panicking if it never comes up.
    fn wait_for_daemon_up(&mut self) {
        let t_0 = OffsetDateTime::now_utc();
        loop {
            let (ok, _value, stderr) = self.lncli(&["getinfo"]);
            if ok {
                return;
            }
            let still_starting = [
                "connection refused",
                "Connection refused",
                "connection reset",
                "i/o timeout",
                "deadline exceeded",
                "failed to connect",
                "error trying to connect",
                "no such file or directory",
                "cannot read macaroon",
                "unable to read macaroon",
                "could not read tls",
                "tls cert",
            ]
            .iter()
            .any(|needle| stderr.contains(needle));
            if !still_starting {
                return; // rpc answered: the daemon is up, just not ready yet
            }
            if (OffsetDateTime::now_utc() - t_0).as_seconds_f32() > 60.0 {
                panic!("lnd did not become reachable: {stderr}");
            }
            std::thread::sleep(std::time::Duration::from_secs(1));
        }
    }

    /// Create the wallet from the fixed seed, then exercise the `unlock` and
    /// `changepassword` flows described by lnd at startup.
    fn initialize_wallet(&mut self) {
        // On a fresh data dir there is no `admin.macaroon` yet, so `lncli` probing
        // commands fail locally; `lncli create` itself (WalletUnlocker RPC) needs no
        // macaroon, so `create_wallet` retries until the daemon accepts it.
        self.create_wallet();

        // Creation unlocks the wallet; restart so the `unlock` flow is exercised.
        self.restart();
        self.wait_for_daemon_up();
        self.unlock_wallet();

        // `changepassword` needs a locked wallet as well, hence the second restart.
        self.restart();
        self.wait_for_daemon_up();
        self.change_password();
    }

    /// Stop the current daemon and spawn a fresh one over the same data directory.
    fn restart(&mut self) {
        let _ = self.lncli(&["stop"]);
        let t_0 = OffsetDateTime::now_utc();
        loop {
            if self.child.try_wait().ok().flatten().is_some() {
                break;
            }
            if (OffsetDateTime::now_utc() - t_0).as_seconds_f32() > 15.0 {
                let _ = self.child.kill();
                let _ = self.child.wait();
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
        self.child = spawn_daemon(
            &self.lnd_bin,
            &self.test_dir,
            self.listening_port,
            self.rpc_port,
            self.rest_port,
        );
    }

    /// Run `lncli <args>` piping `input` to its stdin (for `unlock --stdin`).
    fn lncli_piped(&self, args: &[&str], input: &str) -> (bool, String) {
        let mut command = Command::new(&self.lncli_bin)
            .arg("--lnddir")
            .arg(&self.lnd_dir)
            .arg("--network=regtest")
            .arg("--rpcserver")
            .arg(&self.rpc_server)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("failed to spawn `lncli`");
        {
            let mut stdin = command.stdin.take().unwrap();
            write!(stdin, "{input}").unwrap();
        }
        let output = command.wait_with_output().expect("`lncli` failed to run");
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        (output.status.success(), text)
    }

    /// Run `lncli <args>` under a pseudo-terminal via `script`, since
    /// `lncli create`/`changepassword` read passwords with `term.ReadPassword`,
    /// which requires a TTY.
    fn lncli_pty(&self, args: &[&str], input: &str) -> (bool, String) {
        let invocation = format!(
            "{} --lnddir {} --network=regtest --rpcserver {} {}",
            self.lncli_bin.display(),
            self.lnd_dir,
            self.rpc_server,
            args.join(" ")
        );
        let mut command = Command::new("script")
            .arg("-qe")
            .arg("-c")
            .arg(invocation)
            .arg("/dev/null")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("failed to spawn `script`");
        {
            let mut stdin = command.stdin.take().unwrap();
            write!(stdin, "{input}").unwrap();
        }
        let output = command.wait_with_output().expect("`script` failed to run");
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        (output.status.success(), text)
    }

    fn create_wallet(&mut self) {
        // Answers: password, confirm, "n" (let lnd generate a fresh aezeed cipher
        // seed - hand-picked BIP39 phrases are usually not valid aezeed
        // seeds), then one empty line for the optional cipher seed passphrase.
        let input = format!("{WALLET_PASSWORD}\n{WALLET_PASSWORD}\nn\n\n");
        let t_0 = OffsetDateTime::now_utc();
        let mut last_error = String::new();
        loop {
            let (ok, text) = self.lncli_pty(&["create"], &input);
            if ok {
                return;
            }
            // Retry while the daemon is still coming up (or hasn't finished
            // generating its TLS cert); any other failure is a genuine error.
            let retryable = [
                "connection refused",
                "connection reset",
                "i/o timeout",
                "deadline exceeded",
                "no such file or directory",
                "unable to read macaroon",
                "could not read tls",
                "EOF",
            ]
            .iter()
            .any(|needle| text.contains(needle));
            last_error = text;
            if !retryable {
                panic!("`lncli create` failed: {last_error}");
            }
            if (OffsetDateTime::now_utc() - t_0).as_seconds_f32() > 60.0 {
                panic!("lnd did not become ready for wallet creation: {last_error}");
            }
            std::thread::sleep(std::time::Duration::from_secs(1));
        }
    }

    fn unlock_wallet(&mut self) {
        let input = format!("{WALLET_PASSWORD}\n");
        let (ok, text) = self.lncli_piped(&["unlock", "--stdin"], &input);
        assert!(ok, "`lncli unlock` failed: {text}");
    }

    fn change_password(&mut self) {
        let input =
            format!("{WALLET_PASSWORD}\n{NEW_WALLET_PASSWORD}\n{NEW_WALLET_PASSWORD}\n");
        let (ok, text) = self.lncli_pty(&["changepassword"], &input);
        assert!(ok, "`lncli changepassword` failed: {text}");
    }

    /// Wait until `getinfo` returns the node's identity pubkey.
    fn wait_until_ready(&mut self) {
        let t_0 = OffsetDateTime::now_utc();
        loop {
            let (ok, value, _stderr) = self.lncli(&["getinfo"]);
            if ok && value["identity_pubkey"].is_string() {
                return;
            }
            if (OffsetDateTime::now_utc() - t_0).as_seconds_f32() > 120.0 {
                panic!("lnd took too long to create/unlock its wallet");
            }
            std::thread::sleep(std::time::Duration::from_secs(1));
        }
    }
}

impl ExternalLightningNode for StockLndNode {
    fn pubkey(&mut self) -> PublicKey {
        let (ok, value, stderr) = self.lncli(&["getinfo"]);
        assert!(ok, "lncli getinfo failed: {stderr}");
        PublicKey::from_str(
            value["identity_pubkey"]
                .as_str()
                .expect("getinfo missing identity_pubkey"),
        )
        .expect("invalid lnd node identity_pubkey")
    }

    fn onchain_address(&mut self) -> Address {
        let (ok, value, stderr) = self.lncli(&["newaddress", "p2tr"]);
        assert!(ok, "lncli newaddress failed: {stderr}");
        Address::from_str(value["address"].as_str().expect("newaddress missing address"))
            .expect("invalid lnd onchain address")
            .assume_checked()
    }

    fn open_channel(&mut self, peer_pubkey: &PublicKey, peer_addr: &SocketAddr, capacity_sat: u64) {
        let peer = format!("{peer_pubkey}@{peer_addr}");
        let (ok, _value, stderr) = self.lncli(&["connect", peer.as_str()]);
        assert!(
            ok || stderr.contains("already connected") || stderr.contains("already have"),
            "lncli connect failed: {stderr}"
        );
        let node_key = peer_pubkey.to_string();
        let local_amt = capacity_sat.to_string();
        let (ok, _value, stderr) = self.lncli(&[
            "openchannel",
            "--node_key",
            node_key.as_str(),
            "--local_amt",
            local_amt.as_str(),
            "--sat_per_vbyte=1",
        ]);
        assert!(ok, "lncli openchannel failed: {stderr}");
    }

    fn create_invoice(&mut self, amount_msat: u64) -> String {
        let amt = format!("--amt_msat={amount_msat}");
        let (ok, value, stderr) = self.lncli(&["addinvoice", amt.as_str()]);
        assert!(ok, "lncli addinvoice failed: {stderr}");
        value["payment_request"]
            .as_str()
            .expect("addinvoice missing payment_request")
            .to_string()
    }

    fn pay_invoice(&mut self, invoice: &str) -> Result<(), String> {
        let (ok, value, stderr) = self.lncli(&["payinvoice", "--force", "--json", invoice]);
        eprintln!(
            "DEBUG pay_invoice ok={ok} value={value} stderr={stderr:?} invoice={invoice}"
        );
        if !ok {
            return Err(stderr);
        }
        let status = value["status"].as_str().unwrap_or("");
        let payment_error = value["payment_error"].as_str().unwrap_or("");
        if status == "SUCCEEDED" || (payment_error.is_empty() && value["payment_route"].is_object()) {
            Ok(())
        } else {
            Err(format!("lncli payinvoice did not succeed: {payment_error}"))
        }
    }

    fn channel_balances(&mut self) -> (u64, u64) {
        let (ok, value, _stderr) = self.lncli(&["listchannels"]);
        if !ok {
            return (0, 0);
        }
        let mut outbound_msat = 0;
        let mut inbound_msat = 0;
        if let Some(channels) = value["channels"].as_array() {
            for channel in channels {
                outbound_msat += json_sats(&channel["local_balance"]) * 1000;
                inbound_msat += json_sats(&channel["remote_balance"]) * 1000;
            }
        }
        (outbound_msat, inbound_msat)
    }

    fn channels(&mut self) -> (usize, usize) {
        let (ok, value, _stderr) = self.lncli(&["listchannels"]);
        if !ok || !value["channels"].is_array() {
            return (0, 0);
        }
        let channels = value["channels"].as_array().unwrap();
        let ready = channels
            .iter()
            .filter(|channel| channel["active"].as_bool().unwrap_or(false))
            .count();
        let (ok_pending, pending_value, _stderr) = self.lncli(&["pendingchannels"]);
        let pending = if ok_pending {
            ["pending_open_channels", "waiting_close_channels", "force_closing_channels"]
                .iter()
                .map(|key| pending_value[key].as_array().map(|a| a.len()).unwrap_or(0))
                .sum()
        } else {
            0
        };
        (channels.len() + pending, ready)
    }

    fn onchain_spendable(&mut self) -> u64 {
        let (ok, value, _stderr) = self.lncli(&["walletbalance"]);
        if !ok {
            return 0;
        }
        json_sats(&value["confirmed_balance"])
    }

    fn stop(&mut self) {
        let _ = self.lncli(&["stop"]);
    }
}

impl Drop for StockLndNode {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.lncli(&["stop"]);
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}