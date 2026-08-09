# RGB Funding Safety Comparison

## Decision

Keep the transactional, idempotent retry design. Do not keep a pre-funded channel resumption implementation.

The decision is based on recoverability and protocol invariants, not latency. A fresh retry has a new temporary channel ID and funding transaction, while the abandoned operation is resolved from durable RGB, LDK, wallet-database, VSS, and chain evidence. Ambiguous broadcast outcomes fail closed.

## Candidate A: Serializable Pre-Funded Continuation

### Experiment

The receiver was disconnected while processing `funding_created`, after the channel had entered the pre-funded phase. The disconnect call waited 3.702 seconds for the synchronous funding callback to return. LDK then emitted `ChannelClosed(DisconnectedPeer)` and discarded the funding attempt.

The current v1 channel handshake has no reconnect message that replays `funding_created`, and LDK deliberately does not serialize the pre-funded inbound channel. The existing `test_channel_resumption_fail_post_funding` invariant confirms that a post-funding pre-channel is not resumable.

### Result

| Observation | Result |
|---|---:|
| Successful resumptions | 0 |
| Replayed `funding_created` messages after reconnect | 0 |
| Disconnect completion | 3.702 s |
| Channel outcome | Closed and discarded |

Making this safe would require a bilateral LDK protocol extension, versioned serialization of the complete pre-funded channel state, durable signer and monitor context, replay identity, and peer negotiation. Persisting only local continuation data would create a half-open channel that the remote peer cannot safely resume.

Candidate A was rejected and no production implementation was retained.

## Candidate B: Transactional Retry

### Retained Design

- RGB validation and acceptance execute against an isolated stock snapshot.
- A filesystem journal records `Prepared`, `Promoting`, and `Promoted` transitions with file and directory synchronization.
- Promotion atomically swaps staged and live stock while retaining a rollback snapshot.
- The receiver persists funding acceptance phases and ties finalization to LDK's durable initial-monitor completion action.
- `funding_signed` is withheld until the initial monitor and RGB stock commit are durable, including asynchronous signer retry in either completion order.
- The sender journals stock promotion, LDK handoff, broadcast-safe observation, broadcast intent, broadcast completion, and finalization.
- A retry uses a fresh temporary channel ID and funding transaction ID.
- Pre-broadcast interruption rolls back stock, transfer reservations, channel metadata, pending-funding mappings, and signed PSBT data.
- A known transaction without matching durable channel state, or an unknown transaction after broadcast intent, fails closed.
- Journal identities, versions, transaction IDs, channel IDs, peer keys, and canonical RGB allocations are validated before recovery.
- Recovery and ordinary financial mutations share one atomic operation lease, preventing a rollback snapshot from overwriting concurrent wallet work.
- Final RGB stock and canonical channel metadata must be acknowledged by VSS before recovery tombstones are removed.

### 88-History Results

| Scenario | History construction | Funding/retry | Competing `listChannels` | Result |
|---|---:|---:|---:|---|
| Healthy live channel | 198.112 s | 3.242 s | 1 ms | Channel ready |
| Forced abort, restart, and fresh retry | included in 215.68 s total | rollback 709 ms; restart 2.875 s; retry 5.518 s | 1 ms | Abandoned TX absent; retry channel ready |

The history construction creates 88 settled transfers and is test setup, not channel acceptance. The safety decision does not depend on Candidate B being faster.

### Resolver Performance

The same high-history transfer was replayed with Signet-like witness latency:

| Resolver | Validation | Acceptance | Total |
|---|---:|---:|---:|
| Original serial resolver | 116.508 s | 115.614 s | approximately 232 s |
| Operation-scoped cached resolver | prefetch 8.637 s; validate 859 ms | import 51 ms; accept 299 ms | approximately 9.85 s |

The cached resolver deduplicates witness transaction IDs for the operation and uses bounded concurrent retrieval. The funding safety state machine does not depend on these latency gains.

## Verification

| Check | Result |
|---|---|
| 88-history healthy live funding | Pass, 204.28 s total |
| 88-history forced abort, process restart, and retry | Pass, 215.68 s total |
| Competing `listChannels` during both high-history cases | Pass, 1 ms |
| Async signer fails once, monitor persists, signer retries | Pass, 6.81 s |
| Receiver malformed-journal and identity rejection | Pass |
| Sender recovery, exact-broadcast replay, and journal validation | Pass |
| rgb-lib filesystem crash-window matrix | Pass |
| RLN local SQLite process-kill matrix | Pass |
| RLN synchronized VSS process-kill matrix | Pass |
| rgb-lib RGB acceptance process-kill matrix | Pass, 24.69 s native |
| RLN SQLite kill matrix under AddressSanitizer | Pass |
| rgb-lib acceptance kill matrix under AddressSanitizer | Pass, 74.94 s |
| VSS ordering regression under ThreadSanitizer | Pass |
| rust-lightning full modified suite | Pass, 1,188 tests; 9 ignored |
| RLN library suite | Pass, 232 tests; 1 ignored |
| RLN full lib-SDK integration suite | Pass, 27 tests |
| rgb-lib full all-features suite | Pass, 340 tests; 19 declared ignores; doctest pass |
| rgb-ops full native workspace suite | Pass, 36 deterministic tests; 4 explicit live public-network probes not run because this host cannot connect to `mempool.space` |
| rgb-ops WASM/headless-Chrome CI gate | Pass; both WASM crates compiled, no browser-only tests declared |
| RLN Linux WASM compile gate | Pass |
| UniFFI/VLS feature and smoke matrix | Pass |
| C-FFI tests, generated headers, C11 and C++17 syntax/link smoke | Pass |
| Stable formatting, supported clippy matrices, and diff whitespace validation | Pass |

## Remaining Release Gates

- Run interoperability against the intended production LSP, proxy, indexer, VSS deployment, and signer configuration. Local regtest and Signet-like replay validate protocol behavior, but they cannot certify a deployment that was not exercised.
- Exercise the repository's explicitly ignored disruptive VSS scenarios in an isolated release environment. They stop infrastructure or depend on timing and are intentionally excluded from the normal suite.
- Ambiguous post-intent broadcast recovery intentionally requires explicit intervention when chain evidence cannot determine whether the exact transaction was published. This is a safety policy, not an automatic recovery gap; the operator/user path must be present in the consuming product.
- Before release, replace the published experiment-branch dependencies with approved immutable commit pins in every downstream manifest and independent binding lockfile. Local path overrides were used only while running cross-repository validation; the current manifests and lockfiles contain no local path sources.
- Run the four explicit live `mempool.space` compatibility probes from a release environment with outbound access. Deterministic local HTTP-contract tests cover the same status and raw-transaction parsing paths in the normal suite, but this host cannot establish a TCP connection to the public service.
- The pinned rust-lightning revision emits nightly deprecation warnings and one future-incompatibility notice from `proc-macro-error2`; these are dependency-maintenance items, not failures in the funding state machine.

The experiment changes are published on `hardik/rgb-witness-resolution-experiment`.
