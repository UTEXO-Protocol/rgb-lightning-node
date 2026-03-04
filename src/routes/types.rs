use super::*;

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
