#!/usr/bin/env bash
# Browser E2E: WASM SDK ↔ regular rgb-lightning-node (proxy + regtest + Playwright).
#
# Stages (run sequentially, each independently logged):
#   1. setup        — env summary, artifact dir, regular-RLN datadir
#   2. infra-up     — docker compose up + readiness probes
#   3. services-up  — build wasm-proxy-gateway, optionally provision regular RLN
#   4. wasm-build   — wasm-pack build (cached unless E2E_WASM_PACK_BUILD=1)
#   5. playwright   — npm ci / install browsers / run specs
#   6. infra-down   — capture infra logs, kill spawned services
#
# Each stage logs a `==> <stage>` banner and writes to per-stage files in
# the run artifact directory.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$ROOT"

#############################################################################
# Configuration
#############################################################################

E2E_AUTO_PROVISION_REGULAR_RLN="${E2E_AUTO_PROVISION_REGULAR_RLN:-0}"
E2E_RESET_INFRA="${E2E_RESET_INFRA:-0}"
E2E_CLEAN_INFRA_BIND_MOUNTS="${E2E_CLEAN_INFRA_BIND_MOUNTS:-${CI:-0}}"
E2E_NPM_CI="${E2E_NPM_CI:-1}"
E2E_PLAYWRIGHT_INSTALL="${E2E_PLAYWRIGHT_INSTALL:-1}"
E2E_WASM_PACK_BUILD="${E2E_WASM_PACK_BUILD:-0}"
E2E_WASM_PACK_MODE="${E2E_WASM_PACK_MODE:-normal}"
E2E_WASM_PACK_TIMEOUT_SEC="${E2E_WASM_PACK_TIMEOUT_SEC:-900}"
E2E_WASM_PACK_NO_OPT="${E2E_WASM_PACK_NO_OPT:-1}"
# `dev-http` provides the gateway's `/dev/regtest/*` and `/dev/regular-rln/*`
# helpers. Phase 1 specs no longer use them, so we keep the feature off by
# default (gateway runs in production shape) but allow opt-in for legacy specs.
# `dev-http` is an optional gateway feature that exposes `/dev/*` helper
# endpoints. The current E2E suite (ported from `feat/fix-rln-connect`) does
# not require them, and some branches of `wasm-proxy-gateway` may not compile
# the feature at all. Keep it disabled by default.
E2E_USE_DEV_HTTP="${E2E_USE_DEV_HTTP:-0}"
# Phase split:
#   smoke — only the new structured specs (Phase 1).
#   full  — every wasm_regular_rln*.spec.js (legacy + new).
E2E_SUITE="${E2E_SUITE:-smoke}"

COMPOSE_INFRA="$ROOT/bindings/wasm-sdk/compose.wasm-infra.yaml"
GATEWAY_PID=""
RLN_PID=""
ARTIFACTS_DIR="${ARTIFACTS_DIR:-$ROOT/.e2e-artifacts/wasm-regular-rln}"
RUN_ID="${RUN_ID:-$(date +%Y%m%d-%H%M%S)}"
RUN_DIR="$ARTIFACTS_DIR/$RUN_ID"
GATEWAY_LOG="$RUN_DIR/wasm-proxy-gateway.log"
RLN_LOG="$RUN_DIR/regular-rln.log"
COMPOSE_LOG="$RUN_DIR/compose.infra.log"
ENV_LOG="$RUN_DIR/env.txt"
STAGE_LOG="$RUN_DIR/stages.log"
STATUS="ok"

E2E_REGULAR_RLN_DATA_DIR="${E2E_REGULAR_RLN_DATA_DIR:-$ROOT/.e2e-regular-rln-datadir/$RUN_ID}"

#############################################################################
# Utilities
#############################################################################

stage() {
  local name=$1
  shift
  local banner="==> [$(date '+%H:%M:%S')] $name"
  echo "$banner"
  if [[ -d "$RUN_DIR" ]]; then
    echo "$banner" >>"$STAGE_LOG" 2>/dev/null || true
  fi
}

cleanup() {
  cd "$ROOT" || true

  {
    echo "==> docker compose ps"
    docker compose -f "$COMPOSE_INFRA" ps || true
    echo ""
    echo "==> docker compose logs (tail)"
    docker compose -f "$COMPOSE_INFRA" logs --no-color --tail=200 || true
  } >"$COMPOSE_LOG" 2>&1 || true

  if [[ "$STATUS" != "ok" ]]; then
    echo "E2E failed. Logs are in: $RUN_DIR" >&2
  fi

  if [[ -n "$GATEWAY_PID" ]] && kill -0 "$GATEWAY_PID" 2>/dev/null; then
    kill "$GATEWAY_PID" 2>/dev/null || true
  fi
  if [[ -n "$RLN_PID" ]] && kill -0 "$RLN_PID" 2>/dev/null; then
    kill "$RLN_PID" 2>/dev/null || true
  fi
}
trap cleanup EXIT
trap 'STATUS="error"' ERR

port_in_use() {
  local port=$1
  if command -v ss >/dev/null 2>&1; then
    ss -ltn "( sport = :$port )" | tail -n +2 | grep -q .
    return $?
  fi
  if command -v lsof >/dev/null 2>&1; then
    lsof -nP -iTCP:"$port" -sTCP:LISTEN >/dev/null 2>&1
    return $?
  fi
  return 1
}

assert_port_free() {
  local port=$1
  local label=$2
  if port_in_use "$port"; then
    echo "port $port is already in use ($label). Stop the existing process and rerun." >&2
    return 1
  fi
}

wait_http() {
  local url=$1
  local label=$2
  local deadline=$((SECONDS + 300))
  while (( SECONDS < deadline )); do
    local code
    code="$(curl -sS -o /dev/null -w '%{http_code}' "$url" || true)"
    code="${code:-000}"
    if [[ "$code" != "000" ]]; then
      echo "ok: $label (http $code)"
      return 0
    fi
    sleep 2
  done
  echo "timeout waiting for $label ($url)" >&2
  return 1
}

#############################################################################
# Stage 1 — setup
#############################################################################

mkdir -p "$RUN_DIR"

stage "stage 1 / setup"
{
  echo "RUN_ID=$RUN_ID"
  echo "ROOT=$ROOT"
  echo "E2E_SUITE=$E2E_SUITE"
  echo "E2E_AUTO_PROVISION_REGULAR_RLN=$E2E_AUTO_PROVISION_REGULAR_RLN"
  echo "E2E_REGULAR_RLN_DATA_DIR=$E2E_REGULAR_RLN_DATA_DIR"
  echo "E2E_RESET_INFRA=$E2E_RESET_INFRA"
  echo "E2E_CLEAN_INFRA_BIND_MOUNTS=$E2E_CLEAN_INFRA_BIND_MOUNTS"
  echo "E2E_NPM_CI=$E2E_NPM_CI"
  echo "E2E_PLAYWRIGHT_INSTALL=$E2E_PLAYWRIGHT_INSTALL"
  echo "E2E_WASM_PACK_BUILD=$E2E_WASM_PACK_BUILD"
  echo "E2E_WASM_PACK_MODE=$E2E_WASM_PACK_MODE"
  echo "E2E_WASM_PACK_TIMEOUT_SEC=$E2E_WASM_PACK_TIMEOUT_SEC"
  echo "E2E_WASM_PACK_NO_OPT=$E2E_WASM_PACK_NO_OPT"
  echo "E2E_USE_DEV_HTTP=$E2E_USE_DEV_HTTP"
  echo "WASM_PROXY_LISTEN_ADDR=${WASM_PROXY_LISTEN_ADDR:-}"
  echo "WASM_PROXY_RGB_UPSTREAM=${WASM_PROXY_RGB_UPSTREAM:-}"
  echo "WASM_PROXY_REGULAR_RLN_API_BASE=${WASM_PROXY_REGULAR_RLN_API_BASE:-}"
} >"$ENV_LOG"

#############################################################################
# Stage 2 — infra-up
#############################################################################

stage "stage 2 / infra-up"

if [[ "$E2E_RESET_INFRA" == "1" ]]; then
  echo "  reset infra volumes + state"
  docker compose -f "$COMPOSE_INFRA" down -v --remove-orphans || true
  rm -rf bindings/wasm-sdk/tmp/wasm-compose || true
fi

mkdir -p bindings/wasm-sdk/tmp/wasm-compose/datacore bindings/wasm-sdk/tmp/wasm-compose/dataindex
if [[ "$E2E_CLEAN_INFRA_BIND_MOUNTS" == "1" ]]; then
  echo "  clean infra bind mounts (hermetic run)"
  rm -rf bindings/wasm-sdk/tmp/wasm-compose/datacore/* bindings/wasm-sdk/tmp/wasm-compose/dataindex/* || true
fi

docker compose -f "$COMPOSE_INFRA" up -d

wait_http "http://127.0.0.1:3002" "esplora (http)"
wait_http "http://127.0.0.1:3005/healthz" "rgb-proxy (healthz)" || true

#############################################################################
# Stage 3 — services-up (gateway + optional regular RLN)
#############################################################################

stage "stage 3 / services-up"

GATEWAY_FEATURES=()
if [[ "$E2E_USE_DEV_HTTP" == "1" ]]; then
  GATEWAY_FEATURES=(--features dev-http)
  echo "  gateway: dev-http enabled"
else
  echo "  gateway: dev-http disabled (production shape)"
fi

cargo build --release -p wasm-proxy-gateway "${GATEWAY_FEATURES[@]}"

export WASM_PROXY_LISTEN_ADDR="${WASM_PROXY_LISTEN_ADDR:-127.0.0.1:3001}"
export WASM_PROXY_RGB_UPSTREAM="${WASM_PROXY_RGB_UPSTREAM:-http://127.0.0.1:3005/json-rpc}"
export WASM_PROXY_REGULAR_RLN_API_BASE="${WASM_PROXY_REGULAR_RLN_API_BASE:-http://127.0.0.1:3101}"
export WASM_PROXY_RELAY_AUTH_REQUIRED="${WASM_PROXY_RELAY_AUTH_REQUIRED:-false}"
export WASM_PROXY_BITCOIN_RPC_URL="${WASM_PROXY_BITCOIN_RPC_URL:-http://127.0.0.1:19443}"
export WASM_PROXY_BITCOIN_RPC_USER="${WASM_PROXY_BITCOIN_RPC_USER:-admin}"
export WASM_PROXY_BITCOIN_RPC_PASSWORD="${WASM_PROXY_BITCOIN_RPC_PASSWORD:-passw}"
export WASM_PROXY_BITCOIN_RPC_WALLET="${WASM_PROXY_BITCOIN_RPC_WALLET:-bdk-test}"
export WASM_PROXY_REGTEST_COMPOSE_WORKDIR="${WASM_PROXY_REGTEST_COMPOSE_WORKDIR:-$ROOT}"
export WASM_PROXY_REGTEST_COMPOSE_FILE="${WASM_PROXY_REGTEST_COMPOSE_FILE:-bindings/wasm-sdk/compose.wasm-infra.yaml}"
export WASM_PROXY_REGTEST_ESPLORA_SERVICE="${WASM_PROXY_REGTEST_ESPLORA_SERVICE:-esplora}"
export WASM_PROXY_REGTEST_ESPLORA_RPC_USER="${WASM_PROXY_REGTEST_ESPLORA_RPC_USER:-admin}"
export WASM_PROXY_REGTEST_ESPLORA_RPC_PASSWORD="${WASM_PROXY_REGTEST_ESPLORA_RPC_PASSWORD:-passw}"
export WASM_PROXY_REGTEST_ESPLORA_WALLET="${WASM_PROXY_REGTEST_ESPLORA_WALLET:-bdk-test}"

./target/release/wasm-proxy-gateway >"$GATEWAY_LOG" 2>&1 &
GATEWAY_PID=$!

wait_http "http://127.0.0.1:3001/healthz" "wasm-proxy-gateway"

if [[ "$E2E_AUTO_PROVISION_REGULAR_RLN" != "1" ]]; then
  if ! curl -sf --max-time 2 http://127.0.0.1:3101/nodeinfo >/dev/null 2>&1; then
    echo "Regular RLN is not reachable on 3101. Export E2E_AUTO_PROVISION_REGULAR_RLN=1 for a fresh node, or start one manually (see bindings/wasm-sdk/e2e-specs/README.md)." >&2
    exit 1
  fi
fi

if [[ "$E2E_AUTO_PROVISION_REGULAR_RLN" == "1" ]]; then
  echo "  auto-provision regular RLN ($E2E_REGULAR_RLN_DATA_DIR)"
  assert_port_free 3101 "regular RLN REST"
  assert_port_free 9802 "regular RLN LN peer"
  if [[ "${E2E_KEEP_REGULAR_RLN_DATA:-0}" != "1" ]]; then
    rm -rf "$E2E_REGULAR_RLN_DATA_DIR"
  fi
  mkdir -p "$E2E_REGULAR_RLN_DATA_DIR"
  cargo build --release --bin rgb-lightning-node
  ./target/release/rgb-lightning-node "$E2E_REGULAR_RLN_DATA_DIR" \
    --daemon-listening-port 3101 \
    --ldk-peer-listening-port 9802 \
    --network regtest \
    --disable-authentication >"$RLN_LOG" 2>&1 &
  RLN_PID=$!
  wait_http "http://127.0.0.1:3101/nodeinfo" "regular-rln (nodeinfo)"

  nodeinfo_code="$(curl -sS -o /dev/null -w '%{http_code}' http://127.0.0.1:3101/nodeinfo || true)"
  nodeinfo_code="${nodeinfo_code:-000}"
  if [[ "$nodeinfo_code" != "200" ]]; then
    if ! curl -sf -X POST http://127.0.0.1:3101/init \
      -H 'content-type: application/json' \
      -d '{"password":"rln-password","mnemonic":"abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about"}'; then
      echo "(init skipped or failed — node may already be initialized)"
    fi

    curl -sS -f -X POST http://127.0.0.1:3101/unlock \
      -H 'content-type: application/json' \
      -d '{
        "password":"rln-password",
        "indexer_url":"http://127.0.0.1:3002",
        "proxy_endpoint":"rpc://127.0.0.1:3005/json-rpc",
        "announce_addresses":[]
      }'
    echo ""
  else
    echo "(regular RLN already unlocked; skipping init/unlock)"
  fi
fi

#############################################################################
# Stage 4 — wasm-build
#############################################################################

stage "stage 4 / wasm-build"

if [[ "$E2E_WASM_PACK_BUILD" == "1" ]] || [[ ! -f bindings/wasm-sdk/pkg/rln_wasm_sdk.js ]]; then
  echo "  wasm-pack build (wasm-sdk)"
  rustup target add wasm32-unknown-unknown
  if ! command -v wasm-pack >/dev/null 2>&1; then
    cargo install wasm-pack --locked
  fi
  (
    cd bindings/wasm-sdk
    no_opt_args=()
    if [[ "$E2E_WASM_PACK_NO_OPT" == "1" ]]; then
      no_opt_args+=(--no-opt)
    fi
    timeout "${E2E_WASM_PACK_TIMEOUT_SEC}s" \
      wasm-pack build --target web --release --out-dir pkg --mode "$E2E_WASM_PACK_MODE" "${no_opt_args[@]}"
  )
else
  echo "  wasm-pack build skipped (pkg/rln_wasm_sdk.js already exists)"
fi

#############################################################################
# Stage 5 — playwright
#############################################################################

stage "stage 5 / playwright"

cd "$ROOT/bindings/wasm-sdk/e2e-specs"
if [[ "$E2E_NPM_CI" == "1" ]]; then
  npm ci
fi
if [[ "$E2E_PLAYWRIGHT_INSTALL" == "1" ]]; then
  npx playwright install chromium
fi

PLAYWRIGHT_ARGS=()
if [[ -n "${E2E_PLAYWRIGHT_GREP:-}" ]]; then
  PLAYWRIGHT_ARGS+=("-g" "${E2E_PLAYWRIGHT_GREP}")
fi
if [[ -n "${E2E_PLAYWRIGHT_TESTS:-}" ]]; then
  # shellcheck disable=SC2206
  PLAYWRIGHT_ARGS+=(${E2E_PLAYWRIGHT_TESTS})
elif [[ "$E2E_SUITE" == "smoke" ]]; then
  PLAYWRIGHT_ARGS+=("wasm_regular_rln_native_channel.spec.js")
fi

if [[ "${#PLAYWRIGHT_ARGS[@]}" -gt 0 ]]; then
  npx playwright test --config=playwright.wasm_regular_rln.config.js "${PLAYWRIGHT_ARGS[@]}"
else
  npm run test:wasm-regular-rln-e2e
fi

cd "$ROOT"

#############################################################################
# Stage 6 — infra-down (handled by trap cleanup)
#############################################################################

stage "stage 6 / done"
echo "logs in: $RUN_DIR"
