# WASM ↔ regular RLN (browser Playwright)

End-to-end browser tests covering native WASM SDK flows against a regular
`rgb-lightning-node` over the proxy gateway.

Canonical stack overview: `bindings/wasm-sdk/ARCHITECTURE.md`.

## Test architecture

Every spec drives the SDK directly via `window.__sdk` exposed by
`bindings/wasm-sdk/examples/wasm-interop/wasm_e2e_harness.html` (no log
scraping), funds via a Node-side regtest controller (`helpers/regtest.js`)
talking directly to `bitcoind` RPC, and asserts on typed JSON state from
`listChannelsJson()`, `listPaymentsJson()`, `ldkRuntimeStatusJson()`, etc. via
reusable matchers in `helpers/sdk.js`. On failure, a structured snapshot is
dumped under `test-results/<spec>/sdk-snapshot-failure/` plus a console summary.

The suite has a **smoke** target and a **full** target:

- **Smoke** (`E2E_SUITE=smoke` or `npm run test:smoke`): the happy-path channel
  spec only.
- **Full** (`E2E_SUITE=full` or `npm run test:full`): smoke plus the page-reload
  and websocket-disconnect specs.

| Spec | Scenario |
| --- | --- |
| `wasm_regular_rln_native_channel.spec.js` | Happy path: connect peer, open native channel, fund (regtest controller), confirm, keysend. |
| `wasm_regular_rln_page_reload.spec.js` | Mid-flow **full page reload**, then second run; asserts state restored from persistence. |
| `wasm_regular_rln_websocket_disconnect.spec.js` | **WebSocket** to the LN relay closed mid-run; flow still completes after reconnect. |

Run smoke locally:

```bash
E2E_AUTO_PROVISION_REGULAR_RLN=1 \
E2E_SUITE=smoke \
./scripts/ci/wasm_regular_rln_e2e.sh
```

Run full suite (smoke + legacy):

```bash
E2E_AUTO_PROVISION_REGULAR_RLN=1 \
E2E_SUITE=full \
./scripts/ci/wasm_regular_rln_e2e.sh
```

## Port and URL contract

| Service | Default host:port | Notes |
|--------|-------------------|--------|
| Static HTTP (examples + `pkg/`) | `127.0.0.1:8080` | Repo root; Playwright `webServer` starts this automatically. |
| WASM proxy gateway | `127.0.0.1:3001` | LN WS relay, `/rgb/json-rpc`, `/dev/regular-rln/*`, regtest helpers. |
| RGB HTTP proxy (upstream) | `127.0.0.1:3005` | Host port for compose `proxy` (container `3000`). |
| Esplora HTTP (indexer) | `127.0.0.1:3002` | Default in the HTML / manual JS. |
| Bitcoind RPC (compose `bitcoind`) | `127.0.0.1:19443` | Mapped from container `18443`; user `admin`, password `passw` (matches `helpers/regtest.js` defaults). |
| Regular RLN REST | `127.0.0.1:3101` | Native daemon; the structured specs call it directly (`helpers/regular-rln.js`). A gateway `/dev/regular-rln` proxy also exists (feature `dev-http`) but is not used by these specs. |
| Regular RLN LN peer | `127.0.0.1:9802` | WASM connects through gateway to this TCP address. |

Override the static base URL if needed:

```bash
export E2E_HTTP_BASE_URL=http://127.0.0.1:9090
npm run test:wasm-regular-rln-e2e
```

## One-shot local reproduction

From the **repository root**:

1. **Infra** (creates `bindings/wasm-sdk/tmp/wasm-compose/` on first run):

   ```bash
   mkdir -p bindings/wasm-sdk/tmp/wasm-compose/datacore bindings/wasm-sdk/tmp/wasm-compose/dataindex
   docker compose -f bindings/wasm-sdk/compose.wasm-infra.yaml up -d
   ```

   **Option A (recommended, CI-style):** run the gateway on the host (fast, no `cargo run` in Docker):

   ```bash
   cargo build --release -p wasm-proxy-gateway
   WASM_PROXY_REGULAR_RLN_API_BASE=http://127.0.0.1:3101 \
     ./target/release/wasm-proxy-gateway
   ```

   **Option B:** full stack including gateway in Docker (first start can take a long time while the image compiles the workspace):

   ```bash
   docker compose -f bindings/wasm-sdk/compose.wasm.yaml up -d
   ```

   Wait until the gateway answers:

   ```bash
   until curl -sf http://127.0.0.1:3001/healthz; do sleep 2; done
   ```

2. **WASM package** (served from `bindings/wasm-sdk/pkg/`):

   ```bash
   rustup target add wasm32-unknown-unknown
   cargo install wasm-pack --locked  # once
   (cd bindings/wasm-sdk && wasm-pack build --target web --release --out-dir pkg)
   ```

3. **Regular RLN** on `3101` / `9802` (empty storage dir example):

   ```bash
   export E2E_REGULAR_RLN_DATA_DIR="${E2E_REGULAR_RLN_DATA_DIR:-$(pwd)/.e2e-regular-rln-datadir}"
   mkdir -p "$E2E_REGULAR_RLN_DATA_DIR"
   cargo build --release --bin rgb-lightning-node
   ./target/release/rgb-lightning-node "$E2E_REGULAR_RLN_DATA_DIR" \
     --daemon-listening-port 3101 \
     --ldk-peer-listening-port 9802 \
     --network regtest \
     --disable-authentication
   ```

   In another shell, **init + unlock** (first run only; adjust if already initialized):

   ```bash
   curl -sS -X POST http://127.0.0.1:3101/init \
     -H 'content-type: application/json' \
     -d '{"password":"rln-password","mnemonic":"abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about"}'

   curl -sS -f -X POST http://127.0.0.1:3101/unlock \
     -H 'content-type: application/json' \
     -d '{
       "password":"rln-password",
       "bitcoind_rpc_username":"user",
       "bitcoind_rpc_password":"password",
       "bitcoind_rpc_host":"127.0.0.1",
       "bitcoind_rpc_port":19443,
       "indexer_url":"http://127.0.0.1:3002",
       "proxy_endpoint":"rpc://127.0.0.1:3005/json-rpc",
       "announce_addresses":[]
     }'
   ```

4. **Gateway → regular RLN** (defaults match `compose.wasm.yaml`; override if your REST port differs):

   ```bash
   export WASM_PROXY_REGULAR_RLN_API_BASE=http://host.docker.internal:3101
   ```

   If the gateway runs **on the host** (not in Docker), use `http://127.0.0.1:3101`.  
   If it runs **inside Docker** on Linux, use `http://172.17.0.1:3101` or add the daemon to the same compose network; `host.docker.internal` works on Docker Desktop (macOS/Windows).

5. **Playwright** (installs browsers under the user cache, not in this repo):

   ```bash
   cd e2e-specs
   npm ci
   npx playwright install chromium
   npm run test:wasm-regular-rln-e2e
   ```

### Orchestration script

From the repo root (after Docker is available):

```bash
./scripts/ci/wasm_regular_rln_e2e.sh
```

This waits for `http://127.0.0.1:3001/healthz`, provisions a fresh regular RLN data dir when `E2E_AUTO_PROVISION_REGULAR_RLN=1` (CI sets this), builds the WASM `pkg` if missing, then runs `npm ci` and Playwright. With auto-provision, the script removes `E2E_REGULAR_RLN_DATA_DIR` first unless `E2E_KEEP_REGULAR_RLN_DATA=1`.

## CI

This browser suite is not wired into a GitHub Actions workflow; run it locally
with the orchestration script (`scripts/ci/wasm_regular_rln_e2e.sh`) or the
`npm run test:*` commands above. (`.github/workflows/wasm-artifacts.yaml` only
builds the wasm `pkg/` artifact; it does not run these tests.)

## Environment variables (gateway)

See `tools/wasm-proxy-gateway/src/main.rs` for the full list. Common values:

| Variable | Role |
|----------|------|
| `WASM_PROXY_LISTEN_ADDR` | Bind address (default `127.0.0.1:3001`). |
| `WASM_PROXY_RGB_UPSTREAM` | RGB JSON-RPC upstream. |
| `WASM_PROXY_REGULAR_RLN_API_BASE` | Base URL for `/dev/regular-rln` proxy (default `http://127.0.0.1:3101`). |
