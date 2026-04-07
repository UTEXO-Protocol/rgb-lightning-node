# Full LDK Port Plan (WASM)

Goal: replace modeled LN behavior in `bindings/wasm-sdk/src/ln_node.rs` with a real wasm LDK runtime integration (Mutiny-style architecture), then progressively convert scaffolded LN methods to real logic.

## Scope

In scope:
1. LDK runtime lifecycle in wasm (`start/restore/stop`) with browser persistence.
2. Real peer/channel/payment/invoice state from LDK runtime.
3. RLN SDK method parity migration from scaffolded LN methods to runtime-backed methods.

Out of scope (until upstream support exists):
1. UDA issuance via `rgb-lib-wasm` (no exposed primitive yet).
2. Native-only filesystem assumptions from RLN root runtime.

## Current Baseline

Current node behavior in wasm:
1. Websocket/peer/channel/payment are modeled in-memory.
2. Wallet/BTC/RGB operations are partly runtime-backed through `rgb-lib-wasm`.
3. Lifecycle methods (`init/unlock/lock`) are explicit unsupported contracts.

## Execution Phases

### Phase 0: Runtime Design Freeze
Status: in_progress.

Deliverables:
1. Finalize browser persistence model for LN runtime state (IndexedDB keying/versioning).
2. Define key ownership and derivation strategy in wasm for invoice signing + node identity.
3. Define event bridge contract between transport and LDK state machine.

Exit criteria:
1. Single approved runtime architecture doc and interfaces.

### Phase 1: LDK Core Bootstrap
Status: in_progress.

Deliverables:
1. Real wasm LDK runtime bootstrap module (`start/restore/stop`).
2. Deterministic persistence/restore tests across page reload semantics.
3. Runtime health endpoint showing real LDK state readiness.
4. Implemented bootstrap seam:
   - `src/ldk_runtime.rs` with runtime manager abstraction + scaffold manager
   - `RlnWasmNode` runtime status endpoint + readiness hooks in LN operations.
5. Implemented scaffold lifecycle orchestration:
   - explicit `restore/start/stop` manager contract
   - sdk `init/unlock/lock` moved from unsupported stubs to deterministic scaffold flow.
6. Added runtime snapshot persistence seam:
   - scaffold storage keyed by node runtime identity
   - deterministic restart semantics (`running` on first start, `running_restored` after stop/restart).
7. Added browser-backed persistence adapter:
   - runtime snapshots persisted via `localStorage` when available
   - in-memory fallback retained for constrained/test runtimes.
8. Linked sdk lifecycle with node runtime accessibility:
   - sdk lock state now gates sdk-managed node runtime operations and node-handle creation.

Exit criteria:
1. LDK runtime starts in wasm and survives restore with stable node id.

### Phase 2: Real Peer/Channel Path
Status: planned.

Deliverables:
1. Replace modeled peer/channel store with runtime-backed data.
2. Promote methods:
   - `connect_peer`, `disconnect_peer`
   - `list_peers`, `list_channels`, `get_channel_id`, `open_channel`, `close_channel`
3. Runtime-backed contract tests for peer/channel state transitions.

Exit criteria:
1. Scaffolded peer/channel methods removed or switched to real runtime path.

### Phase 3: Real Payment/Invoice Path
Status: planned.

Deliverables:
1. Promote methods:
   - `send_payment`, `keysend`
   - `list_payments`, `get_payment`, `invoice_status`
   - `create_ln_invoice`, `decode_ln_invoice` (already parse-backed; align full contract)
2. Hook/event flow consumes real LDK events, not modeled status updates.
3. Runtime-backed invoice/payment tests with deterministic fixtures.

Exit criteria:
1. LN payment/invoice methods are runtime-backed and no longer modeled.

### Phase 4: Lifecycle + Signing
Status: planned.

Deliverables:
1. Replace unsupported:
   - `unlock`, `lock`
   - `sign_message`
2. Move from scaffolded lifecycle to real runtime lifecycle orchestration.

Exit criteria:
1. Unsupported lifecycle/signing scaffolds removed.

### Phase 5: Higher-Level Integrations
Status: planned.

Deliverables:
1. Revisit swap and onion methods:
   - `maker_init`, `maker_execute`, `taker`
   - `get_swap`, `list_swaps`
   - `send_onion_message`
2. Either runtime-backed implementations or explicit documented non-goals.

Exit criteria:
1. Remaining scaffold methods are either runtime-backed or explicitly deferred with rationale.

## Method Migration Order

Priority order after Phase 1:
1. Peer/channel lifecycle methods.
2. Invoice creation + payment state methods.
3. Lifecycle/signing methods.
4. Swap/onion methods.

## Risks

1. Browser transport reliability and reconnection behavior.
2. State restore consistency across runtime/schema versions.
3. Key management and secure persistence in browser context.

## Tracking

Use:
1. `SDK_WASM_ENDPOINT_MATRIX.md` for endpoint status (`runtime-backed` / `scaffolded`).
2. `SDK_WASM_PARITY_PLAN.md` for incremental execution log.
