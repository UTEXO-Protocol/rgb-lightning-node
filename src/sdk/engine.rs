use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use crate::sdk;
use crate::error::APIError;
use crate::utils::AppState;

use super::{
    AddressData, AssetBalanceData, AssetMetadataData, BtcBalanceData, ChannelData,
    ChannelIdData, CloseChannelRequestData, CreateUtxosRequestData, DecodeLnInvoiceData,
    DecodeRgbInvoiceData, DisconnectPeerRequestData, EstimateFeeData, InitData, InvoiceStatusData,
    IssueAssetCfaRequestData, IssueAssetNiaRequestData, IssueAssetUdaRequestData, KeysendData,
    KeysendRequestData, ListAssetsData, LnInvoiceData, MakerExecuteRequestData, MakerInitData,
    MakerInitRequestData, NetworkInfoData, NodeInfoData, OpenChannelData, OpenChannelRequestData,
    PaymentData, PeerData, PostAssetMediaData, RefreshTransfersRequestData, RgbInvoiceData,
    RgbInvoiceRequestData, SendBtcData, SendBtcRequestData, SendOnionMessageRequestData,
    SendPaymentData, SendPaymentRequestData, SendRgbData, SendRgbRequestData, SignMessageData,
    SwapListData, SwapViewData, TakerRequestData, TransactionData, TransferData, UnlockRequest,
    UnspentData, AssetMediaData, CheckIndexerUrlData, FailTransfersData, FailTransfersRequestData,
    AssetCFA, AssetNIA, AssetUDA,
};

pub(crate) trait SdkEngine: Send + Sync + 'static {
    fn init(
        &self,
        state: Arc<AppState>,
        password: String,
        mnemonic: Option<String>,
    ) -> Pin<Box<dyn Future<Output = Result<InitData, APIError>> + Send>>;

    fn unlock(
        &self,
        state: Arc<AppState>,
        request: UnlockRequest,
    ) -> Pin<Box<dyn Future<Output = Result<(), APIError>> + Send>>;

    fn node_info(
        &self,
        state: Arc<AppState>,
    ) -> Pin<Box<dyn Future<Output = Result<NodeInfoData, APIError>> + Send>>;

    fn network_info(
        &self,
        state: Arc<AppState>,
    ) -> Pin<Box<dyn Future<Output = Result<NetworkInfoData, APIError>> + Send>>;

    fn address(
        &self,
        state: Arc<AppState>,
    ) -> Pin<Box<dyn Future<Output = Result<AddressData, APIError>> + Send>>;

    fn btc_balance(
        &self,
        state: Arc<AppState>,
        skip_sync: bool,
    ) -> Pin<Box<dyn Future<Output = Result<BtcBalanceData, APIError>> + Send>>;

    fn list_channels(
        &self,
        state: Arc<AppState>,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<ChannelData>, APIError>> + Send>>;

    fn list_peers(
        &self,
        state: Arc<AppState>,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<PeerData>, APIError>> + Send>>;

    fn list_transactions(
        &self,
        state: Arc<AppState>,
        skip_sync: bool,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<TransactionData>, APIError>> + Send>>;

    fn sign_message(
        &self,
        state: Arc<AppState>,
        message: String,
    ) -> Pin<Box<dyn Future<Output = Result<SignMessageData, APIError>> + Send>>;

    fn estimate_fee(
        &self,
        state: Arc<AppState>,
        blocks: u16,
    ) -> Pin<Box<dyn Future<Output = Result<EstimateFeeData, APIError>> + Send>>;

    fn get_payment(
        &self,
        state: Arc<AppState>,
        payment_hash_hex: String,
    ) -> Pin<Box<dyn Future<Output = Result<PaymentData, APIError>> + Send>>;

    fn get_swap(
        &self,
        state: Arc<AppState>,
        payment_hash_hex: String,
        taker: bool,
    ) -> Pin<Box<dyn Future<Output = Result<SwapViewData, APIError>> + Send>>;

    fn list_swaps(
        &self,
        state: Arc<AppState>,
    ) -> Pin<Box<dyn Future<Output = Result<SwapListData, APIError>> + Send>>;

    fn list_payments(
        &self,
        state: Arc<AppState>,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<PaymentData>, APIError>> + Send>>;

    fn list_transfers(
        &self,
        state: Arc<AppState>,
        asset_id: String,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<TransferData>, APIError>> + Send>>;

    fn list_unspents(
        &self,
        state: Arc<AppState>,
        skip_sync: bool,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<UnspentData>, APIError>> + Send>>;

    fn asset_balance(
        &self,
        state: Arc<AppState>,
        asset_id: String,
    ) -> Pin<Box<dyn Future<Output = Result<AssetBalanceData, APIError>> + Send>>;

    fn asset_metadata(
        &self,
        state: Arc<AppState>,
        asset_id: String,
    ) -> Pin<Box<dyn Future<Output = Result<AssetMetadataData, APIError>> + Send>>;

    fn list_assets(
        &self,
        state: Arc<AppState>,
        filter_asset_schemas: Vec<rgb_lib::AssetSchema>,
    ) -> Pin<Box<dyn Future<Output = Result<ListAssetsData, APIError>> + Send>>;

    fn decode_ln_invoice(
        &self,
        state: Arc<AppState>,
        invoice: String,
    ) -> Pin<Box<dyn Future<Output = Result<DecodeLnInvoiceData, APIError>> + Send>>;

    fn decode_rgb_invoice(
        &self,
        state: Arc<AppState>,
        invoice: String,
    ) -> Pin<Box<dyn Future<Output = Result<DecodeRgbInvoiceData, APIError>> + Send>>;

    fn invoice_status(
        &self,
        state: Arc<AppState>,
        invoice: String,
    ) -> Pin<Box<dyn Future<Output = Result<InvoiceStatusData, APIError>> + Send>>;

    fn get_channel_id(
        &self,
        state: Arc<AppState>,
        temporary_channel_id: String,
    ) -> Pin<Box<dyn Future<Output = Result<ChannelIdData, APIError>> + Send>>;

    fn connect_peer(
        &self,
        state: Arc<AppState>,
        peer_pubkey_and_addr: String,
    ) -> Pin<Box<dyn Future<Output = Result<(), APIError>> + Send>>;

    fn disconnect_peer(
        &self,
        state: Arc<AppState>,
        request: DisconnectPeerRequestData,
    ) -> Pin<Box<dyn Future<Output = Result<(), APIError>> + Send>>;

    fn close_channel(
        &self,
        state: Arc<AppState>,
        request: CloseChannelRequestData,
    ) -> Pin<Box<dyn Future<Output = Result<(), APIError>> + Send>>;

    fn create_utxos(
        &self,
        state: Arc<AppState>,
        request: CreateUtxosRequestData,
    ) -> Pin<Box<dyn Future<Output = Result<(), APIError>> + Send>>;

    fn sync(
        &self,
        state: Arc<AppState>,
    ) -> Pin<Box<dyn Future<Output = Result<(), APIError>> + Send>>;

    fn check_indexer_url(
        &self,
        state: Arc<AppState>,
        indexer_url: String,
    ) -> Pin<Box<dyn Future<Output = Result<CheckIndexerUrlData, APIError>> + Send>>;

    fn check_proxy_endpoint(
        &self,
        proxy_endpoint: String,
    ) -> Pin<Box<dyn Future<Output = Result<(), APIError>> + Send>>;

    fn get_asset_media(
        &self,
        state: Arc<AppState>,
        digest: String,
    ) -> Pin<Box<dyn Future<Output = Result<AssetMediaData, APIError>> + Send>>;

    fn create_ln_invoice(
        &self,
        state: Arc<AppState>,
        amt_msat: Option<u64>,
        expiry_sec: u32,
        asset_id: Option<String>,
        asset_amount: Option<u64>,
    ) -> Pin<Box<dyn Future<Output = Result<LnInvoiceData, APIError>> + Send>>;

    fn issue_asset_nia(
        &self,
        state: Arc<AppState>,
        request: IssueAssetNiaRequestData,
    ) -> Pin<Box<dyn Future<Output = Result<AssetNIA, APIError>> + Send>>;

    fn issue_asset_cfa(
        &self,
        state: Arc<AppState>,
        request: IssueAssetCfaRequestData,
    ) -> Pin<Box<dyn Future<Output = Result<AssetCFA, APIError>> + Send>>;

    fn issue_asset_uda(
        &self,
        state: Arc<AppState>,
        request: IssueAssetUdaRequestData,
    ) -> Pin<Box<dyn Future<Output = Result<AssetUDA, APIError>> + Send>>;

    fn post_asset_media(
        &self,
        state: Arc<AppState>,
        file_bytes: Vec<u8>,
    ) -> Pin<Box<dyn Future<Output = Result<PostAssetMediaData, APIError>> + Send>>;

    fn rgb_invoice(
        &self,
        state: Arc<AppState>,
        request: RgbInvoiceRequestData,
    ) -> Pin<Box<dyn Future<Output = Result<RgbInvoiceData, APIError>> + Send>>;

    fn send_rgb_from_groups(
        &self,
        state: Arc<AppState>,
        request: SendRgbRequestData,
    ) -> Pin<Box<dyn Future<Output = Result<SendRgbData, APIError>> + Send>>;

    fn keysend(
        &self,
        state: Arc<AppState>,
        request: KeysendRequestData,
    ) -> Pin<Box<dyn Future<Output = Result<KeysendData, APIError>> + Send>>;

    fn send_btc(
        &self,
        state: Arc<AppState>,
        request: SendBtcRequestData,
    ) -> Pin<Box<dyn Future<Output = Result<SendBtcData, APIError>> + Send>>;

    fn maker_init(
        &self,
        state: Arc<AppState>,
        request: MakerInitRequestData,
    ) -> Pin<Box<dyn Future<Output = Result<MakerInitData, APIError>> + Send>>;

    fn maker_execute(
        &self,
        state: Arc<AppState>,
        request: MakerExecuteRequestData,
    ) -> Pin<Box<dyn Future<Output = Result<(), APIError>> + Send>>;

    fn taker(
        &self,
        state: Arc<AppState>,
        request: TakerRequestData,
    ) -> Pin<Box<dyn Future<Output = Result<(), APIError>> + Send>>;

    fn send_onion_message(
        &self,
        state: Arc<AppState>,
        request: SendOnionMessageRequestData,
    ) -> Pin<Box<dyn Future<Output = Result<(), APIError>> + Send>>;

    fn open_channel(
        &self,
        state: Arc<AppState>,
        request: OpenChannelRequestData,
    ) -> Pin<Box<dyn Future<Output = Result<OpenChannelData, APIError>> + Send>>;

    fn send_payment(
        &self,
        state: Arc<AppState>,
        request: SendPaymentRequestData,
    ) -> Pin<Box<dyn Future<Output = Result<SendPaymentData, APIError>> + Send>>;

    fn refresh_transfers(
        &self,
        state: Arc<AppState>,
        request: RefreshTransfersRequestData,
    ) -> Pin<Box<dyn Future<Output = Result<(), APIError>> + Send>>;

    fn fail_transfers(
        &self,
        state: Arc<AppState>,
        request: FailTransfersRequestData,
    ) -> Pin<Box<dyn Future<Output = Result<FailTransfersData, APIError>> + Send>>;
}

pub(crate) struct NativeRootEngine;

// Feature-gated hook for the upcoming SDK-local runtime implementation.
// For now it aliases to root behavior until SDK-internal wiring is complete.
#[cfg(feature = "native-sdk-engine")]
pub(crate) type NativeSdkEngine = NativeRootEngine;

impl SdkEngine for NativeRootEngine {
    fn init(
        &self,
        state: Arc<AppState>,
        password: String,
        mnemonic: Option<String>,
    ) -> Pin<Box<dyn Future<Output = Result<InitData, APIError>> + Send>> {
        Box::pin(sdk::init(state, password, mnemonic))
    }

    fn unlock(
        &self,
        state: Arc<AppState>,
        request: UnlockRequest,
    ) -> Pin<Box<dyn Future<Output = Result<(), APIError>> + Send>> {
        Box::pin(sdk::unlock(state, request))
    }

    fn node_info(
        &self,
        state: Arc<AppState>,
    ) -> Pin<Box<dyn Future<Output = Result<NodeInfoData, APIError>> + Send>> {
        Box::pin(sdk::node_info(state))
    }

    fn network_info(
        &self,
        state: Arc<AppState>,
    ) -> Pin<Box<dyn Future<Output = Result<NetworkInfoData, APIError>> + Send>> {
        Box::pin(sdk::network_info(state))
    }

    fn address(
        &self,
        state: Arc<AppState>,
    ) -> Pin<Box<dyn Future<Output = Result<AddressData, APIError>> + Send>> {
        Box::pin(sdk::address(state))
    }

    fn btc_balance(
        &self,
        state: Arc<AppState>,
        skip_sync: bool,
    ) -> Pin<Box<dyn Future<Output = Result<BtcBalanceData, APIError>> + Send>> {
        Box::pin(sdk::btc_balance(state, skip_sync))
    }

    fn list_channels(
        &self,
        state: Arc<AppState>,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<ChannelData>, APIError>> + Send>> {
        Box::pin(sdk::list_channels(state))
    }

    fn list_peers(
        &self,
        state: Arc<AppState>,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<PeerData>, APIError>> + Send>> {
        Box::pin(sdk::list_peers(state))
    }

    fn list_transactions(
        &self,
        state: Arc<AppState>,
        skip_sync: bool,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<TransactionData>, APIError>> + Send>> {
        Box::pin(sdk::list_transactions(state, skip_sync))
    }

    fn sign_message(
        &self,
        state: Arc<AppState>,
        message: String,
    ) -> Pin<Box<dyn Future<Output = Result<SignMessageData, APIError>> + Send>> {
        Box::pin(sdk::sign_message(state, message))
    }

    fn estimate_fee(
        &self,
        state: Arc<AppState>,
        blocks: u16,
    ) -> Pin<Box<dyn Future<Output = Result<EstimateFeeData, APIError>> + Send>> {
        Box::pin(sdk::estimate_fee(state, blocks))
    }

    fn get_payment(
        &self,
        state: Arc<AppState>,
        payment_hash_hex: String,
    ) -> Pin<Box<dyn Future<Output = Result<PaymentData, APIError>> + Send>> {
        Box::pin(sdk::get_payment(state, payment_hash_hex))
    }

    fn get_swap(
        &self,
        state: Arc<AppState>,
        payment_hash_hex: String,
        taker: bool,
    ) -> Pin<Box<dyn Future<Output = Result<SwapViewData, APIError>> + Send>> {
        Box::pin(sdk::get_swap(state, payment_hash_hex, taker))
    }

    fn list_swaps(
        &self,
        state: Arc<AppState>,
    ) -> Pin<Box<dyn Future<Output = Result<SwapListData, APIError>> + Send>> {
        Box::pin(sdk::list_swaps(state))
    }

    fn list_payments(
        &self,
        state: Arc<AppState>,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<PaymentData>, APIError>> + Send>> {
        Box::pin(sdk::list_payments(state))
    }

    fn list_transfers(
        &self,
        state: Arc<AppState>,
        asset_id: String,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<TransferData>, APIError>> + Send>> {
        Box::pin(sdk::list_transfers(state, asset_id))
    }

    fn list_unspents(
        &self,
        state: Arc<AppState>,
        skip_sync: bool,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<UnspentData>, APIError>> + Send>> {
        Box::pin(sdk::list_unspents(state, skip_sync))
    }

    fn asset_balance(
        &self,
        state: Arc<AppState>,
        asset_id: String,
    ) -> Pin<Box<dyn Future<Output = Result<AssetBalanceData, APIError>> + Send>> {
        Box::pin(sdk::asset_balance(state, asset_id))
    }

    fn asset_metadata(
        &self,
        state: Arc<AppState>,
        asset_id: String,
    ) -> Pin<Box<dyn Future<Output = Result<AssetMetadataData, APIError>> + Send>> {
        Box::pin(sdk::asset_metadata(state, asset_id))
    }

    fn list_assets(
        &self,
        state: Arc<AppState>,
        filter_asset_schemas: Vec<rgb_lib::AssetSchema>,
    ) -> Pin<Box<dyn Future<Output = Result<ListAssetsData, APIError>> + Send>> {
        Box::pin(sdk::list_assets(state, filter_asset_schemas))
    }

    fn decode_ln_invoice(
        &self,
        state: Arc<AppState>,
        invoice: String,
    ) -> Pin<Box<dyn Future<Output = Result<DecodeLnInvoiceData, APIError>> + Send>> {
        Box::pin(sdk::decode_ln_invoice(state, invoice))
    }

    fn decode_rgb_invoice(
        &self,
        state: Arc<AppState>,
        invoice: String,
    ) -> Pin<Box<dyn Future<Output = Result<DecodeRgbInvoiceData, APIError>> + Send>> {
        Box::pin(sdk::decode_rgb_invoice(state, invoice))
    }

    fn invoice_status(
        &self,
        state: Arc<AppState>,
        invoice: String,
    ) -> Pin<Box<dyn Future<Output = Result<InvoiceStatusData, APIError>> + Send>> {
        Box::pin(sdk::invoice_status(state, invoice))
    }

    fn get_channel_id(
        &self,
        state: Arc<AppState>,
        temporary_channel_id: String,
    ) -> Pin<Box<dyn Future<Output = Result<ChannelIdData, APIError>> + Send>> {
        Box::pin(sdk::get_channel_id(state, temporary_channel_id))
    }

    fn connect_peer(
        &self,
        state: Arc<AppState>,
        peer_pubkey_and_addr: String,
    ) -> Pin<Box<dyn Future<Output = Result<(), APIError>> + Send>> {
        Box::pin(sdk::connect_peer(state, peer_pubkey_and_addr))
    }

    fn disconnect_peer(
        &self,
        state: Arc<AppState>,
        request: DisconnectPeerRequestData,
    ) -> Pin<Box<dyn Future<Output = Result<(), APIError>> + Send>> {
        Box::pin(sdk::disconnect_peer(state, request))
    }

    fn close_channel(
        &self,
        state: Arc<AppState>,
        request: CloseChannelRequestData,
    ) -> Pin<Box<dyn Future<Output = Result<(), APIError>> + Send>> {
        Box::pin(sdk::close_channel(state, request))
    }

    fn create_utxos(
        &self,
        state: Arc<AppState>,
        request: CreateUtxosRequestData,
    ) -> Pin<Box<dyn Future<Output = Result<(), APIError>> + Send>> {
        Box::pin(sdk::create_utxos(state, request))
    }

    fn sync(
        &self,
        state: Arc<AppState>,
    ) -> Pin<Box<dyn Future<Output = Result<(), APIError>> + Send>> {
        Box::pin(sdk::sync(state))
    }

    fn check_indexer_url(
        &self,
        state: Arc<AppState>,
        indexer_url: String,
    ) -> Pin<Box<dyn Future<Output = Result<CheckIndexerUrlData, APIError>> + Send>> {
        Box::pin(sdk::check_indexer_url(state, indexer_url))
    }

    fn check_proxy_endpoint(
        &self,
        proxy_endpoint: String,
    ) -> Pin<Box<dyn Future<Output = Result<(), APIError>> + Send>> {
        Box::pin(sdk::check_proxy_endpoint(proxy_endpoint))
    }

    fn get_asset_media(
        &self,
        state: Arc<AppState>,
        digest: String,
    ) -> Pin<Box<dyn Future<Output = Result<AssetMediaData, APIError>> + Send>> {
        Box::pin(sdk::get_asset_media(state, digest))
    }

    fn create_ln_invoice(
        &self,
        state: Arc<AppState>,
        amt_msat: Option<u64>,
        expiry_sec: u32,
        asset_id: Option<String>,
        asset_amount: Option<u64>,
    ) -> Pin<Box<dyn Future<Output = Result<LnInvoiceData, APIError>> + Send>> {
        Box::pin(sdk::create_ln_invoice(
            state,
            amt_msat,
            expiry_sec,
            asset_id,
            asset_amount,
        ))
    }

    fn issue_asset_nia(
        &self,
        state: Arc<AppState>,
        request: IssueAssetNiaRequestData,
    ) -> Pin<Box<dyn Future<Output = Result<AssetNIA, APIError>> + Send>> {
        Box::pin(sdk::issue_asset_nia(state, request))
    }

    fn issue_asset_cfa(
        &self,
        state: Arc<AppState>,
        request: IssueAssetCfaRequestData,
    ) -> Pin<Box<dyn Future<Output = Result<AssetCFA, APIError>> + Send>> {
        Box::pin(sdk::issue_asset_cfa(state, request))
    }

    fn issue_asset_uda(
        &self,
        state: Arc<AppState>,
        request: IssueAssetUdaRequestData,
    ) -> Pin<Box<dyn Future<Output = Result<AssetUDA, APIError>> + Send>> {
        Box::pin(sdk::issue_asset_uda(state, request))
    }

    fn post_asset_media(
        &self,
        state: Arc<AppState>,
        file_bytes: Vec<u8>,
    ) -> Pin<Box<dyn Future<Output = Result<PostAssetMediaData, APIError>> + Send>> {
        Box::pin(sdk::post_asset_media(state, file_bytes))
    }

    fn rgb_invoice(
        &self,
        state: Arc<AppState>,
        request: RgbInvoiceRequestData,
    ) -> Pin<Box<dyn Future<Output = Result<RgbInvoiceData, APIError>> + Send>> {
        Box::pin(sdk::rgb_invoice(state, request))
    }

    fn send_rgb_from_groups(
        &self,
        state: Arc<AppState>,
        request: SendRgbRequestData,
    ) -> Pin<Box<dyn Future<Output = Result<SendRgbData, APIError>> + Send>> {
        Box::pin(sdk::send_rgb_from_groups(state, request))
    }

    fn keysend(
        &self,
        state: Arc<AppState>,
        request: KeysendRequestData,
    ) -> Pin<Box<dyn Future<Output = Result<KeysendData, APIError>> + Send>> {
        Box::pin(sdk::keysend(state, request))
    }

    fn send_btc(
        &self,
        state: Arc<AppState>,
        request: SendBtcRequestData,
    ) -> Pin<Box<dyn Future<Output = Result<SendBtcData, APIError>> + Send>> {
        Box::pin(sdk::send_btc(state, request))
    }

    fn maker_init(
        &self,
        state: Arc<AppState>,
        request: MakerInitRequestData,
    ) -> Pin<Box<dyn Future<Output = Result<MakerInitData, APIError>> + Send>> {
        Box::pin(sdk::maker_init(state, request))
    }

    fn maker_execute(
        &self,
        state: Arc<AppState>,
        request: MakerExecuteRequestData,
    ) -> Pin<Box<dyn Future<Output = Result<(), APIError>> + Send>> {
        Box::pin(sdk::maker_execute(state, request))
    }

    fn taker(
        &self,
        state: Arc<AppState>,
        request: TakerRequestData,
    ) -> Pin<Box<dyn Future<Output = Result<(), APIError>> + Send>> {
        Box::pin(sdk::taker(state, request))
    }

    fn send_onion_message(
        &self,
        state: Arc<AppState>,
        request: SendOnionMessageRequestData,
    ) -> Pin<Box<dyn Future<Output = Result<(), APIError>> + Send>> {
        Box::pin(sdk::send_onion_message(state, request))
    }

    fn open_channel(
        &self,
        state: Arc<AppState>,
        request: OpenChannelRequestData,
    ) -> Pin<Box<dyn Future<Output = Result<OpenChannelData, APIError>> + Send>> {
        Box::pin(sdk::open_channel(state, request))
    }

    fn send_payment(
        &self,
        state: Arc<AppState>,
        request: SendPaymentRequestData,
    ) -> Pin<Box<dyn Future<Output = Result<SendPaymentData, APIError>> + Send>> {
        Box::pin(sdk::send_payment(state, request))
    }

    fn refresh_transfers(
        &self,
        state: Arc<AppState>,
        request: RefreshTransfersRequestData,
    ) -> Pin<Box<dyn Future<Output = Result<(), APIError>> + Send>> {
        Box::pin(sdk::refresh_transfers(state, request))
    }

    fn fail_transfers(
        &self,
        state: Arc<AppState>,
        request: FailTransfersRequestData,
    ) -> Pin<Box<dyn Future<Output = Result<FailTransfersData, APIError>> + Send>> {
        Box::pin(sdk::fail_transfers(state, request))
    }
}

pub(crate) fn default_engine() -> &'static dyn SdkEngine {
    #[cfg(feature = "native-sdk-engine")]
    {
        static ENGINE: NativeSdkEngine = NativeRootEngine;
        return &ENGINE;
    }

    #[cfg(not(feature = "native-sdk-engine"))]
    {
        static ENGINE: NativeRootEngine = NativeRootEngine;
        &ENGINE
    }
}
