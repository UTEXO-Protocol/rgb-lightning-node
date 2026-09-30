/**
 * Shared test helpers: rgb-lib parity fixture access and a PSBT builder for
 * the adversarial verify-before-sign suite. The fixture mnemonics are TEST
 * material only — mnemonics are never a gateway input (I1/I2).
 */
import { readFileSync } from 'node:fs';
import { HDKey } from '@scure/bip32';
import { mnemonicToSeedSync } from '@scure/bip39';
import { base64, hex } from '@scure/base';
import { Address, OutScript, p2tr, Transaction } from '@scure/btc-signer';
import { addressNetwork, HARDENED } from '../src/network.js';

export interface ParityFixture {
  mnemonic: string;
  otherMnemonic: string;
  networks: Record<
    'Regtest' | 'Testnet' | 'Signet' | 'Mainnet',
    {
      xpub: string;
      accountXpubVanilla: string;
      accountXpubColored: string;
      masterFingerprint: string;
    }
  >;
  regtest: { vanillaAddresses: string[]; coloredAddresses: string[] };
  signing: { unsignedPsbt: string; signedPsbt: string; txid: string; witnessSignature: string };
  witnessReceive: { invoice: string; recipientId: string; expirationTimestamp: number };
}

export const fixture: ParityFixture = JSON.parse(
  readFileSync(new URL('./fixtures/rgblib-parity.json', import.meta.url), 'utf8'),
) as ParityFixture;

export const REGTEST = addressNetwork('Regtest');

/** Vanilla-account path children on regtest: m/86'/1'/0'. */
export const VANILLA_PATH = [HARDENED + 86, HARDENED + 1, HARDENED];
/** Colored-account path children on regtest: m/86'/827167'/0'. */
export const COLORED_PATH = [HARDENED + 86, HARDENED + 827167, HARDENED];

export interface TestWallet {
  master: HDKey;
  fingerprint: number;
  keyAt(path: number[], keychain: number, index: number): HDKey;
  scriptAt(path: number[], keychain: number, index: number): Uint8Array;
  xOnlyAt(path: number[], keychain: number, index: number): Uint8Array;
}

export function walletFromMnemonic(mnemonic: string): TestWallet {
  const master = HDKey.fromMasterSeed(mnemonicToSeedSync(mnemonic));
  const keyAt = (path: number[], keychain: number, index: number): HDKey =>
    [...path, keychain, index].reduce((key, child) => key.deriveChild(child), master);
  const xOnlyAt = (path: number[], keychain: number, index: number): Uint8Array =>
    (keyAt(path, keychain, index).publicKey as Uint8Array).slice(1);
  return {
    master,
    fingerprint: master.fingerprint,
    keyAt,
    xOnlyAt,
    scriptAt: (path, keychain, index) =>
      p2tr(xOnlyAt(path, keychain, index), undefined, REGTEST).script,
  };
}

export const ours = walletFromMnemonic(fixture.mnemonic);
export const foreign = walletFromMnemonic(fixture.otherMnemonic);

export interface PsbtInputSpec {
  /** Script actually being spent. */
  script: Uint8Array;
  amount: bigint;
  /** Key-origin metadata to attach (attacker-controlled in the threat model). */
  derivation?: { fingerprint: number; path: number[]; xOnly: Uint8Array } | undefined;
  /**
   * Extra key-origin entries listed BEFORE `derivation`. A hostile server can
   * order the map freely, so this is how a decoy entry is emulated.
   */
  decoyDerivations?: { fingerprint: number; path: number[]; xOnly: Uint8Array }[] | undefined;
  tapInternalKey?: Uint8Array | undefined;
}

export interface PsbtOutputSpec {
  script: Uint8Array;
  amount: bigint;
  derivation?: { fingerprint: number; path: number[]; xOnly: Uint8Array } | undefined;
}

/**
 * Build a base64 PSBT with full control over scripts and metadata. Pass
 * `forgeInvalid` to bypass btc-signer's own construction-time sanity checks
 * and forge PSBTs no honest library would emit (hostile-server emulation).
 */
export function buildPsbt(
  inputs: PsbtInputSpec[],
  outputs: PsbtOutputSpec[],
  options?: { forgeInvalid?: boolean },
): string {
  const tx = new Transaction({
    allowUnknownOutputs: true,
    ...(options?.forgeInvalid === true ? { disableScriptCheck: true } : {}),
  });
  inputs.forEach((input, i) => {
    tx.addInput({
      txid: hex.decode(`${(i + 1).toString(16).padStart(2, '0')}`.repeat(32).slice(0, 64)),
      index: i,
      witnessUtxo: { script: input.script, amount: input.amount },
      ...(input.tapInternalKey !== undefined ? { tapInternalKey: input.tapInternalKey } : {}),
      ...(input.derivation !== undefined
        ? {
            tapBip32Derivation: [...(input.decoyDerivations ?? []), input.derivation].map(
              (entry) =>
                [
                  entry.xOnly,
                  { hashes: [], der: { fingerprint: entry.fingerprint, path: entry.path } },
                ] as [
                  Uint8Array,
                  { hashes: Uint8Array[]; der: { fingerprint: number; path: number[] } },
                ],
            ),
          }
        : {}),
    });
  });
  outputs.forEach((output, i) => {
    tx.addOutput({ script: output.script, amount: output.amount });
    if (output.derivation !== undefined) {
      tx.updateOutput(i, {
        tapInternalKey: output.derivation.xOnly,
        tapBip32Derivation: [
          [
            output.derivation.xOnly,
            {
              hashes: [],
              der: { fingerprint: output.derivation.fingerprint, path: output.derivation.path },
            },
          ],
        ],
      });
    }
  });
  return base64.encode(tx.toPSBT());
}

/** Standard zero-value OP_RETURN output used across cases. */
export const OPRET_SCRIPT = new Uint8Array([0x6a, 0x05, 1, 2, 3, 4, 5]);

/** Regtest address for an output script (intents must carry a real address). */
export function addressFor(script: Uint8Array): string {
  return Address(addressNetwork('Regtest')).encode(OutScript.decode(script));
}

export function ownDerivation(keychain: number, index: number, colored = false) {
  const path = colored ? COLORED_PATH : VANILLA_PATH;
  return {
    fingerprint: ours.fingerprint,
    path: [...path, keychain, index],
    xOnly: ours.xOnlyAt(path, keychain, index),
  };
}
