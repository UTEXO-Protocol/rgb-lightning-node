/**
 * Derivation parity against the rgb-lib-generated fixture: the SDK must
 * reproduce rgb-lib's xpubs, fingerprint and addresses EXACTLY (Task 6
 * requirement — fixture from scripts/generate-rgblib-fixtures.mjs).
 */
import { describe, expect, it } from 'vitest';
import { ClientKeys, InvalidMnemonicError } from '../src/keys.js';
import { deriveForOriginPath, deriveTaproot, parseAccountXpub } from '../src/derive.js';
import { accountPath, HARDENED } from '../src/network.js';
import type { BitcoinNetwork } from '../src/network.js';
import { COLORED_PATH, fixture, VANILLA_PATH } from './helpers.js';

const NETWORKS: BitcoinNetwork[] = ['Regtest', 'Testnet', 'Signet', 'Mainnet'];

describe('key derivation parity with rgb-lib', () => {
  for (const network of NETWORKS) {
    it(`reproduces restore_keys on ${network}`, () => {
      const keys = ClientKeys.fromMnemonic(fixture.mnemonic, network);
      const expected = fixture.networks[network];
      expect(keys.xpubs.vanilla).toBe(expected.accountXpubVanilla);
      expect(keys.xpubs.colored).toBe(expected.accountXpubColored);
      expect(keys.fingerprint).toBe(expected.masterFingerprint);
      expect(keys.xpubs.network).toBe(network);
    });
  }

  it('reproduces rgb-lib vanilla-keychain addresses at indexes 0..4', () => {
    const { accountXpubVanilla } = fixture.networks.Regtest;
    fixture.regtest.vanillaAddresses.forEach((address, index) => {
      expect(deriveTaproot(accountXpubVanilla, 0, index, 'Regtest').address).toBe(address);
    });
  });

  it('reproduces rgb-lib colored-keychain addresses at indexes 0..4', () => {
    const { accountXpubColored } = fixture.networks.Regtest;
    fixture.regtest.coloredAddresses.forEach((address, index) => {
      expect(deriveTaproot(accountXpubColored, 0, index, 'Regtest').address).toBe(address);
    });
  });

  it('rejects an invalid mnemonic', () => {
    expect(() => ClientKeys.fromMnemonic('not a mnemonic at all', 'Regtest')).toThrow(
      InvalidMnemonicError,
    );
  });

  it('generates a fresh valid mnemonic whose keys restore identically', () => {
    const { mnemonic, keys } = ClientKeys.generate('Regtest');
    expect(mnemonic.split(' ')).toHaveLength(12);
    const restored = ClientKeys.fromMnemonic(mnemonic, 'Regtest');
    expect(restored.xpubs).toEqual(keys.xpubs);
  });

  it('uses the rgb-lib account paths (86h/coin-h/0h, colored 827166/827167)', () => {
    expect(accountPath('Regtest', false)).toEqual(VANILLA_PATH);
    expect(accountPath('Regtest', true)).toEqual(COLORED_PATH);
    expect(accountPath('Mainnet', false)).toEqual([HARDENED + 86, HARDENED, HARDENED]);
    expect(accountPath('Mainnet', true)).toEqual([HARDENED + 86, HARDENED + 827166, HARDENED]);
  });
});

describe('deriveForOriginPath', () => {
  const xpubs = {
    vanilla: fixture.networks.Regtest.accountXpubVanilla,
    colored: fixture.networks.Regtest.accountXpubColored,
  };

  it('re-derives a vanilla-keychain origin path to the rgb-lib address', () => {
    const derived = deriveForOriginPath([...VANILLA_PATH, 0, 0], xpubs, 'Regtest');
    expect(derived?.address).toBe(fixture.regtest.vanillaAddresses[0]);
  });

  it('re-derives a colored-keychain origin path to the rgb-lib address', () => {
    const derived = deriveForOriginPath([...COLORED_PATH, 0, 2], xpubs, 'Regtest');
    expect(derived?.address).toBe(fixture.regtest.coloredAddresses[2]);
  });

  it('rejects paths outside our accounts', () => {
    expect(
      deriveForOriginPath([HARDENED + 84, HARDENED + 1, HARDENED, 0, 0], xpubs, 'Regtest'),
    ).toBeNull();
    expect(deriveForOriginPath([...VANILLA_PATH, 0], xpubs, 'Regtest')).toBeNull();
    expect(deriveForOriginPath([...VANILLA_PATH, HARDENED, 0], xpubs, 'Regtest')).toBeNull();
    expect(deriveForOriginPath([], xpubs, 'Regtest')).toBeNull();
  });

  it('accepts pre-parsed HDKey accounts', () => {
    const account = parseAccountXpub(xpubs.vanilla, 'Regtest');
    const derived = deriveForOriginPath(
      [...VANILLA_PATH, 0, 1],
      { vanilla: account, colored: xpubs.colored },
      'Regtest',
    );
    expect(derived?.address).toBe(fixture.regtest.vanillaAddresses[1]);
  });
});
