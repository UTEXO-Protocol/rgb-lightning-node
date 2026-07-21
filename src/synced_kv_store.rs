use std::sync::Arc;

use bitcoin::io;
use lightning::util::persist::KVStoreSync;

use crate::kv_store::SeaOrmKvStore;

/// Maximum number of writes the pending-retry queue is allowed to hold.
///
/// When this cap is reached we drop the oldest entry per key (newest write
/// wins for any given key — channel-monitor persistence is last-write-wins
/// anyway). Bounds memory under prolonged VSS outage while still preserving
/// the latest state for each touched key.
#[cfg(feature = "vss")]
const PENDING_QUEUE_CAP: usize = 1000;

/// How many pending entries to attempt to drain on each successful VSS write.
#[cfg(feature = "vss")]
const PENDING_DRAIN_BATCH: usize = 16;

/// Local-only namespace persisting the pending queue across restarts. Rows are
/// written straight to the local store, so they never replicate to VSS.
#[cfg(feature = "vss")]
const PENDING_NS: &str = "vss_pending";

/// KVStore wrapper that writes to the local SeaORM store and (optionally)
/// replicates to a remote VSS server. Reads always go to the local store for
/// latency. When `remote` is `None`, this behaves identically to a plain
/// `SeaOrmKvStore`.
pub struct SyncedKvStore {
    local: Arc<SeaOrmKvStore>,
    #[cfg(feature = "vss")]
    remote: Option<Arc<crate::vss_kv_store::VssKvStore>>,
    /// VSS-replication failures park here so a transient outage doesn't lose
    /// state from the remote backup. Each successful VSS write drains up to
    /// [`PENDING_DRAIN_BATCH`] entries; the map is capped at
    /// [`PENDING_QUEUE_CAP`].
    ///
    /// Key: encoded VSS key string (same format as `vss_key()`).
    /// Value: bytes that should be re-written to VSS. `None` represents a
    /// pending removal (so a later write to the same key supersedes the
    /// removal correctly).
    #[cfg(feature = "vss")]
    pending: Arc<std::sync::Mutex<std::collections::HashMap<String, Option<Vec<u8>>>>>,
    /// Serializes VSS puts per key across writes and drains so a drained stale
    /// value can never land after a newer one.
    #[cfg(feature = "vss")]
    key_locks: std::sync::Mutex<std::collections::HashMap<String, Arc<std::sync::Mutex<()>>>>,
    /// Set at teardown; [`Self::stop`] then waits out an in-flight drain via
    /// `drain_gate` so no queued put can land after the fence is released.
    #[cfg(feature = "vss")]
    stopped: std::sync::atomic::AtomicBool,
    #[cfg(feature = "vss")]
    drain_gate: std::sync::Mutex<()>,
}

impl SyncedKvStore {
    /// Creates a SyncedKvStore with local-only storage (no VSS replication).
    pub fn local_only(local: Arc<SeaOrmKvStore>) -> Self {
        Self {
            local,
            #[cfg(feature = "vss")]
            remote: None,
            #[cfg(feature = "vss")]
            pending: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            #[cfg(feature = "vss")]
            key_locks: std::sync::Mutex::new(std::collections::HashMap::new()),
            #[cfg(feature = "vss")]
            stopped: std::sync::atomic::AtomicBool::new(false),
            #[cfg(feature = "vss")]
            drain_gate: std::sync::Mutex::new(()),
        }
    }

    /// Creates a SyncedKvStore with local storage and VSS replication,
    /// reloading queued replications persisted by a previous run.
    #[cfg(feature = "vss")]
    pub fn with_vss(
        local: Arc<SeaOrmKvStore>,
        remote: Arc<crate::vss_kv_store::VssKvStore>,
    ) -> Self {
        let mut pending = std::collections::HashMap::new();
        if let Ok(keys) = local.list(PENDING_NS, "") {
            for key in keys {
                match local.read(PENDING_NS, "", &key) {
                    Ok(row) => match row.split_first() {
                        Some((1, buf)) => {
                            pending.insert(key, Some(buf.to_vec()));
                        }
                        Some((0, _)) => {
                            pending.insert(key, None);
                        }
                        _ => tracing::warn!(key, "dropping malformed pending VSS row"),
                    },
                    Err(e) => tracing::warn!(key, error = %e, "failed to load pending VSS row"),
                }
            }
        }
        if !pending.is_empty() {
            tracing::info!(
                count = pending.len(),
                "reloaded pending VSS replications from previous run"
            );
        }
        Self {
            local,
            remote: Some(remote),
            pending: Arc::new(std::sync::Mutex::new(pending)),
            key_locks: std::sync::Mutex::new(std::collections::HashMap::new()),
            stopped: std::sync::atomic::AtomicBool::new(false),
            drain_gate: std::sync::Mutex::new(()),
        }
    }

    #[cfg(feature = "vss")]
    fn key_lock(&self, vss_key: &str) -> Arc<std::sync::Mutex<()>> {
        self.key_locks
            .lock()
            .unwrap()
            .entry(vss_key.to_string())
            .or_default()
            .clone()
    }

    /// Stops future drains and waits for an in-flight one to finish. Call at
    /// teardown before releasing the VSS fence.
    #[cfg(feature = "vss")]
    pub(crate) fn stop(&self) {
        self.stopped
            .store(true, std::sync::atomic::Ordering::Release);
        drop(self.drain_gate.lock().unwrap());
    }

    /// Persist a queue entry so it survives restarts.
    #[cfg(feature = "vss")]
    fn persist_pending_row(&self, vss_key: &str, value: &Option<Vec<u8>>) {
        let mut row = Vec::with_capacity(1 + value.as_ref().map_or(0, |v| v.len()));
        match value {
            Some(buf) => {
                row.push(1);
                row.extend_from_slice(buf);
            }
            None => row.push(0),
        }
        if let Err(e) = self.local.write(PENDING_NS, "", vss_key, row) {
            tracing::warn!(vss_key, error = %e, "failed to persist pending VSS row");
        }
    }

    #[cfg(feature = "vss")]
    fn clear_pending_row(&self, vss_key: &str) {
        if let Err(e) = self.local.remove(PENDING_NS, "", vss_key, false) {
            if e.kind() != io::ErrorKind::NotFound {
                tracing::warn!(vss_key, error = %e, "failed to clear pending VSS row");
            }
        }
    }

    /// Releases the VSS single-writer fence if this instance owns it. No-op
    /// without a remote store.
    #[cfg(feature = "vss")]
    pub(crate) fn release_vss_fence_if_owned(&self) -> Result<(), io::Error> {
        match &self.remote {
            Some(remote) => remote.release_fence_if_owned(),
            None => Ok(()),
        }
    }

    /// Restores all key-value pairs from VSS into the local store.
    ///
    /// `force = false` is the safe default and returns `Ok(0)` if the local
    /// store already has a channel-manager key (i.e. it's not a fresh
    /// device). `force = true` overwrites local state with whatever VSS holds
    /// and is intended only for explicit operator use.
    ///
    /// Returns the number of keys restored, or 0 if VSS is not configured or
    /// the local store is already populated.
    #[cfg(feature = "vss")]
    pub(crate) fn restore_from_vss(&self, force: bool) -> Result<usize, io::Error> {
        let Some(ref remote) = self.remote else {
            return Ok(0);
        };

        if !force {
            // Guard: refuse to clobber an already-populated local store.
            // The caller in `start_ldk` also performs this check, but a
            // belt-and-suspenders guard makes this API hard to misuse.
            use lightning::util::persist::{
                CHANNEL_MANAGER_PERSISTENCE_KEY, CHANNEL_MANAGER_PERSISTENCE_PRIMARY_NAMESPACE,
                CHANNEL_MANAGER_PERSISTENCE_SECONDARY_NAMESPACE,
            };
            if self
                .local
                .read(
                    CHANNEL_MANAGER_PERSISTENCE_PRIMARY_NAMESPACE,
                    CHANNEL_MANAGER_PERSISTENCE_SECONDARY_NAMESPACE,
                    CHANNEL_MANAGER_PERSISTENCE_KEY,
                )
                .is_ok()
            {
                tracing::info!(
                    "VSS restore skipped: local store already has channel manager data \
                     (pass force=true to override)"
                );
                return Ok(0);
            }
        }

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

    /// Local-only removal; lets the restore guard discard a restored key.
    #[cfg(feature = "vss")]
    pub(crate) fn remove_local_only(
        &self,
        primary_namespace: &str,
        secondary_namespace: &str,
        key: &str,
    ) -> Result<(), io::Error> {
        self.local
            .remove(primary_namespace, secondary_namespace, key, false)
    }

    /// Returns the number of pending VSS-replication entries that failed and
    /// are awaiting retry. Surfaced via `/vssbackupinfo` so operators can
    /// alert on persistent backup-staleness.
    #[cfg(feature = "vss")]
    pub fn pending_remote_writes(&self) -> usize {
        self.pending.lock().unwrap().len()
    }

    /// Enqueue a failed VSS replication for later retry, persisted so it
    /// survives restarts.
    #[cfg(feature = "vss")]
    fn enqueue_pending(&self, vss_key: String, value: Option<Vec<u8>>) {
        self.persist_pending_row(&vss_key, &value);
        let mut pending = self.pending.lock().unwrap();
        if pending.len() >= PENDING_QUEUE_CAP && !pending.contains_key(&vss_key) {
            // Evict an arbitrary other key to bound the queue; logged so the
            // operator can alert on it.
            if let Some(evict) = pending.keys().next().cloned() {
                pending.remove(&evict);
                drop(pending);
                self.clear_pending_row(&evict);
                pending = self.pending.lock().unwrap();
                tracing::warn!(
                    evicted_key = evict,
                    cap = PENDING_QUEUE_CAP,
                    "VSS pending-writes queue at cap; evicted an entry to make room"
                );
            }
        }
        pending.insert(vss_key, value);
    }

    /// Attempt to drain up to [`PENDING_DRAIN_BATCH`] entries from the
    /// pending queue. Called after each successful VSS write and periodically
    /// from a background task. Entries that re-fail are re-queued.
    #[cfg(feature = "vss")]
    pub(crate) fn drain_pending(&self) {
        let Some(ref remote) = self.remote else {
            return;
        };
        if self.stopped.load(std::sync::atomic::Ordering::Acquire) {
            return;
        }
        let _gate = self.drain_gate.lock().unwrap();
        // Snapshot without removing: entries leave the queue only once VSS
        // confirms them, so nothing is ever in flight outside the map.
        let snapshot: Vec<(String, Option<Vec<u8>>)> = {
            let pending = self.pending.lock().unwrap();
            pending
                .iter()
                .take(PENDING_DRAIN_BATCH)
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect()
        };
        if snapshot.is_empty() {
            return;
        }
        let mut drained = 0usize;
        for (vss_key, value) in snapshot {
            let lock = self.key_lock(&vss_key);
            let _guard = lock.lock().unwrap();
            // Re-check under the key lock: a concurrent write may have
            // superseded or delivered this entry.
            if self.pending.lock().unwrap().get(&vss_key) != Some(&value) {
                continue;
            }
            let parsed = crate::vss_kv_store::parse_vss_key(&vss_key);
            let Some((primary, secondary, key)) = parsed else {
                tracing::warn!(vss_key, "Dropping unparseable key from pending queue");
                self.pending.lock().unwrap().remove(&vss_key);
                self.clear_pending_row(&vss_key);
                continue;
            };
            let result = match &value {
                Some(buf) => remote.write(&primary, &secondary, &key, buf.clone()),
                None => remote.remove(&primary, &secondary, &key, false),
            };
            match result {
                Ok(()) => {
                    self.pending.lock().unwrap().remove(&vss_key);
                    self.clear_pending_row(&vss_key);
                    drained += 1;
                }
                Err(e) => {
                    // VSS is likely still down; the entry stays queued.
                    tracing::debug!(
                        vss_key,
                        error = %e,
                        "Pending VSS replication still failing"
                    );
                    break;
                }
            }
        }
        tracing::debug!(drained, "Drained pending VSS writes");
    }

    /// Drain until the queue is empty, the deadline passes, or no progress is
    /// made (VSS unreachable). Returns the number of entries still pending.
    #[cfg(feature = "vss")]
    pub(crate) fn flush_pending_until(&self, deadline: std::time::Instant) -> usize {
        loop {
            let before = self.pending_remote_writes();
            if before == 0 || std::time::Instant::now() >= deadline {
                return before;
            }
            self.drain_pending();
            if self.pending_remote_writes() >= before {
                return self.pending_remote_writes();
            }
        }
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

        // Replicate to VSS (best-effort, queue for retry on failure).
        #[cfg(feature = "vss")]
        if let Some(ref remote) = self.remote {
            let vss_key = crate::vss_kv_store::vss_key(primary_namespace, secondary_namespace, key);
            let replicated = {
                let lock = self.key_lock(&vss_key);
                let _guard = lock.lock().unwrap();
                match remote.write(primary_namespace, secondary_namespace, key, buf.clone()) {
                    Ok(()) => {
                        // Drop any stale queued value so a drain can't regress the remote.
                        if self.pending.lock().unwrap().remove(&vss_key).is_some() {
                            self.clear_pending_row(&vss_key);
                        }
                        true
                    }
                    Err(e) => {
                        tracing::warn!(
                            primary_namespace,
                            secondary_namespace,
                            key,
                            error = %e,
                            "VSS replication write failed; queued for retry"
                        );
                        self.enqueue_pending(vss_key, Some(buf));
                        false
                    }
                }
            };
            if replicated {
                self.drain_pending();
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

        // Replicate removal to VSS (best-effort, queue for retry on failure).
        #[cfg(feature = "vss")]
        if let Some(ref remote) = self.remote {
            let vss_key = crate::vss_kv_store::vss_key(primary_namespace, secondary_namespace, key);
            let replicated = {
                let lock = self.key_lock(&vss_key);
                let _guard = lock.lock().unwrap();
                match remote.remove(primary_namespace, secondary_namespace, key, lazy) {
                    Ok(()) => {
                        // Same as in `write`.
                        if self.pending.lock().unwrap().remove(&vss_key).is_some() {
                            self.clear_pending_row(&vss_key);
                        }
                        true
                    }
                    Err(e) => {
                        tracing::warn!(
                            primary_namespace,
                            secondary_namespace,
                            key,
                            error = %e,
                            "VSS replication remove failed; queued for retry"
                        );
                        self.enqueue_pending(vss_key, None);
                        false
                    }
                }
            };
            if replicated {
                self.drain_pending();
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
