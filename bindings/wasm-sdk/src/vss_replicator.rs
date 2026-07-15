//! Best-effort background VSS replication tier for LDK/RGB node state.
//!
//! WASM persists LDK state as snapshots to localStorage/IndexedDB (see
//! [`crate::ldk_live_backend`]); that local write stays the durability *ack gate*.
//! This tier mirrors those same bytes to a remote VSS server in the background so a
//! lost/cleared browser profile can be recovered via
//! [`crate::ldk_live_backend::restore_ldk_state_from_vss`]. It is **best-effort**: a
//! failed replication is queued and retried, but never blocks or fails the local
//! persist. Besides the LDK snapshots (monitors/manager/graph/scorer), the node's
//! RGB KV store is mirrored too, by wrapping it in [`VssMirroredKvStore`].
//!
//! Design (single-threaded wasm, so `RefCell`/`Cell`, no locks):
//!   * `replicate()` records the *latest desired state* per key into a pending map
//!     (last-write-wins) and kicks an async drain via `spawn_local`. Values
//!     byte-identical to what is already queued/uploaded for the key are dropped
//!     (content dirty check) — the snapshot persist path re-replicates every drive
//!     tick, and without this VSS would receive identical PUTs every second. The
//!     reconstructible network-graph/scorer singletons are additionally rate-limited
//!     to one upload per [`RECONSTRUCTIBLE_MIN_UPLOAD_INTERVAL_MS`], since live
//!     gossip changes their bytes nearly every tick.
//!   * The drain pops entries and `.await`s the VSS put/remove. A `draining` flag
//!     makes concurrent `replicate()` calls coalesce into the running drain instead
//!     of spawning a second one. No `RefCell` borrow is ever held across an `.await`.
//!   * On failure the entry is re-queued and the drain stops (no hot-loop under a VSS
//!     outage); the next `replicate()` re-kicks it. Mirrors the native
//!     `SyncedKvStore` opportunistic-drain model.
//!   * The [`crate::vss_kv_store::MANIFEST_KEY`] manifest is rewritten only when the
//!     key *set* changes (new monitor, archived channel), not on value updates, so
//!     steady-state monitor churn costs one round-trip per update, not two.
//!     Membership changes that could not be written yet (manifest unreadable or the
//!     write failed) are parked in `pending_manifest` and retried until they land,
//!     so a key put during a manifest outage is never silently dropped from restore.

// Several items are only reachable from wasm32-only paths (`spawn_local` closures);
// allow dead_code so the native (unit-test) compilation stays warning-free.
#![allow(dead_code)]

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;

use lightning::io;
use lightning::util::persist::KVStoreSync;

use crate::vss_kv_store::{parse_vss_key, vss_key, WasmVssKvStore};

/// Category (VSS secondary namespace) for a channel monitor snapshot.
pub(crate) const CAT_MONITOR: &str = "monitor";
/// Category for the channel-manager snapshot.
pub(crate) const CAT_MANAGER: &str = "manager";
/// Category for the network-graph snapshot.
pub(crate) const CAT_NETWORK_GRAPH: &str = "network-graph";
/// Category for the scorer snapshot.
pub(crate) const CAT_SCORER: &str = "scorer";

/// Stable name for the singleton manager/graph/scorer snapshots (there is exactly
/// one of each per node, unlike monitors which are keyed by `MonitorName`).
const SINGLETON_MANAGER: &str = "channel-manager";
const SINGLETON_NETWORK_GRAPH: &str = "network-graph";
const SINGLETON_SCORER: &str = "scorer";

/// Max entries the pending-retry map may hold before evicting to bound memory under
/// a prolonged VSS outage. Newest write for any given key is always kept (the map is
/// keyed by VSS key, last-write-wins). Matches the native `PENDING_QUEUE_CAP`.
const PENDING_QUEUE_CAP: usize = 1000;

/// Successful writes between mid-session fence re-checks (matches native).
const FENCE_CHECK_INTERVAL: u64 = 100;

/// Minimum spacing between uploads of a *reconstructible* singleton (network graph,
/// scorer). Under live gossip the graph's serialization changes almost every tick,
/// so the content dirty-check alone would still upload multi-MB payloads once a
/// second. Losing up to this much graph/scorer freshness on restore is harmless —
/// both rebuild from gossip / payment history — unlike monitors, the channel
/// manager, and RGB state, which replicate immediately.
const RECONSTRUCTIBLE_MIN_UPLOAD_INTERVAL_MS: f64 = 5.0 * 60.0 * 1000.0;

/// Best-effort replicator owning one [`WasmVssKvStore`]. Always held as `Rc` so it
/// can be shared into `spawn_local` drain tasks and the thread-local registry.
pub(crate) struct VssReplicator {
    store: WasmVssKvStore,
    /// Per-instance fencing id; must match the token this instance wrote to the
    /// store's [`crate::vss_kv_store::FENCE_KEY`].
    instance_id: String,
    /// VSS key -> latest desired state. `Some(bytes)` = put, `None` = remove.
    pending: RefCell<HashMap<String, Option<Vec<u8>>>>,
    /// VSS key -> hash of the most recent bytes accepted into the pipeline (queued
    /// or already uploaded). Dirty check: the snapshot persist path re-replicates
    /// the manager/graph/scorer on every drive tick whether or not they changed, and
    /// without this the replicator would PUT byte-identical values to VSS every
    /// second forever. A removal clears the entry so a later re-put always goes out.
    latest_hash: RefCell<HashMap<String, u64>>,
    /// VSS key -> timestamp (ms) the key's bytes were last accepted for upload.
    /// Backs the [`RECONSTRUCTIBLE_MIN_UPLOAD_INTERVAL_MS`] throttle for the
    /// network-graph/scorer singletons.
    last_accepted_ms: RefCell<HashMap<String, f64>>,
    /// Keys we believe are present in the server-side manifest.
    known: RefCell<HashSet<String>>,
    /// Membership changes (`vss_key -> should_be_present`) not yet reflected in the
    /// server manifest — because the manifest seed hadn't loaded when the value was
    /// put, or a manifest write failed. Retried on every drain until they land.
    pending_manifest: RefCell<HashMap<String, bool>>,
    /// Whether `known` has been seeded from the server manifest yet.
    manifest_loaded: Cell<bool>,
    /// True while a drain task is running, so kicks coalesce.
    draining: Cell<bool>,
    /// Successful writes since the last fence re-check.
    writes_since_fence_check: Cell<u64>,
    /// Set once the fence is lost to another instance; all further replication is a
    /// no-op (we must not keep writing over another owner's state).
    disabled: Cell<bool>,
    /// Last replication error, surfaced for a health/backup-info view.
    last_error: RefCell<Option<String>>,
}

impl VssReplicator {
    pub(crate) fn new(store: WasmVssKvStore, instance_id: String) -> Rc<Self> {
        Rc::new(Self {
            store,
            instance_id,
            pending: RefCell::new(HashMap::new()),
            latest_hash: RefCell::new(HashMap::new()),
            last_accepted_ms: RefCell::new(HashMap::new()),
            known: RefCell::new(HashSet::new()),
            pending_manifest: RefCell::new(HashMap::new()),
            manifest_loaded: Cell::new(false),
            draining: Cell::new(false),
            writes_since_fence_check: Cell::new(0),
            disabled: Cell::new(false),
            last_error: RefCell::new(None),
        })
    }

    /// True once the fence was lost to another instance and replication stopped.
    pub(crate) fn is_disabled(&self) -> bool {
        self.disabled.get()
    }

    /// Best-effort async release of this instance's fence (only if still owned).
    /// Used on teardown so the next instance isn't wedged.
    pub(crate) fn spawn_release_fence(self: &Rc<Self>) {
        let me = Rc::clone(self);
        #[cfg(target_arch = "wasm32")]
        wasm_bindgen_futures::spawn_local(async move {
            if let Err(e) = me.store.release_fence_if_owned(&me.instance_id).await {
                warn(&format!("VSS fence release failed: {e}"));
            }
        });
        #[cfg(not(target_arch = "wasm32"))]
        let _ = me;
    }

    /// Bump the write counter; true every [`FENCE_CHECK_INTERVAL`] successful writes.
    fn fence_check_due(&self) -> bool {
        let n = self.writes_since_fence_check.get() + 1;
        if n >= FENCE_CHECK_INTERVAL {
            self.writes_since_fence_check.set(0);
            true
        } else {
            self.writes_since_fence_check.set(n);
            false
        }
    }

    /// Number of writes queued awaiting (re)replication. Zero in steady state.
    pub(crate) fn pending_count(&self) -> usize {
        self.pending.borrow().len()
    }

    /// The most recent replication error, if any.
    pub(crate) fn last_error(&self) -> Option<String> {
        self.last_error.borrow().clone()
    }

    /// Borrow the underlying store, e.g. to drive a fresh-load restore.
    pub(crate) fn store(&self) -> &WasmVssKvStore {
        &self.store
    }

    /// Queue the latest desired state for `(category, name)` and kick a drain.
    /// `value = None` records a removal. Never blocks; safe to call from a sync
    /// persist path.
    pub(crate) fn replicate(self: &Rc<Self>, category: &str, name: &str, value: Option<Vec<u8>>) {
        self.replicate_triple("", category, name, value)
    }

    /// Queue the latest desired state for a full `(primary, secondary, key)` KV
    /// triple (as mirrored by [`VssMirroredKvStore`]) and kick a drain.
    pub(crate) fn replicate_triple(
        self: &Rc<Self>,
        primary_namespace: &str,
        secondary_namespace: &str,
        key: &str,
        value: Option<Vec<u8>>,
    ) {
        if self.disabled.get() {
            return;
        }
        let vkey = vss_key(primary_namespace, secondary_namespace, key);
        // Dirty check: skip values byte-identical to what is already queued or
        // uploaded for this key. Comparing against the latest *accepted* bytes (not
        // just the last upload) keeps this safe while a drain is in flight.
        if let Some(buf) = &value {
            let hash = hash_bytes(buf);
            if self.latest_hash.borrow().get(&vkey) == Some(&hash) {
                // Identical bytes are already queued or uploaded — but still re-kick
                // the drain if anything is parked: these periodic re-replicates are
                // the retry heartbeat that ends a VSS outage.
                if !self.pending.borrow().is_empty() {
                    self.kick_drain();
                }
                return;
            }
            // Reconstructible singletons (graph/scorer) are additionally
            // rate-limited: under live gossip their bytes change nearly every tick,
            // so the dirty check alone would still upload multi-MB payloads once a
            // second. Skipped values are NOT hashed/timestamped, so the next
            // replicate after the window carries the freshest bytes.
            if primary_namespace.is_empty()
                && (secondary_namespace == CAT_NETWORK_GRAPH || secondary_namespace == CAT_SCORER)
            {
                let now = now_ms();
                let too_soon = self
                    .last_accepted_ms
                    .borrow()
                    .get(&vkey)
                    .is_some_and(|last| now - last < RECONSTRUCTIBLE_MIN_UPLOAD_INTERVAL_MS);
                if too_soon {
                    if !self.pending.borrow().is_empty() {
                        self.kick_drain();
                    }
                    return;
                }
                self.last_accepted_ms.borrow_mut().insert(vkey.clone(), now);
            }
            self.latest_hash.borrow_mut().insert(vkey.clone(), hash);
        } else {
            self.latest_hash.borrow_mut().remove(&vkey);
        }
        {
            let mut pending = self.pending.borrow_mut();
            if pending.len() >= PENDING_QUEUE_CAP && !pending.contains_key(&vkey) {
                if let Some(evict) = pending.keys().next().cloned() {
                    pending.remove(&evict);
                    // The evicted bytes never reached VSS; forget their hash and
                    // throttle stamp so a later replicate isn't wrongly held back.
                    self.latest_hash.borrow_mut().remove(&evict);
                    self.last_accepted_ms.borrow_mut().remove(&evict);
                    warn(&format!(
                        "VSS pending-replication queue at cap ({PENDING_QUEUE_CAP}); evicted {evict}"
                    ));
                }
            }
            pending.insert(vkey, value);
        }
        self.kick_drain();
    }

    fn kick_drain(self: &Rc<Self>) {
        if self.draining.get() {
            return;
        }
        self.draining.set(true);
        let me = Rc::clone(self);
        #[cfg(target_arch = "wasm32")]
        wasm_bindgen_futures::spawn_local(async move {
            me.drain().await;
        });
        #[cfg(not(target_arch = "wasm32"))]
        {
            // No async executor in host unit tests; reset the flag so state stays
            // consistent. Drive `drain()` manually if a test needs it.
            me.draining.set(false);
        }
    }

    /// Drain the pending map to VSS. One entry at a time; stops on the first error
    /// (re-queuing it) to avoid a hot-loop under a VSS outage.
    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    async fn drain(self: Rc<Self>) {
        // Seed `known` from the server manifest once, so manifest maintenance below
        // doesn't clobber existing keys. If it fails, proceed writing values but skip
        // manifest maintenance this round (we'd rather under-write the manifest than
        // overwrite it with a partial set).
        if !self.manifest_loaded.get() {
            match self.store.read_manifest().await {
                Ok(keys) => {
                    *self.known.borrow_mut() = keys.into_iter().collect();
                    self.manifest_loaded.set(true);
                    // Flush membership changes parked while the manifest was
                    // unreadable, so those keys aren't lost from restore.
                    self.sync_manifest().await;
                }
                Err(e) => {
                    *self.last_error.borrow_mut() = Some(e);
                }
            }
        }

        loop {
            let next = {
                let mut pending = self.pending.borrow_mut();
                pending
                    .keys()
                    .next()
                    .cloned()
                    .and_then(|k| pending.remove_entry(&k))
            };
            let Some((vkey, value)) = next else { break };

            let Some((p, s, k)) = parse_vss_key(&vkey) else {
                warn(&format!("VSS replicator dropping unparseable key {vkey}"));
                continue;
            };

            let result = match &value {
                Some(buf) => self.store.put(&p, &s, &k, buf).await,
                None => self.store.remove(&p, &s, &k).await,
            };

            match result {
                Ok(()) => {
                    *self.last_error.borrow_mut() = None;
                    self.maintain_manifest(&vkey, value.is_some()).await;
                    // Periodically re-check the fence; if another instance has taken
                    // over, stop replicating so we don't corrupt their state.
                    if self.fence_check_due() {
                        if let Err(e) = self.store.check_fence(&self.instance_id).await {
                            warn(&format!(
                                "VSS fence lost mid-session ({e}); disabling replication for \
                                 this node to avoid corrupting another instance's state"
                            ));
                            self.disabled.set(true);
                            self.pending.borrow_mut().clear();
                            self.latest_hash.borrow_mut().clear();
                            self.last_accepted_ms.borrow_mut().clear();
                            *self.last_error.borrow_mut() = Some(e);
                            break;
                        }
                    }
                }
                Err(e) => {
                    *self.last_error.borrow_mut() = Some(e);
                    // Re-queue unless a newer desired value arrived during the await.
                    self.pending.borrow_mut().entry(vkey).or_insert(value);
                    break;
                }
            }
        }

        self.draining.set(false);
    }

    /// Record `vkey`'s manifest membership (`is_put` true = present) and try to
    /// write it out. If the manifest isn't writable right now (seed not loaded, or
    /// the write fails) the change stays parked in `pending_manifest` and is retried
    /// on the next drain — a value successfully put to VSS must eventually be listed
    /// in the manifest or restore would silently miss it.
    async fn maintain_manifest(self: &Rc<Self>, vkey: &str, is_put: bool) {
        {
            let already_present = self.known.borrow().contains(vkey);
            let no_parked_changes = self.pending_manifest.borrow().is_empty();
            if self.manifest_loaded.get() && already_present == is_put && no_parked_changes {
                // Membership unchanged (steady-state value update): skip the write.
                return;
            }
        }
        self.pending_manifest
            .borrow_mut()
            .insert(vkey.to_string(), is_put);
        self.sync_manifest().await;
    }

    /// Write `known` + all parked membership changes to the server manifest. On
    /// success the changes are committed into `known` and cleared; on failure they
    /// stay parked for the next round. Only called from within the single-flight
    /// drain, so `pending_manifest` cannot be mutated concurrently across the await.
    async fn sync_manifest(self: &Rc<Self>) {
        if !self.manifest_loaded.get() || self.pending_manifest.borrow().is_empty() {
            return;
        }
        let snapshot: Vec<String> = {
            let known = self.known.borrow();
            let pending = self.pending_manifest.borrow();
            let mut set: HashSet<String> = known.clone();
            for (key, is_put) in pending.iter() {
                if *is_put {
                    set.insert(key.clone());
                } else {
                    set.remove(key);
                }
            }
            set.into_iter().collect()
        };
        match self.store.write_manifest(&snapshot).await {
            Ok(()) => {
                let mut known = self.known.borrow_mut();
                for (key, is_put) in self.pending_manifest.borrow_mut().drain() {
                    if is_put {
                        known.insert(key);
                    } else {
                        known.remove(&key);
                    }
                }
            }
            Err(e) => {
                *self.last_error.borrow_mut() = Some(e);
            }
        }
    }
}

/// Wall-clock milliseconds for the reconstructible-singleton upload throttle.
fn now_ms() -> f64 {
    #[cfg(target_arch = "wasm32")]
    {
        js_sys::Date::now()
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as f64)
            .unwrap_or(0.0)
    }
}

/// Content hash for the replicate dirty check (not adversarial — both writer and
/// reader of the hash are this same in-memory replicator).
fn hash_bytes(buf: &[u8]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    buf.hash(&mut hasher);
    hasher.finish()
}

fn warn(msg: &str) {
    #[cfg(target_arch = "wasm32")]
    web_sys::console::warn_1(&wasm_bindgen::JsValue::from_str(msg));
    #[cfg(not(target_arch = "wasm32"))]
    let _ = msg;
}

// --- Same-origin single-writer guard via the Web Locks API -----------------
//
// The VSS fence catches a *cross-device* second writer, but two tabs in the same
// browser share one origin (and one localStorage) and would both hold the same
// instance's fence, so the fence alone can't tell them apart. `navigator.locks`
// gives a per-origin exclusive lock held for the tab's lifetime; a second tab's
// `ifAvailable` request returns no lock and we refuse to enable replication there.
// The browser auto-releases the lock when the tab/context is destroyed.

/// Web Locks name for a node's VSS writer lock.
pub(crate) fn web_lock_name(runtime_key: &str) -> String {
    format!("rln:vss-writer:{runtime_key}")
}

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen(inline_js = r#"
const __rln_vss_lock_releasers = {};
export function __rln_vss_acquire_lock(name) {
  return new Promise((resolve, reject) => {
    if (!navigator.locks || !navigator.locks.request) { resolve(true); return; }
    navigator.locks.request(name, { ifAvailable: true }, (lock) => {
      if (!lock) { resolve(false); return; }
      resolve(true);
      // Hold the lock until released: the callback's returned promise stays pending.
      return new Promise((release) => { __rln_vss_lock_releasers[name] = release; });
    }).catch((e) => reject(e));
  });
}
export function __rln_vss_release_lock(name) {
  const r = __rln_vss_lock_releasers[name];
  if (r) { delete __rln_vss_lock_releasers[name]; r(); }
}
"#)]
extern "C" {
    fn __rln_vss_acquire_lock(name: &str) -> js_sys::Promise;
    fn __rln_vss_release_lock(name: &str);
}

/// Try to acquire the same-origin writer lock. `Ok(true)` = acquired (or Web Locks
/// unsupported — best-effort allow); `Ok(false)` = another tab holds it.
#[cfg(target_arch = "wasm32")]
pub(crate) async fn acquire_web_lock(name: &str) -> Result<bool, wasm_bindgen::JsValue> {
    let value = wasm_bindgen_futures::JsFuture::from(__rln_vss_acquire_lock(name)).await?;
    Ok(value.as_bool().unwrap_or(false))
}

/// Release a previously acquired same-origin writer lock. No-op if not held.
#[cfg(target_arch = "wasm32")]
pub(crate) fn release_web_lock(name: &str) {
    __rln_vss_release_lock(name);
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) async fn acquire_web_lock(_name: &str) -> Result<bool, wasm_bindgen::JsValue> {
    Ok(true)
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn release_web_lock(_name: &str) {}

// --- Per-node registry -----------------------------------------------------
//
// The snapshot persist sites in `ldk_live_backend` are free functions keyed by
// `runtime_key`, so the replicator for a node is looked up here. When no replicator
// is registered (VSS not configured — the default), the `replicate_*` helpers are
// no-ops, leaving local persistence exactly as before.

thread_local! {
    static VSS_REPLICATORS: RefCell<HashMap<String, Rc<VssReplicator>>> =
        RefCell::new(HashMap::new());
}

pub(crate) fn register_vss_replicator(runtime_key: &str, replicator: Rc<VssReplicator>) {
    VSS_REPLICATORS.with(|r| {
        r.borrow_mut().insert(runtime_key.to_string(), replicator);
    });
}

pub(crate) fn unregister_vss_replicator(runtime_key: &str) {
    VSS_REPLICATORS.with(|r| {
        r.borrow_mut().remove(runtime_key);
    });
}

pub(crate) fn vss_replicator(runtime_key: &str) -> Option<Rc<VssReplicator>> {
    VSS_REPLICATORS.with(|r| r.borrow().get(runtime_key).cloned())
}

/// Fresh-load restore orchestrator: if a VSS store is registered for this node,
/// pull its snapshot state into the local browser store before the object graph
/// builds. No-op (returns `Ok(0)`) when VSS is not configured. Uses the safe
/// non-`force` guard, so a populated local store is never clobbered. Called from
/// `RlnWasmNode::configure_ldk_vss_replication`.
pub(crate) async fn maybe_restore_ldk_state_from_vss(
    runtime_key: &str,
) -> Result<usize, wasm_bindgen::JsValue> {
    let Some(replicator) = vss_replicator(runtime_key) else {
        return Ok(0);
    };
    crate::ldk_live_backend::restore_ldk_state_from_vss(replicator.store(), runtime_key, false)
        .await
}

pub(crate) fn replicate_monitor(runtime_key: &str, name: &str, bytes: Vec<u8>) {
    if let Some(r) = vss_replicator(runtime_key) {
        r.replicate(CAT_MONITOR, name, Some(bytes));
    }
}

pub(crate) fn replicate_monitor_removal(runtime_key: &str, name: &str) {
    if let Some(r) = vss_replicator(runtime_key) {
        r.replicate(CAT_MONITOR, name, None);
    }
}

pub(crate) fn replicate_manager(runtime_key: &str, bytes: Vec<u8>) {
    if let Some(r) = vss_replicator(runtime_key) {
        r.replicate(CAT_MANAGER, SINGLETON_MANAGER, Some(bytes));
    }
}

pub(crate) fn replicate_network_graph(runtime_key: &str, bytes: Vec<u8>) {
    if let Some(r) = vss_replicator(runtime_key) {
        r.replicate(CAT_NETWORK_GRAPH, SINGLETON_NETWORK_GRAPH, Some(bytes));
    }
}

pub(crate) fn replicate_scorer(runtime_key: &str, bytes: Vec<u8>) {
    if let Some(r) = vss_replicator(runtime_key) {
        r.replicate(CAT_SCORER, SINGLETON_SCORER, Some(bytes));
    }
}

/// Tear down VSS replication for a node: release the fence (best-effort, async),
/// free the same-origin Web Lock, and unregister the replicator (queued writes are
/// dropped). Shared by `disableLdkVssReplication`, `RlnWasmNode::drop`, and the
/// configure-failure rollback so no path can leak a guard.
pub(crate) fn teardown_vss_replication(runtime_key: &str) {
    if let Some(r) = vss_replicator(runtime_key) {
        r.spawn_release_fence();
    }
    release_web_lock(&web_lock_name(runtime_key));
    unregister_vss_replicator(runtime_key);
}

// --- Persistent fence instance id -------------------------------------------
//
// The VSS fence token must be *stable across page reloads*: `acquire_fence` is a
// strict equality check, and a tab that reloads (or crashes) never runs its async
// fence release. If each configure call minted a fresh random id, a reloaded tab
// would see "owned by another instance" against its own stale token and lock itself
// out of its own VSS store permanently. Persisting the id in localStorage makes a
// reload (and every tab of this origin — the Web Lock arbitrates between them) the
// same owner, so re-acquire is idempotent.

fn instance_id_storage_key(runtime_key: &str) -> String {
    format!("rln:vss-instance:{runtime_key}")
}

/// Load this browser's stable fence instance id for `runtime_key`, minting and
/// persisting a fresh random one on first use.
pub(crate) fn persistent_instance_id(runtime_key: &str) -> Result<String, wasm_bindgen::JsValue> {
    let storage_key = instance_id_storage_key(runtime_key);
    #[cfg(target_arch = "wasm32")]
    let storage = web_sys::window().and_then(|w| w.local_storage().ok().flatten());
    #[cfg(target_arch = "wasm32")]
    if let Some(storage) = &storage {
        if let Ok(Some(existing)) = storage.get_item(&storage_key) {
            if !existing.is_empty() {
                return Ok(existing);
            }
        }
    }
    let mut id_bytes = [0u8; 16];
    getrandom::getrandom(&mut id_bytes)
        .map_err(|e| wasm_bindgen::JsValue::from_str(&format!("RNG unavailable: {e}")))?;
    let id = hex::encode(id_bytes);
    #[cfg(target_arch = "wasm32")]
    if let Some(storage) = &storage {
        let _ = storage.set_item(&storage_key, &id);
    }
    #[cfg(not(target_arch = "wasm32"))]
    let _ = storage_key;
    Ok(id)
}

// --- Mirroring KV store wrapper ---------------------------------------------

/// [`KVStoreSync`] wrapper that mirrors every successful write/remove to the node's
/// VSS replicator (no-op when none is registered). Wrapping the store — instead of
/// sprinkling `replicate_*` calls at persist sites — guarantees new persist paths
/// through this store can never silently skip replication. Used for the RGB KV
/// store, whose per-channel `RgbInfo`/payment/consignment state is exactly what a
/// fresh-device restore needs to keep colored channels colored.
pub(crate) struct VssMirroredKvStore {
    inner: Arc<dyn KVStoreSync + Send + Sync>,
    runtime_key: String,
}

impl VssMirroredKvStore {
    pub(crate) fn new(inner: Arc<dyn KVStoreSync + Send + Sync>, runtime_key: &str) -> Self {
        Self {
            inner,
            runtime_key: runtime_key.to_string(),
        }
    }
}

impl KVStoreSync for VssMirroredKvStore {
    fn read(
        &self,
        primary_namespace: &str,
        secondary_namespace: &str,
        key: &str,
    ) -> Result<Vec<u8>, io::Error> {
        self.inner.read(primary_namespace, secondary_namespace, key)
    }

    fn write(
        &self,
        primary_namespace: &str,
        secondary_namespace: &str,
        key: &str,
        buf: Vec<u8>,
    ) -> Result<(), io::Error> {
        // Clone only when a replicator is registered, and queue the mirror only
        // after the local write (the durability ack gate) succeeded.
        let mirror = vss_replicator(&self.runtime_key).map(|r| (r, buf.clone()));
        self.inner
            .write(primary_namespace, secondary_namespace, key, buf)?;
        if let Some((replicator, bytes)) = mirror {
            replicator.replicate_triple(primary_namespace, secondary_namespace, key, Some(bytes));
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
        self.inner
            .remove(primary_namespace, secondary_namespace, key, lazy)?;
        if let Some(replicator) = vss_replicator(&self.runtime_key) {
            replicator.replicate_triple(primary_namespace, secondary_namespace, key, None);
        }
        Ok(())
    }

    fn list(
        &self,
        primary_namespace: &str,
        secondary_namespace: &str,
    ) -> Result<Vec<String>, io::Error> {
        self.inner.list(primary_namespace, secondary_namespace)
    }
}

#[cfg(all(test, target_arch = "wasm32"))]
mod tests {
    use super::*;
    use rgb_lib_wasm::bdk_wallet::bitcoin::secp256k1::SecretKey;

    fn test_replicator() -> Rc<VssReplicator> {
        let sk = SecretKey::from_slice(&[9u8; 32]).expect("valid test key");
        // Dummy URL: these tests never drive the network (drain is not awaited here).
        let store = WasmVssKvStore::new(
            "http://localhost:0/vss".to_string(),
            "test-store-ldk".to_string(),
            sk,
        );
        VssReplicator::new(store, "instance-under-test".to_string())
    }

    /// The fence re-check must fire exactly once per `FENCE_CHECK_INTERVAL` writes
    /// and then reset its counter.
    #[wasm_bindgen_test::wasm_bindgen_test]
    fn fence_check_due_fires_on_the_interval_boundary() {
        let r = test_replicator();
        for i in 1..FENCE_CHECK_INTERVAL {
            assert!(!r.fence_check_due(), "should not fire on write {i}");
        }
        assert!(r.fence_check_due(), "must fire on the interval-th write");
        assert!(!r.fence_check_due(), "counter must reset after firing");
    }

    /// Once disabled (fence lost), `replicate` is a no-op — nothing is queued.
    #[wasm_bindgen_test::wasm_bindgen_test]
    fn disabled_replicator_drops_replication() {
        let r = test_replicator();
        r.disabled.set(true);
        r.replicate(CAT_MONITOR, "chan-1", Some(vec![1, 2, 3]));
        assert_eq!(r.pending_count(), 0);
        assert!(r.is_disabled());
    }

    /// A fresh replicator reports no pending work and no error.
    #[wasm_bindgen_test::wasm_bindgen_test]
    fn fresh_replicator_is_clean() {
        let r = test_replicator();
        assert_eq!(r.pending_count(), 0);
        assert!(r.last_error().is_none());
        assert!(!r.is_disabled());
    }

    /// Byte-identical re-replication of an already-uploaded value is dropped (the
    /// per-tick snapshot heartbeat must not turn into per-second identical PUTs),
    /// while changed bytes still queue.
    #[wasm_bindgen_test::wasm_bindgen_test]
    fn replicate_dedupes_unchanged_bytes() {
        let r = test_replicator();
        // Simulate a completed upload of v1: hash recorded, queue drained.
        r.replicate(CAT_MANAGER, "channel-manager", Some(vec![1, 2, 3]));
        r.pending.borrow_mut().clear();
        assert_eq!(r.pending_count(), 0);

        // Same bytes again: dirty check drops it.
        r.replicate(CAT_MANAGER, "channel-manager", Some(vec![1, 2, 3]));
        assert_eq!(r.pending_count(), 0, "identical bytes must not re-queue");

        // Changed bytes: queued.
        r.replicate(CAT_MANAGER, "channel-manager", Some(vec![9, 9, 9]));
        assert_eq!(r.pending_count(), 1, "changed bytes must queue");
    }

    /// Graph/scorer uploads are rate-limited even when the bytes change (gossip
    /// churns them every tick), while monitors always replicate immediately.
    #[wasm_bindgen_test::wasm_bindgen_test]
    fn reconstructible_singletons_are_rate_limited() {
        let r = test_replicator();
        r.replicate(CAT_NETWORK_GRAPH, SINGLETON_NETWORK_GRAPH, Some(vec![1]));
        assert_eq!(r.pending_count(), 1, "first graph upload goes out");
        r.pending.borrow_mut().clear(); // simulate drained upload

        r.replicate(CAT_NETWORK_GRAPH, SINGLETON_NETWORK_GRAPH, Some(vec![2]));
        assert_eq!(
            r.pending_count(),
            0,
            "changed graph bytes inside the window must be throttled"
        );

        // Age the throttle stamp past the window: the next change goes out.
        let vkey = vss_key("", CAT_NETWORK_GRAPH, SINGLETON_NETWORK_GRAPH);
        r.last_accepted_ms.borrow_mut().insert(
            vkey,
            now_ms() - RECONSTRUCTIBLE_MIN_UPLOAD_INTERVAL_MS - 1.0,
        );
        r.replicate(CAT_NETWORK_GRAPH, SINGLETON_NETWORK_GRAPH, Some(vec![2]));
        assert_eq!(r.pending_count(), 1, "post-window change must upload");

        // Monitors are never throttled: consecutive changes both queue.
        r.pending.borrow_mut().clear();
        r.replicate(CAT_MONITOR, "chan-1", Some(vec![1]));
        r.pending.borrow_mut().clear();
        r.replicate(CAT_MONITOR, "chan-1", Some(vec![2]));
        assert_eq!(r.pending_count(), 1, "monitor changes always replicate");
    }

    /// A removal clears the dirty-check hash, so re-putting the previously uploaded
    /// bytes afterwards is NOT deduped away.
    #[wasm_bindgen_test::wasm_bindgen_test]
    fn removal_resets_dirty_check_for_reput() {
        let r = test_replicator();
        r.replicate(CAT_MONITOR, "chan-1", Some(vec![4, 5, 6]));
        r.pending.borrow_mut().clear(); // simulate drained upload

        r.replicate(CAT_MONITOR, "chan-1", None); // archive the monitor
        r.pending.borrow_mut().clear(); // simulate drained removal

        r.replicate(CAT_MONITOR, "chan-1", Some(vec![4, 5, 6]));
        assert_eq!(
            r.pending_count(),
            1,
            "re-put after removal must go out even with identical bytes"
        );
    }
}
