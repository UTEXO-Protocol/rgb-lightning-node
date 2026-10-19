//! Compatibility input types for fork extensions disabled in the local BFA test build.
//! These preserve the wire shape so unsupported requests are rejected explicitly.

pub(crate) const ONCHAIN_ONLY: bool = true;

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

// No producer exists in this build: preparation and consumption both return an
// error. In particular, do not substitute color_psbt_and_consume for preparation.
pub(crate) struct ColorPrepareResult {
    pub(crate) batch_transfer_idx: i32,
    pub(crate) transfers: Vec<rgb_lib::RgbTransfer>,
}
