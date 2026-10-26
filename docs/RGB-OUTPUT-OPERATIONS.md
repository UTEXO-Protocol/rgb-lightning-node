# Durable RGB output operations

Use the reviewed operation API for output setup instead of inferring a transaction ID from a
before/after history difference. The legacy `createutxos` API also uses this state machine, but
its void response cannot provide the caller with the durable operation ID for explicit recovery.

## Contract

Persist a random 32–64 character **lowercase hexadecimal** `operation_id` in the application's
intent journal before calling `preparecreateutxos`. The ID is permanently bound to `up_to`,
resolved `num`/`size`, `fee_rate` and `skip_sync`; reusing it with different parameters fails.

- `preparecreateutxos(SdkPrepareCreateUtxosRequest)` reserves native inputs and returns the
  exact unsigned transaction identity, actual colored output count and actual PSBT fee. RGB can
  reduce the requested output count to fit available funds, so consent must use this result.
- `commitpreparedcreateutxos(operation_id)` signs only that reviewed PSBT. The exact signed
  PSBT and `submitting` state are durably written before native broadcast can run.
- `cancelcreateutxosplan(operation_id)` releases only a provably prebroadcast reservation.
- `getcreateutxosoperation(operation_id)` returns the durable result or null. It can finish the
  bookkeeping of a previously approved `submitting` transaction by retrying the **same signed
  PSBT**. It never selects inputs or creates a replacement transaction.
- `listcreateutxosoperations()` lists live operations. Confirmed, failed and cancelled records
  move to a completed namespace; exact-ID lookup remains available permanently.
- `listpendingvanillatransactions()` exposes native reservation transaction IDs and types,
  including older reservations that lack an operation journal.

The C FFI names are the corresponding `rln_prepare_create_utxos`,
`rln_commit_prepared_create_utxos`, `rln_cancel_create_utxos_plan`,
`rln_get_create_utxos_operation`, `rln_list_create_utxos_operations` and
`rln_list_pending_vanilla_transactions`. Mutating/get-by-ID functions accept JSON requests;
response JSON never includes the unsigned or signed PSBT.

Every operation response contains `operation_id`, nullable `txid`, `status`, `can_cancel`,
`can_commit`, `inputs` (`txid` and `vout`), `num_outputs`, `size_sat`, `fee_sat` and `fee_rate`.
Clients must respect the permission fields rather than infer cancellation safety from status.

| Status | Meaning | Permissions |
| --- | --- | --- |
| `preparing` | An interrupted or uncertain native preparation; no public PSBT exists yet | Neither; native reconciles proven ownership itself |
| `prepared` | Exact unsigned native plan is available for review | Commit; cancel only when `can_cancel` is true |
| `submitting` | The approved exact transaction may have been broadcast | Retry the same commit; cancellation forbidden |
| `broadcast` | Exact transaction observed without confirmation; native bookkeeping complete | Neither action is needed |
| `confirmed` | Fresh exact transaction observation reports confirmation | Neither action is needed |
| `cancelled` / `failed` | This operation is terminal and cannot be reused | Neither; create a new operation ID |

## Crash, restore and input safety

The journal is inside the RGB wallet directory at `rln_output_operations_v1`. LDK's filesystem
store uses atomic replacement and file/directory fsync. Record reads are bounded to 2 MiB,
validate the version, parameters, status, permissions, PSBT identity, exact input list, actual
fee, completed signatures and native colored descriptor count. Live operations are bounded to
64. Terminal tombstones do not consume that capacity or permit ID reuse. Moving a record into
or out of the completed namespace writes a higher durable revision before removing the old
copy, allowing recovery from a crash between those writes.

Preparation intent is durable before native `_begin`. Under the operation mutex, a unique new
CreateUtxos reservation relative to that intent's baseline can be released after an interrupted
preparation: that path has never reached broadcast. An ambiguous set is frozen and retained.
Later preparations cannot accidentally be classified as its reservation. The native library
excludes all reserved vanilla inputs; independent funds can still support another reviewed
operation. Older unowned CreateUtxos reservations remain visible through the native pending
reservation API and are never arbitrarily aborted. Their inputs stay excluded while separate
confirmed funding remains usable. Network absence or an elapsed timer cannot prove they were
not broadcast.

A signer failure releases only the current provably prebroadcast reservation. Before signing,
commit verifies that this exact reservation still exists. This prevents an abort-success /
journal-write-failure crash from turning a stale review into a new unowned spend.

The RGB snapshot's content digest includes the journal, alongside native reservations and BDK
state. Both VSS and encrypted file restore mark older unsigned plans uncertain: a snapshot can
precede a later broadcast, so it cannot grant cancellation authority. A restored exact unsigned
PSBT can be reapproved and signed; no alternate transaction is constructed. A restored
preparation without a PSBT remains retained if its outcome cannot be proven.

A successful status read requires a fresh observation for confirmation. Mempool/reorg loss or
an unavailable observation revokes `confirmed`; it never releases the original input protection
or constructs a replacement. The maintained native adapter additionally coordinates output
operations with its VSS admission/snapshot barrier; qualification of that overlay is separate
from this focused SDK implementation.

## Qualification

Host Rust state-machine tests cover durable/idempotent review, wrong ID/parameters, actual fee
and count, signing failure/mutation/incomplete signatures, cancellation, missing reservation,
independent input exclusion, lost responses, interrupted preparation, ambiguous/restored state,
confirmation/reorg/outage, corruption, terminal tombstones and exact retry recovery.

The opt-in funded test uses a real watch-only RGB wallet and a separate fixture signer, funded
on the isolated Bitcoin regtest fixture. A SQLite trigger fails a native colored-TXO write after
real broadcast and BDK flush; the test proves the old reservation remains, the original exact
PSBT completes bookkeeping, no second setup transaction is created, and fee/output count match
native history after confirmation. It also verifies the real RGB snapshot includes the journal.
A second funded case keeps an older unowned reservation intact, confirms a separate 50,000-sat
funding output, and proves native preparation uses only that funding and truthfully reduces the
requested four 20,000-sat outputs to two. Cancelling the new review leaves the old reservation
untouched. This is host/native evidence, not Android handset acceptance.

```sh
export COMPOSE_FILE="$PWD/tests/fixtures/rgb-output-operations/compose.yaml"
export COMPOSE_PROJECT_NAME=iris_rgb_output_20261026
docker compose up -d
cargo +1.94.0 test --lib --locked --features vss,uniffi \
  rgb_output_operations::tests -- --include-ignored --test-threads=1
```

The fixture pins its images and uses separate relative `runtime` directories, project name and
loopback ports. It does not reuse the shared stack's wallet, volumes or data. The wallet uses
Esplora because the pinned fixture's Electrum implementation does not support the verbose
transaction calls required by this RGB library; Electrum is used only to await indexing height.
The public tests do not run against a hosted bridge or public network.
