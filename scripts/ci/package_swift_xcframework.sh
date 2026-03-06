#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT_DIR"

SWIFT_DIR="target/uniffi/swift"
mkdir -p "$SWIFT_DIR/headers"
cp "$SWIFT_DIR/RGBLightningNodeFFI.h" "$SWIFT_DIR/headers/"
cp "$SWIFT_DIR/RGBLightningNodeFFI.modulemap" "$SWIFT_DIR/headers/module.modulemap"

xcodebuild -create-xcframework \
  -library target/aarch64-apple-ios/release/librgb_lightning_node.a -headers "$SWIFT_DIR/headers" \
  -library target/aarch64-apple-ios-sim/release/librgb_lightning_node.a -headers "$SWIFT_DIR/headers" \
  -library target/x86_64-apple-ios/release/librgb_lightning_node.a -headers "$SWIFT_DIR/headers" \
  -output "$SWIFT_DIR/RGBLightningNode.xcframework"

echo "Packaged $SWIFT_DIR/RGBLightningNode.xcframework"
