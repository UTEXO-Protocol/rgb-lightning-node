/**
 * Completions worker: finishes the bookkeeping of on-chain operations left
 * ambiguous by a failed `complete`.
 *
 * Why the gateway has to own this. rgb-lib broadcasts before it writes its
 * bookkeeping, so any post-broadcast failure surfaces as a 502
 * COMPLETE_AMBIGUOUS over a transaction that is probably already on the network
 * (`wallets/prepare.ts`). The documented recovery used to be "the client
 * retries `complete`" — but a client is entitled to treat a 5xx as unresolved
 * and never retry, because a retry it cannot prove is safe could, for all it
 * knows, send twice. A correct client that refuses to retry therefore left the
 * row pending forever: the transaction confirms, the wallet's colored-txo and
 * spent state never advance, and `GET /v1/onchain/operations/:opId` reports
 * `pending` + `mayHaveBroadcast` until the TTL turns it into `expired` — which
 * `complete` then refuses with 410. Nothing in the system resolved it.
 *
 * So the side that CAN prove the retry is safe does it. The gateway knows the
 * op, the signed PSBT and the recorded txid, and rgb-lib's re-broadcast of a
 * transaction the indexer already knows succeeds rather than double-spending
 * (`online.rs:79-84`), so replaying the wallet end-call only redoes the write
 * that failed. Each pass makes at most one attempt per op and stops after
 * `maxAttempts`, leaving a genuinely unfinishable op for an operator instead of
 * pinning the wallet queue forever.
 *
 * This does NOT retry anything the client has not already signed and submitted:
 * it only ever replays a PSBT the client itself sent to `complete`, for an op
 * whose txid is already recorded. It never prepares, never re-signs and never
 * broadcasts a transaction the client did not authorize.
 */
import type { GatewayDb } from '../db.js';
import type { OnchainService, PendingOpRow } from '../wallets/prepare.js';

/** Attempts before an op is left to an operator. */
export const DEFAULT_MAX_ATTEMPTS = 5;

export interface CompletionsWorkerOptions {
  db: GatewayDb;
  onchain: OnchainService;
  maxAttempts?: number;
  log?: {
    warn(obj: unknown, msg: string): void;
    info(obj: unknown, msg: string): void;
  };
}

export class CompletionsWorker {
  private readonly db: GatewayDb;
  private readonly onchain: OnchainService;
  private readonly maxAttempts: number;
  private readonly log: CompletionsWorkerOptions['log'];
  private timer: NodeJS.Timeout | undefined;
  private running = false;

  constructor(options: CompletionsWorkerOptions) {
    this.db = options.db;
    this.onchain = options.onchain;
    this.maxAttempts = options.maxAttempts ?? DEFAULT_MAX_ATTEMPTS;
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

  /**
   * Ops awaiting a server-side finish: still pending, a txid already recorded
   * (so rgb-lib reached broadcast), the signed PSBT retained, and attempts left.
   * Expiry is deliberately not a filter — see `OnchainService.resumeAmbiguous`.
   */
  pending(): PendingOpRow[] {
    return this.db
      .prepare(
        `SELECT * FROM pending_ops
         WHERE state = 'pending' AND txid IS NOT NULL AND signed_psbt IS NOT NULL
           AND completion_attempts < ?
         ORDER BY created_at`,
      )
      .all(this.maxAttempts) as PendingOpRow[];
  }

  /**
   * One pass. Per-op isolation: `start` invokes this with `void`, so an escaped
   * rejection would take the process down on a transient wallet failure, and one
   * stuck op must not stop the others from resolving.
   */
  async runOnce(): Promise<void> {
    if (this.running) return;
    this.running = true;
    try {
      for (const row of this.pending()) {
        try {
          const settled = await this.onchain.resumeAmbiguous(row);
          if (settled) {
            this.log?.info(
              { opId: row.id, kind: row.kind, txid: row.txid },
              'ambiguous on-chain operation completed server-side',
            );
          } else {
            this.log?.warn(
              { opId: row.id, kind: row.kind, txid: row.txid },
              'ambiguous on-chain operation could not be completed; left for an operator',
            );
          }
        } catch (error) {
          // The attempt counter was already bumped, so a permanently failing op
          // walks itself out of the queue rather than being retried forever.
          this.log?.warn(
            { err: error, opId: row.id, kind: row.kind, txid: row.txid },
            'ambiguous on-chain completion attempt failed',
          );
        }
      }
    } finally {
      this.running = false;
    }
  }

  /**
   * Ops that ran out of attempts and need a human. Surfaced for the operator
   * runbook in the gateway README; also what a health check should alert on.
   */
  unresolved(): PendingOpRow[] {
    return this.db
      .prepare(
        `SELECT * FROM pending_ops
         WHERE state = 'pending' AND txid IS NOT NULL AND completion_attempts >= ?
         ORDER BY created_at`,
      )
      .all(this.maxAttempts) as PendingOpRow[];
  }
}
