use std::cell::{Cell, RefCell};
use std::rc::Rc;

use futures::lock::Mutex;
use futures::{SinkExt, StreamExt};
use gloo_net::websocket::futures::WebSocket;
use gloo_net::websocket::Message;
use js_sys::{Function, Uint8Array};
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::spawn_local;

use crate::proxy_url_for_peer;

struct LnSocketInner {
    write: Mutex<futures::stream::SplitSink<WebSocket, Message>>,
    read: Mutex<futures::stream::SplitStream<WebSocket>>,
    stop: Cell<bool>,
    closed: Cell<bool>,
}

#[wasm_bindgen]
pub struct RlnWasmLnSocket {
    websocket_url: String,
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
                        inner.closed.set(true);
                        inner.stop.set(true);
                        let _ = callback.call1(
                            &JsValue::NULL,
                            &JsValue::from_str(&format!("websocket read error: {err}")),
                        );
                    }
                    None => {
                        inner.closed.set(true);
                        inner.stop.set(true);
                        break;
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
    let websocket_url = proxy_url_for_peer(&proxy_url, &peer_addr)?;
    let ws = WebSocket::open(&websocket_url)
        .map_err(|e| JsValue::from_str(&format!("failed to open websocket: {e}")))?;
    let (write, read) = ws.split();
    let inner = Rc::new(LnSocketInner {
        write: Mutex::new(write),
        read: Mutex::new(read),
        stop: Cell::new(false),
        closed: Cell::new(false),
    });
    Ok(RlnWasmLnSocket {
        websocket_url,
        inner,
        on_message_callback: RefCell::new(None),
    })
}
