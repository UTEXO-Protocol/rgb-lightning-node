use bitcoin_hashes::sha256::Hash as Sha256;
use bitcoin_hashes::Hash as _;
use gloo_net::websocket::futures::WebSocket;
use secp256k1::PublicKey as SecpPublicKey;
use serde::{Deserialize, Serialize};
use serde_wasm_bindgen::to_value as to_js_value;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::str::FromStr;
use std::sync::Once;
use wasm_bindgen::prelude::*;

mod chain_sync;
mod ldk_event_applier;
mod ldk_runtime;
mod ln_node;
mod ln_runtime_native;
mod ln_transport;
mod onion_runtime;
mod peer_session;
mod runtime_store;
mod swap_runtime;
#[cfg(test)]
#[path = "tests/test_utils.rs"]
mod test_utils;
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
struct WasmSdkLifecycleState {
    initialized: bool,
    unlocked: bool,
    password: Option<String>,
    mnemonic: Option<String>,
}

thread_local! {
    static WASM_SDK_LIFECYCLE_STATE: RefCell<WasmSdkLifecycleState> =
        RefCell::new(WasmSdkLifecycleState::default());
    static WASM_MEDIA_STORE: RefCell<HashMap<String, WasmMediaStoreEntry>> =
        RefCell::new(HashMap::new());
    static WASM_SDK_DEFAULT_WALLET: RefCell<Option<Rc<RefCell<rgb_lib_wasm::Wallet>>>> =
        RefCell::new(None);
    static WASM_SDK_DEFAULT_RGB_PROXY_TRANSPORT: RefCell<Option<RlnWasmRgbProxyTransportConfigData>> =
        RefCell::new(None);
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
        if state.initialized && !state.unlocked {
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
        if !state.initialized {
            return None;
        }
        state.mnemonic.clone()
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
    crate::ldk_runtime::set_runtime_session_initialized(state.initialized);
    crate::ldk_runtime::set_runtime_session_authorized(state.unlocked);
}

fn build_auto_wallet_data_json_from_mnemonic(mnemonic: &str) -> Result<String, JsValue> {
    use rgb_lib_wasm::wallet::{DatabaseType, WalletData};
    let keys =
        rgb_lib_wasm::restore_keys(rgb_lib_wasm::BitcoinNetwork::Regtest, mnemonic.to_string())
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
    let fingerprint = keys.master_fingerprint.clone();
    let wallet_data = WalletData {
        data_dir: format!("/tmp/rln_wasm_sdk_auto_{fingerprint}"),
        bitcoin_network: rgb_lib_wasm::BitcoinNetwork::Regtest,
        database_type: DatabaseType::Sqlite,
        max_allocations_per_utxo: 5,
        account_xpub_vanilla: keys.account_xpub_vanilla,
        account_xpub_colored: keys.account_xpub_colored,
        mnemonic: Some(keys.mnemonic),
        master_fingerprint: keys.master_fingerprint,
        vanilla_keychain: None,
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
        if !state.initialized {
            return Err(JsValue::from_str("sdk is not initialized"));
        }
        state
            .mnemonic
            .clone()
            .ok_or_else(|| JsValue::from_str("sdk lifecycle state is inconsistent"))
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
    let id = wasm_online.id.parse::<u64>().map_err(|_| {
        JsValue::from_str("Invalid Online object: id must be a u64 string or safe integer")
    })?;
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
        return Err(JsValue::from_str("recipient_groups cannot be empty"));
    }
    let mut recipient_map: HashMap<String, Vec<rgb_lib_wasm::wallet::Recipient>> = HashMap::new();
    for group in recipient_groups {
        if group.asset_id.trim().is_empty() {
            return Err(JsValue::from_str(
                "recipient group asset_id cannot be empty",
            ));
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
        return Err(JsValue::from_str("invalid media digest"));
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
        return Err(JsValue::from_str("proxy_url cannot be empty"));
    }

    let trimmed_peer_addr = peer_addr.trim();
    if trimmed_peer_addr.is_empty() {
        return Err(JsValue::from_str("peer_addr cannot be empty"));
    }

    let (host, port) = trimmed_peer_addr
        .rsplit_once(':')
        .ok_or_else(|| JsValue::from_str("peer_addr must be in host:port format"))?;
    if host.trim().is_empty() || port.trim().is_empty() {
        return Err(JsValue::from_str("peer_addr must be in host:port format"));
    }
    if !port.chars().all(|c| c.is_ascii_digit()) {
        return Err(JsValue::from_str("peer_addr port must be numeric"));
    }

    let host_slug = host
        .trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .replace('.', "_")
        .replace(':', "_");
    if host_slug.is_empty() {
        return Err(JsValue::from_str("peer_addr host cannot be empty"));
    }

    let proxy_base = trimmed_proxy_url.trim_end_matches('/');
    Ok(format!("{proxy_base}/v1/{host_slug}/{port}"))
}

fn detect_wasm_indexer_protocol(indexer_url: &str) -> Result<&'static str, JsValue> {
    let trimmed = indexer_url.trim();
    if trimmed.is_empty() {
        return Err(JsValue::from_str("indexer_url cannot be empty"));
    }

    let lower = trimmed.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") {
        let (_, remainder) = trimmed
            .split_once("://")
            .ok_or_else(|| JsValue::from_str("invalid indexer_url format"))?;
        if remainder.trim().is_empty() {
            return Err(JsValue::from_str("invalid indexer_url format"));
        }
        let host_candidate = remainder
            .split(|c| c == '/' || c == '?' || c == '#')
            .next()
            .unwrap_or("")
            .trim();
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

#[wasm_bindgen(start)]
pub fn wasm_init() {
    static PANIC_HOOK_INIT: Once = Once::new();
    PANIC_HOOK_INIT.call_once(|| {
        let install_result = std::panic::catch_unwind(|| {
            if std::thread::panicking() {
                return;
            }
            std::panic::set_hook(Box::new(|info| {
                let msg = format!("[rln-wasm-sdk panic] {info}");
                web_sys::console::error_1(&msg.into());
            }));
        });
        if install_result.is_err() {
            web_sys::console::warn_1(
                &"[rln-wasm-sdk] failed to install panic hook; continuing".into(),
            );
        }
    });
}

#[wasm_bindgen(js_name = rgbGenerateKeysJson)]
pub fn rgb_generate_keys_json(network: String) -> Result<String, JsValue> {
    let network = WasmRlnNetwork::parse(&network)?;
    let keys = rgb_lib_wasm::generate_keys(network.as_rgb());
    let data = RlnRgbKeysData::from(keys);
    serde_json::to_string(&data).map_err(|e| JsValue::from_str(&e.to_string()))
}

#[wasm_bindgen(js_name = rgbGenerateKeysValue)]
pub fn rgb_generate_keys_value(network: String) -> Result<JsValue, JsValue> {
    let network = WasmRlnNetwork::parse(&network)?;
    let keys = rgb_lib_wasm::generate_keys(network.as_rgb());
    let data = RlnRgbKeysData::from(keys);
    js_obj(&data)
}

#[wasm_bindgen(js_name = rgbRestoreKeysJson)]
pub fn rgb_restore_keys_json(network: String, mnemonic: String) -> Result<String, JsValue> {
    if mnemonic.trim().is_empty() {
        return Err(JsValue::from_str("mnemonic cannot be empty"));
    }
    let network = WasmRlnNetwork::parse(&network)?;
    let keys = rgb_lib_wasm::restore_keys(network.as_rgb(), mnemonic)
        .map_err(|e| JsValue::from_str(&e.to_string()))?;
    let data = RlnRgbKeysData::from(keys);
    serde_json::to_string(&data).map_err(|e| JsValue::from_str(&e.to_string()))
}

#[wasm_bindgen(js_name = rgbRestoreKeysValue)]
pub fn rgb_restore_keys_value(network: String, mnemonic: String) -> Result<JsValue, JsValue> {
    if mnemonic.trim().is_empty() {
        return Err(JsValue::from_str("mnemonic cannot be empty"));
    }
    let network = WasmRlnNetwork::parse(&network)?;
    let keys = rgb_lib_wasm::restore_keys(network.as_rgb(), mnemonic)
        .map_err(|e| JsValue::from_str(&e.to_string()))?;
    let data = RlnRgbKeysData::from(keys);
    js_obj(&data)
}

#[wasm_bindgen]
pub struct RlnWasmWallet {
    pub(crate) inner: std::rc::Rc<RefCell<rgb_lib_wasm::Wallet>>,
}

#[wasm_bindgen]
pub struct RlnWasmSdk;

#[wasm_bindgen]
pub struct RlnWasmSdkNodeHandle {
    inner: RlnWasmNode,
}

#[wasm_bindgen]
pub struct RlnWasmSdkWalletHandle {
    inner: RlnWasmWallet,
}

impl std::fmt::Debug for RlnWasmSdkNodeHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RlnWasmSdkNodeHandle")
            .finish_non_exhaustive()
    }
}

impl std::fmt::Debug for RlnWasmSdkWalletHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RlnWasmSdkWalletHandle")
            .finish_non_exhaustive()
    }
}

#[wasm_bindgen]
impl RlnWasmSdk {
    #[wasm_bindgen(constructor)]
    pub fn new() -> RlnWasmSdk {
        Self
    }

    #[wasm_bindgen(js_name = healthcheck)]
    pub fn healthcheck(&self) -> String {
        "rln_wasm_sdk_ready".to_string()
    }

    #[wasm_bindgen(js_name = version)]
    pub fn version(&self) -> String {
        env!("CARGO_PKG_VERSION").to_string()
    }

    #[wasm_bindgen(js_name = preloadPersistentRuntimeState)]
    pub async fn preload_persistent_runtime_state(&self) -> Result<(), JsValue> {
        runtime_store::preload_runtime_state_from_persistent_store().await
    }

    #[wasm_bindgen(js_name = initValue)]
    pub async fn init_value(
        &self,
        password: String,
        mnemonic: Option<String>,
    ) -> Result<JsValue, JsValue> {
        runtime_store::preload_runtime_state_from_persistent_store().await?;
        if password.trim().is_empty() {
            return Err(JsValue::from_str("password cannot be empty"));
        }
        if let Some(m) = &mnemonic {
            if m.trim().is_empty() {
                return Err(JsValue::from_str("mnemonic cannot be empty when provided"));
            }
        }
        let init_data = WASM_SDK_LIFECYCLE_STATE.with(|state| {
            let mut state = state.borrow_mut();
            if state.initialized {
                if state.password.as_deref() != Some(password.as_str()) {
                    return Err(JsValue::from_str(
                        "sdk is already initialized with different password",
                    ));
                }
                if let Some(mnemonic) = &mnemonic {
                    if state.mnemonic.as_deref() != Some(mnemonic.as_str()) {
                        return Err(JsValue::from_str(
                            "sdk is already initialized with different mnemonic",
                        ));
                    }
                }
                let existing = state
                    .mnemonic
                    .clone()
                    .ok_or_else(|| JsValue::from_str("sdk lifecycle state is inconsistent"))?;
                sync_runtime_session_authority_from_lifecycle(&state);
                return Ok(RlnWasmInitData { mnemonic: existing });
            }

            let resolved_mnemonic = mnemonic.unwrap_or_else(|| {
                rgb_lib_wasm::generate_keys(rgb_lib_wasm::BitcoinNetwork::Regtest).mnemonic
            });
            state.initialized = true;
            state.unlocked = false;
            state.password = Some(password);
            state.mnemonic = Some(resolved_mnemonic.clone());
            sync_runtime_session_authority_from_lifecycle(&state);
            Ok(RlnWasmInitData {
                mnemonic: resolved_mnemonic,
            })
        })?;
        js_obj(&init_data)
    }

    #[wasm_bindgen(js_name = initJson)]
    pub async fn init_json(
        &self,
        password: String,
        mnemonic: Option<String>,
    ) -> Result<String, JsValue> {
        let value = self.init_value(password, mnemonic).await?;
        let parsed: serde_json::Value = js_from(value)?;
        js_to_json(&parsed)
    }

    #[wasm_bindgen(js_name = unlock)]
    pub async fn unlock(&self, request_json: String) -> Result<(), JsValue> {
        runtime_store::preload_runtime_state_from_persistent_store().await?;
        let request: WasmUnlockRequest = serde_json::from_str(&request_json)
            .map_err(|e| JsValue::from_str(&format!("Invalid unlock request JSON: {e}")))?;
        if request.password.trim().is_empty() {
            return Err(JsValue::from_str("password cannot be empty"));
        }
        WASM_SDK_LIFECYCLE_STATE.with(|state| {
            let mut state = state.borrow_mut();
            if !state.initialized {
                return Err(JsValue::from_str("sdk is not initialized"));
            }
            if state.password.as_deref() != Some(request.password.as_str()) {
                return Err(JsValue::from_str("invalid password"));
            }
            state.unlocked = true;
            sync_runtime_session_authority_from_lifecycle(&state);
            Ok(())
        })?;
        bootstrap_default_wallet_from_lifecycle().await
    }

    #[wasm_bindgen(js_name = lock)]
    pub async fn lock(&self) -> Result<(), JsValue> {
        WASM_SDK_LIFECYCLE_STATE.with(|state| {
            let mut state = state.borrow_mut();
            if !state.initialized {
                return Err(JsValue::from_str("sdk is not initialized"));
            }
            state.unlocked = false;
            sync_runtime_session_authority_from_lifecycle(&state);
            Ok(())
        })?;
        WASM_SDK_DEFAULT_WALLET.with(|slot| {
            *slot.borrow_mut() = None;
        });
        Ok(())
    }

    #[wasm_bindgen(js_name = sendRgbFromGroupsValue)]
    pub async fn send_rgb_from_groups_value(
        &self,
        _request_json: String,
    ) -> Result<JsValue, JsValue> {
        Err(JsValue::from_str(
            "send_rgb_from_groups is not supported in wasm runtime: grouped SDK transfer adapter is unavailable",
        ))
    }

    #[wasm_bindgen(js_name = sendRgbFromGroupsJson)]
    pub async fn send_rgb_from_groups_json(&self, request_json: String) -> Result<String, JsValue> {
        let value = self.send_rgb_from_groups_value(request_json).await?;
        let parsed: serde_json::Value = js_from(value)?;
        js_to_json(&parsed)
    }

    #[wasm_bindgen(js_name = makerInitValue)]
    pub async fn maker_init_value(&self, request_json: String) -> Result<JsValue, JsValue> {
        swap_runtime::maker_init_value(request_json)
    }

    #[wasm_bindgen(js_name = makerInitJson)]
    pub async fn maker_init_json(&self, request_json: String) -> Result<String, JsValue> {
        let value = self.maker_init_value(request_json).await?;
        let parsed: serde_json::Value = js_from(value)?;
        js_to_json(&parsed)
    }

    #[wasm_bindgen(js_name = makerExecuteValue)]
    pub async fn maker_execute_value(&self, swap_string: String) -> Result<JsValue, JsValue> {
        swap_runtime::maker_execute_value(swap_string)
    }

    #[wasm_bindgen(js_name = makerExecuteJson)]
    pub async fn maker_execute_json(&self, swap_string: String) -> Result<String, JsValue> {
        let value = self.maker_execute_value(swap_string).await?;
        let parsed: serde_json::Value = js_from(value)?;
        js_to_json(&parsed)
    }

    #[wasm_bindgen(js_name = taker)]
    pub async fn taker(&self, request_json: String) -> Result<(), JsValue> {
        swap_runtime::taker(request_json)
    }

    #[wasm_bindgen(js_name = getSwapValue)]
    pub async fn get_swap_value(&self, swap_string: String) -> Result<JsValue, JsValue> {
        swap_runtime::get_swap_value(swap_string)
    }

    #[wasm_bindgen(js_name = getSwapJson)]
    pub async fn get_swap_json(&self, swap_string: String) -> Result<String, JsValue> {
        let value = self.get_swap_value(swap_string).await?;
        let parsed: serde_json::Value = js_from(value)?;
        js_to_json(&parsed)
    }

    #[wasm_bindgen(js_name = listSwapsValue)]
    pub async fn list_swaps_value(&self) -> Result<JsValue, JsValue> {
        swap_runtime::list_swaps_value()
    }

    #[wasm_bindgen(js_name = listSwapsJson)]
    pub async fn list_swaps_json(&self) -> Result<String, JsValue> {
        let value = self.list_swaps_value().await?;
        let parsed: serde_json::Value = js_from(value)?;
        js_to_json(&parsed)
    }

    #[wasm_bindgen(js_name = sendOnionMessage)]
    pub async fn send_onion_message(&self, request_json: String) -> Result<(), JsValue> {
        onion_runtime::send_onion_message(request_json)
    }

    #[wasm_bindgen(js_name = attachWallet)]
    pub fn attach_wallet(&self, node: &RlnWasmNode, wallet: &RlnWasmWallet) -> Result<(), JsValue> {
        node.attach_wallet(wallet)
    }

    #[wasm_bindgen(js_name = issueAssetNiaValue)]
    pub fn issue_asset_nia_value(
        &self,
        node: &RlnWasmNode,
        request_js: JsValue,
    ) -> Result<JsValue, JsValue> {
        node.issue_asset_nia_value(request_js)
    }

    #[wasm_bindgen(js_name = issueAssetNiaJson)]
    pub fn issue_asset_nia_json(
        &self,
        node: &RlnWasmNode,
        request_js: JsValue,
    ) -> Result<String, JsValue> {
        node.issue_asset_nia_json(request_js)
    }

    #[wasm_bindgen(js_name = issueAssetCfaValue)]
    pub fn issue_asset_cfa_value(
        &self,
        node: &RlnWasmNode,
        request_js: JsValue,
    ) -> Result<JsValue, JsValue> {
        node.issue_asset_cfa_value(request_js)
    }

    #[wasm_bindgen(js_name = issueAssetCfaJson)]
    pub fn issue_asset_cfa_json(
        &self,
        node: &RlnWasmNode,
        request_js: JsValue,
    ) -> Result<String, JsValue> {
        node.issue_asset_cfa_json(request_js)
    }

    #[wasm_bindgen(js_name = issueAssetUdaValue)]
    pub async fn issue_asset_uda_value(&self, _request_json: String) -> Result<JsValue, JsValue> {
        Err(JsValue::from_str(
            "issue_asset_uda is not supported in wasm runtime: RLN issuance adapter is unavailable",
        ))
    }

    #[wasm_bindgen(js_name = issueAssetUdaJson)]
    pub async fn issue_asset_uda_json(&self, request_json: String) -> Result<String, JsValue> {
        let value = self.issue_asset_uda_value(request_json).await?;
        let parsed: serde_json::Value = js_from(value)?;
        js_to_json(&parsed)
    }

    #[wasm_bindgen(js_name = postAssetMediaValue)]
    pub async fn post_asset_media_value(
        &self,
        mime: String,
        bytes_hex: String,
    ) -> Result<JsValue, JsValue> {
        let mime = mime.trim().to_string();
        if mime.is_empty() {
            return Err(JsValue::from_str("mime cannot be empty"));
        }
        let bytes_hex = bytes_hex.trim();
        if bytes_hex.is_empty() {
            return Err(JsValue::from_str("bytes_hex cannot be empty"));
        }
        let bytes =
            hex::decode(bytes_hex).map_err(|_| JsValue::from_str("bytes_hex must be valid hex"))?;
        if bytes.is_empty() {
            return Err(JsValue::from_str("media file cannot be empty"));
        }
        let digest = Sha256::hash(&bytes).to_string();
        let normalized_bytes_hex = hex::encode(bytes);
        media_store_insert(
            &digest,
            &WasmMediaStoreEntry {
                bytes_hex: normalized_bytes_hex,
                mime,
            },
        );

        js_obj(&WasmPostAssetMediaData { digest })
    }

    #[wasm_bindgen(js_name = postAssetMediaJson)]
    pub async fn post_asset_media_json(
        &self,
        mime: String,
        bytes_hex: String,
    ) -> Result<String, JsValue> {
        let value = self.post_asset_media_value(mime, bytes_hex).await?;
        let parsed: serde_json::Value = js_from(value)?;
        js_to_json(&parsed)
    }

    #[wasm_bindgen(js_name = runtimeCapabilitiesValue)]
    pub fn runtime_capabilities_value(&self) -> Result<JsValue, JsValue> {
        js_obj(&RlnWasmSdkRuntimeCapabilitiesData {
            wallet_runtime: true,
            node_runtime: true,
            ldk_runtime_scaffold: false,
            callback_status_updates: true,
        })
    }

    #[wasm_bindgen(js_name = runtimeCapabilitiesJson)]
    pub fn runtime_capabilities_json(&self) -> Result<String, JsValue> {
        let value = self.runtime_capabilities_value()?;
        let parsed: serde_json::Value = js_from(value)?;
        js_to_json(&parsed)
    }

    #[wasm_bindgen(js_name = setDefaultEnableVirtualChannelsV0)]
    pub fn set_default_enable_virtual_channels_v0_api(&self, enabled: bool) {
        set_sdk_default_enable_virtual_channels_v0(enabled);
    }

    #[wasm_bindgen(js_name = defaultEnableVirtualChannelsV0Value)]
    pub fn default_enable_virtual_channels_v0_value(&self) -> Result<JsValue, JsValue> {
        js_obj(&serde_json::json!({
            "enabled": sdk_default_enable_virtual_channels_v0()
        }))
    }

    #[wasm_bindgen(js_name = defaultEnableVirtualChannelsV0Json)]
    pub fn default_enable_virtual_channels_v0_json(&self) -> Result<String, JsValue> {
        let value = self.default_enable_virtual_channels_v0_value()?;
        let parsed: serde_json::Value = js_from(value)?;
        js_to_json(&parsed)
    }

    #[wasm_bindgen(js_name = newWallet)]
    pub fn new_wallet(&self, wallet_data_json: &str) -> Result<RlnWasmWallet, JsValue> {
        let wallet = RlnWasmWallet::new(wallet_data_json)?;
        apply_default_rgb_proxy_transport_to_wallet(&wallet)?;
        Ok(wallet)
    }

    #[wasm_bindgen(js_name = createWallet)]
    pub async fn create_wallet(&self, wallet_data_json: &str) -> Result<RlnWasmWallet, JsValue> {
        let wallet = RlnWasmWallet::create(wallet_data_json).await?;
        apply_default_rgb_proxy_transport_to_wallet(&wallet)?;
        Ok(wallet)
    }

    #[wasm_bindgen(js_name = newNode)]
    pub fn new_node(&self, proxy_url: String) -> Result<RlnWasmNode, JsValue> {
        ensure_sdk_node_runtime_allowed()?;
        let node = RlnWasmNode::new(proxy_url)?;
        maybe_attach_default_wallet_to_node(&node);
        Ok(node)
    }

    #[wasm_bindgen(js_name = newNodeWithRuntimeId)]
    pub fn new_node_with_runtime_id(
        &self,
        proxy_url: String,
        node_runtime_id: String,
    ) -> Result<RlnWasmNode, JsValue> {
        ensure_sdk_node_runtime_allowed()?;
        let node = RlnWasmNode::new_with_runtime_id_opt(proxy_url, Some(node_runtime_id))?;
        maybe_attach_default_wallet_to_node(&node);
        Ok(node)
    }

    #[wasm_bindgen(js_name = createNodeHandle)]
    pub fn create_node_handle(&self, proxy_url: String) -> Result<RlnWasmSdkNodeHandle, JsValue> {
        ensure_sdk_node_runtime_allowed()?;
        let node = RlnWasmNode::new(proxy_url)?;
        maybe_attach_default_wallet_to_node(&node);
        Ok(RlnWasmSdkNodeHandle { inner: node })
    }

    #[wasm_bindgen(js_name = createNodeHandleWithRuntimeId)]
    pub fn create_node_handle_with_runtime_id(
        &self,
        proxy_url: String,
        node_runtime_id: String,
    ) -> Result<RlnWasmSdkNodeHandle, JsValue> {
        ensure_sdk_node_runtime_allowed()?;
        let node = RlnWasmNode::new_with_runtime_id_opt(proxy_url, Some(node_runtime_id))?;
        maybe_attach_default_wallet_to_node(&node);
        Ok(RlnWasmSdkNodeHandle { inner: node })
    }

    #[wasm_bindgen(js_name = createWalletHandle)]
    pub fn create_wallet_handle(
        &self,
        wallet_data_json: &str,
    ) -> Result<RlnWasmSdkWalletHandle, JsValue> {
        let wallet = RlnWasmWallet::new(wallet_data_json)?;
        apply_default_rgb_proxy_transport_to_wallet(&wallet)?;
        Ok(RlnWasmSdkWalletHandle { inner: wallet })
    }

    #[wasm_bindgen(js_name = createWalletHandleAsync)]
    pub async fn create_wallet_handle_async(
        &self,
        wallet_data_json: &str,
    ) -> Result<RlnWasmSdkWalletHandle, JsValue> {
        let wallet = RlnWasmWallet::create(wallet_data_json).await?;
        apply_default_rgb_proxy_transport_to_wallet(&wallet)?;
        Ok(RlnWasmSdkWalletHandle { inner: wallet })
    }

    #[wasm_bindgen(js_name = setDefaultRgbProxyTransport)]
    pub fn set_default_rgb_proxy_transport(
        &self,
        endpoint: String,
        auth_token: Option<String>,
        node_id: Option<String>,
    ) -> Result<(), JsValue> {
        let config =
            RlnWasmWallet::validate_rgb_proxy_transport_config(endpoint, auth_token, node_id)?;
        WASM_SDK_DEFAULT_RGB_PROXY_TRANSPORT.with(|slot| {
            *slot.borrow_mut() = Some(config);
        });
        Ok(())
    }

    #[wasm_bindgen(js_name = clearDefaultRgbProxyTransport)]
    pub fn clear_default_rgb_proxy_transport(&self) {
        WASM_SDK_DEFAULT_RGB_PROXY_TRANSPORT.with(|slot| {
            slot.borrow_mut().take();
        });
    }

    #[wasm_bindgen(js_name = defaultRgbProxyTransportValue)]
    pub fn default_rgb_proxy_transport_value(&self) -> Result<JsValue, JsValue> {
        let config = sdk_default_rgb_proxy_transport();
        match config {
            Some(cfg) => js_obj(&cfg),
            None => Ok(JsValue::NULL),
        }
    }

    #[wasm_bindgen(js_name = defaultRgbProxyTransportJson)]
    pub fn default_rgb_proxy_transport_json(&self) -> Result<String, JsValue> {
        let value = self.default_rgb_proxy_transport_value()?;
        if value.is_null() {
            return Ok("null".to_string());
        }
        let parsed: RlnWasmRgbProxyTransportConfigData = js_from(value)?;
        js_to_json(&parsed)
    }

    #[wasm_bindgen(js_name = nodeInfoValue)]
    pub fn node_info_value(&self, node: &RlnWasmNode) -> Result<JsValue, JsValue> {
        node.node_info_value()
    }

    #[wasm_bindgen(js_name = nodeInfoJson)]
    pub fn node_info_json(&self, node: &RlnWasmNode) -> Result<String, JsValue> {
        node.node_info_json()
    }

    #[wasm_bindgen(js_name = nodePubkeyValue)]
    pub fn node_pubkey_value(&self, node: &RlnWasmNode) -> Result<JsValue, JsValue> {
        node.node_pubkey_value()
    }

    #[wasm_bindgen(js_name = nodePubkeyJson)]
    pub fn node_pubkey_json(&self, node: &RlnWasmNode) -> Result<String, JsValue> {
        node.node_pubkey_json()
    }

    #[wasm_bindgen(js_name = setRelaySessionAuth)]
    pub fn set_relay_session_auth(
        &self,
        node: &RlnWasmNode,
        relay_auth_token: Option<String>,
        relay_node_id: Option<String>,
    ) -> Result<(), JsValue> {
        node.set_relay_session_auth(relay_auth_token, relay_node_id)
    }

    #[wasm_bindgen(js_name = relaySessionAuthValue)]
    pub fn relay_session_auth_value(&self, node: &RlnWasmNode) -> Result<JsValue, JsValue> {
        node.relay_session_auth_value()
    }

    #[wasm_bindgen(js_name = relaySessionAuthJson)]
    pub fn relay_session_auth_json(&self, node: &RlnWasmNode) -> Result<String, JsValue> {
        node.relay_session_auth_json()
    }

    #[wasm_bindgen(js_name = setEnableVirtualChannelsV0)]
    pub fn set_enable_virtual_channels_v0(&self, node: &RlnWasmNode, enabled: bool) {
        node.set_enable_virtual_channels_v0(enabled);
    }

    #[wasm_bindgen(js_name = enableVirtualChannelsV0Value)]
    pub fn enable_virtual_channels_v0_value(&self, node: &RlnWasmNode) -> Result<JsValue, JsValue> {
        node.enable_virtual_channels_v0_value()
    }

    #[wasm_bindgen(js_name = enableVirtualChannelsV0Json)]
    pub fn enable_virtual_channels_v0_json(&self, node: &RlnWasmNode) -> Result<String, JsValue> {
        node.enable_virtual_channels_v0_json()
    }

    #[wasm_bindgen(js_name = ldkRuntimeStatusValue)]
    pub fn ldk_runtime_status_value(&self, node: &RlnWasmNode) -> Result<JsValue, JsValue> {
        node.ldk_runtime_status_value()
    }

    #[wasm_bindgen(js_name = ldkRuntimeStatusJson)]
    pub fn ldk_runtime_status_json(&self, node: &RlnWasmNode) -> Result<String, JsValue> {
        node.ldk_runtime_status_json()
    }

    #[wasm_bindgen(js_name = ldkRuntimeComponentsValue)]
    pub fn ldk_runtime_components_value(&self, node: &RlnWasmNode) -> Result<JsValue, JsValue> {
        node.ldk_runtime_components_value()
    }

    #[wasm_bindgen(js_name = ldkRuntimeComponentsJson)]
    pub fn ldk_runtime_components_json(&self, node: &RlnWasmNode) -> Result<String, JsValue> {
        node.ldk_runtime_components_json()
    }

    #[wasm_bindgen(js_name = nativeRuntimeCoreStatusValue)]
    pub fn native_runtime_core_status_value(&self, node: &RlnWasmNode) -> Result<JsValue, JsValue> {
        node.native_runtime_core_status_value()
    }

    #[wasm_bindgen(js_name = nativeRuntimeCoreStatusJson)]
    pub fn native_runtime_core_status_json(&self, node: &RlnWasmNode) -> Result<String, JsValue> {
        node.native_runtime_core_status_json()
    }

    #[wasm_bindgen(js_name = drainNativeRuntimeQueueValue)]
    pub fn drain_native_runtime_queue_value(&self, node: &RlnWasmNode) -> Result<JsValue, JsValue> {
        node.drain_native_runtime_queue_value()
    }

    #[wasm_bindgen(js_name = drainNativeRuntimeQueueJson)]
    pub fn drain_native_runtime_queue_json(&self, node: &RlnWasmNode) -> Result<String, JsValue> {
        node.drain_native_runtime_queue_json()
    }

    #[wasm_bindgen(js_name = processNativeRuntimeQueueValue)]
    pub fn process_native_runtime_queue_value(
        &self,
        node: &RlnWasmNode,
    ) -> Result<JsValue, JsValue> {
        node.process_native_runtime_queue_value()
    }

    #[wasm_bindgen(js_name = processNativeRuntimeQueueJson)]
    pub fn process_native_runtime_queue_json(&self, node: &RlnWasmNode) -> Result<String, JsValue> {
        node.process_native_runtime_queue_json()
    }

    #[wasm_bindgen(js_name = networkInfoValue)]
    pub fn network_info_value(&self, node: &RlnWasmNode) -> Result<JsValue, JsValue> {
        node.network_info_value()
    }

    #[wasm_bindgen(js_name = networkInfoJson)]
    pub fn network_info_json(&self, node: &RlnWasmNode) -> Result<String, JsValue> {
        node.network_info_json()
    }

    #[wasm_bindgen(js_name = chainSyncStartValue)]
    pub fn chain_sync_start_value(
        &self,
        node: &RlnWasmNode,
        indexer_url: String,
        poll_interval_ms: Option<u32>,
    ) -> Result<JsValue, JsValue> {
        node.chain_sync_start_value(indexer_url, poll_interval_ms)
    }

    #[wasm_bindgen(js_name = chainSyncStartJson)]
    pub fn chain_sync_start_json(
        &self,
        node: &RlnWasmNode,
        indexer_url: String,
        poll_interval_ms: Option<u32>,
    ) -> Result<String, JsValue> {
        node.chain_sync_start_json(indexer_url, poll_interval_ms)
    }

    #[wasm_bindgen(js_name = chainSyncStopValue)]
    pub fn chain_sync_stop_value(&self, node: &RlnWasmNode) -> Result<JsValue, JsValue> {
        node.chain_sync_stop_value()
    }

    #[wasm_bindgen(js_name = chainSyncStopJson)]
    pub fn chain_sync_stop_json(&self, node: &RlnWasmNode) -> Result<String, JsValue> {
        node.chain_sync_stop_json()
    }

    #[wasm_bindgen(js_name = chainSyncStatusValue)]
    pub fn chain_sync_status_value(&self, node: &RlnWasmNode) -> Result<JsValue, JsValue> {
        node.chain_sync_status_value()
    }

    #[wasm_bindgen(js_name = chainSyncStatusJson)]
    pub fn chain_sync_status_json(&self, node: &RlnWasmNode) -> Result<String, JsValue> {
        node.chain_sync_status_json()
    }

    #[wasm_bindgen(js_name = chainSyncTickValue)]
    pub async fn chain_sync_tick_value(&self, node: &RlnWasmNode) -> Result<JsValue, JsValue> {
        node.chain_sync_tick_value().await
    }

    #[wasm_bindgen(js_name = chainSyncTickJson)]
    pub async fn chain_sync_tick_json(&self, node: &RlnWasmNode) -> Result<String, JsValue> {
        node.chain_sync_tick_json().await
    }

    #[wasm_bindgen(js_name = chainSyncEnqueueRebroadcastTx)]
    pub fn chain_sync_enqueue_rebroadcast_tx(
        &self,
        node: &RlnWasmNode,
        txid: String,
        tx_hex: String,
    ) -> Result<(), JsValue> {
        node.chain_sync_enqueue_rebroadcast_tx(txid, tx_hex)
    }

    #[wasm_bindgen(js_name = signMessageValue)]
    pub fn sign_message_value(
        &self,
        node: &RlnWasmNode,
        message: String,
    ) -> Result<JsValue, JsValue> {
        node.sign_message_value(message)
    }

    #[wasm_bindgen(js_name = signMessageJson)]
    pub fn sign_message_json(
        &self,
        node: &RlnWasmNode,
        message: String,
    ) -> Result<String, JsValue> {
        node.sign_message_json(message)
    }

    #[wasm_bindgen(js_name = listPaymentsValue)]
    pub fn list_payments_value(&self, node: &RlnWasmNode) -> Result<JsValue, JsValue> {
        node.list_payments_value()
    }

    #[wasm_bindgen(js_name = listPaymentsJson)]
    pub fn list_payments_json(&self, node: &RlnWasmNode) -> Result<String, JsValue> {
        node.list_payments_json()
    }

    #[wasm_bindgen(js_name = listRgbLnTransfersValue)]
    pub fn list_rgb_ln_transfers_value(&self, node: &RlnWasmNode) -> Result<JsValue, JsValue> {
        node.list_rgb_ln_transfers_value()
    }

    #[wasm_bindgen(js_name = listRgbLnTransfersJson)]
    pub fn list_rgb_ln_transfers_json(&self, node: &RlnWasmNode) -> Result<String, JsValue> {
        node.list_rgb_ln_transfers_json()
    }

    #[wasm_bindgen(js_name = getPaymentValue)]
    pub fn get_payment_value(
        &self,
        node: &RlnWasmNode,
        payment_hash: String,
    ) -> Result<JsValue, JsValue> {
        node.get_payment_value(payment_hash)
    }

    #[wasm_bindgen(js_name = getPaymentJson)]
    pub fn get_payment_json(
        &self,
        node: &RlnWasmNode,
        payment_hash: String,
    ) -> Result<String, JsValue> {
        node.get_payment_json(payment_hash)
    }

    #[wasm_bindgen(js_name = decodeLnInvoiceValue)]
    pub fn decode_ln_invoice_value(
        &self,
        node: &RlnWasmNode,
        invoice: String,
    ) -> Result<JsValue, JsValue> {
        node.decode_ln_invoice_value(invoice)
    }

    #[wasm_bindgen(js_name = decodeLnInvoiceJson)]
    pub fn decode_ln_invoice_json(
        &self,
        node: &RlnWasmNode,
        invoice: String,
    ) -> Result<String, JsValue> {
        node.decode_ln_invoice_json(invoice)
    }

    #[wasm_bindgen(js_name = decodeRgbInvoiceValue)]
    pub fn decode_rgb_invoice_value(
        &self,
        node: &RlnWasmNode,
        invoice: String,
    ) -> Result<JsValue, JsValue> {
        node.decode_rgb_invoice_value(invoice)
    }

    #[wasm_bindgen(js_name = decodeRgbInvoiceJson)]
    pub fn decode_rgb_invoice_json(
        &self,
        node: &RlnWasmNode,
        invoice: String,
    ) -> Result<String, JsValue> {
        node.decode_rgb_invoice_json(invoice)
    }

    #[wasm_bindgen(js_name = createLnInvoiceValue)]
    pub fn create_ln_invoice_value(
        &self,
        node: &RlnWasmNode,
        amt_msat: Option<u64>,
        expiry_sec: u32,
        asset_id: Option<String>,
        asset_amount: Option<u64>,
    ) -> Result<JsValue, JsValue> {
        node.create_ln_invoice_value(amt_msat, expiry_sec, asset_id, asset_amount)
    }

    #[wasm_bindgen(js_name = createLnInvoiceJson)]
    pub fn create_ln_invoice_json(
        &self,
        node: &RlnWasmNode,
        amt_msat: Option<u64>,
        expiry_sec: u32,
        asset_id: Option<String>,
        asset_amount: Option<u64>,
    ) -> Result<String, JsValue> {
        node.create_ln_invoice_json(amt_msat, expiry_sec, asset_id, asset_amount)
    }

    #[wasm_bindgen(js_name = createHodlLnInvoiceValue)]
    pub fn create_hodl_ln_invoice_value(
        &self,
        node: &RlnWasmNode,
        amt_msat: Option<u64>,
        expiry_sec: u32,
        asset_id: Option<String>,
        asset_amount: Option<u64>,
        payment_hash: String,
    ) -> Result<JsValue, JsValue> {
        node.create_hodl_ln_invoice_value(
            amt_msat,
            expiry_sec,
            asset_id,
            asset_amount,
            payment_hash,
        )
    }

    #[wasm_bindgen(js_name = createHodlLnInvoiceJson)]
    pub fn create_hodl_ln_invoice_json(
        &self,
        node: &RlnWasmNode,
        amt_msat: Option<u64>,
        expiry_sec: u32,
        asset_id: Option<String>,
        asset_amount: Option<u64>,
        payment_hash: String,
    ) -> Result<String, JsValue> {
        node.create_hodl_ln_invoice_json(amt_msat, expiry_sec, asset_id, asset_amount, payment_hash)
    }

    #[wasm_bindgen(js_name = claimHodlInvoiceValue)]
    pub fn claim_hodl_invoice_value(
        &self,
        node: &RlnWasmNode,
        payment_hash: String,
        payment_preimage: String,
    ) -> Result<JsValue, JsValue> {
        node.claim_hodl_invoice_value(payment_hash, payment_preimage)
    }

    #[wasm_bindgen(js_name = claimHodlInvoiceJson)]
    pub fn claim_hodl_invoice_json(
        &self,
        node: &RlnWasmNode,
        payment_hash: String,
        payment_preimage: String,
    ) -> Result<String, JsValue> {
        node.claim_hodl_invoice_json(payment_hash, payment_preimage)
    }

    #[wasm_bindgen(js_name = cancelHodlInvoiceValue)]
    pub fn cancel_hodl_invoice_value(
        &self,
        node: &RlnWasmNode,
        payment_hash: String,
    ) -> Result<JsValue, JsValue> {
        node.cancel_hodl_invoice_value(payment_hash)
    }

    #[wasm_bindgen(js_name = cancelHodlInvoiceJson)]
    pub fn cancel_hodl_invoice_json(
        &self,
        node: &RlnWasmNode,
        payment_hash: String,
    ) -> Result<String, JsValue> {
        node.cancel_hodl_invoice_json(payment_hash)
    }

    #[wasm_bindgen(js_name = walletGetAddress)]
    pub fn wallet_get_address(&self, wallet: &RlnWasmWallet) -> Result<String, JsValue> {
        wallet.get_address()
    }

    #[wasm_bindgen(js_name = walletSignPsbtValue)]
    pub fn wallet_sign_psbt_value(
        &self,
        wallet: &RlnWasmWallet,
        unsigned_psbt: String,
    ) -> Result<String, JsValue> {
        wallet.sign_psbt_value(unsigned_psbt)
    }

    #[wasm_bindgen(js_name = walletSignPsbtJson)]
    pub fn wallet_sign_psbt_json(
        &self,
        wallet: &RlnWasmWallet,
        unsigned_psbt: String,
    ) -> Result<String, JsValue> {
        wallet.sign_psbt_json(unsigned_psbt)
    }

    #[wasm_bindgen(js_name = walletSetRgbProxyTransport)]
    pub fn wallet_set_rgb_proxy_transport(
        &self,
        wallet: &RlnWasmWallet,
        endpoint: String,
        auth_token: Option<String>,
        node_id: Option<String>,
    ) -> Result<(), JsValue> {
        wallet.set_rgb_proxy_transport(endpoint, auth_token, node_id)
    }

    #[wasm_bindgen(js_name = walletClearRgbProxyTransport)]
    pub fn wallet_clear_rgb_proxy_transport(&self, wallet: &RlnWasmWallet) {
        wallet.clear_rgb_proxy_transport();
    }

    #[wasm_bindgen(js_name = walletRgbProxyTransportValue)]
    pub fn wallet_rgb_proxy_transport_value(
        &self,
        wallet: &RlnWasmWallet,
    ) -> Result<JsValue, JsValue> {
        wallet.rgb_proxy_transport_value()
    }

    #[wasm_bindgen(js_name = walletRgbProxyTransportJson)]
    pub fn wallet_rgb_proxy_transport_json(
        &self,
        wallet: &RlnWasmWallet,
    ) -> Result<String, JsValue> {
        wallet.rgb_proxy_transport_json()
    }

    #[wasm_bindgen(js_name = walletGetBtcBalanceValue)]
    pub fn wallet_get_btc_balance_value(&self, wallet: &RlnWasmWallet) -> Result<JsValue, JsValue> {
        wallet.get_btc_balance_value()
    }

    #[wasm_bindgen(js_name = walletGetBtcBalanceJson)]
    pub fn wallet_get_btc_balance_json(&self, wallet: &RlnWasmWallet) -> Result<String, JsValue> {
        wallet.get_btc_balance_json()
    }

    #[wasm_bindgen(js_name = walletListTransactionsValue)]
    pub fn wallet_list_transactions_value(
        &self,
        wallet: &RlnWasmWallet,
    ) -> Result<JsValue, JsValue> {
        wallet.list_transactions_value()
    }

    #[wasm_bindgen(js_name = walletListTransactionsJson)]
    pub fn wallet_list_transactions_json(&self, wallet: &RlnWasmWallet) -> Result<String, JsValue> {
        wallet.list_transactions_json()
    }

    #[wasm_bindgen(js_name = walletListAssetsValue)]
    pub fn wallet_list_assets_value(
        &self,
        wallet: &RlnWasmWallet,
        filter_asset_schemas_js: JsValue,
    ) -> Result<JsValue, JsValue> {
        wallet.list_assets_value(filter_asset_schemas_js)
    }

    #[wasm_bindgen(js_name = walletListAssetsJson)]
    pub fn wallet_list_assets_json(
        &self,
        wallet: &RlnWasmWallet,
        filter_asset_schemas_js: JsValue,
    ) -> Result<String, JsValue> {
        wallet.list_assets_json(filter_asset_schemas_js)
    }

    #[wasm_bindgen(js_name = walletGetAssetMediaValue)]
    pub fn wallet_get_asset_media_value(
        &self,
        wallet: &RlnWasmWallet,
        asset_id: String,
    ) -> Result<JsValue, JsValue> {
        wallet.get_asset_media_value(asset_id)
    }

    #[wasm_bindgen(js_name = walletGetAssetMediaJson)]
    pub fn wallet_get_asset_media_json(
        &self,
        wallet: &RlnWasmWallet,
        asset_id: String,
    ) -> Result<String, JsValue> {
        wallet.get_asset_media_json(asset_id)
    }

    #[wasm_bindgen(js_name = walletIssueAssetUdaValue)]
    pub fn wallet_issue_asset_uda_value(
        &self,
        wallet: &RlnWasmWallet,
        request_js: JsValue,
    ) -> Result<JsValue, JsValue> {
        wallet.issue_asset_uda_value(request_js)
    }

    #[wasm_bindgen(js_name = walletIssueAssetUdaJson)]
    pub fn wallet_issue_asset_uda_json(
        &self,
        wallet: &RlnWasmWallet,
        request_js: JsValue,
    ) -> Result<String, JsValue> {
        wallet.issue_asset_uda_json(request_js)
    }

    #[wasm_bindgen(js_name = walletSendRgbFromGroupsValue)]
    pub async fn wallet_send_rgb_from_groups_value(
        &self,
        wallet: &RlnWasmWallet,
        request_js: JsValue,
    ) -> Result<JsValue, JsValue> {
        wallet.send_rgb_from_groups_value(request_js).await
    }

    #[wasm_bindgen(js_name = walletSendRgbFromGroupsJson)]
    pub async fn wallet_send_rgb_from_groups_json(
        &self,
        wallet: &RlnWasmWallet,
        request_js: JsValue,
    ) -> Result<String, JsValue> {
        wallet.send_rgb_from_groups_json(request_js).await
    }

    #[wasm_bindgen(js_name = connectPeer)]
    pub async fn connect_peer(
        &self,
        node: &RlnWasmNode,
        peer_addr: String,
        peer_pubkey: String,
    ) -> Result<(), JsValue> {
        node.connect_peer(peer_addr, peer_pubkey).await
    }

    #[wasm_bindgen(js_name = disconnectPeer)]
    pub async fn disconnect_peer(
        &self,
        node: &RlnWasmNode,
        peer_pubkey: String,
    ) -> Result<(), JsValue> {
        node.disconnect_peer(peer_pubkey).await
    }

    #[wasm_bindgen(js_name = openChannelValue)]
    pub fn open_channel_value(
        &self,
        node: &RlnWasmNode,
        peer_pubkey: String,
        capacity_sat: u64,
        public: bool,
        asset_id: Option<String>,
        asset_local_amount: Option<u64>,
    ) -> Result<JsValue, JsValue> {
        node.open_channel_value(
            peer_pubkey,
            capacity_sat,
            public,
            asset_id,
            asset_local_amount,
        )
    }

    #[wasm_bindgen(js_name = openChannelJson)]
    pub fn open_channel_json(
        &self,
        node: &RlnWasmNode,
        peer_pubkey: String,
        capacity_sat: u64,
        public: bool,
        asset_id: Option<String>,
        asset_local_amount: Option<u64>,
    ) -> Result<String, JsValue> {
        node.open_channel_json(
            peer_pubkey,
            capacity_sat,
            public,
            asset_id,
            asset_local_amount,
        )
    }

    #[wasm_bindgen(js_name = sendPaymentValue)]
    pub fn send_payment_value(
        &self,
        node: &RlnWasmNode,
        invoice: String,
        amt_msat: Option<u64>,
        asset_id: Option<String>,
        asset_amount: Option<u64>,
    ) -> Result<JsValue, JsValue> {
        node.send_payment_value(invoice, amt_msat, asset_id, asset_amount)
    }

    #[wasm_bindgen(js_name = sendPaymentJson)]
    pub fn send_payment_json(
        &self,
        node: &RlnWasmNode,
        invoice: String,
        amt_msat: Option<u64>,
        asset_id: Option<String>,
        asset_amount: Option<u64>,
    ) -> Result<String, JsValue> {
        node.send_payment_json(invoice, amt_msat, asset_id, asset_amount)
    }

    #[wasm_bindgen(js_name = keysendValue)]
    pub fn keysend_value(
        &self,
        node: &RlnWasmNode,
        dest_pubkey: String,
        amt_msat: u64,
        asset_id: Option<String>,
        asset_amount: Option<u64>,
    ) -> Result<JsValue, JsValue> {
        node.keysend_value(dest_pubkey, amt_msat, asset_id, asset_amount)
    }

    #[wasm_bindgen(js_name = keysendJson)]
    pub fn keysend_json(
        &self,
        node: &RlnWasmNode,
        dest_pubkey: String,
        amt_msat: u64,
        asset_id: Option<String>,
        asset_amount: Option<u64>,
    ) -> Result<String, JsValue> {
        node.keysend_json(dest_pubkey, amt_msat, asset_id, asset_amount)
    }

    #[wasm_bindgen(js_name = listPeersValue)]
    pub fn list_peers_value(&self, node: &RlnWasmNode) -> Result<JsValue, JsValue> {
        node.list_peers_value()
    }

    #[wasm_bindgen(js_name = listPeersJson)]
    pub fn list_peers_json(&self, node: &RlnWasmNode) -> Result<String, JsValue> {
        node.list_peers_json()
    }

    #[wasm_bindgen(js_name = listChannelsValue)]
    pub fn list_channels_value(&self, node: &RlnWasmNode) -> Result<JsValue, JsValue> {
        node.list_channels_value()
    }

    #[wasm_bindgen(js_name = listChannelsJson)]
    pub fn list_channels_json(&self, node: &RlnWasmNode) -> Result<String, JsValue> {
        node.list_channels_json()
    }

    #[wasm_bindgen(js_name = closeChannel)]
    pub fn close_channel(&self, node: &RlnWasmNode, channel_id: String) -> Result<(), JsValue> {
        node.close_channel(channel_id)
    }

    #[wasm_bindgen(js_name = getChannelId)]
    pub fn get_channel_id(
        &self,
        node: &RlnWasmNode,
        temporary_channel_id: String,
    ) -> Result<String, JsValue> {
        node.get_channel_id(temporary_channel_id)
    }

    #[wasm_bindgen(js_name = invoiceStatusValue)]
    pub fn invoice_status_value(
        &self,
        node: &RlnWasmNode,
        invoice: String,
    ) -> Result<JsValue, JsValue> {
        node.invoice_status_value(invoice)
    }

    #[wasm_bindgen(js_name = invoiceStatusJson)]
    pub fn invoice_status_json(
        &self,
        node: &RlnWasmNode,
        invoice: String,
    ) -> Result<String, JsValue> {
        node.invoice_status_json(invoice)
    }

    #[wasm_bindgen(js_name = updatePaymentStatus)]
    pub fn update_payment_status(
        &self,
        node: &RlnWasmNode,
        payment_hash: String,
        status: String,
    ) -> Result<JsValue, JsValue> {
        node.update_payment_status(payment_hash, status)
    }

    #[wasm_bindgen(js_name = updatePaymentStatusJson)]
    pub fn update_payment_status_json(
        &self,
        node: &RlnWasmNode,
        payment_hash: String,
        status: String,
    ) -> Result<String, JsValue> {
        node.update_payment_status_json(payment_hash, status)
    }

    #[wasm_bindgen(js_name = updatePaymentStatusByInvoice)]
    pub fn update_payment_status_by_invoice(
        &self,
        node: &RlnWasmNode,
        invoice: String,
        status: String,
    ) -> Result<JsValue, JsValue> {
        node.update_payment_status_by_invoice(invoice, status)
    }

    #[wasm_bindgen(js_name = updatePaymentStatusByInvoiceJson)]
    pub fn update_payment_status_by_invoice_json(
        &self,
        node: &RlnWasmNode,
        invoice: String,
        status: String,
    ) -> Result<String, JsValue> {
        node.update_payment_status_by_invoice_json(invoice, status)
    }

    #[wasm_bindgen(js_name = ingestReadEventPayloadHex)]
    pub fn ingest_read_event_payload_hex(
        &self,
        node: &RlnWasmNode,
        payload_hex: String,
    ) -> Result<JsValue, JsValue> {
        node.ingest_read_event_payload_hex(payload_hex)
    }

    #[wasm_bindgen(js_name = ingestReadEventPayloadHexJson)]
    pub fn ingest_read_event_payload_hex_json(
        &self,
        node: &RlnWasmNode,
        payload_hex: String,
    ) -> Result<String, JsValue> {
        node.ingest_read_event_payload_hex_json(payload_hex)
    }

    #[wasm_bindgen(js_name = ingestRuntimeTransportEventPayloadHexValue)]
    pub fn ingest_runtime_transport_event_payload_hex_value(
        &self,
        node: &RlnWasmNode,
        payload_hex: String,
    ) -> Result<JsValue, JsValue> {
        node.ingest_runtime_transport_event_payload_hex_value(payload_hex)
    }

    #[wasm_bindgen(js_name = ingestRuntimeTransportEventPayloadHexJson)]
    pub fn ingest_runtime_transport_event_payload_hex_json(
        &self,
        node: &RlnWasmNode,
        payload_hex: String,
    ) -> Result<String, JsValue> {
        node.ingest_runtime_transport_event_payload_hex_json(payload_hex)
    }

    #[wasm_bindgen(js_name = failPendingPayments)]
    pub fn fail_pending_payments(&self, node: &RlnWasmNode) -> Result<JsValue, JsValue> {
        node.fail_pending_payments_api()
    }

    #[wasm_bindgen(js_name = listRuntimeEventsValue)]
    pub fn list_runtime_events_value(&self, node: &RlnWasmNode) -> Result<JsValue, JsValue> {
        node.list_runtime_events_value()
    }

    #[wasm_bindgen(js_name = listRuntimeEventsJson)]
    pub fn list_runtime_events_json(&self, node: &RlnWasmNode) -> Result<String, JsValue> {
        node.list_runtime_events_json()
    }

    #[wasm_bindgen(js_name = installAutoPeerManagerHooks)]
    pub fn install_auto_peer_manager_hooks(&self, node: &RlnWasmNode) {
        node.install_auto_peer_manager_hooks();
    }

    #[wasm_bindgen(js_name = clearAutoPeerManagerHooks)]
    pub fn clear_auto_peer_manager_hooks(&self, node: &RlnWasmNode) {
        node.clear_auto_peer_manager_hooks();
    }
}

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

#[wasm_bindgen]
impl RlnWasmSdkNodeHandle {
    #[wasm_bindgen(js_name = nodeInfoValue)]
    pub fn node_info_value(&self) -> Result<JsValue, JsValue> {
        self.inner.node_info_value()
    }

    #[wasm_bindgen(js_name = nodeInfoJson)]
    pub fn node_info_json(&self) -> Result<String, JsValue> {
        self.inner.node_info_json()
    }

    #[wasm_bindgen(js_name = nodePubkeyValue)]
    pub fn node_pubkey_value(&self) -> Result<JsValue, JsValue> {
        self.inner.node_pubkey_value()
    }

    #[wasm_bindgen(js_name = nodePubkeyJson)]
    pub fn node_pubkey_json(&self) -> Result<String, JsValue> {
        self.inner.node_pubkey_json()
    }

    #[wasm_bindgen(js_name = setRelaySessionAuth)]
    pub fn set_relay_session_auth(
        &self,
        relay_auth_token: Option<String>,
        relay_node_id: Option<String>,
    ) -> Result<(), JsValue> {
        self.inner
            .set_relay_session_auth(relay_auth_token, relay_node_id)
    }

    #[wasm_bindgen(js_name = relaySessionAuthValue)]
    pub fn relay_session_auth_value(&self) -> Result<JsValue, JsValue> {
        self.inner.relay_session_auth_value()
    }

    #[wasm_bindgen(js_name = relaySessionAuthJson)]
    pub fn relay_session_auth_json(&self) -> Result<String, JsValue> {
        self.inner.relay_session_auth_json()
    }

    #[wasm_bindgen(js_name = setEnableVirtualChannelsV0)]
    pub fn set_enable_virtual_channels_v0(&self, enabled: bool) {
        self.inner.set_enable_virtual_channels_v0(enabled);
    }

    #[wasm_bindgen(js_name = enableVirtualChannelsV0Value)]
    pub fn enable_virtual_channels_v0_value(&self) -> Result<JsValue, JsValue> {
        self.inner.enable_virtual_channels_v0_value()
    }

    #[wasm_bindgen(js_name = enableVirtualChannelsV0Json)]
    pub fn enable_virtual_channels_v0_json(&self) -> Result<String, JsValue> {
        self.inner.enable_virtual_channels_v0_json()
    }

    #[wasm_bindgen(js_name = ldkRuntimeStatusValue)]
    pub fn ldk_runtime_status_value(&self) -> Result<JsValue, JsValue> {
        self.inner.ldk_runtime_status_value()
    }

    #[wasm_bindgen(js_name = ldkRuntimeStatusJson)]
    pub fn ldk_runtime_status_json(&self) -> Result<String, JsValue> {
        self.inner.ldk_runtime_status_json()
    }

    #[wasm_bindgen(js_name = ldkRuntimeComponentsValue)]
    pub fn ldk_runtime_components_value(&self) -> Result<JsValue, JsValue> {
        self.inner.ldk_runtime_components_value()
    }

    #[wasm_bindgen(js_name = ldkRuntimeComponentsJson)]
    pub fn ldk_runtime_components_json(&self) -> Result<String, JsValue> {
        self.inner.ldk_runtime_components_json()
    }

    #[wasm_bindgen(js_name = nativeRuntimeCoreStatusValue)]
    pub fn native_runtime_core_status_value(&self) -> Result<JsValue, JsValue> {
        self.inner.native_runtime_core_status_value()
    }

    #[wasm_bindgen(js_name = nativeRuntimeCoreStatusJson)]
    pub fn native_runtime_core_status_json(&self) -> Result<String, JsValue> {
        self.inner.native_runtime_core_status_json()
    }

    #[wasm_bindgen(js_name = drainNativeRuntimeQueueValue)]
    pub fn drain_native_runtime_queue_value(&self) -> Result<JsValue, JsValue> {
        self.inner.drain_native_runtime_queue_value()
    }

    #[wasm_bindgen(js_name = drainNativeRuntimeQueueJson)]
    pub fn drain_native_runtime_queue_json(&self) -> Result<String, JsValue> {
        self.inner.drain_native_runtime_queue_json()
    }

    #[wasm_bindgen(js_name = processNativeRuntimeQueueValue)]
    pub fn process_native_runtime_queue_value(&self) -> Result<JsValue, JsValue> {
        self.inner.process_native_runtime_queue_value()
    }

    #[wasm_bindgen(js_name = processNativeRuntimeQueueJson)]
    pub fn process_native_runtime_queue_json(&self) -> Result<String, JsValue> {
        self.inner.process_native_runtime_queue_json()
    }

    #[wasm_bindgen(js_name = networkInfoValue)]
    pub fn network_info_value(&self) -> Result<JsValue, JsValue> {
        self.inner.network_info_value()
    }

    #[wasm_bindgen(js_name = networkInfoJson)]
    pub fn network_info_json(&self) -> Result<String, JsValue> {
        self.inner.network_info_json()
    }

    #[wasm_bindgen(js_name = chainSyncStartValue)]
    pub fn chain_sync_start_value(
        &self,
        indexer_url: String,
        poll_interval_ms: Option<u32>,
    ) -> Result<JsValue, JsValue> {
        self.inner
            .chain_sync_start_value(indexer_url, poll_interval_ms)
    }

    #[wasm_bindgen(js_name = chainSyncStartJson)]
    pub fn chain_sync_start_json(
        &self,
        indexer_url: String,
        poll_interval_ms: Option<u32>,
    ) -> Result<String, JsValue> {
        self.inner
            .chain_sync_start_json(indexer_url, poll_interval_ms)
    }

    #[wasm_bindgen(js_name = chainSyncStopValue)]
    pub fn chain_sync_stop_value(&self) -> Result<JsValue, JsValue> {
        self.inner.chain_sync_stop_value()
    }

    #[wasm_bindgen(js_name = chainSyncStopJson)]
    pub fn chain_sync_stop_json(&self) -> Result<String, JsValue> {
        self.inner.chain_sync_stop_json()
    }

    #[wasm_bindgen(js_name = chainSyncStatusValue)]
    pub fn chain_sync_status_value(&self) -> Result<JsValue, JsValue> {
        self.inner.chain_sync_status_value()
    }

    #[wasm_bindgen(js_name = chainSyncStatusJson)]
    pub fn chain_sync_status_json(&self) -> Result<String, JsValue> {
        self.inner.chain_sync_status_json()
    }

    #[wasm_bindgen(js_name = chainSyncTickValue)]
    pub async fn chain_sync_tick_value(&self) -> Result<JsValue, JsValue> {
        self.inner.chain_sync_tick_value().await
    }

    #[wasm_bindgen(js_name = chainSyncTickJson)]
    pub async fn chain_sync_tick_json(&self) -> Result<String, JsValue> {
        self.inner.chain_sync_tick_json().await
    }

    #[wasm_bindgen(js_name = chainSyncEnqueueRebroadcastTx)]
    pub fn chain_sync_enqueue_rebroadcast_tx(
        &self,
        txid: String,
        tx_hex: String,
    ) -> Result<(), JsValue> {
        self.inner.chain_sync_enqueue_rebroadcast_tx(txid, tx_hex)
    }

    #[wasm_bindgen(js_name = signMessageValue)]
    pub fn sign_message_value(&self, message: String) -> Result<JsValue, JsValue> {
        self.inner.sign_message_value(message)
    }

    #[wasm_bindgen(js_name = signMessageJson)]
    pub fn sign_message_json(&self, message: String) -> Result<String, JsValue> {
        self.inner.sign_message_json(message)
    }

    #[wasm_bindgen(js_name = listPaymentsValue)]
    pub fn list_payments_value(&self) -> Result<JsValue, JsValue> {
        self.inner.list_payments_value()
    }

    #[wasm_bindgen(js_name = listPaymentsJson)]
    pub fn list_payments_json(&self) -> Result<String, JsValue> {
        self.inner.list_payments_json()
    }

    #[wasm_bindgen(js_name = listRgbLnTransfersValue)]
    pub fn list_rgb_ln_transfers_value(&self) -> Result<JsValue, JsValue> {
        self.inner.list_rgb_ln_transfers_value()
    }

    #[wasm_bindgen(js_name = listRgbLnTransfersJson)]
    pub fn list_rgb_ln_transfers_json(&self) -> Result<String, JsValue> {
        self.inner.list_rgb_ln_transfers_json()
    }

    #[wasm_bindgen(js_name = getPaymentValue)]
    pub fn get_payment_value(&self, payment_hash: String) -> Result<JsValue, JsValue> {
        self.inner.get_payment_value(payment_hash)
    }

    #[wasm_bindgen(js_name = getPaymentJson)]
    pub fn get_payment_json(&self, payment_hash: String) -> Result<String, JsValue> {
        self.inner.get_payment_json(payment_hash)
    }

    #[wasm_bindgen(js_name = keysendValue)]
    pub fn keysend_value(
        &self,
        dest_pubkey: String,
        amt_msat: u64,
        asset_id: Option<String>,
        asset_amount: Option<u64>,
    ) -> Result<JsValue, JsValue> {
        self.inner
            .keysend_value(dest_pubkey, amt_msat, asset_id, asset_amount)
    }

    #[wasm_bindgen(js_name = keysendJson)]
    pub fn keysend_json(
        &self,
        dest_pubkey: String,
        amt_msat: u64,
        asset_id: Option<String>,
        asset_amount: Option<u64>,
    ) -> Result<String, JsValue> {
        self.inner
            .keysend_json(dest_pubkey, amt_msat, asset_id, asset_amount)
    }

    #[wasm_bindgen(js_name = ingestReadEventPayloadHex)]
    pub fn ingest_read_event_payload_hex(&self, payload_hex: String) -> Result<JsValue, JsValue> {
        self.inner.ingest_read_event_payload_hex(payload_hex)
    }

    #[wasm_bindgen(js_name = ingestReadEventPayloadHexJson)]
    pub fn ingest_read_event_payload_hex_json(
        &self,
        payload_hex: String,
    ) -> Result<String, JsValue> {
        self.inner.ingest_read_event_payload_hex_json(payload_hex)
    }

    #[wasm_bindgen(js_name = ingestRuntimeTransportEventPayloadHexValue)]
    pub fn ingest_runtime_transport_event_payload_hex_value(
        &self,
        payload_hex: String,
    ) -> Result<JsValue, JsValue> {
        self.inner
            .ingest_runtime_transport_event_payload_hex_value(payload_hex)
    }

    #[wasm_bindgen(js_name = ingestRuntimeTransportEventPayloadHexJson)]
    pub fn ingest_runtime_transport_event_payload_hex_json(
        &self,
        payload_hex: String,
    ) -> Result<String, JsValue> {
        self.inner
            .ingest_runtime_transport_event_payload_hex_json(payload_hex)
    }

    #[wasm_bindgen(js_name = connectPeer)]
    pub async fn connect_peer(
        &self,
        peer_addr: String,
        peer_pubkey: String,
    ) -> Result<(), JsValue> {
        self.inner.connect_peer(peer_addr, peer_pubkey).await
    }

    #[wasm_bindgen(js_name = disconnectPeer)]
    pub async fn disconnect_peer(&self, peer_pubkey: String) -> Result<(), JsValue> {
        self.inner.disconnect_peer(peer_pubkey).await
    }

    #[wasm_bindgen(js_name = listPeersValue)]
    pub fn list_peers_value(&self) -> Result<JsValue, JsValue> {
        self.inner.list_peers_value()
    }

    #[wasm_bindgen(js_name = listPeersJson)]
    pub fn list_peers_json(&self) -> Result<String, JsValue> {
        self.inner.list_peers_json()
    }

    #[wasm_bindgen(js_name = listChannelsValue)]
    pub fn list_channels_value(&self) -> Result<JsValue, JsValue> {
        self.inner.list_channels_value()
    }

    #[wasm_bindgen(js_name = listChannelsJson)]
    pub fn list_channels_json(&self) -> Result<String, JsValue> {
        self.inner.list_channels_json()
    }

    #[wasm_bindgen(js_name = openChannelValue)]
    pub fn open_channel_value(
        &self,
        peer_pubkey: String,
        capacity_sat: u64,
        public: bool,
        asset_id: Option<String>,
        asset_local_amount: Option<u64>,
    ) -> Result<JsValue, JsValue> {
        self.inner.open_channel_value(
            peer_pubkey,
            capacity_sat,
            public,
            asset_id,
            asset_local_amount,
        )
    }

    #[wasm_bindgen(js_name = openChannelJson)]
    pub fn open_channel_json(
        &self,
        peer_pubkey: String,
        capacity_sat: u64,
        public: bool,
        asset_id: Option<String>,
        asset_local_amount: Option<u64>,
    ) -> Result<String, JsValue> {
        self.inner.open_channel_json(
            peer_pubkey,
            capacity_sat,
            public,
            asset_id,
            asset_local_amount,
        )
    }

    #[wasm_bindgen(js_name = openChannelValueWithOptions)]
    pub fn open_channel_value_with_options(
        &self,
        peer_pubkey: String,
        capacity_sat: u64,
        public: bool,
        asset_id: Option<String>,
        asset_local_amount: Option<u64>,
        virtual_open_mode: Option<String>,
    ) -> Result<JsValue, JsValue> {
        self.inner.open_channel_value_with_options(
            peer_pubkey,
            capacity_sat,
            public,
            asset_id,
            asset_local_amount,
            virtual_open_mode,
        )
    }

    #[wasm_bindgen(js_name = openChannelJsonWithOptions)]
    pub fn open_channel_json_with_options(
        &self,
        peer_pubkey: String,
        capacity_sat: u64,
        public: bool,
        asset_id: Option<String>,
        asset_local_amount: Option<u64>,
        virtual_open_mode: Option<String>,
    ) -> Result<String, JsValue> {
        self.inner.open_channel_json_with_options(
            peer_pubkey,
            capacity_sat,
            public,
            asset_id,
            asset_local_amount,
            virtual_open_mode,
        )
    }

    #[wasm_bindgen(js_name = closeChannel)]
    pub fn close_channel(&self, channel_id: String) -> Result<(), JsValue> {
        self.inner.close_channel(channel_id)
    }

    #[wasm_bindgen(js_name = closeChannelWithOptions)]
    pub fn close_channel_with_options(
        &self,
        channel_id: String,
        peer_pubkey: Option<String>,
        force: bool,
    ) -> Result<(), JsValue> {
        self.inner
            .close_channel_with_options(channel_id, peer_pubkey, force)
    }

    #[wasm_bindgen(js_name = getChannelId)]
    pub fn get_channel_id(&self, temporary_channel_id: String) -> Result<String, JsValue> {
        self.inner.get_channel_id(temporary_channel_id)
    }

    #[wasm_bindgen(js_name = sendPaymentValue)]
    pub fn send_payment_value(
        &self,
        invoice: String,
        amt_msat: Option<u64>,
        asset_id: Option<String>,
        asset_amount: Option<u64>,
    ) -> Result<JsValue, JsValue> {
        self.inner
            .send_payment_value(invoice, amt_msat, asset_id, asset_amount)
    }

    #[wasm_bindgen(js_name = sendPaymentJson)]
    pub fn send_payment_json(
        &self,
        invoice: String,
        amt_msat: Option<u64>,
        asset_id: Option<String>,
        asset_amount: Option<u64>,
    ) -> Result<String, JsValue> {
        self.inner
            .send_payment_json(invoice, amt_msat, asset_id, asset_amount)
    }

    #[wasm_bindgen(js_name = invoiceStatusValue)]
    pub fn invoice_status_value(&self, invoice: String) -> Result<JsValue, JsValue> {
        self.inner.invoice_status_value(invoice)
    }

    #[wasm_bindgen(js_name = invoiceStatusJson)]
    pub fn invoice_status_json(&self, invoice: String) -> Result<String, JsValue> {
        self.inner.invoice_status_json(invoice)
    }

    #[wasm_bindgen(js_name = updatePaymentStatus)]
    pub fn update_payment_status(
        &self,
        payment_hash: String,
        status: String,
    ) -> Result<JsValue, JsValue> {
        self.inner.update_payment_status(payment_hash, status)
    }

    #[wasm_bindgen(js_name = updatePaymentStatusJson)]
    pub fn update_payment_status_json(
        &self,
        payment_hash: String,
        status: String,
    ) -> Result<String, JsValue> {
        self.inner.update_payment_status_json(payment_hash, status)
    }

    #[wasm_bindgen(js_name = updatePaymentStatusByInvoice)]
    pub fn update_payment_status_by_invoice(
        &self,
        invoice: String,
        status: String,
    ) -> Result<JsValue, JsValue> {
        self.inner.update_payment_status_by_invoice(invoice, status)
    }

    #[wasm_bindgen(js_name = updatePaymentStatusByInvoiceJson)]
    pub fn update_payment_status_by_invoice_json(
        &self,
        invoice: String,
        status: String,
    ) -> Result<String, JsValue> {
        self.inner
            .update_payment_status_by_invoice_json(invoice, status)
    }

    #[wasm_bindgen(js_name = decodeLnInvoiceValue)]
    pub fn decode_ln_invoice_value(&self, invoice: String) -> Result<JsValue, JsValue> {
        self.inner.decode_ln_invoice_value(invoice)
    }

    #[wasm_bindgen(js_name = decodeLnInvoiceJson)]
    pub fn decode_ln_invoice_json(&self, invoice: String) -> Result<String, JsValue> {
        self.inner.decode_ln_invoice_json(invoice)
    }

    #[wasm_bindgen(js_name = decodeRgbInvoiceValue)]
    pub fn decode_rgb_invoice_value(&self, invoice: String) -> Result<JsValue, JsValue> {
        self.inner.decode_rgb_invoice_value(invoice)
    }

    #[wasm_bindgen(js_name = decodeRgbInvoiceJson)]
    pub fn decode_rgb_invoice_json(&self, invoice: String) -> Result<String, JsValue> {
        self.inner.decode_rgb_invoice_json(invoice)
    }

    #[wasm_bindgen(js_name = createLnInvoiceValue)]
    pub fn create_ln_invoice_value(
        &self,
        amt_msat: Option<u64>,
        expiry_sec: u32,
        asset_id: Option<String>,
        asset_amount: Option<u64>,
    ) -> Result<JsValue, JsValue> {
        self.inner
            .create_ln_invoice_value(amt_msat, expiry_sec, asset_id, asset_amount)
    }

    #[wasm_bindgen(js_name = createLnInvoiceJson)]
    pub fn create_ln_invoice_json(
        &self,
        amt_msat: Option<u64>,
        expiry_sec: u32,
        asset_id: Option<String>,
        asset_amount: Option<u64>,
    ) -> Result<String, JsValue> {
        self.inner
            .create_ln_invoice_json(amt_msat, expiry_sec, asset_id, asset_amount)
    }

    #[wasm_bindgen(js_name = createHodlLnInvoiceValue)]
    pub fn create_hodl_ln_invoice_value(
        &self,
        amt_msat: Option<u64>,
        expiry_sec: u32,
        asset_id: Option<String>,
        asset_amount: Option<u64>,
        payment_hash: String,
    ) -> Result<JsValue, JsValue> {
        self.inner.create_hodl_ln_invoice_value(
            amt_msat,
            expiry_sec,
            asset_id,
            asset_amount,
            payment_hash,
        )
    }

    #[wasm_bindgen(js_name = createHodlLnInvoiceJson)]
    pub fn create_hodl_ln_invoice_json(
        &self,
        amt_msat: Option<u64>,
        expiry_sec: u32,
        asset_id: Option<String>,
        asset_amount: Option<u64>,
        payment_hash: String,
    ) -> Result<String, JsValue> {
        self.inner.create_hodl_ln_invoice_json(
            amt_msat,
            expiry_sec,
            asset_id,
            asset_amount,
            payment_hash,
        )
    }

    #[wasm_bindgen(js_name = claimHodlInvoiceValue)]
    pub fn claim_hodl_invoice_value(
        &self,
        payment_hash: String,
        payment_preimage: String,
    ) -> Result<JsValue, JsValue> {
        self.inner
            .claim_hodl_invoice_value(payment_hash, payment_preimage)
    }

    #[wasm_bindgen(js_name = claimHodlInvoiceJson)]
    pub fn claim_hodl_invoice_json(
        &self,
        payment_hash: String,
        payment_preimage: String,
    ) -> Result<String, JsValue> {
        self.inner
            .claim_hodl_invoice_json(payment_hash, payment_preimage)
    }

    #[wasm_bindgen(js_name = cancelHodlInvoiceValue)]
    pub fn cancel_hodl_invoice_value(&self, payment_hash: String) -> Result<JsValue, JsValue> {
        self.inner.cancel_hodl_invoice_value(payment_hash)
    }

    #[wasm_bindgen(js_name = cancelHodlInvoiceJson)]
    pub fn cancel_hodl_invoice_json(&self, payment_hash: String) -> Result<String, JsValue> {
        self.inner.cancel_hodl_invoice_json(payment_hash)
    }

    #[wasm_bindgen(js_name = failPendingPayments)]
    pub fn fail_pending_payments(&self) -> Result<JsValue, JsValue> {
        self.inner.fail_pending_payments_api()
    }

    #[wasm_bindgen(js_name = listRuntimeEventsValue)]
    pub fn list_runtime_events_value(&self) -> Result<JsValue, JsValue> {
        self.inner.list_runtime_events_value()
    }

    #[wasm_bindgen(js_name = listRuntimeEventsJson)]
    pub fn list_runtime_events_json(&self) -> Result<String, JsValue> {
        self.inner.list_runtime_events_json()
    }

    #[wasm_bindgen(js_name = installAutoPeerManagerHooks)]
    pub fn install_auto_peer_manager_hooks(&self) {
        self.inner.install_auto_peer_manager_hooks();
    }

    #[wasm_bindgen(js_name = clearAutoPeerManagerHooks)]
    pub fn clear_auto_peer_manager_hooks(&self) {
        self.inner.clear_auto_peer_manager_hooks();
    }

    #[wasm_bindgen(js_name = attachWallet)]
    pub fn attach_wallet(&self, wallet: &RlnWasmWallet) -> Result<(), JsValue> {
        self.inner.attach_wallet(wallet)
    }

    #[wasm_bindgen(js_name = issueAssetNiaValue)]
    pub fn issue_asset_nia_value(&self, request_js: JsValue) -> Result<JsValue, JsValue> {
        self.inner.issue_asset_nia_value(request_js)
    }

    #[wasm_bindgen(js_name = issueAssetNiaJson)]
    pub fn issue_asset_nia_json(&self, request_js: JsValue) -> Result<String, JsValue> {
        self.inner.issue_asset_nia_json(request_js)
    }

    #[wasm_bindgen(js_name = issueAssetCfaValue)]
    pub fn issue_asset_cfa_value(&self, request_js: JsValue) -> Result<JsValue, JsValue> {
        self.inner.issue_asset_cfa_value(request_js)
    }

    #[wasm_bindgen(js_name = issueAssetCfaJson)]
    pub fn issue_asset_cfa_json(&self, request_js: JsValue) -> Result<String, JsValue> {
        self.inner.issue_asset_cfa_json(request_js)
    }
}

#[wasm_bindgen]
impl RlnWasmSdkWalletHandle {
    #[wasm_bindgen(js_name = getAddress)]
    pub fn get_address(&self) -> Result<String, JsValue> {
        self.inner.get_address()
    }

    #[wasm_bindgen(js_name = signPsbtValue)]
    pub fn sign_psbt_value(&self, unsigned_psbt: String) -> Result<String, JsValue> {
        self.inner.sign_psbt_value(unsigned_psbt)
    }

    #[wasm_bindgen(js_name = signPsbtJson)]
    pub fn sign_psbt_json(&self, unsigned_psbt: String) -> Result<String, JsValue> {
        self.inner.sign_psbt_json(unsigned_psbt)
    }

    #[wasm_bindgen(js_name = setRgbProxyTransport)]
    pub fn set_rgb_proxy_transport(
        &self,
        endpoint: String,
        auth_token: Option<String>,
        node_id: Option<String>,
    ) -> Result<(), JsValue> {
        self.inner
            .set_rgb_proxy_transport(endpoint, auth_token, node_id)
    }

    #[wasm_bindgen(js_name = clearRgbProxyTransport)]
    pub fn clear_rgb_proxy_transport(&self) {
        self.inner.clear_rgb_proxy_transport();
    }

    #[wasm_bindgen(js_name = rgbProxyTransportValue)]
    pub fn rgb_proxy_transport_value(&self) -> Result<JsValue, JsValue> {
        self.inner.rgb_proxy_transport_value()
    }

    #[wasm_bindgen(js_name = rgbProxyTransportJson)]
    pub fn rgb_proxy_transport_json(&self) -> Result<String, JsValue> {
        self.inner.rgb_proxy_transport_json()
    }

    #[wasm_bindgen(js_name = getBtcBalanceValue)]
    pub fn get_btc_balance_value(&self) -> Result<JsValue, JsValue> {
        self.inner.get_btc_balance_value()
    }

    #[wasm_bindgen(js_name = getBtcBalanceJson)]
    pub fn get_btc_balance_json(&self) -> Result<String, JsValue> {
        self.inner.get_btc_balance_json()
    }

    #[wasm_bindgen(js_name = listTransactionsValue)]
    pub fn list_transactions_value(&self) -> Result<JsValue, JsValue> {
        self.inner.list_transactions_value()
    }

    #[wasm_bindgen(js_name = listTransactionsJson)]
    pub fn list_transactions_json(&self) -> Result<String, JsValue> {
        self.inner.list_transactions_json()
    }

    #[wasm_bindgen(js_name = listAssetsValue)]
    pub fn list_assets_value(&self, filter_asset_schemas_js: JsValue) -> Result<JsValue, JsValue> {
        self.inner.list_assets_value(filter_asset_schemas_js)
    }

    #[wasm_bindgen(js_name = listAssetsJson)]
    pub fn list_assets_json(&self, filter_asset_schemas_js: JsValue) -> Result<String, JsValue> {
        self.inner.list_assets_json(filter_asset_schemas_js)
    }

    #[wasm_bindgen(js_name = getAssetMediaValue)]
    pub fn get_asset_media_value(&self, asset_id: String) -> Result<JsValue, JsValue> {
        self.inner.get_asset_media_value(asset_id)
    }

    #[wasm_bindgen(js_name = getAssetMediaJson)]
    pub fn get_asset_media_json(&self, asset_id: String) -> Result<String, JsValue> {
        self.inner.get_asset_media_json(asset_id)
    }

    #[wasm_bindgen(js_name = issueAssetUdaValue)]
    pub fn issue_asset_uda_value(&self, request_js: JsValue) -> Result<JsValue, JsValue> {
        self.inner.issue_asset_uda_value(request_js)
    }

    #[wasm_bindgen(js_name = issueAssetUdaJson)]
    pub fn issue_asset_uda_json(&self, request_js: JsValue) -> Result<String, JsValue> {
        self.inner.issue_asset_uda_json(request_js)
    }

    #[wasm_bindgen(js_name = blindReceiveValue)]
    pub fn blind_receive_value(
        &self,
        asset_id: Option<String>,
        assignment_js: JsValue,
        duration_seconds: Option<u32>,
        transport_endpoints_js: JsValue,
        min_confirmations: u8,
    ) -> Result<JsValue, JsValue> {
        self.inner.blind_receive_value(
            asset_id,
            assignment_js,
            duration_seconds,
            transport_endpoints_js,
            min_confirmations,
        )
    }

    #[wasm_bindgen(js_name = blindReceiveJson)]
    pub fn blind_receive_json(
        &self,
        asset_id: Option<String>,
        assignment_js: JsValue,
        duration_seconds: Option<u32>,
        transport_endpoints_js: JsValue,
        min_confirmations: u8,
    ) -> Result<String, JsValue> {
        self.inner.blind_receive_json(
            asset_id,
            assignment_js,
            duration_seconds,
            transport_endpoints_js,
            min_confirmations,
        )
    }

    #[wasm_bindgen(js_name = sendRgbFromGroupsValue)]
    pub async fn send_rgb_from_groups_value(
        &self,
        request_js: JsValue,
    ) -> Result<JsValue, JsValue> {
        self.inner.send_rgb_from_groups_value(request_js).await
    }

    #[wasm_bindgen(js_name = sendRgbFromGroupsJson)]
    pub async fn send_rgb_from_groups_json(&self, request_js: JsValue) -> Result<String, JsValue> {
        self.inner.send_rgb_from_groups_json(request_js).await
    }

    #[wasm_bindgen(js_name = goOnlineValue)]
    pub async fn go_online_value(
        &self,
        skip_consistency_check: bool,
        indexer_url: String,
    ) -> Result<JsValue, JsValue> {
        self.inner
            .go_online_value(skip_consistency_check, indexer_url)
            .await
    }

    #[wasm_bindgen(js_name = goOnlineJson)]
    pub async fn go_online_json(
        &self,
        skip_consistency_check: bool,
        indexer_url: String,
    ) -> Result<String, JsValue> {
        self.inner
            .go_online_json(skip_consistency_check, indexer_url)
            .await
    }

    #[wasm_bindgen(js_name = syncOnline)]
    pub async fn sync_online(&self, online_js: JsValue) -> Result<(), JsValue> {
        self.inner.sync_online(online_js).await
    }

    #[wasm_bindgen(js_name = getFeeEstimation)]
    pub async fn get_fee_estimation(
        &self,
        online_js: JsValue,
        blocks: u16,
    ) -> Result<f64, JsValue> {
        self.inner.get_fee_estimation(online_js, blocks).await
    }

    #[wasm_bindgen(js_name = getFeeEstimationJson)]
    pub async fn get_fee_estimation_json(
        &self,
        online_js: JsValue,
        blocks: u16,
    ) -> Result<String, JsValue> {
        self.inner.get_fee_estimation_json(online_js, blocks).await
    }

    #[wasm_bindgen(js_name = createUtxosBegin)]
    pub async fn create_utxos_begin(
        &self,
        online_js: JsValue,
        up_to: bool,
        num: Option<u8>,
        size: Option<u32>,
        fee_rate: u64,
        skip_sync: bool,
    ) -> Result<String, JsValue> {
        self.inner
            .create_utxos_begin(online_js, up_to, num, size, fee_rate, skip_sync)
            .await
    }

    #[wasm_bindgen(js_name = createUtxosEnd)]
    pub async fn create_utxos_end(
        &self,
        online_js: JsValue,
        signed_psbt: String,
        skip_sync: bool,
    ) -> Result<u8, JsValue> {
        self.inner
            .create_utxos_end(online_js, signed_psbt, skip_sync)
            .await
    }

    #[wasm_bindgen(js_name = createUtxosEndJson)]
    pub async fn create_utxos_end_json(
        &self,
        online_js: JsValue,
        signed_psbt: String,
        skip_sync: bool,
    ) -> Result<String, JsValue> {
        self.inner
            .create_utxos_end_json(online_js, signed_psbt, skip_sync)
            .await
    }

    #[wasm_bindgen(js_name = sendBegin)]
    pub async fn send_begin(
        &self,
        online_js: JsValue,
        recipient_map_js: JsValue,
        donation: bool,
        fee_rate: u64,
        min_confirmations: u8,
    ) -> Result<String, JsValue> {
        self.inner
            .send_begin(
                online_js,
                recipient_map_js,
                donation,
                fee_rate,
                min_confirmations,
            )
            .await
    }

    #[wasm_bindgen(js_name = sendEndValue)]
    pub async fn send_end_value(
        &self,
        online_js: JsValue,
        signed_psbt: String,
        skip_sync: bool,
    ) -> Result<JsValue, JsValue> {
        self.inner
            .send_end_value(online_js, signed_psbt, skip_sync)
            .await
    }

    #[wasm_bindgen(js_name = sendEndJson)]
    pub async fn send_end_json(
        &self,
        online_js: JsValue,
        signed_psbt: String,
        skip_sync: bool,
    ) -> Result<String, JsValue> {
        self.inner
            .send_end_json(online_js, signed_psbt, skip_sync)
            .await
    }

    #[wasm_bindgen(js_name = sendBtcBegin)]
    pub async fn send_btc_begin(
        &self,
        online_js: JsValue,
        address: String,
        amount: u64,
        fee_rate: u64,
        skip_sync: bool,
    ) -> Result<String, JsValue> {
        self.inner
            .send_btc_begin(online_js, address, amount, fee_rate, skip_sync)
            .await
    }

    #[wasm_bindgen(js_name = sendBtcEnd)]
    pub async fn send_btc_end(
        &self,
        online_js: JsValue,
        signed_psbt: String,
        skip_sync: bool,
    ) -> Result<String, JsValue> {
        self.inner
            .send_btc_end(online_js, signed_psbt, skip_sync)
            .await
    }

    #[wasm_bindgen(js_name = refreshValue)]
    pub async fn refresh_value(
        &self,
        online_js: JsValue,
        asset_id: Option<String>,
        filter_js: JsValue,
        skip_sync: bool,
    ) -> Result<JsValue, JsValue> {
        self.inner
            .refresh_value(online_js, asset_id, filter_js, skip_sync)
            .await
    }

    #[wasm_bindgen(js_name = refreshJson)]
    pub async fn refresh_json(
        &self,
        online_js: JsValue,
        asset_id: Option<String>,
        filter_js: JsValue,
        skip_sync: bool,
    ) -> Result<String, JsValue> {
        self.inner
            .refresh_json(online_js, asset_id, filter_js, skip_sync)
            .await
    }

    #[wasm_bindgen(js_name = failTransfers)]
    pub async fn fail_transfers(
        &self,
        online_js: JsValue,
        batch_transfer_idx: Option<i32>,
        no_asset_only: bool,
        skip_sync: bool,
    ) -> Result<bool, JsValue> {
        self.inner
            .fail_transfers(online_js, batch_transfer_idx, no_asset_only, skip_sync)
            .await
    }

    #[wasm_bindgen(js_name = failTransfersJson)]
    pub async fn fail_transfers_json(
        &self,
        online_js: JsValue,
        batch_transfer_idx: Option<i32>,
        no_asset_only: bool,
        skip_sync: bool,
    ) -> Result<String, JsValue> {
        self.inner
            .fail_transfers_json(online_js, batch_transfer_idx, no_asset_only, skip_sync)
            .await
    }

    #[wasm_bindgen(js_name = drainToBegin)]
    pub async fn drain_to_begin(
        &self,
        online_js: JsValue,
        address: String,
        destroy_assets: bool,
        fee_rate: u64,
    ) -> Result<String, JsValue> {
        self.inner
            .drain_to_begin(online_js, address, destroy_assets, fee_rate)
            .await
    }

    #[wasm_bindgen(js_name = drainToEnd)]
    pub async fn drain_to_end(
        &self,
        online_js: JsValue,
        signed_psbt: String,
    ) -> Result<String, JsValue> {
        self.inner.drain_to_end(online_js, signed_psbt).await
    }

    #[wasm_bindgen(js_name = inflateBegin)]
    pub async fn inflate_begin(
        &self,
        online_js: JsValue,
        asset_id: String,
        inflation_amounts_js: JsValue,
        fee_rate: u64,
        min_confirmations: u8,
    ) -> Result<String, JsValue> {
        self.inner
            .inflate_begin(
                online_js,
                asset_id,
                inflation_amounts_js,
                fee_rate,
                min_confirmations,
            )
            .await
    }

    #[wasm_bindgen(js_name = inflateEndValue)]
    pub async fn inflate_end_value(
        &self,
        online_js: JsValue,
        signed_psbt: String,
    ) -> Result<JsValue, JsValue> {
        self.inner.inflate_end_value(online_js, signed_psbt).await
    }

    #[wasm_bindgen(js_name = inflateEndJson)]
    pub async fn inflate_end_json(
        &self,
        online_js: JsValue,
        signed_psbt: String,
    ) -> Result<String, JsValue> {
        self.inner.inflate_end_json(online_js, signed_psbt).await
    }

    #[wasm_bindgen(js_name = listUnspentsVanillaValue)]
    pub async fn list_unspents_vanilla_value(
        &self,
        online_js: JsValue,
        min_confirmations: u8,
        skip_sync: bool,
    ) -> Result<JsValue, JsValue> {
        self.inner
            .list_unspents_vanilla_value(online_js, min_confirmations, skip_sync)
            .await
    }

    #[wasm_bindgen(js_name = listUnspentsVanillaJson)]
    pub async fn list_unspents_vanilla_json(
        &self,
        online_js: JsValue,
        min_confirmations: u8,
        skip_sync: bool,
    ) -> Result<String, JsValue> {
        self.inner
            .list_unspents_vanilla_json(online_js, min_confirmations, skip_sync)
            .await
    }

    #[wasm_bindgen(js_name = backup)]
    pub fn backup(&self, password: String) -> Result<Vec<u8>, JsValue> {
        self.inner.backup(password)
    }

    #[wasm_bindgen(js_name = restoreBackup)]
    pub fn restore_backup(&self, backup_bytes: Vec<u8>, password: String) -> Result<(), JsValue> {
        self.inner.restore_backup(backup_bytes, password)
    }

    #[wasm_bindgen(js_name = backupInfo)]
    pub fn backup_info(&self) -> Result<bool, JsValue> {
        self.inner.backup_info()
    }

    #[wasm_bindgen(js_name = backupInfoJson)]
    pub fn backup_info_json(&self) -> Result<String, JsValue> {
        self.inner.backup_info_json()
    }

    #[wasm_bindgen(js_name = configureVssBackup)]
    pub fn configure_vss_backup(
        &self,
        server_url: String,
        store_id: String,
        signing_key_hex: String,
    ) -> Result<(), JsValue> {
        self.inner
            .configure_vss_backup(server_url, store_id, signing_key_hex)
    }

    #[wasm_bindgen(js_name = disableVssBackup)]
    pub fn disable_vss_backup(&self) {
        self.inner.disable_vss_backup()
    }

    #[wasm_bindgen(js_name = vssBackupValue)]
    pub async fn vss_backup_value(&self) -> Result<JsValue, JsValue> {
        self.inner.vss_backup_value().await
    }

    #[wasm_bindgen(js_name = vssBackupJson)]
    pub async fn vss_backup_json(&self) -> Result<String, JsValue> {
        self.inner.vss_backup_json().await
    }

    #[wasm_bindgen(js_name = vssRestoreBackup)]
    pub async fn vss_restore_backup(&self) -> Result<(), JsValue> {
        self.inner.vss_restore_backup().await
    }

    #[wasm_bindgen(js_name = vssBackupInfoValue)]
    pub async fn vss_backup_info_value(&self) -> Result<JsValue, JsValue> {
        self.inner.vss_backup_info_value().await
    }

    #[wasm_bindgen(js_name = vssBackupInfoJson)]
    pub async fn vss_backup_info_json(&self) -> Result<String, JsValue> {
        self.inner.vss_backup_info_json().await
    }
}

#[wasm_bindgen]
impl RlnWasmWallet {
    #[wasm_bindgen(constructor)]
    pub fn new(wallet_data_json: &str) -> Result<RlnWasmWallet, JsValue> {
        let wallet_data: rgb_lib_wasm::wallet::WalletData = serde_json::from_str(wallet_data_json)
            .map_err(|e| JsValue::from_str(&format!("Invalid WalletData JSON: {e}")))?;
        let wallet = rgb_lib_wasm::Wallet::new(wallet_data)
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        Ok(Self {
            inner: std::rc::Rc::new(RefCell::new(wallet)),
        })
    }

    pub async fn create(wallet_data_json: &str) -> Result<RlnWasmWallet, JsValue> {
        let wallet_data: rgb_lib_wasm::wallet::WalletData = serde_json::from_str(wallet_data_json)
            .map_err(|e| JsValue::from_str(&format!("Invalid WalletData JSON: {e}")))?;
        let mut wallet = rgb_lib_wasm::Wallet::new(wallet_data)
            .map_err(|e| JsValue::from_str(&e.to_string()))?;

        let idb_key = wallet.idb_key();
        match rgb_lib_wasm::wallet::idb_store::load_snapshot(&idb_key).await {
            Ok(Some(snapshot)) => {
                wallet
                    .restore_from_snapshot(snapshot)
                    .map_err(|e| JsValue::from_str(&e.to_string()))?;
            }
            Ok(None) => {}
            Err(err) => {
                web_sys::console::warn_1(
                    &format!("IDB load warning (continuing fresh): {err}").into(),
                );
            }
        }

        Ok(Self {
            inner: std::rc::Rc::new(RefCell::new(wallet)),
        })
    }

    #[wasm_bindgen(js_name = getWalletDataValue)]
    pub fn get_wallet_data_value(&self) -> Result<JsValue, JsValue> {
        let data = self.inner.borrow().get_wallet_data();
        js_obj(&data)
    }

    #[wasm_bindgen(js_name = getWalletDataJson)]
    pub fn get_wallet_data_json(&self) -> Result<String, JsValue> {
        let data = self.inner.borrow().get_wallet_data();
        js_to_json(&data)
    }

    fn validate_rgb_proxy_transport_config(
        endpoint: String,
        auth_token: Option<String>,
        node_id: Option<String>,
    ) -> Result<RlnWasmRgbProxyTransportConfigData, JsValue> {
        let endpoint = endpoint.trim().to_string();
        if endpoint.is_empty() {
            return Err(JsValue::from_str("rgb_proxy_endpoint cannot be empty"));
        }
        let lower = endpoint.to_ascii_lowercase();
        if !lower.starts_with("http://") && !lower.starts_with("https://") {
            return Err(JsValue::from_str(
                "rgb_proxy_endpoint must use http:// or https://",
            ));
        }
        match (auth_token, node_id) {
            (None, None) => Ok(RlnWasmRgbProxyTransportConfigData {
                endpoint,
                auth_token: None,
                node_id: None,
            }),
            (Some(token), Some(node_id)) => {
                let token = token.trim().to_string();
                let node_id = node_id.trim().to_string();
                if token.is_empty() {
                    return Err(JsValue::from_str("rgb_proxy_auth_token cannot be empty"));
                }
                if node_id.is_empty() {
                    return Err(JsValue::from_str("rgb_proxy_node_id cannot be empty"));
                }
                if SecpPublicKey::from_str(&node_id).is_err() {
                    return Err(JsValue::from_str("invalid rgb_proxy_node_id"));
                }
                Ok(RlnWasmRgbProxyTransportConfigData {
                    endpoint,
                    auth_token: Some(token),
                    node_id: Some(node_id),
                })
            }
            _ => Err(JsValue::from_str(
                "rgb_proxy_auth_token and rgb_proxy_node_id must be provided together",
            )),
        }
    }

    fn effective_rgb_proxy_endpoint(
        config: &RlnWasmRgbProxyTransportConfigData,
    ) -> Result<String, JsValue> {
        match (&config.auth_token, &config.node_id) {
            (Some(token), Some(node_id)) => {
                let separator = if config.endpoint.contains('?') {
                    '&'
                } else {
                    '?'
                };
                Ok(format!(
                    "{}{}auth_token={}&node_id={}",
                    config.endpoint,
                    separator,
                    urlencoding::encode(token),
                    urlencoding::encode(node_id),
                ))
            }
            (None, None) => Ok(config.endpoint.clone()),
            _ => Err(JsValue::from_str(
                "rgb_proxy_auth_token and rgb_proxy_node_id must be provided together",
            )),
        }
    }

    fn rgb_proxy_transport_key(&self) -> String {
        self.inner.borrow().idb_key()
    }

    fn current_rgb_proxy_transport(&self) -> Option<RlnWasmRgbProxyTransportConfigData> {
        let key = self.rgb_proxy_transport_key();
        if let Some(config) = wallet_rgb_proxy_transport_get(&key) {
            return Some(config);
        }
        let default = sdk_default_rgb_proxy_transport()?;
        let _ = wallet_rgb_proxy_transport_insert(&key, &default);
        Some(default)
    }

    fn resolve_transport_endpoints(
        &self,
        transport_endpoints_js: JsValue,
    ) -> Result<Vec<String>, JsValue> {
        if transport_endpoints_js.is_null() || transport_endpoints_js.is_undefined() {
            let Some(config) = self.current_rgb_proxy_transport() else {
                return Err(JsValue::from_str(
                    "transport_endpoints must be provided or setRgbProxyTransport must be configured",
                ));
            };
            let endpoint = Self::effective_rgb_proxy_endpoint(&config)?;
            return Ok(vec![endpoint]);
        }
        serde_wasm_bindgen::from_value(transport_endpoints_js)
            .map_err(|e| JsValue::from_str(&format!("Invalid transport endpoints: {e}")))
    }

    #[wasm_bindgen(js_name = setRgbProxyTransport)]
    pub fn set_rgb_proxy_transport(
        &self,
        endpoint: String,
        auth_token: Option<String>,
        node_id: Option<String>,
    ) -> Result<(), JsValue> {
        let config = Self::validate_rgb_proxy_transport_config(endpoint, auth_token, node_id)?;
        let key = self.rgb_proxy_transport_key();
        wallet_rgb_proxy_transport_insert(&key, &config)
    }

    #[wasm_bindgen(js_name = clearRgbProxyTransport)]
    pub fn clear_rgb_proxy_transport(&self) {
        let key = self.rgb_proxy_transport_key();
        wallet_rgb_proxy_transport_remove(&key);
    }

    #[wasm_bindgen(js_name = rgbProxyTransportValue)]
    pub fn rgb_proxy_transport_value(&self) -> Result<JsValue, JsValue> {
        match self.current_rgb_proxy_transport() {
            Some(config) => js_obj(&config),
            None => Ok(JsValue::NULL),
        }
    }

    #[wasm_bindgen(js_name = rgbProxyTransportJson)]
    pub fn rgb_proxy_transport_json(&self) -> Result<String, JsValue> {
        let value = self.rgb_proxy_transport_value()?;
        if value.is_null() {
            return Ok("null".to_string());
        }
        let parsed: RlnWasmRgbProxyTransportConfigData = js_from(value)?;
        js_to_json(&parsed)
    }

    #[wasm_bindgen(js_name = getAddress)]
    pub fn get_address(&self) -> Result<String, JsValue> {
        self.inner
            .borrow_mut()
            .get_address()
            .map_err(|e| JsValue::from_str(&e.to_string()))
    }

    #[wasm_bindgen(js_name = signPsbtValue)]
    pub fn sign_psbt_value(&self, unsigned_psbt: String) -> Result<String, JsValue> {
        if unsigned_psbt.trim().is_empty() {
            return Err(JsValue::from_str("unsigned_psbt cannot be empty"));
        }
        self.inner
            .borrow()
            .sign_psbt(unsigned_psbt, None)
            .map_err(|e| JsValue::from_str(&e.to_string()))
    }

    #[wasm_bindgen(js_name = signPsbtJson)]
    pub fn sign_psbt_json(&self, unsigned_psbt: String) -> Result<String, JsValue> {
        self.sign_psbt_value(unsigned_psbt)
    }

    #[wasm_bindgen(js_name = getBtcBalanceValue)]
    pub fn get_btc_balance_value(&self) -> Result<JsValue, JsValue> {
        let balance = self
            .inner
            .borrow_mut()
            .get_btc_balance(None, true)
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        js_obj(&balance)
    }

    #[wasm_bindgen(js_name = getBtcBalanceJson)]
    pub fn get_btc_balance_json(&self) -> Result<String, JsValue> {
        let value = self.get_btc_balance_value()?;
        let parsed: serde_json::Value = js_from(value)?;
        js_to_json(&parsed)
    }

    #[wasm_bindgen(js_name = listTransactionsValue)]
    pub fn list_transactions_value(&self) -> Result<JsValue, JsValue> {
        let txs = self
            .inner
            .borrow_mut()
            .list_transactions(None, true)
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        js_obj(&txs)
    }

    #[wasm_bindgen(js_name = listTransactionsJson)]
    pub fn list_transactions_json(&self) -> Result<String, JsValue> {
        let value = self.list_transactions_value()?;
        let parsed: serde_json::Value = js_from(value)?;
        js_to_json(&parsed)
    }

    #[wasm_bindgen(js_name = listAssetsValue)]
    pub fn list_assets_value(&self, filter_asset_schemas_js: JsValue) -> Result<JsValue, JsValue> {
        let schemas: Vec<rgb_lib_wasm::AssetSchema> =
            serde_wasm_bindgen::from_value(filter_asset_schemas_js)
                .map_err(|e| JsValue::from_str(&format!("Invalid schemas: {e}")))?;
        let assets = self
            .inner
            .borrow()
            .list_assets(schemas)
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        js_obj(&assets)
    }

    #[wasm_bindgen(js_name = listAssetsJson)]
    pub fn list_assets_json(&self, filter_asset_schemas_js: JsValue) -> Result<String, JsValue> {
        let value = self.list_assets_value(filter_asset_schemas_js)?;
        let parsed: serde_json::Value = js_from(value)?;
        js_to_json(&parsed)
    }

    #[wasm_bindgen(js_name = getAssetMetadataValue)]
    pub fn get_asset_metadata_value(&self, asset_id: String) -> Result<JsValue, JsValue> {
        if asset_id.trim().is_empty() {
            return Err(JsValue::from_str("asset_id cannot be empty"));
        }
        let metadata = self
            .inner
            .borrow()
            .get_asset_metadata(asset_id)
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        js_obj(&metadata)
    }

    #[wasm_bindgen(js_name = getAssetMetadataJson)]
    pub fn get_asset_metadata_json(&self, asset_id: String) -> Result<String, JsValue> {
        let value = self.get_asset_metadata_value(asset_id)?;
        let parsed: serde_json::Value = js_from(value)?;
        js_to_json(&parsed)
    }

    #[wasm_bindgen(js_name = getAssetMediaValue)]
    pub fn get_asset_media_value(&self, asset_id: String) -> Result<JsValue, JsValue> {
        if asset_id.trim().is_empty() {
            return Err(JsValue::from_str("asset_id cannot be empty"));
        }
        let digest = normalize_media_digest(&asset_id)?;
        let media =
            media_store_get(&digest).ok_or_else(|| JsValue::from_str("invalid media digest"))?;
        let _mime_hint = media.mime;
        js_obj(&WasmAssetMediaData {
            bytes_hex: media.bytes_hex,
        })
    }

    #[wasm_bindgen(js_name = getAssetMediaJson)]
    pub fn get_asset_media_json(&self, asset_id: String) -> Result<String, JsValue> {
        let value = self.get_asset_media_value(asset_id)?;
        let parsed: serde_json::Value = js_from(value)?;
        js_to_json(&parsed)
    }

    #[wasm_bindgen(js_name = issueAssetUdaValue)]
    pub fn issue_asset_uda_value(&self, request_js: JsValue) -> Result<JsValue, JsValue> {
        let request: WasmIssueAssetUdaRequest = serde_wasm_bindgen::from_value(request_js)
            .map_err(|e| JsValue::from_str(&format!("Invalid issue_asset_uda request: {e}")))?;
        if request.ticker.trim().is_empty() {
            return Err(JsValue::from_str("ticker cannot be empty"));
        }
        if request.name.trim().is_empty() {
            return Err(JsValue::from_str("name cannot be empty"));
        }
        Err(JsValue::from_str(
            "issue_asset_uda is not supported in wasm runtime: rgb-lib-wasm does not expose a UDA issuance primitive",
        ))
    }

    #[wasm_bindgen(js_name = issueAssetUdaJson)]
    pub fn issue_asset_uda_json(&self, request_js: JsValue) -> Result<String, JsValue> {
        let value = self.issue_asset_uda_value(request_js)?;
        let parsed: serde_json::Value = js_from(value)?;
        js_to_json(&parsed)
    }

    #[wasm_bindgen(js_name = sendRgbFromGroupsValue)]
    pub async fn send_rgb_from_groups_value(
        &self,
        request_js: JsValue,
    ) -> Result<JsValue, JsValue> {
        let request: WasmSendRgbFromGroupsRequest = serde_wasm_bindgen::from_value(request_js)
            .map_err(|e| {
                JsValue::from_str(&format!("Invalid send_rgb_from_groups request: {e}"))
            })?;
        let recipient_map = recipient_map_from_groups(request.recipient_groups)?;

        let mut wallet = self.inner.borrow_mut();
        let unsigned_psbt = wallet
            .send_begin(
                request.online,
                recipient_map,
                request.donation,
                request.fee_rate,
                request.min_confirmations,
            )
            .await
            .map_err(|e| JsValue::from_str(&e.to_string()))?;

        js_obj(&WasmSendRgbFromGroupsData { unsigned_psbt })
    }

    #[wasm_bindgen(js_name = sendRgbFromGroupsJson)]
    pub async fn send_rgb_from_groups_json(&self, request_js: JsValue) -> Result<String, JsValue> {
        let value = self.send_rgb_from_groups_value(request_js).await?;
        let parsed: serde_json::Value = js_from(value)?;
        js_to_json(&parsed)
    }

    #[wasm_bindgen(js_name = getAssetBalanceValue)]
    pub fn get_asset_balance_value(&self, asset_id: String) -> Result<JsValue, JsValue> {
        if asset_id.trim().is_empty() {
            return Err(JsValue::from_str("asset_id cannot be empty"));
        }
        let balance = self
            .inner
            .borrow()
            .get_asset_balance(asset_id)
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        js_obj(&balance)
    }

    #[wasm_bindgen(js_name = getAssetBalanceJson)]
    pub fn get_asset_balance_json(&self, asset_id: String) -> Result<String, JsValue> {
        let value = self.get_asset_balance_value(asset_id)?;
        let parsed: serde_json::Value = js_from(value)?;
        js_to_json(&parsed)
    }

    #[wasm_bindgen(js_name = listTransfersValue)]
    pub fn list_transfers_value(&self, asset_id: Option<String>) -> Result<JsValue, JsValue> {
        if let Some(id) = &asset_id {
            if id.trim().is_empty() {
                return Err(JsValue::from_str("asset_id cannot be empty if provided"));
            }
        }
        let transfers = self
            .inner
            .borrow()
            .list_transfers(asset_id)
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        js_obj(&transfers)
    }

    #[wasm_bindgen(js_name = listTransfersJson)]
    pub fn list_transfers_json(&self, asset_id: Option<String>) -> Result<String, JsValue> {
        let value = self.list_transfers_value(asset_id)?;
        let parsed: serde_json::Value = js_from(value)?;
        js_to_json(&parsed)
    }

    #[wasm_bindgen(js_name = listUnspentsValue)]
    pub fn list_unspents_value(&self, settled_only: bool) -> Result<JsValue, JsValue> {
        let unspents = self
            .inner
            .borrow_mut()
            .list_unspents(None, settled_only, true)
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        js_obj(&unspents)
    }

    #[wasm_bindgen(js_name = listUnspentsJson)]
    pub fn list_unspents_json(&self, settled_only: bool) -> Result<String, JsValue> {
        let value = self.list_unspents_value(settled_only)?;
        let parsed: serde_json::Value = js_from(value)?;
        js_to_json(&parsed)
    }

    #[wasm_bindgen(js_name = blindReceiveValue)]
    pub fn blind_receive_value(
        &self,
        asset_id: Option<String>,
        assignment_js: JsValue,
        duration_seconds: Option<u32>,
        transport_endpoints_js: JsValue,
        min_confirmations: u8,
    ) -> Result<JsValue, JsValue> {
        if let Some(id) = &asset_id {
            if id.trim().is_empty() {
                return Err(JsValue::from_str("asset_id cannot be empty if provided"));
            }
        }
        let assignment: rgb_lib_wasm::Assignment = serde_wasm_bindgen::from_value(assignment_js)
            .map_err(|e| JsValue::from_str(&format!("Invalid assignment: {e}")))?;
        let transport_endpoints = self.resolve_transport_endpoints(transport_endpoints_js)?;
        let data = self
            .inner
            .borrow()
            .blind_receive(
                asset_id,
                assignment,
                duration_seconds,
                transport_endpoints,
                min_confirmations,
            )
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        js_obj(&data)
    }

    #[wasm_bindgen(js_name = blindReceiveJson)]
    pub fn blind_receive_json(
        &self,
        asset_id: Option<String>,
        assignment_js: JsValue,
        duration_seconds: Option<u32>,
        transport_endpoints_js: JsValue,
        min_confirmations: u8,
    ) -> Result<String, JsValue> {
        let value = self.blind_receive_value(
            asset_id,
            assignment_js,
            duration_seconds,
            transport_endpoints_js,
            min_confirmations,
        )?;
        let parsed: serde_json::Value = js_from(value)?;
        js_to_json(&parsed)
    }

    #[wasm_bindgen(js_name = witnessReceiveValue)]
    pub fn witness_receive_value(
        &self,
        asset_id: Option<String>,
        assignment_js: JsValue,
        duration_seconds: Option<u32>,
        transport_endpoints_js: JsValue,
        min_confirmations: u8,
    ) -> Result<JsValue, JsValue> {
        if let Some(id) = &asset_id {
            if id.trim().is_empty() {
                return Err(JsValue::from_str("asset_id cannot be empty if provided"));
            }
        }
        let assignment: rgb_lib_wasm::Assignment = serde_wasm_bindgen::from_value(assignment_js)
            .map_err(|e| JsValue::from_str(&format!("Invalid assignment: {e}")))?;
        let transport_endpoints = self.resolve_transport_endpoints(transport_endpoints_js)?;
        let data = self
            .inner
            .borrow_mut()
            .witness_receive(
                asset_id,
                assignment,
                duration_seconds,
                transport_endpoints,
                min_confirmations,
            )
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        js_obj(&data)
    }

    #[wasm_bindgen(js_name = witnessReceiveJson)]
    pub fn witness_receive_json(
        &self,
        asset_id: Option<String>,
        assignment_js: JsValue,
        duration_seconds: Option<u32>,
        transport_endpoints_js: JsValue,
        min_confirmations: u8,
    ) -> Result<String, JsValue> {
        let value = self.witness_receive_value(
            asset_id,
            assignment_js,
            duration_seconds,
            transport_endpoints_js,
            min_confirmations,
        )?;
        let parsed: serde_json::Value = js_from(value)?;
        js_to_json(&parsed)
    }

    #[wasm_bindgen(js_name = goOnlineValue)]
    pub async fn go_online_value(
        &self,
        skip_consistency_check: bool,
        indexer_url: String,
    ) -> Result<JsValue, JsValue> {
        if indexer_url.trim().is_empty() {
            return Err(JsValue::from_str("indexer_url cannot be empty"));
        }
        let mut wallet = self.inner.borrow_mut();
        let online = wallet
            .go_online(skip_consistency_check, indexer_url)
            .await
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        let mapped = WasmOnlineData {
            id: online.id.to_string(),
            indexer_url: online.indexer_url,
        };
        js_obj(&mapped)
    }

    #[wasm_bindgen(js_name = goOnlineJson)]
    pub async fn go_online_json(
        &self,
        skip_consistency_check: bool,
        indexer_url: String,
    ) -> Result<String, JsValue> {
        let value = self
            .go_online_value(skip_consistency_check, indexer_url)
            .await?;
        let parsed: WasmOnlineData = js_from(value)?;
        js_to_json(&parsed)
    }

    #[wasm_bindgen(js_name = syncOnline)]
    pub async fn sync_online(&self, online_js: JsValue) -> Result<(), JsValue> {
        let online = parse_online(online_js)?;
        let mut wallet = self.inner.borrow_mut();
        wallet
            .sync(online)
            .await
            .map_err(|e| JsValue::from_str(&e.to_string()))
    }

    #[wasm_bindgen(js_name = getFeeEstimation)]
    pub async fn get_fee_estimation(
        &self,
        online_js: JsValue,
        blocks: u16,
    ) -> Result<f64, JsValue> {
        let online = parse_online(online_js)?;
        let wallet = self.inner.borrow();
        wallet
            .get_fee_estimation(online, blocks)
            .await
            .map_err(|e| JsValue::from_str(&e.to_string()))
    }

    #[wasm_bindgen(js_name = getFeeEstimationJson)]
    pub async fn get_fee_estimation_json(
        &self,
        online_js: JsValue,
        blocks: u16,
    ) -> Result<String, JsValue> {
        let value = self.get_fee_estimation(online_js, blocks).await?;
        js_to_json(&value)
    }

    #[wasm_bindgen(js_name = createUtxosBegin)]
    pub async fn create_utxos_begin(
        &self,
        online_js: JsValue,
        up_to: bool,
        num: Option<u8>,
        size: Option<u32>,
        fee_rate: u64,
        skip_sync: bool,
    ) -> Result<String, JsValue> {
        let online = parse_online(online_js)?;
        let mut wallet = self.inner.borrow_mut();
        wallet
            .create_utxos_begin(online, up_to, num, size, fee_rate, skip_sync)
            .await
            .map_err(|e| JsValue::from_str(&e.to_string()))
    }

    #[wasm_bindgen(js_name = createUtxosEnd)]
    pub async fn create_utxos_end(
        &self,
        online_js: JsValue,
        signed_psbt: String,
        skip_sync: bool,
    ) -> Result<u8, JsValue> {
        let online = parse_online(online_js)?;
        let mut wallet = self.inner.borrow_mut();
        wallet
            .create_utxos_end(online, signed_psbt, skip_sync)
            .await
            .map_err(|e| JsValue::from_str(&e.to_string()))
    }

    #[wasm_bindgen(js_name = createUtxosEndJson)]
    pub async fn create_utxos_end_json(
        &self,
        online_js: JsValue,
        signed_psbt: String,
        skip_sync: bool,
    ) -> Result<String, JsValue> {
        let value = self
            .create_utxos_end(online_js, signed_psbt, skip_sync)
            .await?;
        js_to_json(&value)
    }

    #[wasm_bindgen(js_name = sendBegin)]
    pub async fn send_begin(
        &self,
        online_js: JsValue,
        recipient_map_js: JsValue,
        donation: bool,
        fee_rate: u64,
        min_confirmations: u8,
    ) -> Result<String, JsValue> {
        let online = parse_online(online_js)?;
        let recipient_map: HashMap<String, Vec<rgb_lib_wasm::wallet::Recipient>> =
            serde_wasm_bindgen::from_value(recipient_map_js)
                .map_err(|e| JsValue::from_str(&format!("Invalid recipient map: {e}")))?;
        let mut wallet = self.inner.borrow_mut();
        wallet
            .send_begin(online, recipient_map, donation, fee_rate, min_confirmations)
            .await
            .map_err(|e| JsValue::from_str(&e.to_string()))
    }

    #[wasm_bindgen(js_name = sendEndValue)]
    pub async fn send_end_value(
        &self,
        online_js: JsValue,
        signed_psbt: String,
        skip_sync: bool,
    ) -> Result<JsValue, JsValue> {
        let online = parse_online(online_js)?;
        let mut wallet = self.inner.borrow_mut();
        let result = wallet
            .send_end(online, signed_psbt, skip_sync)
            .await
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        js_obj(&result)
    }

    #[wasm_bindgen(js_name = sendEndJson)]
    pub async fn send_end_json(
        &self,
        online_js: JsValue,
        signed_psbt: String,
        skip_sync: bool,
    ) -> Result<String, JsValue> {
        let value = self
            .send_end_value(online_js, signed_psbt, skip_sync)
            .await?;
        let parsed: serde_json::Value = js_from(value)?;
        js_to_json(&parsed)
    }

    #[wasm_bindgen(js_name = sendBtcBegin)]
    pub async fn send_btc_begin(
        &self,
        online_js: JsValue,
        address: String,
        amount: u64,
        fee_rate: u64,
        skip_sync: bool,
    ) -> Result<String, JsValue> {
        if address.trim().is_empty() {
            return Err(JsValue::from_str("address cannot be empty"));
        }
        let online = parse_online(online_js)?;
        let mut wallet = self.inner.borrow_mut();
        wallet
            .send_btc_begin(online, address, amount, fee_rate, skip_sync)
            .await
            .map_err(|e| JsValue::from_str(&e.to_string()))
    }

    #[wasm_bindgen(js_name = sendBtcEnd)]
    pub async fn send_btc_end(
        &self,
        online_js: JsValue,
        signed_psbt: String,
        skip_sync: bool,
    ) -> Result<String, JsValue> {
        let online = parse_online(online_js)?;
        let mut wallet = self.inner.borrow_mut();
        wallet
            .send_btc_end(online, signed_psbt, skip_sync)
            .await
            .map_err(|e| JsValue::from_str(&e.to_string()))
    }

    #[wasm_bindgen(js_name = refreshValue)]
    pub async fn refresh_value(
        &self,
        online_js: JsValue,
        asset_id: Option<String>,
        filter_js: JsValue,
        skip_sync: bool,
    ) -> Result<JsValue, JsValue> {
        if let Some(id) = &asset_id {
            if id.trim().is_empty() {
                return Err(JsValue::from_str("asset_id cannot be empty if provided"));
            }
        }
        let online = parse_online(online_js)?;
        let filter: Vec<rgb_lib_wasm::wallet::RefreshFilter> =
            serde_wasm_bindgen::from_value(filter_js)
                .map_err(|e| JsValue::from_str(&format!("Invalid filter: {e}")))?;
        let mut wallet = self.inner.borrow_mut();
        let result = wallet
            .refresh(online, asset_id, filter, skip_sync)
            .await
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        js_obj(&result)
    }

    #[wasm_bindgen(js_name = refreshJson)]
    pub async fn refresh_json(
        &self,
        online_js: JsValue,
        asset_id: Option<String>,
        filter_js: JsValue,
        skip_sync: bool,
    ) -> Result<String, JsValue> {
        let value = self
            .refresh_value(online_js, asset_id, filter_js, skip_sync)
            .await?;
        let parsed: serde_json::Value = js_from(value)?;
        js_to_json(&parsed)
    }

    #[wasm_bindgen(js_name = failTransfers)]
    pub async fn fail_transfers(
        &self,
        online_js: JsValue,
        batch_transfer_idx: Option<i32>,
        no_asset_only: bool,
        skip_sync: bool,
    ) -> Result<bool, JsValue> {
        let online = parse_online(online_js)?;
        let mut wallet = self.inner.borrow_mut();
        wallet
            .fail_transfers(online, batch_transfer_idx, no_asset_only, skip_sync)
            .await
            .map_err(|e| JsValue::from_str(&e.to_string()))
    }

    #[wasm_bindgen(js_name = failTransfersJson)]
    pub async fn fail_transfers_json(
        &self,
        online_js: JsValue,
        batch_transfer_idx: Option<i32>,
        no_asset_only: bool,
        skip_sync: bool,
    ) -> Result<String, JsValue> {
        let value = self
            .fail_transfers(online_js, batch_transfer_idx, no_asset_only, skip_sync)
            .await?;
        js_to_json(&value)
    }

    #[wasm_bindgen(js_name = drainToBegin)]
    pub async fn drain_to_begin(
        &self,
        online_js: JsValue,
        address: String,
        destroy_assets: bool,
        fee_rate: u64,
    ) -> Result<String, JsValue> {
        if address.trim().is_empty() {
            return Err(JsValue::from_str("address cannot be empty"));
        }
        let online = parse_online(online_js)?;
        let mut wallet = self.inner.borrow_mut();
        wallet
            .drain_to_begin(online, address, destroy_assets, fee_rate)
            .await
            .map_err(|e| JsValue::from_str(&e.to_string()))
    }

    #[wasm_bindgen(js_name = drainToEnd)]
    pub async fn drain_to_end(
        &self,
        online_js: JsValue,
        signed_psbt: String,
    ) -> Result<String, JsValue> {
        let online = parse_online(online_js)?;
        let mut wallet = self.inner.borrow_mut();
        wallet
            .drain_to_end(online, signed_psbt)
            .await
            .map_err(|e| JsValue::from_str(&e.to_string()))
    }

    #[wasm_bindgen(js_name = inflateBegin)]
    pub async fn inflate_begin(
        &self,
        online_js: JsValue,
        asset_id: String,
        inflation_amounts_js: JsValue,
        fee_rate: u64,
        min_confirmations: u8,
    ) -> Result<String, JsValue> {
        if asset_id.trim().is_empty() {
            return Err(JsValue::from_str("asset_id cannot be empty"));
        }
        let online = parse_online(online_js)?;
        let inflation_amounts: Vec<u64> = serde_wasm_bindgen::from_value(inflation_amounts_js)
            .map_err(|e| JsValue::from_str(&format!("Invalid inflation_amounts array: {e}")))?;
        let mut wallet = self.inner.borrow_mut();
        wallet
            .inflate_begin(
                online,
                asset_id,
                inflation_amounts,
                fee_rate,
                min_confirmations,
            )
            .await
            .map_err(|e| JsValue::from_str(&e.to_string()))
    }

    #[wasm_bindgen(js_name = inflateEndValue)]
    pub async fn inflate_end_value(
        &self,
        online_js: JsValue,
        signed_psbt: String,
    ) -> Result<JsValue, JsValue> {
        let online = parse_online(online_js)?;
        let mut wallet = self.inner.borrow_mut();
        let result = wallet
            .inflate_end(online, signed_psbt)
            .await
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        js_obj(&result)
    }

    #[wasm_bindgen(js_name = inflateEndJson)]
    pub async fn inflate_end_json(
        &self,
        online_js: JsValue,
        signed_psbt: String,
    ) -> Result<String, JsValue> {
        let value = self.inflate_end_value(online_js, signed_psbt).await?;
        let parsed: serde_json::Value = js_from(value)?;
        js_to_json(&parsed)
    }

    #[wasm_bindgen(js_name = listUnspentsVanillaValue)]
    pub async fn list_unspents_vanilla_value(
        &self,
        online_js: JsValue,
        min_confirmations: u8,
        skip_sync: bool,
    ) -> Result<JsValue, JsValue> {
        let online = parse_online(online_js)?;
        let mut wallet = self.inner.borrow_mut();
        let unspents = wallet
            .list_unspents_vanilla(online, min_confirmations, skip_sync)
            .await
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        js_obj(&unspents)
    }

    #[wasm_bindgen(js_name = listUnspentsVanillaJson)]
    pub async fn list_unspents_vanilla_json(
        &self,
        online_js: JsValue,
        min_confirmations: u8,
        skip_sync: bool,
    ) -> Result<String, JsValue> {
        let value = self
            .list_unspents_vanilla_value(online_js, min_confirmations, skip_sync)
            .await?;
        let parsed: serde_json::Value = js_from(value)?;
        js_to_json(&parsed)
    }

    #[wasm_bindgen(js_name = backup)]
    pub fn backup(&self, password: String) -> Result<Vec<u8>, JsValue> {
        if password.is_empty() {
            return Err(JsValue::from_str("password cannot be empty"));
        }
        self.inner
            .borrow()
            .backup(&password)
            .map_err(|e| JsValue::from_str(&e.to_string()))
    }

    #[wasm_bindgen(js_name = restoreBackup)]
    pub fn restore_backup(&self, backup_bytes: Vec<u8>, password: String) -> Result<(), JsValue> {
        if password.is_empty() {
            return Err(JsValue::from_str("password cannot be empty"));
        }
        self.inner
            .borrow_mut()
            .restore_backup(&backup_bytes, &password)
            .map_err(|e| JsValue::from_str(&e.to_string()))
    }

    #[wasm_bindgen(js_name = backupInfo)]
    pub fn backup_info(&self) -> Result<bool, JsValue> {
        self.inner
            .borrow()
            .backup_info()
            .map_err(|e| JsValue::from_str(&e.to_string()))
    }

    #[wasm_bindgen(js_name = backupInfoJson)]
    pub fn backup_info_json(&self) -> Result<String, JsValue> {
        let value = self.backup_info()?;
        js_to_json(&value)
    }

    #[wasm_bindgen(js_name = configureVssBackup)]
    pub fn configure_vss_backup(
        &self,
        server_url: String,
        store_id: String,
        signing_key_hex: String,
    ) -> Result<(), JsValue> {
        if server_url.trim().is_empty() {
            return Err(JsValue::from_str("server_url cannot be empty"));
        }
        if store_id.trim().is_empty() {
            return Err(JsValue::from_str("store_id cannot be empty"));
        }
        if signing_key_hex.len() != 64 {
            return Err(JsValue::from_str(&format!(
                "signing_key_hex must be exactly 64 hex chars (32 bytes), got {}",
                signing_key_hex.len()
            )));
        }
        let key_bytes = hex::decode(signing_key_hex)
            .map_err(|e| JsValue::from_str(&format!("Invalid signing key hex: {e}")))?;
        let signing_key =
            rgb_lib_wasm::bdk_wallet::bitcoin::secp256k1::SecretKey::from_slice(&key_bytes)
                .map_err(|e| JsValue::from_str(&format!("Invalid signing key: {e}")))?;
        let config =
            rgb_lib_wasm::wallet::vss::VssBackupConfig::new(server_url, store_id, signing_key);
        self.inner.borrow_mut().configure_vss_backup(&config);
        Ok(())
    }

    #[wasm_bindgen(js_name = disableVssBackup)]
    pub fn disable_vss_backup(&self) {
        self.inner.borrow_mut().disable_vss_backup();
    }

    #[wasm_bindgen(js_name = vssBackupValue)]
    pub async fn vss_backup_value(&self) -> Result<JsValue, JsValue> {
        let wallet = self.inner.borrow();
        let version = wallet
            .vss_backup()
            .await
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        Ok(JsValue::from_f64(version as f64))
    }

    #[wasm_bindgen(js_name = vssBackupJson)]
    pub async fn vss_backup_json(&self) -> Result<String, JsValue> {
        let value = self.vss_backup_value().await?;
        let parsed: serde_json::Value = js_from(value)?;
        js_to_json(&parsed)
    }

    #[wasm_bindgen(js_name = vssRestoreBackup)]
    pub async fn vss_restore_backup(&self) -> Result<(), JsValue> {
        self.inner
            .borrow_mut()
            .vss_restore_backup()
            .await
            .map_err(|e| JsValue::from_str(&e.to_string()))
    }

    #[wasm_bindgen(js_name = vssBackupInfoValue)]
    pub async fn vss_backup_info_value(&self) -> Result<JsValue, JsValue> {
        let wallet = self.inner.borrow();
        let info = wallet
            .vss_backup_info()
            .await
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        js_obj(&info)
    }

    #[wasm_bindgen(js_name = vssBackupInfoJson)]
    pub async fn vss_backup_info_json(&self) -> Result<String, JsValue> {
        let value = self.vss_backup_info_value().await?;
        let parsed: serde_json::Value = js_from(value)?;
        js_to_json(&parsed)
    }
}

#[wasm_bindgen]
pub struct RlnWasmInvoice {
    inner: rgb_lib_wasm::wallet::Invoice,
}

#[wasm_bindgen]
impl RlnWasmInvoice {
    #[wasm_bindgen(constructor)]
    pub fn new(invoice_string: String) -> Result<RlnWasmInvoice, JsValue> {
        if invoice_string.trim().is_empty() {
            return Err(JsValue::from_str("invoice_string cannot be empty"));
        }
        let invoice = rgb_lib_wasm::wallet::Invoice::new(invoice_string)
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        Ok(Self { inner: invoice })
    }

    #[wasm_bindgen(js_name = invoiceDataValue)]
    pub fn invoice_data_value(&self) -> Result<JsValue, JsValue> {
        let data = self.inner.invoice_data();
        js_obj(&data)
    }

    #[wasm_bindgen(js_name = invoiceDataJson)]
    pub fn invoice_data_json(&self) -> Result<String, JsValue> {
        let value = self.invoice_data_value()?;
        let parsed: serde_json::Value = js_from(value)?;
        js_to_json(&parsed)
    }

    #[wasm_bindgen(js_name = invoiceString)]
    pub fn invoice_string(&self) -> String {
        self.inner.invoice_string()
    }
}

#[wasm_bindgen(js_name = checkProxyUrl)]
pub async fn check_proxy_url(proxy_url: String) -> Result<(), JsValue> {
    if proxy_url.trim().is_empty() {
        return Err(JsValue::from_str("proxy_url cannot be empty"));
    }
    rgb_lib_wasm::wallet::rust_only::check_proxy_url(&proxy_url)
        .await
        .map_err(|e| JsValue::from_str(&e.to_string()))
}

#[wasm_bindgen(js_name = checkIndexerUrlValue)]
pub fn check_indexer_url_value(network: String, indexer_url: String) -> Result<JsValue, JsValue> {
    let _ = WasmRlnNetwork::parse(&network)?;
    let protocol = detect_wasm_indexer_protocol(&indexer_url)?;
    js_obj(&CheckIndexerUrlData {
        indexer_protocol: protocol.to_string(),
    })
}

#[wasm_bindgen(js_name = checkIndexerUrlJson)]
pub fn check_indexer_url_json(network: String, indexer_url: String) -> Result<String, JsValue> {
    let value = check_indexer_url_value(network, indexer_url)?;
    let parsed: serde_json::Value = js_from(value)?;
    js_to_json(&parsed)
}

#[wasm_bindgen(js_name = checkLnPeerWebsocketValue)]
pub async fn check_ln_peer_websocket_value(
    proxy_url: String,
    peer_addr: String,
) -> Result<JsValue, JsValue> {
    let websocket_url = proxy_url_for_peer(&proxy_url, &peer_addr)?;
    let socket = WebSocket::open(&websocket_url).map_err(|e| {
        JsValue::from_str(&format!("failed to open websocket to proxy endpoint: {e}"))
    })?;
    drop(socket);

    js_obj(&LnPeerWebsocketCheckData {
        proxy_url,
        peer_addr,
        websocket_url,
    })
}

#[wasm_bindgen(js_name = checkLnPeerWebsocketJson)]
pub async fn check_ln_peer_websocket_json(
    proxy_url: String,
    peer_addr: String,
) -> Result<String, JsValue> {
    let value = check_ln_peer_websocket_value(proxy_url, peer_addr).await?;
    let parsed: serde_json::Value = js_from(value)?;
    js_to_json(&parsed)
}

#[cfg(all(test, target_arch = "wasm32"))]
#[path = "tests/sdk_contract_tests.rs"]
mod sdk_contract_tests;
#[cfg(all(test, target_arch = "wasm32"))]
#[path = "tests/wallet_contract_tests.rs"]
mod wallet_contract_tests;
#[cfg(all(test, target_arch = "wasm32"))]
#[path = "tests/wasm_contract_tests.rs"]
mod wasm_contract_tests;
