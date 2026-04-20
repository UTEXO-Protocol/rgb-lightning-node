# WASM Interop Example

This is a browser-side interop example similar in spirit to the Python SDK example:

1. Initialize wasm package
2. Generate keys (`rgbGenerateKeysJson` + `rgbGenerateKeysValue`)
3. Restore keys from mnemonic (`rgbRestoreKeysJson`)
4. Verify parity checks in JS (xpub consistency)
5. Demonstrate stable error contract (`RlnWasmInvoice("")`)
6. Optionally test `checkProxyUrl`
7. Install peer-manager hooks and run bridge bootstrap path (`RlnWasmRustPeerManagerBridge`)
8. Demonstrate callback-driven payment status transitions via `RlnWasmNode`:
   - explicit JSON status payload
   - event alias JSON payload (`PaymentFailed`)
   - text alias payload (`payment_expired:<payment_hash>`)

## Python-equivalent virtual channel flow example

`virtual_channels_flow.html` + `manual_js_virtual_channels_sdk_flow.js` implements
the same high-level sequence as
`src/uniffi_api/examples/python-interop/manual_py_virtual_channels_sdk.py`:

1. SDK init + unlock
2. Create node A / node B runtime handles
3. Connect peers
4. Open `trusted_no_broadcast` virtual channel (`openChannelValueWithOptions`)
5. Wait for channel usable
6. Keysend A -> B and wait final status
7. Keysend B -> A ("drain") and wait final status
8. Close channel with retry/fallback (`nodeA` then `nodeB`) and verify close semantics on both nodes
9. Verify removal on node A

Differences vs native Python flow:

1. It runs against the wasm runtime model (`ldk_bridge`) in browser memory/storage.
2. Payment finalization is driven through wasm status update APIs (event-driven parity path),
   not native daemon callbacks.
3. Trusted virtual close is treated as host-authoritative parity:
   close completion accepts node A done state while node B may transiently lag.

## Prerequisites

From repo root:

```sh
cd bindings/wasm-sdk
```

Build wasm package (`pkg/`) for browser usage:

```sh
wasm-pack build --target web --dev
```

If `wasm-pack` is not installed:

```sh
cargo install wasm-pack
```

## Run

From repo root:

```sh
python3 -m http.server 8080
```

Open:

```text
http://localhost:8080/bindings/wasm-sdk/examples/wasm-interop/
```

Click `Run Example`.

For the Python-equivalent flow page, open:

```text
http://localhost:8080/bindings/wasm-sdk/examples/wasm-interop/virtual_channels_flow.html
```

Click `Run Flow`.

For RGB asset issuance + transfer flow, open:

```text
http://localhost:8080/bindings/wasm-sdk/examples/wasm-interop/rgb_asset_transfer_flow.html
```

Click `Run RGB Transfer Flow`.

## Notes

1. This example is browser-only (`--target web`).
2. It focuses on deterministic interop surface checks, not full native RLN node runtime behavior.
3. Peer-session start may fail in normal local runs if proxy/peer is not reachable; the example logs this as non-fatal.
4. `rgb_asset_transfer_flow.html` requires a PSBT signer callback in the page context:

```js
window.signPsbt = async (unsignedPsbt) => {
  // return signed PSBT string
  return unsignedPsbt;
};
```

In real usage, replace this with your wallet/hardware signer integration.
5. The RGB transfer page uses fixed constants in JS for:
   - `Indexer URL`: `http://127.0.0.1:3002`
   - `RGB transport endpoint`: `rpc://127.0.0.1:3000/json-rpc`
   - sender/receiver RLN instances are initialized at runtime, each with generated keys and a generated wallet bootstrap
