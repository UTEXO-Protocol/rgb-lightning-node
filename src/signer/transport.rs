use super::types::RlnSignerError;
use std::sync::Arc;

/// Synchronous in-process transport for external signer requests.
pub(crate) trait ExternalSignerTransport: Send + Sync {
    fn call(&self, request: &[u8]) -> Result<Vec<u8>, RlnSignerError>;

    /// Fired whenever this transport transitions from disconnected back to connected (e.g. a daemon
    /// process restart or a network blip clearing). `None` (the default) for transports that can never
    /// actually become unreachable, such as an in-process signer — callers use this to distinguish
    /// "there is a real reconnect event to react to" from "this transport is never briefly
    /// unreachable, so periodically re-driving `signer_unblocked` would be pure overhead."
    fn reconnect_notify(&self) -> Option<Arc<tokio::sync::Notify>> {
        None
    }
}
