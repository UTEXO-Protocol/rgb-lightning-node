use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NetworkInfoData {
    pub network: String,
    pub height: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NodeInfoData {
    pub pubkey: String,
    pub num_channels: usize,
    pub num_usable_channels: usize,
    pub local_balance_sat: u64,
    pub eventual_close_fees_sat: u64,
    pub pending_outbound_payments_sat: u64,
    pub num_peers: usize,
    pub account_xpub_vanilla: String,
    pub account_xpub_colored: String,
    pub max_media_upload_size_mb: u16,
    pub rgb_htlc_min_msat: u64,
    pub rgb_channel_capacity_min_sat: u64,
    pub channel_capacity_min_sat: u64,
    pub channel_capacity_max_sat: u64,
    pub channel_asset_min_amount: u64,
    pub channel_asset_max_amount: u64,
    pub network_nodes: usize,
    pub network_channels: usize,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
pub enum ChannelStatus {
    #[default]
    Opening,
    Opened,
    Closing,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChannelData {
    pub channel_id: String,
    pub funding_txid: Option<String>,
    pub peer_pubkey: String,
    pub peer_alias: Option<String>,
    pub short_channel_id: Option<u64>,
    pub status: ChannelStatus,
    pub ready: bool,
    pub capacity_sat: u64,
    pub local_balance_sat: u64,
    pub outbound_balance_msat: u64,
    pub inbound_balance_msat: u64,
    pub next_outbound_htlc_limit_msat: u64,
    pub next_outbound_htlc_minimum_msat: u64,
    pub is_usable: bool,
    pub public: bool,
    pub asset_id: Option<String>,
    pub asset_local_amount: Option<u64>,
    pub asset_remote_amount: Option<u64>,
    pub virtual_open_mode: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PeerData {
    pub pubkey: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AddressData {
    pub address: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BtcBalance {
    pub settled: u64,
    pub future: u64,
    pub spendable: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BtcBalanceData {
    pub vanilla: BtcBalance,
    pub colored: BtcBalance,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AssetBalanceData {
    pub settled: u64,
    pub future: u64,
    pub spendable: u64,
    pub offchain_outbound: u64,
    pub offchain_inbound: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EstimateFeeData {
    pub fee_rate: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AssetMediaData {
    pub bytes_hex: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SignMessageData {
    pub signed_message: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChannelIdData {
    pub channel_id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LnInvoiceData {
    pub invoice: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum HtlcStatus {
    Pending,
    Succeeded,
    Failed,
    Claimable,
    Claiming,
    Cancelled,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum PaymentType {
    Outbound,
    InboundAutoClaim,
    InboundHodl,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum InvoiceStatus {
    Pending,
    Claimable,
    Claiming,
    Succeeded,
    Cancelled,
    Failed,
    Expired,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InvoiceStatusData {
    pub status: InvoiceStatus,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PaymentData {
    pub amt_msat: Option<u64>,
    pub asset_amount: Option<u64>,
    pub asset_id: Option<String>,
    pub payment_hash: String,
    pub payment_type: PaymentType,
    pub status: HtlcStatus,
    pub created_at: u64,
    pub updated_at: u64,
    pub payee_pubkey: String,
    pub preimage: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DecodeLnInvoiceData {
    pub amt_msat: Option<u64>,
    pub expiry_sec: u64,
    pub timestamp: u64,
    pub asset_id: Option<String>,
    pub asset_amount: Option<u64>,
    pub payment_hash: String,
    pub payment_secret: String,
    pub payee_pubkey: Option<String>,
    pub network: String,
}
