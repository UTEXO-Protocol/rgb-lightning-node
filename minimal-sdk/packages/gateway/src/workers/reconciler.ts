/**
 * Payments reconciler: drives every non-terminal LN state to its outcome.
 *
 *  - Outbound debited_pending/sent payments are checked individually via
 *    /getpayment: Succeeded settles (debit stands), Failed/Cancelled refunds.
 *    A payment RLN does not know about is refunded only after a grace period
 *    (the send may still be queued behind RLN's global lock) and only when it
 *    never reached the 'sent' state.
 *  - Inbound settlements come from a cursor-based walk of /listpayments:
 *    pages are fetched newest→oldest until the persisted cursor is reached,
 *    settled inbound payments credit their invoice owner exactly once, then
 *    the cursor advances — but only past regions where every gateway invoice
 *    is already resolved, because an inbound payment's RLN index is stamped at
 *    INVOICE CREATION and does not move when it settles. Re-processing a seen
 *    payment is a no-op (ledger ref uniqueness), so crashing before the cursor
 *    persists is safe.
 *  - Pending invoices are checked via /invoicestatus for expiry (and for
 *    settlement of fixed-amount invoices, same idempotent credit path).
 *
 * The reconciler NEVER retries a send — it only settles or refunds recorded
 * state (Block 3 double-pay caution).
 */
import type { GatewayDb } from '../db.js';
import { Ledger } from '../ledger.js';
import { RlnHttpError, type RlnApi } from '../rln/client.js';
import {
  refundOutboundPayment,
  settleInboundInvoice,
  settleInvoiceWithLiveAmounts,
  type LnInvoiceRow,
} from '../routes/ln.js';
import { updateResourceState } from '../rln/scoping.js';
import type { Payment } from '../rln/types.js';

const CURSOR_KEY = 'ln_reconciler_cursor';
const PAGE_SIZE = 100;

export interface ReconcilerOptions {
  db: GatewayDb;
  ledger: Ledger;
  rln: RlnApi;
  /** Seconds before an RLN-unknown debited_pending payment is refunded. */
  outboundGraceSeconds: number;
  log?: { warn(obj: unknown, msg: string): void };
}

interface PendingOutboundRow {
  resource_id: string;
  user_id: string;
  state: string;
  created_at: number;
}

export class Reconciler {
  private readonly db: GatewayDb;
  private readonly ledger: Ledger;
  private readonly rln: RlnApi;
  private readonly graceMs: number;
  private readonly log: ReconcilerOptions['log'];
  private timer: NodeJS.Timeout | undefined;
  private running = false;

  constructor(options: ReconcilerOptions) {
    this.db = options.db;
    this.ledger = options.ledger;
    this.rln = options.rln;
    this.graceMs = options.outboundGraceSeconds * 1000;
    this.log = options.log;
  }

  start(intervalMs: number): void {
    if (this.timer !== undefined) return;
    this.timer = setInterval(() => {
      void this.runOnce();
    }, intervalMs);
    this.timer.unref();
  }

  stop(): void {
    if (this.timer !== undefined) clearInterval(this.timer);
    this.timer = undefined;
  }

  cursor(): number {
    const row = this.db.prepare('SELECT value FROM worker_state WHERE key = ?').get(CURSOR_KEY) as
      { value: string } | undefined;
    return row === undefined ? 0 : Number(row.value);
  }

  private persistCursor(value: number): void {
    this.db
      .prepare(
        'INSERT INTO worker_state (key, value) VALUES (?, ?) ON CONFLICT (key) DO UPDATE SET value = excluded.value',
      )
      .run(CURSOR_KEY, String(value));
  }

  /** One reconciliation pass. Each sub-step fails independently and is logged. */
  async runOnce(): Promise<void> {
    if (this.running) return;
    this.running = true;
    try {
      // Every phase catches its own failure: start() invokes this with `void`,
      // so an escaped rejection would crash the whole process (unhandled
      // rejection) on a transient RLN outage.
      for (const phase of [
        () => this.resolveOutbound(),
        () => this.settleInboundFromListPayments(),
        () => this.expirePendingInvoices(),
      ]) {
        try {
          await phase();
        } catch (error) {
          this.log?.warn({ err: error }, 'reconciler phase failed');
        }
      }
    } finally {
      this.running = false;
    }
  }

  private async resolveOutbound(): Promise<void> {
    const pending = this.db
      .prepare(
        `SELECT resource_id, user_id, state, created_at FROM resource_map
         WHERE kind = 'payment_hash' AND state IN ('debited_pending', 'sent')`,
      )
      .all() as PendingOutboundRow[];
    for (const row of pending) {
      try {
        let payment;
        try {
          payment = (
            await this.rln.getPayment({
              payment_hash: row.resource_id,
              payment_type: 'Outbound',
            })
          ).payment;
        } catch (error) {
          if (
            error instanceof RlnHttpError &&
            // Only an explicit PaymentNotFound proves RLN never learned about
            // this payment. Any other HTTP error (LockedNode, ChangingState,
            // 500s — RLN maps them all to non-2xx too) may hide a payment that
            // is still settling: refunding on those would double-spend the
            // float. Retry on the next pass instead.
            error.body?.name === 'PaymentNotFound' &&
            row.state === 'debited_pending' &&
            Date.now() - row.created_at >= this.graceMs
          ) {
            // RLN never learned about this payment and it is old enough that a
            // queued send would have surfaced by now: the debit is refunded.
            this.log?.warn(
              { paymentHash: row.resource_id, userId: row.user_id },
              'refunding debited_pending payment unknown to RLN after grace period',
            );
            refundOutboundPayment(this.db, this.ledger, row.user_id, row.resource_id);
            continue;
          }
          throw error;
        }
        if (payment.status === 'Succeeded') {
          updateResourceState(this.db, 'payment_hash', row.resource_id, row.user_id, 'succeeded');
        } else if (payment.status === 'Failed' || payment.status === 'Cancelled') {
          refundOutboundPayment(this.db, this.ledger, row.user_id, row.resource_id);
        }
        // Pending/Claimable/Claiming: leave for the next pass.
      } catch (error) {
        this.log?.warn({ err: error, paymentHash: row.resource_id }, 'outbound reconcile failed');
      }
    }
  }

  private async settleInboundFromListPayments(): Promise<void> {
    const storedCursor = this.cursor();
    let pageCursor: number | undefined;
    let newestIndex = storedCursor;
    // Lowest safe watermark for this pass. RLN stamps an inbound payment's
    // index when the invoice is CREATED (add_inbound_payment →
    // stamp_payment_idx) and never restamps it on settlement, so advancing the
    // cursor to the newest index on sight would step over invoices that are
    // still pending and never revisit them. /listpayments does not return a
    // per-payment index, so a page holding an unresolved invoice pins the
    // watermark just below that entire page.
    let watermarkFloor: number | undefined;
    // Walk newest → oldest until the page dips at or below the stored cursor.
    for (;;) {
      const page = await this.rln.listPayments({
        ...(pageCursor !== undefined ? { index_offset: pageCursor } : {}),
        max_payments: PAGE_SIZE,
      });
      if (page.payments.length === 0) break;
      if (pageCursor === undefined) {
        newestIndex = Math.max(newestIndex, page.first_index_offset);
      }
      let pageHasUnresolved = false;
      for (const payment of page.payments) {
        // Per-payment isolation, like the other two phases: one malformed
        // payment must not abort the walk, because the cursor is persisted
        // only after it completes — an escaping throw would freeze the cursor
        // permanently and re-fail on every pass.
        try {
          if (!this.settleInboundPayment(payment)) pageHasUnresolved = true;
        } catch (error) {
          // A settlement that threw is not resolved either: hold the watermark
          // so the next pass sees this page again.
          pageHasUnresolved = true;
          this.log?.warn(
            { err: error, paymentHash: payment.payment_hash },
            'inbound settlement failed',
          );
        }
      }
      if (pageHasUnresolved) {
        watermarkFloor = Math.max(0, page.last_index_offset - 1);
      }
      if (page.last_index_offset <= storedCursor + 1 || page.last_index_offset === 0) break;
      // Guard against a non-progressing upstream cursor (would loop forever).
      if (pageCursor !== undefined && page.last_index_offset >= pageCursor) break;
      pageCursor = page.last_index_offset;
    }
    const target =
      watermarkFloor === undefined ? newestIndex : Math.min(newestIndex, watermarkFloor);
    if (target > storedCursor) this.persistCursor(target);
  }

  /**
   * Credit a settled inbound payment to its invoice owner. Returns whether the
   * payment is RESOLVED from the gateway's side; `false` means a gateway
   * invoice for it is still pending and the cursor must not advance past it.
   */
  private settleInboundPayment(payment: Payment): boolean {
    if (payment.payment_type === 'Outbound') return true;
    const row = this.db
      .prepare("SELECT * FROM ln_invoices WHERE payment_hash = ? AND state = 'pending'")
      .get(payment.payment_hash) as LnInvoiceRow | undefined;
    if (row === undefined) return true;
    if (payment.status !== 'Succeeded') return false;
    if (row.asset_id !== null && (payment.asset_amount ?? null) === null) {
      // RGB credits are not bounded by the float caps, so the units are never
      // guessed from the declared row: leave it for settleInvoiceWithLiveAmounts.
      return false;
    }
    const amtMsat = payment.amt_msat ?? row.amt_msat;
    if (amtMsat === null || amtMsat === undefined) {
      // No later pass can resolve this from /listpayments alone; the invoice
      // stays pending and expirePendingInvoices owns it from here.
      this.log?.warn({ paymentHash: payment.payment_hash }, 'settled invoice has no amount');
      return true;
    }
    settleInboundInvoice(
      this.db,
      this.ledger,
      row,
      amtMsat,
      payment.asset_amount ?? row.asset_amount,
    );
    return true;
  }

  private async expirePendingInvoices(): Promise<void> {
    const pending = this.db
      .prepare("SELECT * FROM ln_invoices WHERE state = 'pending'")
      .all() as LnInvoiceRow[];
    for (const row of pending) {
      try {
        const { status } = await this.rln.invoiceStatus({ invoice: row.invoice });
        if (status === 'Expired') {
          this.ledger.inTx(() => {
            this.db
              .prepare("UPDATE ln_invoices SET state = 'expired' WHERE payment_hash = ?")
              .run(row.payment_hash);
            updateResourceState(this.db, 'invoice', row.payment_hash, row.user_id, 'expired');
          });
        } else if (status === 'Succeeded') {
          await settleInvoiceWithLiveAmounts(this.db, this.ledger, this.rln, row, this.log);
        }
      } catch (error) {
        this.log?.warn(
          { err: error, paymentHash: row.payment_hash },
          'invoice status check failed',
        );
      }
    }
  }
}
