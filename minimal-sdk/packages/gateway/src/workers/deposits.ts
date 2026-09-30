/**
 * Deposits watcher: polls esplora (BTC) and RLN transfers (RGB) for pending
 * deposits and credits the float ledger after N confirmations.
 *
 * Exactly-once under replays and crashes: the credit uses the deposit id as
 * the ledger ref, so a poll that crashes between credit and state update (or
 * runs twice) can never double-credit — the second creditOnce is a no-op and
 * the state update is retried on the next tick.
 *
 * Cap policy: the cap was enforced against the DECLARED amount at prepare
 * time; the confirmed on-chain amount is credited without a cap check —
 * refusing it would strand funds the node already holds. Over-funding an
 * intent therefore credits more than the declared amount and can exceed the
 * float caps (documented in the gateway README's float-cap section).
 *
 * One-shot semantics: a deposit intent credits exactly once — the first poll
 * that sees confirmed value credits the total confirmed at that moment and
 * closes the intent. Later payments to the same address are NOT credited
 * (documented in the gateway README): prepare a fresh deposit per payment.
 * Deposit targets are never reused across intents (prepare rejects a repeat
 * target in ANY state), so re-summing an already-credited payment is
 * impossible.
 *
 * Expiry: intents past expires_at flip to 'expired' at the END of each pass —
 * after one last-chance poll, so boundary-funded deposits are credited — and
 * stop counting against float-cap headroom (the exposure query also filters
 * them out by expires_at, so a stopped worker cannot pin headroom either).
 */
import type { GatewayDb } from '../db.js';
import { Ledger } from '../ledger.js';
import type { RlnApi } from '../rln/client.js';
import type { Transfer } from '../rln/types.js';

/** Page size for the node-wide /listtransfers walk. */
const TRANSFERS_PAGE_SIZE = 100;
/** Default deadline for one esplora call. */
const ESPLORA_TIMEOUT_MS = 15_000;

interface PendingDepositRow {
  id: string;
  user_id: string;
  kind: 'btc' | 'rgb';
  asset: string;
  amount: number;
  target: string;
  state: string;
}

/** Esplora /address/:addr/txs item subset the watcher reads. */
interface EsploraTx {
  txid: string;
  status: { confirmed: boolean; block_height?: number };
  vout: Array<{ scriptpubkey_address?: string; value: number }>;
}

export interface DepositsWorkerOptions {
  db: GatewayDb;
  ledger: Ledger;
  rln: RlnApi;
  esploraUrl: string;
  minConfirmations: number;
  /** Per-esplora-call timeout; defaults to ESPLORA_TIMEOUT_MS. */
  esploraTimeoutMs?: number;
  fetchImpl?: typeof fetch;
  log?: { warn(obj: unknown, msg: string): void };
}

export class DepositsWorker {
  private readonly db: GatewayDb;
  private readonly ledger: Ledger;
  private readonly rln: RlnApi;
  private readonly esploraUrl: string;
  private readonly minConfirmations: number;
  private readonly esploraTimeoutMs: number;
  private readonly fetchImpl: typeof fetch;
  private readonly log: DepositsWorkerOptions['log'];
  private timer: NodeJS.Timeout | undefined;
  private running = false;

  constructor(options: DepositsWorkerOptions) {
    this.db = options.db;
    this.ledger = options.ledger;
    this.rln = options.rln;
    this.esploraUrl = options.esploraUrl.replace(/\/+$/, '');
    this.minConfirmations = options.minConfirmations;
    this.esploraTimeoutMs = options.esploraTimeoutMs ?? ESPLORA_TIMEOUT_MS;
    this.fetchImpl = options.fetchImpl ?? fetch;
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
   * One poll pass over all pending deposits. Errors are per-deposit and
   * logged; the pass itself is also caught — start() invokes this with
   * `void`, so an escaped rejection (db closed during shutdown, transient
   * SQLite error) would otherwise crash the whole process.
   */
  async runOnce(): Promise<void> {
    if (this.running) return;
    this.running = true;
    try {
      const now = Date.now();
      const pending = this.db
        .prepare("SELECT * FROM pending_deposits WHERE state = 'pending'")
        .all() as PendingDepositRow[];
      let refreshed = false;
      for (const deposit of pending) {
        try {
          if (deposit.kind === 'btc') {
            await this.pollBtcDeposit(deposit);
          } else {
            if (!refreshed) {
              await this.rln.refreshTransfers({ filter: [], skip_sync: false });
              refreshed = true;
            }
            await this.pollRgbDeposit(deposit);
          }
        } catch (error) {
          this.log?.warn({ err: error, depositId: deposit.id }, 'deposit poll failed');
        }
      }
      // Expire AFTER polling: a past-due intent gets one last-chance poll, so
      // a deposit whose funding confirmed near the TTL boundary is credited
      // rather than expired. Only still-unfunded intents flip to 'expired'.
      this.db
        .prepare(
          "UPDATE pending_deposits SET state = 'expired' WHERE state = 'pending' AND expires_at <= ?",
        )
        .run(now);
    } catch (error) {
      this.log?.warn({ err: error }, 'deposits pass failed');
    } finally {
      this.running = false;
    }
  }

  /**
   * Bounded esplora GET. Without an explicit deadline a stalled indexer holds
   * the pass open for undici's 300s default — per pending deposit — and the
   * `running` guard turns that into a worker that simply stops making
   * progress. The timer stays armed until the BODY is read, like RlnClient.
   */
  private async esploraJson<T>(path: string): Promise<T> {
    const controller = new AbortController();
    const timer = setTimeout(() => controller.abort(), this.esploraTimeoutMs);
    try {
      const response = await this.fetchImpl(`${this.esploraUrl}${path}`, {
        signal: controller.signal,
      });
      if (!response.ok) {
        throw new Error(`esplora ${path} failed with status ${response.status}`);
      }
      return (await response.json()) as T;
    } catch (error) {
      if (controller.signal.aborted) {
        throw new Error(`esplora ${path} timed out after ${this.esploraTimeoutMs}ms`);
      }
      throw error;
    } finally {
      clearTimeout(timer);
    }
  }

  private async pollBtcDeposit(deposit: PendingDepositRow): Promise<void> {
    const tipHeight = await this.esploraJson<number>('/blocks/tip/height');
    const txs = await this.esploraJson<EsploraTx[]>(`/address/${deposit.target}/txs`);
    let totalSat = 0;
    let fundingTxid: string | undefined;
    for (const tx of txs) {
      if (!tx.status.confirmed || tx.status.block_height === undefined) continue;
      if (tipHeight - tx.status.block_height + 1 < this.minConfirmations) continue;
      const paid = tx.vout
        .filter((out) => out.scriptpubkey_address === deposit.target)
        .reduce((sum, out) => sum + out.value, 0);
      if (paid > 0) {
        totalSat += paid;
        fundingTxid ??= tx.txid;
      }
    }
    if (totalSat === 0) return;
    this.credit(deposit, totalSat * 1000, fundingTxid ?? null);
  }

  /**
   * RLN's /listtransfers is node-wide and paginated (newest first, 100 per
   * page by default), so a single unpaginated call would stop finding this
   * deposit's transfer once 100 newer Settled transfers exist for the same
   * asset — the deposit would then never be credited. Walk pages
   * newest→oldest until the recipient id is found or the list is exhausted.
   */
  private async findSettledTransfer(deposit: PendingDepositRow): Promise<Transfer | undefined> {
    let pageCursor: number | undefined;
    for (;;) {
      const page = await this.rln.listTransfers({
        asset_filter: { type: 'Id', value: deposit.asset },
        status: 'Settled',
        max_transfers: TRANSFERS_PAGE_SIZE,
        ...(pageCursor !== undefined ? { index_offset: pageCursor } : {}),
      });
      // The status filter is applied node-side too; re-checking here means a
      // node that ignores it can never make us credit an unsettled transfer.
      const match = page.transfers.find(
        (transfer) => transfer.recipient_id === deposit.target && transfer.status === 'Settled',
      );
      if (match !== undefined) return match;
      // Short page, no cursor to advance to, or a non-progressing upstream
      // cursor (would loop forever): the walk is done.
      if (page.transfers.length < TRANSFERS_PAGE_SIZE || page.last_index_offset === 0) return;
      if (pageCursor !== undefined && page.last_index_offset >= pageCursor) return;
      pageCursor = page.last_index_offset;
    }
  }

  private async pollRgbDeposit(deposit: PendingDepositRow): Promise<void> {
    const settled = await this.findSettledTransfer(deposit);
    if (settled === undefined) return;
    const fungible = settled.assignments.find(
      (assignment): assignment is { type: 'Fungible'; value: number } =>
        assignment.type === 'Fungible',
    );
    if (fungible === undefined) {
      // No fungible assignment means we cannot tell what was actually
      // received. Crediting the amount the USER declared at prepare time
      // would mint units on the ledger that the node may not hold (RGB
      // credits are exempt from the float caps), so leave the intent pending
      // and surface it instead of guessing.
      this.log?.warn(
        { depositId: deposit.id, recipientId: deposit.target },
        'settled RGB transfer carries no fungible assignment; deposit not credited',
      );
      return;
    }
    this.credit(deposit, fungible.value, settled.txid ?? null);
  }

  private credit(deposit: PendingDepositRow, amount: number, txid: string | null): void {
    this.ledger.inTx(() => {
      this.ledger.creditOnce({
        userId: deposit.user_id,
        asset: deposit.asset,
        amount,
        kind: 'deposit',
        ref: deposit.id,
      });
      this.db
        .prepare(
          "UPDATE pending_deposits SET state = 'credited', credited_amount = ?, txid = ? WHERE id = ? AND state = 'pending'",
        )
        .run(amount, txid, deposit.id);
    });
  }
}
