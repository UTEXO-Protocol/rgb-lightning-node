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
