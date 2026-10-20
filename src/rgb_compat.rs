//! Explicit compatibility vocabulary for legacy fork APIs not implemented by rgb-lib-bfa.
//!
//! Their existing request types remain decodable, but unsupported operations fail before signing
//! or mutating RGB state. In particular, a durable preparation is never replaced by eager consume.
pub(crate) use crate::bfa_values::burn_reason;

#[derive(serde::Deserialize, serde::Serialize)]
pub(crate) enum IfaIssuanceType {
    Legacy,
    LinkRightOnly,
    LinkedFromParent {
        contract_id: String,
        request_link_right: bool,
    },
}

pub(crate) struct ColorPrepareResult {
    pub(crate) batch_transfer_idx: i32,
    pub(crate) transfers: Vec<rgb_lib::RgbTransfer>,
}
