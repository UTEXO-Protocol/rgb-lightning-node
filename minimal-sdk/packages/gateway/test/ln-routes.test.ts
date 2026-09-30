import { createHash, randomUUID } from 'node:crypto';
import type { FastifyInstance } from 'fastify';
import { afterEach, describe, expect, it } from 'vitest';
import { createTestUser, testServer, type TestServerOptions, type TestUser } from './helpers.js';
import {
  decodedInvoice,
  fakeRln,
  HASH_A,
  HASH_B,
  inboundPayment,
  outboundPayment,
  nowSec,
  rlnRejection,
  rlnTimeout,
  TEST_INVOICE,
  type RlnOverrides,
} from './ln-mocks.js';
import { BTC_ASSET } from '../src/ledger.js';
import { recordOwnership } from '../src/rln/scoping.js';

const ASSET = 'rgb:AssetTest111111111111111111111';
/** Matches PAYMENTS_PAGE_SIZE in src/routes/ln.ts. */
const PAGE = 100;

describe('LN routes', () => {
  let app: FastifyInstance;

  afterEach(async () => {
    await app.close();
  });

  async function setup(overrides: RlnOverrides, options: TestServerOptions = {}) {
    const rln = fakeRln(overrides);
    app = await testServer({ ...options, rlnClient: rln });
    const user = await createTestUser(app);
    return { rln, user };
  }

  function fund(user: TestUser, amountMsat: number, asset = BTC_ASSET): void {
    app.ledger.credit({
      userId: user.userId,
      asset,
      amount: amountMsat,
      kind: 'deposit',
      ref: randomUUID(),
    });
  }

  function pay(user: TestUser, key: string, body: Record<string, unknown> = {}) {
    return app.inject({
      method: 'POST',
      url: '/v1/ln/pay',
      headers: { authorization: `Bearer ${user.token}`, 'idempotency-key': key },
      payload: { invoice: TEST_INVOICE, ...body },
    });
  }

  function withdrawFor(user: TestUser, key: string, body: Record<string, unknown>) {
    return app.inject({
      method: 'POST',
      url: '/v1/ln/withdraw',
      headers: { authorization: `Bearer ${user.token}`, 'idempotency-key': key },
      payload: body,
    });
  }

  function balance(user: TestUser, asset = BTC_ASSET): number {
    return app.ledger.balance(user.userId, asset);
  }

  describe('pay', () => {
    it('debits, sends and reports pending on the happy path', async () => {
      const { rln, user } = await setup({
        decodeLnInvoice: () => decodedInvoice(HASH_A, 100_000),
        sendPayment: () => ({ payment_id: HASH_A, status: 'Pending' }),
      });
      fund(user, 500_000);
      const response = await pay(user, 'pay-1');
      expect(response.statusCode).toBe(200);
      expect(response.json()).toEqual({ paymentHash: HASH_A, status: 'pending' });
      expect(balance(user)).toBe(400_000);
      expect(rln.calls).toEqual(['decodeLnInvoice', 'sendPayment']);
    });

    it('rejects an assetAmount that conflicts with the invoice', async () => {
      const { rln, user } = await setup({
        decodeLnInvoice: () => decodedInvoice(HASH_A, 3_000, { assetId: ASSET, amount: 500 }),
      });
      fund(user, 500_000);
      fund(user, 500, ASSET);
      const response = await pay(user, 'pay-1', { assetAmount: 50 });
      expect(response.statusCode).toBe(400);
      expect(response.json().error.code).toBe('AMOUNT_MISMATCH');
      // Nothing sent, nothing debited: the caller never approved 500 units.
      expect(rln.calls).not.toContain('sendPayment');
      expect(balance(user, ASSET)).toBe(500);
    });

    it('rejects a zero-amount invoice with 400 rather than an unhandled 500', async () => {
      // `lnbc0…` decodes to amt_msat 0 — lightning-invoice only rejects amounts
      // that are not a whole msat — and 0 slips past a `?? body.amtMsat`
      // fallback straight into the ledger's RangeError.
      const { rln, user } = await setup({
        decodeLnInvoice: () => decodedInvoice(HASH_A, 0),
      });
      fund(user, 500_000);
      const response = await pay(user, 'pay-1');
      expect(response.statusCode).toBe(400);
      expect(response.json().error.code).toBe('INVALID_INVOICE');
      expect(balance(user)).toBe(500_000);
      expect(rln.calls).not.toContain('sendPayment');
    });

    it('rejects an out-of-range invoice amount with 400', async () => {
      const { rln, user } = await setup({
        // Above 2^53: representable as a double, but not a safe integer.
        decodeLnInvoice: () => decodedInvoice(HASH_A, Number.MAX_SAFE_INTEGER + 1),
      });
      fund(user, 500_000);
      const response = await pay(user, 'pay-1');
      expect(response.statusCode).toBe(400);
      expect(response.json().error.code).toBe('INVALID_INVOICE');
      expect(rln.calls).not.toContain('sendPayment');
    });

    it('rejects an out-of-range invoice asset amount with 400', async () => {
      const { rln, user } = await setup({
        decodeLnInvoice: () =>
          decodedInvoice(HASH_A, 3_000, { assetId: ASSET, amount: Number.MAX_SAFE_INTEGER + 1 }),
      });
      fund(user, 500_000);
      fund(user, 500, ASSET);
      const response = await pay(user, 'pay-1');
      expect(response.statusCode).toBe(400);
      expect(response.json().error.code).toBe('INVALID_INVOICE');
      expect(balance(user, ASSET)).toBe(500);
      expect(rln.calls).not.toContain('sendPayment');
    });

    it('rejects when the ledger balance is too low, without sending', async () => {
      const { rln, user } = await setup({
        decodeLnInvoice: () => decodedInvoice(HASH_A, 100_000),
      });
      fund(user, 99_999);
      const response = await pay(user, 'pay-1');
      expect(response.statusCode).toBe(409);
      expect(response.json().error.code).toBe('INSUFFICIENT_BALANCE');
      expect(balance(user)).toBe(99_999);
      expect(rln.calls).not.toContain('sendPayment');
    });

    it('refunds the debit when RLN definitively rejects the send', async () => {
      const { user } = await setup({
        decodeLnInvoice: () => decodedInvoice(HASH_A, 100_000),
        sendPayment: () => {
          throw rlnRejection(400);
        },
      });
      fund(user, 500_000);
      const response = await pay(user, 'pay-1');
      // An RLN 4xx is a rejection of THIS payment, not an unhealthy node.
      expect(response.statusCode).toBe(400);
      expect(response.json().error.code).toBe('PAYMENT_REJECTED');
      expect(balance(user)).toBe(500_000);
      const row = app.db
        .prepare("SELECT state FROM resource_map WHERE kind = 'payment_hash' AND resource_id = ?")
        .get(HASH_A) as { state: string };
      expect(row.state).toBe('failed');
    });

    it('leaves an ambiguous send debited-pending and never auto-retries', async () => {
      let sends = 0;
      const { rln, user } = await setup({
        decodeLnInvoice: () => decodedInvoice(HASH_A, 100_000),
        sendPayment: () => {
          sends += 1;
          throw rlnTimeout();
        },
      });
      fund(user, 500_000);
      const first = await pay(user, 'pay-1');
      expect(first.statusCode).toBe(504);
      expect(balance(user)).toBe(400_000);
      const row = app.db
        .prepare("SELECT state FROM resource_map WHERE kind = 'payment_hash' AND resource_id = ?")
        .get(HASH_A) as { state: string };
      expect(row.state).toBe('debited_pending');

      // A retry (new key) observes the pending state; sendPayment is NOT called again.
      const second = await pay(user, 'pay-2');
      expect(second.statusCode).toBe(200);
      expect(second.json()).toEqual({ paymentHash: HASH_A, status: 'pending' });
      expect(sends).toBe(1);
      expect(balance(user)).toBe(400_000);
      expect(rln.calls.filter((c) => c === 'sendPayment')).toHaveLength(1);
    });

    it('resolves an ambiguous payment via the reconciler: success keeps the debit', async () => {
      const { user } = await setup({
        decodeLnInvoice: () => decodedInvoice(HASH_A, 100_000),
        sendPayment: () => {
          throw rlnTimeout();
        },
        getPayment: () => ({ payment: outboundPayment(HASH_A, 'Succeeded', 100_000) }),
        listPayments: () => ({ payments: [], first_index_offset: 0, last_index_offset: 0 }),
      });
      fund(user, 500_000);
      await pay(user, 'pay-1');
      await app.reconciler.runOnce();
      expect(balance(user)).toBe(400_000);
      const row = app.db
        .prepare("SELECT state FROM resource_map WHERE kind = 'payment_hash' AND resource_id = ?")
        .get(HASH_A) as { state: string };
      expect(row.state).toBe('succeeded');
    });

    it('resolves an ambiguous payment via the reconciler: failure refunds', async () => {
      const { user } = await setup({
        decodeLnInvoice: () => decodedInvoice(HASH_A, 100_000),
        sendPayment: () => {
          throw rlnTimeout();
        },
        getPayment: () => ({ payment: outboundPayment(HASH_A, 'Failed', 100_000) }),
        listPayments: () => ({ payments: [], first_index_offset: 0, last_index_offset: 0 }),
      });
      fund(user, 500_000);
      await pay(user, 'pay-1');
      await app.reconciler.runOnce();
      expect(balance(user)).toBe(500_000);
    });

    it('replaying the same idempotency key does not double-debit', async () => {
      const { rln, user } = await setup({
        decodeLnInvoice: () => decodedInvoice(HASH_A, 100_000),
        sendPayment: () => ({ payment_id: HASH_A, status: 'Succeeded' }),
      });
      fund(user, 500_000);
      const first = await pay(user, 'pay-1');
      expect(first.statusCode).toBe(200);
      const replay = await pay(user, 'pay-1');
      expect(replay.statusCode).toBe(200);
      expect(replay.headers['x-idempotent-replay']).toBe('true');
      expect(replay.json()).toEqual(first.json());
      expect(balance(user)).toBe(400_000);
      expect(rln.calls.filter((c) => c === 'sendPayment')).toHaveLength(1);
    });

    it('refuses to re-pay an invoice that already failed', async () => {
      const { user } = await setup({
        decodeLnInvoice: () => decodedInvoice(HASH_A, 100_000),
        sendPayment: () => {
          throw rlnRejection(400);
        },
      });
      fund(user, 500_000);
      await pay(user, 'pay-1');
      const retry = await pay(user, 'pay-2');
      expect(retry.statusCode).toBe(409);
      expect(retry.json().error.code).toBe('PAYMENT_ALREADY_ATTEMPTED');
      expect(balance(user)).toBe(500_000);
    });

    it('rejects a foreign in-flight invoice with a conflict', async () => {
      const { user } = await setup({
        decodeLnInvoice: () => decodedInvoice(HASH_A, 100_000),
        sendPayment: () => ({ payment_id: HASH_A, status: 'Pending' }),
      });
      const other = await createTestUser(app);
      fund(user, 500_000);
      fund(other, 500_000);
      await pay(user, 'pay-1');
      const conflicting = await pay(other, 'pay-2');
      expect(conflicting.statusCode).toBe(409);
      expect(conflicting.json().error.code).toBe('PAYMENT_CONFLICT');
      expect(balance(other)).toBe(500_000);
    });

    it('refuses to claim a hash another user owns as an inbound invoice', async () => {
      // decodeLnInvoice only verifies the BOLT11 self-signature, so the hash is
      // the caller's to choose. Claiming a hash another user is waiting to be
      // paid on would lock that user's real payer out with a 409.
      const { rln, user } = await setup({
        decodeLnInvoice: () => decodedInvoice(HASH_A, 100_000),
        sendPayment: () => ({ payment_id: HASH_A, status: 'Pending' }),
      });
      const other = await createTestUser(app);
      recordOwnership(app.db, {
        kind: 'invoice',
        resourceId: HASH_A,
        userId: other.userId,
        state: 'pending',
      });
      fund(user, 500_000);
      const response = await pay(user, 'pay-1');
      expect(response.statusCode).toBe(409);
      expect(response.json().error.code).toBe('PAYMENT_CONFLICT');
      expect(balance(user)).toBe(500_000);
      expect(rln.calls).not.toContain('sendPayment');
    });

    it('requires an amount when the invoice carries none', async () => {
      const { user } = await setup({
        decodeLnInvoice: () => decodedInvoice(HASH_A, null),
      });
      fund(user, 500_000);
      const response = await pay(user, 'pay-1');
      expect(response.statusCode).toBe(400);
      expect(response.json().error.code).toBe('AMOUNT_REQUIRED');
    });

    it('pays an asset invoice: debits both the btc and the asset leg', async () => {
      const { user } = await setup({
        decodeLnInvoice: () => decodedInvoice(HASH_A, 3_000, { assetId: ASSET, amount: 50 }),
        sendPayment: () => ({ payment_id: HASH_A, status: 'Pending' }),
      });
      fund(user, 500_000);
      fund(user, 80, ASSET);
      const response = await pay(user, 'pay-1');
      expect(response.statusCode).toBe(200);
      expect(response.json()).toEqual({ paymentHash: HASH_A, status: 'pending' });
      expect(balance(user)).toBe(497_000);
      expect(balance(user, ASSET)).toBe(30);
    });

    it('restores both legs when RLN definitively rejects an asset payment', async () => {
      const { user } = await setup({
        decodeLnInvoice: () => decodedInvoice(HASH_A, 3_000, { assetId: ASSET, amount: 50 }),
        sendPayment: () => {
          throw rlnRejection(400);
        },
      });
      fund(user, 500_000);
      fund(user, 80, ASSET);
      const response = await pay(user, 'pay-1');
      expect(response.statusCode).toBe(400);
      expect(response.json().error.code).toBe('PAYMENT_REJECTED');
      expect(balance(user)).toBe(500_000);
      expect(balance(user, ASSET)).toBe(80);
      const row = app.db
        .prepare("SELECT state FROM resource_map WHERE kind = 'payment_hash' AND resource_id = ?")
        .get(HASH_A) as { state: string };
      expect(row.state).toBe('failed');
    });

    it('requires an asset amount for an asset invoice that carries none', async () => {
      const { rln, user } = await setup({
        decodeLnInvoice: () => decodedInvoice(HASH_A, 3_000, { assetId: ASSET, amount: null }),
      });
      fund(user, 500_000);
      fund(user, 80, ASSET);
      const response = await pay(user, 'pay-1');
      expect(response.statusCode).toBe(400);
      expect(response.json().error.code).toBe('AMOUNT_REQUIRED');
      expect(balance(user)).toBe(500_000);
      expect(balance(user, ASSET)).toBe(80);
      expect(rln.calls).not.toContain('sendPayment');
    });

    it('refunds immediately when sendPayment answers with status Failed', async () => {
      const { user } = await setup({
        decodeLnInvoice: () => decodedInvoice(HASH_A, 100_000),
        sendPayment: () => ({ payment_id: HASH_A, status: 'Failed' }),
      });
      fund(user, 500_000);
      const response = await pay(user, 'pay-1');
      expect(response.statusCode).toBe(200);
      expect(response.json()).toEqual({ paymentHash: HASH_A, status: 'failed' });
      expect(balance(user)).toBe(500_000);
      const row = app.db
        .prepare("SELECT state FROM resource_map WHERE kind = 'payment_hash' AND resource_id = ?")
        .get(HASH_A) as { state: string };
      expect(row.state).toBe('failed');
    });
  });

  describe('invoices', () => {
    const invoiceOverrides: RlnOverrides = {
      lnInvoice: () => ({ invoice: TEST_INVOICE }),
      decodeLnInvoice: () => decodedInvoice(HASH_A, 250_000),
    };

    function createInvoice(user: TestUser, body: Record<string, unknown> = { amtMsat: 250_000 }) {
      return app.inject({
        method: 'POST',
        url: '/v1/ln/invoice',
        headers: { authorization: `Bearer ${user.token}` },
        payload: body,
      });
    }

    it('creates a scoped invoice and hides it from other users', async () => {
      const { user } = await setup(invoiceOverrides);
      const other = await createTestUser(app);
      const created = await createInvoice(user);
      expect(created.statusCode).toBe(201);
      expect(created.json()).toEqual({ invoice: TEST_INVOICE, paymentHash: HASH_A });

      const foreign = await app.inject({
        method: 'GET',
        url: `/v1/ln/invoice/${HASH_A}`,
        headers: { authorization: `Bearer ${other.token}` },
      });
      expect(foreign.statusCode).toBe(404);
    });

    it('reports live status and credits a settled fixed-amount invoice once', async () => {
      const { user } = await setup({
        ...invoiceOverrides,
        invoiceStatus: () => ({ status: 'Succeeded' }),
      });
      await createInvoice(user);
      const read = () =>
        app.inject({
          method: 'GET',
          url: `/v1/ln/invoice/${HASH_A}`,
          headers: { authorization: `Bearer ${user.token}` },
        });
      const first = await read();
      expect(first.statusCode).toBe(200);
      expect(first.json().state).toBe('settled');
      expect(balance(user)).toBe(250_000);
      // A second read must not credit again.
      const second = await read();
      expect(second.json().state).toBe('settled');
      expect(balance(user)).toBe(250_000);
    });

    it('rejects invoice creation beyond the float cap', async () => {
      const { user } = await setup(invoiceOverrides, {
        configOverrides: { floatCapPerUserMsat: 200_000 },
      });
      const response = await createInvoice(user, { amtMsat: 250_000 });
      expect(response.statusCode).toBe(409);
      expect(response.json().error.code).toBe('FLOAT_CAP_EXCEEDED');
    });

    it('requires an amount at creation: btc without amtMsat, asset without assetAmount', async () => {
      const { rln, user } = await setup({});
      const btc = await createInvoice(user, {});
      expect(btc.statusCode).toBe(400);
      expect(btc.json().error.code).toBe('AMOUNT_REQUIRED');
      const asset = await createInvoice(user, { assetId: ASSET });
      expect(asset.statusCode).toBe(400);
      expect(asset.json().error.code).toBe('AMOUNT_REQUIRED');
      expect(rln.calls).toHaveLength(0);
    });

    it('counts pending invoices as exposure: two invoices jointly overshooting the cap', async () => {
      let decodes = 0;
      const { user } = await setup(
        {
          lnInvoice: () => ({ invoice: TEST_INVOICE }),
          decodeLnInvoice: () => decodedInvoice(decodes++ === 0 ? HASH_A : HASH_B, 250_000),
        },
        { configOverrides: { floatCapPerUserMsat: 400_000 } },
      );
      const first = await createInvoice(user, { amtMsat: 250_000 });
      expect(first.statusCode).toBe(201);
      const second = await createInvoice(user, { amtMsat: 250_000 });
      expect(second.statusCode).toBe(409);
      expect(second.json().error.code).toBe('FLOAT_CAP_EXCEEDED');
    });

    it('marks an invoice expired when RLN reports Expired live', async () => {
      const { user } = await setup({
        ...invoiceOverrides,
        invoiceStatus: () => ({ status: 'Expired' }),
      });
      await createInvoice(user);
      const read = await app.inject({
        method: 'GET',
        url: `/v1/ln/invoice/${HASH_A}`,
        headers: { authorization: `Bearer ${user.token}` },
      });
      expect(read.statusCode).toBe(200);
      expect(read.json().state).toBe('expired');
      const row = app.db
        .prepare('SELECT state FROM ln_invoices WHERE payment_hash = ?')
        .get(HASH_A) as { state: string };
      expect(row.state).toBe('expired');
      expect(balance(user)).toBe(0);
    });

    it('carries the configured minimum msat on an asset invoice with no amtMsat', async () => {
      // RLN refuses an asset invoice whose amt_msat is under the channel HTLC
      // floor, so an amount-less asset invoice could never be created.
      let seen: { amt_msat: number | null } | undefined;
      const { user } = await setup({
        lnInvoice: (body) => {
          seen = body as { amt_msat: number | null };
          return { invoice: TEST_INVOICE };
        },
        decodeLnInvoice: () => decodedInvoice(HASH_A, 3_000, { assetId: ASSET, amount: 50 }),
      });
      const created = await createInvoice(user, { assetId: ASSET, assetAmount: 50 });
      expect(created.statusCode).toBe(201);
      expect(seen?.amt_msat).toBe(3_000_000);
      const row = app.db
        .prepare('SELECT amt_msat FROM ln_invoices WHERE payment_hash = ?')
        .get(HASH_A) as { amt_msat: number };
      expect(row.amt_msat).toBe(3_000_000);
    });

    it('rejects an asset invoice whose amtMsat is under the HTLC floor', async () => {
      const { rln, user } = await setup({});
      const created = await createInvoice(user, {
        assetId: ASSET,
        assetAmount: 50,
        amtMsat: 1_000,
      });
      expect(created.statusCode).toBe(400);
      expect(created.json().error.code).toBe('AMOUNT_BELOW_MINIMUM');
      expect(rln.calls).toHaveLength(0);
    });

    it('maps an RLN 4xx on invoice creation to 400, not an opaque 502', async () => {
      const { user } = await setup({
        lnInvoice: () => {
          throw rlnRejection(400);
        },
      });
      const created = await createInvoice(user, { amtMsat: 250_000 });
      expect(created.statusCode).toBe(400);
      expect(created.json().error.code).toBe('INVOICE_REJECTED');
      // Node internals stay inside the gateway (invariant I4).
      expect(JSON.stringify(created.json())).not.toContain('rejected by node');
    });

    it('settles an asset invoice with the live amounts from getPayment', async () => {
      const { user } = await setup({
        lnInvoice: () => ({ invoice: TEST_INVOICE }),
        decodeLnInvoice: () => decodedInvoice(HASH_A, null, { assetId: ASSET, amount: 50 }),
        invoiceStatus: () => ({ status: 'Succeeded' }),
        getPayment: () => ({
          payment: {
            ...inboundPayment(HASH_A, 'Succeeded', 3_000),
            asset_id: ASSET,
            asset_amount: 50,
          },
        }),
      });
      const created = await createInvoice(user, { assetId: ASSET, assetAmount: 50 });
      expect(created.statusCode).toBe(201);
      const read = await app.inject({
        method: 'GET',
        url: `/v1/ln/invoice/${HASH_A}`,
        headers: { authorization: `Bearer ${user.token}` },
      });
      expect(read.statusCode).toBe(200);
      expect(read.json().state).toBe('settled');
      // Both legs are credited from the live payment record (the invoice row
      // itself declared no msat amount).
      expect(balance(user)).toBe(3_000);
      expect(balance(user, ASSET)).toBe(50);
    });

    it('leaves an asset invoice pending (uncredited) when its amount cannot be resolved', async () => {
      // RLN says Succeeded but /getpayment fails, so the RECEIVED asset units
      // are unknown. RGB credits are not bounded by the float caps, so they
      // must NOT be guessed from the declared row — the invoice stays pending
      // for a later pass to resolve.
      const { user } = await setup({
        lnInvoice: () => ({ invoice: TEST_INVOICE }),
        decodeLnInvoice: () => decodedInvoice(HASH_A, null, { assetId: ASSET, amount: 50 }),
        invoiceStatus: () => ({ status: 'Succeeded' }),
        getPayment: () => {
          throw rlnTimeout();
        },
      });
      const created = await createInvoice(user, { assetId: ASSET, assetAmount: 50 });
      expect(created.statusCode).toBe(201);
      const read = await app.inject({
        method: 'GET',
        url: `/v1/ln/invoice/${HASH_A}`,
        headers: { authorization: `Bearer ${user.token}` },
      });
      expect(read.statusCode).toBe(200);
      expect(read.json().state).toBe('pending');
      expect(balance(user)).toBe(0);
      expect(balance(user, ASSET)).toBe(0);
    });

    it('scopes the payments list to the requesting user', async () => {
      const { user } = await setup({
        ...invoiceOverrides,
        listPayments: () => ({
          payments: [
            inboundPayment(HASH_A, 'Succeeded', 250_000),
            outboundPayment('c'.repeat(64), 'Succeeded', 10_000),
          ],
          first_index_offset: 2,
          last_index_offset: 1,
        }),
      });
      const other = await createTestUser(app);
      await createInvoice(user);
      const mine = await app.inject({
        method: 'GET',
        url: '/v1/ln/payments',
        headers: { authorization: `Bearer ${user.token}` },
      });
      expect(mine.statusCode).toBe(200);
      expect(mine.json().payments).toHaveLength(1);
      expect(mine.json().payments[0].paymentHash).toBe(HASH_A);

      const theirs = await app.inject({
        method: 'GET',
        url: '/v1/ln/payments',
        headers: { authorization: `Bearer ${other.token}` },
      });
      expect(theirs.json().payments).toHaveLength(0);
    });

    it("pages past newer node-wide payments to find the user's own", async () => {
      // RLN's /listpayments is node-wide and capped at 100 per page. A single
      // unpaginated call would return only other users' newer payments and
      // report an empty history for this user.
      const filler = (i: number) =>
        outboundPayment(i.toString(16).padStart(64, '0'), 'Succeeded', 1_000);
      const pages: Record<number, ReturnType<typeof outboundPayment>[]> = {
        0: Array.from({ length: 100 }, (_, i) => filler(200 - i)),
        101: [inboundPayment(HASH_A, 'Succeeded', 250_000)],
      };
      const seenOffsets: (number | undefined)[] = [];
      const { user } = await setup({
        ...invoiceOverrides,
        listPayments: (query: { index_offset?: number }) => {
          seenOffsets.push(query.index_offset);
          const payments = pages[query.index_offset ?? 0] ?? [];
          return {
            payments,
            first_index_offset: payments.length > 0 ? 200 : 0,
            last_index_offset: query.index_offset === undefined ? 101 : 1,
          };
        },
      });
      await createInvoice(user);
      const response = await app.inject({
        method: 'GET',
        url: '/v1/ln/payments',
        headers: { authorization: `Bearer ${user.token}` },
      });
      expect(response.statusCode).toBe(200);
      expect(response.json().payments).toHaveLength(1);
      expect(response.json().payments[0].paymentHash).toBe(HASH_A);
      expect(seenOffsets).toEqual([undefined, 101]);
    });

    it('stops at the age floor when an owned payment is unknown to RLN', async () => {
      // pay() records ownership BEFORE /sendpayment, so a send that never
      // reached RLN leaves a resource_map row nothing in /listpayments can ever
      // match. A count- or set-only stop condition would then walk the node's
      // entire history on every call; the age floor bounds it.
      const old = nowSec() - 30 * 86_400;
      const filler = (i: number, createdAt: number) =>
        outboundPayment(i.toString(16).padStart(64, '0'), 'Succeeded', 1_000, createdAt);
      let pagesServed = 0;
      const { rln, user } = await setup({
        ...invoiceOverrides,
        listPayments: () => {
          pagesServed += 1;
          return {
            payments: Array.from({ length: PAGE }, (_, i) => filler(pagesServed * 1000 + i, old)),
            first_index_offset: 200,
            last_index_offset: 200 - pagesServed,
          };
        },
      });
      // An owned resource RLN will never return.
      app.db
        .prepare(
          'INSERT INTO resource_map (kind, resource_id, user_id, state, created_at) VALUES (?, ?, ?, ?, ?)',
        )
        .run('payment_hash', HASH_B, user.userId, 'failed', Date.now());
      const response = await app.inject({
        method: 'GET',
        url: '/v1/ln/payments',
        headers: { authorization: `Bearer ${user.token}` },
      });
      expect(response.statusCode).toBe(200);
      expect(response.json().payments).toEqual([]);
      // One page, not the whole history: every payment on it predates the
      // user's own resource by more than the skew margin.
      expect(pagesServed).toBe(1);
      expect(rln.calls.filter((c) => c === 'listPayments')).toHaveLength(1);
    });

    it('makes no RLN call when the user owns no payments', async () => {
      const { rln, user } = await setup({});
      const response = await app.inject({
        method: 'GET',
        url: '/v1/ln/payments',
        headers: { authorization: `Bearer ${user.token}` },
      });
      expect(response.statusCode).toBe(200);
      expect(response.json().payments).toEqual([]);
      expect(rln.calls).toHaveLength(0);
    });
  });

  describe('auth and idempotency requirements', () => {
    const getRoutes = [`/v1/ln/invoice/${HASH_A}`, '/v1/ln/payments', '/v1/ln/balance'];
    const postRoutes = [
      '/v1/ln/deposit/prepare',
      '/v1/ln/pay',
      '/v1/ln/invoice',
      '/v1/ln/withdraw',
    ];

    it('rejects unauthenticated calls on every route', async () => {
      await setup({});
      for (const url of getRoutes) {
        const response = await app.inject({ method: 'GET', url });
        expect(response.statusCode, url).toBe(401);
      }
      for (const url of postRoutes) {
        const response = await app.inject({ method: 'POST', url, payload: {} });
        expect(response.statusCode, url).toBe(401);
      }
    });

    it('requires an Idempotency-Key on the money-moving routes', async () => {
      const { rln, user } = await setup({});
      const bodies: Record<string, object> = {
        '/v1/ln/pay': { invoice: TEST_INVOICE },
        '/v1/ln/withdraw': { kind: 'btc', address: 'bcrt1qtestaddress0000', amountSat: 5_000 },
      };
      for (const [url, payload] of Object.entries(bodies)) {
        const response = await app.inject({
          method: 'POST',
          url,
          headers: { authorization: `Bearer ${user.token}` },
          payload,
        });
        expect(response.statusCode, url).toBe(400);
        expect(response.json().error.code, url).toBe('IDEMPOTENCY_KEY_REQUIRED');
      }
      // Rejected before any node call: the key guard runs ahead of the handler.
      expect(rln.calls).toHaveLength(0);
    });
  });

  describe('deposits', () => {
    it('prepares a btc deposit and rejects one beyond the per-user cap', async () => {
      const { user } = await setup(
        { address: () => ({ address: 'bcrt1qtestdepositaddr' }) },
        { configOverrides: { floatCapPerUserMsat: 1_000_000 } },
      );
      const ok = await app.inject({
        method: 'POST',
        url: '/v1/ln/deposit/prepare',
        headers: { authorization: `Bearer ${user.token}`, 'idempotency-key': 'dep-1' },
        payload: { kind: 'btc', amountMsat: 600_000 },
      });
      expect(ok.statusCode).toBe(201);
      expect(ok.json().address).toBe('bcrt1qtestdepositaddr');

      // The first pending deposit counts as exposure: this one would overshoot.
      const over = await app.inject({
        method: 'POST',
        url: '/v1/ln/deposit/prepare',
        headers: { authorization: `Bearer ${user.token}`, 'idempotency-key': 'dep-2' },
        payload: { kind: 'btc', amountMsat: 500_000 },
      });
      expect(over.statusCode).toBe(409);
      expect(over.json().error.code).toBe('FLOAT_CAP_EXCEEDED');
    });

    it('rejects a deposit beyond the global cap even under the user cap', async () => {
      const { user } = await setup(
        { address: () => ({ address: 'bcrt1qtestdepositaddr' }) },
        { configOverrides: { floatCapPerUserMsat: 1_000_000, floatCapGlobalMsat: 1_200_000 } },
      );
      const other = await createTestUser(app);
      fund(user, 900_000);
      const response = await app.inject({
        method: 'POST',
        url: '/v1/ln/deposit/prepare',
        headers: { authorization: `Bearer ${other.token}`, 'idempotency-key': 'dep-3' },
        payload: { kind: 'btc', amountMsat: 400_000 },
      });
      expect(response.statusCode).toBe(409);
      expect(response.json().error.code).toBe('FLOAT_CAP_EXCEEDED');
    });

    it('prepares an rgb deposit with an invoice and an expiring pending row', async () => {
      const { user } = await setup({
        rgbInvoice: () => ({
          invoice: 'rgb:inv-1',
          recipient_id: 'utxob:rcpt-1',
          expiration_timestamp: null,
          batch_transfer_idx: 1,
        }),
      });
      const response = await app.inject({
        method: 'POST',
        url: '/v1/ln/deposit/prepare',
        headers: { authorization: `Bearer ${user.token}`, 'idempotency-key': 'dep-4' },
        payload: { kind: 'rgb', assetId: ASSET, amount: 40 },
      });
      expect(response.statusCode).toBe(201);
      const body = response.json();
      expect(body.invoice).toBe('rgb:inv-1');
      expect(body.recipientId).toBe('utxob:rcpt-1');
      const row = app.db
        .prepare('SELECT kind, asset, amount, expires_at FROM pending_deposits WHERE user_id = ?')
        .get(user.userId) as { kind: string; asset: string; amount: number; expires_at: number };
      expect(row.kind).toBe('rgb');
      expect(row.asset).toBe(ASSET);
      expect(row.amount).toBe(40);
      expect(row.expires_at).toBeGreaterThan(Date.now());
    });

    it('rejects an rgb prepare when the node repeats a recipient id already pending', async () => {
      // The watcher attributes RGB by recipient_id with no lower time bound,
      // so two intents on one id would credit the same settled transfer twice.
      const { user } = await setup({
        rgbInvoice: () => ({
          invoice: 'rgb:inv-pinned',
          recipient_id: 'utxob:pinned',
          expiration_timestamp: null,
          batch_transfer_idx: 1,
        }),
      });
      const other = await createTestUser(app);
      const first = await app.inject({
        method: 'POST',
        url: '/v1/ln/deposit/prepare',
        headers: { authorization: `Bearer ${user.token}`, 'idempotency-key': 'dep-rgb-1' },
        payload: { kind: 'rgb', assetId: ASSET, amount: 40 },
      });
      expect(first.statusCode).toBe(201);
      const second = await app.inject({
        method: 'POST',
        url: '/v1/ln/deposit/prepare',
        headers: { authorization: `Bearer ${other.token}`, 'idempotency-key': 'dep-rgb-2' },
        payload: { kind: 'rgb', assetId: ASSET, amount: 40 },
      });
      expect(second.statusCode).toBe(502);
      expect(second.json().error.code).toBe('DEPOSIT_ADDRESS_CONFLICT');
      expect(app.db.prepare('SELECT COUNT(*) AS n FROM pending_deposits').get()).toEqual({ n: 1 });
    });

    it('rejects a btc prepare when the node repeats an address already pending', async () => {
      // RLN pins its address under --reuse-addresses; accepting the repeat
      // would let one on-chain payment credit several deposit intents.
      const { user } = await setup({ address: () => ({ address: 'bcrt1qpinnedaddr' }) });
      const other = await createTestUser(app);
      const first = await app.inject({
        method: 'POST',
        url: '/v1/ln/deposit/prepare',
        headers: { authorization: `Bearer ${user.token}`, 'idempotency-key': 'dep-5' },
        payload: { kind: 'btc', amountMsat: 100_000 },
      });
      expect(first.statusCode).toBe(201);
      const second = await app.inject({
        method: 'POST',
        url: '/v1/ln/deposit/prepare',
        headers: { authorization: `Bearer ${other.token}`, 'idempotency-key': 'dep-6' },
        payload: { kind: 'btc', amountMsat: 100_000 },
      });
      expect(second.statusCode).toBe(502);
      expect(second.json().error.code).toBe('DEPOSIT_ADDRESS_CONFLICT');
      expect(app.db.prepare('SELECT COUNT(*) AS n FROM pending_deposits').get()).toEqual({ n: 1 });
    });

    it('replays a deposit prepare under the same key without minting a second intent', async () => {
      // Each prepare burns a node address and pins float-cap headroom for the
      // deposit TTL, so a retry must return the first intent.
      let addresses = 0;
      const { user } = await setup({
        address: () => ({ address: `bcrt1qaddr${(addresses += 1)}` }),
      });
      const send = () =>
        app.inject({
          method: 'POST',
          url: '/v1/ln/deposit/prepare',
          headers: { authorization: `Bearer ${user.token}`, 'idempotency-key': 'dep-same' },
          payload: { kind: 'btc', amountMsat: 100_000 },
        });
      const first = await send();
      expect(first.statusCode).toBe(201);
      const replay = await send();
      expect(replay.statusCode).toBe(201);
      expect(replay.json()).toEqual(first.json());
      expect(addresses).toBe(1);
      expect(app.db.prepare('SELECT COUNT(*) AS n FROM pending_deposits').get()).toEqual({ n: 1 });
    });

    it('rejects prepares missing their manual-validation fields', async () => {
      const { rln, user } = await setup({});
      const btc = await app.inject({
        method: 'POST',
        url: '/v1/ln/deposit/prepare',
        headers: { authorization: `Bearer ${user.token}`, 'idempotency-key': 'dep-7' },
        payload: { kind: 'btc' },
      });
      expect(btc.statusCode).toBe(400);
      expect(btc.json().error.code).toBe('BAD_REQUEST');
      const rgb = await app.inject({
        method: 'POST',
        url: '/v1/ln/deposit/prepare',
        headers: { authorization: `Bearer ${user.token}`, 'idempotency-key': 'dep-8' },
        payload: { kind: 'rgb' },
      });
      expect(rgb.statusCode).toBe(400);
      expect(rgb.json().error.code).toBe('BAD_REQUEST');
      expect(rln.calls).toHaveLength(0);
    });
  });

  describe('refund namespacing', () => {
    it('refunds a failed pay whose hash collides with an earlier withdrawal id', async () => {
      // Both refs are 64-hex values the caller controls: the withdrawal id is
      // sha256(userId||'\n'||idempotencyKey), and decodeLnInvoice only checks
      // the BOLT11 self-signature so the payment hash is attacker-chosen. A
      // shared 'refund' ledger kind made the second creditOnce a silent no-op,
      // leaving the debit un-refunded with no error and no log line.
      let collidingHash = HASH_A;
      const { user } = await setup({
        sendBtc: () => {
          throw rlnRejection(403);
        },
        decodeLnInvoice: () => decodedInvoice(collidingHash, 100_000),
        sendPayment: () => {
          throw rlnRejection(400);
        },
      });
      collidingHash = createHash('sha256')
        .update(user.userId)
        .update('\n')
        .update('wd-collide')
        .digest('hex');
      fund(user, 500_000);

      const wd = await withdrawFor(user, 'wd-collide', {
        kind: 'btc',
        address: 'bcrt1qvaultaddr000000',
        amountSat: 300,
      });
      expect(wd.statusCode).toBe(400);
      expect(balance(user)).toBe(500_000);

      const paid = await pay(user, 'pay-collide');
      expect(paid.statusCode).toBe(400);
      // The pay debit must be refunded too, not swallowed by the withdrawal's
      // refund row.
      expect(balance(user)).toBe(500_000);
    });
  });

  describe('withdraw', () => {
    function withdraw(user: TestUser, key: string, body: Record<string, unknown>) {
      return app.inject({
        method: 'POST',
        url: '/v1/ln/withdraw',
        headers: { authorization: `Bearer ${user.token}`, 'idempotency-key': key },
        payload: body,
      });
    }
    const btcBody = { kind: 'btc', address: 'bcrt1qvaultaddr000000', amountSat: 300 };

    it('debits and reports the txid on the happy path', async () => {
      const { user } = await setup({ sendBtc: () => ({ txid: 'txid-1' }) });
      fund(user, 500_000);
      const response = await withdraw(user, 'wd-1', btcBody);
      expect(response.statusCode).toBe(201);
      expect(response.json().txid).toBe('txid-1');
      expect(balance(user)).toBe(200_000);

      // Idempotent replay does not debit again.
      const replay = await withdraw(user, 'wd-1', btcBody);
      expect(replay.headers['x-idempotent-replay']).toBe('true');
      expect(balance(user)).toBe(200_000);
    });

    it('refunds when RLN definitively rejects the send', async () => {
      const { user } = await setup({
        sendBtc: () => {
          throw rlnRejection(403);
        },
      });
      fund(user, 500_000);
      const response = await withdraw(user, 'wd-1', btcBody);
      expect(response.statusCode).toBe(400);
      expect(response.json().error.code).toBe('WITHDRAW_REJECTED');
      expect(balance(user)).toBe(500_000);
      const row = app.db.prepare('SELECT state FROM withdrawals').get() as { state: string };
      expect(row.state).toBe('failed');
    });

    it('keeps the debit and marks ambiguous on an RLN 5xx', async () => {
      // rgb-lib broadcasts BEFORE its post-broadcast bookkeeping, and every
      // failure point after the broadcast reaches RLN as a 500 — so a 5xx may
      // mean the transaction is already on the network. Refunding it would
      // hand back money the node actually paid out.
      const { user } = await setup({
        sendBtc: () => {
          throw rlnRejection(500);
        },
      });
      fund(user, 500_000);
      const response = await withdraw(user, 'wd-1', btcBody);
      expect(response.statusCode).toBe(502);
      expect(balance(user)).toBe(200_000);
      const row = app.db.prepare('SELECT state FROM withdrawals').get() as { state: string };
      expect(row.state).toBe('ambiguous');
    });

    it('keeps the debit and marks ambiguous on a timeout', async () => {
      const { user } = await setup({
        sendBtc: () => {
          throw rlnTimeout();
        },
      });
      fund(user, 500_000);
      const response = await withdraw(user, 'wd-1', btcBody);
      expect(response.statusCode).toBe(504);
      expect(balance(user)).toBe(200_000);
      const row = app.db.prepare('SELECT state FROM withdrawals').get() as { state: string };
      expect(row.state).toBe('ambiguous');
    });

    it('never re-executes a same-key retry after an ambiguous outcome', async () => {
      const { rln, user } = await setup({
        sendBtc: () => {
          throw rlnTimeout();
        },
      });
      fund(user, 500_000);
      const first = await withdraw(user, 'wd-1', btcBody);
      expect(first.statusCode).toBe(504);
      // The 504 released the idempotency claim, so the documented same-key
      // retry re-enters the handler — the withdrawals row must stop it from
      // debiting and broadcasting again.
      const retry = await withdraw(user, 'wd-1', btcBody);
      expect(retry.statusCode).toBe(409);
      expect(retry.json().error.code).toBe('WITHDRAWAL_UNRESOLVED');
      expect(balance(user)).toBe(200_000);
      expect(rln.calls.filter((c) => c === 'sendBtc')).toHaveLength(1);
      expect(app.db.prepare('SELECT COUNT(*) AS n FROM withdrawals').get()).toEqual({ n: 1 });
    });

    it('replays the recorded result on a same-key retry after a crash-reclaimed success', async () => {
      const { rln, user } = await setup({ sendBtc: () => ({ txid: 'txid-1' }) });
      fund(user, 500_000);
      const first = await withdraw(user, 'wd-1', btcBody);
      expect(first.statusCode).toBe(201);
      // Simulate a crash after broadcast but before the response was cached:
      // the idempotency claim is gone, so the retry executes the handler.
      app.db.prepare('DELETE FROM idempotency').run();
      const retry = await withdraw(user, 'wd-1', btcBody);
      expect(retry.statusCode).toBe(201);
      expect(retry.json()).toEqual(first.json());
      expect(balance(user)).toBe(200_000);
      expect(rln.calls.filter((c) => c === 'sendBtc')).toHaveLength(1);
    });

    it('never re-sends a same-key retry of a failed (refunded) withdrawal', async () => {
      const { rln, user } = await setup({
        sendBtc: () => {
          throw rlnRejection(403);
        },
      });
      fund(user, 500_000);
      const first = await withdraw(user, 'wd-1', btcBody);
      expect(first.statusCode).toBe(400);
      expect(first.json().error.code).toBe('WITHDRAW_REJECTED');
      expect(balance(user)).toBe(500_000);
      // 400 is not retryable, so the idempotency claim is kept and the retry is
      // answered from the cache: the handler is never re-entered and the send
      // is never repeated.
      const retry = await withdraw(user, 'wd-1', btcBody);
      expect(retry.statusCode).toBe(400);
      expect(retry.headers['x-idempotent-replay']).toBe('true');
      expect(balance(user)).toBe(500_000);
      expect(rln.calls.filter((c) => c === 'sendBtc')).toHaveLength(1);
    });

    it('refuses a fresh-key retry of a failed (refunded) withdrawal that RLN timed out', async () => {
      // The withdrawals row — not the idempotency cache — is the replay guard
      // when the claim IS released (5xx): a same-key retry hits the terminal row.
      const { rln, user } = await setup({
        sendBtc: () => {
          throw new Error('boom');
        },
      });
      fund(user, 500_000);
      const first = await withdraw(user, 'wd-1', btcBody);
      expect(first.statusCode).toBe(502);
      const retry = await withdraw(user, 'wd-1', btcBody);
      expect(retry.statusCode).toBe(409);
      expect(retry.json().error.code).toBe('WITHDRAWAL_UNRESOLVED');
      expect(rln.calls.filter((c) => c === 'sendBtc')).toHaveLength(1);
    });

    it('rejects a withdraw beyond the balance', async () => {
      const { user } = await setup({});
      fund(user, 100_000);
      const response = await withdraw(user, 'wd-1', btcBody);
      expect(response.statusCode).toBe(409);
      expect(response.json().error.code).toBe('INSUFFICIENT_BALANCE');
      expect(balance(user)).toBe(100_000);
    });

    it('sends an rgb withdraw through the allowlisted proxy and debits the asset', async () => {
      let sent: unknown;
      const { user } = await setup({
        sendRgb: (request) => {
          sent = request;
          return { txid: 'txid-rgb-1' };
        },
      });
      fund(user, 100, ASSET);
      const response = await withdraw(user, 'wd-1', {
        kind: 'rgb',
        assetId: ASSET,
        amount: 40,
        recipientId: 'utxob:rcpt-1',
      });
      expect(response.statusCode).toBe(201);
      expect(response.json().txid).toBe('txid-rgb-1');
      expect(sent).toMatchObject({
        recipient_map: {
          [ASSET]: [
            {
              recipient_id: 'utxob:rcpt-1',
              assignment: { type: 'Fungible', value: 40 },
              transport_endpoints: [app.gatewayConfig.rgbProxyUrl],
            },
          ],
        },
      });
      expect(balance(user, ASSET)).toBe(60);
      const row = app.db.prepare('SELECT state FROM withdrawals').get() as { state: string };
      expect(row.state).toBe('sent');
    });

    it('sends witness_data for a wvout recipient', async () => {
      // rgb-lib rejects a witness beneficiary with no witness data outright
      // ("missing witness data for a witness recipient"), so a withdraw to the
      // wvout invoice /v1/wallet/receive hands out must carry the sat value.
      let sent: { recipient_map: Record<string, unknown[]> } | undefined;
      const { user } = await setup({
        sendRgb: (request) => {
          sent = request as typeof sent;
          return { txid: 'txid-rgb-2' };
        },
      });
      fund(user, 100, ASSET);
      fund(user, 5_000_000);
      const response = await withdraw(user, 'wd-1', {
        kind: 'rgb',
        assetId: ASSET,
        amount: 40,
        recipientId: 'bcrt:wvout:rcpt-w1',
        witnessAmountSat: 1000,
      });
      expect(response.statusCode).toBe(201);
      expect(sent?.recipient_map[ASSET]?.[0]).toMatchObject({
        recipient_id: 'bcrt:wvout:rcpt-w1',
        witness_data: { amount_sat: 1000, blinding: null },
      });
      expect(balance(user, ASSET)).toBe(60);
      // The witness output is funded by the NODE's on-chain wallet, so its
      // sats are debited from the user's msat float too.
      expect(balance(user)).toBe(5_000_000 - 1000 * 1000);
    });

    it('refuses a witness withdraw the caller cannot pay for in msat', async () => {
      // Without the msat debit, witnessAmountSat would let a caller move an
      // arbitrary amount of the node's BTC into a script they control for the
      // price of one asset unit.
      const { rln, user } = await setup({});
      fund(user, 100, ASSET);
      fund(user, 1_000_000);
      const response = await withdraw(user, 'wd-1', {
        kind: 'rgb',
        assetId: ASSET,
        amount: 1,
        recipientId: 'bcrt:wvout:rcpt-w1',
        witnessAmountSat: 40_000_000,
      });
      expect(response.statusCode).toBe(409);
      expect(response.json().error.code).toBe('INSUFFICIENT_BALANCE');
      expect(balance(user, ASSET)).toBe(100);
      expect(balance(user)).toBe(1_000_000);
      expect(rln.calls).toHaveLength(0);
    });

    it('refunds both legs of a witness withdraw the node rejects', async () => {
      const { user } = await setup({
        sendRgb: () => {
          throw rlnRejection(403);
        },
      });
      fund(user, 100, ASSET);
      fund(user, 5_000_000);
      const response = await withdraw(user, 'wd-1', {
        kind: 'rgb',
        assetId: ASSET,
        amount: 40,
        recipientId: 'bcrt:wvout:rcpt-w1',
        witnessAmountSat: 1000,
      });
      expect(response.statusCode).toBe(400);
      expect(balance(user, ASSET)).toBe(100);
      expect(balance(user)).toBe(5_000_000);
      const row = app.db.prepare('SELECT state FROM withdrawals').get() as { state: string };
      expect(row.state).toBe('failed');
    });

    it('rejects a witnessAmountSat whose msat value would not be a safe integer', async () => {
      const { rln, user } = await setup({});
      fund(user, 100, ASSET);
      const response = await withdraw(user, 'wd-1', {
        kind: 'rgb',
        assetId: ASSET,
        amount: 40,
        recipientId: 'bcrt:wvout:rcpt-w1',
        witnessAmountSat: 9_007_199_254_741,
      });
      expect(response.statusCode).toBe(400);
      expect(rln.calls).toHaveLength(0);
      expect(balance(user, ASSET)).toBe(100);
    });

    it('rejects a wvout recipient with no witnessAmountSat before any debit', async () => {
      const { rln, user } = await setup({});
      fund(user, 100, ASSET);
      const response = await withdraw(user, 'wd-1', {
        kind: 'rgb',
        assetId: ASSET,
        amount: 40,
        recipientId: 'bcrt:wvout:rcpt-w1',
      });
      expect(response.statusCode).toBe(400);
      expect(response.json().error.code).toBe('WITNESS_AMOUNT_REQUIRED');
      expect(balance(user, ASSET)).toBe(100);
      expect(rln.calls).toHaveLength(0);
    });

    it('rejects witnessAmountSat on a blind recipient before any debit', async () => {
      // rgb-lib's mirror error: "cannot provide witness data for a blinded
      // recipient".
      const { rln, user } = await setup({});
      fund(user, 100, ASSET);
      const response = await withdraw(user, 'wd-1', {
        kind: 'rgb',
        assetId: ASSET,
        amount: 40,
        recipientId: 'bcrt:utxob:rcpt-1',
        witnessAmountSat: 1000,
      });
      expect(response.statusCode).toBe(400);
      expect(balance(user, ASSET)).toBe(100);
      expect(rln.calls).toHaveLength(0);
    });

    it("rejects an rgb withdraw naming the reserved 'btc' asset before any debit", async () => {
      // BTC_ASSET keys the msat float. Left through, this debits that float in
      // msat units for a send rgb-lib can never resolve — and an ambiguous
      // (timeout) outcome leaves the debit standing permanently.
      const { rln, user } = await setup({});
      fund(user, 500_000);
      const response = await withdraw(user, 'wd-1', {
        kind: 'rgb',
        assetId: BTC_ASSET,
        amount: 40,
        recipientId: 'bcrt:utxob:rcpt-1',
      });
      expect(response.statusCode).toBe(400);
      expect(balance(user)).toBe(500_000);
      expect(rln.calls).toHaveLength(0);
      expect(app.db.prepare('SELECT COUNT(*) AS n FROM withdrawals').get()).toEqual({ n: 0 });
    });

    it('rejects a non-allowlisted transport endpoint before any debit', async () => {
      const { rln, user } = await setup({});
      fund(user, 100, ASSET);
      const response = await withdraw(user, 'wd-1', {
        kind: 'rgb',
        assetId: ASSET,
        amount: 40,
        recipientId: 'utxob:rcpt-1',
        transportEndpoints: ['rpc://evil.internal/x'],
      });
      expect(response.statusCode).toBe(400);
      expect(response.json().error.code).toBe('TRANSPORT_ENDPOINT_NOT_ALLOWED');
      expect(balance(user, ASSET)).toBe(100);
      expect(app.db.prepare('SELECT COUNT(*) AS n FROM withdrawals').get()).toEqual({ n: 0 });
      expect(rln.calls).toHaveLength(0);
    });

    it('rejects a btc withdraw missing address and amountSat', async () => {
      const { rln, user } = await setup({});
      fund(user, 500_000);
      const response = await withdraw(user, 'wd-1', { kind: 'btc' });
      expect(response.statusCode).toBe(400);
      expect(response.json().error.code).toBe('BAD_REQUEST');
      expect(balance(user)).toBe(500_000);
      expect(rln.calls).toHaveLength(0);
    });
  });

  it('reports ledger balances', async () => {
    const { user } = await setup({});
    fund(user, 123_000);
    fund(user, 42, 'rgb:someAsset');
    const response = await app.inject({
      method: 'GET',
      url: '/v1/ln/balance',
      headers: { authorization: `Bearer ${user.token}` },
    });
    expect(response.statusCode).toBe(200);
    expect(response.json()).toEqual({ btcMsat: 123_000, assets: { 'rgb:someAsset': 42 } });
  });
});
