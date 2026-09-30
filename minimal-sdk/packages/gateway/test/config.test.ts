import { describe, expect, it } from 'vitest';
import { ConfigError, loadConfig } from '../src/config.js';
import { testEnv } from './helpers.js';

describe('loadConfig', () => {
  it('loads a full config with defaults applied', () => {
    const config = loadConfig(testEnv());
    expect(config.rlnUrl).toBe('http://127.0.0.1:3001');
    expect(config.host).toBe('127.0.0.1');
    expect(config.queueGlobalConcurrency).toBe(4);
    expect(config.queuePerUserDepth).toBe(16);
    expect(config.floatCapPerUserMsat).toBe(1_000_000_000);
    expect(config.floatCapGlobalMsat).toBe(10_000_000_000);
  });

  it('parses numeric overrides', () => {
    const config = loadConfig(
      testEnv({ GATEWAY_QUEUE_GLOBAL_CONCURRENCY: '2', GATEWAY_FLOAT_CAP_PER_USER_MSAT: '5000' }),
    );
    expect(config.queueGlobalConcurrency).toBe(2);
    expect(config.floatCapPerUserMsat).toBe(5000);
  });

  it('defaults the RGB transport allowlist to the configured proxy only', () => {
    expect(loadConfig(testEnv()).rgbTransportAllowlist).toEqual(['rpc://127.0.0.1:3000/json-rpc']);
  });

  it('parses, trims and dedupes extra RGB transport endpoints', () => {
    // Operator-configured endpoints are the SSRF allowlist: a parsing slip
    // either locks out a legitimate proxy or widens the allowed set.
    const config = loadConfig(
      testEnv({
        GATEWAY_RGB_TRANSPORT_ALLOWLIST:
          ' rpc://proxy-a/json-rpc , rpc://proxy-b/json-rpc ,, rpc://127.0.0.1:3000/json-rpc ',
      }),
    );
    expect(config.rgbTransportAllowlist).toEqual([
      'rpc://127.0.0.1:3000/json-rpc',
      'rpc://proxy-a/json-rpc',
      'rpc://proxy-b/json-rpc',
    ]);
  });

  it('rejects a missing RLN URL', () => {
    const env = testEnv();
    delete (env as Record<string, string | undefined>)['RLN_URL'];
    expect(() => loadConfig(env)).toThrow(ConfigError);
  });

  it('rejects an invalid URL', () => {
    expect(() => loadConfig(testEnv({ ESPLORA_URL: 'not a url' }))).toThrow(ConfigError);
  });

  it('rejects a short operator token', () => {
    expect(() => loadConfig(testEnv({ GATEWAY_OPERATOR_TOKEN: 'short' }))).toThrow(ConfigError);
  });

  it('rejects non-positive or non-integer numeric variables', () => {
    expect(() => loadConfig(testEnv({ GATEWAY_QUEUE_PER_USER_DEPTH: '0' }))).toThrow(ConfigError);
    expect(() => loadConfig(testEnv({ GATEWAY_PORT: 'eighty' }))).toThrow(ConfigError);
  });

  it('loads wallet settings with defaults', () => {
    const config = loadConfig(testEnv());
    expect(config.walletIndexerUrl).toBe('127.0.0.1:50001');
    expect(config.bitcoinNetwork).toBe('Regtest');
    expect(config.walletMaxOpen).toBe(32);
    expect(config.onchainOpTtlSeconds).toBe(600);
  });

  it('parses the on-chain op TTL override', () => {
    expect(loadConfig(testEnv({ GATEWAY_ONCHAIN_OP_TTL_SECONDS: '60' })).onchainOpTtlSeconds).toBe(
      60,
    );
    expect(() => loadConfig(testEnv({ GATEWAY_ONCHAIN_OP_TTL_SECONDS: '0' }))).toThrow(ConfigError);
  });

  it('requires the wallets directory and indexer URL', () => {
    for (const name of ['GATEWAY_WALLETS_DIR', 'GATEWAY_WALLET_INDEXER_URL']) {
      const env = testEnv();
      delete (env as Record<string, string | undefined>)[name];
      expect(() => loadConfig(env)).toThrow(ConfigError);
    }
  });

  it('validates the bitcoin network name', () => {
    expect(loadConfig(testEnv({ GATEWAY_BITCOIN_NETWORK: 'signet' })).bitcoinNetwork).toBe(
      'Signet',
    );
    expect(() => loadConfig(testEnv({ GATEWAY_BITCOIN_NETWORK: 'litecoin' }))).toThrow(ConfigError);
  });

  it('allows an empty RLN admin token (localhost --disable-authentication mode)', () => {
    const env = testEnv();
    delete (env as Record<string, string | undefined>)['RLN_ADMIN_TOKEN'];
    expect(loadConfig(env).rlnAdminToken).toBe('');
  });

  it('accepts every rgb-lib network, including the two signet variants', () => {
    // `Signet` is Bitcoin's DEFAULT signet. A signet with its own challenge is a
    // different chain to RGB even though every signet shares one genesis block
    // hash, so a deployment on a custom signet MUST be able to say so — telling
    // it `Signet` fails at first wallet use with an opaque "resolver is for
    // another chain-network pair".
    for (const network of ['Mainnet', 'Testnet', 'Testnet4', 'Signet', 'SignetCustom', 'Regtest']) {
      const config = loadConfig(testEnv({ GATEWAY_BITCOIN_NETWORK: network }));
      expect(config.bitcoinNetwork).toBe(network);
    }
    // Case-insensitive, as before.
    expect(loadConfig(testEnv({ GATEWAY_BITCOIN_NETWORK: 'signetcustom' })).bitcoinNetwork).toBe(
      'SignetCustom',
    );
    expect(() => loadConfig(testEnv({ GATEWAY_BITCOIN_NETWORK: 'Liquid' }))).toThrow(ConfigError);
  });
});
