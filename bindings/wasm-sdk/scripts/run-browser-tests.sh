#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CRATE_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"
cd "${CRATE_DIR}"

WASM_TEST_BROWSER="${WASM_TEST_BROWSER:-chrome}"

case "${WASM_TEST_BROWSER}" in
  firefox)
    # Prefer non-snap geckodriver in CI/local to avoid permission edge cases.
    if [ -x /usr/local/bin/geckodriver ]; then
      export PATH="/usr/local/bin:${PATH}"
    fi
    if command -v geckodriver >/dev/null 2>&1; then
      echo "Using geckodriver: $(command -v geckodriver)"
      geckodriver --version | head -n 1 || true
    else
      echo "geckodriver not found in PATH (required for firefox mode)" >&2
      exit 1
    fi
    TEST_ARGS=(--firefox)
    ;;
  chrome)
    if ! command -v google-chrome >/dev/null 2>&1 && ! command -v chromium >/dev/null 2>&1 && ! command -v chromium-browser >/dev/null 2>&1; then
      echo "chrome/chromium not found in PATH (required for chrome mode)" >&2
      exit 1
    fi
    TEST_ARGS=(--chrome)
    ;;
  *)
    echo "Unsupported WASM_TEST_BROWSER='${WASM_TEST_BROWSER}', expected 'chrome' or 'firefox'" >&2
    exit 1
    ;;
esac

: "${WASM_BINDGEN_TEST_TIMEOUT:=300}"
export WASM_BINDGEN_TEST_TIMEOUT

: "${WASM_BROWSER_TEST_LOG:=${CRATE_DIR}/target/wasm-browser-tests.log}"
mkdir -p "$(dirname "${WASM_BROWSER_TEST_LOG}")"
LOG_FILE="${WASM_BROWSER_TEST_LOG}"
echo "Logging wasm browser test output to: ${LOG_FILE}"

set +e
wasm-pack test --headless "${TEST_ARGS[@]}" 2>&1 | tee "${LOG_FILE}"
STATUS=${PIPESTATUS[0]}
set -e

exit "${STATUS}"
