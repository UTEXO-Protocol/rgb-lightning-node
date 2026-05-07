use bitcoin_hashes::sha256::Hash as Sha256;
use bitcoin_hashes::Hash as _;
use secp256k1::PublicKey;
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::collections::HashMap;
use std::str::FromStr;
use wasm_bindgen::prelude::JsValue;

use crate::runtime_store::{browser_persistent_state_store, RuntimeStateStore};

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct MakerInitRequestData {
    pub qty_from: u64,
    pub qty_to: u64,
    pub from_asset: Option<String>,
    pub to_asset: Option<String>,
    pub timeout_sec: u32,
}

#[derive(Clone, Debug, Deserialize)]
struct TakerRequestData {
    swapstring: String,
}

#[derive(Clone, Debug, Deserialize)]
struct MakerExecuteRequestData {
    swapstring: String,
    payment_secret: String,
    taker_pubkey: String,
}

#[derive(Clone, Debug, Deserialize)]
struct GetSwapRequestData {
    payment_hash: String,
    taker: bool,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct MakerInitResponseData {
    pub payment_hash: String,
    pub payment_secret: String,
    pub swapstring: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) enum SwapStatus {
    Waiting,
    Pending,
    Succeeded,
    Expired,
    Failed,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct SwapData {
    pub qty_from: u64,
    pub qty_to: u64,
    pub from_asset: Option<String>,
    pub to_asset: Option<String>,
    pub payment_hash: String,
    pub status: SwapStatus,
    pub requested_at: u64,
    pub initiated_at: Option<u64>,
    pub expires_at: u64,
    pub completed_at: Option<u64>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct SwapListData {
    pub taker: Vec<SwapData>,
    pub maker: Vec<SwapData>,
}

#[derive(Clone, Debug, Serialize)]
struct GetSwapResponseData {
    swap: SwapData,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct SwapStringData {
    v: u8,
    payment_hash: String,
    qty_from: u64,
    qty_to: u64,
    from_asset: Option<String>,
    to_asset: Option<String>,
    expires_at: u64,
}

#[derive(Clone, Debug, Default)]
struct SwapRuntimeState {
    maker: HashMap<String, SwapData>,
    taker: HashMap<String, SwapData>,
    sequence: u64,
    loaded: bool,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct SwapRuntimeSnapshot {
    maker: Vec<SwapData>,
    taker: Vec<SwapData>,
    sequence: u64,
}

thread_local! {
    static WASM_SWAP_RUNTIME_STATE: RefCell<SwapRuntimeState> =
        RefCell::new(SwapRuntimeState::default());
}

const SWAP_STRING_PREFIX: &str = "rln-wasm-swap:";
const SWAP_STATE_STORAGE_KEY_PREFIX: &str = "rln:wasm:swap-runtime:v1:";

fn swap_state_storage_key() -> String {
    let identity = crate::sdk_node_identity_seed().unwrap_or_else(|| "ephemeral".to_string());
    let digest = Sha256::hash(identity.as_bytes()).to_string();
    format!("{SWAP_STATE_STORAGE_KEY_PREFIX}{digest}")
}

#[cfg(test)]
#[path = "tests/swap_runtime_test_utils.rs"]
pub(crate) mod test_utils;

fn now_secs() -> u64 {
    (js_sys::Date::now() as u64) / 1000
}

fn snapshot_from_state(state: &SwapRuntimeState) -> SwapRuntimeSnapshot {
    SwapRuntimeSnapshot {
        maker: state.maker.values().cloned().collect(),
        taker: state.taker.values().cloned().collect(),
        sequence: state.sequence,
    }
}

fn hydrate_state(state: &mut SwapRuntimeState, snapshot: SwapRuntimeSnapshot) {
    state.maker.clear();
    state.taker.clear();
    for swap in snapshot.maker {
        state.maker.insert(swap.payment_hash.clone(), swap);
    }
    for swap in snapshot.taker {
        state.taker.insert(swap.payment_hash.clone(), swap);
    }
    state.sequence = snapshot.sequence;
}

fn ensure_swap_runtime_loaded(state: &mut SwapRuntimeState) {
    if state.loaded {
        return;
    }
    let storage_key = swap_state_storage_key();
    let store = browser_persistent_state_store();
    if let Ok(Some(raw)) = store.get(&storage_key) {
        if let Ok(snapshot) = serde_json::from_str::<SwapRuntimeSnapshot>(&raw) {
            hydrate_state(state, snapshot);
        }
    }
    state.loaded = true;
}

fn persist_swap_runtime_state(state: &SwapRuntimeState) {
    let store = browser_persistent_state_store();
    let storage_key = swap_state_storage_key();
    if let Ok(raw) = serde_json::to_string(&snapshot_from_state(state)) {
        let _ = store.set(&storage_key, &raw);
    }
}

fn parse_optional_asset(asset: Option<String>) -> Result<Option<String>, JsValue> {
    match asset {
        Some(asset_id) => {
            let normalized = asset_id.trim().to_string();
            if normalized.is_empty() {
                return Err(JsValue::from_str(sdk_contracts::ERR_ASSET_ID_EMPTY));
            }
            if normalized.len() != 64 || !normalized.chars().all(|c| c.is_ascii_hexdigit()) {
                return Err(JsValue::from_str(sdk_contracts::ERR_ASSET_ID_INVALID));
            }
            Ok(Some(normalized))
        }
        None => Ok(None),
    }
}

fn validate_swap_pair(
    from_asset: &Option<String>,
    to_asset: &Option<String>,
) -> Result<(), JsValue> {
    if from_asset.is_none() && to_asset.is_none() {
        return Err(JsValue::from_str(sdk_contracts::ERR_SWAP_BTC_FOR_BTC));
    }
    if from_asset.is_some() && from_asset == to_asset {
        return Err(JsValue::from_str(sdk_contracts::ERR_SWAP_SAME_ASSET));
    }
    Ok(())
}

fn build_swap_string(swap: &SwapData) -> Result<String, JsValue> {
    let payload = SwapStringData {
        v: 1,
        payment_hash: swap.payment_hash.clone(),
        qty_from: swap.qty_from,
        qty_to: swap.qty_to,
        from_asset: swap.from_asset.clone(),
        to_asset: swap.to_asset.clone(),
        expires_at: swap.expires_at,
    };
    let raw = serde_json::to_string(&payload).map_err(|e| JsValue::from_str(&e.to_string()))?;
    Ok(format!("{SWAP_STRING_PREFIX}{raw}"))
}

fn parse_swap_string(swap_string: &str) -> Result<SwapStringData, JsValue> {
    let trimmed = swap_string.trim();
    if trimmed.is_empty() {
        return Err(JsValue::from_str(sdk_contracts::ERR_SWAPSTRING_EMPTY));
    }
    let payload = trimmed
        .strip_prefix(SWAP_STRING_PREFIX)
        .ok_or_else(|| JsValue::from_str(sdk_contracts::ERR_SWAPSTRING_FORMAT_INVALID))?;
    serde_json::from_str(payload)
        .map_err(|e| JsValue::from_str(&format!("invalid swapstring: {e}")))
}

fn find_swap_by_payment_hash(state: &SwapRuntimeState, payment_hash: &str) -> Option<SwapData> {
    state
        .maker
        .get(payment_hash)
        .cloned()
        .or_else(|| state.taker.get(payment_hash).cloned())
}

fn validate_payment_hash_format(payment_hash: &str) -> Result<(), JsValue> {
    if payment_hash.trim().is_empty() {
        return Err(JsValue::from_str(sdk_contracts::ERR_PAYMENT_HASH_EMPTY));
    }
    if payment_hash.len() != 64 || !payment_hash.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(JsValue::from_str(sdk_contracts::ERR_PAYMENT_HASH_INVALID));
    }
    Ok(())
}

fn validate_payment_secret_format(payment_secret: &str) -> Result<(), JsValue> {
    if payment_secret.trim().is_empty() {
        return Err(JsValue::from_str(sdk_contracts::ERR_PAYMENT_SECRET_EMPTY));
    }
    if payment_secret.len() != 64 || !payment_secret.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(JsValue::from_str(sdk_contracts::ERR_PAYMENT_SECRET_INVALID));
    }
    Ok(())
}

fn validate_taker_pubkey_format(taker_pubkey: &str) -> Result<(), JsValue> {
    if taker_pubkey.trim().is_empty() {
        return Err(JsValue::from_str(sdk_contracts::ERR_TAKER_PUBKEY_EMPTY));
    }
    PublicKey::from_str(taker_pubkey)
        .map_err(|_| JsValue::from_str(sdk_contracts::ERR_TAKER_PUBKEY_INVALID))?;
    Ok(())
}

fn normalize_maker_execute_input(input: &str) -> Result<String, JsValue> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(JsValue::from_str(sdk_contracts::ERR_SWAPSTRING_EMPTY));
    }

    if trimmed.starts_with('{') {
        let parsed: MakerExecuteRequestData = serde_json::from_str(trimmed)
            .map_err(|e| JsValue::from_str(&format!("invalid maker_execute request JSON: {e}")))?;
        validate_payment_secret_format(&parsed.payment_secret)?;
        validate_taker_pubkey_format(&parsed.taker_pubkey)?;
        if parsed.swapstring.trim().is_empty() {
            return Err(JsValue::from_str(sdk_contracts::ERR_SWAPSTRING_EMPTY));
        }
        return Ok(parsed.swapstring);
    }

    Ok(trimmed.to_string())
}

fn with_effective_status(mut swap: SwapData, now: u64) -> SwapData {
    if matches!(swap.status, SwapStatus::Waiting) && now > swap.expires_at {
        swap.status = SwapStatus::Expired;
    }
    swap
}

pub(crate) fn maker_init_value(request_json: String) -> Result<JsValue, JsValue> {
    crate::ensure_sdk_node_runtime_allowed()?;
    let request: MakerInitRequestData = serde_json::from_str(&request_json)
        .map_err(|e| JsValue::from_str(&format!("invalid maker_init request JSON: {e}")))?;
    if request.qty_from == 0 {
        return Err(JsValue::from_str(sdk_contracts::ERR_SWAP_QTY_FROM_ZERO));
    }
    if request.qty_to == 0 {
        return Err(JsValue::from_str(sdk_contracts::ERR_SWAP_QTY_TO_ZERO));
    }
    if request.timeout_sec == 0 {
        return Err(JsValue::from_str(sdk_contracts::ERR_SWAP_TIMEOUT_ZERO));
    }
    let from_asset = parse_optional_asset(request.from_asset)?;
    let to_asset = parse_optional_asset(request.to_asset)?;
    validate_swap_pair(&from_asset, &to_asset)?;

    let now = now_secs();
    let expires_at = now + request.timeout_sec as u64;
    let (payment_hash, payment_secret, swap) = WASM_SWAP_RUNTIME_STATE.with(|state| {
        let mut state = state.borrow_mut();
        ensure_swap_runtime_loaded(&mut state);
        state.sequence = state.sequence.saturating_add(1);
        let entropy = format!(
            "{}:{}:{}:{}:{:?}:{:?}:{}",
            now,
            state.sequence,
            request.qty_from,
            request.qty_to,
            from_asset,
            to_asset,
            request.timeout_sec
        );
        let payment_hash = Sha256::hash(entropy.as_bytes()).to_string();
        let payment_secret = Sha256::hash(format!("secret:{payment_hash}").as_bytes()).to_string();
        let swap = SwapData {
            qty_from: request.qty_from,
            qty_to: request.qty_to,
            from_asset: from_asset.clone(),
            to_asset: to_asset.clone(),
            payment_hash: payment_hash.clone(),
            status: SwapStatus::Waiting,
            requested_at: now,
            initiated_at: None,
            expires_at,
            completed_at: None,
        };
        state.maker.insert(payment_hash.clone(), swap.clone());
        persist_swap_runtime_state(&state);
        (payment_hash, payment_secret, swap)
    });

    let response = MakerInitResponseData {
        payment_hash,
        payment_secret,
        swapstring: build_swap_string(&swap)?,
    };
    crate::js_obj(&response)
}

pub(crate) fn maker_execute_value(swap_string: String) -> Result<JsValue, JsValue> {
    crate::ensure_sdk_node_runtime_allowed()?;
    let normalized_swap_string = normalize_maker_execute_input(&swap_string)?;
    let parsed = parse_swap_string(&normalized_swap_string)?;
    let now = now_secs();
    let updated = WASM_SWAP_RUNTIME_STATE.with(|state| {
        let mut state = state.borrow_mut();
        ensure_swap_runtime_loaded(&mut state);
        let swap = state
            .maker
            .get_mut(&parsed.payment_hash)
            .ok_or_else(|| JsValue::from_str(sdk_contracts::ERR_SWAP_NOT_FOUND))?;
        if now > swap.expires_at {
            swap.status = SwapStatus::Expired;
            persist_swap_runtime_state(&state);
            return Err(JsValue::from_str(sdk_contracts::ERR_SWAP_EXPIRED));
        }
        swap.status = SwapStatus::Pending;
        if swap.initiated_at.is_none() {
            swap.initiated_at = Some(now);
        }
        let updated_swap = swap.clone();

        if let Some(taker_swap) = state.taker.get_mut(&parsed.payment_hash) {
            taker_swap.status = SwapStatus::Pending;
            if taker_swap.initiated_at.is_none() {
                taker_swap.initiated_at = Some(now);
            }
        }
        persist_swap_runtime_state(&state);
        Ok(updated_swap)
    })?;

    crate::js_obj(&updated)
}

pub(crate) fn taker(request_json: String) -> Result<(), JsValue> {
    crate::ensure_sdk_node_runtime_allowed()?;
    let request: TakerRequestData = serde_json::from_str(&request_json)
        .map_err(|e| JsValue::from_str(&format!("invalid taker request JSON: {e}")))?;
    let parsed = parse_swap_string(&request.swapstring)?;
    let now = now_secs();
    if now > parsed.expires_at {
        return Err(JsValue::from_str(sdk_contracts::ERR_SWAP_EXPIRED));
    }

    WASM_SWAP_RUNTIME_STATE.with(|state| {
        let mut state = state.borrow_mut();
        ensure_swap_runtime_loaded(&mut state);
        if state.taker.contains_key(&parsed.payment_hash) {
            return Ok(());
        }
        let status = state
            .maker
            .get(&parsed.payment_hash)
            .map(|swap| swap.status)
            .unwrap_or(SwapStatus::Waiting);
        let initiated_at = state
            .maker
            .get(&parsed.payment_hash)
            .and_then(|swap| swap.initiated_at);
        let completed_at = state
            .maker
            .get(&parsed.payment_hash)
            .and_then(|swap| swap.completed_at);

        let swap = SwapData {
            qty_from: parsed.qty_from,
            qty_to: parsed.qty_to,
            from_asset: parsed.from_asset,
            to_asset: parsed.to_asset,
            payment_hash: parsed.payment_hash.clone(),
            status,
            requested_at: now,
            initiated_at,
            expires_at: parsed.expires_at,
            completed_at,
        };
        state.taker.insert(parsed.payment_hash, swap);
        persist_swap_runtime_state(&state);
        Ok(())
    })
}

pub(crate) fn get_swap_value(swap_ref: String) -> Result<JsValue, JsValue> {
    crate::ensure_sdk_node_runtime_allowed()?;
    let trimmed = swap_ref.trim();
    if trimmed.is_empty() {
        return Err(JsValue::from_str(sdk_contracts::ERR_SWAP_REFERENCE_EMPTY));
    }

    let maybe_request = serde_json::from_str::<GetSwapRequestData>(trimmed).ok();
    let resolved = WASM_SWAP_RUNTIME_STATE.with(|state| -> Result<SwapData, JsValue> {
        let mut state = state.borrow_mut();
        ensure_swap_runtime_loaded(&mut state);
        let now = now_secs();

        if let Some(request) = maybe_request {
            validate_payment_hash_format(&request.payment_hash)?;
            let selected = if request.taker {
                state.taker.get(&request.payment_hash).cloned()
            } else {
                state.maker.get(&request.payment_hash).cloned()
            };
            let swap =
                selected.ok_or_else(|| JsValue::from_str(sdk_contracts::ERR_SWAP_NOT_FOUND))?;
            return Ok(with_effective_status(swap, now));
        }

        if let Ok(parsed) = parse_swap_string(trimmed) {
            let swap = find_swap_by_payment_hash(&state, &parsed.payment_hash)
                .ok_or_else(|| JsValue::from_str(sdk_contracts::ERR_SWAP_NOT_FOUND))?;
            return Ok(with_effective_status(swap, now));
        }

        validate_payment_hash_format(trimmed)?;
        let swap = find_swap_by_payment_hash(&state, trimmed)
            .ok_or_else(|| JsValue::from_str(sdk_contracts::ERR_SWAP_NOT_FOUND))?;
        Ok(with_effective_status(swap, now))
    })?;

    crate::js_obj(&GetSwapResponseData { swap: resolved })
}

pub(crate) fn list_swaps_value() -> Result<JsValue, JsValue> {
    crate::ensure_sdk_node_runtime_allowed()?;
    let data = WASM_SWAP_RUNTIME_STATE.with(|state| {
        let mut state = state.borrow_mut();
        ensure_swap_runtime_loaded(&mut state);
        let now = now_secs();
        let mut taker: Vec<SwapData> = state
            .taker
            .values()
            .cloned()
            .map(|swap| with_effective_status(swap, now))
            .collect();
        taker.sort_by(|a, b| {
            a.requested_at
                .cmp(&b.requested_at)
                .then_with(|| a.payment_hash.cmp(&b.payment_hash))
        });
        let mut maker: Vec<SwapData> = state
            .maker
            .values()
            .cloned()
            .map(|swap| with_effective_status(swap, now))
            .collect();
        maker.sort_by(|a, b| {
            a.requested_at
                .cmp(&b.requested_at)
                .then_with(|| a.payment_hash.cmp(&b.payment_hash))
        });
        SwapListData { taker, maker }
    });
    crate::js_obj(&data)
}

pub(crate) fn apply_payment_status_update(payment_hash: &str, status: &str) {
    let normalized = status.trim().to_ascii_lowercase();
    let next_status = match normalized.as_str() {
        "pending" => SwapStatus::Pending,
        "succeeded" => SwapStatus::Succeeded,
        "failed" => SwapStatus::Failed,
        "expired" => SwapStatus::Expired,
        _ => return,
    };

    let now = now_secs();
    WASM_SWAP_RUNTIME_STATE.with(|state| {
        let mut state = state.borrow_mut();
        ensure_swap_runtime_loaded(&mut state);
        let mut changed = false;

        if let Some(maker_swap) = state.maker.get_mut(payment_hash) {
            maker_swap.status = next_status;
            if matches!(next_status, SwapStatus::Pending) && maker_swap.initiated_at.is_none() {
                maker_swap.initiated_at = Some(now);
            }
            if matches!(
                next_status,
                SwapStatus::Succeeded | SwapStatus::Failed | SwapStatus::Expired
            ) {
                maker_swap.completed_at = Some(now);
            }
            changed = true;
        }

        if let Some(taker_swap) = state.taker.get_mut(payment_hash) {
            taker_swap.status = next_status;
            if matches!(next_status, SwapStatus::Pending) && taker_swap.initiated_at.is_none() {
                taker_swap.initiated_at = Some(now);
            }
            if matches!(
                next_status,
                SwapStatus::Succeeded | SwapStatus::Failed | SwapStatus::Expired
            ) {
                taker_swap.completed_at = Some(now);
            }
            changed = true;
        }

        if changed {
            persist_swap_runtime_state(&state);
        }
    });
}

#[cfg(all(test, target_arch = "wasm32"))]
#[path = "tests/swap_runtime_tests.rs"]
mod tests;
