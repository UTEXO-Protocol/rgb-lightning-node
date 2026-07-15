//! Custom remote-signer daemon (Option A): runs signer-external's full in-process VLS backend with
//! the seed and answers the RLN protobuf contract over a length-prefixed TCP framing. Because the
//! backend embeds the VLS handlers, this one daemon covers *everything the seed is required for* —
//! channel signing, node-crypto (ECDH / inbound-payment / peer-storage / offers), and `sign_rgb_psbt`.
//!
//! Framing (both directions): `u32` big-endian length prefix, then that many bytes of the RLN signer
//! envelope (`crate::signer::proto`). The node ships a request frame; the daemon replies with one
//! frame (or a zero-length frame on a handler error).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Context;
use bitcoin::Network;
use lightning_signer::persist::Persist;
use signer_external::contract::{ExternalSignerBackend, SignerRequest, SignerResponse};
use tokio::net::TcpListener;

use super::framing;
use crate::signer::in_process_vls::{self, InProcessVlsTransport};
use crate::signer::types::{BootstrapData, SignerIdentity};

/// The daemon's public identity — feed these fields to the node's `POST /initexternalsigner`. Also
/// serves directly as that endpoint's request body (see `routes::init_external_signer`): the two used
/// to be independently-defined, field-for-field-identical structs. A flattening of [`BootstrapData`]
/// (kept distinct so the HTTP/`--print-bootstrap` JSON shape stays flat and copy-pasteable); the
/// `From` impls below are the only conversions — add new identity fields there, nowhere else.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct DaemonBootstrap {
    pub node_id: String,
    pub account_xpub_vanilla: String,
    pub account_xpub_colored: String,
    pub master_fingerprint: String,
    pub protocol_version: String,
    pub api_level: u32,
}

impl From<BootstrapData> for DaemonBootstrap {
    fn from(data: BootstrapData) -> Self {
        Self {
            node_id: data.identity.node_id,
            account_xpub_vanilla: data.identity.account_xpub_vanilla,
            account_xpub_colored: data.identity.account_xpub_colored,
            master_fingerprint: data.identity.master_fingerprint,
            protocol_version: data.protocol_version,
            api_level: data.api_level,
        }
    }
}

impl From<DaemonBootstrap> for BootstrapData {
    fn from(bootstrap: DaemonBootstrap) -> Self {
        Self {
            identity: SignerIdentity {
                node_id: bootstrap.node_id,
                account_xpub_vanilla: bootstrap.account_xpub_vanilla,
                account_xpub_colored: bootstrap.account_xpub_colored,
                master_fingerprint: bootstrap.master_fingerprint,
            },
            protocol_version: bootstrap.protocol_version,
            api_level: bootstrap.api_level,
        }
    }
}

/// The daemon's signer: the RLN backend over the in-process VLS handlers, holding the seed.
pub struct DaemonSigner {
    backend: Arc<dyn ExternalSignerBackend>,
    transport: Arc<InProcessVlsTransport>,
}

impl DaemonSigner {
    /// Build the signer from a 32-byte seed, persisting VLS node/channel state (and the dbid
    /// high-water mark that stops a channel's dbid from ever being reissued — see
    /// [`InProcessVlsTransport::initial_next_dbid`]) under `data_dir`, so a daemon process restart
    /// resumes signing for existing channels instead of losing their state, and can never allocate a
    /// dbid already bound to an existing channel's revocable keys.
    pub fn new(
        seed: [u8; 32],
        network: Network,
        permissive_policy: bool,
        data_dir: &Path,
    ) -> anyhow::Result<Self> {
        let persister = in_process_vls::open_restricted_persister(data_dir)?;
        Self::new_with_persister(seed, network, permissive_policy, persister)
    }

    /// Test helper: an in-memory, non-persistent node. The tests that use this never exercise daemon
    /// restarts, so there is nothing to restore; production code must go through [`Self::new`].
    #[cfg(test)]
    pub(crate) fn new_ephemeral(
        seed: [u8; 32],
        network: Network,
        permissive_policy: bool,
    ) -> anyhow::Result<Self> {
        let (backend, transport) =
            in_process_vls::build_backend_ephemeral(network, seed, permissive_policy)
                .context("daemon signer transport init failed")?;
        Ok(Self { backend, transport })
    }

    fn new_with_persister(
        seed: [u8; 32],
        network: Network,
        permissive_policy: bool,
        persister: Arc<dyn Persist>,
    ) -> anyhow::Result<Self> {
        let (backend, transport) =
            in_process_vls::build_backend(network, seed, permissive_policy, persister)
                .context("daemon signer transport init failed")?;
        Ok(Self { backend, transport })
    }

    /// The daemon's public identity, for configuring the watch-only node (`POST /initexternalsigner`).
    /// Calls the backend directly rather than going through [`Self::handle_envelope`] — this is an
    /// in-process call, so there's no reason to pay for a protobuf encode/decode round-trip of a
    /// request and response this method already has typed.
    pub fn bootstrap_identity(&self) -> anyhow::Result<DaemonBootstrap> {
        match self
            .backend
            .call(SignerRequest::Bootstrap)
            .map_err(|e| anyhow::anyhow!("bootstrap backend call failed: {e:?}"))?
        {
            SignerResponse::Bootstrap(data) => Ok(data.into()),
            other => anyhow::bail!("unexpected bootstrap response: {other:?}"),
        }
    }

    /// Handle one RLN signer envelope (protobuf bytes) → response envelope bytes.
    pub fn handle_envelope(&self, request: &[u8]) -> anyhow::Result<Vec<u8>> {
        in_process_vls::handle_envelope(self.backend.as_ref(), &self.transport, request)
    }
}

/// Daemon TLS material. When `client_ca_path` is set, client certs are required + verified (mTLS).
pub struct DaemonTlsConfig {
    pub cert_path: std::path::PathBuf,
    pub key_path: std::path::PathBuf,
    pub client_ca_path: Option<std::path::PathBuf>,
}

/// Configuration for [`run`] (the daemon binary entry point).
pub struct DaemonConfig {
    pub seed: [u8; 32],
    pub network: Network,
    pub listen_addr: std::net::SocketAddr,
    pub permissive_policy: bool,
    /// Directory for the daemon's persisted VLS node/channel state (created if missing). Must survive
    /// process restarts — see [`DaemonSigner::new`].
    pub data_dir: PathBuf,
    /// `None` = plaintext TCP (localhost / trusted link only).
    pub tls: Option<DaemonTlsConfig>,
    /// Allow a non-loopback `listen_addr` without mTLS. Dangerous for a seed-holding signer:
    /// anyone who can reach the port can request signatures (server-auth TLS does not authenticate
    /// the caller). Only for links already secured by other means, e.g. a WireGuard tunnel.
    pub allow_unauthenticated_remote: bool,
}

/// A non-loopback listener accepts signing requests from anything that can reach the port, so it
/// must be paired with mTLS (`client_ca_path`) — server-auth TLS alone still admits any client — or
/// explicitly waived with [`DaemonConfig::allow_unauthenticated_remote`].
fn check_listener_exposure(
    listen_addr: &std::net::SocketAddr,
    mtls_configured: bool,
    allow_unauthenticated_remote: bool,
) -> anyhow::Result<()> {
    if listen_addr.ip().is_loopback() || mtls_configured {
        return Ok(());
    }
    if allow_unauthenticated_remote {
        tracing::warn!(
            addr = %listen_addr,
            "listening on a non-loopback address WITHOUT client authentication: any client that \
             can reach this port can request signatures backed by the daemon's seed"
        );
        return Ok(());
    }
    anyhow::bail!(
        "refusing to listen on non-loopback {listen_addr} without mTLS: any client that can reach \
         the port could request signatures backed by the daemon's seed. Configure TLS with a \
         client CA (--tls-cert/--tls-key/--client-ca), or pass \
         --allow-unauthenticated-remote-signer if the link is secured by other means"
    )
}

/// Build the signer from the seed and serve the RLN protocol on `listen_addr` until the listener errors.
pub async fn run(config: DaemonConfig) -> anyhow::Result<()> {
    check_listener_exposure(
        &config.listen_addr,
        config
            .tls
            .as_ref()
            .is_some_and(|t| t.client_ca_path.is_some()),
        config.allow_unauthenticated_remote,
    )?;
    let signer = Arc::new(
        DaemonSigner::new(
            config.seed,
            config.network,
            config.permissive_policy,
            &config.data_dir,
        )
        .context("build daemon signer")?,
    );
    let acceptor = match &config.tls {
        Some(t) => {
            let server_config =
                super::tls::server_config(&t.cert_path, &t.key_path, t.client_ca_path.as_deref())
                    .context("build daemon TLS server config")?;
            Some(tokio_rustls::TlsAcceptor::from(server_config))
        }
        None => None,
    };
    let listener = TcpListener::bind(config.listen_addr)
        .await
        .with_context(|| format!("bind {}", config.listen_addr))?;
    tracing::info!(
        addr = %config.listen_addr,
        network = %config.network,
        tls = acceptor.is_some(),
        mtls = config.tls.as_ref().is_some_and(|t| t.client_ca_path.is_some()),
        "remote signer daemon listening"
    );
    serve(listener, signer, acceptor).await
}

/// Backoff bounds for [`retry_with_backoff`]. A transient `accept()` failure (e.g. a peer resetting
/// the connection before the kernel finished handing it off) retries almost immediately; a sustained
/// one (e.g. the process is at its file-descriptor limit) backs off exponentially so the loop doesn't
/// spin at 100% CPU re-hitting the same error, capped so the daemon still notices quickly once
/// whatever caused it clears.
const ACCEPT_ERROR_BACKOFF_MIN: std::time::Duration = std::time::Duration::from_millis(10);
const ACCEPT_ERROR_BACKOFF_MAX: std::time::Duration = std::time::Duration::from_secs(30);

/// Retry `op` until it succeeds, backing off exponentially (from [`ACCEPT_ERROR_BACKOFF_MIN`] to
/// [`ACCEPT_ERROR_BACKOFF_MAX`]) between failures. Never gives up — used for `accept()`, where the
/// daemon is the seed-holding node's only signer and nothing else can restart it if the loop exits.
async fn retry_with_backoff<F, Fut, T, E>(mut op: F) -> T
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, E>>,
    E: std::fmt::Debug,
{
    let mut backoff = ACCEPT_ERROR_BACKOFF_MIN;
    loop {
        match op().await {
            Ok(value) => return value,
            Err(e) => {
                tracing::warn!(error = ?e, backoff_ms = backoff.as_millis(), "remote signer daemon accept error, retrying");
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(ACCEPT_ERROR_BACKOFF_MAX);
            }
        }
    }
}

/// Serve the RLN signer protocol on `listener` forever. An `accept()` failure never exits this loop
/// (see [`retry_with_backoff`]) — the caller's `anyhow::Result` return type is only for the shared
/// shape with other daemon entry points; this function does not return in practice.
pub async fn serve(
    listener: TcpListener,
    signer: Arc<DaemonSigner>,
    acceptor: Option<tokio_rustls::TlsAcceptor>,
) -> anyhow::Result<()> {
    loop {
        let (tcp, _peer) = retry_with_backoff(|| listener.accept()).await;
        let signer = Arc::clone(&signer);
        let acceptor = acceptor.clone();
        tokio::spawn(async move {
            let result = match acceptor {
                Some(acceptor) => match acceptor.accept(tcp).await {
                    Ok(tls_stream) => serve_connection(tls_stream, signer).await,
                    Err(e) => Err(anyhow::anyhow!("TLS handshake failed: {e}")),
                },
                None => serve_connection(tcp, signer).await,
            };
            if let Err(e) = result {
                tracing::debug!(error = ?e, "remote signer daemon connection closed");
            }
        });
    }
}

async fn serve_connection<S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin>(
    mut stream: S,
    signer: Arc<DaemonSigner>,
) -> anyhow::Result<()> {
    loop {
        let buf = match framing::read_frame_async(&mut stream)
            .await
            .context("read frame")?
        {
            Some(buf) => buf,
            None => return Ok(()), // peer closed
        };

        // Signing can block; run it on a blocking thread so the reactor stays free.
        let signer = Arc::clone(&signer);
        let reply = tokio::task::spawn_blocking(move || signer.handle_envelope(&buf))
            .await
            .context("join")?;
        let frame = match &reply {
            Ok(bytes) => bytes.as_slice(),
            Err(e) => {
                tracing::error!(error = ?e, "remote signer handler error");
                &[] // zero-length = handler error
            }
        };
        framing::write_frame_async(&mut stream, frame)
            .await
            .context("write frame")?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    fn addr(s: &str) -> std::net::SocketAddr {
        s.parse().expect("valid socket addr")
    }

    #[test]
    fn loopback_listeners_need_no_authentication() {
        check_listener_exposure(&addr("127.0.0.1:9737"), false, false).expect("v4 loopback");
        check_listener_exposure(&addr("[::1]:9737"), false, false).expect("v6 loopback");
    }

    /// Server-auth-only TLS (`mtls_configured == false`) must not unlock a remote listener either:
    /// it authenticates the daemon to the client, not the client to the seed-holding daemon.
    #[test]
    fn non_loopback_without_mtls_is_rejected() {
        for a in ["0.0.0.0:9737", "10.0.0.5:9737", "[::]:9737"] {
            check_listener_exposure(&addr(a), false, false)
                .expect_err("non-loopback without mTLS must be refused");
        }
    }

    #[test]
    fn non_loopback_with_mtls_is_allowed() {
        check_listener_exposure(&addr("10.0.0.5:9737"), true, false).expect("mTLS remote");
    }

    #[test]
    fn explicit_footgun_flag_allows_unauthenticated_remote() {
        check_listener_exposure(&addr("0.0.0.0:9737"), false, true).expect("explicit override");
    }

    /// `accept()` failing transiently must never end the daemon's serve loop — it's the seed-holding
    /// node's only signer, and nothing else can restart it. This exercises the retry logic in
    /// isolation from `TcpListener` (whose errors aren't reliably reproducible in a test): a fake
    /// "accept" that fails a few times before succeeding must eventually return the success value,
    /// having backed off with increasing delay between attempts rather than busy-looping.
    #[tokio::test(start_paused = true)]
    async fn retry_with_backoff_recovers_after_transient_failures() {
        let attempts = AtomicU32::new(0);
        let started_at = tokio::time::Instant::now();

        let result = retry_with_backoff(|| {
            let n = attempts.fetch_add(1, Ordering::SeqCst);
            async move {
                if n < 3 {
                    Err(format!("simulated transient accept error #{n}"))
                } else {
                    Ok(n)
                }
            }
        })
        .await;

        assert_eq!(
            result, 3,
            "should succeed on the 4th attempt (n = 0,1,2 fail, n = 3 succeeds)"
        );
        assert_eq!(
            attempts.load(Ordering::SeqCst),
            4,
            "should have retried exactly 3 times"
        );

        // 3 failures wait ACCEPT_ERROR_BACKOFF_MIN, *2, *4 respectively before the next attempt.
        let expected_min_elapsed =
            ACCEPT_ERROR_BACKOFF_MIN + ACCEPT_ERROR_BACKOFF_MIN * 2 + ACCEPT_ERROR_BACKOFF_MIN * 4;
        assert!(
            started_at.elapsed() >= expected_min_elapsed,
            "elapsed {:?} should be at least {:?} — backoff is not actually delaying retries",
            started_at.elapsed(),
            expected_min_elapsed
        );
    }

    /// A failure that never clears must not make the daemon spin at 100% CPU: the backoff must keep
    /// growing (not stay pinned at the minimum) up to the cap, rather than retrying at a fixed
    /// near-zero interval forever.
    #[tokio::test(start_paused = true)]
    async fn retry_with_backoff_caps_growth_under_sustained_failure() {
        let attempts = AtomicU32::new(0);
        let started_at = tokio::time::Instant::now();

        // Never succeeds within this many attempts; race the retry loop against a timeout so the test
        // itself terminates deterministically under virtual time instead of actually looping forever.
        let outcome = tokio::time::timeout(
            ACCEPT_ERROR_BACKOFF_MAX * 3,
            retry_with_backoff(|| {
                attempts.fetch_add(1, Ordering::SeqCst);
                async move { Err::<(), _>("simulated sustained accept failure") }
            }),
        )
        .await;

        assert!(
            outcome.is_err(),
            "retry_with_backoff must never give up on its own"
        );
        // With growth capped at ACCEPT_ERROR_BACKOFF_MAX, at least (3x cap / cap) - epsilon retries
        // must fit in the timeout window; if backoff were uncapped it would grow well past this and
        // starve the attempt count, and if backoff were stuck at the minimum it would vastly exceed it.
        let attempts = attempts.load(Ordering::SeqCst);
        assert!(attempts >= 2, "backoff capped at {ACCEPT_ERROR_BACKOFF_MAX:?} should allow multiple retries in {:?}, got {attempts}", ACCEPT_ERROR_BACKOFF_MAX * 3);
        assert!(
            attempts < 1000,
            "backoff does not appear to be growing — {attempts} attempts in {:?} implies it stayed near the {:?} minimum",
            started_at.elapsed(),
            ACCEPT_ERROR_BACKOFF_MIN
        );
    }
}
