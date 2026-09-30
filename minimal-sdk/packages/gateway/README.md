# @utexo/minimal-gateway

TypeScript API gateway in front of ONE shared RLN instance. It owns per-user auth,
per-user FIFO queues with a global concurrency cap, idempotency keys on money-moving
routes, per-user **watch-only** rgb-lib wallets for PSBT preparation, a capped-float
ledger for the custodial LN working balance, and the user↔resource scoping map. Every
RLN list/get response is filtered through that map before leaving the gateway — RLN
state is node-wide and is never proxied raw.

Design authority: `docs/design/minimal-sdk-and-lightweight-rln.md` (repo root). Trust
invariants: no secret ever reaches the gateway (I1), one watch-only wallet per user
(I2), no cross-user visibility (I3), only the gateway holds the RLN credential (I4).

## Running

Requirements: Node 20+, pnpm, and a platform with a published rgb-lib native module —
`linux-x64`, `linux-arm64` or `darwin-arm64` (`src/wallets/rgblib.ts`). Elsewhere the
gateway still starts, but every wallet operation fails at first use with
`rgb-lib native module unavailable`.

```sh
RLN_URL=http://127.0.0.1:3101 \
ESPLORA_URL=http://127.0.0.1:3002 \
RGB_PROXY_URL=rpc://127.0.0.1:3000/json-rpc \
GATEWAY_SQLITE_PATH=/var/lib/minimal-gateway/gateway.sqlite \
GATEWAY_WALLETS_DIR=/var/lib/minimal-gateway/wallets \
GATEWAY_WALLET_INDEXER_URL=127.0.0.1:50001 \
GATEWAY_OPERATOR_TOKEN=<random, 16+ chars> \
node dist/main.js   # or: pnpm --filter @utexo/minimal-gateway start
```

The production entrypoint (`src/main.ts`) runs `loadConfig` → `buildServer` with the
background workers (deposits watcher + payments reconciler) → `listen`. `buildServer`
starts the workers only when called with `startWorkers: true` — `main.ts` does this; a
library embedding must opt in explicitly.

Users are created by the operator via `POST /v1/users` (guarded by the operator token);
the response contains the user's opaque bearer token exactly once — only its hash is
stored.

## API

Auth is one of: the operator token (`x-operator-token` header), the user's bearer token
(`Authorization: Bearer <token>`), or none. Money-moving routes additionally require an
`Idempotency-Key` header (400 without it): a replay of the same key **and** the same
request body returns the stored response, and completed rows are garbage-collected after
7 days.

Two details matter for retries. Responses with status ≥ 500 or 429 are deliberately
**not** cached — the claim is released so the request can be retried — and an in-flight
claim whose request never stored a response is reclaimable after 15 minutes, but only by
a retry of the _same_ request (a different body under that key is always 409
`IDEMPOTENCY_KEY_REUSED`). After a 5xx on `/v1/ln/pay` or `/v1/ln/withdraw` the retry is
therefore answered by the flow's own replay guard rather than by a cached body: the
gateway reports the recorded state instead of sending again, because it never
auto-retries a send.

| Method | Path                                | Auth     | Idempotency-Key |
| ------ | ----------------------------------- | -------- | --------------- |
| GET    | `/v1/health`                        | none     | —               |
| POST   | `/v1/users`                         | operator | —               |
| GET    | `/v1/me`                            | bearer   | —               |
| POST   | `/v1/wallet/xpubs`                  | bearer   | —               |
| GET    | `/v1/wallet/address`                | bearer   | —               |
| GET    | `/v1/wallet/balances`               | bearer   | —               |
| GET    | `/v1/wallet/unspents`               | bearer   | —               |
| GET    | `/v1/wallet/transfers`              | bearer   | —               |
| POST   | `/v1/wallet/receive`                | bearer   | —               |
| POST   | `/v1/wallet/sync`                   | bearer   | —               |
| POST   | `/v1/onchain/send-btc/prepare`      | bearer   | required        |
| POST   | `/v1/onchain/send-btc/complete`     | bearer   | required        |
| POST   | `/v1/onchain/send-asset/prepare`    | bearer   | required        |
| POST   | `/v1/onchain/send-asset/complete`   | bearer   | required        |
| POST   | `/v1/onchain/create-utxos/prepare`  | bearer   | required        |
| POST   | `/v1/onchain/create-utxos/complete` | bearer   | required        |
| GET    | `/v1/onchain/operations/:opId`      | bearer   | —               |
| GET    | `/v1/onchain/fee-estimate`          | bearer   | —               |
| POST   | `/v1/ln/deposit/prepare`            | bearer   | required        |
| POST   | `/v1/ln/pay`                        | bearer   | required        |
| POST   | `/v1/ln/invoice`                    | bearer   | —               |
| GET    | `/v1/ln/invoice/:hash`              | bearer   | —               |
| GET    | `/v1/ln/payments`                   | bearer   | —               |
| GET    | `/v1/ln/balance`                    | bearer   | —               |
| POST   | `/v1/ln/withdraw`                   | bearer   | required        |

Every response carries an `x-request-id` header (a fresh random UUID per
request; a caller-supplied one is never adopted). Error bodies carry only `code`
and `message` — the RLN or wallet failure they were mapped from is logged under
that id and nowhere else, so it is the handle to quote when asking an operator
about a `COMPLETE_AMBIGUOUS` or `WITHDRAWAL_UNRESOLVED`.

### Errors

Every error response is `{"error": {"code", "message"}}`. Notable codes:

| Code                                     | Status    | Meaning                                                                                                                                                                                                                                        |
| ---------------------------------------- | --------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `QUEUE_FULL`                             | 429       | Per-user queue depth exceeded; a `retry-after` header accompanies it.                                                                                                                                                                          |
| `AMOUNT_REQUIRED`                        | 400       | BTC invoices must declare `amtMsat`, asset invoices `assetAmount` (their `amtMsat` HTLC carrier defaults to `GATEWAY_ASSET_INVOICE_MIN_MSAT`), and paying an amount-less invoice requires `amtMsat` (a float-cap bypass).                      |
| `AMOUNT_MISMATCH`                        | 400       | `amtMsat` or `assetAmount` conflicts with the amount encoded in the invoice.                                                                                                                                                                   |
| `AMOUNT_BELOW_MINIMUM`                   | 400       | An asset invoice's `amtMsat` HTLC carrier value is under `GATEWAY_ASSET_INVOICE_MIN_MSAT`; the node would refuse it.                                                                                                                           |
| `INVOICE_REJECTED`                       | 400       | The node rejected these invoice parameters. Detail stays server-side.                                                                                                                                                                          |
| `INVALID_INVOICE`                        | 400       | The BOLT11 could not be decoded, or its amount is zero / outside the safe-integer range.                                                                                                                                                       |
| `PAYMENT_REJECTED` / `WITHDRAW_REJECTED` | 400       | The node rejected this payment or withdrawal (an RLN 4xx). The debit is refunded. Detail stays server-side.                                                                                                                                    |
| `WITNESS_AMOUNT_REQUIRED`                | 400       | An RGB withdraw to a `wvout:` recipient needs `witnessAmountSat` (rgb-lib rejects a witness beneficiary without it). The node funds that output, so its sats are debited from the caller's msat float.                                         |
| `TRANSPORT_ENDPOINT_NOT_ALLOWED`         | 400       | Consignment endpoint outside `GATEWAY_RGB_TRANSPORT_ALLOWLIST` (SSRF guard).                                                                                                                                                                   |
| `WALLET_REQUEST_REJECTED`                | 400       | rgb-lib rejected the request itself — bad recipient id, asset id, address, fee rate or transport endpoint. Detail stays server-side.                                                                                                           |
| `INSUFFICIENT_FUNDS`                     | 400       | Not enough confirmed bitcoins, assets or allocation slots for an on-chain operation.                                                                                                                                                           |
| `PSBT_REJECTED` / `PSBT_MISMATCH`        | 400       | The signed PSBT was rejected, or does not correspond to the prepared operation.                                                                                                                                                                |
| `FLOAT_CAP_EXCEEDED`                     | 409       | Per-user or global float cap would be exceeded by this new exposure.                                                                                                                                                                           |
| `INSUFFICIENT_BALANCE`                   | 409       | The user's ledger balance is short.                                                                                                                                                                                                            |
| `IDEMPOTENCY_KEY_REUSED`                 | 409       | Same key, different request body.                                                                                                                                                                                                              |
| `IDEMPOTENCY_IN_FLIGHT`                  | 409       | A request with this key is still running.                                                                                                                                                                                                      |
| `PAYMENT_CONFLICT`                       | 409       | Another user is already paying this invoice.                                                                                                                                                                                                   |
| `PAYMENT_ALREADY_ATTEMPTED`              | 409       | This invoice already failed and was refunded — request a fresh invoice rather than re-paying an old one.                                                                                                                                       |
| `WITHDRAWAL_ALREADY_ATTEMPTED`           | 409       | A withdrawal under this key failed and was refunded — use a fresh key. (A rejection answered 4xx is instead replayed from the idempotency cache.)                                                                                              |
| `WITHDRAWAL_UNRESOLVED`                  | 409       | A withdrawal under this key is unresolved and may have broadcast; operator resolution required (see Known limitations).                                                                                                                        |
| `DEPOSIT_ADDRESS_CONFLICT`               | 502       | RLN returned a deposit address (or RGB `recipient_id`) already assigned to another intent — it is misconfigured (see the deployment note).                                                                                                     |
| `OP_EXPIRED`                             | 410       | The prepared on-chain operation passed `GATEWAY_ONCHAIN_OP_TTL_SECONDS`; prepare again.                                                                                                                                                        |
| `UPSTREAM_ERROR` / `UPSTREAM_TIMEOUT`    | 502 / 504 | Sanitized RLN failures.                                                                                                                                                                                                                        |
| `AMOUNT_NOT_REPRESENTABLE`               | 502       | The wallet holds an amount above 2^53−1, which JSON numbers cannot carry exactly. Permanent, and not the caller's fault: reported rather than silently rounded (see Known limitations).                                                        |
| `FEE_ESTIMATE_UNAVAILABLE`               | 502       | The indexer gave no usable fee estimate. Detail stays server-side.                                                                                                                                                                             |
| `COMPLETE_AMBIGUOUS`                     | 502       | The wallet failed while completing an on-chain op, after rgb-lib may already have broadcast. The op stays pending with its txid recorded; read it back with `GET /v1/onchain/operations/:opId` and retry `complete` to finish the bookkeeping. |

## What the wallet reads carry for a verifying client

A client that keeps its own keys has to be able to check what the gateway tells it, and to
act on RGB state afterwards. Three things in the wallet responses exist for that:

- `GET /v1/wallet/address` returns `derivation` — `account`, `keychain`, `index`,
  `derivationPath` and `scriptHex` — so the address can be re-derived from the client's own
  seed before it is shown as a deposit target. `null` means the gateway could not attribute
  it; see Known limitations.
- `GET /v1/wallet/transfers` and `POST /v1/wallet/receive` return `batchTransferIdx`.
  rgb-lib keys refresh, fail and delete by the **batch** index, not by `idx`, so without it
  a client cannot address the transfer it just read or created.
- `GET /v1/wallet/unspents` returns `pendingBlinded` per UTXO, and every allocation carries
  its full `assignment` (`{kind, amount}`) alongside the fungible `amount` summary. Free
  allocation slots cannot be counted from `allocations` alone — a pending blind receive
  reserves a slot before it holds an allocation — and a non-fungible assignment has no
  `amount` to summarize.

`assignment.kind` is the rgb-lib variant name verbatim (`Fungible`, `Any`,
`InflationRight`, `NonFungible`, ...), so a variant added upstream arrives named rather
than dropped.

## Fee estimates

`GET /v1/onchain/fee-estimate?blocks=N` (default `N=6`, range 1..1008) answers
`{blocks, feeRateSatPerVb, sourceBlocks}` from the shared indexer. Clients cannot reach
esplora themselves — it sits behind the gateway with RLN and the RGB proxy — so without
this they either hardcode a rate or send none and take the 2 sat/vB default.

`feeRateSatPerVb` is a whole sat/vB in the same 1..1000 range every fee-taking route
accepts, so it can be passed straight back. Two roundings are deliberate, both away from
under-paying: the fractional rate is rounded **up**, and a target the indexer does not
publish resolves to the nearest published target **at or below** it (the more urgent, so
never cheaper, neighbour) — which `sourceBlocks` reports. The answer is cached for 30s, so
a burst of clients is one indexer call. It needs no `Idempotency-Key`, no registered
wallet, and is not queued behind the per-user wallet work: a client needs a fee rate
_before_ it can prepare anything.

## Recovering a lost on-chain completion

`complete` can leave a client without an answer: a client-side timeout, a crash, or the
502 `COMPLETE_AMBIGUOUS` the gateway returns when the wallet fails _after_ rgb-lib may
already have broadcast (rgb-lib broadcasts before it writes its bookkeeping). The
operation row is the durable record of what happened, and
`GET /v1/onchain/operations/:opId` reads it back.

```jsonc
{
  "opId": "…", "kind": "send_btc",
  "state": "pending",              // pending | completed | expired
  "txid": "…",                     // final when completed; possibly-broadcast otherwise
  "mayHaveBroadcast": true,        // state != completed AND a txid is recorded
  "intent": { … },                 // the same summary prepare returned
  "createdAt": 1700000000000, "expiresAt": 1700000900000
}
```

How to read it:

| `state`     | `mayHaveBroadcast` | Meaning                                           | Do                                                                                      |
| ----------- | ------------------ | ------------------------------------------------- | --------------------------------------------------------------------------------------- |
| `completed` | `false`            | Done; `txid` is final                             | nothing                                                                                 |
| `pending`   | `false`            | Prepared, never broadcast                         | sign and `complete`, or let it expire                                                   |
| `pending`   | `true`             | **The transaction may already be on the network** | nothing — **the gateway finishes this itself** (below); poll until it reads `completed` |
| `expired`   | `true`             | Past TTL while ambiguous                          | same: the gateway still finishes it, and completion is not blocked by the TTL           |

Three properties worth relying on. It takes **no** `Idempotency-Key` and is not queued
behind the per-user wallet work, so it answers even while the `complete` you are asking
about is still running — safe to poll. It never echoes the PSBT; the client already holds
it from `prepare`. And an operation belonging to another user is reported as `404
OP_NOT_FOUND`, identical to one that does not exist (I3).

Expiry is **derived** on read, never written: an op past `expiresAt` reports `expired`
here while the stored row stays `pending` until it is completed.

### The gateway resolves ambiguous completions itself

A client that gets a 502 is entitled never to retry: a 5xx is not proof the send failed,
and a retry it cannot prove safe could, for all it knows, send twice. So the side that
_can_ prove it is safe does it. On the ambiguous path the gateway retains the signed PSBT
next to the recorded txid, and a background worker (`src/workers/completions.ts`) replays
the wallet's end-call until the bookkeeping lands. Replaying is safe because rgb-lib
re-broadcasts and treats a transaction the indexer already knows as success, so the retry
only redoes the write that failed.

Three boundaries on that:

- It only ever replays a PSBT **the client itself already submitted** to `complete`, for
  an op whose txid is already recorded. It never prepares, re-signs, or broadcasts
  anything the client did not authorize, and an op that never reached broadcast (no txid)
  is left alone.
- The retained PSBT must still be the transaction the row's txid names, checked before any
  wallet call; otherwise the op is left for an operator rather than completed under a
  transaction its intent does not describe.
- It stops after `GATEWAY_COMPLETIONS_MAX_ATTEMPTS`. An op that runs out of attempts stays
  `pending` with its txid and needs a human: check the node for the recorded txid, then
  mark the row completed or resolve it by hand. `CompletionsWorker.unresolved()` lists
  exactly those rows and is what a health check should alert on.

The op TTL deliberately does **not** apply here. The TTL exists so an _unsigned_ PSBT
stops being completable; an ambiguous op's transaction is signed and probably confirmed,
and refusing to record that after ten minutes would strand the bookkeeping for money that
already moved.

## Configuration reference

All configuration comes from environment variables (`src/config.ts`).

Required:

| Variable                     | Meaning                                                                       |
| ---------------------------- | ----------------------------------------------------------------------------- |
| `RLN_URL`                    | Base URL of the single shared RLN instance (reachable only from the gateway). |
| `ESPLORA_URL`                | Esplora REST API; used by the deposits watcher to confirm BTC deposits.       |
| `RGB_PROXY_URL`              | RGB proxy endpoint handed to receive-invoice transport endpoints.             |
| `GATEWAY_SQLITE_PATH`        | Path to the gateway SQLite database (`:memory:` for tests).                   |
| `GATEWAY_WALLETS_DIR`        | Gateway-owned directory holding one watch-only wallet data-dir per user.      |
| `GATEWAY_WALLET_INDEXER_URL` | Electrum `host:port` passed to rgb-lib `goOnline` for user wallets.           |
| `GATEWAY_OPERATOR_TOKEN`     | Guards the bootstrap-only `POST /v1/users` route. Minimum 16 characters.      |

Optional (default in parentheses):

| Variable                                         | Meaning                                                                                                                                                                                                                                              |
| ------------------------------------------------ | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `GATEWAY_HOST` (`127.0.0.1`)                     | Interface the gateway listens on.                                                                                                                                                                                                                    |
| `GATEWAY_PORT` (`8480`)                          | Gateway HTTP port.                                                                                                                                                                                                                                   |
| `RLN_ADMIN_TOKEN` (empty)                        | Biscuit admin token; empty when RLN runs with `--disable-authentication` on localhost. Never logged, never echoed (I4).                                                                                                                              |
| `GATEWAY_BITCOIN_NETWORK` (`Regtest`)            | rgb-lib network for user wallets: `Mainnet`, `Testnet`, `Testnet4`, `Signet`, `SignetCustom`, `Regtest`. **`Signet` means Bitcoin's _default_ signet** — see the indexer note below.                                                                 |
| `GATEWAY_WALLET_MAX_OPEN` (`32`)                 | LRU capacity of the open-wallet pool.                                                                                                                                                                                                                |
| `GATEWAY_ONCHAIN_OP_TTL_SECONDS` (`600`)         | Seconds an unsigned prepare-step PSBT stays completable before expiring.                                                                                                                                                                             |
| `GATEWAY_FLOAT_CAP_PER_USER_MSAT` (`1000000000`) | Per-user ceiling on the custodial LN float.                                                                                                                                                                                                          |
| `GATEWAY_FLOAT_CAP_GLOBAL_MSAT` (`10000000000`)  | Global ceiling on the custodial LN float.                                                                                                                                                                                                            |
| `GATEWAY_DEPOSIT_MIN_CONFIRMATIONS` (`1`)        | Confirmations before a BTC/RGB deposit is credited to the ledger.                                                                                                                                                                                    |
| `GATEWAY_DEPOSIT_TTL_SECONDS` (`86400`)          | Seconds an unfunded deposit intent stays pending (and counts against the float caps) before expiring.                                                                                                                                                |
| `GATEWAY_ASSET_INVOICE_MIN_MSAT` (`3000000`)     | HTLC carrier value (msat) put on asset invoices that declare no `amtMsat`. Must match the node's `channels.htlc_min_msat` (or the higher `inbound_htlc_minimum_msat` of the asset's channels) — RLN refuses an asset invoice below that floor.       |
| `GATEWAY_RGB_TRANSPORT_ALLOWLIST` (empty)        | Comma-separated extra RGB consignment-proxy endpoints users may pin in withdraw/send-asset requests. `RGB_PROXY_URL` is always allowlisted; anything else is rejected with 400 `TRANSPORT_ENDPOINT_NOT_ALLOWED` (SSRF guard — the node dials these). |
| `GATEWAY_DEPOSITS_INTERVAL_MS` (`10000`)         | Poll interval of the deposits watcher.                                                                                                                                                                                                               |
| `GATEWAY_COMPLETIONS_INTERVAL_MS` (`30000`)      | Poll interval of the ambiguous-completion worker.                                                                                                                                                                                                    |
| `GATEWAY_COMPLETIONS_MAX_ATTEMPTS` (`5`)         | Attempts at finishing one ambiguous operation before it is left to an operator.                                                                                                                                                                      |
| `GATEWAY_RECONCILER_INTERVAL_MS` (`10000`)       | Poll interval of the payments reconciler.                                                                                                                                                                                                            |
| `GATEWAY_RECONCILER_GRACE_SECONDS` (`600`)       | Age before the reconciler refunds a debited-pending send RLN does not know about (the send may still be queued behind RLN's global lock).                                                                                                            |
| `GATEWAY_QUEUE_GLOBAL_CONCURRENCY` (`4`)         | Max downstream operations running at once across all users.                                                                                                                                                                                          |
| `GATEWAY_QUEUE_PER_USER_DEPTH` (`16`)            | Max queued+running operations per user before 429.                                                                                                                                                                                                   |

## Deployment note: RLN is gateway-only

RLN is single-tenant and its API is node-wide: anyone who can reach it can see and move
everything. It must therefore be reachable **only from the gateway** — bind it to
localhost on the same host, or keep it on a private network segment with the gateway as
the sole peer. **Never expose RLN publicly.** The gateway holds the only RLN credential:
either a biscuit admin token (`RLN_ADMIN_TOKEN`), or RLN runs with
`--disable-authentication`, which is acceptable strictly on localhost behind the gateway
(invariant I4). The same applies to the esplora and RGB-proxy endpoints the node uses.

**RLN must not run with `--reuse-addresses`** (nor `reuse_addresses = true` in its
`config.toml`). The deposits watcher attributes on-chain funds by address, so every
deposit intent needs an address no earlier intent has used; a reused address would let
one payment credit several users' ledgers. The gateway refuses such an intent with 502
`DEPOSIT_ADDRESS_CONFLICT` — seeing that code means RLN is misconfigured. RGB deposit
intents are held to the same rule on the `recipient_id` the node blinds for them, for
the same reason (the watcher attributes RGB by recipient id).

## Indexer requirements

`GATEWAY_WALLET_INDEXER_URL` accepts **either** an Electrum server (`host:port`,
`ssl://host:port`) **or** an esplora REST base URL (`https://…`). The published rgb-lib
native module carries both the `electrum` and `esplora` features, and rgb-lib picks the
client by probing Electrum first and falling back to esplora.

The two are not equivalent, and esplora is the more forgiving:

|                                                        | Electrum resolver | esplora resolver |
| ------------------------------------------------------ | ----------------- | ---------------- |
| genesis hash matches the configured network            | required          | required         |
| a known probe transaction exists (default signet only) | required          | —                |
| verbose transactions supported                         | required          | —                |

So Blockstream/mempool `electrs` — the binary behind the esplora REST API — is **rejected
over its Electrum port** (`verbose transactions are currently unsupported`) while working
fine over its **esplora REST port**. Pointing this variable at the same esplora URL as
`ESPLORA_URL` is a valid and usually simpler deployment; a full Electrum server
(romanz/electrs, Fulcrum) is the alternative.

### `Signet` vs `SignetCustom`

`GATEWAY_BITCOIN_NETWORK=Signet` means Bitcoin's **default, public** signet. A signet
running its own challenge is `SignetCustom`. Every signet shares the same genesis block
hash, so the indexer check alone will NOT catch the difference when using esplora — but
the two are distinct chains to RGB and they tag invoices differently: `sb` for `Signet`,
`sbc` for `SignetCustom`. Getting this wrong yields a wallet that opens cleanly and then
produces RGB invoices no counterparty on the real chain will accept. Match whatever RLN
is running (`--network signetcustom`).

### Diagnosing a failed `POST /v1/wallet/xpubs`

Both mistakes surface as a wallet-open failure that names neither the variable nor the
fix:

```
RgbLib(InvalidIndexer { details: "resolver is for another chain-network pair" })
RgbLib(InvalidIndexer { details: "... verbose transactions are unsupported ..." })
```

If that route fails on a fresh deployment while every other route works, check these two
first. Two commands settle it:

```sh
# 1. Electrum URL only: does it support verbose transactions?
#    (result = usable, error = use the esplora URL instead, or a full Electrum server)
printf '{"jsonrpc":"2.0","id":1,"method":"blockchain.transaction.get","params":["<a txid on your chain>",true]}\n' | nc <host> <port>

# 2. Is this the DEFAULT signet? (404 = custom signet, so use SignetCustom)
curl -s -o /dev/null -w '%{http_code}\n' "$ESPLORA_URL/tx/8153034f45e695453250a8fb7225a5e545144071d8ed7b0d3211efa1f3c92ad8"
```

## Float-cap tuning

The LN working balance is custodial (see the custody boundary below), so the caps are
the loss bound: `GATEWAY_FLOAT_CAP_PER_USER_MSAT` bounds what any one user can lose if
the node or gateway is compromised, and `GATEWAY_FLOAT_CAP_GLOBAL_MSAT` bounds the total
custodial exposure of the deployment. Caps are enforced where new custodial exposure is
created — deposit prepare and invoice creation, counting pending intents so parallel
prepares cannot overshoot — while settlement credits and refunds are never blocked
(refusing them would strand funds the node already holds).

**The caps cover BTC only.** They are denominated in msat and are checked on the BTC
legs alone — `POST /v1/ln/deposit/prepare {kind:'rgb'}` and the `assetAmount` of an asset
invoice pass no headroom check, so RGB units enter the custodial float unbounded (only
the msat HTLC carrier of an asset invoice is capped). There is no meaningful common
denomination to cap heterogeneous RGB assets against, so per-asset ceilings are deferred
rather than guessed; until they land, the loss bound above is a bound on custodial
_bitcoin_, and RGB custodial exposure must be monitored out of band.

One consequence is worth stating plainly: the cap is checked against the amount
_declared_ at deposit-prepare time, while the watcher credits what actually confirmed on
chain. A user who declares a small deposit and then over-funds the returned address is
credited the full confirmed amount, so the caps bound _accepted intents_, not the
absolute custodial total. Refusing the excess would strand funds the node already holds,
so the trade-off is deliberate; monitor for it rather than relying on the cap alone.

Start small: set the global
cap to what the operation can afford to lose outright, the per-user cap to a routine
spending balance (defaults: 0.01 BTC per user, 0.1 BTC global), and revisit after
load-testing real usage. The global cap must also stay below the node's actual outbound
channel liquidity, or deposits will be accepted that cannot be spent.

## The custody boundary, honestly

Quoted verbatim from `docs/design/minimal-sdk-and-lightweight-rln.md`:

> Client-held keys cover **at-rest user funds**: on-chain BTC and RGB allocations on user
> UTXOs cannot move without a client signature (for RGB, subject to the send-time limitation
> above). They do **not** cover the Lightning working balance — an LN protocol constraint,
> not an implementation gap: channel funds are controlled by the channel keys and the shared
> node is their single owner, so balances inside its channels (in-flight HTLCs, routed
> float, funds awaiting sweep) are custodial.
>
> Mitigations, cheapest first (effort classes are estimates):
>
> | Mitigation                                                                                                                                                                                                        | Where                      | Effort       |
> | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | -------------------------- | ------------ |
> | Capped float: hard per-user + global ceiling on custodial LN balance                                                                                                                                              | gateway policy             | days         |
> | Auto-sweep: above a threshold, prepare on-chain sweep to user keys, client signs                                                                                                                                  | gateway + client SDK       | days         |
> | Submarine/atomic swaps for trust-minimized in/out via the existing swap API (`/makerinit`, `/makerexecute`, `/taker`, `src/main.rs:228-229,246`) and UTEXO thunder-swap                                           | gateway + existing RLN API | weeks        |
> | Per-user signer over a WebSocket `ExternalSignerTransport` (`src/signer/transport.rs:56`; today framed TCP + mTLS, `src/signer/remote/mod.rs`, browser-unreachable; `/initexternalsigner`: `src/main.rs:255-256`) | RLN + SDK                  | weeks        |
> | Per-user RLN nodes, or the wasm node in the browser (Block 2) — full self-custody                                                                                                                                 | infra / SDK                | weeks–months |

The "send-time limitation above" it references, also verbatim:

> Stated limitation — **send-time trust for RGB state**. The opret commitment bytes are
> opaque to the client, and in RGB the bitcoin-level spend of a colored input _is_ the
> authorization of whatever transition the server committed to it. The checks above verify
> all _bitcoin-value_ movement, but a compromised server can reassign **every RGB
> allocation on the spent inputs — amount sent and change alike — to seals it controls**,
> and such a transfer validates fine at its recipient: redirection (theft), not merely a
> burn, and the client cannot bound it (the server does coin selection). So RGB is safe
> **at rest** — nothing moves without a client signature — while every server-prepared
> colored spend exposes its inputs' allocations. Interim controls are server-side (hardened
> wallet service, independent consignment audit); removing the trust means shipping RGB
> validation client-side, i.e. the wasm variant.

Of the mitigation table, this gateway implements the first row (the capped float);
the rest are deliberately deferred (see the workspace README).

## Known limitations

- **Synchronous wallet FFI.** rgb-lib wallet calls are synchronous native FFI on the
  main thread — a slow indexer round-trip stalls all requests while it runs. Moving
  wallet calls onto a worker-thread pool is future work.
- **Fee subsidy.** LN routing fees and on-chain withdrawal fees are paid by the node,
  not debited from user balances — an operator subsidy whose exposure is bounded by the
  float caps. The _value_ the node pays out is not subsidized: an RGB withdraw to a
  `wvout:` recipient debits `witnessAmountSat` (in msat) from the caller's BTC float,
  because rgb-lib funds that new output from the node's on-chain wallet into a script the
  caller names — a withdraw the caller cannot cover answers 409 `INSUFFICIENT_BALANCE`.
- **One-shot deposit crediting.** A deposit intent expires after
  `GATEWAY_DEPOSIT_TTL_SECONDS`, and each intent credits exactly once — on the first
  confirmed poll. Later payments to the same address are not credited; prepare a fresh
  deposit per payment.
- **Amounts above 2^53−1 are reported, not rounded.** rgb-lib counts assets in u64 while
  JSON numbers are doubles, so a balance or assignment above `Number.MAX_SAFE_INTEGER`
  cannot round-trip. It is detected exactly (any such u64 decodes to at least 2^53) and
  answered with 502 `AMOUNT_NOT_REPRESENTABLE` rather than served as a wrong number.
  Carrying such amounts losslessly needs decimal-string fields on the wire, which is a
  breaking change to the wallet read routes and is deferred; until then an asset with a
  supply that large is unusable through this gateway rather than silently mis-reported.
- **Ambiguous withdrawals need manual resolution.** The completions worker covers the
  on-chain `pending_ops` table; the LN `withdrawals` table has no equivalent. The
  reconciler drives LN _payments_ to a terminal state but does not cover `withdrawals`. If RLN times out or
  answers 5xx mid-`/sendbtc`/`/sendrgb` — rgb-lib broadcasts before writing its
  bookkeeping, and every post-broadcast failure surfaces as a 500, so the transaction may
  already be on the network — the row is marked `ambiguous`, the debit stands, and
  same-key retries answer 409 `WITHDRAWAL_UNRESOLVED`. The operator must check the node
  (`/listtransactions`, `/listtransfers`) for a matching broadcast and then, in the
  gateway SQLite, either mark the row `sent` with its txid or mark it `failed` and post
  a compensating `refund` ledger entry. There is no admin API for this yet.
- **Address attribution is best-effort.** `GET /v1/wallet/address` reports the derivation
  it recovered by re-deriving candidates from the registered xpub and matching the script,
  because rgb-lib's `get_address` returns only the encoded address. The scan follows the
  user's own high-water mark plus a window, so it covers sequential use; a wallet restored
  far past that window, or an rgb-lib `reuse_addresses` deployment, reports `derivation:
null`. A client that needs ownership proof must treat null as unproven — it is the same
  position it was in when the route returned a bare string.
- **Unattributable RGB deposits stay pending.** If a settled RGB transfer for a deposit
  target carries no fungible assignment, the watcher cannot tell what was received and
  leaves the intent pending with a warning rather than crediting the declared amount
  (which would mint ledger units the node may not hold — RGB credits are not capped).
