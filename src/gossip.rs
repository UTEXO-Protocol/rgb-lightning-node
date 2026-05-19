use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::disk::FilesystemLogger;
use crate::ldk::{GossipVerifier, NetworkGraph, P2PGossipSync, RapidGossipSync};

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

pub(crate) enum GossipSource {
    P2PNetwork {
        gossip_sync: Arc<P2PGossipSync>,
    },
    RapidGossipSync {
        gossip_sync: Arc<RapidGossipSync>,
        server_url: String,
        latest_sync_timestamp: AtomicU32,
        logger: Arc<FilesystemLogger>,
    },
}

impl GossipSource {
    pub(crate) fn new_p2p(
        network_graph: Arc<NetworkGraph>,
        utxo_lookup: Option<Arc<GossipVerifier>>,
        logger: Arc<FilesystemLogger>,
    ) -> Self {
        let gossip_sync = Arc::new(P2PGossipSync::new(network_graph, utxo_lookup, logger));
        Self::P2PNetwork { gossip_sync }
    }

    pub(crate) fn new_rgs(
        server_url: String,
        latest_sync_timestamp: u32,
        network_graph: Arc<NetworkGraph>,
        logger: Arc<FilesystemLogger>,
    ) -> Self {
        let gossip_sync = Arc::new(RapidGossipSync::new(network_graph, Arc::clone(&logger)));
        Self::RapidGossipSync {
            gossip_sync,
            server_url,
            latest_sync_timestamp: AtomicU32::new(latest_sync_timestamp),
            logger,
        }
    }

    pub(crate) fn is_rgs(&self) -> bool {
        matches!(self, Self::RapidGossipSync { .. })
    }

    pub(crate) fn as_gossip_sync(&self) -> crate::ldk::GossipSync {
        use lightning_background_processor::GossipSync as Lbp;
        match self {
            Self::RapidGossipSync { gossip_sync, .. } => Lbp::Rapid(Arc::clone(gossip_sync)),
            Self::P2PNetwork { gossip_sync } => Lbp::P2P(Arc::clone(gossip_sync)),
        }
    }

    pub(crate) async fn update_rgs_snapshot(&self) -> Result<u32, crate::error::APIError> {
        let (gossip_sync, server_url, latest_sync_timestamp) = match self {
            Self::P2PNetwork { .. } => return Ok(0),
            Self::RapidGossipSync {
                gossip_sync,
                server_url,
                latest_sync_timestamp,
                ..
            } => (gossip_sync, server_url, latest_sync_timestamp),
        };

        let ts = latest_sync_timestamp.load(Ordering::Acquire);
        let url = format!("{}/{}", server_url.trim_end_matches('/'), ts);

        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(RGS_SYNC_TIMEOUT_SECS))
            .build()
            .map_err(|e| crate::error::APIError::GossipUpdateFailed(e.to_string()))?;

        let response = client.get(&url).send().await.map_err(|e| {
            if e.is_timeout() {
                crate::error::APIError::GossipUpdateTimeout
            } else {
                crate::error::APIError::GossipUpdateFailed(e.to_string())
            }
        })?;

        if !response.status().is_success() {
            return Err(crate::error::APIError::GossipUpdateFailed(format!(
                "HTTP {}",
                response.status()
            )));
        }

        let bytes = response
            .bytes()
            .await
            .map_err(|e| crate::error::APIError::GossipUpdateFailed(e.to_string()))?;

        if bytes.len() > RGS_SNAPSHOT_MAX_SIZE {
            return Err(crate::error::APIError::GossipUpdateFailed(format!(
                "snapshot too large: {} bytes",
                bytes.len()
            )));
        }

        let new_timestamp = gossip_sync
            .update_network_graph(&bytes)
            .map_err(|e| crate::error::APIError::GossipUpdateFailed(format!("{e:?}")))?;

        latest_sync_timestamp.store(new_timestamp, Ordering::Release);
        tracing::info!("RGS snapshot applied, new timestamp: {new_timestamp}");
        Ok(new_timestamp)
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

#[cfg(test)]
mod source_tests {
    use super::*;
    use crate::disk::FilesystemLogger;
    use crate::ldk::NetworkGraph;
    use bitcoin::Network;
    use std::sync::Arc;

    fn test_logger() -> Arc<FilesystemLogger> {
        Arc::new(FilesystemLogger::new(tempfile::tempdir().unwrap().keep()))
    }

    fn test_graph(logger: Arc<FilesystemLogger>) -> Arc<NetworkGraph> {
        Arc::new(NetworkGraph::new(Network::Regtest, logger))
    }

    #[test]
    fn p2p_source_reports_not_rgs() {
        let logger = test_logger();
        let graph = test_graph(Arc::clone(&logger));
        let source = GossipSource::new_p2p(graph, None, logger);
        assert!(!source.is_rgs());
    }

    #[test]
    fn rgs_source_reports_rgs() {
        let logger = test_logger();
        let graph = test_graph(Arc::clone(&logger));
        let source =
            GossipSource::new_rgs("https://example.invalid/snapshot".into(), 0, graph, logger);
        assert!(source.is_rgs());
    }

    #[test]
    fn p2p_as_gossip_sync_returns_p2p_variant() {
        let logger = test_logger();
        let graph = test_graph(Arc::clone(&logger));
        let source = GossipSource::new_p2p(graph, None, logger);
        assert!(matches!(
            source.as_gossip_sync(),
            lightning_background_processor::GossipSync::P2P(_)
        ));
    }

    #[test]
    fn rgs_as_gossip_sync_returns_rapid_variant() {
        let logger = test_logger();
        let graph = test_graph(Arc::clone(&logger));
        let source =
            GossipSource::new_rgs("https://example.invalid/snapshot".into(), 0, graph, logger);
        assert!(matches!(
            source.as_gossip_sync(),
            lightning_background_processor::GossipSync::Rapid(_)
        ));
    }

    #[tokio::test]
    async fn update_rgs_snapshot_returns_zero_for_p2p() {
        let logger = test_logger();
        let graph = test_graph(Arc::clone(&logger));
        let source = GossipSource::new_p2p(graph, None, logger);
        assert_eq!(source.update_rgs_snapshot().await.unwrap(), 0);
    }

    #[tokio::test]
    async fn update_rgs_snapshot_fails_on_http_500() {
        use tokio::io::AsyncWriteExt;
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            if let Ok((mut sock, _)) = listener.accept().await {
                let _ = sock
                    .write_all(b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\n\r\n")
                    .await;
            }
        });

        let logger = test_logger();
        let graph = test_graph(Arc::clone(&logger));
        let source = GossipSource::new_rgs(format!("http://{addr}/snapshot"), 0, graph, logger);
        match source.update_rgs_snapshot().await {
            Err(crate::error::APIError::GossipUpdateFailed(_)) => {}
            other => panic!("expected GossipUpdateFailed, got {other:?}"),
        }
    }
}
