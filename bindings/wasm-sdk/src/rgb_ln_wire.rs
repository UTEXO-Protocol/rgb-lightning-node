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
use lightning::util::ser::{LengthLimitedRead, Readable, Writeable, Writer};
#[cfg(target_arch = "wasm32")]
use wasm_bindgen::JsValue;

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

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RgbLnForkCustomMessage {
    CapabilityPing(RgbLnForkCapabilityPing),
}

impl lightning::ln::wire::Type for RgbLnForkCustomMessage {
    fn type_id(&self) -> u16 {
        match self {
            RgbLnForkCustomMessage::CapabilityPing(_) => RGB_LN_FORK_CUSTOM_CAP_PING_TYPE,
        }
    }
}

impl Writeable for RgbLnForkCustomMessage {
    fn write<W: Writer>(&self, w: &mut W) -> Result<(), io::Error> {
        match self {
            RgbLnForkCustomMessage::CapabilityPing(inner) => inner.write(w),
        }
    }
}

#[derive(Debug, Default)]
pub struct RgbLnForkCustomMessageHandler;

impl RgbLnForkCustomMessageHandler {
    pub fn new() -> Self {
        Self
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
        if message_type != RGB_LN_FORK_CUSTOM_CAP_PING_TYPE {
            return Ok(None);
        }
        let wire_version: u16 = Readable::read(buffer)?;
        Ok(Some(RgbLnForkCustomMessage::CapabilityPing(
            RgbLnForkCapabilityPing { wire_version },
        )))
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
                Ok(())
            }
        }
    }

    fn get_and_clear_pending_msg(&self) -> Vec<(PublicKey, Self::CustomMessage)> {
        Vec::new()
    }

    fn peer_disconnected(&self, their_node_id: PublicKey) {
        fork_log(&format!(
            "rgb-ln-fork-wire: peer disconnected {their_node_id}"
        ));
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
