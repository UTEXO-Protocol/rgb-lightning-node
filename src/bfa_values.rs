//! Validations shared by the local native BFA receive and burn paths.

pub(crate) fn burn_reason(
    recipient: Option<&[u8]>,
    is_bfa: bool,
) -> Result<Option<String>, String> {
    use std::fmt::Write;
    match recipient {
        Some(bytes) if bytes.len() == 32 => {
            let mut hex = String::with_capacity(64);
            for byte in bytes {
                write!(&mut hex, "{byte:02x}").unwrap();
            }
            Ok(Some(hex))
        }
        Some(_) => Err("burn destination must be exactly 32 bytes".into()),
        None if is_bfa => Err("BFA burn requires a payout destination".into()),
        None => Ok(None),
    }
}

// Used by the library SDK; the standalone binary only uses the other validators.
#[allow(dead_code)]
pub(crate) fn invoice_expiration(duration: Option<u32>, now: u64) -> Result<u64, String> {
    let duration = duration.unwrap_or(86400);
    if duration == 0 || duration > 2_592_000 {
        return Err("RGB invoice duration must be between 1 and 2592000 seconds".into());
    }
    now.checked_add(u64::from(duration))
        .ok_or_else(|| "RGB invoice expiration overflow".into())
}

pub(crate) fn parse_evm_chain_id(
    value: Option<&str>,
    has_error: bool,
) -> Result<u64, &'static str> {
    let value = value.and_then(|value| value.strip_prefix("0x"));
    if has_error {
        return Err("EVM RPC returned an error");
    }
    let value = value
        .filter(|value| !value.is_empty() && value.bytes().all(|b| b.is_ascii_hexdigit()))
        .and_then(|value| u64::from_str_radix(value, 16).ok())
        .filter(|value| *value > 0);
    value.ok_or("invalid eth_chainId response")
}

// Used by the library NodeHandle before it opens wallet storage.
#[allow(dead_code)]
pub(crate) fn check_fresh_storage(path: &std::path::Path) -> Result<(), String> {
    const MARKER: &str = "local-bfa-rgb-lib-3f571ac-v1";
    let marker = path.join(".bfa-local-format");
    if marker.exists() {
        if std::fs::read_to_string(&marker).map_err(|_| "Cannot read BFA wallet format marker")?
            == MARKER
        {
            return Ok(());
        }
        return Err("Wallet belongs to another native BFA format".into());
    }
    if path.exists()
        && std::fs::read_dir(path)
            .map_err(|_| "Cannot inspect wallet directory")?
            .next()
            .is_some()
    {
        return Err("This local BFA build requires a fresh wallet directory; existing wallet data was left untouched".into());
    }
    std::fs::create_dir_all(path).map_err(|_| "Cannot create fresh BFA wallet directory")?;
    std::fs::write(marker, MARKER).map_err(|_| "Cannot write BFA wallet format marker")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn payout_bytes_are_preserved_exactly_including_leading_zeroes() {
        let mut bytes = [0u8; 32];
        bytes[12..].copy_from_slice(&[
            0x02, 0x58, 0xc3, 0x12, 0x91, 0x02, 0xef, 0xbc, 0x67, 0x2f, 0xa5, 0xe4, 0x37, 0x7f,
            0x02, 0x48, 0x69, 0x5a, 0x37, 0x5a,
        ]);
        assert_eq!(
            burn_reason(Some(&bytes), true).unwrap().unwrap(),
            "0000000000000000000000000258c3129102efbc672fa5e4377f0248695a375a"
        );
        assert!(burn_reason(None, true).is_err());
        for n in [0, 20, 31, 33] {
            assert!(burn_reason(Some(&vec![1; n]), true).is_err());
        }
        assert_eq!(burn_reason(None, false).unwrap(), None);
    }
    #[test]
    fn rgb_expiration_defaults_to_24_hours_and_preserves_valid_ttl() {
        assert_eq!(invoice_expiration(None, 100).unwrap(), 86500);
        for ttl in [1, 90, 86400, 2592000] {
            assert_eq!(
                invoice_expiration(Some(ttl), 100).unwrap(),
                100 + u64::from(ttl)
            );
        }
        for ttl in [0, 2592001, u32::MAX] {
            assert!(invoice_expiration(Some(ttl), 100).is_err());
        }
        assert!(invoice_expiration(None, u64::MAX).is_err());
    }
    #[test]
    fn evm_chain_id_must_be_valid_and_is_not_assumed_to_be_mainnet() {
        for (value, id) in [("0xa4b1", 42161), ("0x1", 1), ("0xaa36a7", 11155111)] {
            assert_eq!(parse_evm_chain_id(Some(value), false).unwrap(), id);
        }
        for value in [
            None,
            Some("0x0"),
            Some("42161"),
            Some("0x+1"),
            Some("0x10000000000000000"),
        ] {
            assert!(parse_evm_chain_id(value, false).is_err());
        }
        assert!(parse_evm_chain_id(Some("0x1"), true).is_err());
    }
    #[test]
    fn existing_wallet_directory_is_rejected_without_changing_its_contents() {
        let path = std::env::temp_dir().join(format!(
            "bfa-storage-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&path).unwrap();
        let old = path.join("existing-wallet.db");
        std::fs::write(&old, b"keep exactly").unwrap();
        assert!(check_fresh_storage(&path).is_err());
        assert_eq!(std::fs::read(&old).unwrap(), b"keep exactly");
        assert!(!path.join(".bfa-local-format").exists());
        std::fs::remove_file(old).unwrap();
        check_fresh_storage(&path).unwrap();
        std::fs::write(path.join("new-wallet.db"), b"new data").unwrap();
        check_fresh_storage(&path).unwrap();
        std::fs::remove_dir_all(path).unwrap();
    }
}
