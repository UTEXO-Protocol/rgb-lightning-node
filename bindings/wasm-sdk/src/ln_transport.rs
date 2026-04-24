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
}

impl Default for RlnWasmLnSocketConnectOptionsData {
    fn default() -> Self {
        Self {
            max_reconnect_attempts: Some(3),
            reconnect_initial_delay_ms: Some(250),
            reconnect_max_delay_ms: Some(4_000),
            relay_auth_token: None,
            relay_node_id: None,
        }
    }
}

struct LnSocketInner {
    write: Mutex<futures::stream::SplitSink<WebSocket, Message>>,
    read: Mutex<futures::stream::SplitStream<WebSocket>>,
    stop: Cell<bool>,
    closed: Cell<bool>,
}

#[wasm_bindgen]
pub struct RlnWasmLnSocket {
    websocket_url: String,
    proxy_url: String,
    peer_addr: String,
    options: RlnWasmLnSocketConnectOptionsData,
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
            return Err(JsValue::from_str("payload_hex cannot be empty"));
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

    #[wasm_bindgen(js_name = startReadLoop)]
    pub fn start_read_loop(&self, on_message: Function) -> Result<(), JsValue> {
        self.on_message_callback.replace(Some(on_message));
        self.inner.stop.set(false);

        let inner = self.inner.clone();
        let callback = self.on_message_callback.borrow().as_ref().cloned();
        let callback = callback.ok_or_else(|| JsValue::from_str("missing on_message callback"))?;
        let proxy_url = self.proxy_url.clone();
        let peer_addr = self.peer_addr.clone();
        let options = self.options.clone();

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
                    Some(Ok(Message::Text(_))) => {}
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
    Ok(RlnWasmLnSocket {
        websocket_url,
        proxy_url,
        peer_addr,
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
        return Err(JsValue::from_str("reconnect delays must be > 0"));
    }
    if initial_delay > max_delay {
        return Err(JsValue::from_str(
            "reconnect_initial_delay_ms cannot be greater than reconnect_max_delay_ms",
        ));
    }
    if max_attempts > 100 {
        return Err(JsValue::from_str("max_reconnect_attempts is too large"));
    }
    if let Some(token) = options.relay_auth_token.as_ref() {
        if token.trim().is_empty() {
            return Err(JsValue::from_str("relay_auth_token cannot be empty"));
        }
    }
    if let Some(node_id) = options.relay_node_id.as_ref() {
        if node_id.trim().is_empty() {
            return Err(JsValue::from_str("relay_node_id cannot be empty"));
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
    if query.is_empty() {
        return Ok(base);
    }
    let sep = if base.contains('?') { '&' } else { '?' };
    Ok(format!("{base}{sep}{}", query.join("&")))
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
