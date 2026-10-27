use super::*;
use bitcoin::hashes::Hash;
use bitcoin::{
    absolute, transaction::Version, Amount, OutPoint, ScriptBuf, Sequence, Transaction, TxIn,
    TxOut, Txid, Witness,
};
use std::{collections::HashMap, sync::Mutex};

#[derive(Default)]
struct State {
    pending: HashMap<String, String>,
    observed: HashMap<String, bool>,
    begin_calls: usize,
    finish_calls: Vec<String>,
    cancel_calls: Vec<String>,
    sign_failure: bool,
    mutate_sign: bool,
    incomplete_sign: bool,
    finish_failure: u8, // 1 before broadcast, 2 after broadcast before DB commit, 3 lost reply after DB commit
    offline: bool,
    available: usize,
}
struct Backend(Mutex<State>);
impl Backend {
    fn new() -> Self {
        Self(Mutex::new(State {
            available: 8,
            ..State::default()
        }))
    }
}
fn request(number: u8) -> OutputRequest {
    OutputRequest {
        operation_id: format!("{number:032x}"),
        up_to: false,
        num: 4,
        size: 20_000,
        fee_rate: 2,
        skip_sync: true,
    }
}
fn psbt(number: usize, num: u8) -> Psbt {
    let mut hash = [0u8; 32];
    hash[..8].copy_from_slice(&(number as u64).to_le_bytes());
    let tx = Transaction {
        version: Version::TWO,
        lock_time: absolute::LockTime::ZERO,
        input: vec![TxIn {
            previous_output: OutPoint {
                txid: Txid::from_byte_array(hash),
                vout: 0,
            },
            script_sig: ScriptBuf::new(),
            sequence: Sequence::MAX,
            witness: Witness::default(),
        }],
        output: (0..num)
            .map(|_| TxOut {
                value: Amount::from_sat(20_000),
                script_pubkey: ScriptBuf::from_bytes(vec![0x51]),
            })
            .collect(),
    };
    let mut psbt = Psbt::from_unsigned_tx(tx).unwrap();
    psbt.inputs[0].witness_utxo = Some(TxOut {
        value: Amount::from_sat(20_000 * u64::from(num) + 1_000),
        script_pubkey: ScriptBuf::from_bytes(vec![0x51]),
    });
    psbt
}
impl OutputBackend for Backend {
    fn pending(&self) -> Result<Vec<PendingOutput>, APIError> {
        Ok(self
            .0
            .lock()
            .unwrap()
            .pending
            .keys()
            .map(|txid| PendingOutput {
                txid: txid.clone(),
                operation_type: "CreateUtxos".into(),
            })
            .collect())
    }
    fn prepare(&self, request: &OutputRequest) -> Result<PreparedOutput, APIError> {
        let mut state = self.0.lock().unwrap();
        state.begin_calls += 1;
        if state.pending.len() >= state.available {
            return Err(APIError::InvalidRequest(
                "fixture insufficient available inputs".into(),
            ));
        }
        let number = (1..=state.available)
            .find(|number| {
                !state
                    .pending
                    .values()
                    .any(|number_string| number_string == &number.to_string())
            })
            .unwrap();
        let count = request.num.min(3); // Mimic the real library reducing count to available funds.
        let unsigned = psbt(number, count);
        state.pending.insert(
            unsigned.unsigned_tx.compute_txid().to_string(),
            number.to_string(),
        );
        Ok(PreparedOutput {
            unsigned_psbt: unsigned.to_string(),
            num_outputs: count,
        })
    }
    fn sign(&self, unsigned: String) -> Result<String, APIError> {
        let state = self.0.lock().unwrap();
        if state.sign_failure {
            return Err(APIError::InvalidRequest(
                "fixture signer unavailable".into(),
            ));
        }
        let mut signed = Psbt::from_str(&unsigned).unwrap();
        if state.mutate_sign {
            signed.unsigned_tx.output[0].value = Amount::from_sat(19_999);
        }
        if !state.incomplete_sign {
            signed.inputs[0].final_script_witness = Some(Witness::from_slice(&[&[1u8][..]]));
        }
        Ok(signed.to_string())
    }
    fn finish(&self, signed: String) -> Result<u8, APIError> {
        let psbt = Psbt::from_str(&signed).unwrap();
        let txid = psbt.unsigned_tx.compute_txid().to_string();
        let mut state = self.0.lock().unwrap();
        state.finish_calls.push(txid.clone());
        if state.finish_failure == 1 {
            return Err(APIError::FailedBroadcast("fixture before broadcast".into()));
        }
        state.observed.insert(txid.clone(), false);
        if state.finish_failure == 2 {
            return Err(APIError::Unexpected(
                "fixture post-broadcast SQL commit failed".into(),
            ));
        }
        state.pending.remove(&txid);
        if state.finish_failure == 3 {
            return Err(APIError::Unexpected("fixture reply lost".into()));
        }
        Ok(psbt.unsigned_tx.output.len() as u8)
    }
    fn cancel(&self, txid: String) -> Result<(), APIError> {
        let mut state = self.0.lock().unwrap();
        state.cancel_calls.push(txid.clone());
        assert!(
            !state.observed.contains_key(&txid),
            "a broadcast transaction must never be aborted"
        );
        state.pending.remove(&txid);
        Ok(())
    }
    fn observed(&self, txid: &str) -> Result<Option<bool>, APIError> {
        let state = self.0.lock().unwrap();
        if state.offline {
            return Err(APIError::Unexpected("fixture offline".into()));
        }
        Ok(state.observed.get(txid).copied())
    }
}
fn store() -> (tempfile::TempDir, OutputStore) {
    let directory = tempfile::tempdir().unwrap();
    let store = OutputStore::new(directory.path().to_path_buf());
    (directory, store)
}

/// Build an actual BDK PSBT with the same public/private descriptors and output-origin metadata
/// as native RGB split construction. Its local graph is synthetic; no network, funding or signer
/// fallback is involved, and every wallet uses freshly generated test keys.
fn native_colored_psbt(
    private_keys: bool,
    witness_version: rgb_lib::keys::WitnessVersion,
) -> (tempfile::TempDir, rgb_lib::wallet::Wallet, Psbt) {
    use rgb_lib::{
        bdk_wallet::{KeychainKind, Wallet as BdkWallet},
        keys::generate_keys,
        wallet::{DatabaseType, SinglesigKeys, Wallet, WalletData},
        AssetSchema, BitcoinNetwork,
    };
    let directory = tempfile::tempdir().unwrap();
    let keys = generate_keys(BitcoinNetwork::Regtest, witness_version);
    let wallet = Wallet::new(
        WalletData {
            data_dir: directory.path().to_string_lossy().into(),
            bitcoin_network: BitcoinNetwork::Regtest,
            database_type: DatabaseType::Sqlite,
            max_allocations_per_utxo: 1,
            supported_schemas: vec![AssetSchema::Nia],
        },
        if private_keys {
            SinglesigKeys::from_keys(&keys, None)
        } else {
            SinglesigKeys::from_keys_no_mnemonic(&keys, None)
        },
    )
    .unwrap();
    let descriptors = wallet.get_descriptors();
    let mut bdk = BdkWallet::create(descriptors.colored, descriptors.vanilla)
        .network(bitcoin::Network::Regtest)
        .create_wallet_no_persist()
        .unwrap();
    let funding = Transaction {
        version: Version::TWO,
        lock_time: absolute::LockTime::ZERO,
        input: vec![TxIn {
            previous_output: OutPoint::new(Txid::from_byte_array([1; 32]), 0),
            ..Default::default()
        }],
        output: vec![TxOut {
            value: Amount::from_sat(200_000),
            script_pubkey: bdk
                .reveal_next_address(KeychainKind::Internal)
                .script_pubkey(),
        }],
    };
    let input = OutPoint::new(funding.compute_txid(), 0);
    bdk.apply_unconfirmed_txs([(funding, 1)]);
    let colored: Vec<_> = bdk.reveal_addresses_to(KeychainKind::External, 7).collect();
    assert_eq!(colored.len(), 8);
    let foreign = ScriptBuf::from_bytes([vec![0x51, 0x20], vec![9; 32]].concat());
    let mut builder = bdk.build_tx();
    builder
        .add_utxo(input)
        .unwrap()
        .manually_selected_only()
        .fee_rate(bitcoin::FeeRate::from_sat_per_vb_u32(2))
        .add_recipient(colored[0].script_pubkey(), Amount::from_sat(20_000))
        .add_recipient(colored[7].script_pubkey(), Amount::from_sat(20_000))
        .add_recipient(foreign, Amount::from_sat(20_000));
    (directory, wallet, builder.finish().unwrap())
}

#[test]
fn native_colored_output_count_admits_private_and_public_descriptors() {
    use rgb_lib::bdk_wallet::miniscript::{Descriptor, DescriptorPublicKey};
    use rgb_lib::keys::WitnessVersion;
    for private_keys in [false, true] {
        for witness in [WitnessVersion::Taproot, WitnessVersion::SegWitV0] {
            let (_directory, wallet, psbt) = native_colored_psbt(private_keys, witness);
            // This is the exact former parsing assumption. It deterministically rejects the
            // secret-bearing descriptor used by the REST daemon's internal-mnemonic profile.
            assert_eq!(
                wallet
                    .get_descriptors()
                    .colored
                    .parse::<Descriptor<DescriptorPublicKey>>()
                    .is_ok(),
                !private_keys
            );
            assert_eq!(colored_output_count(&wallet, &psbt, 20_000).unwrap(), 2);
            assert!(psbt.outputs.iter().any(|output| {
                output.bip32_derivation.values().any(|(_, path)| {
                    path.as_ref().last() == Some(&bitcoin::bip32::ChildNumber::Normal { index: 7 })
                }) || output.tap_key_origins.values().any(|(_, (_, path))| {
                    path.as_ref().last() == Some(&bitcoin::bip32::ChildNumber::Normal { index: 7 })
                })
            }));
        }
    }
}

#[test]
fn native_colored_output_count_verifies_scripts_amounts_and_origin_indices() {
    use rgb_lib::keys::WitnessVersion;
    let (_directory, wallet, mut psbt) = native_colored_psbt(false, WitnessVersion::Taproot);
    let colored = psbt
        .outputs
        .iter()
        .position(|output| {
            output.tap_key_origins.values().any(|(_, (_, path))| {
                path.as_ref().last() == Some(&bitcoin::bip32::ChildNumber::Normal { index: 7 })
            })
        })
        .unwrap();
    let foreign_script = ScriptBuf::from_bytes([vec![0x51, 0x20], vec![9; 32]].concat());
    let foreign = psbt
        .unsigned_tx
        .output
        .iter()
        .position(|output| output.script_pubkey == foreign_script)
        .unwrap();
    psbt.outputs[foreign] = psbt.outputs[colored].clone(); // spoofed metadata is not ownership
    assert_eq!(colored_output_count(&wallet, &psbt, 20_000).unwrap(), 2);
    psbt.unsigned_tx.output[colored].value = Amount::from_sat(19_999);
    assert!(colored_output_count(&wallet, &psbt, 20_000).is_err());
    psbt.unsigned_tx.output[colored].value = Amount::from_sat(20_000);
    psbt.outputs[colored].tap_key_origins.clear();
    assert_eq!(colored_output_count(&wallet, &psbt, 20_000).unwrap(), 1);
}

#[test]
fn prepare_is_durable_idempotent_and_reports_actual_fee_and_count() {
    let (directory, store) = store();
    let backend = Backend::new();
    let first = store.prepare(&backend, request(1)).unwrap();
    assert_eq!(first.status, "prepared");
    assert_eq!(first.num_outputs, 3);
    assert_eq!(first.fee_sat, 1_000);
    let reopened = OutputStore::new(directory.path().to_path_buf());
    assert_eq!(reopened.prepare(&backend, request(1)).unwrap(), first);
    assert_eq!(backend.0.lock().unwrap().begin_calls, 1);
    let mut different = request(1);
    different.fee_rate += 1;
    assert!(reopened.prepare(&backend, different).is_err());
    assert_eq!(backend.0.lock().unwrap().begin_calls, 1);
}
#[test]
fn prepare_rejects_noncanonical_id_and_zero_parameters_before_reserving() {
    let (_, store) = store();
    let backend = Backend::new();
    for id in [
        "a".repeat(31),
        "A".repeat(32),
        "g".repeat(32),
        "a/".repeat(16),
        "a".repeat(65),
    ] {
        let mut req = request(1);
        req.operation_id = id;
        assert!(store.prepare(&backend, req).is_err());
    }
    let mut req = request(1);
    req.num = 0;
    assert!(store.prepare(&backend, req).is_err());
    let mut req = request(1);
    req.size = 0;
    assert!(store.prepare(&backend, req).is_err());
    let mut req = request(1);
    req.fee_rate = 0;
    assert!(store.prepare(&backend, req).is_err());
    assert_eq!(backend.0.lock().unwrap().begin_calls, 0);
}
#[test]
fn signer_failure_releases_exact_owned_inputs_and_allows_new_review() {
    let (_, store) = store();
    let backend = Backend::new();
    let first = store.prepare(&backend, request(1)).unwrap();
    backend.0.lock().unwrap().sign_failure = true;
    assert!(store.commit(&backend, &first.operation_id).is_err());
    let failed = store.get(&backend, &first.operation_id).unwrap().unwrap();
    assert_eq!(failed.status, "failed");
    assert_eq!(failed.txid, first.txid);
    assert_eq!(failed.fee_sat, first.fee_sat);
    assert!(backend.pending().unwrap().is_empty());
    assert!(backend.0.lock().unwrap().finish_calls.is_empty());
    backend.0.lock().unwrap().sign_failure = false;
    let second = store.prepare(&backend, request(2)).unwrap();
    assert_eq!(second.inputs, first.inputs);
}
#[test]
fn malformed_or_incomplete_signer_response_is_prebroadcast_failure() {
    for mutate in [false, true] {
        let (_, store) = store();
        let backend = Backend::new();
        let first = store.prepare(&backend, request(1)).unwrap();
        {
            let mut state = backend.0.lock().unwrap();
            state.mutate_sign = mutate;
            state.incomplete_sign = !mutate;
        }
        assert!(store.commit(&backend, &first.operation_id).is_err());
        assert!(backend.pending().unwrap().is_empty());
        assert!(backend.0.lock().unwrap().finish_calls.is_empty());
    }
}
#[test]
fn cancellation_is_idempotent_and_cannot_reuse_operation_id() {
    let (_, store) = store();
    let backend = Backend::new();
    let first = store.prepare(&backend, request(1)).unwrap();
    assert_eq!(
        store.cancel(&backend, &first.operation_id).unwrap().status,
        "cancelled"
    );
    assert_eq!(
        store.cancel(&backend, &first.operation_id).unwrap().status,
        "cancelled"
    );
    assert_eq!(
        store.prepare(&backend, request(1)).unwrap().status,
        "cancelled"
    );
    assert!(store.commit(&backend, &first.operation_id).is_err());
    assert_eq!(backend.0.lock().unwrap().begin_calls, 1);
}
#[test]
fn independent_prepared_operations_exclude_reserved_inputs() {
    let (_, store) = store();
    let backend = Backend::new();
    let first = store.prepare(&backend, request(1)).unwrap();
    let second = store.prepare(&backend, request(2)).unwrap();
    assert_ne!(first.inputs, second.inputs);
    store.cancel(&backend, &second.operation_id).unwrap();
    assert_eq!(backend.pending().unwrap().len(), 1);
    assert_eq!(backend.pending().unwrap()[0].txid, first.txid.unwrap());
}
#[test]
fn committing_with_lost_reservation_never_signs_or_broadcasts() {
    let (_, store) = store();
    let backend = Backend::new();
    let first = store.prepare(&backend, request(1)).unwrap();
    // Simulates abort success followed by process death before the journal write.
    backend.0.lock().unwrap().pending.clear();
    assert!(store.commit(&backend, &first.operation_id).is_err());
    assert!(backend.0.lock().unwrap().finish_calls.is_empty());
    assert_eq!(
        store
            .get(&backend, &first.operation_id)
            .unwrap()
            .unwrap()
            .status,
        "failed"
    );
}
#[test]
fn lost_commit_response_and_repeat_commit_do_not_construct_another_transaction() {
    let (directory, store) = store();
    let backend = Backend::new();
    let first = store.prepare(&backend, request(1)).unwrap();
    backend.0.lock().unwrap().finish_failure = 3;
    assert_eq!(
        store.commit(&backend, &first.operation_id).unwrap().status,
        "broadcast"
    );
    let reopened = OutputStore::new(directory.path().to_path_buf());
    assert_eq!(
        reopened.commit(&backend, &first.operation_id).unwrap().txid,
        first.txid
    );
    let state = backend.0.lock().unwrap();
    assert_eq!(state.begin_calls, 1);
    assert_eq!(state.finish_calls.len(), 1);
}
#[test]
fn submission_uncertainty_retains_exact_transaction_and_can_only_retry_it() {
    let (directory, store) = store();
    let backend = Backend::new();
    let first = store.prepare(&backend, request(1)).unwrap();
    backend.0.lock().unwrap().finish_failure = 1;
    assert_eq!(
        store.commit(&backend, &first.operation_id).unwrap().status,
        "submitting"
    );
    assert!(store.cancel(&backend, &first.operation_id).is_err());
    backend.0.lock().unwrap().finish_failure = 0;
    let reopened = OutputStore::new(directory.path().to_path_buf());
    assert_eq!(
        reopened
            .get(&backend, &first.operation_id)
            .unwrap()
            .unwrap()
            .status,
        "broadcast"
    );
    let state = backend.0.lock().unwrap();
    assert_eq!(state.begin_calls, 1);
    assert!(state
        .finish_calls
        .iter()
        .all(|txid| Some(txid) == first.txid.as_ref()));
    assert!(state.cancel_calls.is_empty());
}
#[test]
fn check_status_heals_broadcast_before_database_commit_with_original_psbt() {
    let (_, store) = store();
    let backend = Backend::new();
    let first = store.prepare(&backend, request(1)).unwrap();
    backend.0.lock().unwrap().finish_failure = 2;
    assert_eq!(
        store.commit(&backend, &first.operation_id).unwrap().status,
        "submitting"
    );
    assert_eq!(backend.pending().unwrap().len(), 1);
    backend.0.lock().unwrap().finish_failure = 0;
    assert_eq!(
        store
            .get(&backend, &first.operation_id)
            .unwrap()
            .unwrap()
            .status,
        "broadcast"
    );
    assert!(backend.pending().unwrap().is_empty());
    let state = backend.0.lock().unwrap();
    assert_eq!(state.begin_calls, 1);
    assert_eq!(state.finish_calls.len(), 2);
    assert_eq!(state.finish_calls[0], state.finish_calls[1]);
}
#[test]
fn confirmation_reorg_removal_and_network_outage_revoke_confirmation() {
    let (_, store) = store();
    let backend = Backend::new();
    let first = store.prepare(&backend, request(1)).unwrap();
    store.commit(&backend, &first.operation_id).unwrap();
    let txid = first.txid.clone().unwrap();
    backend
        .0
        .lock()
        .unwrap()
        .observed
        .insert(txid.clone(), true);
    assert_eq!(
        store
            .get(&backend, &first.operation_id)
            .unwrap()
            .unwrap()
            .status,
        "confirmed"
    );
    backend
        .0
        .lock()
        .unwrap()
        .observed
        .insert(txid.clone(), false);
    assert_eq!(
        store
            .get(&backend, &first.operation_id)
            .unwrap()
            .unwrap()
            .status,
        "broadcast"
    );
    backend
        .0
        .lock()
        .unwrap()
        .observed
        .insert(txid.clone(), true);
    assert_eq!(
        store
            .get(&backend, &first.operation_id)
            .unwrap()
            .unwrap()
            .status,
        "confirmed"
    );
    backend.0.lock().unwrap().offline = true;
    assert_eq!(
        store
            .get(&backend, &first.operation_id)
            .unwrap()
            .unwrap()
            .status,
        "submitting"
    );
    backend.0.lock().unwrap().offline = false;
    assert_eq!(
        store
            .get(&backend, &first.operation_id)
            .unwrap()
            .unwrap()
            .status,
        "confirmed"
    );
    backend.0.lock().unwrap().observed.clear();
    assert_eq!(
        store
            .get(&backend, &first.operation_id)
            .unwrap()
            .unwrap()
            .status,
        "submitting"
    );
    assert!(backend.0.lock().unwrap().cancel_calls.is_empty());
}
fn interrupted_record(req: OutputRequest, before: Vec<String>) -> Record {
    Record {
        version: 1,
        revision: 0,
        restored_ambiguous: false,
        retained_pending: vec![],
        operation: OutputOperation {
            operation_id: req.operation_id.clone(),
            txid: None,
            status: "preparing".into(),
            can_cancel: false,
            can_commit: false,
            inputs: vec![],
            num_outputs: 0,
            size_sat: req.size,
            fee_sat: 0,
            fee_rate: req.fee_rate,
        },
        request: req,
        before_pending: before,
        unsigned_psbt: None,
        signed_psbt: None,
    }
}
#[test]
fn interrupted_prepare_only_cleans_its_proven_reservation() {
    let (_, store) = store();
    let backend = Backend::new();
    let before = store.prepare(&backend, request(1)).unwrap();
    let mut record = interrupted_record(request(2), vec![before.txid.clone().unwrap()]);
    store.write(&mut record).unwrap();
    let own = backend.prepare(&request(2)).unwrap();
    let own_txid = Psbt::from_str(&own.unsigned_psbt)
        .unwrap()
        .unsigned_tx
        .compute_txid()
        .to_string();
    assert_eq!(
        store
            .get(&backend, &record.operation.operation_id)
            .unwrap()
            .unwrap()
            .status,
        "failed"
    );
    assert_eq!(backend.pending().unwrap().len(), 1);
    assert_eq!(backend.pending().unwrap()[0].txid, before.txid.unwrap());
    assert_eq!(backend.0.lock().unwrap().cancel_calls, vec![own_txid]);
}
#[test]
fn interrupted_prepare_ambiguous_ownership_never_aborts() {
    let (_, store) = store();
    let backend = Backend::new();
    let mut record = interrupted_record(request(1), vec![]);
    store.write(&mut record).unwrap();
    backend.prepare(&request(2)).unwrap();
    backend.prepare(&request(3)).unwrap();
    assert_eq!(
        store
            .get(&backend, &record.operation.operation_id)
            .unwrap()
            .unwrap()
            .status,
        "preparing"
    );
    assert_eq!(backend.pending().unwrap().len(), 2);
    assert!(backend.0.lock().unwrap().cancel_calls.is_empty());
}
#[test]
fn legacy_unowned_reservation_is_preserved_while_independent_inputs_are_used() {
    let (_, store) = store();
    let backend = Backend::new();
    let legacy = backend.prepare(&request(1)).unwrap();
    let legacy_txid = Psbt::from_str(&legacy.unsigned_psbt)
        .unwrap()
        .unsigned_tx
        .compute_txid()
        .to_string();
    let fresh = store.prepare(&backend, request(2)).unwrap();
    assert_ne!(fresh.txid.as_deref(), Some(legacy_txid.as_str()));
    store.cancel(&backend, &fresh.operation_id).unwrap();
    assert_eq!(backend.pending().unwrap().len(), 1);
    assert_eq!(backend.pending().unwrap()[0].txid, legacy_txid);
    assert!(!backend
        .0
        .lock()
        .unwrap()
        .cancel_calls
        .contains(&legacy_txid));
}
#[test]
fn corruption_of_status_inputs_or_fee_is_rejected_before_wallet_mutation() {
    for field in ["status", "fee_sat", "inputs"] {
        let (directory, store) = store();
        let backend = Backend::new();
        let first = store.prepare(&backend, request(1)).unwrap();
        let path = directory.path().join(&first.operation_id);
        let mut value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        match field {
            "status" => value["operation"][field] = "ready".into(),
            "fee_sat" => value["operation"][field] = 1.into(),
            _ => value["operation"][field][0]["vout"] = 8.into(),
        }
        std::fs::write(path, serde_json::to_vec(&value).unwrap()).unwrap();
        assert!(store.commit(&backend, &first.operation_id).is_err());
        let state = backend.0.lock().unwrap();
        assert!(state.finish_calls.is_empty());
        assert!(state.cancel_calls.is_empty());
    }
}
#[test]
fn terminal_tombstones_do_not_consume_live_capacity_or_permit_id_reuse() {
    let (_, store) = store();
    let backend = Backend::new();
    for number in 1..=100u8 {
        let req = request(number);
        let operation = store.prepare(&backend, req.clone()).unwrap();
        store.cancel(&backend, &operation.operation_id).unwrap();
        assert_eq!(store.prepare(&backend, req).unwrap().status, "cancelled");
    }
    assert!(store.list(&backend).unwrap().is_empty());
    assert_eq!(backend.0.lock().unwrap().begin_calls, 100);
}

#[test]
fn restored_prepared_plan_cannot_be_cancelled_but_exact_transaction_can_be_reapproved() {
    let (_, store) = store();
    let backend = Backend::new();
    let first = store.prepare(&backend, request(1)).unwrap();
    store
        .mark_restored(
            &backend
                .pending()
                .unwrap()
                .into_iter()
                .map(|pending| pending.txid)
                .collect::<Vec<_>>(),
        )
        .unwrap();
    let restored = store.get(&backend, &first.operation_id).unwrap().unwrap();
    assert!(!restored.can_cancel);
    assert!(restored.can_commit);
    assert!(store.cancel(&backend, &first.operation_id).is_err());
    assert_eq!(
        store.commit(&backend, &first.operation_id).unwrap().txid,
        first.txid
    );
    assert!(backend.0.lock().unwrap().cancel_calls.is_empty());
}
#[test]
fn sticky_unknown_preparing_allows_independent_inputs_without_later_aborting_them() {
    let (_, store) = store();
    let backend = Backend::new();
    let mut interrupted = interrupted_record(request(1), vec![]);
    store.write(&mut interrupted).unwrap();
    backend.prepare(&request(2)).unwrap();
    backend.prepare(&request(3)).unwrap();
    let unknown = store
        .get(&backend, &interrupted.operation.operation_id)
        .unwrap()
        .unwrap();
    assert_eq!(unknown.status, "preparing");
    assert!(!unknown.can_cancel);
    assert!(!unknown.can_commit);
    let fresh = store.prepare(&backend, request(4)).unwrap();
    assert_eq!(fresh.status, "prepared");
    assert_eq!(
        store
            .get(&backend, &interrupted.operation.operation_id)
            .unwrap()
            .unwrap(),
        unknown
    );
    assert_eq!(backend.pending().unwrap().len(), 3);
    assert!(backend.0.lock().unwrap().cancel_calls.is_empty());
}
#[test]
fn restored_preparing_is_retained_even_when_only_one_reservation_exists() {
    let (_, store) = store();
    let backend = Backend::new();
    let mut interrupted = interrupted_record(request(1), vec![]);
    store.write(&mut interrupted).unwrap();
    backend.prepare(&request(1)).unwrap();
    store
        .mark_restored(
            &backend
                .pending()
                .unwrap()
                .into_iter()
                .map(|pending| pending.txid)
                .collect::<Vec<_>>(),
        )
        .unwrap();
    assert_eq!(
        store
            .get(&backend, &interrupted.operation.operation_id)
            .unwrap()
            .unwrap()
            .status,
        "preparing"
    );
    assert_eq!(backend.pending().unwrap().len(), 1);
    assert!(backend.0.lock().unwrap().cancel_calls.is_empty());
    assert_eq!(
        store.prepare(&backend, request(2)).unwrap().status,
        "prepared"
    );
}

#[cfg(feature = "electrum")]
mod funded {
    use super::*;
    use electrum_client::ElectrumApi;
    use rgb_lib::{
        keys::{generate_keys, WitnessVersion},
        wallet::{
            DatabaseType, IndexerConfig, Online, OnlineOptions, RgbWalletOpsOffline, SinglesigKeys,
            Wallet, WalletData,
        },
        AssetSchema, BitcoinNetwork,
    };
    use std::{
        process::Command,
        sync::atomic::{AtomicBool, Ordering},
        time::{Duration, Instant},
    };

    struct FundedBackend {
        wallet: Mutex<Wallet>,
        signer: Mutex<Wallet>,
        online: Online,
        fail_sign: AtomicBool,
    }
    impl OutputBackend for FundedBackend {
        fn validate_plan(&self, psbt: &str, operation: &OutputOperation) -> Result<(), APIError> {
            let psbt = Psbt::from_str(psbt).unwrap();
            assert_eq!(
                colored_output_count(&self.wallet.lock().unwrap(), &psbt, operation.size_sat)?,
                operation.num_outputs
            );
            Ok(())
        }
        fn pending(&self) -> Result<Vec<PendingOutput>, APIError> {
            Ok(self
                .wallet
                .lock()
                .unwrap()
                .list_pending_vanilla_txs()?
                .into_iter()
                .map(|pending| PendingOutput {
                    txid: pending.txid,
                    operation_type: match pending.r#type {
                        rgb_lib::WalletTransactionType::CreateUtxos => "CreateUtxos",
                        rgb_lib::WalletTransactionType::SendBtc => "SendBtc",
                        rgb_lib::WalletTransactionType::Drain => "Drain",
                    }
                    .into(),
                })
                .collect())
        }
        fn prepare(&self, request: &OutputRequest) -> Result<PreparedOutput, APIError> {
            prepare_native_outputs(&mut self.wallet.lock().unwrap(), self.online, request)
        }
        fn sign(&self, unsigned: String) -> Result<String, APIError> {
            if self.fail_sign.load(Ordering::SeqCst) {
                return Err(APIError::ExternalSignerUnavailable(
                    "injected fixture signer failure".into(),
                ));
            }
            self.signer
                .lock()
                .unwrap()
                .sign_psbt(unsigned, None)
                .map_err(APIError::from)
        }
        fn finish(&self, signed: String) -> Result<u8, APIError> {
            self.wallet
                .lock()
                .unwrap()
                .create_utxos_end(self.online, signed)
                .map_err(APIError::from)
        }
        fn cancel(&self, txid: String) -> Result<(), APIError> {
            self.wallet
                .lock()
                .unwrap()
                .abort_pending_vanilla_tx(txid)
                .map_err(APIError::from)
        }
        fn observed(&self, txid: &str) -> Result<Option<bool>, APIError> {
            Ok(self
                .wallet
                .lock()
                .unwrap()
                .list_transactions(Some(self.online), false)?
                .into_iter()
                .find(|transaction| transaction.txid == txid)
                .map(|transaction| transaction.confirmation_time.is_some()))
        }
    }
    fn assert_fixture_admission() {
        assert_eq!(
            std::env::var("COMPOSE_PROJECT_NAME").as_deref(),
            Ok("iris_rgb_output_20261026"),
            "funded tests require the exact isolated compose project"
        );
        let expected = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("qualification-consumers/rgb-output-native-fixture-20261026/compose.yaml")
            .canonicalize()
            .expect("owned isolated fixture Compose file must exist");
        let configured = PathBuf::from(
            std::env::var_os("COMPOSE_FILE")
                .expect("funded tests require an explicit owned COMPOSE_FILE"),
        )
        .canonicalize()
        .expect("configured fixture Compose file must exist");
        assert_eq!(
            configured, expected,
            "funded tests cannot operate any other Compose configuration"
        );
    }
    fn bitcoin(args: &[&str]) -> serde_json::Value {
        assert_fixture_admission();
        let output = Command::new("docker")
            .args([
                "compose",
                "exec",
                "-T",
                "-u",
                "blits",
                "bitcoind",
                "bitcoin-cli",
                "-regtest",
                "-rpcuser=user",
                "-rpcpassword=password",
                "-rpcwallet=iris-output-tests",
            ])
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "isolated bitcoind fixture command failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let output = String::from_utf8(output.stdout).unwrap();
        serde_json::from_str(&output).unwrap_or_else(|_| output.trim().into())
    }
    fn mine(address: &str, count: &str) {
        bitcoin(&["generatetoaddress", count, address]);
        let expected = bitcoin(&["getblockcount"]).as_u64().unwrap();
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if let Ok(client) = electrum_client::Client::new("tcp://127.0.0.1:50001") {
                if let Ok(header) = client.block_headers_subscribe() {
                    if header.height as u64 >= expected {
                        break;
                    }
                }
            }
            assert!(
                Instant::now() < deadline,
                "isolated electrs did not catch up"
            );
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    fn sql(path: &std::path::Path, query: &str) {
        let output = Command::new("sqlite3")
            .arg(path)
            .arg(query)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "fixture SQL fault injection failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    fn fixture() -> (tempfile::TempDir, tempfile::TempDir, FundedBackend, String) {
        // Admission must precede even wallet creation, not merely the first funding call.
        assert_fixture_admission();
        let create = Command::new("docker")
            .args([
                "compose",
                "exec",
                "-T",
                "-u",
                "blits",
                "bitcoind",
                "bitcoin-cli",
                "-regtest",
                "-rpcuser=user",
                "-rpcpassword=password",
                "createwallet",
                "iris-output-tests",
            ])
            .output()
            .unwrap();
        assert!(
            create.status.success()
                || String::from_utf8_lossy(&create.stderr).contains("already exists")
        );
        let miner = bitcoin(&["getnewaddress"]).as_str().unwrap().to_string();
        if bitcoin(&["getblockcount"]).as_u64().unwrap() < 101 {
            mine(&miner, "101");
        }
        let watch_dir = tempfile::tempdir().unwrap();
        let sign_dir = tempfile::tempdir().unwrap();
        let keys = generate_keys(BitcoinNetwork::Regtest, WitnessVersion::Taproot);
        let data = |directory: &std::path::Path| WalletData {
            data_dir: directory.to_string_lossy().into(),
            bitcoin_network: BitcoinNetwork::Regtest,
            database_type: DatabaseType::Sqlite,
            max_allocations_per_utxo: 1,
            supported_schemas: vec![AssetSchema::Nia],
        };
        let mut watch = Wallet::new(
            data(watch_dir.path()),
            SinglesigKeys::from_keys_no_mnemonic(&keys, None),
        )
        .unwrap();
        let signer =
            Wallet::new(data(sign_dir.path()), SinglesigKeys::from_keys(&keys, None)).unwrap();
        let online = watch
            .go_online(OnlineOptions {
                indexer_config: IndexerConfig::Esplora {
                    url: "http://127.0.0.1:3002".into(),
                },
                skip_consistency_check: false,
                vanilla_sync_lookback: 20,
                failed_witness_receive_grace_secs: 3600,
                proxy_auth_keys: HashMap::new(),
                evm_rpc_urls: HashMap::new(),
            })
            .unwrap();
        assert!(
            watch.get_keys().mnemonic.is_none(),
            "the transaction wallet must remain watch-only"
        );
        (
            watch_dir,
            sign_dir,
            FundedBackend {
                wallet: Mutex::new(watch),
                signer: Mutex::new(signer),
                online,
                fail_sign: AtomicBool::new(false),
            },
            miner,
        )
    }
    fn fund(backend: &FundedBackend, miner: &str, amount: &str) -> String {
        let address = backend.wallet.lock().unwrap().get_address().unwrap();
        let txid = bitcoin(&["sendtoaddress", &address, amount])
            .as_str()
            .unwrap()
            .to_string();
        mine(miner, "1");
        backend
            .wallet
            .lock()
            .unwrap()
            .list_unspents_vanilla(backend.online, 1, false)
            .unwrap();
        txid
    }

    #[test]
    #[ignore = "Requires exact isolated iris_rgb_output_20261026 Bitcoin/Electrum fixture; never runs against shared stack"]
    fn funded_external_watch_only_output_operation_recovery() {
        let (_watch_dir, _sign_dir, backend, miner) = fixture();
        let wallet_dir = backend.wallet.lock().unwrap().get_wallet_dir();
        let store = OutputStore::new(wallet_dir.join("rln_output_operations_v1"));
        fund(&backend, &miner, "0.00200000");
        let mut req = request(1);
        req.skip_sync = false;
        let prepared = store.prepare(&backend, req.clone()).unwrap();
        assert_eq!(prepared.num_outputs, 4);
        assert!(prepared.fee_sat > 0);
        assert_eq!(prepared.inputs.len(), 1);
        let reservation = backend.pending().unwrap();
        assert_eq!(reservation.len(), 1);
        assert_eq!(reservation[0].txid, prepared.txid.clone().unwrap());
        assert_eq!(
            backend
                .wallet
                .lock()
                .unwrap()
                .get_btc_balance(Some(backend.online), false)
                .unwrap()
                .vanilla
                .spendable,
            0
        );
        backend.fail_sign.store(true, Ordering::SeqCst);
        assert!(store.commit(&backend, &prepared.operation_id).is_err());
        assert!(backend.pending().unwrap().is_empty());
        assert_eq!(
            store
                .get(&backend, &prepared.operation_id)
                .unwrap()
                .unwrap()
                .status,
            "failed"
        );
        backend.fail_sign.store(false, Ordering::SeqCst);
        let mut second_req = request(2);
        second_req.skip_sync = false;
        let second = store.prepare(&backend, second_req).unwrap();
        assert_eq!(second.inputs, prepared.inputs);
        // Beginning another setup with all inputs reserved cannot spend them again.
        let mut rejected = request(3);
        rejected.skip_sync = false;
        assert!(store.prepare(&backend, rejected).is_err());
        // New independent confirmed funds remain usable while the first review is reserved.
        let independent_txid = fund(&backend, &miner, "0.00200000");
        let mut third_req = request(4);
        third_req.skip_sync = false;
        let third = store.prepare(&backend, third_req).unwrap();
        assert!(third
            .inputs
            .iter()
            .all(|input| input.txid == independent_txid));
        assert!(!third
            .inputs
            .iter()
            .any(|input| second.inputs.contains(input)));
        store.cancel(&backend, &third.operation_id).unwrap();
        // Fail a genuine database write after RGB's broadcast+BDK durable flush. The enclosing
        // RGB transaction rolls back, leaving the original durable input reservation intact.
        let database = wallet_dir.join("rgb_lib_db");
        sql(&database, "CREATE TRIGGER iris_fail_after_broadcast BEFORE INSERT ON txo BEGIN SELECT RAISE(ABORT,'injected post-broadcast bookkeeping failure'); END;");
        let result = store.commit(&backend, &second.operation_id).unwrap();
        assert_eq!(result.status, "submitting");
        let txid = second.txid.clone().unwrap();
        assert!(bitcoin(&["getrawmempool"])
            .as_array()
            .unwrap()
            .iter()
            .any(|value| value.as_str() == Some(&txid)));
        assert!(backend
            .pending()
            .unwrap()
            .iter()
            .any(|pending| pending.txid == txid));
        assert!(store.cancel(&backend, &second.operation_id).is_err());
        sql(&database, "DROP TRIGGER iris_fail_after_broadcast;");
        let reopened = OutputStore::new(wallet_dir.join("rln_output_operations_v1"));
        assert_eq!(
            reopened
                .get(&backend, &second.operation_id)
                .unwrap()
                .unwrap()
                .status,
            "broadcast"
        );
        assert!(!backend
            .pending()
            .unwrap()
            .iter()
            .any(|pending| pending.txid == txid));
        assert_eq!(
            reopened
                .commit(&backend, &second.operation_id)
                .unwrap()
                .txid,
            Some(txid.clone())
        );
        let setup_txs = backend
            .wallet
            .lock()
            .unwrap()
            .list_transactions(Some(backend.online), false)
            .unwrap()
            .into_iter()
            .filter(|tx| tx.transaction_type == rgb_lib::wallet::TransactionType::CreateUtxos)
            .collect::<Vec<_>>();
        assert_eq!(setup_txs.len(), 1);
        assert_eq!(setup_txs[0].txid, txid);
        assert_eq!(setup_txs[0].fee, second.fee_sat);
        mine(&miner, "1");
        assert_eq!(
            reopened
                .get(&backend, &second.operation_id)
                .unwrap()
                .unwrap()
                .status,
            "confirmed"
        );
        let outputs = backend
            .wallet
            .lock()
            .unwrap()
            .list_unspents(None, false, true)
            .unwrap();
        assert_eq!(
            outputs
                .iter()
                .filter(|output| output.utxo.colorable
                    && output.utxo.exists
                    && output.utxo.outpoint.txid == txid)
                .count(),
            usize::from(second.num_outputs)
        );
        // The operation journal is included in RGB's content-addressed snapshot and restored
        // atomically with its reservations; it is not an unsnapshotted second database.
        let snapshot = backend.wallet.lock().unwrap().prepare_snapshot().unwrap();
        let archive = zip::ZipArchive::new(std::io::Cursor::new(snapshot.data)).unwrap();
        assert!(archive.file_names().any(|name| name
            .expect("valid snapshot entry name")
            .contains("rln_output_operations_v1/completed/")));
        eprintln!("Funded watch-only setup: signer failure cleanup, reserved-input exclusion, independent funding, real broadcast/SQL-failure recovery, exact txid/fee/count, idempotent recommit, confirmation and snapshot inclusion passed");
    }
    #[test]
    #[ignore = "Requires exact isolated iris_rgb_output_20261026 Bitcoin/Electrum fixture; never runs against shared stack"]
    fn funded_legacy_reservation_does_not_block_independent_funding_or_reduce_truthful_review() {
        let (_watch_dir, _sign_dir, backend, miner) = fixture();
        let wallet_dir = backend.wallet.lock().unwrap().get_wallet_dir();
        let store = OutputStore::new(wallet_dir.join("rln_output_operations_v1"));
        fund(&backend, &miner, "0.00200000");
        let mut legacy_request = request(11);
        legacy_request.skip_sync = false;
        // Simulate an old runtime's begin whose journal/signature status is unavailable.
        // The new runtime must neither replay it nor assume it is safe to cancel.
        let legacy = backend.prepare(&legacy_request).unwrap();
        let legacy_psbt = Psbt::from_str(&legacy.unsigned_psbt).unwrap();
        let legacy_txid = legacy_psbt.unsigned_tx.compute_txid().to_string();
        let mut unavailable = request(12);
        unavailable.skip_sync = false;
        assert!(store.prepare(&backend, unavailable).is_err());
        assert_eq!(backend.pending().unwrap().len(), 1);
        assert_eq!(backend.pending().unwrap()[0].txid, legacy_txid);
        let separate_funding = fund(&backend, &miner, "0.00050000");
        let mut fresh_request = request(13);
        fresh_request.skip_sync = false;
        let fresh = store.prepare(&backend, fresh_request).unwrap();
        // Real native construction reduces four requested 20k outputs to two, with the exact
        // fee and input ownership still shown in the public reviewed operation.
        assert_eq!(fresh.num_outputs, 2);
        assert!(fresh.fee_sat > 0);
        assert!(fresh
            .inputs
            .iter()
            .all(|input| input.txid == separate_funding));
        assert!(!fresh.inputs.iter().any(|input| legacy_psbt
            .unsigned_tx
            .input
            .iter()
            .any(|old| old.previous_output.txid.to_string() == input.txid
                && old.previous_output.vout == input.vout)));
        assert_eq!(backend.pending().unwrap().len(), 2);
        store.cancel(&backend, &fresh.operation_id).unwrap();
        assert_eq!(backend.pending().unwrap().len(), 1);
        assert_eq!(backend.pending().unwrap()[0].txid, legacy_txid);
        assert!(bitcoin(&["getrawmempool"]).as_array().unwrap().is_empty());
        eprintln!("Real legacy reservation retained; unavailable inputs reject locally; independent confirmed funding prepares exactly two outputs with native fee and no legacy input reuse");
    }
}
