# External-signer burn

Base: RLN dev `3973ebf`, including the external-unlock Ethereum RPC change (#192).
Dependency revisions and public burn request/response shapes remain unchanged.

## Implementation

- [x] Route HTTP and SDK burn through the shared wallet implementation.
- [x] Keep the internal/password signer path unchanged.
- [x] Reserve burn inputs before external signing and keep the wallet mutex through completion.
- [x] Reject changed transactions and unfinalized signer responses; retain prepared RGB metadata.
- [x] Preserve pending state on completion failure; never retry or release inputs automatically.
- [x] Unit regression tests: all 272 library tests passed, including six burn PSBT checks.
- [x] Funded IFA burn with native/attached signers and password-signer regression.
- [x] Unavailable signer, altered transaction, unfinalized PSBT, lost signing response,
      retained reservations, reopen and explicit reconciliation.
- [x] Real local BFA lock/mint, two confirmed burns, balances and terminal proof recipients.
- [x] C binding `cargo check --locked --offline`; existing ABI unchanged.
- [x] Library Clippy, formatting and whitespace checks passed.
- [x] Funded IFA regression added to the existing SDK E2E workflow.
- [x] Final concurrent-burn regression and full unit rerun.
- [ ] Hosted CI results after opening the draft.
- [x] Final diff/security review: dependency pins, request/response shapes and password path unchanged.

## Qualification

No production funds or public-network burns are used for testing. Ethereum payout,
application-level idempotency, a WDK burn journal and cross-platform release builds
are outside this RLN change. A failed/uncertain burn must be reconciled by transfer
ID and on-chain state before any new burn is requested.

Local disk space was 2.7 GiB at the start. The first integration link exhausted
disk space; after clearing reproducible build caches, the serialized build
succeeded. Docker also reported a disk-full engine failure, so the first funded
test stopped at connection refusal, before creating or funding any wallets.
Docker was recovered without resetting volumes. The existing indexers then failed
on a cached block hash absent from bitcoind; the existing Bitcoin data and index
volumes are preserved. A separate fresh Electrs 0.12.0 index against the same
Bitcoin chain allowed both funded test scenarios to pass. These were local
qualification infrastructure failures, not burn runtime defects.

The BFA proof scenario is explicitly opt-in because it requires a deployed and
funded Anvil bridge. It was run locally, not inferred from the IFA test. Hosted
CI adds the IFA/recovery scenario; it does not deploy Anvil or verify an EVM payout.

All local runs used macOS arm64. The final IFA/recovery run passed in 43 seconds;
the repeated BFA proof run passed in 20 seconds. The C binding was compile-checked,
not exercised by a separate funded C ABI client. Physical devices, process-kill
fault injection and loss of a broadcast response were not qualified by these tests.
