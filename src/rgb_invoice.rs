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
    recipient_from_invoice_assignment(
        invoice,
        asset_id,
        Assignment::Fungible(amount),
        network,
        witness_data,
    )
}

pub(crate) fn recipient_from_invoice_assignment(
    invoice: String,
    asset_id: &str,
    assignment: Assignment,
    network: BitcoinNetwork,
    witness_data: Option<WitnessData>,
) -> Result<Recipient, APIError> {
    if !matches!(
        assignment,
        Assignment::Fungible(1..)
            | Assignment::NonFungible
            | Assignment::InflationRight(1..)
            | Assignment::MintRight
    ) {
        return Err(APIError::InvalidAmount(
            "RGB send requires a concrete positive assignment".into(),
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
    // Any permits a concrete selection. A zero fungible invoice is only a fungible
    // amount placeholder; it cannot authorize a different assignment kind.
    if !(data.assignment == Assignment::Any
        || data.assignment == assignment
        || (data.assignment == Assignment::Fungible(0)
            && matches!(assignment, Assignment::Fungible(_))))
    {
        return Err(APIError::InvalidAmount(
            "RGB assignment does not match the invoice".into(),
        ));
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
        assignment,
        transport_endpoints: data.transport_endpoints,
        public_key: data.public_key,
        encryption: data.encryption,
        expiration_timestamp: data.expiration_timestamp,
    })
}

/// Complete-invoice batch boundaries shared by REST and generated native SDKs.
pub(crate) const MAX_BATCH_ASSETS: usize = 64;
pub(crate) const MAX_BATCH_RECIPIENTS: usize = 256;
pub(crate) const MAX_INVOICE_BYTES: usize = 16 * 1024;
pub(crate) const MAX_BATCH_INVOICE_BYTES: usize = 1024 * 1024;

#[derive(Clone)]
pub(crate) struct InvoiceRecipient {
    pub(crate) invoice: String,
    pub(crate) assignment: Assignment,
    pub(crate) witness_data: Option<WitnessData>,
}

pub(crate) fn validate_batch_bounds<'a, I, J>(groups: I) -> Result<(), APIError>
where
    I: IntoIterator<Item = J>,
    J: IntoIterator<Item = &'a str>,
{
    let invalid = || {
        APIError::InvalidRequest(
            "RGB invoice batch exceeds its bounds or has an empty group".into(),
        )
    };
    let mut assets = 0usize;
    let mut recipients = 0usize;
    let mut bytes = 0usize;
    for group in groups {
        assets += 1;
        if assets > MAX_BATCH_ASSETS {
            return Err(invalid());
        }
        let before = recipients;
        for invoice in group {
            recipients = recipients.checked_add(1).ok_or_else(invalid)?;
            bytes = bytes.checked_add(invoice.len()).ok_or_else(invalid)?;
            if invoice.is_empty()
                || invoice.len() > MAX_INVOICE_BYTES
                || recipients > MAX_BATCH_RECIPIENTS
                || bytes > MAX_BATCH_INVOICE_BYTES
            {
                return Err(invalid());
            }
        }
        if before == recipients {
            return Err(invalid());
        }
    }
    if assets == 0 {
        return Err(invalid());
    }
    Ok(())
}

pub(crate) fn recipients_from_invoices(
    groups: Vec<(String, Vec<InvoiceRecipient>)>,
    network: BitcoinNetwork,
) -> Result<HashMap<String, Vec<Recipient>>, APIError> {
    use std::{collections::HashSet, str::FromStr};
    validate_batch_bounds(
        groups
            .iter()
            .map(|(_, recipients)| recipients.iter().map(|r| r.invoice.as_str())),
    )?;
    let invalid = || {
        APIError::InvalidRequest("Invalid RGB invoice batch bounds or duplicate recipient".into())
    };
    if groups.is_empty() || groups.len() > MAX_BATCH_ASSETS {
        return Err(invalid());
    }
    let mut recipients = HashMap::new();
    let mut total_count = 0usize;
    let mut total_bytes = 0usize;
    for (asset_id, group) in groups {
        let asset_id = rgb_lib::ContractId::from_str(&asset_id)
            .map_err(|_| APIError::InvalidAssetID("Invalid RGB batch asset ID".into()))?
            .to_string();
        total_count = total_count.checked_add(group.len()).ok_or_else(invalid)?;
        if group.is_empty()
            || total_count > MAX_BATCH_RECIPIENTS
            || recipients.contains_key(&asset_id)
        {
            return Err(invalid());
        }
        let mut seen = HashSet::new();
        let mut admitted = Vec::with_capacity(group.len());
        for input in group {
            total_bytes = total_bytes
                .checked_add(input.invoice.len())
                .ok_or_else(invalid)?;
            if input.invoice.is_empty()
                || input.invoice.len() > MAX_INVOICE_BYTES
                || total_bytes > MAX_BATCH_INVOICE_BYTES
            {
                return Err(invalid());
            }
            let recipient = recipient_from_invoice_assignment(
                input.invoice,
                &asset_id,
                input.assignment,
                network,
                input.witness_data,
            )?;
            if !seen.insert(recipient.recipient_id.clone()) {
                return Err(invalid());
            }
            admitted.push(recipient);
        }
        recipients.insert(asset_id, admitted);
    }
    Ok(recipients)
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

#[cfg(test)]
mod batch_tests {
    use super::*;
    const ASSET: &str = "rgb:OG9No4hZ-UrxoWmR-09ohtOk-ppxBN7h-g~KkQro-ncKc9A4";
    const OTHER: &str = "rgb:bslIXT4U-xgVHutV-XwymZwu-4chvqMh-D~jaKFe-ZCTMTwQ";

    fn invoice(
        tag: u8,
        amount: Option<u64>,
        contract: Option<&str>,
        key: bool,
        expiry: bool,
    ) -> String {
        let mut bytes = vec![0, 20];
        bytes.extend([tag; 20]);
        let id = rgb_lib::utils::recipient_id_from_script_buf(
            bitcoin::ScriptBuf::from_bytes(bytes),
            BitcoinNetwork::Regtest,
        );
        let mut builder = rgbinvoice::RgbInvoiceBuilder::new(
            id.parse::<rgbinvoice::XChainNet<rgbinvoice::Beneficiary>>()
                .unwrap(),
        );
        if let Some(amount) = amount {
            builder = builder
                .set_amount_raw(amount)
                .set_assignment_name("assetOwner");
        }
        if let Some(contract) = contract {
            builder = builder.set_contract(contract.parse().unwrap());
        }
        if key {
            builder = builder.set_public_key([1; 32]);
        }
        if expiry {
            builder = builder.set_expiry_timestamp(2000);
        }
        builder
            .set_encryption(rgbinvoice::Encryption::Required)
            .add_transports(["rpc://proxy/json-rpc"])
            .unwrap()
            .finish()
            .to_string()
    }
    fn input(invoice: String, assignment: Assignment) -> InvoiceRecipient {
        InvoiceRecipient {
            invoice,
            assignment,
            witness_data: Some(WitnessData {
                amount_sat: 1000,
                blinding: None,
            }),
        }
    }
    fn admit(group: Vec<InvoiceRecipient>) -> Result<HashMap<String, Vec<Recipient>>, APIError> {
        recipients_from_invoices(vec![(ASSET.into(), group)], BitcoinNetwork::Regtest)
    }
    #[test]
    fn authenticated_batch_preserves_all_invoice_fields_and_explicit_assignments() {
        let map = admit(vec![
            input(
                invoice(1, Some(7), Some(ASSET), true, true),
                Assignment::Fungible(7),
            ),
            input(invoice(2, None, None, true, true), Assignment::NonFungible),
            input(
                invoice(3, None, None, true, true),
                Assignment::InflationRight(5),
            ),
        ])
        .unwrap();
        let group = &map[ASSET];
        assert_eq!(group.len(), 3);
        assert_eq!(group[1].assignment, Assignment::NonFungible);
        assert_eq!(group[2].assignment, Assignment::InflationRight(5));
        for recipient in group {
            assert_eq!(
                recipient.public_key.as_deref(),
                Some("01".repeat(32).as_str())
            );
            assert_eq!(recipient.encryption, rgb_lib::Encryption::Required);
            assert_eq!(recipient.expiration_timestamp, Some(2000));
        }
        assert_eq!(operation_expiration(&map, false, 100).unwrap(), 1400);
    }
    #[test]
    fn batch_rejects_network_asset_amount_assignment_and_witness_contradictions() {
        let valid = invoice(1, Some(7), Some(ASSET), true, true);
        for assignment in [
            Assignment::Fungible(0),
            Assignment::Fungible(8),
            Assignment::NonFungible,
            Assignment::InflationRight(7),
            Assignment::Any,
        ] {
            assert!(admit(vec![input(valid.clone(), assignment)]).is_err());
        }
        assert!(recipients_from_invoices(
            vec![(
                ASSET.into(),
                vec![input(valid.clone(), Assignment::Fungible(7))]
            )],
            BitcoinNetwork::Signet
        )
        .is_err());
        assert!(admit(vec![input(
            invoice(1, Some(7), Some(OTHER), true, true),
            Assignment::Fungible(7)
        )])
        .is_err());
        let mut bad = input(valid.clone(), Assignment::Fungible(7));
        bad.witness_data = None;
        assert!(admit(vec![bad]).is_err());
        let mut bad = input(valid, Assignment::Fungible(7));
        bad.witness_data.as_mut().unwrap().blinding = Some(1);
        assert!(admit(vec![bad]).is_err());
        // A zero fungible placeholder never authorizes an NFT or inflation right.
        assert!(admit(vec![input(
            invoice(1, Some(0), None, true, true),
            Assignment::NonFungible
        )])
        .is_err());
    }
    #[test]
    fn batch_rejects_missing_keys_expiration_and_expired_operations_before_dispatch() {
        assert!(admit(vec![input(
            invoice(1, Some(7), None, false, true),
            Assignment::Fungible(7)
        )])
        .is_err());
        assert!(admit(vec![input(
            invoice(1, Some(7), None, true, false),
            Assignment::Fungible(7)
        )])
        .is_err());
        let map = admit(vec![input(
            invoice(1, Some(7), None, true, true),
            Assignment::Fungible(7),
        )])
        .unwrap();
        assert!(operation_expiration(&map, false, 2001).is_err());
        assert!(operation_expiration(&map, false, 1701).is_err());
    }
    #[test]
    fn batch_rejects_duplicate_groups_and_recipient_aliases_without_overwriting() {
        let value = input(
            invoice(1, Some(7), None, true, true),
            Assignment::Fungible(7),
        );
        assert!(admit(vec![value.clone(), value.clone()]).is_err());
        assert!(recipients_from_invoices(
            vec![
                (ASSET.into(), vec![value.clone()]),
                (
                    ASSET.into(),
                    vec![input(
                        invoice(2, Some(7), None, true, true),
                        Assignment::Fungible(7)
                    )]
                )
            ],
            BitcoinNetwork::Regtest
        )
        .is_err());
        assert!(admit(vec![]).is_err());
        assert!(recipients_from_invoices(vec![], BitcoinNetwork::Regtest).is_err());
        assert!(recipients_from_invoices(
            vec![("invalid".into(), vec![value])],
            BitcoinNetwork::Regtest
        )
        .is_err());
    }
    #[test]
    fn batch_bounds_are_the_same_before_rest_and_sdk_conversion() {
        assert!(validate_batch_bounds(vec![vec!["x"; MAX_BATCH_RECIPIENTS + 1]]).is_err());
        assert!(validate_batch_bounds(vec![vec!["x"]; MAX_BATCH_ASSETS + 1]).is_err());
        assert!(validate_batch_bounds(vec![vec![""]]).is_err());
        assert!(validate_batch_bounds(vec![Vec::<&str>::new()]).is_err());
        let large = "x".repeat(MAX_INVOICE_BYTES + 1);
        assert!(validate_batch_bounds(vec![vec![large.as_str()]]).is_err());
        let max = "x".repeat(MAX_INVOICE_BYTES);
        assert!(validate_batch_bounds(vec![vec![
            max.as_str();
            MAX_BATCH_INVOICE_BYTES / MAX_INVOICE_BYTES + 1
        ]])
        .is_err());
        assert!(validate_batch_bounds(vec![vec!["x"; MAX_BATCH_RECIPIENTS]]).is_ok());
    }
}
