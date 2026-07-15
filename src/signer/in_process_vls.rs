//! In-process VLS transport: drives the VLS handlers (`InitHandler` → `RootHandler` → `ChannelHandler`)
//! directly, with no network below it. Shared by the uniffi in-process signer
//! (`uniffi_api::native_signer`) and the remote-signer daemon (`signer::remote::daemon`) so the two do
//! not each carry a copy.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use anyhow::Context;
use bitcoin::hex::{DisplayHex, FromHex};
use bitcoin::secp256k1::Secp256k1;
use bitcoin::Network;
use signer_external::contract::{
    ChannelOp, ChannelRequest, ChannelResponse, ExternalSignerBackend, SignerRequest,
    SignerResponse,
};
use signer_external::vls_adapter::vls_real::RealVlsClient;
use signer_external::vls_adapter::VlsSignerAdapter;
use vls_protocol::msgs;
use vls_protocol_client::{Error as VlsClientError, Transport};
use vls_protocol_signer::approver::WarningPositiveApprover;
use vls_protocol_signer::handler::{Handler, InitHandler, RootHandler};
use vls_protocol_signer::lightning_signer;
use vls_protocol_signer::lightning_signer::lightning::sign::ChannelSigner as _;
use vls_protocol_signer::lightning_signer::node::{Node, NodeConfig, NodeServices};
use vls_protocol_signer::lightning_signer::persist::{DummyPersister, Persist};
use vls_protocol_signer::lightning_signer::policy::filter::PolicyFilter;
use vls_protocol_signer::lightning_signer::policy::simple_validator::{
    make_default_simple_policy, SimpleValidatorFactory,
};
use vls_protocol_signer::lightning_signer::signer::derive::KeyDerivationStyle;
use vls_protocol_signer::lightning_signer::signer::ClockStartingTimeFactory;
use vls_protocol_signer::lightning_signer::util::clock::StandardClock;

struct VlsTransportState {
    init_handler: Option<InitHandler>,
    root_handler: Option<RootHandler>,
    channel_handlers: HashMap<(u64, [u8; 33]), vls_protocol_signer::handler::ChannelHandler>,
    cached_hsmd_init2_reply: Option<Vec<u8>>,
}

/// In-process [`Transport`] over the VLS handlers, owning the VLS `Node` (which holds the seed).
pub(crate) struct InProcessVlsTransport {
    state: Mutex<VlsTransportState>,
    /// One past the highest channel dbid already in use when this transport was constructed (from a
    /// restored node's persisted channels, or `1` for a fresh node). A caller that allocates new
    /// channel dbids (`RealVlsClient`) must resume counting from here — see the field's use in
    /// [`crate::signer::remote::daemon`] — so a process restart never reissues a dbid already bound to
    /// an existing channel's revocable keys.
    initial_next_dbid: u64,
}

impl InProcessVlsTransport {
    /// Dev/test helper: no on-disk state, so every call starts a fresh, stateless VLS node — never
    /// restores channels from a prior run. Suitable for a signer that shares its process's lifetime
    /// with its caller (nothing to restore across a process restart because there is no restart
    /// independent of the caller). Production signers that can restart independently (the remote-signer
    /// daemon) must use [`Self::new`] with a disk-backed persister instead — see the field doc on
    /// [`Self::initial_next_dbid`] for why that matters.
    pub(crate) fn new_ephemeral(
        network: Network,
        seed: [u8; 32],
        permissive_policy: bool,
    ) -> anyhow::Result<Self> {
        let persister: Arc<dyn Persist> = Arc::new(DummyPersister {});
        Self::new(network, seed, permissive_policy, persister)
    }

    /// `persister` determines restart behavior: [`DummyPersister`] (dev/test, see [`Self::new_ephemeral`])
    /// starts a fresh, stateless `Node` every call; a disk-backed [`Persist`] restores the previously
    /// persisted `Node` (channels included) when one already exists at that store.
    pub(crate) fn new(
        network: Network,
        seed: [u8; 32],
        permissive_policy: bool,
        persister: Arc<dyn Persist>,
    ) -> anyhow::Result<Self> {
        let validator_factory: Arc<dyn lightning_signer::policy::validator::ValidatorFactory> =
            if permissive_policy {
                let mut policy = make_default_simple_policy(network);
                policy.filter = PolicyFilter::new_permissive();
                Arc::new(SimpleValidatorFactory::new_with_policy(policy))
            } else {
                Arc::new(SimpleValidatorFactory::new())
            };
        let services = NodeServices {
            validator_factory,
            starting_time_factory: ClockStartingTimeFactory::new(),
            persister: persister.clone(),
            clock: Arc::new(StandardClock()),
            trusted_oracle_pubkeys: vec![],
        };

        let node = Self::new_or_restore_node(network, &seed, services, persister.as_ref())?;
        let initial_next_dbid = node
            .chaninfo()
            .into_iter()
            .map(|slot| slot.oid)
            .max()
            .map_or(1, |max| max + 1);

        let approver: Arc<dyn vls_protocol_signer::approver::Approve> = if permissive_policy {
            Arc::new(WarningPositiveApprover())
        } else {
            Arc::new(vls_protocol_signer::approver::NegativeApprover())
        };
        let handler = InitHandler::new(1, node, approver, msgs::DEFAULT_MAX_PROTOCOL_VERSION);
        Ok(Self {
            state: Mutex::new(VlsTransportState {
                init_handler: Some(handler),
                root_handler: None,
                channel_handlers: HashMap::new(),
                cached_hsmd_init2_reply: None,
            }),
            initial_next_dbid,
        })
    }

    /// Load the previously persisted node (channels included) if `persister` already has one, else
    /// create a fresh node and persist its initial state.
    ///
    /// This mirrors `vls-protocol-signer`'s `HandlerBuilder::build`, but with our own `NodeConfig`
    /// (LDK key derivation, checkpoints off) instead of its checkpoint-enabled native-derivation
    /// default: checkpoint validation in vls-core can panic on missing checkpoint state in some
    /// environments, and this transport never feeds the node chain data anyway. `use_checkpoints`
    /// only affects fresh-node tracker construction (`Node::new`); the restore path always loads the
    /// already-persisted tracker directly, so it is unaffected by this override.
    fn new_or_restore_node(
        network: Network,
        seed: &[u8; 32],
        services: NodeServices,
        persister: &dyn Persist,
    ) -> anyhow::Result<Arc<Node>> {
        let nodes = persister
            .get_nodes()
            .map_err(|e| anyhow::anyhow!("load persisted nodes: {e}"))?;
        if nodes.is_empty() {
            let config = NodeConfig {
                network,
                key_derivation_style: KeyDerivationStyle::Ldk,
                use_checkpoints: false,
                allow_deep_reorgs: true,
            };
            let node = Arc::new(Node::new(config, seed, vec![], services));
            // Empty allowlist, but still required: `Persist::new_node` doesn't itself write the
            // allowlist table, and `Node::restore_node` unconditionally expects a row there.
            node.add_allowlist(&[])
                .map_err(|e| anyhow::anyhow!("initialize node allowlist: {e:?}"))?;
            persister
                .new_node(&node.get_id(), &config, &node.get_state())
                .map_err(|e| anyhow::anyhow!("persist new node: {e}"))?;
            persister
                .new_tracker(&node.get_id(), &node.get_tracker())
                .map_err(|e| anyhow::anyhow!("persist new tracker: {e}"))?;
            Ok(node)
        } else {
            anyhow::ensure!(
                nodes.len() == 1,
                "expected exactly one persisted node, found {}",
                nodes.len()
            );
            let (node_id, entry) = nodes.into_iter().next().expect("checked len == 1");
            Node::restore_node(&node_id, entry, seed, services)
                .map_err(|e| anyhow::anyhow!("restore persisted node: {e:?}"))
        }
    }

    /// One past the highest channel dbid known when this transport was constructed. See the field doc
    /// on [`Self::initial_next_dbid`].
    pub(crate) fn initial_next_dbid(&self) -> u64 {
        self.initial_next_dbid
    }

    /// Synthesize a pre-`SetupChannel` per-commitment point from the channel stub (LDK may request one
    /// before setup on inbound opens). `None` on any failure — callers treat that as "no fallback".
    pub(crate) fn synthesize_stub_commitment_point(&self, dbid: u64, idx: u64) -> Option<String> {
        let state = self.state.lock().ok()?;
        let node = state.root_handler.as_ref()?.node();
        let slot = node.chaninfo().into_iter().find(|slot| slot.oid == dbid)?;
        let slot_arc = node.get_channel(&slot.id).ok()?;
        let slot_guard = slot_arc.lock().ok()?;
        let secp = secp256k1_all();
        let point = match &*slot_guard {
            lightning_signer::channel::ChannelSlot::Stub(stub) => {
                stub.keys.get_per_commitment_point(idx, secp).ok()?
            }
            lightning_signer::channel::ChannelSlot::Ready(chan) => {
                chan.keys.get_per_commitment_point(idx, secp).ok()?
            }
        };
        Some(point.serialize().to_lower_hex_string())
    }

    /// Pre-setup `GetPerCommitmentPoint` may arrive before `SetupChannel` on an inbound open;
    /// synthesize it from the channel stub so those still work. `None` if `channel_keys_id_hex` is
    /// malformed or no matching stub/channel exists — callers treat that as "no fallback available".
    fn per_commitment_point_fallback(
        &self,
        channel_keys_id_hex: &str,
        idx: u64,
    ) -> Option<SignerResponse> {
        let dbid = dbid_from_channel_keys_id_hex(channel_keys_id_hex)?;
        let point_hex = self.synthesize_stub_commitment_point(dbid, idx)?;
        Some(SignerResponse::Channel(
            ChannelResponse::PerCommitmentPoint { point_hex },
        ))
    }
}

/// Build the complete in-process signer stack over `persister`: the [`InProcessVlsTransport`] plus
/// the [`ExternalSignerBackend`] wired through `RealVlsClient` with the transport's dbid high-water
/// mark. The single construction site shared by the remote-signer daemon and the uniffi in-process
/// signer — the `initial_next_dbid` threading is safety-critical (a copy that dropped it would
/// reissue dbids and hand a new channel an existing channel's revocable keys), so it must not be
/// hand-wired per caller.
pub(crate) fn build_backend(
    network: Network,
    seed: [u8; 32],
    permissive_policy: bool,
    persister: Arc<dyn Persist>,
) -> anyhow::Result<(Arc<dyn ExternalSignerBackend>, Arc<InProcessVlsTransport>)> {
    let transport = Arc::new(InProcessVlsTransport::new(
        network,
        seed,
        permissive_policy,
        persister,
    )?);
    let backend: Arc<dyn ExternalSignerBackend> = Arc::new(VlsSignerAdapter::new(
        RealVlsClient::new_with_network_seed_and_next_dbid(
            transport.clone(),
            network.to_string(),
            Some(seed),
            transport.initial_next_dbid(),
        ),
    ));
    Ok((backend, transport))
}

/// Open the disk-backed VLS store under `data_dir` with signer-state hygiene: the directory is
/// created 0700 (or, when pre-existing, verified owner-only — failing closed on broader
/// permissions), and the `redb` database files are tightened to 0600 afterwards, since `redb`
/// creates them at the process umask. The store carries channel signer state and the dbid
/// high-water mark that prevents key reuse after a restart — treat it like the seed. The single
/// construction site shared by the remote-signer daemon and the uniffi persistent signer.
pub(crate) fn open_restricted_persister(
    data_dir: &std::path::Path,
) -> anyhow::Result<Arc<dyn Persist>> {
    use vls_persist::kvv::redb::RedbKVVStore;
    use vls_persist::kvv::{JsonFormat, KVVPersister};

    super::key_source::create_or_check_restricted_dir(data_dir)
        .with_context(|| format!("VLS signer state dir {}", data_dir.display()))?;
    let store = RedbKVVStore::new(data_dir);
    // "redb" is the store's database file; "redb.db2" appears after a redb 1.x → 2.x migration.
    for name in ["redb", "redb.db2"] {
        super::key_source::restrict_existing_file(&data_dir.join(name))
            .with_context(|| format!("restrict VLS db file {}", data_dir.join(name).display()))?;
    }
    Ok(Arc::new(KVVPersister(store, JsonFormat)))
}

/// [`build_backend`] over a [`DummyPersister`]: no on-disk state — see
/// [`InProcessVlsTransport::new_ephemeral`] for when that is (and is not) appropriate.
pub(crate) fn build_backend_ephemeral(
    network: Network,
    seed: [u8; 32],
    permissive_policy: bool,
) -> anyhow::Result<(Arc<dyn ExternalSignerBackend>, Arc<InProcessVlsTransport>)> {
    build_backend(
        network,
        seed,
        permissive_policy,
        Arc::new(DummyPersister {}),
    )
}

/// Handle one RLN signer envelope (protobuf request bytes → response envelope bytes): decode →
/// backend call → (on a backend error) the pre-`SetupChannel` per-commitment-point fallback →
/// encode. The single envelope pipeline shared by the remote-signer daemon and the uniffi in-process
/// signer, so the fallback dispatch and error mapping cannot drift between the two.
pub(crate) fn handle_envelope(
    backend: &dyn ExternalSignerBackend,
    transport: &InProcessVlsTransport,
    request: &[u8],
) -> anyhow::Result<Vec<u8>> {
    let signer_request =
        crate::signer::proto::decode_signer_request(request).context("decode signer request")?;
    // Extracted up front (a few bytes) so the request itself can be passed to the backend by value
    // without cloning — RGB PSBT sign requests can be megabytes.
    let fallback_params = fallback_params(&signer_request);
    let signer_response = match backend.call(signer_request) {
        Ok(response) => response,
        Err(e) => fallback_params
            .and_then(|(channel_keys_id_hex, idx)| {
                transport.per_commitment_point_fallback(&channel_keys_id_hex, idx)
            })
            .inspect(|fallback| {
                tracing::debug!(?fallback, "external signer backend fallback response");
            })
            .ok_or_else(|| anyhow::anyhow!("backend call failed: {e:?}"))?,
    };
    crate::signer::proto::encode_signer_response(&signer_response).context("encode signer response")
}

/// The `(channel_keys_id_hex, idx)` a [`handle_envelope`] fallback would need — `Some` only for the
/// pre-setup `GetPerCommitmentPoint` op, the one request [`per_commitment_point_fallback`] can
/// answer.
///
/// [`per_commitment_point_fallback`]: InProcessVlsTransport::per_commitment_point_fallback
fn fallback_params(request: &SignerRequest) -> Option<(String, u64)> {
    let SignerRequest::Channel(ChannelRequest::Op {
        channel_keys_id_hex,
        op: ChannelOp::GetPerCommitmentPoint { idx },
    }) = request
    else {
        return None;
    };
    Some((channel_keys_id_hex.clone(), *idx))
}

/// Parse a 32-byte `channel_keys_id_hex` (dbid encoded big-endian in the first 8 bytes) back into the
/// dbid it was generated from. `None` on any malformed input.
fn dbid_from_channel_keys_id_hex(channel_keys_id_hex: &str) -> Option<u64> {
    let bytes = Vec::<u8>::from_hex(channel_keys_id_hex).ok()?;
    if bytes.len() != 32 {
        return None;
    }
    let mut dbid_bytes = [0u8; 8];
    dbid_bytes.copy_from_slice(&bytes[..8]);
    Some(u64::from_be_bytes(dbid_bytes))
}

/// Shared `secp256k1::All` context (the trait bound `get_per_commitment_point` requires — a
/// verification-only context isn't sufficient). Built once per process instead of per call; the
/// context's precomputed tables make `Secp256k1::new()` non-trivial to construct.
fn secp256k1_all() -> &'static Secp256k1<bitcoin::secp256k1::All> {
    static CTX: std::sync::OnceLock<Secp256k1<bitcoin::secp256k1::All>> =
        std::sync::OnceLock::new();
    CTX.get_or_init(Secp256k1::new)
}

impl Transport for InProcessVlsTransport {
    fn node_call(&self, message: Vec<u8>) -> Result<Vec<u8>, VlsClientError> {
        let msg = msgs::from_vec(message).map_err(VlsClientError::Protocol)?;
        let is_hsmd_init2 = matches!(msg, msgs::Message::HsmdInit2(_));
        let mut state = self.state.lock().map_err(|_| VlsClientError::Transport)?;

        if state.root_handler.is_none() {
            // Only init-phase messages may reach the `InitHandler`: vls-protocol-signer's handler
            // ends in `unimplemented!()` for anything else, which would panic while the state Mutex
            // is held and poison it — wedging every later call on every connection. A non-init
            // message here means the caller skipped the Bootstrap handshake (e.g. a node talking to
            // a restarted daemon without replaying it); reject the one call instead of taking the
            // whole signer down.
            if !matches!(
                msg,
                msgs::Message::Ping(_) | msgs::Message::HsmdInit(_) | msgs::Message::HsmdInit2(_)
            ) {
                return Err(VlsClientError::Transport);
            }
            let init = state
                .init_handler
                .as_mut()
                .ok_or(VlsClientError::Transport)?;
            let (done, reply_opt) = init.handle(msg).map_err(|_| VlsClientError::Transport)?;
            let reply = reply_opt.ok_or(VlsClientError::Transport)?;
            let reply_vec = reply.as_vec();
            if is_hsmd_init2 {
                state.cached_hsmd_init2_reply = Some(reply_vec.clone());
            }
            if done {
                let init_taken = state.init_handler.take().ok_or(VlsClientError::Transport)?;
                state.root_handler = Some(init_taken.into());
            }
            return Ok(reply_vec);
        }

        if is_hsmd_init2 {
            return state
                .cached_hsmd_init2_reply
                .clone()
                .ok_or(VlsClientError::Transport);
        }

        let root = state
            .root_handler
            .as_ref()
            .cloned()
            .ok_or(VlsClientError::Transport)?;
        // The handler work only needs the (cloned) root handler, not the transport state; release
        // the lock so concurrent node calls don't serialize behind it.
        drop(state);
        let reply = root.handle(msg).map_err(|_| VlsClientError::Transport)?;
        Ok(reply.as_vec())
    }

    fn call(
        &self,
        dbid: u64,
        peer_id: vls_protocol::model::PubKey,
        message: Vec<u8>,
    ) -> Result<Vec<u8>, VlsClientError> {
        let msg = msgs::from_vec(message).map_err(VlsClientError::Protocol)?;
        let mut state = self.state.lock().map_err(|_| VlsClientError::Transport)?;
        let root = state
            .root_handler
            .as_ref()
            .cloned()
            .ok_or(VlsClientError::Transport)?;

        if matches!(
            msg,
            msgs::Message::NewChannel(_) | msgs::Message::GetChannelBasepoints(_)
        ) {
            drop(state);
            let reply = root.handle(msg).map_err(|_| VlsClientError::Transport)?;
            return Ok(reply.as_vec());
        }

        let key = (dbid, peer_id.0);
        let handler = state
            .channel_handlers
            .entry(key)
            .or_insert_with(|| root.for_new_client(1, peer_id, dbid));
        let reply = handler.handle(msg).map_err(|_| VlsClientError::Transport)?;
        Ok(reply.as_vec())
    }
}
