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

/// Reject legacy link-right requests before acquiring an RGB mutation guard.
/// Ordinary IFA issuance remains supported; link rights need library support.
pub(crate) fn validate_ifa_issuance(
    issuance_type: &Option<IfaIssuanceType>,
) -> Result<(), crate::error::APIError> {
    if matches!(issuance_type, None | Some(IfaIssuanceType::Legacy)) {
        Ok(())
    } else {
        Err(crate::error::APIError::InvalidRequest(
            "IFA link rights are unavailable in the BFA RGB library".into(),
        ))
    }
}

pub(crate) struct ColorPrepareResult {
    pub(crate) batch_transfer_idx: i32,
    pub(crate) transfers: Vec<rgb_lib::RgbTransfer>,
}
