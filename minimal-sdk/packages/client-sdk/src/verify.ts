/**
 * Verify-before-sign: the design doc's 5 checks, evaluated against a parsed
 * PSBT and the gateway's machine-readable intent summary. Signing is never
 * blind — sign.ts refuses unless the verdict here is ok.
 *
 * All PSBT content (scripts, amounts, key-origin metadata) is treated as
 * adversarial. Ownership is proven by RE-DERIVING scripts from the user's own
 * account xpubs: a hostile server may supply any path it likes, but only a
 * path under the user's accounts can re-derive to a script the user controls.
 * This function never throws on malformed input — it returns a failed verdict.
 *
 * Known limitation (design doc, "send-time trust"): RGB allocations on the
 * spent inputs are NOT verifiable here; only bitcoin-value movement is.
 */
import { base64, hex } from '@scure/base';
import { Address, OutScript, Transaction } from '@scure/btc-signer';
import { deriveForOriginPath, deriveTaproot, parseAccountXpub } from './derive.js';
import type { AccountXpubs } from './keys.js';
import { addressNetwork, KEYCHAIN, type BitcoinNetwork } from './network.js';

/** Mirrors the gateway's prepare-response intent summary (kept in sync). */
export interface IntentRecipient {
  address: string;
  scriptHex: string;
  amountSat: number;
}

export interface IntentAsset {
  assetId: string;
  amount: number;
  recipientId: string;
  witnessAmountSat: number | null;
  transportEndpoints: string[];
}

export interface IntentUtxos {
  upTo: boolean;
  num: number;
  size: number;
}

export interface OnchainIntent {
  kind: 'send_btc' | 'send_asset' | 'create_utxos';
  feeRateSatPerVb: number;
  recipients: IntentRecipient[];
  asset: IntentAsset | null;
  utxos: IntentUtxos | null;
}

export type CheckName =
  'inputs-own' | 'recipients-match' | 'change-own' | 'fee-budget' | 'opret-zero';

const ALL_CHECKS: CheckName[] = [
  'inputs-own',
  'recipients-match',
  'change-own',
  'fee-budget',
  'opret-zero',
];

export interface CheckResult {
  check: CheckName;
  ok: boolean;
  detail: string | null;
}

export interface VerifyVerdict {
  ok: boolean;
  checks: CheckResult[];
  /** Absolute fee in sats, when computable. */
  feeSat: number | null;
  /** Txid of the unsigned transaction, when parseable. */
  txid: string | null;
}

export interface VerifyParams {
  /** Base64 PSBT from the gateway's prepare response. */
  psbt: string;
  /**
   * Intent summary from the same prepare response. It is SERVER-produced: the
   * caller must have bound it to the user's own request first, which
   * GatewayClient's prepare methods do (IntentMismatchError). Check 2 is only
   * as trustworthy as that binding.
   */
  intent: OnchainIntent;
  /** The user's own account xpubs + fingerprint (ClientKeys.xpubs). */
  xpubs: AccountXpubs;
  /** User-approved absolute fee budget in sats (check 4). */
  maxFeeSat: number;
  /**
   * Fallback for own outputs lacking key-origin metadata: accept scripts
   * re-derived at keychain 0, indexes 0..window-1, on both accounts.
   * 0 disables the fallback (metadata-only). Default 30.
   */
  changeScanWindow?: number;
  /**
   * Highest derivation index accepted for a metadata-proven own OUTPUT
   * (check 3). Any index under the account xpub re-derives, but only indexes
   * a descriptor wallet will actually scan are recoverable — without this
   * bound a hostile gateway could steer "change" to e.g. index 9e8, where the
   * funds are technically the user's yet invisible to every wallet scan.
   * Kept far above realistic per-user usage while cheap to sweep in a
   * recovery scan. Default 10000.
   */
  maxOwnOutputIndex?: number;
}

const DEFAULT_CHANGE_SCAN_WINDOW = 30;
const DEFAULT_MAX_OWN_OUTPUT_INDEX = 10_000;

export const PSBT_PARSE_OPTIONS = {
  allowUnknownInputs: true,
  allowUnknownOutputs: true,
} as const;

export type TapDerivation = [
  Uint8Array,
  { hashes: Uint8Array[]; der: { fingerprint: number; path: number[] } },
];

interface ParsedOutput {
  index: number;
  scriptHex: string;
  amountSat: number;
  isOpret: boolean;
  /** Proven ours by re-derivation (metadata path or scan window). */
  isOwn: boolean;
}

/** Output script (lowercase hex) for an address, or null if it does not decode. */
function scriptHexFromAddress(address: string, network: BitcoinNetwork): string | null {
  try {
    return hex.encode(OutScript.encode(Address(addressNetwork(network)).decode(address)));
  } catch {
    return null;
  }
}

function failAll(detail: string): VerifyVerdict {
  return {
    ok: false,
    checks: ALL_CHECKS.map((check) => ({ check, ok: false, detail })),
    feeSat: null,
    txid: null,
  };
}

/**
 * First metadata entry carrying the user's master fingerprint, if any.
 *
 * Exported because sign.ts MUST pick the signing key from the very entry this
 * proved: selecting by path shape alone would let a decoy entry with a foreign
 * fingerprint, ordered first, divert signing to a key the verdict never
 * covered.
 */
export function ownDerivation(
  derivations: TapDerivation[] | undefined,
  fingerprint: number,
): TapDerivation | null {
  for (const entry of derivations ?? []) {
    if (entry[1]?.der?.fingerprint === fingerprint) return entry;
  }
  return null;
}

export function verifyPsbt(params: VerifyParams): VerifyVerdict {
  try {
    return verifyParsed(params);
  } catch (error) {
    return failAll(
      `psbt could not be safely parsed: ${error instanceof Error ? error.message : String(error)}`,
    );
  }
}

function verifyParsed(params: VerifyParams): VerifyVerdict {
  const { intent, xpubs, maxFeeSat } = params;
  const network = xpubs.network;
  const ourFingerprint = Number.parseInt(xpubs.fingerprint, 16);
  const accounts = {
    vanilla: parseAccountXpub(xpubs.vanilla, network),
    colored: parseAccountXpub(xpubs.colored, network),
  };
  const tx = Transaction.fromPSBT(base64.decode(params.psbt), PSBT_PARSE_OPTIONS);
  const checks: CheckResult[] = [];

  // Check 1 — every input is ours: key-origin fingerprint matches AND the
  // path re-derives (from OUR xpubs) to the exact script being spent.
  const inputFailures: string[] = [];
  let inputTotalSat = 0;
  for (let i = 0; i < tx.inputsLength; i += 1) {
    const input = tx.getInput(i);
    const utxo = input.witnessUtxo;
    if (utxo === undefined) {
      inputFailures.push(`input ${i}: no witnessUtxo, cannot verify what is being spent`);
      continue;
    }
    inputTotalSat += Number(utxo.amount);
    const derivation = ownDerivation(input.tapBip32Derivation as TapDerivation[], ourFingerprint);
    if (derivation === null) {
      inputFailures.push(`input ${i}: no key origin with our fingerprint (foreign input)`);
      continue;
    }
    const derived = deriveForOriginPath(derivation[1].der.path, accounts, network);
    if (derived === null) {
      inputFailures.push(`input ${i}: key-origin path is not under our accounts`);
      continue;
    }
    if (derived.scriptHex !== hex.encode(utxo.script)) {
      inputFailures.push(`input ${i}: spent script does not re-derive from our keys`);
      continue;
    }
    if (
      input.tapInternalKey !== undefined &&
      hex.encode(input.tapInternalKey) !== derived.internalKeyHex
    ) {
      inputFailures.push(`input ${i}: tapInternalKey mismatch with re-derived key`);
    }
  }
  checks.push({
    check: 'inputs-own',
    ok: inputFailures.length === 0 && tx.inputsLength > 0,
    detail:
      tx.inputsLength === 0
        ? 'transaction has no inputs'
        : inputFailures.length > 0
          ? inputFailures.join('; ')
          : null,
  });

  // Own-script set for the metadata-less change fallback (keychain 0 only —
  // the single keychain rgb-lib uses on both accounts). Built lazily: real
  // rgb-lib PSBTs carry tapBip32Derivation on their change outputs, so the
  // common path must not pay for 2 * scanWindow key derivations.
  const scanWindow = params.changeScanWindow ?? DEFAULT_CHANGE_SCAN_WINDOW;
  let scannedOwnScripts: Set<string> | undefined;
  const isScannedOwnScript = (scriptHex: string): boolean => {
    if (scannedOwnScripts === undefined) {
      scannedOwnScripts = new Set<string>();
      for (const account of [accounts.vanilla, accounts.colored]) {
        for (let index = 0; index < scanWindow; index += 1) {
          scannedOwnScripts.add(deriveTaproot(account, 0, index, network).scriptHex);
        }
      }
    }
    return scannedOwnScripts.has(scriptHex);
  };

  const outputs: ParsedOutput[] = [];
  let outputTotalSat = 0;
  for (let i = 0; i < tx.outputsLength; i += 1) {
    const output = tx.getOutput(i);
    const script = output.script ?? new Uint8Array();
    const scriptHex = hex.encode(script);
    const amountSat = Number(output.amount ?? 0n);
    outputTotalSat += amountSat;
    const isOpret = script[0] === 0x6a;
    let isOwn = false;
    const derivation = ownDerivation(output.tapBip32Derivation as TapDerivation[], ourFingerprint);
    if (derivation !== null) {
      const derived = deriveForOriginPath(derivation[1].der.path, accounts, network);
      // Own OUTPUTS must live in the wallet's real descriptor space: keychain
      // 0 (the single keychain rgb-lib scans, network.ts KEYCHAIN) at a
      // bounded index. Inputs (check 1) stay unrestricted — spending from an
      // odd path is proven safe by the script re-derivation alone.
      isOwn =
        derived !== null &&
        derived.scriptHex === scriptHex &&
        derived.keychain === KEYCHAIN &&
        derived.index <= (params.maxOwnOutputIndex ?? DEFAULT_MAX_OWN_OUTPUT_INDEX);
    }
    if (!isOwn && !isOpret) {
      isOwn = isScannedOwnScript(scriptHex);
    }
    outputs.push({ index: i, scriptHex, amountSat, isOpret, isOwn });
  }

  // Check 2 — recipient outputs match the user-stated intent (script+amount),
  // matched as a multiset; plus (witness asset sends) exactly one foreign
  // output carrying exactly the approved witness amount.
  //
  // The expected script is RE-DERIVED from the human-readable address — the
  // field the user actually reviews. The server-supplied scriptHex is only
  // cross-checked: a hostile gateway could otherwise pair the intended
  // address with an attacker script and both would "match".
  const accounted = new Set<number>();
  const recipientFailures: string[] = [];
  for (const recipient of intent.recipients) {
    const expectedScriptHex = scriptHexFromAddress(recipient.address, network);
    if (expectedScriptHex === null) {
      recipientFailures.push(
        `recipient address ${recipient.address} does not decode on ${network}`,
      );
      continue;
    }
    if (recipient.scriptHex.toLowerCase() !== expectedScriptHex) {
      recipientFailures.push(
        `intent scriptHex does not match recipient address ${recipient.address}`,
      );
      continue;
    }
    const match = outputs.find(
      (o) =>
        !accounted.has(o.index) &&
        o.scriptHex === expectedScriptHex &&
        o.amountSat === recipient.amountSat,
    );
    if (match === undefined) {
      recipientFailures.push(
        `no output pays ${recipient.amountSat} sat to intended recipient ${recipient.address}`,
      );
    } else {
      accounted.add(match.index);
    }
  }
  const witnessAmount = intent.asset?.witnessAmountSat ?? null;
  if (witnessAmount !== null) {
    const candidates = outputs.filter(
      (o) => !accounted.has(o.index) && !o.isOwn && !o.isOpret && o.amountSat === witnessAmount,
    );
    const first = candidates[0];
    if (candidates.length === 1 && first !== undefined) {
      accounted.add(first.index);
    } else {
      recipientFailures.push(
        `expected exactly one witness output of ${witnessAmount} sat, found ${candidates.length}`,
      );
    }
  }
  checks.push({
    check: 'recipients-match',
    ok: recipientFailures.length === 0,
    detail: recipientFailures.length > 0 ? recipientFailures.join('; ') : null,
  });

  // Check 3 — change pays only re-derivable own scripts: everything that is
  // not an intended recipient, the approved witness output, or an OP_RETURN
  // must prove ownership.
  const changeFailures: string[] = [];
  for (const output of outputs) {
    if (accounted.has(output.index) || output.isOpret) continue;
    if (!output.isOwn) {
      changeFailures.push(
        `output ${output.index} (${output.amountSat} sat) does not re-derive from our keys`,
      );
    }
  }
  checks.push({
    check: 'change-own',
    ok: changeFailures.length === 0,
    detail: changeFailures.length > 0 ? changeFailures.join('; ') : null,
  });

  // Check 4 — fee = inputs − outputs within the user-approved budget.
  const feeSat = inputTotalSat - outputTotalSat;
  const feeOk = feeSat > 0 && feeSat <= maxFeeSat;
  checks.push({
    check: 'fee-budget',
    ok: feeOk,
    detail: feeOk ? null : `fee ${feeSat} sat outside budget (0, ${maxFeeSat}]`,
  });

  // Check 5 — OP_RETURN outputs carry 0 sats.
  const opretFailures = outputs.filter((o) => o.isOpret && o.amountSat !== 0);
  checks.push({
    check: 'opret-zero',
    ok: opretFailures.length === 0,
    detail:
      opretFailures.length > 0
        ? opretFailures
            .map((o) => `OP_RETURN output ${o.index} carries ${o.amountSat} sat`)
            .join('; ')
        : null,
  });

  let txid: string | null;
  try {
    txid = tx.id;
  } catch {
    txid = null;
  }
  return { ok: checks.every((c) => c.ok), checks, feeSat, txid };
}
