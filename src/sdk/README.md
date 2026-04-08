# SDK Module Notes

This document describes the current `src/sdk` structure after wasm-port cleanup.

## Active modules

- `src/sdk/mod.rs`
  - Native SDK implementation used on non-wasm targets.
  - Mirrors core route behavior for SDK consumers.
- `src/sdk/wasm.rs`
  - Wasm boundary module used when `target_arch = "wasm32"`.
  - Defines wasm SDK state/types and async wasm-facing API surface.
- `src/sdk/wasm_bindings.rs`
  - `wasm-bindgen` bridge for JS-facing calls (`*Value` / `*Json`).

## Target wiring

`src/lib.rs` selects SDK module by target:

- non-wasm: `mod sdk;` -> `src/sdk/mod.rs`
- wasm: `#[path = "sdk/wasm.rs"] pub mod sdk;`

This means native and wasm SDK entrypoints are separated at compile time.

## Removed staged files

The previous staged duplication modules were removed because they were not
connected to the active module graph:

- `src/sdk/args.rs`
- `src/sdk/bitcoind.rs`
- `src/sdk/core_types.rs`
- `src/sdk/disk.rs`
- `src/sdk/engine.rs`
- `src/sdk/error.rs`
- `src/sdk/ldk.rs`
- `src/sdk/rgb.rs`
- `src/sdk/swap.rs`
- `src/sdk/utils.rs`

## Validation commands

Run from repository root:

```bash
cargo check
cargo check --target wasm32-unknown-unknown --lib
cargo check --manifest-path bindings/wasm-sdk/Cargo.toml
cargo check --manifest-path bindings/wasm-sdk/Cargo.toml --target wasm32-unknown-unknown
```
