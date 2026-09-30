# @utexo/minimal-client-sdk

Browser/mobile TypeScript SDK for the minimal gateway. No wasm, no chain sync, no LN
state — just key management, verify-before-sign PSBT handling, invoice decoding, and a
typed gateway client. Built on audited primitives (`@scure/bip39`, `@scure/bip32`,
`@scure/btc-signer`); the production bundle is gated below 500KB by the test suite.

**The mnemonic is the user's only backup.** All key material is generated and stays on
the client (invariant I1); the gateway only ever sees account xpubs and the master
fingerprint. The client is stateless between sessions — there is no server-side or
VSS-encrypted key backup, so if the user loses the mnemonic, the vault funds are
unrecoverable. Show it once at generation time and make the user store it.

## Quickstart

```ts
import {
  ClientKeys,
  GatewayClient,
  generateIdempotencyKey,
  verifyAndSignPsbt,
} from '@utexo/minimal-client-sdk';

// 1. Generate keys client-side. Display `mnemonic` for backup — it never
//    leaves the device. Restore later with ClientKeys.fromMnemonic(...).
const { mnemonic, keys } = ClientKeys.generate('Mainnet');

// 2. Connect to the gateway. The bearer token comes from the operator-run
//    user bootstrap (POST /v1/users) and is returned exactly once.
const client = new GatewayClient({ baseUrl: 'https://gateway.example.com', token });

// 3. Register the account xpubs; the gateway builds a per-user watch-only
//    wallet from them and returns the first vault address.
const { address } = await client.registerXpubs({
  vanilla: keys.xpubs.vanilla,
  colored: keys.xpubs.colored,
  fingerprint: keys.xpubs.fingerprint,
});

// 4. Receive RGB assets: hand `invoice` to the payer, then watch
//    client.getTransfers() / client.getBalances() for settlement.
const receive = await client.receive({ mode: 'blind' });

// 5. Send BTC — the server prepares, the CLIENT verifies and signs, the
//    server broadcasts. verifyAndSignPsbt refuses to sign (throws
//    VerificationFailedError) unless every check passes; there is no bypass.
const prepared = await client.prepareSendBtc({
  address: 'bc1p…',
  amountSat: 50_000,
  feeRateSatPerVb: 2,
});
const { signedPsbt } = verifyAndSignPsbt(keys, prepared.psbt, {
  intent: prepared.intent, // machine-readable intent from the prepare response
  xpubs: keys.xpubs,
  maxFeeSat: 1_000, // the user-approved fee budget
});
const { txid } = await client.completeSendBtc({ opId: prepared.opId, signedPsbt });

// 6. Pay a BOLT11 invoice from the custodial LN float. Money-moving calls
//    take an idempotency key: replaying the same key + body returns the
//    cached response for any completed outcome and never double-pays. After a
//    5xx the response is deliberately not cached, and the gateway reports the
//    recorded state (409 PAYMENT_ALREADY_ATTEMPTED / WITHDRAWAL_UNRESOLVED)
//    instead of re-sending — it never auto-retries a send.
const paid = await client.payLnInvoice({ invoice: 'lnbc…' }, generateIdempotencyKey());
```

If a `complete` call is lost — a timeout, a crash, or a 502 `COMPLETE_AMBIGUOUS` — do not
prepare a replacement. Read the operation back instead: `getOnchainOperation(opId)` is an
unqueued, key-free read, so it answers even while the completion is still running. A
`mayHaveBroadcast: true` response means the transaction may already be on the network;
retry `complete` to finish the bookkeeping rather than sending again.

Asset sends (`prepareSendAsset`/`completeSendAsset`) and colorable-UTXO creation
(`prepareCreateUtxos`/`completeCreateUtxos`) follow the same prepare → verify-and-sign →
complete shape. LN float deposits (`prepareLnDeposit`), invoices (`createLnInvoice`,
`getLnInvoice`), payment listing (`listLnPayments`), balances (`getLnBalance`) and
withdrawals (`withdrawLn`) round out the surface; `decodeBolt11`/`decodeRgbInvoice`
decode invoices locally for display before paying.

## The verify-before-sign contract

Signing is never blind. `verifyAndSignPsbt` runs `verifyPsbt` internally and refuses to
sign unless the verdict is clean; `verifyPsbt` can also be called on its own to show the
user what they are about to authorize. The five checks (from the design doc,
`docs/design/minimal-sdk-and-lightweight-rln.md`):

1. **Every input is ours** — each input's script re-derives from the client's own
   descriptors, and the PSBT's key-origin fingerprint/path matches the user's master
   fingerprint.
2. **Recipient outputs match intent** — script and amount, checked against the intent
   summary returned by the prepare call. That summary is server-produced, so on its own
   it would prove nothing against a hostile gateway: `prepareSendBtc`, `prepareSendAsset`
   and `prepareCreateUtxos` therefore assert it field-by-field against the request you
   passed and raise `IntentMismatchError` on any divergence, before the intent ever
   reaches `verifyPsbt`. Build the intent some other way and that binding is yours to
   make — verify it against what the user approved.
3. **Change pays only us, at a recoverable path** — every non-recipient output re-derives
   from the client's own vanilla or colored descriptor, at keychain 0 and index at most
   `maxOwnOutputIndex` (default 10 000). The index bound matters: any index under the
   account xpub is technically the user's, but only indexes a descriptor wallet actually
   scans are recoverable, so a hostile gateway must not be able to park "change" at index
   10⁸. Outputs carrying no key-origin metadata are accepted only if they match a script
   re-derived within the first `changeScanWindow` indexes (default 30; set it to 0 to
   require metadata on every output). Both are `VerifyParams` knobs.
4. **Fee within budget** — inputs − outputs is at most the user-approved `maxFeeSat`.
5. **OP_RETURN carries 0 sats** — the RGB opret commitment output may exist, but it may
   not carry value.

`verifyPsbt` returns a structured verdict (per-check pass/fail with detail) and never
throws on adversarial input — a malformed PSBT is a failed verdict, not a crash.

Known limitation (documented, not solved at this layer): the checks verify all
bitcoin-value movement, but the RGB opret commitment itself is opaque to the client, so
every server-prepared **colored** spend trusts the server for the RGB state transition at
send time. See "The custody boundary, honestly" in the gateway README for the full
statement and mitigations.
