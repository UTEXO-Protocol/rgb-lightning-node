use std::fs;
use std::path::Path;

use serde::Deserialize;

use crate::error::AppError;

/// Mirror of the TOML config file. Every field is optional: absent keys keep
/// the built-in defaults, unknown keys are hard errors.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TomlConfig {
    pub(crate) node: Option<TomlNode>,
    pub(crate) auth: Option<TomlAuth>,
    pub(crate) chain: Option<TomlChain>,
    pub(crate) rgb: Option<TomlRgb>,
    pub(crate) channels: Option<TomlChannels>,
    pub(crate) payments: Option<TomlPayments>,
    pub(crate) gossip: Option<TomlGossip>,
    pub(crate) lsp: Option<TomlLsp>,
    pub(crate) vss: Option<TomlVss>,
    pub(crate) api: Option<TomlApi>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TomlNode {
    pub(crate) network: Option<String>,
    pub(crate) daemon_listening_port: Option<u16>,
    pub(crate) ldk_peer_listening_port: Option<u16>,
    pub(crate) announce_alias: Option<String>,
    pub(crate) announce_addresses: Option<Vec<String>>,
    pub(crate) reuse_addresses: Option<bool>,
    pub(crate) peer_reconnect_interval_secs: Option<u64>,
    pub(crate) announce_initial_delay_secs: Option<u64>,
    pub(crate) announce_refresh_interval_secs: Option<u64>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TomlAuth {
    pub(crate) disable_authentication: Option<bool>,
    pub(crate) root_public_key: Option<String>,
    pub(crate) password_min_length: Option<u8>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TomlChain {
    pub(crate) indexer_url: Option<String>,
    pub(crate) proxy_endpoint: Option<String>,
    pub(crate) indexer_timeout_secs: Option<u64>,
    pub(crate) fee_refresh_interval_secs: Option<u64>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TomlRgb {
    pub(crate) fee_rate_sat_vb: Option<u64>,
    pub(crate) utxo_size_sat: Option<u32>,
    pub(crate) utxo_num: Option<u8>,
    pub(crate) min_channel_confirmations: Option<u8>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TomlChannels {
    pub(crate) htlc_min_msat: Option<u64>,
    pub(crate) virtual_htlc_min_msat: Option<u64>,
    pub(crate) dust_limit_msat: Option<u64>,
    pub(crate) open_min_sat: Option<u64>,
    pub(crate) open_max_sat: Option<u64>,
    pub(crate) open_min_rgb_amount: Option<u64>,
    pub(crate) their_to_self_delay: Option<u16>,
    pub(crate) accept_forwards_to_priv_channels: Option<bool>,
    pub(crate) cltv_expiry_delta: Option<u16>,
    pub(crate) our_to_self_delay: Option<u16>,
    pub(crate) forwarding_fee_base_msat: Option<u32>,
    pub(crate) forwarding_fee_proportional_millionths: Option<u32>,
    pub(crate) max_dust_htlc_exposure_multiplier: Option<u64>,
    pub(crate) max_dust_htlc_exposure_fixed_msat: Option<u64>,
    pub(crate) max_inbound_htlc_value_in_flight_percent: Option<u8>,
    pub(crate) our_max_accepted_htlcs: Option<u16>,
    pub(crate) max_minimum_depth: Option<u32>,
    pub(crate) enable_virtual_channels_v0: Option<bool>,
    pub(crate) virtual_peer_pubkeys: Option<Vec<String>>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TomlPayments {
    pub(crate) final_cltv_expiry_delta: Option<u32>,
    pub(crate) retry_timeout_secs: Option<u64>,
    pub(crate) max_swap_fee_msat: Option<u64>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TomlGossip {
    pub(crate) rgs_sync_interval_secs: Option<u64>,
    pub(crate) rgs_snapshot_max_size_mb: Option<u64>,
    pub(crate) rgs_connect_timeout_secs: Option<u64>,
    pub(crate) rgs_sync_timeout_secs: Option<u64>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TomlLsp {
    pub(crate) base_url: Option<String>,
    pub(crate) bearer_token: Option<String>,
    pub(crate) order_response_timeout_secs: Option<u64>,
    pub(crate) request_timeout_secs: Option<u64>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TomlVss {
    pub(crate) url: Option<String>,
    pub(crate) allow_http: Option<bool>,
    pub(crate) allow_empty_restore: Option<bool>,
    pub(crate) retry_backoff_ms: Option<u64>,
    pub(crate) retry_max_attempts: Option<u32>,
    pub(crate) retry_max_total_delay_secs: Option<u64>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TomlApi {
    pub(crate) max_media_upload_size_mb: Option<u16>,
    pub(crate) default_page_size: Option<u64>,
}

impl TomlConfig {
    pub(crate) fn parse(content: &str) -> Result<Self, AppError> {
        toml::from_str(content).map_err(|e| AppError::InvalidConfig(e.to_string()))
    }
}

pub(crate) fn load_config_file(path: &Path) -> Result<TomlConfig, AppError> {
    let content = fs::read_to_string(path).map_err(|e| {
        AppError::InvalidConfig(format!("cannot read config file {}: {e}", path.display()))
    })?;
    TomlConfig::parse(&content).map_err(|e| match e {
        AppError::InvalidConfig(m) => AppError::InvalidConfig(format!("{}: {m}", path.display())),
        other => other,
    })
}
