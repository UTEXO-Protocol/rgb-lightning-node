# SDK WASM Notes

This document tracks the wasm-facing SDK boundary and the refactor work that moved wasm APIs into `src/sdk`.

## What Changed

1. WASM bindings were moved to SDK scope:
   - from `src/wasm_bindings.rs`
   - to `src/sdk/wasm_bindings.rs`
2. WASM SDK boundary lives in `src/sdk/wasm.rs`.
3. On wasm targets, library export now points `pub mod sdk;` to `src/sdk/wasm.rs` through `src/lib.rs`.
4. UniFFI/native runtime modules are gated out for wasm builds.

## Major SDK Refactors (Engine / Disk / Core)

The wasm work was preceded by a larger SDK restructuring so runtime-dependent logic and SDK-facing contracts are separated.

1. Engine abstraction introduced (`src/sdk/engine.rs`)
   - Added a dedicated `SdkEngine` trait containing SDK operations (init/unlock, node/network info, channel/payment/rgb/swap APIs, and mutating operations).
   - This decouples API surface from concrete runtime wiring and enables different engine implementations per target/runtime.
   - Outcome: wasm can keep a boundary engine/state model while native keeps full runtime behavior.
2. Disk/storage logic moved into SDK namespace (`src/sdk/disk.rs`)
   - Filesystem persistence and recovery helpers were moved under SDK scope:
     - channel-peer map persistence
     - LDK graph/scorer/payment storage reads
     - swap/channel id persistence helpers
   - Outcome: stateful runtime storage concerns are isolated from wasm boundary and can be replaced with wasm-safe adapters later.
3. Core shared types/constants extracted for SDK (`src/sdk/core_types.rs`)
   - Shared protocol and runtime constants/status enums (`FEE_RATE`, `HTLCStatus`, `SwapStatus`, `UnlockRequest`, etc.) are now local to SDK internals.
   - Outcome: wasm and native paths can share API contracts while diverging in execution/runtime backend.
4. Native runtime modules duplicated/scoped at SDK level
   - Native runtime-heavy modules now exist in `src/sdk/*` (`args`, `bitcoind`, `error`, `ldk`, `rgb`, `swap`, `utils`) instead of coupling directly to root module layout.
   - `src/sdk/mod.rs` gates most of these under `feature = "native-sdk-engine"`.
   - Outcome: SDK evolution can happen in one module tree, and wasm-facing code can be ported incrementally without pulling native internals.
5. Cargo feature and target separation
   - `native-sdk-engine` feature marks native engine path.
   - `cfg(target_arch = "wasm32")` compiles only wasm-safe dependencies and modules.
   - Outcome: clear compile-time boundary between native full node runtime and wasm SDK boundary.

## Module Mapping (Root -> SDK)

| Previous root module | SDK-level module | Status |
| --- | --- | --- |
| `src/args.rs` | `src/sdk/args.rs` | extracted for native-sdk path |
| `src/bitcoind.rs` | `src/sdk/bitcoind.rs` | extracted for native-sdk path |
| `src/core_types.rs` | `src/sdk/core_types.rs` | extracted/shared contract constants |
| `src/disk.rs` | `src/sdk/disk.rs` | extracted for SDK runtime persistence |
| `src/error.rs` | `src/sdk/error.rs` | extracted for SDK runtime errors |
| `src/ldk.rs` | `src/sdk/ldk.rs` | extracted for SDK native runtime |
| `src/rgb.rs` | `src/sdk/rgb.rs` | extracted for SDK native runtime |
| `src/swap.rs` | `src/sdk/swap.rs` | extracted for SDK native runtime |
| `src/utils.rs` | `src/sdk/utils.rs` | extracted/shared SDK helpers |
| `src/wasm_bindings.rs` | `src/sdk/wasm_bindings.rs` | moved to SDK wasm boundary |
| n/a (new) | `src/sdk/engine.rs` | new abstraction for runtime backends |
| n/a (new) | `src/sdk/wasm.rs` | new wasm boundary module |

## Module Layout

- `src/lib.rs`
  - native-only modules are behind `#[cfg(not(target_arch = "wasm32"))]`
  - wasm SDK module is exposed with:
    - `#[cfg(target_arch = "wasm32")]`
    - `#[path = "sdk/wasm.rs"]`
    - `pub mod sdk;`
- `src/sdk/wasm.rs`
  - wasm-safe SDK data types
  - state container (`WasmSdkState`, `WasmSdkStateConfig`)
  - async wasm API surface (`init`, `network_info`, `list_channels`, `list_peers`, `node_info`, `estimate_fee`, `list_transactions`, `runtime_capabilities`, `get_channel_id`, `address`, `healthcheck`, `sdk_info`)
  - exports wasm JS bridge module:
    - `pub mod wasm_bindings;`
- `src/sdk/wasm_bindings.rs`
  - `wasm-bindgen` bridge for JS consumers
  - `sdk_init(...)` constructor
  - JSON-returning methods (`*Json`)
  - typed `JsValue` methods (`*Value`) using `serde-wasm-bindgen`

## Runtime Model

Current wasm SDK layer is an explicit boundary module:

- It is read-only and deterministic.
- It does not start native LDK/RGB background services on wasm.
- It provides stable API/shape for frontend integration while runtime-backed operations are ported incrementally.

In other words, the major refactor split the project into:

- SDK contracts + engine boundary (`src/sdk/engine.rs` and SDK types)
- native runtime implementation modules (`src/sdk/ldk.rs`, `src/sdk/rgb.rs`, `src/sdk/disk.rs`, etc.)
- wasm boundary implementation (`src/sdk/wasm.rs` + `src/sdk/wasm_bindings.rs`)

`WasmSdkStateConfig` lets callers inject network, height, fee-rate hint, channels, and peers so integration tests and UI demos can run against predictable state.

## Validation Commands

Run from repository root:

```bash
cargo check --target wasm32-unknown-unknown --lib
cargo test --target wasm32-unknown-unknown --lib --no-run
cargo test --target wasm32-unknown-unknown --tests --no-run
cargo check --features "uniffi,native-sdk-engine" --lib
```

## Dependency and Build Updates

- `Cargo.toml` includes wasm target support deps:
  - `wasm-bindgen`
  - `serde-wasm-bindgen`
  - `getrandom` with `js` feature
- Explicit `[[bin]]` test handling is configured so wasm `--tests --no-run` is not blocked by native-only bin harness assumptions.

## Next Porting Steps

1. Replace placeholder wasm engine behavior with real async runtime adapters where browser-compatible.
2. Separate read-only and mutating API groups in wasm exports.
3. Add wasm integration tests from JS side (e.g. Node + browser harness) for API-level contract checks.
