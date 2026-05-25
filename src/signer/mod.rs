//! Lightning / RGB signer wiring for RLN.
//!
//! ## Phase D (attached external signer + RGB holder commitments)
//!
//! [`channel_signer::ExternalChannelSigner`] sets `commitment_unsigned_tx_hex` on
//! `ValidateHolderCommitment` when the built holder commitment tx is RGB-colored
//! ([`lightning::rgb_utils::is_tx_colored`]). The external process (`rln-external-signer`) then uses
//! the full-tx VLS message path (Phase C there) together with patched **`vls-core`**
//! (`rgb-commitment-compat`; see `vendor/vls-core-rgb/UTEXO-RGB-PATCH.md`).
//!
//! **In-process** `NativeExternalSigner` (see `uniffi_api/native_signer.rs`) still uses the LDK
//! summary-only VLS path for holder validate; it does not send the wire tx. End-to-end RGB + native
//! VLS is not supported until that path is aligned or flows use an attached external signer.
//!
//! ## Phase E (verification)
//!
//! After `rgb-lib` API updates, `cargo check` / `lib_sdk` external-signer tests exercise the wire
//! contract end-to-end; RGB holder validate with full tx is covered by the attached-signer path
//! above plus `rln-external-signer` tests with **`with-vls`**.
use bitcoin::psbt::Psbt;
use bitcoin::secp256k1::{All, Secp256k1};
use lightning::sign::ecdsa::EcdsaChannelSigner;
use lightning::sign::{NodeSigner, OutputSpender, SignerProvider, SpendableOutputDescriptor};
use std::sync::Arc;

pub(crate) mod channel_signer;
pub(crate) mod dyn_signer;
pub(crate) mod entropy;
pub(crate) mod external;
#[cfg(test)]
pub(crate) mod in_process_transport;
pub(crate) mod internal;
pub(crate) mod key_source;
pub(crate) mod proto;
pub(crate) mod transport;
pub(crate) mod types;
pub(crate) mod vls_adapter;

pub(crate) use dyn_signer::{DynRlnChannelSigner, DynRlnSigner};
pub(crate) use entropy::{LightningEntropySource, RlnEntropySource, SystemEntropySource};
pub(crate) use external::{ExternalSigner, ExternalSignerAttachment};
#[allow(unused_imports)]
pub(crate) use key_source::{
    read_key_source_file, validate_key_source_matches_bootstrap, write_key_source_file,
    KeySourceFile,
};
pub(crate) use transport::ExternalSignerTransport;
#[allow(unused_imports)]
pub(crate) use types::{
    validate_bootstrap_payload, BootstrapData, RlnSignerError, SignerIdentity,
    SUPPORTED_SIGNER_API_LEVEL,
};

/// Active signer type used by the current runtime wiring (internal mnemonic mode).
/// Dynamic wrapper supports both internal and external implementations.
pub(crate) type ActiveSigner = DynRlnSigner;
pub(crate) type ActiveSignerRef = Arc<ActiveSigner>;

pub(crate) trait RlnKeysInterface:
    NodeSigner + SignerProvider + OutputSpender + Send + Sync
{
    fn sign_spendable_outputs_psbt(
        &self,
        descriptors: &[&SpendableOutputDescriptor],
        psbt: Psbt,
        secp_ctx: &Secp256k1<All>,
    ) -> Result<Psbt, ()>;

    fn sign_rgb_psbt(
        &self,
        descriptors: Vec<String>,
        psbt: String,
    ) -> Result<String, RlnSignerError>;
}

pub(crate) trait RlnChannelSigner: EcdsaChannelSigner + Send + Sync {}
