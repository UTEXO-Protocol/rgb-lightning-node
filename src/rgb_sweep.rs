//! Durable preparation for RGB sweeps. Apply the fascia only after the sweep is confirmed.

use crate::rgb_compat::ColorPrepareResult;
use bincode::Options;
use bitcoin::io;
use lightning::util::persist::KVStoreSync;
use rgb_lib::{bitcoin::psbt::Psbt, ConsignmentExt, FileContent};
use serde::{Deserialize, Serialize};
use std::{collections::HashSet, fs, str::FromStr};

const PRIMARY_NAMESPACE: &str = "rgb_sweeps";
const MAX_RECEIPT_BYTES: u64 = 32 * 1024 * 1024;

// Match bincode::serialize's fixed-width wire encoding while rejecting oversized
// or trailing data. Both byte admission and decoder limits precede allocation.
fn receipt_options() -> impl bincode::Options {
    bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .with_limit(MAX_RECEIPT_BYTES)
        .reject_trailing_bytes()
}

// Admission of the fixed bincode layout uses checked offsets without allocation,
// before serde can reserve a vector using an attacker-controlled element count.
fn admit_receipt_bytes(bytes: &[u8]) -> Result<(), String> {
    let invalid = || "invalid or oversized prepared RGB sweep receipt".to_string();
    if bytes.len() as u64 > MAX_RECEIPT_BYTES {
        return Err(invalid());
    }
    fn take<'a>(bytes: &'a [u8], position: &mut usize, length: usize) -> Result<&'a [u8], String> {
        let end = position
            .checked_add(length)
            .ok_or_else(|| "sweep receipt length overflow".to_string())?;
        let slice = bytes
            .get(*position..end)
            .ok_or_else(|| "truncated sweep receipt".to_string())?;
        *position = end;
        Ok(slice)
    }
    fn length(bytes: &[u8], position: &mut usize, max: usize) -> Result<usize, String> {
        let raw: u64 = u64::from_le_bytes(
            take(bytes, position, 8)?
                .try_into()
                .map_err(|_| "invalid sweep receipt length".to_string())?,
        );
        let value =
            usize::try_from(raw).map_err(|_| "sweep receipt length overflow".to_string())?;
        if value == 0 || value > max {
            return Err("invalid sweep receipt component bound".into());
        }
        Ok(value)
    }
    let mut position = 0usize;
    let psbt_bytes = length(bytes, &mut position, 16 * 1024 * 1024)?;
    take(bytes, &mut position, psbt_bytes)?;
    take(bytes, &mut position, 4)?; // i32 batch identity, validated against the native record.
    let proofs = length(bytes, &mut position, 64)?;
    for _ in 0..proofs {
        let asset_bytes = length(bytes, &mut position, 256)?;
        take(bytes, &mut position, asset_bytes)?;
        let proof_bytes = length(bytes, &mut position, 16 * 1024 * 1024)?;
        take(bytes, &mut position, proof_bytes)?;
    }
    if position != bytes.len() {
        return Err(invalid());
    }
    Ok(())
}

#[derive(Clone, Debug, Serialize, Deserialize)]
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

    pub(crate) fn validate_then_mark_handoff(
        &self,
        canonical: &Self,
        mark: impl FnOnce(i32, String) -> Result<(), String>,
    ) -> Result<(), String> {
        if self.batch_transfer_idx != canonical.batch_transfer_idx || self.psbt != canonical.psbt {
            return Err("prepared RGB sweep does not match its canonical native PSBT/batch".into());
        }
        fn proofs(
            receipts: &[(String, Vec<u8>)],
        ) -> Result<std::collections::HashMap<&str, &[u8]>, String> {
            let mut out = std::collections::HashMap::new();
            for (asset, bytes) in receipts {
                if out.insert(asset.as_str(), bytes.as_slice()).is_some() {
                    return Err("prepared RGB sweep has duplicate proof asset IDs".into());
                }
            }
            Ok(out)
        }
        if proofs(&self.consignments)? != proofs(&canonical.consignments)? {
            return Err("prepared RGB sweep does not match its canonical native proofs".into());
        }
        mark(self.batch_transfer_idx, self.txid()?)
    }

    pub(crate) fn read(store: &impl KVStoreSync, key: &str) -> Result<Option<Self>, String> {
        match store.read(PRIMARY_NAMESPACE, "", key) {
            Ok(bytes) => {
                if bytes.len() as u64 > MAX_RECEIPT_BYTES {
                    return Err(format!("prepared RGB sweep {key} exceeds its size bound"));
                }
                admit_receipt_bytes(&bytes)?;
                receipt_options()
                    .deserialize(&bytes)
                    .map(Some)
                    .map_err(|e| format!("invalid prepared RGB sweep {key}: {e}"))
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(format!("cannot read prepared RGB sweep {key}: {e}")),
        }
    }

    pub(crate) fn persist(&self, store: &impl KVStoreSync, key: &str) -> Result<(), String> {
        let bytes = receipt_options()
            .serialize(self)
            .map_err(|e| e.to_string())?;
        admit_receipt_bytes(&bytes)?;
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

// Match library-owned orphan preparations using the same exact-input rule as KV.
// A protected operation can be handed off again only with its original PSBT/proofs.
pub(crate) fn find_prepared_sweep_in_records(
    records: Vec<PreparedRgbSweep>,
    inputs: &HashSet<bitcoin::OutPoint>,
) -> Result<Option<PreparedRgbSweep>, String> {
    let mut matching = None;
    for prepared in records {
        let psbt = Psbt::from_str(&prepared.psbt)
            .map_err(|e| format!("invalid native prepared sweep PSBT: {e}"))?;
        let saved = psbt
            .unsigned_tx
            .input
            .iter()
            .map(|input| input.previous_output)
            .collect::<HashSet<_>>();
        if saved.is_disjoint(inputs) {
            continue;
        }
        if saved != *inputs || matching.is_some() {
            return Err("inputs overlap another native prepared RGB sweep".into());
        }
        matching = Some(prepared);
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
    #[test]
    fn native_orphan_recovery_preserves_exact_psbt_and_proofs_before_kv_handoff() {
        let prepared = with_inputs(&[0, 1]);
        let recovered = find_prepared_sweep_in_records(vec![prepared.clone()], &inputs(&[1, 0]))
            .unwrap()
            .unwrap();
        assert_eq!(recovered.psbt, prepared.psbt);
        assert_eq!(recovered.consignments, prepared.consignments);
        assert_eq!(recovered.batch_transfer_idx, prepared.batch_transfer_idx);
        let store = Store::default();
        recovered.persist(&store, "recovered").unwrap();
        assert_eq!(
            PreparedRgbSweep::read(&store, "recovered")
                .unwrap()
                .unwrap()
                .psbt,
            prepared.psbt
        );
        for partial in [&[0][..], &[1, 2], &[0, 1, 2]] {
            assert!(
                find_prepared_sweep_in_records(vec![prepared.clone()], &inputs(partial)).is_err()
            );
        }
        assert!(find_prepared_sweep_in_records(
            vec![prepared.clone(), prepared.clone()],
            &inputs(&[0, 1])
        )
        .is_err());
        assert!(
            find_prepared_sweep_in_records(vec![prepared], &inputs(&[2]))
                .unwrap()
                .is_none()
        );
    }
    #[test]
    fn invalid_native_orphan_record_blocks_new_preparation() {
        let mut prepared = fixture();
        prepared.psbt = "invalid".into();
        assert!(find_prepared_sweep_in_records(vec![prepared], &inputs(&[2])).is_err());
    }
    #[test]
    fn canonical_native_receipt_is_required_before_any_handoff_or_signing() {
        let native = fixture();
        let mut changed_map = native.clone();
        let mut psbt = Psbt::from_str(&changed_map.psbt).unwrap();
        psbt.inputs[0].sighash_type = Some(bitcoin::EcdsaSighashType::All.into());
        changed_map.psbt = psbt.to_string();
        assert_eq!(changed_map.txid().unwrap(), native.txid().unwrap());
        let mut changed_proof = native.clone();
        changed_proof.consignments[0].1.push(9);
        let mut swapped_asset = native.clone();
        swapped_asset.consignments[0].0 = "other".into();
        let mut duplicate_proof = native.clone();
        duplicate_proof
            .consignments
            .push(duplicate_proof.consignments[0].clone());
        let mut wrong_batch = native.clone();
        wrong_batch.batch_transfer_idx += 1;
        for corrupt in [
            changed_map,
            changed_proof,
            swapped_asset,
            duplicate_proof,
            wrong_batch,
        ] {
            let mutated = AtomicBool::new(false);
            assert!(corrupt
                .validate_then_mark_handoff(&native, |_, _| {
                    mutated.store(true, Ordering::SeqCst);
                    Ok(())
                })
                .is_err());
            assert!(
                !mutated.load(Ordering::SeqCst),
                "no native mutation or signer may see a corrupt KV receipt"
            );
        }
        native
            .validate_then_mark_handoff(&native, |batch, txid| {
                assert_eq!(batch, 42);
                assert_eq!(txid, native.txid().unwrap());
                Ok(())
            })
            .unwrap();
    }

    #[test]
    fn kv_receipt_decoder_rejects_forged_lengths_trailing_and_oversized_bytes() {
        let store = Store::default();
        store
            .write(
                PRIMARY_NAMESPACE,
                "",
                "forged",
                u64::MAX.to_le_bytes().to_vec(),
            )
            .unwrap();
        assert!(PreparedRgbSweep::read(&store, "forged").is_err());
        let mut forged_count = bincode::serialize(&fixture()).unwrap();
        let count_offset = 8 + fixture().psbt.len() + 4;
        forged_count[count_offset..count_offset + 8].copy_from_slice(&u64::MAX.to_le_bytes());
        store
            .write(PRIMARY_NAMESPACE, "", "forged_count", forged_count)
            .unwrap();
        assert!(PreparedRgbSweep::read(&store, "forged_count").is_err());
        let mut trailing = bincode::serialize(&fixture()).unwrap();
        trailing.push(0);
        store
            .write(PRIMARY_NAMESPACE, "", "trailing", trailing)
            .unwrap();
        assert!(PreparedRgbSweep::read(&store, "trailing").is_err());
        store
            .write(
                PRIMARY_NAMESPACE,
                "",
                "oversized",
                vec![0; MAX_RECEIPT_BYTES as usize + 1],
            )
            .unwrap();
        assert!(PreparedRgbSweep::read(&store, "oversized").is_err());
        fixture().persist(&store, "bounded").unwrap();
        assert_eq!(
            PreparedRgbSweep::read(&store, "bounded")
                .unwrap()
                .unwrap()
                .psbt,
            fixture().psbt
        );
    }
}
