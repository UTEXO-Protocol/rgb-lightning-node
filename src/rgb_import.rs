use crate::error::APIError;
use crate::utils::AppState;
use base64::{engine::general_purpose, Engine as _};
use rgb_lib::wallet::Metadata as RgbLibMetadata;
use rgb_lib::{
    ConsignmentExt, ContractId, Error as RgbLibError, FileContent, RgbContract, RgbTransfer,
    RgbTxid,
};
use std::str::FromStr;
use std::sync::Arc;

pub(crate) const MAX_RGB_IMPORT_BASE64_CHARACTERS: usize = 16 * 1024 * 1024;
pub(crate) const MAX_RGB_IMPORT_BODY_BYTES: usize = MAX_RGB_IMPORT_BASE64_CHARACTERS + 4 * 1024;

pub(crate) struct ImportRgbTransferConsignmentRequestData {
    pub(crate) consignment_base64: String,
    pub(crate) offchain_txid: String,
    pub(crate) expected_asset_id: Option<String>,
}

pub(crate) struct ImportRgbContractRequestData {
    pub(crate) contract_base64: String,
    pub(crate) expected_asset_id: String,
}

pub(crate) struct ImportRgbData {
    pub(crate) asset_id: String,
    pub(crate) already_imported: bool,
    pub(crate) metadata: RgbLibMetadata,
}

#[derive(Clone, Copy)]
enum RgbPayloadKind {
    Contract,
    TransferConsignment,
}

impl RgbPayloadKind {
    fn invalid(self, details: String) -> APIError {
        match self {
            Self::Contract => APIError::InvalidRgbContract(details),
            Self::TransferConsignment => APIError::InvalidRgbConsignment(details),
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Contract => "contract",
            Self::TransferConsignment => "transfer consignment",
        }
    }
}

fn decode_rgb_base64(value: String, kind: RgbPayloadKind) -> Result<Vec<u8>, APIError> {
    if value.is_empty() || value.len() > MAX_RGB_IMPORT_BASE64_CHARACTERS {
        return Err(kind.invalid(format!("{} payload size is invalid", kind.label())));
    }
    general_purpose::STANDARD
        .decode(value)
        .map_err(|error| kind.invalid(format!("invalid base64: {error}")))
}

fn validate_expected_asset_id(
    expected_asset_id: &str,
    actual_contract_id: &ContractId,
    kind: RgbPayloadKind,
) -> Result<(), APIError> {
    let expected_contract_id = ContractId::from_str(expected_asset_id)
        .map_err(|_| APIError::InvalidAssetID(expected_asset_id.to_string()))?;
    if &expected_contract_id != actual_contract_id {
        return Err(kind.invalid(format!(
            "expected asset id {expected_asset_id}, got {actual_contract_id}"
        )));
    }
    Ok(())
}

async fn parse_rgb_payload<T, F>(operation: &'static str, parse: F) -> Result<T, APIError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, APIError> + Send + 'static,
{
    tokio::task::spawn_blocking(parse)
        .await
        .map_err(|error| APIError::Unexpected(format!("{operation} parse task failed: {error}")))?
}

async fn run_rgb_import<F>(
    state: Arc<AppState>,
    operation: &'static str,
    import: F,
) -> Result<ImportRgbData, APIError>
where
    F: FnOnce(Arc<crate::utils::UnlockedAppState>) -> Result<ImportRgbData, APIError>
        + Send
        + 'static,
{
    // The outer task deliberately owns the lifecycle guard. If an HTTP or FFI caller stops
    // waiting, a mutation that has already started still runs to completion before shutdown can
    // replace the unlocked state.
    let task = tokio::spawn(async move {
        if *state.get_changing_state() {
            return Err(APIError::ChangingState);
        }
        let unlocked_state_guard = state.get_unlocked_app_state().await;
        let unlocked_state = unlocked_state_guard
            .as_ref()
            .cloned()
            .ok_or(APIError::LockedNode)?;
        let result = tokio::task::spawn_blocking(move || import(unlocked_state))
            .await
            .map_err(|error| {
                APIError::Unexpected(format!("{operation} import task failed: {error}"))
            })?;
        drop(unlocked_state_guard);
        result
    });

    task.await
        .map_err(|error| APIError::Unexpected(format!("{operation} task failed: {error}")))?
}

fn map_contract_import_error(error: RgbLibError) -> APIError {
    match error {
        RgbLibError::InvalidConsignment => {
            APIError::InvalidRgbContract("contract validation failed".to_string())
        }
        other => APIError::from(other),
    }
}

fn map_transfer_import_error(error: RgbLibError) -> APIError {
    match error {
        RgbLibError::InvalidConsignment => {
            APIError::InvalidRgbConsignment("transfer validation failed".to_string())
        }
        RgbLibError::InvalidTxid => {
            APIError::InvalidRgbConsignment("off-chain transaction ID is invalid".to_string())
        }
        other => APIError::from(other),
    }
}

/// Register metadata from a transfer the native RGB receive path has already accepted.
///
/// This does not accept asset allocations or replace the normal receive protocol. The transfer
/// payload and off-chain transaction ID are still validated on every call; duplicate metadata
/// registration is idempotent only when the RGB stock remains consistent with the database.
pub(crate) async fn import_rgb_transfer_consignment(
    state: Arc<AppState>,
    request: ImportRgbTransferConsignmentRequestData,
) -> Result<ImportRgbData, APIError> {
    RgbTxid::from_str(&request.offchain_txid).map_err(|_| {
        APIError::InvalidRgbConsignment("off-chain transaction ID is invalid".to_string())
    })?;
    let expected_asset_id = request.expected_asset_id;
    let (consignment, contract_id) = parse_rgb_payload("transfer consignment", move || {
        let bytes = decode_rgb_base64(
            request.consignment_base64,
            RgbPayloadKind::TransferConsignment,
        )?;
        let consignment = RgbTransfer::load(&bytes[..])
            .map_err(|error| APIError::InvalidRgbConsignment(error.to_string()))?;
        let contract_id = consignment.contract_id();
        if let Some(expected_asset_id) = expected_asset_id.as_deref() {
            validate_expected_asset_id(
                expected_asset_id,
                &contract_id,
                RgbPayloadKind::TransferConsignment,
            )?;
        }
        Ok((consignment, contract_id))
    })
    .await?;
    let offchain_txid = request.offchain_txid;
    run_rgb_import(state, "transfer consignment", move |unlocked_state| {
        let (metadata, already_imported) = unlocked_state
            .rgb_import_transfer_consignment(consignment, offchain_txid)
            .map_err(map_transfer_import_error)?;
        Ok(ImportRgbData {
            asset_id: contract_id.to_string(),
            already_imported,
            metadata,
        })
    })
    .await
}

/// Validate and register public RGB contract metadata without importing allocations.
pub(crate) async fn import_rgb_contract(
    state: Arc<AppState>,
    request: ImportRgbContractRequestData,
) -> Result<ImportRgbData, APIError> {
    let expected_asset_id = request.expected_asset_id;
    let (contract, contract_id) = parse_rgb_payload("contract", move || {
        let bytes = decode_rgb_base64(request.contract_base64, RgbPayloadKind::Contract)?;
        let contract = RgbContract::load(&bytes[..])
            .map_err(|error| APIError::InvalidRgbContract(error.to_string()))?;
        let contract_id = contract.contract_id();
        validate_expected_asset_id(&expected_asset_id, &contract_id, RgbPayloadKind::Contract)?;
        Ok((contract, contract_id))
    })
    .await?;

    run_rgb_import(state, "contract", move |unlocked_state| {
        let (metadata, already_imported) = unlocked_state
            .rgb_import_asset_contract(contract)
            .map_err(map_contract_import_error)?;
        Ok(ImportRgbData {
            asset_id: contract_id.to_string(),
            already_imported,
            metadata,
        })
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_validation_is_bounded_and_payload_specific() {
        assert!(matches!(
            decode_rgb_base64(String::new(), RgbPayloadKind::Contract),
            Err(APIError::InvalidRgbContract(_))
        ));
        assert!(matches!(
            decode_rgb_base64(
                "not base64".to_string(),
                RgbPayloadKind::TransferConsignment
            ),
            Err(APIError::InvalidRgbConsignment(_))
        ));
        assert!(matches!(
            decode_rgb_base64(
                "A".repeat(MAX_RGB_IMPORT_BASE64_CHARACTERS + 1),
                RgbPayloadKind::Contract,
            ),
            Err(APIError::InvalidRgbContract(_))
        ));
        assert_eq!(
            decode_rgb_base64("YWJj".to_string(), RgbPayloadKind::Contract).unwrap(),
            b"abc"
        );
    }
}
