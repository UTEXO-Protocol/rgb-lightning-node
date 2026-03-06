#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT_DIR"

OUT_DIR="target/uniffi/kotlin-android/jniLibs"
rm -rf "$OUT_DIR"
cargo ndk -t arm64-v8a -o "$OUT_DIR" build --release --features uniffi --lib
cargo ndk -t armeabi-v7a -o "$OUT_DIR" build --release --features uniffi --lib
cargo ndk -t x86_64 -o "$OUT_DIR" build --release --features uniffi --lib

echo "Built Android JNI libs in $OUT_DIR"
