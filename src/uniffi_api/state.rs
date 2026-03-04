use std::sync::Arc;

use crate::error::APIError;
use crate::utils::AppState;

use super::types::{uniffi_state_slot, RlnError};

pub(crate) fn set_uniffi_app_state(state: Arc<AppState>) {
    // UniFFI currently uses a single global node state per process.
    let mut slot = uniffi_state_slot().lock().unwrap();
    *slot = Some(state);
}

pub(crate) fn clear_uniffi_app_state() {
    // Clear global state on daemon shutdown to avoid stale handles.
    let mut slot = uniffi_state_slot().lock().unwrap();
    *slot = None;
}

pub(super) fn is_uniffi_app_state_initialized() -> bool {
    // Lightweight readiness probe used by SDK clients before making calls.
    uniffi_state_slot().lock().unwrap().is_some()
}

pub(super) fn get_uniffi_app_state() -> Result<Arc<AppState>, RlnError> {
    uniffi_state_slot()
        .lock()
        .unwrap()
        .clone()
        .ok_or(RlnError::NotInitialized)
}

pub(super) fn block_on_sdk<F, T>(fut: F) -> Result<T, RlnError>
where
    F: std::future::Future<Output = Result<T, APIError>>,
{
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        // If we're already inside a Tokio runtime, reuse it.
        tokio::task::block_in_place(|| handle.block_on(fut)).map_err(map_api_error)
    } else {
        // Otherwise use a shared runtime for UniFFI calls from non-async hosts.
        static RT: std::sync::OnceLock<tokio::runtime::Runtime> = std::sync::OnceLock::new();
        let rt = RT.get_or_init(|| {
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .expect("failed to build uniffi runtime")
        });
        rt.block_on(fut).map_err(map_api_error)
    }
}

pub(super) fn map_api_error(err: APIError) -> RlnError {
    match err {
        APIError::LockedNode | APIError::NotInitialized => RlnError::NotInitialized,
        APIError::InvalidAddress(_)
        | APIError::InvalidAmount(_)
        | APIError::InvalidAssetID(_)
        | APIError::InvalidChannelID
        | APIError::InvalidInvoice(_)
        | APIError::InvalidPaymentHash(_)
        | APIError::InvalidRecipientData(_)
        | APIError::InvalidRecipientID
        | APIError::InvalidRecipientNetwork
        | APIError::InvalidTransportEndpoint(_)
        | APIError::InvalidTransportEndpoints(_)
        | APIError::PaymentNotFound(_)
        | APIError::SwapNotFound(_)
        | APIError::UnknownChannelId
        | APIError::UnknownContractId
        | APIError::UnknownLNInvoice
        | APIError::UnknownTemporaryChannelId
        | APIError::IncompleteRGBInfo => RlnError::InvalidRequest,
        _ => RlnError::Internal,
    }
}
