use std::sync::Arc;

use crate::error::AppError;
use crate::error::APIError;
use crate::utils::AppState;
use crate::NodeHandle;

use super::types::{uniffi_state_slot, RlnError};

pub(crate) fn set_uniffi_app_state(state: Arc<AppState>) {
    set_uniffi_node_handle(NodeHandle::from_app_state(state));
}

pub(crate) fn set_uniffi_node_handle(handle: NodeHandle) {
    // UniFFI currently uses a single global node handle per process.
    let mut slot = uniffi_state_slot().lock().unwrap();
    *slot = Some(handle);
}

pub(crate) fn clear_uniffi_app_state() {
    clear_uniffi_node_handle();
}

pub(crate) fn clear_uniffi_node_handle() {
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
        .map(|h| h.app_state())
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

pub(super) fn block_on_app<F, T>(fut: F) -> Result<T, RlnError>
where
    F: std::future::Future<Output = Result<T, AppError>>,
{
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        tokio::task::block_in_place(|| handle.block_on(fut)).map_err(map_app_error)
    } else {
        static RT: std::sync::OnceLock<tokio::runtime::Runtime> = std::sync::OnceLock::new();
        let rt = RT.get_or_init(|| {
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .expect("failed to build uniffi runtime")
        });
        rt.block_on(fut).map_err(map_app_error)
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

pub(super) fn map_app_error(err: AppError) -> RlnError {
    match err {
        AppError::UnavailablePort(_) | AppError::InvalidAuthenticationArgs => RlnError::InvalidRequest,
        _ => RlnError::Internal,
    }
}
