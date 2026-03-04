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

mod conversions;
mod types;

pub(crate) use types::*;

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

mod assets;
mod node;
mod payments;

pub(crate) use assets::{
    asset_balance, asset_metadata, get_asset_media, issue_asset_cfa, issue_asset_nia,
    issue_asset_uda, list_assets, post_asset_media, refresh_transfers, rgb_invoice, send_rgb,
};
pub(crate) use node::{
    address, backup, btc_balance, change_password, check_indexer_url, check_proxy_endpoint,
    close_channel, connect_peer, create_utxos, disconnect_peer, estimate_fee, fail_transfers,
    get_channel_id, init, list_channels, list_peers, list_transactions, list_transfers,
    list_unspents, lock, network_info, node_info, open_channel, restore, revoke_token, send_btc,
    send_onion_message, shutdown, sign_message, sync, unlock,
};
pub(crate) use payments::{
    decode_ln_invoice, decode_rgb_invoice, get_payment, get_swap, invoice_status, keysend,
    list_payments, list_swaps, ln_invoice, maker_execute, maker_init, send_payment, taker,
};
