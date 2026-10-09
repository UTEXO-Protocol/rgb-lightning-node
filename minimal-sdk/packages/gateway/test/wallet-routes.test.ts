import type { FastifyInstance } from 'fastify';
import { hex } from '@scure/base';
import { HDKey } from '@scure/bip32';
import { p2tr } from '@scure/btc-signer';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { WalletBackendError } from '../src/wallets/backend.js';
import { createTestUser, testServer, type TestUser } from './helpers.js';
import { EMPTY_BALANCE, MockWalletBackend } from './wallet-mocks.js';

// Real regtest account xpubs (deterministic fixture, Preflight A script).
const XPUBS = {
  vanilla:
    'tpubDDfvzhdVV4unsoKt5aE6dcsNsfeWbTgmLZPi8LQDYU2xixrYemMfWJ3BaVneH3u7DBQePdTwhpybaKRU95pi6PMUtLPBJLVQRpzEnjfjZzX',
  colored:
    'tpubDCtpoJs6YJcjLnr9gq6jYriYNMuWEu8mSDvEQU5st3ZkJbFqqzwpHUiPvxqD2366ciFAfpehk1k2d7Tyk7AJEr8uZva7KfnX4RpsiVSoEcZ',
  fingerprint: '73c5da0a',
};

/**
 * Re-derive the fixture wallet's vanilla addresses the way a client would, so
 * the attribution the route reports is checked against an independent
 * derivation rather than against the gateway's own helper.
 */
function deriveVanilla(index: number): { address: string; scriptHex: string } {
  const account = HDKey.fromExtendedKey(XPUBS.vanilla, {
    private: 0x04358394,
    public: 0x043587cf,
  });
  const key = account.deriveChild(0).deriveChild(index);
  const payment = p2tr(key.publicKey!.slice(1), undefined, {
    bech32: 'bcrt',
    pubKeyHash: 0x6f,
    scriptHash: 0xc4,
    wif: 0xef,
  });
  return { address: payment.address as string, scriptHex: hex.encode(payment.script) };
}

describe('wallet routes', () => {
  let app: FastifyInstance;
  let backend: MockWalletBackend;
  let user: TestUser;

  beforeEach(async () => {
    backend = new MockWalletBackend();
    app = await testServer({ walletBackend: backend });
    user = await createTestUser(app);
  });

  afterEach(async () => {
    await app.close();
  });

  function authed(token: string = user.token) {
    return { authorization: `Bearer ${token}` };
  }

  async function register(token: string = user.token) {
    return app.inject({
      method: 'POST',
      url: '/v1/wallet/xpubs',
      headers: authed(token),
      payload: XPUBS,
    });
  }

  describe('xpub registration', () => {
    it('registers xpubs and returns the first address', async () => {
      const response = await register();
      expect(response.statusCode).toBe(201);
      expect(response.json()).toEqual({
        fingerprint: XPUBS.fingerprint,
        address: `addr-${user.userId}`,
      });
      expect(backend.openCalls[0]?.xpubs).toEqual(XPUBS);
    });

    it('is idempotent for identical re-registration', async () => {
      await register();
      const again = await register();
      expect(again.statusCode).toBe(201);
      expect(again.json().fingerprint).toBe(XPUBS.fingerprint);
    });

    it('rejects re-registration with a different fingerprint', async () => {
      await register();
      const response = await app.inject({
        method: 'POST',
        url: '/v1/wallet/xpubs',
        headers: authed(),
        payload: { ...XPUBS, fingerprint: 'deadbeef' },
      });
      expect(response.statusCode).toBe(409);
      expect(response.json().error.code).toBe('XPUBS_MISMATCH');
    });

    it('rejects re-registration with different xpubs even under the same fingerprint', async () => {
      await register();
      const response = await app.inject({
        method: 'POST',
        url: '/v1/wallet/xpubs',
        headers: authed(),
        payload: { ...XPUBS, vanilla: XPUBS.colored },
      });
      expect(response.statusCode).toBe(409);
    });

    it('rejects malformed fingerprints and xpubs at the schema layer', async () => {
      const bad = await app.inject({
        method: 'POST',
        url: '/v1/wallet/xpubs',
        headers: authed(),
        payload: { ...XPUBS, fingerprint: 'nothex!!' },
      });
      expect(bad.statusCode).toBe(400);
      const short = await app.inject({
        method: 'POST',
        url: '/v1/wallet/xpubs',
        headers: authed(),
        payload: { ...XPUBS, vanilla: 'tooshort' },
      });
      expect(short.statusCode).toBe(400);
    });

    it('maps a REJECTED xpub to 400 INVALID_XPUBS and persists nothing', async () => {
      backend.openError = new WalletBackendError(
        'wallet construction failed',
        'RgbLib(InvalidBitcoinKeys)',
        false,
        true,
      );
      const response = await register();
      expect(response.statusCode).toBe(400);
      expect(response.json().error.code).toBe('INVALID_XPUBS');

      backend.openError = undefined;
      const retry = await register();
      expect(retry.statusCode).toBe(201);
    });

    it('does NOT blame the xpubs for a deployment failure, and logs the cause', async () => {
      // A missing rgb-lib native module or an unreachable indexer also fails the
      // smoke-open. Answering 400 INVALID_XPUBS sent an operator hunting for a
      // client bug — and, because the central handler logs a 400 only when it
      // carries a cause, left the most common bring-up failure with no trace at
      // all. Both halves are asserted here.
      const lines: string[] = [];
      const logged = await testServer({
        walletBackend: backend,
        loggerStream: { write: (msg) => lines.push(msg) },
      });
      const loggedUser = await createTestUser(logged);
      backend.openError = new WalletBackendError(
        'wallet construction failed',
        'rgb-lib native module unavailable: no platform package for linux-x64',
      );
      const response = await logged.inject({
        method: 'POST',
        url: '/v1/wallet/xpubs',
        headers: { authorization: `Bearer ${loggedUser.token}` },
        payload: XPUBS,
      });
      expect(response.statusCode).toBe(500);
      expect(response.json().error.code).toBe('INTERNAL');
      // Opaque to the client (I4) but present in the log for the operator.
      expect(response.body).not.toContain('rgb-lib');
      expect(lines.join('\n')).toContain('no platform package for linux-x64');
      backend.openError = undefined;
      await logged.close();
    });

    it('records the cause of a rejected xpub so a 400 is still diagnosable', async () => {
      const lines: string[] = [];
      const logged = await testServer({
        walletBackend: backend,
        loggerStream: { write: (msg) => lines.push(msg) },
      });
      const loggedUser = await createTestUser(logged);
      backend.openError = new WalletBackendError(
        'wallet construction failed',
        'RgbLib(InvalidFingerprint)',
        false,
        true,
      );
      const response = await logged.inject({
        method: 'POST',
        url: '/v1/wallet/xpubs',
        headers: { authorization: `Bearer ${loggedUser.token}` },
        payload: XPUBS,
      });
      expect(response.statusCode).toBe(400);
      expect(response.body).not.toContain('RgbLib');
      expect(lines.join('\n')).toContain('InvalidFingerprint');
      backend.openError = undefined;
      await logged.close();
    });

    it('evicts the smoke-opened wallet when persisting the xpubs fails', async () => {
      // The pool keys handles by userId alone, so a handle cached for xpubs
      // that were never persisted would serve addresses derived from the WRONG
      // key on a retry with different xpubs — funds sent to an address the
      // registered wallet never watches.
      const otherXpubs = {
        vanilla:
          'tpubDCtpoJs6YJcjLnr9gq6jYriYNMuWEu8mSDvEQU5st3ZkJbFqqzwpHUiPvxqD2366ciFAfpehk1k2d7Tyk7AJEr8uZva7KfnX4RpsiVSoEcZ',
        colored:
          'tpubDDfvzhdVV4unsoKt5aE6dcsNsfeWbTgmLZPi8LQDYU2xixrYemMfWJ3BaVneH3u7DBQePdTwhpybaKRU95pi6PMUtLPBJLVQRpzEnjfjZzX',
        fingerprint: '73c5da0a',
      };
      // Fail the INSERT specifically (reads still work, so the smoke-open runs).
      app.db.exec(
        `CREATE TRIGGER fail_xpubs_insert BEFORE INSERT ON user_xpubs
         BEGIN SELECT RAISE(ABORT, 'simulated persistence failure'); END`,
      );
      const failed = await register();
      expect(failed.statusCode).toBe(500);
      expect(app.walletPool.openCount()).toBe(0);
      await new Promise((resolve) => setImmediate(resolve));
      expect(backend.handleFor(user.userId)?.closed).toBe(true);

      // Restore persistence and register DIFFERENT xpubs: the address must come
      // from a wallet reopened with those xpubs, not from the cached handle.
      app.db.exec('DROP TRIGGER fail_xpubs_insert');
      const retry = await app.inject({
        method: 'POST',
        url: '/v1/wallet/xpubs',
        headers: authed(),
        payload: otherXpubs,
      });
      expect(retry.statusCode).toBe(201);
      expect(backend.openCalls).toHaveLength(2);
      expect(backend.openCalls[1]?.xpubs).toEqual(otherXpubs);
    });
  });

  describe('wallet reads', () => {
    it('answers 404 WALLET_NOT_REGISTERED before registration', async () => {
      for (const url of [
        '/v1/wallet/address',
        '/v1/wallet/balances',
        '/v1/wallet/unspents',
        '/v1/wallet/transfers',
      ]) {
        const response = await app.inject({ method: 'GET', url, headers: authed() });
        expect(response.statusCode).toBe(404);
        expect(response.json().error.code).toBe('WALLET_NOT_REGISTERED');
      }
    });

    it('requires auth on every wallet route', async () => {
      for (const [method, url] of [
        ['POST', '/v1/wallet/xpubs'],
        ['GET', '/v1/wallet/address'],
        ['GET', '/v1/wallet/balances'],
        ['GET', '/v1/wallet/unspents'],
        ['GET', '/v1/wallet/transfers'],
        ['POST', '/v1/wallet/receive'],
        ['POST', '/v1/wallet/sync'],
      ] as const) {
        const response = await app.inject({ method, url });
        expect(response.statusCode).toBe(401);
      }
    });

    it('returns the address of the registered wallet', async () => {
      await register();
      const response = await app.inject({
        method: 'GET',
        url: '/v1/wallet/address',
        headers: authed(),
      });
      expect(response.statusCode).toBe(200);
      // The mock hands out a synthetic address, so it cannot be attributed to a
      // derivation under the registered xpub: null evidence, not a guess.
      expect(response.json()).toEqual({ address: `addr-${user.userId}`, derivation: null });
    });

    it('attributes a real rgb-lib address to its vanilla derivation', async () => {
      // Derived from the fixture xpub at m/86'/1'/0'/0/0 — the first address
      // rgb-lib reveals on the vanilla keychain for these xpubs.
      const derived = deriveVanilla(0);
      backend.dataFor = () => ({ address: derived.address });
      await register();
      const response = await app.inject({
        method: 'GET',
        url: '/v1/wallet/address',
        headers: authed(),
      });
      expect(response.statusCode).toBe(200);
      expect(response.json()).toEqual({
        address: derived.address,
        derivation: {
          account: 'vanilla',
          keychain: 0,
          index: 0,
          derivationPath: "m/86'/1'/0'/0/0",
          scriptHex: derived.scriptHex,
        },
      });
    });

    it('attributes an address past the first index and advances the scan window', async () => {
      const derived = deriveVanilla(7);
      backend.dataFor = () => ({ address: derived.address });
      await register();
      const response = await app.inject({
        method: 'GET',
        url: '/v1/wallet/address',
        headers: authed(),
      });
      expect(response.json().derivation).toMatchObject({
        index: 7,
        derivationPath: "m/86'/1'/0'/0/7",
      });
      // High-water mark persisted, so attribution keeps working as the wallet's
      // revealed index grows instead of falling off a fixed window.
      const row = app.db
        .prepare('SELECT address_index_high_water AS h FROM user_xpubs WHERE user_id = ?')
        .get(user.userId) as { h: number };
      expect(row.h).toBe(7);
    });

    it('reports balances from the user wallet (BTC + per-asset)', async () => {
      backend.dataFor = () => ({
        btcBalance: {
          vanilla: { settled: 5000, future: 5000, spendable: 5000 },
          colored: { settled: 700, future: 700, spendable: 700 },
        },
        assets: [
          {
            assetId: 'rgb:asset-1',
            schema: 'nia',
            ticker: 'TST',
            name: 'Test',
            details: null,
            precision: 0,
            issuedSupply: 1000,
            timestamp: 1_700_000_000,
            addedAt: 1_700_000_100,
            balance: { settled: 100, future: 100, spendable: 100 },
          },
        ],
      });
      await register();
      const response = await app.inject({
        method: 'GET',
        url: '/v1/wallet/balances',
        headers: authed(),
      });
      expect(response.statusCode).toBe(200);
      const body = response.json();
      expect(body.btc.vanilla.settled).toBe(5000);
      expect(body.assets).toEqual([
        {
          assetId: 'rgb:asset-1',
          schema: 'nia',
          ticker: 'TST',
          name: 'Test',
          details: null,
          precision: 0,
          issuedSupply: 1000,
          timestamp: 1_700_000_000,
          addedAt: 1_700_000_100,
          balance: { settled: 100, future: 100, spendable: 100 },
        },
      ]);
    });

    it('lists unspents', async () => {
      backend.dataFor = () => ({
        unspents: [
          {
            txid: 'ab'.repeat(32),
            vout: 1,
            amountSat: 9999,
            colorable: true,
            exists: true,
            pendingBlinded: 2,
            allocations: [
              {
                assetId: 'rgb:asset-1',
                amount: 42,
                assignment: { kind: 'Fungible', amount: 42 },
                settled: true,
              },
            ],
          },
        ],
      });
      await register();
      const response = await app.inject({
        method: 'GET',
        url: '/v1/wallet/unspents',
        headers: authed(),
      });
      expect(response.statusCode).toBe(200);
      // pendingBlinded and the full assignment survive serialization: a client
      // counting free allocation slots needs both.
      expect(response.json().unspents[0]).toEqual({
        txid: 'ab'.repeat(32),
        vout: 1,
        amountSat: 9999,
        colorable: true,
        exists: true,
        pendingBlinded: 2,
        allocations: [
          {
            assetId: 'rgb:asset-1',
            amount: 42,
            assignment: { kind: 'Fungible', amount: 42 },
            settled: true,
          },
        ],
      });
    });

    it('aggregates transfers across assets, and filters by assetId when asked', async () => {
      const transfer = (idx: number, assetId: string | null) => ({
        idx,
        batchTransferIdx: idx + 100,
        assetId,
        amount: 10,
        assignments: [{ kind: 'Fungible', amount: 10 }],
        kind: 'ReceiveBlind',
        status: 'Settled',
        txid: null,
        recipientId: `rcpt-${idx}`,
        expiration: null,
        createdAt: 1,
        updatedAt: 2,
      });
      backend.dataFor = () => ({
        assets: [
          {
            assetId: 'rgb:a1',
            schema: 'nia',
            ticker: null,
            name: 'A1',
            details: null,
            precision: 0,
            issuedSupply: null,
            timestamp: 0,
            addedAt: 0,
            balance: EMPTY_BALANCE,
          },
        ],
        transfersByAsset: {
          null: [transfer(1, null)],
          'rgb:a1': [transfer(2, 'rgb:a1')],
        },
      });
      await register();

      const all = await app.inject({
        method: 'GET',
        url: '/v1/wallet/transfers',
        headers: authed(),
      });
      expect(all.statusCode).toBe(200);
      expect(all.json().transfers.map((t: { idx: number }) => t.idx)).toEqual([1, 2]);

      const filtered = await app.inject({
        method: 'GET',
        url: '/v1/wallet/transfers?assetId=rgb:a1',
        headers: authed(),
      });
      expect(filtered.json().transfers.map((t: { idx: number }) => t.idx)).toEqual([2]);
    });
  });

  describe('receive', () => {
    it('creates a blind receive and records ownership of the recipient id', async () => {
      await register();
      const response = await app.inject({
        method: 'POST',
        url: '/v1/wallet/receive',
        headers: authed(),
        payload: { mode: 'blind', assetId: 'rgb:a1', amount: 5 },
      });
      expect(response.statusCode).toBe(201);
      const body = response.json();
      expect(body.mode).toBe('blind');
      expect(body.invoice).toContain('mock-invoice');

      const row = app.db
        .prepare('SELECT user_id, state FROM resource_map WHERE kind = ? AND resource_id = ?')
        .get('asset_transfer', body.recipientId) as { user_id: string; state: string };
      expect(row.user_id).toBe(user.userId);
      expect(row.state).toBe('pending');
    });

    it('passes gateway-controlled transport endpoints and expiration to the wallet', async () => {
      await register();
      const before = Math.floor(Date.now() / 1000);
      await app.inject({
        method: 'POST',
        url: '/v1/wallet/receive',
        headers: authed(),
        payload: { mode: 'witness', durationSeconds: 600 },
      });
      const handle = backend.handleFor(user.userId);
      expect(handle?.operations).toContain('receive:witness');
      const request = handle?.lastReceiveRequest;
      if (request === undefined) throw new Error('receive request not recorded');
      // Transport endpoints are gateway-controlled, never client input.
      expect(request.transportEndpoints).toEqual([app.gatewayConfig.rgbProxyUrl]);
      expect(request.minConfirmations).toBe(1);
      // expirationTimestamp = floor(now/1000) + durationSeconds (±5s tolerance).
      expect(request.expirationTimestamp).toBeGreaterThanOrEqual(before + 600);
      expect(request.expirationTimestamp).toBeLessThanOrEqual(before + 600 + 5);
    });

    it('rejects a fractional receive amount at the schema layer', async () => {
      await register();
      const response = await app.inject({
        method: 'POST',
        url: '/v1/wallet/receive',
        headers: authed(),
        payload: { mode: 'blind', assetId: 'rgb:asset', amount: 1.5 },
      });
      expect(response.statusCode).toBe(400);
    });

    it('rejects an unknown mode at the schema layer', async () => {
      await register();
      const response = await app.inject({
        method: 'POST',
        url: '/v1/wallet/receive',
        headers: authed(),
        payload: { mode: 'psychic' },
      });
      expect(response.statusCode).toBe(400);
    });

    it('maps insufficient-funds failures to a clean 400, not a 500', async () => {
      await register();
      const handle = backend.handleFor(user.userId);
      if (handle === undefined) throw new Error('no handle');
      handle.failWith = new WalletBackendError(
        'wallet blindReceive failed',
        'RgbLib(InsufficientBitcoins { needed: 2000, available: 0 })',
        true,
      );
      const response = await app.inject({
        method: 'POST',
        url: '/v1/wallet/receive',
        headers: authed(),
        payload: { mode: 'blind' },
      });
      expect(response.statusCode).toBe(400);
      expect(response.json().error.code).toBe('INSUFFICIENT_FUNDS');
      // The rgb-lib detail must not leak to the client.
      expect(response.body).not.toContain('RgbLib');
    });
  });

  describe('sync and isolation', () => {
    it('syncs then refreshes the user wallet', async () => {
      await register();
      const response = await app.inject({
        method: 'POST',
        url: '/v1/wallet/sync',
        headers: authed(),
      });
      expect(response.statusCode).toBe(200);
      const handle = backend.handleFor(user.userId);
      expect(handle?.syncCount).toBe(1);
      expect(handle?.refreshCount).toBe(1);
    });

    it("keeps users isolated: a second user never touches the first user's wallet", async () => {
      await register();
      const other = await createTestUser(app);
      const response = await app.inject({
        method: 'GET',
        url: '/v1/wallet/address',
        headers: authed(other.token),
      });
      expect(response.statusCode).toBe(404);
      expect(backend.openCalls.every((call) => call.userId === user.userId)).toBe(true);
    });
  });
});
