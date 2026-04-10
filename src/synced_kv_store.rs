use std::sync::Arc;

use bitcoin::io;
use lightning::util::persist::KVStoreSync;

use crate::kv_store::SeaOrmKvStore;

/// KVStore implementation that writes to local SQLite and optionally replicates
/// to a remote VSS server. Reads always go to the local store for speed.
///
/// When `remote` is `None`, this behaves identically to a plain `SeaOrmKvStore`.
pub struct SyncedKvStore {
    local: Arc<SeaOrmKvStore>,
    #[cfg(feature = "vss")]
    remote: Option<Arc<crate::vss_kv_store::VssKvStore>>,
}

impl SyncedKvStore {
    /// Creates a SyncedKvStore with local-only storage (no VSS replication).
    pub fn local_only(local: Arc<SeaOrmKvStore>) -> Self {
        Self {
            local,
            #[cfg(feature = "vss")]
            remote: None,
        }
    }

    /// Creates a SyncedKvStore with local storage and VSS replication.
    #[cfg(feature = "vss")]
    pub fn with_vss(
        local: Arc<SeaOrmKvStore>,
        remote: Arc<crate::vss_kv_store::VssKvStore>,
    ) -> Self {
        Self {
            local,
            remote: Some(remote),
        }
    }

    /// Restores all key-value pairs from VSS into the local store.
    ///
    /// Downloads all data from the remote VSS server and writes each entry
    /// into the local SQLite database. Used for disaster recovery on a fresh device.
    ///
    /// Returns the number of keys restored, or 0 if VSS is not configured.
    #[cfg(feature = "vss")]
    pub fn restore_from_vss(&self) -> Result<usize, io::Error> {
        let Some(ref remote) = self.remote else {
            return Ok(0);
        };

        tracing::info!("Starting restore from VSS...");
        let items = remote.download_all()?;
        let total = items.len();
        let mut restored = 0usize;

        for (vss_key_str, value) in items {
            if let Some((primary_ns, secondary_ns, key)) =
                crate::vss_kv_store::parse_vss_key(&vss_key_str)
            {
                if let Err(e) = self.local.write(&primary_ns, &secondary_ns, &key, value) {
                    tracing::warn!(
                        vss_key = vss_key_str,
                        error = %e,
                        "Failed to restore key to local store"
                    );
                } else {
                    restored += 1;
                }
            } else {
                tracing::warn!(
                    vss_key = vss_key_str,
                    "Skipping unrecognized VSS key format"
                );
            }
        }

        tracing::info!(restored, total, "VSS restore complete");
        Ok(restored)
    }
}

impl KVStoreSync for SyncedKvStore {
    fn read(
        &self,
        primary_namespace: &str,
        secondary_namespace: &str,
        key: &str,
    ) -> Result<Vec<u8>, io::Error> {
        // Always read from local store
        self.local.read(primary_namespace, secondary_namespace, key)
    }

    fn write(
        &self,
        primary_namespace: &str,
        secondary_namespace: &str,
        key: &str,
        buf: Vec<u8>,
    ) -> Result<(), io::Error> {
        // Write to local first (must succeed)
        self.local
            .write(primary_namespace, secondary_namespace, key, buf.clone())?;

        // Replicate to VSS (best-effort, log errors but don't fail)
        #[cfg(feature = "vss")]
        if let Some(ref remote) = self.remote {
            if let Err(e) = remote.write(primary_namespace, secondary_namespace, key, buf) {
                tracing::warn!(
                    primary_namespace,
                    secondary_namespace,
                    key,
                    error = %e,
                    "VSS replication write failed"
                );
            }
        }

        Ok(())
    }

    fn remove(
        &self,
        primary_namespace: &str,
        secondary_namespace: &str,
        key: &str,
        lazy: bool,
    ) -> Result<(), io::Error> {
        // Remove from local first (must succeed)
        self.local
            .remove(primary_namespace, secondary_namespace, key, lazy)?;

        // Replicate removal to VSS (best-effort)
        #[cfg(feature = "vss")]
        if let Some(ref remote) = self.remote {
            if let Err(e) = remote.remove(primary_namespace, secondary_namespace, key, lazy) {
                tracing::warn!(
                    primary_namespace,
                    secondary_namespace,
                    key,
                    error = %e,
                    "VSS replication remove failed"
                );
            }
        }

        Ok(())
    }

    fn list(
        &self,
        primary_namespace: &str,
        secondary_namespace: &str,
    ) -> Result<Vec<String>, io::Error> {
        // Always list from local store
        self.local.list(primary_namespace, secondary_namespace)
    }
}
