#[cfg(target_arch = "wasm32")]
use js_sys::Array;
use wasm_bindgen::prelude::JsValue;
#[cfg(target_arch = "wasm32")]
use wasm_bindgen_futures::{spawn_local, JsFuture};

#[cfg(test)]
#[path = "tests/runtime_store_tests.rs"]
mod tests;

pub(crate) trait RuntimeStateStore {
    fn get(&self, key: &str) -> Result<Option<String>, JsValue>;
    fn set(&self, key: &str, value: &str) -> Result<(), JsValue>;
    fn delete(&self, key: &str) -> Result<(), JsValue>;
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct BrowserPersistentStateStore;

impl RuntimeStateStore for BrowserPersistentStateStore {
    fn get(&self, key: &str) -> Result<Option<String>, JsValue> {
        local_storage_get_item(key)
    }

    fn set(&self, key: &str, value: &str) -> Result<(), JsValue> {
        local_storage_set_item(key, value)?;
        persist_to_indexed_db_background(key.to_string(), value.to_string());
        Ok(())
    }

    fn delete(&self, key: &str) -> Result<(), JsValue> {
        local_storage_remove_item(key)?;
        remove_from_indexed_db_background(key.to_string());
        Ok(())
    }
}

pub(crate) fn browser_persistent_state_store() -> BrowserPersistentStateStore {
    BrowserPersistentStateStore
}

pub(crate) const RUNTIME_STATE_HYDRATE_PREFIXES: &[&str] = &[
    crate::wasm_node_persistence::WASM_LDK_RUNTIME_STORAGE_PREFIX,
    "rln:wasm:swap-runtime:",
    "rln:wasm:media:",
    "rln:wasm:wallet-rgb-proxy:",
    crate::wasm_node_persistence::WASM_LN_RUNTIME_CORE_STORAGE_PREFIX,
    crate::wasm_node_persistence::WASM_CHAIN_SYNC_STORAGE_PREFIX,
    crate::wasm_node_persistence::WASM_LDK_BROADCAST_QUEUE_STORAGE_PREFIX,
    crate::wasm_node_persistence::WASM_LDK_MONITORS_STORAGE_PREFIX,
    crate::wasm_node_persistence::WASM_RUNTIME_EVENTS_STORAGE_PREFIX,
    crate::wasm_node_persistence::WASM_RGB_LN_TRANSFERS_STORAGE_PREFIX,
    crate::wasm_node_persistence::WASM_VIRTUAL_CHANNELS_V0_STORAGE_PREFIX,
    crate::wasm_node_persistence::WASM_PEER_SESSIONS_STORAGE_PREFIX,
];

pub(crate) async fn preload_runtime_state_from_persistent_store() -> Result<(), JsValue> {
    hydrate_local_storage_from_indexed_db_prefixes(RUNTIME_STATE_HYDRATE_PREFIXES).await
}

#[cfg(target_arch = "wasm32")]
thread_local! {
    static RUNTIME_STATE_PRELOADED: std::cell::RefCell<bool> = const { std::cell::RefCell::new(false) };
}

#[cfg(target_arch = "wasm32")]
async fn hydrate_local_storage_from_indexed_db_prefixes(prefixes: &[&str]) -> Result<(), JsValue> {
    let already = RUNTIME_STATE_PRELOADED.with(|loaded| *loaded.borrow());
    if already {
        return Ok(());
    }

    let entries = indexed_db_list_entries().await?;
    for entry in entries {
        let pair = Array::from(&entry);
        if pair.length() != 2 {
            continue;
        }
        let key = pair.get(0).as_string();
        let value = pair.get(1).as_string();
        let (Some(key), Some(value)) = (key, value) else {
            continue;
        };
        if prefixes.iter().any(|prefix| key.starts_with(prefix)) {
            let _ = local_storage_set_item(&key, &value);
        }
    }

    RUNTIME_STATE_PRELOADED.with(|loaded| {
        *loaded.borrow_mut() = true;
    });
    Ok(())
}

#[cfg(not(target_arch = "wasm32"))]
async fn hydrate_local_storage_from_indexed_db_prefixes(_prefixes: &[&str]) -> Result<(), JsValue> {
    Ok(())
}

#[cfg(target_arch = "wasm32")]
fn persist_to_indexed_db_background(key: String, value: String) {
    spawn_local(async move {
        let _ = indexed_db_set_item(&key, &value).await;
    });
}

#[cfg(target_arch = "wasm32")]
fn remove_from_indexed_db_background(key: String) {
    spawn_local(async move {
        let _ = indexed_db_delete_item(&key).await;
    });
}

#[cfg(not(target_arch = "wasm32"))]
fn persist_to_indexed_db_background(_key: String, _value: String) {}

#[cfg(not(target_arch = "wasm32"))]
fn remove_from_indexed_db_background(_key: String) {}

#[cfg(target_arch = "wasm32")]
fn local_storage_get_item(key: &str) -> Result<Option<String>, JsValue> {
    let Some(window) = web_sys::window() else {
        return Ok(None);
    };
    let Some(storage) = window.local_storage()? else {
        return Ok(None);
    };
    storage.get_item(key)
}

#[cfg(not(target_arch = "wasm32"))]
fn local_storage_get_item(_key: &str) -> Result<Option<String>, JsValue> {
    Ok(None)
}

#[cfg(target_arch = "wasm32")]
fn local_storage_set_item(key: &str, value: &str) -> Result<(), JsValue> {
    let Some(window) = web_sys::window() else {
        return Ok(());
    };
    let Some(storage) = window.local_storage()? else {
        return Ok(());
    };
    storage.set_item(key, value)
}

#[cfg(target_arch = "wasm32")]
fn local_storage_remove_item(key: &str) -> Result<(), JsValue> {
    let Some(window) = web_sys::window() else {
        return Ok(());
    };
    let Some(storage) = window.local_storage()? else {
        return Ok(());
    };
    storage.remove_item(key)
}

#[cfg(not(target_arch = "wasm32"))]
fn local_storage_set_item(_key: &str, _value: &str) -> Result<(), JsValue> {
    Ok(())
}

#[cfg(not(target_arch = "wasm32"))]
fn local_storage_remove_item(_key: &str) -> Result<(), JsValue> {
    Ok(())
}

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen(inline_js = r#"
export function __rln_runtime_idb_set(key, value) {
  return new Promise((resolve, reject) => {
    const req = indexedDB.open("rln_wasm_sdk_runtime", 1);
    req.onupgradeneeded = () => {
      const db = req.result;
      if (!db.objectStoreNames.contains("state")) db.createObjectStore("state");
    };
    req.onerror = () => reject(req.error || new Error("indexedDB open failed"));
    req.onsuccess = () => {
      const db = req.result;
      const tx = db.transaction("state", "readwrite");
      const store = tx.objectStore("state");
      const putReq = store.put(value, key);
      putReq.onerror = () => reject(putReq.error || new Error("indexedDB put failed"));
      tx.oncomplete = () => { db.close(); resolve(undefined); };
      tx.onerror = () => { db.close(); reject(tx.error || new Error("indexedDB tx failed")); };
      tx.onabort = () => { db.close(); reject(tx.error || new Error("indexedDB tx aborted")); };
    };
  });
}

export function __rln_runtime_idb_entries() {
  return new Promise((resolve, reject) => {
    const req = indexedDB.open("rln_wasm_sdk_runtime", 1);
    req.onupgradeneeded = () => {
      const db = req.result;
      if (!db.objectStoreNames.contains("state")) db.createObjectStore("state");
    };
    req.onerror = () => reject(req.error || new Error("indexedDB open failed"));
    req.onsuccess = () => {
      const db = req.result;
      const tx = db.transaction("state", "readonly");
      const store = tx.objectStore("state");
      const entriesReq = store.getAll();
      const keysReq = store.getAllKeys();
      entriesReq.onerror = () => reject(entriesReq.error || new Error("indexedDB getAll failed"));
      keysReq.onerror = () => reject(keysReq.error || new Error("indexedDB getAllKeys failed"));
      tx.oncomplete = () => {
        const keys = keysReq.result || [];
        const vals = entriesReq.result || [];
        const out = [];
        for (let i = 0; i < keys.length; i += 1) {
          out.push([String(keys[i]), typeof vals[i] === "string" ? vals[i] : ""]);
        }
        db.close();
        resolve(out);
      };
      tx.onerror = () => { db.close(); reject(tx.error || new Error("indexedDB tx failed")); };
      tx.onabort = () => { db.close(); reject(tx.error || new Error("indexedDB tx aborted")); };
    };
  });
}

export function __rln_runtime_idb_delete(key) {
  return new Promise((resolve, reject) => {
    const req = indexedDB.open("rln_wasm_sdk_runtime", 1);
    req.onupgradeneeded = () => {
      const db = req.result;
      if (!db.objectStoreNames.contains("state")) db.createObjectStore("state");
    };
    req.onerror = () => reject(req.error || new Error("indexedDB open failed"));
    req.onsuccess = () => {
      const db = req.result;
      const tx = db.transaction("state", "readwrite");
      const store = tx.objectStore("state");
      const delReq = store.delete(key);
      delReq.onerror = () => reject(delReq.error || new Error("indexedDB delete failed"));
      tx.oncomplete = () => { db.close(); resolve(undefined); };
      tx.onerror = () => { db.close(); reject(tx.error || new Error("indexedDB tx failed")); };
      tx.onabort = () => { db.close(); reject(tx.error || new Error("indexedDB tx aborted")); };
    };
  });
}
"#)]
extern "C" {
    fn __rln_runtime_idb_set(key: &str, value: &str) -> js_sys::Promise;
    fn __rln_runtime_idb_entries() -> js_sys::Promise;
    fn __rln_runtime_idb_delete(key: &str) -> js_sys::Promise;
}

#[cfg(target_arch = "wasm32")]
async fn indexed_db_set_item(key: &str, value: &str) -> Result<(), JsValue> {
    let promise = __rln_runtime_idb_set(key, value);
    let _ = JsFuture::from(promise).await?;
    Ok(())
}

#[cfg(target_arch = "wasm32")]
async fn indexed_db_list_entries() -> Result<Vec<JsValue>, JsValue> {
    let promise = __rln_runtime_idb_entries();
    let value = JsFuture::from(promise).await?;
    Ok(Array::from(&value).to_vec())
}

#[cfg(target_arch = "wasm32")]
async fn indexed_db_delete_item(key: &str) -> Result<(), JsValue> {
    let promise = __rln_runtime_idb_delete(key);
    let _ = JsFuture::from(promise).await?;
    Ok(())
}
