# Mutiny-Node Reference for RLN LDK-on-WASM

This document maps proven `mutiny-node` wasm patterns to `rgb-lightning-node` (RLN) so we can port LDK-dependent SDK methods in deterministic phases.

## What Mutiny Actually Does

Mutiny does not run native TCP in browser wasm. It keeps LDK and replaces transport/runtime edges:

1. `mutiny-core` holds core wallet/LDK logic.
2. `mutiny-wasm` provides wasm-bindgen API and browser storage wiring.
3. On wasm, peer transport is websocket proxy + custom `SocketDescriptor`.
4. Storage is abstracted (IndexedDB + optional remote sync), not native filesystem.

Key files:

1. `mutiny-core/src/networking/socket.rs`
2. `mutiny-core/src/networking/ws_socket.rs`
3. `mutiny-core/src/networking/proxy.rs`
4. `mutiny-core/src/peermanager.rs`
5. `mutiny-core/src/node.rs`
6. `mutiny-core/src/nodemanager.rs`
7. `mutiny-wasm/src/lib.rs`

## Patterns We Should Reuse in RLN

1. Keep LDK core logic in shared/core crate layer.
2. Put browser-only glue in wasm crate/bindings layer.
3. Introduce wasm-only peer transport adapter:
   - `Proxy` trait (`send/read/close`)
   - websocket implementation (`gloo-net`)
   - `SocketDescriptor` adapter that feeds bytes into `PeerManager`.
4. Keep startup builder target-aware:
   - native path uses current TCP stack
   - wasm path requires websocket proxy endpoint and wasm storage/runtime.
5. Keep explicit capability contract for unsupported features.

## RLN Delta (Current State)

1. Root RLN native SDK (`src/sdk/mod.rs`, `src/sdk/ldk.rs`) is native-only and tightly coupled to filesystem/Tokio/TCP assumptions.
2. wasm entry (`src/sdk/wasm.rs`) is a boundary scaffold and does not run LDK runtime.
3. `bindings/wasm-sdk` currently provides real RGB wallet flows through `rgb-lib-wasm`, but not LDK peer/channel runtime.

## RLN Port Tasks (LDK Methods)

### Phase A: Isolate runtime interfaces in RLN

1. Split LDK runtime edges behind traits/modules:
   - peer transport
   - persistence backend
   - timer/spawn glue
2. Move native-only wiring into a `native` implementation module.
3. Keep business logic independent from concrete transport.

### Phase B: Add wasm LDK transport adapter

1. Implement websocket proxy client for browser (equivalent to Mutiny `WsProxy`).
2. Implement wasm `SocketDescriptor` wrapper.
3. Add descriptor read loop feeding `peer_manager.read_event/process_events`.

### Phase C: Add wasm persistence adapter

1. Replace direct filesystem persistence assumptions with interface.
2. Provide wasm backend (IndexedDB) for required LDK state blobs.
3. Keep serialization contract identical between native and wasm where possible.

### Phase D: Enable wasm Node startup path

1. Add wasm startup config with `websocket_proxy_endpoint`.
2. Build node manager with wasm adapters.
3. Implement `connect_peer`/`disconnect_peer` on wasm.

### Phase E: Port LDK-dependent SDK methods

Prioritize in order:

1. `connect_peer`, `disconnect_peer`
2. `list_peers`, `list_channels`, `node_info`
3. channel open/close
4. LN send/keysend + invoice flows

### Phase F: Browser integration tests

1. `wasm-bindgen-test` for transport and state persistence.
2. local proxy-based integration scenario.
3. contract tests matching native API response shapes.

## Acceptance for “LDK ported to wasm”

We can claim LDK wasm support only when:

1. Peer connections work over websocket proxy in browser.
2. Channel lifecycle endpoints run on wasm runtime.
3. LN payment path works (invoice pay + receive).
4. Restart persistence recovers channel manager/monitors in wasm storage.
