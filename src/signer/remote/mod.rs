//! Remote external signer (native only, Option A): the node connects to the signer daemon over
//! framed TCP and ships RLN protobuf envelopes. The daemon holds the seed and answers everything
//! (identity/scripts/channel signing/node-crypto/RGB PSBT), so the node itself stays watch-only and
//! needs no VLS client stack of its own — see [`daemon`] for the daemon side.

pub(crate) mod daemon;
pub(crate) mod framing;
pub(crate) mod tls;

use std::net::SocketAddr;
use std::sync::Arc;

use crate::signer::transport::{ExternalSignerTransport, SignerLinkWatch};
use crate::signer::types::RlnSignerError;

/// Build the external-signer attachment for native unlock via the **custom daemon** (Option A): the
/// node connects to the daemon over framed TCP and ships RLN envelopes. The daemon holds the seed and
/// answers every op (identity/scripts/channel signing/node-crypto/RGB PSBT), so no watch-only
/// derivation or RGB-identity override is needed here.
pub(crate) fn build_external_signer_attachment_via_daemon(
    addr: SocketAddr,
    tls: Option<std::sync::Arc<rustls::ClientConfig>>,
) -> Result<crate::signer::ExternalSignerAttachment, crate::error::APIError> {
    let transport = DaemonEnvelopeTransport::connect(addr, tls)
        .map_err(|e| crate::error::APIError::ExternalSignerUnavailable(e.to_string()))?;
    crate::ldk::attach_external_signer_transport(Arc::new(transport))
}

/// Resolve `--remote-signer-addr`, load the node's TLS material from the storage dir, and
/// connect-and-probe the daemon — the one connect recipe shared by `/initexternalsigner` (probe) and
/// `/unlock` (attach), so the two entry points cannot drift. TLS material loading (blocking file
/// reads), the daemon TCP connect, and the bootstrap probe round-trip are all synchronous and each
/// can take up to [`DAEMON_IO_TIMEOUT`]; they run on the blocking pool instead of stalling a tokio
/// worker.
pub(crate) async fn connect_daemon_attachment(
    state: &Arc<crate::utils::AppState>,
) -> Result<crate::signer::ExternalSignerAttachment, crate::error::APIError> {
    use crate::error::APIError;

    let daemon_addr = state
        .static_state
        .remote_signer_listen_addr
        .ok_or_else(|| {
            APIError::ExternalSignerProtocolError(
                "external-signer mode requires --remote-signer-addr".to_string(),
            )
        })?;
    let storage_dir_path = state.static_state.storage_dir_path.clone();
    tokio::task::spawn_blocking(move || {
        // TLS/mTLS material by convention under <storage_dir>/remote-signer-tls/ (ca.pem + optional
        // client.pem/client.key). Absent → plaintext (localhost / trusted link).
        let tls = tls::node_client_config_from_dir(&storage_dir_path)
            .map_err(|e| APIError::ExternalSignerProtocolError(e.to_string()))?;
        build_external_signer_attachment_via_daemon(daemon_addr, tls)
    })
    .await
    .map_err(|e| APIError::Unexpected(e.to_string()))?
}

/// Blanket over the two concrete stream kinds the node uses: plain `TcpStream` (dev/localhost) and
/// `rustls::StreamOwned<ClientConnection, TcpStream>` (TLS/mTLS).
trait ReadWrite: std::io::Read + std::io::Write + Send {}
impl<T: std::io::Read + std::io::Write + Send> ReadWrite for T {}

/// Per-call TCP connect/read/write timeout, so a hung daemon surfaces as an error (→ async-pending)
/// rather than blocking a signer call forever.
const DAEMON_IO_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

pub(crate) struct DaemonEnvelopeTransport {
    addr: SocketAddr,
    tls: Option<std::sync::Arc<rustls::ClientConfig>>,
    /// `None` when disconnected; re-established on the next call. Force-close resilience: a transient
    /// blip (brief network drop, or a daemon process restart — the daemon persists its VLS node and
    /// channel state, and [`Self::reconnect`] replays the Bootstrap handshake so a restarted process
    /// re-initializes its VLS handler stack and resumes signing for existing channels) is absorbed by
    /// reconnecting and retrying once, so it never surfaces to LDK as a signing error. A sustained
    /// outage returns `Err` → the channel signer maps it to LDK's async-pending sentinel, and
    /// [`Self::link`] drives `signer_unblocked` once we're back.
    stream: std::sync::Mutex<Option<Box<dyn ReadWrite>>>,
    /// Pre-encoded `SignerRequest::Bootstrap` envelope, replayed by [`Self::reconnect`].
    bootstrap_request: Vec<u8>,
    /// Marked down when a call observes a broken connection and up when one re-establishes it.
    /// Exposed via [`ExternalSignerTransport::link_watch`] so `start_ldk` drives `signer_unblocked`
    /// exactly while there's an outage to recover from, instead of polling forever.
    link: Arc<SignerLinkWatch>,
}

impl DaemonEnvelopeTransport {
    /// Connect to the daemon. `tls = Some` establishes a (m)TLS session verifying the daemon cert's
    /// SAN against [`tls::SERVER_NAME`]; `None` is plaintext (localhost / trusted link only).
    pub(crate) fn connect(
        addr: SocketAddr,
        tls: Option<std::sync::Arc<rustls::ClientConfig>>,
    ) -> Result<Self, RlnSignerError> {
        let bootstrap_request = crate::signer::proto::encode_signer_request(
            &signer_external::contract::SignerRequest::Bootstrap,
        )
        .map_err(|e| RlnSignerError::Protocol(format!("encode bootstrap request: {e}")))?;
        let transport = Self {
            addr,
            tls,
            stream: std::sync::Mutex::new(None),
            bootstrap_request,
            link: Arc::new(SignerLinkWatch::new_connected()),
        };
        // Establish eagerly so attach-time failures are surfaced immediately. No handshake replay
        // here: every attach flow probes `SignerRequest::Bootstrap` itself right after connecting.
        let stream = transport.new_stream()?;
        *transport.stream.lock().expect("stream lock") = Some(stream);
        Ok(transport)
    }

    fn new_stream(&self) -> Result<Box<dyn ReadWrite>, RlnSignerError> {
        let tcp = std::net::TcpStream::connect_timeout(&self.addr, DAEMON_IO_TIMEOUT)
            .map_err(|e| RlnSignerError::Transport(format!("connect remote signer daemon: {e}")))?;
        tcp.set_read_timeout(Some(DAEMON_IO_TIMEOUT))
            .and_then(|_| tcp.set_write_timeout(Some(DAEMON_IO_TIMEOUT)))
            .map_err(|e| RlnSignerError::Transport(format!("remote signer io timeout: {e}")))?;
        match &self.tls {
            Some(config) => {
                let server_name = rustls::pki_types::ServerName::try_from(tls::SERVER_NAME)
                    .map_err(|e| {
                        RlnSignerError::Transport(format!("invalid TLS server name: {e}"))
                    })?;
                let conn =
                    rustls::ClientConnection::new(config.clone(), server_name).map_err(|e| {
                        RlnSignerError::Transport(format!("remote signer TLS client: {e}"))
                    })?;
                Ok(Box::new(rustls::StreamOwned::new(conn, tcp)))
            }
            None => Ok(Box::new(tcp)),
        }
    }

    /// One request/response over an established stream. `Ok(None)` = the daemon's 0-length handler-error
    /// sentinel (a valid response, not an IO failure — do NOT reconnect).
    fn framed_call(stream: &mut dyn ReadWrite, request: &[u8]) -> std::io::Result<Option<Vec<u8>>> {
        framing::write_frame(stream, request)?;
        framing::read_frame(stream)
    }

    /// Establish a fresh connection and replay the Bootstrap handshake over it before anything else
    /// is sent: a restarted daemon process starts with an uninitialized VLS handler stack (the
    /// `HsmdInit2` init runs once per process), so without the replay every op after a daemon restart
    /// would fail forever — the very restart the daemon's persisted state exists to absorb. On an
    /// already-initialized daemon (the outage was on our side) the replay is an idempotent no-op.
    fn reconnect(&self) -> Result<Box<dyn ReadWrite>, RlnSignerError> {
        let mut stream = self.new_stream()?;
        match Self::framed_call(stream.as_mut(), &self.bootstrap_request) {
            Ok(Some(_)) => Ok(stream),
            Ok(None) => Err(RlnSignerError::Transport(
                "remote signer daemon rejected the bootstrap handshake replayed on reconnect"
                    .into(),
            )),
            Err(e) => Err(RlnSignerError::Transport(format!(
                "remote signer bootstrap replay on reconnect: {e}"
            ))),
        }
    }

    fn call_blocking(&self, request: &[u8]) -> Result<Vec<u8>, RlnSignerError> {
        if request.len() > framing::MAX_FRAME_LEN as usize {
            // Reject before touching the connection: the daemon would refuse the frame by dropping
            // the socket, tearing down a perfectly good link and surfacing an opaque IO error.
            return Err(RlnSignerError::Transport(format!(
                "signer request of {} bytes exceeds the {} byte frame limit",
                request.len(),
                framing::MAX_FRAME_LEN
            )));
        }
        let mut guard = self
            .stream
            .lock()
            .map_err(|_| RlnSignerError::Transport("remote signer transport poisoned".into()))?;

        // Attempt over the live connection; on an IO failure, reconnect once and retry (absorbs a
        // daemon restart / brief drop). A protocol handler-error (0-length reply) is not retried.
        let mut last_err: Option<RlnSignerError> = None;
        for _ in 0..2 {
            if guard.is_none() {
                match self.reconnect() {
                    Ok(stream) => {
                        *guard = Some(stream);
                        // We were disconnected and just re-established the link: wake anything
                        // waiting to re-drive parked signer operations.
                        self.link.mark_reconnected();
                    }
                    Err(e) => {
                        last_err = Some(e);
                        break;
                    }
                }
            }
            let stream = guard.as_mut().expect("stream present");
            match Self::framed_call(stream.as_mut(), request) {
                Ok(Some(reply)) => return Ok(reply),
                Ok(None) => {
                    return Err(RlnSignerError::Transport(
                        "remote signer daemon reported a handler error".into(),
                    ))
                }
                Err(e) => {
                    // Broken connection: drop it so the next iteration reconnects.
                    *guard = None;
                    self.link.mark_disconnected();
                    last_err = Some(RlnSignerError::Transport(format!("remote signer io: {e}")));
                }
            }
        }
        Err(last_err
            .unwrap_or_else(|| RlnSignerError::Transport("remote signer unavailable".into())))
    }
}

#[cfg(test)]
impl DaemonEnvelopeTransport {
    /// Simulate a dropped connection (transient blip) — the next call must reconnect.
    fn force_disconnect(&self) {
        *self.stream.lock().expect("stream lock") = None;
    }
}

impl ExternalSignerTransport for DaemonEnvelopeTransport {
    fn call(&self, request: &[u8]) -> Result<Vec<u8>, RlnSignerError> {
        // LDK invokes signer calls synchronously, sometimes from a tokio worker thread. A round-trip
        // here can block for up to `DAEMON_IO_TIMEOUT` (doubled by the reconnect-and-retry in
        // `call_blocking`); without `block_in_place`, that would stall every other task scheduled on
        // this worker for the same duration. `block_in_place` tells the runtime to move those tasks
        // to another worker first. Only valid on a multi-thread runtime worker: it panics on a
        // current_thread runtime (which a `NodeHandle` embedder may well be running us on — the
        // runtime flavor is the caller's choice, not ours), and is neither valid nor needed on a
        // plain OS thread (e.g. the daemon binary's own threads in tests).
        match tokio::runtime::Handle::try_current() {
            Ok(handle) if handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread => {
                tokio::task::block_in_place(|| self.call_blocking(request))
            }
            _ => self.call_blocking(request),
        }
    }

    fn link_watch(&self) -> Option<Arc<SignerLinkWatch>> {
        Some(Arc::clone(&self.link))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::signer::proto::{decode_signer_response, encode_signer_request};
    use signer_external::contract::{SignerRequest, SignerResponse};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    /// End-to-end vertical slice: node connects to the daemon over the framed TCP envelope; bootstrap
    /// with correctly-derived RGB xpubs, the destination script, AND a seed-only RLN-private op
    /// (offer-key HMAC) that a watch-only node cannot compute.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn custom_daemon_answers_identity_scripts_and_private_ops() {
        use signer_external::contract::{NodeRequest, NodeResponse};

        let seed = [42u8; 32];
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        let signer = std::sync::Arc::new(
            super::daemon::DaemonSigner::new_ephemeral(seed, bitcoin::Network::Regtest, true)
                .expect("daemon signer"),
        );
        tokio::spawn(super::daemon::serve(listener, signer, None));

        let result = tokio::task::spawn_blocking(move || -> Result<(), String> {
            let transport =
                DaemonEnvelopeTransport::connect(addr, None).map_err(|e| e.to_string())?;
            let call = |req: &SignerRequest| -> Result<SignerResponse, String> {
                let bytes = encode_signer_request(req).map_err(|e| e.to_string())?;
                let reply = transport.call(&bytes).map_err(|e| e.to_string())?;
                decode_signer_response(&reply).map_err(|e| e.to_string())
            };

            // Bootstrap: RGB xpubs derived from the seed (vanilla != colored — different coin types).
            match call(&SignerRequest::Bootstrap)? {
                SignerResponse::Bootstrap(d) => {
                    if d.identity.node_id.is_empty() {
                        return Err("empty node_id".into());
                    }
                    if d.identity.account_xpub_vanilla.is_empty()
                        || d.identity.account_xpub_vanilla == d.identity.account_xpub_colored
                    {
                        return Err("RGB xpubs not properly derived (vanilla == colored)".into());
                    }
                }
                other => return Err(format!("bootstrap: {other:?}")),
            }

            // Destination script (P2WPKH "0014"+40hex).
            match call(&SignerRequest::Node(NodeRequest::GetDestinationScript {
                channel_keys_id_hex: "00".repeat(32),
            }))? {
                SignerResponse::Node(NodeResponse::Script { script_hex }) => {
                    if !script_hex.starts_with("0014") || script_hex.len() != 44 {
                        return Err(format!("destination not P2WPKH: {script_hex}"));
                    }
                }
                other => return Err(format!("destination: {other:?}")),
            }

            // Seed-only RLN-private op the watch-only node cannot compute.
            match call(&SignerRequest::Node(NodeRequest::GetHmacForOfferKey))? {
                SignerResponse::Node(NodeResponse::HmacForOfferKey { key_hex }) => {
                    if key_hex.len() != 64 {
                        return Err(format!("offer HMAC key wrong length: {key_hex}"));
                    }
                }
                other => return Err(format!("offer hmac: {other:?}")),
            }
            Ok(())
        })
        .await
        .expect("join blocking");

        result.expect("daemon responses");
    }

    /// The custom daemon over **mTLS**: self-signed server cert (SAN = `tls::SERVER_NAME`) pinned by the
    /// node, plus a node client cert the daemon requires + verifies. A bootstrap must round-trip.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn custom_daemon_over_mtls() {
        let server = rcgen::generate_simple_self_signed(vec![super::tls::SERVER_NAME.to_string()])
            .expect("server cert");
        let client = rcgen::generate_simple_self_signed(vec!["rln-node-client".to_string()])
            .expect("client cert");
        let dir = tempfile::tempdir().expect("tempdir");
        let write = |name: &str, contents: String| {
            let p = dir.path().join(name);
            std::fs::write(&p, contents).expect("write pem");
            p
        };
        let server_cert = write("server.pem", server.cert.pem());
        let server_key = write("server.key", server.key_pair.serialize_pem());
        let client_cert = write("client.pem", client.cert.pem());
        let client_key = write("client.key", client.key_pair.serialize_pem());

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        let server_config =
            super::tls::server_config(&server_cert, &server_key, Some(&client_cert))
                .expect("server tls config");
        let acceptor = tokio_rustls::TlsAcceptor::from(server_config);
        let signer = std::sync::Arc::new(
            super::daemon::DaemonSigner::new_ephemeral([7u8; 32], bitcoin::Network::Regtest, true)
                .expect("daemon signer"),
        );
        tokio::spawn(super::daemon::serve(listener, signer, Some(acceptor)));

        let client_config =
            super::tls::client_config(&server_cert, Some((&client_cert, &client_key)))
                .expect("client tls config");

        let ok = tokio::task::spawn_blocking(move || -> Result<(), String> {
            let transport = DaemonEnvelopeTransport::connect(addr, Some(client_config))
                .map_err(|e| e.to_string())?;
            let bytes =
                encode_signer_request(&SignerRequest::Bootstrap).map_err(|e| e.to_string())?;
            let reply = transport.call(&bytes).map_err(|e| e.to_string())?;
            match decode_signer_response(&reply).map_err(|e| e.to_string())? {
                SignerResponse::Bootstrap(d) if !d.identity.node_id.is_empty() => Ok(()),
                other => Err(format!("unexpected response: {other:?}")),
            }
        })
        .await
        .expect("join blocking");

        ok.expect("mTLS bootstrap");
    }

    /// Force-close resilience: a dropped connection (transient blip) is absorbed — the transport
    /// reconnects and the next call succeeds instead of surfacing an error to LDK.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn daemon_transport_reconnects_after_blip() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        let signer = std::sync::Arc::new(
            super::daemon::DaemonSigner::new_ephemeral([9u8; 32], bitcoin::Network::Regtest, true)
                .expect("daemon signer"),
        );
        tokio::spawn(super::daemon::serve(listener, signer, None));

        let ok = tokio::task::spawn_blocking(move || -> Result<(), String> {
            let transport =
                DaemonEnvelopeTransport::connect(addr, None).map_err(|e| e.to_string())?;
            let req =
                encode_signer_request(&SignerRequest::Bootstrap).map_err(|e| e.to_string())?;

            // First call over the live connection.
            transport.call(&req).map_err(|e| e.to_string())?;
            // Simulate a blip, then call again — must reconnect and succeed.
            transport.force_disconnect();
            transport
                .call(&req)
                .map_err(|e| format!("reconnect call failed: {e}"))?;
            Ok(())
        })
        .await
        .expect("join blocking");

        ok.expect("reconnect");
    }

    /// The event-driven half of the `signer_unblocked` resilience fix in `start_ldk`: a genuine
    /// reconnect (recovering from a dropped connection, not the initial connect) must fire the
    /// link watch's `changed()` signal with `is_connected` back to `true`, so a waiting task can
    /// react immediately instead of waiting out a polling interval.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn daemon_transport_notifies_on_reconnect_after_blip() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        let signer = std::sync::Arc::new(
            super::daemon::DaemonSigner::new_ephemeral([13u8; 32], bitcoin::Network::Regtest, true)
                .expect("daemon signer"),
        );
        tokio::spawn(super::daemon::serve(listener, signer, None));

        let transport = Arc::new(
            tokio::task::spawn_blocking(move || DaemonEnvelopeTransport::connect(addr, None))
                .await
                .expect("join blocking")
                .expect("connect"),
        );
        let link = transport
            .link_watch()
            .expect("remote transport must expose a link watch");
        let req = encode_signer_request(&SignerRequest::Bootstrap).expect("encode bootstrap");

        // First call over the live connection — establishes a working link, no reconnect yet, so no
        // notification should be pending.
        {
            let transport = Arc::clone(&transport);
            let req = req.clone();
            tokio::task::spawn_blocking(move || transport.call(&req))
                .await
                .expect("join blocking")
                .expect("first call");
        }

        transport.force_disconnect();

        // `mark_reconnected` buffers a permit if it fires before anyone is waiting, so this is
        // robust to whichever of these two tasks the scheduler runs first.
        let wait_for_notify = tokio::spawn({
            let link = Arc::clone(&link);
            async move {
                tokio::time::timeout(std::time::Duration::from_secs(5), link.changed())
                    .await
                    .expect("reconnect signal did not fire within timeout")
            }
        });

        // The call that actually observes the dropped connection and reconnects.
        let transport2 = Arc::clone(&transport);
        tokio::task::spawn_blocking(move || transport2.call(&req))
            .await
            .expect("join blocking")
            .expect("reconnect call failed");

        wait_for_notify.await.expect("notify task join");
        assert!(link.is_connected(), "link must report connected again");
    }

    /// A daemon **process restart** must be absorbed transparently: the restarted daemon starts with
    /// an uninitialized VLS handler stack (`HsmdInit2` runs once per process), so the transport's
    /// reconnect path must replay the Bootstrap handshake before resending the pending envelope.
    /// Without the replay, every op after a daemon restart fails forever — the exact outage the
    /// daemon's persisted state exists to absorb. Ephemeral daemons keep this focused on the
    /// transport's replay (a fresh signer over the same seed is exactly "uninitialized handler
    /// stack"); state restoration across restarts is covered by
    /// `daemon_restart_with_persistence_never_reissues_a_dbid`.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn daemon_transport_replays_bootstrap_after_daemon_process_restart() {
        use signer_external::contract::{NodeRequest, NodeResponse};

        let seed = [21u8; 32];

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        let signer = std::sync::Arc::new(
            super::daemon::DaemonSigner::new_ephemeral(seed, bitcoin::Network::Regtest, true)
                .expect("daemon signer"),
        );
        let serve_task = tokio::spawn(super::daemon::serve(listener, signer, None));

        let transport = Arc::new(
            tokio::task::spawn_blocking(move || DaemonEnvelopeTransport::connect(addr, None))
                .await
                .expect("join blocking")
                .expect("connect"),
        );
        // ECDH goes straight through the VLS root handler (no lazy xpub fetch that would initialize
        // the handler stack as a side effect), so it only succeeds on an initialized daemon —
        // exactly what makes it prove the handshake replay happened.
        let peer_key = bitcoin::secp256k1::SecretKey::from_slice(&[3u8; 32])
            .expect("secret key")
            .public_key(&bitcoin::secp256k1::Secp256k1::new());
        let node_op = encode_signer_request(&SignerRequest::Node(NodeRequest::Ecdh {
            recipient: "node".to_string(),
            other_key: peer_key.to_string(),
            tweak: None,
        }))
        .expect("encode node op");

        // Normal session: bootstrap (as every attach flow does), then a node op that requires the
        // initialized root handler.
        {
            let transport = Arc::clone(&transport);
            let node_op = node_op.clone();
            tokio::task::spawn_blocking(move || {
                let bootstrap =
                    encode_signer_request(&SignerRequest::Bootstrap).expect("encode bootstrap");
                transport.call(&bootstrap).expect("bootstrap");
                transport.call(&node_op).expect("node op before restart");
            })
            .await
            .expect("join blocking");
        }

        // "Restart" the daemon process: drop the node's connection, stop the old serve loop, and
        // bring up a fresh daemon (uninitialized VLS handler stack) over the same seed and address.
        transport.force_disconnect();
        serve_task.abort();
        let _ = serve_task.await;
        let listener = tokio::net::TcpListener::bind(addr).await.expect("rebind");
        let signer = std::sync::Arc::new(
            super::daemon::DaemonSigner::new_ephemeral(seed, bitcoin::Network::Regtest, true)
                .expect("restarted daemon signer"),
        );
        tokio::spawn(super::daemon::serve(listener, signer, None));

        // The very next op must succeed: reconnect + Bootstrap handshake replay, then the envelope.
        let reply = tokio::task::spawn_blocking({
            let transport = Arc::clone(&transport);
            move || transport.call(&node_op)
        })
        .await
        .expect("join blocking")
        .expect("node op after daemon restart");
        match decode_signer_response(&reply).expect("decode") {
            SignerResponse::Node(NodeResponse::Ecdh { shared_secret_hex }) => {
                assert_eq!(shared_secret_hex.len(), 64, "not a 32-byte shared secret")
            }
            other => panic!("unexpected response: {other:?}"),
        }
    }

    /// Restart safety: VLS derives each channel's keys from `seed + dbid`, so a dbid must never be
    /// reissued for a given seed — reusing one would give a brand-new channel the same revocable keys
    /// as an existing one (see `InProcessVlsTransport::initial_next_dbid`). A daemon that persists to
    /// disk (unlike the ephemeral `new_ephemeral` used by the other tests here) must restore its
    /// existing channel across a process restart and continue allocating fresh dbids from where it
    /// left off, never from `1` again.
    #[test]
    fn daemon_restart_with_persistence_never_reissues_a_dbid() {
        use signer_external::contract::{ChannelRequest, ChannelResponse};

        let tmp = tempfile::tempdir().expect("tempdir");
        // A subdirectory the signer creates itself: `tempdir()` inherits the process umask, and the
        // signer refuses (by design) a pre-existing group/other-accessible state dir.
        let data_dir = tmp.path().join("signer-db");
        let seed = [11u8; 32];

        let generate_and_derive = |signer: &super::daemon::DaemonSigner| -> String {
            // The wire handshake (Bootstrap -> HsmdInit2) must run before any channel op, on both the
            // fresh and the restarted daemon, exactly as a real node reconnecting after a daemon
            // restart would do.
            signer.bootstrap_identity().expect("bootstrap");
            let generate = encode_signer_request(&SignerRequest::Channel(
                ChannelRequest::GenerateChannelKeysId {
                    inbound: false,
                    channel_value_satoshis: 100_000,
                    user_channel_id: 1,
                },
            ))
            .expect("encode generate");
            let reply = signer.handle_envelope(&generate).expect("generate keys id");
            let channel_keys_id_hex = match decode_signer_response(&reply).expect("decode generate")
            {
                SignerResponse::Channel(ChannelResponse::GeneratedChannelKeysId {
                    channel_keys_id_hex,
                }) => channel_keys_id_hex,
                other => panic!("unexpected generate response: {other:?}"),
            };
            let derive = encode_signer_request(&SignerRequest::Channel(
                ChannelRequest::DeriveChannelSigner {
                    channel_value_satoshis: 100_000,
                    channel_keys_id_hex: channel_keys_id_hex.clone(),
                },
            ))
            .expect("encode derive");
            signer
                .handle_envelope(&derive)
                .expect("derive channel signer");
            channel_keys_id_hex
        };

        // First "session": open one channel, then drop the daemon (simulating a process exit)
        // without ever forgetting/closing the channel.
        let first_channel_keys_id_hex = {
            let signer =
                super::daemon::DaemonSigner::new(seed, bitcoin::Network::Regtest, true, &data_dir)
                    .expect("first daemon signer");
            generate_and_derive(&signer)
        };

        // "Restart": a fresh `DaemonSigner` over the same data_dir must restore the existing channel
        // and allocate a fresh dbid for the new one, never reissuing the first channel's dbid.
        let signer =
            super::daemon::DaemonSigner::new(seed, bitcoin::Network::Regtest, true, &data_dir)
                .expect("restarted daemon signer");
        let second_channel_keys_id_hex = generate_and_derive(&signer);

        assert_ne!(
            first_channel_keys_id_hex, second_channel_keys_id_hex,
            "restarted daemon reissued the first channel's dbid: {first_channel_keys_id_hex}"
        );
    }

    /// `block_in_place` bridging: calling `transport.call` directly on an async task (as LDK's sync
    /// signer trait does, not wrapped in `spawn_blocking`) must not starve other tasks scheduled on
    /// the same worker for the duration of a slow round-trip. On a single-worker runtime, a
    /// concurrently spawned task's plain async sleep must still complete while `call` is blocked
    /// reading the (deliberately slow) daemon's reply — proving the worker was freed rather than
    /// pinned by the blocking TCP read.
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn daemon_transport_call_does_not_starve_other_tasks_on_single_worker() {
        // A raw stub daemon: reads one frame, sleeps (simulating a slow daemon under load), then
        // replies with the 0-length handler-error sentinel.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept");
            framing::read_frame_async(&mut stream)
                .await
                .expect("read frame")
                .expect("open frame");
            tokio::time::sleep(Duration::from_millis(300)).await;
            framing::write_frame_async(&mut stream, &[])
                .await
                .expect("write handler-error sentinel");
        });

        let transport = DaemonEnvelopeTransport::connect(addr, None).expect("connect");

        // Spawned (not awaited inline) so it runs concurrently with `other_task` below on the sole
        // worker. `call` is a plain sync method here — exactly how LDK's sync signer trait invokes it.
        let call_task = tokio::spawn(async move { transport.call(&[]) });

        let progressed = std::sync::Arc::new(AtomicBool::new(false));
        let progressed_clone = progressed.clone();
        let other_task = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            progressed_clone.store(true, Ordering::SeqCst);
        });

        // The daemon stub sleeps 300ms before replying; give `other_task` (50ms sleep) a wide margin
        // to complete first, then confirm it actually did — while `call_task`'s round-trip is still
        // in flight — before waiting on `call_task` itself.
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert!(
            progressed.load(Ordering::SeqCst),
            "other_task did not progress while transport.call() was blocking the sole worker \
             on the daemon round-trip — block_in_place is not freeing the worker"
        );

        other_task.await.expect("other_task join");
        call_task
            .await
            .expect("call_task join")
            .expect_err("stub daemon replies 0-length");
    }
}
