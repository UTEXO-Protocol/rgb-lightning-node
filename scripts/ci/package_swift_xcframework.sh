#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT_DIR"

mkdir -p bindings/swift/headers
cp bindings/swift/RGBLightningNodeFFI.h bindings/swift/headers/
cp bindings/swift/RGBLightningNodeFFI.modulemap bindings/swift/headers/module.modulemap

xcodebuild -create-xcframework \
  -library target/aarch64-apple-ios/release/librgb_lightning_node.a -headers bindings/swift/headers \
  -library target/aarch64-apple-ios-sim/release/librgb_lightning_node.a -headers bindings/swift/headers \
  -library target/x86_64-apple-ios/release/librgb_lightning_node.a -headers bindings/swift/headers \
  -output bindings/swift/RGBLightningNode.xcframework

echo "Packaged bindings/swift/RGBLightningNode.xcframework"
