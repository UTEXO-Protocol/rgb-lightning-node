use bitcoin::secp256k1::PublicKey;
use lightning::{
    ln::{
        msgs::{DecodeError, Init, LightningError},
        peer_handler::CustomMessageHandler,
        wire::{CustomMessageReader, Type},
    },
    types::features::{InitFeatures, NodeFeatures},
    util::ser::{LengthLimitedRead, Writeable, Writer},
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::oneshot;
use tracing::warn;

use crate::asset_link::{AssetLinkMessage, AssetLinkMessageHandler};
use crate::async_order::{AsyncOrderMessage, AsyncOrderMessageHandler};

pub(crate) const JSONRPC_INTERNAL_ERROR: i64 = -32603;
pub(crate) const JSONRPC_INVALID_PARAMS: i64 = -32602;
pub(crate) const JSONRPC_INVALID_REQUEST: i64 = -32600;
pub(crate) const JSONRPC_METHOD_NOT_FOUND: i64 = -32601;
pub(crate) const JSONRPC_MSG_RESPONSE_TIMEOUT_SECS: u64 = 30;
pub(crate) const JSONRPC_PARSE_ERROR: i64 = -32700;
pub(crate) const JSONRPC_VERSION: &str = "2.0";

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct JsonRpcErrorWire {
    pub(crate) code: i64,
    pub(crate) message: String,
}

impl JsonRpcErrorWire {
    pub(crate) fn application_error(code: i64, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub(crate) fn internal_error(message: String) -> Self {
        Self {
            code: JSONRPC_INTERNAL_ERROR,
            message,
        }
    }

    pub(crate) fn invalid_params(message: impl Into<String>) -> Self {
        Self {
            code: JSONRPC_INVALID_PARAMS,
            message: message.into(),
        }
    }

    pub(crate) fn invalid_request() -> Self {
        Self {
            code: JSONRPC_INVALID_REQUEST,
            message: "invalid request".to_owned(),
        }
    }

    pub(crate) fn parse_error() -> Self {
        Self {
            code: JSONRPC_PARSE_ERROR,
            message: "parse error".to_owned(),
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct CustomMsgRpcEnvelope {
    pub(crate) jsonrpc: String,
    #[serde(default)]
    pub(crate) id: Option<Value>,
    #[serde(default)]
    pub(crate) method: Option<String>,
    #[serde(default)]
    pub(crate) params: Option<Value>,
    #[serde(default)]
    pub(crate) result: Option<Value>,
    #[serde(default)]
    pub(crate) error: Option<Value>,
}

impl CustomMsgRpcEnvelope {
    pub(crate) fn is_response_like(&self) -> bool {
        self.result.is_some()
            || self.error.is_some()
            || (self.method.is_none() && self.id.is_some() && self.params.is_none())
    }
}

pub(crate) type CustomMsgRpcResponse = Result<Value, JsonRpcErrorWire>;
pub(crate) type CustomMsgRpcResponseReceiver = oneshot::Receiver<CustomMsgRpcResponse>;
pub(crate) type CustomMsgRpcResponseSender = oneshot::Sender<CustomMsgRpcResponse>;

pub(crate) type PendingResponses = HashMap<(PublicKey, String), CustomMsgRpcResponseSender>;

pub(crate) fn jsonrpc_error_value(id: Value, error: JsonRpcErrorWire) -> Value {
    json!({ "jsonrpc": JSONRPC_VERSION, "id": id, "error": error })
}

pub(crate) fn jsonrpc_result_value(id: Value, result: impl Serialize) -> Value {
    json!({ "jsonrpc": JSONRPC_VERSION, "id": id, "result": result })
}

pub(crate) fn resolve_pending_response(
    pending: &mut PendingResponses,
    protocol: &str,
    sender_node_id: PublicKey,
    id: Option<Value>,
    result: Option<Value>,
    error: Option<Value>,
) {
    let Some(Value::String(request_id)) = id else {
        warn!(peer = %sender_node_id, "ignoring {protocol} response with missing or non-string id");
        return;
    };

    let response = match (result, error) {
        (Some(result), None) => Ok(result),
        (None, Some(error)) => match serde_json::from_value::<JsonRpcErrorWire>(error) {
            Ok(err) => Err(err),
            Err(err) => Err(JsonRpcErrorWire::internal_error(format!(
                "invalid_{protocol}_response_error: {err}"
            ))),
        },
        (Some(_), Some(_)) => Err(JsonRpcErrorWire::internal_error(format!(
            "invalid_{protocol}_response: contained both result and error"
        ))),
        (None, None) => Err(JsonRpcErrorWire::internal_error(format!(
            "invalid_{protocol}_response: missing result and error"
        ))),
    };

    if let Some(response_sender) = pending.remove(&(sender_node_id, request_id)) {
        let _ = response_sender.send(response);
    }
}

pub(crate) trait CustomMsgPeerAccessControl: Send + Sync {
    fn allows_peer(&self, peer: &PublicKey) -> bool;
}

#[derive(Debug)]
pub(crate) enum NodeCustomMessage {
    AsyncOrder(AsyncOrderMessage),
    AssetLink(AssetLinkMessage),
}

impl Type for NodeCustomMessage {
    fn type_id(&self) -> u16 {
        match self {
            NodeCustomMessage::AsyncOrder(msg) => msg.type_id(),
            NodeCustomMessage::AssetLink(msg) => msg.type_id(),
        }
    }
}

impl Writeable for NodeCustomMessage {
    fn write<W: Writer>(&self, w: &mut W) -> Result<(), lightning::io::Error> {
        match self {
            NodeCustomMessage::AsyncOrder(msg) => msg.write(w),
            NodeCustomMessage::AssetLink(msg) => msg.write(w),
        }
    }
}

pub(crate) struct CustomMessenger {
    pub(crate) async_order: Arc<AsyncOrderMessageHandler>,
    pub(crate) asset_link: Arc<AssetLinkMessageHandler>,
}

impl CustomMessageReader for CustomMessenger {
    type CustomMessage = NodeCustomMessage;

    fn read<RD: LengthLimitedRead>(
        &self,
        message_type: u16,
        buffer: &mut RD,
    ) -> Result<Option<Self::CustomMessage>, DecodeError> {
        if let Some(msg) = self.async_order.read(message_type, buffer)? {
            return Ok(Some(NodeCustomMessage::AsyncOrder(msg)));
        }
        if let Some(msg) = self.asset_link.read(message_type, buffer)? {
            return Ok(Some(NodeCustomMessage::AssetLink(msg)));
        }
        Ok(None)
    }
}

impl CustomMessageHandler for CustomMessenger {
    fn handle_custom_message(
        &self,
        msg: Self::CustomMessage,
        sender_node_id: PublicKey,
    ) -> Result<(), LightningError> {
        match msg {
            NodeCustomMessage::AsyncOrder(msg) => {
                self.async_order.handle_custom_message(msg, sender_node_id)
            }
            NodeCustomMessage::AssetLink(msg) => {
                self.asset_link.handle_custom_message(msg, sender_node_id)
            }
        }
    }

    fn get_and_clear_pending_msg(&self) -> Vec<(PublicKey, Self::CustomMessage)> {
        let mut pending = Vec::new();
        for (peer, msg) in self.async_order.get_and_clear_pending_msg() {
            pending.push((peer, NodeCustomMessage::AsyncOrder(msg)));
        }
        for (peer, msg) in self.asset_link.get_and_clear_pending_msg() {
            pending.push((peer, NodeCustomMessage::AssetLink(msg)));
        }
        pending
    }

    fn peer_disconnected(&self, their_node_id: PublicKey) {
        self.async_order.peer_disconnected(their_node_id);
        self.asset_link.peer_disconnected(their_node_id);
    }

    fn peer_connected(
        &self,
        their_node_id: PublicKey,
        msg: &Init,
        inbound: bool,
    ) -> Result<(), ()> {
        self.async_order
            .peer_connected(their_node_id, msg, inbound)?;
        self.asset_link.peer_connected(their_node_id, msg, inbound)
    }

    fn provided_node_features(&self) -> NodeFeatures {
        self.async_order.provided_node_features() | self.asset_link.provided_node_features()
    }

    fn provided_init_features(&self, their_node_id: PublicKey) -> InitFeatures {
        self.async_order.provided_init_features(their_node_id)
            | self.asset_link.provided_init_features(their_node_id)
    }
}
