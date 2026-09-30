import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { openDb, type GatewayDb } from '../src/db.js';
import { BTC_ASSET, Ledger } from '../src/ledger.js';
import { recordOwnership } from '../src/rln/scoping.js';
import { Reconciler } from '../src/workers/reconciler.js';
import {
  fakeRln,
  HASH_A,
  HASH_B,
  inboundPayment,
  rlnNetworkFailure,
  rlnPaymentNotFound,
  rlnRejection,
  TEST_INVOICE,
  type RlnOverrides,
} from './ln-mocks.js';

const USER = 'user-a';
const ASSET = 'rgb:testAsset-000';

describe('Reconciler', () => {
  let db: GatewayDb;
  let ledger: Ledger;

  beforeEach(() => {
    db = openDb(':memory:');
    db.prepare('INSERT INTO users (id, token_hash, created_at) VALUES (?, ?, ?)').run(
      USER,
      'hash-a',
      Date.now(),
    );
    ledger = new Ledger(db);
  });

  afterEach(() => {
    db.close();
  });

  function reconciler(overrides: RlnOverrides, graceSeconds = 600): Reconciler {
    return new Reconciler({
      db,
      ledger,
      rln: fakeRln(overrides),
      outboundGraceSeconds: graceSeconds,
    });
  }

  function pendingInvoice(
    hash: string,
    amtMsat: number | null,
    asset?: { assetId: string; assetAmount: number },
  ): void {
    recordOwnership(db, { kind: 'invoice', resourceId: hash, userId: USER, state: 'pending' });
    const now = Date.now();
    db.prepare(
      `INSERT INTO ln_invoices (payment_hash, user_id, invoice, amt_msat, asset_id, asset_amount, state, created_at, expires_at)
       VALUES (?, ?, ?, ?, ?, ?, 'pending', ?, ?)`,
    ).run(
      hash,
      USER,
      TEST_INVOICE,
      amtMsat,
      asset?.assetId ?? null,
      asset?.assetAmount ?? null,
      now,
      now + 3_600_000,
    );
  }

  function invoiceState(hash: string): string {
    const row = db.prepare('SELECT state FROM ln_invoices WHERE payment_hash = ?').get(hash) as {
      state: string;
    };
    return row.state;
  }

  const emptyPage = { payments: [], first_index_offset: 0, last_index_offset: 0 };

  it('credits a settled inbound invoice exactly once across replayed runs', async () => {
    pendingInvoice(HASH_A, 250_000);
    let listCalls = 0;
    const worker = reconciler({
      listPayments: () => {
        listCalls += 1;
        return {
          payments: [inboundPayment(HASH_A, 'Succeeded', 250_000)],
          first_index_offset: 1,
          last_index_offset: 1,
        };
      },
      invoiceStatus: () => ({ status: 'Pending' }),
    });
    await worker.runOnce();
    expect(ledger.balance(USER, BTC_ASSET)).toBe(250_000);
    expect(invoiceState(HASH_A)).toBe('settled');

    // Replays — including one that re-reads the same page — are no-ops.
    await worker.runOnce();
    await worker.runOnce();
    expect(ledger.balance(USER, BTC_ASSET)).toBe(250_000);
    expect(listCalls).toBe(3);
  });

  it('persists the cursor and survives a restart without re-crediting', async () => {
    pendingInvoice(HASH_A, 250_000);
    const overrides: RlnOverrides = {
      // Like real RLN: paging past the oldest payment yields an empty page.
      listPayments: (query) =>
        query?.index_offset !== undefined
          ? emptyPage
          : {
              payments: [inboundPayment(HASH_A, 'Succeeded', 250_000)],
              first_index_offset: 7,
              last_index_offset: 7,
            },
      invoiceStatus: () => ({ status: 'Pending' }),
    };
    const first = reconciler(overrides);
    await first.runOnce();
    expect(first.cursor()).toBe(7);

    // A fresh instance over the same database (simulated restart).
    const second = reconciler(overrides);
    expect(second.cursor()).toBe(7);
    await second.runOnce();
    expect(ledger.balance(USER, BTC_ASSET)).toBe(250_000);
  });

  it('walks multiple pages down to the stored cursor', async () => {
    pendingInvoice(HASH_A, 100_000);
    pendingInvoice(HASH_B, 200_000);
    const pages: Record<string, unknown> = {
      newest: {
        payments: [inboundPayment(HASH_B, 'Succeeded', 200_000)],
        first_index_offset: 4,
        last_index_offset: 3,
      },
      older: {
        payments: [inboundPayment(HASH_A, 'Succeeded', 100_000)],
        first_index_offset: 2,
        last_index_offset: 1,
      },
    };
    const worker = reconciler({
      listPayments: (query) =>
        query?.index_offset === undefined ? pages['newest'] : pages['older'],
      invoiceStatus: () => ({ status: 'Pending' }),
    });
    await worker.runOnce();
    expect(ledger.balance(USER, BTC_ASSET)).toBe(300_000);
    expect(worker.cursor()).toBe(4);
  });

  it('settles an outbound payment RLN reports as succeeded', async () => {
    ledger.credit({ userId: USER, asset: BTC_ASSET, amount: 500_000, kind: 'deposit', ref: 'd1' });
    ledger.debit({ userId: USER, asset: BTC_ASSET, amount: 100_000, kind: 'ln_out', ref: HASH_A });
    recordOwnership(db, {
      kind: 'payment_hash',
      resourceId: HASH_A,
      userId: USER,
      state: 'debited_pending',
    });
    const worker = reconciler({
      getPayment: () => ({
        payment: { ...inboundPayment(HASH_A, 'Succeeded', 100_000), payment_type: 'Outbound' },
      }),
      listPayments: () => emptyPage,
    });
    await worker.runOnce();
    expect(ledger.balance(USER, BTC_ASSET)).toBe(400_000);
    const row = db
      .prepare("SELECT state FROM resource_map WHERE kind = 'payment_hash' AND resource_id = ?")
      .get(HASH_A) as { state: string };
    expect(row.state).toBe('succeeded');
  });

  function debitedPayment(state = 'debited_pending'): void {
    ledger.credit({ userId: USER, asset: BTC_ASSET, amount: 500_000, kind: 'deposit', ref: 'd1' });
    ledger.debit({ userId: USER, asset: BTC_ASSET, amount: 100_000, kind: 'ln_out', ref: HASH_A });
    recordOwnership(db, { kind: 'payment_hash', resourceId: HASH_A, userId: USER, state });
  }

  function paymentState(hash: string): string {
    const row = db
      .prepare("SELECT state FROM resource_map WHERE kind = 'payment_hash' AND resource_id = ?")
      .get(hash) as { state: string };
    return row.state;
  }

  it('refunds an RLN-unknown debited-pending payment only after the grace period', async () => {
    debitedPayment();
    const overrides: RlnOverrides = {
      getPayment: () => {
        throw rlnPaymentNotFound();
      },
      listPayments: () => emptyPage,
    };

    // Inside the grace period: nothing changes.
    const patient = reconciler(overrides, 600);
    await patient.runOnce();
    expect(ledger.balance(USER, BTC_ASSET)).toBe(400_000);

    // Past the grace period: the debit is refunded, exactly once.
    const expired = reconciler(overrides, 0);
    await expired.runOnce();
    expect(ledger.balance(USER, BTC_ASSET)).toBe(500_000);
    await expired.runOnce();
    expect(ledger.balance(USER, BTC_ASSET)).toBe(500_000);
  });

  it('never refunds on an RLN error that is not PaymentNotFound (locked node)', async () => {
    debitedPayment();
    // A locked/erroring node answers 403 without the PaymentNotFound name —
    // the payment may well be settling; refunding would double-spend the float.
    const worker = reconciler(
      {
        getPayment: () => {
          throw rlnRejection(403);
        },
        listPayments: () => emptyPage,
      },
      0,
    );
    await worker.runOnce();
    expect(ledger.balance(USER, BTC_ASSET)).toBe(400_000);
    expect(paymentState(HASH_A)).toBe('debited_pending');
  });

  it('never refunds a payment that reached the sent state on PaymentNotFound', async () => {
    debitedPayment('sent');
    const worker = reconciler(
      {
        getPayment: () => {
          throw rlnPaymentNotFound();
        },
        listPayments: () => emptyPage,
      },
      0,
    );
    await worker.runOnce();
    expect(ledger.balance(USER, BTC_ASSET)).toBe(400_000);
    expect(paymentState(HASH_A)).toBe('sent');
  });

  it('refunds an outbound payment RLN reports as Cancelled', async () => {
    debitedPayment('sent');
    const worker = reconciler({
      getPayment: () => ({
        payment: { ...inboundPayment(HASH_A, 'Cancelled', 100_000), payment_type: 'Outbound' },
      }),
      listPayments: () => emptyPage,
    });
    await worker.runOnce();
    expect(ledger.balance(USER, BTC_ASSET)).toBe(500_000);
    expect(paymentState(HASH_A)).toBe('failed');
  });

  it('survives a listPayments outage without an unhandled rejection', async () => {
    pendingInvoice(HASH_A, 250_000);
    const worker = reconciler({
      listPayments: () => {
        throw rlnNetworkFailure();
      },
      invoiceStatus: () => ({ status: 'Expired' }),
    });
    // Must not reject — start() invokes runOnce with `void`, so a rejection
    // here would crash the process. Later phases still run.
    await expect(worker.runOnce()).resolves.toBeUndefined();
    expect(invoiceState(HASH_A)).toBe('expired');
  });

  it('marks an expired invoice expired', async () => {
    pendingInvoice(HASH_A, 250_000);
    const worker = reconciler({
      listPayments: () => emptyPage,
      invoiceStatus: () => ({ status: 'Expired' }),
    });
    await worker.runOnce();
    expect(invoiceState(HASH_A)).toBe('expired');
    expect(ledger.balance(USER, BTC_ASSET)).toBe(0);
  });

  it('settles an asset invoice in the background from live /getpayment amounts', async () => {
    // Asset invoices declare no amt_msat, so the /listpayments walk cannot
    // settle them ("settled invoice has no amount"). Without this branch they
    // stay pending — and uncredited — unless the user happens to GET them.
    pendingInvoice(HASH_A, null, { assetId: ASSET, assetAmount: 50 });
    const worker = reconciler({
      listPayments: () => emptyPage,
      invoiceStatus: () => ({ status: 'Succeeded' }),
      getPayment: () => ({
        payment: {
          ...inboundPayment(HASH_A, 'Succeeded', 3_000),
          asset_id: ASSET,
          asset_amount: 50,
        },
      }),
    });
    await worker.runOnce();
    expect(invoiceState(HASH_A)).toBe('settled');
    expect(ledger.balance(USER, BTC_ASSET)).toBe(3_000);
    expect(ledger.balance(USER, ASSET)).toBe(50);

    // Replays are no-ops (ledger ref uniqueness).
    await worker.runOnce();
    expect(ledger.balance(USER, ASSET)).toBe(50);
  });

  it('does not advance the cursor past a still-pending invoice, and settles it later', async () => {
    // RLN stamps an inbound payment's index when the INVOICE is created and
    // never restamps it on settlement, so a cursor that jumped to the newest
    // index would step over this invoice and never look at it again.
    pendingInvoice(HASH_A, 250_000);
    let status: 'Pending' | 'Succeeded' = 'Pending';
    const worker = reconciler({
      listPayments: () => ({
        payments: [inboundPayment(HASH_A, status, 250_000)],
        first_index_offset: 7,
        last_index_offset: 7,
      }),
      invoiceStatus: () => ({ status: 'Pending' }),
    });
    await worker.runOnce();
    expect(invoiceState(HASH_A)).toBe('pending');
    // Pinned just below the page holding the unresolved invoice, not at 7.
    expect(worker.cursor()).toBe(6);

    // Same index, now settled: the walk still covers it.
    status = 'Succeeded';
    await worker.runOnce();
    expect(invoiceState(HASH_A)).toBe('settled');
    expect(ledger.balance(USER, BTC_ASSET)).toBe(250_000);
    // Everything in view is resolved, so the watermark is free to advance.
    expect(worker.cursor()).toBe(7);
  });

  it('never credits asset units the payment record does not confirm', async () => {
    // RGB credits are not bounded by the float caps: a declared-but-unconfirmed
    // amount would mint ledger units.
    pendingInvoice(HASH_A, 3_000_000, { assetId: ASSET, assetAmount: 50 });
    const worker = reconciler({
      listPayments: () => ({
        // Settled, but /listpayments could not read the RGB payment info.
        payments: [inboundPayment(HASH_A, 'Succeeded', 3_000_000)],
        first_index_offset: 4,
        last_index_offset: 4,
      }),
      invoiceStatus: () => ({ status: 'Pending' }),
    });
    await worker.runOnce();
    expect(invoiceState(HASH_A)).toBe('pending');
    expect(ledger.balance(USER, ASSET)).toBe(0);
    expect(ledger.balance(USER, BTC_ASSET)).toBe(0);
    expect(worker.cursor()).toBe(3);
  });

  it('keeps walking /listpayments when one payment fails to settle', async () => {
    // The cursor is persisted only after the walk finishes, so an escaping
    // throw would freeze it permanently and re-fail on every pass. The page
    // still holds an unresolved invoice, so the watermark must NOT advance
    // past it either — inbound indexes are stamped at invoice creation and
    // never move, so a cursor that ran ahead would never look here again.
    pendingInvoice(HASH_A, 0);
    pendingInvoice(HASH_B, 250_000);
    const warnings: unknown[] = [];
    const worker = new Reconciler({
      db,
      ledger,
      rln: fakeRln({
        listPayments: () => ({
          // A zero-amount settled payment makes the ledger credit throw
          // (RangeError from assertPositiveAmount) mid-walk.
          payments: [
            inboundPayment(HASH_A, 'Succeeded', 0),
            inboundPayment(HASH_B, 'Succeeded', 250_000),
          ],
          first_index_offset: 2,
          last_index_offset: 1,
        }),
        invoiceStatus: () => ({ status: 'Pending' }),
      }),
      outboundGraceSeconds: 600,
      log: { warn: (obj) => warnings.push(obj) },
    });
    await worker.runOnce();
    expect(invoiceState(HASH_A)).toBe('pending');
    expect(invoiceState(HASH_B)).toBe('settled');
    expect(ledger.balance(USER, BTC_ASSET)).toBe(250_000);
    expect(worker.cursor()).toBe(0);
    expect(warnings).toHaveLength(1);
  });

  it('isolates per-row failures: an erroring payment does not block the next one', async () => {
    ledger.credit({ userId: USER, asset: BTC_ASSET, amount: 500_000, kind: 'deposit', ref: 'd1' });
    ledger.debit({ userId: USER, asset: BTC_ASSET, amount: 100_000, kind: 'ln_out', ref: HASH_A });
    ledger.debit({ userId: USER, asset: BTC_ASSET, amount: 100_000, kind: 'ln_out', ref: HASH_B });
    for (const hash of [HASH_A, HASH_B]) {
      recordOwnership(db, {
        kind: 'payment_hash',
        resourceId: hash,
        userId: USER,
        state: 'debited_pending',
      });
    }
    const worker = reconciler({
      getPayment: (query) => {
        if (query.payment_hash === HASH_A) throw rlnNetworkFailure();
        return {
          payment: { ...inboundPayment(HASH_B, 'Succeeded', 100_000), payment_type: 'Outbound' },
        };
      },
      listPayments: () => emptyPage,
    });
    await worker.runOnce();
    expect(paymentState(HASH_A)).toBe('debited_pending');
    expect(paymentState(HASH_B)).toBe('succeeded');
    // Both debits stand: the failed row waits for the next pass.
    expect(ledger.balance(USER, BTC_ASSET)).toBe(300_000);
  });

  it('breaks out of a non-progressing listPayments cursor', { timeout: 5_000 }, async () => {
    let listCalls = 0;
    const worker = reconciler({
      // Upstream that never progresses: every page echoes the requested
      // index_offset back as last_index_offset.
      listPayments: (query) => {
        listCalls += 1;
        return {
          payments: [inboundPayment(HASH_A, 'Pending', 100_000)],
          first_index_offset: 10,
          last_index_offset: query?.index_offset ?? 5,
        };
      },
    });
    await expect(worker.runOnce()).resolves.toBeUndefined();
    expect(listCalls).toBe(2);
  });
});
