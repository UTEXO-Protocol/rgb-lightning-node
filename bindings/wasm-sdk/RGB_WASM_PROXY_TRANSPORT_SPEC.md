# RGB WASM Proxy Transport Spec (Step 1)

## Purpose

This document freezes the architecture boundary for porting RGB business logic
to native wasm runtime while routing RGB network transport through a proxy
service colocated with LN websocket proxying.

This is the Step 1 scope/contract baseline for the RGB-native migration track.

## Scope Boundary

### In scope (must become wasm-native)

1. RGB transfer orchestration and lifecycle state transitions in wasm runtime.
2. RGB runtime persistence and recovery in browser storage.
3. Deterministic wasm API contracts for RGB operations and status surfaces.
4. LN<->RGB status synchronization in wasm runtime observers.

### Out of scope (external infra still required)

1. RGB network transport service itself (provided by proxy upstream).
2. Bitcoin chain source/indexer (Esplora).
3. Funding/signing provider integration for on-chain tx flows.

## Proxy Contract (Frozen for Step 1)

Browser-facing routes (example):

1. `GET/POST /ln/*` for LN websocket bridge behavior.
2. `POST /rgb/json-rpc` for RGB JSON-RPC pass-through.

Transport contract requirements:

1. JSON-RPC request/response payloads are forwarded without semantic mutation.
2. Stable HTTP status and JSON error envelope mapping.
3. Per-session auth binding (`token`, `node_id`) enforced at proxy boundary.
4. Tenant isolation: requests for one session/node cannot access another session.
5. Request size/time limits and rate limiting are enforced.

## Wasm Runtime Contract (Frozen for Step 1)

1. Wasm runtime is authoritative for RGB transfer state machine.
2. Proxy is transport-only; business decisions are not delegated to proxy.
3. Runtime state persistence uses browser-backed storage with deterministic
   recovery semantics.
4. Error contracts returned by wasm APIs remain deterministic and documented in
   `ERROR_CONTRACT.md`.

## Step 1 Deliverables

1. This boundary/spec document.
2. README reference to this document.
3. No behavior changes are required in Step 1.

## Exit Criteria for Step 1

1. Team agrees on responsibility split (wasm runtime vs proxy).
2. No ambiguous ownership for transport, state, or auth/session logic.
3. Follow-up implementation steps can proceed without redefining boundaries.
