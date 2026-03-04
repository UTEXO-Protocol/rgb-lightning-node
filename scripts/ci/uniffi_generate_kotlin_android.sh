#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT_DIR"

cargo run --manifest-path bindings/uniffi-bindgen/Cargo.toml -- \
  generate bindings/rgb_lightning_node.udl \
  --language kotlin \
  --config uniffi-android.toml \
  -o bindings/kotlin-android

echo "Generated Kotlin Android bindings in bindings/kotlin-android"
