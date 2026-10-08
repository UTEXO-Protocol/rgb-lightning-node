use crate::error::APIError;
use crate::utils::UnlockedAppState;
use lightning::chain::channelmonitor::Balance;

/// Lightning fields in the shared node-info response. Wallet balances are reported separately.
#[derive(Default)]
pub(crate) struct LightningInfo {
    pub(crate) num_channels: usize,
    pub(crate) num_usable_channels: usize,
    pub(crate) local_balance_sat: u64,
    pub(crate) eventual_close_fees_sat: u64,
    pub(crate) pending_outbound_payments_sat: u64,
    pub(crate) num_peers: usize,
    pub(crate) network_nodes: usize,
    pub(crate) network_channels: usize,
    pub(crate) latest_rgs_snapshot_timestamp: Option<u64>,
}

impl LightningInfo {
    pub(crate) fn from_state(state: &UnlockedAppState) -> Self {
        let Some(lightning) = state.lightning.as_ref() else {
            return Self::default();
        };
        let channels = lightning.channel_manager.list_channels();
        let balances = lightning.chain_monitor.get_claimable_balances(&[]);
        let graph = lightning.network_graph.read_only();
        Self {
            num_channels: channels.len(),
            num_usable_channels: channels.iter().filter(|channel| channel.is_usable).count(),
            local_balance_sat: balances
                .iter()
                .map(Balance::claimable_amount_satoshis)
                .sum(),
            eventual_close_fees_sat: balances
                .iter()
                .map(|balance| match balance {
                    Balance::ClaimableOnChannelClose {
                        balance_candidates,
                        confirmed_balance_candidate_index,
                        ..
                    } => {
                        balance_candidates[*confirmed_balance_candidate_index]
                            .transaction_fee_satoshis
                    }
                    _ => 0,
                })
                .sum(),
            pending_outbound_payments_sat: balances
                .iter()
                .map(|balance| match balance {
                    Balance::MaybeTimeoutClaimableHTLC {
                        amount_satoshis,
                        outbound_payment: true,
                        ..
                    } => *amount_satoshis,
                    _ => 0,
                })
                .sum(),
            num_peers: lightning.peer_manager.list_peers().len(),
            network_nodes: graph.nodes().len(),
            network_channels: graph.channels().len(),
            latest_rgs_snapshot_timestamp: lightning
                .network_graph
                .get_last_rapid_gossip_sync_timestamp()
                .map(u64::from),
        }
    }
}

/// Read the wallet indexer's tip without constructing an LDK chain backend or polling worker.
/// The caller releases the lifecycle lock before awaiting the query. Client timeouts limit
/// socket operations; Electrum hostname resolution still uses the system resolver's timing.
pub(crate) async fn mainnet_height(indexer_url: String) -> Result<u32, APIError> {
    tokio::task::spawn_blocking(move || read_mainnet_height(&indexer_url, 5))
        .await
        .map_err(|err| APIError::Unexpected(format!("indexer height task failed: {err}")))?
}

fn read_mainnet_height(indexer_url: &str, timeout_seconds: u8) -> Result<u32, APIError> {
    let genesis =
        bitcoin::blockdata::constants::genesis_block(bitcoin::Network::Bitcoin).block_hash();
    #[cfg(feature = "electrum")]
    if !indexer_url.starts_with("http://") && !indexer_url.starts_with("https://") {
        use electrum_client::ElectrumApi;
        let config = electrum_client::ConfigBuilder::new()
            .timeout(Some(timeout_seconds))
            .retry(0)
            .build();
        let client = electrum_client::Client::from_config(indexer_url, config).map_err(|err| {
            APIError::Network(format!("cannot read wallet indexer height: {err}"))
        })?;
        let header = client.block_header(0).map_err(|err| {
            APIError::Network(format!("cannot validate wallet indexer network: {err}"))
        })?;
        if header.block_hash() != genesis {
            return Err(APIError::InvalidIndexer(
                "wallet indexer is not on mainnet".into(),
            ));
        }
        let tip = client.block_headers_subscribe().map_err(|err| {
            APIError::Network(format!("cannot read wallet indexer height: {err}"))
        })?;
        return u32::try_from(tip.height)
            .map_err(|_| APIError::Network("wallet indexer returned an invalid height".into()));
    }
    #[cfg(feature = "esplora")]
    if indexer_url.starts_with("https://") || indexer_url.starts_with("http://") {
        let client = esplora_client::Builder::new(indexer_url)
            .timeout(u64::from(timeout_seconds))
            .max_retries(0)
            .build_blocking();
        let remote_genesis = client.get_block_hash(0).map_err(|err| {
            APIError::Network(format!("cannot validate wallet indexer network: {err}"))
        })?;
        if remote_genesis != genesis {
            return Err(APIError::InvalidIndexer(
                "wallet indexer is not on mainnet".into(),
            ));
        }
        return client
            .get_height()
            .map_err(|err| APIError::Network(format!("cannot read wallet indexer height: {err}")));
    }
    Err(APIError::InvalidIndexer(
        "unsupported wallet indexer protocol".into(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn indexer_that_accepts_without_reply_does_not_leave_a_blocked_worker() {
        #[allow(unused_mut)]
        let mut schemes = Vec::new();
        #[cfg(feature = "electrum")]
        schemes.push("tcp");
        #[cfg(feature = "esplora")]
        schemes.push("http");
        for scheme in schemes {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("{scheme}://{}", listener.local_addr().unwrap());
            let server = tokio::spawn(async move {
                let (_socket, _) = listener.accept().await.unwrap();
                std::future::pending::<()>().await;
            });
            let started = Instant::now();
            let result = tokio::task::spawn_blocking(move || read_mainnet_height(&url, 1))
                .await
                .unwrap();
            assert!(
                matches!(result, Err(APIError::Network(_))),
                "{scheme}: {result:?}"
            );
            assert!(started.elapsed() < Duration::from_secs(4), "{scheme}");
            // Awaiting the worker above, rather than timing out its JoinHandle, proves it exited.
            server.abort();
        }
    }

    #[cfg(feature = "esplora")]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn esplora_height_is_live_and_validates_the_network() {
        for network in [bitcoin::Network::Bitcoin, bitcoin::Network::Regtest] {
            let genesis = bitcoin::blockdata::constants::genesis_block(network)
                .block_hash()
                .to_string();
            let router = axum::Router::new()
                .route(
                    "/block-height/0",
                    axum::routing::get(move || async move { genesis }),
                )
                .route(
                    "/blocks/tip/height",
                    axum::routing::get(|| async { "123456" }),
                );
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
            let result = mainnet_height(url).await;
            if network == bitcoin::Network::Bitcoin {
                assert_eq!(result.unwrap(), 123456);
            } else {
                assert!(matches!(result, Err(APIError::InvalidIndexer(_))));
            }
            server.abort();
        }
    }
}
