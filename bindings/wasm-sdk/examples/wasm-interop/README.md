# WASM Interop Examples

Automated, end-to-end RGB-over-Lightning flows that run the wasm SDK runtime headless
against a native `rgb-native-phase5-node` peer. Each driver boots the native node(s),
serves this directory over a tiny static server, launches system Chrome via
`puppeteer-core` (headless, web-security disabled), runs the in-browser flow, and asserts
success.

## Flows

- **`run_e2e_full_flow.mjs`** (page `rgb_e2e_full_flow.html` + `manual_js_rgb_e2e_full_flow.js`):
  full single-peer flow over **trusted virtual channels**. The native node acts as an **LSP** that
  *opens* both a vanilla (BTC) and an RGB virtual channel **to** the wasm node (0-conf, scid-privacy,
  never-broadcast dust=1 funding); the wasm node **accepts** them via its `Event::OpenChannelRequest`
  handler and holds the BTC + RGB liquidity the LSP pushes. The LSP issues the RGB asset. Then settle
  real BTC / BOLT11 / HODL payments both directions and RGB BOLT11 + keysend both directions; finally
  the LSP abandons both virtual channels and we verify state is restored from persistence.
  **Requires the native LSP to run with `--enable-virtual-channels-v0`.**
- **`run_multihop_flow.mjs`** (page `rgb_multihop_flow.html` + `manual_js_rgb_multihop_flow.js`):
  multi-hop forwarding `native-A -> WASM -> native-B` (boots two native nodes).
- **`run_apay_lsp_flow.mjs`** (page `apay_lsp_flow.html` + `manual_js_apay_lsp_flow.js`):
  **async payments with LSP** — two independent wasm recipient nodes each open a channel to a
  native invoice-host node and call `apayNewWithAddress` / `apayNew` to register signed hash
  batches with a real `utexo-lsp`; the flow asserts the LSP tracked
  them as separate orders. See the dedicated section below.

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

> **Virtual-channel full flow (`run_e2e_full_flow.mjs`):** the native LSP node
> (LN peer `127.0.0.1:9802`, REST `127.0.0.1:3101`) must be started with
> `--enable-virtual-channels-v0`. In this flow the LSP is the channel opener/funder **and** the RGB
> asset issuer — it opens both virtual channels to the wasm node and pushes BTC + RGB liquidity; the
> wasm node only accepts. The LSP wallet is funded on-chain by the flow itself via the gateway
> regtest faucet, so it just needs to be unlocked and online.

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
`window.__sdk` without driving anything. The Playwright suite under `../../e2e-specs`
drives this harness, so keep it in sync with the SDK surface.

## Async payments with LSP (`run_apay_lsp_flow.mjs`)

This flow needs two extra services beyond the standard infra + wasm pkg: a **native invoice-host
node** and a **utexo-lsp** instance. Topology:

```
wasm client ──(LN custom msg 37915: async_order.new)──▶ native invoice-host RLN node
                                                         │  (--lsp-base-url)
                                                         ▼ HTTP /internal/async_order/new
                                                       utexo-lsp ──▶ sqlite
```

The wasm node never talks to the LSP directly — it speaks Lightning to the host node, which relays
to the LSP over HTTP.

### 1. Standard infra + wasm pkg

```sh
cd bindings/wasm-sdk
docker compose -f compose.wasm.yaml up -d
wasm-pack build --target web --dev --out-dir pkg
```

### 2. Native invoice-host node (peer `9802`, REST `3101`), wired to the LSP

```sh
APAY_TOKEN=devtoken
cargo build --release --bin rgb-lightning-node
rm -rf /tmp/apay-host-data && mkdir -p /tmp/apay-host-data
./target/release/rgb-lightning-node /tmp/apay-host-data \
  --daemon-listening-port 3101 \
  --ldk-peer-listening-port 9802 \
  --network regtest \
  --disable-authentication \
  --lsp-base-url http://127.0.0.1:8080 \
  --lsp-bearer-token "$APAY_TOKEN" &

# init + unlock (point it at the compose infra indexer/proxy)
curl -sf -X POST http://127.0.0.1:3101/init -H 'content-type: application/json' \
  -d '{"password":"rln-password","mnemonic":"abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about"}' || true
curl -sf -X POST http://127.0.0.1:3101/unlock -H 'content-type: application/json' \
  -d '{"password":"rln-password","indexer_url":"http://127.0.0.1:3002","proxy_endpoint":"rpc://127.0.0.1:3005/json-rpc","announce_addresses":[]}'
```

### 3. utexo-lsp (`:8080`), pointed back at the host node

```sh
cd ../utexo-lsp              # sibling of the rgb-lightning-node repo root
export SERVER_ADDR=":8080"
export LSP_BASE_URL="http://127.0.0.1:3101"        # the invoice-host node's REST
export RGB_NODE_BASE_URL="http://127.0.0.1:3101"
export LIGHTNING_ADDRESS_DOMAIN_URL="http://127.0.0.1:8080"
export APAY_BEARER_TOKEN="devtoken"                # must match --lsp-bearer-token above
export CRON_EVERY="10s"
go run .
# health: curl -s http://127.0.0.1:8080/health
```

### 4. Run the flow

```sh
PUPPETEER_EXECUTABLE_PATH=/usr/bin/google-chrome \
E2E_PUPPETEER=/tmp/e2e-driver/node_modules/puppeteer-core \
node bindings/wasm-sdk/examples/wasm-interop/run_apay_lsp_flow.mjs
```

It provisions **two** independent wasm recipient nodes; each opens a vanilla channel to the host,
calls `apayNewWithAddress` (registering a signed 200-hash batch + a `username@domain`
Lightning-Address attestation), then `apayNew` to refill, asserting the LSP accepted both batches
and the per-host hash cursor advanced (no hash reuse). It then asserts the two recipients are
distinct LN peers tracked as separate LSP orders.

It then runs the **payment path**: node A keysends the host some outbound liquidity, node B resolves
node A's Lightning Address via the LSP's LNURL-pay endpoints and pays it. The host holds B's HTLC
and asks A for an invoice (`async_order.request_invoice` over custom message 37915); A's wasm backend
re-derives the preimage for the requested hash and mints the invoice. **That A-mints-the-invoice
step (the new wasm capability) is hard-asserted.** A successful run ends with `✅✅✅ ... PASSED`
and exit code 0.

> **Full settlement is best-effort, not asserted.** The final host→A forward + B settle requires the
> host node to `/sendpayment` an invoice whose hash it already holds as a HODL inbound — utexo-lsp's
> intended single-node forward (it pays the outbound to *learn the preimage*, then claims B's hodl).
> A stock `rgb-lightning-node` rejects this with `PaymentHashAlreadyUsed`
> ([routes.rs](../../../../src/routes.rs), in `send_payment`), so B's payment stays pending and the
> example logs that rather than failing. To observe full end-to-end settlement, relax that guard so
> the node permits paying a hash it holds as a hodl inbound. The payment leg also depends on the
> utexo-lsp cron (`CRON_EVERY`) advancing its outbox and on enough host→A liquidity (seeded by A's
> keysend).

The defaults (host peer `127.0.0.1:9802`, host
REST `127.0.0.1:3101`, LN-address domain `127.0.0.1:8080`) live in `DEFAULTS` in
`manual_js_apay_lsp_flow.js` and can be overridden per run via page query params
(`?nativePeerAddr=`, `?nativeMgmtUrl=`, `?lnAddressDomain=`) when opening `apay_lsp_flow.html`
manually in a browser.

## Notes

1. The wasm SDK is browser-only (`--target web`); these flows drive it from headless Chrome.
2. RGB-over-LN payments use a minimum compatible LN amount of `3_000_000 msat`; canonical
   RGB `asset_id` values (`rgb:...`) are accepted in LN APIs.
3. Runtime observability/control APIs available on the node/facade surfaces include
   `ldkRuntimeComponentsValue/Json`, the `chainSync*` family, and
   `listRgbLnTransfersValue/Json` (RGB-over-LN transfer ledger derived from LN payment state).
