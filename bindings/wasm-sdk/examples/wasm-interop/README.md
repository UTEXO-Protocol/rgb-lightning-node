# WASM Interop Example

This is a browser-side interop example similar in spirit to the Python SDK example:

1. Initialize wasm package
2. Preload persistent runtime state (`sdk.preloadPersistentRuntimeState()`) to recover
   runtime snapshots after reload before init/unlock calls
3. Generate keys (`rgbGenerateKeysJson` + `rgbGenerateKeysValue`)
4. Restore keys from mnemonic (`rgbRestoreKeysJson`)
5. Verify parity checks in JS (xpub consistency)
6. Demonstrate stable error contract (`RlnWasmInvoice("")`)
7. Optionally test `checkProxyUrl`
8. Install peer-manager hooks and run bridge bootstrap path (`RlnWasmRustPeerManagerBridge`)
9. Demonstrate runtime-aware payment status handling:
   - `wasm_native_ldk`: manual mutation is skipped; statuses are runtime-event-driven only

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

1. It runs against the wasm runtime model (`wasm_native_ldk`) in browser memory/storage.
2. Payment finalization is driven through wasm status update APIs (event-driven parity path),
   not native daemon callbacks.
3. Trusted virtual close is treated as host-authoritative parity:
   close completion accepts node A done state while node B may transiently lag.
4. Browser examples do not assume a default local runtime service URL; provide proxy URL explicitly when needed.

## Prerequisites

From repo root:

```sh
cd bindings/wasm-sdk
docker compose -f compose.wasm.yaml up -d
```

This starts local services needed by wasm interop pages:
1. RGB proxy (`127.0.0.1:3000`)
2. Esplora HTTP indexer (`127.0.0.1:3002`)
3. Electrum (`127.0.0.1:50001`)
4. Unified wasm gateway (`127.0.0.1:3001`)

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

If you are not using `compose.wasm.yaml`, start the unified wasm gateway manually
(LN websocket relay + RGB JSON-RPC pass-through):

```sh
cargo run -p wasm-proxy-gateway
```

Defaults:

1. Listen: `127.0.0.1:3001`
2. RGB upstream: `http://127.0.0.1:3000/json-rpc`

Useful env vars:

1. `WASM_PROXY_LISTEN_ADDR=0.0.0.0:3001`
2. `WASM_PROXY_RGB_UPSTREAM=http://127.0.0.1:3000/json-rpc`
3. `WASM_PROXY_RELAY_AUTH_REQUIRED=true`
4. `WASM_PROXY_RELAY_AUTH_TOKEN=...`
5. `WASM_PROXY_RELAY_AUTH_NODE_ID=...`
6. `WASM_PROXY_RGB_MAX_BODY_BYTES=2097152`
7. `WASM_PROXY_RGB_REQUEST_TIMEOUT_MS=15000`
8. `WASM_PROXY_TCP_CONNECT_TIMEOUT_MS=5000`
9. `WASM_PROXY_IO_IDLE_TIMEOUT_MS=60000`
10. `WASM_PROXY_WS_MAX_FRAME_BYTES=131072`
11. `WASM_PROXY_WS_MAX_MESSAGE_BYTES=262144`
12. `WASM_PROXY_MAX_ACTIVE_WS=512`
13. `WASM_PROXY_MAX_ACTIVE_WS_PER_IP=64`
14. `WASM_PROXY_ALLOW_PUBLIC_TARGETS=false`
15. `WASM_PROXY_TARGET_ALLOWLIST=127.0.0.1,localhost,::1`

Open:

```text
http://localhost:8080/bindings/wasm-sdk/examples/wasm-interop/
```

Click `Run Example`.
`index.html` is prefilled with local defaults:
`ws://127.0.0.1:3001`, `127.0.0.1:9746`,
`02399514a480a9b9d041651fd408c1483a2a3dff33a74a158dc948d120930fa011`.

For the Python-equivalent flow page, open:

```text
http://localhost:8080/bindings/wasm-sdk/examples/wasm-interop/virtual_channels_flow.html
```

Click `Run Flow`.
`virtual_channels_flow.html` is prefilled with local defaults for:
proxy URLs (`ws://127.0.0.1:3001`), peer addrs (`127.0.0.1:9745/9746`),
peer pubkeys, and indexer URL (`http://127.0.0.1:3002`).

If your proxy requires authenticated websocket sessions, define this callback in
the browser console before running the flow:

```js
window.getRelaySessionAuth = async ({ nodeLabel, challenge, signedMessage }) => {
  // Send challenge + signedMessage to your backend/proxy auth endpoint.
  // Backend verifies signature and returns relay token + relay node id.
  return {
    relayAuthToken: "paste-token-here",
    relayNodeId: "02aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
  };
};
```

The flow signs a challenge per node via `node.signMessageValue(challenge)` and
applies returned credentials with `node.setRelaySessionAuth(...)` before
`connectPeer`/`openChannel`.

Alternatively, you can fill relay auth values directly in the
`virtual_channels_flow.html` inputs:

1. `Node A relay auth token` + `Node A relay node id`
2. `Node B relay auth token` + `Node B relay node id`

If UI values are present, they are used first and callback mode is skipped.

For RGB-over-Lightning flow, open:

```text
http://localhost:8080/bindings/wasm-sdk/examples/wasm-interop/rgb_asset_transfer_flow.html
```

Click `Run RGB over Lightning Flow`.
`rgb_asset_transfer_flow.html` is prefilled with local defaults for:
indexer URL (`http://127.0.0.1:3002`), node proxy URL (`ws://127.0.0.1:3001`),
RGB transport endpoint (`http://127.0.0.1:3001/rgb/json-rpc`),
and peer addresses (`127.0.0.1:9745` / `127.0.0.1:9746`).

The flow is:
`connectPeer` -> `openChannelValue` ->
`issueAssetNia` -> `createLnInvoiceValue` ->
`sendPaymentValue` with `asset_id` + `asset_amount`.

When sender wallet BTC is zero, the page now tries automatic local regtest funding
through wasm gateway endpoint `POST /dev/regtest/fund`.

Defaults in `wasm-proxy-gateway`:

1. `WASM_PROXY_REGTEST_FUNDING_ENABLED=true`
2. `WASM_PROXY_REGTEST_SCRIPT_PATH=./regtest.sh`
3. `WASM_PROXY_REGTEST_FUNDING_USE_REGTEST_SH=true`
4. `WASM_PROXY_REGTEST_FUNDING_USE_ESPLORA_RPC=true`

If auto-funding is disabled or unavailable, the page logs a manual fallback command:

```sh
./regtest.sh sendtoaddress <sender_address> 1
./regtest.sh mine 6
```

## Notes

1. This example is browser-only (`--target web`).
2. It focuses on deterministic interop surface checks, not full native RLN node runtime behavior.
3. Peer-session start may fail in normal local runs if proxy/peer is not reachable; the example logs this as non-fatal.
4. The RGB over Lightning page uses fixed constants in JS for:
   - `Indexer URL`: `http://127.0.0.1:3002`
   - `Node proxy URL` (for node handle construction): `ws://127.0.0.1:3001`
   - `RGB transport endpoint`: `http://127.0.0.1:3001/rgb/json-rpc`
   - `LN amount`: `3_000_000 msat` (minimum RGB-LN compatible amount)
   - canonical RGB `asset_id` values (`rgb:...`) are accepted in LN APIs
   - sender/receiver RLN instances are initialized at runtime and each SDK instance configures transport via `setDefaultRgbProxyTransport(...)`
5. Additional runtime observability/control APIs are available on node/facade/node-handle surfaces:
   - `ldkRuntimeComponentsValue/Json` (runtime component readiness + lifecycle counters)
   - `chainSyncStart*` / `chainSyncStatus*` / `chainSyncStop*` / `chainSyncTick*` / `chainSyncEnqueueRebroadcastTx`
   - `listRgbLnTransfersValue/Json` (RGB-over-LN transfer ledger derived from LN payment state)
6. `manual_js_wasm_interop.js` and `manual_js_virtual_channels_sdk_flow.js` now demonstrate these APIs directly
   (components status, chain sync start/status/stop, RGB-LN transfer listing).
