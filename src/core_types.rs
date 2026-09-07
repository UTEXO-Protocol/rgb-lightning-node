use crate::signer::{BootstrapData, ExternalSignerAttachment};
use lightning::impl_writeable_tlv_based_enum;
use rgb_lib::bdk_wallet::keys::bip39::Mnemonic;
use serde::{Deserialize, Serialize};

pub(crate) const FEE_RATE: u64 = 7;
// matches lnd's MinChanFundingSize
pub(crate) const UTXO_SIZE_SAT: u32 = 20000;
pub(crate) const MIN_CHANNEL_CONFIRMATIONS: u8 = 3;
pub(crate) const DUST_LIMIT_MSAT: u64 = 546000;
// must clear the commitment dust limit plus HTLC-tx fees: RGB assets ride the
// HTLC output, so a dust-trimmed HTLC cannot settle an asset payment
pub(crate) const HTLC_MIN_MSAT: u64 = 3_000_000;
pub(crate) const VIRTUAL_HTLC_MIN_MSAT: u64 = 1_000;
pub(crate) const MAX_SWAP_FEE_MSAT: u64 = 3_000_000;
pub(crate) const PENDING_SWAP_TIMEOUT_SECS: u64 = 24 * 60 * 60;
pub(crate) const DEFAULT_FINAL_CLTV_EXPIRY_DELTA: u32 = 14;

pub mod asset_link {
    use serde::{Deserialize, Serialize};

    #[derive(Clone, Debug, Deserialize, Serialize)]
    pub(crate) struct AssetLinkRequest {
        pub(crate) parent_asset_id: String,
        pub(crate) child_asset_id: String,
        pub(crate) min_confirmations: u8,
    }

    #[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
    pub(crate) struct AssetLinkResponse {
        pub(crate) parent_asset_id: String,
        pub(crate) child_asset_id: Option<String>,
        pub(crate) created_at: Option<u64>,
        pub(crate) txid: Option<String>,
    }
}

pub mod async_order {
    use crate::async_order::{AsyncOrderNewHashWire, AsyncOrderRequestInvoiceParamsWire};
    use serde::{Deserialize, Serialize};

    #[derive(Clone, Debug, Deserialize, Serialize)]
    pub(crate) struct AsyncOrderNewRequest {
        pub(crate) host_node_id: String,
        #[serde(default)]
        pub(crate) username: Option<String>,
        #[serde(default)]
        pub(crate) domain: Option<String>,
    }

    #[derive(Clone, Debug, Deserialize, Serialize)]
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

    #[derive(Clone, Debug, Deserialize, Serialize)]
    pub(crate) struct AsyncOrderOutboundInvoiceRequest {
        pub(crate) client_node_id: String,
        pub(crate) params: AsyncOrderRequestInvoiceParamsWire,
    }

    #[derive(Clone, Debug, Deserialize, Serialize)]
    pub(crate) struct AsyncOrderOutboundInvoiceResponse {
        pub(crate) payment_hash: String,
        pub(crate) bolt11: String,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Deserialize, Serialize)]
pub(crate) enum HTLCStatus {
    Pending,
    Succeeded,
    Failed,
    Claimable,
    Claiming,
    Cancelled,
}

impl std::fmt::Display for HTLCStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let label = match self {
            HTLCStatus::Pending => "Pending",
            HTLCStatus::Succeeded => "Succeeded",
            HTLCStatus::Failed => "Failed",
            HTLCStatus::Claimable => "Claimable",
            HTLCStatus::Claiming => "Claiming",
            HTLCStatus::Cancelled => "Cancelled",
        };
        write!(f, "{label}")
    }
}

impl_writeable_tlv_based_enum!(HTLCStatus,
    (0, Pending) => {},
    (1, Succeeded) => {},
    (2, Failed) => {},
    (3, Claimable) => {},
    (4, Claiming) => {},
    (5, Cancelled) => {},
);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub(crate) enum SwapStatus {
    Waiting,
    Pending,
    Succeeded,
    Expired,
    Failed,
}

impl_writeable_tlv_based_enum!(SwapStatus,
    (0, Waiting) => {},
    (1, Pending) => {},
    (2, Succeeded) => {},
    (3, Expired) => {},
    (4, Failed) => {},
);

/// How LDK follows the chain: full blocks from bitcoind, or the indexer.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "mode", content = "config")]
pub(crate) enum LdkChainSync {
    #[cfg(feature = "block-sync")]
    BlockSync {
        bitcoind_rpc_username: String,
        bitcoind_rpc_password: String,
        bitcoind_rpc_host: String,
        bitcoind_rpc_port: u16,
    },
    #[cfg(feature = "transaction-sync")]
    TransactionSync { indexer_url: String },
}

#[derive(Clone, Debug)]
pub(crate) struct UnlockRequest {
    pub(crate) ldk_chain_sync: LdkChainSync,
    pub(crate) indexer_url: Option<String>,
    pub(crate) proxy_endpoint: Option<String>,
    pub(crate) announce_addresses: Vec<String>,
    pub(crate) announce_alias: Option<String>,
    pub(crate) gossip_source: Option<crate::gossip::GossipSourceConfig>,
}

#[derive(Clone)]
pub(crate) struct ExternalKeySource {
    pub(crate) bootstrap: BootstrapData,
    pub(crate) signer_attachment: ExternalSignerAttachment,
}

#[derive(Clone)]
#[allow(clippy::large_enum_variant)]
pub(crate) enum NodeKeySource {
    InternalMnemonic(Mnemonic),
    #[allow(dead_code)]
    External(ExternalKeySource),
}
