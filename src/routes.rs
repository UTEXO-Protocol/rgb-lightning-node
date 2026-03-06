use amplify::{map, s, Display};
use axum::{
    extract::{Multipart, State},
    Json,
};
use axum_extra::extract::WithRejection;
use biscuit_auth::Biscuit;
use bitcoin::hashes::sha256::{self, Hash as Sha256};
use bitcoin::hashes::Hash;
use bitcoin::secp256k1::PublicKey;
use bitcoin::{Network, ScriptBuf};
use hex::DisplayHex;
use lightning::impl_writeable_tlv_based_enum;
use lightning::ln::{channelmanager::OptionalOfferPaymentParams, types::ChannelId};
use lightning::offers::offer::{self, Offer};
use lightning::onion_message::messenger::Destination;
use lightning::rgb_utils::{get_rgb_channel_info_path, STATIC_BLINDING};
use lightning::routing::gossip::RoutingFees;
use lightning::routing::router::{Path as LnPath, Route, RouteHint, RouteHintHop};
use lightning::sign::EntropySource;
use lightning::util::config::ChannelConfig;
use lightning::{
    ln::channel_state::ChannelShutdownState, onion_message::messenger::MessageSendInstructions,
};
use lightning::{
    ln::channelmanager::{PaymentId, RecipientOnionFields, Retry},
    rgb_utils::{write_rgb_channel_info, write_rgb_payment_info_file, RgbInfo},
    routing::router::{PaymentParameters, RouteParameters},
    util::config::{ChannelHandshakeConfig, ChannelHandshakeLimits, UserConfig},
    util::{errors::APIError as LDKAPIError, IS_SWAP_SCID},
};
use lightning::{
    routing::router::RouteParametersConfig,
    types::payment::{PaymentHash, PaymentPreimage},
};
use lightning_invoice::{Bolt11Invoice, PaymentSecret};
use regex::Regex;
use rgb_lib::{
    generate_keys,
    utils::recipient_id_from_script_buf,
    wallet::{
        rust_only::IndexerProtocol as RgbLibIndexerProtocol, AssetCFA as RgbLibAssetCFA,
        AssetNIA as RgbLibAssetNIA, AssetUDA as RgbLibAssetUDA, Balance as RgbLibBalance,
        EmbeddedMedia as RgbLibEmbeddedMedia, Media as RgbLibMedia,
        ProofOfReserves as RgbLibProofOfReserves, Recipient as RgbLibRecipient,
        RecipientType as RgbLibRecipientType, Token as RgbLibToken, TokenLight as RgbLibTokenLight,
        WitnessData as RgbLibWitnessData,
    },
    AssetSchema as RgbLibAssetSchema, Assignment as RgbLibAssignment,
    BitcoinNetwork as RgbLibNetwork, ContractId, RgbTransport,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    net::ToSocketAddrs,
    path::{Path, PathBuf},
    str::FromStr,
    sync::Arc,
    time::Duration,
};
use tokio::{
    fs::File,
    io::{AsyncReadExt, AsyncWriteExt, BufReader},
    sync::MutexGuard as TokioMutexGuard,
};

use crate::ldk::{start_ldk, stop_ldk, LdkBackgroundServices, MIN_CHANNEL_CONFIRMATIONS};
use crate::sdk;
use crate::swap::{SwapData, SwapInfo, SwapString};
use crate::utils::{
    check_already_initialized, check_channel_id, check_password_strength, check_password_validity,
    encrypt_and_save_mnemonic, get_max_local_rgb_amount, get_mnemonic_path, get_route, hex_str,
    hex_str_to_compressed_pubkey, hex_str_to_vec, UnlockedAppState, UserOnionMessageContents,
};
use crate::{
    backup::{do_backup, restore_backup},
    rgb::get_rgb_channel_info_optional,
};
use crate::{
    disk::{self, CHANNEL_PEER_DATA},
    error::APIError,
    ldk::{PaymentInfo, FEE_RATE, UTXO_SIZE_SAT},
    utils::{
        connect_peer_if_necessary, get_current_timestamp, no_cancel, parse_peer_info, AppState,
    },
};

const UTXO_NUM: u8 = 4;

pub(crate) const HTLC_MIN_MSAT: u64 = 3000000;
pub(crate) const MAX_SWAP_FEE_MSAT: u64 = HTLC_MIN_MSAT;

const OPENRGBCHANNEL_MIN_SAT: u64 = HTLC_MIN_MSAT / 1000 * 10 + 10;
const OPENCHANNEL_MIN_SAT: u64 = 5506;
const OPENCHANNEL_MAX_SAT: u64 = 16777215;
const OPENCHANNEL_MIN_RGB_AMT: u64 = 1;

pub const DUST_LIMIT_MSAT: u64 = 546000;

pub(crate) const INVOICE_MIN_MSAT: u64 = HTLC_MIN_MSAT;

pub(crate) const DEFAULT_FINAL_CLTV_EXPIRY_DELTA: u32 = 14;

impl AppState {
    fn check_changing_state(&self) -> Result<(), APIError> {
        if *self.get_changing_state() {
            return Err(APIError::ChangingState);
        }
        Ok(())
    }

    pub(crate) async fn check_locked(
        &self,
    ) -> Result<TokioMutexGuard<'_, Option<Arc<UnlockedAppState>>>, APIError> {
        self.check_changing_state()?;
        let unlocked_app_state = self.get_unlocked_app_state().await;
        if unlocked_app_state.is_some() {
            Err(APIError::UnlockedNode)
        } else {
            Ok(unlocked_app_state)
        }
    }

    pub(crate) async fn check_unlocked(
        &self,
    ) -> Result<TokioMutexGuard<'_, Option<Arc<UnlockedAppState>>>, APIError> {
        self.check_changing_state()?;
        let unlocked_app_state = self.get_unlocked_app_state().await;
        if unlocked_app_state.is_none() {
            Err(APIError::LockedNode)
        } else {
            Ok(unlocked_app_state)
        }
    }

    pub(crate) fn update_changing_state(&self, updated: bool) {
        let mut changing_state = self.get_changing_state();
        *changing_state = updated;
    }

    pub(crate) fn update_ldk_background_services(&self, updated: Option<LdkBackgroundServices>) {
        let mut ldk_background_services = self.get_ldk_background_services();
        *ldk_background_services = updated;
    }

    pub(crate) async fn update_unlocked_app_state(&self, updated: Option<Arc<UnlockedAppState>>) {
        let mut unlocked_app_state = self.get_unlocked_app_state().await;
        *unlocked_app_state = updated;
    }
}



#[derive(Deserialize, Serialize)]
pub(crate) struct AddressResponse {
    pub(crate) address: String,
}

#[derive(Deserialize, Serialize)]
pub(crate) struct AssetBalanceRequest {
    pub(crate) asset_id: String,
}

#[derive(Deserialize, Serialize)]
pub(crate) struct AssetBalanceResponse {
    pub(crate) settled: u64,
    pub(crate) future: u64,
    pub(crate) spendable: u64,
    pub(crate) offchain_outbound: u64,
    pub(crate) offchain_inbound: u64,
}

#[derive(Deserialize, Serialize)]
pub(crate) struct AssetCFA {
    pub(crate) asset_id: String,
    pub(crate) name: String,
    pub(crate) details: Option<String>,
    pub(crate) precision: u8,
    pub(crate) issued_supply: u64,
    pub(crate) timestamp: i64,
    pub(crate) added_at: i64,
    pub(crate) balance: AssetBalanceResponse,
    pub(crate) media: Option<Media>,
}

#[derive(Deserialize, Serialize)]
pub(crate) struct AssetMetadataRequest {
    pub(crate) asset_id: String,
}

#[derive(Deserialize, Serialize)]
pub(crate) struct AssetMetadataResponse {
    pub(crate) asset_schema: AssetSchema,
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

#[derive(Deserialize, Serialize)]
pub(crate) struct AssetNIA {
    pub(crate) asset_id: String,
    pub(crate) ticker: String,
    pub(crate) name: String,
    pub(crate) details: Option<String>,
    pub(crate) precision: u8,
    pub(crate) issued_supply: u64,
    pub(crate) timestamp: i64,
    pub(crate) added_at: i64,
    pub(crate) balance: AssetBalanceResponse,
    pub(crate) media: Option<Media>,
}

#[derive(Deserialize, Serialize)]
pub(crate) enum AssetSchema {
    Nia,
    Uda,
    Cfa,
}

#[derive(Deserialize, Serialize)]
pub(crate) struct AssetUDA {
    pub(crate) asset_id: String,
    pub(crate) ticker: String,
    pub(crate) name: String,
    pub(crate) details: Option<String>,
    pub(crate) precision: u8,
    pub(crate) timestamp: i64,
    pub(crate) added_at: i64,
    pub(crate) balance: AssetBalanceResponse,
    pub(crate) token: Option<TokenLight>,
}

#[derive(Debug, PartialEq, Deserialize, Serialize)]
#[serde(tag = "type", content = "value")]
pub(crate) enum Assignment {
    Fungible(u64),
    NonFungible,
    InflationRight(u64),
    ReplaceRight,
    Any,
}

#[derive(Deserialize, Serialize)]
pub(crate) struct BackupRequest {
    pub(crate) backup_path: String,
    pub(crate) password: String,
}

#[derive(Debug, PartialEq, Deserialize, Serialize)]
pub(crate) enum BitcoinNetwork {
    Mainnet,
    Testnet,
    Testnet4,
    Signet,
    Regtest,
}

#[derive(Debug, Deserialize, Serialize)]
pub(crate) struct BlockTime {
    pub(crate) height: u32,
    pub(crate) timestamp: u64,
}

#[derive(Debug, Deserialize, Serialize)]
pub(crate) struct BtcBalance {
    pub(crate) settled: u64,
    pub(crate) future: u64,
    pub(crate) spendable: u64,
}

#[derive(Debug, Deserialize, Serialize)]
pub(crate) struct BtcBalanceRequest {
    pub(crate) skip_sync: bool,
}

#[derive(Debug, Deserialize, Serialize)]
pub(crate) struct BtcBalanceResponse {
    pub(crate) vanilla: BtcBalance,
    pub(crate) colored: BtcBalance,
}

#[derive(Debug, Deserialize, Serialize)]
pub(crate) struct ChangePasswordRequest {
    pub(crate) old_password: String,
    pub(crate) new_password: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub(crate) struct Channel {
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

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub(crate) enum ChannelStatus {
    #[default]
    Opening,
    Opened,
    Closing,
}

#[derive(Debug, Deserialize, Serialize)]
pub(crate) struct CheckIndexerUrlRequest {
    pub(crate) indexer_url: String,
}

#[derive(Debug, Deserialize, Serialize)]
pub(crate) struct CheckIndexerUrlResponse {
    pub(crate) indexer_protocol: IndexerProtocol,
}

#[derive(Debug, Deserialize, Serialize)]
pub(crate) struct CheckProxyEndpointRequest {
    pub(crate) proxy_endpoint: String,
}

#[derive(Deserialize, Serialize)]
pub(crate) struct CloseChannelRequest {
    pub(crate) channel_id: String,
    pub(crate) peer_pubkey: String,
    pub(crate) force: bool,
}

#[derive(Deserialize, Serialize)]
pub(crate) struct ConnectPeerRequest {
    pub(crate) peer_pubkey_and_addr: String,
}

#[derive(Deserialize, Serialize)]
pub(crate) struct CreateUtxosRequest {
    pub(crate) up_to: bool,
    pub(crate) num: Option<u8>,
    pub(crate) size: Option<u32>,
    pub(crate) fee_rate: u64,
    pub(crate) skip_sync: bool,
}

#[derive(Deserialize, Serialize)]
pub(crate) struct DecodeLNInvoiceRequest {
    pub(crate) invoice: String,
}

#[derive(Deserialize, Serialize)]
pub(crate) struct DecodeLNInvoiceResponse {
    pub(crate) amt_msat: Option<u64>,
    pub(crate) expiry_sec: u64,
    pub(crate) timestamp: u64,
    pub(crate) asset_id: Option<String>,
    pub(crate) asset_amount: Option<u64>,
    pub(crate) payment_hash: String,
    pub(crate) payment_secret: String,
    pub(crate) payee_pubkey: Option<String>,
    pub(crate) network: BitcoinNetwork,
}

#[derive(Deserialize, Serialize)]
pub(crate) struct DecodeRGBInvoiceRequest {
    pub(crate) invoice: String,
}

#[derive(Deserialize, Serialize)]
pub(crate) struct DecodeRGBInvoiceResponse {
    pub(crate) recipient_id: String,
    pub(crate) recipient_type: RecipientType,
    pub(crate) asset_schema: Option<AssetSchema>,
    pub(crate) asset_id: Option<String>,
    pub(crate) assignment: Assignment,
    pub(crate) network: BitcoinNetwork,
    pub(crate) expiration_timestamp: Option<i64>,
    pub(crate) transport_endpoints: Vec<String>,
}

#[derive(Deserialize, Serialize)]
pub(crate) struct DisconnectPeerRequest {
    pub(crate) peer_pubkey: String,
}

#[derive(Deserialize, Serialize)]
pub(crate) struct EmbeddedMedia {
    pub(crate) mime: String,
    pub(crate) data: Vec<u8>,
}

#[derive(Deserialize, Serialize)]
pub(crate) struct EmptyResponse {}

#[derive(Deserialize, Serialize)]
pub(crate) struct EstimateFeeRequest {
    pub(crate) blocks: u16,
}

#[derive(Deserialize, Serialize)]
pub(crate) struct EstimateFeeResponse {
    pub(crate) fee_rate: f64,
}

#[derive(Deserialize, Serialize)]
pub(crate) struct FailTransfersRequest {
    pub(crate) batch_transfer_idx: Option<i32>,
    pub(crate) no_asset_only: bool,
    pub(crate) skip_sync: bool,
}

#[derive(Deserialize, Serialize)]
pub(crate) struct FailTransfersResponse {
    pub(crate) transfers_changed: bool,
}

#[derive(Deserialize, Serialize)]
pub(crate) struct GetAssetMediaRequest {
    pub(crate) digest: String,
}

#[derive(Deserialize, Serialize)]
pub(crate) struct GetAssetMediaResponse {
    pub(crate) bytes_hex: String,
}

#[derive(Deserialize, Serialize)]
pub(crate) struct GetChannelIdRequest {
    pub(crate) temporary_channel_id: String,
}

#[derive(Deserialize, Serialize)]
pub(crate) struct GetChannelIdResponse {
    pub(crate) channel_id: String,
}

#[derive(Deserialize, Serialize)]
pub(crate) struct GetPaymentRequest {
    pub(crate) payment_hash: String,
}

#[derive(Deserialize, Serialize)]
pub(crate) struct GetPaymentResponse {
    pub(crate) payment: Payment,
}

#[derive(Deserialize, Serialize)]
pub(crate) struct GetSwapRequest {
    pub(crate) payment_hash: String,
    pub(crate) taker: bool,
}

#[derive(Deserialize, Serialize)]
pub(crate) struct GetSwapResponse {
    pub(crate) swap: Swap,
}

#[derive(Clone, Copy, Debug, PartialEq, Deserialize, Serialize, Display)]
#[display(inner)]
pub(crate) enum HTLCStatus {
    Pending,
    Succeeded,
    Failed,
}

impl_writeable_tlv_based_enum!(HTLCStatus,
    (0, Pending) => {},
    (1, Succeeded) => {},
    (2, Failed) => {},
);

#[derive(Debug, Deserialize, Serialize)]
pub(crate) enum IndexerProtocol {
    Electrum,
    Esplora,
}

#[derive(Deserialize, Serialize)]
pub(crate) struct InitRequest {
    pub(crate) password: String,
}

#[derive(Deserialize, Serialize)]
pub(crate) struct InitResponse {
    pub(crate) mnemonic: String,
}

#[derive(Clone, Copy, Deserialize, Serialize)]
pub(crate) enum InvoiceStatus {
    Pending,
    Succeeded,
    Failed,
    Expired,
}

#[derive(Deserialize, Serialize)]
pub(crate) struct InvoiceStatusRequest {
    pub(crate) invoice: String,
}

#[derive(Deserialize, Serialize)]
pub(crate) struct InvoiceStatusResponse {
    pub(crate) status: InvoiceStatus,
}

#[derive(Deserialize, Serialize)]
pub(crate) struct IssueAssetCFARequest {
    pub(crate) amounts: Vec<u64>,
    pub(crate) name: String,
    pub(crate) details: Option<String>,
    pub(crate) precision: u8,
    pub(crate) file_digest: Option<String>,
}

#[derive(Deserialize, Serialize)]
pub(crate) struct IssueAssetCFAResponse {
    pub(crate) asset: AssetCFA,
}

#[derive(Deserialize, Serialize)]
pub(crate) struct IssueAssetNIARequest {
    pub(crate) amounts: Vec<u64>,
    pub(crate) ticker: String,
    pub(crate) name: String,
    pub(crate) precision: u8,
}

#[derive(Deserialize, Serialize)]
pub(crate) struct IssueAssetNIAResponse {
    pub(crate) asset: AssetNIA,
}

#[derive(Deserialize, Serialize)]
pub(crate) struct IssueAssetUDARequest {
    pub(crate) ticker: String,
    pub(crate) name: String,
    pub(crate) details: Option<String>,
    pub(crate) precision: u8,
    pub(crate) media_file_digest: Option<String>,
    pub(crate) attachments_file_digests: Vec<String>,
}

#[derive(Deserialize, Serialize)]
pub(crate) struct IssueAssetUDAResponse {
    pub(crate) asset: AssetUDA,
}

#[derive(Deserialize, Serialize)]
pub(crate) struct KeysendRequest {
    pub(crate) dest_pubkey: String,
    pub(crate) amt_msat: u64,
    pub(crate) asset_id: Option<String>,
    pub(crate) asset_amount: Option<u64>,
}

#[derive(Deserialize, Serialize)]
pub(crate) struct KeysendResponse {
    pub(crate) payment_hash: String,
    pub(crate) payment_preimage: String,
    pub(crate) status: HTLCStatus,
}

#[derive(Deserialize, Serialize)]
pub(crate) struct ListAssetsRequest {
    pub(crate) filter_asset_schemas: Vec<AssetSchema>,
}

#[derive(Deserialize, Serialize)]
pub(crate) struct ListAssetsResponse {
    pub(crate) nia: Option<Vec<AssetNIA>>,
    pub(crate) uda: Option<Vec<AssetUDA>>,
    pub(crate) cfa: Option<Vec<AssetCFA>>,
}

#[derive(Deserialize, Serialize)]
pub(crate) struct ListChannelsResponse {
    pub(crate) channels: Vec<Channel>,
}

#[derive(Deserialize, Serialize)]
pub(crate) struct ListPaymentsResponse {
    pub(crate) payments: Vec<Payment>,
}

#[derive(Deserialize, Serialize)]
pub(crate) struct ListPeersResponse {
    pub(crate) peers: Vec<Peer>,
}

#[derive(Debug, Deserialize, Serialize)]
pub(crate) struct ListSwapsResponse {
    pub(crate) maker: Vec<Swap>,
    pub(crate) taker: Vec<Swap>,
}

#[derive(Deserialize, Serialize)]
pub(crate) struct ListTransactionsRequest {
    pub(crate) skip_sync: bool,
}

#[derive(Deserialize, Serialize)]
pub(crate) struct ListTransactionsResponse {
    pub(crate) transactions: Vec<Transaction>,
}

#[derive(Deserialize, Serialize)]
pub(crate) struct ListTransfersRequest {
    pub(crate) asset_id: String,
}

#[derive(Deserialize, Serialize)]
pub(crate) struct ListTransfersResponse {
    pub(crate) transfers: Vec<Transfer>,
}

#[derive(Deserialize, Serialize)]
pub(crate) struct ListUnspentsRequest {
    pub(crate) skip_sync: bool,
}

#[derive(Deserialize, Serialize)]
pub(crate) struct ListUnspentsResponse {
    pub(crate) unspents: Vec<Unspent>,
}

#[derive(Deserialize, Serialize)]
pub(crate) struct LNInvoiceRequest {
    pub(crate) amt_msat: Option<u64>,
    pub(crate) expiry_sec: u32,
    pub(crate) asset_id: Option<String>,
    pub(crate) asset_amount: Option<u64>,
}

#[derive(Deserialize, Serialize)]
pub(crate) struct LNInvoiceResponse {
    pub(crate) invoice: String,
}

#[derive(Deserialize, Serialize)]
pub(crate) struct MakerExecuteRequest {
    pub(crate) swapstring: String,
    pub(crate) payment_secret: String,
    pub(crate) taker_pubkey: String,
}

// "from" and "to" are seen from the taker's perspective, so:
// - "from" is what the taker will send and the maker will receive
// - "to" is what the taker will receive and the maker will send
// qty_from and qty_to are in msat when the asset is BTC
#[derive(Deserialize, Serialize)]
pub(crate) struct MakerInitRequest {
    pub(crate) qty_from: u64,
    pub(crate) qty_to: u64,
    pub(crate) from_asset: Option<String>,
    pub(crate) to_asset: Option<String>,
    pub(crate) timeout_sec: u32,
}

#[derive(Deserialize, Serialize)]
pub(crate) struct MakerInitResponse {
    pub(crate) payment_hash: String,
    pub(crate) payment_secret: String,
    pub(crate) swapstring: String,
}

#[derive(Deserialize, Serialize)]
pub(crate) struct Media {
    pub(crate) file_path: String,
    pub(crate) digest: String,
    pub(crate) mime: String,
}

#[derive(Deserialize, Serialize)]
pub(crate) struct NetworkInfoResponse {
    pub(crate) network: BitcoinNetwork,
    pub(crate) height: u32,
}

#[derive(Deserialize, Serialize)]
pub(crate) struct NodeInfoResponse {
    pub(crate) pubkey: String,
    pub(crate) num_channels: usize,
    pub(crate) num_usable_channels: usize,
    pub(crate) local_balance_sat: u64,
    pub(crate) eventual_close_fees_sat: u64,
    pub(crate) pending_outbound_payments_sat: u64,
    pub(crate) num_peers: usize,
    pub(crate) account_xpub_vanilla: String,
    pub(crate) account_xpub_colored: String,
    pub(crate) max_media_upload_size_mb: u16,
    pub(crate) rgb_htlc_min_msat: u64,
    pub(crate) rgb_channel_capacity_min_sat: u64,
    pub(crate) channel_capacity_min_sat: u64,
    pub(crate) channel_capacity_max_sat: u64,
    pub(crate) channel_asset_min_amount: u64,
    pub(crate) channel_asset_max_amount: u64,
    pub(crate) network_nodes: usize,
    pub(crate) network_channels: usize,
}

#[derive(Deserialize, Serialize)]
pub(crate) struct OpenChannelRequest {
    pub(crate) peer_pubkey_and_opt_addr: String,
    pub(crate) capacity_sat: u64,
    pub(crate) push_msat: u64,
    pub(crate) asset_amount: Option<u64>,
    pub(crate) asset_id: Option<String>,
    pub(crate) public: bool,
    pub(crate) with_anchors: bool,
    pub(crate) fee_base_msat: Option<u32>,
    pub(crate) fee_proportional_millionths: Option<u32>,
    pub(crate) temporary_channel_id: Option<String>,
}

#[derive(Deserialize, Serialize)]
pub(crate) struct OpenChannelResponse {
    pub(crate) temporary_channel_id: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub(crate) struct Payment {
    pub(crate) amt_msat: Option<u64>,
    pub(crate) asset_amount: Option<u64>,
    pub(crate) asset_id: Option<String>,
    pub(crate) payment_hash: String,
    pub(crate) inbound: bool,
    pub(crate) status: HTLCStatus,
    pub(crate) created_at: u64,
    pub(crate) updated_at: u64,
    pub(crate) payee_pubkey: String,
}

#[derive(Clone, Deserialize, Serialize)]
pub(crate) struct Peer {
    pub(crate) pubkey: String,
}

#[derive(Deserialize, Serialize)]
pub(crate) struct PostAssetMediaResponse {
    pub(crate) digest: String,
}

#[derive(Deserialize, Serialize)]
pub(crate) struct ProofOfReserves {
    pub(crate) utxo: String,
    pub(crate) proof: Vec<u8>,
}

#[derive(Deserialize, Serialize)]
pub(crate) struct Recipient {
    pub(crate) recipient_id: String,
    pub(crate) witness_data: Option<WitnessData>,
    pub(crate) assignment: Assignment,
    pub(crate) transport_endpoints: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
pub(crate) enum RecipientType {
    Blind,
    Witness,
}

#[derive(Deserialize, Serialize)]
pub(crate) struct RefreshRequest {
    pub(crate) skip_sync: bool,
}

#[derive(Deserialize, Serialize)]
pub(crate) struct RestoreRequest {
    pub(crate) backup_path: String,
    pub(crate) password: String,
}

#[derive(Debug, Deserialize, Serialize)]
pub(crate) struct RevokeTokenRequest {
    pub(crate) token: String,
}

#[derive(Deserialize, Serialize)]
pub(crate) struct RgbAllocation {
    pub(crate) asset_id: Option<String>,
    pub(crate) assignment: Assignment,
    pub(crate) settled: bool,
}

#[derive(Deserialize, Serialize)]
pub(crate) struct RgbInvoiceRequest {
    pub(crate) asset_id: Option<String>,
    pub(crate) assignment: Option<Assignment>,
    pub(crate) duration_seconds: Option<u32>,
    pub(crate) min_confirmations: u8,
    pub(crate) witness: bool,
}

#[derive(Deserialize, Serialize)]
pub(crate) struct RgbInvoiceResponse {
    pub(crate) recipient_id: String,
    pub(crate) invoice: String,
    pub(crate) expiration_timestamp: Option<i64>,
    pub(crate) batch_transfer_idx: i32,
}

#[derive(Deserialize, Serialize)]
pub(crate) struct SendBtcRequest {
    pub(crate) amount: u64,
    pub(crate) address: String,
    pub(crate) fee_rate: u64,
    pub(crate) skip_sync: bool,
}

#[derive(Deserialize, Serialize)]
pub(crate) struct SendBtcResponse {
    pub(crate) txid: String,
}

#[derive(Deserialize, Serialize)]
pub(crate) struct SendOnionMessageRequest {
    pub(crate) node_ids: Vec<String>,
    pub(crate) tlv_type: u64,
    pub(crate) data: String,
}

#[derive(Deserialize, Serialize)]
pub(crate) struct SendPaymentRequest {
    pub(crate) invoice: String,
    pub(crate) amt_msat: Option<u64>,
}

#[derive(Deserialize, Serialize)]
pub(crate) struct SendPaymentResponse {
    pub(crate) payment_id: String,
    pub(crate) payment_hash: Option<String>,
    pub(crate) payment_secret: Option<String>,
    pub(crate) status: HTLCStatus,
}

#[derive(Deserialize, Serialize)]
pub(crate) struct SendRgbRequest {
    pub(crate) donation: bool,
    pub(crate) fee_rate: u64,
    pub(crate) min_confirmations: u8,
    pub(crate) recipient_map: HashMap<String, Vec<Recipient>>,
    pub(crate) skip_sync: bool,
}

#[derive(Deserialize, Serialize)]
pub(crate) struct SendRgbResponse {
    pub(crate) txid: String,
}

#[derive(Deserialize, Serialize)]
pub(crate) struct SignMessageRequest {
    pub(crate) message: String,
}

#[derive(Deserialize, Serialize)]
pub(crate) struct SignMessageResponse {
    pub(crate) signed_message: String,
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct Swap {
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

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
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

#[derive(Deserialize, Serialize)]
pub(crate) struct TakerRequest {
    pub(crate) swapstring: String,
}

#[derive(Deserialize, Serialize)]
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

#[derive(Deserialize, Serialize)]
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

#[derive(Debug, Deserialize, Serialize)]
pub(crate) struct Transaction {
    pub(crate) transaction_type: TransactionType,
    pub(crate) txid: String,
    pub(crate) received: u64,
    pub(crate) sent: u64,
    pub(crate) fee: u64,
    pub(crate) confirmation_time: Option<BlockTime>,
}

#[derive(Debug, PartialEq, Deserialize, Serialize)]
pub(crate) enum TransactionType {
    RgbSend,
    Drain,
    CreateUtxos,
    User,
}

#[derive(Debug, Deserialize, Serialize)]
pub(crate) struct Transfer {
    pub(crate) idx: i32,
    pub(crate) created_at: i64,
    pub(crate) updated_at: i64,
    pub(crate) status: TransferStatus,
    pub(crate) requested_assignment: Option<Assignment>,
    pub(crate) assignments: Vec<Assignment>,
    pub(crate) kind: TransferKind,
    pub(crate) txid: Option<String>,
    pub(crate) recipient_id: Option<String>,
    pub(crate) receive_utxo: Option<String>,
    pub(crate) change_utxo: Option<String>,
    pub(crate) expiration: Option<i64>,
    pub(crate) transport_endpoints: Vec<TransferTransportEndpoint>,
}

#[derive(Debug, PartialEq, Deserialize, Serialize)]
pub(crate) enum TransferKind {
    Issuance,
    ReceiveBlind,
    ReceiveWitness,
    Send,
    Inflation,
}

#[derive(Debug, PartialEq, Deserialize, Serialize)]
pub(crate) enum TransferStatus {
    WaitingCounterparty,
    WaitingConfirmations,
    Settled,
    Failed,
}

#[derive(Debug, Deserialize, Serialize)]
pub(crate) struct TransferTransportEndpoint {
    pub(crate) endpoint: String,
    pub(crate) transport_type: TransportType,
    pub(crate) used: bool,
}

#[derive(Debug, Deserialize, Serialize)]
pub(crate) enum TransportType {
    JsonRpc,
}

#[derive(Deserialize, Serialize)]
pub(crate) struct UnlockRequest {
    pub(crate) password: String,
    pub(crate) bitcoind_rpc_username: String,
    pub(crate) bitcoind_rpc_password: String,
    pub(crate) bitcoind_rpc_host: String,
    pub(crate) bitcoind_rpc_port: u16,
    pub(crate) indexer_url: Option<String>,
    pub(crate) proxy_endpoint: Option<String>,
    pub(crate) announce_addresses: Vec<String>,
    pub(crate) announce_alias: Option<String>,
}

#[derive(Deserialize, Serialize)]
pub(crate) struct Unspent {
    pub(crate) utxo: Utxo,
    pub(crate) rgb_allocations: Vec<RgbAllocation>,
}

#[derive(Deserialize, Serialize)]
pub(crate) struct Utxo {
    pub(crate) outpoint: String,
    pub(crate) btc_amount: u64,
    pub(crate) colorable: bool,
}

#[derive(Deserialize, Serialize)]
pub(crate) struct WitnessData {
    pub(crate) amount_sat: u64,
    pub(crate) blinding: Option<u64>,
}


impl From<RgbLibBalance> for AssetBalanceResponse {
    fn from(value: RgbLibBalance) -> Self {
        Self {
            settled: value.settled,
            future: value.future,
            spendable: value.spendable,
            offchain_outbound: 0,
            offchain_inbound: 0,
        }
    }
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
            balance: value.balance.into(),
            media: value.media.map(|m| m.into()),
        }
    }
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
            balance: value.balance.into(),
            media: value.media.map(|m| m.into()),
        }
    }
}

impl From<AssetSchema> for RgbLibAssetSchema {
    fn from(value: AssetSchema) -> Self {
        match value {
            AssetSchema::Nia => Self::Nia,
            AssetSchema::Uda => Self::Uda,
            AssetSchema::Cfa => Self::Cfa,
        }
    }
}

impl From<RgbLibAssetSchema> for AssetSchema {
    fn from(value: RgbLibAssetSchema) -> Self {
        match value {
            RgbLibAssetSchema::Nia => Self::Nia,
            RgbLibAssetSchema::Uda => Self::Uda,
            RgbLibAssetSchema::Cfa => Self::Cfa,
            RgbLibAssetSchema::Ifa => todo!(),
        }
    }
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
            balance: value.balance.into(),
            token: value.token.map(|t| t.into()),
        }
    }
}

impl From<RgbLibAssignment> for Assignment {
    fn from(x: RgbLibAssignment) -> Self {
        match x {
            RgbLibAssignment::Fungible(amt) => Self::Fungible(amt),
            RgbLibAssignment::NonFungible => Self::NonFungible,
            RgbLibAssignment::InflationRight(amt) => Self::InflationRight(amt),
            RgbLibAssignment::ReplaceRight => Self::ReplaceRight,
            RgbLibAssignment::Any => Self::Any,
        }
    }
}

impl From<Assignment> for RgbLibAssignment {
    fn from(x: Assignment) -> Self {
        match x {
            Assignment::Fungible(amt) => Self::Fungible(amt),
            Assignment::NonFungible => Self::NonFungible,
            Assignment::InflationRight(amt) => Self::InflationRight(amt),
            Assignment::ReplaceRight => Self::ReplaceRight,
            Assignment::Any => Self::Any,
        }
    }
}

impl From<Network> for BitcoinNetwork {
    fn from(x: Network) -> Self {
        match x {
            Network::Bitcoin => Self::Mainnet,
            Network::Testnet => Self::Testnet,
            Network::Testnet4 => Self::Testnet4,
            Network::Regtest => Self::Regtest,
            Network::Signet => Self::Signet,
            _ => unimplemented!("unsupported network"),
        }
    }
}

impl From<RgbLibNetwork> for BitcoinNetwork {
    fn from(x: RgbLibNetwork) -> Self {
        match x {
            RgbLibNetwork::Mainnet => Self::Mainnet,
            RgbLibNetwork::Testnet => Self::Testnet,
            RgbLibNetwork::Testnet4 => Self::Testnet4,
            RgbLibNetwork::Regtest => Self::Regtest,
            RgbLibNetwork::Signet => Self::Signet,
        }
    }
}

impl From<sdk::ChannelStatus> for ChannelStatus {
    fn from(value: sdk::ChannelStatus) -> Self {
        match value {
            sdk::ChannelStatus::Opening => Self::Opening,
            sdk::ChannelStatus::Opened => Self::Opened,
            sdk::ChannelStatus::Closing => Self::Closing,
        }
    }
}

impl From<RgbLibEmbeddedMedia> for EmbeddedMedia {
    fn from(value: RgbLibEmbeddedMedia) -> Self {
        Self {
            mime: value.mime,
            data: value.data,
        }
    }
}

impl From<sdk::HtlcStatus> for HTLCStatus {
    fn from(value: sdk::HtlcStatus) -> Self {
        match value {
            sdk::HtlcStatus::Pending => Self::Pending,
            sdk::HtlcStatus::Succeeded => Self::Succeeded,
            sdk::HtlcStatus::Failed => Self::Failed,
        }
    }
}

impl From<HTLCStatus> for sdk::HtlcStatus {
    fn from(value: HTLCStatus) -> Self {
        match value {
            HTLCStatus::Pending => Self::Pending,
            HTLCStatus::Succeeded => Self::Succeeded,
            HTLCStatus::Failed => Self::Failed,
        }
    }
}

impl From<RgbLibIndexerProtocol> for IndexerProtocol {
    fn from(x: RgbLibIndexerProtocol) -> Self {
        match x {
            RgbLibIndexerProtocol::Electrum => Self::Electrum,
            RgbLibIndexerProtocol::Esplora => Self::Esplora,
        }
    }
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

impl From<RgbLibProofOfReserves> for ProofOfReserves {
    fn from(value: RgbLibProofOfReserves) -> Self {
        Self {
            utxo: value.utxo.to_string(),
            proof: value.proof,
        }
    }
}

impl From<Recipient> for RgbLibRecipient {
    fn from(value: Recipient) -> Self {
        Self {
            recipient_id: value.recipient_id,
            witness_data: value.witness_data.map(|w| w.into()),
            assignment: value.assignment.into(),
            transport_endpoints: value.transport_endpoints,
        }
    }
}

impl From<RgbLibRecipientType> for RecipientType {
    fn from(value: RgbLibRecipientType) -> Self {
        match value {
            RgbLibRecipientType::Blind => Self::Blind,
            RgbLibRecipientType::Witness => Self::Witness,
        }
    }
}

impl From<sdk::SwapStatus> for SwapStatus {
    fn from(value: sdk::SwapStatus) -> Self {
        match value {
            sdk::SwapStatus::Waiting => Self::Waiting,
            sdk::SwapStatus::Pending => Self::Pending,
            sdk::SwapStatus::Succeeded => Self::Succeeded,
            sdk::SwapStatus::Expired => Self::Expired,
            sdk::SwapStatus::Failed => Self::Failed,
        }
    }
}

impl From<SwapStatus> for sdk::SwapStatus {
    fn from(value: SwapStatus) -> Self {
        match value {
            SwapStatus::Waiting => Self::Waiting,
            SwapStatus::Pending => Self::Pending,
            SwapStatus::Succeeded => Self::Succeeded,
            SwapStatus::Expired => Self::Expired,
            SwapStatus::Failed => Self::Failed,
        }
    }
}

impl From<RgbLibToken> for Token {
    fn from(value: RgbLibToken) -> Self {
        Self {
            index: value.index,
            ticker: value.ticker,
            name: value.name,
            details: value.details,
            embedded_media: value.embedded_media.map(|em| em.into()),
            media: value.media.map(|m| m.into()),
            attachments: value
                .attachments
                .into_iter()
                .map(|(k, v)| (k, v.into()))
                .collect(),
            reserves: value.reserves.map(|r| r.into()),
        }
    }
}

impl From<RgbLibTokenLight> for TokenLight {
    fn from(value: RgbLibTokenLight) -> Self {
        Self {
            index: value.index,
            ticker: value.ticker,
            name: value.name,
            details: value.details,
            embedded_media: value.embedded_media,
            media: value.media.map(|m| m.into()),
            attachments: value
                .attachments
                .into_iter()
                .map(|(k, v)| (k, v.into()))
                .collect(),
            reserves: value.reserves,
        }
    }
}

impl From<sdk::EmbeddedMedia> for EmbeddedMedia {
    fn from(value: sdk::EmbeddedMedia) -> Self {
        Self {
            mime: value.mime,
            data: value.data,
        }
    }
}

impl From<sdk::Media> for Media {
    fn from(value: sdk::Media) -> Self {
        Self {
            file_path: value.file_path,
            digest: value.digest,
            mime: value.mime,
        }
    }
}

impl From<sdk::ProofOfReserves> for ProofOfReserves {
    fn from(value: sdk::ProofOfReserves) -> Self {
        Self {
            utxo: value.utxo,
            proof: value.proof,
        }
    }
}

impl From<sdk::Token> for Token {
    fn from(value: sdk::Token) -> Self {
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

impl From<sdk::TokenLight> for TokenLight {
    fn from(value: sdk::TokenLight) -> Self {
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

impl From<sdk::AssetBalance> for AssetBalanceResponse {
    fn from(value: sdk::AssetBalance) -> Self {
        Self {
            settled: value.settled,
            future: value.future,
            spendable: value.spendable,
            offchain_outbound: value.offchain_outbound,
            offchain_inbound: value.offchain_inbound,
        }
    }
}

impl From<sdk::AssetNIA> for AssetNIA {
    fn from(value: sdk::AssetNIA) -> Self {
        Self {
            asset_id: value.asset_id,
            ticker: value.ticker,
            name: value.name,
            details: value.details,
            precision: value.precision,
            issued_supply: value.issued_supply,
            timestamp: value.timestamp,
            added_at: value.added_at,
            balance: value.balance.into(),
            media: value.media.map(Into::into),
        }
    }
}

impl From<sdk::AssetUDA> for AssetUDA {
    fn from(value: sdk::AssetUDA) -> Self {
        Self {
            asset_id: value.asset_id,
            ticker: value.ticker,
            name: value.name,
            details: value.details,
            precision: value.precision,
            timestamp: value.timestamp,
            added_at: value.added_at,
            balance: value.balance.into(),
            token: value.token.map(Into::into),
        }
    }
}

impl From<sdk::AssetCFA> for AssetCFA {
    fn from(value: sdk::AssetCFA) -> Self {
        Self {
            asset_id: value.asset_id,
            name: value.name,
            details: value.details,
            precision: value.precision,
            issued_supply: value.issued_supply,
            timestamp: value.timestamp,
            added_at: value.added_at,
            balance: value.balance.into(),
            media: value.media.map(Into::into),
        }
    }
}

impl From<sdk::TransactionType> for TransactionType {
    fn from(value: sdk::TransactionType) -> Self {
        match value {
            sdk::TransactionType::RgbSend => Self::RgbSend,
            sdk::TransactionType::Drain => Self::Drain,
            sdk::TransactionType::CreateUtxos => Self::CreateUtxos,
            sdk::TransactionType::User => Self::User,
        }
    }
}

impl From<sdk::TransferKind> for TransferKind {
    fn from(value: sdk::TransferKind) -> Self {
        match value {
            sdk::TransferKind::Issuance => Self::Issuance,
            sdk::TransferKind::ReceiveBlind => Self::ReceiveBlind,
            sdk::TransferKind::ReceiveWitness => Self::ReceiveWitness,
            sdk::TransferKind::Send => Self::Send,
            sdk::TransferKind::Inflation => Self::Inflation,
        }
    }
}

impl From<sdk::TransferStatus> for TransferStatus {
    fn from(value: sdk::TransferStatus) -> Self {
        match value {
            sdk::TransferStatus::WaitingCounterparty => Self::WaitingCounterparty,
            sdk::TransferStatus::WaitingConfirmations => Self::WaitingConfirmations,
            sdk::TransferStatus::Settled => Self::Settled,
            sdk::TransferStatus::Failed => Self::Failed,
        }
    }
}

impl From<sdk::TransportType> for TransportType {
    fn from(value: sdk::TransportType) -> Self {
        match value {
            sdk::TransportType::JsonRpc => Self::JsonRpc,
        }
    }
}

impl From<WitnessData> for RgbLibWitnessData {
    fn from(value: WitnessData) -> Self {
        Self {
            amount_sat: value.amount_sat,
            blinding: value.blinding,
        }
    }
}


pub(crate) async fn asset_balance(
    State(state): State<Arc<AppState>>,
    WithRejection(Json(payload), _): WithRejection<Json<AssetBalanceRequest>, APIError>,
) -> Result<Json<AssetBalanceResponse>, APIError> {
    let data = sdk::asset_balance(state.clone(), payload.asset_id).await?;
    Ok(Json(AssetBalanceResponse {
        settled: data.settled,
        future: data.future,
        spendable: data.spendable,
        offchain_outbound: data.offchain_outbound,
        offchain_inbound: data.offchain_inbound,
    }))
}

pub(crate) async fn asset_metadata(
    State(state): State<Arc<AppState>>,
    WithRejection(Json(payload), _): WithRejection<Json<AssetMetadataRequest>, APIError>,
) -> Result<Json<AssetMetadataResponse>, APIError> {
    let data = sdk::asset_metadata(state.clone(), payload.asset_id).await?;
    Ok(Json(AssetMetadataResponse {
        asset_schema: data.asset_schema.into(),
        initial_supply: data.initial_supply,
        max_supply: data.max_supply,
        known_circulating_supply: data.known_circulating_supply,
        timestamp: data.timestamp,
        name: data.name,
        precision: data.precision,
        ticker: data.ticker,
        details: data.details,
        token: data.token.map(Into::into),
    }))
}

pub(crate) async fn get_asset_media(
    State(state): State<Arc<AppState>>,
    WithRejection(Json(payload), _): WithRejection<Json<GetAssetMediaRequest>, APIError>,
) -> Result<Json<GetAssetMediaResponse>, APIError> {
    let data = sdk::get_asset_media(state.clone(), payload.digest).await?;
    Ok(Json(GetAssetMediaResponse {
        bytes_hex: data.bytes_hex,
    }))
}

pub(crate) async fn issue_asset_cfa(
    State(state): State<Arc<AppState>>,
    WithRejection(Json(payload), _): WithRejection<Json<IssueAssetCFARequest>, APIError>,
) -> Result<Json<IssueAssetCFAResponse>, APIError> {
    no_cancel(async move {
        let guard = state.check_unlocked().await?;
        let unlocked_state = guard.as_ref().unwrap();

        if *unlocked_state.rgb_send_lock.lock().unwrap() {
            return Err(APIError::OpenChannelInProgress);
        }

        let file_path = payload.file_digest.map(|d: String| {
            unlocked_state
                .rgb_get_media_dir()
                .join(d.to_lowercase())
                .to_string_lossy()
                .to_string()
        });

        let asset = unlocked_state.rgb_issue_asset_cfa(
            payload.name,
            payload.details,
            payload.precision,
            payload.amounts,
            file_path,
        )?;

        Ok(Json(IssueAssetCFAResponse {
            asset: asset.into(),
        }))
    })
    .await
}

pub(crate) async fn issue_asset_nia(
    State(state): State<Arc<AppState>>,
    WithRejection(Json(payload), _): WithRejection<Json<IssueAssetNIARequest>, APIError>,
) -> Result<Json<IssueAssetNIAResponse>, APIError> {
    no_cancel(async move {
        let guard = state.check_unlocked().await?;
        let unlocked_state = guard.as_ref().unwrap();

        if *unlocked_state.rgb_send_lock.lock().unwrap() {
            return Err(APIError::OpenChannelInProgress);
        }

        let asset = unlocked_state.rgb_issue_asset_nia(
            payload.ticker,
            payload.name,
            payload.precision,
            payload.amounts,
        )?;

        Ok(Json(IssueAssetNIAResponse {
            asset: asset.into(),
        }))
    })
    .await
}

pub(crate) async fn issue_asset_uda(
    State(state): State<Arc<AppState>>,
    WithRejection(Json(payload), _): WithRejection<Json<IssueAssetUDARequest>, APIError>,
) -> Result<Json<IssueAssetUDAResponse>, APIError> {
    no_cancel(async move {
        let guard = state.check_unlocked().await?;
        let unlocked_state = guard.as_ref().unwrap();

        if *unlocked_state.rgb_send_lock.lock().unwrap() {
            return Err(APIError::OpenChannelInProgress);
        }

        let rgb_media_dir = unlocked_state.rgb_get_media_dir();
        let get_string_path = |d: String| {
            rgb_media_dir
                .join(d.to_lowercase())
                .to_string_lossy()
                .to_string()
        };
        let media_file_path = payload.media_file_digest.map(get_string_path);
        let attachments_file_paths = payload
            .attachments_file_digests
            .into_iter()
            .map(get_string_path)
            .collect();

        let asset = unlocked_state.rgb_issue_asset_uda(
            payload.ticker,
            payload.name,
            payload.details,
            payload.precision,
            media_file_path,
            attachments_file_paths,
        )?;

        Ok(Json(IssueAssetUDAResponse {
            asset: asset.into(),
        }))
    })
    .await
}

pub(crate) async fn list_assets(
    State(state): State<Arc<AppState>>,
    WithRejection(Json(payload), _): WithRejection<Json<ListAssetsRequest>, APIError>,
) -> Result<Json<ListAssetsResponse>, APIError> {
    let data = sdk::list_assets(
        state.clone(),
        payload
            .filter_asset_schemas
            .into_iter()
            .map(Into::into)
            .collect(),
    )
    .await?;
    Ok(Json(ListAssetsResponse {
        nia: data
            .nia
            .map(|assets| assets.into_iter().map(Into::into).collect()),
        uda: data
            .uda
            .map(|assets| assets.into_iter().map(Into::into).collect()),
        cfa: data
            .cfa
            .map(|assets| assets.into_iter().map(Into::into).collect()),
    }))
}

pub(crate) async fn post_asset_media(
    State(state): State<Arc<AppState>>,
    mut multipart: Multipart,
) -> Result<Json<PostAssetMediaResponse>, APIError> {
    no_cancel(async move {
        let guard = state.check_unlocked().await?;
        let unlocked_state = guard.as_ref().unwrap();

        let digest = if let Some(field) = multipart
            .next_field()
            .await
            .map_err(|_| APIError::MediaFileNotProvided)?
        {
            let file_bytes = field
                .bytes()
                .await
                .map_err(|e| APIError::Unexpected(format!("Failed to read bytes: {e}")))?;
            if file_bytes.is_empty() {
                return Err(APIError::MediaFileEmpty);
            }
            let file_hash: sha256::Hash = Hash::hash(&file_bytes[..]);
            let digest = file_hash.to_string();

            let file_path = unlocked_state.rgb_get_media_dir().join(&digest);
            let mut write = true;
            if file_path.exists() {
                let mut buf_reader = BufReader::new(File::open(&file_path).await?);
                let mut existing_file_bytes = Vec::new();
                buf_reader.read_to_end(&mut existing_file_bytes).await?;
                if file_bytes != existing_file_bytes {
                    tokio::fs::remove_file(&file_path).await?;
                } else {
                    write = false;
                }
            }
            if write {
                let mut file = File::create(&file_path).await?;
                file.write_all(&file_bytes).await?;
            }
            digest
        } else {
            return Err(APIError::MediaFileNotProvided);
        };

        Ok(Json(PostAssetMediaResponse { digest }))
    })
    .await
}

pub(crate) async fn rgb_invoice(
    State(state): State<Arc<AppState>>,
    WithRejection(Json(payload), _): WithRejection<Json<RgbInvoiceRequest>, APIError>,
) -> Result<Json<RgbInvoiceResponse>, APIError> {
    no_cancel(async move {
        let guard = state.check_unlocked().await?;
        let unlocked_state = guard.as_ref().unwrap();

        if *unlocked_state.rgb_send_lock.lock().unwrap() {
            return Err(APIError::OpenChannelInProgress);
        }

        let assignment = payload.assignment.unwrap_or(Assignment::Any).into();

        let receive_data = if payload.witness {
            unlocked_state.rgb_witness_receive(
                payload.asset_id,
                assignment,
                payload.duration_seconds,
                vec![unlocked_state.proxy_endpoint.clone()],
                payload.min_confirmations,
            )?
        } else {
            unlocked_state.rgb_blind_receive(
                payload.asset_id,
                assignment,
                payload.duration_seconds,
                vec![unlocked_state.proxy_endpoint.clone()],
                payload.min_confirmations,
            )?
        };

        Ok(Json(RgbInvoiceResponse {
            recipient_id: receive_data.recipient_id,
            invoice: receive_data.invoice,
            expiration_timestamp: receive_data.expiration_timestamp,
            batch_transfer_idx: receive_data.batch_transfer_idx,
        }))
    })
    .await
}

pub(crate) async fn send_rgb(
    State(state): State<Arc<AppState>>,
    WithRejection(Json(payload), _): WithRejection<Json<SendRgbRequest>, APIError>,
) -> Result<Json<SendRgbResponse>, APIError> {
    no_cancel(async move {
        let recipient_map: HashMap<String, Vec<RgbLibRecipient>> = payload
            .recipient_map
            .into_iter()
            .map(|(asset_id, recipients)| {
                (asset_id, recipients.into_iter().map(|r| r.into()).collect())
            })
            .collect();

        let send_result = sdk::send_rgb(
            state.clone(),
            recipient_map,
            payload.donation,
            payload.fee_rate,
            payload.min_confirmations,
            payload.skip_sync,
        )
        .await?;
        let _batch_transfer_idx = send_result.batch_transfer_idx;

        Ok(Json(SendRgbResponse {
            txid: send_result.txid,
        }))
    })
    .await
}

pub(crate) async fn refresh_transfers(
    State(state): State<Arc<AppState>>,
    WithRejection(Json(payload), _): WithRejection<Json<RefreshRequest>, APIError>,
) -> Result<Json<EmptyResponse>, APIError> {
    no_cancel(async move {
        let guard = state.check_unlocked().await?;
        let unlocked_state = guard.as_ref().unwrap();
        let unlocked_state_copy = unlocked_state.clone();

        tokio::task::spawn_blocking(move || unlocked_state_copy.rgb_refresh(payload.skip_sync))
            .await
            .unwrap()?;

        tracing::info!("Refresh complete");
        Ok(Json(EmptyResponse {}))
    })
    .await
}


pub(crate) async fn address(
    State(state): State<Arc<AppState>>,
) -> Result<Json<AddressResponse>, APIError> {
    let data = sdk::address(state.clone()).await?;
    Ok(Json(AddressResponse {
        address: data.address,
    }))
}

pub(crate) async fn btc_balance(
    State(state): State<Arc<AppState>>,
    WithRejection(Json(payload), _): WithRejection<Json<BtcBalanceRequest>, APIError>,
) -> Result<Json<BtcBalanceResponse>, APIError> {
    let data = sdk::btc_balance(state.clone(), payload.skip_sync).await?;
    Ok(Json(BtcBalanceResponse {
        vanilla: BtcBalance {
            settled: data.vanilla.settled,
            future: data.vanilla.future,
            spendable: data.vanilla.spendable,
        },
        colored: BtcBalance {
            settled: data.colored.settled,
            future: data.colored.future,
            spendable: data.colored.spendable,
        },
    }))
}

pub(crate) async fn check_indexer_url(
    State(state): State<Arc<AppState>>,
    WithRejection(Json(payload), _): WithRejection<Json<CheckIndexerUrlRequest>, APIError>,
) -> Result<Json<CheckIndexerUrlResponse>, APIError> {
    let data = sdk::check_indexer_url(state.clone(), payload.indexer_url).await?;
    Ok(Json(CheckIndexerUrlResponse {
        indexer_protocol: data.indexer_protocol.into(),
    }))
}

pub(crate) async fn check_proxy_endpoint(
    WithRejection(Json(payload), _): WithRejection<Json<CheckProxyEndpointRequest>, APIError>,
) -> Result<Json<EmptyResponse>, APIError> {
    sdk::check_proxy_endpoint(payload.proxy_endpoint).await?;
    Ok(Json(EmptyResponse {}))
}

pub(crate) async fn estimate_fee(
    State(state): State<Arc<AppState>>,
    WithRejection(Json(payload), _): WithRejection<Json<EstimateFeeRequest>, APIError>,
) -> Result<Json<EstimateFeeResponse>, APIError> {
    let data = sdk::estimate_fee(state.clone(), payload.blocks).await?;
    Ok(Json(EstimateFeeResponse {
        fee_rate: data.fee_rate,
    }))
}

pub(crate) async fn get_channel_id(
    State(state): State<Arc<AppState>>,
    WithRejection(Json(payload), _): WithRejection<Json<GetChannelIdRequest>, APIError>,
) -> Result<Json<GetChannelIdResponse>, APIError> {
    let data = sdk::get_channel_id(state.clone(), payload.temporary_channel_id).await?;
    Ok(Json(GetChannelIdResponse {
        channel_id: data.channel_id,
    }))
}

pub(crate) async fn list_channels(
    State(state): State<Arc<AppState>>,
) -> Result<Json<ListChannelsResponse>, APIError> {
    let data = sdk::list_channels(state.clone()).await?;
    let channels = data
        .into_iter()
        .map(|c| Channel {
            channel_id: c.channel_id,
            funding_txid: c.funding_txid,
            peer_pubkey: c.peer_pubkey,
            peer_alias: c.peer_alias,
            short_channel_id: c.short_channel_id,
            status: c.status.into(),
            ready: c.ready,
            capacity_sat: c.capacity_sat,
            local_balance_sat: c.local_balance_sat,
            outbound_balance_msat: c.outbound_balance_msat,
            inbound_balance_msat: c.inbound_balance_msat,
            next_outbound_htlc_limit_msat: c.next_outbound_htlc_limit_msat,
            next_outbound_htlc_minimum_msat: c.next_outbound_htlc_minimum_msat,
            is_usable: c.is_usable,
            public: c.public,
            asset_id: c.asset_id,
            asset_local_amount: c.asset_local_amount,
            asset_remote_amount: c.asset_remote_amount,
        })
        .collect();

    Ok(Json(ListChannelsResponse { channels }))
}

pub(crate) async fn list_peers(
    State(state): State<Arc<AppState>>,
) -> Result<Json<ListPeersResponse>, APIError> {
    let data = sdk::list_peers(state.clone()).await?;
    let peers = data
        .into_iter()
        .map(|p| Peer { pubkey: p.pubkey })
        .collect();

    Ok(Json(ListPeersResponse { peers }))
}

pub(crate) async fn list_transactions(
    State(state): State<Arc<AppState>>,
    WithRejection(Json(payload), _): WithRejection<Json<ListTransactionsRequest>, APIError>,
) -> Result<Json<ListTransactionsResponse>, APIError> {
    let data = sdk::list_transactions(state.clone(), payload.skip_sync).await?;
    let transactions = data
        .into_iter()
        .map(|tx| Transaction {
            transaction_type: tx.transaction_type.into(),
            txid: tx.txid,
            received: tx.received,
            sent: tx.sent,
            fee: tx.fee,
            confirmation_time: tx.confirmation_time.map(|ct| BlockTime {
                height: ct.height,
                timestamp: ct.timestamp,
            }),
        })
        .collect();

    Ok(Json(ListTransactionsResponse { transactions }))
}

pub(crate) async fn list_transfers(
    State(state): State<Arc<AppState>>,
    WithRejection(Json(payload), _): WithRejection<Json<ListTransfersRequest>, APIError>,
) -> Result<Json<ListTransfersResponse>, APIError> {
    let data = sdk::list_transfers(state.clone(), payload.asset_id).await?;
    let transfers = data
        .into_iter()
        .map(|t| Transfer {
            idx: t.idx,
            created_at: t.created_at,
            updated_at: t.updated_at,
            status: t.status.into(),
            requested_assignment: t.requested_assignment.map(Into::into),
            assignments: t.assignments.into_iter().map(Into::into).collect(),
            kind: t.kind.into(),
            txid: t.txid,
            recipient_id: t.recipient_id,
            receive_utxo: t.receive_utxo,
            change_utxo: t.change_utxo,
            expiration: t.expiration,
            transport_endpoints: t
                .transport_endpoints
                .into_iter()
                .map(|tte| TransferTransportEndpoint {
                    endpoint: tte.endpoint,
                    transport_type: tte.transport_type.into(),
                    used: tte.used,
                })
                .collect(),
        })
        .collect();
    Ok(Json(ListTransfersResponse { transfers }))
}

pub(crate) async fn list_unspents(
    State(state): State<Arc<AppState>>,
    WithRejection(Json(payload), _): WithRejection<Json<ListUnspentsRequest>, APIError>,
) -> Result<Json<ListUnspentsResponse>, APIError> {
    let data = sdk::list_unspents(state.clone(), payload.skip_sync).await?;
    let unspents = data
        .into_iter()
        .map(|u| Unspent {
            utxo: Utxo {
                outpoint: u.utxo.outpoint,
                btc_amount: u.utxo.btc_amount,
                colorable: u.utxo.colorable,
            },
            rgb_allocations: u
                .rgb_allocations
                .into_iter()
                .map(|a| RgbAllocation {
                    asset_id: a.asset_id,
                    assignment: a.assignment.into(),
                    settled: a.settled,
                })
                .collect(),
        })
        .collect();
    Ok(Json(ListUnspentsResponse { unspents }))
}

pub(crate) async fn network_info(
    State(state): State<Arc<AppState>>,
) -> Result<Json<NetworkInfoResponse>, APIError> {
    let data = sdk::network_info(state.clone()).await?;
    Ok(Json(NetworkInfoResponse {
        network: data.network.into(),
        height: data.height,
    }))
}

pub(crate) async fn node_info(
    State(state): State<Arc<AppState>>,
) -> Result<Json<NodeInfoResponse>, APIError> {
    let data = sdk::node_info(state.clone()).await?;

    Ok(Json(NodeInfoResponse {
        pubkey: data.pubkey,
        num_channels: data.num_channels,
        num_usable_channels: data.num_usable_channels,
        local_balance_sat: data.local_balance_sat,
        eventual_close_fees_sat: data.eventual_close_fees_sat,
        pending_outbound_payments_sat: data.pending_outbound_payments_sat,
        num_peers: data.num_peers,
        account_xpub_vanilla: data.account_xpub_vanilla,
        account_xpub_colored: data.account_xpub_colored,
        max_media_upload_size_mb: state.static_state.max_media_upload_size_mb,
        rgb_htlc_min_msat: HTLC_MIN_MSAT,
        rgb_channel_capacity_min_sat: OPENRGBCHANNEL_MIN_SAT,
        channel_capacity_min_sat: OPENCHANNEL_MIN_SAT,
        channel_capacity_max_sat: OPENCHANNEL_MAX_SAT,
        channel_asset_min_amount: OPENCHANNEL_MIN_RGB_AMT,
        channel_asset_max_amount: u64::MAX,
        network_nodes: data.network_nodes,
        network_channels: data.network_channels,
    }))
}

pub(crate) async fn sign_message(
    State(state): State<Arc<AppState>>,
    WithRejection(Json(payload), _): WithRejection<Json<SignMessageRequest>, APIError>,
) -> Result<Json<SignMessageResponse>, APIError> {
    let data = sdk::sign_message(state.clone(), payload.message).await?;
    Ok(Json(SignMessageResponse {
        signed_message: data.signed_message,
    }))
}


pub(crate) async fn backup(
    State(state): State<Arc<AppState>>,
    WithRejection(Json(payload), _): WithRejection<Json<BackupRequest>, APIError>,
) -> Result<Json<EmptyResponse>, APIError> {
    no_cancel(async move {
        let _guard = state.check_locked().await?;

        let _mnemonic =
            check_password_validity(&payload.password, &state.static_state.storage_dir_path)?;

        do_backup(
            &state.static_state.storage_dir_path,
            Path::new(&payload.backup_path),
            &payload.password,
        )?;

        Ok(Json(EmptyResponse {}))
    })
    .await
}

pub(crate) async fn change_password(
    State(state): State<Arc<AppState>>,
    WithRejection(Json(payload), _): WithRejection<Json<ChangePasswordRequest>, APIError>,
) -> Result<Json<EmptyResponse>, APIError> {
    no_cancel(async move {
        let _guard = state.check_locked().await?;

        check_password_strength(payload.new_password.clone())?;

        let mnemonic =
            check_password_validity(&payload.old_password, &state.static_state.storage_dir_path)?;

        encrypt_and_save_mnemonic(
            payload.new_password,
            mnemonic.to_string(),
            &get_mnemonic_path(&state.static_state.storage_dir_path),
        )?;

        Ok(Json(EmptyResponse {}))
    })
    .await
}

pub(crate) async fn init(
    State(state): State<Arc<AppState>>,
    WithRejection(Json(payload), _): WithRejection<Json<InitRequest>, APIError>,
) -> Result<Json<InitResponse>, APIError> {
    no_cancel(async move {
        let _unlocked_state = state.check_locked().await?;

        check_password_strength(payload.password.clone())?;

        let mnemonic_path = get_mnemonic_path(&state.static_state.storage_dir_path);
        check_already_initialized(&mnemonic_path)?;

        let keys = generate_keys(state.static_state.network);

        let mnemonic = keys.mnemonic;

        encrypt_and_save_mnemonic(payload.password, mnemonic.clone(), &mnemonic_path)?;

        Ok(Json(InitResponse { mnemonic }))
    })
    .await
}

pub(crate) async fn lock(
    State(state): State<Arc<AppState>>,
) -> Result<Json<EmptyResponse>, APIError> {
    tracing::info!("Lock started");
    no_cancel(async move {
        match state.check_unlocked().await {
            Ok(unlocked_state) => {
                state.update_changing_state(true);
                drop(unlocked_state);
            }
            Err(e) => {
                state.update_changing_state(false);
                return Err(e);
            }
        }

        tracing::debug!("Stopping LDK...");
        stop_ldk(state.clone()).await;
        tracing::debug!("LDK stopped");

        state.update_unlocked_app_state(None).await;

        state.update_ldk_background_services(None);

        state.update_changing_state(false);

        tracing::info!("Lock completed");
        Ok(Json(EmptyResponse {}))
    })
    .await
}

pub(crate) async fn restore(
    State(state): State<Arc<AppState>>,
    WithRejection(Json(payload), _): WithRejection<Json<RestoreRequest>, APIError>,
) -> Result<Json<EmptyResponse>, APIError> {
    no_cancel(async move {
        let _unlocked_state = state.check_locked().await?;

        let mnemonic_path = get_mnemonic_path(&state.static_state.storage_dir_path);
        check_already_initialized(&mnemonic_path)?;

        restore_backup(
            Path::new(&payload.backup_path),
            &payload.password,
            &state.static_state.storage_dir_path,
        )?;

        let _mnemonic =
            check_password_validity(&payload.password, &state.static_state.storage_dir_path)?;

        Ok(Json(EmptyResponse {}))
    })
    .await
}

pub(crate) async fn revoke_token(
    State(state): State<Arc<AppState>>,
    WithRejection(Json(payload), _): WithRejection<Json<RevokeTokenRequest>, APIError>,
) -> Result<Json<EmptyResponse>, APIError> {
    let Some(root_pubkey) = state.root_public_key else {
        return Err(APIError::AuthenticationDisabled);
    };

    let token_to_revoke = Biscuit::from_base64(&payload.token, root_pubkey)
        .map_err(|_| APIError::InvalidBiscuitToken)?;
    state.revoke_token(&token_to_revoke)?;

    Ok(Json(EmptyResponse {}))
}

pub(crate) async fn shutdown(
    State(state): State<Arc<AppState>>,
) -> Result<Json<EmptyResponse>, APIError> {
    no_cancel(async move {
        let _unlocked_app_state = state.get_unlocked_app_state();
        state.check_changing_state()?;

        state.cancel_token.cancel();
        Ok(Json(EmptyResponse {}))
    })
    .await
}

pub(crate) async fn unlock(
    State(state): State<Arc<AppState>>,
    WithRejection(Json(payload), _): WithRejection<Json<UnlockRequest>, APIError>,
) -> Result<Json<EmptyResponse>, APIError> {
    tracing::info!("Unlock started");
    no_cancel(async move {
        match state.check_locked().await {
            Ok(unlocked_state) => {
                state.update_changing_state(true);
                drop(unlocked_state);
            }
            Err(e) => {
                return Err(match e {
                    APIError::UnlockedNode => APIError::AlreadyUnlocked,
                    _ => e,
                });
            }
        }

        let mnemonic = match check_password_validity(
            &payload.password,
            &state.static_state.storage_dir_path,
        ) {
            Ok(mnemonic) => mnemonic,
            Err(e) => {
                state.update_changing_state(false);
                return Err(e);
            }
        };

        tracing::debug!("Starting LDK...");
        let (new_ldk_background_services, new_unlocked_app_state) =
            match start_ldk(state.clone(), mnemonic, payload).await {
                Ok((nlbs, nuap)) => (nlbs, nuap),
                Err(e) => {
                    state.update_changing_state(false);
                    return Err(e);
                }
            };
        tracing::debug!("LDK started");

        state
            .update_unlocked_app_state(Some(new_unlocked_app_state))
            .await;

        state.update_ldk_background_services(Some(new_ldk_background_services));

        state.update_changing_state(false);

        tracing::info!("Unlock completed");
        Ok(Json(EmptyResponse {}))
    })
    .await
}


pub(crate) async fn close_channel(
    State(state): State<Arc<AppState>>,
    WithRejection(Json(payload), _): WithRejection<Json<CloseChannelRequest>, APIError>,
) -> Result<Json<EmptyResponse>, APIError> {
    no_cancel(async move {
        let guard = state.check_unlocked().await?;
        let unlocked_state = guard.as_ref().unwrap();

        let channel_id_vec = hex_str_to_vec(&payload.channel_id);
        if channel_id_vec.is_none() || channel_id_vec.as_ref().unwrap().len() != 32 {
            return Err(APIError::InvalidChannelID);
        }
        let requested_cid = ChannelId(channel_id_vec.unwrap().try_into().unwrap());

        let peer_pubkey_vec = match hex_str_to_vec(&payload.peer_pubkey) {
            Some(peer_pubkey_vec) => peer_pubkey_vec,
            None => return Err(APIError::InvalidPubkey),
        };
        let peer_pubkey = match PublicKey::from_slice(&peer_pubkey_vec) {
            Ok(peer_pubkey) => peer_pubkey,
            Err(_) => return Err(APIError::InvalidPubkey),
        };

        if let Some(chan_details) = unlocked_state
            .channel_manager
            .list_channels()
            .iter()
            .find(|c| c.channel_id == requested_cid)
        {
            match chan_details.channel_shutdown_state {
                Some(ChannelShutdownState::NotShuttingDown) => {}
                _ => {
                    return Err(APIError::CannotCloseChannel(s!(
                        "Channel is already being closed"
                    )))
                }
            }
        } else {
            return Err(APIError::UnknownChannelId);
        }

        if payload.force {
            match unlocked_state
                .channel_manager
                .force_close_broadcasting_latest_txn(
                    &requested_cid,
                    &peer_pubkey,
                    "Manually force-closed".to_string(),
                ) {
                Ok(()) => tracing::info!("EVENT: initiating channel force-close"),
                Err(e) => match e {
                    LDKAPIError::APIMisuseError { err } => {
                        return Err(APIError::FailedClosingChannel(err))
                    }
                    _ => return Err(APIError::CannotCloseChannel(format!("{e:?}"))),
                },
            }
        } else {
            match unlocked_state
                .channel_manager
                .close_channel(&requested_cid, &peer_pubkey)
            {
                Ok(()) => tracing::info!("EVENT: initiating channel close"),
                Err(e) => match e {
                    LDKAPIError::APIMisuseError { err } => {
                        return Err(APIError::FailedClosingChannel(err))
                    }
                    _ => return Err(APIError::CannotCloseChannel(format!("{e:?}"))),
                },
            }
        }

        Ok(Json(EmptyResponse {}))
    })
    .await
}

pub(crate) async fn connect_peer(
    State(state): State<Arc<AppState>>,
    WithRejection(Json(payload), _): WithRejection<Json<ConnectPeerRequest>, APIError>,
) -> Result<Json<EmptyResponse>, APIError> {
    no_cancel(async move {
        let guard = state.check_unlocked().await?;
        let unlocked_state = guard.as_ref().unwrap();

        let (peer_pubkey, peer_addr) = parse_peer_info(payload.peer_pubkey_and_addr.to_string())?;

        if let Some(peer_addr) = peer_addr {
            connect_peer_if_necessary(peer_pubkey, peer_addr, unlocked_state.peer_manager.clone())
                .await?;
            disk::persist_channel_peer(
                &state.static_state.ldk_data_dir.join(CHANNEL_PEER_DATA),
                &peer_pubkey,
                &peer_addr,
            )?;
        } else {
            return Err(APIError::InvalidPeerInfo(s!(
                "incorrectly formatted peer info. Should be formatted as: `pubkey@host:port`"
            )));
        }

        Ok(Json(EmptyResponse {}))
    })
    .await
}

pub(crate) async fn disconnect_peer(
    State(state): State<Arc<AppState>>,
    WithRejection(Json(payload), _): WithRejection<Json<DisconnectPeerRequest>, APIError>,
) -> Result<Json<EmptyResponse>, APIError> {
    no_cancel(async move {
        let guard = state.check_unlocked().await?;
        let unlocked_state = guard.as_ref().unwrap();

        let peer_pubkey = match PublicKey::from_str(&payload.peer_pubkey) {
            Ok(pubkey) => pubkey,
            Err(_e) => return Err(APIError::InvalidPubkey),
        };

        //check for open channels with peer
        for channel in unlocked_state.channel_manager.list_channels() {
            if channel.counterparty.node_id == peer_pubkey {
                return Err(APIError::FailedPeerDisconnection(s!(
                    "node has an active channel with this peer, close any channels first"
                )));
            }
        }

        disk::delete_channel_peer(
            &state.static_state.ldk_data_dir.join(CHANNEL_PEER_DATA),
            payload.peer_pubkey,
        )?;

        //check the pubkey matches a valid connected peer
        if unlocked_state
            .peer_manager
            .peer_by_node_id(&peer_pubkey)
            .is_none()
        {
            return Err(APIError::FailedPeerDisconnection(format!(
                "Could not find peer {peer_pubkey}"
            )));
        }

        unlocked_state
            .peer_manager
            .disconnect_by_node_id(peer_pubkey);

        Ok(Json(EmptyResponse {}))
    })
    .await
}

pub(crate) async fn send_onion_message(
    State(state): State<Arc<AppState>>,
    WithRejection(Json(payload), _): WithRejection<Json<SendOnionMessageRequest>, APIError>,
) -> Result<Json<EmptyResponse>, APIError> {
    no_cancel(async move {
        let guard = state.check_unlocked().await?;
        let unlocked_state = guard.as_ref().unwrap();

        if payload.node_ids.is_empty() {
            return Err(APIError::InvalidNodeIds(s!(
                "sendonionmessage requires at least one node id for the path"
            )));
        }

        let mut intermediate_nodes = Vec::new();
        for pk_str in payload.node_ids {
            let node_pubkey_vec = match hex_str_to_vec(&pk_str) {
                Some(peer_pubkey_vec) => peer_pubkey_vec,
                None => {
                    return Err(APIError::InvalidNodeIds(format!(
                        "Couldn't parse peer_pubkey '{pk_str}'"
                    )))
                }
            };
            let node_pubkey = match PublicKey::from_slice(&node_pubkey_vec) {
                Ok(peer_pubkey) => peer_pubkey,
                Err(_) => {
                    return Err(APIError::InvalidNodeIds(format!(
                        "Couldn't parse peer_pubkey '{pk_str}'"
                    )))
                }
            };
            intermediate_nodes.push(node_pubkey);
        }

        if payload.tlv_type < 64 {
            return Err(APIError::InvalidTlvType(s!(
                "need an integral message type above 64"
            )));
        }

        let data = hex_str_to_vec(&payload.data)
            .ok_or(APIError::InvalidOnionData(s!("need a hex data string")))?;

        let destination = Destination::Node(intermediate_nodes.pop().unwrap());
        let message_send_instructions = MessageSendInstructions::WithoutReplyPath { destination };

        unlocked_state
            .onion_messenger
            .send_onion_message(
                UserOnionMessageContents {
                    tlv_type: payload.tlv_type,
                    data,
                },
                message_send_instructions,
            )
            .map_err(|e| APIError::FailedSendingOnionMessage(format!("{e:?}")))?;

        tracing::info!("SUCCESS: forwarded onion message to first hop");

        Ok(Json(EmptyResponse {}))
    })
    .await
}


pub(crate) async fn open_channel(
    State(state): State<Arc<AppState>>,
    WithRejection(Json(payload), _): WithRejection<Json<OpenChannelRequest>, APIError>,
) -> Result<Json<OpenChannelResponse>, APIError> {
    no_cancel(async move {
        let guard = state.check_unlocked().await?;
        let unlocked_state = guard.as_ref().unwrap();

        if *unlocked_state.rgb_send_lock.lock().unwrap() {
            return Err(APIError::OpenChannelInProgress);
        }

        let temporary_channel_id = if let Some(tmp_chan_id_str) = payload.temporary_channel_id {
            let tmp_chan_id = check_channel_id(&tmp_chan_id_str)?;
            if unlocked_state.channel_ids().contains_key(&tmp_chan_id) {
                return Err(APIError::TemporaryChannelIdAlreadyUsed);
            }
            Some(tmp_chan_id)
        } else {
            None
        };

        let colored_info = match (payload.asset_id, payload.asset_amount) {
            (Some(_), Some(amt)) if amt < OPENCHANNEL_MIN_RGB_AMT => {
                return Err(APIError::InvalidAmount(format!(
                    "Channel RGB amount must be equal to or higher than {OPENCHANNEL_MIN_RGB_AMT}"
                )));
            }
            (Some(asset), Some(amt)) => {
                let asset =
                    ContractId::from_str(&asset).map_err(|_| APIError::InvalidAssetID(asset))?;
                Some((asset, amt))
            }
            (None, None) => None,
            _ => {
                return Err(APIError::IncompleteRGBInfo);
            }
        };

        if colored_info.is_some() && payload.capacity_sat < OPENRGBCHANNEL_MIN_SAT {
            return Err(APIError::InvalidAmount(format!(
                "RGB channel amount must be equal to or higher than {OPENRGBCHANNEL_MIN_SAT} sats"
            )));
        } else if payload.capacity_sat < OPENCHANNEL_MIN_SAT {
            return Err(APIError::InvalidAmount(format!(
                "Channel amount must be equal to or higher than {OPENCHANNEL_MIN_SAT} sats"
            )));
        }
        if payload.capacity_sat > OPENCHANNEL_MAX_SAT {
            return Err(APIError::InvalidAmount(format!(
                "Channel amount must be equal to or less than {OPENCHANNEL_MAX_SAT} sats"
            )));
        }

        if payload.push_msat > payload.capacity_sat * 1000 {
            return Err(APIError::InvalidAmount(s!(
                "Channel push amount cannot be higher than the capacity"
            )));
        }

        if colored_info.is_some() && !payload.with_anchors {
            return Err(APIError::AnchorsRequired);
        }

        let (peer_pubkey, mut peer_addr) =
            parse_peer_info(payload.peer_pubkey_and_opt_addr.to_string())?;

        let peer_data_path = state.static_state.ldk_data_dir.join(CHANNEL_PEER_DATA);
        if peer_addr.is_none() {
            if let Some(peer) = unlocked_state.peer_manager.peer_by_node_id(&peer_pubkey) {
                if let Some(socket_address) = peer.socket_address {
                    if let Ok(mut socket_addrs) = socket_address.to_socket_addrs() {
                        // assuming there's only one IP address
                        peer_addr = socket_addrs.next();
                    }
                }
            }
        }
        if peer_addr.is_none() {
            let peer_info = disk::read_channel_peer_data(&peer_data_path)?;
            for (pubkey, addr) in peer_info.into_iter() {
                if pubkey == peer_pubkey {
                    peer_addr = Some(addr);
                    break;
                }
            }
        }
        if let Some(peer_addr) = peer_addr {
            connect_peer_if_necessary(peer_pubkey, peer_addr, unlocked_state.peer_manager.clone())
                .await?;
            disk::persist_channel_peer(&peer_data_path, &peer_pubkey, &peer_addr)?;
        } else {
            return Err(APIError::InvalidPeerInfo(s!(
                "cannot find the address for the provided pubkey"
            )));
        }

        let mut channel_config = ChannelConfig::default();
        if let Some(fee_base_msat) = payload.fee_base_msat {
            channel_config.forwarding_fee_base_msat = fee_base_msat;
        }
        if let Some(fee_proportional_millionths) = payload.fee_proportional_millionths {
            channel_config.forwarding_fee_proportional_millionths = fee_proportional_millionths;
        }
        let config = UserConfig {
            channel_handshake_limits: ChannelHandshakeLimits {
                // lnd's max to_self_delay is 2016, so we want to be compatible.
                their_to_self_delay: 2016,
                ..Default::default()
            },
            channel_handshake_config: ChannelHandshakeConfig {
                announce_for_forwarding: payload.public,
                our_htlc_minimum_msat: HTLC_MIN_MSAT,
                minimum_depth: MIN_CHANNEL_CONFIRMATIONS as u32,
                negotiate_anchors_zero_fee_htlc_tx: payload.with_anchors,
                ..Default::default()
            },
            channel_config,
            ..Default::default()
        };

        let consignment_endpoint = if let Some((contract_id, asset_amount)) = &colored_info {
            let balance = unlocked_state.rgb_get_asset_balance(*contract_id)?;
            let spendable_rgb_amount = balance.spendable;

            if *asset_amount > spendable_rgb_amount {
                return Err(APIError::InsufficientAssets);
            }

            Some(RgbTransport::from_str(&unlocked_state.proxy_endpoint).unwrap())
        } else {
            None
        };

        let schema = if let Some((contract_id, asset_amount)) = &colored_info {
            let mut fake_p2wsh: [u8; 34] = [0; 34];
            fake_p2wsh[1] = 32;
            let script_buf = ScriptBuf::from_bytes(fake_p2wsh.to_vec());
            let recipient_id = recipient_id_from_script_buf(script_buf, state.static_state.network);
            let asset_id = contract_id.to_string();
            let schema = unlocked_state
                .rgb_get_asset_metadata(*contract_id)?
                .asset_schema;
            let assignment = match schema {
                RgbLibAssetSchema::Nia | RgbLibAssetSchema::Cfa => {
                    Assignment::Fungible(*asset_amount)
                }
                RgbLibAssetSchema::Uda => Assignment::NonFungible,
                RgbLibAssetSchema::Ifa => todo!(),
            };

            let recipient_map = map! {
                asset_id => vec![RgbLibRecipient {
                    recipient_id,
                    witness_data: Some(RgbLibWitnessData {
                        amount_sat: payload.capacity_sat,
                        blinding: Some(STATIC_BLINDING + 1),
                    }),
                    assignment: assignment.into(),
                    transport_endpoints: vec![unlocked_state.proxy_endpoint.clone()]
            }]};

            let unlocked_state_copy = unlocked_state.clone();
            tokio::task::spawn_blocking(move || {
                unlocked_state_copy.rgb_send_begin(
                    recipient_map,
                    true,
                    FEE_RATE,
                    MIN_CHANNEL_CONFIRMATIONS,
                )
            })
            .await
            .unwrap()?;
            Some(schema)
        } else {
            None
        };

        *unlocked_state.rgb_send_lock.lock().unwrap() = true;
        tracing::debug!("RGB send lock set to true");

        let temporary_channel_id = unlocked_state
            .channel_manager
            .create_channel(
                peer_pubkey,
                payload.capacity_sat,
                payload.push_msat,
                0,
                temporary_channel_id,
                Some(config),
                consignment_endpoint,
            )
            .map_err(|e| {
                *unlocked_state.rgb_send_lock.lock().unwrap() = false;
                tracing::debug!("RGB send lock set to false (open channel failure: {e:?})");
                match e {
                    LDKAPIError::APIMisuseError { err }
                        if err.contains("fee for initial commitment transaction") =>
                    {
                        let mut commitment_tx_fee = 0;
                        let re =
                            Regex::new(r"fee for initial commitment transaction fee of (\d+).")
                                .unwrap();
                        if let Some(captures) = re.captures(&err) {
                            if let Some(fee_str) = captures.get(1) {
                                commitment_tx_fee = fee_str.as_str().parse().unwrap();
                            }
                        }
                        APIError::InsufficientCapacity(commitment_tx_fee)
                    }
                    _ => APIError::FailedOpenChannel(format!("{e:?}")),
                }
            })?;
        let temporary_channel_id = temporary_channel_id.0.as_hex().to_string();
        tracing::info!("EVENT: initiated channel with peer {}", peer_pubkey);

        if let Some((contract_id, asset_amount)) = &colored_info {
            let rgb_info = RgbInfo {
                contract_id: *contract_id,
                schema: schema.unwrap(),
                local_rgb_amount: *asset_amount,
                remote_rgb_amount: 0,
            };
            write_rgb_channel_info(
                &get_rgb_channel_info_path(
                    &temporary_channel_id,
                    &state.static_state.ldk_data_dir,
                    true,
                ),
                &rgb_info,
            );
            write_rgb_channel_info(
                &get_rgb_channel_info_path(
                    &temporary_channel_id,
                    &state.static_state.ldk_data_dir,
                    false,
                ),
                &rgb_info,
            );
        }

        Ok(Json(OpenChannelResponse {
            temporary_channel_id,
        }))
    })
    .await
}


pub(crate) async fn create_utxos(
    State(state): State<Arc<AppState>>,
    WithRejection(Json(payload), _): WithRejection<Json<CreateUtxosRequest>, APIError>,
) -> Result<Json<EmptyResponse>, APIError> {
    no_cancel(async move {
        let guard = state.check_unlocked().await?;
        let unlocked_state = guard.as_ref().unwrap();

        unlocked_state.rgb_create_utxos(
            payload.up_to,
            payload.num.unwrap_or(UTXO_NUM),
            payload.size.unwrap_or(UTXO_SIZE_SAT),
            payload.fee_rate,
            payload.skip_sync,
        )?;
        tracing::debug!("UTXO creation complete");

        Ok(Json(EmptyResponse {}))
    })
    .await
}

pub(crate) async fn fail_transfers(
    State(state): State<Arc<AppState>>,
    WithRejection(Json(payload), _): WithRejection<Json<FailTransfersRequest>, APIError>,
) -> Result<Json<FailTransfersResponse>, APIError> {
    no_cancel(async move {
        let guard = state.check_unlocked().await?;
        let unlocked_state = guard.as_ref().unwrap();

        let unlocked_state_copy = unlocked_state.clone();
        let transfers_changed = tokio::task::spawn_blocking(move || {
            unlocked_state_copy.rgb_fail_transfers(
                payload.batch_transfer_idx,
                payload.no_asset_only,
                payload.skip_sync,
            )
        })
        .await
        .unwrap()?;

        Ok(Json(FailTransfersResponse { transfers_changed }))
    })
    .await
}

pub(crate) async fn send_btc(
    State(state): State<Arc<AppState>>,
    WithRejection(Json(payload), _): WithRejection<Json<SendBtcRequest>, APIError>,
) -> Result<Json<SendBtcResponse>, APIError> {
    no_cancel(async move {
        let guard = state.check_unlocked().await?;
        let unlocked_state = guard.as_ref().unwrap();

        let txid = unlocked_state.rgb_send_btc(
            payload.address,
            payload.amount,
            payload.fee_rate,
            payload.skip_sync,
        )?;

        Ok(Json(SendBtcResponse { txid }))
    })
    .await
}

pub(crate) async fn sync(
    State(state): State<Arc<AppState>>,
) -> Result<Json<EmptyResponse>, APIError> {
    no_cancel(async move {
        let guard = state.check_unlocked().await?;
        let unlocked_state = guard.as_ref().unwrap();

        unlocked_state.rgb_sync()?;

        Ok(Json(EmptyResponse {}))
    })
    .await
}


pub(crate) async fn decode_ln_invoice(
    State(state): State<Arc<AppState>>,
    WithRejection(Json(payload), _): WithRejection<Json<DecodeLNInvoiceRequest>, APIError>,
) -> Result<Json<DecodeLNInvoiceResponse>, APIError> {
    let data = sdk::decode_ln_invoice(state.clone(), payload.invoice).await?;
    Ok(Json(DecodeLNInvoiceResponse {
        amt_msat: data.amt_msat,
        expiry_sec: data.expiry_sec,
        timestamp: data.timestamp,
        asset_id: data.asset_id,
        asset_amount: data.asset_amount,
        payment_hash: data.payment_hash,
        payment_secret: data.payment_secret,
        payee_pubkey: data.payee_pubkey,
        network: data.network.into(),
    }))
}

pub(crate) async fn decode_rgb_invoice(
    State(state): State<Arc<AppState>>,
    WithRejection(Json(payload), _): WithRejection<Json<DecodeRGBInvoiceRequest>, APIError>,
) -> Result<Json<DecodeRGBInvoiceResponse>, APIError> {
    let data = sdk::decode_rgb_invoice(state.clone(), payload.invoice).await?;
    Ok(Json(DecodeRGBInvoiceResponse {
        recipient_id: data.recipient_id,
        recipient_type: data.recipient_type.into(),
        asset_schema: data.asset_schema.map(Into::into),
        asset_id: data.asset_id,
        assignment: data.assignment.into(),
        network: data.network.into(),
        expiration_timestamp: data.expiration_timestamp,
        transport_endpoints: data.transport_endpoints,
    }))
}

pub(crate) async fn get_payment(
    State(state): State<Arc<AppState>>,
    WithRejection(Json(payload), _): WithRejection<Json<GetPaymentRequest>, APIError>,
) -> Result<Json<GetPaymentResponse>, APIError> {
    let data = sdk::get_payment(state.clone(), payload.payment_hash).await?;
    Ok(Json(GetPaymentResponse {
        payment: Payment {
            amt_msat: data.amt_msat,
            asset_amount: data.asset_amount,
            asset_id: data.asset_id,
            payment_hash: data.payment_hash,
            inbound: data.inbound,
            status: data.status.into(),
            created_at: data.created_at,
            updated_at: data.updated_at,
            payee_pubkey: data.payee_pubkey,
        },
    }))
}

pub(crate) async fn get_swap(
    State(state): State<Arc<AppState>>,
    WithRejection(Json(payload), _): WithRejection<Json<GetSwapRequest>, APIError>,
) -> Result<Json<GetSwapResponse>, APIError> {
    let data = sdk::get_swap(state.clone(), payload.payment_hash, payload.taker).await?;
    Ok(Json(GetSwapResponse {
        swap: Swap {
            qty_from: data.qty_from,
            qty_to: data.qty_to,
            from_asset: data.from_asset,
            to_asset: data.to_asset,
            payment_hash: data.payment_hash,
            status: data.status.into(),
            requested_at: data.requested_at,
            initiated_at: data.initiated_at,
            expires_at: data.expires_at,
            completed_at: data.completed_at,
        },
    }))
}

pub(crate) async fn invoice_status(
    State(state): State<Arc<AppState>>,
    WithRejection(Json(payload), _): WithRejection<Json<InvoiceStatusRequest>, APIError>,
) -> Result<Json<InvoiceStatusResponse>, APIError> {
    let data = sdk::invoice_status(state.clone(), payload.invoice).await?;
    Ok(Json(InvoiceStatusResponse {
        status: match data.status {
            sdk::InvoiceStatus::Pending => InvoiceStatus::Pending,
            sdk::InvoiceStatus::Succeeded => InvoiceStatus::Succeeded,
            sdk::InvoiceStatus::Failed => InvoiceStatus::Failed,
            sdk::InvoiceStatus::Expired => InvoiceStatus::Expired,
        },
    }))
}

pub(crate) async fn list_payments(
    State(state): State<Arc<AppState>>,
) -> Result<Json<ListPaymentsResponse>, APIError> {
    let data = sdk::list_payments(state.clone()).await?;
    let payments = data
        .into_iter()
        .map(|p| Payment {
            amt_msat: p.amt_msat,
            asset_amount: p.asset_amount,
            asset_id: p.asset_id,
            payment_hash: p.payment_hash,
            inbound: p.inbound,
            status: p.status.into(),
            created_at: p.created_at,
            updated_at: p.updated_at,
            payee_pubkey: p.payee_pubkey,
        })
        .collect();

    Ok(Json(ListPaymentsResponse { payments }))
}

pub(crate) async fn list_swaps(
    State(state): State<Arc<AppState>>,
) -> Result<Json<ListSwapsResponse>, APIError> {
    let data = sdk::list_swaps(state.clone()).await?;

    Ok(Json(ListSwapsResponse {
        taker: data
            .taker
            .iter()
            .map(|s| Swap {
                qty_from: s.qty_from,
                qty_to: s.qty_to,
                from_asset: s.from_asset.clone(),
                to_asset: s.to_asset.clone(),
                payment_hash: s.payment_hash.clone(),
                status: s.status.into(),
                requested_at: s.requested_at,
                initiated_at: s.initiated_at,
                expires_at: s.expires_at,
                completed_at: s.completed_at,
            })
            .collect(),
        maker: data
            .maker
            .iter()
            .map(|s| Swap {
                qty_from: s.qty_from,
                qty_to: s.qty_to,
                from_asset: s.from_asset.clone(),
                to_asset: s.to_asset.clone(),
                payment_hash: s.payment_hash.clone(),
                status: s.status.into(),
                requested_at: s.requested_at,
                initiated_at: s.initiated_at,
                expires_at: s.expires_at,
                completed_at: s.completed_at,
            })
            .collect(),
    }))
}


pub(crate) async fn keysend(
    State(state): State<Arc<AppState>>,
    WithRejection(Json(payload), _): WithRejection<Json<KeysendRequest>, APIError>,
) -> Result<Json<KeysendResponse>, APIError> {
    no_cancel(async move {
        let guard = state.check_unlocked().await?;
        let unlocked_state = guard.as_ref().unwrap();

        let dest_pubkey = match hex_str_to_compressed_pubkey(&payload.dest_pubkey) {
            Some(pk) => pk,
            None => return Err(APIError::InvalidPubkey),
        };

        let amt_msat = payload.amt_msat;
        if amt_msat < HTLC_MIN_MSAT {
            return Err(APIError::InvalidAmount(format!(
                "amt_msat cannot be less than {HTLC_MIN_MSAT}"
            )));
        }

        let payment_preimage =
            PaymentPreimage(unlocked_state.keys_manager.get_secure_random_bytes());
        let payment_hash_inner = Sha256::hash(&payment_preimage.0[..]).to_byte_array();
        let payment_id = PaymentId(payment_hash_inner);
        let payment_hash = PaymentHash(payment_hash_inner);

        let rgb_payment = match (payload.asset_id, payload.asset_amount) {
            (Some(asset_id), Some(rgb_amount)) => {
                let contract_id = ContractId::from_str(&asset_id)
                    .map_err(|_| APIError::InvalidAssetID(asset_id))?;
                Some((contract_id, rgb_amount))
            }
            (None, None) => None,
            _ => {
                return Err(APIError::IncompleteRGBInfo);
            }
        };

        let route_params = RouteParameters::from_payment_params_and_value(
            PaymentParameters::for_keysend(dest_pubkey, 40, false),
            amt_msat,
            rgb_payment,
        );
        let created_at = get_current_timestamp();
        unlocked_state.add_outbound_payment(
            payment_id,
            PaymentInfo {
                preimage: None,
                secret: None,
                status: HTLCStatus::Pending.into(),
                amt_msat: Some(amt_msat),
                created_at,
                updated_at: created_at,
                payee_pubkey: dest_pubkey,
            },
        )?;
        if let Some((contract_id, rgb_amount)) = rgb_payment {
            write_rgb_payment_info_file(
                &PathBuf::from(&state.static_state.ldk_data_dir),
                &payment_hash,
                contract_id,
                rgb_amount,
                false,
                false,
            );
        }

        let status = match unlocked_state.channel_manager.send_spontaneous_payment(
            Some(payment_preimage),
            RecipientOnionFields::spontaneous_empty(),
            payment_id,
            route_params,
            Retry::Timeout(Duration::from_secs(10)),
        ) {
            Ok(_payment_hash) => {
                tracing::info!(
                    "EVENT: initiated sending {} msats to {}",
                    amt_msat,
                    dest_pubkey
                );
                HTLCStatus::Pending
            }
            Err(e) => {
                tracing::error!("ERROR: failed to send payment: {:?}", e);
                unlocked_state
                    .update_outbound_payment_status(payment_id, HTLCStatus::Failed.into());
                HTLCStatus::Failed
            }
        };

        Ok(Json(KeysendResponse {
            payment_hash: hex_str(&payment_hash.0),
            payment_preimage: hex_str(&payment_preimage.0),
            status,
        }))
    })
    .await
}

pub(crate) async fn ln_invoice(
    State(state): State<Arc<AppState>>,
    WithRejection(Json(payload), _): WithRejection<Json<LNInvoiceRequest>, APIError>,
) -> Result<Json<LNInvoiceResponse>, APIError> {
    no_cancel(async move {
        let data = sdk::create_ln_invoice(
            state.clone(),
            payload.amt_msat,
            payload.expiry_sec,
            payload.asset_id,
            payload.asset_amount,
            INVOICE_MIN_MSAT,
        )
        .await?;

        Ok(Json(LNInvoiceResponse {
            invoice: data.invoice,
        }))
    })
    .await
}

pub(crate) async fn send_payment(
    State(state): State<Arc<AppState>>,
    WithRejection(Json(payload), _): WithRejection<Json<SendPaymentRequest>, APIError>,
) -> Result<Json<SendPaymentResponse>, APIError> {
    no_cancel(async move {
        let guard = state.check_unlocked().await?;
        let unlocked_state = guard.as_ref().unwrap();

        let mut status = HTLCStatus::Pending;
        let created_at = get_current_timestamp();

        let (payment_id, payment_hash, payment_secret) = if let Ok(offer) = Offer::from_str(&payload.invoice) {
            let random_bytes = unlocked_state.keys_manager.get_secure_random_bytes();
            let payment_id = PaymentId(random_bytes);

            let amt_msat = match (offer.amount(), payload.amt_msat) {
                (Some(offer::Amount::Bitcoin { amount_msats }), _) => amount_msats,
                (_, Some(amt)) => amt,
                (amt, _) => {
                    return Err(APIError::InvalidAmount(format!(
                        "cannot process non-Bitcoin-denominated offer value {amt:?}"
                    )));
                },
            };
            if payload.amt_msat.is_some() && payload.amt_msat != Some(amt_msat) {
                return Err(APIError::InvalidAmount(format!(
                    "amount didn't match offer of {amt_msat}msat"
                )));
            }

            // TODO: add and check RGB amount after enabling RGB support for offers

            let secret = None;

            unlocked_state.add_outbound_payment(
                payment_id,
                PaymentInfo {
                    preimage: None,
                    secret,
                    status: status.into(),
                    amt_msat: Some(amt_msat),
                    created_at,
                    updated_at: created_at,
                    payee_pubkey: offer.issuer_signing_pubkey().ok_or(APIError::InvalidInvoice(s!("missing signing pubkey")))?,
                },
            )?;

            let params = OptionalOfferPaymentParams {
                retry_strategy: Retry::Timeout(Duration::from_secs(10)),
                ..Default::default()
            };
            let pay = unlocked_state.channel_manager
                .pay_for_offer(&offer, Some(amt_msat), payment_id, params);
            if pay.is_err() {
                tracing::error!("ERROR: failed to pay: {:?}", pay);
                unlocked_state.update_outbound_payment_status(payment_id, HTLCStatus::Failed.into());
                status = HTLCStatus::Failed;
                unlocked_state.update_outbound_payment_status(payment_id, status.into());
            }
            (payment_id, None, secret)
        } else {
            let invoice = match Bolt11Invoice::from_str(&payload.invoice) {
                Err(e) => return Err(APIError::InvalidInvoice(e.to_string())),
                Ok(v) => v,
            };

            let payment_id = PaymentId((*invoice.payment_hash()).to_byte_array());
            let payment_secret = Some(*invoice.payment_secret());
            let zero_amt_invoice =
                invoice.amount_milli_satoshis().is_none() || invoice.amount_milli_satoshis() == Some(0);

            let amt_msat = if zero_amt_invoice {
                if let Some(amt_msat) = payload.amt_msat {
                    amt_msat
                } else {
                    return Err(APIError::InvalidAmount(s!(
                        "need an amount for the given 0-value invoice"
                    )));
                }
            } else {
                if payload.amt_msat.is_some() && invoice.amount_milli_satoshis() != payload.amt_msat
                {
                    return Err(APIError::InvalidAmount(format!(
                        "amount didn't match invoice value of {}msat", invoice.amount_milli_satoshis().unwrap_or(0)
                    )));
                }
                invoice.amount_milli_satoshis().unwrap_or(0)
            };

            let rgb_payment = match (invoice.rgb_contract_id(), invoice.rgb_amount()) {
                (Some(rgb_contract_id), Some(rgb_amount)) => {
                    if amt_msat < INVOICE_MIN_MSAT {
                        return Err(APIError::InvalidAmount(format!(
                            "msat amount in invoice sending an RGB asset cannot be less than {INVOICE_MIN_MSAT}"
                        )));
                    }
                    Some((rgb_contract_id, rgb_amount))
                },
                (None, None) => None,
                (Some(_), None) => {
                    return Err(APIError::InvalidInvoice(s!(
                        "invoice has an RGB contract ID but not an RGB amount"
                    )))
                }
                (None, Some(_)) => {
                    return Err(APIError::InvalidInvoice(s!(
                        "invoice has an RGB amount but not an RGB contract ID"
                    )))
                }
            };

            let secret = payment_secret;
            unlocked_state.add_outbound_payment(
                payment_id,
                PaymentInfo {
                    preimage: None,
                    secret,
                    status: status.into(),
                    amt_msat: invoice.amount_milli_satoshis(),
                    created_at,
                    updated_at: created_at,
                    payee_pubkey: invoice.get_payee_pub_key(),
                },
            )?;
            let payment_hash = PaymentHash(invoice.payment_hash().to_byte_array());
            if let Some((contract_id, rgb_amount)) = rgb_payment {
                write_rgb_payment_info_file(
                    &PathBuf::from(&state.static_state.ldk_data_dir),
                    &payment_hash,
                    contract_id,
                    rgb_amount,
                    false,
                    false,
                );
            }

            match unlocked_state.channel_manager.pay_for_bolt11_invoice(
                &invoice,
                payment_id,
                Some(amt_msat),
                RouteParametersConfig::default(),
                Retry::Timeout(Duration::from_secs(10)),
            ) {
                Ok(_) => {
                    let payee_pubkey = invoice.recover_payee_pub_key();
                    tracing::info!(
                        "EVENT: initiated sending {} msats to {}",
                        amt_msat,
                        payee_pubkey
                    );
                },
                Err(e) => {
                    tracing::error!("ERROR: failed to send payment: {:?}", e);
                    status = HTLCStatus::Failed;
                    unlocked_state.update_outbound_payment_status(payment_id, status.into());
                },
            };

            (payment_id, Some(payment_hash), secret)
        };

        Ok(Json(SendPaymentResponse {
            payment_id: hex_str(&payment_id.0),
            payment_hash: payment_hash.map(|h| hex_str(&h.0)),
            payment_secret: payment_secret.map(|s| hex_str(&s.0)),
            status,
        }))
    })
    .await
}


pub(crate) async fn maker_execute(
    State(state): State<Arc<AppState>>,
    WithRejection(Json(payload), _): WithRejection<Json<MakerExecuteRequest>, APIError>,
) -> Result<Json<EmptyResponse>, APIError> {
    no_cancel(async move {
        let guard = state.check_unlocked().await?;
        let unlocked_state = guard.as_ref().unwrap();

        let swapstring = SwapString::from_str(&payload.swapstring)
            .map_err(|e| APIError::InvalidSwapString(payload.swapstring.clone(), e.to_string()))?;
        let payment_secret = hex_str_to_vec(&payload.payment_secret)
            .and_then(|data| data.try_into().ok())
            .map(PaymentSecret)
            .ok_or(APIError::InvalidPaymentSecret)?;
        let taker_pk =
            PublicKey::from_str(&payload.taker_pubkey).map_err(|_| APIError::InvalidPubkey)?;

        if get_current_timestamp() > swapstring.swap_info.expiry {
            unlocked_state
                .update_maker_swap_status(&swapstring.payment_hash, SwapStatus::Expired.into());
            return Err(APIError::ExpiredSwapOffer);
        }

        let payment_preimage = unlocked_state
            .channel_manager
            .get_payment_preimage(swapstring.payment_hash, payment_secret)
            .map_err(|_| APIError::MissingSwapPaymentPreimage)?;

        let swap_info = swapstring.swap_info;

        let receive_hints = unlocked_state
            .channel_manager
            .list_usable_channels()
            .iter()
            .filter(|details| {
                match get_rgb_channel_info_optional(
                    &details.channel_id,
                    &state.static_state.ldk_data_dir,
                    false,
                ) {
                    _ if swap_info.is_from_btc() => true,
                    Some((rgb_info, _)) if Some(rgb_info.contract_id) == swap_info.from_asset => {
                        true
                    }
                    _ => false,
                }
            })
            .map(|details| {
                let config = details.counterparty.forwarding_info.as_ref().unwrap();
                RouteHint(vec![RouteHintHop {
                    src_node_id: details.counterparty.node_id,
                    short_channel_id: details.short_channel_id.unwrap(),
                    cltv_expiry_delta: config.cltv_expiry_delta,
                    htlc_maximum_msat: None,
                    htlc_minimum_msat: None,
                    fees: RoutingFees {
                        base_msat: config.fee_base_msat,
                        proportional_millionths: config.fee_proportional_millionths,
                    },
                    htlc_maximum_rgb: None,
                }])
            })
            .collect();

        let rgb_payment = swap_info
            .to_asset
            .map(|to_asset| (to_asset, swap_info.qty_to));
        let first_leg = get_route(
            &unlocked_state.channel_manager,
            &unlocked_state.router,
            unlocked_state.channel_manager.get_our_node_id(),
            taker_pk,
            if swap_info.is_to_btc() {
                Some(swap_info.qty_to + HTLC_MIN_MSAT)
            } else {
                Some(HTLC_MIN_MSAT)
            },
            rgb_payment,
            vec![],
        );

        let rgb_payment = swap_info
            .from_asset
            .map(|from_asset| (from_asset, swap_info.qty_from));
        let second_leg = get_route(
            &unlocked_state.channel_manager,
            &unlocked_state.router,
            taker_pk,
            unlocked_state.channel_manager.get_our_node_id(),
            if swap_info.is_to_btc() || swap_info.is_asset_asset() {
                Some(HTLC_MIN_MSAT)
            } else {
                Some(swap_info.qty_from + HTLC_MIN_MSAT)
            },
            rgb_payment,
            receive_hints,
        );

        let (mut first_leg, mut second_leg) = match (first_leg, second_leg) {
            (Some(f), Some(s)) => (f, s),
            _ => {
                return Err(APIError::NoRoute);
            }
        };

        // Set swap flag
        second_leg.paths[0].hops[0].short_channel_id |= IS_SWAP_SCID;

        // Generally in the last hop the fee_amount is set to the payment amount, so we need to
        // override it with fee = 0
        first_leg.paths[0]
            .hops
            .last_mut()
            .expect("Path not to be empty")
            .fee_msat = 0;

        let fullpaths = first_leg.paths[0]
            .hops
            .clone()
            .into_iter()
            .map(|mut hop| {
                if swap_info.is_to_asset() {
                    hop.rgb_payment = Some((swap_info.to_asset.unwrap(), swap_info.qty_to));
                }
                hop
            })
            .chain(second_leg.paths[0].hops.clone().into_iter().map(|mut hop| {
                if swap_info.is_from_asset() {
                    hop.rgb_payment = Some((swap_info.from_asset.unwrap(), swap_info.qty_from));
                }
                hop
            }))
            .collect::<Vec<_>>();

        // Skip last fee because it's equal to the payment amount
        let total_fee = fullpaths
            .iter()
            .rev()
            .skip(1)
            .map(|hop| hop.fee_msat)
            .sum::<u64>();

        if total_fee >= MAX_SWAP_FEE_MSAT {
            return Err(APIError::FailedPayment(format!(
                "Fee too high: {total_fee}"
            )));
        }

        let route = Route {
            paths: vec![LnPath {
                hops: fullpaths,
                blinded_tail: None,
            }],
            route_params: Some(RouteParameters {
                payment_params: PaymentParameters::for_keysend(
                    unlocked_state.channel_manager.get_our_node_id(),
                    DEFAULT_FINAL_CLTV_EXPIRY_DELTA,
                    false,
                ),
                // This value is not used anywhere, it's set by the router
                // when creating a route, but here we are creating it manually
                // by composing a pre-existing list of hops
                final_value_msat: 0,
                max_total_routing_fee_msat: None,
                // This value is not used anywhere, same as final_value_msat
                rgb_payment: None,
            }),
        };

        if swap_info.is_to_asset() {
            write_rgb_payment_info_file(
                &state.static_state.ldk_data_dir,
                &swapstring.payment_hash,
                swap_info.to_asset.unwrap(),
                swap_info.qty_to,
                true,
                false,
            );
        }

        unlocked_state
            .update_maker_swap_status(&swapstring.payment_hash, SwapStatus::Pending.into());

        let payment_hash: PaymentHash = payment_preimage.into();
        let (_status, err) = match unlocked_state
            .channel_manager
            .send_spontaneous_payment_with_route(
                route,
                payment_hash,
                payment_preimage,
                RecipientOnionFields::spontaneous_empty(),
                PaymentId(swapstring.payment_hash.0),
            ) {
            Ok(()) => {
                tracing::debug!("EVENT: initiated swap");
                (HTLCStatus::Pending, None)
            }
            Err(e) => {
                tracing::warn!("ERROR: failed to send payment: {:?}", e);
                (HTLCStatus::Failed, Some(e))
            }
        };

        match err {
            None => Ok(Json(EmptyResponse {})),
            Some(e) => {
                unlocked_state
                    .update_maker_swap_status(&swapstring.payment_hash, SwapStatus::Failed.into());
                Err(APIError::FailedPayment(format!("{e:?}")))
            }
        }
    })
    .await
}

pub(crate) async fn maker_init(
    State(state): State<Arc<AppState>>,
    WithRejection(Json(payload), _): WithRejection<Json<MakerInitRequest>, APIError>,
) -> Result<Json<MakerInitResponse>, APIError> {
    no_cancel(async move {
        let guard = state.check_unlocked().await?;
        let unlocked_state = guard.as_ref().unwrap();

        let from_asset = match &payload.from_asset {
            None => None,
            Some(asset) => Some(
                ContractId::from_str(asset).map_err(|_| APIError::InvalidAssetID(asset.clone()))?,
            ),
        };

        let to_asset = match &payload.to_asset {
            None => None,
            Some(asset) => Some(
                ContractId::from_str(asset).map_err(|_| APIError::InvalidAssetID(asset.clone()))?,
            ),
        };

        // prevent BTC-to-BTC swaps
        if from_asset.is_none() && to_asset.is_none() {
            return Err(APIError::InvalidSwap(s!("cannot swap BTC for BTC")));
        }

        // prevent swaps of same assets
        if from_asset == to_asset {
            return Err(APIError::InvalidSwap(s!("cannot swap the same asset")));
        }

        let qty_from = payload.qty_from;
        let qty_to = payload.qty_to;

        let expiry = get_current_timestamp() + payload.timeout_sec as u64;
        let swap_info = SwapInfo {
            from_asset,
            to_asset,
            qty_from,
            qty_to,
            expiry,
        };
        let swap_data = SwapData::create_from_swap_info(&swap_info);

        // Check that we have enough assets to send
        if let Some(to_asset) = to_asset {
            let max_balance = get_max_local_rgb_amount(
                to_asset,
                &state.static_state.ldk_data_dir,
                unlocked_state.channel_manager.list_channels().iter(),
            );
            if swap_info.qty_to > max_balance {
                return Err(APIError::InsufficientAssets);
            }
        }

        let (payment_hash, payment_secret) = unlocked_state
            .channel_manager
            .create_inbound_payment(Some(DUST_LIMIT_MSAT), payload.timeout_sec, None)
            .unwrap();
        unlocked_state.add_maker_swap(payment_hash, swap_data);

        let swapstring = SwapString::from_swap_info(&swap_info, payment_hash).to_string();

        let payment_secret = payment_secret.0.as_hex().to_string();
        let payment_hash = payment_hash.0.as_hex().to_string();
        Ok(Json(MakerInitResponse {
            payment_hash,
            payment_secret,
            swapstring,
        }))
    })
    .await
}

pub(crate) async fn taker(
    State(state): State<Arc<AppState>>,
    WithRejection(Json(payload), _): WithRejection<Json<TakerRequest>, APIError>,
) -> Result<Json<EmptyResponse>, APIError> {
    no_cancel(async move {
        let guard = state.check_unlocked().await?;
        let unlocked_state = guard.as_ref().unwrap();
        let swapstring = SwapString::from_str(&payload.swapstring)
            .map_err(|e| APIError::InvalidSwapString(payload.swapstring.clone(), e.to_string()))?;

        if get_current_timestamp() > swapstring.swap_info.expiry {
            return Err(APIError::ExpiredSwapOffer);
        }

        // We are selling assets, check if we have enough
        if let Some(from_asset) = swapstring.swap_info.from_asset {
            let max_balance = get_max_local_rgb_amount(
                from_asset,
                &state.static_state.ldk_data_dir,
                unlocked_state.channel_manager.list_channels().iter(),
            );
            if swapstring.swap_info.qty_from > max_balance {
                return Err(APIError::InsufficientAssets);
            }
        }

        let swap_data = SwapData::create_from_swap_info(&swapstring.swap_info);
        unlocked_state.add_taker_swap(swapstring.payment_hash, swap_data);

        Ok(Json(EmptyResponse {}))
    })
    .await
}
