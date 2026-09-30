/**
 * BIP-341 key-path sign + finalize via @scure/btc-signer. Refuses to sign
 * unless verify-before-sign passed: verification runs inside this module and
 * a failed verdict raises VerificationFailedError — there is no bypass.
 *
 * Keys are derived per input from the PSBT's own key-origin paths, but only
 * after verify.ts proved each path re-derives to the exact script being spent.
 */
import { base64 } from '@scure/base';
import { Transaction } from '@scure/btc-signer';
import type { ClientKeys } from './keys.js';
import {
  ownDerivation,
  PSBT_PARSE_OPTIONS,
  verifyPsbt,
  type TapDerivation,
  type VerifyParams,
  type VerifyVerdict,
} from './verify.js';

export class VerificationFailedError extends Error {
  constructor(readonly verdict: VerifyVerdict) {
    const failed = verdict.checks
      .filter((c) => !c.ok)
      .map((c) => `${c.check}${c.detail !== null ? ` (${c.detail})` : ''}`)
      .join('; ');
    super(`refusing to sign: verification failed: ${failed}`);
    this.name = 'VerificationFailedError';
  }
}

export class SigningError extends Error {
  constructor(detail: string) {
    super(`signing failed: ${detail}`);
    this.name = 'SigningError';
  }
}

export interface SignResult {
  /** Signed AND finalized PSBT, base64 — what the gateway's complete expects. */
  signedPsbt: string;
  txid: string;
  verdict: VerifyVerdict;
}

export type SignParams = Omit<VerifyParams, 'psbt'>;

/**
 * Verify the PSBT against the intent, then key-path-sign every input and
 * finalize. Throws VerificationFailedError when any of the 5 checks fails.
 */
export function verifyAndSignPsbt(
  keys: ClientKeys,
  psbtBase64: string,
  params: SignParams,
): SignResult {
  const verdict = verifyPsbt({ ...params, psbt: psbtBase64 });
  if (!verdict.ok) throw new VerificationFailedError(verdict);

  const ourFingerprint = Number.parseInt(keys.fingerprint, 16);
  const tx = Transaction.fromPSBT(base64.decode(psbtBase64), PSBT_PARSE_OPTIONS);
  for (let index = 0; index < tx.inputsLength; index += 1) {
    const input = tx.getInput(index);
    // Sign with the SAME entry verify.ts proved re-derives to the script being
    // spent — selected by our master fingerprint, not by path shape. Matching
    // on shape alone would pick a decoy entry carrying a foreign fingerprint
    // when the PSBT lists one first, and signing would then fail against a key
    // no check ever covered.
    const derivation = ownDerivation(input.tapBip32Derivation as TapDerivation[], ourFingerprint);
    const privateKey = derivation === null ? null : keys.privateKeyForPath(derivation[1].der.path);
    if (privateKey === null) {
      throw new SigningError(`input ${index}: no signable key-origin path`);
    }
    try {
      if (!tx.signIdx(privateKey, index)) {
        throw new SigningError(`input ${index}: signer declined the derived key`);
      }
    } catch (error) {
      if (error instanceof SigningError) throw error;
      throw new SigningError(
        `input ${index}: ${error instanceof Error ? error.message : String(error)}`,
      );
    }
  }
  try {
    tx.finalize();
  } catch (error) {
    throw new SigningError(`finalize: ${error instanceof Error ? error.message : String(error)}`);
  }
  return { signedPsbt: base64.encode(tx.toPSBT()), txid: tx.id, verdict };
}
