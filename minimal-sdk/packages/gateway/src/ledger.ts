/**
 * Capped-float ledger for the custodial LN working balance (Block 3 / Block 1).
 *
 * Append-only `ledger` rows; balances are always derived (SUM of deltas —
 * never stored).
 *
 * Float caps deliberately do NOT live here. They are a policy on how much new
 * custodial exposure the gateway accepts, and the only place that can decide
 * it is where the exposure is created — LnFlows.assertBtcHeadroom (routes/ln.ts),
 * which counts pending deposit intents and unexpired invoices alongside the
 * settled balance. Once funds have actually arrived, a credit must always
 * succeed: refusing it would strand money the node already holds, and refunds
 * must never fail or money would vanish from the books. Keeping a second cap
 * check here would only create two policies that can silently diverge.
 *
 * Exactly-once: rows carrying a `ref` are unique per (user, asset, kind, ref)
 * — enforced by a partial unique index — so replayed workers and retried
 * requests can call creditOnce freely; the second application is a no-op.
 *
 * All methods are synchronous (better-sqlite3); use inTx() to compose ledger
 * mutations atomically with resource_map / other gateway writes.
 */
import type { GatewayDb } from './db.js';

/** Ledger asset name for the BTC msat balance; anything else is an RGB asset id. */
export const BTC_ASSET = 'btc';

/**
 * Refunds are namespaced per originating flow ('refund_ln_out' reverses an
 * 'ln_out' debit, 'refund_withdraw' a 'withdraw' one) rather than sharing one
 * 'refund' kind. Exactly-once dedupes on (user, asset, kind, ref), and the two
 * refs are both 64-hex values a user can choose on both sides — a payment hash
 * (decodeLnInvoice only checks the BOLT11 self-signature) and
 * sha256(userId||'\n'||idempotencyKey). A shared kind would let the second
 * refund collide with the first, and creditOnce would silently no-op it:
 * the debit would stand un-refunded with no error and no log line.
 */
export type LedgerKind =
  'deposit' | 'ln_in' | 'ln_out' | 'refund_ln_out' | 'refund_withdraw' | 'withdraw';

export class FloatCapExceededError extends Error {
  constructor(readonly scope: 'user' | 'global') {
    super(`${scope} float cap exceeded`);
    this.name = 'FloatCapExceededError';
  }
}

export class InsufficientBalanceError extends Error {
  constructor() {
    super('insufficient ledger balance');
    this.name = 'InsufficientBalanceError';
  }
}

export interface LedgerEntryParams {
  userId: string;
  /** BTC_ASSET (msat) or an RGB asset id (units). */
  asset: string;
  /** Always positive; credit/debit decides the sign. */
  amount: number;
  kind: LedgerKind;
  /** Exactly-once key within (user, asset, kind); null refs are not deduplicated. */
  ref?: string;
}

export interface LedgerRow {
  id: number;
  user_id: string;
  asset: string;
  delta_msat_or_units: number;
  kind: string;
  ref: string | null;
  created_at: number;
}

function assertPositiveAmount(amount: number): void {
  if (!Number.isSafeInteger(amount) || amount <= 0) {
    throw new RangeError(`ledger amount must be a positive integer, got ${amount}`);
  }
}

export class Ledger {
  private readonly insert;
  private readonly sumUserAsset;
  private readonly sumAsset;
  private readonly existsRef;

  constructor(private readonly db: GatewayDb) {
    this.insert = db.prepare(
      `INSERT INTO ledger (user_id, asset, delta_msat_or_units, kind, ref, created_at)
       VALUES (?, ?, ?, ?, ?, ?)`,
    );
    this.sumUserAsset = db.prepare(
      'SELECT COALESCE(SUM(delta_msat_or_units), 0) AS total FROM ledger WHERE user_id = ? AND asset = ?',
    );
    this.sumAsset = db.prepare(
      'SELECT COALESCE(SUM(delta_msat_or_units), 0) AS total FROM ledger WHERE asset = ?',
    );
    this.existsRef = db.prepare(
      'SELECT 1 FROM ledger WHERE user_id = ? AND asset = ? AND kind = ? AND ref = ?',
    );
  }

  /** Run fn atomically (nested calls join the outer transaction). */
  inTx<T>(fn: () => T): T {
    return this.db.transaction(fn)();
  }

  balance(userId: string, asset: string): number {
    return (this.sumUserAsset.get(userId, asset) as { total: number }).total;
  }

  globalBalance(asset: string): number {
    return (this.sumAsset.get(asset) as { total: number }).total;
  }

  /** Every non-zero balance of a user, keyed by asset. */
  balances(userId: string): Record<string, number> {
    const rows = this.db
      .prepare(
        `SELECT asset, COALESCE(SUM(delta_msat_or_units), 0) AS total
         FROM ledger WHERE user_id = ? GROUP BY asset HAVING total != 0`,
      )
      .all(userId) as Array<{ asset: string; total: number }>;
    return Object.fromEntries(rows.map((row) => [row.asset, row.total]));
  }

  /** Append a credit. */
  credit(params: LedgerEntryParams, now: number = Date.now()): void {
    assertPositiveAmount(params.amount);
    this.inTx(() => {
      this.insert.run(
        params.userId,
        params.asset,
        params.amount,
        params.kind,
        params.ref ?? null,
        now,
      );
    });
  }

  /**
   * Credit exactly once per (user, asset, kind, ref). Returns true when the
   * credit was applied, false when an identical entry already exists.
   */
  creditOnce(params: LedgerEntryParams, now: number = Date.now()): boolean {
    if (params.ref === undefined) {
      throw new TypeError('creditOnce requires a ref');
    }
    return this.inTx(() => {
      if (this.existsRef.get(params.userId, params.asset, params.kind, params.ref) !== undefined) {
        return false;
      }
      this.credit(params, now);
      return true;
    });
  }

  /** Append a debit; throws InsufficientBalanceError when the balance is short. */
  debit(params: LedgerEntryParams, now: number = Date.now()): void {
    assertPositiveAmount(params.amount);
    this.inTx(() => {
      if (this.balance(params.userId, params.asset) < params.amount) {
        throw new InsufficientBalanceError();
      }
      this.insert.run(
        params.userId,
        params.asset,
        -params.amount,
        params.kind,
        params.ref ?? null,
        now,
      );
    });
  }

  /** All entries for a user, oldest first (audit/tests). */
  entries(userId: string): LedgerRow[] {
    return this.db
      .prepare('SELECT * FROM ledger WHERE user_id = ? ORDER BY id')
      .all(userId) as LedgerRow[];
  }
}
