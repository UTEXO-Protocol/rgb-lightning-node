//! Client-side **async payments with LSP** logic for the WASM SDK.
//!
//! This is a focused port of the native node's `src/async_order.rs` + `src/apay_merkle.rs`
//! covering the *client* (recipient) half of the flow exercised by `/apay/new`
//! (`apay_new` / `apay_new_with_address` in the uniffi SDK):
//!
//! 1. derive a deterministic batch of payment hashes from the node seed,
//! 2. sign a Merkle batch commitment (and optional Lightning-Address attestation) with the
//!    node key so the invoice-host/LSP can attribute the hashes to this node,
//! 3. hand the signed params to the peer-wire handler ([`crate::rgb_ln_wire`]) which ships them
//!    to the host as an `async_order.new` JSON-RPC request over custom message
//!    [`ASYNC_ORDER_MESSAGE_TYPE_ID`] and awaits the host's response,
//! 4. persist the host-advertised `next_index_expected` so subsequent batches don't reuse hashes.
//!
//! The on-wire bytes, tags and derivation constants are kept byte-for-byte identical to the
//! native implementation so a WASM node and a native node register interchangeably with the
//! same LSP.

use lightning::bitcoin::bip32::{ChildNumber, Xpriv};
use lightning::bitcoin::hashes::{sha256, Hash as _};
use lightning::bitcoin::secp256k1::{PublicKey, Secp256k1};
use lightning::bitcoin::Network;
use lightning::types::payment::{PaymentHash, PaymentPreimage};
use lightning::util::persist::KVStoreSync;
use serde::{Deserialize, Serialize};

use std::cell::Cell;

pub(crate) const ASYNC_ORDER_MESSAGE_TYPE_ID: u16 = 37915;
pub(crate) const ASYNC_ORDER_MAX_HASH_BATCH_SIZE: usize = 200;
pub(crate) const ASYNC_ORDER_RESPONSE_TIMEOUT_MS: u32 = 30_000;

const PROTOCOL_VERSION: u64 = 1;
const ASYNC_ORDER_FIRST_HASH_INDEX: u64 = 1;
const ASYNC_PAYMENTS_ACCOUNT_INDEX: u32 = 0;
const ASYNC_PAYMENTS_BIP32_MAX_CHILD_INDEX: u32 = 0x7fff_ffff;
const ASYNC_PAYMENTS_PREIMAGE_DOMAIN: &[u8] = b"async-payments/v0";
const ASYNC_PAYMENTS_PURPOSE_APAY_INDEX: u32 = 0x4150_4159;
const ASYNC_PAYMENTS_KV_NAMESPACE: &str = "async_payments";
const ASYNC_PAYMENTS_NEXT_INDEX_KV_NAMESPACE: &str = "next_hash_index";

pub(crate) const APAY_BATCH_EXPIRY_SECS: u64 = 365 * 24 * 60 * 60;
const APAY_BATCH_ID_TAG: &[u8] = b"UTEXO_APAY_BATCH_ID_V1";
const APAY_HASH_BATCH_TAG: &[u8] = b"UTEXO_APAY_HASH_BATCH_V1";
const APAY_LNADDR_TAG: &[u8] = b"UTEXO_APAY_LNADDR_V1";
const APAY_HASH_LEAF_TAG: &[u8] = b"UTEXO_APAY_HASH_V1";
const MERKLE_LEAF_PREFIX: u8 = 0x00;
const MERKLE_NODE_PREFIX: u8 = 0x01;

/// JSON-RPC error surfaced by the host (or generated locally on validation failure).
#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct JsonRpcErrorWire {
    pub(crate) code: i64,
    pub(crate) message: String,
}

impl JsonRpcErrorWire {
    pub(crate) fn internal_error(message: impl Into<String>) -> Self {
        Self {
            code: -32603,
            message: message.into(),
        }
    }

    pub(crate) fn invalid_params(message: impl Into<String>) -> Self {
        Self {
            code: -32602,
            message: message.into(),
        }
    }

    pub(crate) fn application_error(code: i64, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    fn invalid_hash_batch() -> Self {
        Self {
            code: 1003,
            message: "invalid_hash_batch".to_owned(),
        }
    }
}

impl std::fmt::Display for JsonRpcErrorWire {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "async_order error {}: {}", self.code, self.message)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AsyncOrderNewHashWire {
    pub hash_index: u64,
    pub payment_hash: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AsyncOrderNewParamsWire {
    pub(crate) protocol_version: u64,
    pub(crate) hashes: Vec<AsyncOrderNewHashWire>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) batch: Option<ApayBatchCommitmentWire>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) address_sig: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct ApayBatchCommitmentWire {
    pub(crate) host_pubkey: String,
    pub(crate) batch_id: String,
    pub(crate) batch_root: String,
    pub(crate) batch_size: u64,
    pub(crate) batch_sig: String,
    pub(crate) created_at: u64,
    pub(crate) expires_at: u64,
}

/// Host response to `async_order.new`.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct AsyncOrderNewResultWire {
    pub(crate) protocol_version: u64,
    pub(crate) order_id: String,
    pub(crate) status: String,
    pub(crate) accepted_through_index: u64,
    pub(crate) next_index_expected: u64,
    pub(crate) unused_hashes: u64,
    pub(crate) refill_batch_size: u64,
}

/// Host → recipient `async_order.request_invoice` params: the invoice-host asks this node (the
/// recipient/payee) to mint an invoice for a previously-registered hash so an inbound async
/// payment can be forwarded and settled.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AsyncOrderRequestInvoiceParamsWire {
    pub(crate) hash_index: String,
    pub(crate) payment_hash: String,
    pub(crate) amount_msat: u64,
    #[serde(default)]
    pub(crate) asset_id: Option<String>,
    #[serde(default)]
    pub(crate) asset_amount: Option<u64>,
    pub(crate) description_hash: String,
    pub(crate) invoice_expiry_sec: u32,
    pub(crate) min_final_cltv_expiry_delta: u16,
}

/// Recipient → host `async_order.request_invoice` result.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct AsyncOrderOutboundInvoiceResultWire {
    pub(crate) payment_hash: String,
    pub(crate) bolt11: String,
}

/// Result returned up to JS from `apayNew` / `apayNewWithAddress`.
#[derive(Clone, Debug, Serialize)]
pub struct AsyncOrderNewResponse {
    pub request_id: String,
    pub host_node_id: String,
    pub protocol_version: u64,
    pub order_id: String,
    pub status: String,
    pub accepted_through_index: u64,
    pub next_index_expected: u64,
    pub unused_hashes: u64,
    pub refill_batch_size: u64,
    pub first_hash_index: u64,
    pub last_hash_index: u64,
    pub hashes: Vec<AsyncOrderNewHashWire>,
}

// ---------------------------------------------------------------------------
// small hex / hashing helpers (kept self-contained to match native byte layout)
// ---------------------------------------------------------------------------

pub(crate) fn hex_str(bytes: &[u8]) -> String {
    hex::encode(bytes)
}

fn validate_and_parse_payment_hash(value: &str) -> Result<PaymentHash, JsonRpcErrorWire> {
    let bytes = hex::decode(value.trim())
        .map_err(|_| JsonRpcErrorWire::invalid_params("invalid_payment_hash"))?;
    let arr: [u8; 32] = bytes
        .try_into()
        .map_err(|_| JsonRpcErrorWire::invalid_params("invalid_payment_hash"))?;
    Ok(PaymentHash(arr))
}

fn apay_decode_hex_fixed<const N: usize>(
    field: &str,
    s: &str,
) -> Result<[u8; N], JsonRpcErrorWire> {
    let bytes = hex::decode(s.trim())
        .map_err(|_| JsonRpcErrorWire::invalid_params(format!("{field} must be {N}-byte hex")))?;
    bytes
        .try_into()
        .map_err(|_| JsonRpcErrorWire::invalid_params(format!("{field} must be {N}-byte hex")))
}

fn digest(data: &[u8]) -> [u8; 32] {
    sha256::Hash::hash(data).to_byte_array()
}

// ---------------------------------------------------------------------------
// Merkle tree over the hash batch (mirrors src/apay_merkle.rs)
// ---------------------------------------------------------------------------

fn leaf_hash(
    recipient_pubkey: &[u8; 33],
    batch_id: &[u8; 16],
    hash_index: u64,
    payment_hash: &[u8; 32],
) -> [u8; 32] {
    let mut data = Vec::with_capacity(1 + APAY_HASH_LEAF_TAG.len() + 33 + 16 + 8 + 32);
    data.push(MERKLE_LEAF_PREFIX);
    data.extend_from_slice(APAY_HASH_LEAF_TAG);
    data.extend_from_slice(recipient_pubkey);
    data.extend_from_slice(batch_id);
    data.extend_from_slice(&hash_index.to_be_bytes());
    data.extend_from_slice(payment_hash);
    digest(&data)
}

fn node_hash(left: &[u8; 32], right: &[u8; 32]) -> [u8; 32] {
    let mut data = Vec::with_capacity(1 + 32 + 32);
    data.push(MERKLE_NODE_PREFIX);
    data.extend_from_slice(left);
    data.extend_from_slice(right);
    digest(&data)
}

fn next_level(level: &[[u8; 32]]) -> Vec<[u8; 32]> {
    let mut next = Vec::with_capacity(level.len().div_ceil(2));
    let mut i = 0;
    while i < level.len() {
        let left = &level[i];
        let right = if i + 1 < level.len() {
            &level[i + 1]
        } else {
            &level[i]
        };
        next.push(node_hash(left, right));
        i += 2;
    }
    next
}

fn merkle_root(leaves: &[[u8; 32]]) -> [u8; 32] {
    debug_assert!(!leaves.is_empty(), "merkle root over empty leaf set");
    let mut level = leaves.to_vec();
    while level.len() > 1 {
        level = next_level(&level);
    }
    level[0]
}

// ---------------------------------------------------------------------------
// batch id / commitment / attestation byte layouts (mirror src/async_order.rs)
// ---------------------------------------------------------------------------

fn apay_derive_batch_id(recipient_pubkey: &[u8; 33], start_index: u64) -> [u8; 16] {
    let mut material = Vec::with_capacity(APAY_BATCH_ID_TAG.len() + recipient_pubkey.len() + 8);
    material.extend_from_slice(APAY_BATCH_ID_TAG);
    material.extend_from_slice(recipient_pubkey);
    material.extend_from_slice(&start_index.to_be_bytes());
    let digest = digest(&material);
    let mut id = [0u8; 16];
    id.copy_from_slice(&digest[..16]);
    id
}

fn apay_batch_root(
    recipient_pubkey: &[u8; 33],
    batch_id: &[u8; 16],
    hashes: &[AsyncOrderNewHashWire],
) -> Result<[u8; 32], JsonRpcErrorWire> {
    if hashes.is_empty() {
        return Err(JsonRpcErrorWire::invalid_hash_batch());
    }
    let mut leaves = Vec::with_capacity(hashes.len());
    for entry in hashes {
        let payment_hash = validate_and_parse_payment_hash(&entry.payment_hash)?;
        leaves.push(leaf_hash(
            recipient_pubkey,
            batch_id,
            entry.hash_index,
            &payment_hash.0,
        ));
    }
    Ok(merkle_root(&leaves))
}

fn apay_commit_bytes(
    recipient_pubkey: &[u8; 33],
    host_pubkey: &[u8; 33],
    batch_id: &[u8; 16],
    batch_root: &[u8; 32],
    batch_size: u64,
    created_at: u64,
    expires_at: u64,
) -> Vec<u8> {
    let mut data = Vec::with_capacity(APAY_HASH_BATCH_TAG.len() + 33 + 33 + 16 + 32 + 24);
    data.extend_from_slice(APAY_HASH_BATCH_TAG);
    data.extend_from_slice(recipient_pubkey);
    data.extend_from_slice(host_pubkey);
    data.extend_from_slice(batch_id);
    data.extend_from_slice(batch_root);
    data.extend_from_slice(&batch_size.to_be_bytes());
    data.extend_from_slice(&created_at.to_be_bytes());
    data.extend_from_slice(&expires_at.to_be_bytes());
    data
}

fn apay_attest_bytes(
    recipient_pubkey: &[u8; 33],
    domain: &str,
    username: &str,
    expires_at: u64,
) -> Vec<u8> {
    let mut data =
        Vec::with_capacity(APAY_LNADDR_TAG.len() + 33 + domain.len() + username.len() + 8);
    data.extend_from_slice(APAY_LNADDR_TAG);
    data.extend_from_slice(recipient_pubkey);
    data.extend_from_slice(domain.as_bytes());
    data.extend_from_slice(username.as_bytes());
    data.extend_from_slice(&expires_at.to_be_bytes());
    data
}

/// Build the signed Merkle batch commitment. `sign` signs raw bytes with the node key and
/// returns the LDK zbase32 signed-message string.
pub(crate) fn build_apay_batch_commitment<F>(
    recipient_pubkey_hex: &str,
    host_pubkey_hex: &str,
    start_index: u64,
    hashes: &[AsyncOrderNewHashWire],
    created_at: u64,
    expires_at: u64,
    sign: F,
) -> Result<ApayBatchCommitmentWire, JsonRpcErrorWire>
where
    F: FnOnce(&[u8]) -> Result<String, ()>,
{
    let recipient_pubkey = apay_decode_hex_fixed::<33>("recipient_pubkey", recipient_pubkey_hex)?;
    let host_pubkey = apay_decode_hex_fixed::<33>("host_pubkey", host_pubkey_hex)?;
    let batch_id = apay_derive_batch_id(&recipient_pubkey, start_index);
    let batch_root = apay_batch_root(&recipient_pubkey, &batch_id, hashes)?;
    let commit = apay_commit_bytes(
        &recipient_pubkey,
        &host_pubkey,
        &batch_id,
        &batch_root,
        hashes.len() as u64,
        created_at,
        expires_at,
    );
    let batch_sig =
        sign(&commit).map_err(|_| JsonRpcErrorWire::internal_error("apay_batch_sign_failed"))?;
    Ok(ApayBatchCommitmentWire {
        host_pubkey: hex_str(&host_pubkey),
        batch_id: hex_str(&batch_id),
        batch_root: hex_str(&batch_root),
        batch_size: hashes.len() as u64,
        batch_sig,
        created_at,
        expires_at,
    })
}

/// Build the Lightning-Address attestation signature binding `username@domain` to this node.
pub(crate) fn build_apay_address_attestation<F>(
    recipient_pubkey_hex: &str,
    domain: &str,
    username: &str,
    expires_at: u64,
    sign: F,
) -> Result<String, JsonRpcErrorWire>
where
    F: FnOnce(&[u8]) -> Result<String, ()>,
{
    let recipient_pubkey = apay_decode_hex_fixed::<33>("recipient_pubkey", recipient_pubkey_hex)?;
    let attest = apay_attest_bytes(&recipient_pubkey, domain, username, expires_at);
    sign(&attest).map_err(|_| JsonRpcErrorWire::internal_error("apay_attest_sign_failed"))
}

// ---------------------------------------------------------------------------
// deterministic preimage/hash batch derivation (mirror AsyncPaymentsPreimageRoot)
// ---------------------------------------------------------------------------

/// BIP32 root used to deterministically derive async-payment preimages and hashes.
#[derive(Clone)]
pub(crate) struct AsyncPaymentsPreimageRoot {
    account_xprv: Xpriv,
}

impl AsyncPaymentsPreimageRoot {
    pub(crate) fn build_from_seed(
        seed: &[u8; 32],
        network: Network,
        this_node_pubkey: &PublicKey,
    ) -> Result<Self, JsonRpcErrorWire> {
        let mut account_xprv = Xpriv::new_master(network, seed).map_err(|err| {
            JsonRpcErrorWire::internal_error(format!("async_payment_root_derivation_failed: {err}"))
        })?;

        let h31 = u32::from_be_bytes(
            digest(&this_node_pubkey.serialize())[0..4]
                .try_into()
                .expect("sha256 hash is 32 bytes"),
        ) & ASYNC_PAYMENTS_BIP32_MAX_CHILD_INDEX;

        let path = [
            ASYNC_PAYMENTS_PURPOSE_APAY_INDEX,
            ASYNC_PAYMENTS_ACCOUNT_INDEX,
            h31,
        ];
        for index in path {
            account_xprv = derive_hardened_child(&account_xprv, index)?;
        }

        Ok(Self { account_xprv })
    }

    pub(crate) fn derive_hash_material(
        &self,
        hash_index: u64,
    ) -> Result<(PaymentPreimage, PaymentHash), JsonRpcErrorWire> {
        if hash_index < ASYNC_ORDER_FIRST_HASH_INDEX {
            return Err(JsonRpcErrorWire::invalid_hash_batch());
        }
        let index = u32::try_from(hash_index).map_err(|_| JsonRpcErrorWire::invalid_hash_batch())?;
        if index > ASYNC_PAYMENTS_BIP32_MAX_CHILD_INDEX {
            return Err(JsonRpcErrorWire::invalid_hash_batch());
        }

        let child_xprv = derive_hardened_child(&self.account_xprv, index)?;
        let child_secret = child_xprv.private_key.secret_bytes();
        let mut preimage_material =
            Vec::with_capacity(ASYNC_PAYMENTS_PREIMAGE_DOMAIN.len() + child_secret.len());
        preimage_material.extend_from_slice(ASYNC_PAYMENTS_PREIMAGE_DOMAIN);
        preimage_material.extend_from_slice(&child_secret);

        let payment_preimage = PaymentPreimage(digest(&preimage_material));
        let payment_hash = PaymentHash(digest(&payment_preimage.0));
        Ok((payment_preimage, payment_hash))
    }

    pub(crate) fn prepare_async_order_new_params(
        &self,
        start_index: u64,
        batch_size: usize,
    ) -> Result<AsyncOrderNewParamsWire, JsonRpcErrorWire> {
        if start_index < ASYNC_ORDER_FIRST_HASH_INDEX
            || batch_size == 0
            || batch_size > ASYNC_ORDER_MAX_HASH_BATCH_SIZE
        {
            return Err(JsonRpcErrorWire::invalid_hash_batch());
        }

        let last_index = start_index
            .checked_add((batch_size - 1) as u64)
            .ok_or_else(JsonRpcErrorWire::invalid_hash_batch)?;
        if last_index > ASYNC_PAYMENTS_BIP32_MAX_CHILD_INDEX as u64 {
            return Err(JsonRpcErrorWire::invalid_hash_batch());
        }

        let mut hashes = Vec::with_capacity(batch_size);
        for hash_index in start_index..=last_index {
            let (_preimage, payment_hash) = self.derive_hash_material(hash_index)?;
            hashes.push(AsyncOrderNewHashWire {
                hash_index,
                payment_hash: hex_str(&payment_hash.0),
            });
        }

        Ok(AsyncOrderNewParamsWire {
            protocol_version: PROTOCOL_VERSION,
            hashes,
            batch: None,
            address_sig: None,
        })
    }
}

fn derive_hardened_child(parent: &Xpriv, index: u32) -> Result<Xpriv, JsonRpcErrorWire> {
    if index > ASYNC_PAYMENTS_BIP32_MAX_CHILD_INDEX {
        return Err(JsonRpcErrorWire::invalid_hash_batch());
    }
    parent
        .derive_priv(&Secp256k1::new(), &ChildNumber::Hardened { index })
        .map_err(|err| {
            JsonRpcErrorWire::internal_error(format!(
                "async_payment_preimage_derivation_failed: {err}"
            ))
        })
}

// ---------------------------------------------------------------------------
// KV persistence of the per-host next hash index (mirror src/async_order.rs)
// ---------------------------------------------------------------------------

fn validate_next_hash_index(next_index: u64) -> Result<(), JsonRpcErrorWire> {
    if !(ASYNC_ORDER_FIRST_HASH_INDEX..=ASYNC_PAYMENTS_BIP32_MAX_CHILD_INDEX as u64 + 1)
        .contains(&next_index)
    {
        return Err(JsonRpcErrorWire::internal_error(
            "async_payments_next_index_out_of_range",
        ));
    }
    Ok(())
}

pub(crate) fn read_async_payments_next_hash_index(
    kv_store: &dyn KVStoreSync,
    host_node_id: &PublicKey,
) -> Result<u64, JsonRpcErrorWire> {
    match kv_store.read(
        ASYNC_PAYMENTS_KV_NAMESPACE,
        ASYNC_PAYMENTS_NEXT_INDEX_KV_NAMESPACE,
        &hex_str(&host_node_id.serialize()),
    ) {
        Ok(bytes) => {
            let value = String::from_utf8(bytes).map_err(|err| {
                JsonRpcErrorWire::internal_error(format!(
                    "async_payments_next_index_invalid_utf8: {err}"
                ))
            })?;
            let next_index = value.parse::<u64>().map_err(|err| {
                JsonRpcErrorWire::internal_error(format!(
                    "async_payments_next_index_invalid_value: {err}"
                ))
            })?;
            validate_next_hash_index(next_index)?;
            Ok(next_index)
        }
        Err(err) if err.kind() == lightning::io::ErrorKind::NotFound => {
            Ok(ASYNC_ORDER_FIRST_HASH_INDEX)
        }
        Err(err) => Err(JsonRpcErrorWire::internal_error(format!(
            "async_payments_next_index_read_failed: {err}"
        ))),
    }
}

pub(crate) fn write_async_payments_next_hash_index(
    kv_store: &dyn KVStoreSync,
    host_node_id: &PublicKey,
    next_index: u64,
) -> Result<(), JsonRpcErrorWire> {
    validate_next_hash_index(next_index)?;
    kv_store
        .write(
            ASYNC_PAYMENTS_KV_NAMESPACE,
            ASYNC_PAYMENTS_NEXT_INDEX_KV_NAMESPACE,
            &hex_str(&host_node_id.serialize()),
            next_index.to_string().into_bytes(),
        )
        .map_err(|err| {
            JsonRpcErrorWire::internal_error(format!(
                "async_payments_next_index_write_failed: {err}"
            ))
        })
}

// ---------------------------------------------------------------------------
// request id generation (no Math.random in scripts; derive deterministically)
// ---------------------------------------------------------------------------

thread_local! {
    static REQUEST_ID_COUNTER: Cell<u64> = const { Cell::new(0) };
}

/// Produce a per-process-unique JSON-RPC request id. Combines a monotonic counter with the
/// local/host pubkey prefixes so concurrent in-flight requests never collide.
pub(crate) fn new_request_id(local_node_hex: &str, host_node_hex: &str) -> String {
    let n = REQUEST_ID_COUNTER.with(|c| {
        let next = c.get().wrapping_add(1);
        c.set(next);
        next
    });
    let local_prefix = local_node_hex.get(..8).unwrap_or(local_node_hex);
    let host_prefix = host_node_hex.get(..8).unwrap_or(host_node_hex);
    format!("apay-{local_prefix}-{host_prefix}-{n}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h32(s: &str) -> [u8; 32] {
        hex::decode(s).unwrap().try_into().unwrap()
    }

    /// The Merkle leaf/root byte layout must stay identical to the native node's
    /// `src/apay_merkle.rs` reference vectors, otherwise the host's `batch_root`/`batch_sig`
    /// verification would reject WASM clients.
    #[test]
    fn merkle_root_matches_native_reference_vector() {
        let mut recipient_pubkey = [0x11u8; 33];
        recipient_pubkey[0] = 0x02;
        let mut batch_id = [0u8; 16];
        for (i, b) in batch_id.iter_mut().enumerate() {
            *b = i as u8;
        }
        let leaves: Vec<[u8; 32]> = (1u64..=5)
            .map(|i| leaf_hash(&recipient_pubkey, &batch_id, i, &[i as u8; 32]))
            .collect();

        assert_eq!(
            leaves[0],
            h32("eabd11aa1b47ea65fa1c5a7cd7992dcaf9f0725ced14eabfebd20a697e8c08a9")
        );
        assert_eq!(
            leaves[4],
            h32("2a583c0c4ee4b0d8d985970116b8035830556eac01a123c0151f8d8dd9fa83fe")
        );
        assert_eq!(
            merkle_root(&leaves),
            h32("2a89ca7e910bae70bc3f03f0252ec22de83cc40a5b4ba1582b649be5ea667132")
        );
    }
}
