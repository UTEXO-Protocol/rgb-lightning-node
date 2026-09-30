/**
 * On-chain prepare/complete route tests with a mocked wallet backend: intent
 * summaries, pending-op lifecycle (TTL, kind, ownership), idempotency, and
 * clean error mapping for rejected PSBTs.
 */
import type { FastifyInstance } from 'fastify';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { randomUUID } from 'node:crypto';
import { base64, hex } from '@scure/base';
import { Transaction } from '@scure/btc-signer';
import { WalletBackendError } from '../src/wallets/backend.js';
import { txidFromPsbt } from '../src/wallets/prepare.js';
import { createTestUser, testServer, type TestUser } from './helpers.js';
import { MockWalletBackend } from './wallet-mocks.js';

const XPUBS = {
  vanilla:
    'tpubDDfvzhdVV4unsoKt5aE6dcsNsfeWbTgmLZPi8LQDYU2xixrYemMfWJ3BaVneH3u7DBQePdTwhpybaKRU95pi6PMUtLPBJLVQRpzEnjfjZzX',
  colored:
    'tpubDCtpoJs6YJcjLnr9gq6jYriYNMuWEu8mSDvEQU5st3ZkJbFqqzwpHUiPvxqD2366ciFAfpehk1k2d7Tyk7AJEr8uZva7KfnX4RpsiVSoEcZ',
  fingerprint: '73c5da0a',
};

const RECIPIENT_ADDRESS = 'bcrt1p8wpt9v4frpf3tkn0srd97pksgsxc5hs52lafxwru9kgeephvs7rqjeprhg';
// Independently computed p2tr output script for RECIPIENT_ADDRESS (bech32m).
const RECIPIENT_SCRIPT_HEX = '51203b82b2b2a9185315da6f80da5f06d0440d8a5e1457fa93387c2d919c86ec8786';
const SIGNED_PSBT = 'cHNidP8BAAAAAAAAAAAAAAAAAAAAAAAA';
const ASSET_ID = 'rgb:aaaaaaa-bbbbbbb-ccccccc';

/** Minimal parsable PSBT spending outpoint `prevTxidHexChar.repeat(64)`:0. */
function parsablePsbt(prevTxidHexChar: string): string {
  const tx = new Transaction({ allowUnknownInputs: true, allowUnknownOutputs: true });
  tx.addInput({ txid: prevTxidHexChar.repeat(64), index: 0 });
  tx.addOutput({ script: hex.decode(RECIPIENT_SCRIPT_HEX), amount: 40_000n });
  return base64.encode(tx.toPSBT());
}
const PARSABLE_PSBT_A = parsablePsbt('a');
const PARSABLE_PSBT_B = parsablePsbt('b');

describe('on-chain prepare/complete routes', () => {
  let app: FastifyInstance;
  let backend: MockWalletBackend;
  let user: TestUser;

  beforeEach(async () => {
    backend = new MockWalletBackend();
    app = await testServer({ walletBackend: backend });
    user = await createTestUser(app);
    const registered = await app.inject({
      method: 'POST',
      url: '/v1/wallet/xpubs',
      headers: { authorization: `Bearer ${user.token}` },
      payload: XPUBS,
    });
    expect(registered.statusCode).toBe(201);
  });

  afterEach(async () => {
    await app.close();
  });

  function headers(options: { token?: string; key?: string } = {}) {
    return {
      authorization: `Bearer ${options.token ?? user.token}`,
      'idempotency-key': options.key ?? randomUUID(),
    };
  }

  async function prepareSendBtc(options: { key?: string; payload?: object; token?: string } = {}) {
    return app.inject({
      method: 'POST',
      url: '/v1/onchain/send-btc/prepare',
      headers: headers(options),
      payload: options.payload ?? { address: RECIPIENT_ADDRESS, amountSat: 40_000 },
    });
  }

  async function complete(
    flow: string,
    opId: string,
    options: { key?: string; token?: string; signedPsbt?: string } = {},
  ) {
    return app.inject({
      method: 'POST',
      url: `/v1/onchain/${flow}/complete`,
      headers: headers(options),
      payload: { opId, signedPsbt: options.signedPsbt ?? SIGNED_PSBT },
    });
  }

  function opRow(opId: string) {
    return app.db.prepare('SELECT * FROM pending_ops WHERE id = ?').get(opId) as
      { state: string; txid: string | null; user_id: string; kind: string } | undefined;
  }

  describe('auth and idempotency requirements', () => {
    const routes = [
      '/v1/onchain/send-btc/prepare',
      '/v1/onchain/send-btc/complete',
      '/v1/onchain/send-asset/prepare',
      '/v1/onchain/send-asset/complete',
      '/v1/onchain/create-utxos/prepare',
      '/v1/onchain/create-utxos/complete',
    ];

    it('rejects unauthenticated calls on every route', async () => {
      for (const url of routes) {
        const response = await app.inject({ method: 'POST', url, payload: {} });
        expect(response.statusCode, url).toBe(401);
      }
    });

    it('requires an Idempotency-Key on every route', async () => {
      const validBodies: Record<string, object> = {
        '/v1/onchain/send-btc/prepare': { address: RECIPIENT_ADDRESS, amountSat: 40_000 },
        '/v1/onchain/send-btc/complete': { opId: randomUUID(), signedPsbt: SIGNED_PSBT },
        '/v1/onchain/send-asset/prepare': { assetId: ASSET_ID, amount: 1, recipientId: 'r1' },
        '/v1/onchain/send-asset/complete': { opId: randomUUID(), signedPsbt: SIGNED_PSBT },
        '/v1/onchain/create-utxos/prepare': {},
        '/v1/onchain/create-utxos/complete': { opId: randomUUID(), signedPsbt: SIGNED_PSBT },
      };
      for (const url of routes) {
        const response = await app.inject({
          method: 'POST',
          url,
          headers: { authorization: `Bearer ${user.token}` },
          payload: validBodies[url],
        });
        expect(response.statusCode, url).toBe(400);
        expect(response.json().error.code, url).toBe('IDEMPOTENCY_KEY_REQUIRED');
      }
    });
  });

  describe('send-btc', () => {
    it('prepares with an intent summary built from request inputs', async () => {
      const before = Date.now();
      const response = await prepareSendBtc();
      expect(response.statusCode).toBe(201);
      const body = response.json();
      expect(body.psbt).toBe(`mock-psbt-${user.userId}`);
      expect(body.intent).toEqual({
        kind: 'send_btc',
        feeRateSatPerVb: 2,
        recipients: [
          {
            address: RECIPIENT_ADDRESS,
            scriptHex: RECIPIENT_SCRIPT_HEX,
            amountSat: 40_000,
          },
        ],
        asset: null,
        utxos: null,
      });
      expect(body.expiresAt).toBeGreaterThanOrEqual(before + 600_000);
      expect(opRow(body.opId)?.state).toBe('pending');
      expect(backend.handleFor(user.userId)?.lastSendBtcArgs).toEqual({
        address: RECIPIENT_ADDRESS,
        amountSat: 40_000,
        feeRateSatPerVb: 2,
      });
    });

    it('honors an explicit fee rate', async () => {
      const response = await prepareSendBtc({
        payload: { address: RECIPIENT_ADDRESS, amountSat: 40_000, feeRateSatPerVb: 7 },
      });
      expect(response.json().intent.feeRateSatPerVb).toBe(7);
      expect(backend.handleFor(user.userId)?.lastSendBtcArgs?.feeRateSatPerVb).toBe(7);
    });

    it('rejects a malformed and a wrong-network address with 400 INVALID_ADDRESS', async () => {
      for (const address of [
        'notanaddressnotanaddress',
        'bc1p8wpt9v4frpf3tkn0srd97pksgsxc5hs52lafxwru9kgeephvs7rqu9khtt',
      ]) {
        const response = await prepareSendBtc({ payload: { address, amountSat: 40_000 } });
        expect(response.statusCode, address).toBe(400);
        expect(response.json().error.code, address).toBe('INVALID_ADDRESS');
      }
    });

    it('maps insufficient funds at prepare time to 400 without leaking detail', async () => {
      const handle = backend.handleFor(user.userId);
      expect(handle).toBeDefined();
      handle!.failWith = new WalletBackendError('boom', 'InsufficientBitcoins { needed: 1 }', true);
      const response = await prepareSendBtc();
      expect(response.statusCode).toBe(400);
      expect(response.json().error.code).toBe('INSUFFICIENT_FUNDS');
      expect(response.body).not.toContain('InsufficientBitcoins');
    });

    it('maps a client-caused rgb-lib rejection at prepare time to 400, not 500', async () => {
      const handle = backend.handleFor(user.userId);
      expect(handle).toBeDefined();
      handle!.failWith = new WalletBackendError(
        'wallet sendAssetBegin failed',
        'RgbLib(InvalidRecipientNetwork)',
        false,
        true,
      );
      const response = await prepareSendBtc();
      expect(response.statusCode).toBe(400);
      expect(response.json().error.code).toBe('WALLET_REQUEST_REJECTED');
      // The rgb-lib detail stays server-side.
      expect(response.body).not.toContain('InvalidRecipientNetwork');
    });

    it('still reports an unclassified wallet failure as 500 INTERNAL', async () => {
      const handle = backend.handleFor(user.userId);
      handle!.failWith = new WalletBackendError(
        'wallet sendBtcBegin failed',
        'RgbLib(Internal { details: "stash corrupted" })',
      );
      const response = await prepareSendBtc();
      expect(response.statusCode).toBe(500);
      expect(response.json().error.code).toBe('INTERNAL');
      expect(response.body).not.toContain('stash corrupted');
    });

    it('completes with the signed PSBT and reports the broadcast txid', async () => {
      const { opId } = (await prepareSendBtc()).json();
      const response = await complete('send-btc', opId);
      expect(response.statusCode).toBe(200);
      expect(response.json()).toEqual({ txid: 'mock-btc-txid' });
      expect(backend.handleFor(user.userId)?.lastSignedPsbt).toBe(SIGNED_PSBT);
      expect(opRow(opId)).toMatchObject({ state: 'completed', txid: 'mock-btc-txid' });
    });

    it('replays an identical complete idempotently and refuses re-completion', async () => {
      const { opId } = (await prepareSendBtc()).json();
      const key = randomUUID();
      const first = await complete('send-btc', opId, { key });
      expect(first.statusCode).toBe(200);
      const replay = await complete('send-btc', opId, { key });
      expect(replay.statusCode).toBe(200);
      expect(replay.headers['x-idempotent-replay']).toBe('true');
      expect(replay.json()).toEqual({ txid: 'mock-btc-txid' });
      // The wallet ran exactly once.
      const ends = backend
        .handleFor(user.userId)!
        .operations.filter((op) => op === 'sendBtcEnd').length;
      expect(ends).toBe(1);
      const again = await complete('send-btc', opId);
      expect(again.statusCode).toBe(409);
      expect(again.json().error.code).toBe('OP_ALREADY_COMPLETED');
    });

    it('replays an identical prepare idempotently (same opId, no second wallet call)', async () => {
      const key = randomUUID();
      const first = (await prepareSendBtc({ key })).json();
      const replay = await prepareSendBtc({ key });
      expect(replay.json().opId).toBe(first.opId);
      expect(replay.headers['x-idempotent-replay']).toBe('true');
      const begins = backend
        .handleFor(user.userId)!
        .operations.filter((op) => op === 'sendBtcBegin').length;
      expect(begins).toBe(1);
    });

    it('rejects reuse of an idempotency key with a different request', async () => {
      const key = randomUUID();
      await prepareSendBtc({ key });
      const conflict = await prepareSendBtc({
        key,
        payload: { address: RECIPIENT_ADDRESS, amountSat: 50_000 },
      });
      expect(conflict.statusCode).toBe(409);
      expect(conflict.json().error.code).toBe('IDEMPOTENCY_KEY_REUSED');
    });

    it('hides another user’s op (404, not 409/410)', async () => {
      const { opId } = (await prepareSendBtc()).json();
      const stranger = await createTestUser(app);
      await app.inject({
        method: 'POST',
        url: '/v1/wallet/xpubs',
        headers: { authorization: `Bearer ${stranger.token}` },
        payload: { ...XPUBS, fingerprint: 'deadbeef' },
      });
      const response = await complete('send-btc', opId, { token: stranger.token });
      expect(response.statusCode).toBe(404);
      expect(response.json().error.code).toBe('OP_NOT_FOUND');
      expect(opRow(opId)?.state).toBe('pending');
    });

    it('expires a stale op with 410 and marks it expired', async () => {
      const { opId } = (await prepareSendBtc()).json();
      app.db
        .prepare('UPDATE pending_ops SET expires_at = ? WHERE id = ?')
        .run(Date.now() - 1, opId);
      const response = await complete('send-btc', opId);
      expect(response.statusCode).toBe(410);
      expect(response.json().error.code).toBe('OP_EXPIRED');
      expect(opRow(opId)?.state).toBe('expired');
    });

    it('rejects completion through the wrong flow with 409 OP_KIND_MISMATCH', async () => {
      const { opId } = (await prepareSendBtc()).json();
      const response = await complete('create-utxos', opId);
      expect(response.statusCode).toBe(409);
      expect(response.json().error.code).toBe('OP_KIND_MISMATCH');
    });

    it('maps a wallet-rejected PSBT to a clean 400 and keeps the op pending', async () => {
      const { opId } = (await prepareSendBtc()).json();
      const handle = backend.handleFor(user.userId)!;
      handle.failWith = new WalletBackendError(
        'wallet sendBtcEnd failed',
        'RgbLib BdkError InvalidPsbt: signature verification failed',
      );
      const rejected = await complete('send-btc', opId);
      expect(rejected.statusCode).toBe(400);
      expect(rejected.json().error.code).toBe('PSBT_REJECTED');
      expect(rejected.body).not.toContain('RgbLib');
      expect(opRow(opId)?.state).toBe('pending');
      // A corrected retry (fresh key) succeeds against the same op.
      handle.failWith = undefined;
      const retry = await complete('send-btc', opId);
      expect(retry.statusCode).toBe(200);
      expect(opRow(opId)?.state).toBe('completed');
    });

    it('maps a wallet failure that may have broadcast to 502 COMPLETE_AMBIGUOUS', async () => {
      backend.dataFor = () => ({ preparedPsbt: PARSABLE_PSBT_A });
      app.walletPool.evict(user.userId); // reopen so the fixture PSBT applies
      const { opId } = (await prepareSendBtc()).json();
      const handle = backend.handleFor(user.userId)!;
      // rgb-lib broadcasts BEFORE it writes its bookkeeping, so a DB/IO failure
      // here can belong to a transaction that is already on the network:
      // blaming the signature would be a lie, and 4xx would cache it forever.
      handle.failWith = new WalletBackendError(
        'wallet sendBtcEnd failed',
        'RgbLib(Database { details: "error returned from database: disk I/O error" })',
      );
      const ambiguous = await complete('send-btc', opId, { signedPsbt: PARSABLE_PSBT_A });
      expect(ambiguous.statusCode).toBe(502);
      expect(ambiguous.json().error.code).toBe('COMPLETE_AMBIGUOUS');
      expect(ambiguous.body).not.toContain('RgbLib');
      // Op stays pending with the txid recorded, so a retry can finish the
      // bookkeeping and an operator can find the transaction either way.
      const row = opRow(opId);
      expect(row?.state).toBe('pending');
      expect(row?.txid).toBe(txidFromPsbt(PARSABLE_PSBT_A));
      expect(row?.txid).toMatch(/^[0-9a-f]{64}$/);

      handle.failWith = undefined;
      const retry = await complete('send-btc', opId, { signedPsbt: PARSABLE_PSBT_A });
      expect(retry.statusCode).toBe(200);
      expect(opRow(opId)?.state).toBe('completed');
    });

    it('maps a no-longer-affordable completion to 400 INSUFFICIENT_FUNDS', async () => {
      const { opId } = (await prepareSendBtc()).json();
      backend.handleFor(user.userId)!.failWith = new WalletBackendError(
        'wallet sendBtcEnd failed',
        'InsufficientBitcoins { needed: 1, available: 0 }',
        true,
      );
      const response = await complete('send-btc', opId);
      expect(response.statusCode).toBe(400);
      expect(response.json().error.code).toBe('INSUFFICIENT_FUNDS');
      expect(response.body).not.toContain('InsufficientBitcoins');
    });

    it('rejects a signed PSBT that belongs to a different prepared transaction', async () => {
      backend.dataFor = () => ({ preparedPsbt: PARSABLE_PSBT_A });
      app.walletPool.evict(user.userId); // reopen so the fixture PSBT applies
      const { opId } = (await prepareSendBtc()).json();
      const mismatch = await complete('send-btc', opId, { signedPsbt: PARSABLE_PSBT_B });
      expect(mismatch.statusCode).toBe(400);
      expect(mismatch.json().error.code).toBe('PSBT_MISMATCH');
      expect(opRow(opId)?.state).toBe('pending');
      // The matching PSBT still completes the op.
      const ok = await complete('send-btc', opId, { signedPsbt: PARSABLE_PSBT_A });
      expect(ok.statusCode).toBe(200);
      expect(opRow(opId)?.state).toBe('completed');
    });

    it('rejects an unparsable signed PSBT instead of skipping the binding check', async () => {
      backend.dataFor = () => ({ preparedPsbt: PARSABLE_PSBT_A });
      app.walletPool.evict(user.userId);
      const { opId } = (await prepareSendBtc()).json();
      // Well-formed base64 that is not a PSBT: passes the schema, so the
      // binding check itself must reject it rather than treat an unparsable
      // txid as “no opinion”.
      const garbage = await complete('send-btc', opId, { signedPsbt: 'A'.repeat(24) });
      expect(garbage.statusCode).toBe(400);
      expect(garbage.json().error.code).toBe('PSBT_MISMATCH');
      expect(opRow(opId)?.state).toBe('pending');
      // Non-base64 never reaches the service: the request schema rejects it.
      const nonBase64 = await complete('send-btc', opId, { signedPsbt: 'not-a-psbt-at-all!!!!' });
      expect(nonBase64.statusCode).toBe(400);
      expect(nonBase64.json().error.code).toBe('BAD_REQUEST');
      expect(opRow(opId)?.state).toBe('pending');
    });

    it('retires the user’s stale pending ops lazily on every prepare', async () => {
      const { opId: staleId } = (await prepareSendBtc()).json();
      app.db
        .prepare('UPDATE pending_ops SET expires_at = ? WHERE id = ?')
        .run(Date.now() - 1, staleId);
      const { opId: freshId } = (await prepareSendBtc()).json();
      expect(opRow(staleId)?.state).toBe('expired');
      expect(opRow(freshId)?.state).toBe('pending');
    });
  });

  describe('send-asset', () => {
    async function prepareSendAsset(payload: object = {}) {
      return app.inject({
        method: 'POST',
        url: '/v1/onchain/send-asset/prepare',
        headers: headers(),
        payload: {
          assetId: ASSET_ID,
          amount: 25,
          recipientId: 'utxob:fixture-recipient',
          ...payload,
        },
      });
    }

    it('rejects a fractional asset amount at the schema layer', async () => {
      // An RGB fungible amount deserializes into a u64 on the rgb-lib side, so
      // a fractional one fails there and surfaces as an unclassified 500.
      const response = await prepareSendAsset({ amount: 1.5 });
      expect(response.statusCode).toBe(400);
    });

    it('prepares with asset intent and gateway-controlled defaults', async () => {
      const response = await prepareSendAsset();
      expect(response.statusCode).toBe(201);
      const body = response.json();
      expect(body.intent).toEqual({
        kind: 'send_asset',
        feeRateSatPerVb: 2,
        recipients: [],
        asset: {
          assetId: ASSET_ID,
          amount: 25,
          recipientId: 'utxob:fixture-recipient',
          witnessAmountSat: null,
          transportEndpoints: [app.gatewayConfig.rgbProxyUrl],
        },
        utxos: null,
      });
      const request = backend.handleFor(user.userId)?.lastSendAssetRequest;
      expect(request).toMatchObject({
        assetId: ASSET_ID,
        amount: 25,
        donation: false,
        minConfirmations: 1,
        witnessAmountSat: null,
      });
      // The wallet-side pending transfer expires together with the op.
      expect(request!.expirationTimestamp).toBe(Math.floor(body.expiresAt / 1000));
    });

    it('passes through witness data and pinned allowlisted transport endpoints', async () => {
      const allowlisted = app.gatewayConfig.rgbProxyUrl;
      const response = await prepareSendAsset({
        witnessAmountSat: 1000,
        transportEndpoints: [allowlisted],
      });
      expect(response.json().intent.asset).toMatchObject({
        witnessAmountSat: 1000,
        transportEndpoints: [allowlisted],
      });
    });

    it('rejects transport endpoints outside the operator allowlist (SSRF guard)', async () => {
      const response = await prepareSendAsset({
        transportEndpoints: ['rpc://attacker.internal/json-rpc'],
      });
      expect(response.statusCode).toBe(400);
      expect(response.json().error.code).toBe('TRANSPORT_ENDPOINT_NOT_ALLOWED');
    });

    it('records the completed transfer in resource_map for scoping', async () => {
      const { opId } = (await prepareSendAsset()).json();
      const response = await complete('send-asset', opId);
      expect(response.statusCode).toBe(200);
      expect(response.json()).toEqual({ txid: 'mock-asset-txid' });
      const owned = app.db
        .prepare('SELECT user_id, state FROM resource_map WHERE kind = ? AND resource_id = ?')
        .get('asset_transfer', 'mock-asset-txid') as { user_id: string; state: string };
      expect(owned).toEqual({ user_id: user.userId, state: 'sent' });
    });
  });

  describe('create-utxos', () => {
    it('prepares with defaults and completes reporting count and txid', async () => {
      const prepared = await app.inject({
        method: 'POST',
        url: '/v1/onchain/create-utxos/prepare',
        headers: headers(),
        payload: {},
      });
      expect(prepared.statusCode).toBe(201);
      const body = prepared.json();
      expect(body.intent).toEqual({
        kind: 'create_utxos',
        feeRateSatPerVb: 2,
        recipients: [],
        asset: null,
        utxos: { upTo: false, num: 5, size: 1000 },
      });
      expect(backend.handleFor(user.userId)?.lastCreateUtxosParams).toEqual({
        upTo: false,
        num: 5,
        size: 1000,
        feeRateSatPerVb: 2,
      });
      const response = await complete('create-utxos', body.opId);
      expect(response.statusCode).toBe(200);
      // SIGNED_PSBT is not a parsable PSBT, so the txid is null (best-effort).
      expect(response.json()).toEqual({ txid: null, utxosCreated: 5 });
    });

    it('honors explicit num/size/upTo', async () => {
      const prepared = await app.inject({
        method: 'POST',
        url: '/v1/onchain/create-utxos/prepare',
        headers: headers(),
        payload: { num: 3, size: 32_000, upTo: true },
      });
      expect(prepared.json().intent.utxos).toEqual({ upTo: true, num: 3, size: 32_000 });
    });
  });

  describe('GET /v1/onchain/operations/:opId', () => {
    async function getOp(opId: string, token?: string) {
      return app.inject({
        method: 'GET',
        url: `/v1/onchain/operations/${opId}`,
        headers: { authorization: `Bearer ${token ?? user.token}` },
      });
    }

    it('reports a freshly prepared op as pending with no txid', async () => {
      const prepared = (await prepareSendBtc()).json();
      const response = await getOp(prepared.opId);
      expect(response.statusCode).toBe(200);
      expect(response.json()).toEqual({
        opId: prepared.opId,
        kind: 'send_btc',
        state: 'pending',
        txid: null,
        mayHaveBroadcast: false,
        intent: prepared.intent,
        createdAt: expect.any(Number),
        expiresAt: prepared.expiresAt,
      });
    });

    it('reports the final txid once the op completed', async () => {
      const { opId } = (await prepareSendBtc()).json();
      const completed = await complete('send-btc', opId);
      expect(completed.statusCode).toBe(200);
      const body = (await getOp(opId)).json();
      expect(body.state).toBe('completed');
      expect(body.txid).toBe(completed.json().txid);
      expect(body.mayHaveBroadcast).toBe(false);
    });

    // The reason this route exists: after a 502 the client does not know whether
    // its money moved, and the op carries the answer.
    it('surfaces mayHaveBroadcast with the txid after COMPLETE_AMBIGUOUS', async () => {
      backend.dataFor = () => ({ preparedPsbt: PARSABLE_PSBT_A });
      app.walletPool.evict(user.userId);
      const { opId } = (await prepareSendBtc()).json();
      backend.handleFor(user.userId)!.failWith = new WalletBackendError(
        'wallet sendBtcEnd failed',
        'RgbLib(Database { details: "error returned from database: disk I/O error" })',
      );
      const ambiguous = await complete('send-btc', opId, { signedPsbt: PARSABLE_PSBT_A });
      expect(ambiguous.statusCode).toBe(502);

      const body = (await getOp(opId)).json();
      expect(body.state).toBe('pending');
      expect(body.txid).toBe(txidFromPsbt(PARSABLE_PSBT_A));
      expect(body.mayHaveBroadcast).toBe(true);

      // And it follows the op through the documented recovery: retry completes.
      backend.handleFor(user.userId)!.failWith = undefined;
      expect((await complete('send-btc', opId, { signedPsbt: PARSABLE_PSBT_A })).statusCode).toBe(
        200,
      );
      const after = (await getOp(opId)).json();
      expect(after.state).toBe('completed');
      expect(after.mayHaveBroadcast).toBe(false);
    });

    it('derives expiry past the TTL without writing to the row', async () => {
      const { opId } = (await prepareSendBtc()).json();
      app.db
        .prepare('UPDATE pending_ops SET expires_at = ? WHERE id = ?')
        .run(Date.now() - 1, opId);
      expect((await getOp(opId)).json().state).toBe('expired');
      // A read must not mutate: the stored state is still what complete() will
      // find and flip itself.
      expect(opRow(opId)?.state).toBe('pending');
    });

    // An expired op whose txid was recorded may still have its transaction on
    // the network, so the signal must survive the derived expiry.
    it('keeps mayHaveBroadcast on an expired op that recorded a txid', async () => {
      const { opId } = (await prepareSendBtc()).json();
      app.db
        .prepare('UPDATE pending_ops SET expires_at = ?, txid = ? WHERE id = ?')
        .run(Date.now() - 1, 'f'.repeat(64), opId);
      const body = (await getOp(opId)).json();
      expect(body.state).toBe('expired');
      expect(body.mayHaveBroadcast).toBe(true);
    });

    it("hides another user's op behind the same 404 as a missing one (I3)", async () => {
      const { opId } = (await prepareSendBtc()).json();
      const stranger = await createTestUser(app);
      const foreign = await getOp(opId, stranger.token);
      const missing = await getOp(randomUUID(), stranger.token);
      expect(foreign.statusCode).toBe(404);
      expect(missing.statusCode).toBe(404);
      expect(foreign.json()).toEqual(missing.json());
      expect(foreign.json().error.code).toBe('OP_NOT_FOUND');
    });

    it('requires auth and never echoes the PSBT', async () => {
      const prepared = (await prepareSendBtc()).json();
      const unauthenticated = await app.inject({
        method: 'GET',
        url: `/v1/onchain/operations/${prepared.opId}`,
      });
      expect(unauthenticated.statusCode).toBe(401);
      const body = (await getOp(prepared.opId)).body;
      expect(body).not.toContain(prepared.psbt);
      expect(JSON.parse(body)).not.toHaveProperty('psbt');
    });

    it('rejects a malformed opId at the schema layer', async () => {
      expect((await getOp('not-a-uuid')).statusCode).toBe(400);
    });
  });

  describe('input validation', () => {
    it('rejects a non-base64 signed PSBT at the schema layer', async () => {
      const response = await app.inject({
        method: 'POST',
        url: '/v1/onchain/send-btc/complete',
        headers: headers(),
        payload: { opId: randomUUID(), signedPsbt: 'not base64 !!!' },
      });
      expect(response.statusCode).toBe(400);
    });

    it('rejects a prepare for an unregistered wallet with 404', async () => {
      const stranger = await createTestUser(app);
      const response = await prepareSendBtc({ token: stranger.token });
      expect(response.statusCode).toBe(404);
      expect(response.json().error.code).toBe('WALLET_NOT_REGISTERED');
    });
  });
});
