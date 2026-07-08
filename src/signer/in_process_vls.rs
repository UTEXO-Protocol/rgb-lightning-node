//! In-process VLS transport: drives the VLS handlers (`InitHandler` → `RootHandler` → `ChannelHandler`)
//! directly, with no network below it. Shared by the uniffi in-process signer
//! (`uniffi_api::native_signer`) and the remote-signer daemon (`signer::remote::daemon`) so the two do
//! not each carry a copy.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use bitcoin::hex::{DisplayHex, FromHex};
use bitcoin::secp256k1::Secp256k1;
use bitcoin::Network;
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
    /// synthesize it from the channel stub so those still work. Shared by the uniffi in-process signer
    /// and the remote-signer daemon (both need the identical dbid-parse + response-shape). `None` if
    /// `request` isn't a `GetPerCommitmentPoint` op, `channel_keys_id_hex` is malformed, or no
    /// matching stub/channel exists — callers treat that as "no fallback available".
    pub(crate) fn fallback_for(
        &self,
        request: &signer_external::contract::SignerRequest,
    ) -> Option<signer_external::contract::SignerResponse> {
        use signer_external::contract::{
            ChannelOp, ChannelRequest, ChannelResponse, SignerRequest,
        };

        let SignerRequest::Channel(ChannelRequest::Op {
            channel_keys_id_hex,
            op: ChannelOp::GetPerCommitmentPoint { idx },
        }) = request
        else {
            return None;
        };
        let dbid = dbid_from_channel_keys_id_hex(channel_keys_id_hex)?;
        let point_hex = self.synthesize_stub_commitment_point(dbid, *idx)?;
        Some(signer_external::contract::SignerResponse::Channel(
            ChannelResponse::PerCommitmentPoint { point_hex },
        ))
    }
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
        let msg_name = msgs::message_name_from_vec(&message);
        let msg = msgs::from_vec(message).map_err(VlsClientError::Protocol)?;
        let mut state = self.state.lock().map_err(|_| VlsClientError::Transport)?;

        if state.root_handler.is_none() {
            let init = state
                .init_handler
                .as_mut()
                .ok_or(VlsClientError::Transport)?;
            let (done, reply_opt) = init.handle(msg).map_err(|_| VlsClientError::Transport)?;
            let reply = reply_opt.ok_or(VlsClientError::Transport)?;
            let reply_vec = reply.as_vec();
            if msg_name == "HsmdInit2" {
                state.cached_hsmd_init2_reply = Some(reply_vec.clone());
            }
            if done {
                let init_taken = state.init_handler.take().ok_or(VlsClientError::Transport)?;
                state.root_handler = Some(init_taken.into());
            }
            return Ok(reply_vec);
        }

        if msg_name == "HsmdInit2" {
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
        let reply = root.handle(msg).map_err(|_| VlsClientError::Transport)?;
        Ok(reply.as_vec())
    }

    fn call(
        &self,
        dbid: u64,
        peer_id: vls_protocol::model::PubKey,
        message: Vec<u8>,
    ) -> Result<Vec<u8>, VlsClientError> {
        let msg_name = msgs::message_name_from_vec(&message);
        let msg = msgs::from_vec(message).map_err(VlsClientError::Protocol)?;
        let mut state = self.state.lock().map_err(|_| VlsClientError::Transport)?;
        let root = state
            .root_handler
            .as_ref()
            .cloned()
            .ok_or(VlsClientError::Transport)?;

        if matches!(msg_name.as_str(), "NewChannel" | "GetChannelBasepoints") {
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
