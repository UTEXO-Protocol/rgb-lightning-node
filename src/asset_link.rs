use bitcoin::secp256k1::PublicKey;
use lightning::{
    io,
    ln::{
        msgs::{DecodeError, Init, LightningError},
        peer_handler::CustomMessageHandler,
        wire::{CustomMessageReader, Type},
    },
    types::features::{InitFeatures, NodeFeatures},
    util::ser::{LengthLimitedRead, LengthReadable, WithoutLength, Writeable, Writer},
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use tracing::warn;

use crate::custom_msg_rpc::{
    jsonrpc_error_value, jsonrpc_result_value, resolve_pending_response,
    CustomMsgPeerAccessControl, CustomMsgRpcEnvelope, CustomMsgRpcResponseReceiver,
    JsonRpcErrorWire, PendingResponses, JSONRPC_INVALID_PARAMS, JSONRPC_METHOD_NOT_FOUND,
    JSONRPC_VERSION,
};

const ASSET_LINK_AUTHORIZE_SWAP_METHOD: &str = "asset_link.authorize_swap";
pub(crate) const ASSET_LINK_ERROR_DUPLICATE_PAYMENT_HASH: i64 = 1203;
pub(crate) const ASSET_LINK_ERROR_INSUFFICIENT_LIQUIDITY: i64 = 1202;
pub(crate) const ASSET_LINK_ERROR_UNKNOWN_LINK: i64 = 1201;
pub(crate) const ASSET_LINK_ERROR_UNSUPPORTED_PROTOCOL_VERSION: i64 = 1200;
pub(crate) const ASSET_LINK_MESSAGE_TYPE_ID: u16 = 37917;
pub(crate) const ASSET_LINK_PROTOCOL_VERSION: u64 = 1;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AssetLinkMessage {
    pub(crate) payload: String,
}

impl Type for AssetLinkMessage {
    fn type_id(&self) -> u16 {
        ASSET_LINK_MESSAGE_TYPE_ID
    }
}

impl Writeable for AssetLinkMessage {
    fn write<W: Writer>(&self, w: &mut W) -> Result<(), io::Error> {
        WithoutLength(&self.payload).write(w)
    }
}

impl LengthReadable for AssetLinkMessage {
    fn read_from_fixed_length_buffer<R: LengthLimitedRead>(r: &mut R) -> Result<Self, DecodeError> {
        let payload_without_length: WithoutLength<String> =
            LengthReadable::read_from_fixed_length_buffer(r)?;
        Ok(Self {
            payload: payload_without_length.0,
        })
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AssetLinkAuthorizeParamsWire {
    pub(crate) protocol_version: u64,
    pub(crate) payment_hash: String,
    pub(crate) asset_id: String,
    pub(crate) linked_asset_id: String,
    pub(crate) amount: u64,
    pub(crate) expiry_sec: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct AssetLinkAuthorizeResultWire {
    pub(crate) authorized: bool,
}

#[derive(Deserialize, Serialize)]
pub(crate) struct AssetLinkSendPaymentRequest {
    pub(crate) invoice: String,
    pub(crate) asset_id: String,
    pub(crate) host_pubkey: String,
}

pub(crate) trait AssetLinkAuthorizer: Send + Sync {
    fn authorize_swap(
        &self,
        sender_node_id: PublicKey,
        params: &AssetLinkAuthorizeParamsWire,
    ) -> Result<(), JsonRpcErrorWire>;
}

pub(crate) struct AssetLinkMessageHandler {
    access_control: Arc<dyn CustomMsgPeerAccessControl>,
    authorizer: Mutex<Option<Arc<dyn AssetLinkAuthorizer>>>,
    state: Mutex<AssetLinkHandlerState>,
}

#[derive(Default)]
struct AssetLinkHandlerState {
    pending: Vec<(PublicKey, AssetLinkMessage)>,
    pending_responses: PendingResponses,
}

impl AssetLinkMessageHandler {
    pub(crate) fn new(access_control: Arc<dyn CustomMsgPeerAccessControl>) -> Self {
        Self {
            access_control,
            authorizer: Mutex::new(None),
            state: Mutex::new(AssetLinkHandlerState::default()),
        }
    }

    pub(crate) fn set_authorizer(&self, authorizer: Arc<dyn AssetLinkAuthorizer>) {
        *self.authorizer.lock().unwrap() = Some(authorizer);
    }

    pub(crate) fn queue_asset_link_authorize(
        &self,
        host_node_id: PublicKey,
        id: Value,
        params: AssetLinkAuthorizeParamsWire,
    ) -> Result<CustomMsgRpcResponseReceiver, JsonRpcErrorWire> {
        let Some(request_id) = id.as_str().map(str::to_owned) else {
            return Err(JsonRpcErrorWire::invalid_request());
        };

        let (response_sender, response_receiver) = tokio::sync::oneshot::channel();
        let mut state = self.state.lock().unwrap();
        let response_key = (host_node_id, request_id);
        if state.pending_responses.contains_key(&response_key) {
            return Err(JsonRpcErrorWire::internal_error(
                "asset_link_request_id_already_pending".to_owned(),
            ));
        }
        state
            .pending_responses
            .insert(response_key, response_sender);
        state.pending.push((
            host_node_id,
            AssetLinkMessage {
                payload: json!({
                    "jsonrpc": JSONRPC_VERSION,
                    "id": id,
                    "method": ASSET_LINK_AUTHORIZE_SWAP_METHOD,
                    "params": params,
                })
                .to_string(),
            },
        ));

        Ok(response_receiver)
    }

    pub(crate) fn forget_asset_link_response(&self, host_node_id: PublicKey, request_id: &str) {
        let mut state = self.state.lock().unwrap();
        state
            .pending_responses
            .remove(&(host_node_id, request_id.to_owned()));
    }

    fn queue_jsonrpc_value(&self, peer: PublicKey, value: Value) {
        let mut state = self.state.lock().unwrap();
        state.pending.push((
            peer,
            AssetLinkMessage {
                payload: value.to_string(),
            },
        ));
    }

    fn queue_jsonrpc_parse_error(&self, peer: PublicKey) {
        self.queue_jsonrpc_value(
            peer,
            jsonrpc_error_value(Value::Null, JsonRpcErrorWire::parse_error()),
        );
    }

    fn queue_jsonrpc_invalid_request(&self, peer: PublicKey) {
        self.queue_jsonrpc_value(
            peer,
            jsonrpc_error_value(Value::Null, JsonRpcErrorWire::invalid_request()),
        );
    }

    fn queue_jsonrpc_result(&self, peer: PublicKey, id: Value, result: impl Serialize) {
        self.queue_jsonrpc_value(peer, jsonrpc_result_value(id, result));
    }

    fn queue_jsonrpc_error(&self, peer: PublicKey, id: Value, code: i64, message: &str) {
        self.queue_jsonrpc_value(
            peer,
            jsonrpc_error_value(
                id,
                JsonRpcErrorWire {
                    code,
                    message: message.to_owned(),
                },
            ),
        );
    }

    fn complete_asset_link_response(
        &self,
        sender_node_id: PublicKey,
        id: Option<Value>,
        result: Option<Value>,
        error: Option<Value>,
    ) {
        let mut state = self.state.lock().unwrap();
        resolve_pending_response(
            &mut state.pending_responses,
            "asset_link",
            sender_node_id,
            id,
            result,
            error,
        );
    }

    fn receive_authorize(
        &self,
        sender_node_id: PublicKey,
        params: AssetLinkAuthorizeParamsWire,
    ) -> Result<AssetLinkAuthorizeResultWire, JsonRpcErrorWire> {
        if params.protocol_version != ASSET_LINK_PROTOCOL_VERSION {
            return Err(JsonRpcErrorWire::application_error(
                ASSET_LINK_ERROR_UNSUPPORTED_PROTOCOL_VERSION,
                "unsupported_protocol_version",
            ));
        }
        let Some(authorizer) = self.authorizer.lock().unwrap().clone() else {
            return Err(JsonRpcErrorWire::internal_error(
                "asset_link_authorizer_not_available".to_owned(),
            ));
        };
        authorizer.authorize_swap(sender_node_id, &params)?;
        Ok(AssetLinkAuthorizeResultWire { authorized: true })
    }
}

impl CustomMessageReader for AssetLinkMessageHandler {
    type CustomMessage = AssetLinkMessage;

    fn read<RD: LengthLimitedRead>(
        &self,
        message_type: u16,
        buffer: &mut RD,
    ) -> Result<Option<Self::CustomMessage>, DecodeError> {
        if message_type != ASSET_LINK_MESSAGE_TYPE_ID {
            return Ok(None);
        }

        Ok(Some(AssetLinkMessage::read_from_fixed_length_buffer(
            buffer,
        )?))
    }
}

impl CustomMessageHandler for AssetLinkMessageHandler {
    fn handle_custom_message(
        &self,
        msg: Self::CustomMessage,
        sender_node_id: PublicKey,
    ) -> Result<(), LightningError> {
        if !self.access_control.allows_peer(&sender_node_id) {
            warn!(peer = %sender_node_id, "rejected asset_link message from untrusted peer");
            return Ok(());
        }

        if msg.payload.as_bytes().contains(&0) {
            warn!(
                peer = %sender_node_id,
                payload_len = msg.payload.len(),
                "asset_link message contained a NUL byte"
            );
            self.queue_jsonrpc_parse_error(sender_node_id);
            return Ok(());
        }

        let envelope: CustomMsgRpcEnvelope = match serde_json::from_str(&msg.payload) {
            Ok(envelope) => envelope,
            Err(err) => {
                warn!(
                    error = %err,
                    peer = %sender_node_id,
                    payload_len = msg.payload.len(),
                    "failed to decode asset_link message"
                );
                self.queue_jsonrpc_parse_error(sender_node_id);
                return Ok(());
            }
        };

        if envelope.jsonrpc != JSONRPC_VERSION {
            self.queue_jsonrpc_invalid_request(sender_node_id);
            return Ok(());
        }

        if envelope.is_response_like() {
            if envelope.method.is_some() {
                warn!(
                    peer = %sender_node_id,
                    "ignoring malformed asset_link response envelope with method field"
                );
                return Ok(());
            }
            self.complete_asset_link_response(
                sender_node_id,
                envelope.id,
                envelope.result,
                envelope.error,
            );
            return Ok(());
        }

        let (Some(method), Some(id)) = (envelope.method, envelope.id) else {
            self.queue_jsonrpc_invalid_request(sender_node_id);
            return Ok(());
        };

        if method == ASSET_LINK_AUTHORIZE_SWAP_METHOD {
            let params_value = match envelope.params {
                Some(params) => params,
                None => {
                    self.queue_jsonrpc_error(
                        sender_node_id,
                        id,
                        JSONRPC_INVALID_PARAMS,
                        "missing params",
                    );
                    return Ok(());
                }
            };

            let params: AssetLinkAuthorizeParamsWire = match serde_json::from_value(params_value) {
                Ok(params) => params,
                Err(err) => {
                    warn!(error = %err, "invalid asset_link.authorize_swap params from {sender_node_id}");
                    self.queue_jsonrpc_error(
                        sender_node_id,
                        id,
                        JSONRPC_INVALID_PARAMS,
                        "invalid_authorize_swap_params",
                    );
                    return Ok(());
                }
            };

            match self.receive_authorize(sender_node_id, params) {
                Ok(result) => self.queue_jsonrpc_result(sender_node_id, id, result),
                Err(err) => self.queue_jsonrpc_error(sender_node_id, id, err.code, &err.message),
            }
            return Ok(());
        }

        self.queue_jsonrpc_error(
            sender_node_id,
            id,
            JSONRPC_METHOD_NOT_FOUND,
            "method not found",
        );
        Ok(())
    }

    fn get_and_clear_pending_msg(&self) -> Vec<(PublicKey, Self::CustomMessage)> {
        let mut state = self.state.lock().unwrap();
        std::mem::take(&mut state.pending)
    }

    fn peer_disconnected(&self, _their_node_id: PublicKey) {}

    fn peer_connected(
        &self,
        _their_node_id: PublicKey,
        _msg: &Init,
        _inbound: bool,
    ) -> Result<(), ()> {
        Ok(())
    }

    fn provided_node_features(&self) -> NodeFeatures {
        NodeFeatures::empty()
    }

    fn provided_init_features(&self, _their_node_id: PublicKey) -> InitFeatures {
        InitFeatures::empty()
    }
}
