use std::sync::Arc;

use base64::{engine::general_purpose, Engine as _};
use lightning::io;
use lightning::util::persist::KVStoreSync;

/// localStorage-backed [`KVStoreSync`] for per-node LDK/RGB key-value state.
///
/// On `wasm32`, values are base64-encoded and stored in `window.localStorage`.
/// On non-wasm32 (unit tests), an in-memory `RwLock<HashMap>` is used instead.
///
/// Key format in localStorage:
///   `rln:ldk-kv:{scope_prefix}:{primary_namespace}:{secondary_namespace}:{key}`
///
/// The `scope_prefix` (the node's `runtime_key`) isolates entries for different
/// node instances in the same browser tab.
pub struct LdkBrowserKvStore {
    scope_prefix: String,
    #[cfg(not(target_arch = "wasm32"))]
    inner: std::sync::RwLock<std::collections::HashMap<(String, String, String), Vec<u8>>>,
}

// localStorage is accessed only on the single WASM thread.
#[cfg(target_arch = "wasm32")]
unsafe impl Send for LdkBrowserKvStore {}
#[cfg(target_arch = "wasm32")]
unsafe impl Sync for LdkBrowserKvStore {}

impl LdkBrowserKvStore {
    pub fn new(scope_prefix: impl Into<String>) -> Self {
        Self {
            scope_prefix: scope_prefix.into(),
            #[cfg(not(target_arch = "wasm32"))]
            inner: std::sync::RwLock::new(std::collections::HashMap::new()),
        }
    }

    fn storage_key(&self, primary_namespace: &str, secondary_namespace: &str, key: &str) -> String {
        format!(
            "rln:ldk-kv:{}:{}:{}:{}",
            self.scope_prefix, primary_namespace, secondary_namespace, key
        )
    }

    fn list_prefix(&self, primary_namespace: &str, secondary_namespace: &str) -> String {
        format!(
            "rln:ldk-kv:{}:{}:{}:",
            self.scope_prefix, primary_namespace, secondary_namespace
        )
    }

    #[cfg(target_arch = "wasm32")]
    fn storage(&self) -> Result<web_sys::Storage, io::Error> {
        web_sys::window()
            .ok_or_else(|| io::Error::new(io::ErrorKind::Other, "browser window unavailable"))?
            .local_storage()
            .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("{e:?}")))?
            .ok_or_else(|| io::Error::new(io::ErrorKind::Other, "localStorage unavailable"))
    }
}

pub fn ldk_browser_kv_store(scope_prefix: &str) -> Arc<LdkBrowserKvStore> {
    Arc::new(LdkBrowserKvStore::new(scope_prefix))
}

impl KVStoreSync for LdkBrowserKvStore {
    fn read(
        &self,
        primary_namespace: &str,
        secondary_namespace: &str,
        key: &str,
    ) -> Result<Vec<u8>, io::Error> {
        #[cfg(target_arch = "wasm32")]
        {
            let raw = self
                .storage()?
                .get_item(&self.storage_key(primary_namespace, secondary_namespace, key))
                .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("{e:?}")))?
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "key not found"))?;
            general_purpose::STANDARD
                .decode(raw)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.inner
                .read()
                .map_err(|_| io::Error::new(io::ErrorKind::Other, "lock poisoned"))?
                .get(&(
                    primary_namespace.to_string(),
                    secondary_namespace.to_string(),
                    key.to_string(),
                ))
                .cloned()
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "key not found"))
        }
    }

    fn write(
        &self,
        primary_namespace: &str,
        secondary_namespace: &str,
        key: &str,
        buf: Vec<u8>,
    ) -> Result<(), io::Error> {
        #[cfg(target_arch = "wasm32")]
        {
            self.storage()?
                .set_item(
                    &self.storage_key(primary_namespace, secondary_namespace, key),
                    &general_purpose::STANDARD.encode(&buf),
                )
                .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("{e:?}")))
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.inner
                .write()
                .map_err(|_| io::Error::new(io::ErrorKind::Other, "lock poisoned"))?
                .insert(
                    (
                        primary_namespace.to_string(),
                        secondary_namespace.to_string(),
                        key.to_string(),
                    ),
                    buf,
                );
            Ok(())
        }
    }

    fn remove(
        &self,
        primary_namespace: &str,
        secondary_namespace: &str,
        key: &str,
        _lazy: bool,
    ) -> Result<(), io::Error> {
        #[cfg(target_arch = "wasm32")]
        {
            self.storage()?
                .remove_item(&self.storage_key(primary_namespace, secondary_namespace, key))
                .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("{e:?}")))
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.inner
                .write()
                .map_err(|_| io::Error::new(io::ErrorKind::Other, "lock poisoned"))?
                .remove(&(
                    primary_namespace.to_string(),
                    secondary_namespace.to_string(),
                    key.to_string(),
                ));
            Ok(())
        }
    }

    fn list(
        &self,
        primary_namespace: &str,
        secondary_namespace: &str,
    ) -> Result<Vec<String>, io::Error> {
        #[cfg(target_arch = "wasm32")]
        {
            let storage = self.storage()?;
            let prefix = self.list_prefix(primary_namespace, secondary_namespace);
            let len = storage
                .length()
                .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("{e:?}")))?;
            let mut keys = Vec::new();
            for i in 0..len {
                if let Some(k) = storage
                    .key(i)
                    .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("{e:?}")))?
                {
                    if let Some(suffix) = k.strip_prefix(&prefix) {
                        keys.push(suffix.to_owned());
                    }
                }
            }
            Ok(keys)
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            let guard = self
                .inner
                .read()
                .map_err(|_| io::Error::new(io::ErrorKind::Other, "lock poisoned"))?;
            let keys: Vec<String> = guard
                .keys()
                .filter(|(p, s, _)| p == primary_namespace && s == secondary_namespace)
                .map(|(_, _, k)| k.clone())
                .collect();
            Ok(keys)
        }
    }
}
