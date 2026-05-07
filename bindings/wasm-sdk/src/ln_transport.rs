use std::cell::{Cell, RefCell};
use std::rc::Rc;

use futures::lock::Mutex;
use futures::{SinkExt, StreamExt};
use gloo_net::websocket::futures::WebSocket;
use gloo_net::websocket::Message;
use js_sys::{Function, Uint8Array};
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::spawn_local;
#[cfg(target_arch = "wasm32")]
use wasm_bindgen_futures::JsFuture;

#[cfg(all(test, target_arch = "wasm32"))]
#[path = "tests/ln_transport_tests.rs"]
mod tests;

use crate::proxy_url_for_peer;
use crate::runtime_store::{browser_persistent_state_store, RuntimeStateStore};

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
pub struct RlnWasmLnSocketConnectOptionsData {
    #[serde(default)]
    pub max_reconnect_attempts: Option<u32>,
    #[serde(default)]
    pub reconnect_initial_delay_ms: Option<u32>,
    #[serde(default)]
    pub reconnect_max_delay_ms: Option<u32>,
    #[serde(default)]
    pub relay_auth_token: Option<String>,
    #[serde(default)]
    pub relay_node_id: Option<String>,
    #[serde(default)]
    pub replay_transport_envelope: Option<bool>,
    #[serde(default)]
    pub replay_session_id: Option<String>,
    #[serde(default)]
    pub replay_last_applied_seq: Option<u64>,
}

impl Default for RlnWasmLnSocketConnectOptionsData {
    fn default() -> Self {
        Self {
            max_reconnect_attempts: Some(3),
            reconnect_initial_delay_ms: Some(250),
            reconnect_max_delay_ms: Some(4_000),
            relay_auth_token: None,
            relay_node_id: None,
            replay_transport_envelope: Some(false),
            replay_session_id: None,
            replay_last_applied_seq: None,
        }
    }
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum ReplayFrameDisposition {
    Accept,
    DropDuplicate,
    Gap { expected_next: u64 },
}

/// Strict monotonic replay rule: next frame must be `last_applied + 1`.
pub(crate) fn replay_frame_disposition(
    last_applied: u64,
    frame_seq: u64,
) -> ReplayFrameDisposition {
    if frame_seq <= last_applied {
        ReplayFrameDisposition::DropDuplicate
    } else if frame_seq > last_applied.saturating_add(1) {
        ReplayFrameDisposition::Gap {
            expected_next: last_applied.saturating_add(1),
        }
    } else {
        ReplayFrameDisposition::Accept
    }
}

fn initial_replay_last_applied_cursor(options: &RlnWasmLnSocketConnectOptionsData) -> u64 {
    if !options.replay_transport_envelope.unwrap_or(false) {
        return 0;
    }
    let Some(sid) = options
        .replay_session_id
        .as_ref()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
    else {
        return options.replay_last_applied_seq.unwrap_or(0);
    };
    options
        .replay_last_applied_seq
        .or_else(|| load_last_applied_seq(sid))
        .unwrap_or(0)
}

#[derive(Debug, serde::Deserialize)]
struct ReplayEnvelopeFrame {
    seq: u64,
    payload_hex: String,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
pub struct RlnWasmReplayInboundFrameData {
    pub seq: u64,
    pub payload_hex: String,
    pub session_id: String,
}

struct LnSocketInner {
    write: Mutex<futures::stream::SplitSink<WebSocket, Message>>,
    read: Mutex<futures::stream::SplitStream<WebSocket>>,
    stop: Cell<bool>,
    closed: Cell<bool>,
}

#[derive(Clone)]
#[wasm_bindgen]
pub struct RlnWasmLnSocket {
    websocket_url: String,
    proxy_url: String,
    peer_addr: String,
    options: RlnWasmLnSocketConnectOptionsData,
    replay_session_id: Option<String>,
    last_inbound_seq: Rc<Cell<u64>>,
    inner: Rc<LnSocketInner>,
    on_message_callback: RefCell<Option<Function>>,
}

#[wasm_bindgen]
impl RlnWasmLnSocket {
    #[wasm_bindgen(js_name = websocketUrl)]
    pub fn websocket_url(&self) -> String {
        self.websocket_url.clone()
    }

    #[wasm_bindgen(js_name = sendHex)]
    pub async fn send_hex(&self, payload_hex: String) -> Result<(), JsValue> {
        if payload_hex.trim().is_empty() {
            return Err(JsValue::from_str(sdk_contracts::ERR_PAYLOAD_HEX_EMPTY));
        }
        let payload = hex::decode(payload_hex)
            .map_err(|e| JsValue::from_str(&format!("invalid payload_hex: {e}")))?;
        let mut writer = self.inner.write.lock().await;
        writer
            .send(Message::Bytes(payload))
            .await
            .map_err(|e| JsValue::from_str(&format!("failed to send websocket bytes: {e}")))?;
        Ok(())
    }

    #[wasm_bindgen(js_name = close)]
    pub async fn close(&self) -> Result<(), JsValue> {
        self.inner.stop.set(true);
        let mut writer = self.inner.write.lock().await;
        writer
            .close()
            .await
            .map_err(|e| JsValue::from_str(&format!("failed to close websocket: {e}")))?;
        self.inner.closed.set(true);
        Ok(())
    }

    #[wasm_bindgen(js_name = isClosed)]
    pub fn is_closed(&self) -> bool {
        self.inner.closed.get()
    }

    #[wasm_bindgen(js_name = lastInboundSeqValue)]
    pub fn last_inbound_seq_value(&self) -> u64 {
        self.last_inbound_seq.get()
    }

    /// When replay envelopes are enabled with a session id, returns the shared cursor that tracks
    /// the last **successfully applied** inbound replay `seq` (aligned with `commit_last_applied_seq`).
    pub(crate) fn replay_applied_seq_cell(&self) -> Option<Rc<Cell<u64>>> {
        if !self.options.replay_transport_envelope.unwrap_or(false) {
            return None;
        }
        let sid = self.replay_session_id.as_ref()?;
        if sid.trim().is_empty() {
            return None;
        }
        Some(Rc::clone(&self.last_inbound_seq))
    }

    #[wasm_bindgen(js_name = startReadLoop)]
    pub fn start_read_loop(&self, on_message: Function) -> Result<(), JsValue> {
        self.on_message_callback.replace(Some(on_message));
        self.inner.stop.set(false);

        let inner = self.inner.clone();
        let callback = self.on_message_callback.borrow().as_ref().cloned();
        let callback = callback
            .ok_or_else(|| JsValue::from_str(sdk_contracts::ERR_ON_MESSAGE_CALLBACK_MISSING))?;
        let proxy_url = self.proxy_url.clone();
        let peer_addr = self.peer_addr.clone();
        let options = self.options.clone();
        let replay_session_id = self.replay_session_id.clone();
        let last_inbound_seq = Rc::clone(&self.last_inbound_seq);

        spawn_local(async move {
            loop {
                if inner.stop.get() {
                    break;
                }

                let next = {
                    let mut reader = inner.read.lock().await;
                    reader.next().await
                };

                match next {
                    Some(Ok(Message::Bytes(bytes))) => {
                        let bytes_js = Uint8Array::from(bytes.as_slice());
                        let _ = callback.call1(&JsValue::NULL, &bytes_js.into());
                    }
                    Some(Ok(Message::Text(text))) => {
                        if !options.replay_transport_envelope.unwrap_or(false) {
                            continue;
                        }
                        let parsed = serde_json::from_str::<ReplayEnvelopeFrame>(&text);
                        let Ok(frame) = parsed else {
                            let _ = callback.call1(
                                &JsValue::NULL,
                                &JsValue::from_str(
                                    sdk_contracts::ERR_REPLAY_ENVELOPE_FRAME_INVALID,
                                ),
                            );
                            continue;
                        };
                        match replay_frame_disposition(last_inbound_seq.get(), frame.seq) {
                            ReplayFrameDisposition::DropDuplicate => continue,
                            ReplayFrameDisposition::Gap { expected_next } => {
                                let _ = callback.call1(
                                    &JsValue::NULL,
                                    &JsValue::from_str(&format!(
                                        "replay sequence gap detected (expected {expected_next}, got {})",
                                        frame.seq
                                    )),
                                );
                                inner.closed.set(true);
                                inner.stop.set(true);
                                break;
                            }
                            ReplayFrameDisposition::Accept => {}
                        }
                        let bytes = match hex::decode(frame.payload_hex.trim()) {
                            Ok(bytes) => bytes,
                            Err(_) => {
                                let _ = callback.call1(
                                    &JsValue::NULL,
                                    &JsValue::from_str(
                                        sdk_contracts::ERR_REPLAY_ENVELOPE_PAYLOAD_HEX_INVALID,
                                    ),
                                );
                                continue;
                            }
                        };
                        if let Some(session_id) = replay_session_id.as_ref() {
                            let replay_frame = RlnWasmReplayInboundFrameData {
                                seq: frame.seq,
                                payload_hex: hex::encode(&bytes),
                                session_id: session_id.clone(),
                            };
                            if let Ok(value) = crate::js_obj(&replay_frame) {
                                let _ = callback.call1(&JsValue::NULL, &value);
                                continue;
                            }
                        }
                        let bytes_js = Uint8Array::from(bytes.as_slice());
                        let _ = callback.call1(&JsValue::NULL, &bytes_js.into());
                    }
                    Some(Err(err)) => {
                        let reconnected =
                            try_reconnect_socket(&inner, &proxy_url, &peer_addr, &options).await;
                        if !reconnected {
                            inner.closed.set(true);
                            inner.stop.set(true);
                            let _ = callback.call1(
                                &JsValue::NULL,
                                &JsValue::from_str(&format!("websocket read error: {err}")),
                            );
                        }
                    }
                    None => {
                        let reconnected =
                            try_reconnect_socket(&inner, &proxy_url, &peer_addr, &options).await;
                        if !reconnected {
                            inner.closed.set(true);
                            inner.stop.set(true);
                            break;
                        }
                    }
                }
            }
        });

        Ok(())
    }

    #[wasm_bindgen(js_name = stopReadLoop)]
    pub fn stop_read_loop(&self) {
        self.inner.stop.set(true);
    }
}

#[wasm_bindgen(js_name = lnSocketConnect)]
pub async fn ln_socket_connect(
    proxy_url: String,
    peer_addr: String,
) -> Result<RlnWasmLnSocket, JsValue> {
    ln_socket_connect_with_options(proxy_url, peer_addr, JsValue::NULL).await
}

#[wasm_bindgen(js_name = lnSocketConnectWithOptions)]
pub async fn ln_socket_connect_with_options(
    proxy_url: String,
    peer_addr: String,
    options_js: JsValue,
) -> Result<RlnWasmLnSocket, JsValue> {
    let options: RlnWasmLnSocketConnectOptionsData =
        if options_js.is_null() || options_js.is_undefined() {
            RlnWasmLnSocketConnectOptionsData::default()
        } else {
            crate::js_from(options_js)?
        };
    validate_connect_options(&options)?;
    let websocket_url = proxy_url_for_peer_with_options(&proxy_url, &peer_addr, &options)?;
    let ws = connect_with_backoff(&proxy_url, &peer_addr, &options).await?;
    let (write, read) = ws.split();
    let inner = Rc::new(LnSocketInner {
        write: Mutex::new(write),
        read: Mutex::new(read),
        stop: Cell::new(false),
        closed: Cell::new(false),
    });
    let replay_cursor0 = initial_replay_last_applied_cursor(&options);
    Ok(RlnWasmLnSocket {
        websocket_url,
        proxy_url,
        peer_addr,
        replay_session_id: options
            .replay_session_id
            .as_ref()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty()),
        last_inbound_seq: Rc::new(Cell::new(replay_cursor0)),
        options,
        inner,
        on_message_callback: RefCell::new(None),
    })
}

fn validate_connect_options(options: &RlnWasmLnSocketConnectOptionsData) -> Result<(), JsValue> {
    let max_attempts = options.max_reconnect_attempts.unwrap_or(3);
    let initial_delay = options.reconnect_initial_delay_ms.unwrap_or(250);
    let max_delay = options.reconnect_max_delay_ms.unwrap_or(4_000);
    if initial_delay == 0 || max_delay == 0 {
        return Err(JsValue::from_str(
            sdk_contracts::ERR_RECONNECT_DELAYS_INVALID,
        ));
    }
    if initial_delay > max_delay {
        return Err(JsValue::from_str(
            "reconnect_initial_delay_ms cannot be greater than reconnect_max_delay_ms",
        ));
    }
    if max_attempts > 100 {
        return Err(JsValue::from_str(
            sdk_contracts::ERR_MAX_RECONNECT_ATTEMPTS_TOO_LARGE,
        ));
    }
    if let Some(token) = options.relay_auth_token.as_ref() {
        if token.trim().is_empty() {
            return Err(JsValue::from_str(sdk_contracts::ERR_RELAY_AUTH_TOKEN_EMPTY));
        }
    }
    if let Some(node_id) = options.relay_node_id.as_ref() {
        if node_id.trim().is_empty() {
            return Err(JsValue::from_str(sdk_contracts::ERR_RELAY_NODE_ID_EMPTY));
        }
    }
    if let Some(session_id) = options.replay_session_id.as_ref() {
        if session_id.trim().is_empty() {
            return Err(JsValue::from_str(
                sdk_contracts::ERR_REPLAY_SESSION_ID_EMPTY,
            ));
        }
    }
    Ok(())
}

fn proxy_url_for_peer_with_options(
    proxy_url: &str,
    peer_addr: &str,
    options: &RlnWasmLnSocketConnectOptionsData,
) -> Result<String, JsValue> {
    let base = proxy_url_for_peer(proxy_url, peer_addr)?;
    let mut query = vec![];
    if let Some(token) = options.relay_auth_token.as_ref() {
        query.push(format!("auth_token={}", urlencoding::encode(token.trim())));
    }
    if let Some(node_id) = options.relay_node_id.as_ref() {
        query.push(format!("node_id={}", urlencoding::encode(node_id.trim())));
    }
    if options.replay_transport_envelope.unwrap_or(false) {
        query.push("replay=1".to_string());
        if let Some(session_id) = options.replay_session_id.as_ref() {
            if !session_id.trim().is_empty() {
                query.push(format!(
                    "session_id={}",
                    urlencoding::encode(session_id.trim())
                ));
                let last = options
                    .replay_last_applied_seq
                    .or_else(|| load_last_applied_seq(session_id));
                if let Some(last_seq) = last {
                    query.push(format!("last_applied_seq={last_seq}"));
                }
            }
        }
    }
    if query.is_empty() {
        return Ok(base);
    }
    let sep = if base.contains('?') { '&' } else { '?' };
    Ok(format!("{base}{sep}{}", query.join("&")))
}

fn replay_seq_storage_key(session_id: &str) -> String {
    format!("rln:wasm:replay-last-seq:{}", session_id.trim())
}

fn load_last_applied_seq(session_id: &str) -> Option<u64> {
    let store = browser_persistent_state_store();
    let key = replay_seq_storage_key(session_id);
    let raw = store.get(&key).ok().flatten()?;
    raw.trim().parse::<u64>().ok()
}

fn persist_last_applied_seq(session_id: &str, seq: u64) {
    let store = browser_persistent_state_store();
    let key = replay_seq_storage_key(session_id);
    let _ = store.set(&key, &seq.to_string());
}

pub(crate) fn commit_last_applied_seq(session_id: &str, seq: u64) {
    if session_id.trim().is_empty() {
        return;
    }
    persist_last_applied_seq(session_id, seq);
}

async fn connect_with_backoff(
    proxy_url: &str,
    peer_addr: &str,
    options: &RlnWasmLnSocketConnectOptionsData,
) -> Result<WebSocket, JsValue> {
    let websocket_url = proxy_url_for_peer_with_options(proxy_url, peer_addr, options)?;
    let max_attempts = options.max_reconnect_attempts.unwrap_or(3).max(1);
    let mut delay = options.reconnect_initial_delay_ms.unwrap_or(250).max(1);
    let max_delay = options.reconnect_max_delay_ms.unwrap_or(4_000).max(delay);

    let mut last_error = None;
    for attempt in 1..=max_attempts {
        match WebSocket::open(&websocket_url) {
            Ok(ws) => return Ok(ws),
            Err(err) => {
                last_error = Some(format!("{err}"));
                if attempt < max_attempts {
                    sleep_ms(delay).await;
                    delay = (delay.saturating_mul(2)).min(max_delay);
                }
            }
        }
    }

    Err(JsValue::from_str(&format!(
        "failed to open websocket after retries: {}",
        last_error.unwrap_or_else(|| "unknown error".to_string())
    )))
}

async fn try_reconnect_socket(
    inner: &Rc<LnSocketInner>,
    proxy_url: &str,
    peer_addr: &str,
    options: &RlnWasmLnSocketConnectOptionsData,
) -> bool {
    if inner.stop.get() {
        return false;
    }
    match connect_with_backoff(proxy_url, peer_addr, options).await {
        Ok(ws) => {
            let (write, read) = ws.split();
            {
                let mut writer = inner.write.lock().await;
                *writer = write;
            }
            {
                let mut reader = inner.read.lock().await;
                *reader = read;
            }
            inner.closed.set(false);
            true
        }
        Err(_) => false,
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
    let _ = JsFuture::from(promise).await;
}

#[cfg(not(target_arch = "wasm32"))]
async fn sleep_ms(ms: u32) {
    std::thread::sleep(std::time::Duration::from_millis(ms as u64));
}

#[cfg(test)]
mod replay_frame_disposition_tests {
    use super::{replay_frame_disposition, ReplayFrameDisposition::*};

    #[test]
    fn accepts_next_then_duplicate() {
        assert!(matches!(replay_frame_disposition(0, 1), Accept));
        assert!(matches!(replay_frame_disposition(1, 1), DropDuplicate));
        assert!(matches!(replay_frame_disposition(1, 2), Accept));
    }

    #[test]
    fn detects_gap() {
        assert!(matches!(
            replay_frame_disposition(1, 5),
            Gap { expected_next: 2 }
        ));
    }
}
