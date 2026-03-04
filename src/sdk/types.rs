use lightning::impl_writeable_tlv_based_enum;
use rgb_lib::wallet::rust_only::IndexerProtocol as RgbLibIndexerProtocol;
use rgb_lib::wallet::RecipientType as RgbLibRecipientType;
use rgb_lib::wallet::{
    AssetCFA as RgbLibAssetCFA, AssetNIA as RgbLibAssetNIA, AssetUDA as RgbLibAssetUDA,
    EmbeddedMedia as RgbLibEmbeddedMedia, Media as RgbLibMedia,
    ProofOfReserves as RgbLibProofOfReserves, Token as RgbLibToken, TokenLight as RgbLibTokenLight,
};
use rgb_lib::BitcoinNetwork as RgbBitcoinNetwork;
use rgb_lib::{AssetSchema as RgbLibAssetSchema, Assignment as RgbLibAssignment};
use std::collections::HashMap;

pub(crate) struct NodeInfoData {
    pub(crate) pubkey: String,
    pub(crate) num_channels: usize,
    pub(crate) num_usable_channels: usize,
    pub(crate) local_balance_sat: u64,
    pub(crate) eventual_close_fees_sat: u64,
    pub(crate) pending_outbound_payments_sat: u64,
    pub(crate) num_peers: usize,
    pub(crate) account_xpub_vanilla: String,
    pub(crate) account_xpub_colored: String,
    pub(crate) network_nodes: usize,
    pub(crate) network_channels: usize,
}

pub(crate) struct NetworkInfoData {
    pub(crate) network: RgbBitcoinNetwork,
    pub(crate) height: u32,
}

pub(crate) struct AddressData {
    pub(crate) address: String,
}

pub(crate) struct AssetBalanceData {
    pub(crate) settled: u64,
    pub(crate) future: u64,
    pub(crate) spendable: u64,
    pub(crate) offchain_outbound: u64,
    pub(crate) offchain_inbound: u64,
}

pub(crate) struct AssetMetadataData {
    pub(crate) asset_schema: RgbLibAssetSchema,
    pub(crate) initial_supply: u64,
    pub(crate) max_supply: u64,
    pub(crate) known_circulating_supply: u64,
    pub(crate) timestamp: i64,
    pub(crate) name: String,
    pub(crate) precision: u8,
    pub(crate) ticker: Option<String>,
    pub(crate) details: Option<String>,
    pub(crate) token: Option<Token>,
}

pub(crate) struct BtcBalance {
    pub(crate) settled: u64,
    pub(crate) future: u64,
    pub(crate) spendable: u64,
}

pub(crate) struct BtcBalanceData {
    pub(crate) vanilla: BtcBalance,
    pub(crate) colored: BtcBalance,
}

pub(crate) struct DecodeLnInvoiceData {
    pub(crate) amt_msat: Option<u64>,
    pub(crate) expiry_sec: u64,
    pub(crate) timestamp: u64,
    pub(crate) asset_id: Option<String>,
    pub(crate) asset_amount: Option<u64>,
    pub(crate) payment_hash: String,
    pub(crate) payment_secret: String,
    pub(crate) payee_pubkey: Option<String>,
    pub(crate) network: RgbBitcoinNetwork,
}

pub(crate) struct DecodeRgbInvoiceData {
    pub(crate) recipient_id: String,
    pub(crate) recipient_type: RgbLibRecipientType,
    pub(crate) asset_schema: Option<RgbLibAssetSchema>,
    pub(crate) asset_id: Option<String>,
    pub(crate) assignment: RgbLibAssignment,
    pub(crate) network: RgbBitcoinNetwork,
    pub(crate) expiration_timestamp: Option<i64>,
    pub(crate) transport_endpoints: Vec<String>,
}

pub(crate) struct EstimateFeeData {
    pub(crate) fee_rate: f64,
}

pub(crate) struct AssetMediaData {
    pub(crate) bytes_hex: String,
}

pub(crate) struct ChannelIdData {
    pub(crate) channel_id: String,
}

pub(crate) struct InvoiceStatusData {
    pub(crate) status: InvoiceStatus,
}

pub(crate) struct CheckIndexerUrlData {
    pub(crate) indexer_protocol: RgbLibIndexerProtocol,
}

pub(crate) struct SignMessageData {
    pub(crate) signed_message: String,
}

pub(crate) struct SendRgbData {
    pub(crate) txid: String,
    pub(crate) batch_transfer_idx: i32,
}

pub(crate) struct ListAssetsData {
    pub(crate) nia: Option<Vec<AssetNIA>>,
    pub(crate) uda: Option<Vec<AssetUDA>>,
    pub(crate) cfa: Option<Vec<AssetCFA>>,
}

pub(crate) struct LnInvoiceData {
    pub(crate) invoice: String,
}

pub(crate) struct PaymentData {
    pub(crate) amt_msat: Option<u64>,
    pub(crate) asset_amount: Option<u64>,
    pub(crate) asset_id: Option<String>,
    pub(crate) payment_hash: String,
    pub(crate) inbound: bool,
    pub(crate) status: HtlcStatus,
    pub(crate) created_at: u64,
    pub(crate) updated_at: u64,
    pub(crate) payee_pubkey: String,
}

pub(crate) struct ChannelData {
    pub(crate) channel_id: String,
    pub(crate) funding_txid: Option<String>,
    pub(crate) peer_pubkey: String,
    pub(crate) peer_alias: Option<String>,
    pub(crate) short_channel_id: Option<u64>,
    pub(crate) status: ChannelStatus,
    pub(crate) ready: bool,
    pub(crate) capacity_sat: u64,
    pub(crate) local_balance_sat: u64,
    pub(crate) outbound_balance_msat: u64,
    pub(crate) inbound_balance_msat: u64,
    pub(crate) next_outbound_htlc_limit_msat: u64,
    pub(crate) next_outbound_htlc_minimum_msat: u64,
    pub(crate) is_usable: bool,
    pub(crate) public: bool,
    pub(crate) asset_id: Option<String>,
    pub(crate) asset_local_amount: Option<u64>,
    pub(crate) asset_remote_amount: Option<u64>,
}

pub(crate) struct TransactionData {
    pub(crate) transaction_type: TransactionType,
    pub(crate) txid: String,
    pub(crate) received: u64,
    pub(crate) sent: u64,
    pub(crate) fee: u64,
    pub(crate) confirmation_time: Option<BlockTime>,
}

pub(crate) struct TransferTransportEndpointData {
    pub(crate) endpoint: String,
    pub(crate) transport_type: TransportType,
    pub(crate) used: bool,
}

pub(crate) struct TransferData {
    pub(crate) idx: i32,
    pub(crate) created_at: i64,
    pub(crate) updated_at: i64,
    pub(crate) status: TransferStatus,
    pub(crate) requested_assignment: Option<RgbLibAssignment>,
    pub(crate) assignments: Vec<RgbLibAssignment>,
    pub(crate) kind: TransferKind,
    pub(crate) txid: Option<String>,
    pub(crate) recipient_id: Option<String>,
    pub(crate) receive_utxo: Option<String>,
    pub(crate) change_utxo: Option<String>,
    pub(crate) expiration: Option<i64>,
    pub(crate) transport_endpoints: Vec<TransferTransportEndpointData>,
}

pub(crate) struct RgbAllocationData {
    pub(crate) asset_id: Option<String>,
    pub(crate) assignment: RgbLibAssignment,
    pub(crate) settled: bool,
}

pub(crate) struct UtxoData {
    pub(crate) outpoint: String,
    pub(crate) btc_amount: u64,
    pub(crate) colorable: bool,
}

pub(crate) struct UnspentData {
    pub(crate) utxo: UtxoData,
    pub(crate) rgb_allocations: Vec<RgbAllocationData>,
}

pub(crate) struct PeerData {
    pub(crate) pubkey: String,
}

pub(crate) struct SwapViewData {
    pub(crate) qty_from: u64,
    pub(crate) qty_to: u64,
    pub(crate) from_asset: Option<String>,
    pub(crate) to_asset: Option<String>,
    pub(crate) payment_hash: String,
    pub(crate) status: SwapStatus,
    pub(crate) requested_at: u64,
    pub(crate) initiated_at: Option<u64>,
    pub(crate) expires_at: u64,
    pub(crate) completed_at: Option<u64>,
}

pub(crate) struct SwapListData {
    pub(crate) taker: Vec<SwapViewData>,
    pub(crate) maker: Vec<SwapViewData>,
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) enum ChannelStatus {
    #[default]
    Opening,
    Opened,
    Closing,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum HtlcStatus {
    Pending,
    Succeeded,
    Failed,
}

impl std::fmt::Display for HtlcStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let label = match self {
            HtlcStatus::Pending => "Pending",
            HtlcStatus::Succeeded => "Succeeded",
            HtlcStatus::Failed => "Failed",
        };
        write!(f, "{label}")
    }
}

impl_writeable_tlv_based_enum!(HtlcStatus,
    (0, Pending) => {},
    (1, Succeeded) => {},
    (2, Failed) => {},
);

#[derive(Clone, Copy, Debug)]
pub(crate) enum InvoiceStatus {
    Pending,
    Succeeded,
    Failed,
    Expired,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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

#[derive(Debug)]
pub(crate) struct BlockTime {
    pub(crate) height: u32,
    pub(crate) timestamp: u64,
}

#[derive(Debug, PartialEq)]
pub(crate) enum TransactionType {
    RgbSend,
    Drain,
    CreateUtxos,
    User,
}

#[derive(Debug, PartialEq)]
pub(crate) enum TransferKind {
    Issuance,
    ReceiveBlind,
    ReceiveWitness,
    Send,
    Inflation,
}

#[derive(Debug, PartialEq)]
pub(crate) enum TransferStatus {
    WaitingCounterparty,
    WaitingConfirmations,
    Settled,
    Failed,
}

#[derive(Debug)]
pub(crate) enum TransportType {
    JsonRpc,
}

pub(crate) struct EmbeddedMedia {
    pub(crate) mime: String,
    pub(crate) data: Vec<u8>,
}

impl From<RgbLibEmbeddedMedia> for EmbeddedMedia {
    fn from(value: RgbLibEmbeddedMedia) -> Self {
        Self {
            mime: value.mime,
            data: value.data,
        }
    }
}

pub(crate) struct Media {
    pub(crate) file_path: String,
    pub(crate) digest: String,
    pub(crate) mime: String,
}

impl From<RgbLibMedia> for Media {
    fn from(value: RgbLibMedia) -> Self {
        Self {
            file_path: value.file_path,
            digest: value.digest,
            mime: value.mime,
        }
    }
}

pub(crate) struct ProofOfReserves {
    pub(crate) utxo: String,
    pub(crate) proof: Vec<u8>,
}

impl From<RgbLibProofOfReserves> for ProofOfReserves {
    fn from(value: RgbLibProofOfReserves) -> Self {
        Self {
            utxo: value.utxo.to_string(),
            proof: value.proof,
        }
    }
}

pub(crate) struct Token {
    pub(crate) index: u32,
    pub(crate) ticker: Option<String>,
    pub(crate) name: Option<String>,
    pub(crate) details: Option<String>,
    pub(crate) embedded_media: Option<EmbeddedMedia>,
    pub(crate) media: Option<Media>,
    pub(crate) attachments: HashMap<u8, Media>,
    pub(crate) reserves: Option<ProofOfReserves>,
}

impl From<RgbLibToken> for Token {
    fn from(value: RgbLibToken) -> Self {
        Self {
            index: value.index,
            ticker: value.ticker,
            name: value.name,
            details: value.details,
            embedded_media: value.embedded_media.map(Into::into),
            media: value.media.map(Into::into),
            attachments: value
                .attachments
                .into_iter()
                .map(|(k, v)| (k, v.into()))
                .collect(),
            reserves: value.reserves.map(Into::into),
        }
    }
}

pub(crate) struct TokenLight {
    pub(crate) index: u32,
    pub(crate) ticker: Option<String>,
    pub(crate) name: Option<String>,
    pub(crate) details: Option<String>,
    pub(crate) embedded_media: bool,
    pub(crate) media: Option<Media>,
    pub(crate) attachments: HashMap<u8, Media>,
    pub(crate) reserves: bool,
}

impl From<RgbLibTokenLight> for TokenLight {
    fn from(value: RgbLibTokenLight) -> Self {
        Self {
            index: value.index,
            ticker: value.ticker,
            name: value.name,
            details: value.details,
            embedded_media: value.embedded_media,
            media: value.media.map(Into::into),
            attachments: value
                .attachments
                .into_iter()
                .map(|(k, v)| (k, v.into()))
                .collect(),
            reserves: value.reserves,
        }
    }
}

pub(crate) struct AssetBalance {
    pub(crate) settled: u64,
    pub(crate) future: u64,
    pub(crate) spendable: u64,
    pub(crate) offchain_outbound: u64,
    pub(crate) offchain_inbound: u64,
}

pub(crate) struct AssetNIA {
    pub(crate) asset_id: String,
    pub(crate) ticker: String,
    pub(crate) name: String,
    pub(crate) details: Option<String>,
    pub(crate) precision: u8,
    pub(crate) issued_supply: u64,
    pub(crate) timestamp: i64,
    pub(crate) added_at: i64,
    pub(crate) balance: AssetBalance,
    pub(crate) media: Option<Media>,
}

impl From<RgbLibAssetNIA> for AssetNIA {
    fn from(value: RgbLibAssetNIA) -> Self {
        Self {
            asset_id: value.asset_id,
            ticker: value.ticker,
            name: value.name,
            details: value.details,
            precision: value.precision,
            issued_supply: value.issued_supply,
            timestamp: value.timestamp,
            added_at: value.added_at,
            balance: AssetBalance {
                settled: value.balance.settled,
                future: value.balance.future,
                spendable: value.balance.spendable,
                offchain_outbound: 0,
                offchain_inbound: 0,
            },
            media: value.media.map(Into::into),
        }
    }
}

pub(crate) struct AssetUDA {
    pub(crate) asset_id: String,
    pub(crate) ticker: String,
    pub(crate) name: String,
    pub(crate) details: Option<String>,
    pub(crate) precision: u8,
    pub(crate) timestamp: i64,
    pub(crate) added_at: i64,
    pub(crate) balance: AssetBalance,
    pub(crate) token: Option<TokenLight>,
}

impl From<RgbLibAssetUDA> for AssetUDA {
    fn from(value: RgbLibAssetUDA) -> Self {
        Self {
            asset_id: value.asset_id,
            ticker: value.ticker,
            name: value.name,
            details: value.details,
            precision: value.precision,
            timestamp: value.timestamp,
            added_at: value.added_at,
            balance: AssetBalance {
                settled: value.balance.settled,
                future: value.balance.future,
                spendable: value.balance.spendable,
                offchain_outbound: 0,
                offchain_inbound: 0,
            },
            token: value.token.map(Into::into),
        }
    }
}

pub(crate) struct AssetCFA {
    pub(crate) asset_id: String,
    pub(crate) name: String,
    pub(crate) details: Option<String>,
    pub(crate) precision: u8,
    pub(crate) issued_supply: u64,
    pub(crate) timestamp: i64,
    pub(crate) added_at: i64,
    pub(crate) balance: AssetBalance,
    pub(crate) media: Option<Media>,
}

impl From<RgbLibAssetCFA> for AssetCFA {
    fn from(value: RgbLibAssetCFA) -> Self {
        Self {
            asset_id: value.asset_id,
            name: value.name,
            details: value.details,
            precision: value.precision,
            issued_supply: value.issued_supply,
            timestamp: value.timestamp,
            added_at: value.added_at,
            balance: AssetBalance {
                settled: value.balance.settled,
                future: value.balance.future,
                spendable: value.balance.spendable,
                offchain_outbound: 0,
                offchain_inbound: 0,
            },
            media: value.media.map(Into::into),
        }
    }
}
