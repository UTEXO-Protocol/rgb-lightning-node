/**
 * Sign+finalize parity with rgb-lib: the SDK signs the SAME unsigned PSBT
 * that rgb-lib's own BDK signer signed in the fixture, and must produce the
 * same txid with a structurally identical (64-byte key-path schnorr) witness.
 * Also proves sign.ts refuses to sign anything verification rejects.
 */
import { base64, hex } from '@scure/base';
import { Transaction } from '@scure/btc-signer';
import { describe, expect, it } from 'vitest';
import { ClientKeys } from '../src/keys.js';
import { verifyAndSignPsbt, VerificationFailedError } from '../src/sign.js';
import { PSBT_PARSE_OPTIONS, type OnchainIntent } from '../src/verify.js';
import {
  addressFor,
  buildPsbt,
  fixture,
  foreign,
  OPRET_SCRIPT,
  ours,
  ownDerivation,
  VANILLA_PATH,
} from './helpers.js';

const keys = ClientKeys.fromMnemonic(fixture.mnemonic, 'Regtest');

const fixtureIntent: OnchainIntent = {
  kind: 'send_btc',
  feeRateSatPerVb: 2,
  recipients: [
    {
      address: addressFor(foreign.scriptAt(VANILLA_PATH, 0, 0)),
      scriptHex: hex.encode(foreign.scriptAt(VANILLA_PATH, 0, 0)),
      amountSat: 40_000,
    },
  ],
  asset: null,
  utxos: null,
};

describe('verifyAndSignPsbt', () => {
  it('signs the rgb-lib fixture PSBT to the same txid as rgb-lib', () => {
    const result = verifyAndSignPsbt(keys, fixture.signing.unsignedPsbt, {
      intent: fixtureIntent,
      xpubs: keys.xpubs,
      maxFeeSat: 2_000,
    });
    expect(result.verdict.ok).toBe(true);
    expect(result.txid).toBe(fixture.signing.txid);

    const signed = Transaction.fromPSBT(base64.decode(result.signedPsbt), PSBT_PARSE_OPTIONS);
    expect(signed.id).toBe(fixture.signing.txid);
    expect(signed.isFinal).toBe(true);
    const witness = signed.getInput(0).finalScriptWitness;
    expect(witness).toHaveLength(1);
    expect((witness as Uint8Array[])[0]).toHaveLength(64);

    // rgb-lib's own signed fixture agrees on the txid (witness bytes differ:
    // BIP-340 signatures are randomized).
    const rgbLibSigned = Transaction.fromPSBT(
      base64.decode(fixture.signing.signedPsbt),
      PSBT_PARSE_OPTIONS,
    );
    expect(rgbLibSigned.id).toBe(fixture.signing.txid);
    expect(
      hex.encode((rgbLibSigned.getInput(0).finalScriptWitness as Uint8Array[])[0] as Uint8Array),
    ).toBe(fixture.signing.witnessSignature);
  });

  it('refuses to sign when verification fails (wrong recipient amount)', () => {
    const tamperedIntent: OnchainIntent = {
      ...fixtureIntent,
      recipients: [
        { ...(fixtureIntent.recipients[0] as OnchainIntent['recipients'][0]), amountSat: 39_000 },
      ],
    };
    expect(() =>
      verifyAndSignPsbt(keys, fixture.signing.unsignedPsbt, {
        intent: tamperedIntent,
        xpubs: keys.xpubs,
        maxFeeSat: 2_000,
      }),
    ).toThrow(VerificationFailedError);
  });

  it('refuses to sign over budget and reports the failed check', () => {
    try {
      verifyAndSignPsbt(keys, fixture.signing.unsignedPsbt, {
        intent: fixtureIntent,
        xpubs: keys.xpubs,
        maxFeeSat: 500,
      });
      expect.unreachable('should have thrown');
    } catch (error) {
      expect(error).toBeInstanceOf(VerificationFailedError);
      const failed = (error as VerificationFailedError).verdict.checks.filter((c) => !c.ok);
      expect(failed.map((c) => c.check)).toEqual(['fee-budget']);
    }
  });

  it('refuses to sign a foreign input even when the intent matches', () => {
    const psbt = buildPsbt(
      [
        {
          script: foreign.scriptAt(VANILLA_PATH, 0, 0),
          amount: 100_000n,
          derivation: ownDerivation(0, 0),
        },
      ],
      [
        { script: foreign.scriptAt(VANILLA_PATH, 0, 0), amount: 40_000n },
        {
          script: ours.scriptAt(VANILLA_PATH, 0, 1),
          amount: 59_000n,
          derivation: ownDerivation(0, 1),
        },
      ],
    );
    expect(() =>
      verifyAndSignPsbt(keys, psbt, { intent: fixtureIntent, xpubs: keys.xpubs, maxFeeSat: 2_000 }),
    ).toThrow(VerificationFailedError);
  });

  it('signs through a decoy key-origin entry ordered before ours', () => {
    // verify.ts selects the entry carrying OUR master fingerprint; sign.ts must
    // select the same one. Picking by path SHAPE alone would take this decoy —
    // a foreign fingerprint on a well-formed path — and signing would fail
    // against a key no check ever covered.
    const psbt = buildPsbt(
      [
        {
          script: ours.scriptAt(VANILLA_PATH, 0, 0),
          amount: 100_000n,
          derivation: ownDerivation(0, 0),
          // Index 1 is chosen because its x-only key sorts BEFORE ours: PSBT
          // key-origin entries are keyed by pubkey, so this is the entry a
          // shape-only match would reach first.
          decoyDerivations: [
            {
              fingerprint: foreign.fingerprint,
              path: [...VANILLA_PATH, 0, 1],
              xOnly: foreign.xOnlyAt(VANILLA_PATH, 0, 1),
            },
          ],
          tapInternalKey: ours.xOnlyAt(VANILLA_PATH, 0, 0),
        },
      ],
      [
        { script: foreign.scriptAt(VANILLA_PATH, 0, 0), amount: 40_000n },
        {
          script: ours.scriptAt(VANILLA_PATH, 0, 1),
          amount: 59_000n,
          derivation: ownDerivation(0, 1),
        },
        { script: OPRET_SCRIPT, amount: 0n },
      ],
    );
    const result = verifyAndSignPsbt(keys, psbt, {
      intent: fixtureIntent,
      xpubs: keys.xpubs,
      maxFeeSat: 2_000,
    });
    expect(result.verdict.ok).toBe(true);
    const signed = Transaction.fromPSBT(base64.decode(result.signedPsbt), PSBT_PARSE_OPTIONS);
    expect(signed.isFinal).toBe(true);
    expect((signed.getInput(0).finalScriptWitness as Uint8Array[])[0]).toHaveLength(64);
  });

  it('refuses to sign an input carrying no key-origin entry of ours', () => {
    const psbt = buildPsbt(
      [
        {
          script: ours.scriptAt(VANILLA_PATH, 0, 0),
          amount: 100_000n,
          derivation: {
            fingerprint: foreign.fingerprint,
            path: [...VANILLA_PATH, 0, 0],
            xOnly: foreign.xOnlyAt(VANILLA_PATH, 0, 0),
          },
          tapInternalKey: ours.xOnlyAt(VANILLA_PATH, 0, 0),
        },
      ],
      [
        { script: foreign.scriptAt(VANILLA_PATH, 0, 0), amount: 40_000n },
        {
          script: ours.scriptAt(VANILLA_PATH, 0, 1),
          amount: 59_000n,
          derivation: ownDerivation(0, 1),
        },
        { script: OPRET_SCRIPT, amount: 0n },
      ],
    );
    // Fails at verification, never reaching the signer: the input is not
    // provably ours.
    expect(() =>
      verifyAndSignPsbt(keys, psbt, { intent: fixtureIntent, xpubs: keys.xpubs, maxFeeSat: 2_000 }),
    ).toThrow(VerificationFailedError);
  });

  it('never signs adversarial garbage', () => {
    expect(() =>
      verifyAndSignPsbt(keys, 'garbage-not-a-psbt', {
        intent: fixtureIntent,
        xpubs: keys.xpubs,
        maxFeeSat: 2_000,
      }),
    ).toThrow(VerificationFailedError);
  });
});
