use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

use bitcoin::{hex::DisplayHex, io::ErrorKind, Transaction};
use lightning::{
    chain::chaininterface::BroadcasterInterface, events::bump_transaction::BumpTransactionEvent,
    ln::types::ChannelId, util::persist::KVStoreSync,
};
use serde::{Deserialize, Serialize};

use crate::{
    ldk::BumpTxEventHandler, ldk_chain_backend::DynBroadcaster, synced_kv_store::SyncedKvStore,
};

const STATUS_KEY: &str = "channel_close";
const STATUS_NAMESPACE: &str = "cpfp";
const MAX_TRACKED_CPFP_ATTEMPTS_PER_NODE: usize = 64;
const PACKAGE_UNAVAILABLE_REASON: &str =
    "anchor CPFP requires a package-capable backend; indexer-backed package submission is deferred";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct ChannelCloseBumpRecord {
    pub(crate) backend: String,
    pub(crate) channel_id: String,
    pub(crate) child_txid: Option<String>,
    pub(crate) claim_id: String,
    pub(crate) commitment_txid: String,
    pub(crate) created_at: u64,
    pub(crate) last_error: Option<String>,
    pub(crate) peer_pubkey: String,
    pub(crate) status: String,
    pub(crate) target_feerate_sat_per_1000_weight: u32,
    pub(crate) updated_at: u64,
}

pub(crate) struct CpfpBroadcaster {
    inner: Arc<DynBroadcaster>,
    state: Arc<CpfpState>,
}

pub(crate) struct CpfpState {
    attempt: tokio::sync::Mutex<()>,
    backend: &'static str,
    broadcaster: Arc<DynBroadcaster>,
    capture: Mutex<Option<(String, Vec<Transaction>)>>,
    events: Mutex<HashMap<String, BumpTransactionEvent>>,
    kv_store: Arc<SyncedKvStore>,
    pending_packages: Mutex<HashMap<String, (u32, Vec<Transaction>)>>,
    package_capable: bool,
    records: Mutex<HashMap<String, ChannelCloseBumpRecord>>,
}

impl CpfpState {
    pub(crate) fn event_for(
        &self,
        channel_id: &ChannelId,
        peer_pubkey: &str,
    ) -> Option<BumpTransactionEvent> {
        let record = self.record_for(channel_id)?;
        if record.peer_pubkey != peer_pubkey {
            return None;
        }
        self.events
            .lock()
            .unwrap()
            .get(&attempt_key(
                &record.channel_id,
                &record.commitment_txid,
                &record.claim_id,
            ))
            .cloned()
    }

    pub(crate) async fn handle_event(
        &self,
        event: &BumpTransactionEvent,
        handler: &BumpTxEventHandler,
    ) -> Result<(), std::io::Error> {
        let BumpTransactionEvent::ChannelClose {
            channel_id,
            claim_id,
            commitment_tx,
            package_target_feerate_sat_per_1000_weight,
            ..
        } = event
        else {
            handler.handle_event(event).await;
            return Ok(());
        };
        if !self.package_capable {
            let _attempt = self.attempt.lock().await;
            self.register_event(event)?;
            self.broadcaster.broadcast_transactions(&[commitment_tx]);
            self.update(
                &attempt_key(
                    &channel_id.0.as_hex().to_string(),
                    &commitment_tx.compute_txid().to_string(),
                    &claim_id.0.as_hex().to_string(),
                ),
                "unavailable",
                None,
                Some(PACKAGE_UNAVAILABLE_REASON.to_string()),
            )?;
            tracing::warn!(
                reason = PACKAGE_UNAVAILABLE_REASON,
                "Anchor CPFP is unavailable for this backend; broadcasting only the commitment"
            );
            return Ok(());
        }
        let _attempt = self.attempt.lock().await;
        self.register_event(event)?;
        let txid = commitment_tx.compute_txid().to_string();
        let target = *package_target_feerate_sat_per_1000_weight;
        let attempt_key = attempt_key(
            &channel_id.0.as_hex().to_string(),
            &txid,
            &claim_id.0.as_hex().to_string(),
        );
        let cached = self
            .pending_packages
            .lock()
            .unwrap()
            .get(&attempt_key)
            .filter(|(cached_target, _)| *cached_target == target)
            .map(|(_, txs)| txs.clone());
        if let Some(txs) = cached {
            let child = txs.get(1).map(|tx| tx.compute_txid().to_string());
            return match self.broadcaster.submit_transactions(txs).await {
                Ok(()) => {
                    self.pending_packages.lock().unwrap().remove(&attempt_key);
                    self.update(&attempt_key, "accepted", child, None)
                }
                Err(error) => self.update(&attempt_key, "retryable_failure", child, Some(error)),
            };
        }
        self.pending_packages.lock().unwrap().remove(&attempt_key);
        self.update(&attempt_key, "constructing", None, None)?;
        *self.capture.lock().unwrap() = Some((txid.clone(), Vec::new()));
        handler.handle_event(event).await;
        let (_, txs) = self.capture.lock().unwrap().take().unwrap();
        let child = txs.get(1).map(|tx| tx.compute_txid().to_string());
        if txs.len() > 1 {
            self.pending_packages
                .lock()
                .unwrap()
                .insert(attempt_key.clone(), (target, txs.clone()));
        }
        let result = if txs.is_empty() {
            Err("LDK did not construct a transaction for the CPFP event".to_string())
        } else {
            self.broadcaster.submit_transactions(txs).await
        };
        match result {
            Ok(()) => {
                self.pending_packages.lock().unwrap().remove(&attempt_key);
                self.update(&attempt_key, "accepted", child, None)
            }
            Err(error) => self.update(&attempt_key, "retryable_failure", child, Some(error)),
        }
    }

    pub(crate) fn load(
        kv_store: Arc<SyncedKvStore>,
        package_capable: bool,
        backend: &'static str,
        broadcaster: Arc<DynBroadcaster>,
    ) -> Result<Arc<Self>, std::io::Error> {
        let mut records = match kv_store.read(STATUS_NAMESPACE, "", STATUS_KEY) {
            Ok(bytes) => {
                let stored: HashMap<String, ChannelCloseBumpRecord> =
                    serde_json::from_slice(&bytes).map_err(std::io::Error::other)?;
                stored
                    .into_values()
                    .map(|record| {
                        let key = attempt_key(
                            &record.channel_id,
                            &record.commitment_txid,
                            &record.claim_id,
                        );
                        (key, record)
                    })
                    .collect()
            }
            Err(e) if e.kind() == ErrorKind::NotFound => HashMap::new(),
            Err(e) => return Err(e.into()),
        };
        prune_records(&mut records, None);
        Ok(Arc::new(Self {
            attempt: tokio::sync::Mutex::new(()),
            backend,
            broadcaster,
            capture: Mutex::new(None),
            events: Mutex::new(HashMap::new()),
            kv_store,
            pending_packages: Mutex::new(HashMap::new()),
            package_capable,
            records: Mutex::new(records),
        }))
    }

    pub(crate) fn package_capable(&self) -> bool {
        self.package_capable
    }

    fn persist(
        &self,
        records: &HashMap<String, ChannelCloseBumpRecord>,
    ) -> Result<(), std::io::Error> {
        self.kv_store
            .write(
                STATUS_NAMESPACE,
                "",
                STATUS_KEY,
                serde_json::to_vec(records).map_err(std::io::Error::other)?,
            )
            .map_err(Into::into)
    }

    pub(crate) fn record_for(&self, channel_id: &ChannelId) -> Option<ChannelCloseBumpRecord> {
        let channel_id = channel_id.0.as_hex().to_string();
        self.records
            .lock()
            .unwrap()
            .values()
            .filter(|record| record.channel_id == channel_id)
            .max_by_key(|record| record.updated_at)
            .cloned()
    }

    pub(crate) fn register_event(
        &self,
        event: &BumpTransactionEvent,
    ) -> Result<(), std::io::Error> {
        let BumpTransactionEvent::ChannelClose {
            channel_id,
            counterparty_node_id,
            claim_id,
            package_target_feerate_sat_per_1000_weight,
            commitment_tx,
            ..
        } = event
        else {
            return Ok(());
        };
        let channel_id = channel_id.0.as_hex().to_string();
        let commitment_txid = commitment_tx.compute_txid().to_string();
        let claim_id = claim_id.0.as_hex().to_string();
        let attempt_key = attempt_key(&channel_id, &commitment_txid, &claim_id);
        let mut records = self.records.lock().unwrap();
        let mut next = records.clone();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let record = next
            .entry(attempt_key.clone())
            .or_insert_with(|| ChannelCloseBumpRecord {
                backend: self.backend.to_string(),
                channel_id: channel_id.clone(),
                child_txid: None,
                claim_id: claim_id.clone(),
                commitment_txid: commitment_txid.clone(),
                created_at: now,
                last_error: None,
                peer_pubkey: counterparty_node_id.to_string(),
                status: "pending".to_string(),
                target_feerate_sat_per_1000_weight: *package_target_feerate_sat_per_1000_weight,
                updated_at: now,
            });
        record.backend = self.backend.to_string();
        record.claim_id = claim_id;
        record.commitment_txid = commitment_txid;
        record.peer_pubkey = counterparty_node_id.to_string();
        record.target_feerate_sat_per_1000_weight = *package_target_feerate_sat_per_1000_weight;
        record.updated_at = now;
        prune_records(&mut next, Some(&attempt_key));
        self.persist(&next)?;
        *records = next;
        let retained: HashSet<String> = records.keys().cloned().collect();
        self.events
            .lock()
            .unwrap()
            .retain(|key, _| retained.contains(key));
        self.pending_packages
            .lock()
            .unwrap()
            .retain(|key, _| retained.contains(key));
        self.events
            .lock()
            .unwrap()
            .insert(attempt_key, event.clone());
        Ok(())
    }

    fn update(
        &self,
        attempt_key: &str,
        status: &str,
        child: Option<String>,
        error: Option<String>,
    ) -> Result<(), std::io::Error> {
        let mut records = self.records.lock().unwrap();
        let mut next = records.clone();
        let record = next
            .get_mut(attempt_key)
            .ok_or_else(|| std::io::Error::other("CPFP record missing"))?;
        record.child_txid = child;
        record.last_error = error;
        record.status = status.to_string();
        record.updated_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        self.persist(&next)?;
        *records = next;
        Ok(())
    }
}

fn attempt_key(channel_id: &str, commitment_txid: &str, claim_id: &str) -> String {
    format!("{}:{}:{}", channel_id, commitment_txid, claim_id)
}

fn prune_records(
    records: &mut HashMap<String, ChannelCloseBumpRecord>,
    protected_key: Option<&str>,
) {
    while records.len() > MAX_TRACKED_CPFP_ATTEMPTS_PER_NODE {
        let oldest = records
            .iter()
            .filter(|(key, _)| Some(key.as_str()) != protected_key)
            .min_by_key(|(_, record)| (record.updated_at, record.created_at))
            .map(|(key, _)| key.clone());
        let Some(oldest) = oldest else {
            break;
        };
        records.remove(&oldest);
    }
}

impl CpfpBroadcaster {
    pub(crate) fn new(inner: Arc<DynBroadcaster>, state: Arc<CpfpState>) -> Arc<Self> {
        Arc::new(Self { inner, state })
    }
}

impl BroadcasterInterface for CpfpBroadcaster {
    fn broadcast_transactions(&self, txs: &[&Transaction]) {
        if let Some((txid, captured)) = self.state.capture.lock().unwrap().as_mut() {
            if txs
                .first()
                .is_some_and(|tx| tx.compute_txid().to_string() == *txid)
            {
                *captured = txs.iter().map(|tx| (*tx).clone()).collect();
                return;
            }
        }
        self.inner.broadcast_transactions(txs);
    }
}
