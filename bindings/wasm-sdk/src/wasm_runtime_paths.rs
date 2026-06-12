//! Browser-safe filesystem *names* for WASM (`wasm32`) vs POSIX temp dirs for host tests.
//!
//! LDK and rgb-lib still receive `PathBuf` / `String` directory parameters; on `wasm32` we use
//! single-segment relative names (no `/tmp`) so browser builds never assume a POSIX mount.

use std::path::PathBuf;

fn runtime_descriptor_slug(descriptor: &str) -> String {
    let slug: String = descriptor
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
        .take(80)
        .collect();
    if slug.is_empty() {
        "default".to_string()
    } else {
        slug
    }
}

/// Directory passed to LDK `KeysManager::new` / `ChannelManager::new` RGB transfer scratch paths.
#[allow(dead_code)]
pub(crate) fn ldk_data_dir_for_runtime(runtime_key: &str) -> PathBuf {
    let slug = runtime_descriptor_slug(runtime_key);
    #[cfg(target_arch = "wasm32")]
    {
        PathBuf::from(format!("rln_ldk_{slug}"))
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        PathBuf::from(format!("/tmp/rln_wasm_ldk_{slug}"))
    }
}

/// `WalletData.data_dir` string for rgb-lib-wasm auto / contract wallets.
pub(crate) fn rgb_lib_wallet_data_dir(master_fingerprint: &str) -> String {
    let slug = runtime_descriptor_slug(master_fingerprint);
    #[cfg(target_arch = "wasm32")]
    {
        format!("rln_wallet_{slug}")
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        format!("/tmp/rln_wasm_sdk_wallet_{slug}")
    }
}

#[cfg(test)]
mod tests {
    use super::{ldk_data_dir_for_runtime, rgb_lib_wallet_data_dir};

    #[test]
    fn slug_strips_special_characters_for_ldk() {
        let p = ldk_data_dir_for_runtime("a/b:c");
        let s = p.to_string_lossy();
        #[cfg(target_arch = "wasm32")]
        assert_eq!(s, "rln_ldk_abc");
        #[cfg(not(target_arch = "wasm32"))]
        assert!(
            s.contains("rln_wasm_ldk_abc"),
            "unexpected path (expected slug abc): {s}"
        );
    }

    #[test]
    fn wasm_ldk_avoids_posix_tmp_mount() {
        #[cfg(target_arch = "wasm32")]
        {
            let p = ldk_data_dir_for_runtime("node-1");
            let s = p.to_string_lossy();
            assert!(
                !s.starts_with("/tmp"),
                "browser wasm must not hard-code POSIX /tmp: {s}"
            );
            assert!(
                !s.contains('/') && !s.contains('\\'),
                "use a single path segment in wasm: {s}"
            );
        }
    }

    #[test]
    fn native_ldk_uses_absolute_tmp() {
        #[cfg(not(target_arch = "wasm32"))]
        {
            let p = ldk_data_dir_for_runtime("node-1");
            assert!(p.is_absolute(), "{p:?}");
            assert!(p.starts_with("/tmp"), "{p:?}");
        }
    }

    #[test]
    fn rgb_wallet_dir_tracks_ldk_policy() {
        #[cfg(target_arch = "wasm32")]
        {
            let d = rgb_lib_wallet_data_dir("a1b2c3d4");
            assert!(!d.starts_with("/tmp"), "{d}");
            assert!(!d.contains('/'), "{d}");
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            let d = rgb_lib_wallet_data_dir("a1b2c3d4");
            assert!(d.starts_with("/tmp/"), "{d}");
            assert!(d.contains("rln_wasm_sdk_wallet_"), "{d}");
        }
    }
}
