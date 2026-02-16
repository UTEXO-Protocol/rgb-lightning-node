use base64::{engine::general_purpose, Engine as _};
use bitcoin::blockdata::transaction::Transaction;
use bitcoin::consensus::encode;
use bitcoin::hash_types::BlockHash;
use bitcoin::Amount;
use lightning::chain::chaininterface::{BroadcasterInterface, ConfirmationTarget, FeeEstimator};
use lightning::log_warn;
use lightning::util::logger::Logger;
use lightning_block_sync::http::HttpEndpoint;
use lightning_block_sync::http::JsonResponse;
use lightning_block_sync::rpc::RpcClient;
use lightning_block_sync::{AsyncBlockSourceResult, BlockData, BlockHeaderData, BlockSource};
use serde_json::json;
use std::collections::HashMap;
use std::convert::TryInto;
use std::str::FromStr;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crate::disk::FilesystemLogger;
#[cfg(test)]
use crate::test::mock_fee;

pub struct BitcoindClient {
    pub(crate) bitcoind_rpc_client: Arc<RpcClient>,
    fees: Arc<HashMap<ConfirmationTarget, AtomicU32>>,
    handle: tokio::runtime::Handle,
    logger: Arc<FilesystemLogger>,
}

impl BlockSource for BitcoindClient {
    fn get_header<'a>(
        &'a self,
        header_hash: &'a BlockHash,
        height_hint: Option<u32>,
    ) -> AsyncBlockSourceResult<'a, BlockHeaderData> {
        Box::pin(async move {
            self.bitcoind_rpc_client
                .get_header(header_hash, height_hint)
                .await
        })
    }

    fn get_block<'a>(
        &'a self,
        header_hash: &'a BlockHash,
    ) -> AsyncBlockSourceResult<'a, BlockData> {
        Box::pin(async move { self.bitcoind_rpc_client.get_block(header_hash).await })
    }

    fn get_best_block(&self) -> AsyncBlockSourceResult<'_, (BlockHash, Option<u32>)> {
        Box::pin(async move { self.bitcoind_rpc_client.get_best_block().await })
    }
}

pub struct MempoolMinFeeResponse {
    pub feerate_sat_per_kw: Option<u32>,
    pub errored: bool,
}

impl TryInto<MempoolMinFeeResponse> for JsonResponse {
    type Error = std::io::Error;
    fn try_into(self) -> std::io::Result<MempoolMinFeeResponse> {
        let errored = !self.0["errors"].is_null();
        assert_eq!(self.0["maxmempool"].as_u64(), Some(300000000));
        Ok(MempoolMinFeeResponse {
            errored,
            feerate_sat_per_kw: self.0["mempoolminfee"]
                .as_f64()
                .map(|feerate_btc_per_kvbyte| {
                    (feerate_btc_per_kvbyte * 100_000_000.0 / 4.0).round() as u32
                }),
        })
    }
}

pub struct BlockchainInfo {
    pub latest_height: usize,
    pub latest_blockhash: BlockHash,
    pub chain: String,
}

impl TryInto<BlockchainInfo> for JsonResponse {
    type Error = std::io::Error;
    fn try_into(self) -> std::io::Result<BlockchainInfo> {
        Ok(BlockchainInfo {
            latest_height: self.0["blocks"].as_u64().unwrap() as usize,
            latest_blockhash: BlockHash::from_str(self.0["bestblockhash"].as_str().unwrap())
                .unwrap(),
            chain: self.0["chain"].as_str().unwrap().to_string(),
        })
    }
}

pub struct FeeResponse {
    pub feerate_sat_per_kw: Option<u32>,
    pub errored: bool,
}

#[derive(serde::Deserialize)]
struct ScanTxOutSetResponse {
    pub unspents: Vec<ScanTxOutSetUnspent>,
}

#[derive(serde::Deserialize)]
struct ScanTxOutSetUnspent {
    pub txid: String,
    pub vout: u32,
    pub amount: f64,
    #[serde(rename = "scriptPubKey")]
    pub script_pubkey: String,
}

#[derive(Clone, Debug)]
pub struct ScannedUtxo {
    pub txid: bitcoin::Txid,
    pub vout: u32,
    pub value_sat: u64,
    pub script_pubkey_hex: String,
}

impl TryInto<FeeResponse> for JsonResponse {
    type Error = std::io::Error;
    fn try_into(self) -> std::io::Result<FeeResponse> {
        let errored = !self.0["errors"].is_null();
        Ok(FeeResponse {
            errored,
            feerate_sat_per_kw: self.0["feerate"].as_f64().map(|feerate_btc_per_kvbyte| {
                (feerate_btc_per_kvbyte * 100_000_000.0 / 4.0).round() as u32
            }),
        })
    }
}

impl TryInto<ScanTxOutSetResponse> for JsonResponse {
    type Error = std::io::Error;
    fn try_into(self) -> std::io::Result<ScanTxOutSetResponse> {
        serde_json::from_value(self.0)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))
    }
}

/// The minimum feerate we are allowed to send, as specify by LDK.
const MIN_FEERATE: u32 = 253;

impl BitcoindClient {
    pub(crate) async fn new(
        host: String,
        port: u16,
        rpc_user: String,
        rpc_password: String,
        handle: tokio::runtime::Handle,
        logger: Arc<FilesystemLogger>,
    ) -> std::io::Result<Self> {
        let http_endpoint = HttpEndpoint::for_host(host.clone()).with_port(port);
        let rpc_credentials = general_purpose::STANDARD.encode(format!(
            "{}:{}",
            rpc_user.clone(),
            rpc_password.clone()
        ));
        let bitcoind_rpc_client = RpcClient::new(&rpc_credentials, http_endpoint);
        let _dummy = bitcoind_rpc_client
            .call_method::<BlockchainInfo>("getblockchaininfo", &[])
            .await
            .map_err(|_| {
                std::io::Error::new(std::io::ErrorKind::PermissionDenied,
                "failed to make initial call to bitcoind - please check your RPC user/password and access settings")
            })?;
        let mut fees: HashMap<ConfirmationTarget, AtomicU32> = HashMap::new();
        fees.insert(
            ConfirmationTarget::MaximumFeeEstimate,
            AtomicU32::new(50000),
        );
        fees.insert(ConfirmationTarget::UrgentOnChainSweep, AtomicU32::new(5000));
        fees.insert(
            ConfirmationTarget::MinAllowedAnchorChannelRemoteFee,
            AtomicU32::new(MIN_FEERATE),
        );
        fees.insert(
            ConfirmationTarget::MinAllowedNonAnchorChannelRemoteFee,
            AtomicU32::new(MIN_FEERATE),
        );
        fees.insert(
            ConfirmationTarget::AnchorChannelFee,
            AtomicU32::new(MIN_FEERATE),
        );
        fees.insert(
            ConfirmationTarget::NonAnchorChannelFee,
            AtomicU32::new(2000),
        );
        fees.insert(
            ConfirmationTarget::ChannelCloseMinimum,
            AtomicU32::new(MIN_FEERATE),
        );
        fees.insert(
            ConfirmationTarget::OutputSpendingFee,
            AtomicU32::new(MIN_FEERATE),
        );

        let client = Self {
            bitcoind_rpc_client: Arc::new(bitcoind_rpc_client),
            fees: Arc::new(fees),
            handle: handle.clone(),
            logger,
        };
        BitcoindClient::poll_for_fee_estimates(
            client.fees.clone(),
            client.bitcoind_rpc_client.clone(),
            client.logger.clone(),
            handle,
        );
        Ok(client)
    }

    fn poll_for_fee_estimates(
        fees: Arc<HashMap<ConfirmationTarget, AtomicU32>>,
        rpc_client: Arc<RpcClient>,
        logger: Arc<FilesystemLogger>,
        handle: tokio::runtime::Handle,
    ) {
        handle.spawn(async move {
            async fn get_estimate(
                rpc_client: &Arc<RpcClient>,
                logger: &Arc<FilesystemLogger>,
                params: &[serde_json::Value],
                default: u32,
            ) -> u32 {
                match rpc_client
                    .call_method::<FeeResponse>("estimatesmartfee", params)
                    .await
                {
                    Ok(res) => match res.feerate_sat_per_kw {
                        Some(feerate) => Some(std::cmp::max(feerate, MIN_FEERATE)),
                        None => {
                            log_warn!(logger, "Fee estimation unavailable");
                            None
                        }
                    },
                    Err(e) => {
                        log_warn!(logger, "Error getting fee estimate: {}", e);
                        None
                    }
                }
                .unwrap_or(default)
            }

            loop {
                let mempoolmin_estimate = {
                    match rpc_client
                        .call_method::<MempoolMinFeeResponse>("getmempoolinfo", &[])
                        .await
                    {
                        Ok(res) => match res.feerate_sat_per_kw {
                            Some(feerate) => Some(std::cmp::max(feerate, MIN_FEERATE)),
                            None => {
                                log_warn!(logger, "Mempool info unavailable");
                                None
                            }
                        },
                        Err(e) => {
                            log_warn!(logger, "Error getting mepool info: {}", e);
                            None
                        }
                    }
                    .unwrap_or(MIN_FEERATE)
                };
                let background_estimate = get_estimate(
                    &rpc_client,
                    &logger,
                    &[serde_json::json!(144), serde_json::json!("ECONOMICAL")],
                    MIN_FEERATE,
                )
                .await;

                let normal_estimate = get_estimate(
                    &rpc_client,
                    &logger,
                    &[serde_json::json!(18), serde_json::json!("ECONOMICAL")],
                    2000,
                )
                .await;

                let high_prio_estimate = get_estimate(
                    &rpc_client,
                    &logger,
                    &[serde_json::json!(6), serde_json::json!("CONSERVATIVE")],
                    5000,
                )
                .await;

                let very_high_prio_estimate = get_estimate(
                    &rpc_client,
                    &logger,
                    &[serde_json::json!(2), serde_json::json!("CONSERVATIVE")],
                    50000,
                )
                .await;

                fees.get(&ConfirmationTarget::MaximumFeeEstimate)
                    .unwrap()
                    .store(very_high_prio_estimate, Ordering::Release);
                fees.get(&ConfirmationTarget::UrgentOnChainSweep)
                    .unwrap()
                    .store(high_prio_estimate, Ordering::Release);
                fees.get(&ConfirmationTarget::MinAllowedAnchorChannelRemoteFee)
                    .unwrap()
                    .store(mempoolmin_estimate, Ordering::Release);
                fees.get(&ConfirmationTarget::MinAllowedNonAnchorChannelRemoteFee)
                    .unwrap()
                    .store(background_estimate - 250, Ordering::Release);
                fees.get(&ConfirmationTarget::AnchorChannelFee)
                    .unwrap()
                    .store(background_estimate, Ordering::Release);
                fees.get(&ConfirmationTarget::NonAnchorChannelFee)
                    .unwrap()
                    .store(normal_estimate, Ordering::Release);
                fees.get(&ConfirmationTarget::ChannelCloseMinimum)
                    .unwrap()
                    .store(background_estimate, Ordering::Release);
                fees.get(&ConfirmationTarget::OutputSpendingFee)
                    .unwrap()
                    .store(background_estimate, Ordering::Release);

                tokio::time::sleep(Duration::from_secs(60)).await;
            }
        });
    }

    pub async fn get_blockchain_info(&self) -> BlockchainInfo {
        self.bitcoind_rpc_client
            .call_method::<BlockchainInfo>("getblockchaininfo", &[])
            .await
            .unwrap()
    }

    pub async fn get_block_height(&self) -> std::io::Result<u32> {
        let info = self
            .bitcoind_rpc_client
            .call_method::<BlockchainInfo>("getblockchaininfo", &[])
            .await?;
        u32::try_from(info.latest_height).map_err(|_| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, "block height too large")
        })
    }

    pub(crate) async fn get_txout_confirmations(
        &self,
        txid: &bitcoin::Txid,
        vout: u32,
    ) -> std::io::Result<Option<u64>> {
        let params = [json!(txid.to_string()), json!(vout), json!(true)];
        let res = self
            .bitcoind_rpc_client
            .call_method::<serde_json::Value>("gettxout", &params)
            .await
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        if res.is_null() {
            return Ok(None);
        }
        Ok(res["confirmations"].as_u64())
    }

    pub(crate) fn get_txout_confirmations_blocking(
        &self,
        txid: &bitcoin::Txid,
        vout: u32,
    ) -> std::io::Result<Option<u64>> {
        if tokio::runtime::Handle::try_current().is_ok() {
            tokio::task::block_in_place(|| {
                self.handle
                    .block_on(self.get_txout_confirmations(txid, vout))
            })
        } else {
            self.handle
                .block_on(self.get_txout_confirmations(txid, vout))
        }
    }

    pub(crate) async fn scan_utxos_for_address(
        &self,
        address: &str,
    ) -> std::io::Result<Vec<ScannedUtxo>> {
        let descriptor = format!("addr({address})");
        let params = [serde_json::json!("start"), serde_json::json!([descriptor])];
        let res = self
            .bitcoind_rpc_client
            .call_method::<ScanTxOutSetResponse>("scantxoutset", &params)
            .await
            .map_err(|e| std::io::Error::other(e.to_string()))?;

        let mut utxos = Vec::new();
        for unspent in res.unspents {
            let txid = bitcoin::Txid::from_str(&unspent.txid)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
            let amount = Amount::from_btc(unspent.amount)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
            utxos.push(ScannedUtxo {
                txid,
                vout: unspent.vout,
                value_sat: amount.to_sat(),
                script_pubkey_hex: unspent.script_pubkey,
            });
        }
        Ok(utxos)
    }
}

impl FeeEstimator for BitcoindClient {
    fn get_est_sat_per_1000_weight(&self, confirmation_target: ConfirmationTarget) -> u32 {
        let fee = self
            .fees
            .get(&confirmation_target)
            .unwrap()
            .load(Ordering::Acquire);
        #[cfg(test)]
        let fee = mock_fee(fee);
        fee
    }
}

impl BroadcasterInterface for BitcoindClient {
    fn broadcast_transactions(&self, txs: &[&Transaction]) {
        // As of Bitcoin Core 28, using `submitpackage` allows us to broadcast multiple
        // transactions at once and have them propagate through the network as a whole, avoiding
        // some pitfalls with anchor channels where the first transaction doesn't make it into the
        // mempool at all. Several older versions of Bitcoin Core also support `submitpackage`,
        // however, so we just use it unconditionally here.
        // Sadly, Bitcoin Core has an arbitrary restriction on `submitpackage` - it must actually
        // contain a package (see https://github.com/bitcoin/bitcoin/issues/31085).
        let txn = txs.iter().map(encode::serialize_hex).collect::<Vec<_>>();
        let bitcoind_rpc_client = Arc::clone(&self.bitcoind_rpc_client);
        let logger = Arc::clone(&self.logger);
        self.handle.spawn(async move {
			let res = if txn.len() == 1 {
				let tx_json = serde_json::json!(txn[0]);
				bitcoind_rpc_client
					.call_method::<serde_json::Value>("sendrawtransaction", &[tx_json])
					.await
			} else {
				let tx_json = serde_json::json!(txn);
				bitcoind_rpc_client
					.call_method::<serde_json::Value>("submitpackage", &[tx_json])
					.await
			};
			// This may error due to RL calling `broadcast_transactions` with the same transaction
			// multiple times, but the error is safe to ignore.
			match res {
				Ok(_) => {}
				Err(e) => {
					let err_str = e.get_ref().unwrap().to_string();
					log_warn!(logger,
						"Warning, failed to broadcast a transaction, this is likely okay but may indicate an error: {}\nTransactions: {:?}",
						err_str,
						txn);
					print!("Warning, failed to broadcast a transaction, this is likely okay but may indicate an error: {err_str}\n> ");
				}
			}
        });
    }
}
