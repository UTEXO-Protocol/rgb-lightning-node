//! In-process VLS signer exposed over UniFFI (`NativeExternalSigner`).
//!
//! Holder commitment validation uses `commitment_unsigned_tx_hex` when the built commitment tx is
//! RGB-colored (`ExternalChannelSigner::validate_holder_commitment_with_backend`). Counterparty
//! commitment signing uses the VLS summary RPC (`SignRemoteCommitmentTx2`) like vanilla channels;
//! the wire transaction may differ on RGB outputs while balances match the negotiated commitment.
use super::{ExternalSignerHost, RlnError, SdkExternalSignerBootstrap};
use crate::signer::in_process_vls::InProcessVlsTransport;
use crate::signer::proto::{decode_signer_request, encode_signer_response};
use anyhow::Context;
use bitcoin::hex::FromHex;
use bitcoin::Network;
use rand::rngs::OsRng;
use rand::RngCore;
use signer_external::contract::{BootstrapData, ExternalSignerBackend, SignerRequest};
use signer_external::vls_adapter::vls_real::RealVlsClient;
use signer_external::vls_adapter::VlsSignerAdapter;
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
            _ => Err(RlnError::InvalidRequest),
        }
    }

    fn parse_seed_hex(seed_hex: &str) -> Result<[u8; 32], RlnError> {
        let seed_vec = Vec::<u8>::from_hex(seed_hex).map_err(|_| RlnError::InvalidRequest)?;
        let seed: [u8; 32] = seed_vec.try_into().map_err(|_| RlnError::InvalidRequest)?;
        Ok(seed)
    }

    fn random_seed() -> [u8; 32] {
        let mut seed = [0u8; 32];
        OsRng.fill_bytes(&mut seed);
        seed
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
        // Host must supply a stable 32-byte seed (e.g. loaded from Android Keystore / iOS Keychain)
        // and pass it in-memory; this signer helper does not persist secrets.
        let seed = Self::parse_seed_hex(&seed_hex)?;
        let transport = Arc::new(
            InProcessVlsTransport::new_ephemeral(network, seed, permissive_policy.unwrap_or(true))
                .context("native signer transport init failed")
                .map_err(|_| RlnError::Internal)?,
        );
        let backend: Arc<dyn ExternalSignerBackend> = Arc::new(VlsSignerAdapter::new(
            RealVlsClient::new_with_network_and_seed(
                transport.clone(),
                network.to_string(),
                Some(seed),
            ),
        ));
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
        use lightning_signer::persist::Persist;
        use vls_persist::kvv::redb::RedbKVVStore;
        use vls_persist::kvv::{JsonFormat, KVVPersister};

        let network = Self::parse_network(&network)?;
        let seed = Self::parse_seed_hex(&seed_hex)?;
        std::fs::create_dir_all(&storage_dir_path).map_err(|_| RlnError::Internal)?;
        let persister: Arc<dyn Persist> =
            Arc::new(KVVPersister(RedbKVVStore::new(&storage_dir_path), JsonFormat));
        let transport = Arc::new(
            InProcessVlsTransport::new(network, seed, permissive_policy.unwrap_or(true), persister)
                .context("native signer persistent transport init failed")
                .map_err(|_| RlnError::Internal)?,
        );
        let backend: Arc<dyn ExternalSignerBackend> = Arc::new(VlsSignerAdapter::new(
            RealVlsClient::new_with_network_seed_and_next_dbid(
                transport.clone(),
                network.to_string(),
                Some(seed),
                transport.initial_next_dbid(),
            ),
        ));
        Ok(Arc::new(Self { backend, transport }))
    }

    pub fn bootstrap(&self) -> Result<SdkExternalSignerBootstrap, RlnError> {
        let bootstrap = match self.backend.call(SignerRequest::Bootstrap).map_err(|e| {
            tracing::error!(error = ?e, "native external signer bootstrap failed");
            RlnError::Internal
        })? {
            signer_external::contract::SignerResponse::Bootstrap(data) => data,
            other => {
                tracing::error!(response = ?other, "native external signer returned non-bootstrap response");
                return Err(RlnError::Internal);
            }
        };
        Ok(Self::map_bootstrap(bootstrap))
    }
}

impl ExternalSignerHost for NativeExternalSigner {
    fn call(&self, request: Vec<u8>) -> Result<Vec<u8>, RlnError> {
        let signer_request: SignerRequest = decode_signer_request(&request).map_err(|e| {
            tracing::error!(error = ?e, "native external signer protobuf decode failed");
            RlnError::Internal
        })?;
        let signer_response = match self.backend.call(signer_request.clone()) {
            Ok(response) => response,
            Err(e) => match self.transport.fallback_for(&signer_request) {
                Some(fallback) => {
                    tracing::debug!(
                        ?fallback,
                        "native external signer backend fallback response"
                    );
                    fallback
                }
                None => {
                    tracing::error!(error = ?e, "native external signer backend call failed");
                    return Err(RlnError::Internal);
                }
            },
        };
        encode_signer_response(&signer_response).map_err(|e| {
            tracing::error!(error = %e, "native external signer response encode failed");
            RlnError::Internal
        })
    }
}
