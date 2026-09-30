/**
 * Adversarial verify-before-sign suite: each of the design doc's 5 checks
 * must catch its attack, and verifyPsbt must never throw on hostile input.
 */
import { hex } from '@scure/base';
import { describe, expect, it } from 'vitest';
import type { AccountXpubs } from '../src/keys.js';
import { verifyPsbt, type OnchainIntent, type VerifyVerdict } from '../src/verify.js';
import {
  addressFor,
  buildPsbt,
  COLORED_PATH,
  fixture,
  foreign,
  OPRET_SCRIPT,
  ours,
  ownDerivation,
  VANILLA_PATH,
} from './helpers.js';

const xpubs: AccountXpubs = {
  network: 'Regtest',
  fingerprint: fixture.networks.Regtest.masterFingerprint,
  vanilla: fixture.networks.Regtest.accountXpubVanilla,
  colored: fixture.networks.Regtest.accountXpubColored,
};

const recipientScript = foreign.scriptAt(VANILLA_PATH, 0, 0);

const sendBtcIntent: OnchainIntent = {
  kind: 'send_btc',
  feeRateSatPerVb: 2,
  recipients: [
    {
      address: addressFor(recipientScript),
      scriptHex: hex.encode(recipientScript),
      amountSat: 40_000,
    },
  ],
  asset: null,
  utxos: null,
};

/** Happy-path shape: own input, intended recipient, own change, 0-sat opret. */
function happyPsbt(overrides?: {
  changeScript?: Uint8Array;
  changeDerivation?: ReturnType<typeof ownDerivation> | undefined;
  changeAmount?: bigint;
  opretAmount?: bigint;
}): string {
  return buildPsbt(
    [
      {
        script: ours.scriptAt(VANILLA_PATH, 0, 0),
        amount: 100_000n,
        derivation: ownDerivation(0, 0),
        tapInternalKey: ours.xOnlyAt(VANILLA_PATH, 0, 0),
      },
    ],
    [
      { script: recipientScript, amount: 40_000n },
      {
        script: overrides?.changeScript ?? ours.scriptAt(VANILLA_PATH, 0, 1),
        amount: overrides?.changeAmount ?? 59_000n,
        derivation:
          overrides !== undefined && 'changeDerivation' in overrides
            ? overrides.changeDerivation
            : ownDerivation(0, 1),
      },
      { script: OPRET_SCRIPT, amount: overrides?.opretAmount ?? 0n },
    ],
  );
}

function check(verdict: VerifyVerdict, name: string): { ok: boolean; detail: string | null } {
  const result = verdict.checks.find((c) => c.check === name);
  if (result === undefined) throw new Error(`missing check ${name}`);
  return result;
}

describe('verify-before-sign: happy paths', () => {
  it('accepts a well-formed send-btc PSBT', () => {
    const verdict = verifyPsbt({
      psbt: happyPsbt(),
      intent: sendBtcIntent,
      xpubs,
      maxFeeSat: 2_000,
    });
    expect(verdict.checks.filter((c) => !c.ok)).toEqual([]);
    expect(verdict.ok).toBe(true);
    expect(verdict.feeSat).toBe(1_000);
    expect(verdict.txid).toMatch(/^[0-9a-f]{64}$/);
  });

  it('accepts colored-keychain inputs and change (asset send, blind)', () => {
    const psbt = buildPsbt(
      [
        {
          script: ours.scriptAt(COLORED_PATH, 0, 0),
          amount: 30_000n,
          derivation: ownDerivation(0, 0, true),
        },
      ],
      [
        {
          script: ours.scriptAt(COLORED_PATH, 0, 1),
          amount: 29_000n,
          derivation: ownDerivation(0, 1, true),
        },
        { script: OPRET_SCRIPT, amount: 0n },
      ],
    );
    const intent: OnchainIntent = {
      kind: 'send_asset',
      feeRateSatPerVb: 2,
      recipients: [],
      asset: {
        assetId: 'rgb:fixture',
        amount: 5,
        recipientId: 'bcrt:utxob:fixture',
        witnessAmountSat: null,
        transportEndpoints: ['rpc://localhost:3000/json-rpc'],
      },
      utxos: null,
    };
    const verdict = verifyPsbt({ psbt, intent, xpubs, maxFeeSat: 2_000 });
    expect(verdict.checks.filter((c) => !c.ok)).toEqual([]);
  });

  it('accepts exactly one foreign witness output of the approved amount', () => {
    const witnessIntent: OnchainIntent = {
      kind: 'send_asset',
      feeRateSatPerVb: 2,
      recipients: [],
      asset: {
        assetId: 'rgb:fixture',
        amount: 5,
        recipientId: 'bcrt:wvout:fixture',
        witnessAmountSat: 3_000,
        transportEndpoints: [],
      },
      utxos: null,
    };
    const psbt = buildPsbt(
      [
        {
          script: ours.scriptAt(COLORED_PATH, 0, 0),
          amount: 30_000n,
          derivation: ownDerivation(0, 0, true),
        },
      ],
      [
        { script: foreign.scriptAt(VANILLA_PATH, 0, 3), amount: 3_000n },
        {
          script: ours.scriptAt(COLORED_PATH, 0, 1),
          amount: 26_000n,
          derivation: ownDerivation(0, 1, true),
        },
        { script: OPRET_SCRIPT, amount: 0n },
      ],
    );
    const verdict = verifyPsbt({ psbt, intent: witnessIntent, xpubs, maxFeeSat: 2_000 });
    expect(verdict.checks.filter((c) => !c.ok)).toEqual([]);

    // The same PSBT under a BLIND intent must fail: no foreign output allowed.
    const blindIntent: OnchainIntent = {
      ...witnessIntent,
      asset: {
        ...(witnessIntent.asset as NonNullable<OnchainIntent['asset']>),
        witnessAmountSat: null,
      },
    };
    const blindVerdict = verifyPsbt({ psbt, intent: blindIntent, xpubs, maxFeeSat: 2_000 });
    expect(check(blindVerdict, 'change-own').ok).toBe(false);
  });

  it('accepts metadata-proven change beyond the scan window at a sane index', () => {
    const psbt = happyPsbt({
      changeScript: ours.scriptAt(VANILLA_PATH, 0, 40),
      changeDerivation: ownDerivation(0, 40),
    });
    const verdict = verifyPsbt({ psbt, intent: sendBtcIntent, xpubs, maxFeeSat: 2_000 });
    expect(verdict.checks.filter((c) => !c.ok)).toEqual([]);
  });

  it('accepts a create-utxos PSBT whose outputs are all own, and catches a tampered one', () => {
    const utxosIntent: OnchainIntent = {
      kind: 'create_utxos',
      feeRateSatPerVb: 2,
      recipients: [],
      asset: null,
      utxos: { upTo: false, num: 2, size: 1000 },
    };
    const ownOutputs = [
      {
        script: ours.scriptAt(VANILLA_PATH, 0, 1),
        amount: 1_000n,
        derivation: ownDerivation(0, 1),
      },
      {
        script: ours.scriptAt(VANILLA_PATH, 0, 2),
        amount: 1_000n,
        derivation: ownDerivation(0, 2),
      },
      {
        script: ours.scriptAt(VANILLA_PATH, 0, 3),
        amount: 97_000n,
        derivation: ownDerivation(0, 3),
      },
    ];
    const input = {
      script: ours.scriptAt(VANILLA_PATH, 0, 0),
      amount: 100_000n,
      derivation: ownDerivation(0, 0),
    };
    const good = verifyPsbt({
      psbt: buildPsbt([input], ownOutputs),
      intent: utxosIntent,
      xpubs,
      maxFeeSat: 2_000,
    });
    expect(good.checks.filter((c) => !c.ok)).toEqual([]);

    const tampered = verifyPsbt({
      psbt: buildPsbt(
        [input],
        [
          ...ownOutputs.slice(0, 2),
          { script: foreign.scriptAt(VANILLA_PATH, 0, 3), amount: 97_000n },
        ],
      ),
      intent: utxosIntent,
      xpubs,
      maxFeeSat: 2_000,
    });
    expect(check(tampered, 'change-own').ok).toBe(false);
  });

  it('accepts metadata-less own change via the scan window, refuses with window 0', () => {
    const psbt = happyPsbt({ changeDerivation: undefined });
    const withScan = verifyPsbt({ psbt, intent: sendBtcIntent, xpubs, maxFeeSat: 2_000 });
    expect(withScan.ok).toBe(true);
    const noScan = verifyPsbt({
      psbt,
      intent: sendBtcIntent,
      xpubs,
      maxFeeSat: 2_000,
      changeScanWindow: 0,
    });
    expect(check(noScan, 'change-own').ok).toBe(false);
  });
});

describe('verify-before-sign: adversarial cases', () => {
  it('check 1 — foreign input claiming our fingerprint fails re-derivation', () => {
    const psbt = buildPsbt(
      [
        {
          // Attacker spends THEIR utxo but labels it with our fingerprint/path.
          script: foreign.scriptAt(VANILLA_PATH, 0, 0),
          amount: 100_000n,
          derivation: ownDerivation(0, 0),
        },
      ],
      [
        { script: recipientScript, amount: 40_000n },
        {
          script: ours.scriptAt(VANILLA_PATH, 0, 1),
          amount: 59_000n,
          derivation: ownDerivation(0, 1),
        },
      ],
    );
    const verdict = verifyPsbt({ psbt, intent: sendBtcIntent, xpubs, maxFeeSat: 2_000 });
    expect(check(verdict, 'inputs-own').ok).toBe(false);
    expect(check(verdict, 'inputs-own').detail).toContain('does not re-derive');
    expect(verdict.ok).toBe(false);
  });

  it('check 1 — input without our key origin is foreign', () => {
    const psbt = buildPsbt(
      [
        {
          script: ours.scriptAt(VANILLA_PATH, 0, 0),
          amount: 100_000n,
          derivation: {
            fingerprint: foreign.fingerprint,
            path: [...VANILLA_PATH, 0, 0],
            xOnly: ours.xOnlyAt(VANILLA_PATH, 0, 0),
          },
        },
      ],
      [{ script: recipientScript, amount: 40_000n }],
    );
    const verdict = verifyPsbt({ psbt, intent: sendBtcIntent, xpubs, maxFeeSat: 100_000 });
    expect(check(verdict, 'inputs-own').detail).toContain('foreign input');
  });

  it('check 1 — forged tapInternalKey mismatch fails closed at parse', () => {
    // btc-signer refuses to CONSTRUCT this PSBT (the key does not commit to
    // the spent script), so it is forged with sanity checks disabled; the
    // SDK's strict parse then rejects it and verification fails closed.
    const psbt = buildPsbt(
      [
        {
          script: ours.scriptAt(VANILLA_PATH, 0, 0),
          amount: 100_000n,
          derivation: ownDerivation(0, 0),
          tapInternalKey: ours.xOnlyAt(VANILLA_PATH, 0, 4),
        },
      ],
      [
        { script: recipientScript, amount: 40_000n },
        {
          script: ours.scriptAt(VANILLA_PATH, 0, 1),
          amount: 59_000n,
          derivation: ownDerivation(0, 1),
        },
      ],
      { forgeInvalid: true },
    );
    const verdict = verifyPsbt({ psbt, intent: sendBtcIntent, xpubs, maxFeeSat: 2_000 });
    expect(verdict.ok).toBe(false);
    expect(verdict.checks.every((c) => !c.ok)).toBe(true);
    expect(check(verdict, 'inputs-own').detail).toContain('could not be safely parsed');
  });

  it('check 2 — swapped recipient script is caught', () => {
    const swapped = buildPsbt(
      [
        {
          script: ours.scriptAt(VANILLA_PATH, 0, 0),
          amount: 100_000n,
          derivation: ownDerivation(0, 0),
        },
      ],
      [
        // Attacker redirects the payment to a different script.
        { script: foreign.scriptAt(VANILLA_PATH, 0, 4), amount: 40_000n },
        {
          script: ours.scriptAt(VANILLA_PATH, 0, 1),
          amount: 59_000n,
          derivation: ownDerivation(0, 1),
        },
        { script: OPRET_SCRIPT, amount: 0n },
      ],
    );
    const verdict = verifyPsbt({ psbt: swapped, intent: sendBtcIntent, xpubs, maxFeeSat: 2_000 });
    expect(check(verdict, 'recipients-match').ok).toBe(false);
    expect(verdict.ok).toBe(false);
  });

  it('check 2 — hostile scriptHex paired with the intended address is caught', () => {
    // A compromised gateway shows the user the REAL recipient address but
    // pairs it with an attacker script and a PSBT paying that script. The
    // expected script must be re-derived from the address, never trusted.
    const attackerScript = foreign.scriptAt(VANILLA_PATH, 0, 4);
    const hostileIntent: OnchainIntent = {
      ...sendBtcIntent,
      recipients: [
        {
          address: addressFor(recipientScript),
          scriptHex: hex.encode(attackerScript),
          amountSat: 40_000,
        },
      ],
    };
    const psbt = buildPsbt(
      [
        {
          script: ours.scriptAt(VANILLA_PATH, 0, 0),
          amount: 100_000n,
          derivation: ownDerivation(0, 0),
        },
      ],
      [
        { script: attackerScript, amount: 40_000n },
        {
          script: ours.scriptAt(VANILLA_PATH, 0, 1),
          amount: 59_000n,
          derivation: ownDerivation(0, 1),
        },
        { script: OPRET_SCRIPT, amount: 0n },
      ],
    );
    const verdict = verifyPsbt({ psbt, intent: hostileIntent, xpubs, maxFeeSat: 2_000 });
    expect(check(verdict, 'recipients-match').ok).toBe(false);
    expect(check(verdict, 'recipients-match').detail).toContain('does not match');
    expect(verdict.ok).toBe(false);
  });

  it('check 2 — an undecodable intent address fails instead of being trusted', () => {
    const hostileIntent: OnchainIntent = {
      ...sendBtcIntent,
      recipients: [
        {
          address: 'not-an-address',
          scriptHex: hex.encode(recipientScript),
          amountSat: 40_000,
        },
      ],
    };
    const verdict = verifyPsbt({
      psbt: happyPsbt(),
      intent: hostileIntent,
      xpubs,
      maxFeeSat: 2_000,
    });
    expect(check(verdict, 'recipients-match').ok).toBe(false);
    expect(check(verdict, 'recipients-match').detail).toContain('does not decode');
  });

  it('check 2 — recipient amount tampering is caught', () => {
    const psbt = buildPsbt(
      [
        {
          script: ours.scriptAt(VANILLA_PATH, 0, 0),
          amount: 100_000n,
          derivation: ownDerivation(0, 0),
        },
      ],
      [
        { script: recipientScript, amount: 90_000n },
        {
          script: ours.scriptAt(VANILLA_PATH, 0, 1),
          amount: 9_000n,
          derivation: ownDerivation(0, 1),
        },
      ],
    );
    const verdict = verifyPsbt({ psbt, intent: sendBtcIntent, xpubs, maxFeeSat: 2_000 });
    expect(check(verdict, 'recipients-match').ok).toBe(false);
  });

  it('check 3 — tampered change (foreign script under our metadata) is caught', () => {
    const psbt = happyPsbt({ changeScript: foreign.scriptAt(VANILLA_PATH, 0, 1) });
    const verdict = verifyPsbt({ psbt, intent: sendBtcIntent, xpubs, maxFeeSat: 2_000 });
    expect(check(verdict, 'change-own').ok).toBe(false);
    expect(verdict.ok).toBe(false);
  });

  it('check 3 — "change" steered outside the descriptor keychain is caught', () => {
    // The script genuinely re-derives from the user's xpub at keychain 7, but
    // no descriptor wallet scans keychain 7 — accepting it would let a
    // hostile gateway strand the change where the user can never find it.
    const psbt = happyPsbt({
      changeScript: ours.scriptAt(VANILLA_PATH, 7, 0),
      changeDerivation: ownDerivation(7, 0),
    });
    const verdict = verifyPsbt({ psbt, intent: sendBtcIntent, xpubs, maxFeeSat: 2_000 });
    expect(check(verdict, 'change-own').ok).toBe(false);
    expect(verdict.ok).toBe(false);
  });

  it('check 3 — "change" at an unrecoverable index is caught', () => {
    const psbt = happyPsbt({
      changeScript: ours.scriptAt(VANILLA_PATH, 0, 900_000_000),
      changeDerivation: ownDerivation(0, 900_000_000),
    });
    const verdict = verifyPsbt({ psbt, intent: sendBtcIntent, xpubs, maxFeeSat: 2_000 });
    expect(check(verdict, 'change-own').ok).toBe(false);
    expect(verdict.ok).toBe(false);
  });

  it('check 2 — witness intent fails on zero or multiple candidate outputs', () => {
    const witnessIntent: OnchainIntent = {
      kind: 'send_asset',
      feeRateSatPerVb: 2,
      recipients: [],
      asset: {
        assetId: 'rgb:fixture',
        amount: 5,
        recipientId: 'bcrt:wvout:fixture',
        witnessAmountSat: 3_000,
        transportEndpoints: [],
      },
      utxos: null,
    };
    const input = {
      script: ours.scriptAt(COLORED_PATH, 0, 0),
      amount: 30_000n,
      derivation: ownDerivation(0, 0, true),
    };
    const change = {
      script: ours.scriptAt(COLORED_PATH, 0, 1),
      amount: 23_000n,
      derivation: ownDerivation(0, 1, true),
    };
    // Zero candidates: the only foreign output pays the WRONG amount.
    const wrongAmount = verifyPsbt({
      psbt: buildPsbt(
        [input],
        [{ script: foreign.scriptAt(VANILLA_PATH, 0, 3), amount: 2_999n }, change],
      ),
      intent: witnessIntent,
      xpubs,
      maxFeeSat: 5_000,
    });
    expect(check(wrongAmount, 'recipients-match').ok).toBe(false);
    expect(check(wrongAmount, 'recipients-match').detail).toContain('found 0');

    // Two candidates at the approved amount: ambiguous, must fail.
    const twoCandidates = verifyPsbt({
      psbt: buildPsbt(
        [input],
        [
          { script: foreign.scriptAt(VANILLA_PATH, 0, 3), amount: 3_000n },
          { script: foreign.scriptAt(VANILLA_PATH, 0, 4), amount: 3_000n },
          { ...change, amount: 22_000n },
        ],
      ),
      intent: witnessIntent,
      xpubs,
      maxFeeSat: 5_000,
    });
    expect(check(twoCandidates, 'recipients-match').ok).toBe(false);
    expect(check(twoCandidates, 'recipients-match').detail).toContain('found 2');
  });

  it('check 3 — extra undeclared output is caught', () => {
    const psbt = buildPsbt(
      [
        {
          script: ours.scriptAt(VANILLA_PATH, 0, 0),
          amount: 100_000n,
          derivation: ownDerivation(0, 0),
        },
      ],
      [
        { script: recipientScript, amount: 40_000n },
        {
          script: ours.scriptAt(VANILLA_PATH, 0, 1),
          amount: 54_000n,
          derivation: ownDerivation(0, 1),
        },
        { script: foreign.scriptAt(VANILLA_PATH, 0, 2), amount: 5_000n },
      ],
    );
    const verdict = verifyPsbt({ psbt, intent: sendBtcIntent, xpubs, maxFeeSat: 2_000 });
    expect(check(verdict, 'change-own').ok).toBe(false);
  });

  it('check 4 — inflated fee is caught', () => {
    const psbt = happyPsbt({ changeAmount: 20_000n }); // fee jumps to 40_000
    const verdict = verifyPsbt({ psbt, intent: sendBtcIntent, xpubs, maxFeeSat: 2_000 });
    expect(check(verdict, 'fee-budget').ok).toBe(false);
    expect(verdict.feeSat).toBe(40_000);
  });

  it('check 4 — zero or negative fee is rejected', () => {
    const psbt = happyPsbt({ changeAmount: 60_000n }); // outputs == inputs
    const verdict = verifyPsbt({ psbt, intent: sendBtcIntent, xpubs, maxFeeSat: 2_000 });
    expect(check(verdict, 'fee-budget').ok).toBe(false);
  });

  it('check 5 — OP_RETURN carrying sats is caught', () => {
    const psbt = happyPsbt({ opretAmount: 500n, changeAmount: 58_500n });
    const verdict = verifyPsbt({ psbt, intent: sendBtcIntent, xpubs, maxFeeSat: 2_000 });
    expect(check(verdict, 'opret-zero').ok).toBe(false);
    expect(check(verdict, 'opret-zero').detail).toContain('carries 500 sat');
    expect(verdict.ok).toBe(false);
  });
});

describe('verify-before-sign: adversarial parsing', () => {
  it.each([
    ['not base64', '!!!!not-base64!!!!'],
    ['random bytes', 'AAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=='],
    ['empty string', ''],
    ['truncated psbt magic', 'cHNidP8='],
  ])('never throws on %s', (_label, psbt) => {
    const verdict = verifyPsbt({ psbt, intent: sendBtcIntent, xpubs, maxFeeSat: 2_000 });
    expect(verdict.ok).toBe(false);
    expect(verdict.checks).toHaveLength(5);
    expect(verdict.checks.every((c) => !c.ok)).toBe(true);
  });

  it('fails closed on an inputless transaction', () => {
    const psbt = buildPsbt([], [{ script: recipientScript, amount: 40_000n }]);
    const verdict = verifyPsbt({ psbt, intent: sendBtcIntent, xpubs, maxFeeSat: 2_000 });
    expect(check(verdict, 'inputs-own').ok).toBe(false);
  });
});
