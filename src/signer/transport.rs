use super::types::RlnSignerError;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Health of a remote signer link, shared between the transport (which reports transitions) and the
/// `signer_unblocked` driver loop in `start_ldk` (which watches them). State-based rather than a bare
/// notification so the watcher can always re-check `is_connected` after a wake-up — `Notify` permits
/// cap at one, so a bare notification cannot by itself distinguish "went down" from "went down and
/// came back" while the watcher slept.
pub(crate) struct SignerLinkWatch {
    connected: AtomicBool,
    changed: tokio::sync::Notify,
}

// The producer side (`new_connected`/`mark_*`) is only exercised by the remote-signer transport;
// the consumer side (`is_connected`/`changed`) is used unconditionally by `start_ldk`.
#[cfg_attr(not(feature = "remote-signer"), allow(dead_code))]
impl SignerLinkWatch {
    /// A link that starts out connected (transports establish their connection eagerly).
    pub(crate) fn new_connected() -> Self {
        Self {
            connected: AtomicBool::new(true),
            changed: tokio::sync::Notify::new(),
        }
    }

    pub(crate) fn is_connected(&self) -> bool {
        self.connected.load(Ordering::Acquire)
    }

    /// Mark the link up. Always signals, even without a recorded down transition (the link can drop
    /// silently and be re-established on the next call) — a reconnect is exactly the moment parked
    /// signer operations must be re-driven.
    pub(crate) fn mark_reconnected(&self) {
        self.connected.store(true, Ordering::Release);
        self.changed.notify_one();
    }

    /// Mark the link down. Signals only the down *transition*, not every subsequent failed attempt,
    /// so a sustained outage doesn't spin the watcher.
    pub(crate) fn mark_disconnected(&self) {
        if self.connected.swap(false, Ordering::AcqRel) {
            self.changed.notify_one();
        }
    }

    /// Wait for the next link-state signal. May wake spuriously relative to the current state (a
    /// buffered permit from a transition that already resolved) — callers must re-check
    /// [`Self::is_connected`] and act on the state, not on the wake-up itself.
    pub(crate) async fn changed(&self) {
        self.changed.notified().await
    }
}

/// Synchronous in-process transport for external signer requests.
pub(crate) trait ExternalSignerTransport: Send + Sync {
    fn call(&self, request: &[u8]) -> Result<Vec<u8>, RlnSignerError>;

    /// The link-health watch for transports that can genuinely become unreachable and recover (e.g. a
    /// daemon process restart or a network blip clearing). `None` (the default) for transports that
    /// can never actually become unreachable, such as an in-process signer — callers use this to
    /// distinguish "there are real outages to react to" from "this transport is never briefly
    /// unreachable, so driving `signer_unblocked` for it would be pure overhead."
    fn link_watch(&self) -> Option<Arc<SignerLinkWatch>> {
        None
    }
}
