use std::sync::Arc;

use bitcoin::hashes::hmac::HmacEngine;
use bitcoin::hashes::sha256::Hash as Sha256;
use bitcoin::hex::DisplayHex;
use bitcoin::hex::FromHex;
use bitcoin::psbt::ExtractTxError;
use bitcoin::script::ScriptBuf;
use bitcoin::secp256k1::ecdh::SharedSecret;
use bitcoin::secp256k1::ecdsa::{RecoverableSignature, RecoveryId, Signature};
use bitcoin::secp256k1::{PublicKey, Scalar};
use bitcoin::Psbt;
use lightning::ln::msgs::UnsignedGossipMessage;
use lightning::ln::script::ShutdownScript;
use lightning::offers::invoice::UnsignedBolt12Invoice;
use lightning::offers::nonce::Nonce;
use lightning::sign::{
    EntropySource, NodeSigner, OutputSpender, PeerStorageKey, Recipient, SignerProvider,
    SpendableOutputDescriptor,
};
use lightning::util::ser::Writeable;
use lightning_invoice::RawBolt11Invoice;
use std::str::FromStr;

use super::channel_signer::ExternalChannelSigner;
use super::entropy::SystemEntropySource;
use super::transport::ExternalSignerTransport;
use super::types::{
    validate_bootstrap_payload, AsyncPaymentsHashEntry, BootstrapData, DerivedAddressMatch,
    ExternalNodeRequest, ExternalNodeResponse, ExternalSignerRequest, ExternalSignerResponse,
    RlnSignerError, SpendableDescriptorKind, SpendableOutputSignInput, WalletDerivationMatch,
    WalletInputMetadata,
};
use super::vls_adapter::{ExternalSignerBackend, VlsSignerAdapter};
use super::RlnEntropySource;
use super::RlnKeysInterface;

const ZBASE32_ALPHABET: &[u8; 32] = b"ybndrfg8ejkmcpqxot1uwisza345h769";

fn zbase32_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity((data.len() * 8).div_ceil(5));
    let mut bit_buffer = 0u16;
    let mut pending_bits = 0u8;

    for byte in data {
        bit_buffer = (bit_buffer << 8) | u16::from(*byte);
        pending_bits += 8;
        while pending_bits >= 5 {
            pending_bits -= 5;
            let alphabet_index = ((bit_buffer >> pending_bits) & 0x1f) as usize;
            out.push(ZBASE32_ALPHABET[alphabet_index] as char);
        }
    }

    if pending_bits > 0 {
        let alphabet_index = ((bit_buffer << (5 - pending_bits)) & 0x1f) as usize;
        out.push(ZBASE32_ALPHABET[alphabet_index] as char);
    }

    out
}

fn ln_signed_message_from_compact(signature_hex: &str, recovery_id: u8) -> Result<String, ()> {
    RecoveryId::from_i32(i32::from(recovery_id)).map_err(|_| ())?;
    let compact_signature = Vec::<u8>::from_hex(signature_hex).map_err(|_| ())?;
    if compact_signature.len() != 64 {
        return Err(());
    }

    let mut recoverable_signature = Vec::with_capacity(65);
    recoverable_signature.push(31 + recovery_id);
    recoverable_signature.extend_from_slice(&compact_signature);
    Ok(zbase32_encode(&recoverable_signature))
}

#[cfg(test)]
mod tests {
    use super::*;
    use bitcoin::hashes::{sha256d, Hash};
    use bitcoin::secp256k1::{Message, Secp256k1, SecretKey};
    use std::sync::Mutex;

    use crate::signer::proto::{decode_signer_request, encode_signer_response};
    use crate::signer::transport::ExternalSignerTransport;
    use crate::signer::vls_adapter::VlsSignerAdapter;

    struct RawMessageTransport {
        signature_hex: String,
        recovery_id: u8,
        seen_message_hex: Mutex<Option<String>>,
    }

    impl ExternalSignerTransport for RawMessageTransport {
        fn call(&self, request: &[u8]) -> Result<Vec<u8>, RlnSignerError> {
            let req = decode_signer_request(request)?;
            let response = match req {
                ExternalSignerRequest::Node(ExternalNodeRequest::SignMessageRaw {
                    message_hex,
                }) => {
                    *self.seen_message_hex.lock().expect("lock") = Some(message_hex);
                    ExternalSignerResponse::Node(ExternalNodeResponse::RecoverableSignature {
                        signature_hex: self.signature_hex.clone(),
                        recovery_id: self.recovery_id,
                    })
                }
                other => {
                    return Err(RlnSignerError::Protocol(format!(
                        "unexpected request: {other:?}"
                    )))
                }
            };
            encode_signer_response(&response)
        }
    }

    #[test]
    fn zbase32_encode_matches_known_vectors() {
        assert_eq!(zbase32_encode(&[]), "");
        assert_eq!(zbase32_encode(&[0x00]), "yy");
        assert_eq!(zbase32_encode(&[0x80]), "oy");
        assert_eq!(zbase32_encode(&[0x8b, 0x88, 0x80]), "tqrey");
    }

    #[test]
    fn ln_signed_message_conversion_matches_ldk() {
        let secp = Secp256k1::new();
        let secret = SecretKey::from_slice(&[1u8; 32]).expect("secret");
        let message: &[u8] = b"\x00\xffapay";
        let digest = sha256d::Hash::hash(&[&b"Lightning Signed Message:"[..], message].concat());
        let signature =
            secp.sign_ecdsa_recoverable(&Message::from_digest(digest.to_byte_array()), &secret);
        let (recovery_id, compact) = signature.serialize_compact();

        let actual = ln_signed_message_from_compact(
            &compact.to_lower_hex_string(),
            recovery_id.to_i32() as u8,
        )
        .expect("convert");
        let expected = lightning::util::message_signing::sign(message, &secret);

        assert_eq!(actual, expected);
    }

    #[test]
    fn external_sign_message_uses_raw_hex_and_returns_zbase32() {
        let secp = Secp256k1::new();
        let secret = SecretKey::from_slice(&[2u8; 32]).expect("secret");
        let message: &[u8] = b"\x00\xffapay";
        let digest = sha256d::Hash::hash(&[&b"Lightning Signed Message:"[..], message].concat());
        let signature =
            secp.sign_ecdsa_recoverable(&Message::from_digest(digest.to_byte_array()), &secret);
        let (recovery_id, compact) = signature.serialize_compact();

        let transport = Arc::new(RawMessageTransport {
            signature_hex: compact.to_lower_hex_string(),
            recovery_id: recovery_id.to_i32() as u8,
            seen_message_hex: Mutex::new(None),
        });
        let signer = ExternalSigner {
            backend: Arc::new(VlsSignerAdapter::new(transport.clone())),
        };

        let signed_message = signer.sign_message(message).expect("sign");

        assert_eq!(
            transport.seen_message_hex.lock().expect("lock").as_deref(),
            Some("00ff61706179")
        );
        assert_eq!(
            signed_message,
            lightning::util::message_signing::sign(message, &secret)
        );
    }
}

/// Transport-backed external signer: LDK `NodeSigner` / channel / PSBT ops delegate to the host.
/// Bootstrap currently carries only public signer identity/config, while runtime signer
/// operations fetch all signing-capable material through the external signer backend.
///
/// Production unlock builds this via [`ExternalSigner::from_attachment`]. `crate::ldk::start_ldk`
/// does not construct a local [`lightning::sign::KeysManager`] when the active signer is external.
/// When LDK passes this type as [`lightning::sign::EntropySource`], randomness is drawn from
/// [`SystemEntropySource`] (same OsRng path as `start_ldk`'s `ldk_entropy_source`) so channel-scoped
/// randomness never depends on host RPC latency or policy. Host-backed signing uses [`NodeSigner`],
/// [`SignerProvider`], and related traits only.
#[derive(Clone)]
pub(crate) struct ExternalSigner {
    backend: Arc<dyn ExternalSignerBackend>,
}

#[derive(Clone)]
pub(crate) struct ExternalSignerAttachment {
    pub(crate) bootstrap: BootstrapData,
    pub(crate) transport: Arc<dyn ExternalSignerTransport>,
}

impl ExternalSigner {
    fn parse_signature(signature_hex: &str) -> Result<Signature, ()> {
        let bytes = Vec::<u8>::from_hex(signature_hex).map_err(|_| ())?;
        Signature::from_der(&bytes)
            .or_else(|_| Signature::from_compact(&bytes))
            .map_err(|_| ())
    }

    pub(crate) fn from_attachment(
        attachment: &ExternalSignerAttachment,
    ) -> Result<Self, RlnSignerError> {
        validate_bootstrap_payload(&attachment.bootstrap)?;
        let backend: Arc<dyn ExternalSignerBackend> =
            Arc::new(VlsSignerAdapter::new(Arc::clone(&attachment.transport)));
        Ok(Self { backend })
    }

    pub(crate) fn generate_channel_keys_id(
        &self,
        inbound: bool,
        channel_value_satoshis: u64,
        user_channel_id: u128,
    ) -> Result<String, RlnSignerError> {
        self.backend
            .generate_channel_keys_id(inbound, channel_value_satoshis, user_channel_id)
    }

    pub(crate) fn derive_channel_signer(
        &self,
        channel_value_satoshis: u64,
        channel_keys_id_hex: String,
    ) -> Result<ExternalChannelSigner, RlnSignerError> {
        let (channel_signer_state_hex, channel_pubkeys) = self
            .backend
            .derive_channel_signer(channel_value_satoshis, channel_keys_id_hex.clone())?;
        Ok(ExternalChannelSigner::new(
            Arc::clone(&self.backend),
            channel_keys_id_hex,
            channel_signer_state_hex,
            channel_pubkeys,
        ))
    }

    pub(crate) fn sign_rgb_psbt(
        &self,
        descriptors: Vec<String>,
        psbt: String,
    ) -> Result<String, RlnSignerError> {
        self.backend.sign_rgb_psbt(descriptors, psbt)
    }

    pub(crate) fn get_wallet_input_metadata(
        &self,
        txid_hex: String,
        vout: u32,
        script_pubkey_hex: Option<String>,
        amount_sat: Option<u64>,
    ) -> Result<Option<WalletInputMetadata>, RlnSignerError> {
        self.backend
            .get_wallet_input_metadata(txid_hex, vout, script_pubkey_hex, amount_sat)
    }

    pub(crate) fn prepare_async_payments_hashes(
        &self,
        host_node_id_hex: String,
        start_index: u64,
        batch_size: u32,
    ) -> Result<Vec<AsyncPaymentsHashEntry>, RlnSignerError> {
        self.backend
            .prepare_async_payments_hashes(host_node_id_hex, start_index, batch_size)
    }

    pub(crate) fn get_async_payment_preimage(
        &self,
        host_node_id_hex: String,
        hash_index: u64,
        payment_hash_hex: String,
    ) -> Result<String, RlnSignerError> {
        self.backend
            .node_get_async_payment_preimage(host_node_id_hex, hash_index, payment_hash_hex)
    }

    fn recipient_label(recipient: Recipient) -> &'static str {
        match recipient {
            Recipient::Node => "node",
            Recipient::PhantomNode => "phantom",
        }
    }

    fn spendable_descriptor_to_input(
        &self,
        d: &SpendableOutputDescriptor,
    ) -> Result<SpendableOutputSignInput, ()> {
        Ok(match d {
            SpendableOutputDescriptor::StaticOutput {
                outpoint,
                output,
                channel_keys_id,
            } => {
                let script_pubkey_hex = output.script_pubkey.to_hex_string();
                let wallet_derivation_match = self
                    .find_derivation_match_for_script(&script_pubkey_hex)?
                    .map(|m| WalletDerivationMatch {
                        account_name: m.account_name,
                        keyindex: m.keyindex,
                        derivation_path: m.derivation_path,
                    });
                SpendableOutputSignInput {
                    descriptor_kind: SpendableDescriptorKind::StaticOutput,
                    txid_hex: outpoint.txid.to_string(),
                    vout: outpoint.index as u32,
                    amount_sat: output.value.to_sat(),
                    script_pubkey_hex,
                    channel_keys_id_hex: channel_keys_id.map(|id| id.to_lower_hex_string()),
                    wallet_derivation_match,
                    witness_script_hex: None,
                    redeem_script_hex: None,
                    per_commitment_point_hex: None,
                    to_self_delay: None,
                }
            }
            SpendableOutputDescriptor::DelayedPaymentOutput(o) => SpendableOutputSignInput {
                descriptor_kind: SpendableDescriptorKind::DelayedPaymentOutput,
                txid_hex: o.outpoint.txid.to_string(),
                vout: o.outpoint.index as u32,
                amount_sat: o.output.value.to_sat(),
                script_pubkey_hex: o.output.script_pubkey.to_hex_string(),
                channel_keys_id_hex: Some(o.channel_keys_id.to_lower_hex_string()),
                wallet_derivation_match: None,
                witness_script_hex: None,
                redeem_script_hex: None,
                per_commitment_point_hex: Some(
                    o.per_commitment_point.serialize().to_lower_hex_string(),
                ),
                to_self_delay: Some(o.to_self_delay),
            },
            SpendableOutputDescriptor::StaticPaymentOutput(o) => SpendableOutputSignInput {
                descriptor_kind: SpendableDescriptorKind::StaticPaymentOutput,
                txid_hex: o.outpoint.txid.to_string(),
                vout: o.outpoint.index as u32,
                amount_sat: o.output.value.to_sat(),
                script_pubkey_hex: o.output.script_pubkey.to_hex_string(),
                channel_keys_id_hex: Some(o.channel_keys_id.to_lower_hex_string()),
                wallet_derivation_match: None,
                witness_script_hex: None,
                redeem_script_hex: None,
                per_commitment_point_hex: None,
                to_self_delay: None,
            },
        })
    }

    fn find_derivation_match_for_script(
        &self,
        script_pubkey_hex: &str,
    ) -> Result<Option<DerivedAddressMatch>, ()> {
        const MAX_DERIVATION_SEARCH_INDEX: u32 = 10_000;
        let matches = self
            .backend
            .find_derivation_matches_for_script(
                script_pubkey_hex.to_string(),
                MAX_DERIVATION_SEARCH_INDEX,
            )
            .map_err(|_| ())?;
        Ok(matches.first().cloned())
    }
}

impl EntropySource for ExternalSigner {
    fn get_secure_random_bytes(&self) -> [u8; 32] {
        RlnEntropySource::get_secure_random_bytes(&SystemEntropySource)
    }
}

impl NodeSigner for ExternalSigner {
    fn get_expanded_key(&self) -> lightning::ln::inbound_payment::ExpandedKey {
        panic!("ExternalSigner::get_expanded_key should not be used in external signer mode")
    }

    fn crypt_for_offer(&self, payment_id: [u8; 32], nonce: Nonce) -> [u8; 32] {
        let bytes_hex = self
            .backend
            .node_crypt_for_offer(
                payment_id.to_lower_hex_string(),
                nonce.as_slice().to_lower_hex_string(),
            )
            .unwrap_or_else(|e| {
                panic!(
                    "external signer crypt_for_offer failed: nonce={} error={e}",
                    nonce.as_slice().to_lower_hex_string()
                )
            });
        let bytes = Vec::<u8>::from_hex(&bytes_hex).unwrap_or_else(|e| {
            panic!(
                "external signer crypt_for_offer returned invalid hex: hex={bytes_hex} error={e}"
            )
        });
        let bytes_len = bytes.len();
        bytes.try_into().unwrap_or_else(|_| {
            panic!(
                "external signer crypt_for_offer returned {} bytes, expected 32",
                bytes_len
            )
        })
    }

    fn hmac_for_offer(&self) -> HmacEngine<Sha256> {
        let key_hex = self
            .backend
            .node_get_hmac_for_offer_key()
            .unwrap_or_else(|e| panic!("external signer get_hmac_for_offer_key failed: error={e}"));
        let key = Vec::<u8>::from_hex(&key_hex).unwrap_or_else(|e| {
            panic!("external signer get_hmac_for_offer_key returned invalid hex: hex={key_hex} error={e}")
        });
        HmacEngine::<Sha256>::new(&key)
    }

    fn create_inbound_payment(
        &self,
        min_value_msat: Option<u64>,
        invoice_expiry_delta_secs: u32,
        random_bytes: [u8; 32],
        current_time: u64,
        min_final_cltv_expiry_delta: Option<u16>,
    ) -> Result<
        (
            lightning::types::payment::PaymentHash,
            lightning::types::payment::PaymentSecret,
        ),
        (),
    > {
        let (payment_hash_hex, payment_secret_hex) = self
            .backend
            .node_create_inbound_payment(
                min_value_msat,
                invoice_expiry_delta_secs,
                random_bytes.to_lower_hex_string(),
                current_time,
                min_final_cltv_expiry_delta,
            )
            .map_err(|_| ())?;
        let payment_hash = Vec::<u8>::from_hex(&payment_hash_hex).map_err(|_| ())?;
        let payment_secret = Vec::<u8>::from_hex(&payment_secret_hex).map_err(|_| ())?;
        Ok((
            lightning::types::payment::PaymentHash(payment_hash.try_into().map_err(|_| ())?),
            lightning::types::payment::PaymentSecret(payment_secret.try_into().map_err(|_| ())?),
        ))
    }

    fn create_inbound_payment_for_hash(
        &self,
        payment_hash: lightning::types::payment::PaymentHash,
        min_value_msat: Option<u64>,
        invoice_expiry_delta_secs: u32,
        current_time: u64,
        min_final_cltv_expiry_delta: Option<u16>,
    ) -> Result<lightning::types::payment::PaymentSecret, ()> {
        let payment_secret_hex = self
            .backend
            .node_create_inbound_payment_for_hash(
                payment_hash.0.to_lower_hex_string(),
                min_value_msat,
                invoice_expiry_delta_secs,
                current_time,
                min_final_cltv_expiry_delta,
            )
            .map_err(|_| ())?;
        let payment_secret = Vec::<u8>::from_hex(&payment_secret_hex).map_err(|_| ())?;
        Ok(lightning::types::payment::PaymentSecret(
            payment_secret.try_into().map_err(|_| ())?,
        ))
    }

    fn create_spontaneous_payment_secret(
        &self,
        min_value_msat: Option<u64>,
        invoice_expiry_delta_secs: u32,
        current_time: u64,
        min_final_cltv_expiry_delta: Option<u16>,
    ) -> Result<lightning::types::payment::PaymentSecret, ()> {
        let payment_secret_hex = self
            .backend
            .node_create_spontaneous_payment_secret(
                min_value_msat,
                invoice_expiry_delta_secs,
                current_time,
                min_final_cltv_expiry_delta,
            )
            .map_err(|_| ())?;
        let payment_secret = Vec::<u8>::from_hex(&payment_secret_hex).map_err(|_| ())?;
        Ok(lightning::types::payment::PaymentSecret(
            payment_secret.try_into().map_err(|_| ())?,
        ))
    }

    fn verify_inbound_payment(
        &self,
        payment_hash: lightning::types::payment::PaymentHash,
        payment_data: &lightning::ln::msgs::FinalOnionHopData,
        highest_seen_timestamp: u64,
    ) -> Result<
        (
            Option<lightning::types::payment::PaymentPreimage>,
            Option<u16>,
        ),
        (),
    > {
        let (payment_preimage_hex, min_final_cltv_expiry_delta) = self
            .backend
            .node_verify_inbound_payment(
                payment_hash.0.to_lower_hex_string(),
                payment_data.payment_secret.0.to_lower_hex_string(),
                payment_data.total_msat,
                highest_seen_timestamp,
            )
            .map_err(|_| ())?;
        let payment_preimage = match payment_preimage_hex {
            Some(hex) => {
                let bytes = Vec::<u8>::from_hex(&hex).map_err(|_| ())?;
                Some(lightning::types::payment::PaymentPreimage(
                    bytes.try_into().map_err(|_| ())?,
                ))
            }
            None => None,
        };
        Ok((payment_preimage, min_final_cltv_expiry_delta))
    }

    fn get_payment_preimage(
        &self,
        payment_hash: lightning::types::payment::PaymentHash,
        payment_secret: lightning::types::payment::PaymentSecret,
    ) -> Result<lightning::types::payment::PaymentPreimage, lightning::util::errors::APIError> {
        let payment_preimage_hex = self
            .backend
            .node_get_payment_preimage(
                payment_hash.0.to_lower_hex_string(),
                payment_secret.0.to_lower_hex_string(),
            )
            .map_err(|e| lightning::util::errors::APIError::APIMisuseError {
                err: format!("external signer get_payment_preimage failed: {e}"),
            })?;
        let bytes = Vec::<u8>::from_hex(&payment_preimage_hex).map_err(|e| {
            lightning::util::errors::APIError::APIMisuseError {
                err: format!("external signer get_payment_preimage returned invalid hex: {e}"),
            }
        })?;
        Ok(lightning::types::payment::PaymentPreimage(
            bytes
                .try_into()
                .map_err(|_| lightning::util::errors::APIError::APIMisuseError {
                    err: "external signer get_payment_preimage returned invalid length".to_string(),
                })?,
        ))
    }

    fn get_peer_storage_key(&self) -> PeerStorageKey {
        panic!("ExternalSigner::get_peer_storage_key should not be used in external signer mode")
    }

    fn encrypt_peer_storage_payload(&self, plaintext: Vec<u8>, random_bytes: [u8; 32]) -> Vec<u8> {
        let bytes_hex = self
            .backend
            .node_encrypt_peer_storage_payload(
                plaintext.to_lower_hex_string(),
                random_bytes.to_lower_hex_string(),
            )
            .unwrap_or_else(|e| {
                panic!("external signer encrypt_peer_storage_payload failed: error={e}")
            });
        Vec::<u8>::from_hex(&bytes_hex).unwrap_or_else(|e| {
            panic!(
                "external signer encrypt_peer_storage_payload returned invalid hex: hex={bytes_hex} error={e}"
            )
        })
    }

    fn decrypt_peer_storage_payload(&self, ciphertext: Vec<u8>) -> Result<Vec<u8>, ()> {
        let bytes_hex = self
            .backend
            .node_decrypt_peer_storage_payload(ciphertext.to_lower_hex_string())
            .map_err(|_| ())?;
        Vec::<u8>::from_hex(&bytes_hex).map_err(|_| ())
    }

    fn encrypt_blinded_message_payload(&self, plaintext: Vec<u8>, rho: [u8; 32]) -> Vec<u8> {
        let bytes_hex = self
            .backend
            .node_encrypt_blinded_message_payload(
                plaintext.to_lower_hex_string(),
                rho.to_lower_hex_string(),
            )
            .unwrap_or_else(|e| {
                panic!("external signer encrypt_blinded_message_payload failed: error={e}")
            });
        Vec::<u8>::from_hex(&bytes_hex).unwrap_or_else(|e| {
            panic!(
                "external signer encrypt_blinded_message_payload returned invalid hex: hex={bytes_hex} error={e}"
            )
        })
    }

    fn decrypt_blinded_message_payload(
        &self,
        ciphertext: &[u8],
        rho: [u8; 32],
    ) -> Result<(Vec<u8>, bool), lightning::ln::msgs::DecodeError> {
        let (bytes_hex, used_aad) = self
            .backend
            .node_decrypt_blinded_message_payload(
                ciphertext.to_lower_hex_string(),
                rho.to_lower_hex_string(),
            )
            .map_err(|_| lightning::ln::msgs::DecodeError::InvalidValue)?;
        let bytes = Vec::<u8>::from_hex(&bytes_hex)
            .map_err(|_| lightning::ln::msgs::DecodeError::InvalidValue)?;
        Ok((bytes, used_aad))
    }

    fn get_receive_auth_key(&self) -> lightning::sign::ReceiveAuthKey {
        panic!("ExternalSigner::get_receive_auth_key should not be used in external signer mode")
    }

    fn get_node_id(&self, recipient: Recipient) -> Result<PublicKey, ()> {
        let node_id_hex = self
            .backend
            .node_get_node_id(Self::recipient_label(recipient).to_string())
            .map_err(|_| ())?;
        let bytes = Vec::<u8>::from_hex(&node_id_hex).map_err(|_| ())?;
        PublicKey::from_slice(&bytes).map_err(|_| ())
    }

    fn ecdh(
        &self,
        recipient: Recipient,
        other_key: &PublicKey,
        tweak: Option<&Scalar>,
    ) -> Result<SharedSecret, ()> {
        let req = ExternalSignerRequest::Node(ExternalNodeRequest::Ecdh {
            recipient: Self::recipient_label(recipient).to_string(),
            other_key: other_key.serialize().to_lower_hex_string(),
            tweak: tweak.map(|t| t.to_be_bytes().to_lower_hex_string()),
        });
        let resp = self.backend.call(req).map_err(|_| ())?;
        let ExternalSignerResponse::Node(ExternalNodeResponse::Ecdh { shared_secret_hex }) = resp
        else {
            return Err(());
        };
        let bytes = Vec::<u8>::from_hex(&shared_secret_hex).map_err(|_| ())?;
        let arr: [u8; 32] = bytes.try_into().map_err(|_| ())?;
        SharedSecret::from_slice(&arr).map_err(|_| ())
    }

    fn sign_invoice(
        &self,
        invoice: &RawBolt11Invoice,
        _recipient: Recipient,
    ) -> Result<RecoverableSignature, ()> {
        let (hrp, u5bytes) = invoice.to_raw();
        let req = ExternalSignerRequest::Node(ExternalNodeRequest::SignInvoice {
            hrp,
            u5bytes_hex: u5bytes
                .iter()
                .map(|b| u8::from(*b))
                .collect::<Vec<_>>()
                .to_lower_hex_string(),
        });
        let resp = self.backend.call(req).map_err(|_| ())?;
        let ExternalSignerResponse::Node(ExternalNodeResponse::RecoverableSignature {
            signature_hex,
            recovery_id,
        }) = resp
        else {
            return Err(());
        };
        let sig_bytes = Vec::<u8>::from_hex(&signature_hex).map_err(|_| ())?;
        let sig_arr: [u8; 64] = sig_bytes.try_into().map_err(|_| ())?;
        let rec_id = RecoveryId::from_i32(recovery_id as i32).map_err(|_| ())?;
        RecoverableSignature::from_compact(&sig_arr, rec_id).map_err(|_| ())
    }

    fn sign_bolt12_invoice(
        &self,
        invoice: &UnsignedBolt12Invoice,
    ) -> Result<bitcoin::secp256k1::schnorr::Signature, ()> {
        let req = ExternalSignerRequest::Node(ExternalNodeRequest::SignBolt12Invoice {
            invoice: invoice.encode().to_lower_hex_string(),
        });
        let resp = self.backend.call(req).map_err(|_| ())?;
        let ExternalSignerResponse::Node(ExternalNodeResponse::Signature { signature_hex }) = resp
        else {
            return Err(());
        };
        let bytes = Vec::<u8>::from_hex(&signature_hex).map_err(|_| ())?;
        bitcoin::secp256k1::schnorr::Signature::from_slice(&bytes).map_err(|_| ())
    }

    fn sign_gossip_message(&self, msg: UnsignedGossipMessage) -> Result<Signature, ()> {
        let req = ExternalSignerRequest::Node(ExternalNodeRequest::SignGossipMessage {
            message_hex: msg.encode().to_lower_hex_string(),
        });
        let resp = self.backend.call(req).map_err(|_| ())?;
        let ExternalSignerResponse::Node(ExternalNodeResponse::Signature { signature_hex }) = resp
        else {
            return Err(());
        };
        Self::parse_signature(&signature_hex)
    }

    fn sign_message(&self, msg: &[u8]) -> Result<String, ()> {
        let req = ExternalSignerRequest::Node(ExternalNodeRequest::SignMessageRaw {
            message_hex: msg.to_lower_hex_string(),
        });
        let resp = self.backend.call(req).map_err(|_| ())?;
        let ExternalSignerResponse::Node(ExternalNodeResponse::RecoverableSignature {
            signature_hex,
            recovery_id,
        }) = resp
        else {
            return Err(());
        };
        ln_signed_message_from_compact(&signature_hex, recovery_id)
    }
}

impl SignerProvider for ExternalSigner {
    type EcdsaSigner = ExternalChannelSigner;

    fn generate_channel_keys_id(&self, inbound: bool, user_channel_id: u128) -> [u8; 32] {
        let keys_hex = self.generate_channel_keys_id(inbound, 0, user_channel_id).unwrap_or_else(|e| {
            panic!(
                "external signer generate_channel_keys_id failed: inbound={inbound} user_channel_id={user_channel_id} error={e}"
            )
        });
        let bytes = Vec::<u8>::from_hex(&keys_hex).unwrap_or_else(|e| {
            panic!(
                "external signer generate_channel_keys_id returned invalid hex: inbound={inbound} user_channel_id={user_channel_id} hex={keys_hex} error={e}"
            )
        });
        if bytes.len() < 32 {
            panic!(
                "external signer generate_channel_keys_id returned too few bytes: inbound={inbound} user_channel_id={user_channel_id} len={} hex={keys_hex}",
                bytes.len()
            );
        }
        let mut out = [0u8; 32];
        out.copy_from_slice(&bytes[..32]);
        out
    }

    fn derive_channel_signer(&self, channel_keys_id: [u8; 32]) -> Self::EcdsaSigner {
        self.derive_channel_signer(0, channel_keys_id.to_lower_hex_string())
            .unwrap_or_else(|e| {
                panic!(
                    "external signer derive_channel_signer failed: channel_keys_id={} error={e}",
                    channel_keys_id.to_lower_hex_string()
                )
            })
    }

    fn get_destination_script(&self, channel_keys_id: [u8; 32]) -> Result<ScriptBuf, ()> {
        let script_hex = self
            .backend
            .node_get_destination_script(channel_keys_id.to_lower_hex_string())
            .map_err(|_| ())?;
        let bytes = Vec::<u8>::from_hex(&script_hex).map_err(|_| ())?;
        Ok(ScriptBuf::from_bytes(bytes))
    }

    fn get_shutdown_scriptpubkey(&self) -> Result<ShutdownScript, ()> {
        let script_hex = self
            .backend
            .node_get_shutdown_scriptpubkey()
            .map_err(|_| ())?;
        let bytes = Vec::<u8>::from_hex(&script_hex).map_err(|_| ())?;
        let script = ScriptBuf::from_bytes(bytes);
        ShutdownScript::try_from(script).map_err(|_| ())
    }
}

impl OutputSpender for ExternalSigner {
    fn spend_spendable_outputs(
        &self,
        descriptors: &[&SpendableOutputDescriptor],
        outputs: Vec<bitcoin::TxOut>,
        change_destination_script: ScriptBuf,
        feerate_sat_per_1000_weight: u32,
        locktime: Option<bitcoin::locktime::absolute::LockTime>,
        secp_ctx: &bitcoin::secp256k1::Secp256k1<bitcoin::secp256k1::All>,
    ) -> Result<bitcoin::Transaction, ()> {
        let (psbt, _expected_weight) = SpendableOutputDescriptor::create_spendable_outputs_psbt(
            secp_ctx,
            descriptors,
            outputs,
            change_destination_script,
            feerate_sat_per_1000_weight,
            locktime,
        )
        .map_err(|_| ())?;
        let signed = self.sign_spendable_outputs_psbt(descriptors, psbt, secp_ctx)?;
        match signed.extract_tx() {
            Ok(tx) => Ok(tx),
            Err(ExtractTxError::MissingInputValue { tx }) => Ok(tx),
            Err(_) => Err(()),
        }
    }
}

impl RlnKeysInterface for ExternalSigner {
    fn sign_spendable_outputs_psbt(
        &self,
        descriptors: &[&SpendableOutputDescriptor],
        psbt: Psbt,
        _secp_ctx: &bitcoin::secp256k1::Secp256k1<bitcoin::secp256k1::All>,
    ) -> Result<Psbt, ()> {
        let inputs = descriptors
            .iter()
            .map(|descriptor| self.spendable_descriptor_to_input(descriptor))
            .collect::<Result<Vec<_>, _>>()?;
        let signed_psbt = self
            .backend
            .sign_spendable_outputs_psbt(inputs, psbt.to_string())
            .map_err(|_| ())?;
        Psbt::from_str(&signed_psbt).map_err(|_| ())
    }

    fn sign_rgb_psbt(
        &self,
        descriptors: Vec<String>,
        psbt: String,
    ) -> Result<String, RlnSignerError> {
        self.backend.sign_rgb_psbt(descriptors, psbt)
    }
}
