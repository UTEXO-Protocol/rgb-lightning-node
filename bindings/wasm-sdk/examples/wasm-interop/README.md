# WASM Interop Examples

Automated, end-to-end RGB-over-Lightning flows that run the wasm SDK runtime headless
against a native `rgb-native-phase5-node` peer. Each driver boots the native node(s),
serves this directory over a tiny static server, launches system Chrome via
`puppeteer-core` (headless, web-security disabled), runs the in-browser flow, and asserts
success.

## Flows

- **`run_e2e_full_flow.mjs`** (page `rgb_e2e_full_flow.html` + `manual_js_rgb_e2e_full_flow.js`):
  full single-peer flow — open a vanilla (BTC) channel and an RGB channel, settle real
  BTC / BOLT11 / HODL payments both directions, settle RGB BOLT11 + keysend both
  directions, then close and verify channel state is restored from persistence.
- **`run_multihop_flow.mjs`** (page `rgb_multihop_flow.html` + `manual_js_rgb_multihop_flow.js`):
  multi-hop forwarding `native-A -> WASM -> native-B` (boots two native nodes).

## Prerequisites

Start local infra from the wasm-sdk crate root:

```sh
cd bindings/wasm-sdk
docker compose -f compose.wasm.yaml up -d
```

This starts the services the flows need:
1. RGB proxy (`127.0.0.1:3000`)
2. Esplora HTTP indexer (`127.0.0.1:3002`)
3. Electrum (`127.0.0.1:50001`)
4. Unified wasm gateway — LN websocket relay + RGB JSON-RPC pass-through (`127.0.0.1:3001`)

Build the wasm package (`pkg/`). The `real-wasm-rgb` backend feature is enabled by default,
so no extra `--features` flag is needed:

```sh
cd bindings/wasm-sdk
wasm-pack build --target web --dev --out-dir pkg
```

The `rgb-native-phase5-node` harness binary must be available at the path the driver
expects (`run_multihop_flow.mjs` resolves it under
`../rust-lightning/contrib/rgb-cross-variant-harness/target/debug/`). A headless Chrome
and a `puppeteer-core` install are also required (see env vars below).

## Run

```sh
PUPPETEER_EXECUTABLE_PATH=/usr/bin/google-chrome \
E2E_PUPPETEER=/tmp/e2e-driver/node_modules/puppeteer-core \
node bindings/wasm-sdk/examples/wasm-interop/run_e2e_full_flow.mjs
```

```sh
PUPPETEER_EXECUTABLE_PATH=/usr/bin/google-chrome \
E2E_PUPPETEER=/tmp/e2e-driver/node_modules/puppeteer-core \
node bindings/wasm-sdk/examples/wasm-interop/run_multihop_flow.mjs
```

A successful run ends with `✅✅✅ ... PASSED` and exit code 0.

### Driver env vars

- `PUPPETEER_EXECUTABLE_PATH` — path to Chrome (default `/usr/bin/google-chrome`).
- `E2E_PUPPETEER` — path to a `puppeteer-core` install.
- `E2E_STATIC_PORT` — static-server port (`0` = random free port).
- `E2E_RUN_TIMEOUT_MS` / `E2E_VERIFY_TIMEOUT_MS` — timeouts for the run and verify steps.
- `E2E_ESPLORA_URL` — indexer URL (default `http://127.0.0.1:3002`).
- `E2E_VERBOSE` — surface a narrow set of router/path-finding log lines from the page.

## Gateway

If you are not using `compose.wasm.yaml`, start the unified wasm gateway manually:

```sh
cargo run -p wasm-proxy-gateway
```

Defaults: listen `127.0.0.1:3001`, RGB upstream `http://127.0.0.1:3000/json-rpc`.

Useful env vars:

1. `WASM_PROXY_LISTEN_ADDR=0.0.0.0:3001`
2. `WASM_PROXY_RGB_UPSTREAM=http://127.0.0.1:3000/json-rpc`
3. `WASM_PROXY_RELAY_AUTH_REQUIRED=true`
4. `WASM_PROXY_RELAY_AUTH_TOKEN=...`
5. `WASM_PROXY_RELAY_NODE_ID=...`
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

## Test fixtures

`wasm_e2e_harness.html` + `wasm_e2e_harness.js` are not a standalone flow: they construct
the SDK (`RlnWasmNode` + `RlnWasmWallet`), bring the wallet online, and expose everything on
`window.__sdk` without driving anything. The Playwright suite under `../../../../e2e-specs`
drives this harness, so keep it in sync with the SDK surface.

## Notes

1. The wasm SDK is browser-only (`--target web`); these flows drive it from headless Chrome.
2. RGB-over-LN payments use a minimum compatible LN amount of `3_000_000 msat`; canonical
   RGB `asset_id` values (`rgb:...`) are accepted in LN APIs.
3. Runtime observability/control APIs available on the node/facade surfaces include
   `ldkRuntimeComponentsValue/Json`, the `chainSync*` family, and
   `listRgbLnTransfersValue/Json` (RGB-over-LN transfer ledger derived from LN payment state).
