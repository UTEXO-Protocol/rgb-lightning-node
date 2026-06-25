# SDK -> WASM Flow Reference

This document describes how the main SDK flows are implemented in wasm, with explicit notes on proxy usage and runtime behavior.

## Scope

- Runtime mode: single production runtime, `wasm_native_ldk`.
- Environment: browser-first (`wasm-bindgen`), persistent state via `localStorage` + `IndexedDB`.
- Event model: runtime/event-stream authoritative for LN state transitions.

## High-Level Mapping (Native SDK vs WASM)

| Flow | Native SDK (host) | WASM port (browser) |
|---|---|---|
| Node runtime bootstrap | Host process/runtime context | `RlnWasmNode` + `NativeLnRuntimeCore` + `LdkRuntimeManager` (`wasm_native_ldk`) |
| Peer transport | Native socket/runtime transport | WebSocket via configured proxy URL (`connectPeer`) |
| Open/close channels | SDK runtime APIs | Same API shape, channel state driven by runtime transport events |
| Virtual channels | Trusted-no-broadcast lifecycle in runtime | `trusted_no_broadcast` with persisted draft/session/scope markers and cleanup guards |
| RGB transport | SDK transport endpoint model | Wallet-scoped/default proxy transport endpoint resolution (`setRgbProxyTransport`) |
| Chain sync | Host chain services | `WasmChainSyncDriver` polling Esplora/indexer + rebroadcast queue |
| Persistence/recovery | Host storage/runtime snapshots | Browser-backed snapshots and event log recovery keyed by runtime scope |

## 1) Opening Channels (via current proxy)

### Current wasm behavior

1. App calls `connectPeer(peer_addr, peer_pubkey)`.
2. WASM opens a WebSocket using the node proxy URL + peer address.
3. Peer is marked started in runtime state.
4. App calls `openChannel*` / `openChannel*WithOptions`.
5. Node validates capacity/public/asset constraints and peer-connected preconditions.
6. Node creates runtime channel state and applies canonical transport event transitions.
7. Runtime event log and channel state are persisted in browser storage.

### Important details

- Proxy transport path is browser-safe and explicit; no hidden localhost daemon assumptions in production paths.
- Open flow is event-driven: channel usability/readiness is derived from transport/runtime events.
- API remains object-handle based (`RlnWasmSdk` -> `RlnWasmNode`).

### Relevant implementation

- `connectPeer`: [ln_node.rs](bindings/wasm-sdk/src/ln_node.rs:609)
- Open channel with options: [ln_node.rs](bindings/wasm-sdk/src/ln_node.rs:2155)
- Proxy websocket connect/backoff: [ln_transport.rs](bindings/wasm-sdk/src/ln_transport.rs:253)

## 2) Virtual Channels Logic (`trusted_no_broadcast`)

### Current wasm behavior

Virtual channels are supported through `openChannel*WithOptions(..., virtual_open_mode="trusted_no_broadcast")`.

Core lifecycle:

1. Reserve draft intent (`virtual_channel_add_intent`).
2. Create channel/session mapping (`virtual_channel_session_add_from_open`).
3. Register trusted virtual scope/link markers.
4. Queue/apply runtime transport events for channel usability.
5. On close, require peer/session consistency and enforce cleanup guards.
6. Transition session status (`active -> abandon_pending -> abandoned`) and clear scope markers.

### Cleanup guard behavior

WASM enforces conservative guards before trusted virtual cleanup:

- No in-flight HTLC lifecycle states that block cleanup.
- Counterparty BTC floor checks.
- Counterparty RGB floor checks.

### Relevant implementation

- Virtual open validation + session creation: [ln_node.rs](bindings/wasm-sdk/src/ln_node.rs:2173)
- Virtual close path and guards: [ln_node.rs](bindings/wasm-sdk/src/ln_node.rs:2396)
- Guard checks: [ln_node.rs](bindings/wasm-sdk/src/ln_node.rs:2576)
- Scope/link tracking: [ln_node.rs](bindings/wasm-sdk/src/ln_node.rs:2964)

## 3) RGB Logic (via proxy transport endpoints)

### Current wasm behavior

RGB wallet operations use transport endpoints, with explicit proxy config support:

1. Configure wallet-scoped transport with `setRgbProxyTransport(endpoint, auth_token?, node_id?)`.
2. If a wallet has no explicit config, fallback to SDK default transport config.
3. `blindReceive*` / `witnessReceive*` resolve effective transport endpoints.
4. RGB operations execute through the resolved proxy HTTP endpoint(s).

### Notes

- Wallet transport config is persisted and restored from browser storage.
- Auth token + node id are optional but coupled (must be provided together).
- This keeps parity with endpoint-driven RGB transport while being browser-native.

### Relevant implementation

- Wallet proxy config validation/resolution: [lib.rs](bindings/wasm-sdk/src/lib.rs:3143)
- Resolve default/wallet transport endpoints: [lib.rs](bindings/wasm-sdk/src/lib.rs:3227)
- Wallet API surface (`setRgbProxyTransport`, etc.): [lib.rs](bindings/wasm-sdk/src/lib.rs:3244)

## 4) Chain Sync (critical runtime infrastructure)

### Current wasm behavior

`WasmChainSyncDriver` handles chain-sync state in-browser:

1. `chainSyncStart(indexer_url, poll_interval_ms?)` starts polling.
2. Poll loop fetches chain tip / tx states from configured indexer.
3. Rebroadcast queue tracks pending/confirmed tx rebroadcast attempts.
4. Snapshot state persists after updates for recovery on reload.
5. `chainSyncTick` supports explicit/manual tick flow.

### Notes

- Designed for browser constraints: async loop + durable lightweight snapshot state.
- Supports tip regression tracking and error surfaces in status output.

### Relevant implementation

- Driver model/status/snapshot: [chain_sync.rs](bindings/wasm-sdk/src/chain_sync.rs:21)
- Node chain sync API passthrough: [ln_node.rs](bindings/wasm-sdk/src/ln_node.rs:883)

## 5) Runtime Event / Payment State Model

### Current wasm behavior

- LN/payment/channel state is kept runtime-authoritative.
- Node APIs emit/apply canonical transport and payment-status events.
- Runtime event log and transfer projections are persisted with bounded snapshots.
- Native runtime queue (`NativeLnRuntimeCore`) allows deterministic drain/replay-style processing.

### Relevant implementation

- Runtime queue APIs: [ln_node.rs](bindings/wasm-sdk/src/ln_node.rs:1025)
- Runtime event listing + persistence hooks: [ln_node.rs](bindings/wasm-sdk/src/ln_node.rs:1159)
- Payment status via event stream: [ln_node.rs](bindings/wasm-sdk/src/ln_node.rs:3143)

## 6) Strictness choices in wasm parity

The wasm implementation intentionally omits broad backward-compat paths in production:

- Single runtime backend model (`wasm_native_ldk`) for exported APIs.
- Canonical runtime transport/payment event parsing only.
- No legacy snapshot/global swap restore fallback in production paths.

This keeps one clean working version and reduces ambiguity between SDK and wasm behavior.

