use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use bitcoin::io;
use lightning::util::async_poll::AsyncResult;
use lightning::util::persist::{KVStore, KVStoreSync};

use crate::kv_store::SeaOrmKvStore;
use crate::vss_kv_store::{vss_key, VssKvStore};

/// Remote-first async `KVStore`: a write resolves only after VSS durably stores
/// it, then mirrors to the local store best-effort; a VSS failure fails the write
/// (no silent local-only `Ok`). The local mirror goes through the synchronous
/// `KVStoreSync` path (the node's dedicated DB runtime) so it never re-enters the
/// calling runtime. Per-key writes/removes are serialized to preserve ordering.
/// `remote == None` degrades to local-only (VSS not configured).
pub struct RemoteFirstKvStore {
    local: Arc<SeaOrmKvStore>,
    remote: Option<Arc<VssKvStore>>,
    key_locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
}

impl RemoteFirstKvStore {
    pub fn new(local: Arc<SeaOrmKvStore>, remote: Option<Arc<VssKvStore>>) -> Self {
        Self {
            local,
            remote,
            key_locks: Mutex::new(HashMap::new()),
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
        let lock = self.key_lock(&vss_key(primary_namespace, secondary_namespace, key));
        let (primary, secondary, key) = (
            primary_namespace.to_string(),
            secondary_namespace.to_string(),
            key.to_string(),
        );
        Box::pin(async move {
            let _guard = lock.lock().await;
            if let Some(remote) = &remote {
                // VSS first (durable before ack), then mirror locally best-effort.
                remote
                    .write_async(&primary, &secondary, &key, buf.clone())
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
        let lock = self.key_lock(&vss_key(primary_namespace, secondary_namespace, key));
        let (primary, secondary, key) = (
            primary_namespace.to_string(),
            secondary_namespace.to_string(),
            key.to_string(),
        );
        Box::pin(async move {
            let _guard = lock.lock().await;
            if let Some(remote) = &remote {
                remote.remove_async(&primary, &secondary, &key).await?;
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
