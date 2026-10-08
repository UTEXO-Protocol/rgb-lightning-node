//! Mutation boundary for mainnet shared persistence. Historical Lightning state remains inert;
//! its presence is neither interpreted as an obligation nor used to refuse the on-chain wallet.

use lightning::rgb_utils::{RGB_PRIMARY_NS, RGB_WALLET_CONFIG_NS};

// These are the existing, reconstructible mirrors written by ldk::save_config. This is an
// exact allowlist, not permission to replay arbitrary data under the wallet_config namespace.
const COMMON_CONFIG_KEYS: [&str; 6] = [
    "indexer_url",
    "bitcoin_network",
    "wallet_fingerprint",
    "wallet_account_xpub_vanilla",
    "wallet_account_xpub_colored",
    "wallet_master_fingerprint",
];

pub(crate) fn is_common_config(primary: &str, secondary: &str, key: &str) -> bool {
    primary == RGB_PRIMARY_NS
        && secondary == RGB_WALLET_CONFIG_NS
        && COMMON_CONFIG_KEYS.contains(&key)
}

#[cfg(feature = "vss")]
pub(crate) fn is_common_remote_key(key: &str) -> bool {
    COMMON_CONFIG_KEYS
        .iter()
        .any(|name| key == format!("{RGB_PRIMARY_NS}/{RGB_WALLET_CONFIG_NS}/{name}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_exact_common_config_locations_are_mutable() {
        for key in COMMON_CONFIG_KEYS {
            assert!(is_common_config("rgb", "wallet_config", key));
        }
        for (primary, secondary, key) in [
            ("", "", "manager"),
            ("", "", "output_sweeper"),
            ("monitors", "", "monitor"),
            ("monitor_updates", "channel", "1"),
            ("rgb", "wallet_config", "unknown"),
            ("rgb", "", "indexer_url"),
            ("rgb", "wallet_config", "indexer_url/manager"),
            ("reimport_marker", "", "fascia_replay"),
        ] {
            assert!(!is_common_config(primary, secondary, key));
        }
    }

    #[cfg(feature = "vss")]
    #[test]
    fn only_canonical_common_remote_keys_are_replayed() {
        for key in COMMON_CONFIG_KEYS {
            assert!(is_common_remote_key(&format!("rgb/wallet_config/{key}")));
        }
        for key in [
            "_/_/manager",
            "_/_/output_sweeper",
            "monitor_updates/channel/1",
            "rgb/pending_funding/id",
            "vss_pending/_/rgb/wallet_config/indexer_url",
            "rgb/wallet_config/bitcoin_network/extra",
            "rgb//wallet_config/indexer_url",
            "malformed",
        ] {
            assert!(!is_common_remote_key(key));
        }
    }
}
