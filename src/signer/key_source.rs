use super::types::{BootstrapData, RlnSignerError};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

pub(crate) const KEY_SOURCE_FILE_NAME: &str = "key_source.json";
pub(crate) const EXTERNAL_SIGNER_MODE_V1: &str = "external_signer_v1";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct KeySourceFile {
    pub(crate) mode: String,
    pub(crate) node_id: String,
    pub(crate) account_xpub_vanilla: String,
    pub(crate) account_xpub_colored: String,
    pub(crate) master_fingerprint: String,
    pub(crate) protocol_version: String,
    pub(crate) api_level: u32,
}

impl KeySourceFile {
    pub(crate) fn from_bootstrap(bootstrap: &BootstrapData) -> Self {
        Self {
            mode: EXTERNAL_SIGNER_MODE_V1.to_string(),
            node_id: bootstrap.identity.node_id.clone(),
            account_xpub_vanilla: bootstrap.identity.account_xpub_vanilla.clone(),
            account_xpub_colored: bootstrap.identity.account_xpub_colored.clone(),
            master_fingerprint: bootstrap.identity.master_fingerprint.clone(),
            protocol_version: bootstrap.protocol_version.clone(),
            api_level: bootstrap.api_level,
        }
    }
}

pub(crate) fn key_source_path(storage_dir: &Path) -> PathBuf {
    storage_dir.join(KEY_SOURCE_FILE_NAME)
}

pub(crate) fn read_key_source_file(
    storage_dir: &Path,
) -> Result<Option<KeySourceFile>, RlnSignerError> {
    let path = key_source_path(storage_dir);
    if !path.exists() {
        return Ok(None);
    }
    let bytes = fs::read(&path)
        .map_err(|e| RlnSignerError::Protocol(format!("failed to read key source file: {e}")))?;
    let parsed = serde_json::from_slice::<KeySourceFile>(&bytes)
        .map_err(|e| RlnSignerError::Protocol(format!("failed to decode key source file: {e}")))?;
    Ok(Some(parsed))
}

#[allow(dead_code)]
pub(crate) fn write_key_source_file(
    storage_dir: &Path,
    key_source: &KeySourceFile,
) -> Result<(), RlnSignerError> {
    let path = key_source_path(storage_dir);
    let bytes = serde_json::to_vec_pretty(key_source)
        .map_err(|e| RlnSignerError::Protocol(format!("failed to encode key source file: {e}")))?;
    write_restricted_file(&path, &bytes)
        .map_err(|e| RlnSignerError::Protocol(format!("failed to write key source file: {e}")))?;
    Ok(())
}

/// Atomically create `path` with 0600 permissions (unix) and write `bytes` to it. There is no window
/// where the file exists with broader (e.g. default-umask) permissions — `mode` is applied by the
/// underlying `open(2)` call itself, not a separate `chmod` after the fact.
///
/// Returns a plain `io::Error` (rather than the crate-internal `RlnSignerError`) so this stays usable
/// from outside the crate — it's re-exported at the crate root (see `lib.rs`) for the
/// `rln-signer-daemon` binary to reuse for its seed file.
pub fn write_restricted_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .mode(0o600)
            .open(path)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        Ok(())
    }

    #[cfg(not(unix))]
    {
        fs::write(path, bytes)
    }
}

/// Verify that an existing secret file (e.g. the signer daemon's seed) is safe to trust: a regular
/// file, owned by the current user, with no group/other permission bits. [`write_restricted_file`]
/// guarantees this for files we create; this guards the path where the file already existed — a
/// seed written by a shell redirect is typically 0644 and must be refused, not silently used.
///
/// Only consumed by the `rln-signer-daemon` binary (via the crate-root re-export in `lib.rs`),
/// hence the feature gate — without it the default build flags this as dead code.
#[cfg(any(feature = "remote-signer", test))]
pub fn check_restricted_file(path: &Path) -> std::io::Result<()> {
    let metadata = fs::metadata(path)?;
    if !metadata.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("{} is not a regular file", path.display()),
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let euid = unsafe { libc::geteuid() };
        if metadata.uid() != euid {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                format!(
                    "{} is owned by uid {} but this process runs as uid {euid}",
                    path.display(),
                    metadata.uid(),
                ),
            ));
        }
        if metadata.mode() & 0o077 != 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                format!(
                    "{} is readable by group/others (mode {:o}); fix with: chmod 600 the file",
                    path.display(),
                    metadata.mode() & 0o777,
                ),
            ));
        }
    }
    Ok(())
}

/// Create `path` as an owner-only (0700) directory — including any missing parents — or, if it
/// already exists, verify it is a directory owned by the current user with no group/other
/// permission bits. Fails closed on broader permissions instead of silently tightening them: an
/// already-exposed directory may already have been read, and that is the operator's call to assess.
///
/// Only consumed by `in_process_vls::open_restricted_persister` (gated on `vls`), hence the
/// feature gate — without it the default build flags this as dead code.
#[cfg(any(feature = "vls", test))]
pub fn create_or_check_restricted_dir(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, MetadataExt};
        // `recursive(true)` is a no-op (not an error) when the directory already exists, so the
        // metadata check below always runs — there is no exists()/create window to race.
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path)?;
        let metadata = fs::metadata(path)?;
        let euid = unsafe { libc::geteuid() };
        if metadata.uid() != euid {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                format!(
                    "{} is owned by uid {} but this process runs as uid {euid}",
                    path.display(),
                    metadata.uid(),
                ),
            ));
        }
        if metadata.mode() & 0o077 != 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                format!(
                    "{} is accessible by group/others (mode {:o}); fix with: chmod 700 the directory",
                    path.display(),
                    metadata.mode() & 0o777,
                ),
            ));
        }
        Ok(())
    }

    #[cfg(not(unix))]
    fs::create_dir_all(path)
}

/// Tighten an existing file to owner-only (0600). For files created by third-party libraries (e.g.
/// the VLS `redb` store) whose creation mode follows the process umask. Missing files are fine —
/// the caller doesn't always know which of several candidate files the library created.
///
/// Only consumed by `in_process_vls::open_restricted_persister` (gated on `vls`), hence the
/// feature gate — without it the default build flags this as dead code.
#[cfg(any(feature = "vls", test))]
pub fn restrict_existing_file(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    if path.exists() {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

pub(crate) fn validate_key_source_matches_bootstrap(
    key_source: &KeySourceFile,
    bootstrap: &BootstrapData,
) -> Result<(), RlnSignerError> {
    if key_source.mode != EXTERNAL_SIGNER_MODE_V1 {
        return Err(RlnSignerError::Unsupported(format!(
            "unsupported key source mode: {}",
            key_source.mode
        )));
    }

    let expected = KeySourceFile::from_bootstrap(bootstrap);
    if *key_source == expected {
        return Ok(());
    }

    Err(RlnSignerError::Protocol(
        "external signer identity does not match persisted key_source.json".to_string(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::signer::types::SignerIdentity;

    fn sample_bootstrap() -> BootstrapData {
        BootstrapData {
            identity: SignerIdentity {
                node_id: "02aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                    .to_string(),
                account_xpub_vanilla: "xpub6CUGRUonZSQ4TWtTMmzXdrXDtypWKiKpJ7c2P4YFQki71RJKVQ1BM8DT6vKrrf5gkP7vC18JNpDutLCRa14Q6gttxyPjdvVSxGJySLjeRG".to_string(),
                account_xpub_colored: "xpub6BosfCnifzQ5xU6LhM1cBh73PvvrLpzjAU6YewsGmmKzBSSMSmc5QwDFi1Cdm42Hcps225y7sY9qsJhK8GugHgd6NqBJ38qRAjPR9U1FVL".to_string(),
                master_fingerprint: "deadbeef".to_string(),
            },
            protocol_version: "v1".to_string(),
            api_level: 1,
        }
    }

    #[test]
    fn key_source_roundtrip_read_write() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let bootstrap = sample_bootstrap();
        let key_source = KeySourceFile::from_bootstrap(&bootstrap);

        write_key_source_file(tmp.path(), &key_source).expect("write key source");
        let read_back = read_key_source_file(tmp.path())
            .expect("read key source")
            .expect("key source exists");
        assert_eq!(read_back, key_source);
    }

    #[cfg(unix)]
    #[test]
    fn key_source_written_with_restricted_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().expect("tempdir");
        let bootstrap = sample_bootstrap();
        let key_source = KeySourceFile::from_bootstrap(&bootstrap);

        write_key_source_file(tmp.path(), &key_source).expect("write key source");
        let metadata =
            fs::metadata(key_source_path(tmp.path())).expect("metadata for key source file");
        let mode = metadata.permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn read_key_source_returns_none_when_missing() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let value = read_key_source_file(tmp.path()).expect("read missing key source");
        assert!(value.is_none());
    }

    /// A secret file created by a shell redirect (default umask → typically 0644) must be refused,
    /// not silently used: group/others being able to read it means the seed may already be
    /// compromised and the operator has to decide, not the daemon.
    #[cfg(unix)]
    #[test]
    fn check_restricted_file_rejects_group_other_readable() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("seed");
        fs::write(&path, b"deadbeef").expect("write");

        for mode in [0o644, 0o640, 0o604, 0o660] {
            fs::set_permissions(&path, fs::Permissions::from_mode(mode)).expect("chmod");
            check_restricted_file(&path).expect_err("group/other-accessible file must be refused");
        }
        for mode in [0o600, 0o400] {
            fs::set_permissions(&path, fs::Permissions::from_mode(mode)).expect("chmod");
            check_restricted_file(&path).expect("owner-only file must be accepted");
        }
    }

    #[cfg(unix)]
    #[test]
    fn check_restricted_file_rejects_non_regular_files() {
        let tmp = tempfile::tempdir().expect("tempdir");
        check_restricted_file(tmp.path()).expect_err("a directory is not a regular seed file");
        check_restricted_file(&tmp.path().join("missing")).expect_err("missing file errors");
    }

    #[cfg(unix)]
    #[test]
    fn create_or_check_restricted_dir_creates_0700_and_rejects_broader() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().join("nested/signer-db");
        create_or_check_restricted_dir(&dir).expect("create");
        let mode = fs::metadata(&dir).expect("stat").permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "created directory must be owner-only");
        // Idempotent on a compliant existing directory.
        create_or_check_restricted_dir(&dir).expect("recheck");

        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).expect("chmod");
        create_or_check_restricted_dir(&dir)
            .expect_err("group/other-accessible signer state dir must be refused");
    }

    #[cfg(unix)]
    #[test]
    fn restrict_existing_file_tightens_to_0600_and_ignores_missing() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("redb");
        fs::write(&path, b"db").expect("write");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).expect("chmod");

        restrict_existing_file(&path).expect("restrict");
        let mode = fs::metadata(&path).expect("stat").permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);

        restrict_existing_file(&tmp.path().join("missing")).expect("missing file is fine");
    }

    #[test]
    fn validate_key_source_checks_mode_and_identity() {
        let bootstrap = sample_bootstrap();
        let valid = KeySourceFile::from_bootstrap(&bootstrap);
        assert!(validate_key_source_matches_bootstrap(&valid, &bootstrap).is_ok());

        let mut wrong_mode = valid.clone();
        wrong_mode.mode = "legacy".to_string();
        assert!(matches!(
            validate_key_source_matches_bootstrap(&wrong_mode, &bootstrap),
            Err(RlnSignerError::Unsupported(_))
        ));

        let mut wrong_identity = valid;
        wrong_identity.node_id =
            "03bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_string();
        assert!(matches!(
            validate_key_source_matches_bootstrap(&wrong_identity, &bootstrap),
            Err(RlnSignerError::Protocol(_))
        ));
    }
}
