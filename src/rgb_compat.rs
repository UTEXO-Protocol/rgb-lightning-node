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
/// Ordinary IFA issuance remains supported; the link workflow is not exposed by this adapter.
pub(crate) fn validate_ifa_issuance(
    issuance_type: &Option<IfaIssuanceType>,
) -> Result<(), crate::error::APIError> {
    if matches!(issuance_type, None | Some(IfaIssuanceType::Legacy)) {
        Ok(())
    } else {
        Err(crate::error::APIError::InvalidRequest(
            "IFA link issuance is not exposed by this RLN adapter".into(),
        ))
    }
}

pub(crate) struct ColorPrepareResult {
    pub(crate) batch_transfer_idx: i32,
    pub(crate) transfers: Vec<rgb_lib::RgbTransfer>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bfa5_preserves_legacy_ifa_admission_and_rejects_unexposed_link_workflows() {
        assert!(validate_ifa_issuance(&None).is_ok());
        assert!(validate_ifa_issuance(&Some(IfaIssuanceType::Legacy)).is_ok());
        for request in [
            IfaIssuanceType::LinkRightOnly,
            IfaIssuanceType::LinkedFromParent {
                contract_id: "untrusted-parent".into(),
                request_link_right: true,
            },
        ] {
            assert!(matches!(
                validate_ifa_issuance(&Some(request)),
                Err(crate::error::APIError::InvalidRequest(_))
            ));
        }
    }
}
