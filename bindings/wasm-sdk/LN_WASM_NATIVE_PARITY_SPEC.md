# LN WASM Native Parity

## Purpose

This document describes how LN behavior runs natively in browser WASM
(Mutiny-style runtime model) while keeping the existing RLN SDK API contracts
stable. It covers what "native LN behavior" means here, which infra stays
external, and which behaviors the test suite exercises.

## Scope Boundary

### Native in WASM

1. The LN runtime state-machine authority is local in WASM (no RLN runtime
   bridge service acts as the LN decision-maker).
2. Peer/session/channel/payment state transitions are event-driven from local
   LDK runtime components.
3. Runtime persistence/recovery works across page reload and browser restart.
4. Public wasm node/sdk APIs return deterministic contracts equivalent to the
   existing SDK semantics.

### External infra required

1. Relay/proxy service for browser websocket transport bridging.
2. Bitcoin chain source/indexer (Esplora).
3. RGB transport endpoint.
4. Funding/signing providers (wallet signer integration for on-chain tx flows).

## Parity Properties

1. LN payment and channel lifecycle transitions are driven by local runtime
   events, not manual status APIs.
2. The runtime recovers channel/payment state after reload without corruption.
3. `open_channel`, `close_channel`, `create_ln_invoice`, `send_payment`,
   `keysend`, `invoice_status`, `list_payments`, and `get_payment` honor stable
   contracts for input validation (IV), state transitions (ST), output shape
   (OS), and error contracts (EC).
4. Virtual channel logic is bound to real runtime state and safety checks.

## Behavioral Contracts

### Runtime authority

1. The local runtime owns truth for peers/channels/payments.
2. API read methods (`node_info`, `network_info`, list/get APIs) read from
   runtime state, not from ad-hoc shadow state.

### Payments

1. `pending -> claimable/claiming/succeeded/failed/expired/cancelled`
   transitions obey strict transition rules.
2. Terminal state regressions are rejected with deterministic errors.
3. Swap/runtime observers and RGB-LN transfer observers receive the same final
   status.

### Channels

1. `open -> usable -> closing/closed` semantics are deterministic.
2. Reconnect does not duplicate live channels or peers.
3. Channel close safety checks are consistent across restart/recovery.

### Recovery

1. The snapshot schema is versioned.
2. The checkpoint protocol is crash-safe (pending+committed recovery semantics).
3. A recreated node handle for the same runtime key restores state without
   manual repair.

## Test Coverage

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

## Build and test commands

The crate targets `wasm32` only (the `real-wasm-rgb` backend feature is on by
default), so all compile checks use the wasm32 target:

1. `cargo check --target wasm32-unknown-unknown` (wasm-sdk crate) — run in CI.
2. `cargo test --target wasm32-unknown-unknown --no-run` (wasm-sdk crate).
3. Browser wasm test execution (`scripts/run-browser-tests.sh`, wasm-bindgen
   test path) for the contract tests — run locally.
