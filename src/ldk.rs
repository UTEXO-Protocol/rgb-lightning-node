use amplify::hex::ToHex;
use amplify::{map, s};
use bitcoin::bip32::DerivationPath;
use bitcoin::blockdata::locktime::absolute::LockTime;
use bitcoin::psbt::{ExtractTxError, Psbt};
use bitcoin::secp256k1::{All, PublicKey, Secp256k1};
use bitcoin::sighash::{self, SighashCache, TapSighashType};
use bitcoin::taproot::ControlBlock;
use bitcoin::transaction;
use bitcoin::{io, Amount, Network, PrivateKey};
use bitcoin::{BlockHash, OutPoint, Sequence, Transaction, TxIn, TxOut, Witness};
use bitcoin_bech32::WitnessProgram;
use lightning::chain::chaininterface::BroadcasterInterface;
use lightning::chain::{chainmonitor, ChannelMonitorUpdateStatus};
use lightning::chain::{BestBlock, Filter};
use lightning::events::bump_transaction::{BumpTransactionEventHandler, Wallet};
use lightning::events::{Event, PaymentFailureReason, PaymentPurpose, ReplayEvent};
use lightning::ln::channelmanager::{self, PaymentId, RecentPaymentDetails};
use lightning::ln::channelmanager::{
    ChainParameters, ChannelManagerReadArgs, SimpleArcChannelManager,
};
use lightning::ln::msgs::DecodeError;
use lightning::ln::msgs::SocketAddress;
use lightning::ln::peer_handler::{
    IgnoringMessageHandler, MessageHandler, PeerManager as LdkPeerManager,
};
use lightning::ln::types::ChannelId;
use lightning::onion_message::messenger::{
    DefaultMessageRouter, OnionMessenger as LdkOnionMessenger,
};
use lightning::rgb_utils::{
    get_rgb_channel_info_pending, is_channel_rgb, parse_rgb_payment_info, read_rgb_transfer_info,
    update_rgb_channel_amount, BITCOIN_NETWORK_FNAME, INDEXER_URL_FNAME, STATIC_BLINDING,
    WALLET_ACCOUNT_XPUB_COLORED_FNAME, WALLET_ACCOUNT_XPUB_VANILLA_FNAME, WALLET_FINGERPRINT_FNAME,
    WALLET_MASTER_FINGERPRINT_FNAME,
};
use lightning::routing::gossip;
use lightning::routing::gossip::{NodeId, P2PGossipSync};
use lightning::routing::router::DefaultRouter;
use lightning::routing::scoring::{ProbabilisticScorer, ProbabilisticScoringFeeParameters};
use lightning::sign::{
    EntropySource, InMemorySigner, KeysManager, NodeSigner, OutputSpender,
    SpendableOutputDescriptor,
};
use lightning::types::payment::{PaymentHash, PaymentPreimage};
use lightning::util::config::UserConfig;
use lightning::util::hash_tables::hash_map::Entry;
use lightning::util::hash_tables::HashMap as LdkHashMap;
use lightning::util::persist::{
    KVStoreSync, MonitorUpdatingPersister, OUTPUT_SWEEPER_PERSISTENCE_KEY,
    OUTPUT_SWEEPER_PERSISTENCE_PRIMARY_NAMESPACE, OUTPUT_SWEEPER_PERSISTENCE_SECONDARY_NAMESPACE,
};
use lightning::util::ser::{Readable, ReadableArgs, Writeable, Writer};
use lightning::util::sweep as ldk_sweep;
use lightning::{chain, impl_writeable_tlv_based, impl_writeable_tlv_based_enum};
use lightning_background_processor::{process_events_async, GossipSync, NO_LIQUIDITY_MANAGER};
use lightning_block_sync::gossip::TokioSpawner;
use lightning_block_sync::init;
use lightning_block_sync::poll;
use lightning_block_sync::SpvClient;
use lightning_block_sync::UnboundedCache;
use lightning_dns_resolver::OMDomainResolver;
use lightning_invoice::PaymentSecret;
use lightning_net_tokio::SocketDescriptor;
use lightning_persister::fs_store::FilesystemStore;
use rand::RngCore;
#[cfg(any(feature = "electrum", feature = "esplora"))]
use rgb_lib::RgbTransport;
use rgb_lib::{
    bdk_wallet::keys::{bip39::Mnemonic, DerivableKey, ExtendedKey},
    bitcoin::{
        bip32::{ChildNumber, Xpriv, Xpub},
        psbt::Psbt as RgbLibPsbt,
        secp256k1::Secp256k1 as Secp256k1_30,
        ScriptBuf,
    },
    utils::{get_account_data, recipient_id_from_script_buf, script_buf_from_recipient_id},
    wallet::{
        rust_only::{check_indexer_url, AssetColoringInfo, ColoringInfo},
        DatabaseType, Recipient, TransportEndpoint, Wallet as RgbLibWallet, WalletData,
        WitnessData,
    },
    AssetSchema, Assignment, BitcoinNetwork, ConsignmentExt, ContractId, FileContent, RgbTransfer,
    RgbTxid, WitnessOrd,
};
use std::collections::HashMap;
use std::convert::TryInto;
use std::fs;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::io::BufReader;
use std::net::ToSocketAddrs;
use std::net::{SocketAddr, TcpListener};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, RwLock};
use std::time::{Duration, SystemTime};
use time::OffsetDateTime;
use tokio::runtime::Handle;
use tokio::sync::watch::Sender;
use tokio::task::JoinHandle;

use crate::bitcoind::BitcoindClient;
use crate::disk::{
    self, FilesystemLogger, CHANNEL_IDS_FNAME, CHANNEL_PEER_DATA, CLAIMABLE_HTLCS_FNAME,
    INBOUND_PAYMENTS_FNAME, INVOICE_METADATA_FNAME, MAKER_SWAPS_FNAME, OUTBOUND_PAYMENTS_FNAME,
    OUTPUT_SPENDER_TXES, TAKER_SWAPS_FNAME,
};
use crate::error::APIError;
use crate::rgb::{check_rgb_proxy_endpoint, get_rgb_channel_info_optional, RgbLibWalletWrapper};
use crate::routes::{
    Assignment as RlnAssignment, HTLCStatus, SwapStatus, UnlockRequest, DUST_LIMIT_MSAT,
};
use crate::swap::SwapData;
use crate::utils::{
    check_port_is_available, connect_peer_if_necessary, derive_lp_htlc_xprv_from_path,
    do_connect_peer, get_current_timestamp, hex_str, hex_str_to_vec,
    htlc_full_path_from_payment_hash, AppState, StaticState, UnlockedAppState,
    ELECTRUM_URL_MAINNET, ELECTRUM_URL_REGTEST, ELECTRUM_URL_SIGNET, ELECTRUM_URL_TESTNET,
    ELECTRUM_URL_TESTNET4, PROXY_ENDPOINT_LOCAL, PROXY_ENDPOINT_PUBLIC,
};

pub(crate) const FEE_RATE: u64 = 7;
pub(crate) const UTXO_SIZE_SAT: u32 = 32000;
pub(crate) const MIN_CHANNEL_CONFIRMATIONS: u8 = 6;
pub(crate) const HTLC_TRACKER_FNAME: &str = "htlc_tracker.json";

pub(crate) struct LdkBackgroundServices {
    stop_processing: Arc<AtomicBool>,
    peer_manager: Arc<PeerManager>,
    bp_exit: Sender<()>,
    background_processor: Option<JoinHandle<Result<(), io::Error>>>,
    claimable_expiry_task: Option<JoinHandle<()>>,
}

#[derive(Clone, Debug)]
pub(crate) struct PaymentInfo {
    pub(crate) preimage: Option<PaymentPreimage>,
    pub(crate) secret: Option<PaymentSecret>,
    pub(crate) status: HTLCStatus,
    pub(crate) amt_msat: Option<u64>,
    pub(crate) created_at: u64,
    pub(crate) updated_at: u64,
    pub(crate) payee_pubkey: PublicKey,
}

impl_writeable_tlv_based!(PaymentInfo, {
    (0, preimage, required),
    (2, secret, required),
    (4, status, required),
    (6, amt_msat, required),
    (8, created_at, required),
    (10, updated_at, required),
    (12, payee_pubkey, required),
});

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum InvoiceMode {
    AutoClaim,
    Hodl,
}

impl_writeable_tlv_based_enum!(InvoiceMode,
    (0, AutoClaim) => {},
    (1, Hodl) => {},
);

#[derive(Clone, Debug)]
pub(crate) struct InvoiceMetadata {
    pub(crate) mode: InvoiceMode,
    pub(crate) expected_amt_msat: Option<u64>,
    pub(crate) expiry: Option<u64>,
    pub(crate) external_ref: Option<String>,
}

impl_writeable_tlv_based!(InvoiceMetadata, {
    (0, mode, required),
    (2, expected_amt_msat, required),
    (4, expiry, required),
    (6, external_ref, option),
});

#[derive(Clone, Debug)]
pub(crate) struct ClaimablePayment {
    pub(crate) payment_hash: PaymentHash,
    pub(crate) amount_msat: u64,
    pub(crate) invoice_expiry: Option<u64>,
    pub(crate) claim_deadline_height: Option<u32>,
    pub(crate) created_at: u64,
    pub(crate) settling: Option<bool>,
    pub(crate) settling_since: Option<u64>,
}

impl_writeable_tlv_based!(ClaimablePayment, {
    (0, payment_hash, required),
    (2, amount_msat, required),
    (4, invoice_expiry, required),
    (6, claim_deadline_height, required),
    (8, created_at, required),
    (10, settling, option),
    (12, settling_since, option),
});

pub(crate) struct InboundPaymentInfoStorage {
    pub(crate) payments: LdkHashMap<PaymentHash, PaymentInfo>,
}

impl_writeable_tlv_based!(InboundPaymentInfoStorage, {
    (0, payments, required),
});

pub(crate) struct InvoiceMetadataStorage {
    pub(crate) invoices: LdkHashMap<PaymentHash, InvoiceMetadata>,
}

impl_writeable_tlv_based!(InvoiceMetadataStorage, {
    (0, invoices, required),
});

pub(crate) struct OutboundPaymentInfoStorage {
    pub(crate) payments: LdkHashMap<PaymentId, PaymentInfo>,
}

impl_writeable_tlv_based!(OutboundPaymentInfoStorage, {
    (0, payments, required),
});

pub(crate) struct ClaimablePaymentStorage {
    pub(crate) payments: LdkHashMap<PaymentHash, ClaimablePayment>,
}

impl_writeable_tlv_based!(ClaimablePaymentStorage, {
    (0, payments, required),
});

pub(crate) struct SwapMap {
    pub(crate) swaps: LdkHashMap<PaymentHash, SwapData>,
}

impl_writeable_tlv_based!(SwapMap, {
    (0, swaps, required),
});

pub(crate) struct ChannelIdsMap {
    pub(crate) channel_ids: LdkHashMap<ChannelId, ChannelId>,
}

impl_writeable_tlv_based!(ChannelIdsMap, {
    (0, channel_ids, required),
});

impl UnlockedAppState {
    pub(crate) fn expire_claimables(
        &self,
        now_ts: u64,
        current_height: u32,
    ) -> Vec<ClaimablePayment> {
        let mut claimables = self.get_claimable_htlcs();
        let mut expired = vec![];
        let to_remove: Vec<PaymentHash> = claimables
            .payments
            .iter()
            .filter_map(|(hash, c)| {
                if c.settling.unwrap_or(false) {
                    if let Some(since) = c.settling_since {
                        const ONE_DAY_SECS: u64 = 24 * 60 * 60;
                        if now_ts <= since.saturating_add(ONE_DAY_SECS) {
                            return None;
                        }
                    } else {
                        return None;
                    }
                }
                let deadline_passed = c
                    .claim_deadline_height
                    .map(|h| current_height >= h)
                    .unwrap_or(false);
                let invoice_expired = c.invoice_expiry.map(|e| now_ts >= e).unwrap_or(false);
                if deadline_passed || invoice_expired {
                    Some(*hash)
                } else {
                    None
                }
            })
            .collect();

        for hash in to_remove.iter() {
            if let Some(c) = claimables.payments.remove(hash) {
                expired.push(c);
            }
        }
        if !to_remove.is_empty() {
            self.save_claimable_htlcs(claimables);
        }
        expired
    }

    pub(crate) fn add_maker_swap(&self, payment_hash: PaymentHash, swap: SwapData) {
        let mut maker_swaps = self.get_maker_swaps();
        maker_swaps.swaps.insert(payment_hash, swap);
        self.save_maker_swaps(maker_swaps);
    }

    pub(crate) fn update_maker_swap_status(&self, payment_hash: &PaymentHash, status: SwapStatus) {
        let mut maker_swaps = self.get_maker_swaps();
        let maker_swap = maker_swaps.swaps.get_mut(payment_hash).unwrap();
        match &status {
            SwapStatus::Succeeded | SwapStatus::Failed | SwapStatus::Expired => {
                maker_swap.completed_at = Some(get_current_timestamp())
            }
            SwapStatus::Pending => maker_swap.initiated_at = Some(get_current_timestamp()),
            SwapStatus::Waiting => panic!("this doesn't make sense: swap starts in Waiting status"),
        }
        maker_swap.status = status;
        self.save_maker_swaps(maker_swaps);
    }

    pub(crate) fn is_maker_swap(&self, payment_hash: &PaymentHash) -> bool {
        self.maker_swaps().contains_key(payment_hash)
    }

    pub(crate) fn add_taker_swap(&self, payment_hash: PaymentHash, swap: SwapData) {
        let mut taker_swaps = self.get_taker_swaps();
        taker_swaps.swaps.insert(payment_hash, swap);
        self.save_taker_swaps(taker_swaps);
    }

    pub(crate) fn update_taker_swap_status(&self, payment_hash: &PaymentHash, status: SwapStatus) {
        let mut taker_swaps = self.get_taker_swaps();
        let taker_swap = taker_swaps.swaps.get_mut(payment_hash).unwrap();
        match &status {
            SwapStatus::Succeeded | SwapStatus::Failed | SwapStatus::Expired => {
                taker_swap.completed_at = Some(get_current_timestamp())
            }
            SwapStatus::Pending => taker_swap.initiated_at = Some(get_current_timestamp()),
            SwapStatus::Waiting => panic!("this doesn't make sense: swap starts in Waiting status"),
        }
        taker_swap.status = status;
        self.save_taker_swaps(taker_swaps);
    }

    pub(crate) fn is_taker_swap(&self, payment_hash: &PaymentHash) -> bool {
        self.taker_swaps().contains_key(payment_hash)
    }

    fn save_maker_swaps(&self, swaps: MutexGuard<SwapMap>) {
        self.fs_store
            .write("", "", MAKER_SWAPS_FNAME, swaps.encode())
            .unwrap();
    }

    fn save_taker_swaps(&self, swaps: MutexGuard<SwapMap>) {
        self.fs_store
            .write("", "", TAKER_SWAPS_FNAME, swaps.encode())
            .unwrap();
    }

    pub(crate) fn maker_swaps(&self) -> LdkHashMap<PaymentHash, SwapData> {
        self.get_maker_swaps().swaps.clone()
    }

    pub(crate) fn taker_swaps(&self) -> LdkHashMap<PaymentHash, SwapData> {
        self.get_taker_swaps().swaps.clone()
    }

    pub(crate) fn add_inbound_payment(&self, payment_hash: PaymentHash, payment_info: PaymentInfo) {
        let mut inbound = self.get_inbound_payments();
        inbound.payments.insert(payment_hash, payment_info);
        self.save_inbound_payments(inbound);
    }

    pub(crate) fn add_hodl_invoice_records(
        &self,
        payment_hash: PaymentHash,
        payment_info: PaymentInfo,
        metadata: InvoiceMetadata,
    ) {
        let mut invoices = self.get_invoice_metadata();
        let mut inbound = self.get_inbound_payments();
        invoices.invoices.insert(payment_hash, metadata);
        inbound.payments.insert(payment_hash, payment_info);
        self.save_invoice_metadata(invoices);
        self.save_inbound_payments(inbound);
    }

    pub(crate) fn add_invoice_metadata(
        &self,
        payment_hash: PaymentHash,
        metadata: InvoiceMetadata,
    ) {
        let mut invoices = self.get_invoice_metadata();
        invoices.invoices.insert(payment_hash, metadata);
        self.save_invoice_metadata(invoices);
    }

    pub(crate) fn invoice_metadata(&self) -> LdkHashMap<PaymentHash, InvoiceMetadata> {
        self.get_invoice_metadata().invoices.clone()
    }

    pub(crate) fn upsert_claimable_payment(&self, claimable: ClaimablePayment) {
        let mut claimables = self.get_claimable_htlcs();
        claimables
            .payments
            .insert(claimable.payment_hash, claimable);
        self.save_claimable_htlcs(claimables);
    }

    pub(crate) fn take_claimable_payment(
        &self,
        payment_hash: &PaymentHash,
    ) -> Option<ClaimablePayment> {
        let mut claimables = self.get_claimable_htlcs();
        let res = claimables.payments.remove(payment_hash);
        if res.is_some() {
            self.save_claimable_htlcs(claimables);
        }
        res
    }

    pub(crate) fn mark_claimable_settling(
        &self,
        payment_hash: &PaymentHash,
        invoice_expiry: Option<u64>,
    ) -> Result<ClaimablePayment, APIError> {
        let mut claimables = self.get_claimable_htlcs();
        let Some(claimable) = claimables.payments.get_mut(payment_hash) else {
            return Err(APIError::InvoiceNotClaimable);
        };

        if claimable.settling.unwrap_or(false) {
            return Err(APIError::InvoiceSettlingInProgress);
        }

        let current_height = self.channel_manager.current_best_block().height;
        let now_ts = get_current_timestamp();

        if let Some(deadline_height) = claimable.claim_deadline_height {
            if current_height >= deadline_height {
                return Err(APIError::ClaimDeadlineExceeded);
            }
        }

        if let Some(expiry) = invoice_expiry {
            if now_ts >= expiry {
                return Err(APIError::InvoiceExpired);
            }
        }

        claimable.settling = Some(true);
        claimable.settling_since = Some(now_ts);
        let claimable_clone = claimable.clone();
        self.save_claimable_htlcs(claimables);

        Ok(claimable_clone)
    }

    pub(crate) fn claimable_payment(&self, payment_hash: &PaymentHash) -> Option<ClaimablePayment> {
        self.get_claimable_htlcs()
            .payments
            .get(payment_hash)
            .cloned()
    }

    pub(crate) fn add_outbound_payment(
        &self,
        payment_id: PaymentId,
        payment_info: PaymentInfo,
    ) -> Result<(), APIError> {
        let mut outbound = self.get_outbound_payments();
        if let Some(existing_payment) = outbound.payments.get(&payment_id) {
            if !matches!(existing_payment.status, HTLCStatus::Failed) {
                return Err(APIError::DuplicatePayment(
                    existing_payment.status.to_string(),
                ));
            }
        }
        outbound.payments.insert(payment_id, payment_info);
        self.save_outbound_payments(outbound);
        Ok(())
    }

    fn fail_outbound_pending_payments(&self, recent_payments_payment_ids: Vec<PaymentId>) {
        let mut outbound = self.get_outbound_payments();
        let mut failed = false;
        for (payment_id, payment_info) in outbound
            .payments
            .iter_mut()
            .filter(|(_, i)| matches!(i.status, HTLCStatus::Pending))
        {
            if !recent_payments_payment_ids.contains(payment_id) {
                payment_info.status = HTLCStatus::Failed;
                payment_info.updated_at = get_current_timestamp();
                failed = true;
            }
        }
        if failed {
            self.save_outbound_payments(outbound);
        }
    }

    pub(crate) fn inbound_payments(&self) -> LdkHashMap<PaymentHash, PaymentInfo> {
        self.get_inbound_payments().payments.clone()
    }

    pub(crate) fn outbound_payments(&self) -> LdkHashMap<PaymentId, PaymentInfo> {
        self.get_outbound_payments().payments.clone()
    }

    fn save_inbound_payments(&self, inbound: MutexGuard<InboundPaymentInfoStorage>) {
        self.fs_store
            .write("", "", INBOUND_PAYMENTS_FNAME, inbound.encode())
            .unwrap();
    }

    fn save_invoice_metadata(&self, invoices: MutexGuard<InvoiceMetadataStorage>) {
        self.fs_store
            .write("", "", INVOICE_METADATA_FNAME, invoices.encode())
            .unwrap();
    }

    fn save_outbound_payments(&self, outbound: MutexGuard<OutboundPaymentInfoStorage>) {
        self.fs_store
            .write("", "", OUTBOUND_PAYMENTS_FNAME, outbound.encode())
            .unwrap();
    }

    fn save_claimable_htlcs(&self, claimables: MutexGuard<ClaimablePaymentStorage>) {
        self.fs_store
            .write("", "", CLAIMABLE_HTLCS_FNAME, claimables.encode())
            .unwrap();
    }

    pub fn upsert_inbound_payment(
        &self,
        payment_hash: PaymentHash,
        status: HTLCStatus,
        preimage: Option<PaymentPreimage>,
        secret: Option<PaymentSecret>,
        amt_msat: Option<u64>,
        payee_pubkey: PublicKey,
    ) {
        let mut inbound = self.get_inbound_payments();
        match inbound.payments.entry(payment_hash) {
            Entry::Occupied(mut e) => {
                let payment_info = e.get_mut();
                payment_info.status = status;
                payment_info.preimage = preimage;
                payment_info.secret = secret;
                payment_info.updated_at = get_current_timestamp();
            }
            Entry::Vacant(e) => {
                let created_at = get_current_timestamp();
                e.insert(PaymentInfo {
                    preimage,
                    secret,
                    status,
                    amt_msat,
                    created_at,
                    updated_at: created_at,
                    payee_pubkey,
                });
            }
        }
        self.save_inbound_payments(inbound);
    }

    pub(crate) fn update_outbound_payment(
        &self,
        payment_id: PaymentId,
        status: HTLCStatus,
        preimage: Option<PaymentPreimage>,
    ) -> PaymentInfo {
        let mut outbound = self.get_outbound_payments();
        let payment_info = outbound.payments.get_mut(&payment_id).unwrap();
        payment_info.status = status;
        payment_info.preimage = preimage;
        payment_info.updated_at = get_current_timestamp();
        let payment = (*payment_info).clone();
        self.save_outbound_payments(outbound);
        payment
    }

    pub(crate) fn update_outbound_payment_status(&self, payment_id: PaymentId, status: HTLCStatus) {
        let mut outbound = self.get_outbound_payments();
        let payment_info = outbound.payments.get_mut(&payment_id).unwrap();
        payment_info.status = status;
        payment_info.updated_at = get_current_timestamp();
        self.save_outbound_payments(outbound);
    }

    pub(crate) fn channel_ids(&self) -> LdkHashMap<ChannelId, ChannelId> {
        self.get_channel_ids_map().channel_ids.clone()
    }

    pub(crate) fn add_channel_id(
        &self,
        former_temporary_channel_id: ChannelId,
        channel_id: ChannelId,
    ) {
        let mut channel_ids_map = self.get_channel_ids_map();
        channel_ids_map
            .channel_ids
            .insert(former_temporary_channel_id, channel_id);
        self.save_channel_ids_map(channel_ids_map);
    }

    pub(crate) fn delete_channel_id(&self, channel_id: ChannelId) {
        let mut channel_ids_map = self.get_channel_ids_map();
        if let Some(temporary_channel_id) = channel_ids_map
            .channel_ids
            .clone()
            .into_iter()
            .find_map(|(tmp_chan_id, chan_id)| {
                if chan_id == channel_id {
                    Some(tmp_chan_id)
                } else {
                    None
                }
            })
        {
            channel_ids_map.channel_ids.remove(&temporary_channel_id);
            self.save_channel_ids_map(channel_ids_map);
        }
    }

    fn save_channel_ids_map(&self, channel_ids: MutexGuard<ChannelIdsMap>) {
        self.fs_store
            .write("", "", CHANNEL_IDS_FNAME, channel_ids.encode())
            .unwrap();
    }
}

pub(crate) type ChainMonitor = chainmonitor::ChainMonitor<
    InMemorySigner,
    Arc<dyn Filter + Send + Sync>,
    Arc<BitcoindClient>,
    Arc<BitcoindClient>,
    Arc<FilesystemLogger>,
    Arc<
        MonitorUpdatingPersister<
            Arc<FilesystemStore>,
            Arc<FilesystemLogger>,
            Arc<KeysManager>,
            Arc<KeysManager>,
            Arc<BitcoindClient>,
            Arc<BitcoindClient>,
        >,
    >,
    Arc<KeysManager>,
>;

pub(crate) type GossipVerifier = lightning_block_sync::gossip::GossipVerifier<
    TokioSpawner,
    Arc<lightning_block_sync::rpc::RpcClient>,
    Arc<FilesystemLogger>,
>;

pub(crate) type PeerManager = LdkPeerManager<
    SocketDescriptor,
    Arc<ChannelManager>,
    Arc<P2PGossipSync<Arc<NetworkGraph>, Arc<GossipVerifier>, Arc<FilesystemLogger>>>,
    Arc<OnionMessenger>,
    Arc<FilesystemLogger>,
    IgnoringMessageHandler,
    Arc<KeysManager>,
    Arc<ChainMonitor>,
>;

pub(crate) type Scorer = ProbabilisticScorer<Arc<NetworkGraph>, Arc<FilesystemLogger>>;

pub(crate) type Router = DefaultRouter<
    Arc<NetworkGraph>,
    Arc<FilesystemLogger>,
    Arc<KeysManager>,
    Arc<RwLock<Scorer>>,
    ProbabilisticScoringFeeParameters,
    Scorer,
>;

pub(crate) type ChannelManager =
    SimpleArcChannelManager<ChainMonitor, BitcoindClient, BitcoindClient, FilesystemLogger>;

pub(crate) type NetworkGraph = gossip::NetworkGraph<Arc<FilesystemLogger>>;

pub(crate) type OnionMessenger = LdkOnionMessenger<
    Arc<KeysManager>,
    Arc<KeysManager>,
    Arc<FilesystemLogger>,
    Arc<ChannelManager>,
    Arc<DefaultMessageRouter<Arc<NetworkGraph>, Arc<FilesystemLogger>, Arc<KeysManager>>>,
    Arc<ChannelManager>,
    Arc<ChannelManager>,
    Arc<OMDomainResolver<Arc<ChannelManager>>>,
    IgnoringMessageHandler,
>;

pub(crate) type BumpTxEventHandler = BumpTransactionEventHandler<
    Arc<BitcoindClient>,
    Arc<Wallet<Arc<RgbLibWalletWrapper>, Arc<FilesystemLogger>>>,
    Arc<KeysManager>,
    Arc<FilesystemLogger>,
>;

pub(crate) type OutputSpenderTxes = LdkHashMap<u64, bitcoin::Transaction>;

pub(crate) struct RgbOutputSpender {
    static_state: Arc<StaticState>,
    rgb_wallet_wrapper: Arc<RgbLibWalletWrapper>,
    keys_manager: Arc<KeysManager>,
    fs_store: Arc<FilesystemStore>,
    txes: Arc<Mutex<OutputSpenderTxes>>,
    proxy_endpoint: String,
    lp_htlc_xprv: Xpriv,
    bitcoind_client: Arc<BitcoindClient>,
}

impl RgbOutputSpender {
    fn sweep_htlc_tracker(&self, secp_ctx: &Secp256k1<All>) -> bool {
        let network: Network = self.static_state.network.into();
        let mut tracker = disk::read_htlc_tracker(&self.static_state.ldk_data_dir);
        let mut updated = false;
        let mut accepted_any = false;
        let mut broadcast_txs: Vec<Transaction> = Vec::new();

        for (payment_hash, entry) in tracker.entries.iter_mut() {
            if entry.status == "SweepBroadcast" {
                if self.try_accept_sweep(entry) {
                    entry.status = "ClaimConfirmed".to_string();
                    updated = true;
                    accepted_any = true;
                }
                continue;
            }
            if entry.status != "ClaimRequested" {
                continue;
            }
            let Some(preimage_hex) = entry.preimage.as_ref() else {
                tracing::warn!("HTLC sweep skipped: missing preimage");
                continue;
            };
            let Some(preimage) = hex_str_to_vec(preimage_hex) else {
                tracing::warn!("HTLC sweep skipped: invalid preimage hex");
                continue;
            };
            let Some(claim_hex) = entry.claim_tapscript_hex.as_ref() else {
                tracing::warn!("HTLC sweep skipped: missing claim tapscript");
                continue;
            };
            let Some(claim_script_bytes) = hex_str_to_vec(claim_hex) else {
                tracing::warn!("HTLC sweep skipped: invalid claim tapscript hex");
                continue;
            };
            let Some(control_hex) = entry.control_block_hex.as_ref() else {
                tracing::warn!("HTLC sweep skipped: missing control block");
                continue;
            };
            let Some(control_block_bytes) = hex_str_to_vec(control_hex) else {
                tracing::warn!("HTLC sweep skipped: invalid control block hex");
                continue;
            };
            let Some(htlc_spk_bytes) = hex_str_to_vec(&entry.htlc_script_pubkey) else {
                tracing::warn!("HTLC sweep skipped: invalid htlc script pubkey hex");
                continue;
            };

            let claim_script = ScriptBuf::from_bytes(claim_script_bytes);
            let control_block = match ControlBlock::decode(&control_block_bytes) {
                Ok(cb) => cb,
                Err(_) => {
                    tracing::warn!("HTLC sweep skipped: invalid control block");
                    continue;
                }
            };
            let htlc_script_pubkey = ScriptBuf::from_bytes(htlc_spk_bytes);
            let control_ser = control_block.serialize();

            let key_path = if let Some(path) = entry.lp_key_path.as_deref() {
                path.to_string()
            } else {
                tracing::warn!(
                    "HTLC sweep: missing lp_key_path in tracker entry; falling back to payment hash"
                );
                htlc_full_path_from_payment_hash(network, payment_hash).to_string()
            };
            if entry.lp_key_path.is_none() {
                entry.lp_key_path = Some(key_path.clone());
                updated = true;
            }
            let child_xprv =
                match derive_lp_htlc_xprv_from_path(&self.lp_htlc_xprv, &key_path, network) {
                    Ok(xprv) => xprv,
                    Err(e) => {
                        tracing::warn!("HTLC sweep skipped: failed to derive LP HTLC key: {e}");
                        continue;
                    }
                };
            let btc_priv = PrivateKey::new(child_xprv.private_key, network);
            let pubkey = btc_priv.public_key(secp_ctx);
            let (xonly, _) = pubkey.inner.x_only_public_key();
            let derived_xonly_hex = xonly.serialize().to_hex();
            let entry_xonly = entry.lp_pubkey_xonly.trim();
            if entry_xonly.is_empty() {
                entry.lp_pubkey_xonly = derived_xonly_hex.clone();
                updated = true;
            } else if !entry_xonly.eq_ignore_ascii_case(&derived_xonly_hex) {
                tracing::warn!("HTLC sweep skipped: LP pubkey mismatch for payment hash");
                continue;
            }
            let keypair = match bitcoin::secp256k1::Keypair::from_seckey_slice(
                secp_ctx,
                btc_priv.inner.as_ref(),
            ) {
                Ok(kp) => kp,
                Err(_) => {
                    tracing::warn!("HTLC sweep skipped: failed to build keypair");
                    continue;
                }
            };

            let mut entry_broadcast = false;
            let mut vanilla_descs: Vec<&HtlcSpendableOutputDescriptor> = Vec::new();
            let mut colored_by_asset: HashMap<String, Vec<&HtlcSpendableOutputDescriptor>> =
                HashMap::new();

            for desc in &entry.funding {
                match desc.utxo_kind {
                    HtlcUtxoKind::Vanilla => vanilla_descs.push(desc),
                    HtlcUtxoKind::Colored => {
                        let Some(asset_id) = desc.asset_id.clone() else {
                            tracing::warn!(
                                "HTLC sweep skipped: missing asset_id for colored utxo {}:{}",
                                desc.txid,
                                desc.vout
                            );
                            continue;
                        };
                        colored_by_asset.entry(asset_id).or_default().push(desc);
                    }
                }
            }

            let leaf_hash = claim_script.tapscript_leaf_hash();
            let has_vanilla = !vanilla_descs.is_empty();
            let has_colored = !colored_by_asset.is_empty();

            if !has_vanilla && !has_colored {
                continue;
            }
            if has_vanilla && has_colored && colored_by_asset.len() != 1 {
                tracing::warn!("HTLC sweep skipped: multiple RGB assets not supported");
                continue;
            }

            let min_out = DUST_LIMIT_MSAT / 1000;

            let mut sweep_for_asset =
                |asset_id: Option<&String>, colored_descs: &[&HtlcSpendableOutputDescriptor]| {
                    let has_colored = asset_id.is_some();
                    let mut input_descs: Vec<&HtlcSpendableOutputDescriptor> = Vec::new();
                    if has_colored {
                        input_descs.extend(colored_descs.iter());
                    }
                    if has_vanilla {
                        input_descs.extend(vanilla_descs.iter());
                    }
                    if input_descs.is_empty() {
                        return;
                    }

                    let mut rgb_dest_script: Option<ScriptBuf> = None;
                    let mut btc_dest_script: Option<ScriptBuf> = None;
                    let mut recipient_id: Option<String> = None;
                    let mut proxy_url = String::new();
                    let mut total_rgb: u64 = 0;

                    if has_colored {
                        let Some(dest_script_hex) = entry.rgb_destination_script_hex.as_ref()
                        else {
                            tracing::warn!("HTLC sweep skipped: missing RGB destination script");
                            return;
                        };
                        let Some(dest_script_bytes) = hex_str_to_vec(dest_script_hex) else {
                            tracing::warn!(
                                "HTLC sweep skipped: invalid RGB destination script hex"
                            );
                            return;
                        };
                        let dest_script = ScriptBuf::from_bytes(dest_script_bytes);
                        recipient_id = Some(recipient_id_from_script_buf(
                            dest_script.clone(),
                            self.static_state.network,
                        ));
                        rgb_dest_script = Some(dest_script);

                        proxy_url = match TransportEndpoint::new(self.proxy_endpoint.clone()) {
                            Ok(endpoint) => endpoint.endpoint,
                            Err(e) => {
                                tracing::warn!("HTLC sweep skipped: invalid proxy endpoint: {e}");
                                String::new()
                            }
                        };

                        let mut invalid_assignment = false;
                        for desc in colored_descs.iter() {
                            match desc.assignment.as_ref() {
                                Some(RlnAssignment::Fungible(v)) => {
                                    total_rgb = total_rgb.saturating_add(*v);
                                }
                                _ => {
                                    invalid_assignment = true;
                                    break;
                                }
                            }
                        }
                        if invalid_assignment || total_rgb == 0 {
                            if let Some(asset_id) = asset_id {
                                tracing::warn!(
                                    "HTLC sweep skipped: invalid or missing RGB assignment for asset {}",
                                    asset_id
                                );
                            } else {
                                tracing::warn!(
                                    "HTLC sweep skipped: invalid or missing RGB assignment"
                                );
                            }
                            return;
                        }
                        if let Some(required) = entry.assignment.as_ref().and_then(|a| match a {
                            RlnAssignment::Fungible(v) => Some(*v),
                            _ => None,
                        }) {
                            if total_rgb < required {
                                tracing::warn!(
                                    "HTLC sweep skipped: RGB assignment total {total_rgb} below requested {required}"
                                );
                                return;
                            }
                        }
                    }

                    if has_vanilla {
                        let Some(dest_script_hex) = entry.btc_destination_script_hex.as_ref()
                        else {
                            tracing::warn!("HTLC sweep skipped: missing BTC destination script");
                            return;
                        };
                        let Some(dest_script_bytes) = hex_str_to_vec(dest_script_hex) else {
                            tracing::warn!(
                                "HTLC sweep skipped: invalid BTC destination script hex"
                            );
                            return;
                        };
                        btc_dest_script = Some(ScriptBuf::from_bytes(dest_script_bytes));
                    }

                    let total_sat_colored: u64 = colored_descs.iter().map(|d| d.value_sat).sum();
                    let total_sat_vanilla: u64 = vanilla_descs.iter().map(|d| d.value_sat).sum();
                    let fee_sat = FEE_RATE
                        .saturating_mul(200)
                        .saturating_mul(input_descs.len() as u64);

                    let mut outputs: Vec<TxOut> = Vec::new();
                    let mut rgb_output_index: Option<u32> = None;

                    if has_colored && has_vanilla {
                        let mut btc_out = total_sat_vanilla;
                        let mut rgb_out = total_sat_colored;
                        let mut remaining_fee = fee_sat;
                        if btc_out > min_out {
                            let spendable = btc_out - min_out;
                            let take = spendable.min(remaining_fee);
                            btc_out -= take;
                            remaining_fee -= take;
                        }
                        if remaining_fee > 0 && rgb_out > min_out {
                            let spendable = rgb_out - min_out;
                            let take = spendable.min(remaining_fee);
                            rgb_out -= take;
                            remaining_fee -= take;
                        }
                        if remaining_fee > 0 || btc_out < min_out || rgb_out < min_out {
                            tracing::warn!("HTLC sweep skipped: output value too small");
                            return;
                        }
                        outputs.push(TxOut {
                            value: Amount::from_sat(btc_out),
                            script_pubkey: btc_dest_script
                                .clone()
                                .expect("btc destination script present"),
                        });
                        outputs.push(TxOut {
                            value: Amount::from_sat(rgb_out),
                            script_pubkey: rgb_dest_script
                                .clone()
                                .expect("rgb destination script present"),
                        });
                        rgb_output_index = Some(1);
                    } else if has_vanilla {
                        if total_sat_vanilla <= fee_sat + min_out {
                            tracing::warn!("HTLC sweep skipped: output value too small");
                            return;
                        }
                        let output_value = total_sat_vanilla - fee_sat;
                        outputs.push(TxOut {
                            value: Amount::from_sat(output_value),
                            script_pubkey: btc_dest_script
                                .clone()
                                .expect("btc destination script present"),
                        });
                    } else {
                        if total_sat_colored <= fee_sat + min_out {
                            tracing::warn!("HTLC sweep skipped: output value too small");
                            return;
                        }
                        let output_value = total_sat_colored - fee_sat;
                        outputs.push(TxOut {
                            value: Amount::from_sat(output_value),
                            script_pubkey: rgb_dest_script
                                .clone()
                                .expect("rgb destination script present"),
                        });
                        rgb_output_index = Some(0);
                    }

                    if has_colored {
                        let rgb_script = rgb_dest_script
                            .as_ref()
                            .expect("rgb destination script present");
                        let rgb_output_count = outputs
                            .iter()
                            .filter(|o| o.script_pubkey == *rgb_script)
                            .count();
                        if rgb_output_count != 1 {
                            tracing::warn!(
                                "HTLC sweep skipped: expected single RGB output, found {rgb_output_count}"
                            );
                            return;
                        }
                    }

                    let mut tx = Transaction {
                        version: transaction::Version::TWO,
                        lock_time: LockTime::ZERO,
                        input: input_descs
                            .iter()
                            .map(|desc| TxIn {
                                previous_output: OutPoint {
                                    txid: desc.txid,
                                    vout: desc.vout,
                                },
                                script_sig: ScriptBuf::new(),
                                sequence: Sequence::ENABLE_RBF_NO_LOCKTIME,
                                witness: Witness::default(),
                            })
                            .collect(),
                        output: outputs,
                    };

                    let psbt =
                        Psbt::from_unsigned_tx(tx.clone()).expect("valid HTLC claim transaction");
                    let mut rgb_psbt = RgbLibPsbt::from_str(&psbt.to_string()).unwrap();
                    let mut consignments = Vec::new();

                    if has_colored {
                        let asset_id = asset_id.expect("checked above");
                        let contract_id = match ContractId::from_str(asset_id.as_str()) {
                            Ok(id) => id,
                            Err(_) => {
                                tracing::warn!(
                                    "HTLC sweep skipped: invalid asset_id {} for RGB coloring",
                                    asset_id
                                );
                                return;
                            }
                        };
                        let rgb_vout = rgb_output_index.expect("colored output index set");
                        let mut asset_info_map = map![];
                        asset_info_map.insert(
                            contract_id,
                            AssetColoringInfo {
                                output_map: HashMap::from_iter([(rgb_vout, total_rgb)]),
                                static_blinding: Some(STATIC_BLINDING),
                            },
                        );
                        let coloring_info = ColoringInfo {
                            asset_info_map,
                            static_blinding: Some(STATIC_BLINDING),
                            nonce: None,
                        };
                        let input_outpoints: Vec<OutPoint> = colored_descs
                            .iter()
                            .map(|desc| OutPoint {
                                txid: desc.txid,
                                vout: desc.vout,
                            })
                            .collect();
                        consignments = match self
                            .rgb_wallet_wrapper
                            .color_psbt_for_outpoints_and_consume(
                                &mut rgb_psbt,
                                coloring_info,
                                input_outpoints,
                            ) {
                            Ok(consignments) => consignments,
                            Err(e) => {
                                tracing::warn!("HTLC sweep skipped: RGB coloring failed: {e}");
                                return;
                            }
                        };

                        let psbt = Psbt::from_str(&rgb_psbt.to_string())
                            .expect("valid colored HTLC claim PSBT");
                        tx = match psbt.extract_tx() {
                            Ok(tx) => tx,
                            Err(ExtractTxError::MissingInputValue { tx }) => tx,
                            Err(e) => {
                                tracing::warn!("HTLC sweep skipped: invalid PSBT: {e}");
                                return;
                            }
                        };
                    }

                    let prevouts: Vec<TxOut> = input_descs
                        .iter()
                        .map(|desc| TxOut {
                            value: Amount::from_sat(desc.value_sat),
                            script_pubkey: htlc_script_pubkey.clone(),
                        })
                        .collect();
                    let mut ok = true;
                    for (idx, _desc) in input_descs.iter().enumerate() {
                        let sighash = match SighashCache::new(&tx)
                            .taproot_script_spend_signature_hash(
                                idx,
                                &sighash::Prevouts::All(&prevouts),
                                leaf_hash,
                                TapSighashType::All,
                            ) {
                            Ok(h) => h,
                            Err(_) => {
                                tracing::warn!("HTLC sweep skipped: taproot sighash failure");
                                ok = false;
                                break;
                            }
                        };
                        let msg = bitcoin::secp256k1::Message::from(sighash);
                        let signature = secp_ctx.sign_schnorr(&msg, &keypair);
                        let final_sig = bitcoin::taproot::Signature {
                            signature,
                            sighash_type: TapSighashType::All,
                        };
                        let sig_ser = final_sig.serialize();
                        tx.input[idx].witness = Witness::from_slice(&[
                            sig_ser.as_ref(),
                            preimage.as_slice(),
                            claim_script.as_bytes(),
                            control_ser.as_slice(),
                        ]);
                    }
                    if !ok {
                        return;
                    }

                    let claim_txid = tx.compute_txid().to_string();
                    if has_colored && !consignments.is_empty() && !proxy_url.is_empty() {
                        let rgb_script = rgb_dest_script
                            .as_ref()
                            .expect("rgb destination script present");
                        let rgb_vout = tx
                            .output
                            .iter()
                            .position(|o| o.script_pubkey == *rgb_script)
                            .unwrap_or(rgb_output_index.unwrap_or(0) as usize)
                            as u32;
                        for consignment in consignments {
                            let consignment_path = self
                                .static_state
                                .ldk_data_dir
                                .join(format!("consignment_{claim_txid}"));
                            if consignment.save_file(&consignment_path).is_err() {
                                tracing::warn!(
                                    "HTLC sweep skipped: failed to save consignment for {}",
                                    claim_txid
                                );
                                continue;
                            }
                            if let Some(recipient_id) = recipient_id.as_ref() {
                                if let Err(e) = self.rgb_wallet_wrapper.post_consignment(
                                    &proxy_url,
                                    recipient_id.clone(),
                                    &consignment_path,
                                    claim_txid.clone(),
                                    Some(rgb_vout),
                                ) {
                                    tracing::warn!(
                                        "HTLC sweep skipped: cannot post consignment: {e}"
                                    );
                                }
                            }
                            let _ = fs::remove_file(&consignment_path);
                        }
                    }

                    broadcast_txs.push(tx);
                    entry_broadcast = true;
                };

            if has_colored {
                if has_vanilla {
                    if let Some((asset_id, descs)) = colored_by_asset.iter().next() {
                        sweep_for_asset(Some(asset_id), descs);
                    }
                } else {
                    for (asset_id, descs) in colored_by_asset.iter() {
                        sweep_for_asset(Some(asset_id), descs);
                    }
                }
            } else {
                let empty_colored: Vec<&HtlcSpendableOutputDescriptor> = Vec::new();
                sweep_for_asset(None, &empty_colored);
            }

            if entry_broadcast {
                entry.status = "SweepBroadcast".to_string();
                updated = true;
                if self.try_accept_sweep(entry) {
                    entry.status = "ClaimConfirmed".to_string();
                    updated = true;
                    accepted_any = true;
                }
            }
        }

        if !broadcast_txs.is_empty() {
            let tx_refs: Vec<&Transaction> = broadcast_txs.iter().collect();
            self.bitcoind_client.broadcast_transactions(&tx_refs);
        }

        if updated {
            disk::write_htlc_tracker(&self.static_state.ldk_data_dir, &tracker);
        }

        accepted_any
    }

    #[cfg(test)]
    pub(crate) fn sweep_htlc_tracker_for_tests(&self) {
        let secp_ctx = Secp256k1::new();
        let _ = self.sweep_htlc_tracker(&secp_ctx);
    }

    fn try_accept_sweep(&self, entry: &HtlcTrackerEntry) -> bool {
        if entry.asset_id.is_none() {
            return false;
        }
        let Some(dest_script_hex) = entry.rgb_destination_script_hex.as_ref() else {
            tracing::warn!("HTLC sweep accept skipped: missing RGB destination script");
            return false;
        };
        let Some(dest_script_bytes) = hex_str_to_vec(dest_script_hex) else {
            tracing::warn!("HTLC sweep accept skipped: invalid RGB destination script hex");
            return false;
        };
        let dest_script = ScriptBuf::from_bytes(dest_script_bytes);
        let recipient_id = recipient_id_from_script_buf(dest_script, self.static_state.network);

        #[cfg(any(feature = "electrum", feature = "esplora"))]
        {
            let consignment_endpoint = match RgbTransport::from_str(&self.proxy_endpoint) {
                Ok(endpoint) => endpoint,
                Err(e) => {
                    tracing::warn!("HTLC sweep accept skipped: invalid proxy endpoint: {e}");
                    return false;
                }
            };
            let (consignment, txid_str, vout) = match self
                .rgb_wallet_wrapper
                .fetch_consignment_by_recipient_id(recipient_id, consignment_endpoint)
            {
                Ok(res) => res,
                Err(e) => {
                    tracing::warn!("HTLC sweep accept skipped: {e}");
                    return false;
                }
            };
            let txid = match bitcoin::Txid::from_str(&txid_str) {
                Ok(txid) => txid,
                Err(_) => {
                    tracing::warn!("HTLC sweep accept skipped: invalid txid {txid_str}");
                    return false;
                }
            };

            let min_confirmations = entry.min_confirmations.max(1) as u64;
            let confirmations = match self
                .bitcoind_client
                .get_txout_confirmations_blocking(&txid, vout)
            {
                Ok(res) => res.unwrap_or(0),
                Err(e) => {
                    tracing::warn!("HTLC sweep accept skipped: tx confirmation error: {e}");
                    return false;
                }
            };
            if confirmations < min_confirmations {
                return false;
            }

            if let Err(e) = self.rgb_wallet_wrapper.accept_transfer_from_consignment(
                consignment,
                txid_str.clone(),
                vout,
                STATIC_BLINDING,
            ) {
                tracing::warn!(
                    "HTLC claim accept transfer failed for {}:{}: {}",
                    txid_str,
                    vout,
                    e
                );
                false
            } else {
                tracing::info!(
                    "HTLC claim accept transfer succeeded for {}:{}",
                    txid_str,
                    vout
                );
                true
            }
        }
        #[cfg(not(any(feature = "electrum", feature = "esplora")))]
        {
            tracing::warn!("HTLC accept transfer skipped: rgb-lib indexer features are disabled");
            return false;
        }
    }
}

/// Per-UTXO HTLC sweep descriptor (written by the tracker; read by the sweeper).
/// Invariant HTLC data (keys/scripts/preimage) is stored once on the tracker entry.
#[derive(Debug, Clone)]
pub(crate) struct HtlcSpendableOutputDescriptor {
    txid: bitcoin::Txid,
    vout: u32,
    value_sat: u64,
    utxo_kind: HtlcUtxoKind,
    asset_id: Option<String>,
    assignment: Option<RlnAssignment>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HtlcUtxoKind {
    Vanilla,
    Colored,
}

impl HtlcUtxoKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            HtlcUtxoKind::Vanilla => "Vanilla",
            HtlcUtxoKind::Colored => "Colored",
        }
    }

    fn from_str(input: &str) -> Option<Self> {
        match input {
            "Vanilla" => Some(HtlcUtxoKind::Vanilla),
            "Colored" => Some(HtlcUtxoKind::Colored),
            _ => None,
        }
    }
}

impl HtlcSpendableOutputDescriptor {
    pub(crate) fn utxo_kind(&self) -> HtlcUtxoKind {
        self.utxo_kind
    }

    pub(crate) fn assignment(&self) -> Option<RlnAssignment> {
        self.assignment.clone()
    }

    pub(crate) fn txid(&self) -> bitcoin::Txid {
        self.txid
    }

    pub(crate) fn vout(&self) -> u32 {
        self.vout
    }

    pub(crate) fn value_sat(&self) -> u64 {
        self.value_sat
    }

    pub(crate) fn asset_id(&self) -> Option<String> {
        self.asset_id.clone()
    }

    pub(crate) fn new(
        txid: bitcoin::Txid,
        vout: u32,
        value_sat: u64,
        utxo_kind: HtlcUtxoKind,
        asset_id: Option<String>,
        assignment: Option<RlnAssignment>,
    ) -> Self {
        Self {
            txid,
            vout,
            value_sat,
            utxo_kind,
            asset_id,
            assignment,
        }
    }
}

#[derive(Debug, Default)]
pub(crate) struct HtlcTrackerStorage {
    pub(crate) entries: LdkHashMap<PaymentHash, HtlcTrackerEntry>,
}

#[derive(Debug, Clone)]
pub(crate) struct HtlcTrackerEntry {
    pub(crate) preimage: Option<String>,
    pub(crate) lp_pubkey_xonly: String,
    pub(crate) user_pubkey_xonly: String,
    pub(crate) lp_key_path: Option<String>,
    pub(crate) htlc_script_pubkey: String,
    pub(crate) recipient_id: String,
    pub(crate) rgb_invoice: String,
    pub(crate) claim_tapscript_hex: Option<String>,
    pub(crate) refund_tapscript_hex: Option<String>,
    pub(crate) tapleaf_version: Option<u8>,
    pub(crate) control_block_hex: Option<String>,
    pub(crate) t_lock: u32,
    pub(crate) min_confirmations: u32,
    pub(crate) funding: Vec<HtlcSpendableOutputDescriptor>,
    pub(crate) status: String,
    pub(crate) asset_id: Option<String>,
    pub(crate) assignment: Option<RlnAssignment>,
    pub(crate) btc_destination_script_hex: Option<String>,
    pub(crate) rgb_destination_script_hex: Option<String>,
}

fn write_str_prefixed<W: Writer>(w: &mut W, s: &str) -> Result<(), io::Error> {
    (s.len() as u16).write(w)?;
    w.write_all(s.as_bytes())
}

fn read_str_prefixed<R: io::Read>(r: &mut R) -> Result<String, DecodeError> {
    let len: u16 = Readable::read(r)?;
    let mut buf = vec![0u8; len as usize];
    r.read_exact(&mut buf)
        .map_err(|e| DecodeError::Io(e.kind()))?;
    String::from_utf8(buf).map_err(|_| DecodeError::InvalidValue)
}

fn write_str_opt<W: Writer>(w: &mut W, value: &Option<String>) -> Result<(), io::Error> {
    match value {
        Some(v) => {
            true.write(w)?;
            write_str_prefixed(w, v)?;
        }
        None => false.write(w)?,
    }
    Ok(())
}

fn read_str_opt<R: io::Read>(r: &mut R) -> Result<Option<String>, DecodeError> {
    match Readable::read(r) {
        Ok(true) => Ok(Some(read_str_prefixed(r)?)),
        Ok(false) => Ok(None),
        Err(DecodeError::ShortRead) | Err(DecodeError::Io(io::ErrorKind::UnexpectedEof)) => {
            Ok(None)
        }
        Err(e) => Err(e),
    }
}

fn write_assignment<W: Writer>(w: &mut W, assignment: &RlnAssignment) -> Result<(), io::Error> {
    match assignment {
        RlnAssignment::Fungible(v) => {
            0u8.write(w)?;
            v.write(w)?;
        }
        RlnAssignment::NonFungible => {
            1u8.write(w)?;
        }
        RlnAssignment::InflationRight(v) => {
            2u8.write(w)?;
            v.write(w)?;
        }
        RlnAssignment::ReplaceRight => {
            3u8.write(w)?;
        }
        RlnAssignment::Any => {
            4u8.write(w)?;
        }
    }
    Ok(())
}

fn read_assignment<R: io::Read>(r: &mut R) -> Result<RlnAssignment, DecodeError> {
    let tag: u8 = Readable::read(r)?;
    match tag {
        0 => Ok(RlnAssignment::Fungible(Readable::read(r)?)),
        1 => Ok(RlnAssignment::NonFungible),
        2 => Ok(RlnAssignment::InflationRight(Readable::read(r)?)),
        3 => Ok(RlnAssignment::ReplaceRight),
        4 => Ok(RlnAssignment::Any),
        _ => Err(DecodeError::InvalidValue),
    }
}

fn write_assignment_opt<W: Writer>(
    w: &mut W,
    assignment: &Option<RlnAssignment>,
) -> Result<(), io::Error> {
    match assignment {
        Some(a) => {
            true.write(w)?;
            write_assignment(w, a)?;
        }
        None => false.write(w)?,
    }
    Ok(())
}

fn read_assignment_opt<R: io::Read>(r: &mut R) -> Result<Option<RlnAssignment>, DecodeError> {
    if Readable::read(r)? {
        Ok(Some(read_assignment(r)?))
    } else {
        Ok(None)
    }
}

impl Writeable for HtlcSpendableOutputDescriptor {
    fn write<W: Writer>(&self, w: &mut W) -> Result<(), io::Error> {
        self.txid.write(w)?;
        self.vout.write(w)?;
        self.value_sat.write(w)?;
        write_str_prefixed(w, self.utxo_kind.as_str())?;
        match &self.asset_id {
            Some(a) => {
                true.write(w)?;
                write_str_prefixed(w, a)?;
            }
            None => false.write(w)?,
        }
        write_assignment_opt(w, &self.assignment)?;
        Ok(())
    }
}

impl Readable for HtlcSpendableOutputDescriptor {
    fn read<R: io::Read>(r: &mut R) -> Result<Self, DecodeError> {
        Ok(Self {
            txid: Readable::read(r)?,
            vout: Readable::read(r)?,
            value_sat: Readable::read(r)?,
            utxo_kind: {
                let utxo_kind_str = read_str_prefixed(r)?;
                HtlcUtxoKind::from_str(&utxo_kind_str).ok_or(DecodeError::InvalidValue)?
            },
            asset_id: if Readable::read(r)? {
                Some(read_str_prefixed(r)?)
            } else {
                None
            },
            assignment: read_assignment_opt(r)?,
        })
    }
}

impl Writeable for HtlcTrackerEntry {
    fn write<W: Writer>(&self, w: &mut W) -> Result<(), io::Error> {
        match &self.preimage {
            Some(p) => {
                true.write(w)?;
                write_str_prefixed(w, p)?;
            }
            None => false.write(w)?,
        }
        write_str_prefixed(w, &self.lp_pubkey_xonly)?;
        write_str_prefixed(w, &self.user_pubkey_xonly)?;
        write_str_prefixed(w, &self.htlc_script_pubkey)?;
        write_str_prefixed(w, &self.recipient_id)?;
        write_str_prefixed(w, &self.rgb_invoice)?;
        match &self.claim_tapscript_hex {
            Some(v) => {
                true.write(w)?;
                write_str_prefixed(w, v)?;
            }
            None => false.write(w)?,
        }
        match &self.refund_tapscript_hex {
            Some(v) => {
                true.write(w)?;
                write_str_prefixed(w, v)?;
            }
            None => false.write(w)?,
        }
        match self.tapleaf_version {
            Some(v) => {
                true.write(w)?;
                v.write(w)?;
            }
            None => false.write(w)?,
        }
        match &self.control_block_hex {
            Some(v) => {
                true.write(w)?;
                write_str_prefixed(w, v)?;
            }
            None => false.write(w)?,
        }
        self.t_lock.write(w)?;
        self.min_confirmations.write(w)?;
        (self.funding.len() as u16).write(w)?;
        for f in &self.funding {
            f.write(w)?;
        }
        write_str_prefixed(w, &self.status)?;
        match &self.asset_id {
            Some(a) => {
                true.write(w)?;
                write_str_prefixed(w, a)?;
            }
            None => false.write(w)?,
        }
        write_assignment_opt(w, &self.assignment)?;
        write_str_opt(w, &self.btc_destination_script_hex)?;
        write_str_opt(w, &self.rgb_destination_script_hex)?;
        write_str_opt(w, &self.lp_key_path)?;
        Ok(())
    }
}

impl Readable for HtlcTrackerEntry {
    fn read<R: io::Read>(r: &mut R) -> Result<Self, DecodeError> {
        let preimage = if Readable::read(r)? {
            Some(read_str_prefixed(r)?)
        } else {
            None
        };
        let lp_pubkey_xonly = read_str_prefixed(r)?;
        let user_pubkey_xonly = read_str_prefixed(r)?;
        let htlc_script_pubkey = read_str_prefixed(r)?;
        let recipient_id = read_str_prefixed(r)?;
        let rgb_invoice = read_str_prefixed(r)?;
        let claim_tapscript_hex = if Readable::read(r)? {
            Some(read_str_prefixed(r)?)
        } else {
            None
        };
        let refund_tapscript_hex = if Readable::read(r)? {
            Some(read_str_prefixed(r)?)
        } else {
            None
        };
        let tapleaf_version = if Readable::read(r)? {
            Some(Readable::read(r)?)
        } else {
            None
        };
        let control_block_hex = if Readable::read(r)? {
            Some(read_str_prefixed(r)?)
        } else {
            None
        };
        let t_lock = Readable::read(r)?;
        let min_confirmations = Readable::read(r)?;
        let funding_len: u16 = Readable::read(r)?;
        let mut funding = Vec::with_capacity(funding_len as usize);
        for _ in 0..funding_len {
            funding.push(Readable::read(r)?);
        }
        let status = read_str_prefixed(r)?;
        let asset_id = if Readable::read(r)? {
            Some(read_str_prefixed(r)?)
        } else {
            None
        };
        let assignment = read_assignment_opt(r)?;
        let btc_destination_script_hex = read_str_opt(r)?;
        let rgb_destination_script_hex = read_str_opt(r)?;
        let lp_key_path = match read_str_opt(r) {
            Ok(v) => v,
            Err(DecodeError::Io(io::ErrorKind::UnexpectedEof)) => None,
            Err(e) => return Err(e),
        };
        Ok(Self {
            preimage,
            lp_pubkey_xonly,
            user_pubkey_xonly,
            lp_key_path,
            htlc_script_pubkey,
            recipient_id,
            rgb_invoice,
            claim_tapscript_hex,
            refund_tapscript_hex,
            tapleaf_version,
            control_block_hex,
            t_lock,
            min_confirmations,
            funding,
            status,
            asset_id,
            assignment,
            btc_destination_script_hex,
            rgb_destination_script_hex,
        })
    }
}

impl Writeable for HtlcTrackerStorage {
    fn write<W: Writer>(&self, w: &mut W) -> Result<(), io::Error> {
        self.entries.write(w)
    }
}

impl Readable for HtlcTrackerStorage {
    fn read<R: io::Read>(r: &mut R) -> Result<Self, DecodeError> {
        Ok(Self {
            entries: Readable::read(r)?,
        })
    }
}

pub(crate) type OutputSweeper = ldk_sweep::OutputSweeper<
    Arc<BitcoindClient>,
    Arc<RgbLibWalletWrapper>,
    Arc<BitcoindClient>,
    Arc<dyn Filter + Send + Sync>,
    Arc<FilesystemStore>,
    Arc<FilesystemLogger>,
    Arc<RgbOutputSpender>,
>;

fn _update_rgb_channel_amount(ldk_data_dir: &Path, payment_hash: &PaymentHash, receiver: bool) {
    let payment_hash_str = hex_str(&payment_hash.0);
    for entry in fs::read_dir(ldk_data_dir).unwrap() {
        let file = entry.unwrap();
        let file_name = file.file_name();
        let file_name_str = file_name.to_string_lossy();
        let mut file_path_no_ext = file.path().clone();
        file_path_no_ext.set_extension("");
        let file_name_str_no_ext = file_path_no_ext.file_name().unwrap().to_string_lossy();
        if file_name_str.contains(&payment_hash_str) && file_name_str_no_ext != payment_hash_str {
            let rgb_payment_info = parse_rgb_payment_info(&file.path());
            let channel_id_str = file_name_str_no_ext.replace(&payment_hash_str, "");

            if rgb_payment_info.swap_payment && receiver != rgb_payment_info.inbound {
                continue;
            }

            let (offered, received) = if receiver {
                (0, rgb_payment_info.amount)
            } else {
                (rgb_payment_info.amount, 0)
            };
            update_rgb_channel_amount(&channel_id_str, offered, received, ldk_data_dir, false);
            break;
        }
    }
}

async fn handle_ldk_events(
    event: Event,
    unlocked_state: Arc<UnlockedAppState>,
    static_state: Arc<StaticState>,
) -> Result<(), ReplayEvent> {
    match event {
        Event::FundingGenerationReady {
            temporary_channel_id,
            counterparty_node_id,
            channel_value_satoshis,
            output_script,
            ..
        } => {
            let addr = WitnessProgram::from_scriptpubkey(
                output_script.as_bytes(),
                match static_state.network {
                    BitcoinNetwork::Mainnet => bitcoin_bech32::constants::Network::Bitcoin,
                    BitcoinNetwork::Testnet | BitcoinNetwork::Testnet4 => {
                        bitcoin_bech32::constants::Network::Testnet
                    }
                    BitcoinNetwork::Regtest => bitcoin_bech32::constants::Network::Regtest,
                    BitcoinNetwork::Signet | BitcoinNetwork::SignetCustom(_) => {
                        bitcoin_bech32::constants::Network::Signet
                    }
                },
            )
            .expect("Lightning funding tx should always be to a SegWit output");
            let script_buf = ScriptBuf::from_bytes(addr.to_scriptpubkey());

            let is_colored = is_channel_rgb(
                &temporary_channel_id,
                &PathBuf::from(&static_state.ldk_data_dir),
            );
            let (unsigned_psbt, asset_id) = if is_colored {
                let (rgb_info, _) = get_rgb_channel_info_pending(
                    &temporary_channel_id,
                    &PathBuf::from(&static_state.ldk_data_dir),
                );

                let channel_rgb_amount: u64 = rgb_info.local_rgb_amount;
                let asset_id = rgb_info.contract_id.to_string();
                let assignment = match rgb_info.schema {
                    AssetSchema::Nia | AssetSchema::Cfa => Assignment::Fungible(channel_rgb_amount),
                    AssetSchema::Uda => Assignment::NonFungible,
                    AssetSchema::Ifa => todo!(),
                };

                let recipient_id = recipient_id_from_script_buf(script_buf, static_state.network);

                let recipient_map = map! {
                    asset_id.clone() => vec![Recipient {
                        recipient_id: recipient_id.clone(),
                        witness_data: Some(WitnessData {
                            amount_sat: channel_value_satoshis,
                            blinding: Some(STATIC_BLINDING),
                        }),
                        assignment,
                        transport_endpoints: vec![unlocked_state.proxy_endpoint.clone()]
                }]};

                let unlocked_state_copy = unlocked_state.clone();
                let unsigned_psbt = tokio::task::spawn_blocking(move || {
                    unlocked_state_copy
                        .rgb_send_begin(recipient_map, true, FEE_RATE, MIN_CHANNEL_CONFIRMATIONS)
                        .unwrap()
                })
                .await
                .unwrap();
                (unsigned_psbt, Some(asset_id))
            } else {
                let unsigned_psbt = unlocked_state
                    .rgb_send_btc_begin(addr.to_address(), channel_value_satoshis, FEE_RATE)
                    .unwrap();
                (unsigned_psbt, None)
            };

            let signed_psbt = unlocked_state.rgb_sign_psbt(unsigned_psbt).unwrap();
            let psbt = Psbt::from_str(&signed_psbt).unwrap();

            let funding_tx = psbt.clone().extract_tx().unwrap();
            let funding_txid = funding_tx.compute_txid().to_string();
            tracing::info!("Funding TXID: {funding_txid}");

            let psbt_path = static_state
                .ldk_data_dir
                .join(format!("psbt_{funding_txid}"));
            fs::write(psbt_path, psbt.to_string()).unwrap();

            if let Some(asset_id) = asset_id {
                let unlocked_state_copy = unlocked_state.clone();
                let witness_id = funding_txid.clone();
                tokio::task::spawn_blocking(move || {
                    unlocked_state_copy
                        .rgb_upsert_witness(
                            RgbTxid::from_str(&witness_id).unwrap(),
                            WitnessOrd::Tentative,
                        )
                        .unwrap()
                })
                .await
                .unwrap();

                let consignment_path =
                    unlocked_state.rgb_get_send_consignment_path(&asset_id, &funding_txid);
                let proxy_url = TransportEndpoint::new(unlocked_state.proxy_endpoint.clone())
                    .unwrap()
                    .endpoint;
                let unlocked_state_copy = unlocked_state.clone();
                let res = tokio::task::spawn_blocking(move || {
                    unlocked_state_copy.rgb_post_consignment(
                        &proxy_url,
                        funding_txid.clone(),
                        &consignment_path,
                        funding_txid,
                        None,
                    )
                })
                .await
                .unwrap();

                if let Err(e) = res {
                    tracing::error!("cannot post consignment: {e}");
                    return Err(ReplayEvent());
                }
            }

            let channel_manager_copy = unlocked_state.channel_manager.clone();

            // Give the funding transaction back to LDK for opening the channel.
            if channel_manager_copy
                .funding_transaction_generated(
                    temporary_channel_id,
                    counterparty_node_id,
                    funding_tx,
                )
                .is_err()
            {
                tracing::error!(
                        "ERROR: Channel went away before we could fund it. The peer disconnected or refused the channel.");
                *unlocked_state.rgb_send_lock.lock().unwrap() = false;
            }
        }
        Event::FundingTxBroadcastSafe { .. } => {
            // We don't use the manual broadcasting feature, so this event should never be seen.
        }
        Event::PaymentClaimable {
            payment_hash,
            purpose,
            amount_msat,
            receiver_node_id: _,
            claim_deadline,
            onion_fields: _,
            counterparty_skimmed_fee_msat: _,
            receiving_channel_ids: _,
            payment_id: _,
        } => {
            tracing::info!(
                "EVENT: received payment from payment hash {} of {} millisatoshis",
                payment_hash,
                amount_msat,
            );

            let (payment_preimage, payment_secret) = match purpose {
                PaymentPurpose::Bolt11InvoicePayment {
                    payment_preimage,
                    payment_secret,
                    ..
                } => (payment_preimage, Some(payment_secret)),
                PaymentPurpose::Bolt12OfferPayment {
                    payment_preimage,
                    payment_secret,
                    ..
                } => (payment_preimage, Some(payment_secret)),
                PaymentPurpose::Bolt12RefundPayment {
                    payment_preimage,
                    payment_secret,
                    ..
                } => (payment_preimage, Some(payment_secret)),
                PaymentPurpose::SpontaneousPayment(preimage) => (Some(preimage), None),
            };

            let invoice_metadata = unlocked_state
                .invoice_metadata()
                .get(&payment_hash)
                .cloned();

            let Some(metadata) = invoice_metadata else {
                let inbound_payments = unlocked_state.inbound_payments();
                let legacy_invoice = inbound_payments.get(&payment_hash);

                if let Some(legacy_payment_info) = legacy_invoice {
                    tracing::info!(
                        "Legacy invoice detected (no metadata) for payment {:?}, treating as auto-claim",
                        payment_hash
                    );

                    let Some(preimage) = payment_preimage else {
                        if let Some(stored_preimage) = legacy_payment_info.preimage {
                            tracing::info!(
                                "Using stored preimage from inbound_payment for legacy invoice {:?}",
                                payment_hash
                            );
                            unlocked_state.channel_manager.claim_funds(stored_preimage);
                            return Ok(());
                        }

                        tracing::error!(
                            "Missing payment preimage for legacy invoice {:?}, cannot claim. \
                            This may indicate a corrupted state or LDK version issue.",
                            payment_hash
                        );
                        unlocked_state
                            .channel_manager
                            .fail_htlc_backwards(&payment_hash);
                        return Ok(());
                    };

                    if let Some(expected_amt) = legacy_payment_info.amt_msat {
                        if amount_msat < expected_amt {
                            tracing::warn!(
                                "Received {} msat for legacy invoice {} but expected at least {} msat",
                                amount_msat,
                                payment_hash,
                                expected_amt
                            );
                            unlocked_state
                                .channel_manager
                                .fail_htlc_backwards(&payment_hash);
                            unlocked_state.upsert_inbound_payment(
                                payment_hash,
                                HTLCStatus::Failed,
                                payment_preimage,
                                payment_secret,
                                Some(amount_msat),
                                unlocked_state.channel_manager.get_our_node_id(),
                            );
                            return Ok(());
                        }
                    }

                    tracing::info!("Auto-claiming legacy invoice {:?}", payment_hash);
                    unlocked_state.channel_manager.claim_funds(preimage);
                    return Ok(());
                }

                let Some(preimage) = payment_preimage else {
                    tracing::error!(
                        "Missing payment preimage for payment {:?}, cannot claim",
                        payment_hash
                    );
                    unlocked_state
                        .channel_manager
                        .fail_htlc_backwards(&payment_hash);
                    return Ok(());
                };
                tracing::info!("Auto-claiming payment without metadata {:?}", payment_hash);
                unlocked_state.channel_manager.claim_funds(preimage);
                return Ok(());
            };

            let now_ts = get_current_timestamp();
            if let Some(expiry) = metadata.expiry {
                if now_ts >= expiry {
                    tracing::warn!(
                        "Received HTLC for expired invoice {payment_hash:?} (expiry {expiry})"
                    );
                    unlocked_state
                        .channel_manager
                        .fail_htlc_backwards(&payment_hash);
                    unlocked_state.upsert_inbound_payment(
                        payment_hash,
                        HTLCStatus::Failed,
                        payment_preimage,
                        payment_secret,
                        Some(amount_msat),
                        unlocked_state.channel_manager.get_our_node_id(),
                    );
                    return Ok(());
                }
            }

            if let Some(expected) = metadata.expected_amt_msat {
                if amount_msat < expected {
                    tracing::warn!(
                        "Received {} msat for invoice {} but expected at least {} msat",
                        amount_msat,
                        payment_hash,
                        expected
                    );
                    unlocked_state
                        .channel_manager
                        .fail_htlc_backwards(&payment_hash);
                    unlocked_state.upsert_inbound_payment(
                        payment_hash,
                        HTLCStatus::Failed,
                        payment_preimage,
                        payment_secret,
                        Some(amount_msat),
                        unlocked_state.channel_manager.get_our_node_id(),
                    );
                    return Ok(());
                }
            }

            match metadata.mode {
                InvoiceMode::AutoClaim => {
                    let Some(preimage) = payment_preimage else {
                        tracing::error!(
                            "Missing payment preimage for standard invoice {:?}, cannot claim",
                            payment_hash
                        );
                        unlocked_state
                            .channel_manager
                            .fail_htlc_backwards(&payment_hash);
                        return Ok(());
                    };
                    unlocked_state.channel_manager.claim_funds(preimage);
                }
                InvoiceMode::Hodl => {
                    let claimable = ClaimablePayment {
                        payment_hash,
                        amount_msat,
                        invoice_expiry: metadata.expiry,
                        claim_deadline_height: claim_deadline,
                        created_at: now_ts,
                        settling: Some(false),
                        settling_since: None,
                    };
                    unlocked_state.upsert_claimable_payment(claimable);
                    unlocked_state.upsert_inbound_payment(
                        payment_hash,
                        HTLCStatus::Claimable,
                        None,
                        payment_secret,
                        Some(amount_msat),
                        unlocked_state.channel_manager.get_our_node_id(),
                    );
                }
            }
        }
        Event::PaymentClaimed {
            payment_hash,
            purpose,
            amount_msat,
            receiver_node_id,
            htlcs: _,
            sender_intended_total_msat: _,
            onion_fields: _,
            payment_id: _,
        } => {
            tracing::info!(
                "EVENT: claimed payment from payment hash {} of {} millisatoshis",
                payment_hash,
                amount_msat,
            );
            let (payment_preimage, payment_secret) = match purpose {
                PaymentPurpose::Bolt11InvoicePayment {
                    payment_preimage,
                    payment_secret,
                    ..
                } => (payment_preimage, Some(payment_secret)),
                PaymentPurpose::Bolt12OfferPayment {
                    payment_preimage,
                    payment_secret,
                    ..
                } => (payment_preimage, Some(payment_secret)),
                PaymentPurpose::Bolt12RefundPayment {
                    payment_preimage,
                    payment_secret,
                    ..
                } => (payment_preimage, Some(payment_secret)),
                PaymentPurpose::SpontaneousPayment(preimage) => (Some(preimage), None),
            };

            // check if already claimed
            let is_maker_swap = unlocked_state.is_maker_swap(&payment_hash);
            if is_maker_swap {
                if let Some(swap) = unlocked_state.maker_swaps().get(&payment_hash) {
                    if swap.status == SwapStatus::Succeeded {
                        tracing::info!("EVENT: payment already claimed, skipping");
                        return Ok(());
                    }
                }
            } else if let Some(payment) = unlocked_state
                .get_inbound_payments()
                .payments
                .get(&payment_hash)
            {
                if payment.status == HTLCStatus::Succeeded {
                    tracing::info!("EVENT: payment already claimed, skipping");
                    return Ok(());
                }
            }

            _update_rgb_channel_amount(&static_state.ldk_data_dir, &payment_hash, true);
            if is_maker_swap {
                unlocked_state.update_maker_swap_status(&payment_hash, SwapStatus::Succeeded);
            } else {
                unlocked_state.upsert_inbound_payment(
                    payment_hash,
                    HTLCStatus::Succeeded,
                    payment_preimage,
                    payment_secret,
                    Some(amount_msat),
                    receiver_node_id.unwrap(),
                );
            }

            if unlocked_state.claimable_payment(&payment_hash).is_some() {
                let _ = unlocked_state.take_claimable_payment(&payment_hash);
            }
        }
        Event::PaymentSent {
            payment_preimage,
            payment_hash,
            fee_paid_msat,
            payment_id,
            ..
        } => {
            _update_rgb_channel_amount(&static_state.ldk_data_dir, &payment_hash, false);

            if unlocked_state.is_maker_swap(&payment_hash) {
                tracing::info!(
                    "EVENT: successfully swapped payment with hash {} and preimage {}",
                    payment_hash,
                    payment_preimage
                );
                unlocked_state.update_maker_swap_status(&payment_hash, SwapStatus::Succeeded);
            } else {
                let payment = unlocked_state.update_outbound_payment(
                    payment_id.unwrap(),
                    HTLCStatus::Succeeded,
                    Some(payment_preimage),
                );
                tracing::info!(
                    "EVENT: successfully sent payment of {:?} millisatoshis{} from \
                            payment hash {} with preimage {}",
                    payment.amt_msat,
                    if let Some(fee) = fee_paid_msat {
                        format!(" (fee {fee} msat)")
                    } else {
                        "".to_string()
                    },
                    payment_hash,
                    payment_preimage
                );
            }
        }
        Event::OpenChannelRequest {
            ref temporary_channel_id,
            ref counterparty_node_id,
            ..
        } => {
            let mut random_bytes = [0u8; 16];
            random_bytes
                .copy_from_slice(&unlocked_state.keys_manager.get_secure_random_bytes()[..16]);
            let user_channel_id = u128::from_be_bytes(random_bytes);
            let res = unlocked_state.channel_manager.accept_inbound_channel(
                temporary_channel_id,
                counterparty_node_id,
                user_channel_id,
                None,
            );

            if let Err(e) = res {
                tracing::error!(
                    "EVENT: Failed to accept inbound channel ({}) from {}: {:?}",
                    temporary_channel_id,
                    hex_str(&counterparty_node_id.serialize()),
                    e,
                );
            } else {
                tracing::info!(
                    "EVENT: Accepted inbound channel ({}) from {}",
                    temporary_channel_id,
                    hex_str(&counterparty_node_id.serialize()),
                );
            }
        }
        Event::PaymentPathSuccessful { .. } => {}
        Event::PaymentPathFailed { .. } => {}
        Event::ProbeSuccessful { .. } => {}
        Event::ProbeFailed { .. } => {}
        Event::PaymentFailed {
            payment_hash,
            reason,
            payment_id,
            ..
        } => {
            if let Some(hash) = payment_hash {
                tracing::error!(
                    "EVENT: Failed to send payment to payment ID {}, payment hash {}: {:?}",
                    payment_id,
                    hash,
                    if let Some(r) = reason {
                        r
                    } else {
                        PaymentFailureReason::RetriesExhausted
                    }
                );
                if unlocked_state.is_maker_swap(&hash) {
                    unlocked_state.update_maker_swap_status(&hash, SwapStatus::Failed);
                } else {
                    unlocked_state.update_outbound_payment_status(payment_id, HTLCStatus::Failed);
                }
            } else {
                tracing::error!(
                    "EVENT: Failed fetch invoice for payment ID {}: {:?}",
                    payment_id,
                    if let Some(r) = reason {
                        r
                    } else {
                        PaymentFailureReason::RetriesExhausted
                    }
                );
                unlocked_state.update_outbound_payment_status(payment_id, HTLCStatus::Failed);
            }
        }
        Event::InvoiceReceived { .. } => {
            // We don't use the manual invoice payment logic, so this event should never be seen.
        }
        Event::PaymentForwarded {
            prev_channel_id,
            next_channel_id,
            total_fee_earned_msat,
            claim_from_onchain_tx,
            outbound_amount_forwarded_msat,
            skimmed_fee_msat: _,
            prev_user_channel_id: _,
            next_user_channel_id: _,
            prev_node_id: _,
            next_node_id: _,
            outbound_amount_forwarded_rgb,
            inbound_amount_forwarded_rgb,
            payment_hash,
        } => {
            let prev_channel_id_str = prev_channel_id.expect("prev_channel_id").to_string();
            let next_channel_id_str = next_channel_id.expect("next_channel_id").to_string();

            if let Some(outbound_amount_forwarded_rgb) = outbound_amount_forwarded_rgb {
                update_rgb_channel_amount(
                    &next_channel_id_str,
                    outbound_amount_forwarded_rgb,
                    0,
                    &static_state.ldk_data_dir,
                    false,
                );
            }
            if let Some(inbound_amount_forwarded_rgb) = inbound_amount_forwarded_rgb {
                update_rgb_channel_amount(
                    &prev_channel_id_str,
                    0,
                    inbound_amount_forwarded_rgb,
                    &static_state.ldk_data_dir,
                    false,
                );
            }

            if unlocked_state.is_taker_swap(&payment_hash) {
                unlocked_state.update_taker_swap_status(&payment_hash, SwapStatus::Succeeded);
            }

            let read_only_network_graph = unlocked_state.network_graph.read_only();
            let nodes = read_only_network_graph.nodes();
            let channels = unlocked_state.channel_manager.list_channels();

            let node_str = |channel_id: &Option<ChannelId>| match channel_id {
                None => String::new(),
                Some(channel_id) => match channels.iter().find(|c| c.channel_id == *channel_id) {
                    None => String::new(),
                    Some(channel) => {
                        match nodes.get(&NodeId::from_pubkey(&channel.counterparty.node_id)) {
                            None => "private node".to_string(),
                            Some(node) => match &node.announcement_info {
                                None => "unnamed node".to_string(),
                                Some(announcement) => {
                                    format!("node {}", announcement.alias())
                                }
                            },
                        }
                    }
                },
            };
            let channel_str = |channel_id: &Option<ChannelId>| {
                channel_id
                    .map(|channel_id| format!(" with channel {channel_id}"))
                    .unwrap_or_default()
            };
            let from_prev_str = format!(
                " from {}{}",
                node_str(&prev_channel_id),
                channel_str(&prev_channel_id)
            );
            let to_next_str = format!(
                " to {}{}",
                node_str(&next_channel_id),
                channel_str(&next_channel_id)
            );

            let from_onchain_str = if claim_from_onchain_tx {
                "from onchain downstream claim"
            } else {
                "from HTLC fulfill message"
            };
            let amt_args = if let Some(v) = outbound_amount_forwarded_msat {
                format!("{v}")
            } else {
                "?".to_string()
            };
            if let Some(fee_earned) = total_fee_earned_msat {
                tracing::info!(
                    "EVENT: Forwarded payment for {} msat{}{}, earning {} msat {}",
                    amt_args,
                    from_prev_str,
                    to_next_str,
                    fee_earned,
                    from_onchain_str
                );
            } else {
                tracing::info!(
                    "EVENT: Forwarded payment for {} msat{}{}, claiming onchain {}",
                    amt_args,
                    from_prev_str,
                    to_next_str,
                    from_onchain_str
                );
            }
        }
        Event::HTLCHandlingFailed { .. } => {}
        Event::SpendableOutputs {
            outputs,
            channel_id,
        } => {
            tracing::info!("EVENT: tracking {} spendable outputs", outputs.len(),);

            unlocked_state
                .output_sweeper
                .track_spendable_outputs(outputs, channel_id, false, None)
                .await
                .unwrap();
        }
        Event::ChannelPending {
            channel_id,
            counterparty_node_id,
            funding_txo,
            former_temporary_channel_id,
            ..
        } => {
            tracing::info!(
                "EVENT: Channel {} with peer {} is pending awaiting funding lock-in!",
                channel_id,
                hex_str(&counterparty_node_id.serialize()),
            );

            unlocked_state.add_channel_id(former_temporary_channel_id.unwrap(), channel_id);

            let funding_txid = funding_txo.txid.to_string();
            let psbt_path = static_state
                .ldk_data_dir
                .join(format!("psbt_{funding_txid}"));

            if psbt_path.exists() {
                let psbt_str = fs::read_to_string(psbt_path).unwrap();

                let state_copy = unlocked_state.clone();
                let psbt_str_copy = psbt_str.clone();

                let is_chan_colored =
                    is_channel_rgb(&channel_id, &PathBuf::from(&static_state.ldk_data_dir));
                tracing::info!("Initiator of the channel (colored: {})", is_chan_colored);

                let _txid = tokio::task::spawn_blocking(move || {
                    if is_chan_colored {
                        state_copy.rgb_send_end(psbt_str_copy).map(|r| r.txid)
                    } else {
                        state_copy.rgb_send_btc_end(psbt_str_copy)
                    }
                })
                .await
                .unwrap()
                .map_err(|e| {
                    tracing::error!("Error completing channel opening: {e:?}");
                    ReplayEvent()
                })?;

                *unlocked_state.rgb_send_lock.lock().unwrap() = false;
            } else {
                // acceptor
                let consignment_path = static_state
                    .ldk_data_dir
                    .join(format!("consignment_{funding_txid}"));
                if !consignment_path.exists() {
                    // vanilla channel
                    return Ok(());
                }
                let consignment =
                    RgbTransfer::load_file(consignment_path).expect("successful consignment load");

                match unlocked_state.rgb_save_new_asset(consignment, funding_txid) {
                    Ok(_) => {}
                    Err(e) if e.to_string().contains("UNIQUE constraint failed") => {}
                    Err(e) => panic!("Failed saving asset: {e}"),
                }
            }
        }
        Event::ChannelReady {
            ref channel_id,
            user_channel_id: _,
            ref counterparty_node_id,
            funding_txo: _,
            channel_type: _,
        } => {
            tracing::info!(
                "EVENT: Channel {} with peer {} is ready to be used!",
                channel_id,
                hex_str(&counterparty_node_id.serialize()),
            );

            tokio::task::spawn_blocking(move || {
                unlocked_state.rgb_refresh(false).unwrap();
                unlocked_state.rgb_refresh(true).unwrap()
            })
            .await
            .unwrap();
        }
        Event::ChannelClosed {
            channel_id,
            reason,
            user_channel_id: _,
            counterparty_node_id,
            channel_capacity_sats: _,
            channel_funding_txo: _,
            last_local_balance_msat: _,
        } => {
            tracing::info!(
                "EVENT: Channel {} with counterparty {} closed due to: {:?}",
                channel_id,
                counterparty_node_id
                    .map(|id| format!("{id}"))
                    .unwrap_or("".to_owned()),
                reason
            );

            unlocked_state.delete_channel_id(channel_id);
        }
        Event::DiscardFunding { channel_id, .. } => {
            // A "real" node should probably "lock" the UTXOs spent in funding transactions until
            // the funding transaction either confirms, or this event is generated.
            tracing::info!(
                "EVENT: Discarded funding for channel with ID {}",
                channel_id
            );

            *unlocked_state.rgb_send_lock.lock().unwrap() = false;

            unlocked_state.delete_channel_id(channel_id);
        }
        Event::HTLCIntercepted {
            is_swap,
            payment_hash,
            intercept_id,
            inbound_amount_msat,
            expected_outbound_amount_msat,
            inbound_rgb_amount,
            expected_outbound_rgb_payment,
            requested_next_hop_scid,
            prev_outbound_scid_alias,
        } => {
            if !is_swap {
                tracing::warn!("Intercepted an HTLC that's not related to a swap");
                unlocked_state
                    .channel_manager
                    .fail_intercepted_htlc(intercept_id)
                    .unwrap();
                return Ok(());
            }

            let get_rgb_info = |channel_id| {
                get_rgb_channel_info_optional(
                    channel_id,
                    &PathBuf::from(&static_state.ldk_data_dir),
                    true,
                )
                .map(|(rgb_info, _)| {
                    (
                        rgb_info.contract_id,
                        rgb_info.local_rgb_amount,
                        rgb_info.remote_rgb_amount,
                    )
                })
            };

            let inbound_channel = unlocked_state
                .channel_manager
                .list_channels()
                .into_iter()
                .find(|details| details.outbound_scid_alias == Some(prev_outbound_scid_alias))
                .expect("Should always be a valid channel");
            let outbound_channel = unlocked_state
                .channel_manager
                .list_channels()
                .into_iter()
                .find(|details| details.short_channel_id == Some(requested_next_hop_scid))
                .expect("Should always be a valid channel");

            let inbound_rgb_info = get_rgb_info(&inbound_channel.channel_id);
            let outbound_rgb_info = get_rgb_info(&outbound_channel.channel_id);

            tracing::debug!("EVENT: Requested swap with params inbound_msat={} outbound_msat={} inbound_rgb={:?} outbound_rgb={:?} inbound_contract_id={:?}, outbound_contract_id={:?}", inbound_amount_msat, expected_outbound_amount_msat, inbound_rgb_amount, expected_outbound_rgb_payment.map(|(_, a)| a), inbound_rgb_info.map(|i| i.0), expected_outbound_rgb_payment.map(|(c, _)| c));

            let swaps_lock = unlocked_state.taker_swaps.lock().unwrap();
            let whitelist_swap = match swaps_lock.swaps.get(&payment_hash) {
                None => {
                    tracing::error!("ERROR: rejecting non-whitelisted swap");
                    unlocked_state
                        .channel_manager
                        .fail_intercepted_htlc(intercept_id)
                        .unwrap();
                    return Ok(());
                }
                Some(x) => x,
            };

            let mut fail = false;
            if whitelist_swap.swap_info.is_from_btc() {
                let net_msat_diff = expected_outbound_amount_msat.checked_sub(inbound_amount_msat);

                if inbound_rgb_amount != Some(whitelist_swap.swap_info.qty_to)
                    || inbound_rgb_info.map(|x| x.0) != whitelist_swap.swap_info.to_asset
                    || net_msat_diff != Some(whitelist_swap.swap_info.qty_from)
                {
                    fail = true;
                }
            } else if whitelist_swap.swap_info.is_to_btc() {
                let net_msat_diff =
                    inbound_amount_msat.saturating_sub(expected_outbound_amount_msat);

                if expected_outbound_rgb_payment.map(|(_, a)| a)
                    != Some(whitelist_swap.swap_info.qty_from)
                    || outbound_rgb_info.map(|x| x.0) != whitelist_swap.swap_info.from_asset
                    || net_msat_diff != whitelist_swap.swap_info.qty_to
                {
                    fail = true;
                }
            } else {
                let net_msat_diff = inbound_amount_msat.checked_sub(expected_outbound_amount_msat);

                if net_msat_diff != Some(0)
                    || expected_outbound_rgb_payment.map(|(_, a)| a)
                        != Some(whitelist_swap.swap_info.qty_from)
                    || outbound_rgb_info.map(|x| x.0) != whitelist_swap.swap_info.from_asset
                    || inbound_rgb_amount != Some(whitelist_swap.swap_info.qty_to)
                    || inbound_rgb_info.map(|x| x.0) != whitelist_swap.swap_info.to_asset
                {
                    fail = true;
                }
            }

            drop(swaps_lock);

            if fail {
                tracing::error!("ERROR: swap doesn't match the whitelisted info, rejecting it");
                unlocked_state.update_taker_swap_status(&payment_hash, SwapStatus::Failed);
                unlocked_state
                    .channel_manager
                    .fail_intercepted_htlc(intercept_id)
                    .unwrap();
                return Ok(());
            }

            tracing::debug!("Swap is whitelisted, forwarding the htlc...");
            unlocked_state.update_taker_swap_status(&payment_hash, SwapStatus::Pending);

            unlocked_state
                .channel_manager
                .forward_intercepted_htlc(
                    intercept_id,
                    channelmanager::NextHopForward::ShortChannelId(requested_next_hop_scid),
                    outbound_channel.counterparty.node_id,
                    expected_outbound_amount_msat,
                    expected_outbound_rgb_payment,
                )
                .expect("Forward should be valid");
        }
        Event::OnionMessageIntercepted { .. } => {
            // We don't use the onion message interception feature, so this event should never be
            // seen.
        }
        Event::OnionMessagePeerConnected { .. } => {
            // We don't use the onion message interception feature, so we have no use for this
            // event.
        }
        Event::BumpTransaction(event) => {
            unlocked_state
                .bump_tx_event_handler
                .handle_event(&event)
                .await
        }
        Event::ConnectionNeeded { node_id, addresses } => {
            tokio::spawn(async move {
                for address in addresses {
                    if let Ok(sockaddrs) = address.to_socket_addrs() {
                        for addr in sockaddrs {
                            let pm = Arc::clone(&unlocked_state.peer_manager);
                            if connect_peer_if_necessary(node_id, addr, pm).await.is_ok() {
                                return;
                            }
                        }
                    }
                }
            });
        }
        Event::SplicePending { .. } => {
            // We don't use the splice feature, so this event should never be seen.
        }
        Event::SpliceFailed { .. } => {
            // We don't use the splice feature, so this event should never be seen.
        }
        Event::PersistStaticInvoice { .. } => {
            // We don't use the static invoice feature, so this event should never be seen.
        }
        Event::StaticInvoiceRequested { .. } => {
            // We don't use the static invoice feature, so this event should never be seen.
        }
        Event::FundingTransactionReadyForSigning { .. } => {
            // We don't use the interactive funding transaction construction feature, so this event should never be seen.
        }
    }
    Ok(())
}

impl OutputSpender for RgbOutputSpender {
    fn spend_spendable_outputs(
        &self,
        descriptors: &[&SpendableOutputDescriptor],
        outputs: Vec<TxOut>,
        change_destination_script: ScriptBuf,
        feerate_sat_per_1000_weight: u32,
        locktime: Option<LockTime>,
        secp_ctx: &Secp256k1<All>,
    ) -> Result<bitcoin::Transaction, ()> {
        let _ = self.sweep_htlc_tracker(secp_ctx);

        let mut hasher = DefaultHasher::new();
        descriptors.hash(&mut hasher);
        let descriptors_hash = hasher.finish();
        let mut txes = self.txes.lock().unwrap();
        if let Some(tx) = txes.get(&descriptors_hash) {
            return Ok(tx.clone());
        }

        let mut vout = 0;
        let mut vanilla_descriptor = true;

        let mut txouts = outputs.clone();
        let mut asset_info: HashMap<ContractId, (u32, u64, String)> = map![];

        for outp in descriptors {
            let outpoint = match outp {
                SpendableOutputDescriptor::StaticPaymentOutput(descriptor) => descriptor.outpoint,
                SpendableOutputDescriptor::DelayedPaymentOutput(descriptor) => descriptor.outpoint,
                SpendableOutputDescriptor::StaticOutput { ref outpoint, .. } => *outpoint,
            };

            let txid = outpoint.txid;
            let txid_str = txid.to_string();

            let transfer_info_path = self
                .static_state
                .ldk_data_dir
                .join(format!("{txid_str}_transfer_info"));
            if !transfer_info_path.exists() {
                continue;
            };
            let transfer_info = read_rgb_transfer_info(&transfer_info_path);
            if transfer_info.rgb_amount == 0 {
                continue;
            }

            vanilla_descriptor = false;

            let closing_height = self
                .rgb_wallet_wrapper
                .get_tx_height(txid_str.clone())
                .map_err(|_| ())?;
            let update_res = self
                .rgb_wallet_wrapper
                .update_witnesses(
                    closing_height.unwrap(),
                    vec![RgbTxid::from_str(&txid_str).unwrap()],
                )
                .unwrap();
            if !update_res.failed.is_empty() {
                return Err(());
            }

            let contract_id = transfer_info.contract_id;

            let mut new_asset = false;
            let recipient_id = if let Some((_, _, recipient_id)) = asset_info.get(&contract_id) {
                recipient_id.clone()
            } else {
                new_asset = true;
                let receive_data = self
                    .rgb_wallet_wrapper
                    .witness_receive(
                        None,
                        Assignment::Any,
                        None,
                        vec![self.proxy_endpoint.clone()],
                        0,
                    )
                    .unwrap();
                let script_pubkey = script_buf_from_recipient_id(receive_data.recipient_id.clone())
                    .unwrap()
                    .unwrap();
                txouts.push(TxOut {
                    value: Amount::from_sat(DUST_LIMIT_MSAT / 1000),
                    script_pubkey,
                });
                receive_data.recipient_id
            };

            let amt_rgb = transfer_info.rgb_amount;

            asset_info
                .entry(contract_id)
                .and_modify(|(_, a, _)| {
                    *a += amt_rgb;
                })
                .or_insert_with(|| (vout, amt_rgb, recipient_id));

            if new_asset {
                vout += 1;
            }
        }

        if vanilla_descriptor {
            return self.keys_manager.spend_spendable_outputs(
                descriptors.as_ref(),
                txouts,
                change_destination_script,
                feerate_sat_per_1000_weight,
                locktime,
                secp_ctx,
            );
        }

        let feerate_sat_per_1000_weight = FEE_RATE as u32 * 250; // 1 sat/vB = 250 sat/kw
        let (psbt, _expected_max_weight) =
            SpendableOutputDescriptor::create_spendable_outputs_psbt(
                secp_ctx,
                descriptors,
                txouts,
                change_destination_script,
                feerate_sat_per_1000_weight,
                locktime,
            )
            .unwrap();

        let mut asset_info_map = map![];
        for (contract_id, (vout, amt_rgb, _)) in asset_info.clone() {
            asset_info_map.insert(
                contract_id,
                AssetColoringInfo {
                    output_map: HashMap::from_iter([(vout, amt_rgb)]),
                    static_blinding: None,
                },
            );
        }

        let coloring_info = ColoringInfo {
            asset_info_map,
            static_blinding: None,
            nonce: None,
        };

        let mut psbt = RgbLibPsbt::from_str(&psbt.to_string()).unwrap();
        let consignments = self
            .rgb_wallet_wrapper
            .color_psbt_and_consume(&mut psbt, coloring_info)
            .unwrap();

        let mut psbt = Psbt::from_str(&psbt.to_string()).expect("valid transaction");

        psbt = self
            .keys_manager
            .sign_spendable_outputs_psbt(descriptors, psbt, secp_ctx)
            .unwrap();

        let spending_tx = match psbt.extract_tx() {
            Ok(tx) => tx,
            Err(ExtractTxError::MissingInputValue { tx }) => tx,
            Err(e) => panic!("should never happen: {e}"),
        };

        let closing_txid = spending_tx.compute_txid().to_string();

        let handle = Handle::current();
        let _ = handle.enter();

        for consignment in consignments {
            let contract_id = consignment.contract_id();

            let (mut vout, _, recipient_id) = asset_info[&contract_id].clone();
            vout += 1;

            let consignment_path = self
                .static_state
                .ldk_data_dir
                .join(format!("consignment_{}", closing_txid.clone()));
            consignment
                .save_file(&consignment_path)
                .expect("successful save");
            let proxy_url = TransportEndpoint::new(self.proxy_endpoint.clone())
                .unwrap()
                .endpoint;
            let rgb_wallet_wrapper_copy = self.rgb_wallet_wrapper.clone();
            let closing_txid_copy = closing_txid.clone();
            let consignment_path_copy = consignment_path.clone();
            let res = futures::executor::block_on(tokio::task::spawn_blocking(move || {
                rgb_wallet_wrapper_copy.post_consignment(
                    &proxy_url,
                    recipient_id,
                    &consignment_path_copy,
                    closing_txid_copy,
                    Some(vout),
                )
            }));
            if let Err(e) = res {
                tracing::error!("cannot post consignment: {e}");
                return Err(());
            }
            fs::remove_file(&consignment_path).unwrap();
        }

        txes.insert(descriptors_hash, spending_tx.clone());
        self.fs_store
            .write("", "", OUTPUT_SPENDER_TXES, txes.encode())
            .unwrap();

        Ok(spending_tx)
    }
}

pub(crate) async fn start_ldk(
    app_state: Arc<AppState>,
    mnemonic: Mnemonic,
    unlock_request: UnlockRequest,
) -> Result<(LdkBackgroundServices, Arc<UnlockedAppState>), APIError> {
    let static_state = &app_state.static_state;

    let ldk_data_dir = static_state.ldk_data_dir.clone();
    let ldk_data_dir_path = PathBuf::from(&ldk_data_dir);
    let logger = static_state.logger.clone();
    let bitcoin_network = static_state.network;
    let network: Network = bitcoin_network.into();
    let ldk_peer_listening_port = static_state.ldk_peer_listening_port;

    // Initialize our bitcoind client.
    let bitcoind_client = match BitcoindClient::new(
        unlock_request.bitcoind_rpc_host.clone(),
        unlock_request.bitcoind_rpc_port,
        unlock_request.bitcoind_rpc_username.clone(),
        unlock_request.bitcoind_rpc_password.clone(),
        tokio::runtime::Handle::current(),
        Arc::clone(&logger),
    )
    .await
    {
        Ok(client) => Arc::new(client),
        Err(e) => {
            return Err(APIError::FailedBitcoindConnection(e.to_string()));
        }
    };

    // Check that the bitcoind we've connected to is running the network we expect
    let bitcoind_chain = bitcoind_client.get_blockchain_info().await.chain;
    if bitcoind_chain
        != match bitcoin_network {
            BitcoinNetwork::Mainnet => "main",
            BitcoinNetwork::Testnet => "test",
            BitcoinNetwork::Testnet4 => "testnet4",
            BitcoinNetwork::Regtest => "regtest",
            BitcoinNetwork::Signet | BitcoinNetwork::SignetCustom(_) => "signet",
        }
    {
        return Err(APIError::NetworkMismatch(bitcoind_chain, bitcoin_network));
    }

    // RGB setup
    let indexer_url = if let Some(indexer_url) = &unlock_request.indexer_url {
        let indexer_protocol = check_indexer_url(indexer_url, bitcoin_network)?;
        tracing::info!(
            "Connected to an indexer with the {} protocol",
            indexer_protocol
        );
        indexer_url
    } else {
        tracing::info!("Using the default indexer");
        match bitcoin_network {
            BitcoinNetwork::Regtest => ELECTRUM_URL_REGTEST,
            BitcoinNetwork::Signet | BitcoinNetwork::SignetCustom(_) => ELECTRUM_URL_SIGNET,
            BitcoinNetwork::Testnet => ELECTRUM_URL_TESTNET,
            BitcoinNetwork::Testnet4 => ELECTRUM_URL_TESTNET4,
            BitcoinNetwork::Mainnet => ELECTRUM_URL_MAINNET,
        }
    };
    let proxy_endpoint = if let Some(proxy_endpoint) = &unlock_request.proxy_endpoint {
        check_rgb_proxy_endpoint(proxy_endpoint).await?;
        tracing::info!("Using a custom proxy");
        proxy_endpoint
    } else {
        tracing::info!("Using the default proxy");
        match bitcoin_network {
            BitcoinNetwork::Signet
            | BitcoinNetwork::SignetCustom(_)
            | BitcoinNetwork::Testnet
            | BitcoinNetwork::Testnet4
            | BitcoinNetwork::Mainnet => PROXY_ENDPOINT_PUBLIC,
            BitcoinNetwork::Regtest => PROXY_ENDPOINT_LOCAL,
        }
    };
    let storage_dir_path = app_state.static_state.storage_dir_path.clone();
    fs::write(storage_dir_path.join(INDEXER_URL_FNAME), indexer_url).expect("able to write");
    fs::write(
        storage_dir_path.join(BITCOIN_NETWORK_FNAME),
        bitcoin_network.to_string(),
    )
    .expect("able to write");

    // Initialize the FeeEstimator
    // BitcoindClient implements the FeeEstimator trait, so it'll act as our fee estimator.
    let fee_estimator = bitcoind_client.clone();

    // Initialize the BroadcasterInterface
    // BitcoindClient implements the BroadcasterInterface trait, so it'll act as our transaction
    // broadcaster.
    let broadcaster = bitcoind_client.clone();

    // Initialize the KeysManager
    // The key seed that we use to derive the node privkey (that corresponds to the node pubkey) and
    // other secret key material.
    let xkey: ExtendedKey = mnemonic
        .clone()
        .into_extended_key()
        .expect("a valid key should have been provided");
    let master_xprv = &xkey
        .into_xprv(network)
        .expect("should be possible to get an extended private key");
    let xprv: Xpriv = master_xprv
        .derive_priv(&Secp256k1_30::new(), &ChildNumber::Hardened { index: 535 })
        .unwrap();
    let ldk_seed: [u8; 32] = xprv.private_key.secret_bytes();
    let cur = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap();
    let keys_manager = Arc::new(KeysManager::new(
        &ldk_seed,
        cur.as_secs(),
        cur.subsec_nanos(),
        true,
        ldk_data_dir_path.clone(),
    ));

    // Initialize Persistence
    let fs_store = Arc::new(FilesystemStore::new(ldk_data_dir.clone()));
    let persister = Arc::new(MonitorUpdatingPersister::new(
        Arc::clone(&fs_store),
        Arc::clone(&logger),
        1000,
        Arc::clone(&keys_manager),
        Arc::clone(&keys_manager),
        Arc::clone(&bitcoind_client),
        Arc::clone(&bitcoind_client),
    ));

    // Initialize the ChainMonitor
    let chain_monitor: Arc<ChainMonitor> = Arc::new(chainmonitor::ChainMonitor::new(
        None,
        Arc::clone(&broadcaster),
        Arc::clone(&logger),
        Arc::clone(&fee_estimator),
        Arc::clone(&persister),
        Arc::clone(&keys_manager),
        keys_manager.get_peer_storage_key(),
    ));

    // Read ChannelMonitor state from disk
    let mut channelmonitors = persister.read_all_channel_monitors_with_updates().unwrap();

    // Poll for the best chain tip, which may be used by the channel manager & spv client
    let polled_chain_tip = init::validate_best_block_header(bitcoind_client.as_ref())
        .await
        .expect("Failed to fetch best block header and best block");

    // Initialize routing ProbabilisticScorer
    let network_graph_path = ldk_data_dir.join("network_graph");
    let network_graph = Arc::new(disk::read_network(
        &network_graph_path,
        network,
        logger.clone(),
    ));

    let scorer_path = ldk_data_dir.join("scorer");
    let scorer = Arc::new(RwLock::new(disk::read_scorer(
        &scorer_path,
        Arc::clone(&network_graph),
        Arc::clone(&logger),
    )));

    // Create Routers
    let scoring_fee_params = ProbabilisticScoringFeeParameters::default();
    let router = Arc::new(DefaultRouter::new(
        network_graph.clone(),
        logger.clone(),
        keys_manager.clone(),
        scorer.clone(),
        scoring_fee_params,
    ));
    let message_router = Arc::new(DefaultMessageRouter::new(
        Arc::clone(&network_graph),
        Arc::clone(&keys_manager),
    ));

    // Initialize the ChannelManager
    let mut user_config = UserConfig::default();
    user_config
        .channel_handshake_limits
        .force_announced_channel_preference = false;
    user_config
        .channel_handshake_config
        .negotiate_anchors_zero_fee_htlc_tx = true;
    user_config.manually_accept_inbound_channels = true;
    let mut restarting_node = true;
    let (channel_manager_blockhash, channel_manager) = {
        if let Ok(f) = fs::File::open(ldk_data_dir.join("manager")) {
            let mut channel_monitor_references = Vec::new();
            for (_, channel_monitor) in channelmonitors.iter() {
                channel_monitor_references.push(channel_monitor);
            }
            let read_args = ChannelManagerReadArgs::new(
                keys_manager.clone(),
                keys_manager.clone(),
                keys_manager.clone(),
                fee_estimator.clone(),
                chain_monitor.clone(),
                broadcaster.clone(),
                router.clone(),
                Arc::clone(&message_router),
                logger.clone(),
                user_config,
                channel_monitor_references,
                ldk_data_dir_path.clone(),
            );
            <(BlockHash, ChannelManager)>::read(&mut BufReader::new(f), read_args).unwrap()
        } else {
            // We're starting a fresh node.
            restarting_node = false;

            let polled_best_block = polled_chain_tip.to_best_block();
            let polled_best_block_hash = polled_best_block.block_hash;
            let chain_params = ChainParameters {
                network,
                best_block: polled_best_block,
            };
            let fresh_channel_manager = channelmanager::ChannelManager::new(
                fee_estimator.clone(),
                chain_monitor.clone(),
                broadcaster.clone(),
                router.clone(),
                Arc::clone(&message_router),
                logger.clone(),
                keys_manager.clone(),
                keys_manager.clone(),
                keys_manager.clone(),
                user_config,
                chain_params,
                cur.as_secs() as u32,
                ldk_data_dir_path.clone(),
            );
            (polled_best_block_hash, fresh_channel_manager)
        }
    };

    // Prepare the RGB wallet
    let mnemonic_str = mnemonic.to_string();
    let (_, account_xpub_vanilla, _) =
        get_account_data(bitcoin_network, &mnemonic_str, false).unwrap();
    let (_, account_xpub_colored, master_fingerprint) =
        get_account_data(bitcoin_network, &mnemonic_str, true).unwrap();
    let data_dir = static_state
        .storage_dir_path
        .clone()
        .to_string_lossy()
        .to_string();
    let mut rgb_wallet = tokio::task::spawn_blocking(move || {
        RgbLibWallet::new(WalletData {
            data_dir,
            bitcoin_network,
            database_type: DatabaseType::Sqlite,
            max_allocations_per_utxo: 1,
            account_xpub_vanilla: account_xpub_vanilla.to_string(),
            account_xpub_colored: account_xpub_colored.to_string(),
            master_fingerprint: master_fingerprint.to_string(),
            mnemonic: Some(mnemonic.to_string()),
            vanilla_keychain: None,
            supported_schemas: vec![AssetSchema::Nia, AssetSchema::Cfa, AssetSchema::Uda],
        })
        .expect("valid rgb-lib wallet")
    })
    .await
    .unwrap();
    let rgb_online = rgb_wallet.go_online(false, indexer_url.to_string())?;
    fs::write(
        static_state.storage_dir_path.join(WALLET_FINGERPRINT_FNAME),
        account_xpub_colored.fingerprint().to_string(),
    )
    .expect("able to write");
    fs::write(
        static_state
            .storage_dir_path
            .join(WALLET_ACCOUNT_XPUB_COLORED_FNAME),
        account_xpub_colored.to_string(),
    )
    .expect("able to write");
    fs::write(
        static_state
            .storage_dir_path
            .join(WALLET_ACCOUNT_XPUB_VANILLA_FNAME),
        account_xpub_vanilla.to_string(),
    )
    .expect("able to write");
    fs::write(
        static_state
            .storage_dir_path
            .join(WALLET_MASTER_FINGERPRINT_FNAME),
        master_fingerprint.to_string(),
    )
    .expect("able to write");

    let rgb_wallet_wrapper = Arc::new(RgbLibWalletWrapper::new(
        Arc::new(Mutex::new(rgb_wallet)),
        rgb_online.clone(),
    ));

    // Derive LP Taproot base xprv (BIP86 m/86'/coin'/0'/0) from mnemonic for HTLC claims.
    let coin = match network {
        Network::Bitcoin => 0,
        _ => 1,
    };
    let path: DerivationPath = format!("m/86'/{coin}'/0'/0")
        .parse()
        .map_err(|_| APIError::FailedBitcoindConnection("Invalid BIP86 derivation path".into()))?;
    let secp_xpriv = Secp256k1_30::new();
    let lp_htlc_xprv = master_xprv.derive_priv(&secp_xpriv, &path).map_err(|_| {
        APIError::FailedBitcoindConnection("Failed to derive LP taproot xprv".into())
    })?;
    let lp_htlc_xpub = Xpub::from_priv(&secp_xpriv, &lp_htlc_xprv);

    // Initialize the OutputSweeper.
    let txes = Arc::new(Mutex::new(disk::read_output_spender_txes(
        &ldk_data_dir.join(OUTPUT_SPENDER_TXES),
    )));
    let rgb_output_spender = Arc::new(RgbOutputSpender {
        static_state: static_state.clone(),
        rgb_wallet_wrapper: rgb_wallet_wrapper.clone(),
        keys_manager: keys_manager.clone(),
        fs_store: fs_store.clone(),
        txes,
        proxy_endpoint: proxy_endpoint.to_string(),
        lp_htlc_xprv,
        bitcoind_client: bitcoind_client.clone(),
    });
    let (sweeper_best_block, output_sweeper) = match fs_store.read(
        OUTPUT_SWEEPER_PERSISTENCE_PRIMARY_NAMESPACE,
        OUTPUT_SWEEPER_PERSISTENCE_SECONDARY_NAMESPACE,
        OUTPUT_SWEEPER_PERSISTENCE_KEY,
    ) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            let sweeper = OutputSweeper::new(
                channel_manager.current_best_block(),
                broadcaster.clone(),
                fee_estimator.clone(),
                None,
                rgb_output_spender.clone(),
                rgb_wallet_wrapper.clone(),
                fs_store.clone(),
                logger.clone(),
            );
            (channel_manager.current_best_block(), sweeper)
        }
        Ok(mut bytes) => {
            let read_args = (
                broadcaster.clone(),
                fee_estimator.clone(),
                None,
                rgb_output_spender.clone(),
                rgb_wallet_wrapper.clone(),
                fs_store.clone(),
                logger.clone(),
            );
            let mut reader = io::Cursor::new(&mut bytes);
            <(BestBlock, OutputSweeper)>::read(&mut reader, read_args)
                .expect("Failed to deserialize OutputSweeper")
        }
        Err(e) => panic!("Failed to read OutputSweeper with {e}"),
    };

    // Sync ChannelMonitors, ChannelManager and OutputSweeper to chain tip
    let mut chain_listener_channel_monitors = Vec::new();
    let mut cache = UnboundedCache::new();
    let chain_tip = if restarting_node {
        let mut chain_listeners = vec![
            (
                channel_manager_blockhash,
                &channel_manager as &(dyn chain::Listen + Send + Sync),
            ),
            (
                sweeper_best_block.block_hash,
                &output_sweeper as &(dyn chain::Listen + Send + Sync),
            ),
        ];

        for (blockhash, channel_monitor) in channelmonitors.drain(..) {
            let outpoint = channel_monitor.get_funding_txo();
            chain_listener_channel_monitors.push((
                blockhash,
                (
                    channel_monitor,
                    broadcaster.clone(),
                    fee_estimator.clone(),
                    logger.clone(),
                ),
                outpoint,
            ));
        }

        for monitor_listener_info in chain_listener_channel_monitors.iter_mut() {
            chain_listeners.push((
                monitor_listener_info.0,
                &monitor_listener_info.1 as &(dyn chain::Listen + Send + Sync),
            ));
        }

        let mut attempts = 3;
        loop {
            match init::synchronize_listeners(
                bitcoind_client.as_ref(),
                network,
                &mut cache,
                chain_listeners.clone(),
            )
            .await
            {
                Ok(res) => break res,
                Err(e) => {
                    tracing::error!("Error synchronizing chain: {:?}", e);
                    attempts -= 1;
                    if attempts == 0 {
                        return Err(APIError::FailedBitcoindConnection(
                            e.into_inner().to_string(),
                        ));
                    }
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
            }
        }
    } else {
        polled_chain_tip
    };

    // Give ChannelMonitors to ChainMonitor
    for (_, (channel_monitor, _, _, _), _) in chain_listener_channel_monitors {
        let channel_id = channel_monitor.channel_id();
        assert_eq!(
            chain_monitor.load_existing_monitor(channel_id, channel_monitor),
            Ok(ChannelMonitorUpdateStatus::Completed)
        );
    }

    // Optional: Initialize the P2PGossipSync
    let gossip_sync = Arc::new(P2PGossipSync::new(
        Arc::clone(&network_graph),
        None,
        Arc::clone(&logger),
    ));

    // Initialize an OMDomainResolver as a service to other nodes.
    // As a service to other LDK users, using an `OMDomainResolver` allows others to resolve BIP
    // 353 Human Readable Names for others, providing them DNSSEC proofs over lightning onion
    // messages. Doing this only makes sense for an always-online public routing node, and doesn't
    // provide you any direct value, but it's nice to offer the service for others.
    let channel_manager: Arc<ChannelManager> = Arc::new(channel_manager);
    let resolver = "8.8.8.8:53".to_socket_addrs().unwrap().next().unwrap();
    let domain_resolver = Arc::new(OMDomainResolver::new(
        resolver,
        Some(Arc::clone(&channel_manager)),
    ));

    // Initialize the PeerManager
    let onion_messenger: Arc<OnionMessenger> = Arc::new(LdkOnionMessenger::new(
        Arc::clone(&keys_manager),
        Arc::clone(&keys_manager),
        Arc::clone(&logger),
        Arc::clone(&channel_manager),
        Arc::clone(&message_router),
        Arc::clone(&channel_manager),
        Arc::clone(&channel_manager),
        domain_resolver,
        IgnoringMessageHandler {},
    ));
    let mut ephemeral_bytes = [0; 32];
    let current_time = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    rand::thread_rng().fill_bytes(&mut ephemeral_bytes);
    let lightning_msg_handler = MessageHandler {
        chan_handler: channel_manager.clone(),
        route_handler: gossip_sync.clone(),
        onion_message_handler: onion_messenger.clone(),
        custom_message_handler: IgnoringMessageHandler {},
        send_only_message_handler: Arc::clone(&chain_monitor),
    };
    let peer_manager: Arc<PeerManager> = Arc::new(PeerManager::new(
        lightning_msg_handler,
        current_time.try_into().unwrap(),
        &ephemeral_bytes,
        logger.clone(),
        Arc::clone(&keys_manager),
    ));

    // Install a GossipVerifier in in the P2PGossipSync
    let utxo_lookup = GossipVerifier::new(
        Arc::clone(&bitcoind_client.bitcoind_rpc_client),
        TokioSpawner,
        Arc::clone(&gossip_sync),
        Arc::clone(&peer_manager),
    );
    gossip_sync.add_utxo_lookup(Some(Arc::new(utxo_lookup)));

    // ## Running LDK
    // Initialize networking

    let peer_manager_connection_handler = peer_manager.clone();
    let listening_port = ldk_peer_listening_port;
    let stop_processing = Arc::new(AtomicBool::new(false));
    let stop_listen = Arc::clone(&stop_processing);
    tokio::spawn(async move {
        let listener = tokio::net::TcpListener::bind(format!("[::]:{listening_port}"))
            .await
            .expect("Failed to bind to listen port - is something else already listening on it?");
        loop {
            let peer_mgr = peer_manager_connection_handler.clone();
            let tcp_stream = listener.accept().await.unwrap().0;
            if stop_listen.load(Ordering::Acquire) {
                return;
            }
            tokio::spawn(async move {
                lightning_net_tokio::setup_inbound(
                    peer_mgr.clone(),
                    tcp_stream.into_std().unwrap(),
                )
                .await;
            });
        }
    });

    // Connect and Disconnect Blocks
    let output_sweeper: Arc<OutputSweeper> = Arc::new(output_sweeper);
    let channel_manager_listener = channel_manager.clone();
    let chain_monitor_listener = chain_monitor.clone();
    let output_sweeper_listener = output_sweeper.clone();
    let htlc_output_spender = rgb_output_spender.clone();
    let bitcoind_block_source = bitcoind_client.clone();
    let stop_listen = Arc::clone(&stop_processing);
    tokio::spawn(async move {
        let chain_poller = poll::ChainPoller::new(bitcoind_block_source.as_ref(), network);
        let chain_listener = (
            chain_monitor_listener,
            &(channel_manager_listener, output_sweeper_listener),
        );
        let mut spv_client = SpvClient::new(chain_tip, chain_poller, &mut cache, &chain_listener);
        let secp_ctx = Secp256k1::new();
        loop {
            if stop_listen.load(Ordering::Acquire) {
                return;
            }
            if let Err(e) = spv_client.poll_best_tip().await {
                tracing::error!("Error while polling best tip: {:?}", e);
            } else {
                let accepted_any = htlc_output_spender.sweep_htlc_tracker(&secp_ctx);
                if accepted_any {
                    let rgb_wallet_wrapper = htlc_output_spender.rgb_wallet_wrapper.clone();
                    match tokio::task::spawn_blocking(move || rgb_wallet_wrapper.refresh(false))
                        .await
                    {
                        Ok(Ok(_)) => {}
                        Ok(Err(e)) => {
                            tracing::warn!("HTLC sweep refresh failed: {e}");
                        }
                        Err(e) => {
                            tracing::warn!("HTLC sweep refresh task failed: {e}");
                        }
                    }
                }
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    });

    let inbound_payments = Arc::new(Mutex::new(disk::read_inbound_payment_info(
        &ldk_data_dir.join(INBOUND_PAYMENTS_FNAME),
    )));
    let invoice_metadata = Arc::new(Mutex::new(disk::read_invoice_metadata(
        &ldk_data_dir.join(INVOICE_METADATA_FNAME),
    )));
    let claimable_htlcs = Arc::new(Mutex::new(disk::read_claimable_htlcs(
        &ldk_data_dir.join(CLAIMABLE_HTLCS_FNAME),
    )));
    let outbound_payments = Arc::new(Mutex::new(disk::read_outbound_payment_info(
        &ldk_data_dir.join(OUTBOUND_PAYMENTS_FNAME),
    )));

    let bump_tx_event_handler = Arc::new(BumpTransactionEventHandler::new(
        Arc::clone(&broadcaster),
        Arc::new(Wallet::new(rgb_wallet_wrapper.clone(), Arc::clone(&logger))),
        Arc::clone(&keys_manager),
        Arc::clone(&logger),
    ));

    // Persist ChannelManager and NetworkGraph
    let persister = Arc::new(FilesystemStore::new(ldk_data_dir_path.clone()));

    // Read swaps info
    let maker_swaps = Arc::new(Mutex::new(disk::read_swaps_info(
        &ldk_data_dir.join(MAKER_SWAPS_FNAME),
    )));
    let taker_swaps = Arc::new(Mutex::new(disk::read_swaps_info(
        &ldk_data_dir.join(TAKER_SWAPS_FNAME),
    )));

    // Read channel IDs info
    let channel_ids_map = Arc::new(Mutex::new(disk::read_channel_ids_info(
        &ldk_data_dir.join(CHANNEL_IDS_FNAME),
    )));

    let unlocked_state = Arc::new(UnlockedAppState {
        channel_manager: Arc::clone(&channel_manager),
        inbound_payments,
        invoice_metadata,
        claimable_htlcs,
        keys_manager,
        network_graph,
        chain_monitor: chain_monitor.clone(),
        onion_messenger: onion_messenger.clone(),
        outbound_payments,
        peer_manager: Arc::clone(&peer_manager),
        fs_store: Arc::clone(&fs_store),
        bump_tx_event_handler,
        rgb_wallet_wrapper,
        maker_swaps,
        taker_swaps,
        router: Arc::clone(&router),
        output_sweeper: Arc::clone(&output_sweeper),
        #[cfg(test)]
        htlc_output_spender: rgb_output_spender.clone(),
        bitcoind_client: bitcoind_client.clone(),
        rgb_send_lock: Arc::new(Mutex::new(false)),
        channel_ids_map,
        proxy_endpoint: proxy_endpoint.to_string(),
        lp_htlc_xpub,
    });

    let recent_payments_payment_ids = channel_manager
        .list_recent_payments()
        .into_iter()
        .map(|p| match p {
            RecentPaymentDetails::Pending { payment_id, .. } => payment_id,
            RecentPaymentDetails::Fulfilled { payment_id, .. } => payment_id,
            RecentPaymentDetails::Abandoned { payment_id, .. } => payment_id,
            RecentPaymentDetails::AwaitingInvoice { payment_id } => payment_id,
        })
        .collect::<Vec<PaymentId>>();
    unlocked_state.fail_outbound_pending_payments(recent_payments_payment_ids);

    // Handle LDK Events
    let unlocked_state_copy = Arc::clone(&unlocked_state);
    let static_state_copy = Arc::clone(static_state);
    let event_handler = move |event: Event| {
        let unlocked_state_copy = Arc::clone(&unlocked_state_copy);
        let static_state_copy = Arc::clone(&static_state_copy);
        async move { handle_ldk_events(event, unlocked_state_copy, static_state_copy).await }
    };

    let stop_claimable_expiry = Arc::clone(&stop_processing);
    let unlocked_state_claimable = Arc::clone(&unlocked_state);
    let claimable_expiry_task = tokio::spawn(async move {
        loop {
            if stop_claimable_expiry.load(Ordering::Acquire) {
                return;
            }
            tokio::time::sleep(Duration::from_secs(30)).await;
            if stop_claimable_expiry.load(Ordering::Acquire) {
                return;
            }
            let now = get_current_timestamp();
            let height = unlocked_state_claimable
                .channel_manager
                .current_best_block()
                .height;
            let expired = unlocked_state_claimable.expire_claimables(now, height);
            for claimable in expired {
                tracing::info!(
                    "Expiring claimable payment {:?} (deadline: {:?}, expiry: {:?})",
                    claimable.payment_hash,
                    claimable.claim_deadline_height,
                    claimable.invoice_expiry
                );

                unlocked_state_claimable
                    .channel_manager
                    .fail_htlc_backwards(&claimable.payment_hash);

                unlocked_state_claimable.upsert_inbound_payment(
                    claimable.payment_hash,
                    HTLCStatus::Failed,
                    None,
                    None,
                    Some(claimable.amount_msat),
                    unlocked_state_claimable.channel_manager.get_our_node_id(),
                );
            }
        }
    });

    // Background Processing
    let (bp_exit, bp_exit_check) = tokio::sync::watch::channel(());
    let background_processor = tokio::spawn(process_events_async(
        persister,
        event_handler,
        chain_monitor.clone(),
        channel_manager.clone(),
        Some(onion_messenger),
        GossipSync::p2p(gossip_sync),
        peer_manager.clone(),
        NO_LIQUIDITY_MANAGER,
        Some(Arc::clone(&output_sweeper)),
        logger.clone(),
        Some(scorer.clone()),
        move |t| {
            let mut bp_exit_fut_check = bp_exit_check.clone();
            Box::pin(async move {
                tokio::select! {
                    _ = tokio::time::sleep(t) => false,
                    _ = bp_exit_fut_check.changed() => true,
                }
            })
        },
        false,
        || {
            Some(
                SystemTime::now()
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .unwrap(),
            )
        },
    ));

    // Regularly reconnect to channel peers.
    let connect_cm = Arc::clone(&channel_manager);
    let connect_pm = Arc::clone(&peer_manager);
    let peer_data_path = ldk_data_dir.join(CHANNEL_PEER_DATA);
    let stop_connect = Arc::clone(&stop_processing);
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(1));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            interval.tick().await;
            match disk::read_channel_peer_data(&peer_data_path) {
                Ok(info) => {
                    for node_id in connect_cm
                        .list_channels()
                        .iter()
                        .map(|chan| chan.counterparty.node_id)
                        .filter(|id| connect_pm.peer_by_node_id(id).is_none())
                    {
                        if stop_connect.load(Ordering::Acquire) {
                            return;
                        }
                        for (pubkey, peer_addr) in info.iter() {
                            if *pubkey == node_id {
                                let _ =
                                    do_connect_peer(*pubkey, *peer_addr, Arc::clone(&connect_pm))
                                        .await;
                            }
                        }
                    }
                }
                Err(e) => tracing::error!(
                    "ERROR: errored reading channel peer info from disk: {:?}",
                    e
                ),
            }
        }
    });

    // Regularly broadcast our node_announcement. This is only required (or possible) if we have
    // some public channels.
    let mut ldk_announced_listen_addr = Vec::new();
    for addr in unlock_request.announce_addresses {
        match SocketAddress::from_str(&addr) {
            Ok(sa) => {
                ldk_announced_listen_addr.push(sa);
            }
            Err(_) => {
                return Err(APIError::InvalidAnnounceAddresses(format!(
                    "failed to parse address '{addr}'"
                )))
            }
        }
    }
    let ldk_announced_node_name = match unlock_request.announce_alias {
        Some(s) => {
            if s.len() > 32 {
                return Err(APIError::InvalidAnnounceAlias(s!(
                    "cannot be longer than 32 bytes"
                )));
            }
            let mut bytes = [0; 32];
            bytes[..s.len()].copy_from_slice(s.as_bytes());
            bytes
        }
        None => [0; 32],
    };
    let peer_man = Arc::clone(&peer_manager);
    let chan_man = Arc::clone(&channel_manager);
    tokio::spawn(async move {
        // First wait a minute until we have some peers and maybe have opened a channel.
        tokio::time::sleep(Duration::from_secs(60)).await;
        // Then, update our announcement once an hour to keep it fresh but avoid unnecessary churn
        // in the global gossip network.
        let mut interval = tokio::time::interval(Duration::from_secs(3600));
        loop {
            interval.tick().await;
            // Don't bother trying to announce if we don't have any public channls, though our
            // peers should drop such an announcement anyway. Note that announcement may not
            // propagate until we have a channel with 6+ confirmations.
            if chan_man
                .list_channels()
                .iter()
                .any(|chan| chan.is_announced)
            {
                peer_man.broadcast_node_announcement(
                    [0; 3],
                    ldk_announced_node_name,
                    ldk_announced_listen_addr.clone(),
                );
            }
        }
    });

    tracing::info!("LDK logs are available at <your-supplied-ldk-data-dir-path>/.ldk/logs");
    tracing::info!("Local Node ID is {}", channel_manager.get_our_node_id());

    Ok((
        LdkBackgroundServices {
            stop_processing,
            peer_manager: peer_manager.clone(),
            bp_exit,
            background_processor: Some(background_processor),
            claimable_expiry_task: Some(claimable_expiry_task),
        },
        unlocked_state,
    ))
}

impl AppState {
    fn stop_ldk(&self) -> Option<JoinHandle<Result<(), io::Error>>> {
        let mut ldk_background_services = self.get_ldk_background_services();

        if ldk_background_services.is_none() {
            // node is locked
            tracing::info!("LDK is not running");
            return None;
        }

        let ldk_background_services = ldk_background_services.as_mut().unwrap();

        // Disconnect our peers and stop accepting new connections. This ensures we don't continue
        // updating our channel data after we've stopped the background processor.
        ldk_background_services
            .stop_processing
            .store(true, Ordering::Release);
        ldk_background_services.peer_manager.disconnect_all_peers();

        if let Some(claimable_task) = ldk_background_services.claimable_expiry_task.take() {
            claimable_task.abort();
        }

        // Stop the background processor.
        if !ldk_background_services.bp_exit.is_closed() {
            ldk_background_services.bp_exit.send(()).unwrap();
            ldk_background_services.background_processor.take()
        } else {
            None
        }
    }
}

pub(crate) async fn stop_ldk(app_state: Arc<AppState>) {
    tracing::info!("Stopping LDK");

    if let Some(join_handle) = app_state.stop_ldk() {
        join_handle.await.unwrap().unwrap();
    }

    // connect to the peer port so it can be released
    let peer_port = app_state.static_state.ldk_peer_listening_port;
    let sock_addr = SocketAddr::from(([127, 0, 0, 1], peer_port));
    let _ = check_port_is_available(peer_port);
    // check the peer port has been released
    let t_0 = OffsetDateTime::now_utc();
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        if TcpListener::bind(sock_addr).is_ok() {
            break;
        }
        if (OffsetDateTime::now_utc() - t_0).as_seconds_f32() > 10.0 {
            panic!("LDK peer port not being released")
        }
    }

    tracing::info!("Stopped LDK");
}
