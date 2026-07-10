//! RGB-related Lightning **peer wire** (browser WASM vs native daemon).
//!
//! # Where RGB lives on the wire
//!
//! RGB channel semantics are embedded in **standard channel messages** handled by
//! [`lightning::ln::channelmanager::ChannelManager`] as `MessageHandler::chan_handler` (see
//! `lightning::ln::msgs` RGB fields and `rust-lightning/lightning/src/rgb_utils`).
//!
//! # BOLT #1 fork custom messages
//!
//! This module also registers an **experimental** custom message type
//! [`RGB_LN_FORK_CUSTOM_CAP_PING_TYPE`] (see BOLT #1).
//!
//! The WASM proxy remains **transport-only** for RGB JSON-RPC; LN bytes terminate in
//! `PeerManager` / `ChannelManager`.

use lightning::bitcoin::io;
use lightning::bitcoin::secp256k1::PublicKey;
use lightning::ln::msgs::{DecodeError, Init, LightningError};
use lightning::ln::peer_handler::CustomMessageHandler;
use lightning::ln::wire::{CustomMessageReader, Type};
use lightning::types::features::{InitFeatures, NodeFeatures};
use lightning::util::ser::{
    LengthLimitedRead, LengthReadable, Readable, WithoutLength, Writeable, Writer,
};
use std::collections::HashMap;
use std::sync::Mutex;
#[cfg(target_arch = "wasm32")]
use wasm_bindgen::JsValue;

use crate::apay::{JsonRpcErrorWire, ASYNC_ORDER_MESSAGE_TYPE_ID};

/// Experimental type id (BOLT #1 range).
pub const RGB_LN_FORK_CUSTOM_CAP_PING_TYPE: u16 = 45_001;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RgbLnForkCapabilityPing {
    pub wire_version: u16,
}

impl Type for RgbLnForkCapabilityPing {
    fn type_id(&self) -> u16 {
        RGB_LN_FORK_CUSTOM_CAP_PING_TYPE
    }
}

impl Writeable for RgbLnForkCapabilityPing {
    fn write<W: Writer>(&self, w: &mut W) -> Result<(), io::Error> {
        self.wire_version.write(w)
    }
}

/// Async-payments-with-LSP JSON-RPC message (`async_order.*`). The `payload` is a JSON-RPC 2.0
/// envelope string carried verbatim under custom message type [`ASYNC_ORDER_MESSAGE_TYPE_ID`].
/// Byte layout matches the native node's `AsyncOrderMessage` so a WASM client interoperates with
/// the same invoice-host / LSP.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AsyncOrderMessage {
    pub payload: String,
}

impl Type for AsyncOrderMessage {
    fn type_id(&self) -> u16 {
        ASYNC_ORDER_MESSAGE_TYPE_ID
    }
}

impl Writeable for AsyncOrderMessage {
    fn write<W: Writer>(&self, w: &mut W) -> Result<(), io::Error> {
        WithoutLength(&self.payload).write(w)
    }
}

impl LengthReadable for AsyncOrderMessage {
    fn read_from_fixed_length_buffer<R: LengthLimitedRead>(r: &mut R) -> Result<Self, DecodeError> {
        let payload: WithoutLength<String> = LengthReadable::read_from_fixed_length_buffer(r)?;
        Ok(Self { payload: payload.0 })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RgbLnForkCustomMessage {
    CapabilityPing(RgbLnForkCapabilityPing),
    AsyncOrder(AsyncOrderMessage),
}

impl lightning::ln::wire::Type for RgbLnForkCustomMessage {
    fn type_id(&self) -> u16 {
        match self {
            RgbLnForkCustomMessage::CapabilityPing(_) => RGB_LN_FORK_CUSTOM_CAP_PING_TYPE,
            RgbLnForkCustomMessage::AsyncOrder(_) => ASYNC_ORDER_MESSAGE_TYPE_ID,
        }
    }
}

impl Writeable for RgbLnForkCustomMessage {
    fn write<W: Writer>(&self, w: &mut W) -> Result<(), io::Error> {
        match self {
            RgbLnForkCustomMessage::CapabilityPing(inner) => inner.write(w),
            RgbLnForkCustomMessage::AsyncOrder(inner) => inner.write(w),
        }
    }
}

/// Minimal JSON-RPC envelope parsed off the wire to route responses back to the awaiting caller.
#[derive(Debug, serde::Deserialize)]
struct AsyncOrderEnvelope {
    #[serde(default)]
    id: Option<serde_json::Value>,
    #[serde(default)]
    method: Option<String>,
    #[serde(default)]
    params: Option<serde_json::Value>,
    #[serde(default)]
    result: Option<serde_json::Value>,
    #[serde(default)]
    error: Option<serde_json::Value>,
}

type AsyncOrderResponse = Result<serde_json::Value, JsonRpcErrorWire>;
pub(crate) type AsyncOrderResponseReceiver = futures::channel::oneshot::Receiver<AsyncOrderResponse>;
type AsyncOrderResponseSender = futures::channel::oneshot::Sender<AsyncOrderResponse>;

#[derive(Default)]
struct ForkWireState {
    /// Outbound custom messages drained by the `PeerManager` on `process_events`.
    pending: Vec<(PublicKey, RgbLnForkCustomMessage)>,
    /// In-flight outbound `async_order.*` requests keyed by `(peer, request_id)`.
    pending_responses: HashMap<(PublicKey, String), AsyncOrderResponseSender>,
    /// Inbound `async_order.request_invoice` requests awaiting the recipient provider, as
    /// `(sender, request_id, params)`. Drained by the backend during its drive loop, which mints
    /// the invoice and queues the response back via [`RgbLnForkCustomMessageHandler::queue_async_order_result`].
    inbound_requests: Vec<(PublicKey, String, serde_json::Value)>,
}

#[derive(Default)]
pub struct RgbLnForkCustomMessageHandler {
    state: Mutex<ForkWireState>,
}

impl RgbLnForkCustomMessageHandler {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(ForkWireState::default()),
        }
    }

    /// Queue an `async_order.new` request to `host_node_id` and return a receiver that resolves
    /// when the host replies (or is dropped on timeout via [`Self::forget_response`]). The caller
    /// must trigger `PeerManager::process_events` to actually flush the message.
    pub(crate) fn queue_async_order_new(
        &self,
        host_node_id: PublicKey,
        request_id: String,
        params: serde_json::Value,
    ) -> AsyncOrderResponseReceiver {
        let payload = serde_json::json!({
            "jsonrpc": "2.0",
            "id": request_id,
            "method": "async_order.new",
            "params": params,
        })
        .to_string();

        let (tx, rx) = futures::channel::oneshot::channel();
        let mut state = self.state.lock().unwrap();
        state
            .pending_responses
            .insert((host_node_id, request_id), tx);
        state.pending.push((
            host_node_id,
            RgbLnForkCustomMessage::AsyncOrder(AsyncOrderMessage { payload }),
        ));
        rx
    }

    /// Drop a pending response (e.g. after a timeout) so a late reply is ignored.
    pub(crate) fn forget_response(&self, host_node_id: PublicKey, request_id: &str) {
        let mut state = self.state.lock().unwrap();
        state
            .pending_responses
            .remove(&(host_node_id, request_id.to_owned()));
    }

    /// Drain inbound `async_order.request_invoice` requests for the backend to service.
    pub(crate) fn take_inbound_requests(&self) -> Vec<(PublicKey, String, serde_json::Value)> {
        let mut state = self.state.lock().unwrap();
        std::mem::take(&mut state.inbound_requests)
    }

    /// Queue a successful `async_order.*` response (`result`) back to `peer`.
    pub(crate) fn queue_async_order_result(
        &self,
        peer: PublicKey,
        request_id: &str,
        result: serde_json::Value,
    ) {
        let payload = serde_json::json!({
            "jsonrpc": "2.0",
            "id": request_id,
            "result": result,
        })
        .to_string();
        self.state.lock().unwrap().pending.push((
            peer,
            RgbLnForkCustomMessage::AsyncOrder(AsyncOrderMessage { payload }),
        ));
    }

    /// Queue an error `async_order.*` response back to `peer`.
    pub(crate) fn queue_async_order_error(
        &self,
        peer: PublicKey,
        request_id: &str,
        error: &JsonRpcErrorWire,
    ) {
        let payload = serde_json::json!({
            "jsonrpc": "2.0",
            "id": request_id,
            "error": { "code": error.code, "message": error.message },
        })
        .to_string();
        self.state.lock().unwrap().pending.push((
            peer,
            RgbLnForkCustomMessage::AsyncOrder(AsyncOrderMessage { payload }),
        ));
    }

    /// Route an inbound `async_order.*` envelope. A `method` present means it is a *request* to us:
    /// `async_order.request_invoice` is queued for the recipient provider (backend) to service;
    /// any other method is dropped. Otherwise it is a *response* completing a pending request.
    fn dispatch_async_order(&self, sender_node_id: PublicKey, payload: &str) {
        let envelope: AsyncOrderEnvelope = match serde_json::from_str(payload) {
            Ok(env) => env,
            Err(err) => {
                fork_log(&format!("rgb-ln-fork-wire: bad async_order envelope: {err}"));
                return;
            }
        };

        if let Some(method) = envelope.method.as_deref() {
            let Some(serde_json::Value::String(request_id)) = envelope.id else {
                fork_log("rgb-ln-fork-wire: async_order request missing string id");
                return;
            };
            if method == "async_order.request_invoice" {
                let params = envelope.params.unwrap_or(serde_json::Value::Null);
                self.state
                    .lock()
                    .unwrap()
                    .inbound_requests
                    .push((sender_node_id, request_id, params));
            } else {
                fork_log(&format!(
                    "rgb-ln-fork-wire: ignoring unsupported async_order method {method}"
                ));
            }
            return;
        }

        let Some(serde_json::Value::String(request_id)) = envelope.id else {
            fork_log("rgb-ln-fork-wire: async_order response missing string id");
            return;
        };

        let response: AsyncOrderResponse = match (envelope.result, envelope.error) {
            (Some(result), None) => Ok(result),
            (None, Some(error)) => Err(serde_json::from_value::<JsonRpcErrorWire>(error)
                .unwrap_or_else(|err| {
                    JsonRpcErrorWire::internal_error(format!("invalid_async_order_error: {err}"))
                })),
            _ => Err(JsonRpcErrorWire::internal_error(
                "invalid_async_order_response: expected exactly one of result/error",
            )),
        };

        let sender = {
            let mut state = self.state.lock().unwrap();
            state
                .pending_responses
                .remove(&(sender_node_id, request_id))
        };
        if let Some(sender) = sender {
            let _ = sender.send(response);
        }
    }
}

#[inline]
fn fork_log(msg: &str) {
    // Debug-only logging. Custom message handlers can trigger frequently
    // during peer connect/reconnect; keep release builds quiet by default.
    #[cfg(all(target_arch = "wasm32", debug_assertions))]
    web_sys::console::log_1(&JsValue::from_str(msg));
    #[cfg(all(not(target_arch = "wasm32"), debug_assertions))]
    eprintln!("{msg}");
    #[cfg(not(debug_assertions))]
    let _ = msg;
}

impl CustomMessageReader for RgbLnForkCustomMessageHandler {
    type CustomMessage = RgbLnForkCustomMessage;

    fn read<R: LengthLimitedRead>(
        &self,
        message_type: u16,
        buffer: &mut R,
    ) -> Result<Option<Self::CustomMessage>, DecodeError> {
        match message_type {
            RGB_LN_FORK_CUSTOM_CAP_PING_TYPE => {
                let wire_version: u16 = Readable::read(buffer)?;
                Ok(Some(RgbLnForkCustomMessage::CapabilityPing(
                    RgbLnForkCapabilityPing { wire_version },
                )))
            }
            ASYNC_ORDER_MESSAGE_TYPE_ID => Ok(Some(RgbLnForkCustomMessage::AsyncOrder(
                AsyncOrderMessage::read_from_fixed_length_buffer(buffer)?,
            ))),
            _ => Ok(None),
        }
    }
}

impl CustomMessageHandler for RgbLnForkCustomMessageHandler {
    fn handle_custom_message(
        &self,
        msg: Self::CustomMessage,
        sender_node_id: PublicKey,
    ) -> Result<(), LightningError> {
        match msg {
            RgbLnForkCustomMessage::CapabilityPing(p) => {
                fork_log(&format!(
                    "rgb-ln-fork-wire: capability ping from {sender_node_id} version {}",
                    p.wire_version
                ));
            }
            RgbLnForkCustomMessage::AsyncOrder(inner) => {
                self.dispatch_async_order(sender_node_id, &inner.payload);
            }
        }
        Ok(())
    }

    fn get_and_clear_pending_msg(&self) -> Vec<(PublicKey, Self::CustomMessage)> {
        let mut state = self.state.lock().unwrap();
        std::mem::take(&mut state.pending)
    }

    fn peer_disconnected(&self, their_node_id: PublicKey) {
        fork_log(&format!(
            "rgb-ln-fork-wire: peer disconnected {their_node_id}"
        ));
        // Drop any in-flight async_order responses awaiting this peer so callers fail fast
        // (the dropped sender resolves their receiver with a Canceled error) instead of hanging
        // until the timeout.
        let mut state = self.state.lock().unwrap();
        state
            .pending_responses
            .retain(|(peer, _), _| peer != &their_node_id);
    }

    fn peer_connected(
        &self,
        their_node_id: PublicKey,
        _msg: &Init,
        _inbound: bool,
    ) -> Result<(), ()> {
        fork_log(&format!("rgb-ln-fork-wire: peer connected {their_node_id}"));
        Ok(())
    }

    fn provided_node_features(&self) -> NodeFeatures {
        NodeFeatures::empty()
    }

    fn provided_init_features(&self, _their_node_id: PublicKey) -> InitFeatures {
        InitFeatures::empty()
    }
}

/// Used by [`crate::ldk_live_backend`] when constructing [`lightning::ln::peer_handler::PeerManager`].
pub(crate) fn rgb_ln_fork_custom_message_handler() -> std::sync::Arc<RgbLnForkCustomMessageHandler>
{
    std::sync::Arc::new(RgbLnForkCustomMessageHandler::new())
}

#[cfg(test)]
mod tests {
    use super::*;
    use lightning::ln::wire::Type;

    #[test]
    fn capability_ping_roundtrip_type_id() {
        let msg =
            RgbLnForkCustomMessage::CapabilityPing(RgbLnForkCapabilityPing { wire_version: 1 });
        assert_eq!(msg.type_id(), RGB_LN_FORK_CUSTOM_CAP_PING_TYPE);
        let mut w = Vec::new();
        msg.write(&mut w).unwrap();
        let mut slice = w.as_slice();
        let h = RgbLnForkCustomMessageHandler::new();
        let decoded = h
            .read(RGB_LN_FORK_CUSTOM_CAP_PING_TYPE, &mut slice)
            .unwrap()
            .unwrap();
        assert_eq!(decoded, msg);
    }
}
