//! The wire framing shared by the node-side transport ([`super::DaemonEnvelopeTransport`]) and the
//! daemon ([`super::daemon`]): a `u32` big-endian length prefix, then that many bytes of RLN signer
//! envelope (`crate::signer::proto`). A zero-length reply frame is the daemon's handler-error
//! sentinel. Both sides enforce [`MAX_FRAME_LEN`] — defining the prefix width, endianness, sentinel,
//! and size limit in one place is what keeps the two ends of the protocol from drifting (they used
//! to be independent copies, and only the daemon side had the size limit).

use std::io::{Read, Write};

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Max envelope frame either side will read or write (defensive; RGB PSBTs can be sizeable but not
/// huge). Enforcing it on read protects each side from allocating whatever a corrupt/malicious
/// length prefix claims (up to 4 GiB); enforcing it on write turns an oversize request into a clear
/// error instead of a silently dropped connection at the peer.
pub(crate) const MAX_FRAME_LEN: u32 = 8 * 1024 * 1024;

fn oversize_error(len: u32) -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        format!("signer frame of {len} bytes exceeds the {MAX_FRAME_LEN} byte limit"),
    )
}

/// Write one frame and flush (node side, sync). Prefix and payload go out as a single write so a
/// frame is one TCP segment when it fits.
pub(crate) fn write_frame(stream: &mut dyn Write, payload: &[u8]) -> std::io::Result<()> {
    let len = u32::try_from(payload.len())
        .ok()
        .filter(|len| *len <= MAX_FRAME_LEN)
        .ok_or_else(|| oversize_error(payload.len().min(u32::MAX as usize) as u32))?;
    let mut framed = Vec::with_capacity(4 + payload.len());
    framed.extend_from_slice(&len.to_be_bytes());
    framed.extend_from_slice(payload);
    stream.write_all(&framed)?;
    stream.flush()
}

/// Read one frame (node side, sync). `Ok(None)` = the daemon's 0-length handler-error sentinel (a
/// valid response, not an IO failure — do NOT reconnect).
pub(crate) fn read_frame(stream: &mut dyn Read) -> std::io::Result<Option<Vec<u8>>> {
    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf)?;
    let len = u32::from_be_bytes(len_buf);
    if len == 0 {
        return Ok(None);
    }
    if len > MAX_FRAME_LEN {
        return Err(oversize_error(len));
    }
    let mut buf = vec![0u8; len as usize];
    stream.read_exact(&mut buf)?;
    Ok(Some(buf))
}

/// Read one frame (daemon side, async). `Ok(None)` = the peer closed the connection at a frame
/// boundary (any failure to read the length prefix counts as a close). A 0-length frame is returned
/// as `Some(vec![])` — on the request path it is not a sentinel, just an (invalid) empty envelope
/// the handler will reject.
pub(crate) async fn read_frame_async<S: AsyncRead + Unpin>(
    stream: &mut S,
) -> std::io::Result<Option<Vec<u8>>> {
    let len = match stream.read_u32().await {
        Ok(len) => len,
        Err(_) => return Ok(None), // peer closed
    };
    if len > MAX_FRAME_LEN {
        return Err(oversize_error(len));
    }
    let mut buf = vec![0u8; len as usize];
    stream.read_exact(&mut buf).await?;
    Ok(Some(buf))
}

/// Write one frame and flush (daemon side, async). An empty `payload` writes the 0-length
/// handler-error sentinel.
pub(crate) async fn write_frame_async<S: AsyncWrite + Unpin>(
    stream: &mut S,
    payload: &[u8],
) -> std::io::Result<()> {
    let len = u32::try_from(payload.len())
        .ok()
        .filter(|len| *len <= MAX_FRAME_LEN)
        .ok_or_else(|| oversize_error(payload.len().min(u32::MAX as usize) as u32))?;
    stream.write_u32(len).await?;
    stream.write_all(payload).await?;
    stream.flush().await
}
