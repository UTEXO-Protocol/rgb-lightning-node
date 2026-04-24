# RLN WASM SDK

Browser-facing WASM SDK for `rgb-lightning-node`.

This crate provides a JS/WASM API for SDK-level operations.

## Scope

- Target: browser and Node.js consumers via `wasm-bindgen` output.
- Focus: SDK parity where feasible in WASM.
- Current intentional gap: `issue_asset_uda` remains unsupported for now.

For endpoint-level status, see [SDK_WASM_ENDPOINT_MATRIX.md](SDK_WASM_ENDPOINT_MATRIX.md).

## Build

From repository root:

```sh
cargo check --manifest-path bindings/wasm-sdk/Cargo.toml
cargo test --manifest-path bindings/wasm-sdk/Cargo.toml --no-run
cargo check --manifest-path bindings/wasm-sdk/Cargo.toml --target wasm32-unknown-unknown
```

Generate package artifacts:

```sh
cd bindings/wasm-sdk
wasm-pack build --target web --out-dir pkg
```

## Runtime model

- Public API: `wasm_native_ldk` runtime only.
- Boundary: no implicit dependency on a local runtime service endpoint
  (for example `ws://127.0.0.1:3001`) in production paths.

The SDK surface is object-handle based (same direction as UniFFI SDK instance
model) and avoids REST coupling.

## Examples

- WASM example flow: `bindings/wasm-sdk/examples/`
- Python SDK reference flow: `src/uniffi_api/examples/python-interop/`

## CI

WASM checks are wired into CI in `.github/workflows/test.yaml`:

- `wasm-sdk` (native host checks for crate)
- `wasm-sdk-wasm32` (target compatibility check)
- `wasm-sdk-browser` (headless browser wasm-bindgen test execution via `wasm-pack test`)

## Additional docs

- [ERROR_CONTRACT.md](ERROR_CONTRACT.md)
- [SDK_WASM_ENDPOINT_MATRIX.md](SDK_WASM_ENDPOINT_MATRIX.md)
- [SDK_WASM_UML_COMPARE.md](SDK_WASM_UML_COMPARE.md)
- [SDK_WASM_FLOWS_PORTING.md](SDK_WASM_FLOWS_PORTING.md)
- [LN_WASM_NATIVE_PARITY_SPEC.md](LN_WASM_NATIVE_PARITY_SPEC.md)
- [RGB_WASM_PROXY_TRANSPORT_SPEC.md](RGB_WASM_PROXY_TRANSPORT_SPEC.md)

## Related

- UniFFI SDK docs: `src/uniffi_api/README.md`
- Core project docs: repository `README.md`
