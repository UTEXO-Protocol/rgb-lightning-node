/**
 * Re-derivation of taproot `tr(key)` scripts/addresses from account xpubs at
 * arbitrary keychain/index — the client-side half of change verification
 * (verify-before-sign check 3) and input-ownership proofs (check 1).
 */
import { HDKey } from '@scure/bip32';
import { hex } from '@scure/base';
import { p2tr } from '@scure/btc-signer';
import {
  accountPath,
  addressNetwork,
  HARDENED,
  hdVersions,
  type BitcoinNetwork,
} from './network.js';

export interface DerivedScript {
  keychain: number;
  index: number;
  /** BIP-341 tweaked output script (OP_1 <32-byte key>). */
  scriptHex: string;
  address: string;
  /** Untweaked x-only internal key, 32 bytes hex. */
  internalKeyHex: string;
}

export class InvalidXpubError extends Error {
  constructor(detail: string) {
    super(`invalid account xpub: ${detail}`);
    this.name = 'InvalidXpubError';
  }
}

export function parseAccountXpub(xpub: string, network: BitcoinNetwork): HDKey {
  try {
    return HDKey.fromExtendedKey(xpub, hdVersions(network));
  } catch (error) {
    throw new InvalidXpubError(error instanceof Error ? error.message : String(error));
  }
}

function xOnly(publicKey: Uint8Array): Uint8Array {
  return publicKey.slice(1);
}

/** Derive the taproot script/address at `keychain/index` under an account xpub. */
export function deriveTaproot(
  accountXpub: string | HDKey,
  keychain: number,
  index: number,
  network: BitcoinNetwork,
): DerivedScript {
  const account =
    typeof accountXpub === 'string' ? parseAccountXpub(accountXpub, network) : accountXpub;
  const key = account.deriveChild(keychain).deriveChild(index);
  if (key.publicKey === null) throw new InvalidXpubError('cannot derive public key');
  const internalKey = xOnly(key.publicKey);
  const payment = p2tr(internalKey, undefined, addressNetwork(network));
  return {
    keychain,
    index,
    scriptHex: hex.encode(payment.script),
    address: payment.address as string,
    internalKeyHex: hex.encode(internalKey),
  };
}

export interface OriginPathMatch {
  colored: boolean;
  keychain: number;
  index: number;
}

/**
 * Match a full master-based key-origin path ([86', coin', 0', keychain,
 * index]) against the user's two accounts. Single source of truth for path
 * acceptance: verify.ts proves scripts with it and sign.ts (via
 * ClientKeys.privateKeyForPath) picks signing keys with it — the two must
 * never drift, or verify could approve what sign will not use. Returns null
 * (never throws) on foreign paths — paths are attacker-controlled PSBT input.
 */
export function matchOriginPath(path: number[], network: BitcoinNetwork): OriginPathMatch | null {
  for (const colored of [false, true]) {
    const prefix = accountPath(network, colored);
    if (path.length !== prefix.length + 2) continue;
    if (!prefix.every((child, i) => path[i] === child)) continue;
    const keychain = path[prefix.length];
    const index = path[prefix.length + 1];
    if (keychain === undefined || index === undefined) continue;
    if (keychain >= HARDENED || index >= HARDENED) continue;
    return { colored, keychain, index };
  }
  return null;
}

/**
 * Re-derive the taproot script for a full master-based key-origin path
 * against the matching account xpub; null when the path is not ours.
 */
export function deriveForOriginPath(
  path: number[],
  xpubs: { vanilla: string | HDKey; colored: string | HDKey },
  network: BitcoinNetwork,
): DerivedScript | null {
  const match = matchOriginPath(path, network);
  if (match === null) return null;
  try {
    return deriveTaproot(
      match.colored ? xpubs.colored : xpubs.vanilla,
      match.keychain,
      match.index,
      network,
    );
  } catch {
    return null;
  }
}
