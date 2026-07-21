//! In-process VLS signer exposed over UniFFI (`NativeExternalSigner`).
//!
//! Holder commitment validation uses `commitment_unsigned_tx_hex` when the built commitment tx is
//! RGB-colored (`ExternalChannelSigner::validate_holder_commitment_with_backend`). Counterparty
//! commitment signing uses the VLS summary RPC (`SignRemoteCommitmentTx2`) like vanilla channels;
//! the wire transaction may differ on RGB outputs while balances match the negotiated commitment.
use super::{ExternalSignerHost, RlnError, SdkExternalSignerBootstrap};
use crate::signer::in_process_vls::{self, InProcessVlsTransport};
use bitcoin::hex::FromHex;
use bitcoin::Network;
use rand::rngs::OsRng;
use rand::RngCore;
use signer_external::contract::{BootstrapData, ExternalSignerBackend, SignerRequest};
use std::sync::Arc;

#[derive(uniffi::Object)]
pub struct NativeExternalSigner {
    backend: Arc<dyn ExternalSignerBackend>,
    transport: Arc<InProcessVlsTransport>,
}

impl NativeExternalSigner {
    fn parse_network(network: &str) -> Result<Network, RlnError> {
        match network.to_lowercase().as_str() {
            "mainnet" | "bitcoin" => Ok(Network::Bitcoin),
            "testnet" | "testnet4" => Ok(Network::Testnet),
            "signet" => Ok(Network::Signet),
            "regtest" => Ok(Network::Regtest),
            _ => Err(RlnError::InvalidRequest(format!(
                "invalid network: {network}"
            ))),
        }
    }

    fn parse_seed_hex(seed_hex: &str) -> Result<[u8; 32], RlnError> {
        let seed_vec = Vec::<u8>::from_hex(seed_hex)
            .map_err(|e| RlnError::InvalidRequest(format!("invalid seed hex: {e}")))?;
        let seed: [u8; 32] = seed_vec
            .try_into()
            .map_err(|_| RlnError::InvalidRequest("seed must be 32 bytes".to_string()))?;
        Ok(seed)
    }

    fn random_seed() -> [u8; 32] {
        let mut seed = [0u8; 32];
        OsRng.fill_bytes(&mut seed);
        seed
    }

    /// Resolve the host's `permissive_policy` request. Strict is the default (`None` → `false`):
    /// a host must opt in to `PolicyFilter::new_permissive()` knowingly, and never on mainnet —
    /// same rule the remote signer daemon enforces for its `--permissive` flag.
    fn resolve_permissive_policy(
        network: Network,
        permissive_policy: Option<bool>,
    ) -> Result<bool, RlnError> {
        let permissive = permissive_policy.unwrap_or(false);
        if permissive && network == Network::Bitcoin {
            tracing::error!("permissive VLS policy is not allowed on mainnet");
            return Err(RlnError::InvalidRequest(
                "permissive VLS policy is not allowed on mainnet".to_string(),
            ));
        }
        Ok(permissive)
    }

    fn map_bootstrap(data: BootstrapData) -> SdkExternalSignerBootstrap {
        SdkExternalSignerBootstrap {
            node_id: data.identity.node_id,
            account_xpub_vanilla: data.identity.account_xpub_vanilla,
            account_xpub_colored: data.identity.account_xpub_colored,
            master_fingerprint: data.identity.master_fingerprint,
            protocol_version: data.protocol_version,
            api_level: data.api_level,
        }
    }
}

#[uniffi::export]
impl NativeExternalSigner {
    #[uniffi::constructor]
    pub fn new(
        seed_hex: String,
        network: String,
        permissive_policy: Option<bool>,
    ) -> Result<Arc<Self>, RlnError> {
        let network = Self::parse_network(&network)?;
        let permissive = Self::resolve_permissive_policy(network, permissive_policy)?;
        // Host must supply a stable 32-byte seed (e.g. loaded from Android Keystore / iOS Keychain)
        // and pass it in-memory; this signer helper does not persist secrets.
        let seed = Self::parse_seed_hex(&seed_hex)?;
        let (backend, transport) =
            in_process_vls::build_backend_ephemeral(network, seed, permissive).map_err(|e| {
                tracing::error!(error = ?e, "native signer transport init failed");
                RlnError::internal(format!("native signer transport init failed: {e}"))
            })?;
        Ok(Arc::new(Self { backend, transport }))
    }

    /// Like [`Self::new`], but with a disk-backed VLS store under `storage_dir_path`, so a
    /// process restart restores the signer's channel state (channels, commitment counters, dbid
    /// high-water mark) instead of starting over.
    ///
    /// The ephemeral [`Self::new`] signer loses all VLS channel state on restart: it can
    /// re-derive channel keys from the seed, but a stateful validating signer cannot validate
    /// commitment state it never tracked, so payments over channels restored from LDK
    /// persistence fail (`Failed to validate our commitment` → channel force-close). Hosts that
    /// keep channels across process restarts (the "device restarts and unlocks again" flow)
    /// must use this constructor with a stable directory. Same disk layout as the remote
    /// signer daemon (`redb` KVV store).
    #[uniffi::constructor]
    pub fn new_with_storage(
        seed_hex: String,
        network: String,
        permissive_policy: Option<bool>,
        storage_dir_path: String,
    ) -> Result<Arc<Self>, RlnError> {
        let network = Self::parse_network(&network)?;
        let permissive = Self::resolve_permissive_policy(network, permissive_policy)?;
        let seed = Self::parse_seed_hex(&seed_hex)?;
        // Creates/verifies the store directory owner-only (0700) and tightens the redb files to
        // 0600 — the store carries channel signer state and the dbid high-water mark.
        let persister =
            in_process_vls::open_restricted_persister(std::path::Path::new(&storage_dir_path))
                .map_err(|e| {
                    tracing::error!(error = ?e, "native signer VLS store init failed");
                    RlnError::internal(format!("native signer VLS store init failed: {e}"))
                })?;
        let (backend, transport) =
            in_process_vls::build_backend(network, seed, permissive, persister).map_err(|e| {
                tracing::error!(error = ?e, "native signer persistent transport init failed");
                RlnError::internal(format!(
                    "native signer persistent transport init failed: {e}"
                ))
            })?;
        Ok(Arc::new(Self { backend, transport }))
    }

    pub fn bootstrap(&self) -> Result<SdkExternalSignerBootstrap, RlnError> {
        let bootstrap = match self.backend.call(SignerRequest::Bootstrap).map_err(|e| {
            tracing::error!(error = ?e, "native external signer bootstrap failed");
            RlnError::internal(format!("native external signer bootstrap failed: {e}"))
        })? {
            signer_external::contract::SignerResponse::Bootstrap(data) => data,
            other => {
                tracing::error!(response = ?other, "native external signer returned non-bootstrap response");
                return Err(RlnError::internal(
                    "native external signer returned non-bootstrap response",
                ));
            }
        };
        Ok(Self::map_bootstrap(bootstrap))
    }
}

impl ExternalSignerHost for NativeExternalSigner {
    fn call(&self, request: Vec<u8>) -> Result<Vec<u8>, RlnError> {
        in_process_vls::handle_envelope(self.backend.as_ref(), &self.transport, &request).map_err(
            |e| {
                tracing::error!(error = ?e, "native external signer envelope failed");
                RlnError::internal(format!("native external signer envelope failed: {e}"))
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The production/persistent native signer must never run VLS with
    /// `PolicyFilter::new_permissive()` on mainnet — the same rule the remote signer daemon
    /// enforces for its `--permissive` flag.
    #[test]
    fn native_persistent_signer_rejects_mainnet_permissive() {
        let dir = tempfile::tempdir().unwrap();
        let res = NativeExternalSigner::new_with_storage(
            "11".repeat(32),
            "bitcoin".to_string(),
            Some(true),
            dir.path().display().to_string(),
        );
        assert!(
            res.is_err(),
            "mainnet permissive VLS policy must not be allowed"
        );
    }

    #[test]
    fn native_ephemeral_signer_rejects_mainnet_permissive() {
        let res = NativeExternalSigner::new("11".repeat(32), "bitcoin".to_string(), Some(true));
        assert!(
            res.is_err(),
            "mainnet permissive VLS policy must not be allowed"
        );
    }

    /// A host that passes `None` gets the strict policy — permissive requires an explicit opt-in.
    #[test]
    fn permissive_policy_defaults_to_strict() {
        for network in [Network::Bitcoin, Network::Regtest] {
            assert!(
                !NativeExternalSigner::resolve_permissive_policy(network, None)
                    .expect("strict default is always allowed"),
                "None must resolve to the strict policy on {network}"
            );
        }
        assert!(
            NativeExternalSigner::resolve_permissive_policy(Network::Regtest, Some(true))
                .expect("explicit permissive is allowed off-mainnet")
        );
    }
}
