# RLN WASM SDK

Browser-facing WASM SDK for `rgb-lightning-node`.

This crate provides a JS/WASM API for SDK-level operations.

## Canonical architecture doc

See [ARCHITECTURE.md](ARCHITECTURE.md) for the consolidated WASM stack overview (browser runtime + gateway + E2E + contracts).

## Scope

- Target: browser and Node.js consumers via `wasm-bindgen` output.
- Focus: SDK parity in WASM.
- `issue_asset_uda` is unsupported (no UDA primitive in `rgb-lib-wasm`).

For endpoint-level status, see [SDK_WASM_ENDPOINT_MATRIX.md](SDK_WASM_ENDPOINT_MATRIX.md).

## Mainnet: on-chain only, without a Lightning runtime

A Mainnet `RlnWasmNode` does not construct an LDK runtime manager, object graph, chain-sync
driver, peer hooks or background Lightning workers. This applies to explicit
`newWithNodeRuntimeId(..., "mainnet")` and to a networkless node that adopts a Mainnet
wallet. Constructing or inspecting a networkless node leaves it dormant. Its first
successful Lightning initialization selects the historical Regtest default; attach
the wallet first when another network is intended. A scope already bound to a
network or identity cannot be reused with a conflicting one. Compatible handles
share the existing runtime without reseeding it.

Existing mainnet wallets may contain idle Lightning snapshots written by older
on-chain-only releases. Mainnet construction and wallet attachment accept those
records without decoding, resuming, deleting or replacing them. No Lightning-state
preload or empty-history check is required to construct or attach a Mainnet node.

SDK `init`/`unlock` and `preloadPersistentRuntimeState` still preload browser state.
Lightning snapshots, queues, peer/transfer/event records and standalone swap state
are staged in memory; their existing best-effort IndexedDB-to-localStorage copy is
performed only when a Lightning consumer restores the particular key. Mainnet and
unresolved nodes do not restore those views. Supported non-mainnet activation and
standalone runtimes continue restoring their saved state. This also preserves
inactive records when localStorage and IndexedDB contain different bytes. Media,
RGB proxy settings and the shared virtual-channel preference keep their existing
hydration behavior; explicit administrative changes to that preference are allowed.

This policy assumes no unresolved historical mainnet Lightning obligations in the
supported rollout. Preserving records does not monitor or recover old channels,
certify other devices/tabs or make a future Lightning-enablement/downgrade safe.
Normal mainnet wallet use does not restore or replicate the separate `<store_id>-ldk`
VSS stream. Wallet `configureVssBackup` remains independent. The browser cannot
certify remote history: SDK `init`/`unlock` do not take LDK store credentials, and
the pinned VSS client lacks complete key listing. An absent manifest is not proof
of empty state. Independent wallet objects and explicitly invoked standalone
transports/administration retain their own behavior.

Mainnet rejects peer/connect/reconnect, channel/funding, Lightning invoice/payment,
async-payment and event-processing methods, including `chainSyncTick*`,
`chainSyncStart*`, `chainSyncEnqueueRebroadcastTx`, `installAutoPeerManagerHooks`,
`persistLdkRuntimeState` and `configureLdkVssReplication`. The chain driver combines
manual rebroadcasts with Lightning broadcasts, so its manual enqueue method is also
restricted. Use the on-chain wallet's sync/send methods for on-chain work. The
node-owned bridge rejects before opening a socket. A Mainnet node does not replace
another node's global hooks; standalone transports and custom hooks without a
configured node retain their independent behavior. SDK facades/handles propagate:

```text
LightningUnsupportedOnMainnet: RLN on mainnet currently supports only on-chain methods. Lightning APIs are not supported.
```

On-chain wallet APIs, RGB invoices and message signing retain their requirements and
identity derivation. `nodePubkey*` derives the historical live KeysManager public
key when an online wallet is attached, without constructing LDK. Without an online
wallet it retains the existing raw signing identity. Previously, a failed LDK graph
initialization could also make `nodePubkey*` fall back to the raw identity despite
an online wallet; that error-dependent fallback is no longer attempted. Message
signing continues to use its existing raw signing key in either case.

Shared node/status calls do not start Lightning. An absent
runtime reports `disabled` on Mainnet (`cold` before activation otherwise), no active components
and zero active peers/channels. `chainSyncStop*` is an inactive no-op. Synchronous
`networkInfo*` returns `NetworkInfoUnavailable` when no chain driver exists; it does
not invent a chain height. Query the wallet's on-chain indexer for chain information.
Wallet backups/VSS remain independent of the restricted LDK VSS stream; LDK VSS
health, disable and fence administration remain available.

Explicit supported non-mainnet constructors retain runtime preparation and saved
chain-driver resumption. Standalone swap bookkeeping and onion-request validation
have no configured node and do not execute node Lightning operations.

## Build

From repository root. The crate targets `wasm32` only and builds against the real
`rgb-lib-wasm` backend (the `real-wasm-rgb` feature is enabled by default), so all checks
use the wasm32 target:

```sh
cargo check --manifest-path bindings/wasm-sdk/Cargo.toml --target wasm32-unknown-unknown
cargo test --manifest-path bindings/wasm-sdk/Cargo.toml --target wasm32-unknown-unknown --no-run
```

The unit tests are executed in a browser via `wasm-pack` (see [CI](#ci) below); a plain
`cargo test` host run is not supported because the wasm RGB backend requires a wasm32 target.

LDK `KeysManager` / `ChannelManager` RGB scratch paths and rgb-lib **auto-wallet** `data_dir`: on **wasm32** use single-segment names `rln_ldk_<slug>` / `rln_wallet_<slug>` (no POSIX `/tmp`); on native host tests use `/tmp/rln_wasm_ldk_<slug>` and `/tmp/rln_wasm_sdk_wallet_<slug>`. See `src/wasm_runtime_paths.rs`.

RGB **LN peer wire** vs the RGB **HTTP proxy**: channel RGB data is handled inside LDK
`ChannelManager` / `PeerManager`, plus optional BOLT1 fork custom messages in `src/rgb_ln_wire.rs`.
The HTTP proxy contract in `RGB_WASM_PROXY_TRANSPORT_SPEC.md` is orthogonal transport for JSON-RPC.

Generate package artifacts:

```sh
cd bindings/wasm-sdk
wasm-pack build --target web --out-dir pkg
```

## Run Local Environment

Use this when you want to run browser interop flows against local regtest infra.

Prerequisites:

- Docker + Docker Compose
- Rust toolchain (for `wasm-pack` build and optional gateway runs)
- `wasm-pack` (`cargo install wasm-pack`)

Start local infra (from repo root):

```sh
cd bindings/wasm-sdk
docker compose -f compose.wasm.yaml up -d
```

This brings up:

- RGB proxy: `127.0.0.1:3000` (host port mapped from compose `proxy`)
- WASM proxy gateway: `127.0.0.1:3001`
- Esplora HTTP: `127.0.0.1:3002`
- Electrum: `127.0.0.1:50001` (host port mapped from compose `electrs`)
- Bitcoind RPC: `127.0.0.1:18443` (host port mapped from compose `bitcoind`)

Build WASM package for browser examples:

```sh
cd bindings/wasm-sdk
wasm-pack build --target web --dev
```

Serve repository files (from repo root):

```sh
python3 -m http.server 8080
```

Examples are automated, headless end-to-end flows (see
`examples/wasm-interop/README.md`): `run_e2e_full_flow.mjs` (full
RGB-over-Lightning flow) and `run_multihop_flow.mjs` (native → WASM → native
forwarding).

Stop local infra:

```sh
cd bindings/wasm-sdk
docker compose -f compose.wasm.yaml down
```

If ports are busy, either stop conflicting services or edit `compose.wasm.yaml` port mappings.

## Runtime model

- Public API: `wasm_native_ldk` runtime only.
- Boundary: no implicit dependency on a local runtime service endpoint
  (for example `ws://127.0.0.1:3001`) in production paths.

The SDK surface is object-handle based (same direction as UniFFI SDK instance
model) and avoids REST coupling.

## Selecting the Bitcoin network

The node takes an **explicit network**, mirroring the native node's `--network` flag: it is the node's
single source of truth and drives the LDK `ChannelManager`/`NetworkGraph` (and therefore the chain
advertised in the `Init` handshake). Pass it as the third argument to `newWithNodeRuntimeId`, then
create the wallet on the **same** network:

```js
// 1. Create the node with an explicit network: "mainnet" | "testnet" | "testnet4" | "signet" |
//    "regtest" (case-insensitive).
const node = RlnWasmNode.newWithNodeRuntimeId(proxyUrl, runtimeId, "Signet");

// 2. Create the wallet on the SAME network.
const keys = rgbRestoreKeysValue("signet", mnemonic);
const wallet = await RlnWasmWallet.create(/* ... */ { bitcoin_network: "Signet", /* ... */ });

// 3. Attach the wallet BEFORE connecting to any peer. attachWallet validates that the wallet's
//    network matches the node's; a mismatch throws (like the native node's NetworkMismatch).
node.attachWallet(wallet);

// 4. Now start chain sync / connect. The Init handshake advertises the matching chain hash in its
//    `networks` field.
node.chainSyncStartValue(esploraUrl, pollIntervalMs);
```

Notes and gotchas:

- **Node owns the network.** With an explicit network the node registers it with the LDK backend at
  construction (before the object graph exists), so the correct chain is advertised even if no wallet
  is ever attached.
- **`attachWallet` validates.** If the wallet's network differs from the node's configured network,
  `attachWallet` throws `wallet network (…) does not match the node's configured network (…)`. This
  catches the common "created the wallet on the wrong chain" mistake up front.
- **Wallet/peer mismatch.** If your (matching) network still differs from the peer's — e.g. you are on
  Regtest but the LSP is on Signet — the connection is rejected with
  `Peer does not support any of our supported chains` and the handshake disconnects. The `networks`
  field in the LDK `Init` log is the genesis/chain hash — Signet `f61eee3b…`, Regtest `06226e46…`,
  Testnet `43497fd7…`, Mainnet `6fe28c0a…`.
- **`SignetCustom`** (custom / mutinynet-style signets) maps to standard `Signet` for LDK purposes.
- **Bare constructor / facade.** `new RlnWasmNode(proxyUrl)` (and the SDK-facade `newNode*` helpers)
  do not take a network: the node stays unconfigured and **adopts** the first attached wallet's network
  (falling back to `Regtest` until then). Use this only for dev/regtest or when the wallet is the
  intended source of truth.

Implementation: `NETWORK_REGISTRY` / `set_network_for_runtime` in `src/ldk_live_backend.rs`; network
selection and validation in `new_with_runtime_id_opt` / `attach_wallet_shared` in `src/ln_node.rs`.

## Examples

- WASM example flow: `bindings/wasm-sdk/examples/`
- Python SDK reference flow: `src/uniffi_api/examples/python-interop/`

## CI

WASM checks run in `.github/workflows/test.yaml`: the `feature-matrix` job's
`wasm-without-vls` mode runs `cargo check --target wasm32-unknown-unknown` against
`bindings/wasm-sdk/Cargo.toml`. The package `pkg/` artifact is built separately by
`.github/workflows/wasm-artifacts.yaml` via `wasm-pack build`. That job also runs
the browser unit suite, including startup and storage regressions, in headless
Chrome with the existing `wasm-bindgen-test` harness. The default suite does not
require funded wallets or live Lightning services.

Run the complete browser unit suite locally:

```sh
WASM_BINDGEN_TEST_TIMEOUT=300 WASM_TEST_BROWSER=chrome ./bindings/wasm-sdk/scripts/run-browser-tests.sh
```

`WASM_TEST_BROWSER` supports `chrome` (default) and `firefox`.

## Additional docs

- [ERROR_CONTRACT.md](ERROR_CONTRACT.md)
- [SDK_WASM_ENDPOINT_MATRIX.md](SDK_WASM_ENDPOINT_MATRIX.md)
- [SDK_WASM_UML_COMPARE.md](SDK_WASM_UML_COMPARE.md)
- [SDK_WASM_FLOWS_PORTING.md](SDK_WASM_FLOWS_PORTING.md)
- [LN_WASM_NATIVE_PARITY_SPEC.md](LN_WASM_NATIVE_PARITY_SPEC.md)
- [RGB_WASM_PROXY_TRANSPORT_SPEC.md](RGB_WASM_PROXY_TRANSPORT_SPEC.md)

## Related

- UniFFI SDK docs: `src/uniffi_api/README.md`
- Core project docs: repository `README.md`
