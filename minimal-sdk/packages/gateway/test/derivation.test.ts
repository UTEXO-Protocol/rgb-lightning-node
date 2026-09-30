/**
 * Address attribution: recovering the derivation an rgb-lib address came from.
 *
 * The point of the module is to give a client something it can check, so these
 * tests derive the expected addresses here with an independent code path
 * (@scure/bip32 + p2tr directly) rather than reusing the module's own helper.
 *
 * The derivation under test mirrors rgb-lib exactly: BIP-86 purpose, vanilla
 * coin type (0 mainnet / 1 otherwise), account 0, keychain 0 — see
 * `rgb-lib/src/utils.rs` (`PURPOSE`, `get_coin_type`, `KEYCHAIN_BTC`).
 */
import { hex } from '@scure/base';
import { HDKey } from '@scure/bip32';
import { p2tr } from '@scure/btc-signer';
import { describe, expect, it } from 'vitest';
import {
  attributeVanillaAddress,
  ATTRIBUTION_WINDOW,
  VANILLA_KEYCHAIN,
} from '../src/wallets/derivation.js';

/** Real regtest account xpubs (deterministic fixture, Preflight A script). */
const VANILLA_TPUB =
  'tpubDDfvzhdVV4unsoKt5aE6dcsNsfeWbTgmLZPi8LQDYU2xixrYemMfWJ3BaVneH3u7DBQePdTwhpybaKRU95pi6PMUtLPBJLVQRpzEnjfjZzX';
const COLORED_TPUB =
  'tpubDCtpoJs6YJcjLnr9gq6jYriYNMuWEu8mSDvEQU5st3ZkJbFqqzwpHUiPvxqD2366ciFAfpehk1k2d7Tyk7AJEr8uZva7KfnX4RpsiVSoEcZ';

const TESTNET_VERSIONS = { private: 0x04358394, public: 0x043587cf };
const MAINNET_VERSIONS = { private: 0x0488ade4, public: 0x0488b21e };
const HARDENED = 0x80000000;
const REGTEST_ADDRESS = { bech32: 'bcrt', pubKeyHash: 0x6f, scriptHash: 0xc4, wif: 0xef };
const TESTNET_ADDRESS = { bech32: 'tb', pubKeyHash: 0x6f, scriptHash: 0xc4, wif: 0xef };
const MAINNET_ADDRESS = { bech32: 'bc', pubKeyHash: 0x00, scriptHash: 0x05, wif: 0x80 };

/** Independent derivation of `xpub/keychain/index` as a taproot address. */
function derive(
  xpub: string,
  keychain: number,
  index: number,
  network: typeof REGTEST_ADDRESS,
): { address: string; scriptHex: string } {
  const key = HDKey.fromExtendedKey(xpub, TESTNET_VERSIONS)
    .deriveChild(keychain)
    .deriveChild(index);
  const payment = p2tr(key.publicKey!.slice(1), undefined, network);
  return { address: payment.address as string, scriptHex: hex.encode(payment.script) };
}

describe('attributeVanillaAddress', () => {
  it('attributes the first revealed address', () => {
    const expected = derive(VANILLA_TPUB, VANILLA_KEYCHAIN, 0, REGTEST_ADDRESS);
    expect(attributeVanillaAddress(VANILLA_TPUB, 'Regtest', expected.address, 20)).toEqual({
      account: 'vanilla',
      keychain: 0,
      index: 0,
      derivationPath: "m/86'/1'/0'/0/0",
      scriptHex: expected.scriptHex,
    });
  });

  it('attributes a later index and reports the path for it', () => {
    const expected = derive(VANILLA_TPUB, VANILLA_KEYCHAIN, 13, REGTEST_ADDRESS);
    expect(attributeVanillaAddress(VANILLA_TPUB, 'Regtest', expected.address, 20)).toMatchObject({
      index: 13,
      derivationPath: "m/86'/1'/0'/0/13",
      scriptHex: expected.scriptHex,
    });
  });

  it('uses the mainnet coin type only on mainnet', () => {
    // rgb-lib's get_coin_type(network, rgb: false) is 0 on mainnet and 1 for
    // every test network, so the reported path must differ by network even
    // though the xpub and the derived key do not.
    const testnet = derive(VANILLA_TPUB, VANILLA_KEYCHAIN, 0, TESTNET_ADDRESS);
    expect(attributeVanillaAddress(VANILLA_TPUB, 'Testnet', testnet.address, 5)).toMatchObject({
      derivationPath: "m/86'/1'/0'/0/0",
    });
    expect(attributeVanillaAddress(VANILLA_TPUB, 'Signet', testnet.address, 5)).toMatchObject({
      derivationPath: "m/86'/1'/0'/0/0",
    });
  });

  it('returns null past the scan ceiling rather than guessing', () => {
    const expected = derive(VANILLA_TPUB, VANILLA_KEYCHAIN, 30, REGTEST_ADDRESS);
    expect(attributeVanillaAddress(VANILLA_TPUB, 'Regtest', expected.address, 10)).toBeNull();
    expect(attributeVanillaAddress(VANILLA_TPUB, 'Regtest', expected.address, 30)).not.toBeNull();
  });

  it('returns null for an address that is not this wallet at all', () => {
    // A colored-account address, a wrong-network encoding of the right key, and
    // plain garbage must all be unproven rather than mis-attributed.
    const colored = derive(COLORED_TPUB, VANILLA_KEYCHAIN, 0, REGTEST_ADDRESS);
    const wrongNetwork = derive(VANILLA_TPUB, VANILLA_KEYCHAIN, 0, TESTNET_ADDRESS);
    for (const address of [
      colored.address,
      wrongNetwork.address,
      'bcrt1qw508d6qejxtdg4y5r3zarvary0c5xw7kygt080',
      'not-an-address',
      '',
    ]) {
      expect(attributeVanillaAddress(VANILLA_TPUB, 'Regtest', address, 20)).toBeNull();
    }
  });

  it('returns null instead of throwing on an unusable stored xpub', () => {
    // The route was only asked for an address; a malformed stored xpub must
    // degrade to "no evidence", not fail the request.
    for (const xpub of ['', 'not-an-xpub', VANILLA_TPUB.slice(0, -4)]) {
      expect(attributeVanillaAddress(xpub, 'Regtest', 'whatever', 5)).toBeNull();
    }
  });

  it('parses a mainnet-serialized xpub as well as a tpub', () => {
    // rgb-lib normalizes a registered xpub's network kind (`str_to_xpub`), so
    // both serializations reach the wallet; attribution must not reject a key
    // rgb-lib itself accepted. On mainnet the coin type is 0, not 1.
    const account = HDKey.fromMasterSeed(new Uint8Array(32).fill(7), MAINNET_VERSIONS)
      .deriveChild(HARDENED + 86)
      .deriveChild(HARDENED + 0)
      .deriveChild(HARDENED + 0);
    const xpub = account.publicExtendedKey;
    expect(xpub.startsWith('xpub')).toBe(true);
    const child = account.deriveChild(VANILLA_KEYCHAIN).deriveChild(0);
    const address = p2tr(child.publicKey!.slice(1), undefined, MAINNET_ADDRESS).address as string;

    expect(attributeVanillaAddress(xpub, 'Mainnet', address, 5)).toMatchObject({
      index: 0,
      derivationPath: "m/86'/0'/0'/0/0",
    });
  });

  it('treats both signet variants like every other non-mainnet chain', () => {
    // SignetCustom differs from Signet only in which chain RGB thinks it is; the
    // address encoding and the vanilla coin type (1) are identical, so
    // attribution must work the same for both.
    const expected = derive(VANILLA_TPUB, VANILLA_KEYCHAIN, 0, TESTNET_ADDRESS);
    for (const network of ['Signet', 'SignetCustom', 'Testnet4'] as const) {
      expect(attributeVanillaAddress(VANILLA_TPUB, network, expected.address, 5)).toMatchObject({
        index: 0,
        derivationPath: "m/86'/1'/0'/0/0",
        scriptHex: expected.scriptHex,
      });
    }
  });

  it('exposes a scan window wide enough to be useful', () => {
    expect(ATTRIBUTION_WINDOW).toBeGreaterThanOrEqual(100);
  });
});
