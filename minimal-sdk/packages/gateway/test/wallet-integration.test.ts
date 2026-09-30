/**
 * Integration test against the repo's regtest stack (Preflight B bring-up):
 * requires bitcoind + electrs on 127.0.0.1:50001. Run with:
 *
 *   ESPLORA=1 ./regtest.sh start          # from the repo root
 *   WALLET_REGTEST=1 pnpm --filter @utexo/minimal-gateway test wallet-integration
 *
 * Skipped (not failed) when WALLET_REGTEST is unset so the default unit-test
 * command needs no infrastructure.
 */
import { execSync } from 'node:child_process';
import { mkdtempSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import type { FastifyInstance } from 'fastify';
import { afterAll, beforeAll, describe, expect, it } from 'vitest';
import { restoreKeysForTests } from '../src/wallets/rgblib.js';
import { createTestUser, sleep, testServer, type TestUser } from './helpers.js';

const REPO_ROOT = fileURLToPath(new URL('../../../..', import.meta.url));
const ELECTRUM_URL = '127.0.0.1:50001';

// TEST fixture only: mnemonics are never a gateway input (I1/I2). The client
// SDK (Task 6) must reproduce these xpubs and this first address exactly.
const FIXTURE_MNEMONIC =
  'abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about';
const FIXTURE_FIRST_ADDRESS = 'bcrt1p8wpt9v4frpf3tkn0srd97pksgsxc5hs52lafxwru9kgeephvs7rqjeprhg';

const FUND_BTC = 0.5;
const FUND_SAT = 50_000_000;

function regtest(command: string): string {
  return execSync(`./regtest.sh ${command}`, { cwd: REPO_ROOT, encoding: 'utf8' }).trim();
}

describe.runIf(process.env['WALLET_REGTEST'] === '1')('wallet service on regtest', () => {
  let app: FastifyInstance;
  let user: TestUser;

  beforeAll(async () => {
    app = await testServer({
      configOverrides: {
        walletsDir: mkdtempSync(join(tmpdir(), 'utexo-gateway-it-')),
        walletIndexerUrl: ELECTRUM_URL,
      },
    });
    user = await createTestUser(app);
  });

  afterAll(async () => {
    await app?.close();
  });

  function authed() {
    return { authorization: `Bearer ${user.token}` };
  }

  it('registers fixture xpubs and derives the expected first address', async () => {
    const xpubs = restoreKeysForTests('Regtest', FIXTURE_MNEMONIC);
    const response = await app.inject({
      method: 'POST',
      url: '/v1/wallet/xpubs',
      headers: authed(),
      payload: { vanilla: xpubs.vanilla, colored: xpubs.colored, fingerprint: xpubs.fingerprint },
    });
    expect(response.statusCode).toBe(201);
    expect(response.json()).toEqual({
      fingerprint: '73c5da0a',
      address: FIXTURE_FIRST_ADDRESS,
    });
  }, 60_000);

  it('sees funded balance on the watch-only wallet after sync', async () => {
    const before = (
      await app.inject({ method: 'GET', url: '/v1/wallet/balances', headers: authed() })
    ).json() as { btc: { vanilla: { settled: number } } };

    regtest(`sendtoaddress ${FIXTURE_FIRST_ADDRESS} ${FUND_BTC}`);
    regtest('mine 1');

    let settled = before.btc.vanilla.settled;
    for (let attempt = 0; attempt < 20; attempt += 1) {
      await sleep(1500);
      const syncResponse = await app.inject({
        method: 'POST',
        url: '/v1/wallet/sync',
        headers: authed(),
      });
      expect(syncResponse.statusCode).toBe(200);
      const balances = (
        await app.inject({ method: 'GET', url: '/v1/wallet/balances', headers: authed() })
      ).json() as { btc: { vanilla: { settled: number } } };
      settled = balances.btc.vanilla.settled;
      if (settled >= before.btc.vanilla.settled + FUND_SAT) break;
    }
    expect(settled).toBeGreaterThanOrEqual(before.btc.vanilla.settled + FUND_SAT);

    const unspents = (
      await app.inject({ method: 'GET', url: '/v1/wallet/unspents', headers: authed() })
    ).json() as { unspents: { txid: string; amountSat: number }[] };
    expect(unspents.unspents.length).toBeGreaterThanOrEqual(1);
    expect(unspents.unspents.some((u) => u.amountSat === FUND_SAT)).toBe(true);
  }, 120_000);

  it('maps a real rgb-lib insufficient-allocation failure to a clean 400', async () => {
    // No colored utxos were created (that needs client signing — Task 5), so a
    // receive must fail with the sanitized INSUFFICIENT_FUNDS error, not a 500.
    const response = await app.inject({
      method: 'POST',
      url: '/v1/wallet/receive',
      headers: authed(),
      payload: { mode: 'blind' },
    });
    expect(response.statusCode).toBe(400);
    expect(response.json().error.code).toBe('INSUFFICIENT_FUNDS');
    expect(response.body).not.toContain('RgbLib');
  }, 60_000);

  it('lists transfers (empty but well-formed) on the fresh wallet', async () => {
    const response = await app.inject({
      method: 'GET',
      url: '/v1/wallet/transfers',
      headers: authed(),
    });
    expect(response.statusCode).toBe(200);
    expect(response.json()).toEqual({ transfers: [] });
  }, 60_000);
});
