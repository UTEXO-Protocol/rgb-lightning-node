/**
 * Key generation and restore, mirroring rgb-lib's derivation exactly
 * (cross-checked against the rgb-lib-generated fixture in test/fixtures):
 * BIP-39 seed (empty passphrase) → master → account xprv/xpub at
 * m/86'/coin'/0' for the vanilla and colored keychains.
 *
 * The mnemonic is the user's ONLY backup and never leaves the client;
 * only xpubs and the master fingerprint are shared with the gateway (I1/I2).
 */
import { HDKey } from '@scure/bip32';
import { generateMnemonic, mnemonicToSeedSync, validateMnemonic } from '@scure/bip39';
import { wordlist } from '@scure/bip39/wordlists/english.js';
import { matchOriginPath } from './derive.js';
import { accountPath, hdVersions, type BitcoinNetwork } from './network.js';

/** Public account material — safe to share, used to register with the gateway. */
export interface AccountXpubs {
  network: BitcoinNetwork;
  /** Master key fingerprint, 8 lowercase hex chars. */
  fingerprint: string;
  /** Account xpub for the vanilla (BTC) keychain, m/86'/coin'/0'. */
  vanilla: string;
  /** Account xpub for the colored (RGB) keychain, m/86'/rgb-coin'/0'. */
  colored: string;
}

export class InvalidMnemonicError extends Error {
  constructor() {
    super('invalid BIP-39 mnemonic');
    this.name = 'InvalidMnemonicError';
  }
}

export function fingerprintHex(fingerprint: number): string {
  return (fingerprint >>> 0).toString(16).padStart(8, '0');
}

/** Client-side key material. Holds the account xprvs needed for signing. */
export class ClientKeys {
  private constructor(
    readonly network: BitcoinNetwork,
    private readonly master: HDKey,
    private readonly vanillaAccount: HDKey,
    private readonly coloredAccount: HDKey,
  ) {}

  /** Generate a fresh 12-word mnemonic and derive keys from it. */
  static generate(network: BitcoinNetwork): { mnemonic: string; keys: ClientKeys } {
    const mnemonic = generateMnemonic(wordlist, 128);
    return { mnemonic, keys: ClientKeys.fromMnemonic(mnemonic, network) };
  }

  /** Restore keys from an existing mnemonic (throws InvalidMnemonicError). */
  static fromMnemonic(mnemonic: string, network: BitcoinNetwork): ClientKeys {
    if (!validateMnemonic(mnemonic, wordlist)) throw new InvalidMnemonicError();
    const seed = mnemonicToSeedSync(mnemonic);
    const master = HDKey.fromMasterSeed(seed, hdVersions(network));
    const derive = (colored: boolean) =>
      accountPath(network, colored).reduce((key, child) => key.deriveChild(child), master);
    return new ClientKeys(network, master, derive(false), derive(true));
  }

  get fingerprint(): string {
    return fingerprintHex(this.master.fingerprint);
  }

  /** Public registration material; never contains private keys. */
  get xpubs(): AccountXpubs {
    return {
      network: this.network,
      fingerprint: this.fingerprint,
      vanilla: this.vanillaAccount.publicExtendedKey,
      colored: this.coloredAccount.publicExtendedKey,
    };
  }

  /**
   * Private key for a full master-based path [86', coin', 0', keychain, index]
   * as found in PSBT key-origin metadata; null when the path is not one of
   * this wallet's two accounts (never throws — used on adversarial input).
   */
  privateKeyForPath(path: number[]): Uint8Array | null {
    // Path acceptance is shared with verify.ts via matchOriginPath: what
    // verify proved is exactly what sign derives.
    const match = matchOriginPath(path, this.network);
    if (match === null) return null;
    const account = match.colored ? this.coloredAccount : this.vanillaAccount;
    return account.deriveChild(match.keychain).deriveChild(match.index).privateKey;
  }
}
