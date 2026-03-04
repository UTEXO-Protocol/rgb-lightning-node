#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT_DIR"

rm -rf bindings/kotlin-android/jniLibs
cargo ndk -t arm64-v8a -o bindings/kotlin-android/jniLibs build --release --features uniffi --lib
cargo ndk -t armeabi-v7a -o bindings/kotlin-android/jniLibs build --release --features uniffi --lib
cargo ndk -t x86_64 -o bindings/kotlin-android/jniLibs build --release --features uniffi --lib

echo "Built Android JNI libs in bindings/kotlin-android/jniLibs"
