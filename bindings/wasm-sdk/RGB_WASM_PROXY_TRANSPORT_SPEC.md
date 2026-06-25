# RGB WASM Proxy Transport

## Purpose

This document describes the architecture boundary: RGB business logic runs in
the native wasm runtime, while RGB network transport is routed through a proxy
service colocated with LN websocket proxying.

## Scope Boundary

### Native in the wasm runtime

1. RGB transfer orchestration and lifecycle state transitions.
2. RGB runtime persistence and recovery in browser storage.
3. Deterministic wasm API contracts for RGB operations and status surfaces.
4. LN<->RGB status synchronization in wasm runtime observers.

### External infra required

1. RGB network transport service itself (provided by proxy upstream).
2. Bitcoin chain source/indexer (Esplora).
3. Funding/signing provider integration for on-chain tx flows.

## Proxy Contract

Browser-facing routes (example):

1. `GET/POST /ln/*` for LN websocket bridge behavior.
2. `POST /rgb/json-rpc` for RGB JSON-RPC pass-through.

Transport contract requirements:

1. JSON-RPC request/response payloads are forwarded without semantic mutation.
2. Stable HTTP status and JSON error envelope mapping.
3. Per-session auth binding (`token`, `node_id`) enforced at proxy boundary.
4. Tenant isolation: requests for one session/node cannot access another session.
5. Request size/time limits and rate limiting are enforced.

## Wasm Runtime Contract

1. The wasm runtime is authoritative for the RGB transfer state machine.
2. The proxy is transport-only; business decisions are not delegated to it.
3. Runtime state persistence uses browser-backed storage with deterministic
   recovery semantics.
4. Error contracts returned by wasm APIs are deterministic and documented in
   `ERROR_CONTRACT.md`.

## Responsibility split

The wasm runtime owns RGB transfer state, persistence, and auth/session logic;
the proxy owns only transport. There is no shared ownership of transport, state,
or auth/session logic across the boundary.

## RGB on the LN peer link (not the HTTP proxy)

The HTTP/RGB JSON-RPC proxy is **transport-only** for RGB consignment/indexer-style calls.

RGB-aware **LN peer bytes** in this repository are carried inside **channel messages** parsed by
`ChannelManager` (forked `lightning::ln::msgs` + `rgb_utils`). The wasm runtime also includes a
small **BOLT #1 experimental** custom-message handler for fork capability discovery (`type`
**45001**; see `bindings/wasm-sdk/src/rgb_ln_wire.rs`).
