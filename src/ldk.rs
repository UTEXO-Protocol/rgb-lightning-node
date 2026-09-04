use crate::asset_link::{
    AssetLinkAuthorizeParamsWire, AssetLinkAuthorizer, AssetLinkMessageHandler,
    ASSET_LINK_ERROR_DUPLICATE_PAYMENT_HASH, ASSET_LINK_ERROR_INSUFFICIENT_LIQUIDITY,
    ASSET_LINK_ERROR_UNKNOWN_ASSET, ASSET_LINK_ERROR_UNKNOWN_LINK,
};
use crate::async_order::{
    AsyncOrderInvoiceProvider, AsyncOrderMessageHandler, AsyncOrderOutboundInvoiceResultWire,
    AsyncOrderRequestInvoiceParamsWire, AsyncPaymentsPreimageRoot,
    ASYNC_ERROR_INVOICE_HASH_MISMATCH, ASYNC_ERROR_STALE_FLOW,
};
use crate::custom_msg_rpc::{CustomMessenger, CustomMsgPeerAccessControl, JsonRpcErrorWire};
use crate::synced_kv_store::SyncedKvStore;
use amplify::{map, s};
use bitcoin::blockdata::locktime::absolute::LockTime;
use bitcoin::hashes::{sha256, Hash as BitcoinHash};
use bitcoin::psbt::{ExtractTxError, Psbt};
use bitcoin::secp256k1::{All, PublicKey, Secp256k1};
use bitcoin::Sequence;
use bitcoin::{io, Amount, Network, Txid};
use bitcoin::{BlockHash, TxOut};
use bitcoin_bech32::WitnessProgram;
use hex::DisplayHex;
#[cfg(feature = "transaction-sync")]
use lightning::chain::Confirm;
use lightning::chain::{chainmonitor, transaction::OutPoint, ChannelMonitorUpdateStatus};
use lightning::chain::{BestBlock, Filter};
use lightning::events::bump_transaction::{BumpTransactionEventHandler, Wallet};
use lightning::events::{Event, PaymentFailureReason, PaymentPurpose, ReplayEvent};
use lightning::ln::channel_state::ChannelDetails;
use lightning::ln::channelmanager::{
    self, Bolt11InvoiceParameters, ChannelFundingType, PaymentId, RecentPaymentDetails,
};
use lightning::ln::channelmanager::{ChainParameters, ChannelManagerReadArgs};
use lightning::ln::msgs::SocketAddress;
use lightning::ln::peer_handler::{
    IgnoringMessageHandler, MessageHandler, PeerManager as LdkPeerManager,
};
use lightning::ln::types::ChannelId;
use lightning::onion_message::messenger::{
    DefaultMessageRouter, OnionMessenger as LdkOnionMessenger,
};
use lightning::rgb_utils::{
    deserialize_fascia, get_rgb_channel_info_pending, is_channel_rgb,
    read_pending_funding_acceptance, remove_pending_funding_acceptance, update_rgb_channel_amount,
    write_pending_funding_acceptance, FundingAcceptanceStage, PendingFundingAcceptance,
    RgbKvStoreExt, RGB_CHANNEL_INFO_NS, RGB_CHANNEL_INFO_PENDING_NS, RGB_COMMITMENT_FASCIA_NS,
    RGB_CONSIGNMENT_NS, RGB_FUNDING_ACCEPTANCE_NS, RGB_PAYMENT_INFO_INBOUND_NS,
    RGB_PAYMENT_INFO_OUTBOUND_NS, RGB_PRIMARY_NS,
};
use lightning::rgb_utils::{RgbInfo, RgbPaymentInfo, TransferInfo, STATIC_BLINDING};
use lightning::routing::gossip;
use lightning::routing::gossip::NodeId;
use lightning::routing::router::DefaultRouter;
use lightning::routing::scoring::{ProbabilisticScorer, ProbabilisticScoringFeeParameters};
use lightning::routing::utxo::UtxoLookup;
use lightning::sign::{KeysManager, OutputSpender, SpendableOutputDescriptor};
// Used by the non-VSS ChainMonitor encryptor closure and the signer unit tests.
#[cfg(feature = "block-sync")]
use lightning::chain;
#[cfg(feature = "vss")]
use lightning::chain::chainmonitor::AsyncPersister;
#[cfg(any(not(feature = "vss"), test))]
use lightning::sign::NodeSigner;
#[cfg(feature = "vss")]
use lightning::sign::PeerStorageKey;
use lightning::types::payment::{PaymentHash, PaymentPreimage};
use lightning::util::config::UserConfig;
use lightning::util::hash_tables::hash_map::Entry;
use lightning::util::hash_tables::{new_hash_map, HashMap as LdkHashMap};
#[cfg(feature = "vss")]
use lightning::util::native_async::FutureSpawner;
#[cfg(not(feature = "vss"))]
use lightning::util::persist::KVStoreSyncWrapper;
#[cfg(not(feature = "vss"))]
use lightning::util::persist::MonitorUpdatingPersister;
#[cfg(feature = "vss")]
use lightning::util::persist::MonitorUpdatingPersisterAsync;
use lightning::util::persist::{
    KVStoreSync, CHANNEL_MANAGER_PERSISTENCE_KEY, CHANNEL_MANAGER_PERSISTENCE_PRIMARY_NAMESPACE,
    CHANNEL_MANAGER_PERSISTENCE_SECONDARY_NAMESPACE, OUTPUT_SWEEPER_PERSISTENCE_KEY,
    OUTPUT_SWEEPER_PERSISTENCE_PRIMARY_NAMESPACE, OUTPUT_SWEEPER_PERSISTENCE_SECONDARY_NAMESPACE,
};
use lightning::util::ser::{Readable, ReadableArgs, Writeable};
use lightning::util::sweep as ldk_sweep;
use lightning::{impl_writeable_tlv_based, impl_writeable_tlv_based_enum};
use lightning_background_processor::{process_events_async, NO_LIQUIDITY_MANAGER};
#[cfg(feature = "block-sync")]
use lightning_block_sync::{init, poll, SpvClient, UnboundedCache};
use lightning_dns_resolver::OMDomainResolver;
use lightning_invoice::{Bolt11InvoiceDescription, PaymentSecret};
use lightning_net_tokio::SocketDescriptor;
use rand::RngCore;
use rgb_lib::{
    bdk_wallet::keys::{DerivableKey, ExtendedKey},
    bitcoin::{
        bip32::{ChildNumber, Xpriv},
        psbt::Psbt as RgbLibPsbt,
        secp256k1::Secp256k1 as Secp256k1_30,
        ScriptBuf,
    },
    keys::WitnessVersion,
    utils::{get_account_data, recipient_id_from_script_buf, script_buf_from_recipient_id},
    wallet::{
        rust_only::{check_indexer_url, AssetColoringInfo, ColoringInfo},
        DatabaseType, OnlineOptions, Recipient, SinglesigKeys, Wallet as RgbLibWallet, WalletData,
        WitnessData,
    },
    AssetSchema, Assignment, BitcoinNetwork, ConsignmentExt, ContractId, Error as RgbLibError,
    Fascia, FileContent, RgbTransfer, RgbTxid, TransferStatus, WitnessOrd,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::convert::TryInto;
use std::fs;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::net::ToSocketAddrs;
use std::net::{SocketAddr, TcpListener};
use std::path::{Path, PathBuf};
use std::str::FromStr;
#[cfg(test)]
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(feature = "test-utils")]
use std::sync::OnceLock;
use std::sync::{Arc, Mutex, MutexGuard, RwLock, Weak};
#[cfg(any(test, feature = "vss"))]
use std::time::Instant;
use std::time::{Duration, SystemTime};
use time::OffsetDateTime;
use tokio::runtime::Handle;
use tokio::sync::watch::Sender;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

#[cfg(feature = "vss")]
use crate::async_kv_store::RemoteFirstKvStore;
use crate::core_types::{
    HTLCStatus, LdkChainSync, NodeKeySource, SwapStatus, UnlockRequest, PENDING_SWAP_TIMEOUT_SECS,
};
use crate::database::RlnDatabase;
use crate::disk::{self, FilesystemLogger};
use crate::gossip::{GossipSource, GossipSourceConfig};

pub(crate) const INBOUND_PAYMENTS_KEY: &str = "inbound_payments";
const OUTBOUND_PAYMENTS_KEY: &str = "outbound_payments";
const CHANNEL_IDS_KEY: &str = "channel_ids";
const MAKER_SWAPS_KEY: &str = "maker_swaps";
const TAKER_SWAPS_KEY: &str = "taker_swaps";
const ASSET_LINK_SWAP_AUTHORIZATION_MAX_EXPIRY_SECS: u64 = 24 * 60 * 60;
const OUTPUT_SPENDER_TXES_KEY: &str = "output_spender_txes";
const OUTPUT_SWEEPER_WALLET_OPERATION_WAIT: Duration = Duration::from_secs(1);
pub(crate) const PSBT_NAMESPACE: &str = crate::synced_kv_store::PSBT_NAMESPACE;
pub(crate) const PENDING_FUNDING_NAMESPACE: &str =
    crate::synced_kv_store::PENDING_FUNDING_NAMESPACE;
/// Funding consignments keyed by funding txid, kept for wallet re-seeding
/// after a restore without an RGB backup (issue #111).
const FUNDING_CONSIGNMENT_NAMESPACE: &str = "funding_consignment";
/// Local-only marker: absent on a freshly restored device, so the fascia
/// replay reruns until it completes once.
const REIMPORT_MARKER_NAMESPACE: &str = "reimport_marker";
const REIMPORT_MARKER_KEY: &str = "fascia_replay";
pub(crate) const RGB_SENDER_FUNDING_NAMESPACE: &str =
    crate::synced_kv_store::RGB_SENDER_FUNDING_NAMESPACE;
const CONFIG_INDEXER_URL: &str = "indexer_url";
const CONFIG_BITCOIN_NETWORK: &str = "bitcoin_network";
const CONFIG_WALLET_FINGERPRINT: &str = "wallet_fingerprint";
const CONFIG_WALLET_ACCOUNT_XPUB_VANILLA: &str = "wallet_account_xpub_vanilla";
const CONFIG_WALLET_ACCOUNT_XPUB_COLORED: &str = "wallet_account_xpub_colored";
const CONFIG_WALLET_MASTER_FINGERPRINT: &str = "wallet_master_fingerprint";
const VIRTUAL_CHANNEL_DRAFTS_KEY: &str = "virtual_channel_drafts";
const VIRTUAL_CHANNEL_SESSIONS_KEY: &str = "virtual_channel_sessions";

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RgbSenderFundingStage {
    Preparing,
    StockPromoted,
    HandoffReady,
    HandedToLdk,
    BroadcastSafeObserved,
    Broadcasting,
    BroadcastCommitted,
    Finalized,
    DurablyCompleted,
    RollingBack,
    RetryRequired,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum RgbSenderConsignmentDelivery {
    #[default]
    Proxy,
    P2p,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct RgbSenderFundingRecord {
    version: u8,
    #[serde(default)]
    manual_broadcast: bool,
    temporary_channel_id: String,
    final_channel_id: Option<String>,
    funding_txid: String,
    batch_transfer_idx: i32,
    #[serde(default)]
    rgb_info: Option<RgbInfo>,
    #[serde(default)]
    consignment_delivery: RgbSenderConsignmentDelivery,
    stage: RgbSenderFundingStage,
}

impl RgbSenderFundingRecord {
    const LEGACY_VERSION: u8 = 1;
    const MANUAL_BROADCAST_VERSION: u8 = 2;
    const RGB_INFO_VERSION: u8 = 3;
    const VERSION: u8 = 4;

    fn validate(&self) -> Result<(), RgbLibError> {
        let is_fixed_hex = |value: &str, byte_len: usize| {
            value.len() == byte_len * 2
                && value.as_bytes().iter().all(|byte| byte.is_ascii_hexdigit())
        };
        if !matches!(
            self.version,
            Self::LEGACY_VERSION
                | Self::MANUAL_BROADCAST_VERSION
                | Self::RGB_INFO_VERSION
                | Self::VERSION
        ) || (self.version == Self::LEGACY_VERSION && self.manual_broadcast)
            || (self.version != Self::LEGACY_VERSION && !self.manual_broadcast)
            || !is_fixed_hex(&self.temporary_channel_id, 32)
            || !is_fixed_hex(&self.funding_txid, 32)
            || self
                .final_channel_id
                .as_ref()
                .is_some_and(|channel_id| !is_fixed_hex(channel_id, 32))
            || self.batch_transfer_idx < 0
            || (self.version >= Self::RGB_INFO_VERSION && self.rgb_info.is_none())
            || (self.version < Self::VERSION
                && self.consignment_delivery != RgbSenderConsignmentDelivery::Proxy)
            || (self.version == Self::VERSION
                && self.consignment_delivery != RgbSenderConsignmentDelivery::P2p)
        {
            return Err(RgbLibError::Internal {
                details: "invalid RGB sender funding journal".to_owned(),
            });
        }
        if matches!(
            self.stage,
            RgbSenderFundingStage::HandoffReady
                | RgbSenderFundingStage::HandedToLdk
                | RgbSenderFundingStage::BroadcastSafeObserved
                | RgbSenderFundingStage::Broadcasting
                | RgbSenderFundingStage::BroadcastCommitted
                | RgbSenderFundingStage::Finalized
                | RgbSenderFundingStage::DurablyCompleted
        ) && self.final_channel_id.is_none()
        {
            return Err(RgbLibError::Internal {
                details: "RGB sender funding journal stage requires a final channel ID".to_owned(),
            });
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RgbSenderRecoveryAction {
    Finalize,
    ResumeBroadcast,
    Rollback,
    FailClosed,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RgbFundingRecoveryState {
    pub(crate) funding_txid: String,
    pub(crate) temporary_channel_id: String,
    pub(crate) final_channel_id: Option<String>,
    pub(crate) stage: RgbFundingRecoveryStage,
    pub(crate) channel_is_durable: bool,
    pub(crate) transaction_is_known: Option<bool>,
    pub(crate) error: Option<String>,
    pub(crate) action: RgbFundingRecoveryAction,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RgbFundingRecoveryResolution {
    pub(crate) recovery: Option<RgbFundingRecoveryState>,
    pub(crate) recoveries: Vec<RgbFundingRecoveryState>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RgbFundingRecoveryStage {
    Sender(RgbSenderFundingStage),
    Receiver(FundingAcceptanceStage),
}

impl RgbFundingRecoveryStage {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Sender(stage) => match stage {
                RgbSenderFundingStage::Preparing => "sender_preparing",
                RgbSenderFundingStage::StockPromoted => "sender_stock_promoted",
                RgbSenderFundingStage::HandoffReady => "sender_handoff_ready",
                RgbSenderFundingStage::HandedToLdk => "sender_handed_to_ldk",
                RgbSenderFundingStage::BroadcastSafeObserved => "sender_broadcast_safe_observed",
                RgbSenderFundingStage::Broadcasting => "sender_broadcasting",
                RgbSenderFundingStage::BroadcastCommitted => "sender_broadcast_committed",
                RgbSenderFundingStage::Finalized => "sender_finalized",
                RgbSenderFundingStage::DurablyCompleted => "sender_durably_completed",
                RgbSenderFundingStage::RollingBack => "sender_rolling_back",
                RgbSenderFundingStage::RetryRequired => "sender_retry_required",
            },
            Self::Receiver(stage) => match stage {
                FundingAcceptanceStage::Validating => "receiver_validating",
                FundingAcceptanceStage::Prepared => "receiver_prepared",
                FundingAcceptanceStage::Promoted => "receiver_promoted",
                FundingAcceptanceStage::Finalizing => "receiver_finalizing",
                FundingAcceptanceStage::Finalized => "receiver_finalized",
                FundingAcceptanceStage::RollingBack => "receiver_rolling_back",
                FundingAcceptanceStage::RetryRequired => "receiver_retry_required",
            },
        }
    }

    const fn sort_key(self) -> (u8, u8) {
        match self {
            Self::Sender(stage) => (
                0,
                match stage {
                    RgbSenderFundingStage::Preparing => 0,
                    RgbSenderFundingStage::StockPromoted => 1,
                    RgbSenderFundingStage::HandoffReady => 2,
                    RgbSenderFundingStage::HandedToLdk => 3,
                    RgbSenderFundingStage::BroadcastSafeObserved => 4,
                    RgbSenderFundingStage::Broadcasting => 5,
                    RgbSenderFundingStage::BroadcastCommitted => 6,
                    RgbSenderFundingStage::Finalized => 7,
                    RgbSenderFundingStage::DurablyCompleted => 8,
                    RgbSenderFundingStage::RollingBack => 9,
                    RgbSenderFundingStage::RetryRequired => 10,
                },
            ),
            Self::Receiver(stage) => (
                1,
                match stage {
                    FundingAcceptanceStage::Validating => 0,
                    FundingAcceptanceStage::Prepared => 1,
                    FundingAcceptanceStage::Promoted => 2,
                    FundingAcceptanceStage::Finalizing => 3,
                    FundingAcceptanceStage::Finalized => 4,
                    FundingAcceptanceStage::RollingBack => 5,
                    FundingAcceptanceStage::RetryRequired => 6,
                },
            ),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RgbFundingRecoveryAction {
    RetryReconciliation,
    ResumeBroadcast,
    RetryChainObservation,
    ManualChannelStateRecovery,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RgbFundingRecoveryCommand {
    Recheck,
    ResumeBroadcast,
}

impl RgbFundingRecoveryAction {
    const fn as_str(self) -> &'static str {
        match self {
            Self::RetryReconciliation => "retry_reconciliation",
            Self::ResumeBroadcast => "resume_broadcast",
            Self::RetryChainObservation => "retry_chain_observation",
            Self::ManualChannelStateRecovery => "manual_channel_state_recovery",
        }
    }
}

/// Fail-closed admission state for interrupted RGB channel funding.
///
/// An unresolved sender or receiver journal can represent a transaction whose broadcast outcome
/// or matching LDK channel state is not yet known. Read-only inspection and recovery remain
/// available, but new RGB-wallet mutations must not be admitted until every record is reconciled.
///
/// The operation lease is intentionally wallet-wide. rgb-lib currently persists one pending RGB
/// acceptance and one rollback snapshot for the wallet, not one per funding allocation. Allowing
/// another stock mutation under a per-txid lock could overwrite the only rollback owner. This can
/// be narrowed only after the underlying stock journal identifies and isolates every allocation
/// touched by each concurrent operation.
#[derive(Debug, Default)]
pub(crate) struct RgbFundingRecoveryGuard {
    funding_txids: RwLock<BTreeSet<String>>,
    operation_lock: Arc<tokio::sync::Mutex<()>>,
}

pub(crate) struct RgbFundingOperationLease {
    _guard: tokio::sync::OwnedMutexGuard<()>,
    acquired_at: std::time::Instant,
    owner: &'static str,
}

impl RgbFundingOperationLease {
    fn new(guard: tokio::sync::OwnedMutexGuard<()>, owner: &'static str) -> Self {
        Self {
            _guard: guard,
            acquired_at: std::time::Instant::now(),
            owner,
        }
    }
}

impl Drop for RgbFundingOperationLease {
    fn drop(&mut self) {
        let held_for = self.acquired_at.elapsed();
        if held_for >= Duration::from_millis(250) {
            tracing::warn!(
                owner = self.owner,
                held_ms = held_for.as_millis(),
                "RGB wallet operation lease exceeded the latency budget"
            );
        }
    }
}

impl RgbFundingRecoveryGuard {
    pub(crate) async fn lock_operation(&self) -> RgbFundingOperationLease {
        RgbFundingOperationLease::new(
            Arc::clone(&self.operation_lock).lock_owned().await,
            "funding-event",
        )
    }

    pub(crate) fn blocking_lock_operation(&self) -> RgbFundingOperationLease {
        RgbFundingOperationLease::new(
            Arc::clone(&self.operation_lock).blocking_lock_owned(),
            "startup-reconciliation",
        )
    }

    pub(crate) fn blocking_lock_recovery_operation(&self) -> RgbFundingOperationLease {
        RgbFundingOperationLease::new(
            Arc::clone(&self.operation_lock).blocking_lock_owned(),
            "recovery-api",
        )
    }

    pub(crate) fn replace(&self, recoveries: &[RgbFundingRecoveryState]) {
        let mut funding_txids = self.funding_txids.write().unwrap_or_else(|poisoned| {
            tracing::error!("RGB funding recovery guard was poisoned; preserving quarantine");
            poisoned.into_inner()
        });
        *funding_txids = recoveries
            .iter()
            .map(|recovery| recovery.funding_txid.clone())
            .collect();
    }

    fn clear(&self, funding_txid: &str) {
        self.funding_txids
            .write()
            .unwrap_or_else(|poisoned| {
                tracing::error!("RGB funding recovery guard was poisoned; preserving quarantine");
                poisoned.into_inner()
            })
            .remove(funding_txid);
    }

    fn quarantine(&self, funding_txid: &str) {
        self.funding_txids
            .write()
            .unwrap_or_else(|poisoned| {
                tracing::error!("RGB funding recovery guard was poisoned; preserving quarantine");
                poisoned.into_inner()
            })
            .insert(funding_txid.to_owned());
    }

    pub(crate) fn lock_rgb_wallet_mutation(&self) -> Result<RgbFundingOperationLease, APIError> {
        // Admission and execution must be one atomic lease. A check-only gate permits a funding
        // transition to start immediately after the check, allowing its rollback snapshot to
        // overwrite a concurrent wallet mutation.
        let operation = Arc::clone(&self.operation_lock)
            .try_lock_owned()
            .map_err(|_| APIError::ChangingState)?;
        self.ensure_wallet_mutation_is_admitted()?;
        Ok(RgbFundingOperationLease::new(operation, "rgb-wallet-api"))
    }

    async fn lock_rgb_wallet_mutation_for(
        &self,
        wait: Duration,
        owner: &'static str,
    ) -> Result<RgbFundingOperationLease, APIError> {
        self.ensure_wallet_mutation_is_admitted()?;
        let operation = tokio::time::timeout(wait, Arc::clone(&self.operation_lock).lock_owned())
            .await
            .map_err(|_| APIError::ChangingState)?;
        self.ensure_wallet_mutation_is_admitted()?;
        Ok(RgbFundingOperationLease::new(operation, owner))
    }

    /// Give LDK's infrequent output sweep a bounded, fair chance to follow a short wallet API
    /// mutation. The bound keeps the background processor responsive during long funding work.
    pub(crate) async fn lock_output_sweeper_wallet_mutation(
        &self,
    ) -> Result<RgbFundingOperationLease, APIError> {
        self.lock_rgb_wallet_mutation_for(OUTPUT_SWEEPER_WALLET_OPERATION_WAIT, "output-sweeper")
            .await
    }

    fn ensure_wallet_mutation_is_admitted(&self) -> Result<(), APIError> {
        let funding_txids = self.funding_txids.read().unwrap_or_else(|poisoned| {
            tracing::error!("RGB funding recovery guard was poisoned; preserving quarantine");
            poisoned.into_inner()
        });
        if funding_txids.is_empty() {
            return Ok(());
        }
        Err(APIError::RgbFundingRecoveryRequired(
            funding_txids.iter().cloned().collect::<Vec<_>>().join(","),
        ))
    }

    /// Existing BTC-only Lightning traffic does not touch rgb-lib's stock or rollback snapshot and
    /// must remain available while an unrelated RGB funding record is quarantined. RGB channel
    /// payments retain the wallet-wide lease until rgb-lib can isolate concurrent stock journals
    /// by allocation.
    pub(crate) fn lock_channel_payment(
        &self,
        carries_rgb: bool,
    ) -> Result<Option<RgbFundingOperationLease>, APIError> {
        carries_rgb
            .then(|| self.lock_rgb_wallet_mutation())
            .transpose()
    }

    #[cfg(test)]
    fn snapshot(&self) -> Vec<String> {
        self.funding_txids
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .iter()
            .cloned()
            .collect()
    }
}

fn rgb_funding_recovery_view(
    record: &RgbSenderFundingRecord,
    channel_is_durable: bool,
    transaction_observation: Result<Option<bool>, &RgbLibError>,
    reconciliation_error: Option<&RgbLibError>,
) -> RgbFundingRecoveryState {
    let (transaction_is_known, observation_error, mut action) = match transaction_observation {
        Err(error) => {
            let action = if matches!(
                record.stage,
                RgbSenderFundingStage::Finalized | RgbSenderFundingStage::DurablyCompleted
            ) {
                if channel_is_durable {
                    RgbFundingRecoveryAction::RetryReconciliation
                } else {
                    RgbFundingRecoveryAction::ManualChannelStateRecovery
                }
            } else {
                RgbFundingRecoveryAction::RetryChainObservation
            };
            (None, Some(error.to_string()), action)
        }
        Ok(transaction_is_known) => {
            let recovery_action = rgb_sender_recovery_action(
                record,
                channel_is_durable,
                transaction_is_known.unwrap_or(false),
            );
            let action = match recovery_action {
                RgbSenderRecoveryAction::Finalize | RgbSenderRecoveryAction::Rollback => {
                    RgbFundingRecoveryAction::RetryReconciliation
                }
                RgbSenderRecoveryAction::ResumeBroadcast => {
                    RgbFundingRecoveryAction::ResumeBroadcast
                }
                RgbSenderRecoveryAction::FailClosed => {
                    RgbFundingRecoveryAction::ManualChannelStateRecovery
                }
            };
            (transaction_is_known, None, action)
        }
    };
    if reconciliation_error.is_some()
        && action != RgbFundingRecoveryAction::ManualChannelStateRecovery
    {
        action = RgbFundingRecoveryAction::RetryReconciliation;
    }
    RgbFundingRecoveryState {
        funding_txid: record.funding_txid.clone(),
        temporary_channel_id: record.temporary_channel_id.clone(),
        final_channel_id: record.final_channel_id.clone(),
        stage: RgbFundingRecoveryStage::Sender(record.stage),
        channel_is_durable,
        transaction_is_known,
        error: reconciliation_error
            .map(ToString::to_string)
            .or(observation_error),
        action,
    }
}

fn rgb_receiver_funding_recovery_view(
    record: &PendingFundingAcceptance,
    channel_is_durable: bool,
    error: Option<String>,
) -> Result<RgbFundingRecoveryState, RgbLibError> {
    let action = match rgb_receiver_recovery_action(record.stage, channel_is_durable) {
        RgbReceiverRecoveryAction::Quarantine => {
            RgbFundingRecoveryAction::ManualChannelStateRecovery
        }
        RgbReceiverRecoveryAction::Rollback
        | RgbReceiverRecoveryAction::Finalize
        | RgbReceiverRecoveryAction::Complete => RgbFundingRecoveryAction::RetryReconciliation,
    };
    Ok(RgbFundingRecoveryState {
        funding_txid: record.funding_txid.clone(),
        temporary_channel_id: record.temporary_channel_id.clone(),
        final_channel_id: Some(receiver_final_channel_id(record)?),
        stage: RgbFundingRecoveryStage::Receiver(record.stage),
        channel_is_durable,
        transaction_is_known: None,
        error,
        action,
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RgbReceiverRecoveryAction {
    Rollback,
    Finalize,
    Complete,
    Quarantine,
}

fn rgb_receiver_recovery_action(
    stage: FundingAcceptanceStage,
    channel_is_durable: bool,
) -> RgbReceiverRecoveryAction {
    match (stage, channel_is_durable) {
        (
            FundingAcceptanceStage::Validating
            | FundingAcceptanceStage::Prepared
            | FundingAcceptanceStage::RollingBack
            | FundingAcceptanceStage::RetryRequired,
            false,
        ) => RgbReceiverRecoveryAction::Rollback,
        (FundingAcceptanceStage::Promoted | FundingAcceptanceStage::Finalizing, true) => {
            RgbReceiverRecoveryAction::Finalize
        }
        (FundingAcceptanceStage::Finalized, true) => RgbReceiverRecoveryAction::Complete,
        _ => RgbReceiverRecoveryAction::Quarantine,
    }
}

fn rgb_sender_recovery_action(
    record: &RgbSenderFundingRecord,
    channel_is_durable: bool,
    transaction_is_known: bool,
) -> RgbSenderRecoveryAction {
    if matches!(
        record.stage,
        RgbSenderFundingStage::Finalized | RgbSenderFundingStage::DurablyCompleted
    ) {
        return if channel_is_durable {
            RgbSenderRecoveryAction::Finalize
        } else {
            RgbSenderRecoveryAction::FailClosed
        };
    }
    if transaction_is_known {
        return if channel_is_durable {
            RgbSenderRecoveryAction::Finalize
        } else {
            RgbSenderRecoveryAction::FailClosed
        };
    }
    if channel_is_durable {
        return if matches!(
            record.stage,
            RgbSenderFundingStage::Broadcasting | RgbSenderFundingStage::BroadcastCommitted
        ) {
            RgbSenderRecoveryAction::Finalize
        } else {
            RgbSenderRecoveryAction::ResumeBroadcast
        };
    }
    match record.stage {
        RgbSenderFundingStage::Preparing | RgbSenderFundingStage::StockPromoted => {
            RgbSenderRecoveryAction::Rollback
        }
        RgbSenderFundingStage::HandoffReady
        | RgbSenderFundingStage::HandedToLdk
        | RgbSenderFundingStage::BroadcastSafeObserved
            if record.manual_broadcast =>
        {
            RgbSenderRecoveryAction::Rollback
        }
        RgbSenderFundingStage::RollingBack | RgbSenderFundingStage::RetryRequired => {
            RgbSenderRecoveryAction::Rollback
        }
        // Version-one journals used LDK's automatic broadcast path. Once handoff may have begun,
        // absence from one indexer is not proof that the transaction was never broadcast.
        RgbSenderFundingStage::HandoffReady
        | RgbSenderFundingStage::HandedToLdk
        | RgbSenderFundingStage::BroadcastSafeObserved
        | RgbSenderFundingStage::Broadcasting
        | RgbSenderFundingStage::BroadcastCommitted
        | RgbSenderFundingStage::Finalized
        | RgbSenderFundingStage::DurablyCompleted => RgbSenderRecoveryAction::FailClosed,
    }
}
use crate::error::APIError;
#[cfg(feature = "block-sync")]
use crate::ldk_chain_backend::block_sync::{BitcoindClient, BlockSyncGossipVerifier};
#[cfg(feature = "transaction-sync")]
use crate::ldk_chain_backend::sync_chain_data;
#[cfg(feature = "transaction-sync")]
use crate::ldk_chain_backend::transaction_sync::{
    IndexerClient, IndexerGossipVerifier, IndexerSyncClient,
};
use crate::ldk_chain_backend::{ChainBackend, ChainSetup, DynBroadcaster, DynFeeEstimator};
use crate::rgb::{
    check_rgb_proxy_endpoint, get_rgb_channel_info_optional, RgbBumpWalletSource,
    RgbChangeDestinationSource, RgbLibWalletWrapper,
};
use crate::rgb_file_transfer::{
    PeerChannelGate, RgbFileTransferHandler, REASSEMBLY_SWEEP_INTERVAL,
};
use crate::signer::vls_adapter::{ExternalSignerBackend, VlsSignerAdapter};
use crate::signer::{
    read_key_source_file, validate_bootstrap_payload, validate_key_source_matches_bootstrap,
    ExternalSigner, ExternalSignerAttachment, ExternalSignerTransport, SUPPORTED_SIGNER_API_LEVEL,
};
use crate::signer::{
    ActiveSignerRef, DynRlnChannelSigner, DynRlnSigner, LightningEntropySource, RlnKeysInterface,
    SystemEntropySource,
};
use crate::swap::{SwapData, SwapInfo};
use crate::utils::{
    check_port_is_available, connect_peer_if_necessary, description_from_invoice,
    description_hash_from_invoice, do_connect_peer, get_current_timestamp,
    get_max_local_rgb_amount, hex_str, validate_and_parse_payment_hash,
    validate_and_parse_payment_preimage, AppState, StaticState, UnlockedAppState, FATAL_ERROR,
    PROXY_ENDPOINT_LOCAL, PROXY_ENDPOINT_PUBLIC,
};

const RGB_TRANSFER_CHAN_EXPIRATION_SECS: u64 = 86400;
// don't reuse a cached sweep receive this close to its expiration
const RGB_RECEIVE_REUSE_MARGIN_SECS: u64 = 3600;
// smaller margin when addresses are reused, where reissuing is harmful: still enough for the
// receive to outlast the sweep that uses it
const RGB_RECEIVE_REUSE_MARGIN_ADDR_REUSE_SECS: u64 = 300;
const VIRTUAL_CHANNEL_DOMAIN_SEPARATOR: &[u8] = b"rln_virtual_channels_v0";

// A reissued receive under address reuse gets the same recipient id as the cached one (rgb-lib
// rotates only the invoice nonce), so the sweep's provide_out_of_band_consignment later fails with
// an ambiguous-recipient error. Hold the cached entry longer in that case, keeping enough margin
// for it to outlast the sweep it is used by.
fn sweep_receive_is_reusable(now: u64, expiration: u64, reuse_addresses: bool) -> bool {
    let margin = if reuse_addresses {
        RGB_RECEIVE_REUSE_MARGIN_ADDR_REUSE_SECS
    } else {
        RGB_RECEIVE_REUSE_MARGIN_SECS
    };
    now + margin < expiration
}

pub(crate) fn virtual_channel_synthetic_outpoint(
    network: BitcoinNetwork,
    local_node_id: &PublicKey,
    peer_node_id: &PublicKey,
) -> OutPoint {
    let mut ordered = [local_node_id.serialize(), peer_node_id.serialize()];
    ordered.sort();
    let network_tag = match network {
        BitcoinNetwork::Mainnet => b"mainnet".as_slice(),
        BitcoinNetwork::Testnet => b"testnet".as_slice(),
        BitcoinNetwork::Testnet4 => b"testnet4".as_slice(),
        BitcoinNetwork::Regtest => b"regtest".as_slice(),
        BitcoinNetwork::Signet | BitcoinNetwork::SignetCustom => b"signet".as_slice(),
    };

    let mut preimage = Vec::with_capacity(
        VIRTUAL_CHANNEL_DOMAIN_SEPARATOR.len() + network_tag.len() + ordered[0].len() * 2,
    );
    preimage.extend_from_slice(VIRTUAL_CHANNEL_DOMAIN_SEPARATOR);
    preimage.extend_from_slice(network_tag);
    preimage.extend_from_slice(&ordered[0]);
    preimage.extend_from_slice(&ordered[1]);

    let txid = bitcoin::Txid::from_byte_array(
        <sha256::Hash as BitcoinHash>::hash(&preimage).to_byte_array(),
    );
    OutPoint { txid, index: 0 }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum InvoiceType {
    AutoClaim,
    Hodl { async_payment_recipient: bool },
}

impl_writeable_tlv_based_enum!(InvoiceType,
    (0, AutoClaim) => {},
    (1, Hodl) => {
        (0, async_payment_recipient, (default_value, false)),
    },
);

/// Save config to database (source of truth) and sync to KVStore for rust-lightning.
fn save_config(
    database: &sea_orm::DatabaseConnection,
    kv_store: &dyn KVStoreSync,
    key: &str,
    value: &str,
) -> Result<(), APIError> {
    let db = RlnDatabase::new(database.clone());
    db.set_config(key, value)?;
    kv_store.write_config(key, value);
    Ok(())
}

/// Sync config from database to KVStore on startup.
fn sync_config_to_kvstore(
    database: &sea_orm::DatabaseConnection,
    kv_store: &dyn KVStoreSync,
) -> Result<(), APIError> {
    let db = RlnDatabase::new(database.clone());

    for key in [
        CONFIG_INDEXER_URL,
        CONFIG_BITCOIN_NETWORK,
        CONFIG_WALLET_FINGERPRINT,
        CONFIG_WALLET_ACCOUNT_XPUB_VANILLA,
        CONFIG_WALLET_ACCOUNT_XPUB_COLORED,
        CONFIG_WALLET_MASTER_FINGERPRINT,
    ] {
        if let Some(value) = db.get_config(key)? {
            kv_store.write_config(key, &value);
        }
    }

    Ok(())
}

// Test-only: while set, the node with this pubkey defers claiming incoming payments; handling of
// the PaymentClaimable event is suspended until the gate is cleared
#[cfg(test)]
pub(crate) static DEFER_PAYMENT_CLAIMABLE_ON_NODE: Mutex<Option<PublicKey>> = Mutex::new(None);

// Test-only: whether a payment has been deferred via DEFER_PAYMENT_CLAIMABLE_ON_NODE since the
// gate was set. This is a flag rather than a count because a node handles its events sequentially:
// while a PaymentClaimable is being deferred no further event is handled, so at most one payment
// can be deferred at a time
#[cfg(test)]
pub(crate) static PAYMENT_CLAIMABLE_DEFERRED: AtomicBool = AtomicBool::new(false);

// Test-only: a payment is never deferred for longer than this, so that a test failing to release
// the gate fails on its own assertions instead of hanging the node's event handling
#[cfg(test)]
const MAX_PAYMENT_DEFERRAL: Duration = Duration::from_secs(60);

#[cfg(test)]
pub(crate) static IGNORE_INBOUND_CHANNELS_ON_NODE: Mutex<Option<PublicKey>> = Mutex::new(None);

// Test-only: the node with this pubkey holds incoming payments instead of claiming them, keeping
// their HTLCs pending
#[cfg(test)]
pub(crate) static HOLD_PAYMENT_CLAIMABLE_ON_NODE: Mutex<Option<PublicKey>> = Mutex::new(None);

// Test-only: number of payments held via HOLD_PAYMENT_CLAIMABLE_ON_NODE
#[cfg(test)]
pub(crate) static HELD_PAYMENT_CLAIMABLE_COUNT: AtomicUsize = AtomicUsize::new(0);

// Test-only: the node with this pubkey emits a `push_asset_amount` greater than the channel asset
// amount on the wire in `open_channel`, regardless of the value validated by its REST layer. Used
// to model a channel counterparty whose wire client is not bound by the sender-side clamp.
#[cfg(test)]
pub(crate) static FORCE_PUSH_ASSET_AMOUNT_ON_NODE: Mutex<Option<PublicKey>> = Mutex::new(None);

// Test-only: whether the given override targets the node we are running as
#[cfg(test)]
pub(crate) fn node_override_matches(
    target: &Mutex<Option<PublicKey>>,
    our_node_id: PublicKey,
) -> bool {
    target
        .lock()
        .unwrap()
        .as_ref()
        .is_some_and(|id| *id == our_node_id)
}

#[cfg(feature = "test-utils")]
fn processed_channel_ready_events() -> &'static Mutex<HashSet<(String, String)>> {
    static EVENTS: OnceLock<Mutex<HashSet<(String, String)>>> = OnceLock::new();
    EVENTS.get_or_init(|| Mutex::new(HashSet::new()))
}

#[cfg(feature = "test-utils")]
fn record_processed_channel_ready_event(channel_id: &ChannelId, node_id: PublicKey) {
    processed_channel_ready_events()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert((channel_id.to_string(), node_id.to_string()));
}

#[cfg(feature = "test-utils")]
#[allow(dead_code)]
pub(crate) fn processed_channel_ready_event_participants(channel_id: &ChannelId) -> usize {
    let channel_id = channel_id.to_string();
    processed_channel_ready_events()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .iter()
        .filter(|(processed_channel_id, _)| processed_channel_id == &channel_id)
        .count()
}

pub(crate) struct LdkBackgroundServices {
    stop_processing: Arc<AtomicBool>,
    gossip_shutdown: Arc<tokio::sync::Notify>,
    peer_manager: Arc<PeerManager>,
    bp_exit: Sender<()>,
    background_processor: Option<JoinHandle<Result<(), io::Error>>>,
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
    pub(crate) expires_at: Option<u64>,
    pub(crate) claim_deadline_height: Option<u32>,
    pub(crate) invoice_type: Option<InvoiceType>,
    pub(crate) description: Option<String>,
    pub(crate) description_hash: Option<[u8; 32]>,
    pub(crate) payment_idx: Option<u64>,
    pub(crate) async_hash_index: Option<u64>,
    pub(crate) async_host_node_id: Option<PublicKey>,
}

impl_writeable_tlv_based!(PaymentInfo, {
    (0, preimage, required),
    (2, secret, required),
    (4, status, required),
    (6, amt_msat, required),
    (8, created_at, required),
    (10, updated_at, required),
    (12, payee_pubkey, required),
    (14, expires_at, option),
    (16, claim_deadline_height, option),
    (18, invoice_type, option),
    (20, description_hash, option),
    (22, payment_idx, option),
    (24, async_hash_index, option),
    (26, async_host_node_id, option),
    // odd type so older binaries skip the field instead of failing the whole read
    (29, description, option),
});

pub(crate) struct InboundPaymentInfoStorage {
    pub(crate) payments: LdkHashMap<PaymentHash, PaymentInfo>,
}

impl_writeable_tlv_based!(InboundPaymentInfoStorage, {
    (0, payments, required),
});

pub(crate) struct OutboundPaymentInfoStorage {
    pub(crate) payments: LdkHashMap<PaymentId, PaymentInfo>,
}

impl_writeable_tlv_based!(OutboundPaymentInfoStorage, {
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

#[derive(Clone, Debug)]
pub(crate) struct VirtualChannelDraft {
    pub(crate) created_at: u64,
    pub(crate) peer_id: PublicKey,
    pub(crate) temporary_channel_id: ChannelId,
}

impl_writeable_tlv_based!(VirtualChannelDraft, {
    (0, created_at, required),
    (2, peer_id, required),
    (4, temporary_channel_id, required),
});

pub(crate) struct VirtualChannelDraftStore {
    pub(crate) entries: LdkHashMap<ChannelId, VirtualChannelDraft>,
}

impl_writeable_tlv_based!(VirtualChannelDraftStore, {
    (0, entries, required),
});

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum VirtualChannelSessionStatus {
    Active,
    AbandonPending,
    Abandoned,
}

impl_writeable_tlv_based_enum!(VirtualChannelSessionStatus,
    (0, Active) => {},
    (2, AbandonPending) => {},
    (4, Abandoned) => {},
);

#[derive(Clone, Debug)]
pub(crate) struct VirtualChannelSession {
    pub(crate) channel_id: ChannelId,
    pub(crate) created_at: u64,
    pub(crate) former_temporary_channel_id: ChannelId,
    pub(crate) peer_id: PublicKey,
    pub(crate) status: VirtualChannelSessionStatus,
    pub(crate) virtual_funding_txo: OutPoint,
    pub(crate) updated_at: u64,
}

impl_writeable_tlv_based!(VirtualChannelSession, {
    (0, channel_id, required),
    (2, created_at, required),
    (4, former_temporary_channel_id, required),
    (6, peer_id, required),
    (8, status, (default_value, VirtualChannelSessionStatus::Active)),
    (10, virtual_funding_txo, required),
    (12, updated_at, (default_value, created_at)),
});

pub(crate) struct VirtualChannelSessionStore {
    pub(crate) entries: LdkHashMap<ChannelId, VirtualChannelSession>,
}

impl_writeable_tlv_based!(VirtualChannelSessionStore, {
    (0, entries, required),
});

impl VirtualChannelSessionStore {
    pub(crate) fn contains_virtual_funding_txo(&self, virtual_funding_txo: &OutPoint) -> bool {
        self.entries
            .values()
            .any(|session| session.virtual_funding_txo == *virtual_funding_txo)
    }
}

fn persist_staged_inbound_payment(
    kv_store: &dyn KVStoreSync,
    next_payment_idx: &std::sync::atomic::AtomicU64,
    inbound: &mut InboundPaymentInfoStorage,
    payment_hash: PaymentHash,
    mut payment_info: PaymentInfo,
) -> Result<(), JsonRpcErrorWire> {
    if payment_info.payment_idx.is_none() {
        payment_info.payment_idx =
            Some(next_payment_idx.fetch_add(1, std::sync::atomic::Ordering::SeqCst));
    }
    let mut staged_inbound = InboundPaymentInfoStorage {
        payments: inbound.payments.clone(),
    };
    staged_inbound.payments.insert(payment_hash, payment_info);
    kv_store
        .write("", "", INBOUND_PAYMENTS_KEY, staged_inbound.encode())
        .map_err(|err| {
            JsonRpcErrorWire::internal_error(format!(
                "async_order_request_outbound_invoice_persist_failed: {err}"
            ))
        })?;
    inbound.payments = staged_inbound.payments;
    Ok(())
}

fn effective_inbound_payment_status(
    status: HTLCStatus,
    expires_at: Option<u64>,
    claim_deadline_height: Option<u32>,
    now: u64,
    height: u32,
) -> HTLCStatus {
    match status {
        HTLCStatus::Pending if expires_at.is_some_and(|expiry| now > expiry) => HTLCStatus::Failed,
        HTLCStatus::Claimable
            if claim_deadline_height.is_some_and(|deadline| height >= deadline)
                || expires_at.is_some_and(|expiry| now >= expiry) =>
        {
            HTLCStatus::Failed
        }
        _ => status,
    }
}

impl UnlockedAppState {
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

    pub(crate) fn update_taker_swap_pending_intercept(
        &self,
        payment_hash: &PaymentHash,
        intercept_id: channelmanager::InterceptId,
    ) {
        let mut taker_swaps = self.get_taker_swaps();
        let taker_swap = taker_swaps.swaps.get_mut(payment_hash).unwrap();
        taker_swap.status = SwapStatus::Pending;
        taker_swap.initiated_at = Some(get_current_timestamp());
        taker_swap.pending_intercept_id = Some(intercept_id);
        self.save_taker_swaps(taker_swaps);
    }

    pub(crate) fn update_taker_swap_status(&self, payment_hash: &PaymentHash, status: SwapStatus) {
        let mut taker_swaps = self.get_taker_swaps();
        let taker_swap = taker_swaps.swaps.get_mut(payment_hash).unwrap();
        match &status {
            SwapStatus::Succeeded | SwapStatus::Failed | SwapStatus::Expired => {
                taker_swap.completed_at = Some(get_current_timestamp());
                taker_swap.pending_intercept_id = None;
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
        self.kv_store
            .write("", "", MAKER_SWAPS_KEY, swaps.encode())
            .unwrap();
    }

    fn save_taker_swaps(&self, swaps: MutexGuard<SwapMap>) {
        self.kv_store
            .write("", "", TAKER_SWAPS_KEY, swaps.encode())
            .unwrap();
    }

    pub(crate) fn maker_swaps(&self) -> LdkHashMap<PaymentHash, SwapData> {
        self.get_maker_swaps().swaps.clone()
    }

    pub(crate) fn taker_swaps(&self) -> LdkHashMap<PaymentHash, SwapData> {
        self.get_taker_swaps().swaps.clone()
    }

    /// Assign a stable, monotonically increasing index to a payment if it does
    /// not already have one. Indices are shared across inbound and outbound
    /// payments so the two sets can be merged and paged in a stable order.
    pub(crate) fn stamp_payment_idx(&self, payment_info: &mut PaymentInfo) {
        if payment_info.payment_idx.is_none() {
            payment_info.payment_idx = Some(
                self.next_payment_idx
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst),
            );
        }
    }

    pub(crate) fn add_inbound_payment(
        &self,
        payment_hash: PaymentHash,
        mut payment_info: PaymentInfo,
    ) {
        let mut inbound = self.get_inbound_payments();
        self.stamp_payment_idx(&mut payment_info);
        inbound.payments.insert(payment_hash, payment_info);
        self.save_inbound_payments(inbound);
    }

    pub(crate) fn add_outbound_payment(
        &self,
        payment_id: PaymentId,
        mut payment_info: PaymentInfo,
    ) -> Result<(), APIError> {
        let mut outbound = self.get_outbound_payments();
        if let Some(existing_payment) = outbound.payments.get(&payment_id) {
            if !matches!(existing_payment.status, HTLCStatus::Failed) {
                return Err(APIError::DuplicatePayment(
                    existing_payment.status.to_string(),
                ));
            }
        }
        self.stamp_payment_idx(&mut payment_info);
        outbound.payments.insert(payment_id, payment_info);
        self.save_outbound_payments(outbound);
        Ok(())
    }

    pub(crate) fn fail_htlc_backwards_and_update_inbound_payment(
        &self,
        payment_hash: PaymentHash,
        status: HTLCStatus,
    ) {
        self.channel_manager.fail_htlc_backwards(&payment_hash);
        self.upsert_inbound_payment(
            payment_hash,
            status,
            None,
            None,
            None,
            self.channel_manager.get_our_node_id(),
            None,
            None,
        );
        clear_rgb_payment_pending(&payment_hash, true, self.kv_store.as_ref());
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

    pub(crate) fn list_updated_inbound_payments(&self) -> LdkHashMap<PaymentHash, PaymentInfo> {
        let now = get_current_timestamp();
        let height = self.channel_manager.current_best_block().height;

        let _rgb_wallet_operation = match self.lock_rgb_wallet_mutation() {
            Ok(operation) => operation,
            Err(error) => {
                let mut payments = self.inbound_payments();
                for payment_info in payments.values_mut() {
                    let effective_status = effective_inbound_payment_status(
                        payment_info.status,
                        payment_info.expires_at,
                        payment_info.claim_deadline_height,
                        now,
                        height,
                    );
                    if effective_status != payment_info.status {
                        payment_info.status = effective_status;
                        payment_info.updated_at = now;
                    }
                }
                tracing::debug!(
                    %error,
                    "returning effective inbound payment statuses without persisting them"
                );
                return payments;
            }
        };

        let mut inbound = self.get_inbound_payments();
        let mut failed = false;
        let mut claimables_to_fail = vec![];
        for (payment_hash, payment_info) in inbound.payments.iter_mut() {
            let effective_status = effective_inbound_payment_status(
                payment_info.status,
                payment_info.expires_at,
                payment_info.claim_deadline_height,
                now,
                height,
            );
            if effective_status == payment_info.status {
                continue;
            }

            match payment_info.status {
                HTLCStatus::Pending => {
                    payment_info.status = effective_status;
                    payment_info.updated_at = now;
                    failed = true;
                }
                HTLCStatus::Claimable => claimables_to_fail.push((
                    *payment_hash,
                    payment_info.claim_deadline_height,
                    payment_info.expires_at,
                )),
                _ => unreachable!("only pending and claimable payments can expire"),
            }
        }

        if claimables_to_fail.is_empty() {
            let payments = inbound.payments.clone();
            if failed {
                self.save_inbound_payments(inbound);
            }
            return payments;
        }

        if failed {
            self.save_inbound_payments(inbound);
        } else {
            drop(inbound);
        }

        for (payment_hash, claim_deadline_height, expires_at) in claimables_to_fail {
            tracing::info!(
                "Expiring claimable payment {:?} (deadline: {:?}, expiry: {:?})",
                payment_hash,
                claim_deadline_height,
                expires_at
            );
            self.fail_htlc_backwards_and_update_inbound_payment(payment_hash, HTLCStatus::Failed);
        }

        self.inbound_payments()
    }

    pub(crate) fn inbound_payments(&self) -> LdkHashMap<PaymentHash, PaymentInfo> {
        self.get_inbound_payments().payments.clone()
    }

    pub(crate) fn outbound_payments(&self) -> LdkHashMap<PaymentId, PaymentInfo> {
        self.get_outbound_payments().payments.clone()
    }

    pub(crate) fn save_inbound_payments(&self, inbound: MutexGuard<InboundPaymentInfoStorage>) {
        self.kv_store
            .write("", "", INBOUND_PAYMENTS_KEY, inbound.encode())
            .unwrap();
    }

    fn save_outbound_payments(&self, outbound: MutexGuard<OutboundPaymentInfoStorage>) {
        self.kv_store
            .write("", "", OUTBOUND_PAYMENTS_KEY, outbound.encode())
            .unwrap();
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn upsert_inbound_payment(
        &self,
        payment_hash: PaymentHash,
        status: HTLCStatus,
        preimage: Option<PaymentPreimage>,
        secret: Option<PaymentSecret>,
        amt_msat: Option<u64>,
        payee_pubkey: PublicKey,
        claim_deadline_height: Option<u32>,
        invoice_type: Option<InvoiceType>,
    ) {
        let mut inbound = self.get_inbound_payments();
        match inbound.payments.entry(payment_hash) {
            Entry::Occupied(mut e) => {
                let payment_info = e.get_mut();
                payment_info.status = status;
                payment_info.preimage = preimage;
                payment_info.secret = secret;
                if amt_msat.is_some() {
                    payment_info.amt_msat = amt_msat;
                }
                payment_info.updated_at = get_current_timestamp();
                if claim_deadline_height.is_some() {
                    payment_info.claim_deadline_height = claim_deadline_height;
                }
            }
            Entry::Vacant(e) => {
                let created_at = get_current_timestamp();
                let mut payment_info = PaymentInfo {
                    preimage,
                    secret,
                    status,
                    amt_msat,
                    created_at,
                    updated_at: created_at,
                    payee_pubkey,
                    expires_at: None,
                    claim_deadline_height,
                    invoice_type,
                    description: None,
                    description_hash: None,
                    payment_idx: None,
                    async_hash_index: None,
                    async_host_node_id: None,
                };
                self.stamp_payment_idx(&mut payment_info);
                e.insert(payment_info);
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

    pub(crate) fn delete_channel_id(&self, channel_id: ChannelId) -> Option<ChannelId> {
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
            Some(temporary_channel_id)
        } else {
            None
        }
    }

    fn save_channel_ids_map(&self, channel_ids: MutexGuard<ChannelIdsMap>) {
        self.kv_store
            .write("", "", CHANNEL_IDS_KEY, channel_ids.encode())
            .unwrap();
    }

    pub(crate) fn virtual_channel_add_intent(
        &self,
        peer_id: PublicKey,
        temporary_channel_id: Option<ChannelId>,
    ) -> Result<ChannelId, APIError> {
        let mut drafts = self.get_virtual_channel_draft_store();
        let duplicate_virtual_draft = drafts
            .entries
            .values()
            .any(|draft| draft.peer_id == peer_id);
        if duplicate_virtual_draft {
            return Err(APIError::InvalidRequest(
                "virtual channel draft already exists for this peer pair".to_string(),
            ));
        }

        let duplicate_virtual_session = self
            .get_virtual_channel_session_store()
            .entries
            .values()
            .any(|session| session.peer_id == peer_id);
        if duplicate_virtual_session {
            return Err(APIError::InvalidRequest(
                "virtual channel session already exists for this peer pair".to_string(),
            ));
        }

        let channel_ids = self.channel_ids();
        let temporary_channel_id = if let Some(temporary_channel_id) = temporary_channel_id {
            if channel_ids.contains_key(&temporary_channel_id)
                || drafts.entries.contains_key(&temporary_channel_id)
            {
                return Err(APIError::TemporaryChannelIdAlreadyUsed);
            }
            temporary_channel_id
        } else {
            loop {
                let mut tmp_channel_id_bytes = [0u8; 32];
                tmp_channel_id_bytes
                    .copy_from_slice(&self.entropy_source.get_secure_random_bytes()[..32]);
                let candidate = ChannelId::from_bytes(tmp_channel_id_bytes);
                if !channel_ids.contains_key(&candidate) && !drafts.entries.contains_key(&candidate)
                {
                    break candidate;
                }
            }
        };

        drafts.entries.insert(
            temporary_channel_id,
            VirtualChannelDraft {
                temporary_channel_id,
                peer_id,
                created_at: get_current_timestamp(),
            },
        );
        self.virtual_channel_draft_store_save(drafts);

        Ok(temporary_channel_id)
    }

    pub(crate) fn virtual_channel_draft_delete(&self, temporary_channel_id: &ChannelId) {
        let mut drafts = self.get_virtual_channel_draft_store();
        drafts.entries.remove(temporary_channel_id);
        self.virtual_channel_draft_store_save(drafts);
    }

    pub(crate) fn virtual_channel_draft_get(
        &self,
        temporary_channel_id: &ChannelId,
    ) -> Option<VirtualChannelDraft> {
        self.get_virtual_channel_draft_store()
            .entries
            .get(temporary_channel_id)
            .cloned()
    }

    pub(crate) fn virtual_channel_draft_store(&self) -> LdkHashMap<ChannelId, VirtualChannelDraft> {
        self.get_virtual_channel_draft_store().entries.clone()
    }

    fn virtual_channel_draft_store_save(&self, drafts: MutexGuard<VirtualChannelDraftStore>) {
        self.kv_store
            .write("", "", VIRTUAL_CHANNEL_DRAFTS_KEY, drafts.encode())
            .unwrap();
    }

    pub(crate) fn virtual_channel_ensure_no_client_value(
        &self,
        chan_details: &ChannelDetails,
    ) -> Result<(), String> {
        if chan_details.has_inflight_htlcs {
            return Err("virtual cleanup is blocked while HTLCs are still in flight".to_string());
        }
        let channel_id_hex = chan_details.channel_id.0.as_hex().to_string();
        let kv = self.kv_store.as_ref();

        for namespace in [RGB_PAYMENT_INFO_INBOUND_NS, RGB_PAYMENT_INFO_OUTBOUND_NS] {
            let keys = kv
                .list(RGB_PRIMARY_NS, namespace)
                .map_err(|_| "virtual cleanup could not inspect RGB temp artifacts".to_string())?;
            for key in keys {
                if key.starts_with(&channel_id_hex)
                    && key.len() > channel_id_hex.len()
                    && key.ends_with("_pending")
                {
                    return Err(
                        "virtual cleanup is blocked while RGB payment temp artifacts remain"
                            .to_string(),
                    );
                }
            }
        }

        match chan_details.counterparty_balance_sats_floor {
            Some(0) => {}
            Some(balance_floor) => {
                return Err(format!(
                    "virtual cleanup is blocked while counterparty BTC balance floor is {balance_floor} sat"
                ))
            }
            None => {
                return Err(
                    "virtual cleanup requires an exact counterparty BTC balance floor proof"
                        .to_string(),
                )
            }
        }

        if let Ok(rgb_state) = kv.read_rgb_channel_info(&channel_id_hex, false) {
            kv.write_rgb_channel_info(&channel_id_hex, &rgb_state, true);
        }

        let final_rgb_state = kv.read_rgb_channel_info(&channel_id_hex, false);
        let pending_rgb_state = kv.read_rgb_channel_info(&channel_id_hex, true);

        let is_rgb_backed = final_rgb_state.is_ok() || pending_rgb_state.is_ok();
        if !is_rgb_backed {
            return Ok(());
        }

        let final_rgb_state = final_rgb_state.map_err(|_| {
            "virtual cleanup requires both final and pending RGB channel state".to_string()
        })?;
        let pending_rgb_state = pending_rgb_state.map_err(|_| {
            "virtual cleanup requires both final and pending RGB channel state".to_string()
        })?;

        if final_rgb_state.contract_id != pending_rgb_state.contract_id
            || final_rgb_state.schema != pending_rgb_state.schema
            || final_rgb_state.local_rgb_amount != pending_rgb_state.local_rgb_amount
            || final_rgb_state.remote_rgb_amount != pending_rgb_state.remote_rgb_amount
        {
            return Err(
                "virtual cleanup is blocked while RGB channel state is still diverged".to_string(),
            );
        }

        if final_rgb_state.remote_rgb_amount != 0 {
            return Err(format!(
                "virtual cleanup is blocked while counterparty RGB balance is {}",
                final_rgb_state.remote_rgb_amount
            ));
        }

        Ok(())
    }

    pub(crate) fn virtual_channel_session_add(&self, session: VirtualChannelSession) {
        let mut sessions = self.get_virtual_channel_session_store();
        sessions.entries.insert(session.channel_id, session);
        self.virtual_channel_session_store_save(sessions);
    }

    pub(crate) fn virtual_channel_session_get(
        &self,
        channel_id: &ChannelId,
    ) -> Option<VirtualChannelSession> {
        self.get_virtual_channel_session_store()
            .entries
            .get(channel_id)
            .cloned()
    }

    pub(crate) fn virtual_channel_session_update(&self, session: VirtualChannelSession) {
        let mut sessions = self.get_virtual_channel_session_store();
        sessions.entries.insert(session.channel_id, session);
        self.virtual_channel_session_store_save(sessions);
    }

    pub(crate) fn virtual_channel_session_update_status(
        &self,
        session: &VirtualChannelSession,
        status: VirtualChannelSessionStatus,
    ) {
        let mut updated_session = session.clone();
        updated_session.status = status;
        updated_session.updated_at = get_current_timestamp();
        self.virtual_channel_session_update(updated_session);
    }

    pub(crate) fn virtual_channel_session_store(
        &self,
    ) -> LdkHashMap<ChannelId, VirtualChannelSession> {
        self.get_virtual_channel_session_store().entries.clone()
    }

    fn virtual_channel_session_store_save(&self, sessions: MutexGuard<VirtualChannelSessionStore>) {
        self.kv_store
            .write("", "", VIRTUAL_CHANNEL_SESSIONS_KEY, sessions.encode())
            .unwrap();
    }
}

/// `FutureSpawner` backed by `tokio::spawn`, used to drive async monitor
/// persistence completions for `MonitorUpdatingPersisterAsync`.
#[cfg(feature = "vss")]
pub(crate) struct TokioFutureSpawner;

#[cfg(feature = "vss")]
impl FutureSpawner for TokioFutureSpawner {
    fn spawn<T: std::future::Future<Output = ()> + Send + 'static>(&self, future: T) {
        tokio::spawn(future);
    }
}

/// The `ChainMonitor` persister generic: with VSS, the async
/// `MonitorUpdatingPersisterAsync` (wrapped as `AsyncPersister`, returning
/// `InProgress`); without VSS, the synchronous `MonitorUpdatingPersister`.
#[cfg(feature = "vss")]
pub(crate) type MonitorPersister = AsyncPersister<
    Arc<RemoteFirstKvStore>,
    TokioFutureSpawner,
    Arc<FilesystemLogger>,
    ActiveSignerRef,
    ActiveSignerRef,
    Arc<DynBroadcaster>,
    Arc<DynFeeEstimator>,
>;

#[cfg(not(feature = "vss"))]
pub(crate) type MonitorPersister = Arc<
    MonitorUpdatingPersister<
        Arc<SyncedKvStore>,
        Arc<FilesystemLogger>,
        ActiveSignerRef,
        ActiveSignerRef,
        Arc<DynBroadcaster>,
        Arc<DynFeeEstimator>,
    >,
>;

pub(crate) type ChainMonitor = chainmonitor::ChainMonitor<
    DynRlnChannelSigner,
    Arc<dyn Filter + Send + Sync>,
    Arc<DynBroadcaster>,
    Arc<DynFeeEstimator>,
    Arc<FilesystemLogger>,
    MonitorPersister,
    ActiveSignerRef,
>;

pub(crate) type RoutingMessageHandler =
    dyn lightning::ln::msgs::RoutingMessageHandler + Send + Sync;

pub(crate) type PeerManager = LdkPeerManager<
    SocketDescriptor,
    Arc<ChannelManager>,
    Arc<RoutingMessageHandler>,
    Arc<OnionMessenger>,
    Arc<FilesystemLogger>,
    Arc<CustomMessenger>,
    ActiveSignerRef,
    Arc<ChainMonitor>,
>;

pub(crate) type Scorer = ProbabilisticScorer<Arc<NetworkGraph>, Arc<FilesystemLogger>>;

pub(crate) type Router = DefaultRouter<
    Arc<NetworkGraph>,
    Arc<FilesystemLogger>,
    Arc<LightningEntropySource>,
    Arc<RwLock<Scorer>>,
    ProbabilisticScoringFeeParameters,
    Scorer,
>;

pub(crate) type ChannelManager = channelmanager::ChannelManager<
    Arc<ChainMonitor>,
    Arc<DynBroadcaster>,
    Arc<LightningEntropySource>,
    ActiveSignerRef,
    ActiveSignerRef,
    Arc<DynFeeEstimator>,
    Arc<Router>,
    Arc<
        DefaultMessageRouter<Arc<NetworkGraph>, Arc<FilesystemLogger>, Arc<LightningEntropySource>>,
    >,
    Arc<FilesystemLogger>,
>;

impl PeerChannelGate for ChannelManager {
    fn channel_count_with(&self, peer: &PublicKey) -> usize {
        // unlike list_channels, this doesn't filter out unfunded channels
        self.list_channels_with_counterparty(peer).len()
    }

    fn has_channel_funded_by(&self, funding_txid: &str) -> bool {
        self.list_channels().iter().any(|chan| {
            chan.funding_txo
                .is_some_and(|txo| txo.txid.to_string() == funding_txid)
        })
    }
}

pub(crate) type NetworkGraph = gossip::NetworkGraph<Arc<FilesystemLogger>>;

// the UTXO lookup is a trait object so a single gossip type serves both sync backends
pub(crate) type P2PGossipSync = lightning::routing::gossip::P2PGossipSync<
    Arc<NetworkGraph>,
    Arc<dyn UtxoLookup + Send + Sync>,
    Arc<FilesystemLogger>,
>;

pub(crate) type RapidGossipSync =
    lightning_rapid_gossip_sync::RapidGossipSync<Arc<NetworkGraph>, Arc<FilesystemLogger>>;

pub(crate) type GossipSync = lightning_background_processor::GossipSync<
    Arc<P2PGossipSync>,
    Arc<RapidGossipSync>,
    Arc<NetworkGraph>,
    Arc<dyn UtxoLookup + Send + Sync>,
    Arc<FilesystemLogger>,
>;

pub(crate) type OnionMessenger = LdkOnionMessenger<
    Arc<LightningEntropySource>,
    ActiveSignerRef,
    Arc<FilesystemLogger>,
    Arc<ChannelManager>,
    Arc<
        DefaultMessageRouter<Arc<NetworkGraph>, Arc<FilesystemLogger>, Arc<LightningEntropySource>>,
    >,
    Arc<ChannelManager>,
    Arc<ChannelManager>,
    Arc<OMDomainResolver<Arc<ChannelManager>>>,
    IgnoringMessageHandler,
>;

pub(crate) type BumpTxEventHandler = BumpTransactionEventHandler<
    Arc<DynBroadcaster>,
    Arc<Wallet<Arc<RgbBumpWalletSource>, Arc<FilesystemLogger>>>,
    ActiveSignerRef,
    Arc<FilesystemLogger>,
>;

pub(crate) type OutputSpenderTxes = LdkHashMap<u64, bitcoin::Transaction>;
// (descriptors hash, contract) -> (recipient id, expiration)
type SweepRecipients = HashMap<(u64, ContractId), (String, u64)>;

pub(crate) struct RgbOutputSpender {
    static_state: Arc<StaticState>,
    rgb_wallet_wrapper: Arc<RgbLibWalletWrapper>,
    signer: Arc<dyn RlnKeysInterface<EcdsaSigner = DynRlnChannelSigner>>,
    kv_store: Arc<SyncedKvStore>,
    txes: Arc<Mutex<OutputSpenderTxes>>,
    // receives issued for an in-flight sweep, reused across retries so a repeatedly failing sweep
    // does not leave a new receive slot behind on every attempt
    sweep_recipients: Arc<Mutex<SweepRecipients>>,
    rgb_funding_recovery_guard: Arc<RgbFundingRecoveryGuard>,
}

// The sweeper store type is shared with the background processor's persister
// (same generic in `process_events_async`).
#[cfg(feature = "vss")]
pub(crate) type BpKvStore = Arc<crate::async_kv_store::BpKvStoreRouter>;
#[cfg(not(feature = "vss"))]
pub(crate) type BpKvStore = KVStoreSyncWrapper<Arc<SyncedKvStore>>;

pub(crate) type OutputSweeper = ldk_sweep::OutputSweeper<
    Arc<DynBroadcaster>,
    Arc<RgbChangeDestinationSource>,
    Arc<DynFeeEstimator>,
    Arc<dyn Filter + Send + Sync>,
    BpKvStore,
    Arc<FilesystemLogger>,
    Arc<RgbOutputSpender>,
>;

trait LiveChannelLookup: Send + Sync {
    fn peer_has_live_channel(&self, peer: &PublicKey) -> bool;
}

pub(crate) fn peer_has_live_channel(channel_manager: &ChannelManager, peer: &PublicKey) -> bool {
    channel_manager
        .list_usable_channels()
        .iter()
        .any(|channel| channel.counterparty.node_id == *peer)
}

struct UsablePeerChannelLookup {
    channel_manager: Arc<ChannelManager>,
}

impl LiveChannelLookup for UsablePeerChannelLookup {
    fn peer_has_live_channel(&self, peer: &PublicKey) -> bool {
        peer_has_live_channel(&self.channel_manager, peer)
    }
}

struct LiveChannelAccess {
    lookup: Arc<dyn LiveChannelLookup>,
}

impl LiveChannelAccess {
    fn new(channel_manager: Arc<ChannelManager>) -> Self {
        Self {
            lookup: Arc::new(UsablePeerChannelLookup { channel_manager }),
        }
    }

    #[cfg(test)]
    fn new_with_lookup(lookup: Arc<dyn LiveChannelLookup>) -> Self {
        Self { lookup }
    }
}

impl CustomMsgPeerAccessControl for LiveChannelAccess {
    fn allows_peer(&self, peer: &PublicKey) -> bool {
        self.lookup.peer_has_live_channel(peer)
    }
}

struct NodeAssetLinkAuthorizer {
    unlocked_state_weak: Weak<UnlockedAppState>,
    channel_manager: Arc<ChannelManager>,
    kv_store: Arc<SyncedKvStore>,
    taker_swaps: Arc<Mutex<SwapMap>>,
}

fn reserved_outbound_rgb_amount(
    taker_swaps: &SwapMap,
    outbound_contract_id: ContractId,
    current_time: u64,
) -> u64 {
    taker_swaps
        .swaps
        .values()
        .filter(|swap| {
            if swap.swap_info.from_asset != Some(outbound_contract_id) {
                return false;
            }

            match swap.status {
                SwapStatus::Waiting => current_time <= swap.swap_info.expiry,
                SwapStatus::Pending => swap.initiated_at.is_none_or(|initiated_at| {
                    current_time <= initiated_at.saturating_add(PENDING_SWAP_TIMEOUT_SECS)
                }),
                SwapStatus::Succeeded | SwapStatus::Expired | SwapStatus::Failed => false,
            }
        })
        .fold(0, |reserved_amount, swap| {
            reserved_amount.saturating_add(swap.swap_info.qty_from)
        })
}

impl AssetLinkAuthorizer for NodeAssetLinkAuthorizer {
    fn authorize_swap(
        &self,
        sender_node_id: PublicKey,
        params: &AssetLinkAuthorizeParamsWire,
    ) -> Result<(), JsonRpcErrorWire> {
        let payment_hash = validate_and_parse_payment_hash(&params.payment_hash)
            .map_err(|_| JsonRpcErrorWire::invalid_params("invalid_payment_hash"))?;
        let asset_contract_id = ContractId::from_str(&params.asset_id)
            .map_err(|_| JsonRpcErrorWire::invalid_params("invalid_asset_id"))?;
        let linked_contract_id = ContractId::from_str(&params.linked_asset_id)
            .map_err(|_| JsonRpcErrorWire::invalid_params("invalid_linked_asset_id"))?;
        if params.amount == 0 {
            return Err(JsonRpcErrorWire::invalid_params("invalid_amount"));
        }
        if params.expiry_sec == 0 {
            return Err(JsonRpcErrorWire::invalid_params("invalid_expiry_sec"));
        }

        let unlocked_state = self.unlocked_state_weak.upgrade().ok_or_else(|| {
            JsonRpcErrorWire::internal_error("asset_link_state_unavailable".to_owned())
        })?;
        let asset_metadata = unlocked_state
            .rgb_get_asset_metadata(asset_contract_id)
            .map_err(|_| {
                JsonRpcErrorWire::application_error(ASSET_LINK_ERROR_UNKNOWN_ASSET, "unknown_link")
            })?;
        let linked_asset_metadata = unlocked_state
            .rgb_get_asset_metadata(linked_contract_id)
            .map_err(|_| {
                JsonRpcErrorWire::application_error(ASSET_LINK_ERROR_UNKNOWN_ASSET, "unknown_link")
            })?;

        let asset_is_parent_of_linked_asset = asset_metadata.linked_to_asset_id.as_deref()
            == Some(params.linked_asset_id.as_str())
            && linked_asset_metadata.linked_from_asset_id.as_deref()
                == Some(params.asset_id.as_str());

        let linked_asset_is_parent_of_asset = linked_asset_metadata.linked_to_asset_id.as_deref()
            == Some(params.asset_id.as_str())
            && asset_metadata.linked_from_asset_id.as_deref()
                == Some(params.linked_asset_id.as_str());

        let parent_contract_id = if asset_is_parent_of_linked_asset {
            Some(asset_contract_id)
        } else if linked_asset_is_parent_of_asset {
            Some(linked_contract_id)
        } else {
            None
        };

        let Some(parent_contract_id) = parent_contract_id else {
            return Err(JsonRpcErrorWire::application_error(
                ASSET_LINK_ERROR_UNKNOWN_LINK,
                "unknown_link",
            ));
        };

        if asset_metadata.asset_schema != AssetSchema::Ifa
            || linked_asset_metadata.asset_schema != AssetSchema::Ifa
        {
            return Err(JsonRpcErrorWire::application_error(
                ASSET_LINK_ERROR_UNKNOWN_LINK,
                "unknown_link",
            ));
        }

        let link_is_settled = unlocked_state
            .rgb_find_link_transfer(parent_contract_id)
            .map_err(|_| {
                JsonRpcErrorWire::application_error(ASSET_LINK_ERROR_UNKNOWN_LINK, "unknown_link")
            })?
            .is_some_and(|transfer| transfer.status == TransferStatus::Settled);
        if !link_is_settled {
            return Err(JsonRpcErrorWire::application_error(
                ASSET_LINK_ERROR_UNKNOWN_LINK,
                "unknown_link",
            ));
        }

        let max_balance = get_max_local_rgb_amount(
            asset_contract_id,
            self.channel_manager.list_channels().iter(),
            self.kv_store.as_ref(),
        );
        let mut taker_swaps = self.taker_swaps.lock().unwrap();
        if taker_swaps.swaps.contains_key(&payment_hash) {
            return Err(JsonRpcErrorWire::application_error(
                ASSET_LINK_ERROR_DUPLICATE_PAYMENT_HASH,
                "duplicate_payment_hash",
            ));
        }

        let authorization_time = get_current_timestamp();
        let reserved_amount =
            reserved_outbound_rgb_amount(&taker_swaps, asset_contract_id, authorization_time);
        let available_amount = max_balance.saturating_sub(reserved_amount);
        if params.amount > available_amount {
            return Err(JsonRpcErrorWire::application_error(
                ASSET_LINK_ERROR_INSUFFICIENT_LIQUIDITY,
                "insufficient_liquidity",
            ));
        }

        let expiry_sec = params
            .expiry_sec
            .min(ASSET_LINK_SWAP_AUTHORIZATION_MAX_EXPIRY_SECS);
        let one_to_one_redemption_amount = params.amount;
        let swap_info = SwapInfo {
            qty_from: one_to_one_redemption_amount,
            qty_to: one_to_one_redemption_amount,
            from_asset: Some(asset_contract_id),
            to_asset: Some(linked_contract_id),
            expiry: authorization_time.saturating_add(expiry_sec),
        };
        assert_eq!(
            swap_info.qty_from, swap_info.qty_to,
            "linked-asset redemption must remain 1:1"
        );
        let mut swap_data = SwapData::create_from_swap_info(&swap_info);
        swap_data.authorized_peer = Some(sender_node_id);
        taker_swaps.swaps.insert(payment_hash, swap_data);
        if let Err(e) = self
            .kv_store
            .write("", "", TAKER_SWAPS_KEY, taker_swaps.encode())
        {
            taker_swaps.swaps.remove(&payment_hash);
            return Err(JsonRpcErrorWire::internal_error(format!(
                "taker_swaps_write_failed: {e}"
            )));
        }

        tracing::info!(
            peer = %sender_node_id,
            payment_hash = %hex_str(&payment_hash.0),
            asset_id = %params.asset_id,
            linked_asset_id = %params.linked_asset_id,
            amount = params.amount,
            "authorized linked-asset payment"
        );
        Ok(())
    }
}

struct AsyncOrderRecipientInvoiceProvider {
    config: Arc<crate::config::Config>,
    channel_manager: Arc<ChannelManager>,
    inbound_payments: Arc<Mutex<InboundPaymentInfoStorage>>,
    async_payments_preimage_root: Arc<AsyncPaymentsPreimageRoot>,
    kv_store: Arc<SyncedKvStore>,
    next_payment_idx: Arc<std::sync::atomic::AtomicU64>,
    external_signer_mode: bool,
    external_signer: Option<Arc<ExternalSigner>>,
}

impl AsyncOrderRecipientInvoiceProvider {
    fn parse_u64_field(value: &str, field: &str) -> Result<u64, JsonRpcErrorWire> {
        value
            .parse::<u64>()
            .map_err(|_| JsonRpcErrorWire::invalid_params(format!("invalid_{field}")))
    }

    fn stale_flow_error() -> JsonRpcErrorWire {
        JsonRpcErrorWire::application_error(ASYNC_ERROR_STALE_FLOW, "stale_flow")
    }
}

impl AsyncOrderInvoiceProvider for AsyncOrderRecipientInvoiceProvider {
    fn request_outbound_invoice(
        &self,
        sender_node_id: PublicKey,
        params: AsyncOrderRequestInvoiceParamsWire,
    ) -> Result<AsyncOrderOutboundInvoiceResultWire, JsonRpcErrorWire> {
        let hash_index = Self::parse_u64_field(&params.hash_index, "hash_index")?;
        let amount_msat = params.amount_msat;
        let htlc_min_msat = if self.channel_manager.list_channels().iter().any(|channel| {
            channel.counterparty.node_id == sender_node_id && channel.trusted_no_broadcast
        }) {
            self.config.channels.virtual_htlc_min_msat
        } else {
            self.config.channels.htlc_min_msat
        };
        if amount_msat < htlc_min_msat {
            return Err(JsonRpcErrorWire::invalid_params(format!(
                "amt_msat cannot be less than {htlc_min_msat}"
            )));
        }
        if matches!(params.asset_amount, Some(0)) {
            return Err(JsonRpcErrorWire::invalid_params("invalid_asset_amount"));
        }
        if params.description_hash.trim().is_empty() {
            return Err(JsonRpcErrorWire::invalid_params("invalid_description_hash"));
        }

        let (contract_id, asset_amount) = match (&params.asset_id, params.asset_amount) {
            (Some(asset_id), Some(asset_amount)) => (
                Some(
                    ContractId::from_str(asset_id)
                        .map_err(|_| JsonRpcErrorWire::invalid_params("invalid_asset_id"))?,
                ),
                Some(asset_amount),
            ),
            (None, None) => (None, None),
            _ => return Err(JsonRpcErrorWire::invalid_params("incomplete_rgb_info")),
        };

        let requested_payment_hash = validate_and_parse_payment_hash(&params.payment_hash)
            .map_err(|_| JsonRpcErrorWire::invalid_params("async_order_invalid_payment_hash"))?;

        let invoice_preimage = if self.external_signer_mode {
            let external_signer = self.external_signer.as_ref().ok_or_else(|| {
                JsonRpcErrorWire::internal_error(
                    "async_order_external_signer_unavailable".to_owned(),
                )
            })?;
            let derived = external_signer
                .prepare_async_payments_hashes(hex_str(&sender_node_id.serialize()), hash_index, 1)
                .map_err(|err| {
                    JsonRpcErrorWire::internal_error(format!(
                        "async_order_signer_derive_failed: {err}"
                    ))
                })?;
            let derived_hash = derived
                .first()
                .and_then(|entry| validate_and_parse_payment_hash(&entry.payment_hash_hex).ok())
                .ok_or_else(|| {
                    JsonRpcErrorWire::internal_error(
                        "async_order_external_signer_derived_hash_error".to_owned(),
                    )
                })?;
            if derived_hash != requested_payment_hash {
                return Err(JsonRpcErrorWire::application_error(
                    ASYNC_ERROR_INVOICE_HASH_MISMATCH,
                    "invoice_hash_mismatch",
                ));
            }
            None
        } else {
            let material = self
                .async_payments_preimage_root
                .derive_hash_material(hash_index)?;
            if material.payment_hash != requested_payment_hash {
                return Err(JsonRpcErrorWire::application_error(
                    ASYNC_ERROR_INVOICE_HASH_MISMATCH,
                    "invoice_hash_mismatch",
                ));
            }
            Some(material.payment_preimage)
        };

        let mut inbound = self.inbound_payments.lock().unwrap();
        if let Some(existing) = inbound.payments.get(&requested_payment_hash) {
            let expired = existing
                .expires_at
                .map(|expires_at| get_current_timestamp() >= expires_at)
                .unwrap_or(false);
            let reusable = matches!(existing.status, HTLCStatus::Failed | HTLCStatus::Cancelled)
                || (matches!(existing.status, HTLCStatus::Pending) && expired);
            if !reusable {
                return Err(Self::stale_flow_error());
            }
        }

        let description_hash = lightning_invoice::Sha256(
            sha256::Hash::from_str(params.description_hash.trim())
                .map_err(|_| JsonRpcErrorWire::invalid_params("invalid_description_hash"))?,
        );

        let invoice_params = Bolt11InvoiceParameters {
            amount_msats: Some(amount_msat),
            description: Bolt11InvoiceDescription::Hash(description_hash),
            invoice_expiry_delta_secs: Some(params.invoice_expiry_sec),
            min_final_cltv_expiry_delta: Some(params.min_final_cltv_expiry_delta),
            payment_hash: Some(requested_payment_hash),
            contract_id,
            asset_amount,
        };
        let invoice = self
            .channel_manager
            .create_bolt11_invoice(invoice_params)
            .map_err(|err| {
                JsonRpcErrorWire::internal_error(format!(
                    "async_order_request_outbound_invoice_failed: {err}"
                ))
            })?;

        let created_at = get_current_timestamp();
        let expires_at = created_at + params.invoice_expiry_sec as u64;
        let result = AsyncOrderOutboundInvoiceResultWire {
            payment_hash: hex_str(&requested_payment_hash.0),
            bolt11: invoice.to_string(),
        };
        persist_staged_inbound_payment(
            self.kv_store.as_ref(),
            self.next_payment_idx.as_ref(),
            &mut inbound,
            requested_payment_hash,
            PaymentInfo {
                preimage: invoice_preimage,
                secret: Some(*invoice.payment_secret()),
                status: HTLCStatus::Pending,
                amt_msat: Some(amount_msat),
                created_at,
                updated_at: created_at,
                payee_pubkey: self.channel_manager.get_our_node_id(),
                expires_at: Some(expires_at),
                claim_deadline_height: None,
                invoice_type: Some(InvoiceType::Hodl {
                    async_payment_recipient: true,
                }),
                description: description_from_invoice(&invoice),
                description_hash: description_hash_from_invoice(&invoice),
                payment_idx: None,
                async_hash_index: self.external_signer_mode.then_some(hash_index),
                async_host_node_id: self.external_signer_mode.then_some(sender_node_id),
            },
        )?;

        Ok(result)
    }
}

fn _safe_update_rgb_channel_amount(
    channel_id: &str,
    rgb_offered_htlc: u64,
    rgb_received_htlc: u64,
    kv_store: &dyn KVStoreSync,
) -> io::Result<bool> {
    match kv_store.read_rgb_channel_info(channel_id, false) {
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            tracing::warn!(
                "Skipping RGB channel balance update for channel {} because channel RGB info is missing",
                channel_id
            );
            return Ok(false);
        }
        Err(e) => return Err(e),
    }
    update_rgb_channel_amount(
        channel_id,
        rgb_offered_htlc,
        rgb_received_htlc,
        false,
        kv_store,
    );
    Ok(true)
}

fn _finalize_rgb_channel_payment(
    payment_hash: &PaymentHash,
    receiver: bool,
    kv_store: &Arc<dyn KVStoreSync + Send + Sync>,
) -> io::Result<()> {
    let payment_hash_str = hex_str(&payment_hash.0);
    let pending_suffix = format!("{payment_hash_str}_pending");
    let mut applied_any = false;

    for inbound in [true, false] {
        let namespace = if inbound {
            RGB_PAYMENT_INFO_INBOUND_NS
        } else {
            RGB_PAYMENT_INFO_OUTBOUND_NS
        };

        let keys = kv_store.list(RGB_PRIMARY_NS, namespace)?;

        let mut applied_keys = Vec::new();

        for key in &keys {
            if !key.ends_with(&pending_suffix) || key.len() <= pending_suffix.len() {
                continue;
            }
            let channel_id_str = &key[..key.len() - pending_suffix.len()];
            if channel_id_str.len() != 64 {
                continue;
            }

            let data = match kv_store.read(RGB_PRIMARY_NS, namespace, key) {
                Ok(data) => data,
                Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                Err(e) => return Err(e),
            };

            let rgb_payment_info: RgbPaymentInfo = match bincode::deserialize(&data) {
                Ok(info) => info,
                Err(e) => {
                    tracing::warn!("failed to parse payment info for key {key}: {e}");
                    continue;
                }
            };

            if rgb_payment_info.swap_payment && receiver != rgb_payment_info.inbound {
                continue;
            }

            let (offered, received) = if receiver {
                (0, rgb_payment_info.amount)
            } else {
                (rgb_payment_info.amount, 0)
            };
            if _safe_update_rgb_channel_amount(
                channel_id_str,
                offered,
                received,
                kv_store.as_ref(),
            )? {
                applied_keys.push(key.clone());
                applied_any = true;
            }
        }

        for key in &applied_keys {
            let _ = kv_store.remove(RGB_PRIMARY_NS, namespace, key, false);
        }
    }

    if applied_any {
        let raw_pending_key = format!("{payment_hash_str}_pending");
        for namespace in [RGB_PAYMENT_INFO_INBOUND_NS, RGB_PAYMENT_INFO_OUTBOUND_NS] {
            let keys = kv_store.list(RGB_PRIMARY_NS, namespace)?;
            let remaining = keys
                .iter()
                .any(|k| k.ends_with(&pending_suffix) && k.len() > pending_suffix.len());
            if !remaining {
                let _ = kv_store.remove(RGB_PRIMARY_NS, namespace, &raw_pending_key, false);
            }
        }
    } else {
        tracing::warn!("no matching payment info found for payment_hash={payment_hash_str}");
    }

    Ok(())
}

fn _finalize_virtual_rgb_channel_info(
    temporary_channel_id: &ChannelId,
    channel_id: &ChannelId,
    kv_store: &dyn KVStoreSync,
) {
    let tmp_id = temporary_channel_id.to_string();
    let final_id = channel_id.to_string();
    for pending in [false, true] {
        match kv_store.read_rgb_channel_info(&tmp_id, pending) {
            Ok(rgb_info) => {
                if kv_store.read_rgb_channel_info(&final_id, pending).is_err() {
                    kv_store.write_rgb_channel_info(&final_id, &rgb_info, pending);
                }
                let _ = kv_store.remove_rgb_channel_info(&tmp_id, pending);
            }
            Err(_) => continue,
        }
    }
}

// rgb-lib sets the PSBT locktime to the chain tip height it just synced to. If LDK's
// channel_manager hasn't yet polled that block, it will reject the funding tx as non-final.
// Detect this and clamp the locktime down to the height LDK already knows about.
// Only safe for BTC channels — RGB channels must preserve the txid because rgb-lib has
// already created transfer state keyed by it.
fn normalize_funding_psbt_locktime(
    unsigned_psbt: String,
    current_best_height: u32,
) -> Result<String, String> {
    let mut psbt = Psbt::from_str(&unsigned_psbt).map_err(|e| e.to_string())?;
    let tx = &mut psbt.unsigned_tx;
    let needs_locktime_adjustment = !tx.input.iter().all(|input| input.sequence == Sequence::MAX)
        && tx.lock_time.is_block_height()
        && tx.lock_time.to_consensus_u32() > current_best_height + 1;
    if needs_locktime_adjustment {
        let old_locktime = tx.lock_time.to_consensus_u32();
        tx.lock_time = LockTime::from_height(current_best_height).unwrap_or(LockTime::ZERO);
        tracing::warn!(
            old_locktime,
            new_locktime = tx.lock_time.to_consensus_u32(),
            current_best_height,
            "adjusted funding PSBT locktime to match LDK best height"
        );
    }
    Ok(psbt.to_string())
}

// Funding checkpoint reached after the RGB stock is promoted (fascia consumed, allocations
// swept into the batch transfer) but before the funding tx is handed to LDK.
#[cfg(debug_assertions)]
pub(crate) const FUNDING_CHECKPOINT_AFTER_COLOR: &str = "after-color-before-handoff";
#[cfg(debug_assertions)]
pub(crate) const FUNDING_CHECKPOINT_HANDOFF_READY: &str = "handoff-ready";
#[cfg(debug_assertions)]
pub(crate) const FUNDING_CHECKPOINT_HANDED_TO_LDK: &str = "handed-to-ldk";
#[cfg(debug_assertions)]
pub(crate) const FUNDING_CHECKPOINT_BROADCAST_SAFE: &str = "broadcast-safe-before-broadcast";
#[cfg(debug_assertions)]
pub(crate) const FUNDING_CHECKPOINT_BROADCASTING: &str = "broadcasting-before-send-end";
#[cfg(debug_assertions)]
pub(crate) const FUNDING_CHECKPOINT_BROADCAST_COMMITTED: &str = "broadcast-committed";
#[cfg(debug_assertions)]
pub(crate) const FUNDING_CHECKPOINT_FINALIZED: &str = "finalized-before-cleanup";
#[cfg(debug_assertions)]
pub(crate) const FUNDING_CHECKPOINT_DURABLY_COMPLETED: &str = "durably-completed-before-ack";

// Test-only crash injection: parks the process at a named funding checkpoint when
// `RLN_FUNDING_KILL_AT` matches, so a test harness can SIGKILL it there. Debug builds only.
#[cfg(debug_assertions)]
fn funding_kill_checkpoint(name: &str) {
    if std::env::var("RLN_FUNDING_KILL_AT").as_deref() == Ok(name) {
        if let Ok(path) = std::env::var("RLN_FUNDING_KILL_READY_PATH") {
            let _ = fs::write(path, name);
        }
        loop {
            std::thread::park();
        }
    }
}

fn rgb_sender_funding_error(context: &str, error: impl std::fmt::Display) -> RgbLibError {
    RgbLibError::Internal {
        details: format!("{context}: {error}"),
    }
}

fn write_rgb_sender_funding_record(
    record: &RgbSenderFundingRecord,
    kv_store: &dyn KVStoreSync,
) -> Result<(), RgbLibError> {
    record.validate()?;
    let bytes = serde_json::to_vec(record).map_err(|error| {
        rgb_sender_funding_error("cannot serialize sender funding journal", error)
    })?;
    kv_store
        .write(
            RGB_SENDER_FUNDING_NAMESPACE,
            "",
            &record.funding_txid,
            bytes,
        )
        .map_err(|error| rgb_sender_funding_error("cannot persist sender funding journal", error))
}

fn read_rgb_sender_funding_record(
    funding_txid: &str,
    kv_store: &dyn KVStoreSync,
) -> Result<RgbSenderFundingRecord, RgbLibError> {
    let bytes = kv_store
        .read(RGB_SENDER_FUNDING_NAMESPACE, "", funding_txid)
        .map_err(|error| rgb_sender_funding_error("cannot read sender funding journal", error))?;
    let record: RgbSenderFundingRecord = serde_json::from_slice(&bytes)
        .map_err(|error| rgb_sender_funding_error("cannot decode sender funding journal", error))?;
    record.validate()?;
    if record.funding_txid != funding_txid {
        return Err(RgbLibError::Internal {
            details: "sender funding journal key does not match its transaction ID".to_owned(),
        });
    }
    Ok(record)
}

fn read_rgb_sender_funding_record_optional(
    funding_txid: &str,
    kv_store: &dyn KVStoreSync,
) -> Result<Option<RgbSenderFundingRecord>, RgbLibError> {
    match kv_store.read(RGB_SENDER_FUNDING_NAMESPACE, "", funding_txid) {
        Ok(bytes) => {
            let record: RgbSenderFundingRecord =
                serde_json::from_slice(&bytes).map_err(|error| {
                    rgb_sender_funding_error("cannot decode sender funding journal", error)
                })?;
            record.validate()?;
            if record.funding_txid != funding_txid {
                return Err(RgbLibError::Internal {
                    details: "sender funding journal key does not match its transaction ID"
                        .to_owned(),
                });
            }
            Ok(Some(record))
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(rgb_sender_funding_error(
            "cannot read sender funding journal",
            error,
        )),
    }
}

fn remove_rgb_sender_funding_record(
    funding_txid: &str,
    kv_store: &dyn KVStoreSync,
) -> Result<(), RgbLibError> {
    kv_store
        .remove(RGB_SENDER_FUNDING_NAMESPACE, "", funding_txid, false)
        .map_err(|error| rgb_sender_funding_error("cannot remove sender funding journal", error))
}

fn remove_rgb_sender_funding_entry(
    namespace: &str,
    key: &str,
    context: &str,
    kv_store: &dyn KVStoreSync,
) -> Result<(), RgbLibError> {
    match kv_store.remove(namespace, "", key, false) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(rgb_sender_funding_error(context, error)),
    }
}

fn sender_transfer_status(
    wallet: &RgbLibWalletWrapper,
    funding_txid: &str,
) -> Result<Option<TransferStatus>, RgbLibError> {
    let transfers = wallet.list_transfers(
        rgb_lib::wallet::AssetFilter::AnyOrNone,
        Some(funding_txid.to_owned()),
    )?;
    let mut statuses = transfers.iter().map(|transfer| transfer.status);
    let Some(status) = statuses.next() else {
        return Ok(None);
    };
    if statuses.any(|candidate| candidate != status) {
        return Err(RgbLibError::Internal {
            details: format!(
                "RGB funding transfer '{funding_txid}' has inconsistent transfer statuses"
            ),
        });
    }
    Ok(Some(status))
}

fn read_rgb_sender_signed_psbt(
    record: &RgbSenderFundingRecord,
    kv_store: &dyn KVStoreSync,
) -> Result<String, RgbLibError> {
    let bytes = kv_store
        .read(PSBT_NAMESPACE, "", &record.funding_txid)
        .map_err(|error| rgb_sender_funding_error("cannot recover signed funding PSBT", error))?;
    let encoded = String::from_utf8(bytes)
        .map_err(|error| rgb_sender_funding_error("signed funding PSBT is not UTF-8", error))?;
    let psbt = Psbt::from_str(&encoded)
        .map_err(|error| rgb_sender_funding_error("signed funding PSBT is invalid", error))?;
    let actual_txid = psbt.unsigned_tx.compute_txid().to_string();
    if actual_txid != record.funding_txid {
        return Err(RgbLibError::Internal {
            details: format!(
                "signed funding PSBT transaction '{}' does not match recovery journal '{}'",
                actual_txid, record.funding_txid
            ),
        });
    }
    psbt.extract_tx().map_err(|error| {
        rgb_sender_funding_error("signed funding PSBT cannot be extracted", error)
    })?;
    Ok(encoded)
}

fn rollback_rgb_sender_funding(
    mut record: RgbSenderFundingRecord,
    wallet: &RgbLibWalletWrapper,
    kv_store: &dyn KVStoreSync,
) -> Result<(), RgbLibError> {
    record.stage = RgbSenderFundingStage::RollingBack;
    write_rgb_sender_funding_record(&record, kv_store)?;

    if let Some((operation_id, _)) = wallet.pending_funding_fascia()? {
        if operation_id != record.funding_txid {
            return Err(RgbLibError::Internal {
                details: format!(
                    "sender funding '{}' cannot roll back RGB operation '{operation_id}'",
                    record.funding_txid
                ),
            });
        }
        wallet.rollback_funding_fascia_if_present(&record.funding_txid)?;
    }

    match sender_transfer_status(wallet, &record.funding_txid)? {
        Some(TransferStatus::Initiated) => {
            if !wallet.fail_transfers(Some(record.batch_transfer_idx), false, true)? {
                return Err(RgbLibError::Internal {
                    details: format!(
                        "RGB funding transfer '{}' remained initiated during rollback",
                        record.funding_txid
                    ),
                });
            }
        }
        Some(TransferStatus::Failed) => {}
        Some(status) => {
            return Err(RgbLibError::Internal {
                details: format!(
                    "refusing to roll back RGB funding '{}' in transfer status {status:?}",
                    record.funding_txid
                ),
            });
        }
        None => {
            return Err(RgbLibError::Internal {
                details: format!(
                    "RGB funding transfer '{}' is missing during rollback",
                    record.funding_txid
                ),
            });
        }
    }

    // A pre-handoff backup may contain the promoted stock and rollback journal. Do not remove the
    // sender recovery record until VSS contains the clean rolled-back wallet state.
    wallet.checked_vss_backup()?;
    for channel_id in
        std::iter::once(&record.temporary_channel_id).chain(record.final_channel_id.iter())
    {
        for pending in [false, true] {
            if let Err(error) = kv_store.remove_rgb_channel_info(channel_id, pending) {
                if error.kind() != io::ErrorKind::NotFound {
                    return Err(rgb_sender_funding_error(
                        "cannot remove abandoned RGB channel metadata",
                        error,
                    ));
                }
            }
        }
    }
    if let Some(final_channel_id) = record.final_channel_id.as_ref() {
        remove_rgb_sender_funding_entry(
            PENDING_FUNDING_NAMESPACE,
            final_channel_id,
            "cannot remove abandoned pending-funding mapping",
            kv_store,
        )?;
    }
    remove_rgb_sender_funding_entry(
        PSBT_NAMESPACE,
        &record.funding_txid,
        "cannot remove abandoned signed funding PSBT",
        kv_store,
    )?;

    record.stage = RgbSenderFundingStage::RetryRequired;
    write_rgb_sender_funding_record(&record, kv_store)?;
    remove_rgb_sender_funding_record(&record.funding_txid, kv_store)
}

fn commit_rgb_sender_broadcast(
    mut record: RgbSenderFundingRecord,
    wallet: &RgbLibWalletWrapper,
    kv_store: &dyn KVStoreSync,
) -> Result<RgbSenderFundingRecord, RgbLibError> {
    if sender_transfer_status(wallet, &record.funding_txid)? == Some(TransferStatus::Initiated) {
        let signed_psbt = read_rgb_sender_signed_psbt(&record, kv_store)?;
        let result = match record.consignment_delivery {
            RgbSenderConsignmentDelivery::Proxy => {
                wallet.send_end_preconsumed_for_operation(&record.funding_txid, signed_psbt)?
            }
            RgbSenderConsignmentDelivery::P2p => {
                wallet.send_end_db_update_only_for_operation(&record.funding_txid, signed_psbt)?
            }
        };
        if result.txid != record.funding_txid {
            return Err(RgbLibError::Internal {
                details: format!(
                    "RGB funding broadcast returned transaction '{}' instead of '{}'",
                    result.txid, record.funding_txid
                ),
            });
        }
    }

    match sender_transfer_status(wallet, &record.funding_txid)? {
        Some(
            TransferStatus::WaitingConfirmations
            | TransferStatus::WaitingSafeHeight
            | TransferStatus::Settled,
        ) => {}
        status => {
            return Err(RgbLibError::Internal {
                details: format!(
                    "cannot commit RGB funding '{}' from transfer status {status:?}",
                    record.funding_txid
                ),
            });
        }
    }

    record.stage = RgbSenderFundingStage::BroadcastCommitted;
    write_rgb_sender_funding_record(&record, kv_store)?;
    #[cfg(debug_assertions)]
    funding_kill_checkpoint(FUNDING_CHECKPOINT_BROADCAST_COMMITTED);
    Ok(record)
}

fn rgb_sender_channel_is_durable(
    record: &RgbSenderFundingRecord,
    channel_manager: &ChannelManager,
) -> bool {
    let Some(final_channel_id) = record.final_channel_id.as_deref() else {
        return false;
    };
    channel_manager
        .list_funded_channels()
        .into_iter()
        .any(|channel| {
            channel.channel_id.to_string() == final_channel_id
                && channel
                    .funding_txo
                    .is_some_and(|outpoint| outpoint.txid.to_string() == record.funding_txid)
        })
}

fn resume_rgb_sender_broadcast(
    mut record: RgbSenderFundingRecord,
    channel_manager: &ChannelManager,
    wallet: &RgbLibWalletWrapper,
    kv_store: &dyn KVStoreSync,
) -> Result<(), RgbLibError> {
    if !rgb_sender_channel_is_durable(&record, channel_manager) {
        return Err(RgbLibError::Internal {
            details: format!(
                "cannot resume RGB funding '{}': matching durable channel state is unavailable",
                record.funding_txid
            ),
        });
    }
    if matches!(
        record.stage,
        RgbSenderFundingStage::Finalized | RgbSenderFundingStage::DurablyCompleted
    ) {
        return Ok(());
    }
    if !matches!(
        record.stage,
        RgbSenderFundingStage::HandoffReady
            | RgbSenderFundingStage::HandedToLdk
            | RgbSenderFundingStage::BroadcastSafeObserved
            | RgbSenderFundingStage::Broadcasting
            | RgbSenderFundingStage::BroadcastCommitted
    ) {
        return Err(RgbLibError::Internal {
            details: format!(
                "cannot resume RGB funding '{}' from stage {:?}",
                record.funding_txid, record.stage
            ),
        });
    }

    // Validate the complete signed transaction before advancing the durable broadcast intent.
    read_rgb_sender_signed_psbt(&record, kv_store)?;
    if matches!(
        record.stage,
        RgbSenderFundingStage::HandoffReady
            | RgbSenderFundingStage::HandedToLdk
            | RgbSenderFundingStage::BroadcastSafeObserved
    ) {
        record.stage = RgbSenderFundingStage::Broadcasting;
        write_rgb_sender_funding_record(&record, kv_store)?;
    }
    commit_and_finalize_rgb_sender_funding(record, wallet, kv_store)
}

fn finalize_rgb_sender_funding(
    mut record: RgbSenderFundingRecord,
    wallet: &RgbLibWalletWrapper,
    kv_store: &dyn KVStoreSync,
) -> Result<(), RgbLibError> {
    match sender_transfer_status(wallet, &record.funding_txid)? {
        Some(
            TransferStatus::WaitingConfirmations
            | TransferStatus::WaitingSafeHeight
            | TransferStatus::Settled,
        ) => {}
        status => {
            return Err(RgbLibError::Internal {
                details: format!(
                    "cannot finalize RGB funding '{}' from transfer status {status:?}",
                    record.funding_txid
                ),
            });
        }
    }

    // Persist the broadcast transfer together with the promoted acceptance journal first. If the
    // process or device disappears during finalization, this snapshot can deterministically replay
    // the exact operation instead of depending on the transport endpoint.
    wallet.checked_vss_backup()?;

    if let Some((operation_id, _)) = wallet.pending_funding_fascia()? {
        if operation_id != record.funding_txid {
            return Err(RgbLibError::Internal {
                details: format!(
                    "sender funding '{}' cannot finalize RGB operation '{operation_id}'",
                    record.funding_txid
                ),
            });
        }
        wallet.finalize_funding_fascia(&record.funding_txid)?;
    }

    // Finalization deletes the stock rollback snapshot. Confirm that the resulting wallet is
    // remotely durable before deleting the signed PSBT or advancing the sender tombstone.
    wallet.checked_vss_backup()?;

    if let Some(final_channel_id) = record.final_channel_id.as_ref() {
        remove_rgb_sender_funding_entry(
            PENDING_FUNDING_NAMESPACE,
            final_channel_id,
            "cannot remove finalized pending-funding mapping",
            kv_store,
        )?;
    }
    remove_rgb_sender_funding_entry(
        PSBT_NAMESPACE,
        &record.funding_txid,
        "cannot remove finalized signed funding PSBT",
        kv_store,
    )?;

    record.stage = RgbSenderFundingStage::Finalized;
    write_rgb_sender_funding_record(&record, kv_store)?;
    #[cfg(debug_assertions)]
    funding_kill_checkpoint(FUNDING_CHECKPOINT_FINALIZED);
    Ok(())
}

fn commit_and_finalize_rgb_sender_funding(
    record: RgbSenderFundingRecord,
    wallet: &RgbLibWalletWrapper,
    kv_store: &dyn KVStoreSync,
) -> Result<(), RgbLibError> {
    if matches!(
        record.stage,
        RgbSenderFundingStage::Finalized | RgbSenderFundingStage::DurablyCompleted
    ) {
        return Ok(());
    }
    // Always reconcile the wallet's transfer status. A device restored from the last pre-handoff
    // RGB backup can have an Initiated transfer while the independently durable sender journal is
    // already BroadcastCommitted; replaying the exact signed PSBT is safe and idempotent.
    let record = commit_rgb_sender_broadcast(record, wallet, kv_store)?;
    finalize_rgb_sender_funding(record, wallet, kv_store)
}

fn remove_rgb_recovery_entry_if_present(
    primary_namespace: &str,
    secondary_namespace: &str,
    key: &str,
    kv_store: &dyn KVStoreSync,
) -> Result<(), RgbLibError> {
    match kv_store.remove(primary_namespace, secondary_namespace, key, false) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(rgb_sender_funding_error(
            "cannot clean finalized RGB receiver recovery artifact",
            error,
        )),
    }
}

fn persist_canonical_rgb_channel_info(
    channel_id: &str,
    expected: &RgbInfo,
    kv_store: &SyncedKvStore,
) -> Result<(), RgbLibError> {
    let total_amount = |info: &RgbInfo| {
        info.local_rgb_amount
            .checked_add(info.remote_rgb_amount)
            .ok_or_else(|| RgbLibError::Internal {
                details: format!("RGB allocation overflows for channel '{channel_id}'"),
            })
    };
    let expected_total = total_amount(expected)?;
    let canonical = match kv_store.read_rgb_channel_info(channel_id, false) {
        Ok(existing) => {
            let same_allocation = existing.contract_id == expected.contract_id
                && existing.schema == expected.schema
                && total_amount(&existing)? == expected_total;
            if !same_allocation {
                return Err(RgbLibError::Internal {
                    details: format!(
                        "refusing to overwrite conflicting RGB metadata for channel '{channel_id}'"
                    ),
                });
            }
            // LDK's canonical record is authoritative for the current local/remote balance split.
            // The sender journal contains the opening allocation and a transient batch index.
            existing
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let mut reconstructed = expected.clone();
            reconstructed.batch_transfer_idx = None;
            reconstructed
        }
        Err(error) => {
            return Err(rgb_sender_funding_error(
                "cannot inspect canonical RGB channel metadata",
                error,
            ));
        }
    };

    let bytes = bincode::serialize(&canonical).map_err(|error| {
        rgb_sender_funding_error("cannot serialize canonical RGB channel metadata", error)
    })?;
    kv_store
        .write_remote_required(RGB_PRIMARY_NS, RGB_CHANNEL_INFO_NS, channel_id, bytes)
        .map_err(|error| {
            rgb_sender_funding_error(
                "canonical RGB channel metadata was not acknowledged by VSS",
                error,
            )
        })
}

fn complete_finalized_sender_funding(
    record: &RgbSenderFundingRecord,
    wallet: &RgbLibWalletWrapper,
    kv_store: &SyncedKvStore,
) -> Result<(), RgbLibError> {
    if record.stage == RgbSenderFundingStage::DurablyCompleted {
        return Ok(());
    }
    if record.stage != RgbSenderFundingStage::Finalized {
        return Err(RgbLibError::Internal {
            details: format!(
                "sender funding '{}' cannot complete from stage {:?}",
                record.funding_txid, record.stage
            ),
        });
    }
    let final_channel_id =
        record
            .final_channel_id
            .as_deref()
            .ok_or_else(|| RgbLibError::Internal {
                details: format!(
                    "finalized sender funding '{}' is missing its channel ID",
                    record.funding_txid
                ),
            })?;

    match sender_transfer_status(wallet, &record.funding_txid)? {
        Some(
            TransferStatus::WaitingConfirmations
            | TransferStatus::WaitingSafeHeight
            | TransferStatus::Settled,
        ) => {}
        status => {
            return Err(RgbLibError::Internal {
                details: format!(
                    "finalized sender funding '{}' has unexpected transfer status {status:?}",
                    record.funding_txid
                ),
            });
        }
    }
    if wallet.pending_funding_fascia()?.is_some() {
        return Err(RgbLibError::Internal {
            details: format!(
                "finalized sender funding '{}' still has a pending stock journal",
                record.funding_txid
            ),
        });
    }

    let rgb_info = match record.rgb_info.as_ref() {
        Some(rgb_info) => rgb_info.clone(),
        None => kv_store
            .read_rgb_channel_info(final_channel_id, false)
            .map_err(|error| {
                rgb_sender_funding_error(
                    "legacy finalized sender funding has no recoverable channel metadata",
                    error,
                )
            })?,
    };

    // The stock backup and canonical metadata must both be remotely durable before the pending
    // marker is removed. Keep the finalized sender journal as an acknowledgement tombstone: after
    // a restart, LDK may still replay FundingTxBroadcastSafe even though startup reconciliation
    // has already finalized the exact transaction. Removing the journal here would make that
    // replay fail forever. ChannelClosed prunes the tombstone after LDK event ordering proves the
    // funding event has been acknowledged.
    wallet.checked_vss_backup()?;
    persist_canonical_rgb_channel_info(final_channel_id, &rgb_info, kv_store)?;
    remove_rgb_sender_funding_entry(
        PENDING_FUNDING_NAMESPACE,
        final_channel_id,
        "cannot remove finalized RGB pending-funding marker",
        kv_store,
    )?;

    let mut completed = record.clone();
    completed.stage = RgbSenderFundingStage::DurablyCompleted;
    write_rgb_sender_funding_record(&completed, kv_store)?;
    #[cfg(debug_assertions)]
    funding_kill_checkpoint(FUNDING_CHECKPOINT_DURABLY_COMPLETED);
    Ok(())
}

fn remove_finalized_sender_tombstone_for_channel(
    channel_id: &ChannelId,
    kv_store: &dyn KVStoreSync,
) {
    let channel_id = channel_id.0.as_hex().to_string();
    let keys = match kv_store.list(RGB_SENDER_FUNDING_NAMESPACE, "") {
        Ok(keys) => keys,
        Err(error) => {
            tracing::warn!(
                channel_id,
                error = %error,
                "cannot list finalized RGB funding tombstones after channel close"
            );
            return;
        }
    };
    for key in keys {
        let record = match read_rgb_sender_funding_record(&key, kv_store) {
            Ok(record) => record,
            Err(error) => {
                tracing::warn!(
                    funding_txid = key,
                    error = %error,
                    "cannot inspect RGB funding tombstone after channel close"
                );
                continue;
            }
        };
        if record.stage != RgbSenderFundingStage::DurablyCompleted
            || record.final_channel_id.as_deref() != Some(channel_id.as_str())
        {
            continue;
        }
        if let Err(error) = remove_rgb_sender_funding_record(&record.funding_txid, kv_store) {
            tracing::warn!(
                funding_txid = %record.funding_txid,
                channel_id,
                error = %error,
                "cannot remove finalized RGB funding tombstone after channel close"
            );
        }
    }
}

fn receiver_final_channel_id(record: &PendingFundingAcceptance) -> Result<String, RgbLibError> {
    let funding_txid = Txid::from_str(&record.funding_txid).map_err(|error| {
        rgb_sender_funding_error("invalid finalized receiver funding transaction ID", error)
    })?;
    Ok(
        ChannelId::v1_from_funding_txid(funding_txid.as_byte_array(), record.funding_output_index)
            .0
            .as_hex()
            .to_string(),
    )
}

fn complete_finalized_receiver_funding(
    record: &PendingFundingAcceptance,
    wallet: &RgbLibWalletWrapper,
    kv_store: &SyncedKvStore,
) -> Result<(), RgbLibError> {
    if record.stage != FundingAcceptanceStage::Finalized {
        return Err(RgbLibError::Internal {
            details: format!(
                "receiver funding '{}' cannot complete from stage {:?}",
                record.funding_txid, record.stage
            ),
        });
    }
    let consignment = record
        .consignment
        .clone()
        .ok_or_else(|| RgbLibError::Internal {
            details: format!(
                "finalized receiver funding '{}' is missing its consignment",
                record.funding_txid
            ),
        })?;
    let rgb_info = record
        .rgb_info
        .as_ref()
        .ok_or_else(|| RgbLibError::Internal {
            details: format!(
                "finalized receiver funding '{}' is missing channel metadata",
                record.funding_txid
            ),
        })?;

    kv_store
        .write(
            FUNDING_CONSIGNMENT_NAMESPACE,
            "",
            &record.funding_txid,
            consignment.clone(),
        )
        .map_err(|error| {
            rgb_sender_funding_error(
                "cannot retain finalized receiver consignment for wallet recovery",
                error,
            )
        })?;

    wallet.ensure_finalized_funding_transfer(
        &record.funding_txid,
        record.funding_output_index as u32,
        consignment,
        rgb_info,
        STATIC_BLINDING,
    )?;
    wallet.checked_vss_backup()?;

    let final_channel_id = receiver_final_channel_id(record)?;
    persist_canonical_rgb_channel_info(&final_channel_id, rgb_info, kv_store)?;

    for (namespace, key) in [
        (RGB_CHANNEL_INFO_NS, record.temporary_channel_id.as_str()),
        (
            RGB_CHANNEL_INFO_PENDING_NS,
            record.temporary_channel_id.as_str(),
        ),
        (RGB_CONSIGNMENT_NS, record.temporary_channel_id.as_str()),
        (RGB_CONSIGNMENT_NS, record.funding_txid.as_str()),
        (RGB_CONSIGNMENT_NS, final_channel_id.as_str()),
    ] {
        remove_rgb_recovery_entry_if_present(RGB_PRIMARY_NS, namespace, key, kv_store)?;
    }
    remove_pending_funding_acceptance(&record.temporary_channel_id, kv_store).map_err(|error| {
        rgb_sender_funding_error("cannot remove finalized RGB receiver journal", error)
    })
}

fn write_receiver_funding_stage(
    record: &PendingFundingAcceptance,
    stage: FundingAcceptanceStage,
    kv_store: &dyn KVStoreSync,
) -> Result<PendingFundingAcceptance, RgbLibError> {
    let mut updated = record.clone();
    updated.stage = stage;
    write_pending_funding_acceptance(&updated, kv_store).map_err(|error| {
        rgb_sender_funding_error("cannot persist RGB receiver funding stage", error)
    })?;
    Ok(updated)
}

fn remove_receiver_funding_journal(
    temporary_channel_id: &str,
    kv_store: &dyn KVStoreSync,
) -> Result<(), RgbLibError> {
    match remove_pending_funding_acceptance(temporary_channel_id, kv_store) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(rgb_sender_funding_error(
            "cannot remove resolved RGB receiver journal",
            error,
        )),
    }
}

fn remove_rolled_back_receiver_artifacts(
    record: &PendingFundingAcceptance,
    kv_store: &dyn KVStoreSync,
) -> Result<(), RgbLibError> {
    let final_channel_id = receiver_final_channel_id(record)?;
    for (namespace, key) in [
        (RGB_CHANNEL_INFO_NS, record.temporary_channel_id.as_str()),
        (
            RGB_CHANNEL_INFO_PENDING_NS,
            record.temporary_channel_id.as_str(),
        ),
        (RGB_CHANNEL_INFO_NS, final_channel_id.as_str()),
        (RGB_CHANNEL_INFO_PENDING_NS, final_channel_id.as_str()),
        (RGB_CONSIGNMENT_NS, record.temporary_channel_id.as_str()),
        (RGB_CONSIGNMENT_NS, record.funding_txid.as_str()),
        (RGB_CONSIGNMENT_NS, final_channel_id.as_str()),
    ] {
        remove_rgb_recovery_entry_if_present(RGB_PRIMARY_NS, namespace, key, kv_store)?;
    }
    Ok(())
}

fn rollback_receiver_funding(
    record: &PendingFundingAcceptance,
    wallet: &RgbLibWalletWrapper,
    kv_store: &SyncedKvStore,
) -> Result<(), RgbLibError> {
    let rolling_back = if record.stage == FundingAcceptanceStage::RollingBack {
        record.clone()
    } else {
        write_receiver_funding_stage(record, FundingAcceptanceStage::RollingBack, kv_store)?
    };

    if let Some((operation_id, _)) = wallet.pending_funding_fascia()? {
        if operation_id != rolling_back.funding_txid {
            return Err(RgbLibError::Internal {
                details: format!(
                    "receiver funding '{}' cannot roll back RGB operation '{operation_id}'",
                    rolling_back.funding_txid
                ),
            });
        }
        wallet.rollback_funding_fascia_if_present(&rolling_back.funding_txid)?;
    }

    // A backup may have captured the staged or promoted stock. Keep the recovery journal until
    // the restored stock is remotely durable, then remove all temporary and derived artifacts.
    wallet.checked_vss_backup()?;
    remove_rolled_back_receiver_artifacts(&rolling_back, kv_store)?;
    let retry_required = write_receiver_funding_stage(
        &rolling_back,
        FundingAcceptanceStage::RetryRequired,
        kv_store,
    )?;
    remove_receiver_funding_journal(&retry_required.temporary_channel_id, kv_store)
}

fn finalize_receiver_funding(
    record: &PendingFundingAcceptance,
    wallet: &RgbLibWalletWrapper,
    kv_store: &SyncedKvStore,
) -> Result<(), RgbLibError> {
    let finalizing = if record.stage == FundingAcceptanceStage::Finalizing {
        record.clone()
    } else {
        write_receiver_funding_stage(record, FundingAcceptanceStage::Finalizing, kv_store)?
    };
    let consignment = finalizing
        .consignment
        .clone()
        .ok_or_else(|| RgbLibError::Internal {
            details: format!(
                "receiver funding '{}' is missing its durable consignment",
                finalizing.funding_txid
            ),
        })?;
    let rgb_info = finalizing
        .rgb_info
        .as_ref()
        .ok_or_else(|| RgbLibError::Internal {
            details: format!(
                "receiver funding '{}' is missing durable channel metadata",
                finalizing.funding_txid
            ),
        })?;

    wallet.ensure_finalized_funding_transfer(
        &finalizing.funding_txid,
        finalizing.funding_output_index as u32,
        consignment,
        rgb_info,
        STATIC_BLINDING,
    )?;
    let finalized =
        write_receiver_funding_stage(&finalizing, FundingAcceptanceStage::Finalized, kv_store)?;
    complete_finalized_receiver_funding(&finalized, wallet, kv_store)
}

fn reconcile_receiver_funding_record(
    record: &PendingFundingAcceptance,
    funded_channel_ids: &BTreeSet<String>,
    wallet: &RgbLibWalletWrapper,
    kv_store: &SyncedKvStore,
) -> Result<Option<RgbFundingRecoveryState>, RgbLibError> {
    let final_channel_id = receiver_final_channel_id(record)?;
    let channel_is_durable = funded_channel_ids.contains(&final_channel_id);

    match rgb_receiver_recovery_action(record.stage, channel_is_durable) {
        RgbReceiverRecoveryAction::Rollback => {
            rollback_receiver_funding(record, wallet, kv_store)?;
            Ok(None)
        }
        RgbReceiverRecoveryAction::Finalize => {
            finalize_receiver_funding(record, wallet, kv_store)?;
            Ok(None)
        }
        RgbReceiverRecoveryAction::Complete => {
            complete_finalized_receiver_funding(record, wallet, kv_store)?;
            Ok(None)
        }
        RgbReceiverRecoveryAction::Quarantine => {
            rgb_receiver_funding_recovery_view(record, channel_is_durable, None).map(Some)
        }
    }
}

fn funded_channel_ids(channel_manager: &ChannelManager) -> BTreeSet<String> {
    channel_manager
        .list_funded_channels()
        .into_iter()
        .map(|channel| channel.channel_id.0.as_hex().to_string())
        .collect()
}

fn reconcile_rgb_receiver_funding(
    channel_manager: &ChannelManager,
    wallet: &RgbLibWalletWrapper,
    kv_store: &SyncedKvStore,
) -> Result<(usize, Vec<RgbFundingRecoveryState>), RgbLibError> {
    let keys = kv_store
        .list(RGB_PRIMARY_NS, RGB_FUNDING_ACCEPTANCE_NS)
        .map_err(|error| {
            rgb_sender_funding_error("cannot list RGB receiver funding journals", error)
        })?;
    let funded_channel_ids = funded_channel_ids(channel_manager);
    let mut completed = 0;
    let mut unresolved = Vec::new();
    for key in keys {
        let record = read_pending_funding_acceptance(&key, kv_store).map_err(|error| {
            rgb_sender_funding_error("cannot read RGB receiver funding journal", error)
        })?;
        match reconcile_receiver_funding_record(&record, &funded_channel_ids, wallet, kv_store) {
            Ok(None) => completed += 1,
            Ok(Some(recovery)) => {
                tracing::warn!(
                    funding_txid = %record.funding_txid,
                    temporary_channel_id = %record.temporary_channel_id,
                    final_channel_id = ?recovery.final_channel_id,
                    stage = ?record.stage,
                    "quarantining RGB receiver funding until matching LDK channel state is durable"
                );
                unresolved.push(recovery);
            }
            Err(error) => {
                let final_channel_id = receiver_final_channel_id(&record)?;
                let channel_is_durable = funded_channel_ids.contains(&final_channel_id);
                tracing::error!(
                    funding_txid = %record.funding_txid,
                    temporary_channel_id = %record.temporary_channel_id,
                    stage = ?record.stage,
                    error = %error,
                    "RGB receiver funding reconciliation failed; preserving recovery evidence"
                );
                unresolved.push(rgb_receiver_funding_recovery_view(
                    &record,
                    channel_is_durable,
                    Some(error.to_string()),
                )?);
            }
        }
    }
    Ok((completed, unresolved))
}

pub(crate) fn reconcile_rgb_sender_funding(
    channel_manager: &ChannelManager,
    wallet: &RgbLibWalletWrapper,
    kv_store: &SyncedKvStore,
) -> Result<Vec<RgbFundingRecoveryState>, RgbLibError> {
    let keys = kv_store
        .list(RGB_SENDER_FUNDING_NAMESPACE, "")
        .map_err(|error| rgb_sender_funding_error("cannot list sender funding journals", error))?;

    let mut unresolved = Vec::new();
    for key in keys {
        let record = read_rgb_sender_funding_record(&key, kv_store)?;
        if record.stage == RgbSenderFundingStage::RetryRequired {
            if let Err(error) = remove_rgb_sender_funding_record(&record.funding_txid, kv_store) {
                tracing::error!(
                    funding_txid = %record.funding_txid,
                    error = %error,
                    "cannot remove completed RGB sender recovery evidence; continuing startup in quarantine"
                );
                unresolved.push(rgb_funding_recovery_view(
                    &record,
                    false,
                    Ok(None),
                    Some(&error),
                ));
            }
            continue;
        }
        let channel_is_durable = rgb_sender_channel_is_durable(&record, channel_manager);
        if record.stage == RgbSenderFundingStage::DurablyCompleted && channel_is_durable {
            continue;
        }
        if record.stage == RgbSenderFundingStage::Finalized && channel_is_durable {
            if let Err(error) = complete_finalized_sender_funding(&record, wallet, kv_store) {
                tracing::error!(
                    funding_txid = %record.funding_txid,
                    error = %error,
                    "retaining finalized RGB sender journal after recovery completion failed"
                );
                unresolved.push(rgb_funding_recovery_view(
                    &record,
                    true,
                    Ok(None),
                    Some(&error),
                ));
            }
            continue;
        }

        let deterministic_action = rgb_sender_recovery_action(&record, channel_is_durable, false);
        let must_check_chain = deterministic_action == RgbSenderRecoveryAction::FailClosed;
        let transaction_observation = if must_check_chain {
            wallet.is_tx_known(record.funding_txid.clone()).map(Some)
        } else {
            Ok(None)
        };
        let transaction_is_known = match transaction_observation.as_ref() {
            Ok(value) => *value,
            Err(error) => {
                tracing::warn!(
                    funding_txid = %record.funding_txid,
                    error = %error,
                    "deferring RGB sender recovery until chain evidence is available"
                );
                unresolved.push(rgb_funding_recovery_view(
                    &record,
                    channel_is_durable,
                    Err(error),
                    None,
                ));
                continue;
            }
        };

        let recovery_action = rgb_sender_recovery_action(
            &record,
            channel_is_durable,
            transaction_is_known.unwrap_or(false),
        );
        let recovery_record = record.clone();
        let recovery_result = match recovery_action {
            RgbSenderRecoveryAction::Finalize => {
                let funding_txid = record.funding_txid.clone();
                commit_and_finalize_rgb_sender_funding(record, wallet, kv_store)
                    .and_then(|()| read_rgb_sender_funding_record(&funding_txid, kv_store))
                    .and_then(|finalized| {
                        complete_finalized_sender_funding(&finalized, wallet, kv_store)
                    })
            }
            RgbSenderRecoveryAction::ResumeBroadcast => {
                let funding_txid = record.funding_txid.clone();
                tracing::info!(
                    funding_txid,
                    stage = ?record.stage,
                    "resuming exact RGB funding transaction from durable LDK state"
                );
                resume_rgb_sender_broadcast(record, channel_manager, wallet, kv_store)
                    .and_then(|()| read_rgb_sender_funding_record(&funding_txid, kv_store))
                    .and_then(|finalized| {
                        complete_finalized_sender_funding(&finalized, wallet, kv_store)
                    })
            }
            RgbSenderRecoveryAction::Rollback => {
                rollback_rgb_sender_funding(record, wallet, kv_store)
            }
            RgbSenderRecoveryAction::FailClosed => {
                let recovery = rgb_funding_recovery_view(
                    &record,
                    channel_is_durable,
                    Ok(transaction_is_known),
                    None,
                );
                tracing::error!(
                    funding_txid = %record.funding_txid,
                    required_action = recovery.action.as_str(),
                    "RGB funding requires explicit recovery; automatic mutation is disabled"
                );
                unresolved.push(recovery);
                Ok(())
            }
        };
        if let Err(error) = recovery_result {
            tracing::error!(
                funding_txid = %recovery_record.funding_txid,
                stage = ?recovery_record.stage,
                action = ?recovery_action,
                error = %error,
                "RGB sender reconciliation failed; preserving recovery evidence and continuing startup"
            );
            unresolved.push(rgb_funding_recovery_view(
                &recovery_record,
                channel_is_durable,
                Ok(transaction_is_known),
                Some(&error),
            ));
        }
    }
    Ok(unresolved)
}

fn should_complete_deferred_rgb_consistency_check(
    was_deferred: bool,
    pending_stock_operation: Option<&str>,
    unresolved: &[RgbFundingRecoveryState],
) -> Result<bool, RgbLibError> {
    match (was_deferred, pending_stock_operation) {
        (false, None) => Ok(false),
        (true, None) => Ok(true),
        (true, Some(operation_id))
            if unresolved
                .iter()
                .any(|recovery| recovery.funding_txid == operation_id) =>
        {
            Ok(false)
        }
        (true, Some(operation_id)) => Err(RgbLibError::Internal {
            details: format!(
                "RGB stock operation '{operation_id}' has no matching durable funding recovery record"
            ),
        }),
        (false, Some(operation_id)) => Err(RgbLibError::Internal {
            details: format!(
                "RGB stock operation '{operation_id}' appeared after the startup consistency check"
            ),
        }),
    }
}

pub(crate) fn list_rgb_funding_recoveries(
    channel_manager: &ChannelManager,
    wallet: &RgbLibWalletWrapper,
    kv_store: &dyn KVStoreSync,
) -> Result<Vec<RgbFundingRecoveryState>, RgbLibError> {
    let keys = kv_store
        .list(RGB_SENDER_FUNDING_NAMESPACE, "")
        .map_err(|error| rgb_sender_funding_error("cannot list sender funding journals", error))?;
    let mut recoveries = Vec::new();
    for key in keys {
        let record = read_rgb_sender_funding_record(&key, kv_store)?;
        let channel_is_durable = rgb_sender_channel_is_durable(&record, channel_manager);
        if !should_report_rgb_sender_recovery(&record, channel_is_durable) {
            continue;
        }

        let deterministic_action = rgb_sender_recovery_action(&record, channel_is_durable, false);
        let transaction_observation = if deterministic_action == RgbSenderRecoveryAction::FailClosed
            && !matches!(
                record.stage,
                RgbSenderFundingStage::Finalized | RgbSenderFundingStage::DurablyCompleted
            ) {
            wallet.is_tx_known(record.funding_txid.clone()).map(Some)
        } else {
            Ok(None)
        };
        recoveries.push(rgb_funding_recovery_view(
            &record,
            channel_is_durable,
            transaction_observation.as_ref().map(|value| *value),
            None,
        ));
    }

    let receiver_keys = kv_store
        .list(RGB_PRIMARY_NS, RGB_FUNDING_ACCEPTANCE_NS)
        .map_err(|error| {
            rgb_sender_funding_error("cannot list receiver funding journals", error)
        })?;
    let funded_channel_ids = funded_channel_ids(channel_manager);
    for key in receiver_keys {
        let record = read_pending_funding_acceptance(&key, kv_store).map_err(|error| {
            rgb_sender_funding_error("cannot read receiver funding journal", error)
        })?;
        let final_channel_id = receiver_final_channel_id(&record)?;
        recoveries.push(rgb_receiver_funding_recovery_view(
            &record,
            funded_channel_ids.contains(&final_channel_id),
            None,
        )?);
    }

    sort_rgb_funding_recoveries(&mut recoveries);
    Ok(recoveries)
}

fn sort_rgb_funding_recoveries(recoveries: &mut [RgbFundingRecoveryState]) {
    recoveries.sort_by(|a, b| {
        a.funding_txid
            .cmp(&b.funding_txid)
            .then_with(|| a.stage.sort_key().cmp(&b.stage.sort_key()))
    });
}

fn should_report_rgb_sender_recovery(
    record: &RgbSenderFundingRecord,
    channel_is_durable: bool,
) -> bool {
    // Durably-completed journals are retained as harmless replay acknowledgements. Every other
    // journal must remain visible until it is deleted, including RetryRequired records whose
    // final deletion failed after rollback.
    record.stage != RgbSenderFundingStage::DurablyCompleted || !channel_is_durable
}

pub(crate) fn resolve_rgb_funding_recovery(
    funding_txid: &str,
    command: RgbFundingRecoveryCommand,
    channel_manager: &ChannelManager,
    wallet: &RgbLibWalletWrapper,
    kv_store: &SyncedKvStore,
) -> Result<RgbFundingRecoveryResolution, RgbLibError> {
    let mut recoveries = match command {
        RgbFundingRecoveryCommand::Recheck => {
            let (_, mut receiver_recoveries) =
                reconcile_rgb_receiver_funding(channel_manager, wallet, kv_store)?;
            receiver_recoveries.extend(reconcile_rgb_sender_funding(
                channel_manager,
                wallet,
                kv_store,
            )?);
            receiver_recoveries
        }
        RgbFundingRecoveryCommand::ResumeBroadcast => {
            let receiver_exists = kv_store
                .list(RGB_PRIMARY_NS, RGB_FUNDING_ACCEPTANCE_NS)
                .map_err(|error| {
                    rgb_sender_funding_error("cannot list receiver funding journals", error)
                })?
                .into_iter()
                .map(|key| read_pending_funding_acceptance(&key, kv_store))
                .collect::<Result<Vec<_>, _>>()
                .map_err(|error| {
                    rgb_sender_funding_error("cannot read receiver funding journal", error)
                })?
                .into_iter()
                .any(|record| record.funding_txid == funding_txid);
            if receiver_exists {
                return Err(RgbLibError::Internal {
                    details: format!(
                        "receiver funding '{funding_txid}' cannot be broadcast by this node"
                    ),
                });
            }

            let Some(record) = read_rgb_sender_funding_record_optional(funding_txid, kv_store)?
            else {
                let recoveries = list_rgb_funding_recoveries(channel_manager, wallet, kv_store)?;
                return Ok(RgbFundingRecoveryResolution {
                    recovery: None,
                    recoveries,
                });
            };
            let channel_is_durable = rgb_sender_channel_is_durable(&record, channel_manager);
            if rgb_sender_recovery_action(&record, channel_is_durable, false)
                != RgbSenderRecoveryAction::ResumeBroadcast
            {
                return Err(RgbLibError::Internal {
                    details: format!(
                        "sender funding '{funding_txid}' is not eligible for broadcast resumption"
                    ),
                });
            }
            resume_rgb_sender_broadcast(record, channel_manager, wallet, kv_store)?;
            let finalized = read_rgb_sender_funding_record(funding_txid, kv_store)?;
            complete_finalized_sender_funding(&finalized, wallet, kv_store)?;
            list_rgb_funding_recoveries(channel_manager, wallet, kv_store)?
        }
    };

    sort_rgb_funding_recoveries(&mut recoveries);
    let recovery = recoveries
        .iter()
        .find(|recovery| recovery.funding_txid == funding_txid)
        .cloned();
    Ok(RgbFundingRecoveryResolution {
        recovery,
        recoveries,
    })
}

// Handle an rgb-lib error that happened while preparing a channel funding transaction in
// FundingGenerationReady. Returns the value to propagate from the event handler: `Err(ReplayEvent)`
// to retry the event (for transient network errors), or `Ok(())` after force-closing the channel
// (for terminal errors).
fn handle_funding_prepare_err(
    e: RgbLibError,
    channel_manager: &ChannelManager,
    temporary_channel_id: &ChannelId,
    counterparty_node_id: &PublicKey,
) -> Result<(), ReplayEvent> {
    match e {
        RgbLibError::Indexer { details }
        | RgbLibError::InvalidIndexer { details }
        | RgbLibError::Network { details } => {
            tracing::error!("Network error during channel opening: {details}");
            Err(ReplayEvent())
        }
        e => abort_funding(
            e.to_string(),
            channel_manager,
            temporary_channel_id,
            counterparty_node_id,
        ),
    }
}

// Give up on a channel funding for a reason retrying cannot fix, closing the channel rather than
// leaving the peer waiting on a funding that will never come.
fn abort_funding(
    reason: String,
    channel_manager: &ChannelManager,
    temporary_channel_id: &ChannelId,
    counterparty_node_id: &PublicKey,
) -> Result<(), ReplayEvent> {
    tracing::error!("Cannot open channel: {reason}");
    if let Err(close_err) = channel_manager.force_close_broadcasting_latest_txn(
        temporary_channel_id,
        counterparty_node_id,
        reason,
    ) {
        tracing::error!(
            "Failed to abort funding by force-closing the channel {temporary_channel_id} after error: {close_err:?}"
        );
    }
    Ok(())
}

/// Release the funds locked for a channel open that failed before the funding
/// transaction was broadcast. For colored channels this fails the pending RGB
/// batch transfer; for vanilla channels it aborts the pending vanilla tx that
/// was created (and locked the UTXOs) during `FundingGenerationReady`.
async fn handle_open_chan_fail(channel_id: &ChannelId, unlocked_state: Arc<UnlockedAppState>) {
    let channel_id_hex = channel_id.0.as_hex().to_string();
    if let Some(mut rgb_info) =
        get_rgb_channel_info_optional(channel_id, true, unlocked_state.kv_store.as_ref())
    {
        let _rgb_funding_operation = unlocked_state
            .rgb_funding_recovery_guard
            .lock_operation()
            .await;
        let funding_txid_bytes =
            match unlocked_state
                .kv_store
                .read(PENDING_FUNDING_NAMESPACE, "", &channel_id_hex)
            {
                Ok(bytes) => bytes,
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    tracing::debug!(
                        channel_id = %channel_id,
                        "channel has no pending funding marker; skipping pre-broadcast RGB cleanup"
                    );
                    return;
                }
                Err(error) => {
                    tracing::error!(
                        channel_id = %channel_id,
                        error = %error,
                        "cannot determine whether RGB channel funding is still pending"
                    );
                    return;
                }
            };
        let funding_txid = match String::from_utf8(funding_txid_bytes) {
            Ok(txid) => txid,
            Err(error) => {
                tracing::error!(
                    channel_id = %channel_id,
                    error = %error,
                    "pending RGB funding marker is not valid UTF-8"
                );
                return;
            }
        };
        match read_rgb_sender_funding_record_optional(
            &funding_txid,
            unlocked_state.kv_store.as_ref(),
        ) {
            Ok(Some(record)) => {
                let unlocked_state_copy = unlocked_state.clone();
                let resolved = match tokio::task::spawn_blocking(move || {
                        match record.stage {
                            RgbSenderFundingStage::Broadcasting
                            | RgbSenderFundingStage::BroadcastCommitted
                            | RgbSenderFundingStage::Finalized
                            | RgbSenderFundingStage::DurablyCompleted => {
                                commit_and_finalize_rgb_sender_funding(
                                    record,
                                    unlocked_state_copy.rgb_wallet_wrapper.as_ref(),
                                    unlocked_state_copy.kv_store.as_ref(),
                                )
                            }
                            RgbSenderFundingStage::Preparing
                            | RgbSenderFundingStage::StockPromoted => rollback_rgb_sender_funding(
                                record,
                                unlocked_state_copy.rgb_wallet_wrapper.as_ref(),
                                unlocked_state_copy.kv_store.as_ref(),
                            ),
                            RgbSenderFundingStage::HandoffReady
                            | RgbSenderFundingStage::HandedToLdk
                            | RgbSenderFundingStage::BroadcastSafeObserved
                                if record.manual_broadcast =>
                            {
                                rollback_rgb_sender_funding(
                                    record,
                                    unlocked_state_copy.rgb_wallet_wrapper.as_ref(),
                                    unlocked_state_copy.kv_store.as_ref(),
                                )
                            }
                            _ => Err(RgbLibError::Internal {
                                details: "legacy RGB funding crossed the automatic-broadcast handoff; retaining its recovery journal"
                                    .to_owned(),
                            }),
                        }
                    })
                    .await
                {
                    Ok(resolved) => resolved,
                    Err(error) => {
                        tracing::error!(
                            channel_id = %channel_id,
                            error = %error,
                            "RGB channel cleanup worker failed; retaining recovery state"
                        );
                        return;
                    }
                };
                if let Err(error) = resolved {
                    tracing::error!(
                            "Refusing to release RGB transfer state for channel {channel_id}: {error:?}"
                        );
                    return;
                }
                let _ = unlocked_state.kv_store.remove(
                    PENDING_FUNDING_NAMESPACE,
                    "",
                    &channel_id_hex,
                    false,
                );
                return;
            }
            Ok(None) => {}
            Err(error) => {
                tracing::error!(
                    "Cannot inspect RGB sender funding journal for channel {channel_id}: {error:?}"
                );
                return;
            }
        }
        let unlocked_state_copy = unlocked_state.clone();
        let rollback_funding_txid = funding_txid.clone();
        let rollback = match tokio::task::spawn_blocking(move || {
            unlocked_state_copy.rgb_rollback_funding_fascia_if_present(&rollback_funding_txid)
        })
        .await
        {
            Ok(rollback) => rollback,
            Err(error) => {
                tracing::error!(
                    channel_id = %channel_id,
                    error = %error,
                    "RGB stock rollback worker failed; retaining recovery state"
                );
                return;
            }
        };
        if let Err(error) = rollback {
            tracing::error!(
                "Refusing to release RGB transfer state for channel {channel_id} after stock rollback failed: {error:?}"
            );
            return;
        }
        if let Some(batch_transfer_idx) = rgb_info.batch_transfer_idx {
            let unlocked_state_copy = unlocked_state.clone();
            let failed = match tokio::task::spawn_blocking(move || {
                unlocked_state_copy.rgb_fail_transfers(Some(batch_transfer_idx), false, true)
            })
            .await
            {
                Ok(failed) => failed,
                Err(error) => {
                    tracing::error!(
                        channel_id = %channel_id,
                        batch_transfer_idx,
                        error = %error,
                        "RGB transfer cleanup worker failed; retaining recovery state"
                    );
                    return;
                }
            };
            match failed {
                Ok(_) => {
                    rgb_info.batch_transfer_idx = None;
                    unlocked_state.kv_store.write_rgb_channel_info(
                        &channel_id_hex,
                        &rgb_info,
                        true,
                    );
                }
                Err(e) => {
                    tracing::error!(
                        "Error failing RGB transfer batch_transfer_idx={batch_transfer_idx} for channel {channel_id}: {e:?}"
                    );
                    return;
                }
            }
        }
    } else if let Ok(funding_txid_bytes) =
        unlocked_state
            .kv_store
            .read(PENDING_FUNDING_NAMESPACE, "", &channel_id_hex)
    {
        let funding_txid = match String::from_utf8(funding_txid_bytes) {
            Ok(funding_txid) => funding_txid,
            Err(error) => {
                tracing::error!(
                    channel_id = %channel_id,
                    error = %error,
                    "pending vanilla funding marker is not valid UTF-8"
                );
                return;
            }
        };
        let unlocked_state_copy = unlocked_state.clone();
        let txid_copy = funding_txid.clone();
        let result = match tokio::task::spawn_blocking(move || {
            unlocked_state_copy.rgb_abort_pending_vanilla_tx(txid_copy)
        })
        .await
        {
            Ok(result) => result,
            Err(error) => {
                tracing::error!(
                    channel_id = %channel_id,
                    error = %error,
                    "vanilla funding cleanup worker failed; retaining recovery state"
                );
                return;
            }
        };
        match result {
            Ok(()) => {
                tracing::info!("Aborted pending vanilla tx {funding_txid} for channel {channel_id}")
            }
            Err(e) => {
                tracing::error!(
                    "Error aborting pending vanilla tx {funding_txid} for channel {channel_id}: {e:?}"
                );
                return;
            }
        }
    }
    let _ = unlocked_state
        .kv_store
        .remove(PENDING_FUNDING_NAMESPACE, "", &channel_id_hex, false);
}

/// Undo what a standard channel's `FundingGenerationReady` preparation staged, so a failure
/// between preparation and a fully-signed funding tx (e.g. a remote external signer briefly
/// unreachable, or returning a malformed reply) can replay the event without leaking locked funds:
/// for a colored channel fail the pending RGB batch transfer (releasing the reserved allocation),
/// for a vanilla channel abort the pending vanilla tx (releasing the locked UTXOs). This runs
/// *before* the `PENDING_FUNDING_NAMESPACE` mapping exists, so `handle_open_chan_fail` could not do
/// the vanilla cleanup later — the staged tx would be unabortable. Cleanup failure is terminal:
/// replay is unsafe until the staged stock and transfer reservation are both released.
async fn abort_staged_standard_funding(
    unlocked_state: Arc<UnlockedAppState>,
    temporary_channel_id: &ChannelId,
    unsigned_psbt: &str,
    is_colored: bool,
) -> Result<(), RgbLibError> {
    if is_colored {
        let psbt = RgbLibPsbt::from_str(unsigned_psbt).map_err(|error| RgbLibError::Internal {
            details: format!("cannot parse staged RGB funding PSBT: {error}"),
        })?;
        let funding_txid = psbt.unsigned_tx.compute_txid().to_string();
        if let Some(record) = read_rgb_sender_funding_record_optional(
            &funding_txid,
            unlocked_state.kv_store.as_ref(),
        )? {
            let unlocked_state_copy = unlocked_state.clone();
            return tokio::task::spawn_blocking(move || {
                rollback_rgb_sender_funding(
                    record,
                    unlocked_state_copy.rgb_wallet_wrapper.as_ref(),
                    unlocked_state_copy.kv_store.as_ref(),
                )
            })
            .await
            .map_err(|error| {
                rgb_sender_funding_error("RGB sender rollback worker failed", error)
            })?;
        }

        // Legacy fallback for an in-flight open created before sender journals were introduced.
        let unlocked_state_copy = unlocked_state.clone();
        tokio::task::spawn_blocking(move || {
            unlocked_state_copy.rgb_rollback_funding_fascia_if_present(&funding_txid)
        })
        .await
        .map_err(|error| rgb_sender_funding_error("RGB stock rollback worker failed", error))??;

        if let Some(mut rgb_info) = get_rgb_channel_info_optional(
            temporary_channel_id,
            true,
            unlocked_state.kv_store.as_ref(),
        ) {
            if let Some(batch_transfer_idx) = rgb_info.batch_transfer_idx {
                let unlocked_state_copy = unlocked_state.clone();
                tokio::task::spawn_blocking(move || {
                    unlocked_state_copy.rgb_fail_transfers(Some(batch_transfer_idx), false, true)
                })
                .await
                .map_err(|error| {
                    rgb_sender_funding_error("RGB transfer cleanup worker failed", error)
                })??;
                // Clear the recorded idx: the transfer is already failed, and the replayed event
                // will stage a fresh transfer and record its own idx.
                rgb_info.batch_transfer_idx = None;
                unlocked_state.kv_store.write_rgb_channel_info(
                    &temporary_channel_id.0.as_hex().to_string(),
                    &rgb_info,
                    true,
                );
            }
        }
    } else {
        // The txid of the pending vanilla tx: witness data is excluded from the txid, so the
        // unsigned PSBT's tx computes the same txid rgb-lib recorded for the pending tx.
        match Psbt::from_str(unsigned_psbt) {
            Ok(psbt) => {
                let txid = psbt.unsigned_tx.compute_txid().to_string();
                let unlocked_state_copy = unlocked_state.clone();
                let txid_copy = txid.clone();
                let result = tokio::task::spawn_blocking(move || {
                    unlocked_state_copy.rgb_abort_pending_vanilla_tx(txid_copy)
                })
                .await
                .map_err(|error| {
                    rgb_sender_funding_error("vanilla funding cleanup worker failed", error)
                })?;
                match result {
                    Ok(()) => tracing::info!(
                        "Aborted staged vanilla funding tx {txid} for channel {temporary_channel_id}"
                    ),
                    Err(e) => return Err(e),
                }
            }
            Err(e) => {
                return Err(RgbLibError::Internal {
                    details: format!(
                        "cannot parse staged funding PSBT for channel {temporary_channel_id}: {e}"
                    ),
                });
            }
        }
    }
    Ok(())
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
            let is_colored =
                is_channel_rgb(&temporary_channel_id, unlocked_state.kv_store.as_ref());
            let _rgb_funding_operation = if is_colored {
                Some(
                    unlocked_state
                        .rgb_funding_recovery_guard
                        .lock_operation()
                        .await,
                )
            } else {
                None
            };

            let addr = WitnessProgram::from_scriptpubkey(
                output_script.as_bytes(),
                match static_state.network {
                    BitcoinNetwork::Mainnet => bitcoin_bech32::constants::Network::Bitcoin,
                    BitcoinNetwork::Testnet | BitcoinNetwork::Testnet4 => {
                        bitcoin_bech32::constants::Network::Testnet
                    }
                    BitcoinNetwork::Regtest => bitcoin_bech32::constants::Network::Regtest,
                    BitcoinNetwork::Signet | BitcoinNetwork::SignetCustom => {
                        bitcoin_bech32::constants::Network::Signet
                    }
                },
            )
            .expect("Lightning funding tx should always be to a SegWit output");
            let script_buf = ScriptBuf::from_bytes(addr.to_scriptpubkey());

            if let Some(virtual_draft) =
                unlocked_state.virtual_channel_draft_get(&temporary_channel_id)
            {
                let reject_virtual_open = |reason: String| {
                    tracing::error!(
                        "rejecting virtual channel {} with {}: {}",
                        temporary_channel_id,
                        hex_str(&counterparty_node_id.serialize()),
                        reason,
                    );
                    let _ = unlocked_state.kv_store.remove(
                        "",
                        "",
                        &format!("virtual_channel_{}", temporary_channel_id),
                        false,
                    );
                    unlocked_state.virtual_channel_draft_delete(&temporary_channel_id);
                };

                let mut virtual_funding_txo = virtual_channel_synthetic_outpoint(
                    static_state.network,
                    &unlocked_state.channel_manager.get_our_node_id(),
                    &counterparty_node_id,
                );
                let duplicate_synthetic_funding_txo = {
                    let session_store = unlocked_state.get_virtual_channel_session_store();
                    session_store.contains_virtual_funding_txo(&virtual_funding_txo)
                };
                if duplicate_synthetic_funding_txo {
                    reject_virtual_open(format!(
                        "duplicate synthetic funding outpoint {} already exists in session store",
                        virtual_funding_txo,
                    ));
                    return Ok(());
                }
                let mut channel_id = ChannelId::v1_from_funding_outpoint(virtual_funding_txo);

                if is_colored {
                    let rgb_info = get_rgb_channel_info_pending(
                        &temporary_channel_id,
                        unlocked_state.kv_store.as_ref(),
                    );
                    let channel_rgb_amount = rgb_info.local_rgb_amount + rgb_info.remote_rgb_amount;
                    let asset_id = rgb_info.contract_id.to_string();
                    let assignment = match rgb_info.schema {
                        AssetSchema::Nia | AssetSchema::Cfa | AssetSchema::Ifa => {
                            Assignment::Fungible(channel_rgb_amount)
                        }
                        AssetSchema::Uda => Assignment::NonFungible,
                    };
                    let recipient_id =
                        recipient_id_from_script_buf(script_buf, static_state.network);
                    let recipient_map = map! {
                        asset_id.clone() => vec![Recipient {
                            recipient_id,
                            witness_data: Some(WitnessData {
                                amount_sat: channel_value_satoshis,
                                blinding: Some(STATIC_BLINDING),
                            }),
                            assignment,
                            transport_endpoints: vec![unlocked_state.proxy_endpoint.clone()]
                    }]};
                    let fee_rate_sat_vb = unlocked_state.config.rgb.fee_rate_sat_vb;
                    let unlocked_state_copy = unlocked_state.clone();
                    let res = tokio::task::spawn_blocking(
                        move || -> Result<(String, Option<i32>), String> {
                            let res = unlocked_state_copy
                                .rgb_send_begin(
                                    recipient_map,
                                    true,
                                    fee_rate_sat_vb,
                                    0,
                                    get_current_timestamp() + RGB_TRANSFER_CHAN_EXPIRATION_SECS,
                                    false,
                                    Some(0),
                                )
                                .map_err(|e| e.to_string())?;
                            let fascia_str = fs::read_to_string(&res.details.fascia_path)
                                .map_err(|e| e.to_string())?;
                            let fascia: Fascia =
                                serde_json::from_str(&fascia_str).map_err(|e| e.to_string())?;
                            unlocked_state_copy
                                .rgb_consume_fascia(fascia, None)
                                .map_err(|e| e.to_string())?;
                            unlocked_state_copy
                                .rgb_create_consignments(res.psbt.clone())
                                .map_err(|e| e.to_string())?;
                            Ok((res.psbt, res.batch_transfer_idx))
                        },
                    )
                    .await
                    .unwrap();

                    let (unsigned_psbt, batch_transfer_idx) = match res {
                        Ok(result) => result,
                        Err(e) => {
                            tracing::error!("cannot prepare virtual funding transfer: {e}");
                            return Err(ReplayEvent());
                        }
                    };

                    // Record the batch transfer index so a failed open can fail the pending
                    // transfer and release the locked assets (see handle_open_chan_fail).
                    if let Some(mut rgb_info) = get_rgb_channel_info_optional(
                        &temporary_channel_id,
                        true,
                        unlocked_state.kv_store.as_ref(),
                    ) {
                        rgb_info.batch_transfer_idx = batch_transfer_idx;
                        unlocked_state.kv_store.write_rgb_channel_info(
                            &temporary_channel_id.0.as_hex().to_string(),
                            &rgb_info,
                            true,
                        );
                    }

                    let signed_psbt = match unlocked_state.rgb_sign_psbt(unsigned_psbt) {
                        Ok(psbt) => psbt,
                        Err(e) => {
                            tracing::error!("cannot sign virtual funding transfer PSBT: {e}");
                            return Err(ReplayEvent());
                        }
                    };
                    let psbt = match Psbt::from_str(&signed_psbt) {
                        Ok(psbt) => psbt,
                        Err(e) => {
                            tracing::error!(
                                "cannot parse signed virtual funding transfer PSBT: {e}"
                            );
                            return Err(ReplayEvent());
                        }
                    };
                    let funding_psbt = match psbt.extract_tx() {
                        Ok(tx) => tx,
                        Err(e) => {
                            tracing::error!("cannot extract virtual funding transaction: {e}");
                            return Err(ReplayEvent());
                        }
                    };
                    let Some(virtual_funding_vout) = funding_psbt
                        .output
                        .iter()
                        .position(|txout| {
                            txout.script_pubkey.as_bytes() == output_script.as_bytes()
                        })
                        .map(|vout| vout as u16)
                    else {
                        tracing::error!(
                            "cannot find virtual funding output in extracted transaction"
                        );
                        return Err(ReplayEvent());
                    };
                    virtual_funding_txo = OutPoint {
                        txid: funding_psbt.compute_txid(),
                        index: virtual_funding_vout,
                    };
                    channel_id = ChannelId::v1_from_funding_outpoint(virtual_funding_txo);

                    let duplicate_virtual_funding_txo = {
                        let session_store = unlocked_state.get_virtual_channel_session_store();
                        session_store.contains_virtual_funding_txo(&virtual_funding_txo)
                    };
                    if duplicate_virtual_funding_txo {
                        reject_virtual_open(format!(
                            "duplicate virtual funding outpoint {} already exists in session store",
                            virtual_funding_txo,
                        ));
                        return Ok(());
                    }

                    let witness_id = virtual_funding_txo.txid.to_string();

                    let witness_id_clone = witness_id.clone();
                    let unlocked_state_copy = unlocked_state.clone();
                    let res = tokio::task::spawn_blocking(move || {
                        unlocked_state_copy.rgb_upsert_witness(
                            RgbTxid::from_str(&witness_id_clone).unwrap(),
                            WitnessOrd::Tentative,
                        )
                    })
                    .await
                    .unwrap();

                    if let Err(e) = res {
                        tracing::error!("cannot register virtual funding witness: {e}");
                        return Err(ReplayEvent());
                    }

                    let consignment_path =
                        unlocked_state.rgb_get_send_consignment_path(&asset_id, &witness_id);
                    let consignment_bytes = match fs::read(&consignment_path) {
                        Ok(bytes) => bytes,
                        Err(e) => {
                            return abort_funding(
                                format!("cannot read funding consignment: {e}"),
                                &unlocked_state.channel_manager,
                                &temporary_channel_id,
                                &counterparty_node_id,
                            );
                        }
                    };
                    if unlocked_state
                        .rgb_file_transfer_handler
                        .queue_consignment(
                            counterparty_node_id,
                            witness_id.clone(),
                            consignment_bytes,
                        )
                        .is_err()
                    {
                        let _ = fs::remove_file(&consignment_path);
                        return abort_funding(
                            s!("consignment is too large to send over p2p"),
                            &unlocked_state.channel_manager,
                            &temporary_channel_id,
                            &counterparty_node_id,
                        );
                    }

                    // send the asset's media files over the same p2p link
                    if rgb_info.counterparty_knows_asset {
                        tracing::info!(
                            "counterparty already knows asset {asset_id}, not sending its media"
                        );
                    } else {
                        let unlocked_state_copy = unlocked_state.clone();
                        let medias = match tokio::task::spawn_blocking(move || {
                            unlocked_state_copy.rgb_list_asset_media(asset_id)
                        })
                        .await
                        .unwrap()
                        {
                            Ok(medias) => medias,
                            Err(e) => {
                                let _ = fs::remove_file(&consignment_path);
                                return handle_funding_prepare_err(
                                    e,
                                    &unlocked_state.channel_manager,
                                    &temporary_channel_id,
                                    &counterparty_node_id,
                                );
                            }
                        };
                        for media in medias {
                            let media_bytes = match fs::read(&media.file_path) {
                                Ok(bytes) => bytes,
                                Err(e) => {
                                    let _ = fs::remove_file(&consignment_path);
                                    return abort_funding(
                                        format!("cannot read asset media file: {e}"),
                                        &unlocked_state.channel_manager,
                                        &temporary_channel_id,
                                        &counterparty_node_id,
                                    );
                                }
                            };
                            if unlocked_state
                                .rgb_file_transfer_handler
                                .queue_media(
                                    counterparty_node_id,
                                    witness_id.clone(),
                                    media.digest,
                                    media_bytes,
                                )
                                .is_err()
                            {
                                let _ = fs::remove_file(&consignment_path);
                                return abort_funding(
                                    s!("asset media is too large to send over p2p"),
                                    &unlocked_state.channel_manager,
                                    &temporary_channel_id,
                                    &counterparty_node_id,
                                );
                            }
                        }
                    }

                    unlocked_state.peer_manager.process_events();
                    let _ = fs::remove_file(&consignment_path);
                }

                match unlocked_state
                    .channel_manager
                    .unsafe_manual_funding_transaction_generated(
                        temporary_channel_id,
                        counterparty_node_id,
                        virtual_funding_txo,
                        ChannelFundingType::Virtual,
                    ) {
                    Ok(()) => {
                        _finalize_virtual_rgb_channel_info(
                            &temporary_channel_id,
                            &channel_id,
                            unlocked_state.kv_store.as_ref(),
                        );
                        unlocked_state
                            .kv_store
                            .write("", "", &format!("virtual_channel_{}", channel_id), vec![])
                            .expect("able to persist virtual channel marker");
                        unlocked_state.virtual_channel_session_add(VirtualChannelSession {
                            channel_id,
                            created_at: virtual_draft.created_at,
                            former_temporary_channel_id: temporary_channel_id,
                            peer_id: virtual_draft.peer_id,
                            status: VirtualChannelSessionStatus::Active,
                            virtual_funding_txo,
                            updated_at: get_current_timestamp(),
                        });
                        unlocked_state.virtual_channel_draft_delete(&temporary_channel_id);
                        tracing::info!(
                            "EVENT: registered trusted no-broadcast funding {} for virtual channel {}",
                            virtual_funding_txo,
                            channel_id,
                        );
                    }
                    Err(e) => {
                        tracing::error!(
                            "ERROR: Failed trusted no-broadcast funding registration for {}: {:?}",
                            temporary_channel_id,
                            e,
                        );
                        let _ = unlocked_state.kv_store.remove(
                            "",
                            "",
                            &format!("virtual_channel_{}", temporary_channel_id),
                            false,
                        );
                        unlocked_state.virtual_channel_draft_delete(&temporary_channel_id);
                    }
                }
                return Ok(());
            }

            let (unsigned_psbt, asset_id, mut sender_record) = if is_colored {
                let rgb_info = get_rgb_channel_info_pending(
                    &temporary_channel_id,
                    unlocked_state.kv_store.as_ref(),
                );

                let channel_rgb_amount = rgb_info.local_rgb_amount + rgb_info.remote_rgb_amount;
                let asset_id = rgb_info.contract_id.to_string();
                let assignment = match rgb_info.schema {
                    AssetSchema::Nia | AssetSchema::Cfa | AssetSchema::Ifa => {
                        Assignment::Fungible(channel_rgb_amount)
                    }
                    AssetSchema::Uda => Assignment::NonFungible,
                };

                let recipient_id =
                    recipient_id_from_script_buf(script_buf.clone(), static_state.network);

                let recipient_map = map! {
                    asset_id.clone() => vec![Recipient {
                        recipient_id: recipient_id.clone(),
                        witness_data: Some(WitnessData {
                            amount_sat: channel_value_satoshis,
                            blinding: Some(STATIC_BLINDING),
                        }),
                        assignment,
                        transport_endpoints: vec![]
                }]};

                let fee_rate_sat_vb = unlocked_state.config.rgb.fee_rate_sat_vb;
                let min_channel_confirmations = unlocked_state.config.rgb.min_channel_confirmations;
                let unlocked_state_copy = unlocked_state.clone();
                let temporary_channel_id_hex = temporary_channel_id.0.as_hex().to_string();
                let res = match tokio::task::spawn_blocking(
                    move || -> Result<(String, Option<i32>, RgbSenderFundingRecord), RgbLibError> {
                        let res = unlocked_state_copy.rgb_send_begin(
                            recipient_map,
                            true,
                            fee_rate_sat_vb,
                            min_channel_confirmations,
                            get_current_timestamp() + RGB_TRANSFER_CHAN_EXPIRATION_SECS,
                            false,
                            // Final locktime: this colored tx funds an LN channel.
                            Some(0),
                        )?;
                        let fascia_str = fs::read_to_string(&res.details.fascia_path)?;
                        let fascia: Fascia =
                            serde_json::from_str(&fascia_str).map_err(|error| {
                                RgbLibError::Internal {
                                    details: format!("invalid funding fascia: {error}"),
                                }
                            })?;
                        unlocked_state_copy.rgb_create_consignments(res.psbt.clone())?;
                        let funding_psbt = RgbLibPsbt::from_str(&res.psbt).map_err(|error| {
                            RgbLibError::Internal {
                                details: format!("invalid funding PSBT: {error}"),
                            }
                        })?;
                        let funding_txid = funding_psbt.unsigned_tx.compute_txid().to_string();
                        let batch_transfer_idx = res.batch_transfer_idx.ok_or_else(|| {
                            RgbLibError::Internal {
                                details: "RGB funding transfer has no batch transfer ID".to_owned(),
                            }
                        })?;
                        let mut journal_rgb_info = rgb_info.clone();
                        journal_rgb_info.batch_transfer_idx = Some(batch_transfer_idx);
                        let mut sender_record = RgbSenderFundingRecord {
                            version: RgbSenderFundingRecord::VERSION,
                            manual_broadcast: true,
                            temporary_channel_id: temporary_channel_id_hex,
                            final_channel_id: None,
                            funding_txid: funding_txid.clone(),
                            batch_transfer_idx,
                            rgb_info: Some(journal_rgb_info),
                            consignment_delivery: RgbSenderConsignmentDelivery::P2p,
                            stage: RgbSenderFundingStage::Preparing,
                        };
                        if let Err(error) = write_rgb_sender_funding_record(
                            &sender_record,
                            unlocked_state_copy.kv_store.as_ref(),
                        ) {
                            let cleanup = unlocked_state_copy.rgb_fail_transfers(
                                Some(batch_transfer_idx),
                                false,
                                true,
                            );
                            return match cleanup {
                                Ok(true) => Err(error),
                                Ok(false) => Err(RgbLibError::Internal {
                                    details: format!(
                                        "cannot persist RGB sender funding journal ({error}); the newly created transfer could not be released"
                                    ),
                                }),
                                Err(cleanup_error) => Err(RgbLibError::Internal {
                                    details: format!(
                                        "cannot persist RGB sender funding journal ({error}); transfer cleanup also failed: {cleanup_error}"
                                    ),
                                }),
                            };
                        }
                        if let Err(error) = unlocked_state_copy
                            .rgb_prepare_funding_fascia(funding_txid, fascia)
                        {
                            let cleanup = rollback_rgb_sender_funding(
                                sender_record,
                                unlocked_state_copy.rgb_wallet_wrapper.as_ref(),
                                unlocked_state_copy.kv_store.as_ref(),
                            );
                            return match cleanup {
                                Ok(()) => Err(error),
                                Err(cleanup_error) => Err(cleanup_error),
                            };
                        }
                        sender_record.stage = RgbSenderFundingStage::StockPromoted;
                        if let Err(error) = write_rgb_sender_funding_record(
                            &sender_record,
                            unlocked_state_copy.kv_store.as_ref(),
                        ) {
                            let cleanup = rollback_rgb_sender_funding(
                                sender_record.clone(),
                                unlocked_state_copy.rgb_wallet_wrapper.as_ref(),
                                unlocked_state_copy.kv_store.as_ref(),
                            );
                            return match cleanup {
                                Ok(()) => Err(error),
                                Err(cleanup_error) => Err(RgbLibError::Internal {
                                    details: format!(
                                        "cannot persist promoted RGB sender funding state ({error}); rollback also failed: {cleanup_error}"
                                    ),
                                }),
                            };
                        }
                        if let Err(error) = unlocked_state_copy
                            .rgb_wallet_wrapper
                            .checked_vss_backup()
                        {
                            let cleanup = rollback_rgb_sender_funding(
                                sender_record,
                                unlocked_state_copy.rgb_wallet_wrapper.as_ref(),
                                unlocked_state_copy.kv_store.as_ref(),
                            );
                            return match cleanup {
                                Ok(()) => Err(error),
                                Err(cleanup_error) => Err(cleanup_error),
                            };
                        }
                        Ok((res.psbt, Some(batch_transfer_idx), sender_record))
                    },
                )
                .await
                {
                    Ok(result) => result,
                    Err(error) => {
                        return handle_funding_prepare_err(
                            RgbLibError::Internal {
                                details: format!(
                                    "RGB channel funding preparation worker failed: {error}"
                                ),
                            },
                            &unlocked_state.channel_manager,
                            &temporary_channel_id,
                            &counterparty_node_id,
                        );
                    }
                };
                let (unsigned_psbt, batch_transfer_idx, sender_record) = match res {
                    Ok(result) => result,
                    // A failed funding preparation (e.g. the asset allocation is
                    // momentarily reserved by a concurrent open) must fail the
                    // channel so the caller can retry, not retry the event
                    // forever. handle_open_chan_fail (on ChannelClosed) then
                    // releases any reserved allocation.
                    Err(e) => {
                        return handle_funding_prepare_err(
                            e,
                            &unlocked_state.channel_manager,
                            &temporary_channel_id,
                            &counterparty_node_id,
                        );
                    }
                };
                // Record the batch transfer index on the channel's RGB info so a failed
                // open can fail the pending transfer and release the locked assets
                // (see handle_open_chan_fail).
                if let Some(mut rgb_info) = get_rgb_channel_info_optional(
                    &temporary_channel_id,
                    true,
                    unlocked_state.kv_store.as_ref(),
                ) {
                    rgb_info.batch_transfer_idx = batch_transfer_idx;
                    unlocked_state.kv_store.write_rgb_channel_info(
                        &temporary_channel_id.0.as_hex().to_string(),
                        &rgb_info,
                        true,
                    );
                }
                (unsigned_psbt, Some(asset_id), Some(sender_record))
            } else {
                // Mirror the colored path: a failed funding preparation must fail
                // the channel (so the caller can retry) rather than panic the event
                // task. handle_funding_prepare_err force-closes on terminal errors
                // and replays the event on transient network errors.
                let raw_psbt = match unlocked_state.rgb_send_btc_begin(
                    addr.to_address(),
                    channel_value_satoshis,
                    unlocked_state.config.rgb.fee_rate_sat_vb,
                ) {
                    Ok(psbt) => psbt,
                    Err(e) => {
                        return handle_funding_prepare_err(
                            e,
                            &unlocked_state.channel_manager,
                            &temporary_channel_id,
                            &counterparty_node_id,
                        );
                    }
                };
                let current_best_height =
                    unlocked_state.channel_manager.current_best_block().height;
                let unsigned_psbt =
                    match normalize_funding_psbt_locktime(raw_psbt, current_best_height) {
                        Ok(psbt) => psbt,
                        Err(e) => {
                            tracing::error!(
                                "failed to normalize channel funding PSBT locktime: {e}"
                            );
                            return Err(ReplayEvent());
                        }
                    };
                (unsigned_psbt, None, None)
            };
            #[cfg(debug_assertions)]
            funding_kill_checkpoint(FUNDING_CHECKPOINT_AFTER_COLOR);

            // With a remote external signer this call crosses the network: a transient transport
            // failure or a malformed reply must not panic the event task. Take the same
            // cooperative path as virtual funding — undo what the preparation staged (the pending
            // vanilla tx / the RGB batch transfer, which would otherwise be unabortable since the
            // PENDING_FUNDING mapping is only written after signing), then replay the event to
            // retry the preparation from scratch.
            let signing_outcome = unlocked_state
                .rgb_sign_psbt(unsigned_psbt.clone())
                .map_err(|e| format!("signing failed: {e}"))
                .and_then(|signed| {
                    Psbt::from_str(&signed).map_err(|e| format!("signed PSBT does not parse: {e}"))
                })
                .and_then(|psbt| {
                    psbt.clone()
                        .extract_tx()
                        .map(|tx| (psbt, tx))
                        .map_err(|e| format!("signed PSBT does not extract: {e}"))
                });
            let (psbt, funding_tx) = match signing_outcome {
                Ok(result) => result,
                Err(e) => {
                    tracing::error!("cannot sign channel funding transaction: {e}");
                    if let Err(cleanup_error) = abort_staged_standard_funding(
                        unlocked_state.clone(),
                        &temporary_channel_id,
                        &unsigned_psbt,
                        asset_id.is_some(),
                    )
                    .await
                    {
                        tracing::error!(
                            "cannot safely retry channel funding after cleanup failed: {cleanup_error}"
                        );
                        return handle_funding_prepare_err(
                            cleanup_error,
                            &unlocked_state.channel_manager,
                            &temporary_channel_id,
                            &counterparty_node_id,
                        );
                    }
                    return Err(ReplayEvent());
                }
            };
            let funding_txid = funding_tx.compute_txid();
            let funding_txid_str = funding_txid.to_string();
            tracing::info!("Funding TXID: {funding_txid_str}");

            // persist the funding TXID keyed by the final channel ID so handle_open_chan_fail can
            // find it
            let funding_output_index = match funding_tx
                .output
                .iter()
                .position(|o| o.script_pubkey == script_buf)
                .and_then(|index| u16::try_from(index).ok())
            {
                Some(index) => index,
                None => {
                    let error = RgbLibError::Internal {
                        details:
                            "signed funding transaction does not contain a valid expected output"
                                .to_owned(),
                    };
                    let cleanup_error = abort_staged_standard_funding(
                        unlocked_state.clone(),
                        &temporary_channel_id,
                        &unsigned_psbt,
                        is_colored,
                    )
                    .await
                    .err();
                    return handle_funding_prepare_err(
                        cleanup_error.unwrap_or(error),
                        &unlocked_state.channel_manager,
                        &temporary_channel_id,
                        &counterparty_node_id,
                    );
                }
            };
            let final_channel_id = ChannelId::v1_from_funding_txid(
                bitcoin::hashes::Hash::as_byte_array(&funding_txid),
                funding_output_index,
            );
            let final_channel_id_hex = final_channel_id.0.as_hex().to_string();
            if let Some(record) = sender_record.as_mut() {
                if record.funding_txid != funding_txid_str {
                    let error = RgbLibError::Internal {
                        details: "signed RGB funding transaction ID changed after preparation"
                            .to_owned(),
                    };
                    let cleanup_error = abort_staged_standard_funding(
                        unlocked_state.clone(),
                        &temporary_channel_id,
                        &unsigned_psbt,
                        true,
                    )
                    .await
                    .err();
                    return handle_funding_prepare_err(
                        cleanup_error.unwrap_or(error),
                        &unlocked_state.channel_manager,
                        &temporary_channel_id,
                        &counterparty_node_id,
                    );
                }
                record.final_channel_id = Some(final_channel_id_hex.clone());
                record.stage = RgbSenderFundingStage::HandoffReady;
                if let Err(error) =
                    write_rgb_sender_funding_record(record, unlocked_state.kv_store.as_ref())
                {
                    let cleanup = abort_staged_standard_funding(
                        unlocked_state.clone(),
                        &temporary_channel_id,
                        &unsigned_psbt,
                        true,
                    )
                    .await;
                    return handle_funding_prepare_err(
                        cleanup.err().unwrap_or(error),
                        &unlocked_state.channel_manager,
                        &temporary_channel_id,
                        &counterparty_node_id,
                    );
                }
                #[cfg(debug_assertions)]
                funding_kill_checkpoint(FUNDING_CHECKPOINT_HANDOFF_READY);
            }

            let persistence_result = unlocked_state
                .kv_store
                .write(
                    PENDING_FUNDING_NAMESPACE,
                    "",
                    &final_channel_id_hex,
                    funding_txid_str.clone().into_bytes(),
                )
                .and_then(|_| {
                    unlocked_state.kv_store.write(
                        PSBT_NAMESPACE,
                        "",
                        &funding_txid_str,
                        psbt.to_string().into_bytes(),
                    )
                });
            if let Err(error) = persistence_result {
                let error = rgb_sender_funding_error(
                    "cannot persist prepared channel funding transaction",
                    error,
                );
                let cleanup = abort_staged_standard_funding(
                    unlocked_state.clone(),
                    &temporary_channel_id,
                    &unsigned_psbt,
                    is_colored,
                )
                .await;
                return handle_funding_prepare_err(
                    cleanup.err().unwrap_or(error),
                    &unlocked_state.channel_manager,
                    &temporary_channel_id,
                    &counterparty_node_id,
                );
            }

            if let Some(asset_id) = asset_id {
                let witness_result = match RgbTxid::from_str(&funding_txid_str) {
                    Ok(witness_id) => {
                        let unlocked_state_copy = unlocked_state.clone();
                        let operation_id = funding_txid_str.clone();
                        tokio::task::spawn_blocking(move || {
                            unlocked_state_copy.rgb_upsert_witness_for_operation(
                                &operation_id,
                                witness_id,
                                WitnessOrd::Tentative,
                            )
                        })
                        .await
                        .map_err(|error| {
                            rgb_sender_funding_error("RGB witness worker failed", error)
                        })
                        .and_then(|result| result)
                    }
                    Err(error) => Err(rgb_sender_funding_error(
                        "cannot parse RGB funding witness transaction ID",
                        error,
                    )),
                };
                if let Err(error) = witness_result {
                    let cleanup = abort_staged_standard_funding(
                        unlocked_state.clone(),
                        &temporary_channel_id,
                        &unsigned_psbt,
                        true,
                    )
                    .await;
                    return handle_funding_prepare_err(
                        cleanup.err().unwrap_or(error),
                        &unlocked_state.channel_manager,
                        &temporary_channel_id,
                        &counterparty_node_id,
                    );
                }

                // send the consignment to the channel counterparty over the encrypted p2p link
                let consignment_path =
                    unlocked_state.rgb_get_send_consignment_path(&asset_id, &funding_txid_str);
                let consignment_bytes = match fs::read(&consignment_path) {
                    Ok(data) => data,
                    Err(e) => {
                        return abort_funding(
                            format!("cannot read funding consignment: {e}"),
                            &unlocked_state.channel_manager,
                            &temporary_channel_id,
                            &counterparty_node_id,
                        );
                    }
                };
                if let Err(e) = unlocked_state.kv_store.write(
                    FUNDING_CONSIGNMENT_NAMESPACE,
                    "",
                    &funding_txid_str,
                    consignment_bytes.clone(),
                ) {
                    tracing::error!("cannot store funding consignment: {e}");
                }
                if unlocked_state
                    .rgb_file_transfer_handler
                    .queue_consignment(
                        counterparty_node_id,
                        funding_txid_str.clone(),
                        consignment_bytes,
                    )
                    .is_err()
                {
                    return abort_funding(
                        s!("consignment is too large to send over p2p"),
                        &unlocked_state.channel_manager,
                        &temporary_channel_id,
                        &counterparty_node_id,
                    );
                }
                tracing::debug!(
                    asset_id,
                    consignment_path = %consignment_path.display(),
                    "Preserving consignment_out for rgb_send_end"
                );

                // send the asset's media files over the same p2p link
                let rgb_info = get_rgb_channel_info_pending(
                    &temporary_channel_id,
                    unlocked_state.kv_store.as_ref(),
                );
                if rgb_info.counterparty_knows_asset {
                    tracing::info!(
                        "counterparty already knows asset {asset_id}, not sending its media"
                    );
                } else {
                    let unlocked_state_copy = unlocked_state.clone();
                    let medias = match tokio::task::spawn_blocking(move || {
                        unlocked_state_copy.rgb_list_asset_media(asset_id)
                    })
                    .await
                    .unwrap()
                    {
                        Ok(medias) => medias,
                        Err(e) => {
                            return handle_funding_prepare_err(
                                e,
                                &unlocked_state.channel_manager,
                                &temporary_channel_id,
                                &counterparty_node_id,
                            );
                        }
                    };
                    for media in medias {
                        let media_bytes = match fs::read(&media.file_path) {
                            Ok(bytes) => bytes,
                            Err(e) => {
                                return abort_funding(
                                    format!("cannot read asset media file: {e}"),
                                    &unlocked_state.channel_manager,
                                    &temporary_channel_id,
                                    &counterparty_node_id,
                                );
                            }
                        };
                        if unlocked_state
                            .rgb_file_transfer_handler
                            .queue_media(
                                counterparty_node_id,
                                funding_txid_str.clone(),
                                media.digest,
                                media_bytes,
                            )
                            .is_err()
                        {
                            return abort_funding(
                                s!("asset media is too large to send over p2p"),
                                &unlocked_state.channel_manager,
                                &temporary_channel_id,
                                &counterparty_node_id,
                            );
                        }
                    }
                }

                unlocked_state.peer_manager.process_events();
            }

            let channel_manager_copy = unlocked_state.channel_manager.clone();

            // Colored funding is handed to LDK in checked manual-broadcast mode. LDK first
            // persists the counterparty signature and channel monitor, then emits the replayable
            // FundingTxBroadcastSafe event. The RGB transaction is never broadcast before that
            // durable recovery boundary. Vanilla channels retain LDK's automatic broadcaster.
            let handoff_result = if is_colored {
                channel_manager_copy.funding_transaction_generated_manual_broadcast(
                    temporary_channel_id,
                    counterparty_node_id,
                    funding_tx,
                )
            } else {
                channel_manager_copy.funding_transaction_generated(
                    temporary_channel_id,
                    counterparty_node_id,
                    funding_tx,
                )
            };
            if handoff_result.is_err() {
                tracing::error!(
                    "ERROR: Channel went away before we could fund it. The peer disconnected or refused the channel.",
                );
                if let Err(cleanup_error) = abort_staged_standard_funding(
                    unlocked_state.clone(),
                    &temporary_channel_id,
                    &unsigned_psbt,
                    is_colored,
                )
                .await
                {
                    tracing::error!(
                        "Failed to roll back rejected channel funding: {cleanup_error}"
                    );
                }
            } else if let Some(record) = sender_record.as_mut() {
                record.stage = RgbSenderFundingStage::HandedToLdk;
                if let Err(error) =
                    write_rgb_sender_funding_record(record, unlocked_state.kv_store.as_ref())
                {
                    // HandoffReady was persisted before the LDK call. Do not replay or roll back
                    // after LDK accepted the transaction; startup reconciliation uses the durable
                    // channel state to resolve this boundary.
                    tracing::error!(
                        funding_txid = %record.funding_txid,
                        error = %error,
                        "failed to advance RGB sender journal after LDK handoff"
                    );
                }
                #[cfg(debug_assertions)]
                funding_kill_checkpoint(FUNDING_CHECKPOINT_HANDED_TO_LDK);
            }
        }
        Event::FundingTxBroadcastSafe {
            channel_id,
            funding_txo,
            former_temporary_channel_id,
            ..
        } => {
            let _rgb_funding_operation = unlocked_state
                .rgb_funding_recovery_guard
                .lock_operation()
                .await;
            let funding_txid = funding_txo.txid.to_string();
            let mut record =
                read_rgb_sender_funding_record(&funding_txid, unlocked_state.kv_store.as_ref())
                    .map_err(|error| {
                        tracing::error!(
                            funding_txid,
                            error = %error,
                            "cannot load RGB sender journal at the broadcast-safe boundary"
                        );
                        ReplayEvent()
                    })?;
            let expected_temporary_channel_id = former_temporary_channel_id.0.as_hex().to_string();
            let expected_final_channel_id = channel_id.0.as_hex().to_string();
            if !record.manual_broadcast
                || record.temporary_channel_id != expected_temporary_channel_id
                || record.final_channel_id.as_deref() != Some(expected_final_channel_id.as_str())
            {
                tracing::error!(
                    funding_txid,
                    channel_id = %channel_id,
                    former_temporary_channel_id = %former_temporary_channel_id,
                    "RGB sender journal does not match the manual-broadcast event"
                );
                return Err(ReplayEvent());
            }

            unlocked_state.add_channel_id(former_temporary_channel_id, channel_id);
            match record.stage {
                RgbSenderFundingStage::HandoffReady | RgbSenderFundingStage::HandedToLdk => {
                    // Keep the event pending for one complete background-processor cycle. That
                    // cycle persists the funded ChannelManager state before a subsequent replay is
                    // allowed to publish the transaction. If persistence fails, the event remains
                    // pending and the exact PSBT is never broadcast.
                    record.stage = RgbSenderFundingStage::BroadcastSafeObserved;
                    write_rgb_sender_funding_record(&record, unlocked_state.kv_store.as_ref())
                        .map_err(|error| {
                            tracing::error!(
                                funding_txid,
                                error = %error,
                                "cannot persist RGB sender broadcast-safe observation"
                            );
                            ReplayEvent()
                        })?;
                    return Err(ReplayEvent());
                }
                RgbSenderFundingStage::BroadcastSafeObserved => {
                    #[cfg(debug_assertions)]
                    funding_kill_checkpoint(FUNDING_CHECKPOINT_BROADCAST_SAFE);
                    // This intent is durable before the first call that may publish the exact PSBT.
                    // A crash from this point onward always retries the same transaction and never
                    // releases its RGB allocation as though it had not been broadcast.
                    record.stage = RgbSenderFundingStage::Broadcasting;
                    write_rgb_sender_funding_record(&record, unlocked_state.kv_store.as_ref())
                        .map_err(|error| {
                            tracing::error!(
                                funding_txid,
                                error = %error,
                                "cannot persist RGB sender broadcast intent"
                            );
                            ReplayEvent()
                        })?;
                    #[cfg(debug_assertions)]
                    funding_kill_checkpoint(FUNDING_CHECKPOINT_BROADCASTING);
                }
                RgbSenderFundingStage::Broadcasting | RgbSenderFundingStage::BroadcastCommitted => {
                }
                RgbSenderFundingStage::Finalized | RgbSenderFundingStage::DurablyCompleted => {
                    let wallet = Arc::clone(&unlocked_state.rgb_wallet_wrapper);
                    let kv_store = Arc::clone(&unlocked_state.kv_store);
                    let finalized = record.clone();
                    tokio::task::spawn_blocking(move || {
                        complete_finalized_sender_funding(
                            &finalized,
                            wallet.as_ref(),
                            kv_store.as_ref(),
                        )
                    })
                    .await
                    .map_err(|error| {
                        tracing::error!(
                            funding_txid,
                            error = %error,
                            "RGB sender completion task failed during event replay"
                        );
                        ReplayEvent()
                    })?
                    .map_err(|error| {
                        tracing::error!(
                            funding_txid,
                            error = %error,
                            "finalized RGB sender funding is not durably complete"
                        );
                        ReplayEvent()
                    })?;
                    unlocked_state
                        .rgb_funding_recovery_guard
                        .clear(&funding_txid);
                    return Ok(());
                }
                stage => {
                    tracing::error!(
                        funding_txid,
                        ?stage,
                        "RGB sender journal reached broadcast-safe in an invalid stage"
                    );
                    return Err(ReplayEvent());
                }
            }

            let wallet = Arc::clone(&unlocked_state.rgb_wallet_wrapper);
            let kv_store = Arc::clone(&unlocked_state.kv_store);
            tokio::task::spawn_blocking(move || {
                let funding_txid = record.funding_txid.clone();
                commit_and_finalize_rgb_sender_funding(record, wallet.as_ref(), kv_store.as_ref())?;
                let finalized = read_rgb_sender_funding_record(&funding_txid, kv_store.as_ref())?;
                complete_finalized_sender_funding(&finalized, wallet.as_ref(), kv_store.as_ref())
            })
            .await
            .map_err(|error| {
                tracing::error!(
                    funding_txid,
                    error = %error,
                    "RGB sender broadcast task failed"
                );
                ReplayEvent()
            })?
            .map_err(|error| {
                tracing::error!(
                    funding_txid,
                    error = %error,
                    "RGB sender broadcast could not be committed"
                );
                ReplayEvent()
            })?;
            unlocked_state
                .rgb_funding_recovery_guard
                .clear(&funding_txid);
        }
        Event::PaymentClaimable {
            payment_hash,
            purpose,
            amount_msat,
            receiver_node_id: _,
            claim_deadline,
            onion_fields: _,
            counterparty_skimmed_fee_msat: _,
            receiving_channel_ids,
            payment_id: _,
        } => {
            tracing::info!(
                "EVENT: received payment from payment hash {} of {} millisatoshis",
                payment_hash,
                amount_msat,
            );
            #[cfg(test)]
            if node_override_matches(
                &HOLD_PAYMENT_CLAIMABLE_ON_NODE,
                unlocked_state.channel_manager.get_our_node_id(),
            ) {
                tracing::info!("TEST: holding PaymentClaimable for {}", payment_hash);
                HELD_PAYMENT_CLAIMABLE_COUNT.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                return Ok(());
            }
            #[cfg(test)]
            {
                let our_node_id = unlocked_state.channel_manager.get_our_node_id();
                if node_override_matches(&DEFER_PAYMENT_CLAIMABLE_ON_NODE, our_node_id) {
                    tracing::info!("TEST: deferring PaymentClaimable for {}", payment_hash);
                    PAYMENT_CLAIMABLE_DEFERRED.store(true, Ordering::SeqCst);
                    let deferred_at = Instant::now();
                    while node_override_matches(&DEFER_PAYMENT_CLAIMABLE_ON_NODE, our_node_id) {
                        if deferred_at.elapsed() > MAX_PAYMENT_DEFERRAL {
                            panic!(
                                "TEST: PaymentClaimable for {payment_hash} deferred for too long"
                            )
                        }
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    }
                    tracing::info!("TEST: resuming PaymentClaimable for {}", payment_hash);
                }
            }

            // `color_commitment` writes the authoritative per-HTLC record under
            // `chan_id || payment_hash` but never under the bare `<payment_hash>` key — that would
            // poison sibling channels in atomic RGB swaps. As the final hop we know exactly which
            // receiving channel delivered this HTLC, so project that scoped record to the bare
            // inbound key that `get_payment`/`list_payments` consume.
            let kv_store = &unlocked_state.kv_store;
            let htlc_payment_hash = hex_str(&payment_hash.0);
            for (chan_id, _) in &receiving_channel_ids {
                let chan_id_hex = hex_str(&chan_id.0);
                let htlc_proxy_id = format!("{chan_id_hex}{htlc_payment_hash}");
                if let Ok(data) =
                    kv_store.read(RGB_PRIMARY_NS, RGB_PAYMENT_INFO_INBOUND_NS, &htlc_proxy_id)
                {
                    if kv_store
                        .write(
                            RGB_PRIMARY_NS,
                            RGB_PAYMENT_INFO_INBOUND_NS,
                            &htlc_payment_hash,
                            data,
                        )
                        .is_ok()
                    {
                        break;
                    }
                }
            }

            let (payment_preimage, payment_secret, invoice) = match purpose {
                PaymentPurpose::SpontaneousPayment(preimage) => {
                    unlocked_state.channel_manager.claim_funds(preimage);
                    return Ok(());
                }
                PaymentPurpose::Bolt11InvoicePayment {
                    payment_preimage,
                    payment_secret,
                    ..
                }
                | PaymentPurpose::Bolt12OfferPayment {
                    payment_preimage,
                    payment_secret,
                    ..
                }
                | PaymentPurpose::Bolt12RefundPayment {
                    payment_preimage,
                    payment_secret,
                    ..
                } => {
                    let Some(invoice) = unlocked_state
                        .get_inbound_payments()
                        .payments
                        .get(&payment_hash)
                        .cloned()
                    else {
                        tracing::error!(
                            "Missing inbound payment state for claimable payment {:?}",
                            payment_hash
                        );
                        return Err(ReplayEvent());
                    };

                    (payment_preimage, Some(payment_secret), invoice)
                }
            };

            let now_ts = get_current_timestamp();
            if let Some(expiry) = invoice.expires_at {
                if now_ts >= expiry {
                    tracing::warn!(
                        "Received HTLC for expired invoice {payment_hash:?} (expiry {expiry})"
                    );
                    unlocked_state.fail_htlc_backwards_and_update_inbound_payment(
                        payment_hash,
                        HTLCStatus::Failed,
                    );
                    return Ok(());
                }
            }

            if let Some(expected) = invoice.amt_msat {
                if amount_msat < expected {
                    tracing::warn!(
                        "Received {} msat for invoice {} but expected at least {} msat",
                        amount_msat,
                        payment_hash,
                        expected
                    );
                    unlocked_state.fail_htlc_backwards_and_update_inbound_payment(
                        payment_hash,
                        HTLCStatus::Failed,
                    );
                    return Ok(());
                }
            }

            match invoice.invoice_type.unwrap_or(InvoiceType::AutoClaim) {
                InvoiceType::AutoClaim => {
                    let Some(claim_preimage) = payment_preimage else {
                        tracing::error!(
                            "Missing LDK preimage for auto-claim invoice {:?}",
                            payment_hash
                        );
                        return Err(ReplayEvent());
                    };
                    unlocked_state.channel_manager.claim_funds(claim_preimage);
                }
                InvoiceType::Hodl {
                    async_payment_recipient: true,
                } => {
                    let async_preimage = match invoice.preimage {
                        Some(preimage) => preimage,
                        None if unlocked_state.external_signer_mode => {
                            let (
                                Some(external_signer),
                                Some(async_host_node_id),
                                Some(async_hash_index),
                            ) = (
                                unlocked_state.external_signer.as_ref(),
                                invoice.async_host_node_id,
                                invoice.async_hash_index,
                            )
                            else {
                                tracing::error!(
                                    "Async recipient invoice for payment hash {:?} is missing the external-signer claim context",
                                    payment_hash
                                );
                                return Err(ReplayEvent());
                            };
                            match external_signer.get_async_payment_preimage(
                                hex_str(&async_host_node_id.serialize()),
                                async_hash_index,
                                hex_str(&payment_hash.0),
                            ) {
                                Ok(preimage_hex) => {
                                    match validate_and_parse_payment_preimage(
                                        &preimage_hex,
                                        &payment_hash,
                                    ) {
                                        Ok(preimage) => preimage,
                                        Err(_) => {
                                            tracing::error!(
                                                "The external signer returned an invalid async preimage for payment hash {:?}; failing back",
                                                payment_hash
                                            );
                                            unlocked_state
                                                .fail_htlc_backwards_and_update_inbound_payment(
                                                    payment_hash,
                                                    HTLCStatus::Failed,
                                                );
                                            return Ok(());
                                        }
                                    }
                                }
                                Err(err) => {
                                    tracing::warn!(
                                        "Async preimage fetch from external signer failed for payment hash {:?}: {err}; will retry",
                                        payment_hash
                                    );
                                    return Err(ReplayEvent());
                                }
                            }
                        }
                        None => {
                            tracing::error!(
                                "Missing stored preimage for async recipient invoice {:?}",
                                payment_hash
                            );
                            return Err(ReplayEvent());
                        }
                    };
                    unlocked_state.channel_manager.claim_funds(async_preimage);
                }
                InvoiceType::Hodl {
                    async_payment_recipient: false,
                } => {
                    unlocked_state.upsert_inbound_payment(
                        payment_hash,
                        HTLCStatus::Claimable,
                        payment_preimage,
                        payment_secret,
                        Some(amount_msat),
                        unlocked_state.channel_manager.get_our_node_id(),
                        claim_deadline,
                        None,
                    );
                    unlocked_state
                        .async_order_handler
                        .notify_claimable_hodl_invoice(payment_hash, amount_msat, claim_deadline);
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

            let kv_store_dyn: Arc<dyn KVStoreSync + Send + Sync> =
                Arc::clone(&unlocked_state.kv_store) as Arc<dyn KVStoreSync + Send + Sync>;
            if let Err(e) = _finalize_rgb_channel_payment(&payment_hash, true, &kv_store_dyn) {
                tracing::error!(
                    "RGB balance update failed for claimed payment {}: {e}",
                    hex_str(&payment_hash.0)
                );
                return Err(ReplayEvent());
            }
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
                    None,
                    None,
                );
            }
        }
        Event::PaymentSent {
            payment_preimage,
            payment_hash,
            fee_paid_msat,
            payment_id,
            ..
        } => {
            let kv_store_dyn: Arc<dyn KVStoreSync + Send + Sync> =
                Arc::clone(&unlocked_state.kv_store) as Arc<dyn KVStoreSync + Send + Sync>;
            if let Err(e) = _finalize_rgb_channel_payment(&payment_hash, false, &kv_store_dyn) {
                tracing::error!(
                    "RGB balance update failed for sent payment {}: {e}",
                    hex_str(&payment_hash.0)
                );
                return Err(ReplayEvent());
            }

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
                unlocked_state
                    .async_order_handler
                    .notify_payment_sent(payment_hash, payment_preimage);
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
            ref channel_type,
            ..
        } => {
            #[cfg(test)]
            if node_override_matches(
                &IGNORE_INBOUND_CHANNELS_ON_NODE,
                unlocked_state.channel_manager.get_our_node_id(),
            ) {
                tracing::info!(
                    "TEST: ignoring inbound channel {} from {}",
                    temporary_channel_id,
                    hex_str(&counterparty_node_id.serialize()),
                );
                return Ok(());
            }
            let mut random_bytes = [0u8; 16];
            random_bytes
                .copy_from_slice(&unlocked_state.entropy_source.get_secure_random_bytes()[..16]);
            let user_channel_id = u128::from_be_bytes(random_bytes);

            let (res, accepted) = if static_state.enable_virtual_channels_v0 {
                let trusted_virtual_peer = static_state.virtual_peer_pubkeys.is_empty()
                    || static_state
                        .virtual_peer_pubkeys
                        .iter()
                        .any(|trusted_peer| trusted_peer == counterparty_node_id);
                if !trusted_virtual_peer {
                    let err = "untrusted_virtual_peer".to_string();
                    tracing::error!(
                        "EVENT: Rejected inbound trusted virtual channel ({}) from {}: {}",
                        temporary_channel_id,
                        hex_str(&counterparty_node_id.serialize()),
                        err,
                    );
                    (
                        unlocked_state
                            .channel_manager
                            .force_close_broadcasting_latest_txn(
                                temporary_channel_id,
                                counterparty_node_id,
                                err,
                            ),
                        false,
                    )
                } else if !channel_type.supports_scid_privacy() {
                    let err = "unsupported_scid_alias".to_string();
                    tracing::error!(
                        "EVENT: Rejected inbound channel ({}) from {}: {}",
                        temporary_channel_id,
                        hex_str(&counterparty_node_id.serialize()),
                        err,
                    );
                    (
                        unlocked_state
                            .channel_manager
                            .force_close_broadcasting_latest_txn(
                                temporary_channel_id,
                                counterparty_node_id,
                                err,
                            ),
                        false,
                    )
                } else {
                    (
                        unlocked_state
                            .channel_manager
                            .accept_inbound_channel_from_trusted_peer_0conf(
                                temporary_channel_id,
                                counterparty_node_id,
                                user_channel_id,
                                None,
                                ChannelFundingType::Virtual,
                            ),
                        true,
                    )
                }
            } else {
                (
                    unlocked_state.channel_manager.accept_inbound_channel(
                        temporary_channel_id,
                        counterparty_node_id,
                        user_channel_id,
                        None,
                    ),
                    true,
                )
            };

            if let Err(e) = res {
                tracing::error!(
                    "EVENT: Failed to accept inbound channel ({}) from {}: {:?}",
                    temporary_channel_id,
                    hex_str(&counterparty_node_id.serialize()),
                    e,
                );
            } else if accepted {
                tracing::info!(
                    "EVENT: Accepted inbound channel ({}) from {}",
                    temporary_channel_id,
                    hex_str(&counterparty_node_id.serialize()),
                );
            } else {
                tracing::info!(
                    "EVENT: Rejected inbound channel ({}) from {}",
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
                clear_rgb_payment_pending(&hash, false, unlocked_state.kv_store.as_ref());
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
            clear_rgb_payment_pending(&payment_hash, true, unlocked_state.kv_store.as_ref());
            clear_rgb_payment_pending(&payment_hash, false, unlocked_state.kv_store.as_ref());
            let prev_channel_id_str = prev_channel_id.expect("prev_channel_id").to_string();
            let next_channel_id_str = next_channel_id.expect("next_channel_id").to_string();

            if let Some(outbound_amount_forwarded_rgb) = outbound_amount_forwarded_rgb {
                if let Err(e) = _safe_update_rgb_channel_amount(
                    &next_channel_id_str,
                    outbound_amount_forwarded_rgb,
                    0,
                    unlocked_state.kv_store.as_ref(),
                ) {
                    tracing::error!(
                        "RGB outbound balance update failed for forwarded payment on channel {}: {e}",
                        next_channel_id_str
                    );
                    return Err(ReplayEvent());
                }
            }
            if let Some(inbound_amount_forwarded_rgb) = inbound_amount_forwarded_rgb {
                if let Err(e) = _safe_update_rgb_channel_amount(
                    &prev_channel_id_str,
                    0,
                    inbound_amount_forwarded_rgb,
                    unlocked_state.kv_store.as_ref(),
                ) {
                    tracing::error!(
                        "RGB inbound balance update failed for forwarded payment on channel {}: {e}",
                        prev_channel_id_str
                    );
                    return Err(ReplayEvent());
                }
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
            let _rgb_funding_operation = unlocked_state
                .rgb_funding_recovery_guard
                .lock_operation()
                .await;
            tracing::info!(
                "EVENT: Channel {} with peer {} is pending awaiting funding lock-in!",
                channel_id,
                hex_str(&counterparty_node_id.serialize()),
            );

            if let Some(temporary_channel_id) = former_temporary_channel_id {
                unlocked_state.add_channel_id(temporary_channel_id, channel_id);
            }

            if unlocked_state
                .virtual_channel_session_store()
                .contains_key(&channel_id)
            {
                tracing::info!(
                    "EVENT: virtual channel {} is pending in trusted no-broadcast mode",
                    channel_id,
                );
                // reclaim the staged-funding slot now instead of waiting for the sweeper
                unlocked_state
                    .rgb_file_transfer_handler
                    .forget_staged_funding(&funding_txo.txid.to_string());
                return Ok(());
            }

            let funding_txid = funding_txo.txid.to_string();
            let channel_pending_sender_record = read_rgb_sender_funding_record_optional(
                &funding_txid,
                unlocked_state.kv_store.as_ref(),
            )
            .map_err(|error| {
                tracing::error!(
                    funding_txid,
                    error = %error,
                    "cannot inspect RGB sender journal at ChannelPending"
                );
                ReplayEvent()
            })?;
            if let Some(record) = channel_pending_sender_record.as_ref() {
                let expected_channel_id = channel_id.to_string();
                if record.final_channel_id.as_deref() != Some(expected_channel_id.as_str()) {
                    tracing::error!(
                        funding_txid,
                        channel_id = %channel_id,
                        journal_channel_id = ?record.final_channel_id,
                        "RGB sender journal does not match ChannelPending"
                    );
                    return Err(ReplayEvent());
                }
                if matches!(
                    record.stage,
                    RgbSenderFundingStage::Finalized | RgbSenderFundingStage::DurablyCompleted
                ) {
                    let finalized_record = record.clone();
                    let backup_wallet = Arc::clone(&unlocked_state.rgb_wallet_wrapper);
                    let recovery_kv_store = Arc::clone(&unlocked_state.kv_store);
                    tokio::task::spawn_blocking(move || {
                        complete_finalized_sender_funding(
                            &finalized_record,
                            backup_wallet.as_ref(),
                            recovery_kv_store.as_ref(),
                        )
                    })
                    .await
                    .map_err(|error| {
                        tracing::error!(
                            funding_txid,
                            error = %error,
                            "finalized RGB sender backup task failed"
                        );
                        ReplayEvent()
                    })?
                    .map_err(|error| {
                        tracing::error!(
                            funding_txid,
                            error = %error,
                            "cannot complete finalized RGB sender funding"
                        );
                        ReplayEvent()
                    })?;
                    unlocked_state
                        .rgb_funding_recovery_guard
                        .clear(&funding_txid);
                    return Ok(());
                }
                if record.manual_broadcast {
                    // The FundingTxBroadcastSafe event and sender journal exclusively own the
                    // manual-broadcast transaction. ChannelPending may be delivered first or may
                    // survive an interrupted broadcast event; acknowledging it here avoids a hot
                    // replay loop without mutating or discarding the recovery state.
                    tracing::info!(
                        funding_txid,
                        stage = ?record.stage,
                        "deferring RGB ChannelPending finalization to the manual-broadcast journal"
                    );
                    return Ok(());
                }
            }

            // Check if we have a stored PSBT (initiator case)
            match unlocked_state
                .kv_store
                .read(PSBT_NAMESPACE, "", &funding_txid)
            {
                Ok(psbt_bytes) => {
                    let psbt_str = String::from_utf8(psbt_bytes).map_err(|error| {
                        tracing::error!(
                            funding_txid,
                            error = %error,
                            "persisted channel funding PSBT is not valid UTF-8"
                        );
                        ReplayEvent()
                    })?;

                    let state_copy = unlocked_state.clone();
                    let psbt_str_copy = psbt_str.clone();

                    let is_chan_colored =
                        is_channel_rgb(&channel_id, unlocked_state.kv_store.as_ref());
                    tracing::info!("Initiator of the channel (colored: {})", is_chan_colored);

                    let mut sender_record = if is_chan_colored {
                        channel_pending_sender_record
                    } else {
                        None
                    };
                    if let Some(record) = sender_record.as_mut() {
                        record.stage = RgbSenderFundingStage::Broadcasting;
                        if let Err(error) = write_rgb_sender_funding_record(
                            record,
                            unlocked_state.kv_store.as_ref(),
                        ) {
                            tracing::error!("Cannot persist RGB sender broadcast intent: {error}");
                            return Err(ReplayEvent());
                        }
                    }

                    if let Some(record) = sender_record {
                        let recovery_wallet = Arc::clone(&unlocked_state.rgb_wallet_wrapper);
                        let recovery_kv_store = Arc::clone(&unlocked_state.kv_store);
                        let legacy_funding_txid = funding_txid.clone();
                        tokio::task::spawn_blocking(move || {
                            commit_and_finalize_rgb_sender_funding(
                                record,
                                recovery_wallet.as_ref(),
                                recovery_kv_store.as_ref(),
                            )?;
                            let finalized = read_rgb_sender_funding_record(
                                &legacy_funding_txid,
                                recovery_kv_store.as_ref(),
                            )?;
                            complete_finalized_sender_funding(
                                &finalized,
                                recovery_wallet.as_ref(),
                                recovery_kv_store.as_ref(),
                            )
                        })
                        .await
                        .map_err(|error| {
                            tracing::error!("Legacy RGB sender finalization task failed: {error}");
                            ReplayEvent()
                        })?
                        .map_err(|error| {
                            tracing::error!("Legacy RGB sender finalization failed: {error}");
                            ReplayEvent()
                        })?;
                        unlocked_state
                            .rgb_funding_recovery_guard
                            .clear(&funding_txid);
                    } else {
                        let join_result = tokio::task::spawn_blocking(move || {
                            if is_chan_colored {
                                // The consignment already went to the peer over P2P at funding
                                // time, so only local broadcast and DB bookkeeping remain.
                                state_copy
                                    .rgb_send_end_db_update_only(psbt_str_copy)
                                    .map(|result| result.txid)
                            } else {
                                state_copy.rgb_send_btc_end(psbt_str_copy)
                            }
                        })
                        .await;

                        let finalize_result = join_result.map_err(|join_err| {
                            tracing::error!(
                                "Channel opening finalization task failed: {join_err:?}"
                            );
                            ReplayEvent()
                        })?;

                        let _txid = finalize_result.map_err(|error| {
                            tracing::error!("Error completing channel opening: {error:?}");
                            ReplayEvent()
                        })?;
                    }

                    // RGB finalization removes this marker before its recovery tombstone. This
                    // idempotent removal also covers vanilla funding and legacy records.
                    remove_rgb_sender_funding_entry(
                        PENDING_FUNDING_NAMESPACE,
                        &channel_id.0.as_hex().to_string(),
                        "cannot remove finalized pending-funding marker",
                        unlocked_state.kv_store.as_ref(),
                    )
                    .map_err(|error| {
                        tracing::error!(
                            channel_id = %channel_id,
                            error = %error,
                            "cannot remove finalized pending-funding marker"
                        );
                        ReplayEvent()
                    })?;
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound => {
                    // The receiver's validated asset metadata is committed with the durable RGB
                    // funding acceptance. This event only releases the consignment copy retained
                    // for LDK event replay; validating it again here would repeat the full history
                    // walk and can take minutes for mature contracts.
                    if unlocked_state
                        .kv_store
                        .read_rgb_consignment(&funding_txid)
                        .is_ok()
                    {
                        unlocked_state
                            .kv_store
                            .remove_rgb_consignment(&funding_txid);
                    }
                }
                Err(error) => {
                    tracing::error!(
                        funding_txid,
                        error = %error,
                        "cannot read persisted channel funding PSBT"
                    );
                    return Err(ReplayEvent());
                }
            }

            if let Some(temporary_channel_id) = former_temporary_channel_id {
                let temporary_channel_id = temporary_channel_id.0.as_hex().to_string();
                match read_pending_funding_acceptance(
                    &temporary_channel_id,
                    unlocked_state.kv_store.as_ref(),
                ) {
                    Ok(record) => {
                        let recovery_channel_manager = Arc::clone(&unlocked_state.channel_manager);
                        let recovery_wallet = Arc::clone(&unlocked_state.rgb_wallet_wrapper);
                        let recovery_kv_store = Arc::clone(&unlocked_state.kv_store);
                        let recovery_funding_txid = record.funding_txid.clone();
                        tokio::task::spawn_blocking(move || {
                            let funded_channel_ids =
                                funded_channel_ids(recovery_channel_manager.as_ref());
                            match reconcile_receiver_funding_record(
                                &record,
                                &funded_channel_ids,
                                recovery_wallet.as_ref(),
                                recovery_kv_store.as_ref(),
                            )? {
                                None => Ok(()),
                                Some(recovery) => Err(RgbLibError::Internal {
                                    details: format!(
                                        "receiver funding '{}' remains quarantined in {:?}",
                                        recovery.funding_txid, record.stage
                                    ),
                                }),
                            }
                        })
                        .await
                        .map_err(|error| {
                            unlocked_state
                                .rgb_funding_recovery_guard
                                .quarantine(&recovery_funding_txid);
                            tracing::error!(
                                temporary_channel_id,
                                error = %error,
                                "RGB receiver reconciliation task failed"
                            );
                            ReplayEvent()
                        })?
                        .map_err(|error| {
                            unlocked_state
                                .rgb_funding_recovery_guard
                                .quarantine(&recovery_funding_txid);
                            tracing::error!(
                                temporary_channel_id,
                                error = %error,
                                "cannot reconcile RGB receiver funding at ChannelPending"
                            );
                            ReplayEvent()
                        })?;
                        unlocked_state
                            .rgb_funding_recovery_guard
                            .clear(&recovery_funding_txid);
                    }
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => {
                        unlocked_state
                            .rgb_funding_recovery_guard
                            .quarantine(&funding_txid);
                        tracing::error!(
                            temporary_channel_id,
                            error = %error,
                            "cannot inspect RGB receiver funding journal at ChannelPending"
                        );
                        return Err(ReplayEvent());
                    }
                }

                // The consignment record can stop counting against the node-wide cap.
                unlocked_state
                    .rgb_file_transfer_handler
                    .forget_staged_funding(&funding_txid);
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

            #[cfg(feature = "test-utils")]
            let our_node_id = unlocked_state.channel_manager.get_our_node_id();

            let _rgb_wallet_operation = unlocked_state
                .rgb_funding_recovery_guard
                .lock_operation()
                .await;
            match tokio::task::spawn_blocking(move || {
                unlocked_state.rgb_refresh(None, vec![], false)?;
                unlocked_state.rgb_refresh(None, vec![], true).map(|_| ())
            })
            .await
            {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    tracing::warn!(
                        channel_id = %channel_id,
                        error = %error,
                        "channel became ready but wallet refresh did not complete"
                    );
                }
                Err(error) => {
                    tracing::error!(
                        channel_id = %channel_id,
                        error = %error,
                        "channel-ready refresh worker failed"
                    );
                }
            }

            #[cfg(feature = "test-utils")]
            record_processed_channel_ready_event(channel_id, our_node_id);
        }
        Event::ChannelClosed {
            channel_id,
            reason,
            user_channel_id: _,
            counterparty_node_id,
            channel_capacity_sats: _,
            channel_funding_txo,
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

            // we can drop the funding consignment now that the channel has been closed
            if let Some(funding_txo) = channel_funding_txo {
                let funding_txid = funding_txo.txid.to_string();
                unlocked_state
                    .kv_store
                    .remove_rgb_consignment(&funding_txid);
                // drop the in-memory record too, so it stops counting against the node-wide cap
                unlocked_state
                    .rgb_file_transfer_handler
                    .forget_staged_funding(&funding_txid);
            }

            // Release any funds locked for a funding tx that was never broadcast.
            handle_open_chan_fail(&channel_id, unlocked_state.clone()).await;
            remove_finalized_sender_tombstone_for_channel(
                &channel_id,
                unlocked_state.kv_store.as_ref(),
            );

            let former_temporary_channel_id = unlocked_state.delete_channel_id(channel_id);
            let virtual_draft_temporary_channel_id = if unlocked_state
                .virtual_channel_draft_get(&channel_id)
                .is_some()
            {
                Some(channel_id)
            } else {
                former_temporary_channel_id.filter(|temporary_channel_id| {
                    unlocked_state
                        .virtual_channel_draft_get(temporary_channel_id)
                        .is_some()
                })
            };

            if let Some(temporary_channel_id) = virtual_draft_temporary_channel_id {
                unlocked_state.virtual_channel_draft_delete(&temporary_channel_id);
                let _ = unlocked_state.kv_store.remove(
                    "",
                    "",
                    &format!("virtual_channel_{}", temporary_channel_id),
                    false,
                );
                let _ = unlocked_state.kv_store.remove(
                    "",
                    "",
                    &format!("virtual_channel_{}", channel_id),
                    false,
                );

                tracing::warn!(
                    "EVENT: cleaned up failed virtual open draft {} after channel close {}",
                    temporary_channel_id,
                    channel_id,
                );
            }
        }
        Event::DiscardFunding { channel_id, .. } => {
            tracing::info!(
                "EVENT: Discarded funding for channel with ID {}",
                channel_id
            );

            // The funding tx was discarded before broadcast; release the locked funds.
            handle_open_chan_fail(&channel_id, unlocked_state.clone()).await;

            unlocked_state.delete_channel_id(channel_id);
            let _ = unlocked_state.kv_store.remove(
                "",
                "",
                &format!("virtual_channel_{}", channel_id),
                false,
            );
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
            let reject_intercept = |reason: &str| {
                if let Err(e) = unlocked_state
                    .channel_manager
                    .fail_intercepted_htlc(intercept_id)
                {
                    tracing::debug!("could not fail intercepted HTLC ({reason}): {e:?}");
                }
            };

            if !is_swap {
                tracing::warn!("Intercepted an HTLC that's not related to a swap");
                reject_intercept("not a swap");
                return Ok(());
            }

            let get_rgb_info = |channel_id| {
                get_rgb_channel_info_optional(channel_id, true, unlocked_state.kv_store.as_ref())
                    .map(|rgb_info| {
                        (
                            rgb_info.contract_id,
                            rgb_info.local_rgb_amount,
                            rgb_info.remote_rgb_amount,
                        )
                    })
            };

            let Some(inbound_channel) = unlocked_state
                .channel_manager
                .list_channels()
                .into_iter()
                .find(|details| details.outbound_scid_alias == Some(prev_outbound_scid_alias))
            else {
                tracing::error!(
                    "ERROR: no inbound channel matches the intercepted HTLC prev scid alias {prev_outbound_scid_alias}, rejecting it"
                );
                reject_intercept("no inbound channel");
                return Ok(());
            };
            let Some(outbound_channel) = unlocked_state
                .channel_manager
                .list_channels()
                .into_iter()
                .find(|details| {
                    details.short_channel_id == Some(requested_next_hop_scid)
                        || details.outbound_scid_alias == Some(requested_next_hop_scid)
                })
            else {
                tracing::error!(
                    "ERROR: no outbound channel matches the intercepted HTLC next hop scid {requested_next_hop_scid}, rejecting it"
                );
                reject_intercept("no outbound channel");
                return Ok(());
            };

            let inbound_rgb_info = get_rgb_info(&inbound_channel.channel_id);
            let outbound_rgb_info = get_rgb_info(&outbound_channel.channel_id);

            tracing::debug!("EVENT: Requested swap with params inbound_msat={} outbound_msat={} inbound_rgb={:?} outbound_rgb={:?} inbound_contract_id={:?}, outbound_contract_id={:?}", inbound_amount_msat, expected_outbound_amount_msat, inbound_rgb_amount, expected_outbound_rgb_payment.map(|(_, a)| a), inbound_rgb_info.map(|i| i.0), expected_outbound_rgb_payment.map(|(c, _)| c));

            let (whitelist_swap_info, whitelist_swap_status, pending_intercept_id, authorized_peer) = {
                let swaps_lock = unlocked_state.taker_swaps.lock().unwrap();
                match swaps_lock.swaps.get(&payment_hash) {
                    None => {
                        tracing::error!("ERROR: rejecting non-whitelisted swap");
                        reject_intercept("non-whitelisted swap");
                        return Ok(());
                    }
                    Some(x) => (
                        x.swap_info.clone(),
                        x.status,
                        x.pending_intercept_id,
                        x.authorized_peer,
                    ),
                }
            };

            match whitelist_swap_status {
                SwapStatus::Waiting => {}
                SwapStatus::Pending if pending_intercept_id == Some(intercept_id) => {}
                _ => {
                    tracing::error!(
                        "ERROR: swap whitelist entry is not in a forwardable state (status {whitelist_swap_status:?}), rejecting it"
                    );
                    reject_intercept(&format!(
                        "whitelist entry not forwardable (status {whitelist_swap_status:?})"
                    ));
                    return Ok(());
                }
            }

            if let Some(authorized_peer) = authorized_peer {
                if inbound_channel.counterparty.node_id != authorized_peer {
                    tracing::error!(
                        "ERROR: swap whitelist entry was authorized for peer {}, but intercepted HTLC came from {}, rejecting it",
                        authorized_peer,
                        inbound_channel.counterparty.node_id
                    );
                    reject_intercept("unauthorized peer");
                    return Ok(());
                }
            }

            if get_current_timestamp() > whitelist_swap_info.expiry {
                tracing::error!("ERROR: swap whitelist entry expired, rejecting it");
                unlocked_state.update_taker_swap_status(&payment_hash, SwapStatus::Expired);
                reject_intercept("whitelist entry expired");
                return Ok(());
            }

            let mut fail = false;
            if whitelist_swap_info.is_from_btc() {
                let net_msat_diff = expected_outbound_amount_msat.checked_sub(inbound_amount_msat);

                if inbound_rgb_amount != Some(whitelist_swap_info.qty_to)
                    || inbound_rgb_info.map(|x| x.0) != whitelist_swap_info.to_asset
                    || net_msat_diff != Some(whitelist_swap_info.qty_from)
                {
                    fail = true;
                }
            } else if whitelist_swap_info.is_to_btc() {
                let net_msat_diff =
                    inbound_amount_msat.saturating_sub(expected_outbound_amount_msat);

                if expected_outbound_rgb_payment
                    != whitelist_swap_info
                        .from_asset
                        .map(|asset| (asset, whitelist_swap_info.qty_from))
                    || outbound_rgb_info.map(|x| x.0) != whitelist_swap_info.from_asset
                    || net_msat_diff != whitelist_swap_info.qty_to
                {
                    fail = true;
                }
            } else {
                let net_msat_diff = inbound_amount_msat.checked_sub(expected_outbound_amount_msat);

                if net_msat_diff != Some(0)
                    || expected_outbound_rgb_payment
                        != whitelist_swap_info
                            .from_asset
                            .map(|asset| (asset, whitelist_swap_info.qty_from))
                    || outbound_rgb_info.map(|x| x.0) != whitelist_swap_info.from_asset
                    || inbound_rgb_amount != Some(whitelist_swap_info.qty_to)
                    || inbound_rgb_info.map(|x| x.0) != whitelist_swap_info.to_asset
                {
                    fail = true;
                }
            }

            if fail {
                tracing::error!("ERROR: swap doesn't match the whitelisted info, rejecting it");
                unlocked_state.update_taker_swap_status(&payment_hash, SwapStatus::Failed);
                reject_intercept("whitelist mismatch");
                return Ok(());
            }

            tracing::debug!("Swap is whitelisted, forwarding the htlc...");
            unlocked_state.update_taker_swap_pending_intercept(&payment_hash, intercept_id);

            if let Err(e) = unlocked_state.channel_manager.forward_intercepted_htlc(
                intercept_id,
                channelmanager::NextHopForward::ShortChannelId(requested_next_hop_scid),
                outbound_channel.counterparty.node_id,
                expected_outbound_amount_msat,
                expected_outbound_rgb_payment,
            ) {
                tracing::error!("ERROR: failed to forward whitelisted swap HTLC: {e:?}");
                unlocked_state.update_taker_swap_status(&payment_hash, SwapStatus::Failed);
                reject_intercept("forward failed");
            }
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
            let _rgb_wallet_operation = unlocked_state
                .rgb_funding_recovery_guard
                .lock_operation()
                .await;
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

// Resolves the RGB amount a spendable output carries. An empty map is a truly vanilla tx (0);
// a non-empty map that lacks the output is an invariant violation and must error so the sweep
// retries rather than paying the colored output out as vanilla BTC, stranding the allocation.
fn rgb_amount_for_spendable_output(
    output_map: &HashMap<u32, u64>,
    vout: u32,
    txid: &str,
) -> Result<u64, String> {
    match output_map.get(&vout) {
        Some(amt) => Ok(*amt),
        None if output_map.is_empty() => Ok(0),
        None => Err(format!(
            "spendable output {txid}:{vout} absent from a non-empty transfer info map"
        )),
    }
}

impl RgbOutputSpender {
    fn try_spend_spendable_outputs(
        &self,
        descriptors: &[&SpendableOutputDescriptor],
        outputs: Vec<TxOut>,
        change_destination_script: ScriptBuf,
        feerate_sat_per_1000_weight: u32,
        locktime: Option<LockTime>,
        secp_ctx: &Secp256k1<All>,
    ) -> Result<bitcoin::Transaction, String> {
        let _rgb_wallet_operation = self
            .rgb_funding_recovery_guard
            .lock_rgb_wallet_mutation()
            .map_err(|error| {
                tracing::debug!(%error, "deferring RGB output sweep during funding transition");
                error.to_string()
            })?;
        let mut hasher = DefaultHasher::new();
        descriptors.hash(&mut hasher);
        let descriptors_hash = hasher.finish();
        let mut txes = self.txes.lock().unwrap_or_else(|e| e.into_inner());
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

            let Ok(transfer_info_bytes) = self.kv_store.read(
                RGB_PRIMARY_NS,
                lightning::rgb_utils::RGB_TRANSFER_INFO_NS,
                &txid_str,
            ) else {
                continue;
            };
            // decode here rather than via read_rgb_transfer_info: that one panics, and we hold
            // the txes lock, so a bad record would poison it and brick every later sweep
            let transfer_info: TransferInfo = bincode::deserialize(&transfer_info_bytes)
                .map_err(|e| format!("cannot decode transfer info for {txid_str}: {e}"))?;
            let amt_rgb = rgb_amount_for_spendable_output(
                &transfer_info.output_map,
                outpoint.index.into(),
                &txid_str,
            )?;
            if amt_rgb == 0 {
                continue;
            }

            vanilla_descriptor = false;

            let closing_height = self
                .rgb_wallet_wrapper
                .get_tx_height(txid_str.clone())
                .map_err(|e| format!("cannot get height of {txid_str}: {e}"))?
                .ok_or_else(|| format!("transaction {txid_str} is not confirmed yet"))?;
            let witness_id = RgbTxid::from_str(&txid_str)
                .map_err(|error| format!("invalid sweep witness transaction ID: {error}"))?;
            let update_res = self
                .rgb_wallet_wrapper
                .update_witnesses(closing_height, vec![witness_id])
                .map_err(|e| format!("error while updating witnesses for {txid_str}: {e}"))?;
            if !update_res.failed.is_empty() {
                return Err(format!(
                    "failed to update witnesses for {txid_str}: {update_res:?}"
                ));
            }

            let contract_id = transfer_info.contract_id;

            let mut new_asset = false;
            let recipient_id = if let Some((_, _, recipient_id)) = asset_info.get(&contract_id) {
                recipient_id.clone()
            } else {
                new_asset = true;
                let cache_key = (descriptors_hash, contract_id);
                let cached = {
                    let mut recipients = self
                        .sweep_recipients
                        .lock()
                        .unwrap_or_else(|e| e.into_inner());
                    match recipients.get(&cache_key) {
                        Some((recipient_id, expiration))
                            if sweep_receive_is_reusable(
                                get_current_timestamp(),
                                *expiration,
                                self.static_state.reuse_addresses,
                            ) =>
                        {
                            Some(recipient_id.clone())
                        }
                        // too close to expiry to be used again, don't keep retrying against it
                        Some(_) => {
                            recipients.remove(&cache_key);
                            None
                        }
                        None => None,
                    }
                };
                let recipient_id = match cached {
                    Some(recipient_id) => recipient_id,
                    None => {
                        let expiration =
                            get_current_timestamp() + RGB_TRANSFER_CHAN_EXPIRATION_SECS;
                        let receive_data = self
                            .rgb_wallet_wrapper
                            .witness_receive(None, Assignment::Any, expiration, vec![], 0)
                            .map_err(|e| format!("cannot get a witness receive script: {e}"))?;
                        self.sweep_recipients
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .insert(cache_key, (receive_data.recipient_id.clone(), expiration));
                        receive_data.recipient_id
                    }
                };
                let script_pubkey = script_buf_from_recipient_id(recipient_id.clone())
                    .map_err(|e| format!("invalid sweep recipient id: {e}"))?
                    .ok_or_else(|| s!("sweep recipient id has no script"))?;
                txouts.push(TxOut {
                    value: Amount::from_sat(
                        self.static_state.config.channels.dust_limit_msat / 1000,
                    ),
                    script_pubkey,
                });
                recipient_id
            };

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
            return self
                .signer
                .spend_spendable_outputs(
                    descriptors.as_ref(),
                    txouts,
                    change_destination_script,
                    feerate_sat_per_1000_weight,
                    locktime,
                    secp_ctx,
                )
                .map_err(|()| s!("cannot spend vanilla spendable outputs"));
        }

        let feerate_sat_per_1000_weight = self.static_state.config.rgb.fee_rate_sat_vb as u32 * 250; // 1 sat/vB = 250 sat/kw
        let (psbt, _expected_max_weight) =
            SpendableOutputDescriptor::create_spendable_outputs_psbt(
                secp_ctx,
                descriptors,
                txouts,
                change_destination_script,
                feerate_sat_per_1000_weight,
                locktime,
            )
            .map_err(|()| s!("cannot create the spendable outputs PSBT"))?;

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

        let mut psbt = RgbLibPsbt::from_str(&psbt.to_string())
            .map_err(|error| format!("failed to convert sweep PSBT for RGB coloring: {error}"))?;
        let consignments = self
            .rgb_wallet_wrapper
            .color_psbt_and_consume(&mut psbt, coloring_info)
            .map_err(|e| format!("cannot color the sweep PSBT: {e}"))?;

        let mut psbt = Psbt::from_str(&psbt.to_string()).map_err(|error| {
            format!("failed to convert colored sweep PSBT for signing: {error}")
        })?;

        psbt = self
            .signer
            .sign_spendable_outputs_psbt(descriptors, psbt, secp_ctx)
            .map_err(|e| format!("cannot sign the sweep PSBT: {e:?}"))?;

        let spending_tx = match psbt.extract_tx() {
            Ok(tx) => tx,
            Err(ExtractTxError::MissingInputValue { tx }) => tx,
            Err(error) => {
                tracing::error!(%error, "failed to extract signed sweep transaction");
                return Err(format!(
                    "failed to extract signed sweep transaction: {error}"
                ));
            }
        };

        let closing_txid = spending_tx.compute_txid().to_string();

        let handle = Handle::try_current()
            .map_err(|error| format!("RGB output sweep has no Tokio runtime: {error}"))?;
        let _ = handle.enter();

        for consignment in consignments {
            let contract_id = consignment.contract_id();

            // persist consignment and hand it to rgb-lib (out-of-band)
            let consignment_path = self
                .static_state
                .ldk_data_dir
                .join(format!("consignment_{closing_txid}_{contract_id}"));
            consignment
                .save_file(&consignment_path)
                .map_err(|e| format!("cannot save consignment: {e}"))?;
            let consignment_path_str = consignment_path.to_string_lossy().to_string();
            let rgb_wallet_wrapper_copy = self.rgb_wallet_wrapper.clone();
            futures::executor::block_on(tokio::task::spawn_blocking(move || {
                rgb_wallet_wrapper_copy
                    .provide_out_of_band_consignment(consignment_path_str, vec![])
            }))
            .map_err(|e| format!("consignment task failed: {e}"))?
            .map_err(|e| format!("cannot provide consignment: {e}"))?;
            if let Err(e) = fs::remove_file(&consignment_path) {
                tracing::warn!(error = %e, "cannot remove consignment file, leaving it behind");
            }
        }

        // insert so the encoded write includes this entry; roll back if the write fails, or the
        // early return above would hand back a broadcast tx that was never persisted
        txes.insert(descriptors_hash, spending_tx.clone());
        if let Err(e) = self
            .kv_store
            .write("", "", OUTPUT_SPENDER_TXES_KEY, txes.encode())
        {
            txes.remove(&descriptors_hash);
            return Err(format!("cannot persist output spender txes: {e}"));
        }
        self.sweep_recipients
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|(hash, _), _| *hash != descriptors_hash);

        Ok(spending_tx)
    }
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
        self.try_spend_spendable_outputs(
            descriptors,
            outputs,
            change_destination_script,
            feerate_sat_per_1000_weight,
            locktime,
            secp_ctx,
        )
        .map_err(|e| {
            tracing::error!("cannot spend spendable outputs, will retry: {e}");
        })
    }
}

/// VSS identity derived from the wallet mnemonic.
///
/// `signing_key` is used both for sigs-auth against the VSS server and for
/// deriving per-value encryption keys via HKDF-SHA256. `pubkey_hex` is its
/// serialized compressed public key in lower-hex; it serves as the LDK
/// stream's `store_id` directly, and the RGB-wallet stream uses
/// `{pubkey_hex}_rgb` to avoid key collisions within the same VSS server.
#[cfg(feature = "vss")]
use rgb_lib::bdk_wallet::keys::bip39::Mnemonic;

#[cfg(feature = "vss")]
pub(crate) struct VssIdentity {
    pub(crate) signing_key: rgb_lib::bitcoin::secp256k1::SecretKey,
    pub(crate) pubkey_hex: String,
}

/// Derive the VSS identity from the wallet mnemonic at `m/535'/1'` — a
/// hardened child of the node's seed at `m/535'`. Hardened derivation
/// prevents recovering the node seed from the VSS key, but the mnemonic
/// compromises both.
#[cfg(feature = "vss")]
pub(crate) fn derive_vss_identity(
    mnemonic: &Mnemonic,
    network: Network,
) -> Result<VssIdentity, APIError> {
    let xkey: ExtendedKey = mnemonic
        .clone()
        .into_extended_key()
        .map_err(|e| APIError::FailedVssInit(format!("VSS identity: invalid mnemonic: {e}")))?;
    let master_xprv = xkey.into_xprv(network.into()).ok_or_else(|| {
        APIError::FailedVssInit("VSS identity: failed to derive master xprv for network".into())
    })?;
    let secp = Secp256k1_30::new();
    let vss_xprv = master_xprv
        .derive_priv(
            &secp,
            &[
                ChildNumber::Hardened { index: 535 },
                ChildNumber::Hardened { index: 1 },
            ],
        )
        .map_err(|e| APIError::FailedVssInit(format!("VSS identity: derive_priv failed: {e}")))?;
    let signing_key = vss_xprv.private_key;
    let pubkey_hex = hex_str(&signing_key.public_key(&secp).serialize());
    Ok(VssIdentity {
        signing_key,
        pubkey_hex,
    })
}

/// Derive the VSS identity in external-signer mode, where the node does not
/// hold the mnemonic and so cannot reproduce the `m/535'/1'` private
/// derivation [`derive_vss_identity`] uses.
///
/// Instead we deterministically hash the public bootstrap identity (node id,
/// account xpubs, master fingerprint, protocol version) into a 32-byte seed
/// and use it directly as the VSS signing key. This mirrors how
/// [`derive_async_payments_compat_seed_from_bootstrap`] derives the
/// async-payments preimage root in external-signer mode, with a distinct
/// domain-separation tag so the VSS signing key and the APay seed are never
/// the same secret.
///
/// Properties:
/// - **Stable across restarts.** The bootstrap identity is re-derived from
///   the same mnemonic on every launch and persisted in the key-source file,
///   so the same wallet always maps to the same VSS store id + signing key.
/// - **Not mnemonic-equivalent.** The VSS identity here differs from the
///   `m/535'/1'` identity an internal-mnemonic node would produce for the
///   same seed, because the external signer never exposes that private
///   derivation. A wallet that backs up in external-signer mode must restore
///   in external-signer mode (which is the only mode WDK uses). Making the
///   two modes converge would require the external signer itself to expose a
///   VSS key derivation — out of scope here.
#[cfg(feature = "vss")]
pub(crate) fn derive_vss_identity_from_bootstrap(
    bootstrap: &crate::signer::types::BootstrapData,
) -> Result<VssIdentity, APIError> {
    derive_vss_identity_from_public_material(
        &bootstrap.identity.node_id,
        &bootstrap.identity.account_xpub_vanilla,
        &bootstrap.identity.account_xpub_colored,
        &bootstrap.identity.master_fingerprint,
        &bootstrap.protocol_version,
    )
}

/// Derive the same bootstrap VSS identity from the persisted `key_source.json`
/// instead of a live [`BootstrapData`].
///
/// `vss_clear_fence` runs on a *locked* node, so it has no unlock request and
/// therefore no in-memory bootstrap. But the bootstrap identity is mirrored
/// verbatim into the key-source file at external-signer init
/// ([`crate::signer::KeySourceFile::from_bootstrap`]), so this reconstructs a
/// VSS identity byte-identical to the one [`derive_vss_identity_from_bootstrap`]
/// produces at unlock — which is exactly what the single-writer fence needs so
/// the clear targets the same store id the running node acquired. The
/// `key_source_matches_bootstrap_identity` test pins that equivalence.
#[cfg(feature = "vss")]
pub(crate) fn derive_vss_identity_from_key_source(
    key_source: &crate::signer::KeySourceFile,
) -> Result<VssIdentity, APIError> {
    derive_vss_identity_from_public_material(
        &key_source.node_id,
        &key_source.account_xpub_vanilla,
        &key_source.account_xpub_colored,
        &key_source.master_fingerprint,
        &key_source.protocol_version,
    )
}

/// Shared core for the external-signer VSS identity. Hashing the same fields in
/// the same order is what guarantees the unlock-time (bootstrap) and
/// fence-clear-time (key-source) derivations agree; keep both callers routed
/// through here rather than duplicating the seed layout.
#[cfg(feature = "vss")]
fn derive_vss_identity_from_public_material(
    node_id: &str,
    account_xpub_vanilla: &str,
    account_xpub_colored: &str,
    master_fingerprint: &str,
    protocol_version: &str,
) -> Result<VssIdentity, APIError> {
    let mut seed_material = Vec::new();
    seed_material.extend_from_slice(b"rln-vss-identity-v1");
    seed_material.extend_from_slice(node_id.as_bytes());
    seed_material.extend_from_slice(account_xpub_vanilla.as_bytes());
    seed_material.extend_from_slice(account_xpub_colored.as_bytes());
    seed_material.extend_from_slice(master_fingerprint.as_bytes());
    seed_material.extend_from_slice(protocol_version.as_bytes());
    let seed = <sha256::Hash as BitcoinHash>::hash(&seed_material).to_byte_array();

    let secp = Secp256k1_30::new();
    let signing_key = rgb_lib::bitcoin::secp256k1::SecretKey::from_slice(&seed).map_err(|e| {
        APIError::FailedVssInit(format!(
            "VSS identity: invalid derived key from bootstrap: {e}"
        ))
    })?;
    let pubkey_hex = hex_str(&signing_key.public_key(&secp).serialize());
    Ok(VssIdentity {
        signing_key,
        pubkey_hex,
    })
}

#[cfg(all(test, feature = "vss"))]
mod vss_bootstrap_identity_tests {
    use super::*;
    use crate::signer::types::{BootstrapData, SignerIdentity};

    fn fake_bootstrap(node_id: &str) -> BootstrapData {
        BootstrapData {
            identity: SignerIdentity {
                node_id: node_id.to_string(),
                account_xpub_vanilla: "xv".to_string(),
                account_xpub_colored: "xc".to_string(),
                master_fingerprint: "deadbeef".to_string(),
            },
            protocol_version: "1".to_string(),
            api_level: 1,
        }
    }

    #[test]
    fn deterministic_for_same_bootstrap() {
        let b = fake_bootstrap(&"02".repeat(33));
        let a = derive_vss_identity_from_bootstrap(&b).expect("derive");
        let c = derive_vss_identity_from_bootstrap(&b).expect("derive");
        assert_eq!(a.pubkey_hex, c.pubkey_hex);
        assert_eq!(a.signing_key, c.signing_key);
    }

    #[test]
    fn differs_for_different_bootstrap() {
        let a =
            derive_vss_identity_from_bootstrap(&fake_bootstrap(&"02".repeat(33))).expect("derive");
        let b =
            derive_vss_identity_from_bootstrap(&fake_bootstrap(&"03".repeat(33))).expect("derive");
        assert_ne!(a.pubkey_hex, b.pubkey_hex);
    }

    #[test]
    fn domain_separated_from_async_payments_seed() {
        // The VSS signing key must not equal the APay preimage seed, even
        // though both hash the same public bootstrap material.
        let b = fake_bootstrap(&"02".repeat(33));
        let vss = derive_vss_identity_from_bootstrap(&b).expect("derive");
        let apay = crate::signer::types::derive_async_payments_compat_seed_from_bootstrap(&b);
        assert_ne!(vss.signing_key.secret_bytes(), apay);
    }

    #[test]
    fn key_source_matches_bootstrap_identity() {
        // The fence-clear path derives the VSS identity from the persisted
        // key_source.json, while unlock derives it from the live bootstrap.
        // Both must resolve to the same store id + signing key, otherwise
        // `vss_clear_fence` would delete the wrong fence and the running
        // node's store would stay locked.
        let b = fake_bootstrap(&"02".repeat(33));
        let key_source = crate::signer::KeySourceFile::from_bootstrap(&b);
        let from_bootstrap = derive_vss_identity_from_bootstrap(&b).expect("derive bootstrap");
        let from_key_source =
            derive_vss_identity_from_key_source(&key_source).expect("derive key source");
        assert_eq!(from_bootstrap.pubkey_hex, from_key_source.pubkey_hex);
        assert_eq!(from_bootstrap.signing_key, from_key_source.signing_key);
    }
}

/// Restore the RGB wallet directory from VSS if (a) VSS is configured for this
/// node, (b) the local wallet directory for `expected_fingerprint` is absent,
/// and (c) VSS has a backup for the given store. Mirrors the KV-side
/// auto-restore policy at `start_ldk`'s top: silent no-op when nothing is on
/// VSS, hard error otherwise unless `allow_empty_restore` is set.
#[cfg(feature = "vss")]
pub(crate) async fn maybe_restore_rgb_from_vss(
    vss_url: &str,
    rgb_store_id: String,
    signing_key: rgb_lib::bitcoin::secp256k1::SecretKey,
    data_dir: &std::path::Path,
    expected_fingerprint: &str,
    allow_empty_restore: bool,
) -> Result<(), APIError> {
    if data_dir.join(expected_fingerprint).exists() {
        // Local wallet already present — never clobber it with a VSS copy
        // that may be stale.
        return Ok(());
    }

    std::fs::create_dir_all(data_dir).map_err(|e| {
        APIError::FailedVssInit(format!(
            "RGB VSS restore: failed to create data_dir {}: {e}",
            data_dir.display()
        ))
    })?;

    let config =
        rgb_lib::wallet::vss::VssBackupConfig::new(vss_url.to_string(), rgb_store_id, signing_key)
            .with_encryption(true);
    let data_dir_str = data_dir.to_string_lossy().to_string();

    match rgb_lib::wallet::vss::restore_from_vss(config, &data_dir_str).await {
        Ok(path) => {
            tracing::info!(restored_path = %path.display(), "Restored RGB wallet from VSS");
            Ok(())
        }
        Err(rgb_lib::Error::VssBackupNotFound) => {
            tracing::info!("No RGB VSS backup found, starting fresh");
            Ok(())
        }
        Err(e) => {
            if allow_empty_restore {
                tracing::warn!(
                    error = %e,
                    "RGB VSS restore failed; starting fresh due to --vss-allow-empty-restore"
                );
                Ok(())
            } else {
                Err(APIError::FailedVssInit(format!(
                    "RGB VSS restore failed: {e}. Pass --vss-allow-empty-restore \
                     to start with an empty RGB wallet instead (UNSAFE if you \
                     previously had RGB assets)."
                )))
            }
        }
    }
}

// rgb-lib rejects wallets supporting IFA on mainnet
fn supported_asset_schemas(bitcoin_network: BitcoinNetwork) -> Vec<AssetSchema> {
    let mut schemas = vec![AssetSchema::Nia, AssetSchema::Cfa, AssetSchema::Uda];
    if bitcoin_network != BitcoinNetwork::Mainnet {
        schemas.push(AssetSchema::Ifa);
    }
    schemas
}

// A dead background processor must take the node down, not leave it serving without event
// processing; only `stop_processing` termination is expected. The shutdown is requested rather
// than forced, so the VSS teardown still runs; `main` turns `FATAL_ERROR` into exit code 70.
async fn supervise_background_processor(
    bp_future: impl std::future::Future<Output = Result<(), io::Error>> + Send,
    stop_flag: Arc<AtomicBool>,
    cancel_token: CancellationToken,
) -> Result<(), io::Error> {
    let result = futures::FutureExt::catch_unwind(std::panic::AssertUnwindSafe(bp_future)).await;
    let stopping = stop_flag.load(Ordering::Acquire);
    match result {
        Ok(res) => {
            if !stopping {
                let msg = match &res {
                    Ok(()) => "background processor exited unexpectedly".to_string(),
                    Err(e) => format!("background processor failed unexpectedly: {e}"),
                };
                tracing::error!("{msg}; shutting down");
                let _ = FATAL_ERROR.set(msg);
                cancel_token.cancel();
            }
            res
        }
        Err(panic_payload) => {
            let msg = panic_payload
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| panic_payload.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "unknown panic".to_string());
            tracing::error!(
                panic = %msg,
                "background processor panicked; shutting down instead of running without \
                 event processing"
            );
            let _ = FATAL_ERROR.set(format!("background processor panicked: {msg}"));
            cancel_token.cancel();
            Err(io::Error::new(
                io::ErrorKind::Other,
                format!("background processor panicked: {msg}"),
            ))
        }
    }
}

#[cfg(test)]
mod watchdog_tests {
    use super::*;

    // Exits with the same code `main` would once the server future has returned, via the shared
    // decision so this test cannot drift from the real one.
    fn exit_as_main_would() -> ! {
        std::process::exit(crate::utils::fatal_exit_code());
    }

    // Child mode re-runs this test in a subprocess so the exit code is observable.
    #[test]
    fn exits_with_code_70_on_bp_panic() {
        if std::env::var("BP_WATCHDOG_CHILD").is_ok() {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            let stop = Arc::new(AtomicBool::new(false));
            let cancel = CancellationToken::new();
            let _ = rt.block_on(supervise_background_processor(
                async { panic!("test panic") },
                stop,
                cancel.clone(),
            ));
            // The shutdown has to be requested, not forced: the VSS teardown runs on it.
            assert!(cancel.is_cancelled());
            exit_as_main_would();
        }
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "ldk::watchdog_tests::exits_with_code_70_on_bp_panic",
            ])
            .env("BP_WATCHDOG_CHILD", "1")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap();
        assert_eq!(status.code(), Some(70));
    }

    // An unexpected clean return is as fatal as a panic: the node would keep serving without
    // event processing.
    #[test]
    fn exits_with_code_70_on_unexpected_bp_return() {
        if std::env::var("BP_WATCHDOG_CHILD").is_ok() {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            let stop = Arc::new(AtomicBool::new(false));
            let cancel = CancellationToken::new();
            let _ = rt.block_on(supervise_background_processor(
                async { Ok(()) },
                stop,
                cancel.clone(),
            ));
            assert!(cancel.is_cancelled());
            exit_as_main_would();
        }
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "ldk::watchdog_tests::exits_with_code_70_on_unexpected_bp_return",
            ])
            .env("BP_WATCHDOG_CHILD", "1")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap();
        assert_eq!(status.code(), Some(70));
    }

    #[test]
    fn returns_without_exiting_when_stop_requested() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let stop = Arc::new(AtomicBool::new(true));
        let cancel = CancellationToken::new();
        let res = rt.block_on(supervise_background_processor(
            async { Err(io::Error::new(io::ErrorKind::Other, "aborted at teardown")) },
            stop,
            cancel.clone(),
        ));
        assert!(res.is_err());
        assert!(!cancel.is_cancelled());
    }
}

#[cfg(test)]
mod sweeper_predicate_tests {
    use super::*;

    #[test]
    fn rgb_amount_empty_map_is_vanilla() {
        let map = HashMap::new();
        assert_eq!(rgb_amount_for_spendable_output(&map, 0, "tx"), Ok(0));
    }

    #[test]
    fn rgb_amount_present_output_returns_amount() {
        let map = HashMap::from_iter([(1u32, 42u64)]);
        assert_eq!(rgb_amount_for_spendable_output(&map, 1, "tx"), Ok(42));
    }

    #[test]
    fn rgb_amount_missing_from_non_empty_map_errors() {
        let map = HashMap::from_iter([(1u32, 42u64)]);
        assert!(rgb_amount_for_spendable_output(&map, 0, "tx").is_err());
    }
}

/// Re-seed the RGB wallet with funding consignments the wallet doesn't know,
/// so a wallet restored without an RGB backup can color force-close sweeps
/// (issue #111). Failures are logged, never fatal: unlock must proceed.
async fn reimport_funding_consignments(
    rgb_wallet_wrapper: &Arc<RgbLibWalletWrapper>,
    kv_store: &Arc<SyncedKvStore>,
    ldk_data_dir: &Path,
) {
    let mark_replay_done = || {
        let _ =
            kv_store.write_local_only(REIMPORT_MARKER_NAMESPACE, "", REIMPORT_MARKER_KEY, vec![1]);
    };
    let replay_pending = kv_store
        .read(REIMPORT_MARKER_NAMESPACE, "", REIMPORT_MARKER_KEY)
        .is_err();
    let txids = match kv_store.list(FUNDING_CONSIGNMENT_NAMESPACE, "") {
        Ok(keys) => keys,
        Err(e) => {
            tracing::error!("cannot list stored funding consignments: {e}");
            return;
        }
    };
    if txids.is_empty() {
        mark_replay_done();
        return;
    }
    let known: HashSet<String> = match rgb_wallet_wrapper.list_assets(vec![]) {
        Ok(assets) => assets
            .nia
            .unwrap_or_default()
            .into_iter()
            .map(|a| a.asset_id)
            .chain(
                assets
                    .cfa
                    .unwrap_or_default()
                    .into_iter()
                    .map(|a| a.asset_id),
            )
            .chain(
                assets
                    .uda
                    .unwrap_or_default()
                    .into_iter()
                    .map(|a| a.asset_id),
            )
            .chain(
                assets
                    .ifa
                    .unwrap_or_default()
                    .into_iter()
                    .map(|a| a.asset_id),
            )
            .collect(),
        Err(e) => {
            tracing::error!("cannot list assets before consignment re-import: {e}");
            return;
        }
    };
    let mut imported_any = false;
    for txid in txids {
        let data = match kv_store.read(FUNDING_CONSIGNMENT_NAMESPACE, "", &txid) {
            Ok(data) => data,
            Err(e) => {
                tracing::error!("cannot read stored funding consignment for {txid}: {e}");
                continue;
            }
        };
        let consignment = match RgbTransfer::load(&mut std::io::Cursor::new(&data)) {
            Ok(consignment) => consignment,
            Err(e) => {
                tracing::error!("cannot load stored funding consignment for {txid}: {e}");
                continue;
            }
        };
        if known.contains(&consignment.contract_id().to_string()) {
            continue;
        }
        let wrapper = Arc::clone(rgb_wallet_wrapper);
        let txid_copy = txid.clone();
        let consignment_path = ldk_data_dir.join(format!("reimport_consignment_{txid}"));
        // Accept our stored copy straight from disk: this consumes the consignment into
        // the RGB runtime, which save_new_asset requires. Unlike the funding-time acceptor
        // flow there is no media staging dir to promote from -- only consignment bytes are
        // persisted -- so any media the contract declares must already be in the wallet.
        let res = tokio::task::spawn_blocking(move || -> Result<(), String> {
            fs::write(&consignment_path, &data).map_err(|e| e.to_string())?;
            let accept_res = wrapper.accept_transfer_consignment(
                consignment_path.clone(),
                txid_copy.clone(),
                1,
                STATIC_BLINDING,
            );
            let _ = fs::remove_file(&consignment_path);
            let (consignment, _, media_digests) = accept_res.map_err(|e| e.to_string())?;
            let media_dir = wrapper.get_media_dir();
            let missing: Vec<String> = media_digests
                .into_iter()
                .filter(|digest| !media_dir.join(digest).exists())
                .collect();
            if !missing.is_empty() {
                tracing::warn!(
                    "re-imported asset for {txid_copy} is missing {} media file(s) locally: {}",
                    missing.len(),
                    missing.join(", "),
                );
            }
            match wrapper.save_new_asset(consignment, txid_copy) {
                Ok(()) => Ok(()),
                Err(e) if e.to_string().contains("UNIQUE constraint failed") => Ok(()),
                Err(e) => Err(e.to_string()),
            }
        })
        .await;
        match res {
            Ok(Ok(())) => {
                imported_any = true;
                tracing::info!("re-imported funding consignment for {txid}");
            }
            Ok(Err(e)) => tracing::error!("cannot re-import funding consignment for {txid}: {e}"),
            Err(e) => tracing::error!("funding consignment re-import task failed for {txid}: {e}"),
        }
    }

    // The runtime also needs the funding->commitment transitions to color
    // force-close sweeps; re-consume the stored latest fascia of each channel.
    if !imported_any && !replay_pending {
        return;
    }
    let fascia_keys = match kv_store.list(RGB_PRIMARY_NS, RGB_COMMITMENT_FASCIA_NS) {
        Ok(keys) => keys,
        Err(e) => {
            tracing::error!("cannot list stored commitment fascias: {e}");
            return;
        }
    };
    for key in fascia_keys {
        let data = match kv_store.read(RGB_PRIMARY_NS, RGB_COMMITMENT_FASCIA_NS, &key) {
            Ok(data) => data,
            Err(e) => {
                tracing::error!("cannot read stored commitment fascia {key}: {e}");
                continue;
            }
        };
        let fascia = match deserialize_fascia(data) {
            Ok(fascia) => fascia,
            Err(e) => {
                tracing::error!("cannot deserialize stored commitment fascia {key}: {e}");
                continue;
            }
        };
        let wrapper = Arc::clone(rgb_wallet_wrapper);
        match tokio::task::spawn_blocking(move || {
            wrapper.consume_fascia(fascia, Some(WitnessOrd::Ignored))
        })
        .await
        {
            Ok(Ok(())) => tracing::info!("re-consumed commitment fascia {key}"),
            Ok(Err(e)) => tracing::error!("cannot re-consume commitment fascia {key}: {e}"),
            Err(e) => tracing::error!("commitment fascia re-consume task failed for {key}: {e}"),
        }
    }
    mark_replay_done();
}

// The unlock request wins, then the `[chain]` config section. There is no built-in default any
// more, so an indexer that resolves from neither is a hard error rather than a silent fallback.
fn resolve_indexer_url<'a>(
    request: Option<&'a str>,
    config: Option<&'a str>,
) -> Result<&'a str, APIError> {
    request.or(config).ok_or(APIError::MissingIndexerUrl)
}

pub(crate) async fn start_ldk(
    app_state: Arc<AppState>,
    key_source: NodeKeySource,
    mut unlock_request: UnlockRequest,
) -> Result<(LdkBackgroundServices, Arc<UnlockedAppState>), APIError> {
    let gossip_source_config = unlock_request.gossip_source.clone().unwrap_or_default();
    let static_state = &app_state.static_state;

    // Unlock request params take precedence, the config file provides defaults.
    let file_config = &static_state.config;
    unlock_request.proxy_endpoint = unlock_request
        .proxy_endpoint
        .or_else(|| file_config.chain.proxy_endpoint.clone());
    unlock_request.announce_alias = unlock_request
        .announce_alias
        .or_else(|| file_config.node.announce_alias.clone());
    if unlock_request.announce_addresses.is_empty() {
        unlock_request.announce_addresses = file_config.node.announce_addresses.clone();
    }
    let (
        internal_mnemonic,
        external_signer_mode,
        external_bootstrap,
        external_signer,
        external_node_id,
        external_signer_link_watch,
    ) = match key_source {
        NodeKeySource::InternalMnemonic(mnemonic) => {
            (Some(mnemonic), false, None, None, None, None)
        }
        NodeKeySource::External(external) => {
            // Grab this before the transport is wrapped into `ExternalSigner` below: `Some` only for
            // transports that can genuinely go unreachable and recover (the remote-signer daemon
            // link), `None` for e.g. an in-process uniffi signer that can never be unreachable.
            let link_watch = external.signer_attachment.transport.link_watch();
            let signer = ExternalSigner::from_attachment(&external.signer_attachment)
                .map_err(|e| APIError::ExternalSignerProtocolError(e.to_string()))?;
            let bootstrap = external.signer_attachment.bootstrap.clone();
            if bootstrap.api_level != SUPPORTED_SIGNER_API_LEVEL {
                return Err(APIError::ExternalSignerProtocolError(format!(
                    "unsupported external signer api_level {}, expected {}",
                    bootstrap.api_level, SUPPORTED_SIGNER_API_LEVEL
                )));
            }

            let key_source = read_key_source_file(&static_state.storage_dir_path)
                .map_err(|e| APIError::ExternalSignerProtocolError(e.to_string()))?
                .ok_or(APIError::NotInitialized)?;
            validate_key_source_matches_bootstrap(&key_source, &bootstrap)
                .map_err(|_| APIError::ExternalSignerMismatch)?;

            if bootstrap != external.bootstrap {
                return Err(APIError::ExternalSignerMismatch);
            }
            let external_node_id = Some(bootstrap.identity.node_id.clone());
            (
                None,
                true,
                Some(bootstrap),
                Some(Arc::new(signer)),
                external_node_id,
                link_watch,
            )
        }
    };

    // Initialize Persistence using shared database connection
    let local_kv_store = Arc::new(crate::kv_store::SeaOrmKvStore::from_connection(
        static_state.db(),
    ));

    // Initialize VSS replication if configured.
    //
    // The same VSS identity is reused across three call sites further down
    // (RGB-wallet auto-backup config and the rgb-lib VssBackupClient handle),
    // so derive it once here and pass it around instead of repeating the
    // derivation. [[derive_vss_identity]]
    #[cfg(feature = "vss")]
    let vss_identity: Option<VssIdentity> = if static_state.vss_url.is_some() {
        // Internal-mnemonic mode derives the VSS identity from the seed at
        // `m/535'/1'`. External-signer mode never holds the mnemonic, so it
        // derives a stable identity from the public bootstrap instead — see
        // [[derive_vss_identity_from_bootstrap]]. Either path yields a VSS
        // identity stable across restarts for the same wallet.
        match internal_mnemonic.as_ref() {
            Some(mnemonic) => Some(derive_vss_identity(mnemonic, static_state.network.into())?),
            None => {
                let bootstrap = external_bootstrap.as_ref().ok_or_else(|| {
                    APIError::FailedVssInit(
                        "VSS identity: external-signer mode is missing bootstrap data".into(),
                    )
                })?;
                Some(derive_vss_identity_from_bootstrap(bootstrap)?)
            }
        }
    } else {
        None
    };

    // Monitors and the channel manager persist remote-first (VSS-durable before
    // ack); aux state keeps the local-first `kv_store` sync facade. All share
    // the same local DB and (when configured) VSS store.
    #[cfg(feature = "vss")]
    let bp_local_kv_store = Arc::clone(&local_kv_store);
    #[cfg(feature = "vss")]
    let mut fence_guard: Option<crate::vss_kv_store::FenceReleaseGuard> = None;
    #[cfg(feature = "vss")]
    let mut vss_restored_keys: usize = 0;
    #[cfg(feature = "vss")]
    let (kv_store, monitor_kv_store) = if let (Some(ref vss_url), Some(ref identity)) =
        (&static_state.vss_url, &vss_identity)
    {
        tracing::info!(store_id = %identity.pubkey_hex, "Initializing VSS KV store");
        let vss_kv_store = Arc::new(
            crate::vss_kv_store::VssKvStore::new_with_retry(
                vss_url.clone(),
                identity.pubkey_hex.clone(),
                identity.signing_key,
                &static_state.config.vss,
            )
            .map_err(|e| APIError::FailedVssInit(e.to_string()))?,
        );

        // Acquire the single-writer fence before any reads/writes go out.
        // This refuses to start if another instance owns this store_id —
        // see [[single_writer_invariant]] in CLAUDE.md.
        vss_kv_store
            .acquire_fence()
            .map_err(|e| APIError::FailedVssInit(e.to_string()))?;

        // Release the just-acquired fence if the rest of startup fails.
        fence_guard = Some(crate::vss_kv_store::FenceReleaseGuard::new({
            let store = Arc::clone(&vss_kv_store);
            move || {
                if let Err(e) = store.release_fence_if_owned() {
                    tracing::warn!(error = %e, "failed to release VSS fence after aborted unlock");
                }
            }
        }));

        let monitor_kv_store = Arc::new(RemoteFirstKvStore::new(
            Arc::clone(&local_kv_store),
            Some(Arc::clone(&vss_kv_store)),
        ));
        let synced = Arc::new(SyncedKvStore::with_vss(local_kv_store, vss_kv_store));

        // Auto-restore from VSS if local DB has no channel manager data.
        // On failure: abort unlock unless --vss-allow-empty-restore was set.
        // Starting a recovering node with empty local state can lose funds
        // (no channel monitors → can't watch chain), so we refuse by default.
        let has_local_data = synced
            .read(
                CHANNEL_MANAGER_PERSISTENCE_PRIMARY_NAMESPACE,
                CHANNEL_MANAGER_PERSISTENCE_SECONDARY_NAMESPACE,
                CHANNEL_MANAGER_PERSISTENCE_KEY,
            )
            .is_ok();
        if !has_local_data {
            match synced.restore_from_vss(false) {
                Ok(0) => tracing::info!("No VSS backup data found, starting fresh"),
                Ok(n) => {
                    vss_restored_keys = n;
                    tracing::info!(keys_restored = n, "Restored node KV state from VSS");
                }
                Err(e) => {
                    if static_state.vss_allow_empty_restore {
                        tracing::warn!(
                            error = %e,
                            "VSS restore failed; starting fresh due to --vss-allow-empty-restore"
                        );
                    } else {
                        return Err(APIError::FailedVssInit(format!(
                            "VSS restore failed: {e}. Pass --vss-allow-empty-restore \
                             to start with an empty local state instead (UNSAFE if \
                             you previously had active channels)."
                        )));
                    }
                }
            }
        }

        (synced, monitor_kv_store)
    } else {
        let monitor_kv_store = Arc::new(RemoteFirstKvStore::new(Arc::clone(&local_kv_store), None));
        let synced = Arc::new(SyncedKvStore::local_only(local_kv_store));
        (synced, monitor_kv_store)
    };

    #[cfg(not(feature = "vss"))]
    let kv_store = Arc::new(SyncedKvStore::local_only(local_kv_store));

    #[cfg(feature = "vss")]
    let bp_kv_store: BpKvStore = Arc::new(crate::async_kv_store::BpKvStoreRouter::new(
        Arc::clone(&monitor_kv_store),
        bp_local_kv_store,
        Arc::clone(&kv_store),
    ));
    #[cfg(not(feature = "vss"))]
    let bp_kv_store: BpKvStore = KVStoreSyncWrapper(Arc::clone(&kv_store));

    // Sync config from database to KVStore
    sync_config_to_kvstore(&static_state.db(), kv_store.as_ref())?;

    let ldk_data_dir = static_state.ldk_data_dir.clone();
    let ldk_data_dir_path = PathBuf::from(&ldk_data_dir);
    let logger = static_state.logger.clone();
    let bitcoin_network = static_state.network;
    let network: Network = bitcoin_network.into();
    let ldk_peer_listening_port = static_state.ldk_peer_listening_port;

    // RGB setup
    let indexer_url = resolve_indexer_url(
        unlock_request.indexer_url.as_deref(),
        static_state.config.chain.indexer_url.as_deref(),
    )?;
    let indexer_protocol = check_indexer_url(indexer_url, bitcoin_network)?;
    tracing::info!(
        "Connected to an indexer with the {} protocol",
        indexer_protocol
    );
    let proxy_endpoint = if let Some(proxy_endpoint) = &unlock_request.proxy_endpoint {
        check_rgb_proxy_endpoint(proxy_endpoint).await?;
        tracing::info!("Using a custom proxy");
        proxy_endpoint
    } else {
        tracing::info!("Using the default proxy");
        match bitcoin_network {
            BitcoinNetwork::Signet
            | BitcoinNetwork::SignetCustom
            | BitcoinNetwork::Testnet
            | BitcoinNetwork::Testnet4
            | BitcoinNetwork::Mainnet => PROXY_ENDPOINT_PUBLIC,
            BitcoinNetwork::Regtest => PROXY_ENDPOINT_LOCAL,
        }
    };
    save_config(
        &app_state.db(),
        kv_store.as_ref(),
        CONFIG_INDEXER_URL,
        indexer_url,
    )?;
    save_config(
        &app_state.db(),
        kv_store.as_ref(),
        CONFIG_BITCOIN_NETWORK,
        &bitcoin_network.to_string(),
    )?;

    // Initialize the chain backend for the requested sync mode
    let handle = tokio::runtime::Handle::current();
    let ChainSetup {
        backend,
        fee_estimator,
        broadcaster,
        chain_filter,
        initial_best_block,
    } = match &unlock_request.ldk_chain_sync {
        #[cfg(feature = "block-sync")]
        LdkChainSync::BlockSync {
            bitcoind_rpc_username,
            bitcoind_rpc_password,
            bitcoind_rpc_host,
            bitcoind_rpc_port,
        } => {
            let bitcoind_client = match BitcoindClient::new(
                bitcoind_rpc_host.clone(),
                *bitcoind_rpc_port,
                bitcoind_rpc_username.clone(),
                bitcoind_rpc_password.clone(),
                handle.clone(),
                Arc::clone(&logger),
                static_state.config.chain.fee_refresh_interval_secs,
            )
            .await
            {
                Ok(client) => Arc::new(client),
                Err(e) => return Err(APIError::FailedBitcoindConnection(e.to_string())),
            };

            // Check that the bitcoind we've connected to is running the network we expect
            let bitcoind_chain = bitcoind_client.get_blockchain_info().await.chain;
            if bitcoind_chain
                != match bitcoin_network {
                    BitcoinNetwork::Mainnet => "main",
                    BitcoinNetwork::Testnet => "test",
                    BitcoinNetwork::Testnet4 => "testnet4",
                    BitcoinNetwork::Regtest => "regtest",
                    BitcoinNetwork::Signet | BitcoinNetwork::SignetCustom => "signet",
                }
            {
                return Err(APIError::NetworkMismatch(bitcoind_chain, bitcoin_network));
            }

            // Poll for the best chain tip, used by the channel manager & spv client
            let polled_chain_tip = init::validate_best_block_header(bitcoind_client.as_ref())
                .await
                .expect("Failed to fetch best block header and best block");
            let initial_best_block = polled_chain_tip.to_best_block();

            ChainSetup {
                fee_estimator: bitcoind_client.clone(),
                broadcaster: bitcoind_client.clone(),
                backend: ChainBackend::BlockSync {
                    client: bitcoind_client,
                    polled_chain_tip,
                },
                chain_filter: None,
                initial_best_block,
            }
        }
        #[cfg(feature = "transaction-sync")]
        LdkChainSync::TransactionSync {
            indexer_url: ln_indexer_url,
        } => {
            // LDK can sync against a different indexer than the RGB wallet, but when the two
            // match the URL has already been checked above
            let ln_indexer_protocol = if ln_indexer_url == indexer_url {
                indexer_protocol.clone()
            } else {
                check_indexer_url(ln_indexer_url, bitcoin_network)?
            };
            let indexer_client = Arc::new(
                IndexerClient::new(
                    ln_indexer_url.to_string(),
                    ln_indexer_protocol.clone(),
                    handle.clone(),
                    Arc::clone(&logger),
                    static_state.config.chain.indexer_timeout_secs,
                    static_state.config.chain.fee_refresh_interval_secs,
                )
                .map_err(|e| APIError::InvalidIndexer(e.to_string()))?,
            );
            let tx_sync = Arc::new(
                IndexerSyncClient::new(
                    ln_indexer_url.to_string(),
                    ln_indexer_protocol,
                    Arc::clone(&logger),
                )
                .map_err(|e| APIError::InvalidIndexer(e.to_string()))?,
            );
            let initial_best_block = indexer_client
                .get_best_block()
                .map_err(|e| APIError::InvalidIndexer(e.to_string()))?;

            let chain_filter: Arc<dyn Filter + Send + Sync> = tx_sync.clone();
            ChainSetup {
                fee_estimator: indexer_client.clone(),
                broadcaster: indexer_client.clone(),
                backend: ChainBackend::TransactionSync {
                    client: indexer_client,
                    tx_sync,
                },
                chain_filter: Some(chain_filter),
                initial_best_block,
            }
        }
    };

    // LDK signing: internal mode uses `KeysManager` from the mnemonic-derived LDK seed (BIP32 child
    // 535 of the master xpriv). External mode uses `ExternalSigner` only; inbound / peer_storage /
    // receive_auth key material comes from bootstrap hex fields (see `ExternalSigner::from_attachment`).
    let cur = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap();

    let keys_manager: ActiveSignerRef = if let Some(s) = external_signer.as_ref() {
        Arc::new(DynRlnSigner::from_external(Arc::clone(s)))
    } else {
        let mnemonic = internal_mnemonic
            .as_ref()
            .expect("internal mnemonic must be present when external signer is not configured");
        let ldk_seed: [u8; 32] = {
            let xkey: ExtendedKey = mnemonic
                .clone()
                .into_extended_key()
                .expect("a valid key should have been provided");
            let master_xprv = &xkey
                .into_xprv(network.into())
                .expect("should be possible to get an extended private key");
            let xprv: Xpriv = master_xprv
                .derive_priv(&Secp256k1_30::new(), &ChildNumber::Hardened { index: 535 })
                .unwrap();
            xprv.private_key.secret_bytes()
        };
        let internal_keys_manager = Arc::new(KeysManager::new(
            &ldk_seed,
            cur.as_secs(),
            cur.subsec_nanos(),
            true,
            ldk_data_dir_path.clone(),
            Arc::clone(&kv_store) as Arc<dyn KVStoreSync + Send + Sync>,
        ));
        Arc::new(DynRlnSigner::from_internal(internal_keys_manager))
    };
    // `entropy_source` (app APIs) and `ldk_entropy_source` (LDK wiring) always use OsRng.
    // When LDK passes `keys_manager` as `EntropySource`, external `DynRlnSigner` delegates to
    // `ExternalSigner` which uses the same system RNG — never the host `GetSecureRandomBytes` RPC.
    let entropy_source: Arc<dyn crate::signer::RlnEntropySource> = Arc::new(SystemEntropySource);
    let ldk_entropy_source = Arc::new(LightningEntropySource::new(Arc::clone(&entropy_source)));

    // Initialize the ChainMonitor — esplora threads tx_sync as the Filter source.
    //
    // With VSS: monitors persist remote-first via `MonitorUpdatingPersisterAsync`
    // + `ChainMonitor::new_async_beta` (a write returns `InProgress` and completes
    // once VSS durably acks). Without VSS: the original synchronous persister.
    #[cfg(feature = "vss")]
    let (chain_monitor, mut channelmonitors): (Arc<ChainMonitor>, _) = {
        let persister = MonitorUpdatingPersisterAsync::new(
            Arc::clone(&monitor_kv_store),
            TokioFutureSpawner,
            Arc::clone(&logger),
            1000,
            Arc::clone(&keys_manager),
            Arc::clone(&keys_manager),
            Arc::clone(&broadcaster),
            Arc::clone(&fee_estimator),
        );
        // Read before moving the persister into the ChainMonitor.
        let channelmonitors = persister
            .read_all_channel_monitors_with_updates()
            .await
            .unwrap();
        let chain_monitor = Arc::new(chainmonitor::ChainMonitor::new_async_beta(
            chain_filter.clone(),
            Arc::clone(&broadcaster),
            Arc::clone(&logger),
            Arc::clone(&fee_estimator),
            persister,
            Arc::clone(&keys_manager),
            // `peer_storage` is compiled out in this build (cfg never set), so the
            // key is ignored. Pass a placeholder rather than the signer's key —
            // external-signer mode panics on `get_peer_storage_key`.
            PeerStorageKey { inner: [0u8; 32] },
        ));
        (chain_monitor, channelmonitors)
    };

    #[cfg(not(feature = "vss"))]
    let (chain_monitor, mut channelmonitors): (Arc<ChainMonitor>, _) = {
        let persister = Arc::new(MonitorUpdatingPersister::new(
            Arc::clone(&kv_store),
            Arc::clone(&logger),
            1000,
            Arc::clone(&keys_manager),
            Arc::clone(&keys_manager),
            Arc::clone(&broadcaster),
            Arc::clone(&fee_estimator),
        ));
        let peer_storage_signer = Arc::clone(&keys_manager);
        let chain_monitor = Arc::new(chainmonitor::ChainMonitor::new_with_peer_storage_encryptor(
            chain_filter.clone(),
            Arc::clone(&broadcaster),
            Arc::clone(&logger),
            Arc::clone(&fee_estimator),
            Arc::clone(&persister),
            Arc::clone(&keys_manager),
            Arc::new(move |plaintext: Vec<u8>, random_bytes: [u8; 32]| {
                peer_storage_signer.encrypt_peer_storage_payload(plaintext, random_bytes)
            }),
        ));
        let channelmonitors = persister.read_all_channel_monitors_with_updates().unwrap();
        (chain_monitor, channelmonitors)
    };

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
        ldk_entropy_source.clone(),
        scorer.clone(),
        scoring_fee_params,
    ));
    let message_router = Arc::new(DefaultMessageRouter::new(
        Arc::clone(&network_graph),
        Arc::clone(&ldk_entropy_source),
    ));

    // Initialize the ChannelManager
    let channels_config = &static_state.config.channels;
    let mut user_config = UserConfig::default();
    user_config
        .channel_handshake_limits
        .force_announced_channel_preference = false;
    user_config.channel_handshake_limits.their_to_self_delay = channels_config.their_to_self_delay;
    user_config.channel_handshake_limits.max_minimum_depth = channels_config.max_minimum_depth;
    user_config
        .channel_handshake_config
        .negotiate_anchors_zero_fee_htlc_tx = true;
    user_config.channel_handshake_config.our_to_self_delay = channels_config.our_to_self_delay;
    user_config
        .channel_handshake_config
        .max_inbound_htlc_value_in_flight_percent_of_channel =
        channels_config.max_inbound_htlc_value_in_flight_percent;
    user_config.channel_handshake_config.our_max_accepted_htlcs =
        channels_config.our_max_accepted_htlcs;
    user_config.channel_config = channels_config.channel_config();
    // virtual channels are unannounced, so they require private forwarding
    user_config.accept_forwards_to_priv_channels =
        channels_config.accept_forwards_to_priv_channels || static_state.enable_virtual_channels_v0;
    user_config.manually_accept_inbound_channels = true;
    let persisted_manager = kv_store.read(
        CHANNEL_MANAGER_PERSISTENCE_PRIMARY_NAMESPACE,
        CHANNEL_MANAGER_PERSISTENCE_SECONDARY_NAMESPACE,
        CHANNEL_MANAGER_PERSISTENCE_KEY,
    );
    // `restarting_node` and `channel_manager_blockhash` are only consumed by the block-sync
    // restart path
    #[cfg_attr(not(feature = "block-sync"), allow(unused_variables))]
    let restarting_node = persisted_manager.is_ok();
    #[cfg_attr(not(feature = "block-sync"), allow(unused_variables))]
    let (channel_manager_blockhash, channel_manager) = {
        match persisted_manager {
            Ok(bytes) => {
                let mut channel_monitor_references = Vec::new();
                for (_, channel_monitor) in channelmonitors.iter() {
                    channel_monitor_references.push(channel_monitor);
                }
                let read_args = ChannelManagerReadArgs::new(
                    ldk_entropy_source.clone(),
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
                    Arc::clone(&kv_store) as Arc<dyn KVStoreSync + Send + Sync>,
                );
                match <(BlockHash, ChannelManager)>::read(&mut &bytes[..], read_args) {
                    Ok(read) => read,
                    Err(e) => {
                        return Err(APIError::FailedLoadingChannelState(format!(
                            "cannot deserialize the channel manager ({e:?}); the persisted \
                             channel state is incomplete or incompatible, a missing channel \
                             monitor is the most common cause (see the LDK logs)"
                        )))
                    }
                }
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                // We're starting a fresh node.
                let polled_best_block = initial_best_block;
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
                    ldk_entropy_source.clone(),
                    keys_manager.clone(),
                    keys_manager.clone(),
                    user_config,
                    chain_params,
                    cur.as_secs() as u32,
                    ldk_data_dir_path.clone(),
                    Arc::clone(&kv_store) as Arc<dyn KVStoreSync + Send + Sync>,
                );
                (polled_best_block_hash, fresh_channel_manager)
            }
            Err(e) => {
                panic!("Failed to read channel manager from KVStore: {e}");
            }
        }
    };

    // A restored manager lagging a still-open monitor (it reports
    // `Balance::ClaimableOnChannelClose`) would force-close on load; refuse
    // before anything watches monitors or broadcasts.
    #[cfg(feature = "vss")]
    if vss_restored_keys > 0 {
        use lightning::chain::channelmonitor::Balance;
        use std::collections::HashSet;
        let manager_channel_ids: HashSet<ChannelId> = channel_manager
            .list_channels()
            .iter()
            .map(|c| c.channel_id)
            .collect();
        let lost_channels: Vec<String> = channelmonitors
            .iter()
            .filter(|(_, m)| !manager_channel_ids.contains(&m.channel_id()))
            .filter(|(_, m)| {
                m.get_claimable_balances()
                    .iter()
                    .any(|b| matches!(b, Balance::ClaimableOnChannelClose { .. }))
            })
            .map(|(_, m)| m.channel_id().to_string())
            .collect();
        if !lost_channels.is_empty() {
            if static_state.vss_allow_empty_restore {
                tracing::warn!(
                    channels = ?lost_channels,
                    "restored channel manager lags the restored monitors; proceeding due to \
                     --vss-allow-empty-restore — these channels WILL be force-closed"
                );
            } else {
                // Drop the restored manager so the next unlock re-runs restore + guard.
                if let Err(e) = kv_store.remove_local_only(
                    CHANNEL_MANAGER_PERSISTENCE_PRIMARY_NAMESPACE,
                    CHANNEL_MANAGER_PERSISTENCE_SECONDARY_NAMESPACE,
                    CHANNEL_MANAGER_PERSISTENCE_KEY,
                ) {
                    tracing::warn!(error = %e, "failed to drop restored channel manager key");
                }
                return Err(APIError::FailedVssInit(format!(
                    "VSS restore is inconsistent: the restored channel manager does not know \
                     channel(s) {lost_channels:?} that the restored channel monitors consider \
                     open. Unlocking would force-close them. Pass --vss-allow-empty-restore \
                     to proceed anyway and accept the force-close."
                )));
            }
        }
    }

    // Prepare the RGB wallet
    let (account_xpub_vanilla, account_xpub_colored, master_fingerprint, rgb_wallet_mnemonic) =
        if external_signer_mode {
            let bootstrap = external_bootstrap.clone().ok_or_else(|| {
                APIError::ExternalSignerProtocolError(
                    "missing external bootstrap in external mode".to_string(),
                )
            })?;
            (
                bootstrap.identity.account_xpub_vanilla,
                bootstrap.identity.account_xpub_colored,
                bootstrap.identity.master_fingerprint,
                None,
            )
        } else {
            let mnemonic_str = internal_mnemonic
                .as_ref()
                .ok_or_else(|| {
                    APIError::ExternalSignerProtocolError(
                        "missing internal mnemonic in internal mode".to_string(),
                    )
                })?
                .to_string();
            let (_, account_xpub_vanilla, _) = get_account_data(
                &bitcoin_network,
                &mnemonic_str,
                false,
                WitnessVersion::Taproot,
            )
            .unwrap();
            let (_, account_xpub_colored, master_fingerprint) = get_account_data(
                &bitcoin_network,
                &mnemonic_str,
                true,
                WitnessVersion::Taproot,
            )
            .unwrap();
            (
                account_xpub_vanilla.to_string(),
                account_xpub_colored.to_string(),
                master_fingerprint.to_string(),
                Some(mnemonic_str.clone()),
            )
        };
    let data_dir = static_state
        .storage_dir_path
        .clone()
        .to_string_lossy()
        .to_string();

    // Pull the RGB wallet down from VSS before constructing it locally, when
    // VSS is configured and the local wallet directory for this mnemonic's
    // fingerprint is absent. Mirrors the KV-side auto-restore at the top of
    // this function — together they make `unlock` recover the full node
    // state (channels + assets + on-chain) on a fresh device.
    #[cfg(feature = "vss")]
    if let (Some(ref vss_url), Some(ref identity)) = (&static_state.vss_url, &vss_identity) {
        let rgb_store_id = format!("{}_rgb", identity.pubkey_hex);
        maybe_restore_rgb_from_vss(
            vss_url,
            rgb_store_id,
            identity.signing_key,
            &static_state.storage_dir_path,
            &master_fingerprint.to_string(),
            static_state.vss_allow_empty_restore,
        )
        .await?;
    }

    let keys = SinglesigKeys {
        account_xpub_vanilla: account_xpub_vanilla.clone(),
        account_xpub_colored: account_xpub_colored.clone(),
        vanilla_keychain: None,
        master_fingerprint: master_fingerprint.clone(),
        mnemonic: rgb_wallet_mnemonic,
        witness_version: WitnessVersion::Taproot,
    };
    let reuse_addresses = static_state.reuse_addresses;
    let indexer_url_owned = indexer_url.to_string();
    #[cfg(feature = "vss")]
    let rgb_vss_backup = match (&static_state.vss_url, &vss_identity) {
        (Some(vss_url), Some(identity)) => Some((
            vss_url.clone(),
            format!("{}_rgb", identity.pubkey_hex),
            identity.signing_key,
        )),
        _ => None,
    };
    // go_online and configure_vss_backup drive blocking rgb-lib HTTP clients;
    // run them off the async runtime so they don't fail on a single-vCPU host.
    let (rgb_wallet, rgb_online, deferred_rgb_consistency_check) =
        tokio::task::spawn_blocking(move || {
            let mut rgb_wallet = RgbLibWallet::new(
                WalletData {
                    data_dir,
                    bitcoin_network,
                    database_type: DatabaseType::Sqlite,
                    max_allocations_per_utxo: 1,
                    supported_schemas: supported_asset_schemas(bitcoin_network),
                    reuse_addresses,
                },
                keys,
            )
            .expect("valid rgb-lib wallet");
            let deferred_rgb_consistency_check = rgb_wallet.pending_rgb_acceptance()?.is_some();
            let rgb_online = rgb_wallet.go_online(OnlineOptions {
                indexer_url: indexer_url_owned,
                skip_consistency_check: deferred_rgb_consistency_check,
                vanilla_sync_lookback: 20,
            })?;
            if deferred_rgb_consistency_check {
                tracing::info!(
                    "deferred RGB consistency check until durable funding recovery completes"
                );
            }
            #[cfg(feature = "vss")]
            if let Some((vss_url, rgb_store_id, signing_key)) = rgb_vss_backup {
                let vss_config =
                    rgb_lib::wallet::vss::VssBackupConfig::new(vss_url, rgb_store_id, signing_key)
                        .with_encryption(true)
                        .with_auto_backup(true)
                        .with_backup_mode(rgb_lib::wallet::vss::VssBackupMode::Blocking);
                // Fail closed: a misconfigured backup must not silently run local-only.
                rgb_wallet.configure_vss_backup(vss_config).map_err(|e| {
                    APIError::FailedVssInit(format!(
                        "Failed to configure VSS backup for RGB wallet: {e}"
                    ))
                })?;
                tracing::info!("VSS auto-backup (blocking) enabled for RGB wallet");
            }
            Ok::<_, APIError>((rgb_wallet, rgb_online, deferred_rgb_consistency_check))
        })
        .await
        .map_err(|e| APIError::Unexpected(format!("rgb-lib wallet setup task failed: {e}")))??;
    save_config(
        &static_state.db(),
        kv_store.as_ref(),
        CONFIG_WALLET_FINGERPRINT,
        &master_fingerprint,
    )?;
    save_config(
        &static_state.db(),
        kv_store.as_ref(),
        CONFIG_WALLET_ACCOUNT_XPUB_COLORED,
        &account_xpub_colored,
    )?;
    save_config(
        &static_state.db(),
        kv_store.as_ref(),
        CONFIG_WALLET_ACCOUNT_XPUB_VANILLA,
        &account_xpub_vanilla,
    )?;
    save_config(
        &static_state.db(),
        kv_store.as_ref(),
        CONFIG_WALLET_MASTER_FINGERPRINT,
        &master_fingerprint,
    )?;

    // No second VssBackupClient is constructed here: the manual /vssbackup
    // and /vssbackupinfo routes use the wallet's own client, retrievable via
    // `wallet.vss_client()` (R-lib.1 in rgb-lib's PR #31). Keeping a single
    // client per stream avoids running two tokio runtimes for the same
    // backups and removes the race between the two clients writing
    // overlapping state.
    let rgb_wallet_wrapper = Arc::new(RgbLibWalletWrapper::new(
        Arc::new(Mutex::new(rgb_wallet)),
        rgb_online,
    ));
    let rgb_funding_recovery_guard = Arc::new(RgbFundingRecoveryGuard::default());
    let rgb_change_destination_source = Arc::new(RgbChangeDestinationSource {
        inner: Arc::clone(&rgb_wallet_wrapper),
        funding_guard: Arc::clone(&rgb_funding_recovery_guard),
    });

    reimport_funding_consignments(&rgb_wallet_wrapper, &kv_store, &ldk_data_dir).await;

    // Initialize the OutputSweeper.
    let txes: OutputSpenderTxes = match kv_store.read("", "", OUTPUT_SPENDER_TXES_KEY) {
        Ok(bytes) => OutputSpenderTxes::read(&mut &bytes[..]).unwrap_or_else(|_| new_hash_map()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => new_hash_map(),
        Err(e) => panic!("Failed to read output spender txes from KVStore: {e}"),
    };
    let txes = Arc::new(Mutex::new(txes));
    let signer_for_output_spender: Arc<dyn RlnKeysInterface<EcdsaSigner = DynRlnChannelSigner>> =
        keys_manager.clone();
    let rgb_output_spender = Arc::new(RgbOutputSpender {
        static_state: static_state.clone(),
        rgb_wallet_wrapper: rgb_wallet_wrapper.clone(),
        signer: signer_for_output_spender,
        kv_store: kv_store.clone(),
        txes,
        sweep_recipients: Arc::new(Mutex::new(HashMap::new())),
        rgb_funding_recovery_guard: Arc::clone(&rgb_funding_recovery_guard),
    });
    // `sweeper_best_block` is only used by the block-sync restart path.
    #[cfg_attr(not(feature = "block-sync"), allow(unused_variables))]
    let (sweeper_best_block, output_sweeper) = match kv_store.read(
        OUTPUT_SWEEPER_PERSISTENCE_PRIMARY_NAMESPACE,
        OUTPUT_SWEEPER_PERSISTENCE_SECONDARY_NAMESPACE,
        OUTPUT_SWEEPER_PERSISTENCE_KEY,
    ) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            let sweeper = OutputSweeper::new(
                channel_manager.current_best_block(),
                broadcaster.clone(),
                fee_estimator.clone(),
                chain_filter.clone(),
                rgb_output_spender,
                Arc::clone(&rgb_change_destination_source),
                Clone::clone(&bp_kv_store),
                logger.clone(),
            );
            (channel_manager.current_best_block(), sweeper)
        }
        Ok(mut bytes) => {
            let read_args = (
                broadcaster.clone(),
                fee_estimator.clone(),
                chain_filter.clone(),
                rgb_output_spender.clone(),
                Arc::clone(&rgb_change_destination_source),
                Clone::clone(&bp_kv_store),
                logger.clone(),
            );
            let mut reader = io::Cursor::new(&mut bytes);
            <(BestBlock, OutputSweeper)>::read(&mut reader, read_args)
                .expect("Failed to deserialize OutputSweeper")
        }
        Err(e) => panic!("Failed to read OutputSweeper with {e}"),
    };

    // Sync ChannelMonitors, ChannelManager and OutputSweeper to chain tip.
    // block-sync replays blocks from bitcoind before the SPV client takes over, while
    // transaction-sync relies on the indexer via the `Confirm` interface.
    let mut chain_listener_channel_monitors = Vec::new();
    #[cfg(feature = "block-sync")]
    let mut block_sync_cache = UnboundedCache::new();
    // with only block-sync built this is always set below, hence the allow
    #[cfg(feature = "block-sync")]
    #[cfg_attr(not(feature = "transaction-sync"), allow(unused_assignments))]
    let mut block_sync_chain_tip: Option<lightning_block_sync::poll::ValidatedBlockHeader> = None;

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

    match &backend {
        #[cfg(feature = "block-sync")]
        ChainBackend::BlockSync {
            client,
            polled_chain_tip,
        } => {
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
                for monitor_listener_info in chain_listener_channel_monitors.iter_mut() {
                    chain_listeners.push((
                        monitor_listener_info.0,
                        &monitor_listener_info.1 as &(dyn chain::Listen + Send + Sync),
                    ));
                }
                let mut attempts = 3;
                loop {
                    match init::synchronize_listeners(
                        client.as_ref(),
                        network,
                        &mut block_sync_cache,
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
                *polled_chain_tip
            };
            block_sync_chain_tip = Some(chain_tip);
        }
        #[cfg(feature = "transaction-sync")]
        ChainBackend::TransactionSync { .. } => {}
    }

    // Give ChannelMonitors to ChainMonitor
    for (_, (channel_monitor, _, _, _), _) in chain_listener_channel_monitors {
        let channel_id = channel_monitor.channel_id();
        assert_eq!(
            chain_monitor.load_existing_monitor(channel_id, channel_monitor),
            Ok(ChannelMonitorUpdateStatus::Completed)
        );
    }

    // Build the gossip source from the operator's choice (defaults to P2P).
    let gossip_source = Arc::new(match gossip_source_config {
        GossipSourceConfig::P2PNetwork => {
            GossipSource::new_p2p(Arc::clone(&network_graph), None, Arc::clone(&logger))
        }
        GossipSourceConfig::RapidGossipSync { server_url } => {
            let latest_sync_timestamp = network_graph
                .get_last_rapid_gossip_sync_timestamp()
                .unwrap_or(0);
            GossipSource::new_rgs(
                server_url,
                latest_sync_timestamp,
                Arc::clone(&network_graph),
                Arc::clone(&logger),
                crate::gossip::RgsTuning {
                    connect_timeout_secs: static_state.config.gossip.rgs_connect_timeout_secs,
                    sync_timeout_secs: static_state.config.gossip.rgs_sync_timeout_secs,
                    snapshot_max_size: (static_state.config.gossip.rgs_snapshot_max_size_mb
                        as usize)
                        * 1024
                        * 1024,
                },
            )
        }
    });

    // The UTXO verifier can only attach to a P2P sync, and only after PeerManager
    // is built (the verifier holds an Arc<PeerManager>). RGS mode skips it.
    let (p2p_gossip_sync_for_verifier, route_handler): (
        Option<Arc<P2PGossipSync>>,
        Arc<RoutingMessageHandler>,
    ) = match &*gossip_source {
        GossipSource::P2PNetwork { gossip_sync } => (
            Some(Arc::clone(gossip_sync)),
            Arc::clone(gossip_sync) as Arc<RoutingMessageHandler>,
        ),
        GossipSource::RapidGossipSync { .. } => (
            None,
            Arc::new(IgnoringMessageHandler {}) as Arc<RoutingMessageHandler>,
        ),
    };

    // Initialize an OMDomainResolver as a service to other nodes.
    // As a service to other LDK users, using an `OMDomainResolver` allows others to resolve BIP
    // 353 Human Readable Names for others, providing them DNSSEC proofs over lightning onion
    // messages. Doing this only makes sense for an always-online public routing node, and doesn't
    // provide you any direct value, but it's nice to offer the service for others.
    let channel_manager: Arc<ChannelManager> = Arc::new(channel_manager);
    {
        let recovery_channel_manager = Arc::clone(&channel_manager);
        let recovery_wallet = Arc::clone(&rgb_wallet_wrapper);
        let recovery_kv_store = Arc::clone(&kv_store);
        let recovery_operation_guard = Arc::clone(&rgb_funding_recovery_guard);
        let recovery_indexer_url = indexer_url.to_owned();
        let unresolved = tokio::task::spawn_blocking(move || {
            let _operation = recovery_operation_guard.blocking_lock_operation();
            let (recovered_receivers, mut unresolved_receivers) = reconcile_rgb_receiver_funding(
                recovery_channel_manager.as_ref(),
                recovery_wallet.as_ref(),
                recovery_kv_store.as_ref(),
            )?;
            if recovered_receivers > 0 {
                tracing::info!(
                    recovered_receivers,
                    "completed durable RGB receiver recovery before peer startup"
                );
            }
            let mut unresolved = reconcile_rgb_sender_funding(
                recovery_channel_manager.as_ref(),
                recovery_wallet.as_ref(),
                recovery_kv_store.as_ref(),
            )?;
            unresolved.append(&mut unresolved_receivers);
            unresolved.sort_by(|a, b| {
                a.funding_txid
                    .cmp(&b.funding_txid)
                    .then_with(|| a.stage.sort_key().cmp(&b.stage.sort_key()))
            });
            let pending_stock = recovery_wallet.pending_funding_fascia()?;
            let should_check_consistency = should_complete_deferred_rgb_consistency_check(
                deferred_rgb_consistency_check,
                pending_stock
                    .as_ref()
                    .map(|(operation_id, _)| operation_id.as_str()),
                &unresolved,
            )?;
            if should_check_consistency {
                recovery_wallet.complete_deferred_consistency_check(recovery_indexer_url, 20)?;
                tracing::info!("completed deferred RGB consistency check after funding recovery");
            }
            Ok::<_, RgbLibError>(unresolved)
        })
        .await
        .map_err(|error| {
            APIError::Unexpected(format!("RGB funding recovery task failed: {error}"))
        })?
        .map_err(|error| APIError::Unexpected(format!("RGB funding recovery failed: {error}")))?;
        rgb_funding_recovery_guard.replace(&unresolved);
        if !unresolved.is_empty() {
            tracing::error!(
                funding_txids = ?unresolved
                    .iter()
                    .map(|recovery| recovery.funding_txid.as_str())
                    .collect::<Vec<_>>(),
                "RGB wallet mutations are quarantined pending funding recovery"
            );
        }
    }
    let resolver = "8.8.8.8:53".to_socket_addrs().unwrap().next().unwrap();
    let domain_resolver = Arc::new(OMDomainResolver::new(
        resolver,
        Some(Arc::clone(&channel_manager)),
    ));

    // Initialize the PeerManager
    let onion_messenger: Arc<OnionMessenger> = Arc::new(LdkOnionMessenger::new(
        Arc::clone(&ldk_entropy_source),
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

    let live_channel_access = Arc::new(LiveChannelAccess::new(channel_manager.clone()));
    let async_order_handler = match static_state.lsp_base_url.as_ref() {
        Some(lsp_base_url) => Arc::new(AsyncOrderMessageHandler::new_with_lsp_client(
            live_channel_access.clone(),
            lsp_base_url.clone(),
            static_state.lsp_bearer_token.clone(),
            Handle::current(),
            static_state.config.lsp.request_timeout_secs,
        )),
        None => Arc::new(AsyncOrderMessageHandler::new(live_channel_access.clone())),
    };
    let asset_link_handler = Arc::new(AssetLinkMessageHandler::new(live_channel_access));
    let max_aggregated_media_size_per_channel_mb =
        static_state.max_aggregated_media_size_per_channel_mb as usize * 1024 * 1024;
    let rgb_file_transfer_handler: Arc<RgbFileTransferHandler> =
        Arc::new(RgbFileTransferHandler::new(
            ldk_data_dir_path.clone(),
            Arc::clone(&channel_manager) as Arc<dyn PeerChannelGate>,
            static_state.max_pending_consignments,
            max_aggregated_media_size_per_channel_mb,
            static_state.max_media_files_per_channel,
        ));
    rgb_file_transfer_handler.cleanup_orphans_from_previous_run();
    let custom_messenger = Arc::new(CustomMessenger {
        async_order: Arc::clone(&async_order_handler),
        asset_link: Arc::clone(&asset_link_handler),
        rgb_file_transfer: Arc::clone(&rgb_file_transfer_handler),
    });
    let async_payments_preimage_root = Arc::new(
        match internal_mnemonic.as_ref() {
            Some(mnemonic) => AsyncPaymentsPreimageRoot::build_from_mnemonic(
                mnemonic,
                network,
                &channel_manager.get_our_node_id(),
            ),
            None => {
                let bootstrap = external_bootstrap.as_ref().expect("external bootstrap");
                let seed = crate::signer::types::derive_async_payments_compat_seed_from_bootstrap(
                    bootstrap,
                );
                AsyncPaymentsPreimageRoot::build_from_seed(
                    &seed,
                    network,
                    &channel_manager.get_our_node_id(),
                )
            }
        }
        .map_err(|err| APIError::Unexpected(err.message))?,
    );

    let lightning_msg_handler = MessageHandler {
        chan_handler: channel_manager.clone(),
        route_handler: Arc::clone(&route_handler),
        onion_message_handler: onion_messenger.clone(),
        custom_message_handler: Arc::clone(&custom_messenger),
        send_only_message_handler: Arc::clone(&chain_monitor),
    };
    let peer_manager: Arc<PeerManager> = Arc::new(PeerManager::new(
        lightning_msg_handler,
        current_time.try_into().unwrap(),
        &ephemeral_bytes,
        logger.clone(),
        Arc::clone(&keys_manager),
    ));

    // The UTXO lookup can only attach to a P2P sync; RGS mode skips it. Both chain backends
    // provide a verifier, so announcements are checked whatever the sync mode is.
    if let Some(p2p) = &p2p_gossip_sync_for_verifier {
        let peer_manager_wake = Arc::new({
            let peer_manager = Arc::clone(&peer_manager);
            move || peer_manager.process_events()
        });
        let utxo_lookup: Arc<dyn UtxoLookup + Send + Sync> = match &backend {
            #[cfg(feature = "block-sync")]
            ChainBackend::BlockSync { client, .. } => Arc::new(BlockSyncGossipVerifier::new(
                Arc::clone(&client.bitcoind_rpc_client),
                Arc::clone(p2p),
                peer_manager_wake,
                handle.clone(),
            )),
            #[cfg(feature = "transaction-sync")]
            ChainBackend::TransactionSync { client, .. } => Arc::new(IndexerGossipVerifier::new(
                Arc::clone(client),
                Arc::clone(p2p),
                peer_manager_wake,
            )),
        };
        p2p.add_utxo_lookup(Some(utxo_lookup));
    }

    // ## Running LDK
    // Initialize networking

    let peer_manager_connection_handler = peer_manager.clone();
    let listening_port = ldk_peer_listening_port;
    let stop_processing = Arc::new(AtomicBool::new(false));
    let stop_listen = Arc::clone(&stop_processing);
    tokio::spawn(async move {
        // Dual-stack when available; hosts with IPv6 disabled fall back to IPv4.
        let listener = crate::utils::bind_first_available(&[
            format!("[::]:{listening_port}"),
            format!("0.0.0.0:{listening_port}"),
        ])
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
    let stop_listen = Arc::clone(&stop_processing);
    match backend {
        #[cfg(feature = "block-sync")]
        ChainBackend::BlockSync { client, .. } => {
            let channel_manager_listener = channel_manager.clone();
            let chain_monitor_listener = chain_monitor.clone();
            let output_sweeper_listener = output_sweeper.clone();
            let chain_tip =
                block_sync_chain_tip.expect("block-sync chain tip is set while syncing listeners");
            let mut cache = block_sync_cache;
            tokio::spawn(async move {
                let chain_poller = poll::ChainPoller::new(client.as_ref(), network);
                let chain_listener = (
                    chain_monitor_listener,
                    &(channel_manager_listener, output_sweeper_listener),
                );
                let mut spv_client =
                    SpvClient::new(chain_tip, chain_poller, &mut cache, &chain_listener);
                loop {
                    if stop_listen.load(Ordering::Acquire) {
                        return;
                    }
                    if let Err(e) = spv_client.poll_best_tip().await {
                        tracing::error!("Error while polling best tip: {:?}", e);
                    }
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
            });
        }
        #[cfg(feature = "transaction-sync")]
        ChainBackend::TransactionSync { tx_sync, .. } => {
            let confirmables: Vec<Arc<dyn Confirm + Send + Sync>> = vec![
                channel_manager.clone(),
                chain_monitor.clone(),
                output_sweeper.clone(),
            ];
            // bring everything up to the current tip before starting to serve
            sync_chain_data(tx_sync.clone(), confirmables.clone())
                .await
                .map_err(|e| APIError::InvalidIndexer(e.to_string()))?;
            tokio::spawn(async move {
                loop {
                    if stop_listen.load(Ordering::Acquire) {
                        return;
                    }
                    if let Err(e) = sync_chain_data(tx_sync.clone(), confirmables.clone()).await {
                        tracing::error!("Error while syncing via indexer: {:?}", e);
                    }
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
            });
        }
    }

    // Read payment info from KVStore
    let inbound_payments = Arc::new(Mutex::new({
        match kv_store.read("", "", INBOUND_PAYMENTS_KEY) {
            Ok(bytes) => InboundPaymentInfoStorage::read(&mut &bytes[..]).unwrap_or_else(|_| {
                InboundPaymentInfoStorage {
                    payments: new_hash_map(),
                }
            }),
            Err(e) if e.kind() == io::ErrorKind::NotFound => InboundPaymentInfoStorage {
                payments: new_hash_map(),
            },
            Err(e) => panic!("Failed to read inbound payments from KVStore: {e}"),
        }
    }));
    let outbound_payments = Arc::new(Mutex::new({
        match kv_store.read("", "", OUTBOUND_PAYMENTS_KEY) {
            Ok(bytes) => OutboundPaymentInfoStorage::read(&mut &bytes[..]).unwrap_or_else(|_| {
                OutboundPaymentInfoStorage {
                    payments: new_hash_map(),
                }
            }),
            Err(e) if e.kind() == io::ErrorKind::NotFound => OutboundPaymentInfoStorage {
                payments: new_hash_map(),
            },
            Err(e) => panic!("Failed to read outbound payments from KVStore: {e}"),
        }
    }));

    // Seed the shared payment-index counter and backfill any records persisted
    // before payment indexing existed. Missing indices are assigned
    // deterministically by (created_at, payment hash/id) so the ordering is
    // stable across restarts.
    let next_payment_idx = {
        let mut inbound_g = inbound_payments.lock().unwrap();
        let mut outbound_g = outbound_payments.lock().unwrap();

        let mut max_idx = 0u64;
        for info in inbound_g
            .payments
            .values()
            .chain(outbound_g.payments.values())
        {
            if let Some(i) = info.payment_idx {
                max_idx = max_idx.max(i);
            }
        }

        let mut missing: Vec<(u64, [u8; 32], bool)> = Vec::new();
        for (h, info) in inbound_g.payments.iter() {
            if info.payment_idx.is_none() {
                missing.push((info.created_at, h.0, true));
            }
        }
        for (id, info) in outbound_g.payments.iter() {
            if info.payment_idx.is_none() {
                missing.push((info.created_at, id.0, false));
            }
        }
        missing.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));

        let mut changed = false;
        for (_, key, is_inbound) in missing {
            max_idx += 1;
            if is_inbound {
                inbound_g
                    .payments
                    .get_mut(&PaymentHash(key))
                    .unwrap()
                    .payment_idx = Some(max_idx);
            } else {
                outbound_g
                    .payments
                    .get_mut(&PaymentId(key))
                    .unwrap()
                    .payment_idx = Some(max_idx);
            }
            changed = true;
        }
        if changed {
            kv_store
                .write("", "", INBOUND_PAYMENTS_KEY, inbound_g.encode())
                .unwrap();
            kv_store
                .write("", "", OUTBOUND_PAYMENTS_KEY, outbound_g.encode())
                .unwrap();
        }
        Arc::new(std::sync::atomic::AtomicU64::new(max_idx + 1))
    };

    let bump_wallet_source = Arc::new(RgbBumpWalletSource {
        inner: rgb_wallet_wrapper.clone(),
        signer: keys_manager.clone(),
        external_signer: external_signer.clone(),
        external_signer_mode,
    });
    let bump_tx_event_handler = Arc::new(BumpTransactionEventHandler::new(
        Arc::clone(&broadcaster),
        Arc::new(Wallet::new(bump_wallet_source, Arc::clone(&logger))),
        Arc::clone(&keys_manager),
        Arc::clone(&logger),
    ));

    // Persist ChannelManager (remote-first with VSS), NetworkGraph and scorer.
    let persister = Clone::clone(&bp_kv_store);

    // Read swaps info from KVStore
    let maker_swaps = Arc::new(Mutex::new({
        match kv_store.read("", "", MAKER_SWAPS_KEY) {
            Ok(bytes) => SwapMap::read(&mut &bytes[..]).unwrap_or_else(|_| SwapMap {
                swaps: new_hash_map(),
            }),
            Err(e) if e.kind() == io::ErrorKind::NotFound => SwapMap {
                swaps: new_hash_map(),
            },
            Err(e) => panic!("Failed to read maker swaps from KVStore: {e}"),
        }
    }));
    let taker_swaps = Arc::new(Mutex::new({
        match kv_store.read("", "", TAKER_SWAPS_KEY) {
            Ok(bytes) => SwapMap::read(&mut &bytes[..]).unwrap_or_else(|_| SwapMap {
                swaps: new_hash_map(),
            }),
            Err(e) if e.kind() == io::ErrorKind::NotFound => SwapMap {
                swaps: new_hash_map(),
            },
            Err(e) => panic!("Failed to read taker swaps from KVStore: {e}"),
        }
    }));

    // Read channel IDs info from KVStore
    let channel_ids_map = Arc::new(Mutex::new({
        match kv_store.read("", "", CHANNEL_IDS_KEY) {
            Ok(bytes) => ChannelIdsMap::read(&mut &bytes[..]).unwrap_or_else(|_| ChannelIdsMap {
                channel_ids: new_hash_map(),
            }),
            Err(e) if e.kind() == io::ErrorKind::NotFound => ChannelIdsMap {
                channel_ids: new_hash_map(),
            },
            Err(e) => panic!("Failed to read channel IDs from KVStore: {e}"),
        }
    }));

    let virtual_channel_draft_store = Arc::new(Mutex::new({
        match kv_store.read("", "", VIRTUAL_CHANNEL_DRAFTS_KEY) {
            Ok(bytes) => VirtualChannelDraftStore::read(&mut &bytes[..]).unwrap_or_else(|_| {
                VirtualChannelDraftStore {
                    entries: new_hash_map(),
                }
            }),
            Err(e) if e.kind() == io::ErrorKind::NotFound => VirtualChannelDraftStore {
                entries: new_hash_map(),
            },
            Err(e) => panic!("Failed to read virtual channel drafts from KVStore: {e}"),
        }
    }));

    let virtual_channel_session_store = Arc::new(Mutex::new({
        match kv_store.read("", "", VIRTUAL_CHANNEL_SESSIONS_KEY) {
            Ok(bytes) => VirtualChannelSessionStore::read(&mut &bytes[..]).unwrap_or_else(|_| {
                VirtualChannelSessionStore {
                    entries: new_hash_map(),
                }
            }),
            Err(e) if e.kind() == io::ErrorKind::NotFound => VirtualChannelSessionStore {
                entries: new_hash_map(),
            },
            Err(e) => panic!("Failed to read virtual channel sessions from KVStore: {e}"),
        }
    }));

    {
        let sessions = virtual_channel_session_store.lock().unwrap();
        for channel_id in sessions.entries.keys() {
            let marker_key = format!("virtual_channel_{}", channel_id);
            if kv_store.read("", "", &marker_key).is_err() {
                kv_store
                    .write("", "", &marker_key, vec![])
                    .expect("able to recover virtual channel marker");
            }
        }
    }

    async_order_handler.set_invoice_provider(Arc::new(AsyncOrderRecipientInvoiceProvider {
        config: static_state.config.clone(),
        channel_manager: Arc::clone(&channel_manager),
        inbound_payments: Arc::clone(&inbound_payments),
        async_payments_preimage_root: Arc::clone(&async_payments_preimage_root),
        kv_store: Arc::clone(&kv_store),
        next_payment_idx: Arc::clone(&next_payment_idx),
        external_signer_mode,
        external_signer: external_signer.clone(),
    }));

    let unlocked_state = Arc::new(UnlockedAppState {
        config: static_state.config.clone(),
        channel_manager: Arc::clone(&channel_manager),
        gossip_source: Arc::clone(&gossip_source),
        inbound_payments,
        signer: keys_manager,
        entropy_source,
        network_graph,
        chain_monitor: chain_monitor.clone(),
        onion_messenger: onion_messenger.clone(),
        outbound_payments,
        peer_manager: Arc::clone(&peer_manager),
        async_order_handler,
        asset_link_handler: Arc::clone(&asset_link_handler),
        async_payments_preimage_root,
        kv_store: Arc::clone(&kv_store),
        #[cfg(feature = "vss")]
        monitor_kv_store: Arc::clone(&monitor_kv_store),
        rgb_file_transfer_handler: Arc::clone(&rgb_file_transfer_handler),
        bump_tx_event_handler,
        rgb_wallet_wrapper,
        maker_swaps,
        taker_swaps: Arc::clone(&taker_swaps),
        router: Arc::clone(&router),
        output_sweeper: Arc::clone(&output_sweeper),
        channel_ids_map,
        proxy_endpoint: proxy_endpoint.to_string(),
        external_signer_mode,
        external_signer,
        external_node_id,
        virtual_channel_draft_store,
        virtual_channel_session_store,
        next_payment_idx,
        rgb_funding_recovery_guard,
    });

    asset_link_handler.set_authorizer(Arc::new(NodeAssetLinkAuthorizer {
        unlocked_state_weak: Arc::downgrade(&unlocked_state),
        channel_manager: Arc::clone(&channel_manager),
        kv_store: Arc::clone(&kv_store),
        taker_swaps: Arc::clone(&taker_swaps),
    }));

    // Refresh the RGS snapshot on a fixed interval (RGS mode only). The first
    // tick fires immediately, so a freshly unlocked node syncs right away.
    let gossip_shutdown = Arc::new(tokio::sync::Notify::new());
    if unlocked_state.gossip_source.is_rgs() {
        tokio::spawn(crate::gossip::run_rgs_sync_loop(
            Arc::clone(&unlocked_state.gossip_source),
            Arc::clone(&gossip_shutdown),
            Duration::from_secs(static_state.config.gossip.rgs_sync_interval_secs),
        ));
    }

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

    // Background Processing
    let (bp_exit, bp_exit_check) = tokio::sync::watch::channel(());
    let bp_future = process_events_async(
        persister,
        event_handler,
        chain_monitor.clone(),
        channel_manager.clone(),
        Some(onion_messenger),
        gossip_source.as_gossip_sync(),
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
    );

    let background_processor = tokio::spawn(supervise_background_processor(
        bp_future,
        Arc::clone(&stop_processing),
        app_state.cancel_token.clone(),
    ));

    // Periodically drain queued VSS replications so an idle node still heals
    // after an outage (drains are otherwise only triggered by new writes).
    #[cfg(feature = "vss")]
    {
        let drain_store = Arc::clone(&kv_store);
        let stop_drain = Arc::clone(&stop_processing);
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(60));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                interval.tick().await;
                if stop_drain.load(Ordering::Acquire) {
                    break;
                }
                let store = Arc::clone(&drain_store);
                if let Err(e) = tokio::task::spawn_blocking(move || store.drain_pending()).await {
                    tracing::error!(error = %e, "periodic VSS drain task failed");
                }
            }
        });
    }

    // Regularly reconnect to channel peers.
    let connect_cm = Arc::clone(&channel_manager);
    let connect_pm = Arc::clone(&peer_manager);
    let connect_db = static_state.db();
    let stop_connect = Arc::clone(&stop_processing);
    let reconnect_interval_secs = static_state.config.node.peer_reconnect_interval_secs;
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(reconnect_interval_secs));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            interval.tick().await;
            // checked here and not only per peer: with no channels to reconnect, or once the read
            // below starts failing, the inner check is unreachable and the task would outlive the
            // node it belongs to, polling its database by path forever
            if stop_connect.load(Ordering::Acquire) {
                return;
            }
            let db = RlnDatabase::new((*connect_db).clone());
            match db.read_channel_peer_data() {
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
                    "ERROR: errored reading channel peer info from database: {:?}",
                    e
                ),
            }
        }
    });

    // Remote external signer force-close resilience. When the signer daemon is briefly unreachable, a
    // channel signing call returns LDK's async-unavailable sentinel (`Err(())`) and the operation parks
    // instead of failing the channel. While an outage is outstanding, periodically drive
    // `signer_unblocked` so parked operations retry — which is what makes the transport actually
    // attempt to reconnect, since nothing else calls it while everything is parked.
    //
    // Only spawned when the transport can genuinely go unreachable and recover
    // (`external_signer_link_watch` is `Some` only for the remote-signer daemon link — an in-process
    // uniffi signer can never be unreachable), and only ticking while the link is actually down:
    // `signer_unblocked(None)` is NOT a cheap no-op — it walks every peer's channel map under
    // `total_consistency_lock` and unconditionally forces a `ChannelManager` re-persist — so a
    // healthy node must not pay it every few seconds forever.
    //
    // The link watch is state-based (see `SignerLinkWatch`): every wake-up re-checks `is_connected`
    // rather than trusting the wake-up itself, so buffered/spurious signals only ever cost one extra
    // `signer_unblocked` pass, never a stuck loop.
    if let Some(link) = external_signer_link_watch {
        let su_channel_manager = Arc::clone(&channel_manager);
        let su_chain_monitor = Arc::clone(&chain_monitor);
        let su_stop = Arc::clone(&stop_processing);
        tokio::spawn(async move {
            loop {
                // Healthy: idle until the link reports a change (with a coarse timer only to notice
                // node shutdown — it drives no signer work).
                let link_event = tokio::select! {
                    _ = link.changed() => true,
                    _ = tokio::time::sleep(Duration::from_secs(30)) => false,
                };
                if su_stop.load(Ordering::Acquire) {
                    return;
                }
                if link_event && link.is_connected() {
                    // The link dropped and recovered while we slept: one pass covers anything that
                    // parked in between.
                    su_chain_monitor.signer_unblocked(None);
                    su_channel_manager.signer_unblocked(None);
                    continue;
                }
                if link.is_connected() {
                    continue;
                }
                // Outage: drive retries until the transport reports the link is back, then one final
                // pass for anything that parked right around the transition. Reacts immediately to
                // the reconnect signal instead of waiting out the tick interval; the interval is the
                // backstop that drives the reconnect attempts in the first place (its first tick
                // completes immediately).
                let mut interval = tokio::time::interval(Duration::from_secs(5));
                interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                loop {
                    tokio::select! {
                        _ = interval.tick() => {}
                        _ = link.changed() => {}
                    }
                    if su_stop.load(Ordering::Acquire) {
                        return;
                    }
                    su_chain_monitor.signer_unblocked(None);
                    su_channel_manager.signer_unblocked(None);
                    if link.is_connected() {
                        break;
                    }
                }
            }
        });
    }

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

    // cleanup the buffers of RGB file transfers a peer started and never finished
    let sweep_handler = Arc::clone(&rgb_file_transfer_handler);
    let stop_sweep = Arc::clone(&stop_processing);
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(REASSEMBLY_SWEEP_INTERVAL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            if stop_sweep.load(Ordering::Acquire) {
                return;
            }
            sweep_handler.sweep_stale_state();
        }
    });

    let peer_man = Arc::clone(&peer_manager);
    let chan_man = Arc::clone(&channel_manager);
    let announce_initial_delay_secs = static_state.config.node.announce_initial_delay_secs;
    let announce_refresh_interval_secs = static_state.config.node.announce_refresh_interval_secs;
    tokio::spawn(async move {
        // First wait until we have some peers and maybe have opened a channel.
        tokio::time::sleep(Duration::from_secs(announce_initial_delay_secs)).await;
        // Then, update our announcement periodically to keep it fresh but avoid unnecessary churn
        // in the global gossip network.
        let mut interval =
            tokio::time::interval(Duration::from_secs(announce_refresh_interval_secs));
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

    #[cfg(feature = "vss")]
    if let Some(guard) = fence_guard.as_mut() {
        guard.disarm();
    }

    Ok((
        LdkBackgroundServices {
            stop_processing,
            gossip_shutdown,
            peer_manager: peer_manager.clone(),
            bp_exit,
            background_processor: Some(background_processor),
        },
        unlocked_state,
    ))
}

#[allow(dead_code)]
pub(crate) fn attach_external_signer_transport(
    transport: Arc<dyn ExternalSignerTransport>,
) -> Result<ExternalSignerAttachment, APIError> {
    let probe = VlsSignerAdapter::new(Arc::clone(&transport));
    let bootstrap = probe.bootstrap().map_err(|e| match e {
        crate::signer::RlnSignerError::Transport(msg) => APIError::ExternalSignerUnavailable(msg),
        crate::signer::RlnSignerError::Protocol(msg)
        | crate::signer::RlnSignerError::Unsupported(msg) => {
            APIError::ExternalSignerProtocolError(msg)
        }
    })?;
    validate_bootstrap_payload(&bootstrap)
        .map_err(|e| APIError::ExternalSignerProtocolError(e.to_string()))?;
    Ok(ExternalSignerAttachment {
        bootstrap,
        transport,
    })
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
        ldk_background_services.gossip_shutdown.notify_one();
        ldk_background_services.peer_manager.disconnect_all_peers();

        // Stop the background processor. Its `bp_exit` receiver lives inside the
        // `process_events_async` future, so nothing to signal if the background processor is
        // already gone. Also, send can find no receiver during a panic (racy).
        if !ldk_background_services.bp_exit.is_closed() {
            let _ = ldk_background_services.bp_exit.send(());
            ldk_background_services.background_processor.take()
        } else {
            None
        }
    }
}

#[cfg(feature = "vss")]
const BP_SHUTDOWN_FLUSH_TIMEOUT: Duration = Duration::from_secs(30);

/// Budget for draining and stopping the VSS-backed stores at teardown. Same order as the
/// background-processor join above: it covers the flush window plus a stuck remote request being
/// given up on, and keeps a shutdown terminating even when VSS never answers.
#[cfg(feature = "vss")]
const VSS_TEARDOWN_TIMEOUT: Duration = Duration::from_secs(30);

/// Window for the final drain of queued replications, inside the teardown budget.
#[cfg(feature = "vss")]
const VSS_TEARDOWN_FLUSH_WINDOW: Duration = Duration::from_secs(10);

/// Budget for the single VSS round-trip that hands the fence over.
#[cfg(feature = "vss")]
const VSS_FENCE_RELEASE_TIMEOUT: Duration = Duration::from_secs(10);

#[cfg(feature = "vss")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VssTeardown {
    /// Flush and stops finished: no further remote mutation can begin.
    Complete,
    /// A step was abandoned at the deadline: an in-flight write may still land.
    Abandoned,
}

/// Drains queued replications and stops both stores, bounding every step by what is left of
/// `deadline`. `SyncedKvStore::stop` waits on the drain gate, so a hung remote write would
/// otherwise block the shutdown forever.
#[cfg(feature = "vss")]
async fn stop_vss_stores(
    kv_store: &Arc<SyncedKvStore>,
    monitor_kv_store: &Arc<RemoteFirstKvStore>,
    deadline: Instant,
) -> VssTeardown {
    let remaining = || deadline.saturating_duration_since(Instant::now());

    let flush_store = Arc::clone(kv_store);
    let flush_deadline = std::cmp::min(deadline, Instant::now() + VSS_TEARDOWN_FLUSH_WINDOW);
    let flush =
        tokio::task::spawn_blocking(move || flush_store.flush_pending_until(flush_deadline));
    match tokio::time::timeout(remaining(), flush).await {
        Ok(Ok(0)) => {}
        Ok(Ok(n)) => tracing::error!(
            pending = n,
            "VSS replications still queued at shutdown; they persist locally and \
             will retry on next unlock"
        ),
        Ok(Err(e)) => {
            tracing::error!(error = %e, "pending-queue flush task failed");
            monitor_kv_store.stop();
            return VssTeardown::Abandoned;
        }
        Err(_) => {
            tracing::error!("pending-queue flush did not finish within the teardown budget");
            monitor_kv_store.stop();
            return VssTeardown::Abandoned;
        }
    }

    // Stop drains and abort outage-pending writes: once both stores are stopped no remote
    // mutation can begin, which is what makes giving up the fence safe.
    let stop_store = Arc::clone(kv_store);
    let stop = tokio::task::spawn_blocking(move || stop_store.stop());
    match tokio::time::timeout(remaining(), stop).await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => {
            tracing::error!(error = %e, "pending-queue stop task failed");
            monitor_kv_store.stop();
            return VssTeardown::Abandoned;
        }
        Err(_) => {
            tracing::error!("pending-queue stop did not finish within the teardown budget");
            monitor_kv_store.stop();
            return VssTeardown::Abandoned;
        }
    }
    // Only signals the retry loops to abort, so it cannot block. Idempotent: the abandoned
    // paths above may have already called it.
    monitor_kv_store.stop();
    VssTeardown::Complete
}

/// Releases the VSS fence, but only after a complete teardown: a write still in flight could
/// otherwise land on a store another instance has already taken over. Returns whether the
/// release was attempted.
#[cfg(feature = "vss")]
async fn release_vss_fence(kv_store: Arc<SyncedKvStore>, teardown: VssTeardown) -> bool {
    if teardown != VssTeardown::Complete {
        tracing::error!(
            "VSS teardown did not complete within {:?}; keeping the fence, the next \
             instance needs an explicit fence clear to take over",
            VSS_TEARDOWN_TIMEOUT
        );
        return false;
    }
    let release = tokio::task::spawn_blocking(move || kv_store.release_vss_fence_if_owned());
    match tokio::time::timeout(VSS_FENCE_RELEASE_TIMEOUT, release).await {
        Ok(Ok(Ok(()))) => {}
        Ok(Ok(Err(e))) => tracing::warn!(error = %e, "failed to release VSS fence during shutdown"),
        Ok(Err(e)) => tracing::warn!(error = %e, "VSS fence release task failed"),
        Err(_) => tracing::warn!(
            "VSS fence release did not finish within {:?}",
            VSS_FENCE_RELEASE_TIMEOUT
        ),
    }
    true
}

// Runs while shutting down, possibly because the background processor itself
// died, so its outcome is reported instead of unwrapped.
fn log_bp_shutdown_result(res: Result<Result<(), io::Error>, tokio::task::JoinError>) {
    match res {
        Ok(Ok(())) => {}
        Ok(Err(e)) => {
            tracing::error!(error = %e, "background processor exited with error during shutdown")
        }
        Err(e) => tracing::error!(error = %e, "background processor task join failed"),
    }
}

#[cfg(all(test, feature = "vss"))]
mod vss_teardown_tests {
    use super::*;
    use crate::kv_store::SeaOrmKvStore;

    fn local_stores() -> (Arc<SyncedKvStore>, Arc<RemoteFirstKvStore>) {
        let connection = crate::runtime::block_on(sea_orm::Database::connect("sqlite::memory:"))
            .expect("in-memory database");
        let local = Arc::new(SeaOrmKvStore::from_connection(Arc::new(connection)));
        (
            Arc::new(SyncedKvStore::local_only(Arc::clone(&local))),
            Arc::new(RemoteFirstKvStore::new(local, None)),
        )
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn completed_teardown_releases_the_fence() {
        let (kv_store, monitor_kv_store) = local_stores();

        let teardown = stop_vss_stores(
            &kv_store,
            &monitor_kv_store,
            Instant::now() + Duration::from_secs(5),
        )
        .await;

        assert_eq!(teardown, VssTeardown::Complete);
        assert!(release_vss_fence(kv_store, teardown).await);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn abandoned_teardown_keeps_the_fence() {
        let (kv_store, monitor_kv_store) = local_stores();
        // `stop` blocks on the drain gate; a hung remote write must not hold the shutdown.
        kv_store.set_before_stop_gate_hook(Arc::new(|| std::thread::sleep(Duration::from_secs(1))));
        // Stand in for a live retry loop so the shutdown signal has a receiver to observe.
        let shutdown_rx = monitor_kv_store.subscribe_shutdown();

        let teardown = stop_vss_stores(
            &kv_store,
            &monitor_kv_store,
            Instant::now() + Duration::from_millis(100),
        )
        .await;

        assert_eq!(teardown, VssTeardown::Abandoned);
        // The abandoned path must still abort the monitor retries before giving up.
        assert!(*shutdown_rx.borrow());
        assert!(!release_vss_fence(kv_store, teardown).await);
    }
}

pub(crate) async fn stop_ldk(app_state: Arc<AppState>) {
    tracing::info!("Stopping LDK");

    #[cfg(feature = "vss")]
    let stores = app_state
        .get_unlocked_app_state()
        .await
        .as_ref()
        .map(|unlocked| {
            (
                Arc::clone(&unlocked.kv_store),
                Arc::clone(&unlocked.monitor_kv_store),
            )
        });

    #[cfg(feature = "vss")]
    if let Some(mut join_handle) = app_state.stop_ldk() {
        // Bounded flush: give the final remote-first persists time to reach
        // VSS, then abort outage-pending retries so shutdown cannot hang.
        match tokio::time::timeout(BP_SHUTDOWN_FLUSH_TIMEOUT, &mut join_handle).await {
            Ok(res) => log_bp_shutdown_result(res),
            Err(_) => {
                tracing::error!(
                    "final VSS flush did not complete in {:?}; aborting pending \
                     retries — last channel-manager state may not have replicated",
                    BP_SHUTDOWN_FLUSH_TIMEOUT
                );
                if let Some((_, ref monitor_kv_store)) = stores {
                    monitor_kv_store.stop();
                }
                log_bp_shutdown_result(join_handle.await);
            }
        }
    }
    #[cfg(not(feature = "vss"))]
    if let Some(join_handle) = app_state.stop_ldk() {
        log_bp_shutdown_result(join_handle.await);
    }

    // Any shutdown that reaches here (lock, /shutdown, signal, fatal panic) hands the VSS fence
    // over so the next unlock — a fresh instance id — takes over without an explicit
    // /vssclearfence. The teardown is bounded, and the fence is only released once it provably
    // completed; a hard kill, or a teardown abandoned at its deadline, leaves the fence behind.
    #[cfg(feature = "vss")]
    {
        if let Some((kv_store, monitor_kv_store)) = stores {
            let deadline = Instant::now() + VSS_TEARDOWN_TIMEOUT;
            let teardown = stop_vss_stores(&kv_store, &monitor_kv_store, deadline).await;
            release_vss_fence(kv_store, teardown).await;
        }
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
            tracing::error!("LDK peer port {peer_port} was not released within 10s");
            break;
        }
    }

    tracing::info!("Stopped LDK");
}

pub(crate) fn write_rgb_payment_info_file(
    payment_hash: &PaymentHash,
    contract_id: ContractId,
    amount_rgb: u64,
    swap_payment: bool,
    inbound: bool,
    kv_store: &dyn KVStoreSync,
) {
    let payment_info = RgbPaymentInfo {
        contract_id,
        amount: amount_rgb,
        local_rgb_amount: 0,
        remote_rgb_amount: 0,
        swap_payment,
        inbound,
    };
    let data = bincode::serialize(&payment_info).expect("valid rgb payment info");
    let ns = if inbound {
        RGB_PAYMENT_INFO_INBOUND_NS
    } else {
        RGB_PAYMENT_INFO_OUTBOUND_NS
    };
    let key = payment_hash.0.as_hex().to_string();
    let _ = kv_store.write(RGB_PRIMARY_NS, ns, &key, data.clone());
    let _ = kv_store.write(RGB_PRIMARY_NS, ns, &format!("{key}_pending"), data);
}

pub(crate) fn clear_rgb_payment_pending(
    payment_hash: &PaymentHash,
    inbound: bool,
    kv_store: &dyn KVStoreSync,
) {
    let payment_hash_str = hex_str(&payment_hash.0);
    let pending_key = format!("{payment_hash_str}_pending");
    let namespace = if inbound {
        RGB_PAYMENT_INFO_INBOUND_NS
    } else {
        RGB_PAYMENT_INFO_OUTBOUND_NS
    };
    let _ = kv_store.remove(RGB_PRIMARY_NS, namespace, &pending_key, false);
    if let Ok(keys) = kv_store.list(RGB_PRIMARY_NS, namespace) {
        for key in keys {
            if key.ends_with(&pending_key) && key.len() > pending_key.len() {
                let _ = kv_store.remove(RGB_PRIMARY_NS, namespace, &key, false);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // `chain.indexer_url` is the whole reason the unlock request keeps `indexer_url` optional
    // instead of adopting upstream's mandatory field, so the layering itself is pinned here.
    #[test]
    fn indexer_url_falls_back_to_the_config_file() {
        assert_eq!(
            resolve_indexer_url(None, Some("127.0.0.1:50001")).unwrap(),
            "127.0.0.1:50001"
        );
    }

    #[test]
    fn indexer_url_from_the_request_wins_over_the_config_file() {
        assert_eq!(
            resolve_indexer_url(Some("from-request:50001"), Some("from-config:50001")).unwrap(),
            "from-request:50001"
        );
    }

    // the per-network default indexer is gone: neither source means the unlock fails outright
    #[test]
    fn indexer_url_missing_from_both_sources_errors() {
        assert!(matches!(
            resolve_indexer_url(None, None),
            Err(APIError::MissingIndexerUrl)
        ));
    }
    use crate::kv_store::SeaOrmKvStore;
    use lightning::rgb_utils::RgbInfo;
    use rln_migration::{Migrator, MigratorTrait};
    use sea_orm::{ConnectOptions, Database};
    use std::{collections::HashSet, str::FromStr, sync::Mutex};

    fn test_contract_id() -> rgb_lib::ContractId {
        rgb_lib::ContractId::from_str("rgb:EIkAVQvq-WbAb5JG-CYxbUER-oqDNwne-ZNxBDID-p0cpf9U")
            .unwrap()
    }

    fn test_peer_pubkey(tag: u8) -> PublicKey {
        let secp = bitcoin::secp256k1::Secp256k1::new();
        let mut key_bytes = [0u8; 32];
        key_bytes[31] = tag.max(1);
        let secret_key = bitcoin::secp256k1::SecretKey::from_slice(&key_bytes).unwrap();
        PublicKey::from_secret_key(&secp, &secret_key)
    }

    struct MockPeerChannelLookup {
        live_peers: Mutex<HashSet<PublicKey>>,
    }

    impl MockPeerChannelLookup {
        fn new(peers: impl IntoIterator<Item = PublicKey>) -> Self {
            Self {
                live_peers: Mutex::new(peers.into_iter().collect()),
            }
        }
    }

    impl LiveChannelLookup for MockPeerChannelLookup {
        fn peer_has_live_channel(&self, peer: &PublicKey) -> bool {
            self.live_peers.lock().unwrap().contains(peer)
        }
    }

    #[test]
    fn live_channel_access_follows_peer_lookup() {
        let allowed_peer = test_peer_pubkey(1);
        let denied_peer = test_peer_pubkey(2);
        let lookup = Arc::new(MockPeerChannelLookup::new([allowed_peer]));
        let access = LiveChannelAccess::new_with_lookup(lookup);

        assert!(access.allows_peer(&allowed_peer));
        assert!(!access.allows_peer(&denied_peer));
    }

    #[test]
    fn live_channel_access_loses_access_when_last_channel_is_removed() {
        let peer = test_peer_pubkey(3);
        let lookup = Arc::new(MockPeerChannelLookup::new([peer]));
        let access = LiveChannelAccess::new_with_lookup(lookup.clone());

        assert!(access.allows_peer(&peer));
        lookup.live_peers.lock().unwrap().clear();
        assert!(!access.allows_peer(&peer));
    }

    fn build_synced_kv_store() -> Arc<SyncedKvStore> {
        let db_path = std::env::temp_dir().join(format!("rln-ldk-unit-{}", uuid::Uuid::new_v4()));
        let connection_string = format!("sqlite:{}?mode=rwc", db_path.display());
        let db =
            crate::runtime::block_on(Database::connect(ConnectOptions::new(connection_string)))
                .expect("db connection");
        crate::runtime::block_on(Migrator::up(&db, None)).expect("run migrations");
        Arc::new(SyncedKvStore::local_only(Arc::new(
            SeaOrmKvStore::from_connection(Arc::new(db)),
        )))
    }

    fn build_kv_store() -> Arc<dyn KVStoreSync + Send + Sync> {
        build_synced_kv_store()
    }

    #[test]
    fn canonical_rgb_channel_metadata_is_idempotent_and_conflict_safe() {
        let kv_store = build_synced_kv_store();
        let channel_id = "02".repeat(32);
        let expected = RgbInfo {
            contract_id: test_contract_id(),
            schema: AssetSchema::Nia,
            local_rgb_amount: 600,
            remote_rgb_amount: 0,
            batch_transfer_idx: Some(7),
            counterparty_knows_asset: false,
        };

        persist_canonical_rgb_channel_info(&channel_id, &expected, kv_store.as_ref())
            .expect("initial canonical write");
        persist_canonical_rgb_channel_info(&channel_id, &expected, kv_store.as_ref())
            .expect("idempotent canonical replay");

        let shifted = RgbInfo {
            local_rgb_amount: 250,
            remote_rgb_amount: 350,
            batch_transfer_idx: None,
            ..expected.clone()
        };
        kv_store.write_rgb_channel_info(&channel_id, &shifted, false);
        persist_canonical_rgb_channel_info(&channel_id, &expected, kv_store.as_ref())
            .expect("same channel allocation with a live balance split");

        let mut conflicting = expected.clone();
        conflicting.local_rgb_amount = 599;
        persist_canonical_rgb_channel_info(&channel_id, &conflicting, kv_store.as_ref())
            .expect_err("conflicting recovery metadata must fail closed");
        assert_eq!(
            kv_store
                .read_rgb_channel_info(&channel_id, false)
                .expect("canonical metadata"),
            shifted,
        );
    }

    #[test]
    fn finalized_sender_pending_marker_cleanup_is_idempotent() {
        let kv_store = build_synced_kv_store();
        let channel_id = "03".repeat(32);

        kv_store
            .write(
                PENDING_FUNDING_NAMESPACE,
                "",
                &channel_id,
                b"funding-txid".to_vec(),
            )
            .expect("seed pending-funding marker");

        for _ in 0..2 {
            remove_rgb_sender_funding_entry(
                PENDING_FUNDING_NAMESPACE,
                &channel_id,
                "cannot remove finalized RGB pending-funding marker",
                kv_store.as_ref(),
            )
            .expect("finalized cleanup must be replay-safe");
        }
    }

    fn seed_channel_info(
        kv_store: &Arc<dyn KVStoreSync + Send + Sync>,
        channel_id: &str,
        local_rgb_amount: u64,
        remote_rgb_amount: u64,
    ) {
        let info = RgbInfo {
            contract_id: test_contract_id(),
            schema: serde_json::from_str("\"Nia\"").expect("valid schema"),
            local_rgb_amount,
            remote_rgb_amount,
            batch_transfer_idx: None,
            counterparty_knows_asset: false,
        };
        kv_store.write_rgb_channel_info(channel_id, &info, false);
    }

    fn seed_pending_payment_key(
        kv_store: &Arc<dyn KVStoreSync + Send + Sync>,
        namespace: &str,
        channel_id: &str,
        payment_hash: &PaymentHash,
        swap_payment: bool,
        inbound: bool,
    ) -> String {
        let info = RgbPaymentInfo {
            contract_id: test_contract_id(),
            amount: 25,
            local_rgb_amount: 100,
            remote_rgb_amount: 0,
            swap_payment,
            inbound,
        };
        let key = format!("{}{}_pending", channel_id, hex_str(&payment_hash.0));
        let data = bincode::serialize(&info).expect("serialize rgb payment info");
        kv_store
            .write(RGB_PRIMARY_NS, namespace, &key, data)
            .expect("write rgb payment info");
        key
    }

    fn read_local_amount(kv_store: &Arc<dyn KVStoreSync + Send + Sync>, channel_id: &str) -> u64 {
        let info = kv_store
            .read_rgb_channel_info(channel_id, false)
            .expect("read rgb channel info");
        info.local_rgb_amount
    }

    #[test]
    fn finalize_rgb_channel_payment_clears_pending_markers_after_apply() {
        let kv_store = build_kv_store();
        let channel_id = "a".repeat(64);
        let payment_hash = PaymentHash([0xAB; 32]);
        seed_channel_info(&kv_store, &channel_id, 0, 100);
        let key = seed_pending_payment_key(
            &kv_store,
            RGB_PAYMENT_INFO_INBOUND_NS,
            &channel_id,
            &payment_hash,
            false,
            true,
        );

        _finalize_rgb_channel_payment(&payment_hash, true, &kv_store).expect("scanner succeeds");

        assert!(matches!(
            kv_store.read(RGB_PRIMARY_NS, RGB_PAYMENT_INFO_INBOUND_NS, &key),
            Err(e) if e.kind() == io::ErrorKind::NotFound
        ));
        assert_eq!(read_local_amount(&kv_store, &channel_id), 25);
    }

    #[test]
    fn finalize_rgb_channel_payment_leaves_pending_markers_when_nothing_applies() {
        let kv_store = build_kv_store();
        let channel_id = "b".repeat(64);
        let payment_hash = PaymentHash([0xCD; 32]);
        seed_channel_info(&kv_store, &channel_id, 100, 0);
        let key = seed_pending_payment_key(
            &kv_store,
            RGB_PAYMENT_INFO_INBOUND_NS,
            &channel_id,
            &payment_hash,
            true,
            true,
        );

        _finalize_rgb_channel_payment(&payment_hash, false, &kv_store)
            .expect("scanner succeeds without applying");

        assert!(kv_store
            .read(RGB_PRIMARY_NS, RGB_PAYMENT_INFO_INBOUND_NS, &key)
            .is_ok());
        assert_eq!(read_local_amount(&kv_store, &channel_id), 100);
    }

    #[test]
    fn finalize_rgb_channel_payment_ignores_non_pending_keys() {
        let kv_store = build_kv_store();
        let channel_id = "c".repeat(64);
        let payment_hash = PaymentHash([0xEF; 32]);
        seed_channel_info(&kv_store, &channel_id, 100, 0);
        let final_key = format!("{}{}", channel_id, hex_str(&payment_hash.0));
        let info = RgbPaymentInfo {
            contract_id: test_contract_id(),
            amount: 25,
            local_rgb_amount: 100,
            remote_rgb_amount: 0,
            swap_payment: false,
            inbound: true,
        };
        kv_store
            .write(
                RGB_PRIMARY_NS,
                RGB_PAYMENT_INFO_INBOUND_NS,
                &final_key,
                bincode::serialize(&info).unwrap(),
            )
            .unwrap();

        _finalize_rgb_channel_payment(&payment_hash, true, &kv_store).expect("scanner succeeds");

        assert!(kv_store
            .read(RGB_PRIMARY_NS, RGB_PAYMENT_INFO_INBOUND_NS, &final_key)
            .is_ok());
        assert_eq!(read_local_amount(&kv_store, &channel_id), 100);
    }

    #[test]
    fn finalize_rgb_channel_payment_only_touches_matching_payment_hash() {
        let kv_store = build_kv_store();
        let channel_id = "d".repeat(64);
        let target_hash = PaymentHash([0x11; 32]);
        let other_hash = PaymentHash([0x22; 32]);
        seed_channel_info(&kv_store, &channel_id, 0, 100);
        let target_key = seed_pending_payment_key(
            &kv_store,
            RGB_PAYMENT_INFO_INBOUND_NS,
            &channel_id,
            &target_hash,
            false,
            true,
        );
        let other_key = seed_pending_payment_key(
            &kv_store,
            RGB_PAYMENT_INFO_INBOUND_NS,
            &channel_id,
            &other_hash,
            false,
            true,
        );

        _finalize_rgb_channel_payment(&target_hash, true, &kv_store).expect("scanner succeeds");

        assert!(matches!(
            kv_store.read(RGB_PRIMARY_NS, RGB_PAYMENT_INFO_INBOUND_NS, &target_key),
            Err(e) if e.kind() == io::ErrorKind::NotFound
        ));
        assert!(kv_store
            .read(RGB_PRIMARY_NS, RGB_PAYMENT_INFO_INBOUND_NS, &other_key)
            .is_ok());
    }

    #[test]
    fn finalize_rgb_channel_payment_propagates_non_not_found_errors() {
        let db_path = std::env::temp_dir().join(format!("rln-ldk-unit-{}", uuid::Uuid::new_v4()));
        let connection_string = format!("sqlite:{}?mode=rwc", db_path.display());
        let db =
            crate::runtime::block_on(Database::connect(ConnectOptions::new(connection_string)))
                .expect("db connection");
        let kv_store: Arc<dyn KVStoreSync + Send + Sync> =
            Arc::new(SeaOrmKvStore::from_connection(Arc::new(db)));

        let result = _finalize_rgb_channel_payment(&PaymentHash([0; 32]), true, &kv_store);
        match result {
            Err(e) => assert_ne!(
                e.kind(),
                io::ErrorKind::NotFound,
                "real DB errors must not be classified as NotFound"
            ),
            Ok(()) => panic!("expected error when kv_store table is missing"),
        }
    }

    #[test]
    fn ldk_auxiliary_secret_derivation_matches_keys_manager() {
        use lightning::ln::inbound_payment::ExpandedKey;
        let seed = [18u8; 32];
        let kv = build_kv_store();
        let km = KeysManager::new(
            &seed,
            1,
            2,
            true,
            std::env::temp_dir().join(format!("ldk-aux-parity-{}", uuid::Uuid::new_v4())),
            kv,
        );
        let (a, b, c) =
            signer_external::ldk_keys_manager_material::derive_ldk_keys_manager_auxiliary_secret_bytes(
                &seed,
            )
            .expect("derive");
        assert_eq!(km.get_expanded_key(), ExpandedKey::new(a));
        assert_eq!(km.get_peer_storage_key().inner, b);
        assert_eq!(km.get_receive_auth_key().0, c);
    }

    #[test]
    fn ifa_supported_on_all_networks_but_mainnet() {
        assert!(!supported_asset_schemas(BitcoinNetwork::Mainnet).contains(&AssetSchema::Ifa));
        for network in [
            BitcoinNetwork::Testnet,
            BitcoinNetwork::Testnet4,
            BitcoinNetwork::Signet,
            BitcoinNetwork::SignetCustom,
            BitcoinNetwork::Regtest,
        ] {
            let schemas = supported_asset_schemas(network);
            assert!(schemas.contains(&AssetSchema::Ifa));
            assert!(schemas.contains(&AssetSchema::Nia));
            assert!(schemas.contains(&AssetSchema::Cfa));
            assert!(schemas.contains(&AssetSchema::Uda));
        }
    }

    #[test]
    fn sweep_receive_reuse_margin_is_smaller_under_address_reuse() {
        let now = 1_000_000;
        let expiration = now + RGB_TRANSFER_CHAN_EXPIRATION_SECS;

        // at t+23h the 1h margin has been reached, but the reuse margin has not
        let late = expiration - RGB_RECEIVE_REUSE_MARGIN_SECS;
        assert!(!sweep_receive_is_reusable(late, expiration, false));
        assert!(sweep_receive_is_reusable(late, expiration, true));
    }

    #[test]
    fn sweep_receive_reuse_respects_both_margin_boundaries() {
        let expiration = 1_000_000;

        for (reuse, margin) in [
            (false, RGB_RECEIVE_REUSE_MARGIN_SECS),
            (true, RGB_RECEIVE_REUSE_MARGIN_ADDR_REUSE_SECS),
        ] {
            // strictly inside the margin is reusable, the boundary itself is not
            assert!(sweep_receive_is_reusable(
                expiration - margin - 1,
                expiration,
                reuse
            ));
            assert!(!sweep_receive_is_reusable(
                expiration - margin,
                expiration,
                reuse
            ));
        }
    }

    #[test]
    fn inbound_payment_expiry_projection_preserves_boundary_semantics() {
        assert_eq!(
            effective_inbound_payment_status(HTLCStatus::Pending, Some(100), None, 100, 10),
            HTLCStatus::Pending
        );
        assert_eq!(
            effective_inbound_payment_status(HTLCStatus::Pending, Some(100), None, 101, 10),
            HTLCStatus::Failed
        );
        assert_eq!(
            effective_inbound_payment_status(HTLCStatus::Claimable, Some(100), None, 100, 10),
            HTLCStatus::Failed
        );
        assert_eq!(
            effective_inbound_payment_status(HTLCStatus::Claimable, None, Some(10), 99, 10),
            HTLCStatus::Failed
        );
        assert_eq!(
            effective_inbound_payment_status(HTLCStatus::Succeeded, Some(1), Some(1), 100, 100),
            HTLCStatus::Succeeded
        );
    }

    #[test]
    fn rgb_sender_recovery_matrix_is_fail_closed_at_broadcast_boundary() {
        use RgbSenderFundingStage::*;
        use RgbSenderRecoveryAction::*;

        let record = |stage, manual_broadcast| RgbSenderFundingRecord {
            version: if manual_broadcast {
                RgbSenderFundingRecord::MANUAL_BROADCAST_VERSION
            } else {
                RgbSenderFundingRecord::LEGACY_VERSION
            },
            manual_broadcast,
            temporary_channel_id: "01".repeat(32),
            final_channel_id: Some("02".repeat(32)),
            funding_txid: "03".repeat(32),
            batch_transfer_idx: 7,
            rgb_info: None,
            consignment_delivery: RgbSenderConsignmentDelivery::Proxy,
            stage,
        };

        for stage in [
            Preparing,
            StockPromoted,
            HandoffReady,
            HandedToLdk,
            BroadcastSafeObserved,
        ] {
            assert_eq!(
                rgb_sender_recovery_action(&record(stage, true), false, false),
                Rollback
            );
            assert_eq!(
                rgb_sender_recovery_action(&record(stage, true), true, false),
                ResumeBroadcast
            );
        }
        for stage in [Broadcasting, BroadcastCommitted] {
            assert_eq!(
                rgb_sender_recovery_action(&record(stage, true), false, false),
                FailClosed
            );
            assert_eq!(
                rgb_sender_recovery_action(&record(stage, true), true, false),
                Finalize
            );
        }
        for stage in [
            HandoffReady,
            HandedToLdk,
            BroadcastSafeObserved,
            Broadcasting,
            BroadcastCommitted,
        ] {
            assert_eq!(
                rgb_sender_recovery_action(&record(stage, false), false, false),
                FailClosed
            );
        }
        for stage in [
            Preparing,
            StockPromoted,
            HandoffReady,
            HandedToLdk,
            BroadcastSafeObserved,
            Broadcasting,
        ] {
            assert_eq!(
                rgb_sender_recovery_action(&record(stage, true), true, true),
                Finalize
            );
            assert_eq!(
                rgb_sender_recovery_action(&record(stage, true), false, true),
                FailClosed
            );
        }
        for stage in [Finalized, DurablyCompleted] {
            assert_eq!(
                rgb_sender_recovery_action(&record(stage, true), true, false),
                Finalize
            );
            assert_eq!(
                rgb_sender_recovery_action(&record(stage, true), true, true),
                Finalize
            );
            assert_eq!(
                rgb_sender_recovery_action(&record(stage, true), false, false),
                FailClosed
            );
            assert_eq!(
                rgb_sender_recovery_action(&record(stage, true), false, true),
                FailClosed
            );
        }
    }

    #[test]
    fn sweep_receive_past_expiry_is_never_reusable() {
        let expiration = 1_000_000;
        for reuse in [false, true] {
            assert!(!sweep_receive_is_reusable(expiration, expiration, reuse));
            assert!(!sweep_receive_is_reusable(
                expiration + 1,
                expiration,
                reuse
            ));
        }
        // pins the reuse margin itself: a receive with a minute of life must never be handed out,
        // otherwise it can expire mid-sweep. Asserted without reference to the constant, so
        // shrinking it back towards zero fails here.
        assert!(!sweep_receive_is_reusable(
            expiration - 60,
            expiration,
            true
        ));
        // same for the non-reuse margin, which must stay far larger than a sweep's duration
        assert!(!sweep_receive_is_reusable(
            expiration - 1800,
            expiration,
            false
        ));
    }

    #[test]
    fn legacy_sender_handoff_never_uses_negative_observation_as_rollback_proof() {
        let record = RgbSenderFundingRecord {
            version: RgbSenderFundingRecord::LEGACY_VERSION,
            manual_broadcast: false,
            temporary_channel_id: "01".repeat(32),
            final_channel_id: Some("02".repeat(32)),
            funding_txid: "03".repeat(32),
            batch_transfer_idx: 7,
            rgb_info: None,
            consignment_delivery: RgbSenderConsignmentDelivery::Proxy,
            stage: RgbSenderFundingStage::HandedToLdk,
        };
        assert_eq!(
            rgb_sender_recovery_action(&record, false, false),
            RgbSenderRecoveryAction::FailClosed
        );

        let recovery = rgb_funding_recovery_view(&record, false, Ok(Some(false)), None);
        assert_eq!(
            recovery.action,
            RgbFundingRecoveryAction::ManualChannelStateRecovery
        );
        let guard = RgbFundingRecoveryGuard::default();
        guard.replace(&[recovery]);
        assert!(matches!(
            guard.lock_rgb_wallet_mutation(),
            Err(APIError::RgbFundingRecoveryRequired(ref txid))
                if txid == &record.funding_txid
        ));
    }

    #[test]
    fn transient_sender_reconciliation_failure_preserves_retryable_evidence() {
        let record = RgbSenderFundingRecord {
            version: RgbSenderFundingRecord::VERSION,
            manual_broadcast: true,
            temporary_channel_id: "01".repeat(32),
            final_channel_id: Some("02".repeat(32)),
            funding_txid: "03".repeat(32),
            batch_transfer_idx: 7,
            rgb_info: Some(RgbInfo {
                contract_id: test_contract_id(),
                schema: AssetSchema::Nia,
                local_rgb_amount: 1,
                remote_rgb_amount: 2,
                batch_transfer_idx: Some(7),
                counterparty_knows_asset: false,
            }),
            consignment_delivery: RgbSenderConsignmentDelivery::P2p,
            stage: RgbSenderFundingStage::Broadcasting,
        };
        let error = RgbLibError::Network {
            details: "VSS temporarily unavailable".to_owned(),
        };

        let recovery = rgb_funding_recovery_view(&record, true, Ok(None), Some(&error));
        assert_eq!(
            recovery.stage,
            RgbFundingRecoveryStage::Sender(RgbSenderFundingStage::Broadcasting)
        );
        assert_eq!(
            recovery.action,
            RgbFundingRecoveryAction::RetryReconciliation
        );
        assert_eq!(recovery.error, Some(error.to_string()));
    }

    #[test]
    fn recovery_listing_hides_only_durable_completion_tombstones() {
        let record = |stage| RgbSenderFundingRecord {
            version: RgbSenderFundingRecord::VERSION,
            manual_broadcast: true,
            temporary_channel_id: "01".repeat(32),
            final_channel_id: Some("02".repeat(32)),
            funding_txid: "03".repeat(32),
            batch_transfer_idx: 7,
            rgb_info: Some(RgbInfo {
                contract_id: test_contract_id(),
                schema: AssetSchema::Nia,
                local_rgb_amount: 1,
                remote_rgb_amount: 2,
                batch_transfer_idx: Some(7),
                counterparty_knows_asset: false,
            }),
            consignment_delivery: RgbSenderConsignmentDelivery::P2p,
            stage,
        };

        assert!(!should_report_rgb_sender_recovery(
            &record(RgbSenderFundingStage::DurablyCompleted),
            true,
        ));
        assert!(should_report_rgb_sender_recovery(
            &record(RgbSenderFundingStage::DurablyCompleted),
            false,
        ));
        assert!(should_report_rgb_sender_recovery(
            &record(RgbSenderFundingStage::RetryRequired),
            false,
        ));
    }

    #[test]
    fn rgb_funding_recovery_stages_have_stable_opaque_names() {
        use lightning::rgb_utils::FundingAcceptanceStage;

        let stages = [
            (
                RgbFundingRecoveryStage::Sender(RgbSenderFundingStage::Preparing),
                "sender_preparing",
            ),
            (
                RgbFundingRecoveryStage::Sender(RgbSenderFundingStage::StockPromoted),
                "sender_stock_promoted",
            ),
            (
                RgbFundingRecoveryStage::Sender(RgbSenderFundingStage::HandoffReady),
                "sender_handoff_ready",
            ),
            (
                RgbFundingRecoveryStage::Sender(RgbSenderFundingStage::HandedToLdk),
                "sender_handed_to_ldk",
            ),
            (
                RgbFundingRecoveryStage::Sender(RgbSenderFundingStage::BroadcastSafeObserved),
                "sender_broadcast_safe_observed",
            ),
            (
                RgbFundingRecoveryStage::Sender(RgbSenderFundingStage::Broadcasting),
                "sender_broadcasting",
            ),
            (
                RgbFundingRecoveryStage::Sender(RgbSenderFundingStage::BroadcastCommitted),
                "sender_broadcast_committed",
            ),
            (
                RgbFundingRecoveryStage::Sender(RgbSenderFundingStage::Finalized),
                "sender_finalized",
            ),
            (
                RgbFundingRecoveryStage::Sender(RgbSenderFundingStage::DurablyCompleted),
                "sender_durably_completed",
            ),
            (
                RgbFundingRecoveryStage::Sender(RgbSenderFundingStage::RollingBack),
                "sender_rolling_back",
            ),
            (
                RgbFundingRecoveryStage::Sender(RgbSenderFundingStage::RetryRequired),
                "sender_retry_required",
            ),
            (
                RgbFundingRecoveryStage::Receiver(FundingAcceptanceStage::Validating),
                "receiver_validating",
            ),
            (
                RgbFundingRecoveryStage::Receiver(FundingAcceptanceStage::Prepared),
                "receiver_prepared",
            ),
            (
                RgbFundingRecoveryStage::Receiver(FundingAcceptanceStage::Promoted),
                "receiver_promoted",
            ),
            (
                RgbFundingRecoveryStage::Receiver(FundingAcceptanceStage::Finalizing),
                "receiver_finalizing",
            ),
            (
                RgbFundingRecoveryStage::Receiver(FundingAcceptanceStage::Finalized),
                "receiver_finalized",
            ),
            (
                RgbFundingRecoveryStage::Receiver(FundingAcceptanceStage::RollingBack),
                "receiver_rolling_back",
            ),
            (
                RgbFundingRecoveryStage::Receiver(FundingAcceptanceStage::RetryRequired),
                "receiver_retry_required",
            ),
        ];

        for (stage, expected) in stages {
            assert_eq!(stage.as_str(), expected);
        }
    }

    #[test]
    fn receiver_recovery_decisions_are_exhaustive_and_fail_closed() {
        use FundingAcceptanceStage::*;
        use RgbReceiverRecoveryAction::*;

        for stage in [Validating, Prepared, RollingBack, RetryRequired] {
            assert_eq!(rgb_receiver_recovery_action(stage, false), Rollback);
            assert_eq!(rgb_receiver_recovery_action(stage, true), Quarantine);
        }
        for stage in [Promoted, Finalizing] {
            assert_eq!(rgb_receiver_recovery_action(stage, false), Quarantine);
            assert_eq!(rgb_receiver_recovery_action(stage, true), Finalize);
        }
        assert_eq!(rgb_receiver_recovery_action(Finalized, false), Quarantine);
        assert_eq!(rgb_receiver_recovery_action(Finalized, true), Complete);
    }

    #[test]
    fn finalized_receiver_recovery_is_typed_and_fail_closed() {
        let record = PendingFundingAcceptance {
            version: 3,
            temporary_channel_id: "01".repeat(32),
            counterparty_node_id: format!("02{}", "02".repeat(32)),
            funding_txid: "03".repeat(32),
            funding_output_index: 1,
            push_asset_amount: Some(1),
            stage: FundingAcceptanceStage::Finalized,
            consignment: Some(vec![1]),
            rgb_info: None,
        };

        let unresolved = rgb_receiver_funding_recovery_view(&record, false, None).unwrap();
        assert_eq!(
            unresolved.stage,
            RgbFundingRecoveryStage::Receiver(FundingAcceptanceStage::Finalized)
        );
        assert!(!unresolved.channel_is_durable);
        assert_eq!(unresolved.transaction_is_known, None);
        assert_eq!(
            unresolved.action,
            RgbFundingRecoveryAction::ManualChannelStateRecovery
        );

        let durable = rgb_receiver_funding_recovery_view(&record, true, None).unwrap();
        assert_eq!(
            durable.action,
            RgbFundingRecoveryAction::RetryReconciliation
        );
    }

    #[test]
    fn transient_receiver_reconciliation_failure_preserves_retryable_evidence() {
        let record = PendingFundingAcceptance {
            version: 3,
            temporary_channel_id: "01".repeat(32),
            counterparty_node_id: format!("02{}", "02".repeat(32)),
            funding_txid: "03".repeat(32),
            funding_output_index: 1,
            push_asset_amount: Some(1),
            stage: FundingAcceptanceStage::Prepared,
            consignment: Some(vec![1]),
            rgb_info: None,
        };
        let error = "VSS temporarily unavailable".to_owned();

        let recovery =
            rgb_receiver_funding_recovery_view(&record, false, Some(error.clone())).unwrap();
        assert_eq!(
            recovery.stage,
            RgbFundingRecoveryStage::Receiver(FundingAcceptanceStage::Prepared)
        );
        assert_eq!(
            recovery.action,
            RgbFundingRecoveryAction::RetryReconciliation
        );
        assert_eq!(recovery.error, Some(error));
    }

    #[test]
    fn rgb_funding_recovery_guard_is_fail_closed_and_deterministic() {
        let guard = RgbFundingRecoveryGuard::default();
        let recovery = |funding_txid: &str| RgbFundingRecoveryState {
            funding_txid: funding_txid.to_owned(),
            temporary_channel_id: "01".repeat(32),
            final_channel_id: Some("02".repeat(32)),
            stage: RgbFundingRecoveryStage::Sender(RgbSenderFundingStage::Broadcasting),
            channel_is_durable: false,
            transaction_is_known: None,
            error: Some("indexer unavailable".to_owned()),
            action: RgbFundingRecoveryAction::RetryChainObservation,
        };
        let first = "11".repeat(32);
        let second = "22".repeat(32);
        guard.replace(&[recovery(&second), recovery(&first)]);

        assert_eq!(guard.snapshot(), vec![first.clone(), second.clone()]);
        assert!(matches!(
            guard.lock_rgb_wallet_mutation(),
            Err(APIError::RgbFundingRecoveryRequired(ref txids))
                if txids == &format!("{first},{second}")
        ));

        guard.clear(&first);
        assert!(guard.lock_rgb_wallet_mutation().is_err());
        guard.clear(&second);
        assert!(guard.lock_rgb_wallet_mutation().is_ok());
    }

    #[test]
    fn rgb_wallet_mutation_admission_holds_an_exclusive_lease() {
        let guard = RgbFundingRecoveryGuard::default();

        let operation = guard.lock_rgb_wallet_mutation().unwrap();
        assert!(matches!(
            guard.lock_rgb_wallet_mutation(),
            Err(APIError::ChangingState)
        ));

        drop(operation);
        assert!(guard.lock_rgb_wallet_mutation().is_ok());
    }

    #[tokio::test]
    async fn output_sweeper_waits_for_a_short_wallet_mutation() {
        let guard = Arc::new(RgbFundingRecoveryGuard::default());
        let operation = guard.lock_rgb_wallet_mutation().unwrap();
        let waiter_guard = Arc::clone(&guard);
        let waiter = tokio::spawn(async move {
            waiter_guard
                .lock_rgb_wallet_mutation_for(Duration::from_secs(1), "test-sweeper")
                .await
        });

        tokio::task::yield_now().await;
        drop(operation);

        let sweep_operation = tokio::time::timeout(Duration::from_secs(1), waiter)
            .await
            .expect("sweeper admission should not remain blocked")
            .expect("sweeper admission task should not panic")
            .expect("sweeper should acquire the released wallet lease");
        drop(sweep_operation);
        assert!(guard.lock_rgb_wallet_mutation().is_ok());
    }

    #[tokio::test]
    async fn output_sweeper_wait_is_bounded() {
        let guard = RgbFundingRecoveryGuard::default();
        let _operation = guard.lock_rgb_wallet_mutation().unwrap();

        assert!(matches!(
            guard
                .lock_rgb_wallet_mutation_for(Duration::from_millis(10), "test-sweeper")
                .await,
            Err(APIError::ChangingState)
        ));
    }

    #[tokio::test]
    async fn output_sweeper_remains_blocked_by_recovery_quarantine() {
        let guard = RgbFundingRecoveryGuard::default();
        let funding_txid = "11".repeat(32);
        guard.replace(&[RgbFundingRecoveryState {
            funding_txid: funding_txid.clone(),
            temporary_channel_id: "01".repeat(32),
            final_channel_id: Some("02".repeat(32)),
            stage: RgbFundingRecoveryStage::Sender(RgbSenderFundingStage::Broadcasting),
            channel_is_durable: false,
            transaction_is_known: None,
            error: Some("indexer unavailable".to_owned()),
            action: RgbFundingRecoveryAction::RetryChainObservation,
        }]);

        assert!(matches!(
            guard.lock_output_sweeper_wallet_mutation().await,
            Err(APIError::RgbFundingRecoveryRequired(ref blocked_txid))
                if blocked_txid == &funding_txid
        ));
    }

    #[test]
    fn btc_channel_payments_bypass_rgb_recovery_quarantine() {
        let guard = RgbFundingRecoveryGuard::default();
        let funding_txid = "11".repeat(32);
        guard.replace(&[RgbFundingRecoveryState {
            funding_txid: funding_txid.clone(),
            temporary_channel_id: "01".repeat(32),
            final_channel_id: Some("02".repeat(32)),
            stage: RgbFundingRecoveryStage::Sender(RgbSenderFundingStage::Broadcasting),
            channel_is_durable: false,
            transaction_is_known: None,
            error: Some("indexer unavailable".to_owned()),
            action: RgbFundingRecoveryAction::RetryChainObservation,
        }]);

        assert!(matches!(guard.lock_channel_payment(false), Ok(None)));
        assert!(matches!(
            guard.lock_channel_payment(true),
            Err(APIError::RgbFundingRecoveryRequired(ref blocked_txid))
                if blocked_txid == &funding_txid
        ));
    }

    #[test]
    fn btc_channel_payments_bypass_an_active_rgb_wallet_mutation() {
        let guard = RgbFundingRecoveryGuard::default();
        let _rgb_wallet_operation = guard.lock_rgb_wallet_mutation().unwrap();

        assert!(matches!(guard.lock_channel_payment(false), Ok(None)));
        assert!(matches!(
            guard.lock_channel_payment(true),
            Err(APIError::ChangingState)
        ));
    }

    #[test]
    fn deferred_rgb_consistency_requires_a_durable_owner_or_a_resolved_stock() {
        let operation_id = "03".repeat(32);
        let recovery = RgbFundingRecoveryState {
            funding_txid: operation_id.clone(),
            temporary_channel_id: "01".repeat(32),
            final_channel_id: Some("02".repeat(32)),
            stage: RgbFundingRecoveryStage::Sender(RgbSenderFundingStage::Broadcasting),
            channel_is_durable: true,
            transaction_is_known: None,
            error: None,
            action: RgbFundingRecoveryAction::ResumeBroadcast,
        };

        assert!(!should_complete_deferred_rgb_consistency_check(false, None, &[]).unwrap());
        assert!(should_complete_deferred_rgb_consistency_check(true, None, &[]).unwrap());
        assert!(!should_complete_deferred_rgb_consistency_check(
            true,
            Some(&operation_id),
            &[recovery]
        )
        .unwrap());
        assert!(
            should_complete_deferred_rgb_consistency_check(true, Some(&"04".repeat(32)), &[])
                .is_err()
        );
        assert!(
            should_complete_deferred_rgb_consistency_check(false, Some(&operation_id), &[])
                .is_err()
        );
    }

    #[test]
    fn rgb_sender_funding_journal_round_trips_with_version() {
        let record = RgbSenderFundingRecord {
            version: RgbSenderFundingRecord::VERSION,
            manual_broadcast: true,
            temporary_channel_id: "01".repeat(32),
            final_channel_id: Some("02".repeat(32)),
            funding_txid: "03".repeat(32),
            batch_transfer_idx: 7,
            rgb_info: Some(RgbInfo {
                contract_id: test_contract_id(),
                schema: AssetSchema::Nia,
                local_rgb_amount: 1,
                remote_rgb_amount: 2,
                batch_transfer_idx: Some(7),
                counterparty_knows_asset: false,
            }),
            consignment_delivery: RgbSenderConsignmentDelivery::P2p,
            stage: RgbSenderFundingStage::HandedToLdk,
        };
        record.validate().unwrap();
        let encoded = serde_json::to_vec(&record).unwrap();
        assert_eq!(
            serde_json::from_slice::<RgbSenderFundingRecord>(&encoded).unwrap(),
            record
        );

        let mut unsupported = record.clone();
        unsupported.version += 1;
        assert!(unsupported.validate().is_err());

        let mut malformed = unsupported;
        malformed.version = RgbSenderFundingRecord::VERSION;
        malformed.funding_txid = "zz".repeat(32);
        assert!(malformed.validate().is_err());

        let legacy_json = serde_json::json!({
            "version": RgbSenderFundingRecord::LEGACY_VERSION,
            "temporary_channel_id": "01".repeat(32),
            "final_channel_id": "02".repeat(32),
            "funding_txid": "03".repeat(32),
            "batch_transfer_idx": 7,
            "stage": "handed_to_ldk"
        });
        let legacy: RgbSenderFundingRecord = serde_json::from_value(legacy_json).unwrap();
        assert!(!legacy.manual_broadcast);
        assert!(legacy.rgb_info.is_none());
        assert_eq!(
            legacy.consignment_delivery,
            RgbSenderConsignmentDelivery::Proxy
        );
        legacy.validate().unwrap();

        let mut wrong_delivery = record;
        wrong_delivery.consignment_delivery = RgbSenderConsignmentDelivery::Proxy;
        assert!(wrong_delivery.validate().is_err());

        wrong_delivery.version = RgbSenderFundingRecord::RGB_INFO_VERSION;
        wrong_delivery.consignment_delivery = RgbSenderConsignmentDelivery::P2p;
        assert!(wrong_delivery.validate().is_err());
    }
}
