//! wasm-bindgen bridge for the public wasm SDK.
//!
//! Minimal JS usage:
//! ```javascript
//! import init, { sdk_init } from "./pkg/rgb_lightning_node.js";
//!
//! await init();
//! const state = sdk_init("demo-app", "testnet4", 1200, 1.5);
//! const network = JSON.parse(state.networkInfoJson());
//! const channels = JSON.parse(state.listChannelsJson());
//! const peers = JSON.parse(state.listPeersJson());
//! const node = JSON.parse(state.nodeInfoJson());
//! const caps = JSON.parse(state.runtimeCapabilitiesJson());
//! const channelId = JSON.parse(state.getChannelIdJson("wasm-tmp:demo-app"));
//! ```

use futures::executor::block_on;
use serde::Serialize;
use serde_wasm_bindgen::to_value as to_js_value;
use wasm_bindgen::prelude::*;

use crate::sdk::{
    self, BlockTime, ChannelData, ChannelIdData, ChannelStatus, EstimateFeeData, NetworkInfoData,
    NodeInfoData, PeerData, TransactionData, TransactionType, WasmNetwork, WasmRgbKeysData,
    WasmRuntimeCapabilitiesData, WasmSdkState, WasmSdkStateConfig,
};

fn js_err<E: std::fmt::Display>(err: E) -> JsValue {
    JsValue::from_str(&err.to_string())
}

fn js_obj<T: Serialize>(value: &T) -> Result<JsValue, JsValue> {
    to_js_value(value).map_err(js_err)
}

fn parse_network(value: &str) -> WasmNetwork {
    match value.to_ascii_lowercase().as_str() {
        "mainnet" => WasmNetwork::Mainnet,
        "testnet" => WasmNetwork::Testnet,
        "testnet4" => WasmNetwork::Testnet4,
        "signet" => WasmNetwork::Signet,
        "regtest" => WasmNetwork::Regtest,
        custom => WasmNetwork::Custom(custom.to_string()),
    }
}

#[derive(Serialize)]
struct JsNetworkInfoData {
    network: String,
    height: u32,
}

impl From<NetworkInfoData> for JsNetworkInfoData {
    fn from(value: NetworkInfoData) -> Self {
        Self {
            network: value.network,
            height: value.height,
        }
    }
}

#[derive(Serialize)]
struct JsEstimateFeeData {
    fee_rate: f64,
}

impl From<EstimateFeeData> for JsEstimateFeeData {
    fn from(value: EstimateFeeData) -> Self {
        Self {
            fee_rate: value.fee_rate,
        }
    }
}

#[derive(Serialize)]
struct JsRuntimeCapabilitiesData {
    read_only: bool,
    ldk_runtime: bool,
    rgb_runtime: bool,
    uniffi_bridge: bool,
}

impl From<WasmRuntimeCapabilitiesData> for JsRuntimeCapabilitiesData {
    fn from(value: WasmRuntimeCapabilitiesData) -> Self {
        Self {
            read_only: value.read_only,
            ldk_runtime: value.ldk_runtime,
            rgb_runtime: value.rgb_runtime,
            uniffi_bridge: value.uniffi_bridge,
        }
    }
}

#[derive(Serialize)]
struct JsRgbKeysData {
    mnemonic: String,
    xpub: String,
    account_xpub_vanilla: String,
    account_xpub_colored: String,
    master_fingerprint: String,
}

impl From<WasmRgbKeysData> for JsRgbKeysData {
    fn from(value: WasmRgbKeysData) -> Self {
        Self {
            mnemonic: value.mnemonic,
            xpub: value.xpub,
            account_xpub_vanilla: value.account_xpub_vanilla,
            account_xpub_colored: value.account_xpub_colored,
            master_fingerprint: value.master_fingerprint,
        }
    }
}

#[derive(Serialize)]
struct JsChannelIdData {
    channel_id: String,
}

impl From<ChannelIdData> for JsChannelIdData {
    fn from(value: ChannelIdData) -> Self {
        Self {
            channel_id: value.channel_id,
        }
    }
}

#[derive(Serialize)]
struct JsChannelData {
    channel_id: String,
    funding_txid: Option<String>,
    peer_pubkey: String,
    peer_alias: Option<String>,
    short_channel_id: Option<u64>,
    status: String,
    ready: bool,
    capacity_sat: u64,
    local_balance_sat: u64,
    outbound_balance_msat: u64,
    inbound_balance_msat: u64,
    next_outbound_htlc_limit_msat: u64,
    next_outbound_htlc_minimum_msat: u64,
    is_usable: bool,
    public: bool,
    asset_id: Option<String>,
    asset_local_amount: Option<u64>,
    asset_remote_amount: Option<u64>,
    virtual_open_mode: Option<String>,
}

impl From<ChannelData> for JsChannelData {
    fn from(value: ChannelData) -> Self {
        let status = match value.status {
            ChannelStatus::Opening => "opening",
            ChannelStatus::Opened => "opened",
            ChannelStatus::Closing => "closing",
        }
        .to_string();

        Self {
            channel_id: value.channel_id,
            funding_txid: value.funding_txid,
            peer_pubkey: value.peer_pubkey,
            peer_alias: value.peer_alias,
            short_channel_id: value.short_channel_id,
            status,
            ready: value.ready,
            capacity_sat: value.capacity_sat,
            local_balance_sat: value.local_balance_sat,
            outbound_balance_msat: value.outbound_balance_msat,
            inbound_balance_msat: value.inbound_balance_msat,
            next_outbound_htlc_limit_msat: value.next_outbound_htlc_limit_msat,
            next_outbound_htlc_minimum_msat: value.next_outbound_htlc_minimum_msat,
            is_usable: value.is_usable,
            public: value.public,
            asset_id: value.asset_id,
            asset_local_amount: value.asset_local_amount,
            asset_remote_amount: value.asset_remote_amount,
            virtual_open_mode: value.virtual_open_mode,
        }
    }
}

#[derive(Serialize)]
struct JsPeerData {
    pubkey: String,
}

impl From<PeerData> for JsPeerData {
    fn from(value: PeerData) -> Self {
        Self {
            pubkey: value.pubkey,
        }
    }
}

#[derive(Serialize)]
struct JsNodeInfoData {
    pubkey: String,
    num_channels: usize,
    num_usable_channels: usize,
    local_balance_sat: u64,
    eventual_close_fees_sat: u64,
    pending_outbound_payments_sat: u64,
    num_peers: usize,
    account_xpub_vanilla: String,
    account_xpub_colored: String,
    max_media_upload_size_mb: u16,
    rgb_htlc_min_msat: u64,
    rgb_channel_capacity_min_sat: u64,
    channel_capacity_min_sat: u64,
    channel_capacity_max_sat: u64,
    channel_asset_min_amount: u64,
    channel_asset_max_amount: u64,
    network_nodes: usize,
    network_channels: usize,
}

impl From<NodeInfoData> for JsNodeInfoData {
    fn from(value: NodeInfoData) -> Self {
        Self {
            pubkey: value.pubkey,
            num_channels: value.num_channels,
            num_usable_channels: value.num_usable_channels,
            local_balance_sat: value.local_balance_sat,
            eventual_close_fees_sat: value.eventual_close_fees_sat,
            pending_outbound_payments_sat: value.pending_outbound_payments_sat,
            num_peers: value.num_peers,
            account_xpub_vanilla: value.account_xpub_vanilla,
            account_xpub_colored: value.account_xpub_colored,
            max_media_upload_size_mb: value.max_media_upload_size_mb,
            rgb_htlc_min_msat: value.rgb_htlc_min_msat,
            rgb_channel_capacity_min_sat: value.rgb_channel_capacity_min_sat,
            channel_capacity_min_sat: value.channel_capacity_min_sat,
            channel_capacity_max_sat: value.channel_capacity_max_sat,
            channel_asset_min_amount: value.channel_asset_min_amount,
            channel_asset_max_amount: value.channel_asset_max_amount,
            network_nodes: value.network_nodes,
            network_channels: value.network_channels,
        }
    }
}

#[derive(Serialize)]
struct JsBlockTime {
    height: u32,
    timestamp: u64,
}

impl From<BlockTime> for JsBlockTime {
    fn from(value: BlockTime) -> Self {
        Self {
            height: value.height,
            timestamp: value.timestamp,
        }
    }
}

#[derive(Serialize)]
struct JsTransactionData {
    transaction_type: String,
    txid: String,
    received: u64,
    sent: u64,
    fee: u64,
    confirmation_time: Option<JsBlockTime>,
}

impl From<TransactionData> for JsTransactionData {
    fn from(value: TransactionData) -> Self {
        let transaction_type = match value.transaction_type {
            TransactionType::RgbSend => "rgb_send",
            TransactionType::Drain => "drain",
            TransactionType::CreateUtxos => "create_utxos",
            TransactionType::User => "user",
        }
        .to_string();

        Self {
            transaction_type,
            txid: value.txid,
            received: value.received,
            sent: value.sent,
            fee: value.fee,
            confirmation_time: value.confirmation_time.map(Into::into),
        }
    }
}

#[wasm_bindgen]
pub struct JsSdkState {
    inner: WasmSdkState,
}

#[wasm_bindgen]
pub fn sdk_init(
    app_id: String,
    network: Option<String>,
    height: Option<u32>,
    fee_rate_hint: Option<f64>,
) -> Result<JsSdkState, JsValue> {
    let mut config = WasmSdkStateConfig::new(app_id);
    if let Some(network_str) = network {
        config.network = parse_network(&network_str);
    }
    if let Some(height_value) = height {
        config.height = height_value;
    }
    if let Some(fee_rate_hint_value) = fee_rate_hint {
        config.fee_rate_hint = fee_rate_hint_value;
    }

    let state = block_on(sdk::init(config)).map_err(js_err)?;
    Ok(JsSdkState { inner: state })
}

#[wasm_bindgen(js_name = rgbGenerateKeysJson)]
pub fn rgb_generate_keys_json(network: String) -> Result<String, JsValue> {
    let parsed = parse_network(&network);
    let data = block_on(sdk::rgb_generate_keys(parsed)).map_err(js_err)?;
    serde_json::to_string(&JsRgbKeysData::from(data)).map_err(js_err)
}

#[wasm_bindgen(js_name = rgbGenerateKeysValue)]
pub fn rgb_generate_keys_value(network: String) -> Result<JsValue, JsValue> {
    let parsed = parse_network(&network);
    let data = block_on(sdk::rgb_generate_keys(parsed)).map_err(js_err)?;
    let mapped = JsRgbKeysData::from(data);
    js_obj(&mapped)
}

#[wasm_bindgen(js_name = rgbRestoreKeysJson)]
pub fn rgb_restore_keys_json(network: String, mnemonic: String) -> Result<String, JsValue> {
    let parsed = parse_network(&network);
    let data = block_on(sdk::rgb_restore_keys(parsed, mnemonic)).map_err(js_err)?;
    serde_json::to_string(&JsRgbKeysData::from(data)).map_err(js_err)
}

#[wasm_bindgen(js_name = rgbRestoreKeysValue)]
pub fn rgb_restore_keys_value(network: String, mnemonic: String) -> Result<JsValue, JsValue> {
    let parsed = parse_network(&network);
    let data = block_on(sdk::rgb_restore_keys(parsed, mnemonic)).map_err(js_err)?;
    let mapped = JsRgbKeysData::from(data);
    js_obj(&mapped)
}

#[wasm_bindgen]
impl JsSdkState {
    #[wasm_bindgen(js_name = networkInfoJson)]
    pub fn network_info_json(&self) -> Result<String, JsValue> {
        let data = block_on(sdk::network_info(self.inner.clone())).map_err(js_err)?;
        serde_json::to_string(&JsNetworkInfoData::from(data)).map_err(js_err)
    }

    #[wasm_bindgen(js_name = networkInfoValue)]
    pub fn network_info_value(&self) -> Result<JsValue, JsValue> {
        let data = block_on(sdk::network_info(self.inner.clone())).map_err(js_err)?;
        let mapped = JsNetworkInfoData::from(data);
        js_obj(&mapped)
    }

    #[wasm_bindgen(js_name = listChannelsJson)]
    pub fn list_channels_json(&self) -> Result<String, JsValue> {
        let data = block_on(sdk::list_channels(self.inner.clone())).map_err(js_err)?;
        let mapped: Vec<JsChannelData> = data.into_iter().map(Into::into).collect();
        serde_json::to_string(&mapped).map_err(js_err)
    }

    #[wasm_bindgen(js_name = listChannelsValue)]
    pub fn list_channels_value(&self) -> Result<JsValue, JsValue> {
        let data = block_on(sdk::list_channels(self.inner.clone())).map_err(js_err)?;
        let mapped: Vec<JsChannelData> = data.into_iter().map(Into::into).collect();
        js_obj(&mapped)
    }

    #[wasm_bindgen(js_name = listPeersJson)]
    pub fn list_peers_json(&self) -> Result<String, JsValue> {
        let data = block_on(sdk::list_peers(self.inner.clone())).map_err(js_err)?;
        let mapped: Vec<JsPeerData> = data.into_iter().map(Into::into).collect();
        serde_json::to_string(&mapped).map_err(js_err)
    }

    #[wasm_bindgen(js_name = listPeersValue)]
    pub fn list_peers_value(&self) -> Result<JsValue, JsValue> {
        let data = block_on(sdk::list_peers(self.inner.clone())).map_err(js_err)?;
        let mapped: Vec<JsPeerData> = data.into_iter().map(Into::into).collect();
        js_obj(&mapped)
    }

    #[wasm_bindgen(js_name = nodeInfoJson)]
    pub fn node_info_json(&self) -> Result<String, JsValue> {
        let data = block_on(sdk::node_info(self.inner.clone())).map_err(js_err)?;
        serde_json::to_string(&JsNodeInfoData::from(data)).map_err(js_err)
    }

    #[wasm_bindgen(js_name = nodeInfoValue)]
    pub fn node_info_value(&self) -> Result<JsValue, JsValue> {
        let data = block_on(sdk::node_info(self.inner.clone())).map_err(js_err)?;
        let mapped = JsNodeInfoData::from(data);
        js_obj(&mapped)
    }

    #[wasm_bindgen(js_name = estimateFeeJson)]
    pub fn estimate_fee_json(&self, blocks: u16) -> Result<String, JsValue> {
        let data = block_on(sdk::estimate_fee(self.inner.clone(), blocks)).map_err(js_err)?;
        serde_json::to_string(&JsEstimateFeeData::from(data)).map_err(js_err)
    }

    #[wasm_bindgen(js_name = estimateFeeValue)]
    pub fn estimate_fee_value(&self, blocks: u16) -> Result<JsValue, JsValue> {
        let data = block_on(sdk::estimate_fee(self.inner.clone(), blocks)).map_err(js_err)?;
        let mapped = JsEstimateFeeData::from(data);
        js_obj(&mapped)
    }

    #[wasm_bindgen(js_name = listTransactionsJson)]
    pub fn list_transactions_json(&self, skip_sync: bool) -> Result<String, JsValue> {
        let data =
            block_on(sdk::list_transactions(self.inner.clone(), skip_sync)).map_err(js_err)?;
        let mapped: Vec<JsTransactionData> = data.into_iter().map(Into::into).collect();
        serde_json::to_string(&mapped).map_err(js_err)
    }

    #[wasm_bindgen(js_name = listTransactionsValue)]
    pub fn list_transactions_value(&self, skip_sync: bool) -> Result<JsValue, JsValue> {
        let data =
            block_on(sdk::list_transactions(self.inner.clone(), skip_sync)).map_err(js_err)?;
        let mapped: Vec<JsTransactionData> = data.into_iter().map(Into::into).collect();
        js_obj(&mapped)
    }

    #[wasm_bindgen(js_name = runtimeCapabilitiesJson)]
    pub fn runtime_capabilities_json(&self) -> Result<String, JsValue> {
        let data = block_on(sdk::runtime_capabilities(self.inner.clone())).map_err(js_err)?;
        serde_json::to_string(&JsRuntimeCapabilitiesData::from(data)).map_err(js_err)
    }

    #[wasm_bindgen(js_name = runtimeCapabilitiesValue)]
    pub fn runtime_capabilities_value(&self) -> Result<JsValue, JsValue> {
        let data = block_on(sdk::runtime_capabilities(self.inner.clone())).map_err(js_err)?;
        let mapped = JsRuntimeCapabilitiesData::from(data);
        js_obj(&mapped)
    }

    #[wasm_bindgen(js_name = getChannelIdJson)]
    pub fn get_channel_id_json(&self, temporary_channel_id: String) -> Result<String, JsValue> {
        let data = block_on(sdk::get_channel_id(
            self.inner.clone(),
            temporary_channel_id,
        ))
        .map_err(js_err)?;
        serde_json::to_string(&JsChannelIdData::from(data)).map_err(js_err)
    }

    #[wasm_bindgen(js_name = getChannelIdValue)]
    pub fn get_channel_id_value(&self, temporary_channel_id: String) -> Result<JsValue, JsValue> {
        let data = block_on(sdk::get_channel_id(
            self.inner.clone(),
            temporary_channel_id,
        ))
        .map_err(js_err)?;
        let mapped = JsChannelIdData::from(data);
        js_obj(&mapped)
    }
}

#[cfg(all(test, target_arch = "wasm32"))]
mod tests {
    use super::*;

    #[test]
    fn sdk_init_and_network_info_json_smoke() {
        let state = sdk_init(
            "bind-app".to_string(),
            Some("testnet4".to_string()),
            Some(1200),
            Some(2.0),
        )
        .expect("sdk_init should succeed");

        let json = state
            .network_info_json()
            .expect("networkInfoJson should succeed");
        let value: serde_json::Value =
            serde_json::from_str(&json).expect("networkInfoJson should be valid JSON");

        assert_eq!(value["network"], "testnet4");
        assert_eq!(value["height"], 1200);
    }

    #[test]
    fn estimate_fee_json_smoke() {
        let state = sdk_init(
            "bind-fee".to_string(),
            Some("regtest".to_string()),
            Some(0),
            Some(8.0),
        )
        .expect("sdk_init should succeed");

        let json = state
            .estimate_fee_json(4)
            .expect("estimateFeeJson should succeed");
        let value: serde_json::Value =
            serde_json::from_str(&json).expect("estimateFeeJson should be valid JSON");

        assert_eq!(value["fee_rate"], serde_json::json!(2.0));
    }

    #[test]
    fn network_info_value_smoke() {
        let state = sdk_init(
            "bind-value".to_string(),
            Some("testnet".to_string()),
            Some(321),
            Some(1.0),
        )
        .expect("sdk_init should succeed");

        let value_js = state
            .network_info_value()
            .expect("networkInfoValue should succeed");
        let value: serde_json::Value =
            serde_wasm_bindgen::from_value(value_js).expect("networkInfoValue should be object");

        assert_eq!(value["network"], "testnet");
        assert_eq!(value["height"], 321);
    }

    #[test]
    fn list_channels_value_smoke() {
        let state = sdk_init(
            "bind-channels-value".to_string(),
            Some("regtest".to_string()),
            Some(1),
            Some(1.0),
        )
        .expect("sdk_init should succeed");

        let value_js = state
            .list_channels_value()
            .expect("listChannelsValue should succeed");
        let value: serde_json::Value =
            serde_wasm_bindgen::from_value(value_js).expect("listChannelsValue should be array");

        let arr = value.as_array().expect("channels payload must be array");
        assert!(!arr.is_empty(), "channels array should not be empty");
        assert_eq!(arr[0]["channel_id"], "wasm-chan:bind-channels-value");
    }

    #[test]
    fn runtime_capabilities_value_smoke() {
        let state = sdk_init(
            "bind-caps-value".to_string(),
            Some("regtest".to_string()),
            Some(0),
            Some(1.0),
        )
        .expect("sdk_init should succeed");

        let value_js = state
            .runtime_capabilities_value()
            .expect("runtimeCapabilitiesValue should succeed");
        let value: serde_json::Value =
            serde_wasm_bindgen::from_value(value_js).expect("runtimeCapabilitiesValue object");

        assert_eq!(value["read_only"], true);
        assert_eq!(value["ldk_runtime"], false);
        assert_eq!(value["rgb_runtime"], false);
        assert_eq!(value["uniffi_bridge"], false);
    }

    #[test]
    fn get_channel_id_json_smoke() {
        let state = sdk_init(
            "bind-channel".to_string(),
            Some("regtest".to_string()),
            Some(0),
            Some(1.0),
        )
        .expect("sdk_init should succeed");

        let json = state
            .get_channel_id_json("wasm-tmp:bind-channel".to_string())
            .expect("getChannelIdJson should succeed");
        let value: serde_json::Value =
            serde_json::from_str(&json).expect("getChannelIdJson should be valid JSON");
        assert_eq!(value["channel_id"], "wasm-chan:bind-channel");

        let err = state
            .get_channel_id_json("wrong-tmp-id".to_string())
            .expect_err("getChannelIdJson should fail for unknown tmp id");
        let err_s = err.as_string().unwrap_or_default();
        assert!(
            err_s.contains("Unknown temporary channel ID"),
            "unexpected error message: {err_s}"
        );
    }

    #[cfg(feature = "real-wasm-rgb")]
    #[test]
    fn rgb_generate_and_restore_keys_json_smoke() {
        let generated_json = rgb_generate_keys_json("regtest".to_string())
            .expect("rgbGenerateKeysJson should succeed");
        let generated: serde_json::Value =
            serde_json::from_str(&generated_json).expect("generated json should be valid");
        let mnemonic = generated["mnemonic"]
            .as_str()
            .expect("generated mnemonic should be string")
            .to_string();
        assert!(!mnemonic.is_empty());

        let restored_json = rgb_restore_keys_json("regtest".to_string(), mnemonic)
            .expect("rgbRestoreKeysJson should succeed");
        let restored: serde_json::Value =
            serde_json::from_str(&restored_json).expect("restored json should be valid");
        assert!(restored["xpub"].as_str().is_some());
        assert!(restored["master_fingerprint"].as_str().is_some());
    }
}
