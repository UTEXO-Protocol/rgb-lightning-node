use amplify::s;
use bitcoin::{hashes::Hash, secp256k1::PublicKey};
use lightning::{
    io,
    ln::{
        channelmanager::{PaymentId, RecipientOnionFields},
        msgs::{DecodeError, Init, LightningError},
        peer_handler::CustomMessageHandler,
        wire::{CustomMessageReader, Type},
    },
    routing::router::{Path as LnPath, PaymentParameters, Route, RouteParameters},
    types::{
        features::{InitFeatures, NodeFeatures},
        payment::PaymentHash,
    },
    util::{
        ser::{LengthLimitedRead, LengthReadable, WithoutLength, Writeable, Writer},
        IS_SWAP_SCID,
    },
};
use lightning_invoice::{Bolt11Invoice, PaymentSecret};
use rgb_lib::{AssetSchema as RgbLibAssetSchema, ContractId};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    str::FromStr,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::time::timeout;
use tracing::warn;

use crate::custom_msg_rpc::{
    jsonrpc_error_value, jsonrpc_result_value, resolve_pending_response,
    CustomMsgPeerAccessControl, CustomMsgRpcEnvelope, CustomMsgRpcResponseReceiver,
    JsonRpcErrorWire, PendingResponses, JSONRPC_INVALID_PARAMS, JSONRPC_METHOD_NOT_FOUND,
    JSONRPC_MSG_RESPONSE_TIMEOUT_SECS, JSONRPC_VERSION,
};
use crate::{
    core_types::asset_link::{AssetLinkRequest, AssetLinkResponse},
    core_types::{HTLCStatus, MAX_SWAP_FEE_MSAT},
    error::APIError,
    ldk::{clear_rgb_payment_pending, write_rgb_payment_info_file, PaymentInfo},
    rgb::get_rgb_channel_info_optional,
    utils::{
        description_from_invoice, description_hash_from_invoice, get_current_timestamp, get_route,
        hex_str, new_jsonrpc_request_id, UnlockedAppState,
    },
};

const ASSET_LINK_AUTHORIZE_SWAP_METHOD: &str = "asset_link.authorize_swap";
pub(crate) const ASSET_LINK_ERROR_DUPLICATE_PAYMENT_HASH: i64 = 1203;
pub(crate) const ASSET_LINK_ERROR_INSUFFICIENT_LIQUIDITY: i64 = 1202;
pub(crate) const ASSET_LINK_ERROR_UNKNOWN_ASSET: i64 = 1204;
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

pub(crate) struct LinkedAssetPaymentResult {
    pub(crate) payment_id: PaymentId,
    pub(crate) payment_hash: PaymentHash,
    pub(crate) payment_secret: PaymentSecret,
    pub(crate) status: HTLCStatus,
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

pub(crate) fn create_asset_link(
    unlocked_state: &UnlockedAppState,
    params: AssetLinkRequest,
) -> Result<AssetLinkResponse, APIError> {
    if unlocked_state.external_signer_mode {
        return Err(APIError::UnsupportedInExternalSignerMode(s!(
            "asset linking is not supported in external signer mode"
        )));
    }

    let parent_id = params.parent_asset_id.trim().to_string();
    let child_id = params.child_asset_id.trim().to_string();
    let parent_contract_id = ContractId::from_str(&parent_id)
        .map_err(|_| APIError::InvalidAssetID(parent_id.clone()))?;
    let child_contract_id =
        ContractId::from_str(&child_id).map_err(|_| APIError::InvalidAssetID(child_id.clone()))?;

    if parent_contract_id == child_contract_id {
        return Err(APIError::InvalidRequest(s!(
            "parent and child contracts must be different"
        )));
    }

    let parent_metadata = unlocked_state.rgb_get_asset_metadata(parent_contract_id)?;
    let child_metadata = unlocked_state.rgb_get_asset_metadata(child_contract_id)?;
    if parent_metadata.asset_schema != RgbLibAssetSchema::Ifa
        || child_metadata.asset_schema != RgbLibAssetSchema::Ifa
    {
        return Err(APIError::InvalidContractLink(s!(
            "asset linking is only supported for IFA assets"
        )));
    }

    match child_metadata.linked_from_asset_id.as_deref() {
        Some(existing_parent_id) if existing_parent_id == parent_id => {}
        Some(existing_parent_id) => {
            return Err(APIError::InvalidContractLink(format!(
                "child_asset_id {child_id} is already linked from {existing_parent_id}"
            )));
        }
        None => {
            return Err(APIError::InvalidContractLink(format!(
                "child_asset_id {child_id} does not declare parent_asset_id {parent_id} as its parent"
            )));
        }
    }

    match parent_metadata.linked_to_asset_id.as_deref() {
        Some(existing_child_id) if existing_child_id == child_id => {
            let link_transfer = unlocked_state.rgb_find_link_transfer(parent_contract_id)?;
            let created_at = link_transfer
                .as_ref()
                .map(|transfer| transfer.created_at.max(0) as u64)
                .unwrap_or_else(get_current_timestamp);
            return Ok(AssetLinkResponse {
                parent_asset_id: parent_id,
                child_asset_id: Some(child_id),
                created_at: Some(created_at),
                txid: link_transfer.and_then(|transfer| transfer.txid),
            });
        }
        Some(existing_child_id) => {
            return Err(APIError::InvalidContractLink(format!(
                "parent_asset_id {parent_id} is already linked to {existing_child_id}"
            )));
        }
        None => {}
    }

    let link_right_outpoint = unlocked_state
        .rgb_find_link_right_outpoint(parent_contract_id)?
        .ok_or_else(|| {
            APIError::InvalidRequest(format!(
                "missing unspent_link_right_outpoint for parent_asset_id {parent_id}"
            ))
        })?;
    let link_ifa = unlocked_state.rgb_link_ifa(
        parent_id.clone(),
        child_id.clone(),
        link_right_outpoint,
        unlocked_state.config.rgb.fee_rate_sat_vb,
        params.min_confirmations,
    )?;
    let link_transfer = unlocked_state.rgb_find_link_transfer(parent_contract_id)?;
    let created_at = link_transfer
        .as_ref()
        .map(|transfer| transfer.created_at.max(0) as u64)
        .unwrap_or_else(get_current_timestamp);

    Ok(AssetLinkResponse {
        parent_asset_id: parent_id,
        child_asset_id: Some(child_id),
        created_at: Some(created_at),
        txid: Some(link_ifa.txid),
    })
}

pub(crate) fn find_linked_asset_channel(
    unlocked_state: &UnlockedAppState,
    contract_id: ContractId,
    asset_amount: u64,
    amt_msat: u64,
    recipient_pubkey: PublicKey,
) -> Option<(ContractId, PublicKey)> {
    let asset_id = contract_id.to_string();
    let validate_locally_discoverable_asset_link = |linked_contract_id: ContractId| -> Option<()> {
        let linked_metadata = unlocked_state
            .rgb_get_asset_metadata(linked_contract_id)
            .ok()?;
        if linked_metadata.asset_schema != RgbLibAssetSchema::Ifa {
            return None;
        }

        let linked_asset_id = linked_contract_id.to_string();
        if linked_metadata.linked_from_asset_id.as_deref() == Some(asset_id.as_str()) {
            if let Ok(parent_metadata) = unlocked_state.rgb_get_asset_metadata(contract_id) {
                if parent_metadata.linked_to_asset_id.as_deref() != Some(linked_asset_id.as_str()) {
                    return None;
                }
                let is_link_settled = unlocked_state
                    .rgb_find_link_transfer(contract_id)
                    .ok()?
                    .is_some_and(|transfer| transfer.status == rgb_lib::TransferStatus::Settled);
                if !is_link_settled {
                    return None;
                }
            }
        } else if linked_metadata.linked_to_asset_id.as_deref() == Some(asset_id.as_str()) {
            if let Ok(child_metadata) = unlocked_state.rgb_get_asset_metadata(contract_id) {
                if child_metadata.linked_from_asset_id.as_deref() != Some(linked_asset_id.as_str())
                {
                    return None;
                }
            }
            let is_link_settled = unlocked_state
                .rgb_find_link_transfer(linked_contract_id)
                .ok()?
                .is_some_and(|transfer| transfer.status == rgb_lib::TransferStatus::Settled);
            if !is_link_settled {
                return None;
            }
        } else {
            return None;
        }

        Some(())
    };

    unlocked_state
        .channel_manager
        .list_usable_channels()
        .into_iter()
        .filter_map(|channel| {
            if channel.counterparty.node_id == recipient_pubkey
                || channel.next_outbound_htlc_minimum_msat > amt_msat
                || channel.next_outbound_htlc_limit_msat < amt_msat
            {
                return None;
            }

            let rgb_info = get_rgb_channel_info_optional(
                &channel.channel_id,
                false,
                unlocked_state.kv_store.as_ref(),
            )?;
            if rgb_info.contract_id == contract_id || rgb_info.local_rgb_amount < asset_amount {
                return None;
            }

            validate_locally_discoverable_asset_link(rgb_info.contract_id)?;

            Some((
                rgb_info.contract_id,
                channel.counterparty.node_id,
                rgb_info.local_rgb_amount,
            ))
        })
        .max_by_key(|(_, _, local_rgb_amount)| *local_rgb_amount)
        .map(|(linked_contract_id, host_pubkey, _)| (linked_contract_id, host_pubkey))
}

pub(crate) fn has_sufficient_asset_channel(
    unlocked_state: &UnlockedAppState,
    contract_id: ContractId,
    asset_amount: u64,
    amt_msat: u64,
) -> bool {
    unlocked_state
        .channel_manager
        .list_usable_channels()
        .iter()
        .any(|channel| {
            channel.next_outbound_htlc_minimum_msat <= amt_msat
                && channel.next_outbound_htlc_limit_msat >= amt_msat
                && get_rgb_channel_info_optional(
                    &channel.channel_id,
                    false,
                    unlocked_state.kv_store.as_ref(),
                )
                .is_some_and(|rgb_info| {
                    rgb_info.contract_id == contract_id && rgb_info.local_rgb_amount >= asset_amount
                })
        })
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn send_linked_asset_payment(
    unlocked_state: &UnlockedAppState,
    invoice: &Bolt11Invoice,
    contract_id: ContractId,
    linked_contract_id: ContractId,
    asset_amount: u64,
    amt_msat: u64,
    host_pubkey: PublicKey,
) -> Result<LinkedAssetPaymentResult, APIError> {
    let payment_hash = PaymentHash(invoice.payment_hash().to_byte_array());
    let payment_secret = *invoice.payment_secret();
    let recipient_pubkey = invoice.recover_payee_pub_key();

    if contract_id == linked_contract_id
        || host_pubkey == unlocked_state.runtime_node_id()
        || host_pubkey == recipient_pubkey
    {
        return Err(APIError::InvalidRequest(s!(
            "linked payment requires distinct payer, host, recipient, and asset IDs"
        )));
    }

    let first_leg = get_route(
        unlocked_state.config.as_ref(),
        &unlocked_state.channel_manager,
        &unlocked_state.router,
        unlocked_state.kv_store.as_ref(),
        unlocked_state.runtime_node_id(),
        host_pubkey,
        Some(amt_msat),
        Some((linked_contract_id, asset_amount)),
        vec![],
    );
    let second_leg = get_route(
        unlocked_state.config.as_ref(),
        &unlocked_state.channel_manager,
        &unlocked_state.router,
        unlocked_state.kv_store.as_ref(),
        host_pubkey,
        recipient_pubkey,
        Some(amt_msat),
        Some((contract_id, asset_amount)),
        invoice.route_hints(),
    );
    let (mut first_leg, mut second_leg) = match (first_leg, second_leg) {
        (Some(first_leg), Some(second_leg)) => (first_leg, second_leg),
        _ => return Err(APIError::NoRoute),
    };

    second_leg.paths[0].hops[0].short_channel_id |= IS_SWAP_SCID;
    first_leg.paths[0]
        .hops
        .last_mut()
        .expect("Path not to be empty")
        .fee_msat = 0;

    let mut fullpaths = first_leg.paths[0]
        .hops
        .clone()
        .into_iter()
        .map(|mut hop| {
            hop.rgb_payment = Some((linked_contract_id, asset_amount));
            hop
        })
        .chain(second_leg.paths[0].hops.clone().into_iter().map(|mut hop| {
            hop.rgb_payment = Some((contract_id, asset_amount));
            hop
        }))
        .collect::<Vec<_>>();

    if let Some(last_hop) = fullpaths.last_mut() {
        last_hop.cltv_expiry_delta = last_hop
            .cltv_expiry_delta
            .max(invoice.min_final_cltv_expiry_delta() as u32);
    }

    let total_fee = fullpaths
        .iter()
        .rev()
        .skip(1)
        .map(|hop| hop.fee_msat)
        .sum::<u64>();
    if total_fee >= MAX_SWAP_FEE_MSAT {
        return Err(APIError::FailedPayment(format!(
            "Fee too high: {total_fee}"
        )));
    }

    let mut recipient_onion = RecipientOnionFields::secret_only(payment_secret);
    recipient_onion.payment_metadata = invoice.payment_metadata().cloned();

    let mut route_params = RouteParameters::from_payment_params_and_value(
        PaymentParameters::from_bolt11_invoice(invoice),
        amt_msat,
        Some((contract_id, asset_amount)),
    );
    route_params
        .set_max_path_length(
            &recipient_onion,
            false,
            unlocked_state.channel_manager.current_best_block().height,
        )
        .map_err(|()| APIError::FailedPayment(s!("onion packet size exceeded")))?;
    let route = Route {
        paths: vec![LnPath {
            hops: fullpaths,
            blinded_tail: None,
        }],
        route_params: Some(route_params),
    };

    let params = AssetLinkAuthorizeParamsWire {
        protocol_version: ASSET_LINK_PROTOCOL_VERSION,
        payment_hash: hex_str(&payment_hash.0),
        asset_id: contract_id.to_string(),
        linked_asset_id: linked_contract_id.to_string(),
        amount: asset_amount,
        expiry_sec: invoice.duration_until_expiry().as_secs(),
    };
    let request_id = new_jsonrpc_request_id();
    let response_rx = unlocked_state
        .asset_link_handler
        .queue_asset_link_authorize(host_pubkey, Value::String(request_id.clone()), params)
        .map_err(|err| APIError::InvalidRequest(err.message))?;
    unlocked_state.peer_manager.process_events();
    match timeout(
        Duration::from_secs(JSONRPC_MSG_RESPONSE_TIMEOUT_SECS),
        response_rx,
    )
    .await
    {
        Ok(Ok(Ok(result))) => {
            let authorized = serde_json::from_value::<AssetLinkAuthorizeResultWire>(result)
                .map(|result| result.authorized)
                .unwrap_or(false);
            if !authorized {
                return Err(APIError::InvalidRequest(s!(
                    "host did not authorize the linked payment"
                )));
            }
        }
        Ok(Ok(Err(err))) => {
            return Err(APIError::InvalidRequest(format!(
                "authorize_swap error {}: {}",
                err.code, err.message
            )));
        }
        Ok(Err(_)) => {
            return Err(APIError::Network(s!(
                "linked payment response channel closed before the host replied"
            )));
        }
        Err(_) => {
            unlocked_state
                .asset_link_handler
                .forget_asset_link_response(host_pubkey, &request_id);
            return Err(APIError::Network(s!(
                "linked payment timed out waiting for host authorization"
            )));
        }
    }

    let created_at = get_current_timestamp();
    let payment_id = PaymentId(payment_hash.0);
    let mut status = HTLCStatus::Pending;
    unlocked_state.add_outbound_payment(
        payment_id,
        PaymentInfo {
            preimage: None,
            secret: Some(payment_secret),
            status,
            amt_msat: Some(amt_msat),
            created_at,
            updated_at: created_at,
            payee_pubkey: invoice.get_payee_pub_key(),
            expires_at: None,
            claim_deadline_height: None,
            invoice_type: None,
            description: description_from_invoice(invoice),
            description_hash: description_hash_from_invoice(invoice),
            payment_idx: None,
            async_hash_index: None,
            async_host_node_id: None,
        },
    )?;
    write_rgb_payment_info_file(
        &payment_hash,
        linked_contract_id,
        asset_amount,
        true,
        false,
        unlocked_state.kv_store.as_ref(),
    );

    match unlocked_state.channel_manager.send_payment_with_route(
        route,
        payment_hash,
        recipient_onion,
        payment_id,
    ) {
        Ok(()) => {
            tracing::info!(
                asset_id = %contract_id,
                linked_asset_id = %linked_contract_id,
                amount = asset_amount,
                recipient = %recipient_pubkey,
                host = %host_pubkey,
                "sent linked-asset payment"
            );
        }
        Err(err) => {
            tracing::error!("ERROR: failed to send linked-asset payment: {err:?}");
            clear_rgb_payment_pending(&payment_hash, false, unlocked_state.kv_store.as_ref());
            status = HTLCStatus::Failed;
            unlocked_state.update_outbound_payment_status(payment_id, status);
        }
    }

    Ok(LinkedAssetPaymentResult {
        payment_id,
        payment_hash,
        payment_secret,
        status,
    })
}
