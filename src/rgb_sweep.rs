//! Durable RGB sweep preparation, isolated reconciliation and operator recovery.
//!
//! Callers serialize preparation, reconciliation and recovery using the wallet's sweep lock.

use crate::{error::APIError, utils::AppState};
use bitcoin::{
    hashes::{sha256, Hash},
    io,
};
use lightning::util::persist::KVStoreSync;
use rgb_lib::{
    bitcoin::psbt::Psbt, wallet::rust_only::ColorPrepareResult, ConsignmentExt, FileContent,
    TransferStatus,
};
use serde::{Deserialize, Serialize};
use std::{collections::HashSet, fs, str::FromStr, sync::Arc};

const ACTIVE: &str = "rgb_sweeps";
const QUARANTINE: &str = "rgb_sweeps_quarantine";
const ARCHIVE: &str = "rgb_sweeps_archive";
const CLOSED: &str = "rgb_sweeps_closed";
const UNSIGNED: &str = "rgb_sweeps_unsigned";

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct PreparedRgbSweep {
    pub(crate) psbt: String,
    pub(crate) batch_transfer_idx: i32,
    pub(crate) consignments: Vec<(String, Vec<u8>)>,
}

#[derive(Debug, Serialize, Deserialize)]
struct QuarantinedSweep {
    raw: Vec<u8>,
    reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RgbSweepQuarantineInfo {
    pub key: String,
    pub record_id: String,
    pub batch_transfer_idx: Option<i32>,
    pub txid: Option<String>,
    pub reason: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RgbSweepRecoveryAction {
    Resume,
    Reprepare,
    Resolve,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RgbSweepRecoveryRequest {
    pub key: String,
    /// Echo the record_id from list_rgb_sweep_quarantine to reject stale recovery requests.
    pub record_id: String,
    pub action: RgbSweepRecoveryAction,
}

pub(crate) struct SweepBatch {
    pub status: TransferStatus,
    pub txid: Option<String>,
}

pub(crate) enum ConsumeError {
    Retry(String),
    Quarantine(String),
}

pub(crate) trait SweepBackend {
    fn batch(&mut self, idx: i32, txid: &str) -> Result<Option<SweepBatch>, String>;
    fn confirmed(&mut self, txid: &str) -> Result<bool, String>;
    fn consume(&mut self, sweep: &PreparedRgbSweep) -> Result<(), ConsumeError>;
}

#[derive(Debug, Default)]
pub(crate) struct ReconcileSummary {
    pub completed: usize,
    pub quarantined: usize,
    pub errors: usize,
}

fn record_id(raw: &[u8]) -> String {
    sha256::Hash::hash(raw).to_string()
}

fn read_raw(store: &impl KVStoreSync, ns: &str, key: &str) -> Result<Option<Vec<u8>>, String> {
    match store.read(ns, "", key) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("cannot read {ns}/{key}: {e}")),
    }
}

fn write(store: &impl KVStoreSync, ns: &str, key: &str, raw: Vec<u8>) -> Result<(), String> {
    store
        .write(ns, "", key, raw)
        .map_err(|e| format!("cannot write {ns}/{key}: {e}"))
}

fn remove(store: &impl KVStoreSync, ns: &str, key: &str) -> Result<(), String> {
    store
        .remove(ns, "", key, false)
        .map_err(|e| format!("cannot remove {ns}/{key}: {e}"))
}

impl PreparedRgbSweep {
    pub(crate) fn new(psbt: String, prepared: ColorPrepareResult) -> Result<Self, String> {
        let dir = tempfile::tempdir().map_err(|e| e.to_string())?;
        let mut consignments = Vec::with_capacity(prepared.transfers.len());
        for consignment in prepared.transfers {
            let asset_id = consignment.contract_id().to_string();
            let path = dir.path().join("consignment");
            consignment.save_file(&path).map_err(|e| e.to_string())?;
            consignments.push((asset_id, fs::read(&path).map_err(|e| e.to_string())?));
        }
        Ok(Self {
            psbt,
            batch_transfer_idx: prepared.batch_transfer_idx,
            consignments,
        })
    }

    pub(crate) fn txid(&self) -> Result<String, String> {
        Ok(Psbt::from_str(&self.psbt)
            .map_err(|e| format!("invalid sweep PSBT: {e}"))?
            .unsigned_tx
            .compute_txid()
            .to_string())
    }

    pub(crate) fn read(store: &impl KVStoreSync, key: &str) -> Result<Option<Self>, String> {
        read_raw(store, ACTIVE, key)?
            .map(|raw| {
                bincode::deserialize(&raw)
                    .map_err(|e| format!("invalid prepared RGB sweep {key}: {e}"))
            })
            .transpose()
    }

    pub(crate) fn persist_new(&self, store: &impl KVStoreSync, key: &str) -> Result<(), String> {
        let raw = bincode::serialize(self).map_err(|e| e.to_string())?;
        write(store, ACTIVE, key, raw.clone())?;
        // Removed before calling any signer. Absence (including legacy records) is NOT proof
        // that no signature exists. VSS restore invalidates these proofs before starting LDK.
        write(store, UNSIGNED, key, record_id(&raw).into_bytes())
    }

    pub(crate) fn begin_signing(store: &impl KVStoreSync, key: &str) -> Result<(), String> {
        remove(store, UNSIGNED, key)
    }
}

pub(crate) fn ensure_spend_allowed(store: &impl KVStoreSync, key: &str) -> Result<(), String> {
    if read_raw(store, QUARANTINE, key)?.is_some() || read_raw(store, CLOSED, key)?.is_some() {
        return Err(format!(
            "RGB sweep {key} requires operator recovery or is already resolved"
        ));
    }
    Ok(())
}

// LDK can regroup/reorder descriptors between attempts, changing descriptors_hash. Fence the
// actual inputs as well as the original key, including after an operator resolves a sweep.
pub(crate) fn ensure_inputs_spend_allowed(
    store: &impl KVStoreSync,
    inputs: &HashSet<bitcoin::OutPoint>,
) -> Result<(), String> {
    for namespace in [QUARANTINE, CLOSED] {
        for key in store.list(namespace, "").map_err(|e| e.to_string())? {
            let Some(raw) = read_raw(store, namespace, &key)? else {
                continue;
            };
            let raw = if namespace == QUARANTINE {
                bincode::deserialize::<QuarantinedSweep>(&raw)
                    .map_err(|e| format!("cannot inspect quarantine {key}: {e}"))?
                    .raw
            } else {
                raw
            };
            let sweep: PreparedRgbSweep = bincode::deserialize(&raw).map_err(|_| {
                format!("restore corrupt sweep {key} before spending: its inputs are unknown")
            })?;
            let psbt = Psbt::from_str(&sweep.psbt)
                .map_err(|e| format!("cannot inspect sweep {key} inputs: {e}"))?;
            if psbt
                .unsigned_tx
                .input
                .iter()
                .any(|i| inputs.contains(&i.previous_output))
            {
                return Err(format!(
                    "inputs belong to quarantined or resolved RGB sweep {key}"
                ));
            }
        }
    }
    Ok(())
}

pub(crate) fn invalidate_unsigned_proofs(store: &impl KVStoreSync) -> Result<(), String> {
    for key in store.list(UNSIGNED, "").map_err(|e| e.to_string())? {
        remove(store, UNSIGNED, &key)?;
    }
    Ok(())
}

fn quarantine(
    store: &impl KVStoreSync,
    key: &str,
    raw: Vec<u8>,
    reason: String,
) -> Result<(), String> {
    let sweep = bincode::deserialize::<PreparedRgbSweep>(&raw).ok();
    tracing::error!(key, batch_transfer_idx = ?sweep.as_ref().map(|s| s.batch_transfer_idx),
        txid = ?sweep.as_ref().and_then(|s| s.txid().ok()), %reason, "quarantining RGB sweep");
    let entry = QuarantinedSweep { raw, reason };
    write(
        store,
        QUARANTINE,
        key,
        bincode::serialize(&entry).map_err(|e| e.to_string())?,
    )?;
    remove(store, ACTIVE, key)
}

// No individual receipt, database, indexer or storage failure may prevent ordinary wallet refresh.
pub(crate) fn reconcile_prepared_sweeps(
    store: &impl KVStoreSync,
    backend: &mut impl SweepBackend,
) -> ReconcileSummary {
    let mut summary = ReconcileSummary::default();
    let keys = match store.list(ACTIVE, "") {
        Ok(keys) => keys,
        Err(e) => {
            tracing::error!(error = %e, "cannot list prepared RGB sweeps; continuing wallet refresh");
            summary.errors += 1;
            return summary;
        }
    };
    for key in keys {
        if let Err(error) = reconcile_one(store, backend, &key, &mut summary) {
            summary.errors += 1;
            tracing::error!(key, %error, "RGB sweep reconciliation failed; continuing with other sweeps");
        }
    }
    summary
}

fn reconcile_one(
    store: &impl KVStoreSync,
    backend: &mut impl SweepBackend,
    key: &str,
    summary: &mut ReconcileSummary,
) -> Result<(), String> {
    let Some(raw) = read_raw(store, ACTIVE, key)? else {
        return Ok(());
    };
    // A previous quarantine write may have succeeded while removal failed. Never consume it.
    if let Some(saved) = read_raw(store, QUARANTINE, key)? {
        let entry: QuarantinedSweep = bincode::deserialize(&saved).map_err(|e| e.to_string())?;
        if entry.raw != raw {
            return Err("active and quarantined sweep receipts differ".into());
        }
        remove(store, ACTIVE, key)?;
        return Ok(());
    }
    let decoded = bincode::deserialize::<PreparedRgbSweep>(&raw)
        .map_err(|e| e.to_string())
        .and_then(|sweep| sweep.txid().map(|txid| (sweep, txid)));
    let (sweep, txid) = match decoded {
        Ok(pair) => pair,
        Err(reason) => {
            quarantine(store, key, raw, reason)?;
            summary.quarantined += 1;
            return Ok(());
        }
    };
    let batch = backend.batch(sweep.batch_transfer_idx, &txid)?;
    let reason = match batch {
        None => Some("batch not found".into()),
        Some(batch) if batch.txid.as_deref() != Some(txid.as_str()) => {
            Some("batch txid differs from prepared PSBT".into())
        }
        Some(batch) => match batch.status {
            TransferStatus::WaitingConfirmations | TransferStatus::Settled => {
                remove(store, ACTIVE, key)?;
                summary.completed += 1;
                return Ok(());
            }
            TransferStatus::Initiated => {
                if !backend.confirmed(&txid)? {
                    return Ok(());
                }
                match backend.consume(&sweep) {
                    Ok(()) => {
                        remove(store, ACTIVE, key)?;
                        summary.completed += 1;
                        return Ok(());
                    }
                    Err(ConsumeError::Retry(error)) => return Err(error),
                    Err(ConsumeError::Quarantine(reason)) => Some(reason),
                }
            }
            status => Some(format!("batch is {status:?}")),
        },
    };
    if let Some(reason) = reason {
        quarantine(store, key, raw, reason)?;
        summary.quarantined += 1;
    }
    Ok(())
}

pub(crate) fn list_quarantine(
    store: &impl KVStoreSync,
) -> Result<Vec<RgbSweepQuarantineInfo>, String> {
    let mut records = Vec::new();
    for key in store.list(QUARANTINE, "").map_err(|e| e.to_string())? {
        let Some(raw) = read_raw(store, QUARANTINE, &key)? else {
            continue;
        };
        let entry: QuarantinedSweep = bincode::deserialize(&raw)
            .map_err(|e| format!("invalid quarantine envelope {key}: {e}"))?;
        let sweep = bincode::deserialize::<PreparedRgbSweep>(&entry.raw).ok();
        records.push(RgbSweepQuarantineInfo {
            key,
            record_id: record_id(&entry.raw),
            batch_transfer_idx: sweep.as_ref().map(|s| s.batch_transfer_idx),
            txid: sweep.and_then(|s| s.txid().ok()),
            reason: entry.reason,
        });
    }
    records.sort_by(|a, b| a.key.cmp(&b.key));
    Ok(records)
}

pub(crate) fn recover(
    store: &impl KVStoreSync,
    backend: &mut impl SweepBackend,
    request: &RgbSweepRecoveryRequest,
    check_no_broadcast: impl FnOnce(&PreparedRgbSweep) -> Result<(), String>,
) -> Result<(), String> {
    let key = &request.key;
    if key.parse::<u64>().map(|v| v.to_string()).ok().as_ref() != Some(key) {
        return Err("invalid sweep key".into());
    }
    let saved = read_raw(store, QUARANTINE, key)?.ok_or("quarantined sweep not found")?;
    let entry: QuarantinedSweep = bincode::deserialize(&saved).map_err(|e| e.to_string())?;
    if record_id(&entry.raw) != request.record_id {
        return Err("stale quarantine record_id".into());
    }
    if let Some(active) = read_raw(store, ACTIVE, key)? {
        if active != entry.raw {
            return Err("active and quarantined sweep receipts differ".into());
        }
    }
    if request.action != RgbSweepRecoveryAction::Resolve && read_raw(store, CLOSED, key)?.is_some()
    {
        return Err("this sweep has already been resolved".into());
    }
    let sweep: PreparedRgbSweep = bincode::deserialize(&entry.raw).map_err(|_| {
        "corrupt receipt requires restoration from a valid backup; automatic release is unsafe"
    })?;
    let txid = sweep.txid()?;
    let batch = backend
        .batch(sweep.batch_transfer_idx, &txid)?
        .ok_or("restore the missing batch before recovery")?;
    if batch.txid.as_deref() != Some(txid.as_str()) {
        return Err("batch txid differs from prepared PSBT".into());
    }
    match request.action {
        RgbSweepRecoveryAction::Resume => {
            if !matches!(
                batch.status,
                TransferStatus::Initiated
                    | TransferStatus::WaitingConfirmations
                    | TransferStatus::Settled
            ) {
                return Err("resume requires an active or consumed batch".into());
            }
        }
        RgbSweepRecoveryAction::Reprepare => {
            if batch.status != TransferStatus::Failed {
                return Err("reprepare requires an explicitly failed batch".into());
            }
            if read_raw(store, UNSIGNED, key)?.as_deref() != Some(request.record_id.as_bytes()) {
                return Err("cannot prove that signing was never attempted; restore/resume the original sweep".into());
            }
            check_no_broadcast(&sweep)?;
            if backend.confirmed(&txid)? {
                return Err("cannot reprepare a confirmed sweep".into());
            }
        }
        RgbSweepRecoveryAction::Resolve => {
            if !matches!(
                batch.status,
                TransferStatus::WaitingConfirmations | TransferStatus::Settled
            ) || !backend.confirmed(&txid)?
            {
                return Err(
                    "resolve requires a confirmed transaction and an already consumed batch".into(),
                );
            }
        }
    }
    // Keep the original bytes and the operator's decision before changing any active state.
    let archive_key = format!("{key}_{}_{:?}", request.record_id, request.action);
    write(store, ARCHIVE, &archive_key, saved)?;
    match request.action {
        RgbSweepRecoveryAction::Resume => write(store, ACTIVE, key, entry.raw)?,
        RgbSweepRecoveryAction::Reprepare => remove(store, ACTIVE, key)?,
        RgbSweepRecoveryAction::Resolve => {
            write(store, CLOSED, key, entry.raw)?;
            remove(store, ACTIVE, key)?;
        }
    }
    // Last: until this succeeds the spender remains fenced. All preceding writes are retryable.
    remove(store, QUARANTINE, key)?;
    tracing::warn!(key, %txid, batch_transfer_idx = sweep.batch_transfer_idx, action = ?request.action, "operator recovered RGB sweep");
    Ok(())
}

pub(crate) async fn list_rgb_sweep_quarantine(
    state: Arc<AppState>,
) -> Result<Vec<crate::rgb_sweep::RgbSweepQuarantineInfo>, APIError> {
    state.check_lightning_supported()?;
    let guard = check_recovery_unlocked(&state).await?;
    let common = guard.as_ref().unwrap().common.clone();
    tokio::task::spawn_blocking(move || {
        let _guard = common
            .rgb_wallet_wrapper
            .sweep_lock
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        crate::rgb_sweep::list_quarantine(common.kv_store.as_ref()).map_err(APIError::Unexpected)
    })
    .await
    .map_err(|e| APIError::Unexpected(format!("sweep inspection task failed: {e}")))?
}

pub(crate) async fn recover_rgb_sweep(
    state: Arc<AppState>,
    request: crate::rgb_sweep::RgbSweepRecoveryRequest,
) -> Result<(), APIError> {
    crate::utils::no_cancel(async move {
        state.check_lightning_supported()?;
        let guard = check_recovery_unlocked(&state).await?;
        let lightning = guard.as_ref().unwrap().lightning()?.clone();
        tokio::task::spawn_blocking(move || {
            // Snapshot BEFORE acquiring sweep_lock: LDK may hold its sweeper mutex while
            // invoking our OutputSpender, which takes sweep_lock. The reverse order deadlocks.
            let tracked = if request.action == crate::rgb_sweep::RgbSweepRecoveryAction::Reprepare {
                lightning.output_sweeper.tracked_spendable_outputs()
            } else {
                Vec::new()
            };
            let _guard = lightning
                .rgb_wallet_wrapper
                .sweep_lock
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            crate::rgb_sweep::recover(
                lightning.kv_store.as_ref(),
                &mut crate::rgb::WalletSweepBackend(&lightning.rgb_wallet_wrapper),
                &request,
                |prepared| {
                    crate::ldk::check_sweep_not_broadcast(
                        lightning.kv_store.as_ref(),
                        &tracked,
                        prepared,
                    )
                },
            )
            .map_err(APIError::InvalidRequest)
        })
        .await
        .map_err(|e| APIError::Unexpected(format!("sweep recovery task failed: {e}")))?
    })
    .await
}

async fn check_recovery_unlocked(
    state: &Arc<AppState>,
) -> Result<tokio::sync::MutexGuard<'_, Option<Arc<crate::utils::UnlockedAppState>>>, APIError> {
    if *state.changing_state.lock().unwrap() {
        return Err(APIError::ChangingState);
    }
    let guard = state.unlocked_app_state.lock().await;
    if *state.changing_state.lock().unwrap() {
        return Err(APIError::ChangingState);
    }
    if guard.is_none() {
        return Err(APIError::LockedNode);
    }
    Ok(guard)
}

#[cfg(test)]
mod tests;
