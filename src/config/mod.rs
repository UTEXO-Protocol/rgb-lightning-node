mod file;
#[cfg(test)]
mod tests;

pub(crate) use file::{load_config_file, TomlConfig};

use crate::core_types::{
    DEFAULT_FINAL_CLTV_EXPIRY_DELTA, DUST_LIMIT_MSAT, FEE_RATE, HTLC_MIN_MSAT, MAX_SWAP_FEE_MSAT,
    MIN_CHANNEL_CONFIRMATIONS, UTXO_SIZE_SAT, VIRTUAL_HTLC_MIN_MSAT,
};
use lightning::ln::channelmanager::{BREAKDOWN_TIMEOUT, MIN_CLTV_EXPIRY_DELTA};
use lightning::util::config::{
    ChannelConfig as LdkChannelConfig, ChannelHandshakeConfig as LdkChannelHandshakeConfig,
    ChannelHandshakeLimits as LdkChannelHandshakeLimits, MaxDustHTLCExposure,
};

use crate::error::AppError;

pub(crate) const DEFAULT_CONFIG_FILENAME: &str = "config.toml";

const DEFAULT_UTXO_NUM: u8 = 4;
const DEFAULT_OPENCHANNEL_MIN_SAT: u64 = 5506;
const DEFAULT_OPENCHANNEL_MAX_SAT: u64 = 16_777_215;
const DEFAULT_OPENCHANNEL_MIN_RGB_AMT: u64 = 1;
// lnd's max to_self_delay is 2016, so we want to be compatible
const DEFAULT_THEIR_TO_SELF_DELAY: u16 = 2016;
const DEFAULT_PAYMENT_RETRY_TIMEOUT_SECS: u64 = 10;
const DEFAULT_PASSWORD_MIN_LENGTH: u8 = 8;
const DEFAULT_PAGE_SIZE: u64 = 100;
const DEFAULT_PEER_RECONNECT_INTERVAL_SECS: u64 = 1;
const DEFAULT_ANNOUNCE_INITIAL_DELAY_SECS: u64 = 60;
const DEFAULT_ANNOUNCE_REFRESH_INTERVAL_SECS: u64 = 3600;
const DEFAULT_INDEXER_TIMEOUT_SECS: u64 = 10;
const DEFAULT_FEE_REFRESH_INTERVAL_SECS: u64 = 60;
const DEFAULT_RGS_SYNC_INTERVAL_SECS: u64 = 3600;
const DEFAULT_RGS_SNAPSHOT_MAX_SIZE_MB: u64 = 15;
const DEFAULT_RGS_CONNECT_TIMEOUT_SECS: u64 = 5;
const DEFAULT_RGS_SYNC_TIMEOUT_SECS: u64 = 60;
const DEFAULT_VSS_RETRY_BACKOFF_MS: u64 = 100;
const DEFAULT_VSS_RETRY_MAX_ATTEMPTS: u32 = 3;
const DEFAULT_VSS_RETRY_MAX_TOTAL_DELAY_SECS: u64 = 5;
const DEFAULT_LSP_ORDER_RESPONSE_TIMEOUT_SECS: u64 = 30;
// must stay above utexo-lsp's default 15s HTTP timeout with a buffer
const DEFAULT_LSP_REQUEST_TIMEOUT_SECS: u64 = 25;

const MAX_TO_SELF_DELAY: u16 = 2016;
const MAX_CHANNEL_SAT: u64 = 16_777_215;
const MAX_ALIAS_BYTES: usize = 32;
// BOLT-2 maximum for max_accepted_htlcs (LDK silently clamps higher values)
const MAX_ACCEPTED_HTLCS_CAP: u16 = 483;

/// Node settings resolved from built-in defaults overridden by the optional
/// TOML config file. With no config file every value matches the historical
/// hardcoded behavior.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Config {
    pub(crate) node: NodeSection,
    pub(crate) chain: ChainSection,
    pub(crate) rgb: RgbSection,
    pub(crate) channels: ChannelsSection,
    pub(crate) payments: PaymentsSection,
    pub(crate) gossip: GossipSection,
    pub(crate) vss: VssSection,
    pub(crate) lsp: LspSection,
    pub(crate) auth: AuthSection,
    pub(crate) api: ApiSection,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct NodeSection {
    pub(crate) announce_alias: Option<String>,
    pub(crate) announce_addresses: Vec<String>,
    pub(crate) peer_reconnect_interval_secs: u64,
    pub(crate) announce_initial_delay_secs: u64,
    pub(crate) announce_refresh_interval_secs: u64,
}

impl Default for NodeSection {
    fn default() -> Self {
        Self {
            announce_alias: None,
            announce_addresses: Vec::new(),
            peer_reconnect_interval_secs: DEFAULT_PEER_RECONNECT_INTERVAL_SECS,
            announce_initial_delay_secs: DEFAULT_ANNOUNCE_INITIAL_DELAY_SECS,
            announce_refresh_interval_secs: DEFAULT_ANNOUNCE_REFRESH_INTERVAL_SECS,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ChainSection {
    pub(crate) indexer_url: Option<String>,
    pub(crate) proxy_endpoint: Option<String>,
    pub(crate) indexer_timeout_secs: u64,
    pub(crate) fee_refresh_interval_secs: u64,
}

impl Default for ChainSection {
    fn default() -> Self {
        Self {
            indexer_url: None,
            proxy_endpoint: None,
            indexer_timeout_secs: DEFAULT_INDEXER_TIMEOUT_SECS,
            fee_refresh_interval_secs: DEFAULT_FEE_REFRESH_INTERVAL_SECS,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct GossipSection {
    pub(crate) rgs_sync_interval_secs: u64,
    pub(crate) rgs_snapshot_max_size_mb: u64,
    pub(crate) rgs_connect_timeout_secs: u64,
    pub(crate) rgs_sync_timeout_secs: u64,
}

impl Default for GossipSection {
    fn default() -> Self {
        Self {
            rgs_sync_interval_secs: DEFAULT_RGS_SYNC_INTERVAL_SECS,
            rgs_snapshot_max_size_mb: DEFAULT_RGS_SNAPSHOT_MAX_SIZE_MB,
            rgs_connect_timeout_secs: DEFAULT_RGS_CONNECT_TIMEOUT_SECS,
            rgs_sync_timeout_secs: DEFAULT_RGS_SYNC_TIMEOUT_SECS,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct VssSection {
    pub(crate) retry_backoff_ms: u64,
    pub(crate) retry_max_attempts: u32,
    pub(crate) retry_max_total_delay_secs: u64,
}

impl Default for VssSection {
    fn default() -> Self {
        Self {
            retry_backoff_ms: DEFAULT_VSS_RETRY_BACKOFF_MS,
            retry_max_attempts: DEFAULT_VSS_RETRY_MAX_ATTEMPTS,
            retry_max_total_delay_secs: DEFAULT_VSS_RETRY_MAX_TOTAL_DELAY_SECS,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct LspSection {
    pub(crate) order_response_timeout_secs: u64,
    pub(crate) request_timeout_secs: u64,
}

impl Default for LspSection {
    fn default() -> Self {
        Self {
            order_response_timeout_secs: DEFAULT_LSP_ORDER_RESPONSE_TIMEOUT_SECS,
            request_timeout_secs: DEFAULT_LSP_REQUEST_TIMEOUT_SECS,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RgbSection {
    pub(crate) fee_rate_sat_vb: u64,
    pub(crate) utxo_size_sat: u32,
    pub(crate) utxo_num: u8,
    pub(crate) min_channel_confirmations: u8,
}

impl Default for RgbSection {
    fn default() -> Self {
        Self {
            fee_rate_sat_vb: FEE_RATE,
            utxo_size_sat: UTXO_SIZE_SAT,
            utxo_num: DEFAULT_UTXO_NUM,
            min_channel_confirmations: MIN_CHANNEL_CONFIRMATIONS,
        }
    }
}

/// Maximum dust HTLC exposure, mirroring LDK's `MaxDustHTLCExposure`.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum DustExposure {
    FeeRateMultiplier(u64),
    FixedLimitMsat(u64),
}

impl DustExposure {
    fn from_ldk(value: MaxDustHTLCExposure) -> Self {
        match value {
            MaxDustHTLCExposure::FeeRateMultiplier(m) => Self::FeeRateMultiplier(m),
            MaxDustHTLCExposure::FixedLimitMsat(m) => Self::FixedLimitMsat(m),
        }
    }

    fn to_ldk(&self) -> MaxDustHTLCExposure {
        match self {
            Self::FeeRateMultiplier(m) => MaxDustHTLCExposure::FeeRateMultiplier(*m),
            Self::FixedLimitMsat(m) => MaxDustHTLCExposure::FixedLimitMsat(*m),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ChannelsSection {
    pub(crate) htlc_min_msat: u64,
    pub(crate) virtual_htlc_min_msat: u64,
    pub(crate) dust_limit_msat: u64,
    pub(crate) open_min_sat: u64,
    pub(crate) open_max_sat: u64,
    pub(crate) open_min_rgb_amount: u64,
    pub(crate) their_to_self_delay: u16,
    pub(crate) accept_forwards_to_priv_channels: bool,
    pub(crate) cltv_expiry_delta: u16,
    pub(crate) our_to_self_delay: u16,
    pub(crate) forwarding_fee_base_msat: u32,
    pub(crate) forwarding_fee_proportional_millionths: u32,
    pub(crate) max_dust_htlc_exposure: DustExposure,
    pub(crate) max_inbound_htlc_value_in_flight_percent: u8,
    pub(crate) our_max_accepted_htlcs: u16,
    pub(crate) max_minimum_depth: u32,
}

impl Default for ChannelsSection {
    fn default() -> Self {
        // LDK-inherited values source their defaults from LDK itself so they
        // keep tracking the fork
        let ldk_channel = LdkChannelConfig::default();
        let ldk_handshake = LdkChannelHandshakeConfig::default();
        let ldk_limits = LdkChannelHandshakeLimits::default();
        Self {
            htlc_min_msat: HTLC_MIN_MSAT,
            virtual_htlc_min_msat: VIRTUAL_HTLC_MIN_MSAT,
            dust_limit_msat: DUST_LIMIT_MSAT,
            open_min_sat: DEFAULT_OPENCHANNEL_MIN_SAT,
            open_max_sat: DEFAULT_OPENCHANNEL_MAX_SAT,
            open_min_rgb_amount: DEFAULT_OPENCHANNEL_MIN_RGB_AMT,
            their_to_self_delay: DEFAULT_THEIR_TO_SELF_DELAY,
            accept_forwards_to_priv_channels: false,
            cltv_expiry_delta: ldk_channel.cltv_expiry_delta,
            our_to_self_delay: ldk_handshake.our_to_self_delay,
            forwarding_fee_base_msat: ldk_channel.forwarding_fee_base_msat,
            forwarding_fee_proportional_millionths: ldk_channel
                .forwarding_fee_proportional_millionths,
            max_dust_htlc_exposure: DustExposure::from_ldk(ldk_channel.max_dust_htlc_exposure),
            max_inbound_htlc_value_in_flight_percent: ldk_handshake
                .max_inbound_htlc_value_in_flight_percent_of_channel,
            our_max_accepted_htlcs: ldk_handshake.our_max_accepted_htlcs,
            max_minimum_depth: ldk_limits.max_minimum_depth,
        }
    }
}

impl ChannelsSection {
    /// Minimum capacity for an RGB channel, derived from the HTLC minimum so
    /// the channel can always route at least ten minimum HTLCs.
    pub(crate) fn open_rgb_min_sat(&self) -> u64 {
        self.htlc_min_msat / 1000 * 10 + 10
    }

    /// LDK per-channel config with the configured forwarding values applied.
    pub(crate) fn channel_config(&self) -> LdkChannelConfig {
        LdkChannelConfig {
            forwarding_fee_proportional_millionths: self.forwarding_fee_proportional_millionths,
            forwarding_fee_base_msat: self.forwarding_fee_base_msat,
            cltv_expiry_delta: self.cltv_expiry_delta,
            max_dust_htlc_exposure: self.max_dust_htlc_exposure.to_ldk(),
            ..Default::default()
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PaymentsSection {
    pub(crate) final_cltv_expiry_delta: u32,
    pub(crate) retry_timeout_secs: u64,
    pub(crate) max_swap_fee_msat: u64,
}

impl Default for PaymentsSection {
    fn default() -> Self {
        Self {
            final_cltv_expiry_delta: DEFAULT_FINAL_CLTV_EXPIRY_DELTA,
            retry_timeout_secs: DEFAULT_PAYMENT_RETRY_TIMEOUT_SECS,
            max_swap_fee_msat: MAX_SWAP_FEE_MSAT,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct AuthSection {
    pub(crate) password_min_length: u8,
}

impl Default for AuthSection {
    fn default() -> Self {
        Self {
            password_min_length: DEFAULT_PASSWORD_MIN_LENGTH,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ApiSection {
    pub(crate) default_page_size: u64,
}

impl Default for ApiSection {
    fn default() -> Self {
        Self {
            default_page_size: DEFAULT_PAGE_SIZE,
        }
    }
}

fn invalid(msg: String) -> AppError {
    AppError::InvalidConfig(msg)
}

impl Config {
    /// Overlay the file values on the defaults and validate the result.
    pub(crate) fn from_toml(toml: &TomlConfig) -> Result<Self, AppError> {
        if let Some(channels) = &toml.channels {
            if channels.max_dust_htlc_exposure_multiplier.is_some()
                && channels.max_dust_htlc_exposure_fixed_msat.is_some()
            {
                return Err(invalid(
                    "channels.max_dust_htlc_exposure_multiplier and \
                     channels.max_dust_htlc_exposure_fixed_msat are mutually exclusive"
                        .to_string(),
                ));
            }
        }
        let mut config = Config::default();
        config.apply_toml(toml);
        config.validate()?;
        Ok(config)
    }

    fn apply_toml(&mut self, toml: &TomlConfig) {
        if let Some(node) = &toml.node {
            if node.announce_alias.is_some() {
                self.node.announce_alias = node.announce_alias.clone();
            }
            if let Some(addresses) = &node.announce_addresses {
                self.node.announce_addresses = addresses.clone();
            }
            apply(
                &mut self.node.peer_reconnect_interval_secs,
                node.peer_reconnect_interval_secs,
            );
            apply(
                &mut self.node.announce_initial_delay_secs,
                node.announce_initial_delay_secs,
            );
            apply(
                &mut self.node.announce_refresh_interval_secs,
                node.announce_refresh_interval_secs,
            );
        }
        if let Some(chain) = &toml.chain {
            if chain.indexer_url.is_some() {
                self.chain.indexer_url = chain.indexer_url.clone();
            }
            if chain.proxy_endpoint.is_some() {
                self.chain.proxy_endpoint = chain.proxy_endpoint.clone();
            }
            apply(
                &mut self.chain.indexer_timeout_secs,
                chain.indexer_timeout_secs,
            );
            apply(
                &mut self.chain.fee_refresh_interval_secs,
                chain.fee_refresh_interval_secs,
            );
        }
        if let Some(gossip) = &toml.gossip {
            apply(
                &mut self.gossip.rgs_sync_interval_secs,
                gossip.rgs_sync_interval_secs,
            );
            apply(
                &mut self.gossip.rgs_snapshot_max_size_mb,
                gossip.rgs_snapshot_max_size_mb,
            );
            apply(
                &mut self.gossip.rgs_connect_timeout_secs,
                gossip.rgs_connect_timeout_secs,
            );
            apply(
                &mut self.gossip.rgs_sync_timeout_secs,
                gossip.rgs_sync_timeout_secs,
            );
        }
        if let Some(vss) = &toml.vss {
            apply(&mut self.vss.retry_backoff_ms, vss.retry_backoff_ms);
            apply(&mut self.vss.retry_max_attempts, vss.retry_max_attempts);
            apply(
                &mut self.vss.retry_max_total_delay_secs,
                vss.retry_max_total_delay_secs,
            );
        }
        if let Some(lsp) = &toml.lsp {
            apply(
                &mut self.lsp.order_response_timeout_secs,
                lsp.order_response_timeout_secs,
            );
            apply(&mut self.lsp.request_timeout_secs, lsp.request_timeout_secs);
        }
        if let Some(rgb) = &toml.rgb {
            apply(&mut self.rgb.fee_rate_sat_vb, rgb.fee_rate_sat_vb);
            apply(&mut self.rgb.utxo_size_sat, rgb.utxo_size_sat);
            apply(&mut self.rgb.utxo_num, rgb.utxo_num);
            apply(
                &mut self.rgb.min_channel_confirmations,
                rgb.min_channel_confirmations,
            );
        }
        if let Some(channels) = &toml.channels {
            apply(&mut self.channels.htlc_min_msat, channels.htlc_min_msat);
            apply(
                &mut self.channels.virtual_htlc_min_msat,
                channels.virtual_htlc_min_msat,
            );
            apply(&mut self.channels.dust_limit_msat, channels.dust_limit_msat);
            apply(&mut self.channels.open_min_sat, channels.open_min_sat);
            apply(&mut self.channels.open_max_sat, channels.open_max_sat);
            apply(
                &mut self.channels.open_min_rgb_amount,
                channels.open_min_rgb_amount,
            );
            apply(
                &mut self.channels.their_to_self_delay,
                channels.their_to_self_delay,
            );
            apply(
                &mut self.channels.accept_forwards_to_priv_channels,
                channels.accept_forwards_to_priv_channels,
            );
            apply(
                &mut self.channels.cltv_expiry_delta,
                channels.cltv_expiry_delta,
            );
            apply(
                &mut self.channels.our_to_self_delay,
                channels.our_to_self_delay,
            );
            apply(
                &mut self.channels.forwarding_fee_base_msat,
                channels.forwarding_fee_base_msat,
            );
            apply(
                &mut self.channels.forwarding_fee_proportional_millionths,
                channels.forwarding_fee_proportional_millionths,
            );
            if let Some(multiplier) = channels.max_dust_htlc_exposure_multiplier {
                self.channels.max_dust_htlc_exposure = DustExposure::FeeRateMultiplier(multiplier);
            }
            if let Some(fixed_msat) = channels.max_dust_htlc_exposure_fixed_msat {
                self.channels.max_dust_htlc_exposure = DustExposure::FixedLimitMsat(fixed_msat);
            }
            apply(
                &mut self.channels.max_inbound_htlc_value_in_flight_percent,
                channels.max_inbound_htlc_value_in_flight_percent,
            );
            apply(
                &mut self.channels.our_max_accepted_htlcs,
                channels.our_max_accepted_htlcs,
            );
            apply(
                &mut self.channels.max_minimum_depth,
                channels.max_minimum_depth,
            );
        }
        if let Some(payments) = &toml.payments {
            apply(
                &mut self.payments.final_cltv_expiry_delta,
                payments.final_cltv_expiry_delta,
            );
            apply(
                &mut self.payments.retry_timeout_secs,
                payments.retry_timeout_secs,
            );
            apply(
                &mut self.payments.max_swap_fee_msat,
                payments.max_swap_fee_msat,
            );
        }
        if let Some(auth) = &toml.auth {
            apply(&mut self.auth.password_min_length, auth.password_min_length);
        }
        if let Some(api) = &toml.api {
            apply(&mut self.api.default_page_size, api.default_page_size);
        }
    }

    pub(crate) fn validate(&self) -> Result<(), AppError> {
        if let Some(alias) = &self.node.announce_alias {
            if alias.len() > MAX_ALIAS_BYTES {
                return Err(invalid(format!(
                    "announce_alias cannot be longer than {MAX_ALIAS_BYTES} bytes"
                )));
            }
        }
        nonzero(self.rgb.fee_rate_sat_vb, "rgb.fee_rate_sat_vb")?;
        nonzero(self.rgb.utxo_size_sat as u64, "rgb.utxo_size_sat")?;
        nonzero(self.rgb.utxo_num as u64, "rgb.utxo_num")?;
        nonzero(
            self.rgb.min_channel_confirmations as u64,
            "rgb.min_channel_confirmations",
        )?;
        nonzero(self.channels.htlc_min_msat, "channels.htlc_min_msat")?;
        nonzero(
            self.channels.virtual_htlc_min_msat,
            "channels.virtual_htlc_min_msat",
        )?;
        nonzero(self.channels.dust_limit_msat, "channels.dust_limit_msat")?;
        nonzero(self.channels.open_min_sat, "channels.open_min_sat")?;
        nonzero(
            self.channels.open_min_rgb_amount,
            "channels.open_min_rgb_amount",
        )?;
        if self.channels.open_max_sat > MAX_CHANNEL_SAT {
            return Err(invalid(format!(
                "channels.open_max_sat cannot exceed the protocol limit of {MAX_CHANNEL_SAT} sats"
            )));
        }
        if self.channels.open_min_sat > self.channels.open_max_sat {
            return Err(invalid(format!(
                "channels.open_min_sat ({}) cannot exceed channels.open_max_sat ({})",
                self.channels.open_min_sat, self.channels.open_max_sat
            )));
        }
        if self.channels.open_rgb_min_sat() > self.channels.open_max_sat {
            return Err(invalid(format!(
                "the RGB channel minimum derived from channels.htlc_min_msat ({} sats) \
                 cannot exceed channels.open_max_sat ({})",
                self.channels.open_rgb_min_sat(),
                self.channels.open_max_sat
            )));
        }
        if self.channels.their_to_self_delay == 0
            || self.channels.their_to_self_delay > MAX_TO_SELF_DELAY
        {
            return Err(invalid(format!(
                "channels.their_to_self_delay must be between 1 and {MAX_TO_SELF_DELAY}"
            )));
        }
        if self.channels.cltv_expiry_delta < MIN_CLTV_EXPIRY_DELTA {
            return Err(invalid(format!(
                "channels.cltv_expiry_delta must be at least {MIN_CLTV_EXPIRY_DELTA}"
            )));
        }
        if self.channels.our_to_self_delay < BREAKDOWN_TIMEOUT
            || self.channels.our_to_self_delay > MAX_TO_SELF_DELAY
        {
            return Err(invalid(format!(
                "channels.our_to_self_delay must be between {BREAKDOWN_TIMEOUT} and {MAX_TO_SELF_DELAY}"
            )));
        }
        match self.channels.max_dust_htlc_exposure {
            DustExposure::FeeRateMultiplier(0) => {
                return Err(invalid(
                    "channels.max_dust_htlc_exposure_multiplier must be greater than 0".to_string(),
                ));
            }
            DustExposure::FixedLimitMsat(0) => {
                return Err(invalid(
                    "channels.max_dust_htlc_exposure_fixed_msat must be greater than 0".to_string(),
                ));
            }
            _ => {}
        }
        if self.channels.max_inbound_htlc_value_in_flight_percent == 0
            || self.channels.max_inbound_htlc_value_in_flight_percent > 100
        {
            return Err(invalid(
                "channels.max_inbound_htlc_value_in_flight_percent must be between 1 and 100"
                    .to_string(),
            ));
        }
        if self.channels.our_max_accepted_htlcs == 0
            || self.channels.our_max_accepted_htlcs > MAX_ACCEPTED_HTLCS_CAP
        {
            return Err(invalid(format!(
                "channels.our_max_accepted_htlcs must be between 1 and {MAX_ACCEPTED_HTLCS_CAP}"
            )));
        }
        nonzero(
            self.channels.max_minimum_depth as u64,
            "channels.max_minimum_depth",
        )?;
        nonzero(
            self.payments.final_cltv_expiry_delta as u64,
            "payments.final_cltv_expiry_delta",
        )?;
        nonzero(
            self.payments.retry_timeout_secs,
            "payments.retry_timeout_secs",
        )?;
        nonzero(
            self.payments.max_swap_fee_msat,
            "payments.max_swap_fee_msat",
        )?;
        nonzero(
            self.auth.password_min_length as u64,
            "auth.password_min_length",
        )?;
        nonzero(self.api.default_page_size, "api.default_page_size")?;
        nonzero(
            self.node.peer_reconnect_interval_secs,
            "node.peer_reconnect_interval_secs",
        )?;
        nonzero(
            self.node.announce_refresh_interval_secs,
            "node.announce_refresh_interval_secs",
        )?;
        nonzero(
            self.chain.indexer_timeout_secs,
            "chain.indexer_timeout_secs",
        )?;
        nonzero(
            self.chain.fee_refresh_interval_secs,
            "chain.fee_refresh_interval_secs",
        )?;
        nonzero(
            self.gossip.rgs_sync_interval_secs,
            "gossip.rgs_sync_interval_secs",
        )?;
        nonzero(
            self.gossip.rgs_snapshot_max_size_mb,
            "gossip.rgs_snapshot_max_size_mb",
        )?;
        nonzero(
            self.gossip.rgs_connect_timeout_secs,
            "gossip.rgs_connect_timeout_secs",
        )?;
        nonzero(
            self.gossip.rgs_sync_timeout_secs,
            "gossip.rgs_sync_timeout_secs",
        )?;
        nonzero(self.vss.retry_backoff_ms, "vss.retry_backoff_ms")?;
        nonzero(self.vss.retry_max_attempts as u64, "vss.retry_max_attempts")?;
        nonzero(
            self.vss.retry_max_total_delay_secs,
            "vss.retry_max_total_delay_secs",
        )?;
        nonzero(
            self.lsp.order_response_timeout_secs,
            "lsp.order_response_timeout_secs",
        )?;
        nonzero(self.lsp.request_timeout_secs, "lsp.request_timeout_secs")?;
        Ok(())
    }
}

fn apply<T: Copy>(target: &mut T, value: Option<T>) {
    if let Some(value) = value {
        *target = value;
    }
}

fn nonzero(value: u64, key: &str) -> Result<(), AppError> {
    if value == 0 {
        return Err(invalid(format!("{key} must be greater than 0")));
    }
    Ok(())
}
