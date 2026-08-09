# 88-History RGB Funding Experiments

> The initial production boundary in this report was followed by a transactional retry experiment.
> See `88-history-funding-safety-comparison.md` for the retained design and final safety decision.

## Scope

- Generated 87 settled RGB witness transfers through public RLN APIs.
- Opened and confirmed a live RGB Lightning channel as the 88th history entry.
- Replayed the successful 55,164-byte funding consignment through the original and optimized rgb-lib acceptance paths.
- Compared live channel setup with `reuse_addresses` enabled and disabled.
- Prototyped durable funding-acceptance phases while releasing the LDK peer mutex during RGB acceptance.

## Exact Consignment Replay

| Variant | Prefetch | Validate | Import | Accept | Resolver calls | Duplicate calls |
|---|---:|---:|---:|---:|---:|---:|
| Original rgb-ops | 0 ms | 608 ms | 55 ms | 741 ms | 176 | 88 |
| Single-response resolver | 0 ms | 592 ms | 56 ms | 562 ms | 176 | 88 |
| Operation resolver | 85 ms | 222 ms | 59 ms | 368 ms | 0 fallback | 0 |

The operation resolver reduced measured end-to-end acceptance stages from 1,404 ms to 734 ms on local regtest, a 47.7% reduction.

## Live Channel Results

| Variant | History build | Funding created | In-validation `listChannels` | Channel ready |
|---|---:|---:|---:|---|
| Reuse enabled, peer mutex held | 215.247 s | 2.712 s | Probe did not overlap acceptance | Yes |
| Reuse enabled, pending state | 214.802 s | 2.813 s | 1 ms | Yes |
| Reuse disabled, pending state | 210.635 s | 2.656 s | 1 ms | Yes |

The single-run differences between reuse enabled and disabled are within normal local variation. Address reuse did not reduce historical validation growth or live funding latency in this test.

## Pending Funding Prototype

The prototype writes durable `Validating`, `Accepted`, and `RetryRequired` states. It removes the inbound pre-funded channel from the peer map, releases the peer mutex during RGB acceptance, reacquires it for commitment and monitor installation, and removes the record only after funded-channel installation.

Verified:

- A competing `listChannels()` call returned in 1 ms while RGB acceptance was in `Validating`.
- The 88-history channel completed and became ready.
- Interrupted records survive storage reopen and reconcile to `RetryRequired`.
- The existing colored-channel Electrum regression test passed.
- RLN compiled with the modified Lightning dependency and `git diff --check` passed.

## Production Boundary

This prototype must not be merged as-is.

LDK deliberately does not serialize pre-funded channels. After a crash, the durable record can prove that an operation was interrupted and force a retry, but it cannot reconstruct the removed inbound channel. If RGB acceptance completed before the crash, rgb-lib state and temporary channel metadata may already have changed. A production implementation needs one of:

1. Serializable pre-funded channel state plus a resumable continuation.
2. A protocol-level retry design with idempotent or transactional RGB acceptance and deterministic cleanup.

The peer-message thread also remains synchronously occupied by `handle_funding`; releasing the peer mutex makes API reads responsive but does not make funding acceptance asynchronous.

## Evidence Logs

- `/tmp/live-88-original-local.log`
- `/tmp/live-88-one-response-local.log`
- `/tmp/live-88-production-local.log`
- `/tmp/rln-live-88-reuse-rerun.log`
- `/tmp/rln-live-88-pending-state.log`
- `/tmp/rln-live-88-no-reuse-pending-state-rerun.log`
- `/tmp/rln-pending-existing-channel-regression.log`

No experiment changes were committed or pushed.
