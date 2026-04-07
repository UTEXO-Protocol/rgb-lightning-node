# RLN Full SDK WASM Port Plan

This plan tracks the migration from a wasm boundary scaffold to a runtime-backed wasm SDK.

## Scope

Goal: provide a wasm SDK that covers RLN SDK functionality that is technically feasible in a browser/wasm runtime, and clearly marks unsupported LDK transport/runtime operations.

Non-goal: run full native RLN node services (raw TCP peer manager, native FS, bitcoind RPC daemon model) directly in browser wasm.

## Constraints

1. Browser wasm cannot use native sockets / filesystem APIs used by native RLN runtime.
2. RLN native path depends on LDK + bitcoind integrations that are not directly wasm-portable.
3. `rgb-lib-wasm` is the viable RGB wallet/runtime backbone for browser target.

## Architecture Target

1. Keep native RLN SDK (`src/sdk/mod.rs`) as authoritative native engine.
2. Build wasm SDK as a separate crate (`bindings/wasm-sdk`) backed by `rgb-lib-wasm`.
3. Introduce explicit capability contract:
   - `supported` on wasm
   - `unsupported` on wasm (with stable error code/message)
4. Keep API model parity where possible (JSON shapes and naming).

## Endpoint Matrix

Status legend:
- `done`: implemented and runtime-backed in wasm SDK.
- `planned`: in scope for wasm SDK and mapped to `rgb-lib-wasm`.
- `unsupported`: requires native LDK/P2P runtime not feasible in browser wasm.

### Done

1. RGB key generation/restore.
2. Wallet create/open with IndexedDB snapshot restore.
3. Wallet data read.
4. Get address.
5. BTC balance.
6. List transactions.
7. List assets.
8. Asset metadata / asset balance.
9. List transfers / list unspents.
10. `blind_receive` / `witness_receive`.
11. Online lifecycle wrappers (`go_online`, `sync`, fee estimation).
12. Send RGB / send BTC / refresh / fail / create-utxos / drain / inflate flows.
13. Vanilla unspents online listing.
14. RGB invoice parsing wrappers.
15. Proxy URL check wrapper.
16. Backup / restore / VSS lifecycle wrappers.

### Planned (next)

1. RGB invoice create primitives (if needed beyond parser wrapper).
2. LDK-over-websocket runtime port (see `MUTINY_LDK_REFERENCE.md`).
3. Channel lifecycle and LN payment flow on top of websocket peer runtime.

### Unsupported (browser wasm)

1. Native LDK peer/channel lifecycle (`connect_peer`, `open_channel`, `close_channel`).
2. Native LN payments/keysend over direct peer graph.
3. Native bitcoind RPC transport assumptions.

Note: These are unsupported in the current implementation. The LDK-over-wasm phase below is the path to reduce this unsupported set.

## Execution Phases

## Phase 0: Build + Reproducibility

1. Pin RGB dependency graph for wasm crate.
2. Ensure `cargo check --target wasm32-unknown-unknown` passes in CI-equivalent environment.
3. Keep native RLN checks green.

Status: `done`.

## Phase 1: Wallet Read Surface

1. Add runtime-backed wallet wrapper in wasm SDK.
2. Implement read endpoints first (address/balance/transactions/assets).
3. Preserve dual output style (`Value` and `Json`).

Status: `done`.

## Phase 2: RGB Read/Receive Surface

1. Implement asset metadata, asset balance, transfers, unspents.
2. Implement `blind_receive` / `witness_receive`.
3. Add schema conversion and error normalization.

Status: `done`.

## Phase 3: Online + Send Flows

1. Add `go_online` and `sync` wrappers.
2. Add send/create-utxo/drain/inflate flow wrappers.
3. Define deterministic client-side orchestration semantics.

Status: `done`.

## Phase 4: Backup + Recovery

1. Backup/restore API wrappers.
2. VSS backup lifecycle wrappers.
3. Recovery and snapshot consistency checks.

Status: `done`.

## Phase 5: Contract and Test Hardening

1. Add wasm contract tests for JSON schemas and error mapping.
2. Add fixture-based parity checks against native SDK where feasible.
3. Document unsupported endpoints with stable machine-readable errors.

Status: `done`.

## Phase 6: LDK Runtime Port (Mutiny-Inspired)

1. Introduce runtime abstraction in RLN core for:
   - peer transport
   - persistence backend
   - async runtime glue
2. Implement wasm websocket proxy transport and `SocketDescriptor` adapter.
3. Implement wasm persistence backend for required LDK state.
4. Port LDK-dependent SDK methods on wasm:
   - `connect_peer` / `disconnect_peer`
   - `list_peers` / `list_channels` / `node_info`
   - channel open/close
   - invoice/LN payment flow
5. Add browser integration tests for peer/channel/payment roundtrip.

Status: `in_progress` (websocket transport + peer session + node peer/channel/payment lifecycle scaffold implemented; explicit payment status transition APIs and auto hook adapter added, including targeted payment updates from structured read-event payloads and example coverage; full LDK-driven flow still pending).

## Acceptance Criteria

1. Wasm SDK compiles with pinned reproducible dependencies.
2. Supported endpoint set has automated wasm tests.
3. Unsupported endpoint set returns explicit stable error contracts.
4. Public docs clearly separate wasm-supported vs native-only operations.
