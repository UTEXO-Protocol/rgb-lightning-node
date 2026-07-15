//! Remote external signer daemon (Option A) for the RGB Lightning Node.
//!
//! Holds the seed and answers every signing operation the RLN node needs — identity/bootstrap (with
//! correctly-derived RGB xpubs), destination/shutdown scripts, channel signing, node-crypto
//! (ECDH / inbound-payment / peer-storage / offers), and `sign_rgb_psbt` — over a length-prefixed TCP
//! framing. The RLN node connects to `--listen-addr` (its `--remote-signer-addr`).
//!
//! The seed never leaves this process. Because any client that can reach the port can request
//! signatures, a non-loopback `--listen-addr` requires mTLS (`--client-ca`) — server-auth TLS alone
//! does not authenticate the caller — unless `--allow-unauthenticated-remote-signer` explicitly
//! accepts that risk (e.g. the link is secured by a WireGuard tunnel).

use std::fs;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use anyhow::{anyhow, Context};
use bitcoin::hex::{DisplayHex, FromHex};
use bitcoin::Network;
use clap::Parser;
use rand::rngs::OsRng;
use rand::RngCore;

use rgb_lightning_node::{run_signer_daemon, DaemonConfig};

#[derive(Parser)]
#[command(author, version, about = "RLN remote external signer daemon")]
struct Args {
    /// Path to the 32-byte hex seed file. A fresh seed is generated (mode 0600) if it does not exist.
    #[arg(long)]
    seed_file: PathBuf,

    /// Directory for the daemon's persisted VLS node/channel state (created if missing). Defaults to a
    /// `signer-db` directory next to `--seed-file`. Must survive process restarts: it is what lets a
    /// restarted daemon resume signing for existing channels and guarantees a channel's dbid is never
    /// reissued (VLS derives each channel's keys from `seed + dbid`, so a reused dbid would let a new
    /// channel reuse an existing channel's revocable keys).
    #[arg(long)]
    data_dir: Option<PathBuf>,

    /// Bitcoin network: bitcoin | testnet | signet | regtest.
    #[arg(long, default_value = "regtest")]
    network: String,

    /// Address to listen on for the RLN node (its `--remote-signer-addr`). Non-loopback addresses
    /// require mTLS (`--client-ca`) or `--allow-unauthenticated-remote-signer`.
    #[arg(long, default_value = "127.0.0.1:9737")]
    listen_addr: SocketAddr,

    /// Use a permissive VLS policy/approver. Dev/test only — never on mainnet.
    #[arg(long, default_value_t = false)]
    permissive: bool,

    /// Print the daemon's bootstrap identity (JSON) for the node's `POST /initexternalsigner`, then
    /// exit without listening.
    #[arg(long, default_value_t = false)]
    print_bootstrap: bool,

    /// PEM server certificate (SAN must include `rln-remote-signer`). Enables TLS. Without it the
    /// link is plaintext — use only over localhost / a trusted network.
    #[arg(long, requires = "tls_key")]
    tls_cert: Option<PathBuf>,

    /// PEM server private key (pairs with `--tls-cert`).
    #[arg(long, requires = "tls_cert")]
    tls_key: Option<PathBuf>,

    /// PEM CA to verify node client certificates. Enables mTLS (requires `--tls-cert`).
    #[arg(long, requires = "tls_cert")]
    client_ca: Option<PathBuf>,

    /// Allow a non-loopback `--listen-addr` without mTLS. Dangerous for a seed-holding signer:
    /// anyone who can reach the port can request signatures (server-auth TLS does not authenticate
    /// the caller). Only for links already secured by other means, e.g. a WireGuard tunnel.
    #[arg(long, default_value_t = false)]
    allow_unauthenticated_remote_signer: bool,
}

fn load_or_generate_seed(path: &Path) -> anyhow::Result<[u8; 32]> {
    if path.exists() {
        // Freshly generated seeds are created 0600 below, but this file may predate the daemon
        // (e.g. written with a shell redirect at the umask default 0644). Refuse anything that is
        // not a regular owner-only file rather than silently trusting a possibly-exposed seed.
        rgb_lightning_node::check_restricted_file(path)
            .with_context(|| format!("seed file {} failed safety checks", path.display()))?;
        let contents = fs::read_to_string(path)
            .with_context(|| format!("read seed file {}", path.display()))?;
        let bytes = Vec::<u8>::from_hex(contents.trim()).context("seed file must be hex")?;
        bytes
            .try_into()
            .map_err(|_| anyhow!("seed file must decode to exactly 32 bytes"))
    } else {
        let mut seed = [0u8; 32];
        OsRng.fill_bytes(&mut seed);
        // Atomic 0600 creation — no window where the seed is readable at default (umask) permissions.
        rgb_lightning_node::write_restricted_file(path, seed.to_lower_hex_string().as_bytes())
            .map_err(|e| anyhow!("write seed file {}: {e}", path.display()))?;
        tracing::warn!(path = %path.display(), "generated a new signer seed");
        Ok(seed)
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let args = Args::parse();
    let network = Network::from_str(&args.network)
        .map_err(|_| anyhow!("invalid --network: {}", args.network))?;
    if network == Network::Bitcoin && args.permissive {
        anyhow::bail!("--permissive is not allowed on mainnet");
    }
    let seed = load_or_generate_seed(&args.seed_file)?;

    // The data dir itself is created/verified owner-only (0700) inside `DaemonSigner::new` — it
    // holds VLS channel signer state and the dbid high-water mark, i.e. seed-grade material.
    let data_dir = args.data_dir.clone().unwrap_or_else(|| {
        args.seed_file
            .parent()
            .map(|dir| dir.join("signer-db"))
            .unwrap_or_else(|| PathBuf::from("signer-db"))
    });

    if args.print_bootstrap {
        let signer =
            rgb_lightning_node::DaemonSigner::new(seed, network, args.permissive, &data_dir)?;
        let bootstrap = signer.bootstrap_identity()?;
        println!("{}", serde_json::to_string_pretty(&bootstrap)?);
        return Ok(());
    }

    let tls = match (args.tls_cert, args.tls_key) {
        (Some(cert_path), Some(key_path)) => Some(rgb_lightning_node::DaemonTlsConfig {
            cert_path,
            key_path,
            client_ca_path: args.client_ca,
        }),
        _ => None,
    };

    // Listener-exposure policy (non-loopback requires mTLS or the explicit footgun flag) is
    // enforced inside `run_signer_daemon`, so it also covers direct library users.
    run_signer_daemon(DaemonConfig {
        seed,
        network,
        listen_addr: args.listen_addr,
        permissive_policy: args.permissive,
        data_dir,
        tls,
        allow_unauthenticated_remote: args.allow_unauthenticated_remote_signer,
    })
    .await
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    /// A freshly generated seed must be created with 0600 permissions from the moment the file first
    /// exists — never briefly at default (umask) permissions, since the seed controls all node funds.
    #[test]
    fn generated_seed_file_is_created_with_restricted_permissions() {
        let dir = tempfile::tempdir().expect("tempdir");
        let seed_path = dir.path().join("seed");

        let seed = load_or_generate_seed(&seed_path).expect("generate seed");

        let mode = fs::metadata(&seed_path)
            .expect("stat seed file")
            .permissions()
            .mode();
        assert_eq!(
            mode & 0o777,
            0o600,
            "seed file permissions are {:o}, expected 0600",
            mode & 0o777
        );

        // Round-trip: re-reading the same path must reproduce the identical seed, not regenerate one.
        let reloaded = load_or_generate_seed(&seed_path).expect("reload seed");
        assert_eq!(seed, reloaded);
    }

    /// A seed file the operator created by hand (e.g. `echo $SEED > seed`, typically 0644) must be
    /// refused instead of silently used — group/other readability means the seed may already be
    /// exposed. Once tightened to owner-only it must load normally.
    #[test]
    fn preexisting_seed_file_with_broad_permissions_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let seed_path = dir.path().join("seed");
        fs::write(&seed_path, "aa".repeat(32)).expect("write seed");
        fs::set_permissions(&seed_path, fs::Permissions::from_mode(0o644)).expect("chmod");

        load_or_generate_seed(&seed_path).expect_err("group/other-readable seed must be refused");

        fs::set_permissions(&seed_path, fs::Permissions::from_mode(0o600)).expect("chmod");
        load_or_generate_seed(&seed_path).expect("owner-only seed loads");
    }

    /// Polls `path`'s permissions on a background thread for up to ~500ms, recording whether any
    /// observed mode ever differed from 0600.
    fn poll_for_non_0600_permissions(
        path: PathBuf,
    ) -> (std::thread::JoinHandle<()>, Arc<AtomicBool>) {
        let observed_other_than_0600 = Arc::new(AtomicBool::new(false));
        let flag = observed_other_than_0600.clone();
        let handle = std::thread::spawn(move || {
            for _ in 0..500 {
                if let Ok(meta) = fs::metadata(&path) {
                    if meta.permissions().mode() & 0o777 != 0o600 {
                        flag.store(true, Ordering::SeqCst);
                        return;
                    }
                }
                std::thread::sleep(Duration::from_millis(1));
            }
        });
        (handle, observed_other_than_0600)
    }

    /// Reproduces the pre-fix pattern this finding flagged (create via `fs::write`, chmod after) to
    /// demonstrate the window it opens: a concurrent reader can observe the file at broader-than-0600
    /// permissions between the two syscalls. Test-only — production code no longer does this (see
    /// `write_restricted_file`), which is why this lives here rather than in the binary's real logic.
    fn write_then_chmod_racy(path: &Path, bytes: &[u8]) {
        fs::write(path, bytes).expect("write");
        // The real bug's window was just "however the OS schedules the two syscalls apart"; widen it
        // deliberately here so a millisecond-granularity poller reliably observes it in a test.
        std::thread::sleep(Duration::from_millis(50));
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).expect("chmod");
    }

    #[test]
    fn racy_write_then_chmod_exposes_a_permission_window() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("seed");
        let (poller, observed) = poll_for_non_0600_permissions(path.clone());

        write_then_chmod_racy(&path, b"deadbeef");
        poller.join().expect("poller join");

        assert!(
            observed.load(Ordering::SeqCst),
            "expected the racy write-then-chmod pattern to expose a widened-permission window \
             (if this fails, the poller lost the race by scheduling luck, not that the window is gone)"
        );
    }

    /// The atomicity guarantee `write_restricted_file` (and, through it, `load_or_generate_seed`)
    /// relies on: unlike `write_then_chmod_racy` above, a concurrent poller must never observe the
    /// file at any permission other than 0600, because the mode is applied by the single `open(2)`
    /// syscall that creates the file — there is no separate step that could be observed in between.
    #[test]
    fn atomic_seed_write_never_exposes_a_permission_window() {
        let dir = tempfile::tempdir().expect("tempdir");
        let seed_path = dir.path().join("seed");
        let (poller, observed) = poll_for_non_0600_permissions(seed_path.clone());

        load_or_generate_seed(&seed_path).expect("generate seed");
        poller.join().expect("poller join");

        assert!(
            !observed.load(Ordering::SeqCst),
            "load_or_generate_seed exposed a widened-permission window"
        );
    }
}
