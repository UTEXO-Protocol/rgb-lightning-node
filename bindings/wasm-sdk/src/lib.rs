#![allow(clippy::missing_const_for_thread_local)]
#![allow(clippy::await_holding_refcell_ref)]
#![allow(clippy::collapsible_if)]
#![allow(clippy::needless_question_mark)]
#![allow(clippy::new_without_default)]
#![allow(clippy::question_mark)]
#![allow(clippy::too_many_arguments)]
#![allow(clippy::type_complexity)]

use bitcoin_hashes::sha256::Hash as Sha256;
use bitcoin_hashes::Hash as _;
use secp256k1::PublicKey as SecpPublicKey;
use serde::{Deserialize, Serialize};
use serde_wasm_bindgen::to_value as to_js_value;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use wasm_bindgen::prelude::*;

mod browser_kv_store;
mod chain_sync;
mod ldk_event_applier;
#[path = "ldk_live_backend.rs"]
mod ldk_live_backend;
mod ldk_runtime;
mod ln_node;
mod ln_runtime_native;
mod ln_transport;
mod onion_runtime;
mod peer_session;
pub mod prelude;
mod rgb_ln_wire;
mod runtime_store;
mod sdk_facade;
mod swap_runtime;
#[cfg(test)]
#[path = "tests/test_utils.rs"]
mod test_utils;
mod wasm_node_persistence;
mod wasm_runtime_paths;
// Prefer `crate::prelude::*` for downstream imports. Keep module paths stable for now.

pub use sdk_facade::*;
// Re-export the WASM SDK internals for tests and advanced consumers. The public JS surface is
// defined by wasm-bindgen annotations, but Rust unit/wasm-bindgen tests expect these symbols
// to be available via `crate::*`.
pub use ldk_live_backend::*;
pub use ldk_runtime::*;
pub use ln_node::*;
pub use ln_runtime_native::*;
pub use ln_transport::*;
pub use peer_session::*;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RlnWasmSdkRuntimeCapabilitiesData {
    pub wallet_runtime: bool,
    pub node_runtime: bool,
    pub ldk_runtime_scaffold: bool,
    pub callback_status_updates: bool,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WasmRlnNetwork {
    Mainnet,
    Testnet,
    Testnet4,
    Signet,
    Regtest,
}

impl WasmRlnNetwork {
    fn parse(value: &str) -> Result<Self, JsValue> {
        match value.to_ascii_lowercase().as_str() {
            "mainnet" => Ok(Self::Mainnet),
            "testnet" => Ok(Self::Testnet),
            "testnet4" => Ok(Self::Testnet4),
            "signet" => Ok(Self::Signet),
            "regtest" => Ok(Self::Regtest),
            other => Err(JsValue::from_str(&format!("unsupported network: {other}"))),
        }
    }

    fn as_rgb(self) -> rgb_lib_wasm::BitcoinNetwork {
        match self {
            Self::Mainnet => rgb_lib_wasm::BitcoinNetwork::Mainnet,
            Self::Testnet => rgb_lib_wasm::BitcoinNetwork::Testnet,
            Self::Testnet4 => rgb_lib_wasm::BitcoinNetwork::Testnet4,
            Self::Signet => rgb_lib_wasm::BitcoinNetwork::Signet,
            Self::Regtest => rgb_lib_wasm::BitcoinNetwork::Regtest,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RlnRgbKeysData {
    pub mnemonic: String,
    pub xpub: String,
    pub account_xpub_vanilla: String,
    pub account_xpub_colored: String,
    pub master_fingerprint: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RlnWasmInitData {
    pub mnemonic: String,
}

#[derive(Clone, Debug, Deserialize)]
struct WasmUnlockRequest {
    password: String,
}

#[derive(Clone, Debug, Default)]
enum WasmSdkLifecycleState {
    #[default]
    Uninitialized,
    InitializedLocked {
        password: String,
        mnemonic: String,
    },
    Unlocked {
        password: String,
        mnemonic: String,
    },
}

impl WasmSdkLifecycleState {
    fn initialized(&self) -> bool {
        !matches!(self, Self::Uninitialized)
    }

    fn unlocked(&self) -> bool {
        matches!(self, Self::Unlocked { .. })
    }

    fn password(&self) -> Option<&str> {
        match self {
            Self::Uninitialized => None,
            Self::InitializedLocked { password, .. } | Self::Unlocked { password, .. } => {
                Some(password.as_str())
            }
        }
    }

    fn mnemonic(&self) -> Option<&str> {
        match self {
            Self::Uninitialized => None,
            Self::InitializedLocked { mnemonic, .. } | Self::Unlocked { mnemonic, .. } => {
                Some(mnemonic.as_str())
            }
        }
    }
}

thread_local! {
    static WASM_SDK_LIFECYCLE_STATE: RefCell<WasmSdkLifecycleState> =
        const { RefCell::new(WasmSdkLifecycleState::Uninitialized) };
    static WASM_MEDIA_STORE: RefCell<HashMap<String, WasmMediaStoreEntry>> =
        RefCell::new(HashMap::new());
    static WASM_SDK_DEFAULT_WALLET: RefCell<Option<Rc<RefCell<rgb_lib_wasm::Wallet>>>> =
        const { RefCell::new(None) };
    static WASM_SDK_DEFAULT_RGB_PROXY_TRANSPORT: RefCell<Option<RlnWasmRgbProxyTransportConfigData>> =
        const { RefCell::new(None) };
    static WASM_WALLET_RGB_PROXY_TRANSPORTS: RefCell<HashMap<String, RlnWasmRgbProxyTransportConfigData>> =
        RefCell::new(HashMap::new());
    static WASM_SDK_DEFAULT_ENABLE_VIRTUAL_CHANNELS_V0: RefCell<bool> =
        const { RefCell::new(false) };
}

const MEDIA_STORAGE_PREFIX: &str = "rln:wasm:media:";
const WALLET_RGB_PROXY_STORAGE_PREFIX: &str = "rln:wasm:wallet-rgb-proxy:";

pub(crate) fn ensure_sdk_node_runtime_allowed() -> Result<(), JsValue> {
    WASM_SDK_LIFECYCLE_STATE.with(|state| {
        let state = state.borrow();
        if state.initialized() && !state.unlocked() {
            return Err(JsValue::from_str(
                "sdk node runtime is locked; call unlock first",
            ));
        }
        Ok(())
    })
}

pub(crate) fn sdk_node_identity_seed() -> Option<String> {
    WASM_SDK_LIFECYCLE_STATE.with(|state| {
        let state = state.borrow();
        if !state.initialized() {
            return None;
        }
        state.mnemonic().map(|v| v.to_string())
    })
}

pub(crate) fn sdk_default_enable_virtual_channels_v0() -> bool {
    WASM_SDK_DEFAULT_ENABLE_VIRTUAL_CHANNELS_V0.with(|value| *value.borrow())
}

pub(crate) fn set_sdk_default_enable_virtual_channels_v0(enabled: bool) {
    WASM_SDK_DEFAULT_ENABLE_VIRTUAL_CHANNELS_V0.with(|value| {
        *value.borrow_mut() = enabled;
    });
}

fn sync_runtime_session_authority_from_lifecycle(state: &WasmSdkLifecycleState) {
    crate::ldk_runtime::set_runtime_session_initialized(state.initialized());
    crate::ldk_runtime::set_runtime_session_authorized(state.unlocked());
}

fn build_auto_wallet_data_json_from_mnemonic(mnemonic: &str) -> Result<String, JsValue> {
    use rgb_lib_wasm::wallet::{DatabaseType, WalletData};
    let keys =
        rgb_lib_wasm::restore_keys(rgb_lib_wasm::BitcoinNetwork::Regtest, mnemonic.to_string())
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
    let fingerprint = keys.master_fingerprint.clone();
    let wallet_data = WalletData {
        data_dir: crate::wasm_runtime_paths::rgb_lib_wallet_data_dir(&fingerprint),
        bitcoin_network: rgb_lib_wasm::BitcoinNetwork::Regtest,
        database_type: DatabaseType::Sqlite,
        max_allocations_per_utxo: 5,
        account_xpub_vanilla: keys.account_xpub_vanilla,
        account_xpub_colored: keys.account_xpub_colored,
        mnemonic: Some(keys.mnemonic),
        master_fingerprint: keys.master_fingerprint,
        vanilla_keychain: None,
        reuse_addresses: false,
        supported_schemas: vec![
            rgb_lib_wasm::AssetSchema::Nia,
            rgb_lib_wasm::AssetSchema::Ifa,
        ],
    };
    serde_json::to_string(&wallet_data).map_err(|e| JsValue::from_str(&e.to_string()))
}

async fn bootstrap_default_wallet_from_lifecycle() -> Result<(), JsValue> {
    let mnemonic = WASM_SDK_LIFECYCLE_STATE.with(|state| {
        let state = state.borrow();
        if !state.initialized() {
            return Err(JsValue::from_str(sdk_contracts::ERR_SDK_NOT_INITIALIZED));
        }
        state
            .mnemonic()
            .map(|v| v.to_string())
            .ok_or_else(|| JsValue::from_str(sdk_contracts::ERR_SDK_LIFECYCLE_INCONSISTENT))
    })?;
    let wallet_data_json = build_auto_wallet_data_json_from_mnemonic(&mnemonic)?;
    let wallet = RlnWasmWallet::create(&wallet_data_json).await?;
    apply_default_rgb_proxy_transport_to_wallet(&wallet)?;
    WASM_SDK_DEFAULT_WALLET.with(|slot| {
        *slot.borrow_mut() = Some(Rc::clone(&wallet.inner));
    });
    Ok(())
}

fn maybe_attach_default_wallet_to_node(node: &RlnWasmNode) {
    WASM_SDK_DEFAULT_WALLET.with(|slot| {
        if let Some(wallet) = slot.borrow().as_ref() {
            node.attach_wallet_shared(Rc::clone(wallet));
        }
    });
}

pub(crate) fn try_attach_default_wallet_to_node(node: &RlnWasmNode) {
    maybe_attach_default_wallet_to_node(node);
}

fn sdk_default_rgb_proxy_transport() -> Option<RlnWasmRgbProxyTransportConfigData> {
    WASM_SDK_DEFAULT_RGB_PROXY_TRANSPORT.with(|slot| slot.borrow().as_ref().cloned())
}

fn apply_default_rgb_proxy_transport_to_wallet(wallet: &RlnWasmWallet) -> Result<(), JsValue> {
    let Some(config) = sdk_default_rgb_proxy_transport() else {
        return Ok(());
    };
    wallet.set_rgb_proxy_transport(config.endpoint, config.auth_token, config.node_id)
}

impl From<rgb_lib_wasm::keys::Keys> for RlnRgbKeysData {
    fn from(value: rgb_lib_wasm::keys::Keys) -> Self {
        Self {
            mnemonic: value.mnemonic,
            xpub: value.xpub,
            account_xpub_vanilla: value.account_xpub_vanilla,
            account_xpub_colored: value.account_xpub_colored,
            master_fingerprint: value.master_fingerprint,
        }
    }
}

pub(crate) fn js_obj<T: Serialize>(value: &T) -> Result<JsValue, JsValue> {
    to_js_value(value).map_err(|e| JsValue::from_str(&e.to_string()))
}

pub(crate) fn js_from<T: for<'de> Deserialize<'de>>(value: JsValue) -> Result<T, JsValue> {
    serde_wasm_bindgen::from_value(value).map_err(|e| JsValue::from_str(&e.to_string()))
}

pub(crate) fn js_to_json<T: Serialize>(value: &T) -> Result<String, JsValue> {
    serde_json::to_string(value).map_err(|e| JsValue::from_str(&e.to_string()))
}

fn parse_online(value: JsValue) -> Result<rgb_lib_wasm::wallet::Online, JsValue> {
    if let Ok(online) =
        serde_wasm_bindgen::from_value::<rgb_lib_wasm::wallet::Online>(value.clone())
    {
        return Ok(online);
    }
    let wasm_online: WasmOnlineData = serde_wasm_bindgen::from_value(value)
        .map_err(|e| JsValue::from_str(&format!("Invalid Online object: {e}")))?;
    let id = wasm_online
        .id
        .parse::<u64>()
        .map_err(|_| JsValue::from_str(sdk_contracts::ERR_ONLINE_ID_INVALID))?;
    Ok(rgb_lib_wasm::wallet::Online {
        id,
        indexer_url: wasm_online.indexer_url,
    })
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct WasmOnlineData {
    id: String,
    indexer_url: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LnPeerWebsocketCheckData {
    pub proxy_url: String,
    pub peer_addr: String,
    pub websocket_url: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CheckIndexerUrlData {
    pub indexer_protocol: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WasmIssueAssetNiaRequest {
    pub amounts: Vec<u64>,
    pub ticker: String,
    pub name: String,
    pub precision: u8,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WasmIssueAssetCfaRequest {
    pub amounts: Vec<u64>,
    pub name: String,
    pub details: Option<String>,
    pub precision: u8,
    pub file_digest: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WasmIssueAssetIfaRequest {
    pub amounts: Vec<u64>,
    #[serde(default)]
    pub inflation_amounts: Vec<u64>,
    pub ticker: String,
    pub name: String,
    pub precision: u8,
    #[serde(default)]
    pub reject_list_url: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WasmIssueAssetUdaRequest {
    pub ticker: String,
    pub name: String,
    pub details: Option<String>,
    pub precision: u8,
    pub media_file_digest: Option<String>,
    pub attachments_file_digests: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WasmSendRgbAssetRecipientsInput {
    pub asset_id: String,
    pub recipients: Vec<rgb_lib_wasm::wallet::Recipient>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WasmSendRgbFromGroupsRequest {
    pub online: rgb_lib_wasm::wallet::Online,
    pub donation: bool,
    pub fee_rate: u64,
    pub min_confirmations: u8,
    pub recipient_groups: Vec<WasmSendRgbAssetRecipientsInput>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WasmSendRgbFromGroupsData {
    pub unsigned_psbt: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RlnWasmRgbProxyTransportConfigData {
    pub endpoint: String,
    pub auth_token: Option<String>,
    pub node_id: Option<String>,
}

fn recipient_map_from_groups(
    recipient_groups: Vec<WasmSendRgbAssetRecipientsInput>,
) -> Result<HashMap<String, Vec<rgb_lib_wasm::wallet::Recipient>>, JsValue> {
    if recipient_groups.is_empty() {
        return Err(JsValue::from_str(sdk_contracts::ERR_RECIPIENT_GROUPS_EMPTY));
    }
    let mut recipient_map: HashMap<String, Vec<rgb_lib_wasm::wallet::Recipient>> = HashMap::new();
    for group in recipient_groups {
        if group.asset_id.trim().is_empty() {
            return Err(JsValue::from_str(sdk_contracts::ERR_ASSET_ID_EMPTY));
        }
        if group.recipients.is_empty() {
            return Err(JsValue::from_str(
                "recipient group recipients cannot be empty",
            ));
        }
        recipient_map.insert(group.asset_id, group.recipients);
    }
    Ok(recipient_map)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WasmAssetCfaData {
    pub asset_id: String,
    pub name: String,
    pub details: Option<String>,
    pub precision: u8,
    pub issued_supply: u64,
    pub timestamp: i64,
    pub added_at: i64,
    pub balance: rgb_lib_wasm::wallet::Balance,
    pub media: Option<rgb_lib_wasm::wallet::Media>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WasmPostAssetMediaData {
    pub digest: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WasmAssetMediaData {
    pub bytes_hex: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct WasmMediaStoreEntry {
    bytes_hex: String,
    mime: String,
}

fn media_storage_key(digest: &str) -> String {
    format!("{MEDIA_STORAGE_PREFIX}{digest}")
}

fn wallet_rgb_proxy_storage_key(idb_key: &str) -> String {
    format!("{WALLET_RGB_PROXY_STORAGE_PREFIX}{idb_key}")
}

fn media_store_insert(digest: &str, entry: &WasmMediaStoreEntry) {
    WASM_MEDIA_STORE.with(|store| {
        store.borrow_mut().insert(digest.to_string(), entry.clone());
    });
    if let Ok(raw) = serde_json::to_string(entry) {
        let _ = local_storage_set_item(&media_storage_key(digest), &raw);
    }
}

fn media_store_get(digest: &str) -> Option<WasmMediaStoreEntry> {
    if let Some(entry) = WASM_MEDIA_STORE.with(|store| store.borrow().get(digest).cloned()) {
        return Some(entry);
    }
    let raw = local_storage_get_item(&media_storage_key(digest))
        .ok()
        .flatten()?;
    let entry = serde_json::from_str::<WasmMediaStoreEntry>(&raw).ok()?;
    WASM_MEDIA_STORE.with(|store| {
        store.borrow_mut().insert(digest.to_string(), entry.clone());
    });
    Some(entry)
}

fn wallet_rgb_proxy_transport_insert(
    idb_key: &str,
    config: &RlnWasmRgbProxyTransportConfigData,
) -> Result<(), JsValue> {
    WASM_WALLET_RGB_PROXY_TRANSPORTS.with(|store| {
        store
            .borrow_mut()
            .insert(idb_key.to_string(), config.clone());
    });
    let raw = serde_json::to_string(config).map_err(|e| JsValue::from_str(&e.to_string()))?;
    local_storage_set_item(&wallet_rgb_proxy_storage_key(idb_key), &raw)
}

fn wallet_rgb_proxy_transport_remove(idb_key: &str) {
    WASM_WALLET_RGB_PROXY_TRANSPORTS.with(|store| {
        store.borrow_mut().remove(idb_key);
    });
    let _ = local_storage_remove_item(&wallet_rgb_proxy_storage_key(idb_key));
}

fn wallet_rgb_proxy_transport_get(idb_key: &str) -> Option<RlnWasmRgbProxyTransportConfigData> {
    if let Some(config) =
        WASM_WALLET_RGB_PROXY_TRANSPORTS.with(|store| store.borrow().get(idb_key).cloned())
    {
        return Some(config);
    }

    let raw = local_storage_get_item(&wallet_rgb_proxy_storage_key(idb_key))
        .ok()
        .flatten()?;
    let config = serde_json::from_str::<RlnWasmRgbProxyTransportConfigData>(&raw).ok()?;
    WASM_WALLET_RGB_PROXY_TRANSPORTS.with(|store| {
        store
            .borrow_mut()
            .insert(idb_key.to_string(), config.clone());
    });
    Some(config)
}

#[cfg(target_arch = "wasm32")]
fn local_storage_get_item(key: &str) -> Result<Option<String>, JsValue> {
    let Some(window) = web_sys::window() else {
        return Ok(None);
    };
    let Some(storage) = window.local_storage()? else {
        return Ok(None);
    };
    storage.get_item(key)
}

#[cfg(not(target_arch = "wasm32"))]
fn local_storage_get_item(_key: &str) -> Result<Option<String>, JsValue> {
    Ok(None)
}

#[cfg(target_arch = "wasm32")]
fn local_storage_set_item(key: &str, value: &str) -> Result<(), JsValue> {
    let Some(window) = web_sys::window() else {
        return Ok(());
    };
    let Some(storage) = window.local_storage()? else {
        return Ok(());
    };
    storage.set_item(key, value)
}

#[cfg(not(target_arch = "wasm32"))]
fn local_storage_set_item(_key: &str, _value: &str) -> Result<(), JsValue> {
    Ok(())
}

#[cfg(target_arch = "wasm32")]
fn local_storage_remove_item(key: &str) -> Result<(), JsValue> {
    let Some(window) = web_sys::window() else {
        return Ok(());
    };
    let Some(storage) = window.local_storage()? else {
        return Ok(());
    };
    storage.remove_item(key)
}

#[cfg(not(target_arch = "wasm32"))]
fn local_storage_remove_item(_key: &str) -> Result<(), JsValue> {
    Ok(())
}

fn normalize_media_digest(input: &str) -> Result<String, JsValue> {
    let digest = input.trim().to_ascii_lowercase();
    if digest.len() != 64 || !digest.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(JsValue::from_str(sdk_contracts::ERR_MEDIA_DIGEST_INVALID));
    }
    Ok(digest)
}

pub(crate) fn derive_cfa_ticker(name: &str) -> String {
    let mut ticker: String = name
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_uppercase())
        .take(8)
        .collect();
    if ticker.is_empty() {
        ticker = "CFA".to_string();
    }
    ticker
}

pub(crate) fn proxy_url_for_peer(proxy_url: &str, peer_addr: &str) -> Result<String, JsValue> {
    let trimmed_proxy_url = proxy_url.trim();
    if trimmed_proxy_url.is_empty() {
        return Err(JsValue::from_str(sdk_contracts::ERR_PROXY_URL_EMPTY));
    }

    let trimmed_peer_addr = peer_addr.trim();
    if trimmed_peer_addr.is_empty() {
        return Err(JsValue::from_str(sdk_contracts::ERR_PEER_ADDR_EMPTY));
    }

    let (host, port) = trimmed_peer_addr
        .rsplit_once(':')
        .ok_or_else(|| JsValue::from_str(sdk_contracts::ERR_PEER_ADDR_HOST_PORT))?;
    if host.trim().is_empty() || port.trim().is_empty() {
        return Err(JsValue::from_str(sdk_contracts::ERR_PEER_ADDR_HOST_PORT));
    }
    if !port.chars().all(|c| c.is_ascii_digit()) {
        return Err(JsValue::from_str(sdk_contracts::ERR_PEER_ADDR_PORT_NUMERIC));
    }

    let host_slug = host
        .trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .replace(['.', ':'], "_");
    if host_slug.is_empty() {
        return Err(JsValue::from_str(sdk_contracts::ERR_PEER_ADDR_HOST_EMPTY));
    }

    // Allow using a `#runtime:<id>` fragment for persistence scoping, but fragments are not valid
    // in WebSocket URLs. Strip it for the actual transport URL.
    let proxy_base = trimmed_proxy_url
        .split_once('#')
        .map(|(base, _frag)| base)
        .unwrap_or(trimmed_proxy_url)
        .trim_end_matches('/');
    Ok(format!("{proxy_base}/v1/{host_slug}/{port}"))
}

fn detect_wasm_indexer_protocol(indexer_url: &str) -> Result<&'static str, JsValue> {
    let trimmed = indexer_url.trim();
    if trimmed.is_empty() {
        return Err(JsValue::from_str(sdk_contracts::ERR_INDEXER_URL_EMPTY));
    }

    let lower = trimmed.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") {
        let (_, remainder) = trimmed
            .split_once("://")
            .ok_or_else(|| JsValue::from_str("invalid indexer_url format"))?;
        if remainder.trim().is_empty() {
            return Err(JsValue::from_str("invalid indexer_url format"));
        }
        let host_candidate = remainder.split(['/', '?', '#']).next().unwrap_or("").trim();
        if host_candidate.is_empty()
            || host_candidate.contains('@')
            || host_candidate.chars().any(char::is_whitespace)
        {
            return Err(JsValue::from_str("invalid indexer_url format"));
        }
        return Ok("esplora");
    }

    if lower.starts_with("tcp://") || lower.starts_with("ssl://") {
        return Err(JsValue::from_str(
            "electrum indexer URLs are not supported in wasm build",
        ));
    }

    Err(JsValue::from_str(
        "unsupported indexer_url scheme, expected http:// or https://",
    ))
}

// Moved: wasm-bindgen entrypoints and JS-facing structs live in `sdk_facade.rs`.

#[cfg(test)]
impl RlnWasmSdk {
    pub fn new_node_with_runtime_backend(
        &self,
        proxy_url: String,
        runtime_backend: String,
    ) -> Result<RlnWasmNode, JsValue> {
        if runtime_backend.trim() != "wasm_native_ldk" {
            return Err(JsValue::from_str(&format!(
                "unknown runtime backend: {}",
                runtime_backend.trim()
            )));
        }
        self.new_node(proxy_url)
    }

    pub fn new_node_with_runtime_backend_and_runtime_id(
        &self,
        proxy_url: String,
        runtime_backend: String,
        node_runtime_id: String,
    ) -> Result<RlnWasmNode, JsValue> {
        if runtime_backend.trim() != "wasm_native_ldk" {
            return Err(JsValue::from_str(&format!(
                "unknown runtime backend: {}",
                runtime_backend.trim()
            )));
        }
        self.new_node_with_runtime_id(proxy_url, node_runtime_id)
    }

    pub fn create_node_handle_with_runtime_backend(
        &self,
        proxy_url: String,
        runtime_backend: String,
    ) -> Result<RlnWasmSdkNodeHandle, JsValue> {
        if runtime_backend.trim() != "wasm_native_ldk" {
            return Err(JsValue::from_str(&format!(
                "unknown runtime backend: {}",
                runtime_backend.trim()
            )));
        }
        self.create_node_handle(proxy_url)
    }

    pub fn create_node_handle_with_runtime_backend_and_runtime_id(
        &self,
        proxy_url: String,
        runtime_backend: String,
        node_runtime_id: String,
    ) -> Result<RlnWasmSdkNodeHandle, JsValue> {
        if runtime_backend.trim() != "wasm_native_ldk" {
            return Err(JsValue::from_str(&format!(
                "unknown runtime backend: {}",
                runtime_backend.trim()
            )));
        }
        self.create_node_handle_with_runtime_id(proxy_url, node_runtime_id)
    }
}

// Moved: `RlnWasmWallet` wasm-bindgen surface lives in `sdk_facade.rs`.

// Moved: `RlnWasmInvoice`, `checkProxyUrl`, `checkIndexerUrl*`, `checkLnPeerWebsocket*`
// live in `sdk_facade.rs`.

#[cfg(all(test, target_arch = "wasm32"))]
#[path = "tests/sdk_contract_tests.rs"]
mod sdk_contract_tests;
#[cfg(all(test, target_arch = "wasm32"))]
#[path = "tests/wallet_contract_tests.rs"]
mod wallet_contract_tests;
#[cfg(all(test, target_arch = "wasm32"))]
#[path = "tests/wasm_contract_tests.rs"]
mod wasm_contract_tests;
