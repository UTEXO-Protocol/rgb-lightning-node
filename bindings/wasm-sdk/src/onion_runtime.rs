use secp256k1::PublicKey;
use serde::Deserialize;
use std::str::FromStr;
use wasm_bindgen::prelude::JsValue;

#[derive(Clone, Debug, Deserialize)]
struct SendOnionMessageRequestData {
    node_ids: Vec<String>,
    tlv_type: u64,
    data: String,
}

pub(crate) fn send_onion_message(request_json: String) -> Result<(), JsValue> {
    crate::ensure_sdk_node_runtime_allowed()?;
    let request: SendOnionMessageRequestData = serde_json::from_str(&request_json)
        .map_err(|e| JsValue::from_str(&format!("invalid send_onion_message request JSON: {e}")))?;

    if request.node_ids.is_empty() {
        return Err(JsValue::from_str(
            "SendOnionMessage requires at least one node id for the path",
        ));
    }
    for pk_str in request.node_ids {
        PublicKey::from_str(&pk_str)
            .map_err(|_| JsValue::from_str(&format!("Couldn't parse peer_pubkey '{pk_str}'")))?;
    }

    if request.tlv_type < 64 {
        return Err(JsValue::from_str("need an integral message type above 64"));
    }

    if hex::decode(request.data).is_err() {
        return Err(JsValue::from_str("need a hex data string"));
    }

    Ok(())
}
