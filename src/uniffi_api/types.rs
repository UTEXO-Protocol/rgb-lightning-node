use std::sync::Arc;

use crate::utils::AppState;

pub type PublicKey = bitcoin::secp256k1::PublicKey;
pub type Txid = bitcoin::Txid;
pub type ContractId = rgb_lib::ContractId;
pub type ChannelId = lightning::ln::types::ChannelId;
pub type PaymentHash = lightning::types::payment::PaymentHash;
pub type Bolt11Invoice = lightning_invoice::Bolt11Invoice;

pub struct RecipientId(pub String);
pub struct TransportEndpoint(pub String);

#[derive(Debug, thiserror::Error)]
pub enum RlnError {
    #[error("node is not initialized")]
    NotInitialized,
    #[error("invalid request")]
    InvalidRequest,
    #[error("internal error")]
    Internal,
}

pub struct NodeInfoV1 {
    pub pubkey: PublicKey,
    pub num_channels: u64,
    pub num_peers: u64,
    pub network_nodes: u64,
    pub network_channels: u64,
}

pub struct PaymentV1 {
    pub amt_msat: Option<u64>,
    pub asset_amount: Option<u64>,
    pub asset_id: Option<ContractId>,
    pub payment_hash: PaymentHash,
    pub inbound: bool,
    pub status: HtlcStatusV1,
    pub created_at: u64,
    pub updated_at: u64,
    pub payee_pubkey: PublicKey,
}

pub enum HtlcStatusV1 {
    Pending,
    Succeeded,
    Failed,
}

pub struct SwapV1 {
    pub qty_from: u64,
    pub qty_to: u64,
    pub from_asset: Option<ContractId>,
    pub to_asset: Option<ContractId>,
    pub payment_hash: PaymentHash,
    pub status: SwapStatusV1,
    pub requested_at: u64,
    pub initiated_at: Option<u64>,
    pub expires_at: u64,
    pub completed_at: Option<u64>,
}

pub enum SwapStatusV1 {
    Waiting,
    Pending,
    Succeeded,
    Expired,
    Failed,
}

pub struct LnInvoiceRequestV1 {
    pub amt_msat: Option<u64>,
    pub expiry_sec: u32,
    pub asset_id: Option<ContractId>,
    pub asset_amount: Option<u64>,
}

pub struct LnInvoiceResponseV1 {
    pub invoice: Bolt11Invoice,
}

pub struct SendRgbRequestV1 {
    pub donation: bool,
    pub fee_rate: u64,
    pub min_confirmations: u8,
    pub skip_sync: bool,
    pub recipient_groups: Vec<AssetRecipientsV1>,
}

pub struct SendRgbResponseV1 {
    pub txid: Txid,
    pub batch_transfer_idx: i32,
}

pub struct AssetRecipientsV1 {
    pub asset_id: ContractId,
    pub recipients: Vec<RgbRecipientV1>,
}

pub struct RgbRecipientV1 {
    pub recipient_id: RecipientId,
    pub witness_data: Option<WitnessDataV1>,
    pub assignment_kind: AssignmentKindV1,
    pub assignment_amount: Option<u64>,
    pub transport_endpoints: Vec<TransportEndpoint>,
}

pub struct WitnessDataV1 {
    pub amount_sat: u64,
    pub blinding: Option<u64>,
}

pub enum AssignmentKindV1 {
    Fungible,
    NonFungible,
    InflationRight,
    ReplaceRight,
    Any,
}

pub(super) fn uniffi_state_slot() -> &'static std::sync::Mutex<Option<Arc<AppState>>> {
    static SLOT: std::sync::OnceLock<std::sync::Mutex<Option<Arc<AppState>>>> =
        std::sync::OnceLock::new();
    SLOT.get_or_init(|| std::sync::Mutex::new(None))
}
