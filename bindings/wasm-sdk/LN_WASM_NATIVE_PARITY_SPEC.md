# LN WASM Native Parity Spec

## Purpose

This document freezes the implementation scope and acceptance criteria for
fully native LN behavior in browser WASM (Mutiny-style runtime model), while
keeping existing RLN SDK API contracts stable.

This is the source of truth for:

- what "fully ported LN behavior" means,
- which infra is still external,
- and what tests must pass before declaring parity complete.

## Scope Boundary

### In scope (must be native in WASM)

1. LN runtime state machine authority is local in WASM (no RLN runtime bridge
   service as LN decision-maker).
2. Peer/session/channel/payment state transitions are event-driven from local
   LDK runtime components.
3. Runtime persistence/recovery works across page reload and browser restart.
4. Public wasm node/sdk APIs return deterministic contracts equivalent to
   existing SDK semantics.

### Out of scope (external infra remains required)

1. Relay/proxy service for browser websocket transport bridging.
2. Bitcoin chain source/indexer (Esplora).
3. RGB transport endpoint.
4. Funding/signing providers (wallet signer integration for on-chain tx flows).

## Definition of Done (Parity)

Parity is complete only when all are true:

1. LN payment and channel lifecycle transitions are driven by local runtime
   events, not manual status APIs.
2. Runtime can recover channel/payment state after reload without corruption.
3. `open_channel`, `close_channel`, `create_ln_invoice`, `send_payment`,
   `keysend`, `invoice_status`, `list_payments`, `get_payment` match frozen
   contracts for:
   - input validation (IV),
   - state transitions (ST),
   - output shape (OS),
   - error contracts (EC).
4. Virtual channel logic is bound to real runtime state and safety checks.
5. Browser integration tests (not only compile checks) are green in CI.

## Behavioral Contract Freeze

### Runtime authority

1. Local runtime owns truth for peers/channels/payments.
2. API read methods (`node_info`, `network_info`, list/get APIs) read from
   runtime state, not from local ad-hoc shadow state.

### Payments

1. `pending -> claimable/claiming/succeeded/failed/expired/cancelled` transitions
   must obey strict transition rules.
2. Terminal state regressions are rejected with deterministic errors.
3. Swap/runtime observers and RGB-LN transfer observers receive the same final
   status.

### Channels

1. `open -> usable -> closing/closed` semantics remain deterministic.
2. Reconnect must not duplicate live channels or peers.
3. Channel close safety checks must be consistent across restart/recovery.

### Recovery

1. Snapshot schema is versioned.
2. Checkpoint protocol is crash-safe (pending+committed recovery semantics).
3. Recreated node handle for same runtime key restores state without manual
   repair.

## Acceptance Test Matrix (Must be Green)

### A. Runtime lifecycle and recovery

1. Cold start -> start -> stop -> restart roundtrip.
2. Reload restore for peers/channels/payments.
3. Crash-safe checkpoint fallback (pending/committed restore behavior).

### B. Transport and peer behavior

1. Peer connect/disconnect/reconnect transitions.
2. Backoff/retry path on websocket read errors.
3. Relay auth token + node id propagation contract.

### C. Payment lifecycle

1. Invoice creation/decode/status contracts.
2. Keysend/send_payment success and failure route behaviors.
3. Terminal transition rejection contract.
4. HODL create/claim/cancel contract.

### D. Channel lifecycle

1. Open channel validation contracts.
2. Usable/unusable transitions from runtime events.
3. Close channel options, not-found, and virtual close safety contracts.

### E. RGB + LN integration

1. RGB-annotated LN payment registration in runtime ledger.
2. RGB-LN transfer status sync on payment updates.
3. RGB-LN transfer persistence across node recreation.

### F. Chain sync

1. Start/status/stop/tick contracts.
2. Indexer URL validation + poll clamp contracts.
3. Rebroadcast enqueue validation and queue persistence.

### G. API parity surfaces

1. Node direct API.
2. SDK facade forwarding parity.
3. SDK node-handle forwarding parity.

## CI Gate Requirements

1. `cargo check --target wasm32-unknown-unknown` (wasm-sdk crate).
2. `cargo test --target wasm32-unknown-unknown --no-run` (wasm-sdk crate).
3. Browser wasm test execution job (wasm-bindgen test path) for contract tests.

The crate targets `wasm32` only (the `real-wasm-rgb` backend feature is on by default), so all
compile checks use the wasm32 target.

Parity cannot be declared complete until all three gate groups are green on CI.

## Tracking Convention

For every parity work item, include:

1. module/file ownership,
2. contract impact (IV/ST/OS/EC),
3. test id(s) added/updated,
4. CI gate affected.

