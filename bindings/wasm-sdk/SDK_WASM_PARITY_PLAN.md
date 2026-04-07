# SDK to WASM Parity Plan

This plan tracks parity between native `src/sdk/mod.rs` and `bindings/wasm-sdk`.

## Parity Snapshot

Native SDK functions discovered: 51 (`pub(crate) async fn` endpoints in `src/sdk/mod.rs`).

Current wasm coverage split:
1. `RlnWasmWallet` covers most RGB wallet/data flows with real `rgb-lib-wasm` runtime.
2. `RlnWasmNode` covers LN websocket peer/session/channel/payment scaffolding.
3. `RlnWasmNode` currently uses modeled state for LDK-dependent lifecycle and settlement.

## Phase Plan

### Phase A: Inventory and API Mapping
Status: done.

1. Keep a stable mapping table from native SDK functions to wasm counterparts.
2. Tag each endpoint as `runtime-backed`, `scaffolded`, or `missing`.

### Phase B: Close Low-Risk Missing Endpoints
Status: in_progress.

1. Add decode/helper endpoints that can be backed by existing wasm libs.
2. Add explicit contract tests for newly added endpoints.
3. Keep stable error contracts for unsupported inputs.

### Phase C: Unify SDK Entry Surface
Status: in_progress.

1. Introduce a single wasm SDK facade object that composes wallet + node paths.
2. Keep current objects (`RlnWasmWallet`, `RlnWasmNode`) as lower-level APIs.
3. Mirror native request/response naming where feasible.

Progress:
1. Added `RlnWasmSdk` facade with `newWallet/createWallet/newNode` helpers and runtime metadata methods.
2. Added first facade forwarding set for high-frequency node views:
   - `nodeInfo`
   - `listPayments`
   - `decodeLnInvoice`
   - `decodeRgbInvoice`
3. Added wallet read forwarding set:
   - `walletGetAddress`
   - `walletGetBtcBalance`
   - `walletListTransactions`
   - `walletListAssets`
4. Added node write-path forwarding set:
   - `connectPeer` / `disconnectPeer`
   - `openChannel`
   - `sendPayment`
   - `keysend`
5. Added remaining integration-forwarders for:
   - peer/channel listing and channel-id helpers
   - invoice status and payment status update APIs
   - read-event ingestion and auto-hook controls
6. Added initial stateful facade handle pattern:
   - `RlnWasmSdkNodeHandle`
   - `RlnWasmSdkWalletHandle`
7. Expanded `RlnWasmSdkNodeHandle` coverage for peer/channel/payment write and status/update methods.
8. Expanded `RlnWasmSdkWalletHandle` coverage for online/write and backup/VSS methods.
9. Added deterministic handle-only async wallet contract tests for:
   - `goOnline` (empty indexer URL)
   - `refresh` (empty asset_id when provided)
   - `sendBtcBegin` (empty address)
10. Added deterministic handle-only wallet success-path contract tests for:
   - `walletGetAddress`
   - `walletListTransactions`
   - `walletListAssets`
11. Validation run completed:
   - `bindings/wasm-sdk`: `cargo fmt --all`
   - `bindings/wasm-sdk`: `cargo check --target wasm32-unknown-unknown`
   - `bindings/wasm-sdk`: `cargo test --target wasm32-unknown-unknown --no-run`
   - root crate: `cargo check --target wasm32-unknown-unknown`
12. Added decode RGB invoice contract tests across all exposed surfaces:
   - `RlnWasmNode::decodeRgbInvoiceJson` empty-input contract
   - `RlnWasmSdk::decodeRgbInvoiceJson` empty-input contract
   - `RlnWasmSdkNodeHandle::decodeRgbInvoiceJson` empty-input contract
13. Added deterministic async success-path tests for:
   - `RlnWasmSdk::createWalletHandleAsync` + `RlnWasmSdkWalletHandle::getAddress`
   - `RlnWasmSdk::createWalletHandleAsync` + `RlnWasmSdkWalletHandle::listTransactionsValue`
14. Added `getPaymentValue/Json` forwarding on:
   - `RlnWasmSdk`
   - `RlnWasmSdkNodeHandle`
15. Added explicit endpoint matrix document:
   - `bindings/wasm-sdk/SDK_WASM_ENDPOINT_MATRIX.md`
16. Added `checkIndexerUrlValue/Json` helper endpoints with deterministic validation:
   - validates network argument
   - validates URL scheme and maps supported wasm protocol to `esplora`
   - returns explicit unsupported error for electrum (`tcp://` / `ssl://`) in wasm build
17. Added wasm contract tests for `checkIndexerUrl`:
   - success (`https://...` => `esplora`)
   - JSON parity success (`Value`/`Json`)
   - invalid network error
   - empty-url error
   - unsupported-electrum error
18. Added `networkInfoValue/Json` scaffold endpoint:
   - available on `RlnWasmNode`, `RlnWasmSdk`, and `RlnWasmSdkNodeHandle`
   - deterministic scaffold output (`network=regtest`, `height=0`)
19. Added contract tests for facade and node-handle network info forwarding.
20. Added `signMessageValue/Json` endpoint surface:
   - available on `RlnWasmNode`, `RlnWasmSdk`, and `RlnWasmSdkNodeHandle`
   - explicit stable unsupported error contract in wasm scaffold
21. Added contract tests for `signMessage` unsupported behavior (facade + node-handle).
22. Added `getAssetMediaValue/Json` endpoint surface:
   - available on `RlnWasmWallet`, `RlnWasmSdk`, and `RlnWasmSdkWalletHandle`
   - explicit stable unsupported error contract in wasm scaffold
23. Added contract tests for `getAssetMedia`:
   - empty `asset_id` validation
   - unsupported behavior on wallet-handle and facade forwarding
24. Added `createLnInvoiceValue/Json` endpoint surface:
   - available on `RlnWasmNode`, `RlnWasmSdk`, and `RlnWasmSdkNodeHandle`
   - explicit stable unsupported error contract in wasm scaffold
25. Added contract tests for `createLnInvoice` unsupported behavior (facade + node-handle).
26. Added lifecycle endpoint surface on `RlnWasmSdk`:
   - `initValue` / `initJson`
   - `unlock`
   - `lock`
   - all with explicit stable unsupported contracts in wasm scaffold
27. Added contract tests for lifecycle unsupported behavior (`init`, `unlock`, `lock`).
28. Added `sendRgbFromGroupsValue/Json` endpoint surface with explicit unsupported contract.
29. Added swap/onion endpoint surfaces on `RlnWasmSdk` with explicit unsupported contracts:
   - `makerInitValue/Json`
   - `makerExecuteValue/Json`
   - `taker`
   - `getSwapValue/Json`
   - `listSwapsValue/Json`
   - `sendOnionMessage`
30. Added contract tests for `send_rgb_from_groups`, swap, and onion unsupported behavior.
31. Added issuance/media endpoint surfaces on `RlnWasmSdk` with explicit unsupported contracts:
   - `issueAssetNiaValue/Json`
   - `issueAssetCfaValue/Json`
   - `issueAssetUdaValue/Json`
   - `postAssetMediaValue/Json`
32. Added contract tests for issuance/media unsupported behavior.
33. Upgraded `issue_asset_nia` with runtime-backed wallet logic:
   - `RlnWasmWallet::issueAssetNiaValue/Json`
   - `RlnWasmSdk::walletIssueAssetNiaValue/Json`
   - `RlnWasmSdkWalletHandle::issueAssetNiaValue/Json`
34. Added contract tests for NIA issuance request validation on wallet-handle surface.
35. Upgraded `issue_asset_cfa` with runtime-backed wallet compatibility logic:
   - `RlnWasmWallet::issueAssetCfaValue/Json`
   - `RlnWasmSdk::walletIssueAssetCfaValue/Json`
   - `RlnWasmSdkWalletHandle::issueAssetCfaValue/Json`
   - mapped through `rgb-lib-wasm` `Wallet::issue_asset_ifa` and normalized CFA-like response
36. Added contract tests for CFA issuance request validation on wallet-handle surface.
37. Expanded `issue_asset_uda` wallet/facade/handle surfaces:
   - `RlnWasmWallet::issueAssetUdaValue/Json`
   - `RlnWasmSdk::walletIssueAssetUdaValue/Json`
   - `RlnWasmSdkWalletHandle::issueAssetUdaValue/Json`
   - explicit unsupported contract citing missing `rgb-lib-wasm` UDA issuance primitive
38. Added UDA contract tests for request validation and unsupported behavior.
39. Upgraded `send_rgb_from_groups` with runtime-backed wallet adapter logic:
   - `RlnWasmWallet::sendRgbFromGroupsValue/Json`
   - `RlnWasmSdk::walletSendRgbFromGroupsValue/Json`
   - `RlnWasmSdkWalletHandle::sendRgbFromGroupsValue/Json`
   - implemented via `send_begin` orchestration from grouped recipients
40. Added grouped-send contract tests for request validation and empty-group behavior.
41. Next: add broader handle-only async success flow tests (`goOnline` + `walletSync` + payment path) using deterministic fixtures.
42. Added dedicated full LDK wasm migration roadmap:
   - `bindings/wasm-sdk/LDK_FULL_PORT_PLAN.md`
   - phased execution from runtime architecture freeze through lifecycle/signing and higher-level integrations
43. Promoted `decode_ln_invoice` to runtime-backed status:
   - explicit empty-input contract (`invoice cannot be empty`)
   - retained deterministic invalid invoice contract (`invalid invoice: ...`)
   - added empty-input contract tests for node, facade, and node-handle surfaces.
44. Started Phase 1 LDK bootstrap scaffolding in code:
   - added `src/ldk_runtime.rs` with `LdkRuntimeManager` abstraction and scaffold manager implementation.
   - integrated runtime manager into `RlnWasmNode` as an explicit seam for upcoming real wasm LDK runtime swap.
45. Added runtime status surfaces:
   - `RlnWasmNode::ldkRuntimeStatusValue/Json`
   - facade and node-handle forwarding on `RlnWasmSdk` / `RlnWasmSdkNodeHandle`
46. Added deterministic runtime status tests:
   - scaffold defaults (`backend=scaffold`, `state=cold`, `ready=false`)
   - transition to running state after first LN operation bootstrap (`keysend`).
47. Replaced lifecycle unsupported stubs on `RlnWasmSdk` with stateful scaffold logic:
   - `initValue/Json` now performs password/mnemonic bootstrap and returns `RlnWasmInitData`
   - `unlock` now validates request JSON, init precondition, and password
   - `lock` now performs deterministic lock transition with init precondition.
48. Added deterministic lifecycle contract tests:
   - `init` success returns mnemonic
   - `unlock` fails before init
   - `unlock + lock` success flow
   - invalid password and lock-before-init contracts.
49. Upgraded runtime manager orchestration contract:
   - added explicit `restore/start/stop` lifecycle in `LdkRuntimeManager`
   - `ensure_started` now performs `restore -> start` bootstrap path.
50. Added scaffold runtime snapshot storage and keyed restore behavior:
   - runtime snapshots persist in scaffold storage keyed by node runtime id (`proxy_url`-scoped)
   - first startup is `running`; restart after stop transitions to `running_restored`.
51. Added runtime restore contract tests:
   - updated cold->running status transition test for first startup
   - added stop/restart restore test proving `running_restored` state after persisted restart.
52. Hardened payment read/status methods behind runtime-managed contracts:
   - `list_payments` now enforces runtime bootstrap and deterministic sort (`created_at`, then `payment_hash`)
   - `get_payment` now enforces runtime bootstrap and trims hash input
   - `invoice_status` now enforces runtime bootstrap and explicit empty-invoice validation.
53. Added contract tests for payment read/status hardening:
   - trimmed `get_payment` hash acceptance
   - deterministic list ordering invariants
   - empty-invoice `invoice_status` contracts on node, facade, and node-handle surfaces.
54. Hardened event-driven payment settlement contracts:
   - `update_payment_status`, `update_payment_status_by_invoice`, and `ingest_read_event_payload_hex` now enforce runtime bootstrap.
   - explicit payment status transition guard added (`succeeded`/`expired` are terminal except idempotent updates).
   - event ingestion now surfaces invalid status errors directly instead of collapsing to generic not-found behavior.
55. Added settlement contract tests:
   - invalid event status returns explicit allowed-status error
   - terminal transition guard (`succeeded -> failed`) returns explicit transition error.
56. Replaced `create_ln_invoice` unsupported stub with runtime-managed scaffold implementation:
   - builds signed BOLT11 invoice (`lightning-invoice` builder) using deterministic wasm scaffold signing key
   - supports amount/expiry validation and asset pair validation
   - registers pending inbound payment entry for `invoice_status`/payment views consistency.
57. Added `create_ln_invoice` scaffold contract tests:
   - facade + node-handle success path (invoice created, decodable, status pending)
   - asset pair validation contract (`asset_id`/`asset_amount` together).
58. Replaced runtime in-memory-only storage with browser-backed adapter:
   - `localStorage` persistence for runtime snapshots (scoped key prefix)
   - transparent in-memory fallback retained for unavailable/unsupported browser storage contexts.
59. Kept deterministic restore semantics with new adapter:
   - first startup remains `running`
   - restart after persisted stop remains `running_restored`.
60. Connected sdk lifecycle state to node runtime access:
   - added shared runtime guard (`initialized && locked => deny node runtime operations`)
   - applied guard to sdk node creation (`newNode`, `createNodeHandle`) and runtime-started node methods.
61. Added lifecycle/runtime integration contract tests:
   - locked sdk blocks node-handle creation
   - lock blocks existing sdk-managed node runtime calls until unlock.
62. Added explicit runtime event bridge scaffolding:
   - unified event application path for manual ingestion and peer-hook ingestion
   - runtime event log with source/applied/error metadata and deterministic sequencing.
63. Added runtime event inspection surfaces:
   - `listRuntimeEventsValue/Json` on node, sdk facade, and node-handle.
64. Added event-bridge contract tests:
   - successful ingestion records applied runtime event metadata
   - invalid status ingestion records rejected runtime event metadata with explicit error.
65. Extended peer-hook lifecycle callback integration:
   - `socket_disconnected` now records structured runtime control event (`peer_hook_disconnected`)
   - `report_error` now records structured runtime control event (`peer_hook_error`) with encoded payload and error message.
66. Added unit test coverage for runtime control-event recorder:
   - verifies deterministic sequencing and metadata persistence for control events.

### Phase D: Scaffold Parity Checklist (Measurable)
Status: done for LN/payment/read scaffolded set; ongoing for remaining scaffolded groups.

Checklist dimensions:
1. `IV` (Input validation parity): native-like request constraints and type/format checks.
2. `ST` (State transition parity): modeled state follows native lifecycle/transition rules.
3. `OS` (Output shape parity): response fields and JSON/value contract are stable and aligned.
4. `EC` (Error contract parity): deterministic and documented error messages/conditions.
5. `ET` (Endpoint tests): direct tests on node/facade/node-handle surfaces for the contract.

Applied endpoint set (LN/payment/read scaffolded):
1. `connect_peer`: IV/ST/OS/EC/ET
2. `disconnect_peer`: IV/ST/OS/EC/ET
3. `open_channel`: IV/ST/OS/EC/ET
4. `close_channel`: IV/ST/OS/EC/ET
5. `list_peers`: OS/ET
6. `list_channels`: OS/ET
7. `get_channel_id`: IV/OS/EC/ET
8. `keysend`: IV/ST/OS/EC/ET
9. `send_payment`: IV/ST/OS/EC/ET
10. `create_ln_invoice`: IV/ST/OS/EC/ET
11. `invoice_status`: IV/ST/OS/EC/ET
12. `list_payments`: ST/OS/ET
13. `get_payment`: IV/OS/EC/ET
14. `node_info`: OS/ET
15. `network_info`: OS/ET

Remaining scaffolded groups not yet runtime-promoted (unsupported groups are `EC/ET` complete):
1. legacy wasm-facade compatibility surfaces intentionally kept scaffolded (`sendRgbFromGroups*`, legacy sdk-only issuance wrappers, and UDA issuance where runtime primitive is unavailable).
67. Introduced richer runtime event classification:
   - runtime events now include `event_kind` (`payment_status`, `control`, and classified non-payment payload kinds).
   - manual ingest remains strict; peer-hook ingestion now uses tolerant transport mode for non-payment payloads.
68. Added classification contract coverage:
   - wasm tests assert `event_kind=payment_status` on payment settlement events
   - unit test verifies tolerant transport mode records non-payment protocol payloads without failing hook flow.
69. Added runtime transport-event ingestion surface:
   - `ingestRuntimeTransportEventPayloadHexValue/Json` on node, sdk facade, and node-handle.
   - supports explicit transport event kinds (`peer_disconnected`, `peer_reconnected`, `channel_closed`, `channel_usable`, `channel_unusable`).
70. Added transport-event contract coverage:
   - unknown-target transport events are recorded with explicit kind and deterministic `applied=false`.
   - malformed/unknown transport payloads return explicit parse error and record classified event metadata.
71. Completed Step 3 unsupported coverage consolidation:
   - added a dedicated "Unsupported Contract Coverage" matrix section with explicit rationale per unsupported group.
   - mapped unsupported groups to tested surfaces (node/facade/node-handle/wallet-handle).
   - filled direct node-level unsupported contract test for `sign_message`.
72. Reclassified checklist status for unsupported groups:
   - unsupported groups are now tracked as `EC/ET complete` (explicit error contract + tests), while remaining non-applicable checklist dimensions (`IV/ST/OS`) stay scoped to runtime-backed/scaffold promotion phases.
73. Upgraded `sign_message` from unsupported to scaffolded runtime behavior:
   - `RlnWasmNode::signMessageValue/Json` now returns deterministic signatures using the node-scoped scaffold signing identity.
   - signatures are produced from trimmed message input and returned as stable hex-encoded recoverable signature payloads.
   - added/updated contract tests on node, sdk facade, and sdk node-handle surfaces.
74. Upgraded media endpoints from unsupported to scaffolded runtime behavior:
   - `postAssetMediaValue/Json` now validates input, computes SHA-256 digest, and persists media payload in wasm media scaffold storage.
   - `getAssetMediaValue/Json` now validates digest and resolves persisted media bytes with stable `{bytes_hex}` shape.
   - added/updated contract tests on sdk facade + wallet facade + wallet-handle surfaces.
75. Completed Step 1 acceptance freeze:
   - added endpoint-by-endpoint target mode lock (`runtime-backed`, `scaffold-to-runtime`, `unsupported-by-design`) in `SDK_WASM_ENDPOINT_MATRIX.md`.
   - introduced strict gate profiles mapped to `IV/ST/OS/EC/ET` requirements (`P_STATEFUL`, `P_STATEFUL_READ`, `P_STATELESS`, `P_UNSUPPORTED`).
   - this matrix is now the execution contract for further phase work.
76. Completed Step 2 runtime-layer foundation:
   - refactored wasm LDK runtime manager into backend-selectable architecture (`scaffold` and `ldk_bridge`).
   - added `RlnWasmNode::newWithRuntimeBackend` for explicit backend selection without changing existing constructor behavior.
   - added backend selection/status contract tests (including unknown-backend validation).
77. Completed Step 3 first LN-flow migration slice:
   - moved peer/channel read authority (`list_peers`, `list_channels`, `get_channel_id`, `node_info` counters) to runtime-manager state on `ldk_bridge`.
   - moved peer/channel transition application (`peer_*`, `channel_*` transport events) to runtime-manager state on `ldk_bridge`.
   - kept `scaffold` backend behavior unchanged for compatibility and test stability.
   - added bridge-backend contract tests proving runtime-manager authoritative peer/channel views and transitions.
78. Completed Step 4 payment read-authority migration slice:
   - moved payment read authority (`list_payments`, `get_payment`, `invoice_status` lookup/status) to runtime-manager state on `ldk_bridge`.
   - mirrored payment writes/updates (`send_payment`, `keysend`, `create_ln_invoice`, and payment status event updates) into runtime-manager payment state on `ldk_bridge`.
   - kept `scaffold` backend behavior unchanged for compatibility and deterministic legacy contracts.
   - added bridge-backend contract tests proving runtime-manager authoritative payment views.
79. Completed Step 5 payment event-sync migration slice:
   - synchronized bridge runtime payment state during event-driven mutation paths (`ingest_read_event_payload_hex`, `fail_pending_payments`, and status transition application).
   - added bridge-backend contract tests proving runtime-manager payment status updates for manual ingest and pending-fail flows.
   - retained scaffold backend behavior and existing error contracts unchanged.
80. Completed Step 6 lifecycle/runtime authority binding:
   - wired sdk lifecycle state (`init`, `unlock`, `lock`) into runtime-layer session authority state.
81. Closed medium-gap swap persistence isolation:
   - moved swap runtime storage keying from a single global key to SDK-identity-scoped keys (`mnemonic` seed digest), preventing cross-sdk swap-state bleed.
   - added legacy read compatibility for previously persisted global-key snapshots.
82. Began test decoupling from `src/lib.rs` into module-owned files:
   - moved swap runtime contract tests into `src/swap_runtime/tests.rs`.
   - added dedicated swap identity-scope storage-key coverage.
   - added `src/ldk_event_applier/tests.rs` for runtime-bridge manual-event guard contracts.
83. Continued test decoupling for node/runtime-event coverage:
   - moved sdk facade + sdk node-handle runtime-event/status forwarding contracts from `src/lib.rs` into `src/ln_node/tests.rs`.
   - exposed test-only wasm reset helper (`reset_wasm_runtime_state_for_tests`) as `pub(crate)` for module test reuse.
84. Continued test decoupling for bridge backend parity contracts:
   - moved bridge runtime-status/backend-selection and peer/channel/payment reconnect contract tests from `src/lib.rs` into `src/ln_node/tests.rs`.
   - preserved contract assertions while switching the moved async stop/restore case to module-local `block_on` for `close_all_peers`.
85. Continued test decoupling for node facade/handle payment+decode contracts:
   - moved create-ln-invoice facade/handle parity tests, get-payment/list-payments facade/handle tests, invoice-status empty contracts, and decode-ln/decode-rgb empty contracts from `src/lib.rs` into `src/ln_node/tests.rs`.
   - updated moved list-payments ordering check to parse via `serde_json::Value` in module tests, removing dependency on lib-local helper structs.
86. Continued test decoupling for remaining node-surface facade contracts:
   - moved facade payment-view forwarding, facade keysend write-path forwarding, and facade/node-handle sign-message scaffold contracts from `src/lib.rs` into `src/ln_node/tests.rs`.
87. Continued alias/event test decoupling:
   - moved sdk facade/node-handle read-event alias contracts (`payment-success`, `payment timeout`, `eventName/paymentId`, status/state/paymentStatus aliases) into `src/ln_node/tests.rs`.
   - moved primary sdk facade/node-handle transport reconnect JSON-alias contracts into `src/ln_node/tests.rs`.
   - runtime managers now enforce session lock directly during `ensure_started` (`runtime session is locked; call unlock first`).
   - added runtime-layer contract test proving lock enforcement and unlock re-authorization on `ldk_bridge`.
81. Completed Step 7 bridge payment-local-mirror removal:
   - removed bridge-path local payment mirror synchronization for `list_payments`, `get_payment`, and `invoice_status`.
   - bridge payment status mutation paths (`update_payment_status*`, `ingest_read_event_payload_hex`, `fail_pending_payments`) now mutate runtime-manager payment state directly.
   - retained scaffold backend behavior and existing external API contracts unchanged.
82. Completed Step 8 payment event payload mapping expansion:
   - expanded payment event payload parser to accept LDK-style event JSON aliases (`PaymentSent`, `PaymentFailed`, `PaymentExpired`, `PaymentClaimed`) in addition to explicit `status`.
   - expanded text protocol aliases to accept `payment_succeeded:<hash>`, `payment_failed:<hash>`, and `payment_expired:<hash>`.
   - added parser contract tests for both JSON and text alias mappings.
83. Completed Step 9 callback-driven browser example flow:
   - upgraded `examples/wasm-interop/manual_js_wasm_interop.js` to demonstrate three callback payload formats end-to-end (explicit status JSON, event-alias JSON, text alias).
   - added runtime event and post-transition payment list logging in the example to make callback-driven settlement flow observable in browser runs.
   - updated `examples/wasm-interop/README.md` to document the expanded callback transition demo.
84. Completed Step 10 channel lifecycle hardening slice (session-coupled runtime peers):
   - added runtime peer accessors (`get_peer`, `set_peer_started`) on the wasm runtime-manager abstraction.
   - updated bridge peer lifecycle methods so `disconnect_peer` and `close_all_peers` work against runtime-restored peers even when no local websocket session exists.
   - added bridge-backend contract tests for disconnect/close-all behavior with runtime-only peer/channel state.
85. Completed Step 11 runtime state restoration hook slice:
   - extended runtime snapshot schema to persist peer/channel/payment state (not only lifecycle flags) for both `scaffold` and `ldk_bridge` managers.
   - wired mutating runtime-manager operations (`upsert/remove/set_*`) to persist updated runtime snapshots.
   - added bridge-backend contract test proving peer/channel/payment state restore across node instances sharing the same runtime key.
86. Completed Step 12 hook event-source bridge expansion:
   - upgraded auto hook `read_event` path to process both payment status payloads and transport/channel payloads in a unified runtime-event pipeline.
   - hook-driven transport events now mutate scaffold maps or `ldk_bridge` runtime-manager state through the same authoritative transition path.
   - added contract tests covering scaffold and bridge transport event application from hook payloads.
87. Completed Step 13 channel API compatibility hardening (bridge):
   - added a direct bridge-backend contract test covering the full API sequence `open_channel -> get_channel_id -> list_channels -> close_channel`.
   - asserted output/state parity on both wasm API responses and runtime-manager channel storage after close.
88. Completed Step 14 SDK runtime-backend selection parity:
   - added sdk facade constructors for explicit runtime backend selection (`newNodeWithRuntimeBackend`, `createNodeHandleWithRuntimeBackend`).
   - added wasm contract tests for bridge-backend selection and invalid backend error contracts on sdk facade/handle surfaces.
89. Completed Step 15 runtime-access guard hardening (peer/channel surfaces):
   - enforced runtime bootstrap/authority guard on `disconnect_peer`, `close_all_peers`, `list_peers`, `list_channels`, `node_info`, `close_channel`, and `get_channel_id`.
   - aligned peer/channel read and transition APIs with the same runtime-session lock/start gate already used by payment/lifecycle paths.
90. Completed Step 16 lock-guard contract coverage for peer/channel APIs:
   - added explicit tests proving runtime-session lock blocks `list_peers`, `list_channels`, `get_channel_id`, and `close_channel`.
   - confirmed deterministic lock error contract parity (`runtime session is locked; call unlock first`) across these surfaces.
91. Completed Step 17 lock-guard async peer-surface coverage:
   - extended lock contract tests to async peer operations `disconnect_peer` and `close_all_peers`.
   - confirmed these async surfaces return the same deterministic runtime lock error contract before any transport/session side effects.
92. Completed Step 18/19 stateless/read contract hardening:
   - `network_info` now enforces the runtime bootstrap/lock authority guard to match other node read/write surfaces.
   - tightened `check_indexer_url` wasm validation with explicit HTTP/HTTPS host-format checks while preserving stable electrum-unsupported behavior.
   - added contract tests for locked `network_info` and invalid HTTP indexer URL format.
93. Completed Step 20 reconnect semantics hardening:
   - runtime restore now normalizes peer connectivity to disconnected (`started=false`) to avoid stale live-session assumptions after restart.
   - bridge `open_channel` now requires connected peer state (`peer.started=true`) instead of only peer presence.
   - added restart/reconnect contract test proving `open_channel` fails before reconnect and succeeds after reconnect-state reactivation.
94. Completed Step 21 hook settlement-stream hardening:
   - added mixed-stream hook contract coverage for interleaved transport + payment payloads on `ldk_bridge`.
   - verified deterministic runtime-event ordering and terminal payment-state protection (`succeeded` does not regress to `failed` on later hook events).
95. Completed Step 22 sdk-surface reconnect invariants coverage:
   - added bridge reconnect invariant contracts on both sdk facade and sdk node-handle surfaces.
   - verified restored peers are visible as disconnected (`started=false`), `open_channel` fails pre-reconnect, and succeeds after reconnect-state reactivation.
96. Completed Step 23 media scaffold persistence hardening:
   - upgraded wasm media scaffold storage from in-memory-only map to browser-persistent storage (`localStorage`) with in-memory cache fallback.
   - wired `post_asset_media` writes and `get_asset_media` reads through shared media storage helpers to support cross-instance retrieval semantics.
   - added contract test proving media lookup succeeds after in-memory media cache reset (`post -> clear cache -> get`).
97. Completed Step 24 bridge peer-connectivity semantics hardening for payment sends:
   - tightened `send_payment` bridge no-route detection to require at least one connected runtime peer (`peer.started=true`) instead of peer presence only.
   - tightened `keysend` bridge destination routability check to require connected destination peer state (`peer.started=true`).
   - added bridge reconnect contract tests proving restored-disconnected peers force `failed` status until reconnect, then return to `pending` send behavior.
98. Completed Step 25 hook-driven reconnect state activation hardening:
   - fixed bridge transport-event application so `peer_reconnected` updates runtime-manager peer state (`started=true`) instead of presence-only checks.
   - added hook contract test proving `peer_reconnected` payload reactivates a persisted-disconnected bridge peer and records an applied runtime event.
99. Completed Step 26 sdk-surface bridge payment-connectivity coverage:
   - added sdk facade bridge contract test proving `send_payment` returns `failed` when no connected peers are available.
   - added sdk node-handle bridge contract test proving `keysend` returns `failed` when destination peer is not connected.
   - ensures bridge no-route/peer-connectivity semantics are asserted on top-level SDK surfaces, not only node-level contracts.
100. Completed Step 27 bridge payee-aware payment routability hardening:
   - tightened `send_payment` bridge routability policy: when invoice payee exists in runtime peer state, that specific payee must be connected (`started=true`) for `pending` send status.
   - retained fallback behavior requiring at least one connected peer for unknown payees.
   - added bridge contract test proving disconnected known payee forces `failed` until payee reconnect activation.
101. Completed Step 28 runtime-manager connectivity API centralization:
   - promoted peer connectivity checks into runtime manager abstraction via `has_connected_peer` and `has_any_connected_peer`.
   - rewired node connectivity helpers to use runtime-manager connectivity API instead of ad-hoc runtime peer scans.
   - added runtime-manager unit contract test covering connected-peer helper behavior and `started` transitions.
102. Completed Step 29 sdk-surface payee-aware bridge send coverage:
   - added sdk facade bridge contract test proving `send_payment` remains `failed` when a known payee peer is disconnected, even if an unrelated peer is connected.
   - added sdk node-handle bridge contract test for the same payee-aware behavior.
   - both tests verify transition back to `pending` after explicit payee reconnect activation.
103. Completed Step 30 sdk-surface transport reconnect path coverage for payee-aware sends:
   - added sdk facade bridge contract test proving `ingest_runtime_transport_event_payload_hex_value(peer_reconnected)` reactivates known payee connectivity for `send_payment`.
   - added sdk node-handle bridge contract test for the same reconnect-via-transport payload path.
   - both tests assert reconnect payload is marked `applied=true` with `event_kind=peer_reconnected`, then `send_payment` transitions from `failed` to `pending`.
104. Completed Step 31 transport event JSON-alias compatibility hardening:
   - extended transport payload parser to accept event-alias JSON envelopes (`event` + `peer_pubkey`/`channel_id`/`id`) in addition to existing typed JSON and text protocol forms.
   - added node-level unit contract for alias parsing (`PeerReconnected` JSON alias).
   - added wasm contract coverage on node and sdk facade surfaces proving JSON alias reconnect payloads apply and flip peer `started` state to `true`.
105. Completed Step 32 transport JSON-alias coverage expansion:
   - added sdk node-handle contract test proving JSON alias reconnect payloads (`event` + `id`) apply on bridge runtime and reactivate peer `started` state.
   - added node contract test proving channel event aliases (`ChannelUnusable`) can target channels via generic `id` fallback field.
   - asserted resulting runtime state transitions via public list surfaces (`listPeers`, `listChannels`).
106. Completed Step 33 sdk-surface channel JSON-alias fallback coverage:
   - added sdk facade contract test proving channel transport aliases (`ChannelUnusable`) apply via generic `id` field on bridge runtime ingestion.
   - added sdk node-handle contract test for the same `event` + `id` channel-unusable path.
   - both tests assert post-event channel state via public APIs (`list_channels*`) to confirm parity-visible transition.
107. Completed Step 34 transport JSON envelope alias expansion:
   - extended transport alias parser to accept `kind` and `type` keys as event-name sources (in addition to `event`).
   - added node unit contract test proving `kind` + `id` payloads parse into transport events.
   - added sdk facade wasm contract test proving `kind` alias reconnect payloads are applied and reflected by `list_peers`.
108. Completed Step 35 `type` alias transport coverage:
   - added node unit contract test proving `type` + `id` payloads parse into reconnect transport events.
   - added sdk node-handle wasm contract test proving `type` alias reconnect ingestion applies and updates peer started-state in bridge runtime views.
109. Completed Step 36 `type` alias channel-event coverage on SDK surfaces:
   - added node unit contract test proving `type` + `id` payloads parse into channel transport events (`ChannelUnusable`).
   - added sdk facade wasm contract test for `type` alias `channel_unusable` ingestion path.
   - added sdk node-handle wasm contract test for `type` alias `channel_unusable` ingestion path.
   - both SDK tests assert parity-visible channel transition via `list_channels*`.
110. Completed Step 37 `peer_connected` alias normalization:
   - expanded transport event parser to map `peer_connected` aliases (JSON/text) onto reconnect semantics.
   - added node parser contract tests covering both JSON (`PeerConnected`) and text (`peer_connected:<id>`) alias forms.
   - added sdk facade wasm contract test proving `PeerConnected` alias ingestion reactivates peer started-state via bridge transport path.
111. Completed Step 38 channel-open readiness alias normalization:
   - expanded transport event parser to map `channel_opened` and `channel_ready` aliases (JSON/text) onto `channel_usable` semantics.
   - added node parser contract tests covering JSON/text alias forms for channel-open/readiness events.
   - added wasm contract tests on node and sdk node-handle surfaces proving alias ingestion transitions channels from `pending/unusable` back to `opened/usable`.
112. Completed Step 39 channel-disconnected alias normalization:
   - expanded transport event parser to map `channel_disconnected` aliases (JSON/text) onto `channel_unusable` semantics.
   - added node parser contract tests covering both JSON (`ChannelDisconnected`) and text (`channel_disconnected:<id>`) alias forms.
   - added sdk facade wasm contract test proving alias ingestion transitions channels to `pending/unusable` state through public `list_channels` views.
113. Completed Step 40 sdk node-handle channel-disconnected alias coverage:
   - added sdk node-handle wasm contract test for `channel_disconnected:<id>` ingestion path.
   - asserted parity-visible channel transition to `pending/unusable` via `list_channels`.
114. Completed Step 41 peer-online alias normalization:
   - expanded transport parser to map `peer_online` aliases (JSON/text) to reconnect semantics.
   - added node parser contract coverage for `PeerOnline`/`peer_online:<id>`.
115. Completed Step 42 channel-online alias normalization:
   - expanded transport parser to map `channel_online` aliases (JSON/text) to `channel_usable` semantics.
   - added node parser contract coverage for `ChannelOnline`/`channel_online:<id>`.
116. Completed Step 43 sdk facade peer-online alias coverage:
   - added sdk facade wasm contract proving `PeerOnline` alias ingestion reactivates bridge peer started-state.
117. Completed Step 44 sdk node-handle channel-online alias coverage:
   - added sdk node-handle wasm contract proving `channel_online:<id>` alias ingestion reactivates channel usability (`opened/usable`).
118. Completed Step 45 peer-offline alias normalization:
   - expanded transport parser to map `peer_offline` aliases (JSON/text) to disconnect semantics.
   - added node parser contract coverage for `PeerOffline`/`peer_offline:<id>`.
119. Completed Step 46 channel-offline alias normalization:
   - expanded transport parser to map `channel_offline` aliases (JSON/text) to `channel_unusable` semantics.
   - added node parser contract coverage for `channel_offline` alias forms.
120. Completed Step 47 sdk facade peer-offline alias coverage:
   - added sdk facade wasm contract proving `PeerOffline` alias ingestion removes/disconnects peer from bridge runtime views.
121. Completed Step 48 sdk node-handle channel-offline alias coverage:
   - added sdk node-handle wasm contract proving `channel_offline:<id>` alias ingestion transitions channel to `pending/unusable`.
122. Completed Step 49 matrix parity refresh for offline aliases:
   - updated endpoint matrix notes for peer and channel transport alias sets to include `peer_offline` and `channel_offline`.
123. Completed Step 50 peer-up alias normalization:
   - expanded transport parser to map `peer_up` aliases (JSON/text) to reconnect semantics.
124. Completed Step 51 peer-down alias normalization:
   - expanded transport parser to map `peer_down` aliases (JSON/text) to disconnect semantics.
125. Completed Step 52 channel-up alias normalization:
   - expanded transport parser to map `channel_up` aliases (JSON/text) to `channel_usable` semantics.
126. Completed Step 53 channel-down alias normalization:
   - expanded transport parser to map `channel_down` aliases (JSON/text) to `channel_unusable` semantics.
127. Completed Step 54 SDK coverage for up/down aliases:
   - added node parser contract tests for `peer_up/down` and `channel_up/down`.
   - added sdk facade wasm contract for peer up/down ingestion transitions.
   - added sdk node-handle wasm contract for channel up/down ingestion transitions.
   - refreshed endpoint matrix alias notes for peer/channel up/down variants.
128. Completed Step 55 transport kind separator normalization:
   - introduced normalized transport-kind parsing for alias payload kinds, accepting `-`, `.`, and spaces as `_`-equivalent separators.
   - applied normalization uniformly to text protocol and JSON alias parsing paths.
129. Completed Step 56 parser contract coverage for separator-normalized aliases:
   - added node parser tests validating hyphen/dot alias acceptance for peer/channel transport events.
130. Completed Step 57 sdk facade separator-alias coverage:
   - added sdk facade wasm contract proving `peer-up:<id>` text alias ingestion applies reconnect semantics.
131. Completed Step 58 sdk node-handle separator-alias coverage:
   - added sdk node-handle wasm contract proving `channel.down:<id>` text alias ingestion applies unusable semantics.
132. Completed Step 59 matrix alias-compatibility refresh:
   - updated endpoint matrix notes to document separator-normalized alias compatibility for peer/channel transport events.
133. Completed Step 60 payment-event kind normalization:
   - normalized payment event kind parsing to accept separator variants (`-`, `.`, spaces) across text protocol and JSON event aliases.
134. Completed Step 61 payment alias set expansion:
   - added payment status aliases: `payment_success` -> `succeeded`, `payment_fail` -> `failed`, `payment_timeout` -> `expired`.
135. Completed Step 62 node parser coverage for normalized payment aliases:
   - added node contract tests proving normalized text/JSON payment aliases map to expected statuses.
136. Completed Step 63 sdk facade payment alias ingestion coverage:
   - added sdk facade wasm contract proving `payment-success:<hash>` alias ingestion updates status to `succeeded`.
137. Completed Step 64 sdk node-handle payment alias ingestion coverage:
   - added sdk node-handle wasm contract proving JSON alias `payment timeout` updates status to `expired`.
138. Completed Step 65 payment event-name/hash alias expansion:
   - extended payment JSON parsing aliases with `event_name`/`eventName` and `hash`/`payment_id`/`paymentId`.
   - expanded payment-event alias mapping with `payment_completed`, `payment_error`, and `payment_timed_out`.
139. Completed Step 66 transport JSON id alias expansion:
   - extended transport JSON alias parser to accept peer id fields `peerPubkey`, `peer_id`, `peerId`, `node_id`, and `nodeId`.
   - extended channel id alias parsing to accept `channelId` in addition to `channel_id`/`id`.
140. Completed Step 67 node parser contract coverage for extended aliases:
   - added node parser tests for `node_id` peer transport alias and `channelId` channel transport alias.
   - added node parser tests for payment `eventName` + `paymentId` and `event_name` + `payment_id` compatibility, plus `payment_error` text alias mapping.
141. Completed Step 68 sdk facade payment alias ingestion coverage:
   - added sdk facade wasm contract proving JSON aliases (`eventName=PaymentCompleted`, `paymentId`) settle payment status to `succeeded`.
142. Completed Step 69 sdk node-handle transport alias ingestion coverage:
   - added sdk node-handle wasm contract proving `node_id` peer reconnect alias and `channelId` channel-unusable alias are applied through runtime transport ingestion.
143. Completed Step 70 payment `status` alias mapping in JSON payloads:
   - extended `parse_payment_status_event_json` so `status` values can be interpreted through payment-event alias mapping (not only strict canonical status values).
   - preserves existing strict validation behavior for unknown status values (explicit allowed-status error on apply path).
144. Completed Step 71 node parser coverage for `status` alias forms:
   - added node parser tests proving JSON `status` aliases map correctly:
   - `PaymentSent` -> `succeeded`, `payment timed out` -> `expired`, `payment_error` -> `failed`.
145. Completed Step 72 sdk facade `status` alias ingestion coverage:
   - added sdk facade wasm contract proving JSON payload with `status=PaymentSent` settles payment status to `succeeded`.
146. Completed Step 73 sdk node-handle `status` alias ingestion coverage:
   - added sdk node-handle wasm contract proving JSON payload with `status=payment timed out` settles payment status to `expired`.
147. Completed Step 74 matrix parity refresh for `status` alias compatibility:
   - updated endpoint matrix invoice-status notes to document supported JSON `status` alias forms.
148. Completed Step 75 transport event-key alias expansion:
   - extended transport JSON alias parsing to accept `event_name` and `eventName` as event-name sources in addition to `event`/`kind`/`type`.
149. Completed Step 76 node parser coverage for transport event-key aliases:
   - added node parser tests for `eventName` peer reconnect mapping and `event_name` channel-unusable mapping.
150. Completed Step 77 sdk facade transport event-key alias ingestion coverage:
   - added sdk facade wasm contract proving `eventName=PeerConnected` with `node_id` applies reconnect semantics.
151. Completed Step 78 sdk node-handle transport event-key alias ingestion coverage:
   - added sdk node-handle wasm contract proving `event_name=channel_unusable` with `channelId` applies unusable channel transition.
152. Completed Step 79 matrix parity refresh for transport event-key aliases:
   - updated endpoint matrix `connect_peer` notes to document `event_name`/`eventName` ingestion compatibility.
153. Completed Step 80 payment status-field alias expansion:
   - extended payment JSON parsing to accept status-field aliases `state`, `payment_status`, and `paymentStatus` in addition to `status`.
154. Completed Step 81 node parser coverage for status-field aliases:
   - added node parser tests proving alias status fields map correctly through status alias normalization (`PaymentSent`, `payment timed out`, `payment_error`).
155. Completed Step 82 sdk facade status-field alias ingestion coverage:
   - added sdk facade wasm contract proving JSON payload `state=PaymentSent` settles payment status to `succeeded`.
156. Completed Step 83 sdk node-handle status-field alias ingestion coverage:
   - added sdk node-handle wasm contract proving JSON payload `paymentStatus=payment_error` settles payment status to `failed`.
157. Completed Step 84 matrix parity refresh for status-field aliases:
   - updated endpoint matrix invoice-status notes to document `status`/`state`/`payment_status`/`paymentStatus` compatibility.
158. Completed Step 85 media endpoint promotion readiness review:
   - verified media write/read path is runtime-backed via persistent store adapter (`localStorage`) with deterministic in-memory fallback.
   - confirmed contract coverage exists for post/get across wallet and sdk-facade surfaces, including persistence-across-memory-reset behavior.
159. Completed Step 86 `get_asset_media` target-mode promotion:
   - promoted endpoint status from `scaffolded` to `runtime-backed` in endpoint matrix.
   - updated acceptance freeze target mode from `scaffold-to-runtime` to `runtime-backed`.
160. Completed Step 87 `post_asset_media` target-mode promotion:
   - promoted endpoint status from `scaffolded` to `runtime-backed` in endpoint matrix.
   - updated acceptance freeze target mode from `scaffold-to-runtime` to `runtime-backed`.
161. Completed Step 88 media note normalization after promotion:
   - removed scaffold wording from media endpoint notes and kept runtime persistence semantics explicit.
162. Completed Step 89 scaffold backlog reduction checkpoint:
   - reduced `scaffold-to-runtime` backlog by 2 endpoints (`get_asset_media`, `post_asset_media`).
163. Completed Step 90 `check_indexer_url` promotion readiness review:
   - confirmed wasm-side indexer protocol validation has explicit IV/EC contracts (network parsing, empty input, invalid HTTP host format, stable electrum unsupported behavior) with dedicated tests.
164. Completed Step 91 `check_indexer_url` target-mode promotion:
   - promoted endpoint status from `scaffolded` to `runtime-backed` in endpoint matrix.
   - updated acceptance freeze target mode from `scaffold-to-runtime` to `runtime-backed`.
165. Completed Step 92 `sign_message` promotion readiness review:
   - confirmed deterministic node-scoped signing path is runtime-guarded (`ensure_runtime_ready`), trimmed-input normalized, and covered on node/facade/node-handle surfaces.
166. Completed Step 93 `sign_message` target-mode promotion:
   - promoted endpoint status from `scaffolded` to `runtime-backed` in endpoint matrix.
   - updated acceptance freeze target mode from `scaffold-to-runtime` to `runtime-backed`.
167. Completed Step 94 scaffold backlog reduction checkpoint:
   - reduced `scaffold-to-runtime` backlog by 2 endpoints (`check_indexer_url`, `sign_message`).
168. Completed Step 95 node-info contract coverage hardening:
   - added explicit node-level lock contract for `node_info` (`runtime session is locked; call unlock first`).
   - added sdk facade and node-handle forwarding contracts for `node_info` shape parity.
169. Completed Step 96 `node_info` target-mode promotion:
   - promoted endpoint status from `scaffolded` to `runtime-backed` in endpoint matrix.
   - updated acceptance freeze target mode from `scaffold-to-runtime` to `runtime-backed`.
170. Completed Step 97 network-info promotion readiness confirmation:
   - confirmed runtime-guard contract and sdk facade/node-handle forwarding coverage for stable shape parity.
171. Completed Step 98 `network_info` target-mode promotion:
   - promoted endpoint status from `scaffolded` to `runtime-backed` in endpoint matrix.
   - updated acceptance freeze target mode from `scaffold-to-runtime` to `runtime-backed`.
172. Completed Step 99 scaffold backlog reduction checkpoint:
   - reduced `scaffold-to-runtime` backlog by 2 endpoints (`node_info`, `network_info`).
173. Completed Step 100 sdk-surface peer/channel read forwarding coverage:
   - added sdk facade contract proving `list_peers`, `list_channels`, and `get_channel_id` forward correctly on `ldk_bridge`.
   - added sdk node-handle contract proving the same forwarding/read parity on handle surface.
174. Completed Step 101 `list_peers` target-mode promotion:
   - promoted endpoint status from `scaffolded` to `runtime-backed` in endpoint matrix.
   - updated acceptance freeze target mode from `scaffold-to-runtime` to `runtime-backed`.
175. Completed Step 102 `list_channels` target-mode promotion:
   - promoted endpoint status from `scaffolded` to `runtime-backed` in endpoint matrix.
   - updated acceptance freeze target mode from `scaffold-to-runtime` to `runtime-backed`.
176. Completed Step 103 `get_channel_id` target-mode promotion:
   - promoted endpoint status from `scaffolded` to `runtime-backed` in endpoint matrix.
   - updated acceptance freeze target mode from `scaffold-to-runtime` to `runtime-backed`.
177. Completed Step 104 scaffold backlog reduction checkpoint:
   - reduced `scaffold-to-runtime` backlog by 3 endpoints (`list_peers`, `list_channels`, `get_channel_id`).
178. Completed Step 105 payment read/status promotion readiness review:
   - confirmed runtime-ready guards and runtime-manager-authoritative behavior for `list_payments`, `get_payment`, and `invoice_status`.
   - confirmed coverage across node/facade/node-handle surfaces for deterministic ordering, trimmed-hash lookup, and empty-invoice error contracts.
179. Completed Step 106 `list_payments` target-mode promotion:
   - promoted endpoint status from `scaffolded` to `runtime-backed` in endpoint matrix.
   - updated acceptance freeze target mode from `scaffold-to-runtime` to `runtime-backed`.
180. Completed Step 107 `get_payment` target-mode promotion:
   - promoted endpoint status from `scaffolded` to `runtime-backed` in endpoint matrix.
   - updated acceptance freeze target mode from `scaffold-to-runtime` to `runtime-backed`.
181. Completed Step 108 `invoice_status` target-mode promotion:
   - promoted endpoint status from `scaffolded` to `runtime-backed` in endpoint matrix.
   - updated acceptance freeze target mode from `scaffold-to-runtime` to `runtime-backed`.
182. Completed Step 109 scaffold backlog reduction checkpoint:
   - reduced `scaffold-to-runtime` backlog by 3 endpoints (`list_payments`, `get_payment`, `invoice_status`).
183. Completed Step 110 lifecycle promotion readiness review:
   - confirmed `init`, `unlock`, and `lock` have stateful runtime authority wiring with explicit validation/error contracts.
   - confirmed dedicated lifecycle contracts: init success, unlock-before-init rejection, unlock+lock flow, invalid password, lock-before-init, and lock authority gating over node runtime calls.
184. Completed Step 111 `init` target-mode promotion:
   - promoted endpoint status from `scaffolded` to `runtime-backed` in endpoint matrix.
   - updated acceptance freeze target mode from `scaffold-to-runtime` to `runtime-backed`.
185. Completed Step 112 `unlock` + `lock` target-mode promotion:
   - promoted both endpoint statuses from `scaffolded` to `runtime-backed` in endpoint matrix.
   - updated acceptance freeze target modes from `scaffold-to-runtime` to `runtime-backed`.
186. Completed Step 113 `create_ln_invoice` promotion readiness + target-mode update:
   - confirmed runtime-managed invoice creation contracts on node/facade/node-handle surfaces, including asset-pair validation and pending-status registration.
   - promoted `create_ln_invoice` status from `scaffolded` to `runtime-backed` and updated acceptance freeze target mode accordingly.
187. Completed Step 114 scaffold backlog reduction checkpoint:
   - reduced `scaffold-to-runtime` backlog by 4 endpoints (`init`, `unlock`, `lock`, `create_ln_invoice`).
188. Completed Step 115 LN write-path promotion readiness review:
   - verified remaining LN write endpoints (`connect_peer`, `disconnect_peer`, `close_channel`, `open_channel`, `keysend`, `send_payment`) are runtime-ready guarded and runtime-manager authoritative on `ldk_bridge`.
   - confirmed broad node/facade/node-handle contract coverage for validation, transition semantics, alias ingestion, and reconnect/no-route behavior.
189. Completed Step 116 peer/channel write-path target-mode promotion:
   - promoted `connect_peer`, `disconnect_peer`, and `close_channel` from `scaffolded` to `runtime-backed`.
   - updated acceptance freeze target modes from `scaffold-to-runtime` to `runtime-backed` for all three endpoints.
190. Completed Step 117 channel/payment write-path target-mode promotion:
   - promoted `open_channel`, `keysend`, and `send_payment` from `scaffolded` to `runtime-backed`.
   - updated acceptance freeze target modes from `scaffold-to-runtime` to `runtime-backed` for all three endpoints.
191. Completed Step 118 endpoint-note normalization after write-path promotion:
   - removed scaffold wording from promoted LN write endpoints and retained runtime authoritative behavior details.
192. Completed Step 119 scaffold backlog reduction checkpoint:
   - reduced `scaffold-to-runtime` backlog by 6 endpoints (`connect_peer`, `disconnect_peer`, `close_channel`, `open_channel`, `keysend`, `send_payment`).
193. Completed Step 120 swap scaffold parity hardening slice:
   - moved swap contracts behind dedicated module (`bindings/wasm-sdk/src/swap_runtime.rs`) and tightened request validation parity:
     - `maker_init`: 64-hex asset-id validation and native-like swap-pair guards.
     - `maker_execute`: JSON request validation (`payment_secret`, `taker_pubkey`) with compatibility fallback.
     - `get_swap`: strict `payment_hash` validation and native-like `{swap: ...}` response envelope.
   - added deterministic list/read behavior:
     - effective expiry mapping (`Waiting -> Expired` on reads),
     - deterministic `list_swaps` ordering (`requested_at`, then `payment_hash`).
   - added/updated sdk contract coverage for roundtrip + validation paths on swap surfaces.
194. Completed Step 121 swap target-mode promotion:
   - promoted `maker_init`, `maker_execute`, `taker`, `get_swap`, and `list_swaps` from `scaffolded` to `runtime-backed` in endpoint matrix.
   - updated acceptance freeze target modes from `scaffold-to-runtime` to `runtime-backed` for these five swap endpoints.
195. Completed Step 122 scaffold backlog reduction checkpoint:
   - reduced `scaffold-to-runtime` backlog by 5 endpoints (`maker_init`, `maker_execute`, `taker`, `get_swap`, `list_swaps`).
196. Completed Step 123 onion endpoint promotion:
   - implemented `send_onion_message` runtime shim in dedicated module (`bindings/wasm-sdk/src/onion_runtime.rs`).
   - replaced unsupported stub with native-like validation contracts (`node_ids`, secp256k1 pubkey parse, `tlv_type >= 64`, hex payload validation).
   - added sdk contract tests for success and validation error contracts.
197. Completed Step 124 onion target-mode promotion and backlog reduction:
   - promoted `send_onion_message` from `scaffolded` to `runtime-backed` in endpoint matrix.
   - updated acceptance freeze target mode from `unsupported-by-design` to `runtime-backed` (`P_STATEFUL`).
   - removed onion entry from unsupported coverage matrix and reduced remaining scaffold backlog accordingly.
198. Completed Step 125 runtime abstraction boundary hardening:
   - introduced shared runtime state storage abstraction (`bindings/wasm-sdk/src/runtime_store.rs`) via `RuntimeStateStore`.
   - migrated wasm LDK runtime persistence path to use the shared store boundary instead of direct storage calls.
199. Completed Step 126 persistent swap runtime snapshoting:
   - added browser-persistent swap snapshot storage (`rln:wasm:swap-runtime:v1`) with load-on-first-use hydration.
   - maker/taker swap mutations now persist and restore maker/taker maps and sequence counters across browser sessions.
200. Completed Step 127 event-driven swap settlement integration:
   - wired LN payment-status event application paths to swap runtime state updates (`pending/succeeded/failed/expired`) keyed by payment hash.
   - swap transitions are now driven by real payment status updates in runtime event flows, not only synthetic API-only transitions.
201. Completed Step 128 parity contract extension for event-linked swaps:
   - added contract test proving `update_payment_status` mutates linked swap status from `Waiting` to terminal settlement (`Succeeded`) through runtime event flow.
202. Completed Step 129 cross-parity contract fixture slice:
   - added fixture-style wasm contract coverage (`sdk_swap_onion_native_parity_error_contracts`) that asserts wasm error contracts match native RLN semantics for swap/onion validation cases.
   - covered parity-critical contracts for:
     - `maker_init`: BTC/BTC and same-asset rejection.
     - `send_onion_message`: empty path, invalid pubkey, low TLV type, and non-hex payload rejection.
203. Completed Step 130 runtime-event persistence hardening:
   - added persisted runtime-event log snapshots keyed by node runtime (`rln:wasm:runtime-events:<proxy_url>`), restored on node reconstruction.
   - wired persistence through node API event-producing paths and peer-manager hook callbacks.
   - added deterministic node contract test validating event-log restore semantics after recreating a node with the same runtime key.
204. Completed Step 131 event-stream ownership guardrails:
   - introduced dedicated event-ownership policy module (`bindings/wasm-sdk/src/ldk_event_applier.rs`).
   - on `ldk_bridge`, manual status mutation surfaces (`updatePaymentStatus*`, `updatePaymentStatusByInvoice*`, `failPendingPayments`) are now blocked by default unless debug mode is enabled (`debug-manual-events` feature or tests).
   - on `ldk_bridge`, manual event-ingestion surfaces (`ingestReadEventPayloadHex*`, `ingestRuntimeTransportEventPayloadHex*`) are now blocked by default unless debug mode is enabled.
205. Completed Step 132 internal transition stream unification:
   - moved internal auto-transition paths (`send_payment`/`keysend` no-route failures and invoice expiry transition) to use the same runtime-event stream application path instead of direct status mutation.
   - added bridge runtime contract test ensuring event-stream status application updates runtime payment state and emits `payment_status` runtime log entries.
206. Completed Step 133 peer-hook queue/drain semantics:
   - updated auto peer-manager hooks so `read_event` queues payloads and `process_events` drains/applies them through runtime hook applier.
   - reduces immediate callback-side mutation and aligns bridge behavior closer to LDK event-processing cadence.
   - added queue/drain contract test covering ordered transport event application and queue exhaustion.
207. Completed Step 134 queued control-event processing:
   - moved peer-hook `socket_disconnected` and `report_error` side effects into the same queued drain pipeline used for `read_event` payloads.
   - hook callbacks now enqueue control events; all fail-pending + runtime control log writes are applied during `process_events`, preserving single-path event processing cadence.
208. Completed Step 135 peer-session error/disconnect drain enforcement:
   - updated wasm peer-session callback path so error/disconnect branches explicitly invoke `process_events` after enqueuing control events.
   - prevents queued control events from being stranded when the socket exits on read failure or disconnect notifications.

### Phase D: Event-Driven LN State
Status: in_progress.

1. Keep hook-driven payment status transitions.
2. Expand event payload mapping to mark specific payments succeeded/failed/expired.
3. Add end-to-end browser example for callback-driven state transitions.

### Phase E: Channel Lifecycle Hardening
Status: planned.

1. Move from pure in-memory channel stubs toward stronger session-coupled state.
2. Add deterministic reconnect behavior and state restoration hooks.
3. Add compatibility tests for `open/close/list/getChannelId`.

### Phase F: Native-Only Endpoint Strategy
Status: planned.

1. For endpoints requiring full native daemon assumptions, return stable explicit unsupported errors in wasm facade.
2. Document reasons and migration paths.

## Immediate Next Executions

1. Execute `LDK_FULL_PORT_PLAN.md` Phase 0 design freeze:
   - finalize browser persistence adapter interfaces (`storage`, schema migration, restore policy) around the new runtime manager seam.
2. Continue Phase 1 bootstrap:
   - migrate scaffold browser persistence from `localStorage` adapter to final IndexedDB schema-backed adapter.
   - connect sdk-level lifecycle state with node runtime manager instance(s).
3. Continue method-by-method promotions where feasible before full runtime swap (next candidates: `create_ln_invoice`, `send_payment/keysend` runtime event settlement integration).
4. Continue phase progression toward runtime-backed LN:
   - bridge runtime settlement hooks to real LDK event sources (replace scaffold-only event payload simulation).
5. Continue phase progression:
   - migrate scaffold invoice creation from deterministic key/signing placeholder to real LDK invoice runtime path.
6. Continue phase progression:
   - replace scaffold lifecycle/global guard with explicit runtime-session authority model tied to concrete LDK runtime instances.
