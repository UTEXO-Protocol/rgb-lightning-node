use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

#[cfg(target_arch = "wasm32")]
use gloo_net::http::Request;
use serde::{Deserialize, Serialize};
use wasm_bindgen::prelude::JsValue;
#[cfg(target_arch = "wasm32")]
use wasm_bindgen_futures::spawn_local;

use crate::runtime_store::{browser_persistent_state_store, RuntimeStateStore};
use crate::wasm_node_persistence::{
    WASM_CHAIN_SYNC_STORAGE_PREFIX, WASM_LDK_BROADCAST_QUEUE_STORAGE_PREFIX,
};

#[cfg(test)]
#[path = "tests/chain_sync_test_utils.rs"]
pub(crate) mod test_utils;
#[cfg(test)]
#[path = "tests/chain_sync_tests.rs"]
mod tests;

const CHAIN_SYNC_SCHEMA_VERSION: u32 = 1;
const CHAIN_SYNC_DEFAULT_POLL_INTERVAL_MS: u32 = 10_000;
const CHAIN_SYNC_MIN_POLL_INTERVAL_MS: u32 = 1_000;
const CHAIN_SYNC_MAX_POLL_INTERVAL_MS: u32 = 60_000;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RlnWasmChainRebroadcastTxData {
    pub txid: String,
    pub tx_hex: String,
    pub attempts: u32,
    pub last_broadcast_at: Option<u64>,
    pub confirmed_at: Option<u64>,
    pub last_error: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RlnWasmChainSyncStatusData {
    pub network: String,
    pub indexer_url: Option<String>,
    pub running: bool,
    pub poll_interval_ms: u32,
    pub latest_tip_height: Option<u32>,
    pub last_tip_at: Option<u64>,
    pub tip_regressed: bool,
    pub last_tip_regression_at: Option<u64>,
    pub last_tick_at: Option<u64>,
    pub rebroadcast_pending: usize,
    pub rebroadcast_confirmed: usize,
    pub last_error: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct ChainSyncSnapshot {
    schema_version: u32,
    network: String,
    indexer_url: Option<String>,
    running: bool,
    poll_interval_ms: u32,
    latest_tip_height: Option<u32>,
    last_tip_at: Option<u64>,
    #[serde(default)]
    tip_regressed: bool,
    #[serde(default)]
    last_tip_regression_at: Option<u64>,
    last_tick_at: Option<u64>,
    last_error: Option<String>,
    rebroadcast_queue: Vec<RlnWasmChainRebroadcastTxData>,
}

impl ChainSyncSnapshot {
    fn default_for_network(network: String) -> Self {
        Self {
            schema_version: CHAIN_SYNC_SCHEMA_VERSION,
            network,
            indexer_url: None,
            running: false,
            poll_interval_ms: CHAIN_SYNC_DEFAULT_POLL_INTERVAL_MS,
            latest_tip_height: None,
            last_tip_at: None,
            tip_regressed: false,
            last_tip_regression_at: None,
            last_tick_at: None,
            last_error: None,
            rebroadcast_queue: Vec::new(),
        }
    }
}

thread_local! {
    static CHAIN_SYNC_FALLBACK_STORAGE: RefCell<HashMap<String, ChainSyncSnapshot>> =
        RefCell::new(HashMap::new());
}

#[derive(Clone)]
pub struct WasmChainSyncDriver {
    storage_key: String,
    broadcast_queue_key: String,
    state: Rc<RefCell<ChainSyncSnapshot>>,
    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    loop_active: Rc<Cell<bool>>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct PendingBroadcastTx {
    txid: String,
    tx_hex: String,
}

impl WasmChainSyncDriver {
    pub fn new(runtime_key: String, default_network: String) -> Result<Self, JsValue> {
        let storage_key = format!("{WASM_CHAIN_SYNC_STORAGE_PREFIX}{runtime_key}");
        let broadcast_queue_key = format!("{WASM_LDK_BROADCAST_QUEUE_STORAGE_PREFIX}{runtime_key}");
        let loaded = load_snapshot(&storage_key)?;
        let mut snapshot = loaded
            .unwrap_or_else(|| ChainSyncSnapshot::default_for_network(default_network.clone()));
        if snapshot.schema_version > CHAIN_SYNC_SCHEMA_VERSION {
            snapshot = ChainSyncSnapshot::default_for_network(default_network);
        }
        if snapshot.poll_interval_ms < CHAIN_SYNC_MIN_POLL_INTERVAL_MS
            || snapshot.poll_interval_ms > CHAIN_SYNC_MAX_POLL_INTERVAL_MS
        {
            snapshot.poll_interval_ms = CHAIN_SYNC_DEFAULT_POLL_INTERVAL_MS;
        }
        let driver = Self {
            storage_key,
            broadcast_queue_key,
            state: Rc::new(RefCell::new(snapshot)),
            loop_active: Rc::new(Cell::new(false)),
        };
        driver.persist()?;
        if driver.is_running() {
            driver.ensure_background_loop();
        }
        Ok(driver)
    }

    pub fn status(&self) -> RlnWasmChainSyncStatusData {
        let snapshot = self.state.borrow();
        let rebroadcast_pending = snapshot
            .rebroadcast_queue
            .iter()
            .filter(|entry| entry.confirmed_at.is_none())
            .count();
        let rebroadcast_confirmed = snapshot
            .rebroadcast_queue
            .iter()
            .filter(|entry| entry.confirmed_at.is_some())
            .count();
        RlnWasmChainSyncStatusData {
            network: snapshot.network.clone(),
            indexer_url: snapshot.indexer_url.clone(),
            running: snapshot.running,
            poll_interval_ms: snapshot.poll_interval_ms,
            latest_tip_height: snapshot.latest_tip_height,
            last_tip_at: snapshot.last_tip_at,
            tip_regressed: snapshot.tip_regressed,
            last_tip_regression_at: snapshot.last_tip_regression_at,
            last_tick_at: snapshot.last_tick_at,
            rebroadcast_pending,
            rebroadcast_confirmed,
            last_error: snapshot.last_error.clone(),
        }
    }

    pub fn latest_tip_height(&self) -> Option<u32> {
        self.state.borrow().latest_tip_height
    }

    pub fn start(&self, indexer_url: String, poll_interval_ms: Option<u32>) -> Result<(), JsValue> {
        let indexer_url = normalize_indexer_url(&indexer_url)?;
        let poll_interval = normalize_poll_interval_ms(poll_interval_ms);
        {
            let mut state = self.state.borrow_mut();
            state.running = true;
            state.indexer_url = Some(indexer_url);
            state.poll_interval_ms = poll_interval;
            state.last_error = None;
        }
        self.persist()?;
        self.ensure_background_loop();
        Ok(())
    }

    pub fn stop(&self) -> Result<(), JsValue> {
        self.state.borrow_mut().running = false;
        self.persist()
    }

    pub fn enqueue_rebroadcast_tx(&self, txid: String, tx_hex: String) -> Result<(), JsValue> {
        let txid = txid.trim().to_lowercase();
        if txid.len() != 64 || !txid.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(JsValue::from_str("txid must be 64 hex chars"));
        }
        let tx_hex = tx_hex.trim().to_lowercase();
        if tx_hex.is_empty() || !tx_hex.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(JsValue::from_str("tx_hex must be non-empty hex"));
        }
        let mut state = self.state.borrow_mut();
        if let Some(existing) = state
            .rebroadcast_queue
            .iter_mut()
            .find(|entry| entry.txid == txid)
        {
            existing.tx_hex = tx_hex;
            existing.last_error = None;
            if existing.confirmed_at.is_some() {
                existing.confirmed_at = None;
            }
            drop(state);
            return self.persist();
        }
        state.rebroadcast_queue.push(RlnWasmChainRebroadcastTxData {
            txid,
            tx_hex,
            attempts: 0,
            last_broadcast_at: None,
            confirmed_at: None,
            last_error: None,
        });
        drop(state);
        self.persist()
    }

    pub async fn tick(&self) -> Result<(), JsValue> {
        let (running, indexer_url) = {
            let state = self.state.borrow();
            (state.running, state.indexer_url.clone())
        };
        if !running {
            return Err(JsValue::from_str(
                "chain sync is not running; call chainSyncStart first",
            ));
        }
        let indexer_url = indexer_url
            .ok_or_else(|| JsValue::from_str("chain sync has no indexer url configured"))?;
        self.tick_with_indexer(indexer_url).await
    }

    fn is_running(&self) -> bool {
        self.state.borrow().running
    }

    fn poll_interval_ms(&self) -> u32 {
        self.state.borrow().poll_interval_ms
    }

    fn ensure_background_loop(&self) {
        #[cfg(not(target_arch = "wasm32"))]
        {
            return;
        }
        #[cfg(target_arch = "wasm32")]
        {
            if self.loop_active.get() {
                return;
            }
            self.loop_active.set(true);
            let driver = self.clone();
            spawn_local(async move {
                loop {
                    if !driver.is_running() {
                        break;
                    }
                    let indexer_url = driver.state.borrow().indexer_url.clone();
                    if let Some(indexer_url) = indexer_url {
                        let _ = driver.tick_with_indexer(indexer_url).await;
                    }
                    sleep_ms(driver.poll_interval_ms()).await;
                }
                driver.loop_active.set(false);
            });
        }
    }

    async fn tick_with_indexer(&self, indexer_url: String) -> Result<(), JsValue> {
        let now = unix_now_secs();

        // Ingest pending LDK-triggered broadcasts (written by LDK `BroadcasterInterface`)
        // into our stable rebroadcast queue before we query tip/confirmations.
        self.ingest_pending_broadcasts()?;

        let tip_height = match fetch_tip_height(&indexer_url).await {
            Ok(height) => height,
            Err(err) => {
                let mut state = self.state.borrow_mut();
                state.last_tick_at = Some(now);
                state.last_error = Some(err.clone());
                drop(state);
                self.persist()?;
                return Err(JsValue::from_str(&err));
            }
        };

        self.apply_tip_height(tip_height, now);
        self.persist()?;

        let mut queue = self.state.borrow().rebroadcast_queue.clone();
        let poll_interval_sec = (self.poll_interval_ms() / 1000).max(1) as u64;
        let mut global_error: Option<String> = None;

        for entry in queue.iter_mut() {
            if entry.confirmed_at.is_some() {
                continue;
            }
            match fetch_tx_confirmed(&indexer_url, &entry.txid).await {
                Ok(true) => {
                    entry.confirmed_at = Some(now);
                    entry.last_error = None;
                    continue;
                }
                Ok(false) => {}
                Err(err) => {
                    entry.last_error = Some(err.clone());
                    global_error = Some(err);
                    continue;
                }
            }
            let should_rebroadcast = entry
                .last_broadcast_at
                .map(|last| now.saturating_sub(last) >= poll_interval_sec)
                .unwrap_or(true);
            if !should_rebroadcast {
                continue;
            }
            match broadcast_tx(&indexer_url, &entry.tx_hex).await {
                Ok(()) => {
                    entry.attempts = entry.attempts.saturating_add(1);
                    entry.last_broadcast_at = Some(now);
                    entry.last_error = None;
                }
                Err(err) => {
                    entry.attempts = entry.attempts.saturating_add(1);
                    entry.last_broadcast_at = Some(now);
                    entry.last_error = Some(err.clone());
                    global_error = Some(err);
                }
            }
        }

        {
            let mut state = self.state.borrow_mut();
            state.rebroadcast_queue = queue;
            state.last_error = global_error;
            state.last_tick_at = Some(now);
        }
        self.persist()
    }

    fn ingest_pending_broadcasts(&self) -> Result<(), JsValue> {
        let store = browser_persistent_state_store();
        let Some(raw) = store.get(&self.broadcast_queue_key)? else {
            return Ok(());
        };
        let pending: Vec<PendingBroadcastTx> = match serde_json::from_str(&raw) {
            Ok(v) => v,
            Err(_) => {
                // If the queue is corrupted, drop it to avoid wedging chain sync forever.
                store.delete(&self.broadcast_queue_key)?;
                return Ok(());
            }
        };
        if pending.is_empty() {
            store.delete(&self.broadcast_queue_key)?;
            return Ok(());
        }
        for entry in pending {
            // If a single entry is malformed, ignore it; the rest still get ingested.
            let _ = self.enqueue_rebroadcast_tx(entry.txid, entry.tx_hex);
        }
        store.delete(&self.broadcast_queue_key)?;
        Ok(())
    }

    fn persist(&self) -> Result<(), JsValue> {
        let snapshot = self.state.borrow().clone();
        save_snapshot(&self.storage_key, &snapshot)
    }

    fn apply_tip_height(&self, tip_height: u32, now: u64) {
        let mut state = self.state.borrow_mut();
        let prev = state.latest_tip_height;
        let regressed = prev.map(|h| tip_height < h).unwrap_or(false);
        state.latest_tip_height = Some(tip_height);
        state.last_tip_at = Some(now);
        state.last_tick_at = Some(now);
        state.last_error = None;
        state.tip_regressed = regressed;
        if regressed {
            state.last_tip_regression_at = Some(now);
        }
    }
}

fn normalize_poll_interval_ms(poll_interval_ms: Option<u32>) -> u32 {
    let value = poll_interval_ms.unwrap_or(CHAIN_SYNC_DEFAULT_POLL_INTERVAL_MS);
    value.clamp(
        CHAIN_SYNC_MIN_POLL_INTERVAL_MS,
        CHAIN_SYNC_MAX_POLL_INTERVAL_MS,
    )
}

fn normalize_indexer_url(indexer_url: &str) -> Result<String, JsValue> {
    let value = indexer_url.trim().trim_end_matches('/').to_string();
    if value.is_empty() {
        return Err(JsValue::from_str(sdk_contracts::ERR_INDEXER_URL_EMPTY));
    }
    if !(value.starts_with("http://") || value.starts_with("https://")) {
        return Err(JsValue::from_str(
            "indexer_url must start with http:// or https://",
        ));
    }
    Ok(value)
}

#[cfg(target_arch = "wasm32")]
async fn fetch_tip_height(indexer_url: &str) -> Result<u32, String> {
    let url = format!("{indexer_url}/blocks/tip/height");
    let response = Request::get(&url).send().await.map_err(|e| e.to_string())?;
    if !response.ok() {
        return Err(format!(
            "tip query failed with status {}",
            response.status()
        ));
    }
    let text = response.text().await.map_err(|e| e.to_string())?;
    text.trim()
        .parse::<u32>()
        .map_err(|e| format!("invalid tip height response: {e}"))
}

#[cfg(not(target_arch = "wasm32"))]
async fn fetch_tip_height(_indexer_url: &str) -> Result<u32, String> {
    Err("tip polling is only available in wasm32 runtime".to_string())
}

#[cfg(target_arch = "wasm32")]
async fn fetch_tx_confirmed(indexer_url: &str, txid: &str) -> Result<bool, String> {
    let url = format!("{indexer_url}/tx/{txid}/status");
    let response = Request::get(&url).send().await.map_err(|e| e.to_string())?;
    if response.status() == 404 {
        return Ok(false);
    }
    if !response.ok() {
        return Err(format!(
            "tx status query failed for {txid} with status {}",
            response.status()
        ));
    }
    let json: serde_json::Value = response.json().await.map_err(|e| e.to_string())?;
    Ok(json
        .get("confirmed")
        .and_then(|value| value.as_bool())
        .unwrap_or(false))
}

#[cfg(not(target_arch = "wasm32"))]
async fn fetch_tx_confirmed(_indexer_url: &str, _txid: &str) -> Result<bool, String> {
    Err("tx status queries are only available in wasm32 runtime".to_string())
}

#[cfg(target_arch = "wasm32")]
async fn broadcast_tx(indexer_url: &str, tx_hex: &str) -> Result<(), String> {
    let url = format!("{indexer_url}/tx");
    let response = Request::post(&url)
        .header("Content-Type", "text/plain")
        .body(tx_hex.to_string())
        .map_err(|e| e.to_string())?
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if response.ok() {
        return Ok(());
    }
    let body = response.text().await.unwrap_or_default();
    Err(format!(
        "broadcast failed with status {}{}{}",
        response.status(),
        if body.is_empty() { "" } else { ": " },
        body
    ))
}

#[cfg(not(target_arch = "wasm32"))]
async fn broadcast_tx(_indexer_url: &str, _tx_hex: &str) -> Result<(), String> {
    Err("broadcast is only available in wasm32 runtime".to_string())
}

fn unix_now_secs() -> u64 {
    #[cfg(target_arch = "wasm32")]
    {
        (js_sys::Date::now() as u64) / 1000
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        use std::time::{SystemTime, UNIX_EPOCH};
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    }
}

#[cfg(target_arch = "wasm32")]
async fn sleep_ms(ms: u32) {
    let promise = js_sys::Promise::new(&mut |resolve, _reject| {
        if let Some(window) = web_sys::window() {
            let _ =
                window.set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, ms as i32);
        } else {
            let _ = resolve.call0(&JsValue::NULL);
        }
    });
    let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
}

fn load_snapshot(key: &str) -> Result<Option<ChainSyncSnapshot>, JsValue> {
    if let Some(raw) = browser_persistent_state_store().get(key)? {
        if let Ok(snapshot) = serde_json::from_str::<ChainSyncSnapshot>(&raw) {
            return Ok(Some(snapshot));
        }
    }
    Ok(CHAIN_SYNC_FALLBACK_STORAGE.with(|storage| storage.borrow().get(key).cloned()))
}

fn save_snapshot(key: &str, snapshot: &ChainSyncSnapshot) -> Result<(), JsValue> {
    let encoded = serde_json::to_string(snapshot).map_err(|e| JsValue::from_str(&e.to_string()))?;
    browser_persistent_state_store().set(key, &encoded)?;
    CHAIN_SYNC_FALLBACK_STORAGE.with(|storage| {
        storage
            .borrow_mut()
            .insert(key.to_string(), snapshot.clone());
    });
    Ok(())
}
