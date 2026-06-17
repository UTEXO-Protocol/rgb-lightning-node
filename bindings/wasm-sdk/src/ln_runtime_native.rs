use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use serde::{Deserialize, Serialize};

use crate::runtime_store::{browser_persistent_state_store, RuntimeStateStore};
use crate::wasm_node_persistence::WASM_LN_RUNTIME_CORE_STORAGE_PREFIX;

#[cfg(test)]
#[path = "tests/ln_runtime_native_test_utils.rs"]
mod test_utils;
#[cfg(test)]
#[path = "tests/ln_runtime_native_tests.rs"]
mod tests;

const NATIVE_RUNTIME_CORE_PENDING_SUFFIX: &str = ":pending";
const NATIVE_RUNTIME_CORE_COMMITTED_SUFFIX: &str = ":committed";

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum NativeLnRuntimeLifecycleState {
    Cold,
    Starting,
    Running,
    RunningRestored,
    Stopping,
    Stopped,
    RestoredStopped,
}

impl NativeLnRuntimeLifecycleState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Cold => "cold",
            Self::Starting => "starting",
            Self::Running => "running",
            Self::RunningRestored => "running_restored",
            Self::Stopping => "stopping",
            Self::Stopped => "stopped",
            Self::RestoredStopped => "restored_stopped",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NativeLnRuntimeQueuedEventData {
    pub seq: u64,
    pub event_kind: String,
    pub payload_hex: String,
    pub received_at: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct NativeLnRuntimeCoreStatusData {
    pub lifecycle_state: String,
    pub ready: bool,
    pub storage_initialized: bool,
    pub schema_version: u32,
    pub queued_events: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct NativeLnRuntimeCoreSnapshot {
    revision: u64,
    lifecycle_state: NativeLnRuntimeLifecycleState,
    storage_initialized: bool,
    schema_version: u32,
    queued_events: Vec<NativeLnRuntimeQueuedEventData>,
    next_event_seq: u64,
}

impl Default for NativeLnRuntimeCoreSnapshot {
    fn default() -> Self {
        Self {
            revision: 0,
            lifecycle_state: NativeLnRuntimeLifecycleState::Cold,
            storage_initialized: false,
            schema_version: 1,
            queued_events: Vec::new(),
            next_event_seq: 0,
        }
    }
}

thread_local! {
    static NATIVE_RUNTIME_CORE_MEMORY_SNAPSHOTS: RefCell<HashMap<String, NativeLnRuntimeCoreSnapshot>> =
        RefCell::new(HashMap::new());
}

// Cloneable so the autonomous drive loop can hold its own handle to the same runtime queue. The
// `snapshot` is an `Rc<RefCell<..>>`, so a clone shares the underlying queue/state — draining from
// either handle drains the same events.
#[derive(Clone)]
pub struct NativeLnRuntimeCore {
    storage_key_base: String,
    snapshot: Rc<RefCell<NativeLnRuntimeCoreSnapshot>>,
}

impl NativeLnRuntimeCore {
    pub fn new(runtime_key: String) -> Self {
        let storage_key_base = format!("{WASM_LN_RUNTIME_CORE_STORAGE_PREFIX}{runtime_key}");
        let snapshot = load_snapshot_with_recovery(&storage_key_base).unwrap_or_default();
        Self {
            storage_key_base,
            snapshot: Rc::new(RefCell::new(snapshot)),
        }
    }

    pub fn ensure_started(&self) {
        let mut snapshot = self.snapshot.borrow_mut();
        match snapshot.lifecycle_state {
            NativeLnRuntimeLifecycleState::Running
            | NativeLnRuntimeLifecycleState::RunningRestored => {
                if !snapshot.storage_initialized {
                    snapshot.storage_initialized = true;
                    persist_snapshot(&self.storage_key_base, &mut snapshot);
                }
                return;
            }
            NativeLnRuntimeLifecycleState::RestoredStopped => {
                snapshot.lifecycle_state = NativeLnRuntimeLifecycleState::RunningRestored;
            }
            _ => {
                snapshot.lifecycle_state = NativeLnRuntimeLifecycleState::Running;
            }
        }
        snapshot.storage_initialized = true;
        persist_snapshot(&self.storage_key_base, &mut snapshot);
    }

    pub fn stop(&self) {
        let mut snapshot = self.snapshot.borrow_mut();
        snapshot.lifecycle_state = match snapshot.lifecycle_state {
            NativeLnRuntimeLifecycleState::RunningRestored => {
                NativeLnRuntimeLifecycleState::RestoredStopped
            }
            NativeLnRuntimeLifecycleState::Running => NativeLnRuntimeLifecycleState::Stopped,
            other => other,
        };
        persist_snapshot(&self.storage_key_base, &mut snapshot);
    }

    pub fn enqueue_event(
        &self,
        event_kind: String,
        payload_hex: String,
    ) -> NativeLnRuntimeQueuedEventData {
        let mut snapshot = self.snapshot.borrow_mut();
        let seq = snapshot.next_event_seq;
        snapshot.next_event_seq = snapshot.next_event_seq.saturating_add(1);
        let event = NativeLnRuntimeQueuedEventData {
            seq,
            event_kind,
            payload_hex,
            received_at: unix_now_secs(),
        };
        snapshot.queued_events.push(event.clone());
        persist_snapshot(&self.storage_key_base, &mut snapshot);
        event
    }

    pub fn drain_events(&self) -> Vec<NativeLnRuntimeQueuedEventData> {
        let mut snapshot = self.snapshot.borrow_mut();
        let drained = std::mem::take(&mut snapshot.queued_events);
        persist_snapshot(&self.storage_key_base, &mut snapshot);
        drained
    }

    pub fn status(&self) -> NativeLnRuntimeCoreStatusData {
        let snapshot = self.snapshot.borrow();
        NativeLnRuntimeCoreStatusData {
            lifecycle_state: snapshot.lifecycle_state.as_str().to_string(),
            ready: matches!(
                snapshot.lifecycle_state,
                NativeLnRuntimeLifecycleState::Running
                    | NativeLnRuntimeLifecycleState::RunningRestored
            ),
            storage_initialized: snapshot.storage_initialized,
            schema_version: snapshot.schema_version,
            queued_events: snapshot.queued_events.len(),
        }
    }
}

fn storage_keys(base: &str) -> (String, String) {
    let pending = format!("{base}{NATIVE_RUNTIME_CORE_PENDING_SUFFIX}");
    let committed = format!("{base}{NATIVE_RUNTIME_CORE_COMMITTED_SUFFIX}");
    (pending, committed)
}

fn load_snapshot_with_recovery(storage_key_base: &str) -> Option<NativeLnRuntimeCoreSnapshot> {
    let (pending_key, committed_key) = storage_keys(storage_key_base);
    let pending = load_snapshot_entry(&pending_key);
    let committed = load_snapshot_entry(&committed_key);

    let (selected, selected_pending) = match (pending, committed) {
        (Some(p), Some(c)) if p.revision >= c.revision => (p, true),
        (Some(_p), Some(c)) => (c, false),
        (Some(p), None) => (p, true),
        (None, Some(c)) => (c, false),
        (None, None) => return None,
    };

    if selected_pending {
        // Crash-safe recovery: if pending exists and is newest, promote it.
        write_snapshot_entry(&committed_key, &selected);
        delete_snapshot_entry(&pending_key);
    }
    Some(selected)
}

fn load_snapshot_entry(storage_key: &str) -> Option<NativeLnRuntimeCoreSnapshot> {
    let store = browser_persistent_state_store();
    if let Ok(Some(raw)) = store.get(storage_key) {
        if let Ok(snapshot) = serde_json::from_str::<NativeLnRuntimeCoreSnapshot>(&raw) {
            NATIVE_RUNTIME_CORE_MEMORY_SNAPSHOTS.with(|state| {
                state
                    .borrow_mut()
                    .insert(storage_key.to_string(), snapshot.clone());
            });
            return Some(snapshot);
        }
    }
    NATIVE_RUNTIME_CORE_MEMORY_SNAPSHOTS.with(|state| state.borrow().get(storage_key).cloned())
}

fn write_snapshot_entry(storage_key: &str, snapshot: &NativeLnRuntimeCoreSnapshot) {
    if let Ok(raw) = serde_json::to_string(snapshot) {
        let store = browser_persistent_state_store();
        let _ = store.set(storage_key, &raw);
    }
    NATIVE_RUNTIME_CORE_MEMORY_SNAPSHOTS.with(|state| {
        state
            .borrow_mut()
            .insert(storage_key.to_string(), snapshot.clone());
    });
}

fn delete_snapshot_entry(storage_key: &str) {
    let store = browser_persistent_state_store();
    let _ = store.delete(storage_key);
    NATIVE_RUNTIME_CORE_MEMORY_SNAPSHOTS.with(|state| {
        state.borrow_mut().remove(storage_key);
    });
}

fn persist_snapshot(storage_key_base: &str, snapshot: &mut NativeLnRuntimeCoreSnapshot) {
    snapshot.revision = snapshot.revision.saturating_add(1);
    let (pending_key, committed_key) = storage_keys(storage_key_base);
    write_snapshot_entry(&pending_key, snapshot);
    write_snapshot_entry(&committed_key, snapshot);
    delete_snapshot_entry(&pending_key);
}

fn unix_now_secs() -> u64 {
    #[cfg(target_arch = "wasm32")]
    {
        (js_sys::Date::now() as u64) / 1000
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        use std::time::{SystemTime, UNIX_EPOCH};
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    }
}
