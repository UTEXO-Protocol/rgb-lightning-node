//! WASM SDK boundary module.
//! This module is intentionally minimal and excludes native LDK/RGB runtime wiring.
//!
//! Example initialization flow:
//! ```rust,ignore
//! use futures::executor::block_on;
//! use rgb_lightning_node::sdk::{
//!     init, list_channels, network_info, WasmNetwork, WasmSdkStateConfig,
//! };
//!
//! let mut cfg = WasmSdkStateConfig::new("demo-app");
//! cfg.network = WasmNetwork::Testnet4;
//! cfg.height = 1200;
//!
//! let state = block_on(init(cfg)).expect("init");
//! let net = block_on(network_info(state.clone())).expect("network_info");
//! let channels = block_on(list_channels(state)).expect("list_channels");
//!
//! assert_eq!(net.network, "testnet4");
//! assert_eq!(net.height, 1200);
//! assert!(!channels.is_empty());
//! ```

use std::future::Future;
use std::pin::Pin;

pub mod wasm_bindings;

#[derive(Clone, Debug)]
pub struct WasmSdkInfoData {
    pub sdk_version: &'static str,
    pub runtime: &'static str,
    pub boundary_ready: bool,
}

#[derive(Clone, Debug)]
pub struct WasmRgbKeysData {
    pub mnemonic: String,
    pub xpub: String,
    pub account_xpub_vanilla: String,
    pub account_xpub_colored: String,
    pub master_fingerprint: String,
}

#[derive(Clone, Debug)]
pub struct WasmRuntimeCapabilitiesData {
    pub read_only: bool,
    pub ldk_runtime: bool,
    pub rgb_runtime: bool,
    pub uniffi_bridge: bool,
}

#[derive(Clone, Debug)]
pub struct WasmHealthData {
    pub status: &'static str,
    pub runtime: &'static str,
    pub app_id: String,
}

#[derive(Clone, Debug)]
pub struct AddressData {
    pub address: String,
}

#[derive(Clone, Debug)]
pub struct EstimateFeeData {
    pub fee_rate: f64,
}

#[derive(Clone, Debug)]
pub struct ChannelIdData {
    pub channel_id: String,
}

#[derive(Clone, Debug)]
pub enum WasmNetwork {
    Mainnet,
    Testnet,
    Testnet4,
    Signet,
    Regtest,
    Custom(String),
}

impl WasmNetwork {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Mainnet => "mainnet",
            Self::Testnet => "testnet",
            Self::Testnet4 => "testnet4",
            Self::Signet => "signet",
            Self::Regtest => "regtest",
            Self::Custom(value) => value.as_str(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct NetworkInfoData {
    pub network: String,
    pub height: u32,
}

#[derive(Clone, Debug)]
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

#[derive(Clone, Copy, Debug, Default)]
pub enum ChannelStatus {
    #[default]
    Opening,
    Opened,
    Closing,
}

#[derive(Clone, Debug)]
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

#[derive(Clone, Debug)]
pub struct PeerData {
    pub pubkey: String,
}

#[derive(Clone, Debug, PartialEq)]
pub enum TransactionType {
    RgbSend,
    Drain,
    CreateUtxos,
    User,
}

#[derive(Clone, Debug)]
pub struct BlockTime {
    pub height: u32,
    pub timestamp: u64,
}

#[derive(Clone, Debug)]
pub struct TransactionData {
    pub transaction_type: TransactionType,
    pub txid: String,
    pub received: u64,
    pub sent: u64,
    pub fee: u64,
    pub confirmation_time: Option<BlockTime>,
}

#[derive(Clone, Debug)]
pub struct WasmSdkState {
    pub app_id: String,
    pub network: WasmNetwork,
    pub height: u32,
    pub fee_rate_hint: f64,
    pub channels: Vec<ChannelData>,
    pub peers: Vec<PeerData>,
}

#[derive(Clone, Debug)]
pub struct WasmSdkStateConfig {
    pub app_id: String,
    pub network: WasmNetwork,
    pub height: u32,
    pub fee_rate_hint: f64,
    pub channels: Vec<ChannelData>,
    pub peers: Vec<PeerData>,
}

impl WasmSdkStateConfig {
    pub fn new(app_id: impl Into<String>) -> Self {
        Self {
            app_id: app_id.into(),
            network: WasmNetwork::Regtest,
            height: 0,
            fee_rate_hint: 1.0,
            channels: vec![],
            peers: vec![],
        }
    }
}

impl WasmSdkState {
    pub fn new(app_id: impl Into<String>) -> Self {
        let app_id = app_id.into();
        let default_channels = if app_id.is_empty() {
            vec![]
        } else {
            vec![ChannelData {
                channel_id: format!("wasm-chan:{app_id}"),
                funding_txid: None,
                peer_pubkey: format!("wasm-peer:{app_id}"),
                peer_alias: Some("wasm".to_string()),
                short_channel_id: None,
                status: ChannelStatus::Opened,
                ready: true,
                capacity_sat: 0,
                local_balance_sat: 0,
                outbound_balance_msat: 0,
                inbound_balance_msat: 0,
                next_outbound_htlc_limit_msat: 0,
                next_outbound_htlc_minimum_msat: 0,
                is_usable: true,
                public: false,
                asset_id: None,
                asset_local_amount: None,
                asset_remote_amount: None,
                virtual_open_mode: None,
            }]
        };
        let default_peers = if app_id.is_empty() {
            vec![]
        } else {
            vec![PeerData {
                pubkey: format!("wasm-peer:{app_id}"),
            }]
        };
        Self {
            app_id,
            network: WasmNetwork::Regtest,
            height: 0,
            fee_rate_hint: 1.0,
            channels: default_channels,
            peers: default_peers,
        }
    }

    pub fn with_network(mut self, network: WasmNetwork) -> Self {
        self.network = network;
        self
    }

    pub fn with_height(mut self, height: u32) -> Self {
        self.height = height;
        self
    }

    pub fn with_fee_rate_hint(mut self, fee_rate_hint: f64) -> Self {
        self.fee_rate_hint = fee_rate_hint;
        self
    }

    pub fn with_channels(mut self, channels: Vec<ChannelData>) -> Self {
        self.channels = channels;
        self
    }

    pub fn with_peers(mut self, peers: Vec<PeerData>) -> Self {
        self.peers = peers;
        self
    }

    pub fn from_config(config: WasmSdkStateConfig) -> Self {
        Self::new(config.app_id)
            .with_network(config.network)
            .with_height(config.height)
            .with_fee_rate_hint(config.fee_rate_hint)
            .with_channels(config.channels)
            .with_peers(config.peers)
    }

    fn channels_or_default(&self) -> Vec<ChannelData> {
        if self.channels.is_empty() && self.app_id.is_empty() {
            vec![]
        } else {
            self.channels.clone()
        }
    }

    fn peers_or_default(&self, channels: &[ChannelData]) -> Vec<PeerData> {
        if !self.peers.is_empty() {
            return self.peers.clone();
        }

        if channels.is_empty() {
            vec![]
        } else {
            channels
                .iter()
                .map(|c| PeerData {
                    pubkey: c.peer_pubkey.clone(),
                })
                .collect()
        }
    }
}

#[derive(Clone, Debug, thiserror::Error)]
pub enum WasmApiError {
    #[error("Operation is not yet available on wasm target")]
    Unsupported,
    #[error("Unknown temporary channel ID")]
    UnknownTemporaryChannelId,
    #[error("Invalid request: {0}")]
    InvalidRequest(String),
    #[error("Unsupported network for rgb-lib-wasm: {0}")]
    UnsupportedNetwork(String),
}

pub(crate) mod engine {
    use super::{
        AddressData, ChannelData, ChannelIdData, ChannelStatus, EstimateFeeData, NetworkInfoData,
        NodeInfoData, PeerData, TransactionData, TransactionType, WasmApiError, WasmHealthData,
        WasmRgbKeysData, WasmRuntimeCapabilitiesData, WasmSdkInfoData, WasmSdkState,
        WASM_SDK_BOUNDARY_READY,
    };
    use std::future::Future;
    use std::pin::Pin;

    pub(crate) trait WasmEngine: Send + Sync + 'static {
        fn init(
            &self,
            config: super::WasmSdkStateConfig,
        ) -> Pin<Box<dyn Future<Output = Result<WasmSdkState, WasmApiError>> + Send>>;

        fn sdk_info(
            &self,
        ) -> Pin<Box<dyn Future<Output = Result<WasmSdkInfoData, WasmApiError>> + Send>>;

        fn healthcheck(
            &self,
            state: WasmSdkState,
        ) -> Pin<Box<dyn Future<Output = Result<WasmHealthData, WasmApiError>> + Send>>;

        fn runtime_capabilities(
            &self,
            _state: WasmSdkState,
        ) -> Pin<Box<dyn Future<Output = Result<WasmRuntimeCapabilitiesData, WasmApiError>> + Send>>;

        fn address(
            &self,
            state: WasmSdkState,
        ) -> Pin<Box<dyn Future<Output = Result<AddressData, WasmApiError>> + Send>>;

        fn estimate_fee(
            &self,
            _state: WasmSdkState,
            blocks: u16,
        ) -> Pin<Box<dyn Future<Output = Result<EstimateFeeData, WasmApiError>> + Send>>;

        fn network_info(
            &self,
            state: WasmSdkState,
        ) -> Pin<Box<dyn Future<Output = Result<NetworkInfoData, WasmApiError>> + Send>>;

        fn node_info(
            &self,
            state: WasmSdkState,
        ) -> Pin<Box<dyn Future<Output = Result<NodeInfoData, WasmApiError>> + Send>>;

        fn list_peers(
            &self,
            state: WasmSdkState,
        ) -> Pin<Box<dyn Future<Output = Result<Vec<PeerData>, WasmApiError>> + Send>>;

        fn list_channels(
            &self,
            state: WasmSdkState,
        ) -> Pin<Box<dyn Future<Output = Result<Vec<ChannelData>, WasmApiError>> + Send>>;

        fn list_transactions(
            &self,
            state: WasmSdkState,
            _skip_sync: bool,
        ) -> Pin<Box<dyn Future<Output = Result<Vec<TransactionData>, WasmApiError>> + Send>>;

        fn get_channel_id(
            &self,
            state: WasmSdkState,
            temporary_channel_id: String,
        ) -> Pin<Box<dyn Future<Output = Result<ChannelIdData, WasmApiError>> + Send>>;

        fn rgb_generate_keys(
            &self,
            network: super::WasmNetwork,
        ) -> Pin<Box<dyn Future<Output = Result<WasmRgbKeysData, WasmApiError>> + Send>>;

        fn rgb_restore_keys(
            &self,
            network: super::WasmNetwork,
            mnemonic: String,
        ) -> Pin<Box<dyn Future<Output = Result<WasmRgbKeysData, WasmApiError>> + Send>>;
    }

    /// Placeholder engine for wasm builds while SDK runtime APIs are incrementally ported.
    pub(crate) struct WasmSdkEngine;

    #[cfg(feature = "real-wasm-rgb")]
    fn map_network(
        network: super::WasmNetwork,
    ) -> Result<rgb_lib_wasm::BitcoinNetwork, WasmApiError> {
        match network {
            super::WasmNetwork::Mainnet => Ok(rgb_lib_wasm::BitcoinNetwork::Mainnet),
            super::WasmNetwork::Testnet => Ok(rgb_lib_wasm::BitcoinNetwork::Testnet),
            super::WasmNetwork::Testnet4 => Ok(rgb_lib_wasm::BitcoinNetwork::Testnet4),
            super::WasmNetwork::Signet => Ok(rgb_lib_wasm::BitcoinNetwork::Signet),
            super::WasmNetwork::Regtest => Ok(rgb_lib_wasm::BitcoinNetwork::Regtest),
            super::WasmNetwork::Custom(value) => Err(WasmApiError::UnsupportedNetwork(value)),
        }
    }

    #[cfg(feature = "real-wasm-rgb")]
    fn map_keys(keys: rgb_lib_wasm::keys::Keys) -> WasmRgbKeysData {
        WasmRgbKeysData {
            mnemonic: keys.mnemonic,
            xpub: keys.xpub,
            account_xpub_vanilla: keys.account_xpub_vanilla,
            account_xpub_colored: keys.account_xpub_colored,
            master_fingerprint: keys.master_fingerprint,
        }
    }

    impl WasmEngine for WasmSdkEngine {
        fn init(
            &self,
            config: super::WasmSdkStateConfig,
        ) -> Pin<Box<dyn Future<Output = Result<WasmSdkState, WasmApiError>> + Send>> {
            Box::pin(async move {
                if config.app_id.trim().is_empty() {
                    return Err(WasmApiError::InvalidRequest(
                        "app_id cannot be empty".to_string(),
                    ));
                }
                Ok(WasmSdkState::from_config(config))
            })
        }

        fn sdk_info(
            &self,
        ) -> Pin<Box<dyn Future<Output = Result<WasmSdkInfoData, WasmApiError>> + Send>> {
            Box::pin(async move {
                Ok(WasmSdkInfoData {
                    sdk_version: env!("CARGO_PKG_VERSION"),
                    runtime: "wasm32-unknown-unknown",
                    boundary_ready: WASM_SDK_BOUNDARY_READY,
                })
            })
        }

        fn healthcheck(
            &self,
            state: WasmSdkState,
        ) -> Pin<Box<dyn Future<Output = Result<WasmHealthData, WasmApiError>> + Send>> {
            Box::pin(async move {
                Ok(WasmHealthData {
                    status: "ok",
                    runtime: "wasm32-unknown-unknown",
                    app_id: state.app_id,
                })
            })
        }

        fn runtime_capabilities(
            &self,
            _state: WasmSdkState,
        ) -> Pin<Box<dyn Future<Output = Result<WasmRuntimeCapabilitiesData, WasmApiError>> + Send>>
        {
            Box::pin(async move {
                Ok(WasmRuntimeCapabilitiesData {
                    read_only: true,
                    ldk_runtime: false,
                    rgb_runtime: false,
                    uniffi_bridge: false,
                })
            })
        }

        fn address(
            &self,
            state: WasmSdkState,
        ) -> Pin<Box<dyn Future<Output = Result<AddressData, WasmApiError>> + Send>> {
            Box::pin(async move {
                Ok(AddressData {
                    address: format!("wasm-unavailable:{}", state.app_id),
                })
            })
        }

        fn estimate_fee(
            &self,
            state: WasmSdkState,
            blocks: u16,
        ) -> Pin<Box<dyn Future<Output = Result<EstimateFeeData, WasmApiError>> + Send>> {
            let clamped = blocks.max(1) as f64;
            let fee_rate = (state.fee_rate_hint / clamped).max(0.1);
            Box::pin(async move { Ok(EstimateFeeData { fee_rate }) })
        }

        fn network_info(
            &self,
            state: WasmSdkState,
        ) -> Pin<Box<dyn Future<Output = Result<NetworkInfoData, WasmApiError>> + Send>> {
            Box::pin(async move {
                Ok(NetworkInfoData {
                    network: state.network.as_str().to_string(),
                    height: state.height,
                })
            })
        }

        fn node_info(
            &self,
            state: WasmSdkState,
        ) -> Pin<Box<dyn Future<Output = Result<NodeInfoData, WasmApiError>> + Send>> {
            let channels = state.channels_or_default();
            let peers = state.peers_or_default(&channels);
            let num_channels = channels.len();
            let num_usable_channels = channels.iter().filter(|c| c.is_usable).count();
            let num_peers = peers.len();
            Box::pin(async move {
                Ok(NodeInfoData {
                    pubkey: format!("wasm-node:{}", state.app_id),
                    num_channels,
                    num_usable_channels,
                    local_balance_sat: 0,
                    eventual_close_fees_sat: 0,
                    pending_outbound_payments_sat: 0,
                    num_peers,
                    account_xpub_vanilla: "wasm-unavailable".to_string(),
                    account_xpub_colored: "wasm-unavailable".to_string(),
                    max_media_upload_size_mb: 0,
                    rgb_htlc_min_msat: 0,
                    rgb_channel_capacity_min_sat: 0,
                    channel_capacity_min_sat: 0,
                    channel_capacity_max_sat: 0,
                    channel_asset_min_amount: 0,
                    channel_asset_max_amount: 0,
                    network_nodes: 0,
                    network_channels: 0,
                })
            })
        }

        fn list_peers(
            &self,
            state: WasmSdkState,
        ) -> Pin<Box<dyn Future<Output = Result<Vec<PeerData>, WasmApiError>> + Send>> {
            let channels = state.channels_or_default();
            let peers = state.peers_or_default(&channels);
            Box::pin(async move { Ok(peers) })
        }

        fn list_channels(
            &self,
            state: WasmSdkState,
        ) -> Pin<Box<dyn Future<Output = Result<Vec<ChannelData>, WasmApiError>> + Send>> {
            let channels = state.channels_or_default();
            Box::pin(async move { Ok(channels) })
        }

        fn list_transactions(
            &self,
            state: WasmSdkState,
            _skip_sync: bool,
        ) -> Pin<Box<dyn Future<Output = Result<Vec<TransactionData>, WasmApiError>> + Send>>
        {
            Box::pin(async move {
                if state.app_id.is_empty() {
                    return Ok(vec![]);
                }

                Ok(vec![TransactionData {
                    transaction_type: TransactionType::User,
                    txid: format!("wasm-tx:{}", state.app_id),
                    received: 0,
                    sent: 0,
                    fee: 0,
                    confirmation_time: Some(super::BlockTime {
                        height: state.height,
                        timestamp: 0,
                    }),
                }])
            })
        }

        fn get_channel_id(
            &self,
            state: WasmSdkState,
            temporary_channel_id: String,
        ) -> Pin<Box<dyn Future<Output = Result<ChannelIdData, WasmApiError>> + Send>> {
            Box::pin(async move {
                let channels = state.channels_or_default();
                let expected_temporary = format!("wasm-tmp:{}", state.app_id);
                if temporary_channel_id.trim().is_empty()
                    || temporary_channel_id != expected_temporary
                    || channels.is_empty()
                {
                    return Err(WasmApiError::UnknownTemporaryChannelId);
                }

                Ok(ChannelIdData {
                    channel_id: channels[0].channel_id.clone(),
                })
            })
        }

        fn rgb_generate_keys(
            &self,
            network: super::WasmNetwork,
        ) -> Pin<Box<dyn Future<Output = Result<WasmRgbKeysData, WasmApiError>> + Send>> {
            Box::pin(async move {
                #[cfg(feature = "real-wasm-rgb")]
                {
                    let network = map_network(network)?;
                    let keys = rgb_lib_wasm::generate_keys(network);
                    return Ok(map_keys(keys));
                }

                #[cfg(not(feature = "real-wasm-rgb"))]
                {
                    let _ = network;
                    Err(WasmApiError::Unsupported)
                }
            })
        }

        fn rgb_restore_keys(
            &self,
            network: super::WasmNetwork,
            mnemonic: String,
        ) -> Pin<Box<dyn Future<Output = Result<WasmRgbKeysData, WasmApiError>> + Send>> {
            Box::pin(async move {
                #[cfg(feature = "real-wasm-rgb")]
                {
                    if mnemonic.trim().is_empty() {
                        return Err(WasmApiError::InvalidRequest(
                            "mnemonic cannot be empty".to_string(),
                        ));
                    }
                    let network = map_network(network)?;
                    let keys = rgb_lib_wasm::restore_keys(network, mnemonic)
                        .map_err(|e| WasmApiError::InvalidRequest(e.to_string()))?;
                    return Ok(map_keys(keys));
                }

                #[cfg(not(feature = "real-wasm-rgb"))]
                {
                    let _ = network;
                    let _ = mnemonic;
                    Err(WasmApiError::Unsupported)
                }
            })
        }
    }

    pub(crate) fn default_engine() -> &'static WasmSdkEngine {
        static ENGINE: WasmSdkEngine = WasmSdkEngine;
        &ENGINE
    }
}

/// Indicates that the wasm SDK boundary module is compiled and available.
pub const WASM_SDK_BOUNDARY_READY: bool = true;

pub fn init(
    config: WasmSdkStateConfig,
) -> Pin<Box<dyn Future<Output = Result<WasmSdkState, WasmApiError>> + Send>> {
    use crate::sdk::engine::WasmEngine;
    engine::default_engine().init(config)
}

pub fn sdk_info() -> Pin<Box<dyn Future<Output = Result<WasmSdkInfoData, WasmApiError>> + Send>> {
    use crate::sdk::engine::WasmEngine;
    engine::default_engine().sdk_info()
}

pub fn healthcheck(
    state: WasmSdkState,
) -> Pin<Box<dyn Future<Output = Result<WasmHealthData, WasmApiError>> + Send>> {
    use crate::sdk::engine::WasmEngine;
    engine::default_engine().healthcheck(state)
}

pub fn runtime_capabilities(
    state: WasmSdkState,
) -> Pin<Box<dyn Future<Output = Result<WasmRuntimeCapabilitiesData, WasmApiError>> + Send>> {
    use crate::sdk::engine::WasmEngine;
    engine::default_engine().runtime_capabilities(state)
}

pub fn address(
    state: WasmSdkState,
) -> Pin<Box<dyn Future<Output = Result<AddressData, WasmApiError>> + Send>> {
    use crate::sdk::engine::WasmEngine;
    engine::default_engine().address(state)
}

pub fn estimate_fee(
    state: WasmSdkState,
    blocks: u16,
) -> Pin<Box<dyn Future<Output = Result<EstimateFeeData, WasmApiError>> + Send>> {
    use crate::sdk::engine::WasmEngine;
    engine::default_engine().estimate_fee(state, blocks)
}

pub fn network_info(
    state: WasmSdkState,
) -> Pin<Box<dyn Future<Output = Result<NetworkInfoData, WasmApiError>> + Send>> {
    use crate::sdk::engine::WasmEngine;
    engine::default_engine().network_info(state)
}

pub fn node_info(
    state: WasmSdkState,
) -> Pin<Box<dyn Future<Output = Result<NodeInfoData, WasmApiError>> + Send>> {
    use crate::sdk::engine::WasmEngine;
    engine::default_engine().node_info(state)
}

pub fn list_peers(
    state: WasmSdkState,
) -> Pin<Box<dyn Future<Output = Result<Vec<PeerData>, WasmApiError>> + Send>> {
    use crate::sdk::engine::WasmEngine;
    engine::default_engine().list_peers(state)
}

pub fn list_channels(
    state: WasmSdkState,
) -> Pin<Box<dyn Future<Output = Result<Vec<ChannelData>, WasmApiError>> + Send>> {
    use crate::sdk::engine::WasmEngine;
    engine::default_engine().list_channels(state)
}

pub fn list_transactions(
    state: WasmSdkState,
    skip_sync: bool,
) -> Pin<Box<dyn Future<Output = Result<Vec<TransactionData>, WasmApiError>> + Send>> {
    use crate::sdk::engine::WasmEngine;
    engine::default_engine().list_transactions(state, skip_sync)
}

pub fn get_channel_id(
    state: WasmSdkState,
    temporary_channel_id: String,
) -> Pin<Box<dyn Future<Output = Result<ChannelIdData, WasmApiError>> + Send>> {
    use crate::sdk::engine::WasmEngine;
    engine::default_engine().get_channel_id(state, temporary_channel_id)
}

pub fn rgb_generate_keys(
    network: WasmNetwork,
) -> Pin<Box<dyn Future<Output = Result<WasmRgbKeysData, WasmApiError>> + Send>> {
    use crate::sdk::engine::WasmEngine;
    engine::default_engine().rgb_generate_keys(network)
}

pub fn rgb_restore_keys(
    network: WasmNetwork,
    mnemonic: String,
) -> Pin<Box<dyn Future<Output = Result<WasmRgbKeysData, WasmApiError>> + Send>> {
    use crate::sdk::engine::WasmEngine;
    engine::default_engine().rgb_restore_keys(network, mnemonic)
}

#[cfg(all(test, target_arch = "wasm32"))]
mod tests {
    use super::*;
    use futures::executor::block_on;

    fn sample_channel(channel_id: &str, peer_pubkey: &str, is_usable: bool) -> ChannelData {
        ChannelData {
            channel_id: channel_id.to_string(),
            funding_txid: None,
            peer_pubkey: peer_pubkey.to_string(),
            peer_alias: None,
            short_channel_id: None,
            status: ChannelStatus::Opened,
            ready: true,
            capacity_sat: 0,
            local_balance_sat: 0,
            outbound_balance_msat: 0,
            inbound_balance_msat: 0,
            next_outbound_htlc_limit_msat: 0,
            next_outbound_htlc_minimum_msat: 0,
            is_usable,
            public: false,
            asset_id: None,
            asset_local_amount: None,
            asset_remote_amount: None,
            virtual_open_mode: None,
        }
    }

    #[test]
    fn network_info_uses_state_values() {
        let state = block_on(init(WasmSdkStateConfig {
            app_id: "app-1".to_string(),
            network: WasmNetwork::Testnet4,
            height: 777,
            fee_rate_hint: 1.0,
            channels: vec![],
            peers: vec![],
        }))
        .expect("init should succeed");
        let info = block_on(network_info(state)).expect("network_info should succeed");
        assert_eq!(info.network, "testnet4");
        assert_eq!(info.height, 777);
    }

    #[test]
    fn list_channels_and_node_info_are_consistent() {
        let state = WasmSdkState::new("app-2");
        let channels =
            block_on(list_channels(state.clone())).expect("list_channels should succeed");
        let node_info = block_on(node_info(state)).expect("node_info should succeed");
        assert_eq!(channels.len(), node_info.num_channels);
        let usable = channels.iter().filter(|c| c.is_usable).count();
        assert_eq!(usable, node_info.num_usable_channels);
    }

    #[test]
    fn get_channel_id_success_and_failure() {
        let state = WasmSdkState::new("app-3");
        let ok = block_on(get_channel_id(state.clone(), "wasm-tmp:app-3".to_string()))
            .expect("expected valid temporary channel id");
        assert_eq!(ok.channel_id, "wasm-chan:app-3");

        let err = block_on(get_channel_id(state, "bad-id".to_string()))
            .expect_err("expected unknown temporary channel id");
        assert!(matches!(err, WasmApiError::UnknownTemporaryChannelId));
    }

    #[test]
    fn list_channels_uses_custom_state_channels() {
        let state = block_on(init(WasmSdkStateConfig {
            app_id: "app-chans".to_string(),
            network: WasmNetwork::Regtest,
            height: 0,
            fee_rate_hint: 1.0,
            channels: vec![
                sample_channel("chan-1", "peer-a", true),
                sample_channel("chan-2", "peer-b", false),
            ],
            peers: vec![],
        }))
        .expect("init should succeed");
        let channels = block_on(list_channels(state)).expect("list_channels should succeed");
        assert_eq!(channels.len(), 2);
        assert_eq!(channels[0].channel_id, "chan-1");
        assert_eq!(channels[1].channel_id, "chan-2");
    }

    #[test]
    fn list_peers_prefers_custom_peers_over_channel_derivation() {
        let state = block_on(init(WasmSdkStateConfig {
            app_id: "app-peers".to_string(),
            network: WasmNetwork::Regtest,
            height: 0,
            fee_rate_hint: 1.0,
            channels: vec![sample_channel("chan-1", "peer-from-channel", true)],
            peers: vec![PeerData {
                pubkey: "explicit-peer".to_string(),
            }],
        }))
        .expect("init should succeed");

        let peers = block_on(list_peers(state)).expect("list_peers should succeed");
        assert_eq!(peers.len(), 1);
        assert_eq!(peers[0].pubkey, "explicit-peer");
    }

    #[test]
    fn estimate_fee_uses_fee_rate_hint() {
        let mut config = WasmSdkStateConfig::new("app-fee");
        config.fee_rate_hint = 8.0;
        let state = block_on(init(config)).expect("init should succeed");
        let fee = block_on(estimate_fee(state, 4)).expect("estimate_fee should succeed");
        assert_eq!(fee.fee_rate, 2.0);
    }

    #[test]
    fn init_rejects_empty_app_id() {
        let mut config = WasmSdkStateConfig::new("");
        config.app_id = "".to_string();
        let err = block_on(init(config)).expect_err("init should fail");
        assert!(matches!(err, WasmApiError::InvalidRequest(_)));
    }

    #[cfg(feature = "real-wasm-rgb")]
    #[test]
    fn rgb_keys_generate_and_restore_roundtrip() {
        let keys = block_on(rgb_generate_keys(WasmNetwork::Regtest))
            .expect("rgb_generate_keys should succeed");
        assert!(!keys.mnemonic.is_empty());
        assert!(!keys.account_xpub_colored.is_empty());
        assert!(!keys.account_xpub_vanilla.is_empty());

        let restored = block_on(rgb_restore_keys(
            WasmNetwork::Regtest,
            keys.mnemonic.clone(),
        ))
        .expect("rgb_restore_keys should succeed");
        assert_eq!(restored.master_fingerprint, keys.master_fingerprint);
        assert_eq!(restored.account_xpub_colored, keys.account_xpub_colored);
        assert_eq!(restored.account_xpub_vanilla, keys.account_xpub_vanilla);
    }
}
