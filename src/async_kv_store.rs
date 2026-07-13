use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bitcoin::io;
use lightning::util::async_poll::AsyncResult;
use lightning::util::persist::{KVStore, KVStoreSync};

use crate::kv_store::SeaOrmKvStore;
use crate::vss_kv_store::{is_transient_io_error, vss_key, VssKvStore};

/// Initial delay between VSS retry attempts during an outage.
const RETRY_INITIAL_DELAY: Duration = Duration::from_millis(500);

/// Cap for the exponential retry backoff.
const RETRY_MAX_DELAY: Duration = Duration::from_secs(10);

/// Remote-first async `KVStore`: a write resolves only after VSS durably stores
/// it, then mirrors to the local store best-effort; a VSS failure fails the write
/// (no silent local-only `Ok`). Transient VSS failures (outage) are retried with
/// backoff until the server recovers — the pending monitor update keeps the
/// channel paused meanwhile — so an outage never permanently wedges a channel.
/// Permanent errors (auth, conflict) still fail the write. The local mirror goes
/// through the synchronous `KVStoreSync` path (the node's dedicated DB runtime)
/// so it never re-enters the calling runtime. Per-key writes/removes are
/// serialized to preserve ordering. `remote == None` degrades to local-only
/// (VSS not configured).
pub struct RemoteFirstKvStore {
    local: Arc<SeaOrmKvStore>,
    remote: Option<Arc<VssKvStore>>,
    key_locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    /// Flipped by [`Self::stop`] at node teardown: pending retries must abort
    /// before the VSS fence is released, or a write delayed by an outage could
    /// land after another instance owns the store.
    shutdown: tokio::sync::watch::Sender<bool>,
}

impl RemoteFirstKvStore {
    pub fn new(local: Arc<SeaOrmKvStore>, remote: Option<Arc<VssKvStore>>) -> Self {
        Self {
            local,
            remote,
            key_locks: Mutex::new(HashMap::new()),
            shutdown: tokio::sync::watch::channel(false).0,
        }
    }

    fn key_lock(&self, vss_key: &str) -> Arc<tokio::sync::Mutex<()>> {
        self.key_locks
            .lock()
            .unwrap()
            .entry(vss_key.to_string())
            .or_default()
            .clone()
    }

    /// Aborts all pending VSS retry loops. Call at node teardown, before the
    /// VSS fence is released.
    pub fn stop(&self) {
        let _ = self.shutdown.send(true);
    }
}

/// Runs `op` until it succeeds, retrying transient VSS failures with capped
/// exponential backoff. Returns the last error on a permanent failure or once
/// `shutdown` flips to true.
async fn retry_transient<F, Fut>(
    vss_key: &str,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
    op: F,
) -> Result<(), io::Error>
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = Result<(), io::Error>>,
{
    let mut delay = RETRY_INITIAL_DELAY;
    let mut attempts = 0u64;
    loop {
        match op().await {
            Ok(()) => {
                if attempts > 0 {
                    tracing::info!(vss_key, attempts, "VSS write recovered after outage");
                }
                return Ok(());
            }
            Err(e) if is_transient_io_error(&e) => {
                attempts += 1;
                tracing::warn!(
                    vss_key,
                    attempts,
                    retry_in = ?delay,
                    error = %e,
                    "VSS unreachable; persistence pending, retrying"
                );
                tokio::select! {
                    _ = tokio::time::sleep(delay) => {}
                    _ = shutdown.wait_for(|stopped| *stopped) => {
                        tracing::warn!(vss_key, "node stopping; abandoning VSS retry");
                        return Err(e);
                    }
                }
                delay = (delay * 2).min(RETRY_MAX_DELAY);
            }
            Err(e) => return Err(e),
        }
    }
}

impl KVStore for RemoteFirstKvStore {
    fn read(
        &self,
        primary_namespace: &str,
        secondary_namespace: &str,
        key: &str,
    ) -> AsyncResult<'static, Vec<u8>, io::Error> {
        let local = Arc::clone(&self.local);
        let remote = self.remote.clone();
        let (primary, secondary, key) = (
            primary_namespace.to_string(),
            secondary_namespace.to_string(),
            key.to_string(),
        );
        Box::pin(async move {
            // Local fast path; fall back to VSS only when the local copy is missing.
            match KVStoreSync::read(&*local, &primary, &secondary, &key) {
                Ok(value) => Ok(value),
                Err(e) if e.kind() == io::ErrorKind::NotFound => match remote {
                    Some(remote) => remote.read_async(&primary, &secondary, &key).await,
                    None => Err(e),
                },
                Err(e) => Err(e),
            }
        })
    }

    fn write(
        &self,
        primary_namespace: &str,
        secondary_namespace: &str,
        key: &str,
        buf: Vec<u8>,
    ) -> AsyncResult<'static, (), io::Error> {
        let local = Arc::clone(&self.local);
        let remote = self.remote.clone();
        let vss_key_str = vss_key(primary_namespace, secondary_namespace, key);
        let lock = self.key_lock(&vss_key_str);
        let shutdown = self.shutdown.subscribe();
        let (primary, secondary, key) = (
            primary_namespace.to_string(),
            secondary_namespace.to_string(),
            key.to_string(),
        );
        Box::pin(async move {
            let _guard = lock.lock().await;
            if let Some(remote) = &remote {
                // VSS first (durable before ack), then mirror locally best-effort.
                retry_transient(&vss_key_str, shutdown, || {
                    remote.write_async(&primary, &secondary, &key, buf.clone())
                })
                .await?;
                if let Err(e) = KVStoreSync::write(&*local, &primary, &secondary, &key, buf) {
                    tracing::warn!(
                        primary = %primary,
                        secondary = %secondary,
                        key = %key,
                        error = %e,
                        "remote-first: local mirror write failed after VSS durably stored"
                    );
                }
            } else {
                KVStoreSync::write(&*local, &primary, &secondary, &key, buf)?;
            }
            Ok(())
        })
    }

    fn remove(
        &self,
        primary_namespace: &str,
        secondary_namespace: &str,
        key: &str,
        _lazy: bool,
    ) -> AsyncResult<'static, (), io::Error> {
        let local = Arc::clone(&self.local);
        let remote = self.remote.clone();
        let vss_key_str = vss_key(primary_namespace, secondary_namespace, key);
        let lock = self.key_lock(&vss_key_str);
        let shutdown = self.shutdown.subscribe();
        let (primary, secondary, key) = (
            primary_namespace.to_string(),
            secondary_namespace.to_string(),
            key.to_string(),
        );
        Box::pin(async move {
            let _guard = lock.lock().await;
            if let Some(remote) = &remote {
                retry_transient(&vss_key_str, shutdown, || {
                    remote.remove_async(&primary, &secondary, &key)
                })
                .await?;
                if let Err(e) = KVStoreSync::remove(&*local, &primary, &secondary, &key, false) {
                    tracing::warn!(
                        primary = %primary,
                        secondary = %secondary,
                        key = %key,
                        error = %e,
                        "remote-first: local mirror remove failed after VSS durably removed"
                    );
                }
            } else {
                KVStoreSync::remove(&*local, &primary, &secondary, &key, false)?;
            }
            Ok(())
        })
    }

    fn list(
        &self,
        primary_namespace: &str,
        secondary_namespace: &str,
    ) -> AsyncResult<'static, Vec<String>, io::Error> {
        let local = Arc::clone(&self.local);
        let (primary, secondary) = (
            primary_namespace.to_string(),
            secondary_namespace.to_string(),
        );
        Box::pin(async move { KVStoreSync::list(&*local, &primary, &secondary) })
    }
}
