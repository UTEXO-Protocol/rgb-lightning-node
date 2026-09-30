# Claude Review Guidance

This repository contains RLN (RGB Lightning Node), a Rust daemon and SDK surface.

When reviewing pull requests, focus on:

- Correctness and regressions in channel lifecycle, payment state, and persistence.
- Safety around wallet/accounting invariants and RGB transfer state transitions.
- Error handling in daemon APIs and background tasks to avoid partial state writes.
- Security-sensitive boundaries (auth tokens, signing keys, network IO, serialization).
- Test coverage for behavior changes, especially around regtest and integration flows.

Repository conventions:

- Keep changes minimal and scoped to the PR goal.
- Prefer explicit errors over silent fallback behavior.
- Avoid introducing breaking API changes unless the PR explicitly requires it.
- Keep CI/workflow changes conservative and deterministic.

Minimal SDK workspace (`minimal-sdk/`, a pnpm workspace; the root `cargo` commands do
not build it):

- `packages/gateway` is the REST gateway in front of one shared RLN. Its native mobile
  client is the separate `rgb-sdk-kotlin-light` repository, which ships its own Rust
  signing core; this repo holds no mobile SDK any more. Treat the gateway's HTTP surface
  as a published contract: response schemas are strict (`additionalProperties: false`),
  so removing or retyping a field is breaking, while adding one is not.
- `packages/client-sdk` is the TypeScript client (web / React Native) and the behavioural
  reference for that contract; `packages/e2e` is the regtest journey suite. Test with
  `pnpm -C minimal-sdk test`.
- The parity fixture `minimal-sdk/packages/client-sdk/test/fixtures/rgblib-parity.json`
  is rgb-lib-authored ground truth: read by relative path, never copied or regenerated
  here.
- Invariants to enforce in review: no secret ever reaches the gateway (I1), one watch-only
  rgb-lib wallet per user (I2), no cross-user visibility (I3), only the gateway holds the
  RLN credential (I4). No `SdkError`, error body or log line may render a mnemonic, seed,
  xprv, private key or bearer token.
