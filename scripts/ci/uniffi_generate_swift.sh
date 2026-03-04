#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT_DIR"

cargo run --manifest-path bindings/uniffi-bindgen/Cargo.toml -- \
  generate bindings/rgb_lightning_node.udl \
  --language swift \
  --config uniffi.toml \
  -o bindings/swift

echo "Generated Swift bindings in bindings/swift"
