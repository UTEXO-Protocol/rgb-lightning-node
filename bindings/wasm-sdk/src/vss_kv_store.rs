//! Browser-native VSS (Versioned Storage Service) KV store for LDK/RGB node state.
//!
//! This is the WASM counterpart of the native [`VssKvStore`] (see the
//! rgb-lightning-node `src/vss_kv_store.rs`). It maps LDK's
//! `(primary_namespace, secondary_namespace, key)` triple to a single VSS key and
//! encrypts every value at rest with XChaCha20-Poly1305, keyed by HKDF-SHA256 over
//! the node's `signing_key`.
//!
//! Differences from native, forced by the browser:
//!   * **Async only.** The browser VSS transport is `fetch`-based and cannot block,
//!     so there is no synchronous `KVStoreSync`-style facade here — callers `.await`.
//!   * **No `list_key_versions`.** The exposed browser client (rgb-lib-wasm's
//!     `WasmVssClient`) only does get/put/delete, so instead of server-side listing
//!     we keep an explicit **manifest** key ([`MANIFEST_KEY`]) enumerating every
//!     replicated VSS key; restore walks that.
//!   * **Domain separation.** Encryption uses a distinct HKDF info tag
//!     ([`VSS_KV_HKDF_INFO`], matching native) so the derived keys never collide
//!     with rgb-lib's wallet-backup stream, even under the same signing key and a
//!     shared VSS server. Callers should still use a distinct `store_id` for the
//!     LDK stream (e.g. `{pubkey_hex}-ldk`).
//!
//! This module provides the raw store + envelope + manifest primitives; the
//! background replication tier lives in [`crate::vss_replicator`] and the
//! fresh-load restore path in `ldk_live_backend::restore_ldk_state_from_vss`.

// Several items are only reachable from wasm32-only paths; allow dead_code so the
// native (unit-test) compilation stays warning-free.
#![allow(dead_code)]

use rgb_lib_wasm::bdk_wallet::bitcoin::secp256k1::SecretKey;
use rgb_lib_wasm::wallet::vss::{
    decrypt_data, encrypt_data, GetObjectRequest, KeyValue, PutObjectRequest,
    VssEncryptionMetadata, VssError, WasmVssClient, BACKUP_NONCE_LENGTH, BACKUP_SALT_LENGTH,
};

/// HKDF info tag used to derive this KV stream's per-value encryption key.
/// Matches the native `VssKvStore` so a value encrypted by either side round-trips,
/// and domain-separates it from rgb-lib's wallet-backup stream.
const VSS_KV_HKDF_INFO: &[u8] = b"rgb-ln-vss-kv-encryption-v1";

/// Wire-format version for inline-encrypted values stored on VSS.
const VSS_KV_FORMAT_VERSION: u8 = 1;

/// Raw salt length — rgb-lib's, so the envelope always parses with the lengths its
/// `encrypt_data`/`decrypt_data` actually use.
const SALT_LEN: usize = BACKUP_SALT_LENGTH;

/// Raw nonce length — rgb-lib's (see `SALT_LEN`).
const NONCE_LEN: usize = BACKUP_NONCE_LENGTH;

/// Total inline-envelope header: `[1-byte version][32-byte salt][19-byte nonce]`.
const HEADER_LEN: usize = 1 + SALT_LEN + NONCE_LEN;

/// VSS key holding the manifest: a JSON `Vec<String>` of every replicated VSS key.
/// Encrypted at rest with the same envelope as ordinary values. Replaces the
/// `list_key_versions` the browser client does not expose, so restore can enumerate
/// what to fetch. The manifest key itself is never listed inside the manifest.
pub(crate) const MANIFEST_KEY: &str = "__ldk_manifest__";

/// VSS key holding the single-writer fencing token: the current owner's instance id
/// (stored as plaintext UTF-8, like native — it is an id, not a secret). Anything
/// that writes to this store must own this token. Never entered into the manifest,
/// so it is naturally excluded from `download_all`.
pub(crate) const FENCE_KEY: &str = "__rln_instance__";

/// Validate and parse the VSS config surface shared by the wallet-backup
/// (`configureVssBackup`) and LDK replication (`configureLdkVssReplication`)
/// entry points.
pub(crate) fn parse_vss_config(
    server_url: &str,
    store_id: &str,
    signing_key_hex: &str,
) -> Result<SecretKey, wasm_bindgen::JsValue> {
    use wasm_bindgen::JsValue;
    if server_url.trim().is_empty() {
        return Err(JsValue::from_str(sdk_contracts::ERR_SERVER_URL_EMPTY));
    }
    if store_id.trim().is_empty() {
        return Err(JsValue::from_str(sdk_contracts::ERR_STORE_ID_EMPTY));
    }
    if signing_key_hex.len() != 64 {
        return Err(JsValue::from_str(&format!(
            "signing_key_hex must be exactly 64 hex chars (32 bytes), got {}",
            signing_key_hex.len()
        )));
    }
    let key_bytes = hex::decode(signing_key_hex)
        .map_err(|e| JsValue::from_str(&format!("Invalid signing key hex: {e}")))?;
    SecretKey::from_slice(&key_bytes)
        .map_err(|e| JsValue::from_str(&format!("Invalid signing key: {e}")))
}

/// Async KV store backed by a remote VSS server, reachable from the browser.
pub(crate) struct WasmVssKvStore {
    client: WasmVssClient,
    store_id: String,
    signing_key: SecretKey,
}

impl WasmVssKvStore {
    /// Creates a store targeting `server_url` for keyspace `store_id`. `signing_key`
    /// is used both for VSS sigs-auth (inside `WasmVssClient`) and for deriving the
    /// per-value encryption key.
    pub(crate) fn new(server_url: String, store_id: String, signing_key: SecretKey) -> Self {
        Self {
            client: WasmVssClient::new(server_url, signing_key),
            store_id,
            signing_key,
        }
    }

    /// Encrypt a value into the inline wire format `[version|salt|nonce|ciphertext]`.
    fn encrypt_value(&self, plaintext: &[u8]) -> Result<Vec<u8>, String> {
        let mut salt = [0u8; SALT_LEN];
        let mut nonce = [0u8; NONCE_LEN];
        getrandom::getrandom(&mut salt).map_err(|e| format!("VSS RNG unavailable: {e}"))?;
        getrandom::getrandom(&mut nonce).map_err(|e| format!("VSS RNG unavailable: {e}"))?;
        let metadata = VssEncryptionMetadata {
            salt: hex::encode(salt),
            nonce: hex::encode(nonce),
            version: VSS_KV_FORMAT_VERSION,
        };
        let ciphertext = encrypt_data(
            plaintext,
            &self.signing_key,
            &metadata,
            Some(VSS_KV_HKDF_INFO),
        )
        .map_err(|e| format!("VSS encrypt failed: {e}"))?;
        let mut out = Vec::with_capacity(HEADER_LEN + ciphertext.len());
        out.push(VSS_KV_FORMAT_VERSION);
        out.extend_from_slice(&salt);
        out.extend_from_slice(&nonce);
        out.extend_from_slice(&ciphertext);
        Ok(out)
    }

    /// Decrypt a value retrieved from VSS.
    fn decrypt_value(&self, stored: &[u8]) -> Result<Vec<u8>, String> {
        if stored.len() < HEADER_LEN {
            return Err("VSS decrypt: stored value shorter than header".to_string());
        }
        let version = stored[0];
        if version != VSS_KV_FORMAT_VERSION {
            return Err(format!(
                "VSS decrypt: unknown wire format version {version}"
            ));
        }
        let salt = &stored[1..1 + SALT_LEN];
        let nonce = &stored[1 + SALT_LEN..HEADER_LEN];
        let ciphertext = &stored[HEADER_LEN..];
        let metadata = VssEncryptionMetadata {
            salt: hex::encode(salt),
            nonce: hex::encode(nonce),
            version,
        };
        decrypt_data(
            ciphertext,
            &self.signing_key,
            &metadata,
            Some(VSS_KV_HKDF_INFO),
        )
        .map_err(|e| format!("VSS decrypt failed: {e}"))
    }

    /// Read and decrypt a single key. `Ok(None)` means the key is absent.
    pub(crate) async fn get(
        &self,
        primary_namespace: &str,
        secondary_namespace: &str,
        key: &str,
    ) -> Result<Option<Vec<u8>>, String> {
        let vkey = vss_key(primary_namespace, secondary_namespace, key);
        match self
            .client
            .get_object(&GetObjectRequest {
                store_id: self.store_id.clone(),
                key: vkey,
            })
            .await
        {
            Ok(resp) => match resp.value {
                Some(kv) => Ok(Some(self.decrypt_value(&kv.value)?)),
                None => Ok(None),
            },
            Err(VssError::NoSuchKey(_)) => Ok(None),
            Err(e) => Err(format!("VSS get failed: {e:?}")),
        }
    }

    /// Encrypt and write a single key. Uses a non-conditional put (`version = -1`)
    /// so high-frequency writes don't fail on version conflicts — concurrent-writer
    /// safety is the job of the fence (a later phase), not VSS optimistic locking.
    pub(crate) async fn put(
        &self,
        primary_namespace: &str,
        secondary_namespace: &str,
        key: &str,
        buf: &[u8],
    ) -> Result<(), String> {
        let vkey = vss_key(primary_namespace, secondary_namespace, key);
        let stored = self.encrypt_value(buf)?;
        self.client
            .put_object(&PutObjectRequest {
                store_id: self.store_id.clone(),
                global_version: None,
                transaction_items: vec![KeyValue {
                    key: vkey,
                    version: -1,
                    value: stored,
                }],
                delete_items: vec![],
            })
            .await
            .map_err(|e| format!("VSS put failed: {e:?}"))?;
        Ok(())
    }

    /// Delete a single key. VSS honors deletes only against the object's current
    /// version, so we read it first; an absent key means the goal is already met.
    pub(crate) async fn remove(
        &self,
        primary_namespace: &str,
        secondary_namespace: &str,
        key: &str,
    ) -> Result<(), String> {
        let vkey = vss_key(primary_namespace, secondary_namespace, key);
        let version = match self
            .client
            .get_object(&GetObjectRequest {
                store_id: self.store_id.clone(),
                key: vkey.clone(),
            })
            .await
        {
            Ok(resp) => match resp.value {
                Some(kv) => kv.version,
                None => return Ok(()),
            },
            Err(VssError::NoSuchKey(_)) => return Ok(()),
            Err(e) => return Err(format!("VSS remove read failed: {e:?}")),
        };
        match self
            .client
            .remove_object(&self.store_id, &vkey, version)
            .await
        {
            Ok(_) | Err(VssError::NoSuchKey(_)) => Ok(()),
            Err(e) => Err(format!("VSS remove failed: {e:?}")),
        }
    }

    /// Acquire the single-writer fence for this store (cross-device guard). Reads the
    /// fence key; if a *different* instance owns it, refuses. Otherwise conditionally
    /// creates it at `version = 0`; a conflict means a concurrent instance won the
    /// race. Idempotent for the same `instance_id`. Mirrors native `acquire_fence`.
    pub(crate) async fn acquire_fence(&self, instance_id: &str) -> Result<(), String> {
        match self
            .client
            .get_object(&GetObjectRequest {
                store_id: self.store_id.clone(),
                key: FENCE_KEY.to_string(),
            })
            .await
        {
            Ok(resp) => {
                if let Some(kv) = resp.value {
                    let remote = String::from_utf8_lossy(&kv.value);
                    if remote == instance_id {
                        return Ok(());
                    }
                    return Err(format!(
                        "VSS store is owned by another rgb-lightning-node instance ({remote}); \
                         refusing to replicate to avoid concurrent-writer corruption. If that \
                         owner is gone (wiped browser profile, dead device), call \
                         clearLdkVssFence and retry to take over."
                    ));
                }
            }
            Err(VssError::NoSuchKey(_)) => {}
            Err(e) => return Err(format!("VSS fence read failed: {e:?}")),
        }
        match self
            .client
            .put_object(&PutObjectRequest {
                store_id: self.store_id.clone(),
                global_version: None,
                transaction_items: vec![KeyValue {
                    key: FENCE_KEY.to_string(),
                    version: 0,
                    value: instance_id.as_bytes().to_vec(),
                }],
                delete_items: vec![],
            })
            .await
        {
            Ok(_) => Ok(()),
            Err(VssError::Conflict(_)) => {
                Err("VSS fence: another instance won the race to claim this store".to_string())
            }
            Err(e) => Err(format!("VSS fence acquire failed: {e:?}")),
        }
    }

    /// Re-check the fence mid-session: errors if another instance has taken it over
    /// (at which point our writes would corrupt their state). A missing fence is
    /// treated as still-ours (benign; an operator may have cleared it).
    pub(crate) async fn check_fence(&self, instance_id: &str) -> Result<(), String> {
        match self
            .client
            .get_object(&GetObjectRequest {
                store_id: self.store_id.clone(),
                key: FENCE_KEY.to_string(),
            })
            .await
        {
            Ok(resp) => {
                if let Some(kv) = resp.value {
                    let remote = String::from_utf8_lossy(&kv.value);
                    if remote != instance_id {
                        return Err(format!(
                            "VSS fence taken over by another instance ({remote})"
                        ));
                    }
                }
                Ok(())
            }
            Err(VssError::NoSuchKey(_)) => Ok(()),
            Err(e) => Err(format!("VSS fence check failed: {e:?}")),
        }
    }

    /// Release the fence only if this instance still owns it, so a takeover by
    /// another instance is never clobbered. Idempotent. Mirrors native
    /// `release_fence_if_owned`.
    pub(crate) async fn release_fence_if_owned(&self, instance_id: &str) -> Result<(), String> {
        self.remove_fence(Some(instance_id)).await
    }

    /// Remove the fence key *regardless of owner* — the WASM counterpart of the
    /// native SDK's `delete_fence` (`POST /vssclearfence`). Needed when the previous
    /// owner can never release it itself: a wiped browser profile or a dead device
    /// loses its persisted instance id, so every new instance fails `acquire_fence`
    /// with "owned by another instance" until the stale fence is cleared. Idempotent:
    /// a missing key is not an error.
    pub(crate) async fn delete_fence(&self) -> Result<(), String> {
        self.remove_fence(None).await
    }

    /// Shared fence-removal: with `only_if_owner = Some(id)` the fence is left in
    /// place unless `id` owns it; with `None` it is removed unconditionally.
    async fn remove_fence(&self, only_if_owner: Option<&str>) -> Result<(), String> {
        let existing = match self
            .client
            .get_object(&GetObjectRequest {
                store_id: self.store_id.clone(),
                key: FENCE_KEY.to_string(),
            })
            .await
        {
            Ok(resp) => match resp.value {
                Some(kv) => kv,
                None => return Ok(()),
            },
            Err(VssError::NoSuchKey(_)) => return Ok(()),
            Err(e) => return Err(format!("VSS fence read failed: {e:?}")),
        };
        if let Some(owner) = only_if_owner {
            if String::from_utf8_lossy(&existing.value) != owner {
                return Ok(());
            }
        }
        match self
            .client
            .remove_object(&self.store_id, FENCE_KEY, existing.version)
            .await
        {
            Ok(_) | Err(VssError::NoSuchKey(_)) => Ok(()),
            Err(e) => Err(format!("VSS fence release failed: {e:?}")),
        }
    }

    /// Read and decrypt the manifest (empty vec if none exists yet).
    pub(crate) async fn read_manifest(&self) -> Result<Vec<String>, String> {
        match self
            .client
            .get_object(&GetObjectRequest {
                store_id: self.store_id.clone(),
                key: MANIFEST_KEY.to_string(),
            })
            .await
        {
            Ok(resp) => match resp.value {
                Some(kv) => {
                    let plain = self.decrypt_value(&kv.value)?;
                    serde_json::from_slice(&plain)
                        .map_err(|e| format!("VSS manifest decode failed: {e}"))
                }
                None => Ok(Vec::new()),
            },
            Err(VssError::NoSuchKey(_)) => Ok(Vec::new()),
            Err(e) => Err(format!("VSS manifest read failed: {e:?}")),
        }
    }

    /// Encrypt and overwrite the manifest with `keys`.
    pub(crate) async fn write_manifest(&self, keys: &[String]) -> Result<(), String> {
        let raw =
            serde_json::to_vec(keys).map_err(|e| format!("VSS manifest encode failed: {e}"))?;
        let stored = self.encrypt_value(&raw)?;
        self.client
            .put_object(&PutObjectRequest {
                store_id: self.store_id.clone(),
                global_version: None,
                transaction_items: vec![KeyValue {
                    key: MANIFEST_KEY.to_string(),
                    version: -1,
                    value: stored,
                }],
                delete_items: vec![],
            })
            .await
            .map_err(|e| format!("VSS manifest write failed: {e:?}"))?;
        Ok(())
    }

    /// Restore helper: read the manifest, then fetch and decrypt every tracked key.
    /// Returns `(vss_key, plaintext)` pairs; keys that vanished between manifest read
    /// and fetch (benign race) are skipped.
    pub(crate) async fn download_all(&self) -> Result<Vec<(String, Vec<u8>)>, String> {
        let manifest = self.read_manifest().await?;
        let mut out = Vec::with_capacity(manifest.len());
        for vkey in manifest {
            if vkey == MANIFEST_KEY {
                continue;
            }
            match self
                .client
                .get_object(&GetObjectRequest {
                    store_id: self.store_id.clone(),
                    key: vkey.clone(),
                })
                .await
            {
                Ok(resp) => {
                    if let Some(kv) = resp.value {
                        out.push((vkey, self.decrypt_value(&kv.value)?));
                    }
                }
                Err(VssError::NoSuchKey(_)) => {}
                Err(e) => return Err(format!("VSS download_all get failed for {vkey}: {e:?}")),
            }
        }
        Ok(out)
    }
}

/// Encode a `(primary_namespace, secondary_namespace, key)` triple as a single VSS
/// key string. Format `{primary}/{secondary}/{key}`, empty namespaces become `_`.
/// Identical to the native `VssKvStore` encoding.
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

/// Parse a VSS key back into `(primary_namespace, secondary_namespace, key)`.
/// Returns `None` if it doesn't match the expected format.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vss_key_roundtrips_through_parse() {
        let cases = [
            ("mon", "chan", "abc"),
            ("", "", "channel_manager"),
            ("p", "", "k"),
            ("", "s", "k"),
        ];
        for (p, s, k) in cases {
            let encoded = vss_key(p, s, k);
            let (rp, rs, rk) = parse_vss_key(&encoded).expect("should parse");
            assert_eq!((rp.as_str(), rs.as_str(), rk.as_str()), (p, s, k));
        }
    }

    #[test]
    fn empty_namespaces_map_to_underscore_sentinel() {
        assert_eq!(vss_key("", "", "k"), "_/_/k");
        assert_eq!(vss_key("a", "b", "k"), "a/b/k");
    }

    #[test]
    fn parse_rejects_malformed_keys() {
        assert!(parse_vss_key("only-one-part").is_none());
        assert!(parse_vss_key("two/parts").is_none());
        assert!(parse_vss_key("a/b/").is_none()); // empty key
                                                  // A third slash is part of the key itself (splitn(3)).
        assert_eq!(
            parse_vss_key("a/b/c/d"),
            Some(("a".to_string(), "b".to_string(), "c/d".to_string()))
        );
    }

    #[cfg(target_arch = "wasm32")]
    fn test_store() -> WasmVssKvStore {
        let sk = SecretKey::from_slice(&[7u8; 32]).expect("valid test key");
        // No network I/O happens at construction, so a dummy URL is fine here.
        WasmVssKvStore::new(
            "http://localhost:0/vss".to_string(),
            "test-store".to_string(),
            sk,
        )
    }

    /// The inline `[version|salt|nonce|ciphertext]` envelope must round-trip and be
    /// ciphertext at rest. Runs under wasm (needs the JS RNG + rgb-lib-wasm crypto).
    #[cfg(target_arch = "wasm32")]
    #[wasm_bindgen_test::wasm_bindgen_test]
    fn envelope_encrypts_then_decrypts_roundtrip() {
        let store = test_store();
        let plaintext = b"channel-manager snapshot bytes".to_vec();
        let stored = store.encrypt_value(&plaintext).expect("encrypt");
        assert_ne!(stored, plaintext, "value must be encrypted at rest");
        assert_eq!(stored[0], VSS_KV_FORMAT_VERSION);
        assert!(stored.len() >= HEADER_LEN);
        let restored = store.decrypt_value(&stored).expect("decrypt");
        assert_eq!(restored, plaintext);
    }

    /// Fresh random salt+nonce per call, so encrypting the same plaintext twice
    /// yields different ciphertext (no deterministic leakage).
    #[cfg(target_arch = "wasm32")]
    #[wasm_bindgen_test::wasm_bindgen_test]
    fn envelope_uses_fresh_salt_nonce_each_time() {
        let store = test_store();
        let a = store.encrypt_value(b"same-plaintext").unwrap();
        let b = store.encrypt_value(b"same-plaintext").unwrap();
        assert_ne!(a, b);
    }

    /// A truncated or unknown-version envelope is rejected before any crypto runs.
    #[cfg(target_arch = "wasm32")]
    #[wasm_bindgen_test::wasm_bindgen_test]
    fn decrypt_rejects_short_or_unknown_version() {
        let store = test_store();
        assert!(store.decrypt_value(&[0u8; HEADER_LEN - 1]).is_err());
        let mut buf = vec![0u8; HEADER_LEN + 8];
        buf[0] = VSS_KV_FORMAT_VERSION.wrapping_add(1);
        assert!(store.decrypt_value(&buf).is_err());
    }
}
