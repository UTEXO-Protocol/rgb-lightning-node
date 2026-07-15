# wasm-proxy-gateway

Unified gateway for WASM clients:
- `GET /v1/:host/:port` and `GET /ln/v1/:host/:port` - WebSocket to TCP relay for LN peer traffic.
- `POST /rgb/json-rpc` - JSON-RPC pass-through to RGB proxy upstream.
- `GET /healthz` - liveness endpoint.

This crate is designed to run in production without `dev-http`.

## UTEXO signet deployment

This folder includes a deployment template for a public signet endpoint (example: `wss://ln-gateway-utexo.utexo.com`):

- `deploy/compose.signet.yaml`
- `deploy/.env.signet.example`

### 1) Prepare env file

```bash
cd tools/wasm-proxy-gateway/deploy
cp .env.signet.example .env.signet
```

Fill all placeholders before launch.

### 2) Build and start

From repository root:

```bash
docker compose -f tools/wasm-proxy-gateway/deploy/compose.signet.yaml up -d --build
```

### 3) Validate

```bash
curl -fsS http://127.0.0.1:3001/healthz
```

Expected payload:

```json
{"ok":true}
```

## Required envs for signet

These variables must be reviewed with Roman/Renat before production rollout:

- `WASM_PROXY_RGB_UPSTREAM` - RGB proxy upstream JSON-RPC URL.
- `WASM_PROXY_CORS_ALLOW_ORIGINS` - allowed browser origins (comma-separated).
- `WASM_PROXY_RELAY_AUTH_REQUIRED=true`
- `WASM_PROXY_RELAY_AUTH_TOKEN` - relay bearer token shared with trusted WASM clients.
- `WASM_PROXY_RELAY_NODE_ID` - expected node identity used by the client.
- `WASM_PROXY_ALLOW_PUBLIC_TARGETS=true` (required for signet/public peers).
- `WASM_PROXY_TARGET_ALLOWLIST` - optional hard allowlist for known peer hosts.

Recommended operational limits:

- `WASM_PROXY_MAX_ACTIVE_WS`
- `WASM_PROXY_MAX_ACTIVE_WS_PER_IP`
- `WASM_PROXY_IO_IDLE_TIMEOUT_MS`
- `WASM_PROXY_TCP_CONNECT_TIMEOUT_MS`

## Security notes

- Keep `WASM_PROXY_RELAY_AUTH_REQUIRED=true` in production.
- Terminate TLS at Nginx and proxy traffic to `127.0.0.1:3001`.
- If possible, tighten `WASM_PROXY_CORS_ALLOW_ORIGINS` to exact UTEXO frontend origins.
- Rotate relay auth token regularly and after incident response.
