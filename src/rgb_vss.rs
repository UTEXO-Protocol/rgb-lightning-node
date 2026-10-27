//! VSS (Versioned Storage Service) backup module
//!
//! This module provides cloud backup functionality using VSS servers.

use std::collections::HashMap;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use bitcoin::secp256k1::SecretKey;
use hex_bytes as hex;
// Note: this is the RustCrypto crate (XChaCha20). The similarly named
// `chacha20-poly1305` (rust-bitcoin, stateless-only) is a transitive dep
// of vss-client-ng via bitreq — they are not interchangeable.
use chacha20poly1305::{aead::Aead, Key, KeyInit, XChaCha20Poly1305};
use hkdf::Hkdf;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use vss_client::client::VssClient;
use vss_client::error::VssError;
use vss_client::headers::sigs_auth::SigsAuthProvider;
use vss_client::types::{GetObjectRequest, KeyValue, PutObjectRequest};
use vss_client::util::retry::{
    ExponentialBackoffRetryPolicy, MaxAttemptsRetryPolicy, MaxTotalDelayRetryPolicy, RetryPolicy,
};
use zip::write::SimpleFileOptions;

// Transport/crypto format retained from UTEXO-Protocol/rgb-lib@1b951af0c8233308c919975552487be19ad2a930.
// Original MIT license applies. Wallet serialization and dirty acknowledgement are new-library APIs.
const MAX_RAW_SNAPSHOT_BYTES: usize = 128 * 1024 * 1024;
// STREAM encrypts each full 239-byte block plus an authenticated final block (even when empty).
const MAX_BACKUP_BYTES: usize = MAX_RAW_SNAPSHOT_BYTES + (MAX_RAW_SNAPSHOT_BYTES / 239 + 1) * 16;
const MAX_RESTORE_BYTES: u64 = 512 * 1024 * 1024;
const MAX_RESTORE_ENTRIES: usize = 25_000;
const MAX_MANIFEST_BYTES: usize = 64 * 1024;

#[cfg(test)]
#[path = "rgb_vss_tests.rs"]
mod transport_tests;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("VSS: {details}")]
    Internal { details: String },
    #[error("VSS: {details}")]
    VssError { details: String },
    #[error("VSS authentication: {details}")]
    VssAuth { details: String },
    #[error("VSS version conflict: {details}")]
    VssVersionConflict { details: String },
    #[error("VSS backup not found")]
    VssBackupNotFound,
    #[error("Wallet directory already exists: {path}")]
    WalletDirAlreadyExists { path: String },
    #[error(transparent)]
    Io(#[from] std::io::Error),
}
impl From<rgb_lib::Error> for Error {
    fn from(error: rgb_lib::Error) -> Self {
        Self::Internal {
            details: error.to_string(),
        }
    }
}
fn stream_be32_nonce(prefix: &[u8; 19], position: u32, last: bool) -> chacha20poly1305::XNonce {
    let mut nonce = [0u8; 24];
    nonce[..19].copy_from_slice(prefix);
    nonce[19..23].copy_from_slice(&position.to_be_bytes());
    nonce[23] = u8::from(last);
    nonce.into()
}

/// Whether auto-backup uploads block the calling operation or run asynchronously.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VssBackupMode {
    /// Upload runs on a spawned tokio task (fire-and-forget). Default.
    #[default]
    Async,
    /// Upload blocks the calling operation until the backup is persisted.
    Blocking,
}

/// Type alias for our retry policy
type VssRetryPolicy =
    MaxTotalDelayRetryPolicy<MaxAttemptsRetryPolicy<ExponentialBackoffRetryPolicy<VssError>>>;

// Encryption constants (matching backup.rs)
const BACKUP_BUFFER_LEN_ENCRYPT: usize = 239;
const BACKUP_BUFFER_LEN_DECRYPT: usize = BACKUP_BUFFER_LEN_ENCRYPT + 16;
const BACKUP_KEY_LENGTH: usize = 32;
const BACKUP_NONCE_LENGTH: usize = 19;
const VSS_BACKUP_VERSION: u8 = 1;
const VSS_MANIFEST_VERSION: u8 = 2;

/// Default chunk size for large backups
/// Kept under the upstream vss-client-ng 10-second HTTP timeout
/// so that each chunk PUT completes reliably
pub(crate) const VSS_CHUNK_SIZE: usize = 1024 * 1024; // 1MB

/// Key prefix for backup data
const BACKUP_KEY_DATA: &str = "backup/data";
/// Key prefix for backup metadata (encryption params)
const BACKUP_KEY_METADATA: &str = "backup/metadata";
/// Key prefix for backup manifest (chunk info)
const BACKUP_KEY_MANIFEST: &str = "backup/manifest";
/// Key prefix for backup chunks
const BACKUP_KEY_CHUNK_PREFIX: &str = "backup/chunk/";
/// Key for storing wallet fingerprint separately
const BACKUP_KEY_FINGERPRINT: &str = "backup/fingerprint";
/// Generic directory name used in sanitized (plaintext) backups
const SANITIZED_DIR_NAME: &str = "wallet";
/// Marker dropped in a restored wallet dir until its first successful
/// consistency check, so a failure there can be attributed to the restore.
pub(crate) const VSS_RESTORE_MARKER: &str = ".vss_restored";
/// BDK database filename
const BDK_DB_NAME: &str = "bdk_db";
/// BDK watch-only database filename
const BDK_DB_WO_NAME: &str = "bdk_db_watch_only";

/// Salt length for HKDF key derivation (32 bytes, hex-encoded = 64 chars)
const BACKUP_SALT_LENGTH: usize = 32;

/// Encryption metadata stored alongside encrypted backups
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct VssEncryptionMetadata {
    /// Salt used for HKDF key derivation (hex encoded)
    pub salt: String,
    /// Nonce used for encryption
    pub nonce: String,
    /// Version of the encryption format
    pub version: u8,
}

impl Default for VssEncryptionMetadata {
    fn default() -> Self {
        Self::new()
    }
}

impl VssEncryptionMetadata {
    /// Create new encryption metadata with random salt and nonce
    pub fn new() -> Self {
        let salt: [u8; BACKUP_SALT_LENGTH] = rand::random();
        let nonce: [u8; BACKUP_NONCE_LENGTH] = rand::random();

        Self {
            salt: hex::encode(salt),
            nonce: hex::encode(nonce),
            version: VSS_BACKUP_VERSION,
        }
    }

    fn nonce_bytes(&self) -> Result<[u8; BACKUP_NONCE_LENGTH], Error> {
        if self.version != VSS_BACKUP_VERSION {
            return Err(Error::Internal {
                details: "Unsupported VSS encryption version".into(),
            });
        }
        let bytes = hex::decode(&self.nonce).map_err(|e| Error::Internal {
            details: format!("Invalid nonce hex: {e}"),
        })?;
        bytes.as_slice().try_into().map_err(|_| Error::Internal {
            details: "Invalid nonce length".to_string(),
        })
    }
}

/// Configuration for VSS backup service
#[derive(Clone)]
pub struct VssBackupConfig {
    /// VSS server URL
    pub(crate) server_url: String,
    /// Store ID (namespace for this wallet's data)
    pub(crate) store_id: String,
    /// Private key for signing requests and deriving encryption key
    pub(crate) signing_key: SecretKey,
    /// Whether to encrypt data before uploading (default: true)
    pub(crate) encryption_enabled: bool,
    /// Whether to automatically back up after state-changing operations (default: false)
    pub(crate) auto_backup: bool,
    /// Whether auto-backup uploads block the caller or run asynchronously (default: Async)
    pub(crate) backup_mode: VssBackupMode,
}

impl VssBackupConfig {
    /// Create a new VSS backup configuration
    ///
    /// Encryption is enabled by default. The encryption key is derived from the
    /// signing key using HKDF-SHA256, so no separate password is needed.
    pub fn new(server_url: String, store_id: String, signing_key: SecretKey) -> Self {
        Self {
            server_url,
            store_id,
            signing_key,
            encryption_enabled: true,
            auto_backup: false,
            backup_mode: VssBackupMode::default(),
        }
    }

    /// Set encryption enabled/disabled
    pub fn with_encryption(mut self, enabled: bool) -> Self {
        self.encryption_enabled = enabled;
        self
    }

    /// Enable or disable automatic backups after state-changing operations
    pub fn with_auto_backup(mut self, enabled: bool) -> Self {
        self.auto_backup = enabled;
        self
    }

    /// Set the auto-backup mode (Async or Blocking)
    pub fn with_backup_mode(mut self, mode: VssBackupMode) -> Self {
        self.backup_mode = mode;
        self
    }
}

/// Backup manifest for tracking chunked backups
/// A committed generation references immutable payload keys and its own crypto metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BackupManifest {
    pub chunk_count: usize,
    pub total_size: usize,
    pub encrypted: bool,
    pub version: u8,
    pub chunk_prefix: String,
    pub ciphertext_sha256: String,
    pub encryption_metadata: VssEncryptionMetadata,
    pub fingerprint: String,
}

/// VSS backup client wrapper
pub struct VssBackupClient {
    client: VssClient<VssRetryPolicy>,
    store_id: String,
    encryption_enabled: bool,
    signing_key: SecretKey,
    auto_backup: std::sync::atomic::AtomicBool,
    backup_mode: VssBackupMode,
    runtime: Option<tokio::runtime::Runtime>,
    last_auto_backup_error: std::sync::Mutex<Option<String>>,
    upload_lock: tokio::sync::Mutex<()>,
}

impl Drop for VssBackupClient {
    fn drop(&mut self) {
        if let Some(rt) = self.runtime.take() {
            rt.shutdown_background();
        }
    }
}

impl VssBackupClient {
    /// Create a new VSS backup client
    pub fn new(config: VssBackupConfig) -> Result<Self, Error> {
        if !config.encryption_enabled {
            return Err(Error::Internal {
                details: "Plaintext VSS cannot protect the new RGB database".into(),
            });
        }
        let auth_provider = SigsAuthProvider::new(config.signing_key, HashMap::new());

        let retry_policy = ExponentialBackoffRetryPolicy::new(Duration::from_millis(100))
            .with_max_attempts(3)
            .with_max_total_delay(Duration::from_secs(5));

        let client =
            VssClient::new_with_headers(config.server_url, retry_policy, Arc::new(auth_provider));

        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .map_err(|e| Error::Internal {
                details: format!("Failed to create tokio runtime: {e}"),
            })?;

        Ok(Self {
            client,
            store_id: config.store_id,
            encryption_enabled: config.encryption_enabled,
            signing_key: config.signing_key,
            auto_backup: std::sync::atomic::AtomicBool::new(config.auto_backup),
            backup_mode: config.backup_mode,
            runtime: Some(runtime),
            last_auto_backup_error: std::sync::Mutex::new(None),
            upload_lock: tokio::sync::Mutex::new(()),
        })
    }

    /// Error message of the most recent failed auto-backup, cleared on the
    /// next successful one. `None` when the last auto-backup succeeded or none
    /// ran yet.
    pub fn last_auto_backup_error(&self) -> Option<String> {
        self.last_auto_backup_error.lock().unwrap().clone()
    }

    pub(crate) fn record_auto_backup_result(&self, error: Option<String>) {
        *self.last_auto_backup_error.lock().unwrap() = error;
    }

    /// Get a handle to the client's tokio runtime
    pub fn handle(&self) -> &tokio::runtime::Handle {
        self.runtime
            .as_ref()
            .expect("runtime not available")
            .handle()
    }

    /// Upload backup data to VSS server
    ///
    /// If encryption is enabled, data will be encrypted before upload.
    /// The encryption key is derived from the signing key using HKDF-SHA256.
    ///
    /// Returns the version number of the uploaded backup.
    pub async fn upload_backup(&self, data: Vec<u8>) -> Result<i64, Error> {
        let _admission = self.upload_lock.lock().await;
        if data.len() > MAX_RAW_SNAPSHOT_BYTES {
            return Err(Error::Internal {
                details: "VSS snapshot exceeds the size limit".into(),
            });
        }
        let fingerprint = get_fingerprint_from_zip_bytes(&data)?;
        // Migration snapshots belong to a separate fresh store. Validate the existing
        // canonical manifest before any write, so an upload cannot destroy a legacy
        // backup merely because this client would refuse to restore its format.
        let before = match self.get_manifest_with_version().await {
            Ok((previous, version)) => {
                if previous.fingerprint != fingerprint {
                    return Err(Error::Internal {
                        details: "Existing VSS backup belongs to another wallet fingerprint".into(),
                    });
                }
                version
            }
            Err(Error::VssBackupNotFound) => {
                // Absence of a manifest does not authorize replacing recovery material
                // left by an incomplete legacy backup. UUID generation chunks from an
                // interrupted v2 upload are independent and may remain for a fresh retry.
                for key in [BACKUP_KEY_DATA, BACKUP_KEY_METADATA, BACKUP_KEY_FINGERPRINT] {
                    if self.get_current_version(key).await?.is_some() {
                        return Err(Error::Internal {
                            details: "VSS store has backup material without a valid manifest; preserve it and use a separate fresh test wallet/store".into(),
                        });
                    }
                }
                0
            }
            Err(error) => return Err(error),
        };
        let metadata = VssEncryptionMetadata::new();
        let ciphertext = encrypt_data(&data, &self.signing_key, &metadata, None)?;
        if ciphertext.len() > MAX_BACKUP_BYTES {
            return Err(Error::Internal {
                details: "Encrypted VSS snapshot exceeds the size limit".into(),
            });
        }
        // Capture CAS before uploading chunks. Interrupted generations never overwrite old keys.
        let successor = before.checked_add(1).ok_or_else(|| Error::Internal {
            details: "VSS manifest version overflow".into(),
        })?;
        let prefix = format!(
            "{}{}{}",
            BACKUP_KEY_CHUNK_PREFIX,
            uuid::Uuid::new_v4().simple(),
            "/"
        );
        let manifest = BackupManifest {
            chunk_count: ciphertext.len().div_ceil(VSS_CHUNK_SIZE),
            total_size: ciphertext.len(),
            encrypted: true,
            version: VSS_MANIFEST_VERSION,
            chunk_prefix: prefix,
            ciphertext_sha256: hex::encode(Sha256::digest(&ciphertext)),
            encryption_metadata: metadata.clone(),
            fingerprint: fingerprint.clone(),
        };
        for (index, chunk) in ciphertext.chunks(VSS_CHUNK_SIZE).enumerate() {
            self.client
                .put_object(&PutObjectRequest {
                    store_id: self.store_id.clone(),
                    global_version: None,
                    transaction_items: vec![KeyValue {
                        key: format!("{}{}", manifest.chunk_prefix, index),
                        version: 0,
                        value: chunk.to_vec(),
                    }],
                    delete_items: vec![],
                })
                .await
                .map_err(vss_error_to_rgb_error)?;
        }
        let manifest_json = serde_json::to_vec(&manifest).map_err(|e| Error::Internal {
            details: format!("Cannot encode VSS manifest: {e}"),
        })?;
        let fingerprint_version = self
            .get_current_version(BACKUP_KEY_FINGERPRINT)
            .await?
            .unwrap_or(0);
        let metadata_version = self
            .get_current_version(BACKUP_KEY_METADATA)
            .await?
            .unwrap_or(0);
        let mut items = vec![
            KeyValue {
                key: BACKUP_KEY_MANIFEST.into(),
                version: before,
                value: manifest_json.clone(),
            },
            KeyValue {
                key: BACKUP_KEY_FINGERPRINT.into(),
                version: fingerprint_version,
                value: fingerprint.into_bytes(),
            },
            KeyValue {
                key: BACKUP_KEY_METADATA.into(),
                version: metadata_version,
                value: serde_json::to_vec(&metadata).map_err(|e| Error::Internal {
                    details: e.to_string(),
                })?,
            },
        ];
        // Diagnostic latest-small-payload alias; recovery always uses immutable generation keys.
        if ciphertext.len() <= VSS_CHUNK_SIZE {
            items.push(KeyValue {
                key: BACKUP_KEY_DATA.into(),
                version: self
                    .get_current_version(BACKUP_KEY_DATA)
                    .await?
                    .unwrap_or(0),
                value: ciphertext,
            });
        }
        self.client
            .put_object(&PutObjectRequest {
                store_id: self.store_id.clone(),
                global_version: None,
                transaction_items: items,
                delete_items: vec![],
            })
            .await
            .map_err(vss_error_to_rgb_error)?;
        let committed = self
            .client
            .get_object(&GetObjectRequest {
                store_id: self.store_id.clone(),
                key: BACKUP_KEY_MANIFEST.into(),
            })
            .await
            .map_err(vss_error_to_rgb_error)?
            .value
            .ok_or(Error::VssBackupNotFound)?;
        if committed.version != successor || committed.value != manifest_json {
            return Err(Error::VssVersionConflict {
                details: "Canonical VSS manifest does not match the uploaded generation".into(),
            });
        }
        Ok(successor)
    }

    /// Download backup data from VSS server
    ///
    /// If the backup was encrypted, it will be decrypted using a key derived from
    /// the signing key via HKDF-SHA256.
    pub async fn download_backup(&self) -> Result<Vec<u8>, Error> {
        self.download_snapshot().await.map(|(data, _)| data)
    }

    async fn download_snapshot(&self) -> Result<(Vec<u8>, String), Error> {
        let manifest = self.get_manifest().await?;
        let mut encrypted = Vec::with_capacity(manifest.total_size);
        for index in 0..manifest.chunk_count {
            let chunk = self
                .client
                .get_object(&GetObjectRequest {
                    store_id: self.store_id.clone(),
                    key: format!("{}{}", manifest.chunk_prefix, index),
                })
                .await
                .map_err(vss_error_to_rgb_error)?
                .value
                .ok_or(Error::VssBackupNotFound)?
                .value;
            if chunk.is_empty()
                || chunk.len() > VSS_CHUNK_SIZE
                || encrypted.len().saturating_add(chunk.len()) > manifest.total_size
            {
                return Err(Error::Internal {
                    details: "VSS chunk exceeds manifest bounds".into(),
                });
            }
            encrypted.extend(chunk);
        }
        if encrypted.len() != manifest.total_size
            || hex::encode(Sha256::digest(&encrypted)) != manifest.ciphertext_sha256
        {
            return Err(Error::Internal {
                details: "VSS snapshot length or ciphertext hash mismatch".into(),
            });
        }
        let cleartext = decrypt_data(
            &encrypted,
            &self.signing_key,
            &manifest.encryption_metadata,
            None,
        )?;
        if cleartext.len() > MAX_RAW_SNAPSHOT_BYTES {
            return Err(Error::Internal {
                details: "VSS snapshot exceeds raw size limit".into(),
            });
        }
        Ok((cleartext, manifest.fingerprint))
    }

    /// Get the backup manifest
    async fn get_manifest(&self) -> Result<BackupManifest, Error> {
        self.get_manifest_with_version()
            .await
            .map(|(manifest, _)| manifest)
    }

    /// Keep the validated payload and its CAS identity from the same read.
    async fn get_manifest_with_version(&self) -> Result<(BackupManifest, i64), Error> {
        let request = GetObjectRequest {
            store_id: self.store_id.clone(),
            key: BACKUP_KEY_MANIFEST.to_string(),
        };

        let response = self
            .client
            .get_object(&request)
            .await
            .map_err(vss_error_to_rgb_error)?;

        let object = response.value.ok_or(Error::VssBackupNotFound)?;
        let manifest_version = object.version;
        let manifest_bytes = object.value;

        if manifest_bytes.len() > MAX_MANIFEST_BYTES {
            return Err(Error::Internal {
                details: "VSS manifest exceeds limit".into(),
            });
        }
        let value: serde_json::Value =
            serde_json::from_slice(&manifest_bytes).map_err(|e| Error::Internal {
                details: format!("Invalid VSS manifest: {e}"),
            })?;
        if value.get("version").and_then(|v| v.as_u64()) != Some(u64::from(VSS_MANIFEST_VERSION)) {
            return Err(Error::Internal { details: "Legacy VSS backup format is not supported by this RGB migration; use a separate fresh test wallet/store and preserve the previous backup".into() });
        }
        let manifest: BackupManifest =
            serde_json::from_value(value).map_err(|e| Error::Internal {
                details: format!("Invalid VSS manifest: {e}"),
            })?;
        let generation = manifest
            .chunk_prefix
            .strip_prefix(BACKUP_KEY_CHUNK_PREFIX)
            .and_then(|value| value.strip_suffix('/'));
        if manifest_version < 1
            || !manifest.encrypted
            || manifest.total_size == 0
            || manifest.total_size > MAX_BACKUP_BYTES
            || manifest.chunk_count != manifest.total_size.div_ceil(VSS_CHUNK_SIZE)
            || !generation.is_some_and(|value| {
                value.len() == 32
                    && value
                        .bytes()
                        .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
            })
            || manifest.ciphertext_sha256.len() != 64
            || !manifest
                .ciphertext_sha256
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
            || manifest.fingerprint.len() != 8
            || !manifest
                .fingerprint
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        {
            return Err(Error::Internal {
                details: "Invalid VSS manifest identity, generation or bounds".into(),
            });
        }
        // A structurally valid v2 manifest with unusable encryption parameters is
        // still a backup that must not be overwritten by migration admission.
        manifest.encryption_metadata.nonce_bytes()?;
        if manifest.encryption_metadata.salt.len() != BACKUP_SALT_LENGTH * 2
            || !manifest
                .encryption_metadata
                .salt
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(Error::Internal {
                details: "Invalid VSS manifest encryption salt".into(),
            });
        }
        Ok((manifest, manifest_version))
    }

    /// Get the encryption metadata
    async fn get_encryption_metadata(&self) -> Result<VssEncryptionMetadata, Error> {
        let request = GetObjectRequest {
            store_id: self.store_id.clone(),
            key: BACKUP_KEY_METADATA.to_string(),
        };

        let response = self
            .client
            .get_object(&request)
            .await
            .map_err(vss_error_to_rgb_error)?;

        let metadata_bytes = response
            .value
            .map(|kv| kv.value)
            .ok_or(Error::VssBackupNotFound)?;

        if metadata_bytes.len() > 4096 {
            return Err(Error::Internal {
                details: "VSS encryption metadata exceeds limit".into(),
            });
        }
        serde_json::from_slice(&metadata_bytes).map_err(|e| Error::Internal {
            details: format!("Failed to parse encryption metadata: {e}"),
        })
    }

    /// Get the current version of a backup
    ///
    /// Returns `None` if no backup exists, otherwise returns the version number.
    pub async fn get_backup_version(&self) -> Result<Option<i64>, Error> {
        self.get_current_version(BACKUP_KEY_MANIFEST).await
    }

    /// Get the wallet fingerprint stored on the server
    async fn get_fingerprint(&self) -> Result<String, Error> {
        let request = GetObjectRequest {
            store_id: self.store_id.clone(),
            key: BACKUP_KEY_FINGERPRINT.to_string(),
        };

        let response = self
            .client
            .get_object(&request)
            .await
            .map_err(vss_error_to_rgb_error)?;

        let fingerprint_bytes = response
            .value
            .map(|kv| kv.value)
            .ok_or(Error::VssBackupNotFound)?;

        String::from_utf8(fingerprint_bytes).map_err(|e| Error::Internal {
            details: format!("Invalid fingerprint encoding: {e}"),
        })
    }

    /// Get the current version of a key
    async fn get_current_version(&self, key: &str) -> Result<Option<i64>, Error> {
        let request = GetObjectRequest {
            store_id: self.store_id.clone(),
            key: key.to_string(),
        };

        match self.client.get_object(&request).await {
            Ok(response) => Ok(response.value.map(|kv| kv.version)),
            Err(VssError::NoSuchKeyError(_)) => Ok(None),
            Err(e) => Err(vss_error_to_rgb_error(e)),
        }
    }

    /// Delete the backup from VSS server
    pub async fn delete_backup(&self) -> Result<(), Error> {
        // Get manifest to know what to delete
        let (manifest, manifest_version) = match self.get_manifest_with_version().await {
            Ok(value) => value,
            Err(Error::VssBackupNotFound) => return Ok(()),
            Err(error) => return Err(error),
        };

        let mut delete_items = vec![];

        // Every v2 backup has generation chunks, including a one-chunk backup. Historical and
        // orphan generations deliberately remain outside this current-generation operation.
        for i in 0..manifest.chunk_count {
            let key = format!("{}{}", manifest.chunk_prefix, i);
            if let Some(version) = self.get_current_version(&key).await? {
                delete_items.push(KeyValue {
                    key,
                    version,
                    value: vec![],
                });
            }
        }

        // This exact version makes the entire delete fail if another backup commits meanwhile.
        delete_items.push(KeyValue {
            key: BACKUP_KEY_MANIFEST.to_string(),
            version: manifest_version,
            value: vec![],
        });

        // Remove the small-payload diagnostic alias in the same atomic transaction.
        if let Some(version) = self.get_current_version(BACKUP_KEY_DATA).await? {
            delete_items.push(KeyValue {
                key: BACKUP_KEY_DATA.to_string(),
                version,
                value: vec![],
            });
        }

        // Delete metadata if exists
        if let Some(version) = self.get_current_version(BACKUP_KEY_METADATA).await? {
            delete_items.push(KeyValue {
                key: BACKUP_KEY_METADATA.to_string(),
                version,
                value: vec![],
            });
        }

        // Delete fingerprint if exists
        if let Some(version) = self.get_current_version(BACKUP_KEY_FINGERPRINT).await? {
            delete_items.push(KeyValue {
                key: BACKUP_KEY_FINGERPRINT.to_string(),
                version,
                value: vec![],
            });
        }

        if delete_items.is_empty() {
            return Ok(());
        }

        let request = PutObjectRequest {
            store_id: self.store_id.clone(),
            global_version: None,
            transaction_items: vec![],
            delete_items,
        };

        self.client
            .put_object(&request)
            .await
            .map_err(vss_error_to_rgb_error)?;

        Ok(())
    }

    /// Check if encryption is enabled
    #[must_use]
    pub fn encryption_enabled(&self) -> bool {
        self.encryption_enabled
    }

    /// Check if auto-backup is enabled
    #[must_use]
    pub fn auto_backup(&self) -> bool {
        self.auto_backup.load(std::sync::atomic::Ordering::Acquire)
    }

    /// Policy callers persist intent and complete catch-up before enabling this flag.
    pub fn set_auto_backup(&self, enabled: bool) {
        self.auto_backup
            .store(enabled, std::sync::atomic::Ordering::Release);
    }

    /// Get the configured backup mode
    #[must_use]
    pub fn backup_mode(&self) -> VssBackupMode {
        self.backup_mode
    }
}

/// Default HKDF info string used by rgb-lib's wallet backup encryption.
///
/// Downstream callers that re-use [`derive_encryption_key`] / [`encrypt_data`] /
/// [`decrypt_data`] for a different purpose (e.g. encrypting LDK KVStore
/// values rather than wallet zips) should pass their own domain-separation tag
/// via the `info` parameter so that the derived keys differ even when the
/// signing key and salt happen to match.
const HKDF_INFO: &[u8] = b"rgb-lib-vss-backup-encryption-v1";

/// Derive an encryption key from a signing key using HKDF-SHA256.
///
/// When `info` is `None`, rgb-lib's default domain-separation tag
/// (`b"rgb-lib-vss-backup-encryption-v1"`) is used — that's the right choice
/// when the caller is encrypting data that will round-trip through rgb-lib's
/// own backup helpers. Pass `Some(tag)` to supply a custom HKDF info string
/// when encrypting unrelated data (e.g. LDK channel-state values) so that the
/// derived key is distinct from rgb-lib's wallet-backup key for the same
/// signing key and metadata.
pub fn derive_encryption_key(
    signing_key: &SecretKey,
    metadata: &VssEncryptionMetadata,
    info: Option<&[u8]>,
) -> Result<Key, Error> {
    let salt_bytes = hex::decode(&metadata.salt).map_err(|e| Error::Internal {
        details: format!("Invalid salt hex: {e}"),
    })?;

    if salt_bytes.len() != BACKUP_SALT_LENGTH {
        return Err(Error::Internal {
            details: "Invalid VSS salt length".into(),
        });
    }
    let hk = Hkdf::<Sha256>::new(Some(&salt_bytes), &signing_key.secret_bytes());

    let mut key_bytes = [0u8; BACKUP_KEY_LENGTH];
    hk.expand(info.unwrap_or(HKDF_INFO), &mut key_bytes)
        .map_err(|e| Error::Internal {
            details: format!("HKDF expansion failed: {e}"),
        })?;

    Ok(Key::from(key_bytes))
}

/// Encrypt data using XChaCha20-Poly1305 with a key derived from `signing_key`
/// via HKDF-SHA256 (see [`derive_encryption_key`] for the `info` parameter).
///
/// The same `signing_key`, `metadata`, and `info` must be passed to
/// [`decrypt_data`] for the round-trip to succeed.
pub fn encrypt_data(
    data: &[u8],
    signing_key: &SecretKey,
    metadata: &VssEncryptionMetadata,
    info: Option<&[u8]>,
) -> Result<Vec<u8>, Error> {
    let key = derive_encryption_key(signing_key, metadata, info)?;
    let aead = XChaCha20Poly1305::new(&key);
    let nonce_prefix = metadata.nonce_bytes()?;
    let mut position: u32 = 0;
    let mut encrypted = Vec::new();
    let mut buffer = [0u8; BACKUP_BUFFER_LEN_ENCRYPT];
    let mut reader = std::io::Cursor::new(data);

    loop {
        let read_count = reader.read(&mut buffer).map_err(|e| Error::Internal {
            details: format!("Failed to read data: {e}"),
        })?;
        let is_last = read_count != BACKUP_BUFFER_LEN_ENCRYPT;
        if !is_last && position == u32::MAX {
            return Err(Error::Internal {
                details: "data too large".to_string(),
            });
        }

        let nonce = stream_be32_nonce(&nonce_prefix, position, is_last);
        let ciphertext =
            aead.encrypt(&nonce, &buffer[..read_count])
                .map_err(|e| Error::Internal {
                    details: format!("Encryption error: {e}"),
                })?;
        encrypted.extend(ciphertext);

        if is_last {
            break;
        }
        position += 1;
    }

    Ok(encrypted)
}

/// Decrypt data using XChaCha20-Poly1305 with a key derived from `signing_key`
/// via HKDF-SHA256 (see [`derive_encryption_key`] for the `info` parameter).
///
/// The `signing_key`, `metadata`, and `info` must match what was passed to
/// [`encrypt_data`]; otherwise the AEAD authentication tag check fails and an
/// `Error::VssError` is returned.
pub fn decrypt_data(
    encrypted: &[u8],
    signing_key: &SecretKey,
    metadata: &VssEncryptionMetadata,
    info: Option<&[u8]>,
) -> Result<Vec<u8>, Error> {
    let key = derive_encryption_key(signing_key, metadata, info)?;
    let aead = XChaCha20Poly1305::new(&key);
    let nonce_prefix = metadata.nonce_bytes()?;
    let mut position: u32 = 0;
    let mut decrypted = Vec::new();
    let mut buffer = [0u8; BACKUP_BUFFER_LEN_DECRYPT];
    let mut reader = std::io::Cursor::new(encrypted);

    loop {
        let read_count = reader.read(&mut buffer).map_err(|e| Error::Internal {
            details: format!("Failed to read data: {e}"),
        })?;

        if read_count == BACKUP_BUFFER_LEN_DECRYPT {
            if position == u32::MAX {
                return Err(Error::Internal {
                    details: "data too large".to_string(),
                });
            }
            let nonce = stream_be32_nonce(&nonce_prefix, position, false);
            let cleartext =
                aead.decrypt(&nonce, buffer.as_slice())
                    .map_err(|_| Error::VssError {
                        details: "decryption failed: wrong signing key or corrupted data"
                            .to_string(),
                    })?;
            decrypted.extend(cleartext);
        } else if read_count == 0 {
            return Err(Error::VssError {
                details: "truncated VSS encrypted stream".into(),
            });
        } else {
            let nonce = stream_be32_nonce(&nonce_prefix, position, true);
            let cleartext =
                aead.decrypt(&nonce, &buffer[..read_count])
                    .map_err(|_| Error::VssError {
                        details: "decryption failed: wrong signing key or corrupted data"
                            .to_string(),
                    })?;
            decrypted.extend(cleartext);
            break;
        }
        position += 1;
    }

    Ok(decrypted)
}

/// Convert VSS error to RGB-lib error
fn vss_error_to_rgb_error(e: VssError) -> Error {
    match e {
        VssError::NoSuchKeyError(_) => Error::VssBackupNotFound,
        VssError::ConflictError(msg) => Error::VssVersionConflict { details: msg },
        VssError::AuthError(msg) => Error::VssAuth { details: msg },
        VssError::InternalServerError(msg) => Error::VssError {
            details: format!("Server error: {msg}"),
        },
        VssError::InvalidRequestError(msg) => Error::VssError {
            details: format!("Invalid request: {msg}"),
        },
        _ => Error::VssError {
            details: e.to_string(),
        },
    }
}

/// VSS backup info returned by `vss_backup_info()`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VssBackupInfo {
    /// Whether a backup exists on the server
    pub backup_exists: bool,
    /// Server-side version of the backup
    pub server_version: Option<i64>,
    /// Whether the local wallet has changes since last backup
    pub backup_required: bool,
    /// Error of the most recent failed auto-backup, if the last one failed
    pub last_auto_backup_error: Option<String>,
}

/// Extract the wallet fingerprint from a zip archive
///
/// Wallet fingerprints are 4-byte BIP32 fingerprints displayed as 8 lowercase
/// hex characters (e.g., `"a1b2c3d4"`).
fn get_fingerprint_from_zip_bytes(data: &[u8]) -> Result<String, Error> {
    let reader = std::io::Cursor::new(data);
    let mut archive = zip::ZipArchive::new(reader).map_err(|e| Error::Internal {
        details: format!("Failed to read zip archive: {e}"),
    })?;

    let entry = archive.by_index(0).map_err(|_| Error::Internal {
        details: "VSS snapshot has no readable first entry".into(),
    })?;
    // Native snapshots use UTF-8 paths. ZIP's legacy CP437 fallback must not
    // reinterpret corrupted authenticated metadata as a different wallet path.
    let first_entry = std::str::from_utf8(entry.name_raw()).map_err(|_| Error::Internal {
        details: "Invalid VSS snapshot entry name".into(),
    })?;
    let fingerprint = first_entry
        .split('/')
        .next()
        .unwrap_or_default()
        .to_string();

    // Validate: wallet fingerprints are 8 hex characters (4 bytes)
    if fingerprint.len() != 8 || hex::decode(&fingerprint).is_err() {
        return Err(Error::Internal {
            details: format!("Invalid wallet fingerprint in zip: '{fingerprint}'"),
        });
    }

    Ok(fingerprint)
}

/// Prepare a consistent sensitive snapshot, commit it remotely, then acknowledge that exact content.
///
/// The caller owns the wallet/session admission mutex. This helper never locks it recursively and
/// runs the transport on its dedicated runtime even when the caller is on an async runtime thread.
pub fn backup_wallet(
    wallet: &mut rgb_lib::wallet::Wallet,
    client: &VssBackupClient,
) -> Result<i64, Error> {
    use rgb_lib::wallet::RgbWalletOpsOffline;
    let snapshot = wallet.prepare_snapshot()?;
    let data = snapshot.data;
    let version = std::thread::scope(|scope| {
        scope
            .spawn(|| client.handle().block_on(client.upload_backup(data)))
            .join()
    })
    .map_err(|_| Error::Internal {
        details: "VSS upload worker failed".into(),
    })??;
    if !wallet.acknowledge_snapshot(&snapshot.token)? {
        return Err(Error::Internal {
            details: "Wallet changed after its uploaded snapshot; backup remains pending".into(),
        });
    }
    Ok(version)
}

/// Restore a wallet from VSS backup
///
/// Downloads the backup from the VSS server and extracts it to the target directory.
/// For encrypted backups, the fingerprint is embedded in the zip entry paths.
/// Plaintext archives are rejected because the database contains operation keys.
///
/// Returns the path to the restored wallet directory.
pub async fn restore_from_vss(
    config: VssBackupConfig,
    target_dir: &str,
    expected_fingerprint: &str,
) -> Result<PathBuf, Error> {
    fs::create_dir_all(target_dir)?;
    let client = VssBackupClient::new(config)?;
    let (backup_data, fingerprint) = client.download_snapshot().await?;
    if fingerprint != expected_fingerprint {
        return Err(Error::Internal {
            details: "VSS backup fingerprint does not match this wallet".into(),
        });
    }
    let target = PathBuf::from(target_dir);
    tokio::task::spawn_blocking(move || restore_snapshot_bytes(&backup_data, &target, &fingerprint))
        .await
        .map_err(|_| Error::Internal {
            details: "VSS restore worker failed".into(),
        })?
}

/// Admit a bounded authenticated archive into an isolated staging directory, then rename once.
/// The caller has already authenticated/decrypted the cloud stream; this validates filesystem data.
pub(crate) fn restore_snapshot_bytes(
    data: &[u8],
    target: &Path,
    fingerprint: &str,
) -> Result<PathBuf, Error> {
    fn invalid(message: &str) -> Error {
        Error::Internal {
            details: message.into(),
        }
    }
    if fingerprint.len() != 8
        || !fingerprint
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        || data.len() > MAX_RAW_SNAPSHOT_BYTES
    {
        return Err(invalid("Invalid VSS restore identity or size"));
    }
    let destination = target.join(fingerprint);
    if destination.exists() {
        return Err(Error::WalletDirAlreadyExists {
            path: destination.to_string_lossy().into(),
        });
    }
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(data))
        .map_err(|_| invalid("Invalid VSS snapshot archive"))?;
    if archive.len() == 0 || archive.len() > MAX_RESTORE_ENTRIES {
        return Err(invalid("Invalid VSS archive entry count"));
    }
    let staging = tempfile::tempdir_in(target)?;
    let mut names = std::collections::HashSet::new();
    let mut total = 0u64;
    for index in 0..archive.len() {
        let entry = archive
            .by_index(index)
            .map_err(|_| invalid("Invalid VSS archive entry"))?;
        let name = std::str::from_utf8(entry.name_raw())
            .map_err(|_| invalid("Invalid VSS archive entry name"))?
            .to_string();
        let path = entry
            .enclosed_name()
            .ok_or_else(|| invalid("VSS archive path escapes wallet"))?;
        if name.contains('\\')
            || name.split('/').any(|part| part == "." || part == "..")
            || !names.insert(name.clone())
            || path
                .components()
                .next()
                .and_then(|c| c.as_os_str().to_str())
                != Some(fingerprint)
        {
            return Err(invalid(
                "VSS archive has duplicate or mismatched wallet paths",
            ));
        }
        let mode = entry
            .unix_mode()
            .unwrap_or(if entry.is_dir() { 0o040700 } else { 0o100600 });
        if mode & 0o170000 != 0
            && mode & 0o170000 != if entry.is_dir() { 0o040000 } else { 0o100000 }
        {
            return Err(invalid("VSS archive contains a non-regular entry"));
        }
        total = total
            .checked_add(entry.size())
            .ok_or_else(|| invalid("VSS archive size overflow"))?;
        if total > MAX_RESTORE_BYTES
            || entry.size() > entry.compressed_size().max(1).saturating_mul(2000)
        {
            return Err(invalid("VSS archive expansion exceeds limit"));
        }
        let output = staging.path().join(&path);
        if entry.is_dir() {
            fs::create_dir_all(&output)?;
            continue;
        }
        if let Some(parent) = output.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&output)?;
        let expected = entry.size();
        let copied = std::io::copy(&mut entry.take(expected.saturating_add(1)), &mut file)?;
        if copied != expected {
            return Err(invalid("VSS archive entry length mismatch"));
        }
        file.sync_all()?;
    }
    let staged_wallet = staging.path().join(fingerprint);
    if !staged_wallet.join("wallet_manifest.json").is_file()
        || !staged_wallet.join("rgb_lib_db").is_file()
    {
        return Err(invalid(
            "VSS snapshot is missing the wallet manifest or database",
        ));
    }
    let marker = fs::File::create(staged_wallet.join(VSS_RESTORE_MARKER))?;
    marker.sync_all()?;
    #[cfg(unix)]
    fs::File::open(&staged_wallet)?.sync_all()?;
    promote_wallet_without_replacement(&staged_wallet, &destination)?;
    #[cfg(unix)]
    fs::File::open(target)?.sync_all()?;
    Ok(destination)
}

/// The existence check during archive admission is advisory. Promotion itself must reject a
/// destination created while the archive was being checked, including an empty directory.
fn promote_wallet_without_replacement(source: &Path, destination: &Path) -> std::io::Result<()> {
    #[cfg(any(target_vendor = "apple", target_os = "linux", target_os = "android"))]
    {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;
        let source = CString::new(source.as_os_str().as_bytes())?;
        let destination = CString::new(destination.as_os_str().as_bytes())?;
        // Both paths belong to the same staging filesystem. No fallback to replacing rename is
        // allowed when the kernel or filesystem does not support exclusive directory promotion.
        #[cfg(target_vendor = "apple")]
        let result = unsafe {
            libc::renameatx_np(
                libc::AT_FDCWD,
                source.as_ptr(),
                libc::AT_FDCWD,
                destination.as_ptr(),
                libc::RENAME_EXCL,
            )
        };
        #[cfg(any(target_os = "linux", target_os = "android"))]
        let result = unsafe {
            // Android exposes the libc renameat2 symbol only from API 30. Use the kernel
            // syscall so the API-29 native build retains exclusive promotion without an
            // unavailable dynamic symbol or a replacing-rename fallback.
            libc::syscall(
                libc::SYS_renameat2,
                libc::AT_FDCWD,
                source.as_ptr(),
                libc::AT_FDCWD,
                destination.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        };
        if result == 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error())
        }
    }
    #[cfg(windows)]
    {
        // MoveFileExW used by std rejects an existing directory destination on Windows.
        fs::rename(source, destination)
    }
    #[cfg(not(any(
        target_vendor = "apple",
        target_os = "linux",
        target_os = "android",
        windows
    )))]
    {
        let _ = (source, destination);
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "Atomic exclusive wallet promotion is unavailable on this platform",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn archive(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        for (name, bytes) in entries {
            writer
                .start_file(*name, SimpleFileOptions::default())
                .unwrap();
            writer.write_all(bytes).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }

    #[test]
    fn encrypted_stream_round_trip_and_wrong_key_rejection() {
        let key = SecretKey::from_slice(&[1; 32]).unwrap();
        let wrong_key = SecretKey::from_slice(&[2; 32]).unwrap();
        for count in [0, 1, 238, 239, 240, 478, 4096] {
            let plain = vec![42; count];
            let metadata = VssEncryptionMetadata::new();
            let cipher = encrypt_data(&plain, &key, &metadata, None).unwrap();
            assert_eq!(decrypt_data(&cipher, &key, &metadata, None).unwrap(), plain);
            assert!(decrypt_data(&cipher, &wrong_key, &metadata, None).is_err());
            let mut corrupted = cipher.clone();
            corrupted[0] ^= 1;
            assert!(decrypt_data(&corrupted, &key, &metadata, None).is_err());
            assert!(decrypt_data(&cipher[..cipher.len() - 1], &key, &metadata, None).is_err());
            if count == 239 {
                assert!(decrypt_data(&cipher[..255], &key, &metadata, None).is_err());
            }
        }
    }

    #[test]
    fn malformed_encryption_metadata_returns_error_without_panicking() {
        let key = SecretKey::from_slice(&[1; 32]).unwrap();
        let mut metadata = VssEncryptionMetadata::new();
        metadata.nonce = "00".into();
        assert!(encrypt_data(b"wallet", &key, &metadata, None).is_err());
        metadata = VssEncryptionMetadata::new();
        metadata.salt = "00".into();
        assert!(encrypt_data(b"wallet", &key, &metadata, None).is_err());
        metadata = VssEncryptionMetadata::new();
        metadata.version = 2;
        assert!(encrypt_data(b"wallet", &key, &metadata, None).is_err());
    }

    #[test]
    fn restore_is_staged_and_rejects_wrong_wallet_and_path_escape() {
        let root = tempfile::tempdir().unwrap();
        for name in [
            "other123/rgb_lib_db",
            "aabbccdd/../outside",
            "aabbccdd\\rgb_lib_db",
        ] {
            let data = archive(&[(name, b"db")]);
            assert!(restore_snapshot_bytes(&data, root.path(), "aabbccdd").is_err());
            assert_eq!(fs::read_dir(root.path()).unwrap().count(), 0);
        }
        let valid = archive(&[
            ("aabbccdd/rgb_lib_db", b"db"),
            ("aabbccdd/wallet_manifest.json", b"{}"),
        ]);
        let path = restore_snapshot_bytes(&valid, root.path(), "aabbccdd").unwrap();
        assert_eq!(fs::read(path.join("rgb_lib_db")).unwrap(), b"db");
        assert!(path.join(VSS_RESTORE_MARKER).is_file());
        assert!(restore_snapshot_bytes(&valid, root.path(), "aabbccdd").is_err());
        assert_eq!(fs::read(path.join("rgb_lib_db")).unwrap(), b"db");
    }

    #[test]
    fn malformed_archive_names_fail_before_wallet_installation() {
        let root = tempfile::tempdir().unwrap();
        let empty = archive(&[]);
        assert!(get_fingerprint_from_zip_bytes(&empty).is_err());
        assert!(restore_snapshot_bytes(&empty, root.path(), "aabbccdd").is_err());

        let name = b"aabbccdd/rgb_lib_db";
        let mut invalid = archive(&[
            ("aabbccdd/rgb_lib_db", b"db"),
            ("aabbccdd/wallet_manifest.json", b"{}"),
        ]);
        let offsets = invalid
            .windows(name.len())
            .enumerate()
            .filter_map(|(offset, bytes)| (bytes == name).then_some(offset))
            .collect::<Vec<_>>();
        // Keep local and central-directory names consistent, but make their encoding invalid.
        assert_eq!(offsets.len(), 2);
        for offset in offsets {
            invalid[offset + name.len() - 1] = 0xff;
        }
        assert!(get_fingerprint_from_zip_bytes(&invalid).is_err());
        assert!(restore_snapshot_bytes(&invalid, root.path(), "aabbccdd").is_err());
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 0);
    }

    #[test]
    fn restore_promotion_cannot_replace_an_empty_or_concurrently_created_wallet() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        let destination = root.path().join("wallet");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("state"), b"incoming").unwrap();
        fs::create_dir(&destination).unwrap();
        assert!(promote_wallet_without_replacement(&source, &destination).is_err());
        assert_eq!(fs::read_dir(&destination).unwrap().count(), 0);
        assert_eq!(fs::read(source.join("state")).unwrap(), b"incoming");
        fs::remove_dir(&destination).unwrap();

        let competing = root.path().join("competing");
        fs::create_dir(&competing).unwrap();
        fs::write(competing.join("state"), b"competing").unwrap();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let results = std::thread::scope(|scope| {
            let a = scope.spawn(|| {
                barrier.wait();
                promote_wallet_without_replacement(&source, &destination)
            });
            let b = scope.spawn(|| {
                barrier.wait();
                promote_wallet_without_replacement(&competing, &destination)
            });
            (a.join().unwrap(), b.join().unwrap())
        });
        assert_ne!(results.0.is_ok(), results.1.is_ok());
        let winner = fs::read(destination.join("state")).unwrap();
        assert!(winner == b"incoming" || winner == b"competing");
        assert!(source.exists() || competing.exists());
    }

    #[test]
    fn plaintext_vss_is_rejected_before_a_client_is_created() {
        let key = SecretKey::from_slice(&[1; 32]).unwrap();
        assert!(VssBackupClient::new(
            VssBackupConfig::new("https://unused".into(), "test".into(), key)
                .with_encryption(false)
        )
        .is_err());
    }
}
