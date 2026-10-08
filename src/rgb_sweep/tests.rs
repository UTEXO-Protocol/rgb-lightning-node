use super::*;
use bitcoin::{absolute, transaction, Amount, OutPoint, ScriptBuf, Transaction, TxIn, TxOut};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    sync::Mutex,
};

#[derive(Default)]
struct Store {
    entries: Mutex<BTreeMap<(String, String), Vec<u8>>>,
    fail_write: Mutex<HashSet<String>>,
    fail_remove: Mutex<HashSet<String>>,
    fail_read: Mutex<HashSet<String>>,
    fail_list: Mutex<bool>,
}
impl KVStoreSync for Store {
    fn read(&self, ns: &str, _: &str, key: &str) -> Result<Vec<u8>, io::Error> {
        if self.fail_read.lock().unwrap().contains(key) {
            return Err(io::Error::new(io::ErrorKind::Other, "read failed"));
        }
        self.entries
            .lock()
            .unwrap()
            .get(&(ns.into(), key.into()))
            .cloned()
            .ok_or_else(|| io::ErrorKind::NotFound.into())
    }
    fn write(&self, ns: &str, _: &str, key: &str, value: Vec<u8>) -> Result<(), io::Error> {
        if self.fail_write.lock().unwrap().contains(ns) {
            return Err(io::Error::new(io::ErrorKind::Other, "write failed"));
        }
        self.entries
            .lock()
            .unwrap()
            .insert((ns.into(), key.into()), value);
        Ok(())
    }
    fn remove(&self, ns: &str, _: &str, key: &str, _: bool) -> Result<(), io::Error> {
        if self.fail_remove.lock().unwrap().contains(ns) {
            return Err(io::Error::new(io::ErrorKind::Other, "remove failed"));
        }
        self.entries
            .lock()
            .unwrap()
            .remove(&(ns.into(), key.into()));
        Ok(())
    }
    fn list(&self, ns: &str, _: &str) -> Result<Vec<String>, io::Error> {
        if *self.fail_list.lock().unwrap() {
            return Err(io::Error::new(io::ErrorKind::Other, "list failed"));
        }
        Ok(self
            .entries
            .lock()
            .unwrap()
            .keys()
            .filter(|(n, _)| n == ns)
            .map(|(_, key)| key.clone())
            .collect())
    }
}

#[derive(Default)]
struct Backend {
    statuses: HashMap<i32, TransferStatus>,
    missing: HashSet<i32>,
    mismatch: bool,
    lookup_error: HashSet<i32>,
    indexer_error: HashSet<String>,
    unconfirmed: bool,
    consume_error: HashSet<i32>,
    missing_files: bool,
    consumed: Vec<i32>,
}
impl SweepBackend for Backend {
    fn batch(&mut self, idx: i32, txid: &str) -> Result<Option<SweepBatch>, String> {
        if self.lookup_error.contains(&idx) {
            return Err("database offline".into());
        }
        if self.missing.contains(&idx) {
            return Ok(None);
        }
        Ok(Some(SweepBatch {
            status: self
                .statuses
                .get(&idx)
                .copied()
                .unwrap_or(TransferStatus::Initiated),
            txid: Some(if self.mismatch { "different" } else { txid }.into()),
        }))
    }
    fn confirmed(&mut self, txid: &str) -> Result<bool, String> {
        if self.indexer_error.contains(txid) {
            return Err("indexer offline".into());
        }
        Ok(!self.unconfirmed)
    }
    fn consume(&mut self, sweep: &PreparedRgbSweep) -> Result<(), ConsumeError> {
        if self.missing_files {
            return Err(ConsumeError::Quarantine("missing fascia".into()));
        }
        if self.consume_error.contains(&sweep.batch_transfer_idx) {
            return Err(ConsumeError::Retry("indexer lag".into()));
        }
        self.consumed.push(sweep.batch_transfer_idx);
        self.statuses.insert(
            sweep.batch_transfer_idx,
            TransferStatus::WaitingConfirmations,
        );
        Ok(())
    }
}

fn fixture(idx: i32) -> PreparedRgbSweep {
    let psbt = Psbt::from_unsigned_tx(Transaction {
        version: transaction::Version::TWO,
        lock_time: absolute::LockTime::ZERO,
        input: vec![TxIn {
            previous_output: OutPoint::new(bitcoin::Txid::from_byte_array([1; 32]), idx as u32),
            ..Default::default()
        }],
        output: vec![TxOut {
            value: Amount::from_sat(1000),
            script_pubkey: ScriptBuf::new(),
        }],
    })
    .unwrap();
    PreparedRgbSweep {
        psbt: psbt.to_string(),
        batch_transfer_idx: idx,
        consignments: vec![("asset".into(), vec![1, 2, 3])],
    }
}

fn request(store: &Store, action: RgbSweepRecoveryAction) -> RgbSweepRecoveryRequest {
    let info = list_quarantine(store).unwrap().remove(0);
    RgbSweepRecoveryRequest {
        key: info.key,
        record_id: info.record_id,
        action,
    }
}

#[test]
fn corrupt_receipt_is_quarantined_without_blocking_valid_sweep() {
    let store = Store::default();
    store.write(ACTIVE, "", "1", vec![0xff]).unwrap();
    fixture(2).persist_new(&store, "2").unwrap();
    let mut backend = Backend::default();
    let summary = reconcile_prepared_sweeps(&store, &mut backend);
    assert_eq!(
        (summary.completed, summary.quarantined, summary.errors),
        (1, 1, 0)
    );
    assert_eq!(backend.consumed, [2]);
    let raw = store.read(QUARANTINE, "", "1").unwrap();
    assert_eq!(
        bincode::deserialize::<QuarantinedSweep>(&raw).unwrap().raw,
        [0xff]
    );
    assert!(list_quarantine(&store).unwrap()[0].txid.is_none());
    assert!(ensure_spend_allowed(&store, "1").is_err());
}

#[test]
fn failed_missing_and_mismatched_batches_are_quarantined_before_indexer_lookup() {
    for variant in 0..3 {
        let store = Store::default();
        let sweep = fixture(1);
        sweep.persist_new(&store, "1").unwrap();
        let mut backend = Backend::default();
        backend.indexer_error.insert(sweep.txid().unwrap());
        match variant {
            0 => {
                backend.statuses.insert(1, TransferStatus::Failed);
            }
            1 => {
                backend.missing.insert(1);
            }
            _ => {
                backend.mismatch = true;
            }
        }
        let summary = reconcile_prepared_sweeps(&store, &mut backend);
        assert_eq!(summary.quarantined, 1);
        assert!(backend.consumed.is_empty());
    }
}

#[test]
fn consumed_batches_are_cleaned_without_indexer_or_consume() {
    for status in [
        TransferStatus::WaitingConfirmations,
        TransferStatus::Settled,
    ] {
        let store = Store::default();
        let sweep = fixture(1);
        sweep.persist_new(&store, "1").unwrap();
        let mut backend = Backend::default();
        backend.statuses.insert(1, status);
        backend.indexer_error.insert(sweep.txid().unwrap());
        assert_eq!(reconcile_prepared_sweeps(&store, &mut backend).completed, 1);
        assert!(backend.consumed.is_empty());
        assert!(PreparedRgbSweep::read(&store, "1").unwrap().is_none());
    }
}

#[test]
fn unconfirmed_sweep_retains_exact_psbt_and_consignments() {
    let store = Store::default();
    let sweep = fixture(1);
    sweep.persist_new(&store, "1").unwrap();
    let before = store.read(ACTIVE, "", "1").unwrap();
    let mut backend = Backend {
        unconfirmed: true,
        ..Default::default()
    };
    reconcile_prepared_sweeps(&store, &mut backend);
    assert_eq!(before, store.read(ACTIVE, "", "1").unwrap());
    assert!(backend.consumed.is_empty());
}

#[test]
fn temporary_errors_do_not_block_later_records_and_are_retried() {
    for failure in 0..4 {
        let store = Store::default();
        let first = fixture(1);
        first.persist_new(&store, "1").unwrap();
        fixture(2).persist_new(&store, "2").unwrap();
        let mut backend = Backend::default();
        match failure {
            0 => {
                backend.indexer_error.insert(first.txid().unwrap());
            }
            1 => {
                backend.lookup_error.insert(1);
            }
            2 => {
                backend.consume_error.insert(1);
            }
            _ => {
                store.fail_read.lock().unwrap().insert("1".into());
            }
        }
        let summary = reconcile_prepared_sweeps(&store, &mut backend);
        assert_eq!((summary.completed, summary.errors), (1, 1));
        assert_eq!(backend.consumed, [2]);
        store.fail_read.lock().unwrap().clear();
        assert!(PreparedRgbSweep::read(&store, "1").unwrap().is_some());
        backend.indexer_error.clear();
        backend.lookup_error.clear();
        backend.consume_error.clear();
        assert_eq!(reconcile_prepared_sweeps(&store, &mut backend).completed, 1);
        assert_eq!(backend.consumed, [2, 1]);
    }
}

#[test]
fn list_failure_is_reported_without_propagating_to_refresh() {
    let store = Store::default();
    *store.fail_list.lock().unwrap() = true;
    assert_eq!(
        reconcile_prepared_sweeps(&store, &mut Backend::default()).errors,
        1
    );
}

#[test]
fn missing_preparation_files_are_quarantined() {
    let store = Store::default();
    fixture(1).persist_new(&store, "1").unwrap();
    let mut backend = Backend {
        missing_files: true,
        ..Default::default()
    };
    assert_eq!(
        reconcile_prepared_sweeps(&store, &mut backend).quarantined,
        1
    );
}

#[test]
fn failed_quarantine_write_preserves_active_bytes_and_other_work() {
    let store = Store::default();
    store.write(ACTIVE, "", "1", vec![0xff]).unwrap();
    fixture(2).persist_new(&store, "2").unwrap();
    store.fail_write.lock().unwrap().insert(QUARANTINE.into());
    let summary = reconcile_prepared_sweeps(&store, &mut Backend::default());
    assert_eq!((summary.errors, summary.completed), (1, 1));
    assert_eq!(store.read(ACTIVE, "", "1").unwrap(), [0xff]);
}

#[test]
fn failed_active_removal_keeps_quarantine_fence_and_retries_idempotently() {
    let store = Store::default();
    fixture(1).persist_new(&store, "1").unwrap();
    store.fail_remove.lock().unwrap().insert(ACTIVE.into());
    let mut backend = Backend::default();
    backend.statuses.insert(1, TransferStatus::Failed);
    assert_eq!(reconcile_prepared_sweeps(&store, &mut backend).errors, 1);
    assert!(ensure_spend_allowed(&store, "1").is_err());
    backend.statuses.insert(1, TransferStatus::Initiated);
    store.fail_remove.lock().unwrap().clear();
    assert_eq!(reconcile_prepared_sweeps(&store, &mut backend).errors, 0);
    assert!(backend.consumed.is_empty());
    assert!(PreparedRgbSweep::read(&store, "1").unwrap().is_none());
    assert_eq!(list_quarantine(&store).unwrap().len(), 1);
}

#[test]
fn failed_completion_removal_does_not_consume_twice() {
    let store = Store::default();
    fixture(1).persist_new(&store, "1").unwrap();
    store.fail_remove.lock().unwrap().insert(ACTIVE.into());
    let mut backend = Backend::default();
    assert_eq!(reconcile_prepared_sweeps(&store, &mut backend).errors, 1);
    store.fail_remove.lock().unwrap().clear();
    assert_eq!(reconcile_prepared_sweeps(&store, &mut backend).completed, 1);
    assert_eq!(backend.consumed, [1]);
}

fn quarantined_failed() -> (Store, Backend) {
    let store = Store::default();
    fixture(1).persist_new(&store, "1").unwrap();
    let mut backend = Backend {
        unconfirmed: true,
        ..Default::default()
    };
    backend.statuses.insert(1, TransferStatus::Failed);
    reconcile_prepared_sweeps(&store, &mut backend);
    (store, backend)
}

#[test]
fn reprepare_requires_unsigned_proof_no_broadcast_and_failed_batch() {
    for refused in 0..5 {
        let (store, mut backend) = quarantined_failed();
        let request = request(&store, RgbSweepRecoveryAction::Reprepare);
        match refused {
            0 => {
                PreparedRgbSweep::begin_signing(&store, "1").unwrap();
            }
            1 => {
                invalidate_unsigned_proofs(&store).unwrap();
            }
            2 => {
                backend.unconfirmed = false;
            }
            3 => {
                backend.statuses.insert(1, TransferStatus::Initiated);
            }
            _ => {}
        }
        assert!(recover(&store, &mut backend, &request, |_| {
            if refused == 4 {
                Err("LDK has a signed transaction".into())
            } else {
                Ok(())
            }
        })
        .is_err());
        assert!(ensure_spend_allowed(&store, "1").is_err());
    }
    let (store, mut backend) = quarantined_failed();
    let request = request(&store, RgbSweepRecoveryAction::Reprepare);
    recover(&store, &mut backend, &request, |_| Ok(())).unwrap();
    assert!(ensure_spend_allowed(&store, "1").is_ok());
    assert_eq!(store.list(ARCHIVE, "").unwrap().len(), 1);
}

#[test]
fn recovery_archive_failure_and_stale_request_leave_quarantine_intact() {
    let (store, mut backend) = quarantined_failed();
    let mut request = request(&store, RgbSweepRecoveryAction::Reprepare);
    request.record_id = "stale".into();
    assert!(recover(&store, &mut backend, &request, |_| Ok(())).is_err());
    request.record_id = list_quarantine(&store).unwrap()[0].record_id.clone();
    store.fail_write.lock().unwrap().insert(ARCHIVE.into());
    assert!(recover(&store, &mut backend, &request, |_| Ok(())).is_err());
    assert!(ensure_spend_allowed(&store, "1").is_err());
}

#[test]
fn recovery_removal_failure_is_retryable_without_losing_original() {
    let (store, mut backend) = quarantined_failed();
    let request = request(&store, RgbSweepRecoveryAction::Reprepare);
    store.fail_remove.lock().unwrap().insert(QUARANTINE.into());
    assert!(recover(&store, &mut backend, &request, |_| Ok(())).is_err());
    assert!(ensure_spend_allowed(&store, "1").is_err());
    store.fail_remove.lock().unwrap().clear();
    recover(&store, &mut backend, &request, |_| Ok(())).unwrap();
    assert_eq!(store.list(ARCHIVE, "").unwrap().len(), 1);
}

#[test]
fn resume_restored_batch_preserves_receipt_and_does_not_reset_signing_proof() {
    let (store, mut backend) = quarantined_failed();
    PreparedRgbSweep::begin_signing(&store, "1").unwrap();
    let request = request(&store, RgbSweepRecoveryAction::Resume);
    assert!(recover(&store, &mut backend, &request, |_| unreachable!()).is_err());
    backend.statuses.insert(1, TransferStatus::Initiated);
    recover(&store, &mut backend, &request, |_| unreachable!()).unwrap();
    assert_eq!(
        PreparedRgbSweep::read(&store, "1")
            .unwrap()
            .unwrap()
            .consignments,
        fixture(1).consignments
    );
    assert!(read_raw(&store, UNSIGNED, "1").unwrap().is_none());
}

#[test]
fn resolve_requires_confirmed_consumed_batch_and_keeps_spender_fenced() {
    let (store, mut backend) = quarantined_failed();
    let request = request(&store, RgbSweepRecoveryAction::Resolve);
    assert!(recover(&store, &mut backend, &request, |_| unreachable!()).is_err());
    backend.statuses.insert(1, TransferStatus::Settled);
    assert!(recover(&store, &mut backend, &request, |_| unreachable!()).is_err());
    backend.unconfirmed = false;
    recover(&store, &mut backend, &request, |_| unreachable!()).unwrap();
    assert!(list_quarantine(&store).unwrap().is_empty());
    assert!(ensure_spend_allowed(&store, "1").is_err());
}

#[test]
fn quarantine_and_resolved_inputs_cannot_bypass_fence_with_a_different_key() {
    let (store, mut backend) = quarantined_failed();
    let inputs = Psbt::from_str(&fixture(1).psbt)
        .unwrap()
        .unsigned_tx
        .input
        .into_iter()
        .map(|i| i.previous_output)
        .collect();
    assert!(ensure_spend_allowed(&store, "999").is_ok());
    assert!(ensure_inputs_spend_allowed(&store, &inputs).is_err());
    let unrelated = Psbt::from_str(&fixture(2).psbt)
        .unwrap()
        .unsigned_tx
        .input
        .into_iter()
        .map(|i| i.previous_output)
        .collect();
    assert!(ensure_inputs_spend_allowed(&store, &unrelated).is_ok());
    backend.statuses.insert(1, TransferStatus::Settled);
    backend.unconfirmed = false;
    recover(
        &store,
        &mut backend,
        &request(&store, RgbSweepRecoveryAction::Resolve),
        |_| unreachable!(),
    )
    .unwrap();
    assert!(ensure_inputs_spend_allowed(&store, &inputs).is_err());
    assert!(ensure_inputs_spend_allowed(&store, &unrelated).is_ok());
}

#[test]
fn unknown_quarantined_inputs_prevent_unsafe_new_preparation() {
    let store = Store::default();
    quarantine(&store, "1", vec![255], "corrupt".into()).unwrap();
    assert!(ensure_inputs_spend_allowed(&store, &HashSet::new()).is_err());
}

#[test]
fn recovery_never_overwrites_a_different_active_receipt() {
    let (store, mut backend) = quarantined_failed();
    fixture(2).persist_new(&store, "1").unwrap();
    backend.statuses.insert(1, TransferStatus::Initiated);
    assert!(recover(
        &store,
        &mut backend,
        &request(&store, RgbSweepRecoveryAction::Resume),
        |_| unreachable!()
    )
    .is_err());
    assert_eq!(
        PreparedRgbSweep::read(&store, "1")
            .unwrap()
            .unwrap()
            .batch_transfer_idx,
        2
    );
    assert!(store.list(ARCHIVE, "").unwrap().is_empty());
}

#[test]
fn reprepare_checks_signed_cache_and_ldk_broadcast_status_for_every_input() {
    use lightning::{
        sign::SpendableOutputDescriptor,
        util::{
            ser::Writeable,
            sweep::{OutputSpendStatus, TrackedSpendableOutput},
        },
    };
    let store = Store::default();
    let prepared = fixture(1);
    let tx = Psbt::from_str(&prepared.psbt).unwrap().unsigned_tx;
    let input = tx.input[0].previous_output;
    let mut tracked = vec![TrackedSpendableOutput {
        descriptor: SpendableOutputDescriptor::StaticOutput {
            outpoint: lightning::chain::transaction::OutPoint {
                txid: input.txid,
                index: input.vout as u16,
            },
            output: tx.output[0].clone(),
            channel_keys_id: None,
        },
        channel_id: None,
        status: OutputSpendStatus::PendingInitialBroadcast {
            delayed_until_height: None,
        },
    }];
    assert!(crate::ldk::check_sweep_not_broadcast(&store, &tracked, &prepared).is_ok());
    assert!(crate::ldk::check_sweep_not_broadcast(&store, &[], &prepared).is_err());
    tracked[0].status = OutputSpendStatus::PendingFirstConfirmation {
        first_broadcast_hash: bitcoin::BlockHash::all_zeros(),
        latest_broadcast_height: 100,
        latest_spending_tx: tx.clone(),
    };
    assert!(crate::ldk::check_sweep_not_broadcast(&store, &tracked, &prepared).is_err());
    tracked[0].status = OutputSpendStatus::PendingInitialBroadcast {
        delayed_until_height: None,
    };
    let mut cache = crate::ldk::OutputSpenderTxes::default();
    // Different descriptor hash still spends the same input.
    cache.insert(999, tx);
    store
        .write("", "", "output_spender_txes", cache.encode())
        .unwrap();
    assert!(crate::ldk::check_sweep_not_broadcast(&store, &tracked, &prepared).is_err());
    store
        .write("", "", "output_spender_txes", vec![255])
        .unwrap();
    assert!(crate::ldk::check_sweep_not_broadcast(&store, &tracked, &prepared).is_err());
}
