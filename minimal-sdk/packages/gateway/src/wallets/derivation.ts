/**
 * Attribute an address rgb-lib handed out to the derivation path it came from.
 *
 * `GET /v1/wallet/address` used to return a bare string, which is unusable to a
 * client that must prove it owns the address before showing it as a deposit
 * target: the gateway holds the xpubs and could return anything. rgb-lib's
 * `get_address` returns only the encoded address (no index), so the path is
 * recovered here by re-deriving candidates from the SAME account xpub the user
 * registered and matching the script — the client can then repeat the
 * derivation from its own seed and compare.
 *
 * Which account: rgb-lib builds one BDK wallet as
 * `BdkWallet::create(desc_colored, desc_vanilla)` (rgb-lib `wallet/core.rs`),
 * so BDK's *Internal* keychain is the vanilla descriptor — and `get_address`
 * asks for `KeychainKind::Internal` (`wallet/offline.rs`). So the address is
 * always VANILLA, and the branch is the vanilla descriptor's own keychain,
 * `KEYCHAIN_BTC = 0` (`rgb-lib/src/utils.rs`). "Internal" is BDK's slot name,
 * not a BIP-44 change branch: the derivation is `m/86'/coin'/0'/0/index`.
 *
 * Attribution is best-effort by construction — BDK reveals indices
 * sequentially, so a bounded forward scan finds any address it has handed out,
 * but a wallet restored past the window, or a `reuse_addresses` deployment, can
 * legitimately fall outside it. The route reports `null` rather than guessing;
 * a client treats that exactly as it treated the old bare-string response.
 */
import { HDKey } from '@scure/bip32';
import { hex } from '@scure/base';
import { p2tr } from '@scure/btc-signer';
import type { GatewayConfig } from '../config.js';

/** rgb-lib `PURPOSE` (BIP-86) / `ACCOUNT` / `KEYCHAIN_BTC` (src/utils.rs). */
const PURPOSE = 86;
const ACCOUNT = 0;
export const VANILLA_KEYCHAIN = 0;
const HARDENED = 0x80000000;

/** Extended-key version bytes; xpub/tpub differ only by these. */
const HD_VERSIONS = {
  mainnet: { private: 0x0488ade4, public: 0x0488b21e },
  testnet: { private: 0x04358394, public: 0x043587cf },
} as const;

/** Address encoding parameters in the shape @scure/btc-signer expects. */
const ADDRESS_NETWORKS: Record<
  GatewayConfig['bitcoinNetwork'],
  { bech32: string; pubKeyHash: number; scriptHash: number; wif: number }
> = {
  Mainnet: { bech32: 'bc', pubKeyHash: 0x00, scriptHash: 0x05, wif: 0x80 },
  Testnet: { bech32: 'tb', pubKeyHash: 0x6f, scriptHash: 0xc4, wif: 0xef },
  Signet: { bech32: 'tb', pubKeyHash: 0x6f, scriptHash: 0xc4, wif: 0xef },
  Regtest: { bech32: 'bcrt', pubKeyHash: 0x6f, scriptHash: 0xc4, wif: 0xef },
};

/** rgb-lib `get_coin_type(network, rgb: false)` (src/utils.rs). */
function vanillaCoinType(network: GatewayConfig['bitcoinNetwork']): number {
  return network === 'Mainnet' ? 0 : 1;
}

/** How far past the highest index seen so far a scan looks. */
export const ATTRIBUTION_WINDOW = 200;

export interface AddressDerivation {
  /** rgb-lib's `get_address` is always the vanilla side. */
  account: 'vanilla';
  /** Vanilla descriptor keychain; rgb-lib pins this to 0. */
  keychain: number;
  index: number;
  /** Full master-based path, as it appears in PSBT key-origin metadata. */
  derivationPath: string;
  /** BIP-341 tweaked output script, so the client can compare bytes not text. */
  scriptHex: string;
}

/**
 * Parse an account xpub. rgb-lib normalizes the network kind of a registered
 * xpub (`str_to_xpub`), so a client may legitimately register an `xpub` against
 * a Regtest gateway or a `tpub` against mainnet; both version sets are tried
 * rather than rejecting a key rgb-lib itself accepted.
 */
function parseAccountXpub(xpub: string): HDKey | null {
  for (const versions of [HD_VERSIONS.testnet, HD_VERSIONS.mainnet]) {
    try {
      return HDKey.fromExtendedKey(xpub, versions);
    } catch {
      continue;
    }
  }
  return null;
}

function taproot(
  account: HDKey,
  index: number,
  network: GatewayConfig['bitcoinNetwork'],
): { address: string; scriptHex: string } | null {
  const key = account.deriveChild(VANILLA_KEYCHAIN).deriveChild(index);
  if (key.publicKey === null) return null;
  // x-only: drop the compressed-point parity byte.
  const payment = p2tr(key.publicKey.slice(1), undefined, ADDRESS_NETWORKS[network]);
  return { address: payment.address as string, scriptHex: hex.encode(payment.script) };
}

/**
 * Find the vanilla derivation `address` was produced at, or null when it is not
 * in `[0, searchTo]`. Never throws: a malformed stored xpub or an address from
 * an unexpected keychain must degrade to "no evidence", not fail the route that
 * was only asked for an address.
 */
export function attributeVanillaAddress(
  xpubVanilla: string,
  network: GatewayConfig['bitcoinNetwork'],
  address: string,
  searchTo: number,
): AddressDerivation | null {
  const account = parseAccountXpub(xpubVanilla);
  if (account === null) return null;
  const coin = vanillaCoinType(network);
  for (let index = 0; index <= searchTo && index < HARDENED; index += 1) {
    let derived;
    try {
      derived = taproot(account, index, network);
    } catch {
      return null;
    }
    if (derived === null) return null;
    if (derived.address !== address) continue;
    return {
      account: 'vanilla',
      keychain: VANILLA_KEYCHAIN,
      index,
      derivationPath: `m/${PURPOSE}'/${coin}'/${ACCOUNT}'/${VANILLA_KEYCHAIN}/${index}`,
      scriptHex: derived.scriptHex,
    };
  }
  return null;
}
