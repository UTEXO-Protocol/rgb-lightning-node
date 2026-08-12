use std::sync::Arc;

use bitcoin::io;
use lightning::util::persist::KVStoreSync;

use crate::kv_store::SeaOrmKvStore;
#[cfg(feature = "vss")]
use crate::kv_store::{KvStoreEntry, KvStoreKey};

#[cfg(all(test, feature = "vss"))]
fn synced_persistence_checkpoint(name: &str) {
    use std::fs::OpenOptions;
    use std::io::Write;

    if let Ok(path) = std::env::var("RLN_SYNCED_KV_PERSISTENCE_TRACE_PATH") {
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .expect("open synchronized KV persistence trace");
        writeln!(file, "{name}").expect("write synchronized KV persistence trace");
        file.sync_all()
            .expect("sync synchronized KV persistence trace");
    }

    if std::env::var("RLN_SYNCED_KV_KILL_AT").as_deref() == Ok(name) {
        let path = std::env::var("RLN_SYNCED_KV_KILL_READY_PATH")
            .expect("synchronized KV persistence kill-ready path");
        let mut file = std::fs::File::create(path).expect("create synchronized KV kill-ready file");
        writeln!(file, "{name}").expect("write synchronized KV kill-ready file");
        file.sync_all()
            .expect("sync synchronized KV kill-ready file");
        loop {
            std::thread::park();
        }
    }
}

#[cfg(all(not(test), feature = "vss"))]
#[inline]
fn synced_persistence_checkpoint(_name: &str) {}

/// Maximum number of distinct keys the pending-retry queue is allowed to hold.
///
/// Capacity is enforced before the local mutation. Entries are never evicted: losing a durable
/// replication intent would make a later VSS restore silently stale.
#[cfg(feature = "vss")]
const PENDING_QUEUE_CAP: usize = 1000;

/// How many pending entries to attempt to drain on each successful VSS write.
#[cfg(feature = "vss")]
const PENDING_DRAIN_BATCH: usize = 16;

// Protocol records that must be remotely acknowledged before channel funding may advance.
// These names are persisted storage contracts and intentionally live with the durability policy.
#[cfg(feature = "vss")]
pub(crate) const RGB_SENDER_FUNDING_NAMESPACE: &str = "rgb_sender_funding";
#[cfg(feature = "vss")]
pub(crate) const PSBT_NAMESPACE: &str = "psbt";
#[cfg(feature = "vss")]
pub(crate) const PENDING_FUNDING_NAMESPACE: &str = "pending_funding";
#[cfg(feature = "vss")]
pub(crate) const RGB_PRIMARY_NAMESPACE: &str = "rgb";
#[cfg(feature = "vss")]
pub(crate) const RGB_FUNDING_ACCEPTANCE_NAMESPACE: &str = "funding_acceptance";

/// Local-only namespace persisting the pending queue across restarts. Target mutations and rows in
/// this namespace are committed in one SQLite transaction.
#[cfg(feature = "vss")]
pub(crate) const PENDING_NS: &str = "vss_pending";

/// KVStore wrapper that writes to the local SeaORM store and (optionally)
/// replicates to a remote VSS server. Reads always go to the local store for
/// latency. When `remote` is `None`, this behaves identically to a plain
/// `SeaOrmKvStore`.
pub struct SyncedKvStore {
    local: Arc<SeaOrmKvStore>,
    #[cfg(feature = "vss")]
    remote: Option<Arc<crate::vss_kv_store::VssKvStore>>,
    /// Every local mutation is represented here before VSS is contacted. Each successful VSS
    /// write drains up to [`PENDING_DRAIN_BATCH`] entries.
    ///
    /// Key: encoded VSS key string (same format as `vss_key()`).
    /// Value: bytes that should be re-written to VSS. `None` represents a
    /// pending removal (so a later write to the same key supersedes the
    /// removal correctly).
    #[cfg(feature = "vss")]
    pending: Arc<std::sync::Mutex<std::collections::HashMap<String, Option<Vec<u8>>>>>,
    #[cfg(feature = "vss")]
    pending_capacity: usize,
    /// Serializes VSS puts per key across writes and drains so a drained stale
    /// value can never land after a newer one.
    #[cfg(feature = "vss")]
    key_locks: std::sync::Mutex<std::collections::HashMap<String, Arc<std::sync::Mutex<()>>>>,
    /// Set at teardown; [`Self::stop`] then waits out every in-flight remote mutation via
    /// `drain_gate` so no put or removal can land after the fence is released.
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
            pending_capacity: PENDING_QUEUE_CAP,
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
        Self::with_vss_capacity_inner(local, remote, PENDING_QUEUE_CAP)
    }

    #[cfg(all(feature = "vss", test))]
    pub(crate) fn with_vss_capacity(
        local: Arc<SeaOrmKvStore>,
        remote: Arc<crate::vss_kv_store::VssKvStore>,
        pending_capacity: usize,
    ) -> Self {
        Self::with_vss_capacity_inner(local, remote, pending_capacity)
    }

    #[cfg(feature = "vss")]
    fn with_vss_capacity_inner(
        local: Arc<SeaOrmKvStore>,
        remote: Arc<crate::vss_kv_store::VssKvStore>,
        pending_capacity: usize,
    ) -> Self {
        assert!(
            pending_capacity > 0,
            "pending VSS capacity must be non-zero"
        );
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
                        _ => {
                            tracing::error!(key, "removing malformed pending VSS row");
                            if let Err(error) = local.remove(PENDING_NS, "", &key, false) {
                                tracing::error!(
                                    key,
                                    error = %error,
                                    "failed to remove malformed pending VSS row"
                                );
                            }
                        }
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
            pending_capacity,
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

    /// Stops future replication and waits for every in-flight remote mutation to finish. Call at
    /// teardown before releasing the VSS fence.
    #[cfg(feature = "vss")]
    pub(crate) fn stop(&self) {
        self.stopped
            .store(true, std::sync::atomic::Ordering::Release);
        drop(self.drain_gate.lock().unwrap());
    }

    #[cfg(feature = "vss")]
    fn encode_pending_row(value: &Option<Vec<u8>>) -> Vec<u8> {
        let mut row = Vec::with_capacity(1 + value.as_ref().map_or(0, |v| v.len()));
        match value {
            Some(buf) => {
                row.push(1);
                row.extend_from_slice(buf);
            }
            None => row.push(0),
        }
        row
    }

    #[cfg(feature = "vss")]
    fn reserve_pending(
        &self,
        vss_key: &str,
        value: Option<Vec<u8>>,
    ) -> Result<Option<Option<Vec<u8>>>, io::Error> {
        let mut pending = self.pending.lock().unwrap();
        if pending.len() >= self.pending_capacity && !pending.contains_key(vss_key) {
            tracing::error!(
                vss_key,
                pending = pending.len(),
                cap = self.pending_capacity,
                "VSS replication backlog is full; rejecting local mutation"
            );
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                format!(
                    "VSS replication backlog is full ({} distinct keys)",
                    self.pending_capacity
                ),
            ));
        }
        Ok(pending.insert(vss_key.to_owned(), value))
    }

    #[cfg(feature = "vss")]
    fn restore_pending_reservation(&self, vss_key: &str, previous: Option<Option<Vec<u8>>>) {
        let mut pending = self.pending.lock().unwrap();
        match previous {
            Some(value) => {
                pending.insert(vss_key.to_owned(), value);
            }
            None => {
                pending.remove(vss_key);
            }
        }
    }

    #[cfg(feature = "vss")]
    fn clear_pending_row(&self, vss_key: &str) -> Result<(), io::Error> {
        self.local.remove(PENDING_NS, "", vss_key, false)?;
        self.pending.lock().unwrap().remove(vss_key);
        Ok(())
    }

    /// These records define channel-funding recovery boundaries. With VSS configured, callers may
    /// not advance the protocol after only a local acknowledgement. Other keys remain locally
    /// authoritative and use the durable retry queue for eventual VSS convergence.
    #[cfg(feature = "vss")]
    fn requires_remote_durability(primary_namespace: &str, secondary_namespace: &str) -> bool {
        (primary_namespace == RGB_SENDER_FUNDING_NAMESPACE && secondary_namespace.is_empty())
            || (primary_namespace == PSBT_NAMESPACE && secondary_namespace.is_empty())
            || (primary_namespace == PENDING_FUNDING_NAMESPACE && secondary_namespace.is_empty())
            || (primary_namespace == RGB_PRIMARY_NAMESPACE
                && secondary_namespace == RGB_FUNDING_ACCEPTANCE_NAMESPACE)
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

    /// Local-only write; the row is never replicated, so a wipe-and-restore
    /// intentionally loses it.
    pub(crate) fn write_local_only(
        &self,
        primary_namespace: &str,
        secondary_namespace: &str,
        key: &str,
        buf: Vec<u8>,
    ) -> Result<(), io::Error> {
        self.local
            .write(primary_namespace, secondary_namespace, key, buf)
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
                if let Err(error) = self.clear_pending_row(&vss_key) {
                    tracing::error!(
                        vss_key,
                        error = %error,
                        "failed to clear unparseable pending VSS key"
                    );
                }
                continue;
            };
            let result = match &value {
                Some(buf) => remote.write(&primary, &secondary, &key, buf.clone()),
                None => remote.remove(&primary, &secondary, &key, false),
            };
            match result {
                Ok(()) => match self.clear_pending_row(&vss_key) {
                    Ok(()) => drained += 1,
                    Err(error) => {
                        tracing::error!(
                            vss_key,
                            error = %error,
                            "VSS write succeeded but its local retry intent could not be cleared"
                        );
                        break;
                    }
                },
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

    fn write_with_durability(
        &self,
        primary_namespace: &str,
        secondary_namespace: &str,
        key: &str,
        buf: Vec<u8>,
    ) -> Result<(), io::Error> {
        #[cfg(feature = "vss")]
        if let Some(ref remote) = self.remote {
            let drain_gate = self.drain_gate.lock().unwrap();
            if self.stopped.load(std::sync::atomic::Ordering::Acquire) {
                return Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "VSS-synchronized store is stopped",
                ));
            }
            let vss_key = crate::vss_kv_store::vss_key(primary_namespace, secondary_namespace, key);
            let remote_required =
                Self::requires_remote_durability(primary_namespace, secondary_namespace);
            let (replicated, remote_error) = {
                let lock = self.key_lock(&vss_key);
                let _guard = lock.lock().unwrap();

                let pending_value = Some(buf.clone());
                let previous = self.reserve_pending(&vss_key, pending_value.clone())?;
                let pending_row = Self::encode_pending_row(&pending_value);
                if let Err(error) = self.local.write_with_replication_intent(
                    KvStoreEntry::new(primary_namespace, secondary_namespace, key, buf.clone()),
                    KvStoreEntry::new(PENDING_NS, "", &vss_key, pending_row),
                ) {
                    self.restore_pending_reservation(&vss_key, previous);
                    return Err(error);
                }
                synced_persistence_checkpoint("synced-write-after-local-commit");

                synced_persistence_checkpoint("synced-write-before-remote");
                match remote.write(primary_namespace, secondary_namespace, key, buf.clone()) {
                    Ok(()) => {
                        synced_persistence_checkpoint("synced-write-after-remote");
                        synced_persistence_checkpoint("synced-write-before-pending-clear");
                        if let Err(error) = self.clear_pending_row(&vss_key) {
                            tracing::error!(
                                primary_namespace,
                                secondary_namespace,
                                key,
                                error = %error,
                                "VSS write succeeded but its local retry intent could not be cleared"
                            );
                            (false, None)
                        } else {
                            synced_persistence_checkpoint("synced-write-after-pending-clear");
                            (true, None)
                        }
                    }
                    Err(error) => {
                        tracing::warn!(
                            primary_namespace,
                            secondary_namespace,
                            key,
                            error = %error,
                            remote_required,
                            "VSS replication write failed; durable retry intent retained"
                        );
                        (false, Some(error))
                    }
                }
            };
            drop(drain_gate);
            if replicated {
                self.drain_pending();
            }
            if remote_required {
                if let Some(error) = remote_error {
                    return Err(error);
                }
            }
            return Ok(());
        }

        self.local
            .write(primary_namespace, secondary_namespace, key, buf)
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
        self.write_with_durability(primary_namespace, secondary_namespace, key, buf)
    }

    fn remove(
        &self,
        primary_namespace: &str,
        secondary_namespace: &str,
        key: &str,
        lazy: bool,
    ) -> Result<(), io::Error> {
        #[cfg(feature = "vss")]
        if let Some(ref remote) = self.remote {
            let drain_gate = self.drain_gate.lock().unwrap();
            if self.stopped.load(std::sync::atomic::Ordering::Acquire) {
                return Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "VSS-synchronized store is stopped",
                ));
            }
            let vss_key = crate::vss_kv_store::vss_key(primary_namespace, secondary_namespace, key);
            let remote_required =
                Self::requires_remote_durability(primary_namespace, secondary_namespace);
            let (replicated, remote_error) = {
                let lock = self.key_lock(&vss_key);
                let _guard = lock.lock().unwrap();

                let pending_value = None;
                let previous = self.reserve_pending(&vss_key, pending_value.clone())?;
                let pending_row = Self::encode_pending_row(&pending_value);
                if let Err(error) = self.local.remove_with_replication_intent(
                    KvStoreKey::new(primary_namespace, secondary_namespace, key),
                    KvStoreEntry::new(PENDING_NS, "", &vss_key, pending_row),
                ) {
                    self.restore_pending_reservation(&vss_key, previous);
                    return Err(error);
                }
                synced_persistence_checkpoint("synced-remove-after-local-commit");

                synced_persistence_checkpoint("synced-remove-before-remote");
                match remote.remove(primary_namespace, secondary_namespace, key, lazy) {
                    Ok(()) => {
                        synced_persistence_checkpoint("synced-remove-after-remote");
                        synced_persistence_checkpoint("synced-remove-before-pending-clear");
                        if let Err(error) = self.clear_pending_row(&vss_key) {
                            tracing::error!(
                                primary_namespace,
                                secondary_namespace,
                                key,
                                error = %error,
                                "VSS removal succeeded but its local retry intent could not be cleared"
                            );
                            (false, None)
                        } else {
                            synced_persistence_checkpoint("synced-remove-after-pending-clear");
                            (true, None)
                        }
                    }
                    Err(e) => {
                        tracing::warn!(
                            primary_namespace,
                            secondary_namespace,
                            key,
                            error = %e,
                            remote_required,
                            "VSS replication remove failed; durable retry intent retained"
                        );
                        (false, Some(e))
                    }
                }
            };
            drop(drain_gate);
            if replicated {
                self.drain_pending();
            }
            if remote_required {
                if let Some(error) = remote_error {
                    return Err(error);
                }
            }
            return Ok(());
        }

        self.local
            .remove(primary_namespace, secondary_namespace, key, lazy)
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
