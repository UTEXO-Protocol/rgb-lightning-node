use crate::rgb_kv_store::RgbKvStoreExt;
use crate::signer::{ActiveSigner, ActiveSignerRef, ExternalSigner, RlnKeysInterface};
use bitcoin::bip32::ChildNumber;
use bitcoin::bip32::Xpub;
use bitcoin::blockdata::constants::WITNESS_SCALE_FACTOR;
use bitcoin::blockdata::script::ScriptBuf;
use bitcoin::hashes::Hash;
use bitcoin::key::CompressedPublicKey;
use bitcoin::key::XOnlyPublicKey;
use bitcoin::psbt::{ExtractTxError, Psbt};
use bitcoin::secp256k1::Secp256k1;
use bitcoin::{Address, Network, OutPoint, Transaction, TxOut, WPubkeyHash};
use hex::DisplayHex;
use lightning::events::bump_transaction::{Utxo, WalletSource};
use lightning::ln::types::ChannelId;
use lightning::rgb_utils::RgbInfo;
use lightning::sign::ChangeDestinationSource;
use lightning::util::async_poll::AsyncResult;
use lightning::util::persist::KVStoreSync;
use rgb_lib::{
    bdk_wallet::{LocalOutput, SignOptions},
    bitcoin::psbt::Psbt as BitcoinPsbt,
    wallet::{
        rust_only::{check_proxy_url, ColoringInfo},
        AssetCFA, AssetIFA, AssetNIA, AssetUDA, Assets, Balance, BtcBalance, Metadata, Online,
        OperationResult, ReceiveData, Recipient, RefreshResult, RgbWalletOpsOffline,
        RgbWalletOpsOnline, SendBeginResult, SinglesigKeys, Transaction as RgbLibTransaction,
        Transfer, TransportEndpoint, Unspent, Wallet as RgbLibWallet,
    },
    AssetSchema, Assignment, BitcoinNetwork, ContractId, Error as RgbLibError, Fascia, RgbTransfer,
    RgbTransport, RgbTxid, UpdateRes, WitnessOrd,
};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::{Arc, Mutex, MutexGuard};

use crate::{error::APIError, utils::UnlockedAppState};

/// When `sign_rgb_psbt` fails, internal mode falls back to the local RGB wallet; external mode does not.
fn resolve_rgb_psbt_signer_failure(
    external_signer_mode: bool,
    err: &crate::signer::RlnSignerError,
    local_sign_psbt: impl FnOnce() -> Result<String, RgbLibError>,
) -> Result<String, RgbLibError> {
    if external_signer_mode {
        tracing::error!(error = %err, "external signer RGB PSBT signing failed");
        Err(RgbLibError::Internal {
            details: format!("external signer RGB PSBT signing failed: {err}"),
        })
    } else {
        local_sign_psbt()
    }
}

pub(crate) fn rgb_signer_descriptors_for_psbt_with_context(
    rgb_wallet: &RgbLibWalletWrapper,
    signer: &ActiveSigner,
    external_signer: Option<&ExternalSigner>,
    external_signer_mode: bool,
    unsigned_psbt: &str,
) -> Result<Vec<String>, RgbLibError> {
    let psbt = Psbt::from_str(unsigned_psbt).map_err(|e| RgbLibError::Internal {
        details: format!("invalid unsigned PSBT for external signer: {e}"),
    })?;
    let vanilla_unspents = rgb_wallet.list_unspents_vanilla(1, true)?;
    let signer_account = signer.rgb_wallet_account();
    let network = Network::from_str(
        rgb_wallet
            .bitcoin_network()
            .to_string()
            .to_lowercase()
            .as_str(),
    )
    .map_err(|e| RgbLibError::Internal {
        details: format!("invalid bitcoin network for signer descriptor derivation: {e}"),
    })?;
    let secp = Secp256k1::verification_only();
    let account_xpubs = [
        Xpub::from_str(&signer_account.account_xpub_colored).ok(),
        Xpub::from_str(&signer_account.account_xpub_vanilla).ok(),
    ];

    let infer_keyindex_from_script = |script: &ScriptBuf| -> Option<u32> {
        for idx in 0u32..10_000 {
            let idx_child = ChildNumber::from_normal_idx(idx).ok()?;
            for base_xpub in account_xpubs.iter().flatten() {
                // Legacy one-level account child: /idx
                let one_level = base_xpub.derive_pub(&secp, &[idx_child]).ok()?;
                let one_level_cpk =
                    CompressedPublicKey::from_slice(&one_level.public_key.serialize()).ok()?;
                let one_level_p2wpkh = Address::p2wpkh(&one_level_cpk, network).script_pubkey();
                if &one_level_p2wpkh == script {
                    return Some(idx);
                }
                let (one_level_xonly, _) = one_level.public_key.x_only_public_key();
                let one_level_p2tr =
                    Address::p2tr(&secp, one_level_xonly, None, network).script_pubkey();
                if &one_level_p2tr == script {
                    return Some(idx);
                }

                // BIP86/BIP84 style branch paths: /0/idx and /1/idx.
                for branch in [0u32, 1u32] {
                    let branch_child = ChildNumber::from_normal_idx(branch).ok()?;
                    let child = base_xpub
                        .derive_pub(&secp, &[branch_child, idx_child])
                        .ok()?;
                    let cpk =
                        CompressedPublicKey::from_slice(&child.public_key.serialize()).ok()?;
                    let p2wpkh = Address::p2wpkh(&cpk, network).script_pubkey();
                    if &p2wpkh == script {
                        return Some(idx);
                    }
                    let (xonly, _) = child.public_key.x_only_public_key();
                    let p2tr = Address::p2tr(&secp, xonly, None, network).script_pubkey();
                    if &p2tr == script {
                        return Some(idx);
                    }
                }
            }
        }
        None
    };

    let mut by_outpoint = HashMap::with_capacity(vanilla_unspents.len());
    for u in vanilla_unspents {
        by_outpoint.insert((u.outpoint.txid, u.outpoint.vout), u);
    }

    let mut descriptors = Vec::with_capacity(psbt.inputs.len());
    for (idx, input) in psbt.inputs.iter().enumerate() {
        let prevout = psbt
            .unsigned_tx
            .input
            .get(idx)
            .ok_or_else(|| RgbLibError::Internal {
                details: format!("PSBT input index {idx} missing in unsigned tx"),
            })?
            .previous_output;
        let local = by_outpoint
            .get(&(prevout.txid, prevout.vout))
            .ok_or_else(|| RgbLibError::Internal {
                details: format!(
                    "cannot map PSBT input {}:{} to wallet unspents for external signer",
                    prevout.txid, prevout.vout
                ),
            })?;
        let witness_utxo = input
            .witness_utxo
            .as_ref()
            .ok_or_else(|| RgbLibError::Internal {
                details: format!("PSBT input index {idx} missing witness_utxo"),
            })?;
        let keyindex_from_psbt = input
            .bip32_derivation
            .values()
            .find_map(|(_, path)| path.as_ref().last().copied())
            .and_then(|cn| match cn {
                ChildNumber::Normal { index } => Some(index),
                ChildNumber::Hardened { .. } => None,
            });
        let signer_meta = external_signer.and_then(|s| {
            s.get_wallet_input_metadata(
                prevout.txid.to_string(),
                prevout.vout,
                Some(witness_utxo.script_pubkey.as_bytes().as_hex().to_string()),
                Some(witness_utxo.value.to_sat()),
            )
            .ok()
            .flatten()
        });
        let keyindex = if external_signer_mode {
            signer_meta
                .as_ref()
                .map(|m| m.keyindex)
                .ok_or_else(|| RgbLibError::Internal {
                    details: format!(
                        "external signer did not return wallet input metadata for {}:{}",
                        prevout.txid, prevout.vout
                    ),
                })?
        } else {
            signer_meta
                .as_ref()
                .map(|m| m.keyindex)
                .or_else(|| infer_keyindex_from_script(&witness_utxo.script_pubkey))
                .or(keyindex_from_psbt)
                .unwrap_or(local.derivation_index)
        };
        let descriptor = serde_json::json!({
            "txid": prevout.txid.to_string(),
            "outnum": prevout.vout,
            "amount": signer_meta.as_ref().map(|m| m.amount_sat).unwrap_or_else(|| witness_utxo.value.to_sat()),
            "keyindex": keyindex,
            "is_p2sh": signer_meta.as_ref().map(|m| m.is_p2sh).unwrap_or(false),
            "script_hex": signer_meta
                .as_ref()
                .map(|m| m.script_pubkey_hex.clone())
                .unwrap_or_else(|| witness_utxo.script_pubkey.as_bytes().as_hex().to_string()),
            "is_in_coinbase": false,
        });
        descriptors.push(descriptor.to_string());
    }
    Ok(descriptors)
}

impl UnlockedAppState {
    fn rgb_signer_descriptors_for_psbt(
        &self,
        unsigned_psbt: &str,
    ) -> Result<Vec<String>, RgbLibError> {
        rgb_signer_descriptors_for_psbt_with_context(
            self.rgb_wallet_wrapper.as_ref(),
            self.signer.as_ref(),
            self.external_signer.as_deref(),
            self.external_signer_mode,
            unsigned_psbt,
        )
    }

    pub(crate) fn rgb_blind_receive(
        &self,
        asset_id: Option<String>,
        assignment: Assignment,
        expiration_timestamp: Option<u64>,
        transport_endpoints: Vec<String>,
        min_confirmations: u8,
    ) -> Result<ReceiveData, RgbLibError> {
        self.rgb_wallet_wrapper.blind_receive(
            asset_id,
            assignment,
            expiration_timestamp,
            transport_endpoints,
            min_confirmations,
        )
    }

    pub(crate) fn rgb_consume_fascia(
        &self,
        fascia: Fascia,
        witness_ord: Option<WitnessOrd>,
    ) -> Result<(), RgbLibError> {
        self.rgb_wallet_wrapper.consume_fascia(fascia, witness_ord)
    }

    pub(crate) fn rgb_create_consignments(&self, psbt: String) -> Result<(), RgbLibError> {
        self.rgb_wallet_wrapper.create_consigments(psbt)
    }

    pub(crate) fn rgb_create_utxos(
        &self,
        up_to: bool,
        num: u8,
        size: u32,
        fee_rate: u64,
        skip_sync: bool,
    ) -> Result<u8, RgbLibError> {
        self.rgb_wallet_wrapper
            .create_utxos(up_to, num, size, fee_rate, skip_sync)
    }

    pub(crate) fn rgb_create_utxos_begin(
        &self,
        up_to: bool,
        num: u8,
        size: u32,
        fee_rate: u64,
        skip_sync: bool,
    ) -> Result<String, RgbLibError> {
        self.rgb_wallet_wrapper
            .create_utxos_begin(up_to, num, size, fee_rate, skip_sync)
    }

    pub(crate) fn rgb_create_utxos_end(
        &self,
        signed_psbt: String,
        skip_sync: bool,
    ) -> Result<u8, RgbLibError> {
        self.rgb_wallet_wrapper
            .create_utxos_end(signed_psbt, skip_sync)
    }

    pub(crate) fn rgb_fail_transfers(
        &self,
        batch_transfer_idx: Option<i32>,
        no_asset_only: bool,
        skip_sync: bool,
    ) -> Result<bool, RgbLibError> {
        self.rgb_wallet_wrapper
            .fail_transfers(batch_transfer_idx, no_asset_only, skip_sync)
    }

    pub(crate) fn rgb_get_address(&self) -> Result<String, RgbLibError> {
        self.rgb_wallet_wrapper.get_address()
    }

    pub(crate) fn rgb_get_asset_balance(
        &self,
        contract_id: ContractId,
    ) -> Result<Balance, RgbLibError> {
        self.rgb_wallet_wrapper.get_asset_balance(contract_id)
    }

    pub(crate) fn rgb_get_asset_metadata(
        &self,
        contract_id: ContractId,
    ) -> Result<Metadata, RgbLibError> {
        self.rgb_wallet_wrapper.get_asset_metadata(contract_id)
    }

    pub(crate) fn rgb_get_btc_balance(&self, skip_sync: bool) -> Result<BtcBalance, RgbLibError> {
        self.rgb_wallet_wrapper.get_btc_balance(skip_sync)
    }

    pub(crate) fn rgb_get_fee_estimation(&self, blocks: u16) -> Result<f64, RgbLibError> {
        self.rgb_wallet_wrapper.get_fee_estimation(blocks)
    }

    pub(crate) fn rgb_get_keys(&self) -> SinglesigKeys {
        self.rgb_wallet_wrapper.get_keys()
    }

    pub(crate) fn rgb_get_media_dir(&self) -> PathBuf {
        self.rgb_wallet_wrapper.get_media_dir()
    }

    pub(crate) fn rgb_get_send_consignment_path(
        &self,
        asset_id: &str,
        transfer_id: &str,
    ) -> PathBuf {
        self.rgb_wallet_wrapper
            .get_send_consignment_path(asset_id, transfer_id)
    }

    pub(crate) fn rgb_inflate(
        &self,
        asset_id: String,
        inflation_amounts: Vec<u64>,
        fee_rate: u64,
        min_confirmations: u8,
    ) -> Result<OperationResult, RgbLibError> {
        self.rgb_wallet_wrapper
            .inflate(asset_id, inflation_amounts, fee_rate, min_confirmations)
    }

    pub(crate) fn rgb_issue_asset_cfa(
        &self,
        name: String,
        details: Option<String>,
        precision: u8,
        amounts: Vec<u64>,
        file_path: Option<String>,
    ) -> Result<AssetCFA, RgbLibError> {
        self.rgb_wallet_wrapper
            .issue_asset_cfa(name, details, precision, amounts, file_path)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn rgb_issue_asset_ifa(
        &self,
        ticker: String,
        name: String,
        precision: u8,
        amounts: Vec<u64>,
        inflation_amounts: Vec<u64>,
        reject_list_url: Option<String>,
    ) -> Result<AssetIFA, RgbLibError> {
        self.rgb_wallet_wrapper.issue_asset_ifa(
            ticker,
            name,
            precision,
            amounts,
            inflation_amounts,
            reject_list_url,
        )
    }

    pub(crate) fn rgb_issue_asset_nia(
        &self,
        ticker: String,
        name: String,
        precision: u8,
        amounts: Vec<u64>,
    ) -> Result<AssetNIA, RgbLibError> {
        self.rgb_wallet_wrapper
            .issue_asset_nia(ticker, name, precision, amounts)
    }

    pub(crate) fn rgb_issue_asset_uda(
        &self,
        ticker: String,
        name: String,
        details: Option<String>,
        precision: u8,
        media_file_path: Option<String>,
        attachments_file_paths: Vec<String>,
    ) -> Result<AssetUDA, RgbLibError> {
        self.rgb_wallet_wrapper.issue_asset_uda(
            ticker,
            name,
            details,
            precision,
            media_file_path,
            attachments_file_paths,
        )
    }

    pub(crate) fn rgb_list_assets(
        &self,
        filter_asset_schemas: Vec<AssetSchema>,
    ) -> Result<Assets, RgbLibError> {
        self.rgb_wallet_wrapper.list_assets(filter_asset_schemas)
    }

    pub(crate) fn rgb_list_transactions(
        &self,
        skip_sync: bool,
    ) -> Result<Vec<RgbLibTransaction>, RgbLibError> {
        self.rgb_wallet_wrapper.list_transactions(skip_sync)
    }

    pub(crate) fn rgb_list_transfers(
        &self,
        asset_id: String,
    ) -> Result<Vec<Transfer>, RgbLibError> {
        self.rgb_wallet_wrapper.list_transfers(asset_id)
    }

    pub(crate) fn rgb_list_unspents(&self, skip_sync: bool) -> Result<Vec<Unspent>, RgbLibError> {
        self.rgb_wallet_wrapper.list_unspents(skip_sync)
    }

    pub(crate) fn rgb_post_consignment<P: AsRef<Path>>(
        &self,
        proxy_url: &str,
        recipient_id: String,
        consignment_path: P,
        txid: String,
        vout: Option<u32>,
    ) -> Result<(), RgbLibError> {
        self.rgb_wallet_wrapper.post_consignment(
            proxy_url,
            recipient_id,
            consignment_path,
            txid,
            vout,
        )
    }

    pub(crate) fn rgb_refresh(&self, skip_sync: bool) -> Result<RefreshResult, RgbLibError> {
        self.rgb_wallet_wrapper.refresh(skip_sync)
    }

    pub(crate) fn rgb_save_new_asset(
        &self,
        consignment: RgbTransfer,
        offchain_txid: String,
    ) -> Result<(), RgbLibError> {
        self.rgb_wallet_wrapper
            .save_new_asset(consignment, offchain_txid)
    }

    pub(crate) fn rgb_send(
        &self,
        recipient_map: HashMap<String, Vec<Recipient>>,
        donation: bool,
        fee_rate: u64,
        min_confirmations: u8,
        expiration_timestamp: Option<u64>,
        skip_sync: bool,
    ) -> Result<OperationResult, RgbLibError> {
        self.rgb_wallet_wrapper.send(
            recipient_map,
            donation,
            fee_rate,
            min_confirmations,
            expiration_timestamp,
            skip_sync,
        )
    }

    pub(crate) fn rgb_send_begin(
        &self,
        recipient_map: HashMap<String, Vec<Recipient>>,
        donation: bool,
        fee_rate: u64,
        min_confirmations: u8,
        expiration_timestamp: Option<u64>,
        dry_run: bool,
    ) -> Result<SendBeginResult, RgbLibError> {
        self.rgb_wallet_wrapper.send_begin(
            recipient_map,
            donation,
            fee_rate,
            min_confirmations,
            expiration_timestamp,
            dry_run,
        )
    }

    pub(crate) fn rgb_send_btc(
        &self,
        address: String,
        amount: u64,
        fee_rate: u64,
        skip_sync: bool,
    ) -> Result<String, RgbLibError> {
        self.rgb_wallet_wrapper
            .send_btc(address, amount, fee_rate, skip_sync)
    }

    pub(crate) fn rgb_send_btc_begin(
        &self,
        address: String,
        amount: u64,
        fee_rate: u64,
    ) -> Result<String, RgbLibError> {
        self.rgb_wallet_wrapper
            .send_btc_begin(address, amount, fee_rate)
    }

    pub(crate) fn rgb_send_btc_end(&self, signed_psbt: String) -> Result<String, RgbLibError> {
        self.rgb_wallet_wrapper.send_btc_end(signed_psbt)
    }

    pub(crate) fn rgb_send_end(&self, signed_psbt: String) -> Result<OperationResult, RgbLibError> {
        self.rgb_wallet_wrapper.send_end(signed_psbt)
    }

    pub(crate) fn rgb_sign_psbt(&self, unsigned_psbt: String) -> Result<String, RgbLibError> {
        let signer_descriptors = if self.external_signer_mode {
            self.rgb_signer_descriptors_for_psbt(unsigned_psbt.as_str())?
        } else {
            vec![]
        };
        match self
            .signer
            .sign_rgb_psbt(signer_descriptors, unsigned_psbt.clone())
        {
            Ok(signed) => Ok(signed),
            Err(e) => {
                let w = Arc::clone(&self.rgb_wallet_wrapper);
                let unsigned = unsigned_psbt;
                resolve_rgb_psbt_signer_failure(self.external_signer_mode, &e, move || {
                    w.sign_psbt(unsigned)
                })
            }
        }
    }

    pub(crate) fn rgb_sync(&self) -> Result<(), RgbLibError> {
        self.rgb_wallet_wrapper.sync()
    }

    pub(crate) fn rgb_upsert_witness(
        &self,
        witness_id: RgbTxid,
        witness_ord: WitnessOrd,
    ) -> Result<(), RgbLibError> {
        self.rgb_wallet_wrapper
            .upsert_witness(witness_id, witness_ord)
    }

    pub(crate) fn rgb_witness_receive(
        &self,
        asset_id: Option<String>,
        assignment: Assignment,
        expiration_timestamp: Option<u64>,
        transport_endpoints: Vec<String>,
        min_confirmations: u8,
    ) -> Result<ReceiveData, RgbLibError> {
        self.rgb_wallet_wrapper.witness_receive(
            asset_id,
            assignment,
            expiration_timestamp,
            transport_endpoints,
            min_confirmations,
        )
    }
}

pub(crate) struct RgbLibWalletWrapper {
    pub(crate) wallet: Arc<Mutex<RgbLibWallet>>,
    pub(crate) online: Online,
}

impl RgbLibWalletWrapper {
    pub(crate) fn new(wallet: Arc<Mutex<RgbLibWallet>>, online: Online) -> Self {
        RgbLibWalletWrapper { wallet, online }
    }

    pub(crate) fn get_rgb_wallet(&self) -> MutexGuard<'_, RgbLibWallet> {
        // Recover from poisoning instead of unwrapping: a panic crossing
        // `extern "C"` frames aborts via `panic_cannot_unwind`, which would
        // cascade to every subsequent wallet op.
        match self.wallet.lock() {
            Ok(guard) => guard,
            Err(poisoned) => {
                tracing::error!(
                    "RgbLibWallet mutex was poisoned by a prior panic; recovering. \
                     Wallet state may be inconsistent — inspect earlier 'panicked at' lines."
                );
                poisoned.into_inner()
            }
        }
    }

    pub(crate) fn bitcoin_network(&self) -> BitcoinNetwork {
        self.get_rgb_wallet().get_wallet_data().bitcoin_network
    }

    pub(crate) fn blind_receive(
        &self,
        asset_id: Option<String>,
        assignment: Assignment,
        expiration_timestamp: Option<u64>,
        transport_endpoints: Vec<String>,
        min_confirmations: u8,
    ) -> Result<ReceiveData, RgbLibError> {
        self.get_rgb_wallet().blind_receive(
            asset_id,
            assignment,
            expiration_timestamp,
            transport_endpoints,
            min_confirmations,
        )
    }

    pub(crate) fn consume_fascia(
        &self,
        fascia: Fascia,
        witness_ord: Option<WitnessOrd>,
    ) -> Result<(), RgbLibError> {
        self.get_rgb_wallet().consume_fascia(fascia, witness_ord)
    }

    pub(crate) fn color_psbt_and_consume(
        &self,
        psbt_to_color: &mut BitcoinPsbt,
        coloring_info: ColoringInfo,
    ) -> Result<Vec<RgbTransfer>, RgbLibError> {
        self.get_rgb_wallet()
            .color_psbt_and_consume(psbt_to_color, coloring_info)
    }

    pub(crate) fn create_consigments(&self, psbt: String) -> Result<(), RgbLibError> {
        self.get_rgb_wallet().create_consignments(psbt)
    }

    pub(crate) fn create_utxos(
        &self,
        up_to: bool,
        num: u8,
        size: u32,
        fee_rate: u64,
        skip_sync: bool,
    ) -> Result<u8, RgbLibError> {
        self.get_rgb_wallet().create_utxos(
            self.online,
            up_to,
            Some(num),
            Some(size),
            fee_rate,
            skip_sync,
        )
    }

    pub(crate) fn create_utxos_begin(
        &self,
        up_to: bool,
        num: u8,
        size: u32,
        fee_rate: u64,
        skip_sync: bool,
    ) -> Result<String, RgbLibError> {
        self.get_rgb_wallet().create_utxos_begin(
            self.online,
            up_to,
            Some(num),
            Some(size),
            fee_rate,
            skip_sync,
        )
    }

    pub(crate) fn create_utxos_end(
        &self,
        signed_psbt: String,
        skip_sync: bool,
    ) -> Result<u8, RgbLibError> {
        self.get_rgb_wallet()
            .create_utxos_end(self.online, signed_psbt, skip_sync)
    }

    pub(crate) fn fail_transfers(
        &self,
        batch_transfer_idx: Option<i32>,
        no_asset_only: bool,
        skip_sync: bool,
    ) -> Result<bool, RgbLibError> {
        self.get_rgb_wallet().fail_transfers(
            self.online,
            batch_transfer_idx,
            no_asset_only,
            skip_sync,
        )
    }

    pub(crate) fn get_address(&self) -> Result<String, RgbLibError> {
        self.get_rgb_wallet().get_address()
    }

    pub(crate) fn get_asset_balance(
        &self,
        contract_id: ContractId,
    ) -> Result<Balance, RgbLibError> {
        self.get_rgb_wallet()
            .get_asset_balance(contract_id.to_string())
    }

    pub(crate) fn get_asset_metadata(
        &self,
        contract_id: ContractId,
    ) -> Result<Metadata, RgbLibError> {
        self.get_rgb_wallet()
            .get_asset_metadata(contract_id.to_string())
    }

    pub(crate) fn get_btc_balance(&self, skip_sync: bool) -> Result<BtcBalance, RgbLibError> {
        let online = if skip_sync { None } else { Some(self.online) };
        self.get_rgb_wallet().get_btc_balance(online, skip_sync)
    }

    pub(crate) fn get_fee_estimation(&self, blocks: u16) -> Result<f64, RgbLibError> {
        self.get_rgb_wallet()
            .get_fee_estimation(self.online, blocks)
    }

    pub(crate) fn get_keys(&self) -> SinglesigKeys {
        self.get_rgb_wallet().get_keys()
    }

    pub(crate) fn get_media_dir(&self) -> PathBuf {
        self.get_rgb_wallet().get_media_dir()
    }

    pub(crate) fn get_send_consignment_path(&self, asset_id: &str, transfer_id: &str) -> PathBuf {
        self.get_rgb_wallet()
            .get_send_consignment_path(asset_id, transfer_id)
    }

    pub(crate) fn get_tx_height(&self, txid: String) -> Result<Option<u32>, RgbLibError> {
        self.get_rgb_wallet().get_tx_height(txid)
    }

    pub(crate) fn inflate(
        &self,
        asset_id: String,
        inflation_amounts: Vec<u64>,
        fee_rate: u64,
        min_confirmations: u8,
    ) -> Result<OperationResult, RgbLibError> {
        self.get_rgb_wallet().inflate(
            self.online,
            asset_id,
            inflation_amounts,
            fee_rate,
            min_confirmations,
        )
    }

    pub(crate) fn issue_asset_cfa(
        &self,
        name: String,
        details: Option<String>,
        precision: u8,
        amounts: Vec<u64>,
        file_path: Option<String>,
    ) -> Result<AssetCFA, RgbLibError> {
        self.get_rgb_wallet()
            .issue_asset_cfa(name, details, precision, amounts, file_path)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn issue_asset_ifa(
        &self,
        ticker: String,
        name: String,
        precision: u8,
        amounts: Vec<u64>,
        inflation_amounts: Vec<u64>,
        reject_list_url: Option<String>,
    ) -> Result<AssetIFA, RgbLibError> {
        self.get_rgb_wallet().issue_asset_ifa(
            ticker,
            name,
            precision,
            amounts,
            inflation_amounts,
            reject_list_url,
        )
    }

    pub(crate) fn issue_asset_nia(
        &self,
        ticker: String,
        name: String,
        precision: u8,
        amounts: Vec<u64>,
    ) -> Result<AssetNIA, RgbLibError> {
        self.get_rgb_wallet()
            .issue_asset_nia(ticker, name, precision, amounts)
    }

    pub(crate) fn issue_asset_uda(
        &self,
        ticker: String,
        name: String,
        details: Option<String>,
        precision: u8,
        media_file_path: Option<String>,
        attachments_file_paths: Vec<String>,
    ) -> Result<AssetUDA, RgbLibError> {
        self.get_rgb_wallet().issue_asset_uda(
            ticker,
            name,
            details,
            precision,
            media_file_path,
            attachments_file_paths,
        )
    }

    pub(crate) fn list_assets(
        &self,
        filter_asset_schemas: Vec<AssetSchema>,
    ) -> Result<Assets, RgbLibError> {
        self.get_rgb_wallet().list_assets(filter_asset_schemas)
    }

    pub(crate) fn list_transactions(
        &self,
        skip_sync: bool,
    ) -> Result<Vec<RgbLibTransaction>, RgbLibError> {
        let online = if skip_sync { None } else { Some(self.online) };
        self.get_rgb_wallet().list_transactions(online, skip_sync)
    }

    pub(crate) fn list_transfers(&self, asset_id: String) -> Result<Vec<Transfer>, RgbLibError> {
        self.get_rgb_wallet().list_transfers(Some(asset_id))
    }

    pub(crate) fn list_unspents(&self, skip_sync: bool) -> Result<Vec<Unspent>, RgbLibError> {
        let online = if skip_sync { None } else { Some(self.online) };
        self.get_rgb_wallet()
            .list_unspents(online, false, skip_sync)
    }

    pub(crate) fn list_unspents_vanilla(
        &self,
        min_confirmations: u8,
        skip_sync: bool,
    ) -> Result<Vec<LocalOutput>, RgbLibError> {
        self.get_rgb_wallet()
            .list_unspents_vanilla(self.online, min_confirmations, skip_sync)
    }

    pub(crate) fn post_consignment<P: AsRef<Path>>(
        &self,
        proxy_url: &str,
        recipient_id: String,
        consignment_path: P,
        txid: String,
        vout: Option<u32>,
    ) -> Result<(), RgbLibError> {
        self.get_rgb_wallet().post_consignment(
            proxy_url,
            recipient_id,
            consignment_path,
            txid,
            vout,
        )
    }

    pub(crate) fn refresh(&self, skip_sync: bool) -> Result<RefreshResult, RgbLibError> {
        self.get_rgb_wallet()
            .refresh(self.online, None, vec![], skip_sync)
    }

    pub(crate) fn save_new_asset(
        &self,
        consignment: RgbTransfer,
        offchain_txid: String,
    ) -> Result<(), RgbLibError> {
        self.get_rgb_wallet()
            .save_new_asset(consignment, offchain_txid)
    }

    pub(crate) fn send(
        &self,
        recipient_map: HashMap<String, Vec<Recipient>>,
        donation: bool,
        fee_rate: u64,
        min_confirmations: u8,
        expiration_timestamp: Option<u64>,
        skip_sync: bool,
    ) -> Result<OperationResult, RgbLibError> {
        self.get_rgb_wallet().send(
            self.online,
            recipient_map,
            donation,
            fee_rate,
            min_confirmations,
            expiration_timestamp,
            skip_sync,
        )
    }

    pub(crate) fn send_begin(
        &self,
        recipient_map: HashMap<String, Vec<Recipient>>,
        donation: bool,
        fee_rate: u64,
        min_confirmations: u8,
        expiration_timestamp: Option<u64>,
        dry_run: bool,
    ) -> Result<SendBeginResult, RgbLibError> {
        self.get_rgb_wallet().send_begin(
            self.online,
            recipient_map,
            donation,
            fee_rate,
            min_confirmations,
            expiration_timestamp,
            dry_run,
        )
    }

    pub(crate) fn send_btc(
        &self,
        address: String,
        amount: u64,
        fee_rate: u64,
        skip_sync: bool,
    ) -> Result<String, RgbLibError> {
        self.get_rgb_wallet()
            .send_btc(self.online, address, amount, fee_rate, skip_sync)
    }

    pub(crate) fn send_btc_begin(
        &self,
        address: String,
        amount: u64,
        fee_rate: u64,
    ) -> Result<String, RgbLibError> {
        self.get_rgb_wallet()
            .send_btc_begin(self.online, address, amount, fee_rate, false)
    }

    pub(crate) fn send_btc_end(&self, signed_psbt: String) -> Result<String, RgbLibError> {
        self.get_rgb_wallet()
            .send_btc_end(self.online, signed_psbt, false)
    }

    pub(crate) fn send_end(&self, signed_psbt: String) -> Result<OperationResult, RgbLibError> {
        self.get_rgb_wallet()
            .send_end(self.online, signed_psbt, false)
    }

    pub(crate) fn sign_psbt(&self, unsigned_psbt: String) -> Result<String, RgbLibError> {
        let sign_options = SignOptions {
            trust_witness_utxo: true,
            try_finalize: true,
            sign_with_tap_internal_key: true,
            ..Default::default()
        };
        let signed = self
            .get_rgb_wallet()
            .sign_psbt(unsigned_psbt, Some(sign_options.clone()))?;
        self.get_rgb_wallet()
            .finalize_psbt(signed, Some(sign_options))
    }

    pub(crate) fn sync(&self) -> Result<(), RgbLibError> {
        self.get_rgb_wallet().sync(self.online)
    }

    pub(crate) fn update_witnesses(
        &self,
        after_height: u32,
        force_witnesses: Vec<RgbTxid>,
    ) -> Result<UpdateRes, RgbLibError> {
        self.get_rgb_wallet()
            .update_witnesses(after_height, force_witnesses)
    }

    pub(crate) fn upsert_witness(
        &self,
        witness_id: RgbTxid,
        witness_ord: WitnessOrd,
    ) -> Result<(), RgbLibError> {
        self.get_rgb_wallet()
            .upsert_witness(witness_id, witness_ord)
    }

    pub(crate) fn witness_receive(
        &self,
        asset_id: Option<String>,
        assignment: Assignment,
        expiration_timestamp: Option<u64>,
        transport_endpoints: Vec<String>,
        min_confirmations: u8,
    ) -> Result<ReceiveData, RgbLibError> {
        self.get_rgb_wallet().witness_receive(
            asset_id,
            assignment,
            expiration_timestamp,
            transport_endpoints,
            min_confirmations,
        )
    }
}

/// [`WalletSource`] for LDK bump-tx coin selection: in external signer mode PSBT inputs are signed
/// via [`RlnKeysInterface::sign_rgb_psbt`] instead of the watch-only RGB wallet.
pub(crate) struct RgbBumpWalletSource {
    pub(crate) inner: Arc<RgbLibWalletWrapper>,
    pub(crate) signer: ActiveSignerRef,
    pub(crate) external_signer: Option<Arc<ExternalSigner>>,
    pub(crate) external_signer_mode: bool,
}

impl WalletSource for RgbBumpWalletSource {
    fn list_confirmed_utxos<'a>(&'a self) -> AsyncResult<'a, Vec<Utxo>, ()> {
        self.inner.as_ref().list_confirmed_utxos()
    }

    fn get_change_script<'a>(&'a self) -> AsyncResult<'a, ScriptBuf, ()> {
        self.inner.as_ref().get_change_script()
    }

    fn sign_psbt<'a>(&'a self, tx: Psbt) -> AsyncResult<'a, Transaction, ()> {
        if !self.external_signer_mode {
            return WalletSource::sign_psbt(self.inner.as_ref(), tx);
        }
        let inner = Arc::clone(&self.inner);
        let signer = Arc::clone(&self.signer);
        let ext = self.external_signer.clone();
        Box::pin(async move {
            let unsigned_psbt = tx.to_string();
            let descriptors = match rgb_signer_descriptors_for_psbt_with_context(
                inner.as_ref(),
                signer.as_ref(),
                ext.as_deref(),
                true,
                unsigned_psbt.as_str(),
            ) {
                Ok(d) => d,
                Err(e) => {
                    tracing::error!(
                        error = %e,
                        "RGB bump wallet: descriptor preparation for external signer failed"
                    );
                    return Err(());
                }
            };
            let signed_hex = match signer.sign_rgb_psbt(descriptors, unsigned_psbt) {
                Ok(h) => h,
                Err(e) => {
                    tracing::error!(
                        error = %e,
                        "RGB bump wallet: external signer PSBT signing failed"
                    );
                    return Err(());
                }
            };
            let signed_psbt = match Psbt::from_str(&signed_hex) {
                Ok(p) => p,
                Err(e) => {
                    tracing::error!(
                        error = %e,
                        "RGB bump wallet: failed to parse PSBT returned by external signer"
                    );
                    return Err(());
                }
            };
            match signed_psbt.extract_tx() {
                Ok(t) => Ok(t),
                Err(ExtractTxError::MissingInputValue { tx }) => Ok(tx),
                Err(e) => {
                    tracing::error!(error = %e, "RGB bump wallet: extract_tx failed");
                    Err(())
                }
            }
        })
    }
}

impl ChangeDestinationSource for RgbLibWalletWrapper {
    fn get_change_destination_script<'a>(&'a self) -> AsyncResult<'a, ScriptBuf, ()> {
        Box::pin(async move {
            Ok(Address::from_str(&self.get_address().unwrap())
                .unwrap()
                .assume_checked()
                .script_pubkey())
        })
    }
}

impl WalletSource for RgbLibWalletWrapper {
    // All three methods return Result<_, ()> to LDK. Unwrapping internally would
    // poison the wallet mutex on panic and cascade aborts on every later op (see
    // `get_rgb_wallet`), so each fallible step early-returns Err(()) with a log.
    fn list_confirmed_utxos<'a>(&'a self) -> AsyncResult<'a, Vec<Utxo>, ()> {
        Box::pin(async move {
            let network = match Network::from_str(&self.bitcoin_network().to_string().to_lowercase()) {
                Ok(n) => n,
                Err(e) => {
                    tracing::error!(error = %e, "list_confirmed_utxos: failed to parse bitcoin network");
                    return Err(());
                }
            };
            let mut wallet = self.get_rgb_wallet();
            let unspents = match wallet.list_unspents_vanilla(self.online, 1, false) {
                Ok(u) => u,
                Err(e) => {
                    tracing::error!(error = ?e, "list_confirmed_utxos: list_unspents_vanilla failed");
                    return Err(());
                }
            };
            Ok(unspents.iter().filter_map(|u| {
                let script = u.txout.script_pubkey.clone().into_boxed_script();
                let address = match Address::from_script(&script, network) {
                    Ok(a) => a,
                    Err(e) => {
                        // Non-standard scripts are expected in mixed wallets — debug, not error.
                        tracing::debug!(error = %e, "list_confirmed_utxos: skipping non-address script");
                        return None;
                    }
                };
                let outpoint = match OutPoint::from_str(&u.outpoint.to_string()) {
                    Ok(o) => o,
                    Err(e) => {
                        tracing::error!(error = %e, outpoint = %u.outpoint, "list_confirmed_utxos: outpoint parse failed");
                        return None;
                    }
                };
                let value = u.txout.value;
                match address.witness_program() {
                    Some(prog) if prog.is_p2wpkh() => {
                        WPubkeyHash::from_slice(prog.program().as_bytes())
                            .map(|wpkh| Utxo::new_v0_p2wpkh(outpoint, value, &wpkh))
                            .ok()
                    },
                    Some(prog) if prog.is_p2tr() => {
                        // TODO: Add `Utxo::new_v1_p2tr` upstream.
                        XOnlyPublicKey::from_slice(prog.program().as_bytes())
                            .map(|_| Utxo {
                                outpoint,
                                output: TxOut {
                                    value,
                                    script_pubkey: ScriptBuf::new_witness_program(&prog),
                                },
                                #[allow(clippy::identity_op)]
                                satisfaction_weight: 1 /* empty script_sig */ * WITNESS_SCALE_FACTOR as u64 +
                                    1 /* witness items */ + 1 /* schnorr sig len */ + 64, /* schnorr sig */
                            })
                            .ok()
                    },
                    _ => None,
                }
            })
            .collect())
        })
    }

    fn get_change_script<'a>(&'a self) -> AsyncResult<'a, ScriptBuf, ()> {
        Box::pin(async move {
            let addr_str = match self.get_rgb_wallet().get_address() {
                Ok(a) => a,
                Err(e) => {
                    tracing::error!(error = ?e, "get_change_script: wallet.get_address failed");
                    return Err(());
                }
            };
            let addr = match Address::from_str(&addr_str) {
                Ok(a) => a.assume_checked(),
                Err(e) => {
                    tracing::error!(error = %e, addr = %addr_str, "get_change_script: address parse failed");
                    return Err(());
                }
            };
            Ok(addr.script_pubkey())
        })
    }

    fn sign_psbt<'a>(&'a self, tx: Psbt) -> AsyncResult<'a, Transaction, ()> {
        Box::pin(async move {
            let sign_options = SignOptions {
                trust_witness_utxo: true,
                ..Default::default()
            };
            let signed = match self
                .get_rgb_wallet()
                .sign_psbt(tx.to_string(), Some(sign_options))
            {
                Ok(s) => s,
                Err(e) => {
                    tracing::error!(error = ?e, "sign_psbt: wallet.sign_psbt failed");
                    return Err(());
                }
            };
            let psbt = match Psbt::from_str(&signed) {
                Ok(p) => p,
                Err(e) => {
                    tracing::error!(error = %e, "sign_psbt: PSBT parse failed after signing");
                    return Err(());
                }
            };
            psbt.extract_tx().map_err(|e| {
                tracing::error!(error = %e, "sign_psbt: extract_tx failed");
            })
        })
    }
}

pub(crate) async fn check_rgb_proxy_endpoint(proxy_endpoint: &str) -> Result<(), APIError> {
    let rgb_transport =
        RgbTransport::from_str(proxy_endpoint).map_err(|_| APIError::InvalidProxyEndpoint)?;
    let proxy_url = TransportEndpoint::try_from(rgb_transport)?.endpoint;
    tokio::task::spawn_blocking(move || check_proxy_url(&proxy_url))
        .await
        .unwrap()?;
    Ok(())
}

pub(crate) fn get_rgb_channel_info_optional(
    channel_id: &ChannelId,
    pending: bool,
    kv_store: &dyn KVStoreSync,
) -> Option<RgbInfo> {
    let channel_id_str = channel_id.0.as_hex().to_string();
    kv_store
        .read_rgb_channel_info(&channel_id_str, pending)
        .ok()
}

#[cfg(test)]
mod rgb_psbt_signer_failure_tests {
    use super::resolve_rgb_psbt_signer_failure;
    use crate::signer::RlnSignerError;

    #[test]
    fn external_mode_does_not_invoke_local_psbt_sign() {
        let err = RlnSignerError::Protocol("simulated".to_string());
        let res = resolve_rgb_psbt_signer_failure(true, &err, || {
            panic!("local PSBT signing must not run in external signer mode");
        });
        let err_out = res.expect_err("external signer failure must surface as RgbLibError");
        let msg = err_out.to_string();
        assert!(
            msg.contains("external signer RGB PSBT signing failed"),
            "{msg}"
        );
        assert!(msg.contains("simulated"), "{msg}");
    }

    #[test]
    fn internal_mode_falls_back_to_local_psbt_sign() {
        let err = RlnSignerError::Protocol("ignored".to_string());
        let res = resolve_rgb_psbt_signer_failure(false, &err, || Ok("signed-local".into()));
        assert_eq!(res.expect("fallback ok"), "signed-local");
    }

    #[test]
    fn external_mode_surfaces_transport_error_in_message() {
        let err = RlnSignerError::Transport("signer offline".to_string());
        let res = resolve_rgb_psbt_signer_failure(true, &err, || {
            panic!("local PSBT signing must not run in external signer mode");
        });
        let msg = res.expect_err("expected error").to_string();
        assert!(
            msg.contains("external signer RGB PSBT signing failed"),
            "{msg}"
        );
        assert!(msg.contains("signer offline"), "{msg}");
    }
}
