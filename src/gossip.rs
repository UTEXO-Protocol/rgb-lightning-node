use std::time::Duration;

use serde::{Deserialize, Serialize};

pub(crate) const RGS_SYNC_INTERVAL: Duration = Duration::from_secs(60 * 60);
pub(crate) const RGS_SNAPSHOT_MAX_SIZE: usize = 15 * 1024 * 1024;
pub(crate) const RGS_SYNC_TIMEOUT_SECS: u64 = 5;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub(crate) enum GossipSourceConfig {
    #[serde(rename = "p2p")]
    P2PNetwork,
    #[serde(rename = "rgs")]
    RapidGossipSync { server_url: String },
}

impl Default for GossipSourceConfig {
    fn default() -> Self {
        Self::P2PNetwork
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rgs_constants_have_expected_values() {
        assert_eq!(RGS_SYNC_INTERVAL.as_secs(), 60 * 60);
        assert_eq!(RGS_SNAPSHOT_MAX_SIZE, 15 * 1024 * 1024);
        assert_eq!(RGS_SYNC_TIMEOUT_SECS, 5);
    }
}

#[cfg(test)]
mod config_tests {
    use super::*;

    #[test]
    fn p2p_config_roundtrip() {
        let cfg = GossipSourceConfig::P2PNetwork;
        let json = serde_json::to_string(&cfg).unwrap();
        assert_eq!(json, r#"{"type":"p2p"}"#);
        let back: GossipSourceConfig = serde_json::from_str(&json).unwrap();
        assert!(matches!(back, GossipSourceConfig::P2PNetwork));
    }

    #[test]
    fn rgs_config_roundtrip() {
        let cfg = GossipSourceConfig::RapidGossipSync {
            server_url: "https://example.invalid/snapshot".into(),
        };
        let json = serde_json::to_string(&cfg).unwrap();
        assert_eq!(
            json,
            r#"{"type":"rgs","server_url":"https://example.invalid/snapshot"}"#
        );
        let back: GossipSourceConfig = serde_json::from_str(&json).unwrap();
        match back {
            GossipSourceConfig::RapidGossipSync { server_url } => {
                assert_eq!(server_url, "https://example.invalid/snapshot");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn default_is_p2p() {
        assert!(matches!(
            GossipSourceConfig::default(),
            GossipSourceConfig::P2PNetwork
        ));
    }
}
