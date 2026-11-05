//! Durable ownership of RGB-output setup transactions.
//!
//! The unsigned transaction is persisted before review is returned. A signed transaction is
//! persisted as `submitting` *before* calling the library's broadcasting `_end` method. Only
//! that exact transaction can be retried. Network absence is never permission to cancel it.

use std::{collections::HashSet, path::PathBuf, str::FromStr};

use bitcoin::psbt::Psbt;
use lightning::util::persist::KVStoreSync;
use lightning_persister::fs_store::FilesystemStore;
use serde::{Deserialize, Serialize};

use crate::error::APIError;

const MAX_RECORD_BYTES: usize = 2 * 1024 * 1024;
// Bound unfinished plans, not lifetime reviews. Completed/cancelled records are durable
// tombstones in a separate namespace so old operation IDs can never create a new payment.
const MAX_OPERATIONS: usize = 64;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct OutputRequest {
    pub operation_id: String,
    pub up_to: bool,
    pub num: u8,
    pub size: u32,
    pub fee_rate: u64,
    pub skip_sync: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct OutputInput {
    pub txid: String,
    pub vout: u32,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct OutputOperation {
    pub operation_id: String,
    pub txid: Option<String>,
    pub status: String,
    pub can_cancel: bool,
    pub can_commit: bool,
    pub inputs: Vec<OutputInput>,
    pub num_outputs: u8,
    pub size_sat: u32,
    pub fee_sat: u64,
    pub fee_rate: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Record {
    version: u8,
    revision: u64,
    restored_ambiguous: bool,
    retained_pending: Vec<String>,
    request: OutputRequest,
    operation: OutputOperation,
    before_pending: Vec<String>,
    unsigned_psbt: Option<String>,
    signed_psbt: Option<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct PreparedOutput {
    pub unsigned_psbt: String,
    pub num_outputs: u8,
}

#[derive(Clone, Debug)]
pub(crate) struct PendingOutput {
    pub txid: String,
    pub operation_type: String,
}

/// Implementations serialize calls with the wallet's operation mutex. `prepare` reserves the
/// PSBT's inputs, `cancel` only releases that exact reservation, and `finish` may broadcast.
pub(crate) trait OutputBackend {
    fn pending(&self) -> Result<Vec<PendingOutput>, APIError>;
    fn prepare(&self, request: &OutputRequest) -> Result<PreparedOutput, APIError>;
    fn sign(&self, unsigned_psbt: String) -> Result<String, APIError>;
    fn finish(&self, signed_psbt: String) -> Result<u8, APIError>;
    fn cancel(&self, txid: String) -> Result<(), APIError>;
    /// An exact local/network-observed transaction; the bool denotes confirmation.
    fn observed(&self, txid: &str) -> Result<Option<bool>, APIError>;
    fn validate_plan(&self, _psbt: &str, _operation: &OutputOperation) -> Result<(), APIError> {
        Ok(())
    }
}

pub(crate) fn validate_operation_id(id: &str) -> Result<(), APIError> {
    if !(32..=64).contains(&id.len())
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(APIError::InvalidRequest(
            "RGB output operation_id must contain 32–64 lowercase hexadecimal characters".into(),
        ));
    }
    Ok(())
}

fn journal_error() -> APIError {
    APIError::Unexpected(
        "RGB output operation journal could not be read or persisted safely".into(),
    )
}

fn validate_record(record: &Record, id: &str) -> Result<(), APIError> {
    validate_operation_id(id)?;
    let operation = &record.operation;
    let can_cancel = operation.status == "prepared" && !record.restored_ambiguous;
    let can_commit = matches!(operation.status.as_str(), "prepared" | "submitting");
    if operation.can_cancel != can_cancel || operation.can_commit != can_commit {
        return Err(journal_error());
    }
    if record.version != 1
        || operation.operation_id != id
        || record.request.operation_id != id
        || record.request.num == 0
        || record.request.size == 0
        || record.request.fee_rate == 0
        || operation.size_sat != record.request.size
        || operation.fee_rate != record.request.fee_rate
        || !matches!(
            operation.status.as_str(),
            "preparing"
                | "prepared"
                | "submitting"
                | "broadcast"
                | "confirmed"
                | "cancelled"
                | "failed"
        )
        || record
            .before_pending
            .iter()
            .chain(&record.retained_pending)
            .any(|txid| bitcoin::Txid::from_str(txid).is_err())
    {
        return Err(journal_error());
    }
    let active = matches!(
        operation.status.as_str(),
        "prepared" | "submitting" | "broadcast" | "confirmed"
    );
    if active
        && (record.unsigned_psbt.is_none()
            || operation.txid.is_none()
            || operation.inputs.is_empty()
            || operation.num_outputs == 0
            || operation.num_outputs > record.request.num
            || operation.fee_sat == 0)
    {
        return Err(journal_error());
    }
    if operation.status == "preparing"
        && (record.unsigned_psbt.is_some()
            || record.signed_psbt.is_some()
            || operation.txid.is_some()
            || !operation.inputs.is_empty()
            || operation.num_outputs != 0
            || operation.fee_sat != 0)
    {
        return Err(journal_error());
    }
    if matches!(
        operation.status.as_str(),
        "prepared" | "cancelled" | "failed"
    ) && record.signed_psbt.is_some()
    {
        return Err(journal_error());
    }
    if matches!(
        operation.status.as_str(),
        "submitting" | "broadcast" | "confirmed"
    ) && record.signed_psbt.is_none()
    {
        return Err(journal_error());
    }
    if let Some(unsigned) = &record.unsigned_psbt {
        let psbt = Psbt::from_str(unsigned).map_err(|_| journal_error())?;
        let inputs = psbt
            .unsigned_tx
            .input
            .iter()
            .map(|input| OutputInput {
                txid: input.previous_output.txid.to_string(),
                vout: input.previous_output.vout,
            })
            .collect::<Vec<_>>();
        let unique = inputs
            .iter()
            .map(|input| (&input.txid, input.vout))
            .collect::<HashSet<_>>();
        if Some(psbt.unsigned_tx.compute_txid().to_string()) != operation.txid
            || inputs != operation.inputs
            || unique.len() != inputs.len()
            || psbt.fee().map_err(|_| journal_error())?.to_sat() != operation.fee_sat
        {
            return Err(journal_error());
        }
        if let Some(signed) = &record.signed_psbt {
            let signed = Psbt::from_str(signed).map_err(|_| journal_error())?;
            if signed.unsigned_tx != psbt.unsigned_tx
                || !signed.inputs.iter().all(|input| {
                    input.final_script_sig.is_some() || input.final_script_witness.is_some()
                })
            {
                return Err(journal_error());
            }
        }
    }
    Ok(())
}

pub(crate) struct OutputStore {
    store: FilesystemStore,
    directory: PathBuf,
}

pub(crate) struct NativeOutputBackend {
    pub wallet: std::sync::Arc<crate::utils::UnlockedAppState>,
    observed_cache: std::sync::Mutex<Option<Result<Vec<rgb_lib::wallet::Transaction>, ()>>>,
}

impl NativeOutputBackend {
    pub(crate) fn new(wallet: std::sync::Arc<crate::utils::UnlockedAppState>) -> Self {
        Self {
            wallet,
            observed_cache: std::sync::Mutex::new(None),
        }
    }
}

impl OutputBackend for NativeOutputBackend {
    fn validate_plan(&self, psbt: &str, operation: &OutputOperation) -> Result<(), APIError> {
        let wallet = self.wallet.rgb_wallet_wrapper.get_rgb_wallet();
        let psbt = Psbt::from_str(psbt).map_err(|_| journal_error())?;
        if colored_output_count(&wallet, &psbt, operation.size_sat)? != operation.num_outputs {
            return Err(journal_error());
        }
        Ok(())
    }
    fn pending(&self) -> Result<Vec<PendingOutput>, APIError> {
        self.wallet
            .rgb_wallet_wrapper
            .get_rgb_wallet()
            .list_pending_vanilla_txs()
            .map(|pending| {
                pending
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
                    .collect()
            })
            .map_err(APIError::from)
    }

    fn prepare(&self, request: &OutputRequest) -> Result<PreparedOutput, APIError> {
        let mut wallet = self.wallet.rgb_wallet_wrapper.get_rgb_wallet();
        prepare_native_outputs(&mut wallet, self.wallet.rgb_wallet_wrapper.online, request)
    }

    fn sign(&self, unsigned_psbt: String) -> Result<String, APIError> {
        self.wallet
            .rgb_sign_psbt(unsigned_psbt)
            .map_err(APIError::from)
    }

    fn finish(&self, signed_psbt: String) -> Result<u8, APIError> {
        // A prior read must not hide the transaction that this exact retry broadcasts.
        *self.observed_cache.lock().map_err(|_| journal_error())? = None;
        self.wallet
            .rgb_create_utxos_end(signed_psbt, true)
            .map_err(APIError::from)
    }

    fn cancel(&self, txid: String) -> Result<(), APIError> {
        self.wallet
            .rgb_abort_pending_vanilla_tx(txid)
            .map_err(APIError::from)
    }

    fn observed(&self, txid: &str) -> Result<Option<bool>, APIError> {
        let mut cache = self.observed_cache.lock().map_err(|_| journal_error())?;
        if cache.is_none() {
            *cache = Some(
                self.wallet
                    .rgb_wallet_wrapper
                    .list_transactions(false)
                    .map_err(|_| ()),
            );
        }
        match cache.as_ref().unwrap() {
            Ok(transactions) => Ok(transactions
                .iter()
                .find(|transaction| transaction.txid == txid)
                .map(|transaction| transaction.confirmation_time.is_some())),
            Err(_) => Err(APIError::Unexpected(
                "RGB output transaction observation is unavailable".into(),
            )),
        }
    }
}

fn prepare_native_outputs(
    wallet: &mut rgb_lib::wallet::Wallet,
    online: rgb_lib::wallet::Online,
    request: &OutputRequest,
) -> Result<PreparedOutput, APIError> {
    let unsigned_psbt = wallet.create_utxos_begin(
        online,
        request.up_to,
        Some(request.num),
        Some(request.size),
        request.fee_rate,
        request.skip_sync,
        false,
    )?;
    let psbt = Psbt::from_str(&unsigned_psbt).map_err(|_| journal_error())?;
    let count = colored_output_count(wallet, &psbt, request.size)?;
    Ok(PreparedOutput {
        unsigned_psbt,
        num_outputs: count,
    })
}

fn colored_output_count(
    wallet: &rgb_lib::wallet::Wallet,
    psbt: &Psbt,
    size: u32,
) -> Result<u8, APIError> {
    use bitcoin::bip32::ChildNumber;
    use rgb_lib::bdk_wallet::miniscript::{Descriptor, DescriptorPublicKey};
    let secp = bitcoin::secp256k1::Secp256k1::new();
    // A mnemonic-backed RGB wallet exposes a secret-bearing descriptor, whereas an external
    // signer's watch-only wallet exposes an xpub descriptor. Normalize both through miniscript's
    // maintained parser, retaining only its public descriptor. Never log or use the key map to sign.
    let (descriptor, _) = Descriptor::<DescriptorPublicKey>::parse_descriptor(
        &secp,
        &wallet.get_descriptors().colored,
    )
    .map_err(|_| journal_error())?;
    let mut count = 0u8;
    for (output, metadata) in psbt.unsigned_tx.output.iter().zip(&psbt.outputs) {
        let mut indices = HashSet::from([0]);
        for (_, path) in metadata.bip32_derivation.values() {
            if let Some(ChildNumber::Normal { index }) = path.as_ref().last() {
                indices.insert(*index);
            }
        }
        for (_, (_, path)) in metadata.tap_key_origins.values() {
            if let Some(ChildNumber::Normal { index }) = path.as_ref().last() {
                indices.insert(*index);
            }
        }
        let is_colored = indices.into_iter().any(|index| {
            descriptor
                .at_derivation_index(index)
                .ok()
                .and_then(|descriptor| descriptor.derived_descriptor(&secp).ok())
                .is_some_and(|descriptor| descriptor.script_pubkey() == output.script_pubkey)
        });
        if is_colored {
            if output.value.to_sat() != u64::from(size) {
                return Err(journal_error());
            }
            count = count.checked_add(1).ok_or_else(journal_error)?;
        }
    }
    Ok(count)
}

impl OutputStore {
    pub(crate) fn new(directory: PathBuf) -> Self {
        Self {
            store: FilesystemStore::new(directory.clone()),
            directory,
        }
    }

    fn read_namespace(&self, namespace: &str, id: &str) -> Result<Option<Record>, APIError> {
        use std::io::Read;
        let path = self.directory.join(namespace).join(id);
        let metadata = match std::fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(journal_error()),
        };
        if !metadata.is_file()
            || metadata.file_type().is_symlink()
            || metadata.len() > MAX_RECORD_BYTES as u64
        {
            return Err(journal_error());
        }
        let mut bytes = Vec::new();
        std::fs::File::open(path)
            .map_err(|_| journal_error())?
            .take(MAX_RECORD_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| journal_error())?;
        if bytes.len() > MAX_RECORD_BYTES {
            return Err(journal_error());
        }
        let record: Record = serde_json::from_slice(&bytes).map_err(|_| journal_error())?;
        validate_record(&record, id)?;
        Ok(Some(record))
    }

    fn read(&self, id: &str) -> Result<Option<Record>, APIError> {
        validate_operation_id(id)?;
        let live = self.read_namespace("", id)?;
        let completed = self.read_namespace("completed", id)?;
        Ok(match (live, completed) {
            (Some(a), Some(b)) => Some(if a.revision > b.revision { a } else { b }),
            (a, b) => a.or(b),
        })
    }

    fn write(&self, record: &mut Record) -> Result<(), APIError> {
        record.operation.can_cancel =
            record.operation.status == "prepared" && !record.restored_ambiguous;
        record.operation.can_commit =
            matches!(record.operation.status.as_str(), "prepared" | "submitting");
        record.revision = record.revision.checked_add(1).ok_or_else(journal_error)?;
        validate_record(record, &record.operation.operation_id)?;
        let bytes = serde_json::to_vec(record).map_err(|_| journal_error())?;
        if bytes.len() > MAX_RECORD_BYTES {
            return Err(journal_error());
        }
        let completed = matches!(
            record.operation.status.as_str(),
            "cancelled" | "failed" | "confirmed"
        );
        let namespace = if completed { "completed" } else { "" };
        self.store
            .write(namespace, "", &record.operation.operation_id, bytes)
            .map_err(|_| journal_error())?;
        // Write the higher revision before removing the old location. A crash between them
        // preserves both copies; readers choose the highest durable revision.
        self.store
            .remove(
                if completed { "" } else { "completed" },
                "",
                &record.operation.operation_id,
                false,
            )
            .map_err(|_| journal_error())
    }

    /// A restored snapshot may precede a later broadcast. Preserve that uncertainty before
    /// clearing the restore marker, including when a process dies during this admission.
    pub(crate) fn mark_restored(&self, pending: &[String]) -> Result<(), APIError> {
        for id in self.store.list("", "").map_err(|_| journal_error())? {
            let mut record = self.read(&id)?.ok_or_else(journal_error)?;
            if matches!(record.operation.status.as_str(), "preparing" | "prepared")
                && !record.restored_ambiguous
            {
                record.restored_ambiguous = true;
                if record.operation.status == "preparing" {
                    record.retained_pending = pending
                        .iter()
                        .filter(|txid| !record.before_pending.contains(txid))
                        .cloned()
                        .collect();
                }
                self.write(&mut record)?;
            }
        }
        Ok(())
    }

    /// A `preparing` record proves this code has never entered the broadcast path. The wallet
    /// mutex serializes every CreateUtxos begin call, so a unique new reservation belongs to it.
    /// An ambiguous set is deliberately retained rather than guessing which inputs to release.
    fn recover_prepare<B: OutputBackend>(
        &self,
        backend: &B,
        record: &mut Record,
    ) -> Result<(), APIError> {
        if record.operation.status != "preparing" {
            return Ok(());
        }
        if record.restored_ambiguous {
            return Ok(());
        }
        let candidates = backend
            .pending()?
            .into_iter()
            .filter(|pending| {
                pending.operation_type == "CreateUtxos"
                    && !record.before_pending.contains(&pending.txid)
            })
            .collect::<Vec<_>>();
        if candidates.len() > 1 {
            // Freeze this ambiguous set before admitting any later prepare. It must never
            // become a fresh before/after comparison that could abort a newer operation.
            record.restored_ambiguous = true;
            record.retained_pending = candidates.into_iter().map(|pending| pending.txid).collect();
            return self.write(record);
        }
        if let Some(candidate) = candidates.into_iter().next() {
            backend.cancel(candidate.txid)?;
        }
        record.operation.status = "failed".into();
        self.write(record)
    }

    pub(crate) fn prepare<B: OutputBackend>(
        &self,
        backend: &B,
        request: OutputRequest,
    ) -> Result<OutputOperation, APIError> {
        validate_operation_id(&request.operation_id)?;
        if request.num == 0 || request.size == 0 || request.fee_rate == 0 {
            return Err(APIError::InvalidRequest(
                "RGB output count, size and fee rate must be positive".into(),
            ));
        }
        if let Some(mut record) = self.read(&request.operation_id)? {
            if record.request != request {
                return Err(APIError::InvalidRequest(
                    "RGB output operation_id is already bound to different parameters".into(),
                ));
            }
            self.verify_plan(backend, &record)?;
            self.recover_prepare(backend, &mut record)?;
            return Ok(record.operation);
        }
        if self.store.list("", "").map_err(|_| journal_error())?.len() >= MAX_OPERATIONS {
            return Err(APIError::InvalidRequest(
                "RGB output operation journal capacity reached".into(),
            ));
        }
        // Reconcile an interrupted preparation before allowing any other begin call to run.
        for id in self.store.list("", "").map_err(|_| journal_error())? {
            let mut record = self.read(&id)?.ok_or_else(journal_error)?;
            self.recover_prepare(backend, &mut record)?;
        }
        // Existing unowned reservations are retained in the durable baseline. They cannot
        // become this operation's interrupted-begin candidate, and native coin selection
        // excludes their inputs. Independent funds need not be globally blocked by them.
        let mut record = Record {
            version: 1,
            revision: 0,
            restored_ambiguous: false,
            retained_pending: vec![],
            operation: OutputOperation {
                operation_id: request.operation_id.clone(),
                txid: None,
                status: "preparing".into(),
                can_cancel: false,
                can_commit: false,
                inputs: vec![],
                num_outputs: 0,
                size_sat: request.size,
                fee_sat: 0,
                fee_rate: request.fee_rate,
            },
            before_pending: backend
                .pending()?
                .into_iter()
                .map(|pending| pending.txid)
                .collect(),
            request,
            unsigned_psbt: None,
            signed_psbt: None,
        };
        self.write(&mut record)?;
        let prepared = match backend.prepare(&record.request) {
            Ok(prepared) => prepared,
            Err(error) => {
                self.recover_prepare(backend, &mut record)?;
                return Err(error);
            }
        };
        let psbt = match Psbt::from_str(&prepared.unsigned_psbt) {
            Ok(psbt) => psbt,
            Err(_) => {
                self.recover_prepare(backend, &mut record)?;
                return Err(journal_error());
            }
        };
        let txid = psbt.unsigned_tx.compute_txid().to_string();
        let fee = psbt.fee().map_err(|_| journal_error())?.to_sat();
        if prepared.num_outputs == 0
            || prepared.num_outputs > record.request.num
            || fee == 0
            || psbt.inputs.is_empty()
        {
            self.recover_prepare(backend, &mut record)?;
            return Err(journal_error());
        }
        record.operation.txid = Some(txid);
        record.operation.status = "prepared".into();
        record.operation.inputs = psbt
            .unsigned_tx
            .input
            .iter()
            .map(|input| OutputInput {
                txid: input.previous_output.txid.to_string(),
                vout: input.previous_output.vout,
            })
            .collect();
        record.operation.num_outputs = prepared.num_outputs;
        record.operation.fee_sat = fee;
        record.unsigned_psbt = Some(prepared.unsigned_psbt);
        // If this write fails, the durable `preparing` record remains, allowing only safe
        // prebroadcast reservation cleanup on the next call.
        self.write(&mut record)?;
        Ok(record.operation)
    }

    fn verify_plan<B: OutputBackend>(&self, backend: &B, record: &Record) -> Result<(), APIError> {
        if let Some(psbt) = &record.unsigned_psbt {
            backend.validate_plan(psbt, &record.operation)?;
        }
        Ok(())
    }

    fn reconcile<B: OutputBackend>(
        &self,
        backend: &B,
        record: &mut Record,
    ) -> Result<(), APIError> {
        self.recover_prepare(backend, record)?;
        if !matches!(
            record.operation.status.as_str(),
            "submitting" | "broadcast" | "confirmed"
        ) {
            return Ok(());
        }
        let txid = record.operation.txid.as_ref().ok_or_else(journal_error)?;
        let still_reserved = backend
            .pending()?
            .iter()
            .any(|pending| &pending.txid == txid);
        let status = match backend.observed(txid) {
            Ok(Some(confirmed)) if !still_reserved => {
                if confirmed {
                    "confirmed"
                } else {
                    "broadcast"
                }
            }
            // Confirmation is a fresh observation, never a permanent readiness grant. An
            // outage/reorg/removal keeps the exact original transaction and input protection.
            _ => "submitting",
        };
        if record.operation.status != status {
            record.operation.status = status.into();
            self.write(record)?;
        }
        Ok(())
    }

    // The shared REST module compiles this SDK recovery path; SDK APIs live in the library.
    #[allow(dead_code)]
    fn recover_submission<B: OutputBackend>(
        &self,
        backend: &B,
        record: &mut Record,
    ) -> Result<(), APIError> {
        if record.operation.status != "submitting" {
            return Ok(());
        }
        let txid = record.operation.txid.as_ref().ok_or_else(journal_error)?;
        if backend
            .pending()?
            .iter()
            .any(|pending| &pending.txid == txid)
        {
            // This operation was already approved and durably marked submitting. Repeating
            // its exact signed PSBT can heal a crash after broadcast but before bookkeeping;
            // it can never select inputs or construct a second transaction.
            if let Ok(count) = backend.finish(record.signed_psbt.clone().ok_or_else(journal_error)?)
            {
                if count != record.operation.num_outputs {
                    return Err(journal_error());
                }
                record.operation.status = "broadcast".into();
                self.write(record)?;
            }
        }
        self.reconcile(backend, record)
    }

    // Called by library SDK output-status/recovery APIs, absent from the REST binary.
    #[allow(dead_code)]
    pub(crate) fn get<B: OutputBackend>(
        &self,
        backend: &B,
        id: &str,
    ) -> Result<Option<OutputOperation>, APIError> {
        let Some(mut record) = self.read(id)? else {
            return Ok(None);
        };
        self.verify_plan(backend, &record)?;
        self.reconcile(backend, &mut record)?;
        self.recover_submission(backend, &mut record)?;
        Ok(Some(record.operation))
    }

    // Called by library SDK output-status/recovery APIs, absent from the REST binary.
    #[allow(dead_code)]
    pub(crate) fn list<B: OutputBackend>(
        &self,
        backend: &B,
    ) -> Result<Vec<OutputOperation>, APIError> {
        let ids = self.store.list("", "").map_err(|_| journal_error())?;
        if ids.len() > MAX_OPERATIONS {
            return Err(journal_error());
        }
        ids.into_iter()
            .map(|id| self.get(backend, &id)?.ok_or_else(journal_error))
            .collect()
    }

    fn release_prebroadcast<B: OutputBackend>(
        &self,
        backend: &B,
        record: &mut Record,
        status: &str,
    ) -> Result<(), APIError> {
        if record.restored_ambiguous {
            return Err(APIError::InvalidRequest("The restored RGB output plan may have been submitted since its snapshot; cancellation cannot be proven safe".into()));
        }
        if let Some(txid) = &record.operation.txid {
            if backend
                .pending()?
                .iter()
                .any(|pending| &pending.txid == txid)
            {
                backend.cancel(txid.clone())?;
            }
        }
        record.operation.status = status.into();
        record.unsigned_psbt = None;
        record.signed_psbt = None;
        self.write(record)
    }

    // Called by the library SDK's reviewed prebroadcast cancellation API, not standalone REST.
    #[allow(dead_code)]
    pub(crate) fn cancel<B: OutputBackend>(
        &self,
        backend: &B,
        id: &str,
    ) -> Result<OutputOperation, APIError> {
        let mut record = self
            .read(id)?
            .ok_or_else(|| APIError::InvalidRequest("RGB output operation was not found".into()))?;
        self.verify_plan(backend, &record)?;
        self.recover_prepare(backend, &mut record)?;
        match record.operation.status.as_str() {
            "prepared" => self.release_prebroadcast(backend, &mut record, "cancelled")?,
            "cancelled" | "failed" => {}
            _ => {
                return Err(APIError::InvalidRequest(
                    "RGB output transaction may have been broadcast; cancellation is unsafe".into(),
                ))
            }
        }
        Ok(record.operation)
    }

    pub(crate) fn commit<B: OutputBackend>(
        &self,
        backend: &B,
        id: &str,
    ) -> Result<OutputOperation, APIError> {
        let mut record = self
            .read(id)?
            .ok_or_else(|| APIError::InvalidRequest("RGB output operation was not found".into()))?;
        self.verify_plan(backend, &record)?;
        self.reconcile(backend, &mut record)?;
        match record.operation.status.as_str() {
            "broadcast" | "confirmed" => return Ok(record.operation),
            "prepared" => {
                let txid = record.operation.txid.as_ref().ok_or_else(journal_error)?;
                if !backend
                    .pending()?
                    .iter()
                    .any(|pending| pending.operation_type == "CreateUtxos" && &pending.txid == txid)
                {
                    self.release_prebroadcast(backend, &mut record, "failed")?;
                    return Err(APIError::InvalidRequest("The reviewed RGB output reservation is no longer held; prepare a new operation".into()));
                }
                let unsigned = record.unsigned_psbt.clone().ok_or_else(journal_error)?;
                let signed = match backend.sign(unsigned.clone()) {
                    Ok(signed) => signed,
                    Err(error) => {
                        self.release_prebroadcast(backend, &mut record, "failed")?;
                        return Err(error);
                    }
                };
                let valid = Psbt::from_str(&signed).ok().filter(|signed| {
                    Psbt::from_str(&unsigned)
                        .is_ok_and(|unsigned| signed.unsigned_tx == unsigned.unsigned_tx)
                        && signed.inputs.iter().all(|input| {
                            input.final_script_sig.is_some() || input.final_script_witness.is_some()
                        })
                });
                if valid.is_none() {
                    self.release_prebroadcast(backend, &mut record, "failed")?;
                    return Err(APIError::InvalidRequest(
                        "RGB output signer did not return the complete reviewed transaction".into(),
                    ));
                }
                record.signed_psbt = Some(signed);
                record.operation.status = "submitting".into();
                self.write(&mut record)?;
            }
            "submitting" => {}
            _ => {
                return Err(APIError::InvalidRequest(
                    "RGB output operation cannot be committed; prepare a new operation".into(),
                ))
            }
        }
        let signed = record.signed_psbt.clone().ok_or_else(journal_error)?;
        match backend.finish(signed) {
            Ok(actual_count) => {
                if actual_count != record.operation.num_outputs {
                    return Err(journal_error());
                }
                record.operation.status = "broadcast".into();
                self.write(&mut record)?;
                self.reconcile(backend, &mut record)?;
            }
            Err(_) => {
                // A transport error or a failed post-broadcast DB commit cannot prove absence.
                // Preserve the signed transaction and exact input ownership for safe retry.
                self.reconcile(backend, &mut record)?;
            }
        }
        Ok(record.operation)
    }
}

#[cfg(test)]
mod tests;
