# SDK WASM Endpoint Matrix

Source: async SDK endpoints in `src/sdk/mod.rs` mapped to their WASM equivalents.

Status legend:
- `runtime-backed`: backed by `rgb-lib-wasm` wallet/runtime behavior.
- `unsupported-by-design`: intentionally unavailable in wasm with stable error/contract coverage.
- `missing`: no direct wasm-sdk endpoint yet.

Checklist tags used in Notes for LN/payment/read endpoints:
- `IV`: input validation parity
- `ST`: state transition parity
- `OS`: output shape parity
- `EC`: error contract parity
- `ET`: endpoint tests present (node/facade/node-handle)

## Core Rewritten Parts (SDK -> WASM)

Core SDK behavior has been rewritten in wasm where native runtime assumptions do not hold in browser/JS environments:

| Area | What was rewritten in wasm | Motivation |
|---|---|---|
| Runtime storage | Browser-persistent state stores for node/runtime/swap/media state (with in-memory fallback paths where needed). | Native file/daemon-backed state assumptions do not map cleanly to wasm; browser persistence is required for session continuity, deterministic restore, and testability across SDK instances. |
| Transport/event bridge | Runtime transport/read-event ingestion pipeline with queue/drain semantics and deterministic event application. | Native LDK transport loops are not directly portable to wasm execution surfaces, so wasm needs explicit event ingestion and replay-safe transitions for peer/channel/payment parity. |
| Runtime authority | The `wasm_native_ldk` runtime is the single backend; node/runtime-manager state is authoritative for peer/channel/payment reads. | Browser runtime keeps authoritative state in the node runtime manager rather than relying on native process/file assumptions, with stable facade contracts. |
| Swap runtime | Dedicated wasm swap runtime manager (`bindings/wasm-sdk/src/swap_runtime.rs`) for maker/taker lifecycle + persistence. | Decouples swap state from native process assumptions and provides deterministic lifecycle handling plus persistent recovery in browser contexts. |

| Native SDK endpoint (`src/sdk/mod.rs`) | Wasm equivalent (`bindings/wasm-sdk`) | Status | Notes |
|---|---|---|---|
| `estimate_fee` | `getFeeEstimation` / `getFeeEstimationJson` | runtime-backed | Via wallet online object. |
| `check_indexer_url` | `checkIndexerUrlValue` / `checkIndexerUrlJson` | runtime-backed | Deterministic wasm-side protocol validation (`esplora` only) with explicit HTTP/HTTPS host-format checks and stable electrum-unsupported contract. |
| `check_proxy_endpoint` | `checkProxyUrl` | runtime-backed | URL/proxy format validation surface exists. |
| `node_info` | `nodeInfoValue` / `nodeInfoJson` | runtime-backed | Runtime-backed node metadata from wasm node runtime manager with runtime-ready guard parity and stable shape across node/facade/node-handle surfaces. |
| `network_info` | `networkInfoValue` / `networkInfoJson` | runtime-backed | Runtime-guarded wasm node network metadata (`network`, `height`) with stable shape across node/facade/node-handle surfaces. `height` is sourced from chain-sync tip state when chain sync is running; otherwise it defaults to `0`. |
| `address` | `getAddress` / `walletGetAddress` | runtime-backed | Wallet address fetch supported. |
| `btc_balance` | `getBtcBalanceValue` / `getBtcBalanceJson` | runtime-backed | Wallet balance supported. |
| `sign_message` | `signMessageValue` / `signMessageJson` | runtime-backed | Deterministic signing with node-scoped derived key (`proxy_url`-scoped), runtime-ready guard parity, trimmed-input parity, and stable hex-encoded recoverable signature shape. |
| `get_channel_id` | `getChannelId` | runtime-backed | Runtime-manager-backed temporary->final channel id resolution with runtime-ready guard and stable not-found error contract (`unknown temporary_channel_id`). |
| `list_channels` | `listChannelsValue` / `listChannelsJson` | runtime-backed | Runtime-backed channel read surface using runtime-manager authoritative state with deterministic output shape and runtime-ready guard parity. |
| `list_peers` | `listPeersValue` / `listPeersJson` | runtime-backed | Runtime-backed peer read surface using runtime-manager authoritative state with deterministic output shape and runtime-ready guard parity. |
| `asset_balance` | `getAssetBalanceValue` / `getAssetBalanceJson` | runtime-backed | Wallet-backed asset queries. |
| `asset_metadata` | `getAssetMetadataValue` / `getAssetMetadataJson` | runtime-backed | Wallet-backed metadata queries. |
| `get_asset_media` | `getAssetMediaValue` / `getAssetMediaJson` | runtime-backed | Media-digest lookup backed by wasm media store with native-like digest validation and stable `{bytes_hex}` response shape; media entries are browser-persistent (`localStorage`) with in-memory cache fallback. |
| `list_assets` | `listAssetsValue` / `listAssetsJson` | runtime-backed | Wallet-backed listing. |
| `send_rgb` | `sendBegin` + `sendEndValue/Json` | runtime-backed | Two-step PSBT flow. |
| `send_rgb_from_groups` | `walletSendRgbFromGroupsValue` / `walletSendRgbFromGroupsJson` and wallet-handle `sendRgbFromGroupsValue` / `sendRgbFromGroupsJson` | runtime-backed | Adapter implemented on wallet surfaces via real `send_begin` orchestration; legacy SDK-only `sendRgbFromGroups*` remains compatibility-only/unsupported on wasm facade. |
| `init` | `initValue` / `initJson` | runtime-backed | Stateful wasm lifecycle initialization (`password+mnemonic` bootstrap) with stable validation/error contracts and runtime-session authority propagation (`initialized=true`, `authorized=false`). |
| `unlock` | `unlock` | runtime-backed | Runtime-backed lifecycle unlock with request parsing, init precondition, password validation, runtime-session authorization transition (`authorized=true`), and default RGB wallet bootstrap from lifecycle mnemonic (auto-attached to newly created nodes). |
| `lock` | `lock` | runtime-backed | Runtime-backed lifecycle lock with init precondition and deterministic authorization transition (`authorized=false`) so runtime managers reject locked sessions until next unlock. |
| `connect_peer` | `connectPeer` | runtime-backed | Runtime-backed websocket peer-connect path with secp256k1/`host:port` validation and runtime-event application through shared transport pipeline. Peer state is runtime-manager authoritative and reconnect aliases reactivate persisted peers (`started=true`), with JSON/text alias compatibility for event/id fields and separator-normalized kinds. |
| `disconnect_peer` | `disconnectPeer` | runtime-backed | Runtime-backed disconnect path with secp256k1 validation and `peer_disconnected` transport transition application, including stale channel cleanup and runtime-manager-authoritative state transitions. |
| `close_channel` | `closeChannel` | runtime-backed | Runtime-backed channel-close transition via `channel_closed` transport event path with runtime-manager-authoritative application, runtime-ready guard parity, and stable not-found contract. Extended options path (`closeChannelWithOptions`) supports native-like peer/session validation, virtual-session status transitions (`active -> abandon_pending -> abandoned`), and `force=true` rejection for trusted virtual channels. |
| `create_utxos` | `createUtxosBegin` + `createUtxosEnd/Json` | runtime-backed | Two-step PSBT flow. |
| `issue_asset_nia` | node/facade `issueAssetNiaValue` / `issueAssetNiaJson` | runtime-backed | Node-level adapter wired to `rgb-lib-wasm` `Wallet::issue_asset_nia`; default wallet is bootstrapped on `unlock`, auto-attached to created nodes, and lazily attached on first issuance call if missing (manual `attachWallet` still supported to override). |
| `issue_asset_cfa` | node/facade `issueAssetCfaValue` / `issueAssetCfaJson` | runtime-backed | Node-level adapter wired via `rgb-lib-wasm` `Wallet::issue_asset_ifa` compatibility mapping; default wallet is bootstrapped on `unlock`, auto-attached to created nodes, and lazily attached on first issuance call if missing (manual `attachWallet` still supported to override). |
| `issue_asset_uda` | `walletIssueAssetUdaValue` / `walletIssueAssetUdaJson` and wallet-handle `issueAssetUdaValue` / `issueAssetUdaJson` | unsupported-by-design | Explicit unsupported contract: `rgb-lib-wasm` currently has no UDA issuance primitive; legacy SDK-only `issueAssetUda*` remains compatibility-only/unsupported on wasm facade. |
| `keysend` | `keysendValue` / `keysendJson` | runtime-backed | Runtime-backed payment send path enforcing native min amount parity (`SDK_HTLC_MIN_MSAT`), destination pubkey validation, RGB payload validation (`asset_id` format + `asset_amount > 0`), and no-route failure transitions through runtime payment-status events. Delivery requires connected destination peer state (`peer.started=true`). |
| `send_btc` | `sendBtcBegin` + `sendBtcEnd` | runtime-backed | Two-step PSBT flow. |
| `post_asset_media` | `postAssetMediaValue` / `postAssetMediaJson` | runtime-backed | Media upload stores hex payload in wasm media store keyed by SHA-256 digest and returns stable `{digest}` response shape; writes persist to browser storage for cross-instance retrieval parity. |
| `rgb_invoice` | `blindReceiveValue/Json` + `witnessReceiveValue/Json` | runtime-backed | Functional equivalent split by receive mode. Transport endpoints can be passed explicitly, sourced from wallet-scoped `setRgbProxyTransport`, or inherited from SDK-scoped `setDefaultRgbProxyTransport` on wallet creation. |
| `rgb_proxy_transport_config` | wallet/facade/handle `setRgbProxyTransport`, `clearRgbProxyTransport`, `rgbProxyTransportValue/Json` | runtime-backed | Wallet-scoped default RGB transport endpoint for browser proxy flows. `blindReceive`/`witnessReceive` fallback to configured endpoint when `transport_endpoints` is omitted; auth token + node-id are optional but must be provided together. Config is keyed by wallet runtime identity (`idb_key`) so it is shared across wrappers/handles for the same wallet and restored from browser `localStorage` when in-memory runtime state is cold. If wallet-scoped config is missing, SDK-level default transport is used as runtime fallback and backfilled into wallet-scoped storage. Precedence is strict: wallet-scoped config overrides SDK default; after wallet config clear, SDK default applies again. |
| `open_channel` | `openChannelValue` / `openChannelJson` | runtime-backed | Runtime-backed channel open path with native-like bounds/pair validation (`capacity_sat` min/max, RGB pair + min RGB-capacity constraints), `asset_id` format validation, and transport-driven usable/unusable transitions (including alias compatibility for channel events). Channel state is runtime-manager authoritative and requires connected peer state (`peer.started=true`). Virtual-channel mode (`trusted_no_broadcast`) is exposed on extended options endpoints (`openChannelValueWithOptions` / `openChannelJsonWithOptions`) with draft reservation, persisted session state, and reconciliation on runtime restore. |
| `send_payment` | `sendPaymentValue` / `sendPaymentJson` | runtime-backed | Runtime-backed invoice payment path with invoice parsing/amount contracts, RGB min-amount validation (`SDK_INVOICE_MIN_MSAT`), `asset_id` format checks, payment-secret return, and no-route failure via runtime payment-status transitions. Routability requires connected peers and connected known payee when present in runtime peer state. |
| `fail_transfers` | `failTransfers` / `failTransfersJson` | runtime-backed | Wallet transfer state operation. |
| `refresh_transfers` | `refreshValue` / `refreshJson` | runtime-backed | Wallet refresh operation. |
| `maker_execute` | `makerExecuteValue` / `makerExecuteJson` | runtime-backed | Runtime-backed by wasm swap state manager (`bindings/wasm-sdk/src/swap_runtime.rs`), with `Waiting -> Pending` transitions, expiry checks, request-contract validation for JSON execute payloads (`payment_secret`, `taker_pubkey`), and payment-event-driven settlement updates (`pending/succeeded/failed/expired`). |
| `maker_init` | `makerInitValue` / `makerInitJson` | runtime-backed | Runtime-backed by wasm swap state manager (`bindings/wasm-sdk/src/swap_runtime.rs`) with native-like pair validation, 64-hex asset-id checks, deterministic swapstring encoding, and browser-persistent runtime snapshots scoped by SDK identity seed (with legacy global-key read compatibility). |
| `taker` | `taker` | runtime-backed | Runtime-backed by wasm swap state manager (`bindings/wasm-sdk/src/swap_runtime.rs`) with swapstring parsing and taker-book insertion. |
| `send_onion_message` | `sendOnionMessage` | runtime-backed | Runtime-backed validation/runtime shim with native-like request checks (`node_ids`, `tlv_type >= 64`, hex payload, secp256k1 pubkeys) and deterministic success/error contracts. |
| `sync` | `syncOnline` | runtime-backed | Requires explicit `Online` input in wasm. |
| `decode_ln_invoice` | `decodeLnInvoiceValue` / `decodeLnInvoiceJson` | runtime-backed | Real BOLT11 parsing via `lightning-invoice`; explicit empty-input + invalid-invoice contracts. |
| `decode_rgb_invoice` | `decodeRgbInvoiceValue` / `decodeRgbInvoiceJson` | runtime-backed | Uses `rgb-lib-wasm` invoice decode. |
| `invoice_status` | `invoiceStatusValue` / `invoiceStatusJson` | runtime-backed | Runtime-backed payment-state read contract with explicit empty-input validation and runtime-manager-authoritative status lookup. Event ingestion supports status aliases from JSON/text payloads, including separator-normalized variants, event/hash field aliases, and status-field aliases (`status`/`state`/`payment_status`/`paymentStatus`). HODL lifecycle states are supported (`claimable`, `claiming`, `cancelled`) in addition to `pending/succeeded/failed/expired`. |
| `create_ln_invoice` | `createLnInvoiceValue` / `createLnInvoiceJson` | runtime-backed | Runtime-backed invoice creation path builds signed BOLT11 invoices using node-scoped identity, registers inbound payment state, and enforces RGB min-amount checks (`SDK_INVOICE_MIN_MSAT`) plus `asset_id` format validation. Default invoice type is `auto_claim`; HODL invoice creation is exposed on wasm node/facade surfaces via `createHodlLnInvoiceValue/Json` with explicit `payment_hash`. |
| `list_payments` | `listPaymentsValue` / `listPaymentsJson` | runtime-backed | Runtime-backed payment list read path with deterministic ordering (`created_at`, then `payment_hash`) and runtime-ready guard parity; runtime-manager authoritative. Payment entries include HODL metadata (`invoice_type`, `preimage`) when present. |
| `get_payment` | `getPaymentValue` / `getPaymentJson` | runtime-backed | Runtime-backed payment lookup with trimmed-hash acceptance, explicit empty-hash/not-found contracts, and runtime-manager-authoritative resolution. |
| `get_swap` | `getSwapValue` / `getSwapJson` | runtime-backed | Runtime-backed by wasm swap state manager (`bindings/wasm-sdk/src/swap_runtime.rs`) with native-like `{swap: ...}` envelope support, strict `payment_hash` format validation, and status values synchronized from payment-status runtime events. |
| `list_swaps` | `listSwapsValue` / `listSwapsJson` | runtime-backed | Runtime-backed by wasm swap state manager (`bindings/wasm-sdk/src/swap_runtime.rs`) returning maker/taker swap collections with deterministic ordering, effective-expiry status mapping, and browser-persistent snapshot restore scoped by SDK identity seed. |
| `list_transactions` | `listTransactionsValue` / `listTransactionsJson` | runtime-backed | Wallet-backed BTC tx list. |
| `list_transfers` | `listTransfersValue` / `listTransfersJson` | runtime-backed | Wallet-backed RGB transfer list. |
| `list_unspents` | `listUnspentsValue/Json` + `listUnspentsVanillaValue/Json` | runtime-backed | Wallet-backed unspent listing. |

## Acceptance Gates

This section records the behavior mode per native endpoint and the gate profile each one is covered by.

Mode legend:
- `runtime-backed`: endpoint is runtime-backed in wasm.
- `unsupported-by-design`: intentionally unsupported in wasm, with stable error contracts.

Gate profile criteria:

| Profile | IV | ST | OS | EC | ET | Meaning |
|---|---|---|---|---|---|---|
| `P_STATEFUL` | required | required | required | required | required | Stateful write/lifecycle/channel/payment methods. |
| `P_STATEFUL_READ` | optional | required | required | required | required | Runtime-backed reads that reflect evolving state. |
| `P_STATELESS` | required | n/a | required | required | required | Stateless parsing/validation/read adapters. |
| `P_UNSUPPORTED` | n/a | n/a | optional | required | required | Explicit unsupported-by-design endpoints. |

Endpoint modes:

| Native endpoint | Mode | Gate profile |
|---|---|---|
| `estimate_fee` | runtime-backed | `P_STATELESS` |
| `check_indexer_url` | runtime-backed | `P_STATELESS` |
| `check_proxy_endpoint` | runtime-backed | `P_STATELESS` |
| `node_info` | runtime-backed | `P_STATEFUL_READ` |
| `network_info` | runtime-backed | `P_STATEFUL_READ` |
| `address` | runtime-backed | `P_STATELESS` |
| `btc_balance` | runtime-backed | `P_STATEFUL_READ` |
| `sign_message` | runtime-backed | `P_STATELESS` |
| `get_channel_id` | runtime-backed | `P_STATEFUL_READ` |
| `list_channels` | runtime-backed | `P_STATEFUL_READ` |
| `list_peers` | runtime-backed | `P_STATEFUL_READ` |
| `asset_balance` | runtime-backed | `P_STATEFUL_READ` |
| `asset_metadata` | runtime-backed | `P_STATELESS` |
| `get_asset_media` | runtime-backed | `P_STATELESS` |
| `list_assets` | runtime-backed | `P_STATEFUL_READ` |
| `send_rgb` | runtime-backed | `P_STATEFUL` |
| `send_rgb_from_groups` | runtime-backed | `P_STATEFUL` |
| `init` | runtime-backed | `P_STATEFUL` |
| `unlock` | runtime-backed | `P_STATEFUL` |
| `lock` | runtime-backed | `P_STATEFUL` |
| `connect_peer` | runtime-backed | `P_STATEFUL` |
| `disconnect_peer` | runtime-backed | `P_STATEFUL` |
| `close_channel` | runtime-backed | `P_STATEFUL` |
| `create_utxos` | runtime-backed | `P_STATEFUL` |
| `issue_asset_nia` | runtime-backed | `P_STATEFUL` |
| `issue_asset_cfa` | runtime-backed | `P_STATEFUL` |
| `issue_asset_uda` | unsupported-by-design | `P_UNSUPPORTED` |
| `keysend` | runtime-backed | `P_STATEFUL` |
| `send_btc` | runtime-backed | `P_STATEFUL` |
| `post_asset_media` | runtime-backed | `P_STATELESS` |
| `rgb_invoice` | runtime-backed | `P_STATEFUL` |
| `open_channel` | runtime-backed | `P_STATEFUL` |
| `send_payment` | runtime-backed | `P_STATEFUL` |
| `fail_transfers` | runtime-backed | `P_STATEFUL` |
| `refresh_transfers` | runtime-backed | `P_STATEFUL` |
| `maker_execute` | runtime-backed | `P_STATEFUL` |
| `maker_init` | runtime-backed | `P_STATEFUL` |
| `taker` | runtime-backed | `P_STATEFUL` |
| `send_onion_message` | runtime-backed | `P_STATEFUL` |
| `sync` | runtime-backed | `P_STATEFUL` |
| `decode_ln_invoice` | runtime-backed | `P_STATELESS` |
| `decode_rgb_invoice` | runtime-backed | `P_STATELESS` |
| `invoice_status` | runtime-backed | `P_STATEFUL_READ` |
| `create_ln_invoice` | runtime-backed | `P_STATEFUL` |
| `list_payments` | runtime-backed | `P_STATEFUL_READ` |
| `get_payment` | runtime-backed | `P_STATEFUL_READ` |
| `get_swap` | runtime-backed | `P_STATEFUL_READ` |
| `list_swaps` | runtime-backed | `P_STATEFUL_READ` |
| `list_transactions` | runtime-backed | `P_STATEFUL_READ` |
| `list_transfers` | runtime-backed | `P_STATEFUL_READ` |
| `list_unspents` | runtime-backed | `P_STATEFUL_READ` |

## Unsupported Contract Coverage

The endpoints below are intentionally unsupported-by-design and have explicit rationale + deterministic error-contract tests.

| Endpoint group | Wasm endpoint surfaces | Unsupported rationale | Tested surfaces |
|---|---|---|---|
| UDA issuance | legacy sdk `issueAssetUdaValue/Json`; wallet surfaces `walletIssueAssetUdaValue/Json` / handle `issueAssetUdaValue/Json` | Legacy RLN issuance adapter unavailable in wasm; `rgb-lib-wasm` does not expose UDA issuance primitive. | sdk facade, sdk wallet-facade, wallet-handle |
| Legacy grouped transfer endpoint | legacy sdk `sendRgbFromGroupsValue/Json` | Legacy grouped SDK adapter unavailable in wasm facade; runtime-backed grouped send is available on wallet surfaces (`walletSendRgbFromGroups*` / handle). | sdk facade, wallet-handle (runtime-backed path) |
| Native host-only virtual cleanup proofs | `openChannel*` / `closeChannel*` node surfaces | Wasm runtime implements virtual draft/session lifecycle + reconciliation and enforces conservative preflight guards from runtime payment ledger (`pending/claimable/claiming` HTLC block + net counterparty BTC/RGB value floor checks since session creation). Full native host-only proofs that depend on filesystem RGB temp artifacts and exact counterparty floor internals are intentionally not replicated in browser runtime. | Node-level contract tests for virtual session lifecycle + validation are present |

## Wasm-Only Node Runtime Helpers

These APIs do not map 1:1 to native SDK endpoints but are used by the wasm runtime contract:

| Wasm endpoint | Purpose | Current behavior |
|---|---|---|
| `createHodlLnInvoiceValue` (+ `Json`) | HODL invoice creation | Creates BOLT11 invoice with explicit `payment_hash`, persists inbound payment as `invoice_type=hodl`, and reuses core invoice validation (`expiry_sec`, RGB min amount, `asset_id` checks). |
| `claimHodlInvoiceValue` (+ `Json`) | HODL settlement | Validates `payment_preimage` against invoice `payment_hash`, enforces HODL-only + `claimable` precondition, performs idempotent settle (`{ changed: false }` on replay), and persists `preimage` + terminal `succeeded` state. |
| `cancelHodlInvoiceValue` (+ `Json`) | HODL cancellation | Enforces HODL-only + `claimable` precondition and persists terminal `cancelled` state with deterministic error contracts for non-claimable/terminal invoices. |
| `ingestReadEventPayloadHex` (+ `Json`) | Manual payment-status event ingestion | Applies `payment_status` events (explicit status and alias mappings such as `PaymentSent`/`payment_succeeded`) and records runtime-event log entries; payment status mutation is applied directly against runtime-manager storage. Disabled by default; requires debug mode (`debug-manual-events` feature or tests). |
| `ingestRuntimeTransportEventPayloadHexValue` (+ `Json`) | Manual transport-event ingestion | Applies peer/channel transport events and records runtime-event log entries. Disabled by default; requires debug mode (`debug-manual-events` feature or tests). |
| `installAutoPeerManagerHooks` | Hook-driven runtime event bridge | Auto hook now follows queue/drain semantics closer to LDK flow: `read_event` enqueues payloads and `process_events` drains/applies them; socket disconnect/error side effects are also queued and processed in the same drain pass. Peer-session error/disconnect branches force a final `process_events` call so queued control events are not stranded. Transport transitions mutate runtime-manager state directly. |
| `listRuntimeEventsValue` (+ `Json`) | Runtime event introspection | Returns ordered runtime-event audit stream; log is persisted/restored per node runtime key through browser-backed state store. |
| `failPendingPayments` | Force pending payment failures | Transitions `pending -> failed` via `payment_status` runtime events (no direct status mutation); resulting statuses are applied directly in runtime-manager storage. Disabled by default; requires debug mode (`debug-manual-events` feature or tests). |
| `ldkRuntimeStatusValue/Json` | Runtime backend introspection | Reports the active runtime backend (`wasm_native_ldk`) and readiness. Test-only constructors (`new_with_runtime_backend`) accept only `wasm_native_ldk` and reject any other backend name. |
| `chainSyncStartValue/Json`, `chainSyncStatusValue/Json`, `chainSyncStopValue/Json`, `chainSyncTickValue/Json`, `chainSyncEnqueueRebroadcastTx` | Chain/source sync runtime driver | Esplora-backed chain sync control surface (tip polling + persisted status + rebroadcast queue) on node/facade/node-handle APIs; updates `network_info.height` from synced tip state and exposes reorg-ish tip regression tracking (`tip_regressed`, `last_tip_regression_at`). |
| `ldkRuntimeComponentsValue/Json` | Runtime component diagnostics | Exposes wasm-native LDK runtime component readiness and lifecycle counters (invoice/payment/keysend/channel open/close) for deterministic observability across node/facade/node-handle surfaces. |
| `nativeRuntimeCoreStatusValue/Json` | Native runtime core diagnostics | Exposes wasm-native runtime-core lifecycle/readiness/schema/queue-size state for deterministic startup/restore introspection across node/facade/node-handle surfaces. |
| `drainNativeRuntimeQueueValue/Json` | Native runtime core queue drain | Drains deterministic runtime-core event queue for replay/debug/contract assertions; queue state is persisted with crash-safe pending/committed checkpoint semantics. |
| `setRelaySessionAuth`, `relaySessionAuthValue/Json` | Relay auth/session binding | Configures relay auth token + node-id binding at node runtime level; validated (`token` non-empty, `node_id` secp256k1 pubkey) and propagated to websocket connect options for peer-session transport (`auth_token` + `node_id` query). |
| `setDefaultRgbProxyTransport`, `clearDefaultRgbProxyTransport`, `defaultRgbProxyTransportValue/Json` | SDK-level RGB transport defaults | Stores validated wallet transport defaults at SDK scope and auto-applies them to newly created wallets/handles (including lifecycle-bootstrapped default wallet). |
| `listRgbLnTransfersValue/Json` | RGB-over-LN transfer ledger view | Returns runtime-tracked RGB-over-LN payment ledger derived from LN payment records with RGB payload (`asset_id`/`asset_amount`) and synchronized status transitions (`pending/claimable/claiming/succeeded/cancelled/failed/expired`). |
