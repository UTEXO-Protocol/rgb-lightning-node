//! Durable preparation for RGB sweeps. Apply the fascia only after the sweep is confirmed.

use bitcoin::io;
use lightning::util::persist::KVStoreSync;
use rgb_lib::{
    bitcoin::psbt::Psbt, wallet::rust_only::ColorPrepareResult, ConsignmentExt, FileContent,
};
use serde::{Deserialize, Serialize};
use std::{fs, str::FromStr};

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

// A confirmed witness also satisfies rgb-lib's requirement that the indexer see the transaction.
// Keep the record on any failure; consume_transfer_fascia is idempotent if removal must be retried.
pub(crate) fn reconcile_prepared_sweeps(
    store: &impl KVStoreSync,
    mut is_confirmed: impl FnMut(String) -> Result<bool, String>,
    mut consume: impl FnMut(i32) -> Result<(), String>,
) -> Result<(), String> {
    let keys = store
        .list(PRIMARY_NAMESPACE, "")
        .map_err(|e| format!("cannot list prepared RGB sweeps: {e}"))?;
    for key in keys {
        let Some(sweep) = PreparedRgbSweep::read(store, &key)? else {
            continue;
        };
        if !is_confirmed(sweep.txid()?)? {
            continue;
        }
        consume(sweep.batch_transfer_idx)?;
        store
            .remove(PRIMARY_NAMESPACE, "", &key, false)
            .map_err(|e| format!("cannot remove completed RGB sweep {key}: {e}"))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use bitcoin::{
        absolute, hashes::Hash, transaction, Amount, OutPoint, ScriptBuf, Transaction, TxIn, TxOut,
    };
    use std::{
        collections::HashMap,
        sync::{
            atomic::{AtomicBool, Ordering},
            Mutex,
        },
    };

    #[derive(Default)]
    struct Store {
        entries: Mutex<HashMap<String, Vec<u8>>>,
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
        )
        .unwrap();
        let restored = PreparedRgbSweep::read(&store, "sweep").unwrap().unwrap();
        assert_eq!(restored.psbt, prepared.psbt);
        assert_eq!(restored.consignments, prepared.consignments);
        assert_eq!(restored.batch_transfer_idx, prepared.batch_transfer_idx);
    }

    #[test]
    fn failed_consume_is_retried_from_the_persisted_batch() {
        let store = Store::default();
        fixture().persist(&store, "sweep").unwrap();
        assert!(
            reconcile_prepared_sweeps(&store, |_| Ok(true), |_| Err("indexer lag".into())).is_err()
        );
        assert!(PreparedRgbSweep::read(&store, "sweep").unwrap().is_some());
        let mut consumed = Vec::new();
        reconcile_prepared_sweeps(
            &store,
            |_| Ok(true),
            |idx| {
                consumed.push(idx);
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(consumed, vec![42]);
        assert!(PreparedRgbSweep::read(&store, "sweep").unwrap().is_none());
    }

    #[test]
    fn removal_failure_retains_the_receipt_for_idempotent_consume() {
        let store = Store::default();
        fixture().persist(&store, "sweep").unwrap();
        store.fail_remove.store(true, Ordering::SeqCst);
        let mut consumed = Vec::new();
        assert!(reconcile_prepared_sweeps(
            &store,
            |_| Ok(true),
            |idx| {
                consumed.push(idx);
                Ok(())
            }
        )
        .is_err());
        assert!(PreparedRgbSweep::read(&store, "sweep").unwrap().is_some());
        store.fail_remove.store(false, Ordering::SeqCst);
        reconcile_prepared_sweeps(
            &store,
            |_| Ok(true),
            |idx| {
                consumed.push(idx);
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(consumed, vec![42, 42]);
        assert!(PreparedRgbSweep::read(&store, "sweep").unwrap().is_none());
    }

    #[test]
    fn indexer_failure_keeps_the_receipt_and_does_not_consume() {
        let store = Store::default();
        fixture().persist(&store, "sweep").unwrap();
        assert!(reconcile_prepared_sweeps(
            &store,
            |_| Err("offline".into()),
            |_| panic!("unknown confirmation state")
        )
        .is_err());
        assert!(PreparedRgbSweep::read(&store, "sweep").unwrap().is_some());
    }

    #[test]
    fn corrupt_receipt_is_an_error_not_a_new_sweep() {
        let store = Store::default();
        store
            .write(PRIMARY_NAMESPACE, "", "sweep", vec![0xff])
            .unwrap();
        assert!(PreparedRgbSweep::read(&store, "sweep").is_err());
        assert!(reconcile_prepared_sweeps(
            &store,
            |_| panic!("corrupt receipt"),
            |_| panic!("corrupt receipt")
        )
        .is_err());
        assert!(store.read(PRIMARY_NAMESPACE, "", "sweep").is_ok());
    }
}
