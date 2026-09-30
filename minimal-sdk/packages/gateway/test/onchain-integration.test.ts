/**
 * On-chain prepare→sign→complete flows against the real regtest stack:
 * bitcoind + electrs (127.0.0.1:50001) + RGB proxy (127.0.0.1:3000). Run with:
 *
 *   ./regtest.sh start                    # from the repo root
 *   WALLET_REGTEST=1 pnpm --filter @utexo/minimal-gateway test onchain-integration
 *
 * The gateway user is a watch-only wallet (fixture mnemonic #1 signs client-
 * side via @scure); the counterparty is a keys-bearing native rgb-lib wallet
 * driven directly (test-only — mnemonics are never a gateway input).
 *
 * Needs a freshly reset chain. When combined with wallet-integration (same
 * fixture account), run that suite FIRST: this one creates colorable UTXOs,
 * which would break its INSUFFICIENT_FUNDS assertion.
 */
import { execSync } from 'node:child_process';
import { mkdtempSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import type { FastifyInstance } from 'fastify';
import { afterAll, beforeAll, describe, expect, it } from 'vitest';
import { loadNativeRgbLib, restoreKeysForTests } from '../src/wallets/rgblib.js';
import { masterFromMnemonic, signPsbtWithMaster } from './fixture-signer.js';
import { createTestUser, sleep, testServer, type TestUser } from './helpers.js';

const REPO_ROOT = fileURLToPath(new URL('../../../..', import.meta.url));
const ELECTRUM_URL = '127.0.0.1:50001';
const PROXY_URL = 'rpc://127.0.0.1:3000/json-rpc';

// TEST fixture only (same as wallet-integration): client-side signer material.
const FIXTURE_MNEMONIC =
  'abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about';

const master = masterFromMnemonic(FIXTURE_MNEMONIC);

function regtest(command: string): string {
  return execSync(`./regtest.sh ${command}`, { cwd: REPO_ROOT, encoding: 'utf8' }).trim();
}

type Opaque = unknown;

/** One-shot native calls the gateway interface deliberately excludes. */
interface CounterpartyLib {
  rgblib_new_wallet(walletData: string, keys: string): Opaque;
  rgblib_go_online(wallet: Opaque, skip: boolean, indexer: string): Opaque;
  rgblib_generate_keys(network: string): string;
  rgblib_get_address(wallet: Opaque): string;
  rgblib_get_btc_balance(wallet: Opaque, online: Opaque, skipSync: boolean): string;
  rgblib_get_asset_balance(wallet: Opaque, assetId: string): string;
  rgblib_create_utxos(
    wallet: Opaque,
    online: Opaque,
    upTo: boolean,
    num: string,
    size: string,
    feeRate: string,
    skipSync: boolean,
  ): string;
  rgblib_issue_asset_nia(
    wallet: Opaque,
    ticker: string,
    name: string,
    precision: string,
    amounts: string,
  ): string;
  rgblib_blind_receive(
    wallet: Opaque,
    assetId: string | null,
    assignment: string,
    expiration: string,
    transportEndpoints: string,
    minConfirmations: string,
  ): string;
  rgblib_send(
    wallet: Opaque,
    online: Opaque,
    recipientMap: string,
    donation: boolean,
    feeRate: string,
    minConfirmations: string,
    expirationTimestamp: string,
    skipSync: boolean,
  ): string;
  rgblib_refresh(
    wallet: Opaque,
    online: Opaque,
    assetId: string | null,
    filter: string,
    skipSync: boolean,
  ): string;
  free_wallet(wallet: Opaque): void;
  free_online(online: Opaque): void;
}

interface Counterparty {
  lib: CounterpartyLib;
  wallet: Opaque;
  online: Opaque;
  address: string;
}

function openCounterparty(): Counterparty {
  const lib = loadNativeRgbLib() as unknown as CounterpartyLib;
  const keysJson = lib.rgblib_generate_keys('Regtest');
  const keys = JSON.parse(keysJson) as Record<string, string>;
  const dataDir = mkdtempSync(join(tmpdir(), 'utexo-onchain-cp-'));
  const walletData = JSON.stringify({
    dataDir,
    bitcoinNetwork: 'Regtest',
    databaseType: 'Sqlite',
    maxAllocationsPerUtxo: '1',
    accountXpubVanilla: keys['accountXpubVanilla'],
    accountXpubColored: keys['accountXpubColored'],
    vanillaKeychain: null,
    supportedSchemas: ['Nia', 'Cfa', 'Uda'],
  });
  const wallet = lib.rgblib_new_wallet(
    walletData,
    JSON.stringify({ ...(JSON.parse(keysJson) as object), vanillaKeychain: null }),
  );
  const online = lib.rgblib_go_online(wallet, false, ELECTRUM_URL);
  const address = lib.rgblib_get_address(wallet);
  return { lib, wallet, online, address };
}

async function retryUntil<T>(
  probe: () => Promise<T | undefined>,
  timeoutMs: number,
  intervalMs = 2000,
): Promise<T> {
  const deadline = Date.now() + timeoutMs;
  for (;;) {
    const value = await probe();
    if (value !== undefined) return value;
    if (Date.now() > deadline) throw new Error('condition not reached before timeout');
    await sleep(intervalMs);
  }
}

describe.runIf(process.env['WALLET_REGTEST'] === '1')('on-chain flows on regtest', () => {
  let app: FastifyInstance;
  let user: TestUser;
  let cp: Counterparty;
  let assetId: string;

  beforeAll(async () => {
    app = await testServer({
      configOverrides: {
        walletsDir: mkdtempSync(join(tmpdir(), 'utexo-onchain-it-')),
        walletIndexerUrl: ELECTRUM_URL,
        rgbProxyUrl: PROXY_URL,
      },
    });
    user = await createTestUser(app);
    const xpubs = restoreKeysForTests('Regtest', FIXTURE_MNEMONIC);
    const registered = await app.inject({
      method: 'POST',
      url: '/v1/wallet/xpubs',
      headers: authed(),
      payload: { vanilla: xpubs.vanilla, colored: xpubs.colored, fingerprint: xpubs.fingerprint },
    });
    expect(registered.statusCode).toBe(201);
    const address = (registered.json() as { address: string }).address;

    // Faucet-fund the user's vault and wait for the watch-only wallet to see it.
    regtest(`sendtoaddress ${address} 1`);
    regtest('mine 1');
    await retryUntil(async () => {
      await gateway('POST', '/v1/wallet/sync');
      const balances = (await gateway('GET', '/v1/wallet/balances')) as {
        btc: { vanilla: { settled: number } };
      };
      return balances.btc.vanilla.settled >= 100_000_000 ? true : undefined;
    }, 60_000);

    cp = openCounterparty();
  }, 180_000);

  afterAll(async () => {
    if (cp !== undefined) {
      cp.lib.free_online(cp.online);
      cp.lib.free_wallet(cp.wallet);
    }
    await app?.close();
  });

  function authed() {
    return { authorization: `Bearer ${user.token}` };
  }

  let keyCounter = 0;
  async function gateway(method: 'GET' | 'POST', url: string, payload?: object): Promise<unknown> {
    keyCounter += 1;
    const response = await app.inject({
      method,
      url,
      headers: { ...authed(), 'idempotency-key': `it-${Date.now()}-${keyCounter}` },
      ...(payload !== undefined ? { payload } : {}),
    });
    if (response.statusCode >= 400) {
      throw new Error(`${method} ${url} -> ${response.statusCode} ${response.body}`);
    }
    return response.json();
  }

  function cpRefresh(): void {
    cp.lib.rgblib_refresh(cp.wallet, cp.online, null, JSON.stringify([]), false);
  }

  it('send-btc: prepare, reject unsigned, sign client-side, complete, funds arrive', async () => {
    const prepared = (await gateway('POST', '/v1/onchain/send-btc/prepare', {
      address: cp.address,
      amountSat: 1_000_000,
      feeRateSatPerVb: 2,
    })) as { opId: string; psbt: string; intent: { recipients: { scriptHex: string }[] } };
    expect(prepared.psbt.length).toBeGreaterThan(100);
    expect(prepared.intent.recipients[0]!.scriptHex).toMatch(/^5120[0-9a-f]{64}$/);

    // Unsigned completion must fail via rgb-lib's own finalize error, not a 500.
    const unsigned = await app.inject({
      method: 'POST',
      url: '/v1/onchain/send-btc/complete',
      headers: { ...authed(), 'idempotency-key': `it-tamper-${Date.now()}` },
      payload: { opId: prepared.opId, signedPsbt: prepared.psbt },
    });
    expect(unsigned.statusCode).toBe(400);
    expect((unsigned.json() as { error: { code: string } }).error.code).toBe('PSBT_REJECTED');

    const signedPsbt = signPsbtWithMaster(master, prepared.psbt);
    const completed = (await gateway('POST', '/v1/onchain/send-btc/complete', {
      opId: prepared.opId,
      signedPsbt,
    })) as { txid: string };
    expect(completed.txid).toMatch(/^[0-9a-f]{64}$/);

    regtest('mine 1');
    await retryUntil(async () => {
      const balance = JSON.parse(cp.lib.rgblib_get_btc_balance(cp.wallet, cp.online, false)) as {
        vanilla: { settled: number };
      };
      return balance.vanilla.settled >= 1_000_000 ? true : undefined;
    }, 60_000);
  }, 120_000);

  it('create-utxos: prepare, sign client-side, complete; colorable utxos appear', async () => {
    const prepared = (await gateway('POST', '/v1/onchain/create-utxos/prepare', {
      num: 5,
      size: 5000,
    })) as { opId: string; psbt: string };
    const signedPsbt = signPsbtWithMaster(master, prepared.psbt);
    const completed = (await gateway('POST', '/v1/onchain/create-utxos/complete', {
      opId: prepared.opId,
      signedPsbt,
    })) as { txid: string | null; utxosCreated: number };
    expect(completed.utxosCreated).toBe(5);
    expect(completed.txid).toMatch(/^[0-9a-f]{64}$/);

    regtest('mine 1');
    await retryUntil(async () => {
      await gateway('POST', '/v1/wallet/sync');
      const { unspents } = (await gateway('GET', '/v1/wallet/unspents')) as {
        unspents: { colorable: boolean }[];
      };
      return unspents.filter((u) => u.colorable).length >= 5 ? true : undefined;
    }, 60_000);
  }, 120_000);

  it('receives a real RGB asset from the counterparty into the vault', async () => {
    // Counterparty side: colorable utxos + NIA issuance (no signing needed).
    cp.lib.rgblib_create_utxos(cp.wallet, cp.online, false, '5', '5000', '2', false);
    regtest('mine 1');
    const issued = JSON.parse(
      cp.lib.rgblib_issue_asset_nia(cp.wallet, 'ITEST', 'Integration Test', '0', '["1000"]'),
    ) as { assetId: string };
    assetId = issued.assetId;
    expect(assetId.length).toBeGreaterThan(10);

    const receive = (await gateway('POST', '/v1/wallet/receive', { mode: 'blind' })) as {
      recipientId: string;
    };
    const recipientMap = JSON.stringify({
      [assetId]: [
        {
          recipientId: receive.recipientId,
          witnessData: null,
          assignment: { Fungible: 100 },
          transportEndpoints: [PROXY_URL],
        },
      ],
    });
    const sent = JSON.parse(
      cp.lib.rgblib_send(
        cp.wallet,
        cp.online,
        recipientMap,
        false,
        '2',
        '1',
        String(Math.floor(Date.now() / 1000) + 3600),
        false,
      ),
    ) as { txid: string };
    expect(sent.txid).toMatch(/^[0-9a-f]{64}$/);

    // Settlement dance (donation=false): receiver ACKs via refresh, sender's
    // refresh then broadcasts, and a confirmation settles both sides — so
    // sync/refresh/mine run inside the poll loop.
    await retryUntil(async () => {
      await gateway('POST', '/v1/wallet/sync');
      cpRefresh();
      const balances = (await gateway('GET', '/v1/wallet/balances')) as {
        assets: { assetId: string; balance: { settled: number } }[];
      };
      const asset = balances.assets.find((entry) => entry.assetId === assetId);
      if (asset !== undefined && asset.balance.settled >= 100) return true;
      regtest('mine 1');
      return undefined;
    }, 90_000);
  }, 180_000);

  it('send-asset: prepare, sign client-side (colored keychain), complete, settle', async () => {
    const back = JSON.parse(
      cp.lib.rgblib_blind_receive(
        cp.wallet,
        null,
        JSON.stringify('Any'),
        String(Math.floor(Date.now() / 1000) + 3600),
        JSON.stringify([PROXY_URL]),
        '1',
      ),
    ) as { recipientId: string };

    const prepared = (await gateway('POST', '/v1/onchain/send-asset/prepare', {
      assetId,
      amount: 40,
      recipientId: back.recipientId,
      transportEndpoints: [PROXY_URL],
    })) as { opId: string; psbt: string; intent: { asset: { amount: number } } };
    expect(prepared.intent.asset.amount).toBe(40);

    const signedPsbt = signPsbtWithMaster(master, prepared.psbt);
    const completed = (await gateway('POST', '/v1/onchain/send-asset/complete', {
      opId: prepared.opId,
      signedPsbt,
    })) as { txid: string };
    expect(completed.txid).toMatch(/^[0-9a-f]{64}$/);

    // Ownership recorded for I3 scoping.
    const owned = app.db
      .prepare('SELECT user_id, state FROM resource_map WHERE kind = ? AND resource_id = ?')
      .get('asset_transfer', completed.txid) as { user_id: string; state: string };
    expect(owned).toEqual({ user_id: user.userId, state: 'sent' });

    // Same ACK dance in reverse: cp ACKs, the user's refresh broadcasts.
    await retryUntil(async () => {
      cpRefresh();
      await gateway('POST', '/v1/wallet/sync');
      const balance = JSON.parse(cp.lib.rgblib_get_asset_balance(cp.wallet, assetId)) as {
        settled: number;
      };
      if (balance.settled >= 40) return true;
      regtest('mine 1');
      return undefined;
    }, 90_000);
  }, 180_000);
});
