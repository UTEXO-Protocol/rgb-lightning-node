//! Complete invoice admission for protocol-1.0 RGB sends.
use crate::error::APIError;
use rgb_lib::{
    wallet::{Invoice, Recipient, RecipientInfo, RecipientType, WitnessData},
    Assignment, BitcoinNetwork,
};
use std::collections::HashMap;

/// Derive a send operation deadline that leaves time for the proxy ACK/release exchange.
pub(crate) fn operation_expiration(
    recipients: &HashMap<String, Vec<Recipient>>,
    donation: bool,
    now: u64,
) -> Result<u64, APIError> {
    let mut expiration = now
        .checked_add(86400)
        .ok_or_else(|| APIError::InvalidRequest("RGB operation expiration overflow".into()))?;
    for recipient in recipients.values().flatten() {
        if recipient.transport_endpoints.is_empty() {
            continue;
        }
        let invoice_expiry = recipient.expiration_timestamp.ok_or_else(|| {
            APIError::InvalidRequest("Proxy RGB recipient requires invoice expiration".into())
        })?;
        if invoice_expiry <= now {
            return Err(APIError::InvalidRequest("RGB invoice has expired".into()));
        }
        if !donation {
            expiration =
                expiration.min(invoice_expiry.checked_sub(600).ok_or_else(|| {
                    APIError::InvalidRequest("RGB invoice expires too soon".into())
                })?);
        }
    }
    if expiration
        < now
            .checked_add(300)
            .ok_or_else(|| APIError::InvalidRequest("RGB operation expiration overflow".into()))?
    {
        return Err(APIError::InvalidRequest(
            "RGB invoice needs at least 15 minutes remaining for an acknowledged send".into(),
        ));
    }
    Ok(expiration)
}

pub(crate) fn recipient_from_invoice(
    invoice: String,
    asset_id: &str,
    amount: u64,
    network: BitcoinNetwork,
    witness_data: Option<WitnessData>,
) -> Result<Recipient, APIError> {
    if amount == 0 {
        return Err(APIError::InvalidAmount(
            "RGB amount must be positive".into(),
        ));
    }
    let data = Invoice::new(invoice)?.invoice_data();
    if data.network != network {
        return Err(APIError::InvalidRequest(
            "RGB invoice is on a different network".into(),
        ));
    }
    if data.asset_id.as_deref().is_some_and(|id| id != asset_id) {
        return Err(APIError::InvalidRequest(
            "RGB invoice requests a different asset".into(),
        ));
    }
    match data.assignment {
        Assignment::Any | Assignment::Fungible(0) => {}
        Assignment::Fungible(requested) if requested == amount => {}
        Assignment::Fungible(_) => {
            return Err(APIError::InvalidAmount(
                "RGB amount does not match the invoice".into(),
            ))
        }
        _ => {
            return Err(APIError::InvalidRequest(
                "Invoice-first fungible send does not support this assignment".into(),
            ))
        }
    }
    let info = RecipientInfo::new(data.recipient_id.clone())?;
    match (info.recipient_type, witness_data.as_ref()) {
        (RecipientType::Blind, Some(_)) => {
            return Err(APIError::InvalidRequest(
                "Blinded RGB invoice cannot have witness data".into(),
            ))
        }
        (RecipientType::Witness, None) => {
            return Err(APIError::InvalidRequest(
                "Witness RGB invoice requires a Bitcoin output amount".into(),
            ))
        }
        (RecipientType::Witness, Some(w))
            if !data.transport_endpoints.is_empty() && w.blinding.is_some() =>
        {
            return Err(APIError::InvalidRequest(
                "Proxy witness blinding is derived from the invoice keys".into(),
            ));
        }
        _ => {}
    }
    if !data.transport_endpoints.is_empty() {
        let key = data.public_key.as_deref().ok_or_else(|| {
            APIError::InvalidRequest("Proxy RGB invoice requires its recipient public key".into())
        })?;
        if key.len() != 64 || !key.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(APIError::InvalidRequest(
                "Invalid RGB recipient public key".into(),
            ));
        }
        if data.expiration_timestamp.is_none() {
            return Err(APIError::InvalidRequest(
                "Proxy RGB invoice requires expiration".into(),
            ));
        }
    }
    Ok(Recipient {
        recipient_id: data.recipient_id,
        witness_data,
        assignment: Assignment::Fungible(amount),
        transport_endpoints: data.transport_endpoints,
        public_key: data.public_key,
        encryption: data.encryption,
        expiration_timestamp: data.expiration_timestamp,
    })
}

/// Shared dispatch for the REST daemon and SDK after complete recipient admission.
/// The caller must retain the unlocked-session guard until this future completes.
pub(crate) async fn send_recipients(
    unlocked_state: std::sync::Arc<crate::utils::UnlockedAppState>,
    recipient_map: HashMap<String, Vec<Recipient>>,
    donation: bool,
    fee_rate: u64,
    min_confirmations: u8,
    operation_expiration: u64,
) -> Result<rgb_lib::wallet::OperationResult, APIError> {
    let result = if unlocked_state.external_signer_mode {
        let unlocked_state_copy = unlocked_state.clone();
        let begin_result = tokio::task::spawn_blocking(move || {
            unlocked_state_copy.rgb_send_begin(
                recipient_map,
                donation,
                fee_rate,
                min_confirmations,
                operation_expiration,
                false,
                None,
            )
        })
        .await
        .unwrap()?;
        let unlocked_state_copy = unlocked_state.clone();
        let signed_psbt = tokio::task::spawn_blocking(move || {
            unlocked_state_copy.rgb_sign_psbt(begin_result.psbt)
        })
        .await
        .unwrap()
        .map_err(|e| {
            tracing::error!("rgb_sign_psbt failed during RGB send: {e}");
            APIError::from(e)
        })?;
        let unlocked_state_copy = unlocked_state.clone();
        tokio::task::spawn_blocking(move || unlocked_state_copy.rgb_send_end(signed_psbt))
            .await
            .unwrap()?
    } else {
        let unlocked_state_copy = unlocked_state.clone();
        tokio::task::spawn_blocking(move || {
            unlocked_state_copy.rgb_send(
                recipient_map,
                donation,
                fee_rate,
                min_confirmations,
                operation_expiration,
            )
        })
        .await
        .unwrap()?
    };
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn invoice(encryption: rgbinvoice::Encryption) -> String {
        let mut bytes = vec![0, 20];
        bytes.extend([3; 20]);
        let recipient = rgb_lib::utils::recipient_id_from_script_buf(
            bitcoin::ScriptBuf::from_bytes(bytes),
            BitcoinNetwork::Regtest,
        );
        let beneficiary: rgbinvoice::XChainNet<rgbinvoice::Beneficiary> =
            recipient.parse().unwrap();
        rgbinvoice::RgbInvoiceBuilder::new(beneficiary)
            .set_amount_raw(7u64)
            .set_assignment_name("assetOwner")
            .set_expiry_timestamp(2000)
            .set_public_key([1; 32])
            .set_encryption(encryption)
            .add_transports(["rpc://proxy/json-rpc"])
            .unwrap()
            .finish()
            .to_string()
    }
    fn recipient(expiry: u64) -> Recipient {
        Recipient {
            recipient_id: String::new(),
            witness_data: None,
            assignment: Assignment::Fungible(1),
            transport_endpoints: vec!["rpc://proxy/json-rpc".into()],
            public_key: Some("01".repeat(32)),
            encryption: rgb_lib::Encryption::Required,
            expiration_timestamp: Some(expiry),
        }
    }
    #[test]
    fn acknowledged_send_reserves_the_proxy_expiration_margin() {
        let recipients = HashMap::from([("asset".into(), vec![recipient(2000), recipient(1500)])]);
        assert_eq!(operation_expiration(&recipients, false, 100).unwrap(), 900);
        assert!(operation_expiration(&recipients, false, 601).is_err());
        assert!(operation_expiration(&recipients, false, 1500).is_err());
    }
    #[test]
    fn missing_expiration_cannot_reach_signing() {
        let mut value = recipient(1000);
        value.expiration_timestamp = None;
        assert!(
            operation_expiration(&HashMap::from([("asset".into(), vec![value])]), false, 1)
                .is_err()
        );
    }
    #[test]
    fn out_of_band_send_does_not_invent_an_invoice_deadline() {
        let mut value = recipient(0);
        value.transport_endpoints.clear();
        value.expiration_timestamp = None;
        assert_eq!(
            operation_expiration(&HashMap::from([("asset".into(), vec![value])]), false, 1)
                .unwrap(),
            86401
        );
    }

    #[test]
    fn complete_invoice_preserves_proxy_authentication_and_requested_amount() {
        for (encoded, expected) in [
            (
                rgbinvoice::Encryption::Required,
                rgb_lib::Encryption::Required,
            ),
            (
                rgbinvoice::Encryption::Optional,
                rgb_lib::Encryption::Optional,
            ),
            (
                rgbinvoice::Encryption::Unsupported,
                rgb_lib::Encryption::Unsupported,
            ),
        ] {
            let encoded = invoice(encoded);
            let recipient = recipient_from_invoice(
                encoded.clone(),
                "selected",
                7,
                BitcoinNetwork::Regtest,
                Some(WitnessData {
                    amount_sat: 1000,
                    blinding: None,
                }),
            )
            .unwrap();
            assert_eq!(
                recipient.public_key.as_deref(),
                Some("01".repeat(32).as_str())
            );
            assert_eq!(recipient.encryption, expected);
            assert_eq!(recipient.expiration_timestamp, Some(2000));
            assert_eq!(recipient.assignment, Assignment::Fungible(7));
            assert!(recipient_from_invoice(
                encoded.clone(),
                "selected",
                8,
                BitcoinNetwork::Regtest,
                Some(WitnessData {
                    amount_sat: 1000,
                    blinding: None
                })
            )
            .is_err());
            assert!(recipient_from_invoice(
                encoded.clone(),
                "selected",
                7,
                BitcoinNetwork::Signet,
                Some(WitnessData {
                    amount_sat: 1000,
                    blinding: None
                })
            )
            .is_err());
            assert!(recipient_from_invoice(
                encoded.clone(),
                "selected",
                7,
                BitcoinNetwork::Regtest,
                None
            )
            .is_err());
            assert!(recipient_from_invoice(
                encoded,
                "selected",
                7,
                BitcoinNetwork::Regtest,
                Some(WitnessData {
                    amount_sat: 1000,
                    blinding: Some(1)
                })
            )
            .is_err());
        }
    }
}
