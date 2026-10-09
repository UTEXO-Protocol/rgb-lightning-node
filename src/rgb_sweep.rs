//! Durable preparation for RGB sweeps. Apply the fascia only after the sweep is confirmed.

use bitcoin::io;
use lightning::util::persist::KVStoreSync;
use rgb_lib::{
    bitcoin::psbt::Psbt, wallet::rust_only::ColorPrepareResult, ConsignmentExt, FileContent,
};
use serde::{Deserialize, Serialize};
use std::{collections::HashSet, fs, str::FromStr};

const PRIMARY_NAMESPACE: &str = "rgb_sweeps";

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct PreparedRgbSweep {
    pub(crate) psbt: String,
    pub(crate) batch_transfer_idx: i32,
    pub(crate) consignments: Vec<(String, Vec<u8>)>,
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
        let psbt = Psbt::from_str(&self.psbt).map_err(|e| format!("invalid sweep PSBT: {e}"))?;
        Ok(psbt.unsigned_tx.compute_txid().to_string())
    }

    pub(crate) fn read(store: &impl KVStoreSync, key: &str) -> Result<Option<Self>, String> {
        match store.read(PRIMARY_NAMESPACE, "", key) {
            Ok(bytes) => bincode::deserialize(&bytes)
                .map(Some)
                .map_err(|e| format!("invalid prepared RGB sweep {key}: {e}")),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(format!("cannot read prepared RGB sweep {key}: {e}")),
        }
    }

    pub(crate) fn persist(&self, store: &impl KVStoreSync, key: &str) -> Result<(), String> {
        let bytes = bincode::serialize(self).map_err(|e| e.to_string())?;
        store
            .write(PRIMARY_NAMESPACE, "", key, bytes)
            .map_err(|e| format!("cannot persist prepared RGB sweep {key}: {e}"))
    }
}

// Match input outpoints because LDK can reorder descriptors between attempts. Reuse an exact
// input set; reject partial overlaps until the existing sweep is resolved. Unreadable records
// block new preparations but allow retrying a known matching transaction.
pub(crate) fn find_prepared_sweep(
    store: &impl KVStoreSync,
    inputs: &HashSet<bitcoin::OutPoint>,
) -> Result<Option<PreparedRgbSweep>, String> {
    let mut matching = None;
    let mut unreadable = false;
    for key in store
        .list(PRIMARY_NAMESPACE, "")
        .map_err(|e| e.to_string())?
    {
        let decoded = PreparedRgbSweep::read(store, &key).and_then(|sweep| {
            sweep
                .map(|sweep| {
                    let psbt = Psbt::from_str(&sweep.psbt).map_err(|e| e.to_string())?;
                    let saved = psbt
                        .unsigned_tx
                        .input
                        .iter()
                        .map(|i| i.previous_output)
                        .collect::<HashSet<_>>();
                    Ok((sweep, saved))
                })
                .transpose()
        });
        let (sweep, saved) = match decoded {
            Ok(Some(pair)) => pair,
            Ok(None) => continue,
            Err(error) => {
                tracing::error!(key, %error, "cannot inspect prepared RGB sweep inputs");
                unreadable = true;
                continue;
            }
        };
        if saved.is_disjoint(inputs) {
            continue;
        }
        if saved != *inputs || matching.is_some() {
            return Err(format!("inputs overlap another prepared RGB sweep {key}"));
        }
        matching = Some(sweep);
    }
    if matching.is_none() && unreadable {
        return Err("repair unreadable RGB sweep receipts before preparing a new sweep".into());
    }
    Ok(matching)
}

// Conservatively wait for confirmation before applying fascia. rgb-lib only requires that
// the indexer see the transaction.
// Consumption is driven by wallet refresh (API/SDK/ChannelReady), not a separate background job.
// A transaction that never confirms retains its receipt and Initiated batch for operator repair.
// Each receipt is independent; even a failed list/read/consume/remove must not abort wallet refresh.
pub(crate) fn reconcile_prepared_sweeps(
    store: &impl KVStoreSync,
    mut is_confirmed: impl FnMut(String) -> Result<bool, String>,
    mut consume: impl FnMut(i32) -> Result<(), String>,
) {
    let keys = match store.list(PRIMARY_NAMESPACE, "") {
        Ok(keys) => keys,
        Err(error) => {
            tracing::error!(%error, "cannot list prepared RGB sweeps; continuing wallet refresh");
            return;
        }
    };
    for key in keys {
        let mut batch_transfer_idx = None;
        let mut txid = None;
        let result = (|| -> Result<(), String> {
            let Some(sweep) = PreparedRgbSweep::read(store, &key)? else {
                return Ok(());
            };
            batch_transfer_idx = Some(sweep.batch_transfer_idx);
            let prepared_txid = sweep.txid()?;
            txid = Some(prepared_txid.clone());
            if is_confirmed(prepared_txid)? {
                consume(sweep.batch_transfer_idx)?;
                store
                    .remove(PRIMARY_NAMESPACE, "", &key, false)
                    .map_err(|e| format!("cannot remove completed RGB sweep: {e}"))?;
            }
            Ok(())
        })();
        if let Err(error) = result {
            tracing::error!(key, ?batch_transfer_idx, ?txid, %error,
                "cannot reconcile RGB sweep; retaining receipt and continuing wallet refresh");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bitcoin::{
        absolute, hashes::Hash, transaction, Amount, OutPoint, ScriptBuf, Transaction, TxIn, TxOut,
    };
    use std::{
        collections::BTreeMap,
        sync::{
            atomic::{AtomicBool, Ordering},
            Mutex,
        },
    };

    #[derive(Default)]
    struct Store {
        entries: Mutex<BTreeMap<String, Vec<u8>>>,
        fail_remove: AtomicBool,
    }

    impl KVStoreSync for Store {
        fn read(&self, _: &str, _: &str, key: &str) -> Result<Vec<u8>, io::Error> {
            self.entries
                .lock()
                .unwrap()
                .get(key)
                .cloned()
                .ok_or_else(|| io::ErrorKind::NotFound.into())
        }
        fn write(&self, _: &str, _: &str, key: &str, data: Vec<u8>) -> Result<(), io::Error> {
            self.entries.lock().unwrap().insert(key.to_string(), data);
            Ok(())
        }
        fn remove(&self, _: &str, _: &str, key: &str, _: bool) -> Result<(), io::Error> {
            if self.fail_remove.load(Ordering::SeqCst) {
                return Err(io::Error::new(
                    io::ErrorKind::Other,
                    "injected storage failure",
                ));
            }
            self.entries.lock().unwrap().remove(key);
            Ok(())
        }
        fn list(&self, _: &str, _: &str) -> Result<Vec<String>, io::Error> {
            Ok(self.entries.lock().unwrap().keys().cloned().collect())
        }
    }

    fn fixture() -> PreparedRgbSweep {
        let psbt = Psbt::from_unsigned_tx(Transaction {
            version: transaction::Version::TWO,
            lock_time: absolute::LockTime::ZERO,
            input: vec![TxIn {
                previous_output: OutPoint::new(bitcoin::Txid::from_byte_array([1; 32]), 0),
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
            batch_transfer_idx: 42,
            consignments: vec![("asset".into(), vec![1, 2, 3])],
        }
    }

    #[test]
    fn unconfirmed_sweep_keeps_exact_psbt_and_consignments_for_retry() {
        let store = Store::default();
        let prepared = fixture();
        prepared.persist(&store, "sweep").unwrap();
        reconcile_prepared_sweeps(
            &store,
            |_| Ok(false),
            |_| panic!("must not consume before confirmation"),
        );
        let restored = PreparedRgbSweep::read(&store, "sweep").unwrap().unwrap();
        assert_eq!(restored.psbt, prepared.psbt);
        assert_eq!(restored.consignments, prepared.consignments);
        assert_eq!(restored.batch_transfer_idx, prepared.batch_transfer_idx);
    }

    #[test]
    fn failed_consume_is_retried_from_the_persisted_batch() {
        let store = Store::default();
        fixture().persist(&store, "sweep").unwrap();
        reconcile_prepared_sweeps(&store, |_| Ok(true), |_| Err("indexer lag".into()));
        assert!(PreparedRgbSweep::read(&store, "sweep").unwrap().is_some());
        let mut consumed = Vec::new();
        reconcile_prepared_sweeps(
            &store,
            |_| Ok(true),
            |idx| {
                consumed.push(idx);
                Ok(())
            },
        );
        assert_eq!(consumed, vec![42]);
        assert!(PreparedRgbSweep::read(&store, "sweep").unwrap().is_none());
    }

    #[test]
    fn removal_failure_retains_the_receipt_for_idempotent_consume() {
        let store = Store::default();
        fixture().persist(&store, "sweep").unwrap();
        store.fail_remove.store(true, Ordering::SeqCst);
        let mut consumed = Vec::new();
        reconcile_prepared_sweeps(
            &store,
            |_| Ok(true),
            |idx| {
                consumed.push(idx);
                Ok(())
            },
        );
        assert!(PreparedRgbSweep::read(&store, "sweep").unwrap().is_some());
        store.fail_remove.store(false, Ordering::SeqCst);
        reconcile_prepared_sweeps(
            &store,
            |_| Ok(true),
            |idx| {
                consumed.push(idx);
                Ok(())
            },
        );
        assert_eq!(consumed, vec![42, 42]);
        assert!(PreparedRgbSweep::read(&store, "sweep").unwrap().is_none());
    }

    fn with_inputs(vouts: &[u32]) -> PreparedRgbSweep {
        let mut sweep = fixture();
        let mut tx = Psbt::from_str(&sweep.psbt).unwrap().unsigned_tx;
        tx.input = vouts
            .iter()
            .map(|vout| TxIn {
                previous_output: OutPoint::new(bitcoin::Txid::from_byte_array([1; 32]), *vout),
                ..Default::default()
            })
            .collect();
        sweep.psbt = Psbt::from_unsigned_tx(tx).unwrap().to_string();
        sweep
    }

    fn inputs(vouts: &[u32]) -> HashSet<OutPoint> {
        vouts
            .iter()
            .map(|vout| OutPoint::new(bitcoin::Txid::from_byte_array([1; 32]), *vout))
            .collect()
    }

    #[test]
    fn indexer_failure_retains_receipt_and_continues_with_next_sweep() {
        let store = Store::default();
        let first = with_inputs(&[0]);
        let mut second = with_inputs(&[1]);
        second.batch_transfer_idx = 43;
        first.persist(&store, "a").unwrap();
        second.persist(&store, "b").unwrap();
        let mut consumed = Vec::new();
        reconcile_prepared_sweeps(
            &store,
            |txid| {
                if txid == first.txid().unwrap() {
                    Err("offline".into())
                } else {
                    Ok(true)
                }
            },
            |idx| {
                consumed.push(idx);
                Ok(())
            },
        );
        assert_eq!(consumed, [43]);
        assert!(PreparedRgbSweep::read(&store, "a").unwrap().is_some());
        assert!(PreparedRgbSweep::read(&store, "b").unwrap().is_none());
    }

    #[test]
    #[tracing_test::traced_test]
    fn corrupt_receipt_does_not_block_valid_receipt_reconciliation() {
        let store = Store::default();
        store
            .write(PRIMARY_NAMESPACE, "", "a_corrupt", vec![0xff])
            .unwrap();
        fixture().persist(&store, "b_valid").unwrap();
        let mut consumed = Vec::new();
        reconcile_prepared_sweeps(
            &store,
            |_| Ok(true),
            |idx| {
                consumed.push(idx);
                Ok(())
            },
        );
        assert_eq!(consumed, [42]);
        assert_eq!(
            store.read(PRIMARY_NAMESPACE, "", "a_corrupt").unwrap(),
            [0xff]
        );
        assert!(PreparedRgbSweep::read(&store, "b_valid").unwrap().is_none());
        assert!(logs_contain("a_corrupt"));
    }

    #[test]
    fn reordered_inputs_reuse_psbt_but_partial_overlap_blocks_new_preparation() {
        let store = Store::default();
        let prepared = with_inputs(&[0, 1]);
        prepared.persist(&store, "original_key").unwrap();
        let restored = find_prepared_sweep(&store, &inputs(&[1, 0]))
            .unwrap()
            .unwrap();
        assert_eq!(restored.psbt, prepared.psbt);
        assert_eq!(restored.consignments, prepared.consignments);
        assert_eq!(restored.batch_transfer_idx, prepared.batch_transfer_idx);
        for partial in [&[0][..], &[1, 2], &[0, 1, 2]] {
            assert!(find_prepared_sweep(&store, &inputs(partial)).is_err());
        }
        assert!(find_prepared_sweep(&store, &inputs(&[2]))
            .unwrap()
            .is_none());
    }

    #[test]
    #[tracing_test::traced_test]
    fn unreadable_inputs_block_new_preparation_but_allow_known_retry() {
        let store = Store::default();
        store
            .write(PRIMARY_NAMESPACE, "", "a_corrupt", vec![0xff])
            .unwrap();
        assert!(find_prepared_sweep(&store, &inputs(&[2])).is_err());
        with_inputs(&[0]).persist(&store, "b_valid").unwrap();
        assert!(find_prepared_sweep(&store, &inputs(&[0]))
            .unwrap()
            .is_some());
        assert!(logs_contain("a_corrupt"));
    }
}
