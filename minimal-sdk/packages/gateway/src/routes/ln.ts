/**
 * LN float flows: deposits into the custodial working balance, payments out,
 * invoices in, withdrawals back to the user vault.
 *
 * State machines (persisted, crash-safe):
 *  - outbound payment (resource_map kind 'payment_hash'):
 *      debited_pending → sent → succeeded | failed(refunded)
 *    The debit happens BEFORE /sendpayment in one SQLite transaction with the
 *    ownership record. A definite RLN rejection refunds immediately; an
 *    ambiguous outcome (timeout/network) leaves debited_pending for the
 *    reconciler — the gateway NEVER auto-retries a send (Block 3 double-pay
 *    caution). A replayed request finds the existing state and does not send
 *    again; a hash in terminal 'failed' state answers 409 — get a fresh
 *    invoice rather than silently re-paying an old one.
 *  - inbound invoice (resource_map kind 'invoice' + ln_invoices):
 *      pending → settled(credited exactly-once) | expired
 *
 * Float caps are enforced where NEW custodial exposure is created (deposit
 * prepare, invoice creation) counting pending intents; settlement credits and
 * refunds are never blocked (the node already holds those funds).
 *
 * Fee policy: LN routing fees and on-chain withdrawal fees are paid by the
 * node and NOT debited from the user's float balance — the operator subsidizes
 * them (documented in the gateway README; caps bound the total exposure).
 */
import { createHash, randomUUID } from 'node:crypto';
import type { FastifyInstance } from 'fastify';
import type { GatewayDb } from '../db.js';
import { HttpError } from '../errors.js';
import { BTC_ASSET, FloatCapExceededError, type Ledger } from '../ledger.js';
import { RlnHttpError, sanitizeRlnError, type RlnApi } from '../rln/client.js';
import { ownerOf, recordOwnership, scopePayments, updateResourceState } from '../rln/scoping.js';
import type { GatewayConfig } from '../config.js';
import type { HtlcStatus, Payment } from '../rln/types.js';
import {
  depositPrepareRouteSchema,
  invoiceCreateRouteSchema,
  invoiceGetRouteSchema,
  lnBalanceRouteSchema,
  paymentsListRouteSchema,
  payRouteSchema,
  withdrawRouteSchema,
} from '../schemas/ln.js';

export type GatewayPaymentStatus = 'pending' | 'succeeded' | 'failed';

export function mapHtlcStatus(status: HtlcStatus): GatewayPaymentStatus {
  switch (status) {
    case 'Succeeded':
      return 'succeeded';
    case 'Cancelled':
    case 'Failed':
      return 'failed';
    default:
      return 'pending';
  }
}

export interface LnInvoiceRow {
  payment_hash: string;
  user_id: string;
  invoice: string;
  amt_msat: number | null;
  asset_id: string | null;
  asset_amount: number | null;
  state: 'pending' | 'settled' | 'expired';
  created_at: number;
  /** BOLT11 expiry (ms). A pending invoice past it stops pinning cap headroom. */
  expires_at: number;
}

/**
 * Credit a settled inbound invoice exactly once and mark it settled. Safe to
 * call from every observer (reconciler, live status reads, replays): the
 * ledger's (user, asset, kind, ref) uniqueness makes the second call a no-op.
 */
export function settleInboundInvoice(
  db: GatewayDb,
  ledger: Ledger,
  row: Pick<LnInvoiceRow, 'payment_hash' | 'user_id' | 'asset_id'>,
  amtMsat: number,
  assetAmount: number | null,
): void {
  ledger.inTx(() => {
    ledger.creditOnce({
      userId: row.user_id,
      asset: BTC_ASSET,
      amount: amtMsat,
      kind: 'ln_in',
      ref: row.payment_hash,
    });
    if (row.asset_id !== null && assetAmount !== null && assetAmount > 0) {
      ledger.creditOnce({
        userId: row.user_id,
        asset: row.asset_id,
        amount: assetAmount,
        kind: 'ln_in',
        ref: row.payment_hash,
      });
    }
    db.prepare("UPDATE ln_invoices SET state = 'settled' WHERE payment_hash = ?").run(
      row.payment_hash,
    );
    updateResourceState(db, 'invoice', row.payment_hash, row.user_id, 'settled');
  });
}

/**
 * Settle a pending invoice using the ACTUAL received amounts from RLN's
 * payment record (falls back to the declared row amounts when the lookup
 * fails), so the user is credited what was really paid rather than what was
 * asked for. An ASSET invoice is never settled from declared amounts alone:
 * RGB credits are not bounded by the float caps, so guessing the units would
 * mint ledger balance. Those stay pending for a later pass.
 */
export async function settleInvoiceWithLiveAmounts(
  db: GatewayDb,
  ledger: Ledger,
  rln: Pick<RlnApi, 'getPayment'>,
  row: LnInvoiceRow,
  log?: { warn(obj: unknown, msg: string): void },
): Promise<boolean> {
  let payment: Payment | undefined;
  try {
    // Gateway invoices come from /lninvoice, which are auto-claim inbound.
    payment = (
      await rln.getPayment({ payment_hash: row.payment_hash, payment_type: 'InboundAutoClaim' })
    ).payment;
  } catch {
    // Fall back to the declared amounts below.
  }
  if (row.asset_id !== null && (payment?.asset_amount ?? null) === null) {
    log?.warn(
      { paymentHash: row.payment_hash },
      'settled asset invoice has no live asset amount; leaving pending',
    );
    return false;
  }
  const amtMsat = payment?.amt_msat ?? row.amt_msat;
  if (amtMsat === null || amtMsat === undefined) {
    log?.warn({ paymentHash: row.payment_hash }, 'settled invoice has no resolvable amount');
    return false;
  }
  settleInboundInvoice(db, ledger, row, amtMsat, payment?.asset_amount ?? row.asset_amount);
  return true;
}

/**
 * Refund every ln_out debit recorded for a payment hash (exactly once) and
 * mark the payment failed.
 */
export function refundOutboundPayment(
  db: GatewayDb,
  ledger: Ledger,
  userId: string,
  paymentHash: string,
): void {
  ledger.inTx(() => {
    const debits = db
      .prepare(
        "SELECT asset, delta_msat_or_units FROM ledger WHERE user_id = ? AND kind = 'ln_out' AND ref = ?",
      )
      .all(userId, paymentHash) as Array<{ asset: string; delta_msat_or_units: number }>;
    for (const debit of debits) {
      ledger.creditOnce({
        userId,
        asset: debit.asset,
        amount: -debit.delta_msat_or_units,
        kind: 'refund_ln_out',
        ref: paymentHash,
      });
    }
    updateResourceState(db, 'payment_hash', paymentHash, userId, 'failed');
  });
}

/**
 * Refund every `withdraw` debit recorded for a withdrawal id (exactly once)
 * and mark the row failed. A witness RGB withdraw debits TWO assets — the RGB
 * units and the msat that funds the node-paid witness output — so the refund
 * has to mirror what was actually debited rather than the row's single
 * asset/amount pair.
 */
export function refundWithdrawal(
  db: GatewayDb,
  ledger: Ledger,
  userId: string,
  withdrawalId: string,
): void {
  ledger.inTx(() => {
    const debits = db
      .prepare(
        "SELECT asset, delta_msat_or_units FROM ledger WHERE user_id = ? AND kind = 'withdraw' AND ref = ?",
      )
      .all(userId, withdrawalId) as Array<{ asset: string; delta_msat_or_units: number }>;
    for (const debit of debits) {
      ledger.creditOnce({
        userId,
        asset: debit.asset,
        amount: -debit.delta_msat_or_units,
        kind: 'refund_withdraw',
        ref: withdrawalId,
      });
    }
    db.prepare("UPDATE withdrawals SET state = 'failed' WHERE id = ?").run(withdrawalId);
  });
}

interface DepositPrepareBody {
  kind: 'btc' | 'rgb';
  amountMsat?: number;
  assetId?: string;
  amount?: number;
}

interface PayBody {
  invoice: string;
  amtMsat?: number;
  assetAmount?: number;
}

interface InvoiceCreateBody {
  amtMsat?: number;
  expirySec?: number;
  assetId?: string;
  assetAmount?: number;
  description?: string;
}

interface WithdrawBody {
  kind: 'btc' | 'rgb';
  address?: string;
  amountSat?: number;
  assetId?: string;
  amount?: number;
  recipientId?: string;
  witnessAmountSat?: number;
  transportEndpoints?: string[];
  feeRateSatPerVb?: number;
}

const DEFAULT_INVOICE_EXPIRY_SEC = 3600;
const DEFAULT_WITHDRAW_FEE_RATE = 2;
/**
 * A witness recipient id (`<chain>:wvout:…`) is paid by a NEW output, so
 * rgb-lib demands its sat value up front and rejects the send outright without
 * it; a blind one (`<chain>:utxob:…`) rejects the send if given one.
 */
const WITNESS_RECIPIENT_PATTERN = /^([a-z0-9]+:)?wvout:/;

/**
 * Classify an RLN failure on a money route. A 4xx means the node rejected
 * THESE parameters, not that it is unhealthy: 502 would tell the caller to
 * retry something that can never succeed, and — because the idempotency
 * middleware treats 5xx as retryable — would release the claim for a request
 * whose own row is already terminal. Anything else stays sanitized (502/504).
 */
function rlnRejection(error: unknown, code: string, message: string): HttpError {
  if (error instanceof RlnHttpError && error.status >= 400 && error.status < 500) {
    return new HttpError(400, code, message, { cause: error });
  }
  return sanitizeRlnError(error);
}
/** Page size for the node-wide /listpayments walk behind GET /v1/ln/payments. */
const PAYMENTS_PAGE_SIZE = 100;
/**
 * Slack applied to the /listpayments age floor. RLN reports payment timestamps
 * in unix seconds from its own clock, so the floor must tolerate gateway/node
 * clock drift rather than cut a real payment out of the listing.
 */
const PAYMENT_WALK_SKEW_SEC = 86_400;

export class LnFlows {
  constructor(
    private readonly db: GatewayDb,
    private readonly ledger: Ledger,
    private readonly rln: RlnApi,
    private readonly config: GatewayConfig,
  ) {}

  /**
   * BTC cap headroom check for NEW exposure, counting not-yet-settled intents
   * (pending deposits and pending fixed-amount invoices) so parallel prepares
   * cannot overshoot the caps.
   */
  private pendingBtcExposure(userId?: string, now: number = Date.now()): number {
    const scope = userId === undefined ? '' : 'AND user_id = ?';
    const params = userId === undefined ? [] : [userId];
    // Expired deposit intents stop counting even before the worker sweeps
    // them, so abandoned prepares cannot pin cap headroom forever.
    const deposits = this.db
      .prepare(
        `SELECT COALESCE(SUM(amount), 0) AS total FROM pending_deposits
         WHERE asset = ? AND state = 'pending' AND expires_at > ? ${scope}`,
      )
      .get(BTC_ASSET, now, ...params) as { total: number };
    // Same reasoning for invoices: an invoice past its BOLT11 expiry can no
    // longer be paid, so it must stop pinning headroom even when the
    // reconciler has not yet reached RLN to flip it to 'expired'.
    const invoices = this.db
      .prepare(
        `SELECT COALESCE(SUM(amt_msat), 0) AS total FROM ln_invoices
         WHERE state = 'pending' AND amt_msat IS NOT NULL AND expires_at > ? ${scope}`,
      )
      .get(now, ...params) as { total: number };
    return deposits.total + invoices.total;
  }

  private assertBtcHeadroom(userId: string, amountMsat: number): void {
    const userExposure = this.ledger.balance(userId, BTC_ASSET) + this.pendingBtcExposure(userId);
    if (userExposure + amountMsat > this.config.floatCapPerUserMsat) {
      throw new FloatCapExceededError('user');
    }
    const globalExposure = this.ledger.globalBalance(BTC_ASSET) + this.pendingBtcExposure();
    if (globalExposure + amountMsat > this.config.floatCapGlobalMsat) {
      throw new FloatCapExceededError('global');
    }
  }

  async prepareDeposit(userId: string, body: DepositPrepareBody) {
    const depositId = randomUUID();
    if (body.kind === 'btc') {
      if (body.amountMsat === undefined) {
        throw new HttpError(400, 'BAD_REQUEST', 'amountMsat is required for a btc deposit');
      }
      // Fast-fail before the RLN round-trip; re-checked atomically below.
      this.assertBtcHeadroom(userId, body.amountMsat);
      let address: string;
      try {
        address = (await this.rln.address()).address;
      } catch (error) {
        throw sanitizeRlnError(error);
      }
      // Re-check + insert in ONE transaction: other users' inserts between the
      // pre-check and here must not let the global cap overshoot.
      this.ledger.inTx(() => {
        // The deposits worker attributes on-chain funds by address, so each
        // intent needs an address NEVER used by any earlier intent — not just
        // by a currently-pending one. pollBtcDeposit sums every confirmed tx
        // paying the address with no lower time bound, so reusing the address
        // of an already-credited (or expired) deposit would re-credit that
        // same payment to a second intent (money creation). RLN only pins
        // addresses when run with --reuse-addresses (forbidden in the gateway
        // README's deployment note); a collision here means that
        // misconfiguration.
        const clash = this.db
          .prepare('SELECT 1 FROM pending_deposits WHERE target = ?')
          .get(address);
        if (clash !== undefined) {
          throw new HttpError(
            502,
            'DEPOSIT_ADDRESS_CONFLICT',
            'node returned an address already assigned to a pending deposit',
          );
        }
        this.assertBtcHeadroom(userId, body.amountMsat as number);
        this.insertPendingDeposit(depositId, userId, 'btc', BTC_ASSET, body.amountMsat as number, {
          target: address,
          invoice: null,
        });
      });
      return { depositId, kind: 'btc' as const, address, invoice: null, recipientId: null };
    }
    if (body.assetId === undefined || body.amount === undefined) {
      throw new HttpError(400, 'BAD_REQUEST', 'assetId and amount are required for an rgb deposit');
    }
    let invoice: string;
    let recipientId: string;
    try {
      const response = await this.rln.rgbInvoice({
        min_confirmations: this.config.depositMinConfirmations,
        asset_id: body.assetId,
        assignment: { type: 'Fungible', value: body.amount },
        witness: false,
        transport_endpoints: [this.config.rgbProxyUrl],
      });
      invoice = response.invoice;
      recipientId = response.recipient_id;
    } catch (error) {
      throw sanitizeRlnError(error);
    }
    // Same target-uniqueness rule as the BTC branch, and for the same reason:
    // the deposits worker attributes RGB by recipient_id (findSettledTransfer
    // matches transfers on it with no lower time bound), so two intents sharing
    // one recipient id would credit the SAME settled transfer twice — units
    // minted on the ledger the node does not hold. rgb-lib blinds a fresh
    // secret per /rgbinvoice so a repeat is not expected; the check makes the
    // invariant the deposits worker documents actually hold rather than assumed.
    const assetId = body.assetId;
    const amount = body.amount;
    this.ledger.inTx(() => {
      const clash = this.db
        .prepare('SELECT 1 FROM pending_deposits WHERE target = ?')
        .get(recipientId);
      if (clash !== undefined) {
        throw new HttpError(
          502,
          'DEPOSIT_ADDRESS_CONFLICT',
          'node returned a recipient id already assigned to a pending deposit',
        );
      }
      this.insertPendingDeposit(depositId, userId, 'rgb', assetId, amount, {
        target: recipientId,
        invoice,
      });
    });
    return { depositId, kind: 'rgb' as const, address: null, invoice, recipientId };
  }

  private insertPendingDeposit(
    depositId: string,
    userId: string,
    kind: 'btc' | 'rgb',
    asset: string,
    amount: number,
    details: { target: string; invoice: string | null },
    now: number = Date.now(),
  ): void {
    this.db
      .prepare(
        `INSERT INTO pending_deposits (id, user_id, kind, asset, amount, target, invoice, state, created_at, expires_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, 'pending', ?, ?)`,
      )
      .run(
        depositId,
        userId,
        kind,
        asset,
        amount,
        details.target,
        details.invoice,
        now,
        now + this.config.depositTtlSeconds * 1000,
      );
  }

  async pay(userId: string, body: PayBody) {
    let decoded;
    try {
      decoded = await this.rln.decodeLnInvoice({ invoice: body.invoice });
    } catch (error) {
      throw error instanceof RlnHttpError
        ? new HttpError(400, 'INVALID_INVOICE', 'invoice could not be decoded')
        : sanitizeRlnError(error);
    }
    const paymentHash = decoded.payment_hash;
    const invoiceAmtMsat = decoded.amt_msat ?? null;
    if (invoiceAmtMsat !== null && body.amtMsat !== undefined && body.amtMsat !== invoiceAmtMsat) {
      throw new HttpError(400, 'AMOUNT_MISMATCH', 'amtMsat conflicts with the invoice amount');
    }
    const amtMsat = invoiceAmtMsat ?? body.amtMsat;
    if (amtMsat === undefined) {
      throw new HttpError(400, 'AMOUNT_REQUIRED', 'invoice has no amount; amtMsat is required');
    }
    // A decoded amount is attacker-supplied and NOT covered by the body schema:
    // `lnbc0…` decodes to 0 (lightning-invoice only rejects amounts that are
    // not a whole msat) and an out-of-range one to an unsafe integer. Both
    // reach Ledger.debit, whose RangeError would surface as a 500 instead of
    // the 400 a malformed invoice deserves.
    if (!Number.isSafeInteger(amtMsat) || amtMsat <= 0) {
      throw new HttpError(400, 'INVALID_INVOICE', 'invoice amount is out of range');
    }
    const assetId = decoded.asset_id ?? null;
    const invoiceAssetAmount = decoded.asset_amount ?? null;
    if (
      invoiceAssetAmount !== null &&
      body.assetAmount !== undefined &&
      body.assetAmount !== invoiceAssetAmount
    ) {
      // Same rule as the msat leg above: the invoice wins, so a caller whose
      // stated amount differs must be told rather than have the invoice's
      // amount silently debited from their asset float.
      throw new HttpError(400, 'AMOUNT_MISMATCH', 'assetAmount conflicts with the invoice amount');
    }
    const assetAmount = invoiceAssetAmount ?? body.assetAmount ?? null;
    if (assetId !== null && assetAmount === null) {
      throw new HttpError(
        400,
        'AMOUNT_REQUIRED',
        'asset invoice has no amount; assetAmount is required',
      );
    }
    // Same reasoning as the msat leg: an RGB amount decoded from the invoice
    // bypasses the body schema, so it is range-checked before the debit.
    if (assetAmount !== null && (!Number.isSafeInteger(assetAmount) || assetAmount <= 0)) {
      throw new HttpError(400, 'INVALID_INVOICE', 'invoice asset amount is out of range');
    }

    // Claim phase: debit + ownership atomically, or observe the existing state.
    const existingState = this.ledger.inTx((): string | undefined => {
      const owner = ownerOf(this.db, 'payment_hash', paymentHash);
      if (owner !== undefined && owner !== userId) {
        throw new HttpError(409, 'PAYMENT_CONFLICT', 'this invoice is already being handled');
      }
      // decodeLnInvoice only verifies the BOLT11 self-signature, so the hash is
      // attacker-chosen: refuse to hang an outbound claim off a hash another
      // user owns as an inbound invoice, which would otherwise lock that
      // user's real payer out with a 409 for the price of one refunded send.
      const invoiceOwner = ownerOf(this.db, 'invoice', paymentHash);
      if (invoiceOwner !== undefined && invoiceOwner !== userId) {
        throw new HttpError(409, 'PAYMENT_CONFLICT', 'this invoice is already being handled');
      }
      if (owner === userId) {
        const row = this.db
          .prepare("SELECT state FROM resource_map WHERE kind = 'payment_hash' AND resource_id = ?")
          .get(paymentHash) as { state: string };
        return row.state;
      }
      this.debitForPay(userId, paymentHash, amtMsat, assetId, assetAmount);
      recordOwnership(this.db, {
        kind: 'payment_hash',
        resourceId: paymentHash,
        userId,
        state: 'debited_pending',
      });
      return undefined;
    });
    if (existingState !== undefined) {
      // Never auto-retry: report the known state instead of sending again.
      if (existingState === 'succeeded') return { paymentHash, status: 'succeeded' as const };
      if (existingState === 'failed') {
        throw new HttpError(
          409,
          'PAYMENT_ALREADY_ATTEMPTED',
          'this invoice already failed; request a fresh invoice',
        );
      }
      return { paymentHash, status: 'pending' as const };
    }

    let response;
    try {
      response = await this.rln.sendPayment({
        invoice: body.invoice,
        ...(invoiceAmtMsat === null ? { amt_msat: amtMsat } : {}),
        ...(assetId !== null && invoiceAssetAmount === null && assetAmount !== null
          ? { asset_amount: assetAmount }
          : {}),
      });
    } catch (error) {
      if (error instanceof RlnHttpError) {
        // Definite rejection: RLN answered and did not accept the payment.
        refundOutboundPayment(this.db, this.ledger, userId, paymentHash);
        throw rlnRejection(error, 'PAYMENT_REJECTED', 'the node rejected this payment');
      }
      // Ambiguous (timeout / network): leave debited_pending for the reconciler.
      throw sanitizeRlnError(error);
    }
    const status = mapHtlcStatus(response.status);
    if (status === 'failed') {
      refundOutboundPayment(this.db, this.ledger, userId, paymentHash);
    } else {
      updateResourceState(
        this.db,
        'payment_hash',
        paymentHash,
        userId,
        status === 'succeeded' ? 'succeeded' : 'sent',
      );
    }
    return { paymentHash, status };
  }

  // InsufficientBalanceError escapes to the central error handler (server.ts),
  // which maps it to 409 INSUFFICIENT_BALANCE.
  private debitForPay(
    userId: string,
    paymentHash: string,
    amtMsat: number,
    assetId: string | null,
    assetAmount: number | null,
  ): void {
    this.ledger.debit({
      userId,
      asset: BTC_ASSET,
      amount: amtMsat,
      kind: 'ln_out',
      ref: paymentHash,
    });
    if (assetId !== null && assetAmount !== null) {
      this.ledger.debit({
        userId,
        asset: assetId,
        amount: assetAmount,
        kind: 'ln_out',
        ref: paymentHash,
      });
    }
  }

  async createInvoice(userId: string, body: InvoiceCreateBody) {
    // A BTC invoice MUST declare its amount: the float cap is enforced against
    // declared amounts at creation, and an amount-less invoice would accept
    // arbitrarily large payments outside any cap. Asset invoices declare the
    // asset amount instead (their BTC leg is the tiny HTLC carrier value).
    if (body.assetId === undefined && body.amtMsat === undefined) {
      throw new HttpError(400, 'AMOUNT_REQUIRED', 'amtMsat is required for a btc invoice');
    }
    if (body.assetId !== undefined && body.assetAmount === undefined) {
      throw new HttpError(400, 'AMOUNT_REQUIRED', 'assetAmount is required for an asset invoice');
    }
    // RLN rejects an asset invoice whose msat amount is under the channel HTLC
    // floor (`amt_msat.unwrap_or(0) < htlc_min_msat`), so an asset invoice with
    // no declared msat amount can never be created. Default the HTLC carrier
    // value to the configured floor, and reject a smaller explicit one here
    // with an actionable error instead of an opaque upstream failure.
    const amtMsat =
      body.assetId === undefined
        ? (body.amtMsat as number)
        : (body.amtMsat ?? this.config.assetInvoiceMinMsat);
    if (body.assetId !== undefined && amtMsat < this.config.assetInvoiceMinMsat) {
      throw new HttpError(
        400,
        'AMOUNT_BELOW_MINIMUM',
        `an asset invoice must carry at least ${this.config.assetInvoiceMinMsat} msat`,
      );
    }
    // Fast-fail before the RLN round-trip; re-checked atomically below.
    this.assertBtcHeadroom(userId, amtMsat);
    const expirySec = body.expirySec ?? DEFAULT_INVOICE_EXPIRY_SEC;
    let invoice: string;
    let paymentHash: string;
    try {
      const created = await this.rln.lnInvoice({
        amt_msat: amtMsat,
        expiry_sec: expirySec,
        asset_id: body.assetId ?? null,
        asset_amount: body.assetAmount ?? null,
        ...(body.description !== undefined ? { description: body.description } : {}),
      });
      invoice = created.invoice;
      paymentHash = (await this.rln.decodeLnInvoice({ invoice })).payment_hash;
    } catch (error) {
      throw rlnRejection(error, 'INVOICE_REJECTED', 'the node rejected these invoice parameters');
    }
    this.ledger.inTx(() => {
      // Re-check inside the insert transaction so parallel creations by
      // different users cannot jointly overshoot the global cap. Accepted
      // tradeoff: a failure here leaves the just-created invoice orphaned on
      // the node — harmless, because the invoice string never reached the
      // caller, so nobody can pay it.
      this.assertBtcHeadroom(userId, amtMsat);
      recordOwnership(this.db, {
        kind: 'invoice',
        resourceId: paymentHash,
        userId,
        state: 'pending',
      });
      const now = Date.now();
      this.db
        .prepare(
          `INSERT INTO ln_invoices (payment_hash, user_id, invoice, amt_msat, asset_id, asset_amount, state, created_at, expires_at)
           VALUES (?, ?, ?, ?, ?, ?, 'pending', ?, ?)`,
        )
        .run(
          paymentHash,
          userId,
          invoice,
          amtMsat,
          body.assetId ?? null,
          body.assetAmount ?? null,
          now,
          now + expirySec * 1000,
        );
    });
    return { invoice, paymentHash };
  }

  invoiceRow(paymentHash: string): LnInvoiceRow | undefined {
    return this.db.prepare('SELECT * FROM ln_invoices WHERE payment_hash = ?').get(paymentHash) as
      LnInvoiceRow | undefined;
  }

  async getInvoice(userId: string, hash: string) {
    const row = this.invoiceRow(hash);
    if (row === undefined || row.user_id !== userId) {
      throw new HttpError(404, 'NOT_FOUND', 'resource not found');
    }
    let state: LnInvoiceRow['state'] = row.state;
    if (row.state === 'pending') {
      let live;
      try {
        live = (await this.rln.invoiceStatus({ invoice: row.invoice })).status;
      } catch (error) {
        throw sanitizeRlnError(error);
      }
      if (live === 'Succeeded') {
        const settled = await settleInvoiceWithLiveAmounts(this.db, this.ledger, this.rln, row);
        if (settled) state = 'settled';
      } else if (live === 'Expired') {
        this.ledger.inTx(() => {
          this.db
            .prepare("UPDATE ln_invoices SET state = 'expired' WHERE payment_hash = ?")
            .run(hash);
          updateResourceState(this.db, 'invoice', hash, userId, 'expired');
        });
        state = 'expired';
      }
    }
    return {
      paymentHash: row.payment_hash,
      invoice: row.invoice,
      state,
      amtMsat: row.amt_msat,
      assetId: row.asset_id,
      assetAmount: row.asset_amount,
      createdAt: row.created_at,
    };
  }

  /**
   * The user's payments. RLN's /listpayments is node-wide and paginated
   * (newest first, 100 per page by default), so a single unpaginated call
   * would silently hide a user's history behind 100 newer payments from other
   * users. Walk pages newest→oldest, scoping each one, and stop as soon as
   * every payment_hash/invoice this user owns has been seen.
   */
  async listPayments(userId: string) {
    const owned = this.db
      .prepare(
        `SELECT DISTINCT resource_id, created_at FROM resource_map
         WHERE user_id = ? AND kind IN ('payment_hash', 'invoice')`,
      )
      .all(userId) as { resource_id: string; created_at: number }[];
    // Two independent stop conditions, because neither alone terminates.
    //
    // `remaining` is the fast one: stop as soon as every owned id has been
    // seen. It cannot be a count comparison — a resource_map row can exist for
    // a payment RLN never learned about (pay records ownership BEFORE
    // /sendpayment, and the reconciler refunds and leaves the row 'failed' when
    // the send never landed), so `seen === owned.length` is permanently
    // unreachable for that user.
    //
    // The age floor is the backstop for exactly that case: pages come
    // newest-first, so once an entire page predates the user's oldest owned
    // resource, no older page can hold one of their payments. Without it a
    // single unfindable id walks the node's whole payment history on EVERY
    // call. The skew margin absorbs the gateway/node clock difference and the
    // fact that RLN stamps an invoice's payment before the row is inserted.
    const remaining = new Set(owned.map((row) => row.resource_id));
    const oldestOwnedMs = owned.reduce(
      (min, row) => Math.min(min, row.created_at),
      Number.POSITIVE_INFINITY,
    );
    const oldestOwnedSec = Math.floor(oldestOwnedMs / 1000) - PAYMENT_WALK_SKEW_SEC;
    const scoped: Payment[] = [];
    let pageCursor: number | undefined;
    while (remaining.size > 0) {
      let page;
      try {
        page = await this.rln.listPayments({
          ...(pageCursor !== undefined ? { index_offset: pageCursor } : {}),
          max_payments: PAYMENTS_PAGE_SIZE,
        });
      } catch (error) {
        throw sanitizeRlnError(error);
      }
      for (const payment of scopePayments(this.db, userId, page.payments)) {
        scoped.push(payment);
        remaining.delete(payment.payment_hash);
      }
      // Every payment on this page predates anything the user owns: older
      // pages cannot match either.
      if (page.payments.every((payment) => payment.created_at < oldestOwnedSec)) break;
      // Short page, no cursor to advance to, or a non-progressing upstream
      // cursor (would loop forever): the walk is done.
      if (page.payments.length < PAYMENTS_PAGE_SIZE || page.last_index_offset === 0) break;
      if (pageCursor !== undefined && page.last_index_offset >= pageCursor) break;
      pageCursor = page.last_index_offset;
    }
    return {
      payments: scoped.map((payment: Payment) => ({
        paymentHash: payment.payment_hash,
        direction:
          payment.payment_type === 'Outbound' ? ('outbound' as const) : ('inbound' as const),
        status: mapHtlcStatus(payment.status),
        amtMsat: payment.amt_msat ?? null,
        assetId: payment.asset_id ?? null,
        assetAmount: payment.asset_amount ?? null,
        createdAt: payment.created_at,
        updatedAt: payment.updated_at,
      })),
    };
  }

  /**
   * Withdraw from the float back to the user. Unlike pay (payment_hash) and
   * on-chain complete (pending_ops), a withdrawal has no natural resource id,
   * so the idempotency KEY is the resource: the withdrawal id derives from
   * (user, key) and the withdrawals row is the replay guard. The idempotency
   * middleware alone is NOT enough — it releases claims on 5xx (an ambiguous
   * RLN timeout is a 504) and reclaims stale ones after a crash, and either
   * path would re-debit and re-broadcast on the documented same-key retry.
   */
  async withdraw(userId: string, body: WithdrawBody, idempotencyKey: string) {
    const withdrawalId = createHash('sha256')
      .update(userId)
      .update('\n')
      .update(idempotencyKey)
      .digest('hex');
    const feeRate = body.feeRateSatPerVb ?? DEFAULT_WITHDRAW_FEE_RATE;
    let asset: string;
    let amount: number;
    let target: string;
    if (body.kind === 'btc') {
      if (body.address === undefined || body.amountSat === undefined) {
        throw new HttpError(
          400,
          'BAD_REQUEST',
          'address and amountSat are required for a btc withdraw',
        );
      }
      asset = BTC_ASSET;
      amount = body.amountSat * 1000;
      target = body.address;
    } else {
      if (
        body.assetId === undefined ||
        body.amount === undefined ||
        body.recipientId === undefined
      ) {
        throw new HttpError(
          400,
          'BAD_REQUEST',
          'assetId, amount and recipientId are required for an rgb withdraw',
        );
      }
      if (body.assetId === BTC_ASSET) {
        // BTC_ASSET is the ledger's reserved key for the msat float. Letting it
        // through would debit that float in msat units for an RGB send that
        // rgb-lib can never resolve, and an ambiguous (timeout) outcome leaves
        // that debit standing permanently.
        throw new HttpError(400, 'BAD_REQUEST', `assetId must not be '${BTC_ASSET}'`);
      }
      asset = body.assetId;
      amount = body.amount;
      target = body.recipientId;
    }
    const isWitnessRecipient = body.kind === 'rgb' && WITNESS_RECIPIENT_PATTERN.test(target);
    if (body.kind === 'rgb') {
      if (isWitnessRecipient && body.witnessAmountSat === undefined) {
        throw new HttpError(
          400,
          'WITNESS_AMOUNT_REQUIRED',
          'witnessAmountSat is required for a wvout recipient',
        );
      }
      if (!isWitnessRecipient && body.witnessAmountSat !== undefined) {
        throw new HttpError(
          400,
          'BAD_REQUEST',
          'witnessAmountSat is only valid for a wvout recipient',
        );
      }
    }
    // SSRF guard: consignment endpoints make the node dial out; only
    // operator-allowlisted proxies are accepted (explicit error, no fallback).
    const transportEndpoints = body.transportEndpoints ?? [this.config.rgbProxyUrl];
    if (body.kind === 'rgb') {
      const denied = transportEndpoints.filter(
        (endpoint) => !this.config.rgbTransportAllowlist.includes(endpoint),
      );
      if (denied.length > 0) {
        throw new HttpError(
          400,
          'TRANSPORT_ENDPOINT_NOT_ALLOWED',
          'transportEndpoints must be on the operator allowlist',
        );
      }
    }

    // A `wvout:` recipient is paid by a NEW output that the NODE's on-chain
    // wallet funds: rgb-lib pushes (script, amount_sat) as an extra output of
    // the vanilla transaction (wallet/online.rs, Beneficiary::WitnessVout), and
    // the script is encoded in the caller-supplied recipient id. Those sats
    // land in the user's own vault, so they are a BTC withdrawal in all but
    // name and are debited from the user's msat float alongside the RGB units.
    // Without this a caller could name any sat value and drain the node's
    // on-chain balance into a script they control for the price of one asset
    // unit. The two debits share the withdrawal ref but differ in asset, so
    // the ledger's (user, asset, kind, ref) uniqueness still holds.
    const witnessMsat = isWitnessRecipient ? (body.witnessAmountSat as number) * 1000 : 0;

    // Claim phase: debit + row atomically, or observe the recorded state of a
    // previous attempt under the same key. InsufficientBalanceError escapes to
    // the central handler (409).
    const existing = this.ledger.inTx((): { state: string; txid: string | null } | undefined => {
      const row = this.db
        .prepare('SELECT state, txid FROM withdrawals WHERE id = ?')
        .get(withdrawalId) as { state: string; txid: string | null } | undefined;
      if (row !== undefined) return row;
      this.ledger.debit({ userId, asset, amount, kind: 'withdraw', ref: withdrawalId });
      if (witnessMsat > 0) {
        this.ledger.debit({
          userId,
          asset: BTC_ASSET,
          amount: witnessMsat,
          kind: 'withdraw',
          ref: withdrawalId,
        });
      }
      this.db
        .prepare(
          `INSERT INTO withdrawals (id, user_id, asset, amount, target, state, created_at)
           VALUES (?, ?, ?, ?, ?, 'pending', ?)`,
        )
        .run(withdrawalId, userId, asset, amount, target, Date.now());
      return undefined;
    });
    if (existing !== undefined) {
      // Never auto-retry a send: report the recorded outcome instead.
      if (existing.state === 'sent' && existing.txid !== null) {
        return { withdrawalId, txid: existing.txid };
      }
      if (existing.state === 'failed') {
        throw new HttpError(
          409,
          'WITHDRAWAL_ALREADY_ATTEMPTED',
          'a withdrawal with this Idempotency-Key already failed and was refunded; use a fresh key',
        );
      }
      // pending (crashed mid-flight) or ambiguous: the send may have broadcast.
      throw new HttpError(
        409,
        'WITHDRAWAL_UNRESOLVED',
        'a withdrawal with this Idempotency-Key is unresolved and may have broadcast; contact the operator',
      );
    }

    let txid: string;
    try {
      if (body.kind === 'btc') {
        txid = (
          await this.rln.sendBtc({
            amount: body.amountSat as number,
            address: target,
            fee_rate: feeRate,
            skip_sync: false,
          })
        ).txid;
      } else {
        txid = (
          await this.rln.sendRgb({
            donation: false,
            fee_rate: feeRate,
            min_confirmations: this.config.depositMinConfirmations,
            recipient_map: {
              [asset]: [
                {
                  recipient_id: target,
                  assignment: { type: 'Fungible', value: amount },
                  transport_endpoints: transportEndpoints,
                  ...(isWitnessRecipient
                    ? {
                        witness_data: {
                          amount_sat: body.witnessAmountSat as number,
                          blinding: null,
                        },
                      }
                    : {}),
                },
              ],
            },
          })
        ).txid;
      }
    } catch (error) {
      if (error instanceof RlnHttpError && error.status < 500) {
        // Definite rejection: RLN refused THESE parameters before broadcasting
        // anything (every 4xx variant is raised pre-broadcast). Refund and mark
        // failed.
        refundWithdrawal(this.db, this.ledger, userId, withdrawalId);
      } else {
        // Ambiguous: leave the debit in place for operator resolution — the
        // send may have broadcast. Never auto-retry. A 5xx counts as ambiguous,
        // not as a rejection: rgb-lib broadcasts FIRST and then writes its
        // bookkeeping (broadcast_psbt -> BDK persist -> txo/DB updates ->
        // commit), and every one of those post-broadcast failure points reaches
        // RLN as APIError::IO/Unexpected, i.e. a 500 for a transaction that is
        // already on the network. Refunding it would hand back money the node
        // actually paid out.
        this.db
          .prepare("UPDATE withdrawals SET state = 'ambiguous' WHERE id = ?")
          .run(withdrawalId);
      }
      throw rlnRejection(error, 'WITHDRAW_REJECTED', 'the node rejected this withdrawal');
    }
    this.db
      .prepare("UPDATE withdrawals SET state = 'sent', txid = ? WHERE id = ?")
      .run(txid, withdrawalId);
    return { withdrawalId, txid };
  }
}

export function registerLnRoutes(app: FastifyInstance): void {
  const auth = { onRequest: [app.authenticate] };
  const moneyMoving = {
    onRequest: [app.authenticate],
    preHandler: [app.idempotency.preHandler],
    onSend: app.idempotency.onSend,
  };

  // Money-moving in the sense the idempotency middleware guards: each call
  // burns a node address and pins float-cap headroom for the deposit TTL, so a
  // retried prepare must return the first intent rather than mint a new one.
  app.post(
    '/v1/ln/deposit/prepare',
    { schema: depositPrepareRouteSchema, ...moneyMoving },
    (request, reply) => {
      const userId = request.userId as string;
      const body = request.body as DepositPrepareBody;
      return app.queues.enqueue(userId, async () => {
        const prepared = await app.ln.prepareDeposit(userId, body);
        return reply.code(201).send(prepared);
      });
    },
  );

  app.post('/v1/ln/pay', { schema: payRouteSchema, ...moneyMoving }, (request) => {
    const userId = request.userId as string;
    const body = request.body as PayBody;
    return app.queues.enqueue(userId, () => app.ln.pay(userId, body));
  });

  app.post('/v1/ln/invoice', { schema: invoiceCreateRouteSchema, ...auth }, (request, reply) => {
    const userId = request.userId as string;
    const body = request.body as InvoiceCreateBody;
    return app.queues.enqueue(userId, async () => {
      const created = await app.ln.createInvoice(userId, body);
      return reply.code(201).send(created);
    });
  });

  app.get('/v1/ln/invoice/:hash', { schema: invoiceGetRouteSchema, ...auth }, (request) => {
    const userId = request.userId as string;
    const { hash } = request.params as { hash: string };
    return app.queues.enqueue(userId, () => app.ln.getInvoice(userId, hash));
  });

  app.get('/v1/ln/payments', { schema: paymentsListRouteSchema, ...auth }, (request) => {
    const userId = request.userId as string;
    return app.queues.enqueue(userId, () => app.ln.listPayments(userId));
  });

  app.get('/v1/ln/balance', { schema: lnBalanceRouteSchema, ...auth }, (request) => {
    const userId = request.userId as string;
    const { [BTC_ASSET]: btcMsat = 0, ...assets } = app.ledger.balances(userId);
    return { btcMsat, assets };
  });

  app.post('/v1/ln/withdraw', { schema: withdrawRouteSchema, ...moneyMoving }, (request, reply) => {
    const userId = request.userId as string;
    const body = request.body as WithdrawBody;
    const idempotencyKey = request.idempotency?.key;
    if (idempotencyKey === undefined) {
      throw new HttpError(500, 'INTERNAL', 'withdraw requires the idempotency preHandler');
    }
    return app.queues.enqueue(userId, async () => {
      const result = await app.ln.withdraw(userId, body, idempotencyKey);
      return reply.code(201).send(result);
    });
  });
}
