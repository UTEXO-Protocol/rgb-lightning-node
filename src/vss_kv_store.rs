use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bitcoin::io;
use bitcoin::secp256k1::SecretKey;
use lightning::util::persist::KVStoreSync;
use vss_client::client::VssClient;
use vss_client::error::VssError;
use vss_client::headers::sigs_auth::SigsAuthProvider;
use vss_client::types::{GetObjectRequest, KeyValue, ListKeyVersionsRequest, PutObjectRequest};
use vss_client::util::retry::{
    ExponentialBackoffRetryPolicy, MaxAttemptsRetryPolicy, MaxTotalDelayRetryPolicy, RetryPolicy,
};

/// Type alias for the retry policy used by VssClient.
type VssRetryPolicy =
    MaxTotalDelayRetryPolicy<MaxAttemptsRetryPolicy<ExponentialBackoffRetryPolicy<VssError>>>;

/// KVStore implementation backed by a VSS (Versioned Storage Service) server.
///
/// Maps LDK's `(primary_namespace, secondary_namespace, key)` triple to a single
/// VSS key string and provides version tracking for optimistic locking.
pub struct VssKvStore {
    client: VssClient<VssRetryPolicy>,
    store_id: String,
    /// Tracks the current version for each VSS key for optimistic locking.
    versions: Mutex<HashMap<String, i64>>,
    /// Dedicated tokio runtime for async VSS client operations.
    runtime: tokio::runtime::Runtime,
}

impl VssKvStore {
    /// Creates a new VssKvStore connected to the given VSS server.
    ///
    /// # Arguments
    /// * `server_url` - VSS server URL (e.g., "http://localhost:8081/vss")
    /// * `store_id` - Keyspace identifier (derived from node pubkey)
    /// * `signing_key` - Secret key for signature-based authentication
    pub fn new(
        server_url: String,
        store_id: String,
        signing_key: SecretKey,
    ) -> Result<Self, io::Error> {
        let auth_provider = SigsAuthProvider::new(signing_key, HashMap::new());

        let retry_policy = ExponentialBackoffRetryPolicy::new(Duration::from_millis(100))
            .with_max_attempts(3)
            .with_max_total_delay(Duration::from_secs(5));

        let client = VssClient::new_with_headers(server_url, retry_policy, Arc::new(auth_provider));

        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .thread_name("vss-runtime")
            .enable_all()
            .build()
            .map_err(|e| {
                io::Error::new(
                    io::ErrorKind::Other,
                    format!("Failed to create VSS tokio runtime: {e}"),
                )
            })?;

        Ok(Self {
            client,
            store_id,
            versions: Mutex::new(HashMap::new()),
            runtime,
        })
    }

    /// Returns the store_id used by this VssKvStore.
    pub fn store_id(&self) -> &str {
        &self.store_id
    }

    /// Runs an async future on the dedicated VSS runtime, handling the case
    /// where we may already be inside a tokio context.
    fn block_on<F>(&self, future: F) -> F::Output
    where
        F: std::future::Future + Send,
        F::Output: Send,
    {
        if tokio::runtime::Handle::try_current().is_ok() {
            // Already inside a tokio runtime — run on a separate thread
            std::thread::scope(|s| {
                s.spawn(|| self.runtime.block_on(future))
                    .join()
                    .expect("VSS thread panicked")
            })
        } else {
            self.runtime.block_on(future)
        }
    }

    /// Gets the cached version for a key, defaulting to 0 (first write).
    fn get_cached_version(&self, vss_key: &str) -> i64 {
        self.versions
            .lock()
            .unwrap()
            .get(vss_key)
            .copied()
            .unwrap_or(0)
    }

    /// Updates the cached version for a key after a successful write.
    fn update_cached_version(&self, vss_key: &str, version: i64) {
        self.versions
            .lock()
            .unwrap()
            .insert(vss_key.to_string(), version);
    }

    /// Removes the cached version for a key after deletion.
    fn remove_cached_version(&self, vss_key: &str) {
        self.versions.lock().unwrap().remove(vss_key);
    }

    /// Downloads all key-value pairs from VSS for this store_id.
    /// Used for restore operations.
    pub fn download_all(&self) -> Result<Vec<(String, Vec<u8>)>, io::Error> {
        let mut all_items = Vec::new();
        let mut page_token: Option<String> = None;

        loop {
            let request = ListKeyVersionsRequest {
                store_id: self.store_id.clone(),
                key_prefix: None,
                page_size: None,
                page_token: page_token.clone(),
            };

            let response = self
                .block_on(self.client.list_key_versions(&request))
                .map_err(|e| {
                    io::Error::new(
                        io::ErrorKind::Other,
                        format!("VSS list_key_versions failed: {e}"),
                    )
                })?;

            for kv in &response.key_versions {
                // Fetch each value individually
                let get_req = GetObjectRequest {
                    store_id: self.store_id.clone(),
                    key: kv.key.clone(),
                };
                match self.block_on(self.client.get_object(&get_req)) {
                    Ok(resp) => {
                        if let Some(value) = resp.value {
                            all_items.push((value.key, value.value));
                        }
                    }
                    Err(VssError::NoSuchKeyError(_)) => continue,
                    Err(e) => {
                        return Err(io::Error::new(
                            io::ErrorKind::Other,
                            format!("VSS get_object failed during download_all: {e}"),
                        ));
                    }
                }
            }

            let next_token: Option<String> = response.next_page_token;
            match next_token {
                Some(token) if !token.is_empty() => {
                    page_token = Some(token);
                }
                _ => break,
            }
        }

        Ok(all_items)
    }
}

/// Maps LDK's (primary_namespace, secondary_namespace, key) to a single VSS key.
///
/// Format: `{primary_ns}/{secondary_ns}/{key}` where empty namespaces become `_`.
pub(crate) fn vss_key(primary_namespace: &str, secondary_namespace: &str, key: &str) -> String {
    let primary = if primary_namespace.is_empty() {
        "_"
    } else {
        primary_namespace
    };
    let secondary = if secondary_namespace.is_empty() {
        "_"
    } else {
        secondary_namespace
    };
    format!("{primary}/{secondary}/{key}")
}

/// Parses a VSS key back into (primary_namespace, secondary_namespace, key).
///
/// Returns `None` if the key doesn't match the expected format.
pub(crate) fn parse_vss_key(vss_key: &str) -> Option<(String, String, String)> {
    let mut parts = vss_key.splitn(3, '/');
    let primary = parts.next()?;
    let secondary = parts.next()?;
    let key = parts.next()?;
    if key.is_empty() {
        return None;
    }
    let primary = if primary == "_" {
        String::new()
    } else {
        primary.to_string()
    };
    let secondary = if secondary == "_" {
        String::new()
    } else {
        secondary.to_string()
    };
    Some((primary, secondary, key.to_string()))
}

/// Returns the VSS key prefix for listing all keys in a namespace.
fn vss_key_prefix(primary_namespace: &str, secondary_namespace: &str) -> String {
    let primary = if primary_namespace.is_empty() {
        "_"
    } else {
        primary_namespace
    };
    let secondary = if secondary_namespace.is_empty() {
        "_"
    } else {
        secondary_namespace
    };
    format!("{primary}/{secondary}/")
}

impl KVStoreSync for VssKvStore {
    fn read(
        &self,
        primary_namespace: &str,
        secondary_namespace: &str,
        key: &str,
    ) -> Result<Vec<u8>, io::Error> {
        let vss_key = vss_key(primary_namespace, secondary_namespace, key);
        tracing::trace!(vss_key, "VssKvStore read");

        let request = GetObjectRequest {
            store_id: self.store_id.clone(),
            key: vss_key.clone(),
        };

        let response = self.block_on(self.client.get_object(&request));

        match response {
            Ok(resp) => {
                if let Some(kv) = resp.value {
                    self.update_cached_version(&vss_key, kv.version);
                    Ok(kv.value)
                } else {
                    Err(io::Error::new(io::ErrorKind::NotFound, "Key not found"))
                }
            }
            Err(VssError::NoSuchKeyError(_)) => {
                Err(io::Error::new(io::ErrorKind::NotFound, "Key not found"))
            }
            Err(e) => {
                tracing::error!(vss_key, error = %e, "VssKvStore read failed");
                Err(io::Error::new(
                    io::ErrorKind::Other,
                    format!("VSS read failed: {e}"),
                ))
            }
        }
    }

    fn write(
        &self,
        primary_namespace: &str,
        secondary_namespace: &str,
        key: &str,
        buf: Vec<u8>,
    ) -> Result<(), io::Error> {
        let vss_key = vss_key(primary_namespace, secondary_namespace, key);
        tracing::trace!(vss_key, value_len = buf.len(), "VssKvStore write");

        // Use non-conditional writes (version = -1) to avoid version conflicts
        // under high-frequency LDK persistence (channel monitor updates).
        let request = PutObjectRequest {
            store_id: self.store_id.clone(),
            global_version: None,
            transaction_items: vec![KeyValue {
                key: vss_key.clone(),
                version: -1,
                value: buf,
            }],
            delete_items: vec![],
        };

        self.block_on(self.client.put_object(&request))
            .map_err(|e| {
                tracing::error!(vss_key, error = %e, "VssKvStore write failed");
                io::Error::new(io::ErrorKind::Other, format!("VSS write failed: {e}"))
            })?;

        // After non-conditional write, server resets version to 1
        self.update_cached_version(&vss_key, 1);
        Ok(())
    }

    fn remove(
        &self,
        primary_namespace: &str,
        secondary_namespace: &str,
        key: &str,
        _lazy: bool,
    ) -> Result<(), io::Error> {
        let vss_key = vss_key(primary_namespace, secondary_namespace, key);
        tracing::trace!(vss_key, "VssKvStore remove");

        // Use non-conditional delete (version = -1)
        let request = PutObjectRequest {
            store_id: self.store_id.clone(),
            global_version: None,
            transaction_items: vec![],
            delete_items: vec![KeyValue {
                key: vss_key.clone(),
                version: -1,
                value: vec![],
            }],
        };

        self.block_on(self.client.put_object(&request))
            .map_err(|e| {
                tracing::error!(vss_key, error = %e, "VssKvStore remove failed");
                io::Error::new(io::ErrorKind::Other, format!("VSS remove failed: {e}"))
            })?;

        self.remove_cached_version(&vss_key);
        Ok(())
    }

    fn list(
        &self,
        primary_namespace: &str,
        secondary_namespace: &str,
    ) -> Result<Vec<String>, io::Error> {
        let prefix = vss_key_prefix(primary_namespace, secondary_namespace);
        tracing::trace!(prefix, "VssKvStore list");

        let mut keys = Vec::new();
        let mut page_token: Option<String> = None;

        loop {
            let request = ListKeyVersionsRequest {
                store_id: self.store_id.clone(),
                key_prefix: Some(prefix.clone()),
                page_size: None,
                page_token: page_token.clone(),
            };

            let response = self
                .block_on(self.client.list_key_versions(&request))
                .map_err(|e| {
                    tracing::error!(prefix, error = %e, "VssKvStore list failed");
                    io::Error::new(io::ErrorKind::Other, format!("VSS list failed: {e}"))
                })?;

            for kv in &response.key_versions {
                // Strip the prefix to get just the key name
                let kv_key: &str = &kv.key;
                if let Some(key_name) = kv_key.strip_prefix(prefix.as_str()) {
                    keys.push(key_name.to_string());
                    self.update_cached_version(kv_key, kv.version);
                }
            }

            let next_token: Option<String> = response.next_page_token;
            match next_token {
                Some(token) if !token.is_empty() => {
                    page_token = Some(token);
                }
                _ => break,
            }
        }

        Ok(keys)
    }
}
