//! Caller authentication for the remote-signer daemon link.
//!
//! The daemon holds the seed and answers every signing request it accepts, so *being able to reach
//! the port* must not be sufficient to be served. Loopback is not an authentication boundary: every
//! local uid — and anything sharing the network namespace — can connect to `127.0.0.1`, and without
//! this handshake an unprivileged local process could ask the daemon for the node's account xpubs,
//! its seed-derived offer-key HMAC, or an ECDH secret against an attacker-chosen pubkey. The daemon
//! already treats other local users as adversaries for its on-disk material (0600 seed file, 0700
//! data dir); this closes the same gap on the socket.
//!
//! Both ends share a 32-byte token stored in an owner-only file, and the node presents it as the
//! first frame of every connection (see [`AuthToken::handshake_frame`]) before any signer op is
//! accepted. It is a bearer secret rather than a challenge/response on purpose: on loopback an
//! observer needs root, which can read the seed file anyway, and a non-loopback listener already
//! requires mTLS (see `daemon::check_listener_exposure`), which encrypts the token in transit.

use std::path::{Path, PathBuf};

use anyhow::Context;
use bitcoin::hex::{DisplayHex, FromHex};
use rand::rngs::OsRng;
use rand::RngCore;

use crate::signer::key_source::{check_restricted_file, write_restricted_file};

/// Shared-secret length. 32 bytes of OS entropy — the token is only ever compared, never stretched.
pub(crate) const TOKEN_LEN: usize = 32;

/// Prefixes the node's first frame so an auth handshake can never be confused with a signer envelope
/// (or vice versa), and so a future scheme can be introduced as a distinct tag.
const HANDSHAKE_TAG: &[u8] = b"RLNSIGNERAUTH1";

/// The daemon's reply to a token it accepted. A 0-length reply frame means rejected.
const HANDSHAKE_ACK: &[u8] = b"RLNSIGNERAUTHOK";

/// File name of the daemon's token inside its `--data-dir`, when `--auth-token-file` is not given.
const DAEMON_TOKEN_FILE: &str = "auth-token";

/// File name the node reads its copy of the token from, under its storage dir.
const NODE_TOKEN_FILE: &str = "remote-signer-auth-token";

/// Compare two byte strings without an early exit, so a wrong token cannot be recovered byte by byte
/// from the time the daemon takes to reject it. Lengths are not secret, so returning early on a
/// length mismatch is fine.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// The shared token authenticating the node to the daemon. Zeroed on drop; never logged or
/// `Debug`-printed (no `Debug` impl on purpose, so it cannot be captured by a tracing field).
#[derive(Clone)]
pub struct AuthToken([u8; TOKEN_LEN]);

impl Drop for AuthToken {
    fn drop(&mut self) {
        self.0.fill(0);
    }
}

impl AuthToken {
    /// Where the daemon keeps its token by default: inside the (0700) data dir, beside the VLS state.
    pub(crate) fn daemon_default_path(data_dir: &Path) -> PathBuf {
        data_dir.join(DAEMON_TOKEN_FILE)
    }

    /// Where the node reads its copy of the token from.
    pub(crate) fn node_path(storage_dir: &Path) -> PathBuf {
        storage_dir.join(NODE_TOKEN_FILE)
    }

    /// Daemon side: load the token from `path`, generating a fresh one (mode 0600, atomically) if the
    /// file does not exist yet — the same load-or-generate treatment as the seed file, so a daemon
    /// started with no extra flags is authenticated rather than open to every local caller.
    pub(crate) fn load_or_generate(path: &Path) -> anyhow::Result<Self> {
        if path.exists() {
            return Self::load(path);
        }
        let mut token = [0u8; TOKEN_LEN];
        OsRng.fill_bytes(&mut token);
        write_restricted_file(path, token.to_lower_hex_string().as_bytes())
            .with_context(|| format!("write auth token file {}", path.display()))?;
        Ok(Self(token))
    }

    /// Load an existing token file, refusing one that is group/other-readable or owned by another
    /// user — a token an attacker can read is not a credential.
    pub(crate) fn load(path: &Path) -> anyhow::Result<Self> {
        check_restricted_file(path)
            .with_context(|| format!("auth token file {} failed safety checks", path.display()))?;
        let contents = std::fs::read_to_string(path)
            .with_context(|| format!("read auth token file {}", path.display()))?;
        let bytes = Vec::<u8>::from_hex(contents.trim())
            .with_context(|| format!("auth token file {} must be hex", path.display()))?;
        let token: [u8; TOKEN_LEN] = bytes.try_into().map_err(|_| {
            anyhow::anyhow!(
                "auth token file {} must decode to exactly {TOKEN_LEN} bytes",
                path.display()
            )
        })?;
        Ok(Self(token))
    }

    /// Test-only constructor. Production tokens always come from an owner-only file, so that the
    /// secret's only home is one `check_restricted_file` has vetted.
    #[cfg(test)]
    pub(crate) fn from_bytes(bytes: [u8; TOKEN_LEN]) -> Self {
        Self(bytes)
    }

    /// The node's first frame on every (re)connect.
    pub(crate) fn handshake_frame(&self) -> Vec<u8> {
        let mut frame = Vec::with_capacity(HANDSHAKE_TAG.len() + TOKEN_LEN);
        frame.extend_from_slice(HANDSHAKE_TAG);
        frame.extend_from_slice(&self.0);
        frame
    }

    /// Daemon side: does `frame` carry this token? Constant-time in the token bytes.
    pub(crate) fn matches_handshake(&self, frame: &[u8]) -> bool {
        let Some(token) = frame.strip_prefix(HANDSHAKE_TAG) else {
            return false;
        };
        constant_time_eq(token, &self.0)
    }
}

/// The daemon's "token accepted" frame.
pub(crate) fn ack_frame() -> &'static [u8] {
    HANDSHAKE_ACK
}

/// Node side: is this the daemon's ack?
pub(crate) fn is_ack(frame: &[u8]) -> bool {
    frame == HANDSHAKE_ACK
}

#[cfg(test)]
mod tests {
    use super::*;

    fn token_of(byte: u8) -> AuthToken {
        AuthToken([byte; TOKEN_LEN])
    }

    #[test]
    fn handshake_frame_round_trips_and_rejects_a_different_token() {
        let token = token_of(1);
        let frame = token.handshake_frame();

        assert!(token.matches_handshake(&frame));
        assert!(
            !token_of(2).matches_handshake(&frame),
            "a different token must not authenticate"
        );
    }

    /// A signer envelope (or any frame without the tag) must never be mistaken for a handshake, and a
    /// frame that is the bare token without the tag must not authenticate either.
    #[test]
    fn untagged_and_truncated_frames_do_not_authenticate() {
        let token = token_of(3);

        assert!(!token.matches_handshake(&[]));
        assert!(!token.matches_handshake(&[3u8; TOKEN_LEN]));
        assert!(!token.matches_handshake(HANDSHAKE_TAG));
        let mut short = token.handshake_frame();
        short.pop();
        assert!(!token.matches_handshake(&short));
        let mut long = token.handshake_frame();
        long.push(0);
        assert!(!token.matches_handshake(&long));
    }

    #[test]
    fn generated_token_file_round_trips() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = AuthToken::daemon_default_path(dir.path());

        let generated = AuthToken::load_or_generate(&path).expect("generate");
        assert!(path.exists(), "token file must be created");

        // A second call must reuse the file, not mint a new secret — otherwise every daemon restart
        // would invalidate the node's copy.
        let reloaded = AuthToken::load_or_generate(&path).expect("reload");
        assert!(reloaded.matches_handshake(&generated.handshake_frame()));

        // And the node-side loader must read the same bytes the daemon wrote.
        let node_side = AuthToken::load(&path).expect("load");
        assert!(node_side.matches_handshake(&generated.handshake_frame()));
    }

    #[cfg(unix)]
    #[test]
    fn generated_token_file_is_owner_only_and_broad_permissions_are_refused() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("auth-token");

        AuthToken::load_or_generate(&path).expect("generate");
        let mode = std::fs::metadata(&path).expect("stat").permissions().mode();
        assert_eq!(
            mode & 0o777,
            0o600,
            "token file permissions are {:o}, expected 0600",
            mode & 0o777
        );

        // A token any local user can read is not a credential: refuse it rather than trust it.
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).expect("chmod");
        assert!(
            AuthToken::load(&path).is_err(),
            "group/other-readable token must be refused"
        );
        assert!(
            AuthToken::load_or_generate(&path).is_err(),
            "load_or_generate must not accept an exposed token file either"
        );
    }

    #[test]
    fn malformed_token_file_is_rejected() {
        let dir = tempfile::tempdir().expect("tempdir");

        let not_hex = dir.path().join("not-hex");
        write_restricted_file(&not_hex, b"zzzz").expect("write");
        assert!(
            AuthToken::load(&not_hex).is_err(),
            "non-hex token must be refused"
        );

        let too_short = dir.path().join("too-short");
        write_restricted_file(&too_short, b"aabb").expect("write");
        assert!(
            AuthToken::load(&too_short).is_err(),
            "short token must be refused"
        );

        let missing = dir.path().join("missing");
        assert!(
            AuthToken::load(&missing).is_err(),
            "absent token must be an error, not a default"
        );
    }

    #[test]
    fn constant_time_eq_matches_plain_equality() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"ab"));
        assert!(constant_time_eq(b"", b""));
    }
}
