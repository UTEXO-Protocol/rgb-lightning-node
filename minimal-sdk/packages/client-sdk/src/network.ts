/**
 * Network constants mirroring rgb-lib exactly (rgb-lib src/utils.rs:14-19,
 * get_coin_type): BIP-86 purpose, account 0, keychain 0 on both the vanilla
 * (standard coin type) and colored (RGB coin type 827166/827167) sides.
 */

export type BitcoinNetwork = 'Mainnet' | 'Testnet' | 'Signet' | 'Regtest';

export const PURPOSE = 86;
export const ACCOUNT = 0;
export const KEYCHAIN = 0;
export const HARDENED = 0x80000000;

const COIN_RGB_MAINNET = 827166;
const COIN_RGB_TESTNET = 827167;

/** Version bytes for extended key serialization (xprv/xpub vs tprv/tpub). */
export interface HdVersions {
  private: number;
  public: number;
}

const MAINNET_VERSIONS: HdVersions = { private: 0x0488ade4, public: 0x0488b21e };
const TESTNET_VERSIONS: HdVersions = { private: 0x04358394, public: 0x043587cf };

/** Address encoding parameters in the shape @scure/btc-signer expects. */
export interface AddressNetwork {
  bech32: string;
  pubKeyHash: number;
  scriptHash: number;
  wif: number;
}

const ADDRESS_NETWORKS: Record<BitcoinNetwork, AddressNetwork> = {
  Mainnet: { bech32: 'bc', pubKeyHash: 0x00, scriptHash: 0x05, wif: 0x80 },
  Testnet: { bech32: 'tb', pubKeyHash: 0x6f, scriptHash: 0xc4, wif: 0xef },
  Signet: { bech32: 'tb', pubKeyHash: 0x6f, scriptHash: 0xc4, wif: 0xef },
  Regtest: { bech32: 'bcrt', pubKeyHash: 0x6f, scriptHash: 0xc4, wif: 0xef },
};

export function addressNetwork(network: BitcoinNetwork): AddressNetwork {
  return ADDRESS_NETWORKS[network];
}

export function hdVersions(network: BitcoinNetwork): HdVersions {
  return network === 'Mainnet' ? MAINNET_VERSIONS : TESTNET_VERSIONS;
}

export function coinType(network: BitcoinNetwork, colored: boolean): number {
  if (colored) return network === 'Mainnet' ? COIN_RGB_MAINNET : COIN_RGB_TESTNET;
  return network === 'Mainnet' ? 0 : 1;
}

/** Hardened account derivation path children: [86', coin', 0']. */
export function accountPath(network: BitcoinNetwork, colored: boolean): number[] {
  return [HARDENED + PURPOSE, HARDENED + coinType(network, colored), HARDENED + ACCOUNT];
}
