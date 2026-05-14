use std::path::PathBuf;
use std::sync::Arc;

use bitcoin::bip32::{ChildNumber, DerivationPath, Xpriv, Xpub};
use bitcoin::hex::DisplayHex;
use bitcoin::hex::FromHex;
use bitcoin::psbt::ExtractTxError;
use bitcoin::script::ScriptBuf;
use bitcoin::secp256k1::ecdh::SharedSecret;
use bitcoin::secp256k1::ecdsa::{RecoverableSignature, RecoveryId, Signature};
use bitcoin::secp256k1::{Message, PublicKey, Scalar};
use bitcoin::sighash::{EcdsaSighashType, SighashCache};
use bitcoin::Address;
use bitcoin::Psbt;
use lightning::io;
use lightning::ln::msgs::UnsignedGossipMessage;
use lightning::ln::script::ShutdownScript;
use lightning::offers::invoice::UnsignedBolt12Invoice;
use lightning::sign::{
    EntropySource, InMemorySigner, KeysManager, NodeSigner, OutputSpender, PeerStorageKey,
    Recipient, SignerProvider, SpendableOutputDescriptor,
};
use lightning::util::persist::KVStoreSync;
use lightning::util::ser::Writeable;
use lightning_invoice::RawBolt11Invoice;
use lightning_signer::channel::ChannelId as VlsChannelId;
use lightning_signer::signer::derive::{key_derive as vls_key_derive, KeyDerivationStyle};
use std::str::FromStr;

use super::channel_signer::ExternalChannelSigner;
use super::entropy::SystemEntropySource;
use super::transport::ExternalSignerTransport;
use super::types::{
    async_payments_root_seed_bytes, validate_bootstrap_ldk_auxiliary_keys, BootstrapData,
    DerivedAddressMatch, ExternalNodeRequest, ExternalNodeResponse, ExternalSignerRequest,
    ExternalSignerResponse, RgbWalletAccountInfo, RlnSignerError, SpendableOutputUtxo,
    WalletInputMetadata,
};
use super::vls_adapter::{ExternalSignerBackend, VlsSignerAdapter};
use super::RlnEntropySource;
use super::RlnKeysInterface;

type LdkAuxiliaryKeysTriple = ([u8; 32], [u8; 32], [u8; 32]);

struct NullKvStore;

impl KVStoreSync for NullKvStore {
    fn read(
        &self,
        _primary_namespace: &str,
        _secondary_namespace: &str,
        _key: &str,
    ) -> Result<Vec<u8>, io::Error> {
        Err(io::Error::new(io::ErrorKind::NotFound, "noop kv store"))
    }

    fn write(
        &self,
        _primary_namespace: &str,
        _secondary_namespace: &str,
        _key: &str,
        _buf: Vec<u8>,
    ) -> Result<(), io::Error> {
        Ok(())
    }

    fn remove(
        &self,
        _primary_namespace: &str,
        _secondary_namespace: &str,
        _key: &str,
        _lazy: bool,
    ) -> Result<(), io::Error> {
        Ok(())
    }

    fn list(
        &self,
        _primary_namespace: &str,
        _secondary_namespace: &str,
    ) -> Result<Vec<String>, io::Error> {
        Ok(Vec::new())
    }
}

/// Transport-backed external signer: LDK `NodeSigner` / channel / PSBT ops delegate to the host.
/// Inbound-payment, peer-storage, and receive-auth key material is read from bootstrap hex fields
/// (same 32-byte triple as [`lightning::sign::KeysManager`] uses for expanded / peer storage /
/// receive auth from the LDK seed — the host supplies them via [`BootstrapData`]).
/// When set, bootstrap `async_payments_root_seed_hex` carries the same 32-byte LDK/VLS node seed
/// for async LSP preimage derivation; empty means a legacy deterministic fallback.
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
    ldk_inbound_payment_key: [u8; 32],
    ldk_peer_storage_key: [u8; 32],
    ldk_receive_auth_key: [u8; 32],
    signer_seed: [u8; 32],
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
        let (ldk_inbound_payment_key, ldk_peer_storage_key, ldk_receive_auth_key) =
            Self::ldk_aux_from_bootstrap(&attachment.bootstrap)?;
        let signer_seed = async_payments_root_seed_bytes(&attachment.bootstrap)?;
        let backend: Arc<dyn ExternalSignerBackend> =
            Arc::new(VlsSignerAdapter::new(Arc::clone(&attachment.transport)));
        Ok(Self {
            backend,
            ldk_inbound_payment_key,
            ldk_peer_storage_key,
            ldk_receive_auth_key,
            signer_seed,
        })
    }

    fn ldk_aux_from_bootstrap(
        bootstrap: &BootstrapData,
    ) -> Result<LdkAuxiliaryKeysTriple, RlnSignerError> {
        validate_bootstrap_ldk_auxiliary_keys(bootstrap)?;
        let parse32 = |h: &str| -> Result<[u8; 32], RlnSignerError> {
            let v = Vec::<u8>::from_hex(h).map_err(|e| {
                RlnSignerError::Protocol(format!("invalid LDK auxiliary key hex: {e}"))
            })?;
            v.try_into().map_err(|_| {
                RlnSignerError::Protocol("LDK auxiliary key must decode to 32 bytes".into())
            })
        };
        Ok((
            parse32(&bootstrap.ldk_inbound_payment_key_hex)?,
            parse32(&bootstrap.ldk_peer_storage_key_hex)?,
            parse32(&bootstrap.ldk_receive_auth_key_hex)?,
        ))
    }

    pub(crate) fn bootstrap(&self) -> Result<BootstrapData, RlnSignerError> {
        self.backend.bootstrap()
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

    fn recipient_label(recipient: Recipient) -> &'static str {
        match recipient {
            Recipient::Node => "node",
            Recipient::PhantomNode => "phantom",
        }
    }

    fn spendable_descriptor_to_utxo(
        &self,
        d: &SpendableOutputDescriptor,
    ) -> Result<SpendableOutputUtxo, ()> {
        Ok(match d {
            SpendableOutputDescriptor::StaticOutput {
                outpoint,
                output,
                channel_keys_id,
            } => {
                let script_pubkey_hex = output.script_pubkey.to_hex_string();
                let mut keyindex = self
                    .find_keyindex_for_script(&script_pubkey_hex)?
                    .unwrap_or(0);
                if keyindex == 0 {
                    if let Some(channel_keys_id) = channel_keys_id {
                        if self
                            .backend
                            .node_get_destination_script(channel_keys_id.to_lower_hex_string())
                            .map(|script| script.eq_ignore_ascii_case(&script_pubkey_hex))
                            .unwrap_or(false)
                        {
                            let mut dbid_bytes = [0u8; 8];
                            dbid_bytes.copy_from_slice(&channel_keys_id[..8]);
                            keyindex = u64::from_be_bytes(dbid_bytes).try_into().map_err(|_| ())?;
                        }
                    }
                }
                SpendableOutputUtxo {
                    txid_hex: outpoint.txid.to_string(),
                    vout: outpoint.index as u32,
                    amount_sat: output.value.to_sat(),
                    keyindex,
                    is_p2sh: false,
                    script_pubkey_hex,
                    is_in_coinbase: false,
                }
            }
            SpendableOutputDescriptor::DelayedPaymentOutput(o) => SpendableOutputUtxo {
                txid_hex: o.outpoint.txid.to_string(),
                vout: o.outpoint.index as u32,
                amount_sat: o.output.value.to_sat(),
                keyindex: 0,
                is_p2sh: false,
                script_pubkey_hex: o.output.script_pubkey.to_hex_string(),
                is_in_coinbase: false,
            },
            SpendableOutputDescriptor::StaticPaymentOutput(o) => SpendableOutputUtxo {
                txid_hex: o.outpoint.txid.to_string(),
                vout: o.outpoint.index as u32,
                amount_sat: o.output.value.to_sat(),
                keyindex: 0,
                is_p2sh: false,
                script_pubkey_hex: o.output.script_pubkey.to_hex_string(),
                is_in_coinbase: false,
            },
        })
    }

    fn rgb_coin_type(rgb: bool) -> u32 {
        if rgb {
            827_167
        } else {
            1
        }
    }

    fn rgb_account_derivation_path(rgb: bool) -> DerivationPath {
        DerivationPath::from(vec![
            ChildNumber::from_hardened_idx(86).expect("valid purpose"),
            ChildNumber::from_hardened_idx(Self::rgb_coin_type(rgb)).expect("valid coin type"),
            ChildNumber::from_hardened_idx(0).expect("valid account"),
        ])
    }

    fn rgb_account_xpriv(&self, rgb: bool) -> Result<Xpriv, ()> {
        let secp = bitcoin::secp256k1::Secp256k1::signing_only();
        Xpriv::new_master(bitcoin::Network::Regtest, &self.signer_seed)
            .map_err(|_| ())?
            .derive_priv(&secp, &Self::rgb_account_derivation_path(rgb))
            .map_err(|_| ())
    }

    fn derivation_path_from_match(m: &DerivedAddressMatch) -> Result<DerivationPath, ()> {
        if m.derivation_path.is_empty() {
            return Ok(DerivationPath::from(Vec::<ChildNumber>::new()));
        }
        let mut path = Vec::new();
        for segment in m.derivation_path.split('/') {
            let idx = segment.parse::<u32>().map_err(|_| ())?;
            path.push(ChildNumber::from_normal_idx(idx).map_err(|_| ())?);
        }
        Ok(DerivationPath::from(path))
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

    fn find_keyindex_for_script(&self, script_pubkey_hex: &str) -> Result<Option<u32>, ()> {
        Ok(self
            .find_derivation_match_for_script(script_pubkey_hex)?
            .map(|m| m.keyindex))
    }

    fn sign_static_output_input(
        &self,
        outpoint: &lightning::chain::transaction::OutPoint,
        output: &bitcoin::TxOut,
        derived_match: &DerivedAddressMatch,
        psbt: &mut Psbt,
        secp_ctx: &bitcoin::secp256k1::Secp256k1<bitcoin::secp256k1::All>,
    ) -> Result<(), ()> {
        let input_idx = psbt
            .unsigned_tx
            .input
            .iter()
            .position(|i| {
                i.previous_output.txid == outpoint.txid
                    && i.previous_output.vout == outpoint.index as u32
            })
            .ok_or(())?;
        let account_xpriv = match derived_match.account_name.as_str() {
            "colored" => self.rgb_account_xpriv(true)?,
            "vanilla" => self.rgb_account_xpriv(false)?,
            _ => return Err(()),
        };
        let child_xpriv = account_xpriv
            .derive_priv(secp_ctx, &Self::derivation_path_from_match(derived_match)?)
            .map_err(|_| ())?;
        let pubkey = bitcoin::PublicKey::new(Xpub::from_priv(secp_ctx, &child_xpriv).public_key);
        let expected_script = Address::p2wpkh(
            &bitcoin::CompressedPublicKey::try_from(pubkey).map_err(|_| ())?,
            bitcoin::Network::Regtest,
        )
        .script_pubkey();
        if expected_script != output.script_pubkey {
            return Err(());
        }
        let sighash = Message::from(
            SighashCache::new(&psbt.unsigned_tx)
                .p2wpkh_signature_hash(
                    input_idx,
                    &expected_script,
                    output.value,
                    EcdsaSighashType::All,
                )
                .map_err(|_| ())?,
        );
        let sig = secp_ctx.sign_ecdsa(&sighash, &child_xpriv.private_key);
        let mut sig_ser = sig.serialize_der().to_vec();
        sig_ser.push(EcdsaSighashType::All as u8);
        psbt.inputs[input_idx].final_script_witness = Some(bitcoin::Witness::from_slice(&[
            &sig_ser,
            &pubkey.inner.serialize().to_vec(),
        ]));
        Ok(())
    }

    fn local_keys_manager(&self) -> KeysManager {
        KeysManager::new(
            &self.signer_seed,
            0,
            0,
            true,
            PathBuf::new(),
            Arc::new(NullKvStore),
        )
    }

    fn local_ldk_channel_keys_id(&self, channel_keys_id: &[u8; 32]) -> [u8; 32] {
        // The VLS-backed external signer encodes `channel_keys_id` as a dbid envelope:
        // first 8 bytes are a big-endian dbid, remaining bytes are zero. Convert it to the
        // actual LDK/VLS keys_id before deriving a local signer for spendable outputs.
        if channel_keys_id[8..].iter().all(|b| *b == 0) {
            let mut dbid_bytes = [0u8; 8];
            dbid_bytes.copy_from_slice(&channel_keys_id[..8]);
            let dbid = u64::from_be_bytes(dbid_bytes);
            let channel_id = VlsChannelId::new_from_peer_id_and_oid(&[0u8; 33], dbid);
            let key_derive = vls_key_derive(KeyDerivationStyle::Ldk, bitcoin::Network::Testnet);
            let channel_seed_base = key_derive.channels_seed(&self.signer_seed);
            key_derive.keys_id(channel_id, &channel_seed_base)
        } else {
            *channel_keys_id
        }
    }

    fn local_channel_signer(&self, channel_keys_id: &[u8; 32]) -> InMemorySigner {
        let ldk_channel_keys_id = self.local_ldk_channel_keys_id(channel_keys_id);
        self.local_keys_manager()
            .derive_channel_keys(&ldk_channel_keys_id)
    }

    fn find_psbt_input_idx(
        outpoint: &lightning::chain::transaction::OutPoint,
        psbt: &Psbt,
    ) -> Result<usize, ()> {
        psbt.unsigned_tx
            .input
            .iter()
            .position(|i| {
                i.previous_output.txid == outpoint.txid
                    && i.previous_output.vout == outpoint.index as u32
            })
            .ok_or(())
    }

    fn sign_static_payment_output_input(
        &self,
        descriptor: &lightning::sign::StaticPaymentOutputDescriptor,
        psbt: &mut Psbt,
        secp_ctx: &bitcoin::secp256k1::Secp256k1<bitcoin::secp256k1::All>,
    ) -> Result<(), ()> {
        let input_idx = Self::find_psbt_input_idx(&descriptor.outpoint, psbt)?;
        let witness = self
            .local_channel_signer(&descriptor.channel_keys_id)
            .sign_counterparty_payment_input(&psbt.unsigned_tx, input_idx, descriptor, secp_ctx)?;
        psbt.inputs[input_idx].final_script_witness = Some(witness);
        Ok(())
    }

    fn sign_delayed_payment_output_input(
        &self,
        descriptor: &lightning::sign::DelayedPaymentOutputDescriptor,
        psbt: &mut Psbt,
        secp_ctx: &bitcoin::secp256k1::Secp256k1<bitcoin::secp256k1::All>,
    ) -> Result<(), ()> {
        let input_idx = Self::find_psbt_input_idx(&descriptor.outpoint, psbt)?;
        let witness = self
            .local_channel_signer(&descriptor.channel_keys_id)
            .sign_dynamic_p2wsh_input(&psbt.unsigned_tx, input_idx, descriptor, secp_ctx)?;
        psbt.inputs[input_idx].final_script_witness = Some(witness);
        Ok(())
    }
}

impl EntropySource for ExternalSigner {
    fn get_secure_random_bytes(&self) -> [u8; 32] {
        RlnEntropySource::get_secure_random_bytes(&SystemEntropySource)
    }
}

impl NodeSigner for ExternalSigner {
    fn get_expanded_key(&self) -> lightning::ln::inbound_payment::ExpandedKey {
        lightning::ln::inbound_payment::ExpandedKey::new(self.ldk_inbound_payment_key)
    }

    fn get_peer_storage_key(&self) -> PeerStorageKey {
        PeerStorageKey {
            inner: self.ldk_peer_storage_key,
        }
    }

    fn get_receive_auth_key(&self) -> lightning::sign::ReceiveAuthKey {
        lightning::sign::ReceiveAuthKey(self.ldk_receive_auth_key)
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
        let req = ExternalSignerRequest::Node(ExternalNodeRequest::SignMessage {
            message: String::from_utf8(msg.to_vec()).map_err(|_| ())?,
        });
        let resp = self.backend.call(req).map_err(|_| ())?;
        let ExternalSignerResponse::Node(ExternalNodeResponse::Signature { signature_hex }) = resp
        else {
            return Err(());
        };
        Ok(signature_hex)
    }
}

impl SignerProvider for ExternalSigner {
    type EcdsaSigner = ExternalChannelSigner;

    fn generate_channel_keys_id(&self, inbound: bool, user_channel_id: u128) -> [u8; 32] {
        let keys_hex = self
            .generate_channel_keys_id(inbound, 0, user_channel_id)
            .unwrap_or_else(|e| {
                tracing::warn!(
                    %inbound,
                    %user_channel_id,
                    error = %e,
                    "external signer generate_channel_keys_id fallback"
                );
                "00".repeat(32)
            });
        let bytes = Vec::<u8>::from_hex(&keys_hex).unwrap_or_else(|_| vec![0u8; 32]);
        let mut out = [0u8; 32];
        if bytes.len() >= 32 {
            out.copy_from_slice(&bytes[..32]);
        }
        out
    }

    fn derive_channel_signer(&self, channel_keys_id: [u8; 32]) -> Self::EcdsaSigner {
        self.derive_channel_signer(0, channel_keys_id.to_lower_hex_string())
            .unwrap_or_else(|e| {
                tracing::warn!(
                    channel_keys_id = %channel_keys_id.to_lower_hex_string(),
                    error = %e,
                    "external signer derive_channel_signer fallback"
                );
                ExternalChannelSigner::new(
                    Arc::clone(&self.backend),
                    channel_keys_id.to_lower_hex_string(),
                    String::new(),
                    super::types::ChannelPublicKeys {
                        funding_pubkey_hex: "02".repeat(33),
                        revocation_basepoint_hex: "02".repeat(33),
                        payment_point_hex: "02".repeat(33),
                        delayed_payment_basepoint_hex: "02".repeat(33),
                        htlc_basepoint_hex: "02".repeat(33),
                    },
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
        mut psbt: Psbt,
        secp_ctx: &bitcoin::secp256k1::Secp256k1<bitcoin::secp256k1::All>,
    ) -> Result<Psbt, ()> {
        let mut backend_utxos = Vec::new();
        for spendable_descriptor in descriptors {
            match spendable_descriptor {
                SpendableOutputDescriptor::StaticOutput {
                    outpoint, output, ..
                } => {
                    let script_pubkey_hex = output.script_pubkey.to_hex_string();
                    if let Some(derived_match) =
                        self.find_derivation_match_for_script(&script_pubkey_hex)?
                    {
                        self.sign_static_output_input(
                            outpoint,
                            output,
                            &derived_match,
                            &mut psbt,
                            secp_ctx,
                        )?;
                    } else {
                        backend_utxos
                            .push(self.spendable_descriptor_to_utxo(spendable_descriptor)?);
                    }
                }
                SpendableOutputDescriptor::DelayedPaymentOutput(descriptor) => {
                    self.sign_delayed_payment_output_input(descriptor, &mut psbt, secp_ctx)?;
                }
                SpendableOutputDescriptor::StaticPaymentOutput(descriptor) => {
                    self.sign_static_payment_output_input(descriptor, &mut psbt, secp_ctx)?;
                }
            }
        }
        if !backend_utxos.is_empty() {
            let signed_psbt = self
                .backend
                .sign_spendable_outputs_psbt(backend_utxos, psbt.to_string())
                .map_err(|_| ())?;
            psbt = Psbt::from_str(&signed_psbt).map_err(|_| ())?;
        }
        Ok(psbt)
    }

    fn sign_rgb_psbt(
        &self,
        descriptors: Vec<String>,
        psbt: String,
    ) -> Result<String, RlnSignerError> {
        self.backend.sign_rgb_psbt(descriptors, psbt)
    }

    fn rgb_wallet_account(&self) -> RgbWalletAccountInfo {
        match self.bootstrap() {
            Ok(bootstrap) => RgbWalletAccountInfo {
                account_xpub_vanilla: bootstrap.identity.account_xpub_vanilla,
                account_xpub_colored: bootstrap.identity.account_xpub_colored,
                master_fingerprint: bootstrap.identity.master_fingerprint,
                vanilla_keychain: None,
            },
            Err(_) => RgbWalletAccountInfo {
                account_xpub_vanilla: String::new(),
                account_xpub_colored: String::new(),
                master_fingerprint: String::new(),
                vanilla_keychain: None,
            },
        }
    }
}
