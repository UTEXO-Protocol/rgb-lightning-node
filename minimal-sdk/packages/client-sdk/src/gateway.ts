/**
 * Thin typed client for the minimal gateway API. Browser/mobile-friendly:
 * global fetch + AbortController + crypto only, no Node imports.
 *
 * Money-moving calls carry an Idempotency-Key; one is generated per logical
 * operation unless the caller pins its own (retry the SAME operation with the
 * SAME key to get the cached response instead of a duplicate spend).
 */
import type { OnchainIntent } from './verify.js';

export interface GatewayClientOptions {
  baseUrl: string;
  /** Per-user bearer token; omitted only for createUser (operator bootstrap). */
  token?: string;
  timeoutMs?: number;
  /** Injectable for tests; defaults to globalThis.fetch. */
  fetchFn?: typeof fetch;
}

export class GatewayError extends Error {
  constructor(
    readonly status: number,
    readonly code: string,
    message: string,
  ) {
    super(message);
    this.name = 'GatewayError';
  }
}

export interface CreatedUser {
  userId: string;
  token: string;
  createdAt: number;
}

export interface RegisterXpubsParams {
  vanilla: string;
  colored: string;
  fingerprint: string;
}

export interface Balance {
  settled: number;
  future: number;
  spendable: number;
}

/**
 * An rgb-lib Assignment as a variant name plus its fungible value. `kind`
 * carries the variant verbatim (`Fungible`, `Any`, `InflationRight`, ...), so a
 * variant this SDK predates still arrives named rather than dropped.
 */
export interface Assignment {
  kind: string;
  amount: number | null;
}

export interface WalletAsset {
  assetId: string;
  schema: string;
  ticker: string | null;
  name: string;
  details: string | null;
  precision: number;
  /** Total issued amount; null for schemas without one (UDA). */
  issuedSupply: number | null;
  /** Unix seconds of asset genesis. */
  timestamp: number;
  /** Unix seconds the gateway wallet imported the asset. */
  addedAt: number;
  balance: Balance;
}

export interface WalletBalances {
  btc: { vanilla: Balance; colored: Balance };
  assets: WalletAsset[];
}

/**
 * Derivation an address came from. Re-derive it from your own keys
 * (`deriveTaproot`) and compare before showing it as a deposit target: the
 * gateway holds the xpubs and could otherwise return any address. Null means the
 * gateway could not attribute it — treat that as unproven, not as safe.
 */
export interface AddressDerivation {
  account: 'vanilla';
  keychain: number;
  index: number;
  derivationPath: string;
  scriptHex: string;
}

export interface WalletAddress {
  address: string;
  derivation: AddressDerivation | null;
}

export interface WalletUnspent {
  txid: string;
  vout: number;
  amountSat: number;
  colorable: boolean;
  /** Blind receives already promised against this UTXO (reserved slots). */
  pendingBlinded: number;
  allocations: {
    assetId: string | null;
    amount: number | null;
    assignment: Assignment;
    settled: boolean;
  }[];
}

export interface WalletTransfer {
  idx: number;
  /** Batch this transfer belongs to; rgb-lib keys refresh/fail/delete by it. */
  batchTransferIdx: number | null;
  assetId: string | null;
  amount: number | null;
  assignments: Assignment[];
  kind: string;
  status: string;
  txid: string | null;
  recipientId: string | null;
  expiration: number | null;
  createdAt: number;
  updatedAt: number;
}

export interface FeeEstimate {
  /** Confirmation target asked for. */
  blocks: number;
  /** Whole sat/vB, ready to pass back as `feeRateSatPerVb`. */
  feeRateSatPerVb: number;
  /** Indexer ladder target the estimate came from (at or below `blocks`). */
  sourceBlocks: number;
}

export interface ReceiveParams {
  mode: 'blind' | 'witness';
  assetId?: string;
  amount?: number;
  durationSeconds?: number;
  minConfirmations?: number;
}

export interface ReceiveResult {
  invoice: string;
  recipientId: string;
  expirationTimestamp: number | null;
  /** Batch index of the created receive; null only on rgb-lib shape drift. */
  batchTransferIdx: number | null;
  mode: 'blind' | 'witness';
}

export interface PreparedOp {
  opId: string;
  psbt: string;
  intent: OnchainIntent;
  expiresAt: number;
}

export interface PrepareSendBtcParams {
  address: string;
  amountSat: number;
  feeRateSatPerVb?: number;
}

export interface PrepareSendAssetParams {
  assetId: string;
  amount: number;
  recipientId: string;
  witnessAmountSat?: number;
  transportEndpoints?: string[];
  donation?: boolean;
  minConfirmations?: number;
  feeRateSatPerVb?: number;
}

export interface PrepareCreateUtxosParams {
  num?: number;
  size?: number;
  upTo?: boolean;
  feeRateSatPerVb?: number;
}

export interface CompleteParams {
  opId: string;
  signedPsbt: string;
}

/**
 * Durable state of a prepared on-chain operation, for recovery after the
 * outcome of `complete` was lost (timeout, crash, 502 COMPLETE_AMBIGUOUS).
 *
 * `mayHaveBroadcast` is the field that matters after a failed complete: the
 * gateway's wallet broadcasts before it finishes its bookkeeping, so a txid can
 * be recorded on an operation that never completed. When it is true the
 * transaction may already be on the network — retry `complete` (which is safe
 * and finishes the bookkeeping) rather than preparing a second operation.
 */
export interface OperationStatus {
  opId: string;
  kind: OnchainIntent['kind'];
  state: 'pending' | 'completed' | 'expired';
  txid: string | null;
  mayHaveBroadcast: boolean;
  intent: OnchainIntent;
  createdAt: number;
  expiresAt: number;
}

/** A prepare response whose intent summary contradicts the request that produced it. */
export class IntentMismatchError extends Error {
  constructor(readonly mismatches: string[]) {
    super(`gateway intent does not match the request: ${mismatches.join('; ')}`);
    this.name = 'IntentMismatchError';
  }
}

/**
 * Bind the gateway's intent summary to the caller's OWN request.
 *
 * verify.ts check 2 matches the PSBT's outputs against `intent`. If that
 * intent is simply whatever the server chose to return, the check is vacuous
 * against the threat verify-before-sign exists for: a hostile gateway pairs an
 * attacker script in the PSBT with the same attacker address in the intent and
 * both "match", while checks 1/3/4/5 stay clean (the output is accounted, so
 * change-own skips it). The design doc calls for the *user's* stated intent
 * (docs/design/minimal-sdk-and-lightweight-rln.md, check 2), so every field the
 * caller actually stated is asserted here before the intent is handed on.
 *
 * Fields the caller left to the server (fee rate, transport endpoints, utxo
 * shape) are asserted only when the caller pinned them: they carry no bitcoin
 * value for check 2, and the absolute fee stays bounded by `maxFeeSat`. The
 * shape assertions (no recipients on an asset/utxo intent, no asset on a
 * send-btc intent) are not cosmetic — a foreign `asset.witnessAmountSat` or an
 * extra recipient is exactly how a server would smuggle an unaccounted output
 * past check 3.
 */
function checkedIntent(
  prepared: PreparedOp,
  kind: OnchainIntent['kind'],
  compare: (intent: OnchainIntent, mismatch: (detail: string) => void) => void,
): PreparedOp {
  const mismatches: string[] = [];
  const intent = prepared.intent as OnchainIntent | undefined;
  if (intent === undefined || intent === null || typeof intent !== 'object') {
    throw new IntentMismatchError(['prepare response carries no intent summary']);
  }
  if (intent.kind !== kind) mismatches.push(`kind ${String(intent.kind)} != ${kind}`);
  if (!Array.isArray(intent.recipients)) mismatches.push('intent.recipients is not an array');
  else compare(intent, (detail) => mismatches.push(detail));
  if (mismatches.length > 0) throw new IntentMismatchError(mismatches);
  return prepared;
}

function checkPinned<T>(name: string, requested: T | undefined, returned: T): string | null {
  if (requested === undefined || requested === returned) return null;
  return `${name} ${String(returned)} != ${String(requested)}`;
}

export interface LnDepositPrepareParams {
  kind: 'btc' | 'rgb';
  /** Declared BTC deposit amount (cap check), required for kind 'btc'. */
  amountMsat?: number;
  assetId?: string;
  amount?: number;
}

export interface LnDepositPrepareResult {
  depositId: string;
  kind: 'btc' | 'rgb';
  /** Node-owned BTC target address (kind 'btc'). */
  address: string | null;
  /** RGB invoice to pay into the node (kind 'rgb'). */
  invoice: string | null;
  recipientId: string | null;
}

export interface LnPayParams {
  invoice: string;
  /** Required when the invoice carries no amount. */
  amtMsat?: number;
  assetAmount?: number;
}

export type LnPaymentStatus = 'pending' | 'succeeded' | 'failed';

export interface LnPayResult {
  paymentHash: string;
  status: LnPaymentStatus;
}

export interface LnInvoiceCreateParams {
  amtMsat?: number;
  expirySec?: number;
  assetId?: string;
  assetAmount?: number;
  description?: string;
}

export interface LnInvoiceInfo {
  paymentHash: string;
  invoice: string;
  state: 'pending' | 'settled' | 'expired';
  amtMsat: number | null;
  assetId: string | null;
  assetAmount: number | null;
  createdAt: number;
}

export interface LnPaymentEntry {
  paymentHash: string;
  direction: 'inbound' | 'outbound';
  status: LnPaymentStatus;
  amtMsat: number | null;
  assetId: string | null;
  assetAmount: number | null;
  createdAt: number;
  updatedAt: number;
}

export interface LnBalance {
  btcMsat: number;
  assets: Record<string, number>;
}

export interface LnWithdrawParams {
  kind: 'btc' | 'rgb';
  address?: string;
  amountSat?: number;
  assetId?: string;
  amount?: number;
  recipientId?: string;
  /** Required when recipientId is a witness (`<chain>:wvout:…`) beneficiary. */
  witnessAmountSat?: number;
  transportEndpoints?: string[];
  feeRateSatPerVb?: number;
}

export interface LnWithdrawResult {
  withdrawalId: string;
  txid: string;
}

/** RFC-4122 v4 id from crypto.getRandomValues; used for idempotency keys. */
export function generateIdempotencyKey(): string {
  const cryptoObj = globalThis.crypto;
  if (typeof cryptoObj.randomUUID === 'function') return cryptoObj.randomUUID();
  const bytes = cryptoObj.getRandomValues(new Uint8Array(16));
  bytes[6] = ((bytes[6] as number) & 0x0f) | 0x40;
  bytes[8] = ((bytes[8] as number) & 0x3f) | 0x80;
  const hex = Array.from(bytes, (b) => b.toString(16).padStart(2, '0')).join('');
  return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20)}`;
}

const DEFAULT_TIMEOUT_MS = 30_000;

interface RequestOptions {
  method: 'GET' | 'POST';
  path: string;
  body?: unknown;
  headers?: Record<string, string>;
  auth?: boolean;
}

export class GatewayClient {
  private readonly baseUrl: string;
  private readonly token: string | undefined;
  private readonly timeoutMs: number;
  private readonly fetchFn: typeof fetch;

  constructor(options: GatewayClientOptions) {
    this.baseUrl = options.baseUrl.replace(/\/+$/, '');
    this.token = options.token;
    this.timeoutMs = options.timeoutMs ?? DEFAULT_TIMEOUT_MS;
    this.fetchFn = options.fetchFn ?? globalThis.fetch.bind(globalThis);
  }

  private async request<T>(options: RequestOptions): Promise<T> {
    const headers: Record<string, string> = { ...options.headers };
    if (options.auth !== false) {
      if (this.token === undefined) {
        throw new GatewayError(0, 'NO_TOKEN', 'gateway client has no bearer token configured');
      }
      headers['authorization'] = `Bearer ${this.token}`;
    }
    if (options.body !== undefined) headers['content-type'] = 'application/json';

    const controller = new AbortController();
    const timer = setTimeout(() => controller.abort(), this.timeoutMs);
    // The timer stays armed until the body is read: a stalled body must not
    // hang the caller past the configured timeout.
    let response: Response;
    let payload: unknown = null;
    try {
      try {
        response = await this.fetchFn(`${this.baseUrl}${options.path}`, {
          method: options.method,
          headers,
          body: options.body === undefined ? null : JSON.stringify(options.body),
          signal: controller.signal,
        });
      } catch (error) {
        if (controller.signal.aborted) {
          throw new GatewayError(0, 'TIMEOUT', `request timed out after ${this.timeoutMs}ms`);
        }
        throw new GatewayError(
          0,
          'NETWORK_ERROR',
          error instanceof Error ? error.message : String(error),
        );
      }
      try {
        payload = await response.json();
      } catch {
        if (controller.signal.aborted) {
          throw new GatewayError(0, 'TIMEOUT', `request timed out after ${this.timeoutMs}ms`);
        }
        payload = null;
      }
    } finally {
      clearTimeout(timer);
    }
    if (!response.ok) {
      const errorBody = payload as { error?: { code?: string; message?: string } } | null;
      throw new GatewayError(
        response.status,
        errorBody?.error?.code ?? 'UNKNOWN',
        errorBody?.error?.message ?? `gateway returned ${response.status}`,
      );
    }
    return payload as T;
  }

  private post<T>(path: string, body: unknown, idempotencyKey?: string): Promise<T> {
    return this.request<T>({
      method: 'POST',
      path,
      body,
      headers: { 'idempotency-key': idempotencyKey ?? generateIdempotencyKey() },
    });
  }

  /** Bootstrap-only; requires the operator token, not a user token. */
  createUser(operatorToken: string): Promise<CreatedUser> {
    return this.request<CreatedUser>({
      method: 'POST',
      path: '/v1/users',
      auth: false,
      headers: { 'x-operator-token': operatorToken },
    });
  }

  me(): Promise<{ userId: string; createdAt: number }> {
    return this.request({ method: 'GET', path: '/v1/me' });
  }

  registerXpubs(params: RegisterXpubsParams): Promise<{ fingerprint: string; address: string }> {
    return this.request({ method: 'POST', path: '/v1/wallet/xpubs', body: params });
  }

  getAddress(): Promise<WalletAddress> {
    return this.request({ method: 'GET', path: '/v1/wallet/address' });
  }

  /**
   * Fee estimate from the gateway's indexer, which clients cannot reach
   * directly. Already a whole sat/vB in the range every fee-taking route
   * accepts, rounded up so signing at exactly this rate never lands below the
   * estimate.
   */
  getFeeEstimate(blocks?: number): Promise<FeeEstimate> {
    const query = blocks === undefined ? '' : `?blocks=${encodeURIComponent(String(blocks))}`;
    return this.request({ method: 'GET', path: `/v1/onchain/fee-estimate${query}` });
  }

  getBalances(): Promise<WalletBalances> {
    return this.request({ method: 'GET', path: '/v1/wallet/balances' });
  }

  getUnspents(): Promise<{ unspents: WalletUnspent[] }> {
    return this.request({ method: 'GET', path: '/v1/wallet/unspents' });
  }

  getTransfers(assetId?: string): Promise<{ transfers: WalletTransfer[] }> {
    const query = assetId === undefined ? '' : `?assetId=${encodeURIComponent(assetId)}`;
    return this.request({ method: 'GET', path: `/v1/wallet/transfers${query}` });
  }

  receive(params: ReceiveParams): Promise<ReceiveResult> {
    return this.request({ method: 'POST', path: '/v1/wallet/receive', body: params });
  }

  sync(): Promise<{ status: 'ok' }> {
    return this.request({ method: 'POST', path: '/v1/wallet/sync' });
  }

  /** Throws IntentMismatchError if the returned intent contradicts `params`. */
  async prepareSendBtc(params: PrepareSendBtcParams, idempotencyKey?: string): Promise<PreparedOp> {
    const prepared = await this.post<PreparedOp>(
      '/v1/onchain/send-btc/prepare',
      params,
      idempotencyKey,
    );
    return checkedIntent(prepared, 'send_btc', (intent, mismatch) => {
      const only = intent.recipients.length === 1 ? intent.recipients[0] : undefined;
      if (only === undefined) {
        mismatch(`expected exactly 1 recipient, got ${intent.recipients.length}`);
      } else {
        if (only.address !== params.address) {
          mismatch(`recipient address ${only.address} != ${params.address}`);
        }
        if (only.amountSat !== params.amountSat) {
          mismatch(`recipient amountSat ${only.amountSat} != ${params.amountSat}`);
        }
      }
      if (intent.asset !== null) mismatch('send-btc intent carries an asset');
      if (intent.utxos !== null) mismatch('send-btc intent carries a utxo shape');
      const fee = checkPinned('feeRateSatPerVb', params.feeRateSatPerVb, intent.feeRateSatPerVb);
      if (fee !== null) mismatch(fee);
    });
  }

  completeSendBtc(params: CompleteParams, idempotencyKey?: string): Promise<{ txid: string }> {
    return this.post('/v1/onchain/send-btc/complete', params, idempotencyKey);
  }

  /** Throws IntentMismatchError if the returned intent contradicts `params`. */
  async prepareSendAsset(
    params: PrepareSendAssetParams,
    idempotencyKey?: string,
  ): Promise<PreparedOp> {
    const prepared = await this.post<PreparedOp>(
      '/v1/onchain/send-asset/prepare',
      params,
      idempotencyKey,
    );
    return checkedIntent(prepared, 'send_asset', (intent, mismatch) => {
      if (intent.recipients.length !== 0) {
        mismatch(`send-asset intent carries ${intent.recipients.length} bitcoin recipient(s)`);
      }
      if (intent.utxos !== null) mismatch('send-asset intent carries a utxo shape');
      const asset = intent.asset;
      if (asset === null || typeof asset !== 'object') {
        mismatch('send-asset intent carries no asset');
        return;
      }
      if (asset.assetId !== params.assetId)
        mismatch(`assetId ${asset.assetId} != ${params.assetId}`);
      if (asset.amount !== params.amount) mismatch(`amount ${asset.amount} != ${params.amount}`);
      if (asset.recipientId !== params.recipientId) {
        mismatch(`recipientId ${asset.recipientId} != ${params.recipientId}`);
      }
      // Always asserted, including the blind case: a witness amount the caller
      // never asked for lets check 2 account for one foreign output of that
      // size, which check 3 would otherwise have refused.
      const witness = params.witnessAmountSat ?? null;
      if (asset.witnessAmountSat !== witness) {
        mismatch(`witnessAmountSat ${String(asset.witnessAmountSat)} != ${String(witness)}`);
      }
      const endpoints = params.transportEndpoints;
      if (
        endpoints !== undefined &&
        (asset.transportEndpoints.length !== endpoints.length ||
          !endpoints.every((endpoint, i) => asset.transportEndpoints[i] === endpoint))
      ) {
        mismatch(
          `transportEndpoints ${asset.transportEndpoints.join(',')} != ${endpoints.join(',')}`,
        );
      }
      const fee = checkPinned('feeRateSatPerVb', params.feeRateSatPerVb, intent.feeRateSatPerVb);
      if (fee !== null) mismatch(fee);
    });
  }

  completeSendAsset(params: CompleteParams, idempotencyKey?: string): Promise<{ txid: string }> {
    return this.post('/v1/onchain/send-asset/complete', params, idempotencyKey);
  }

  /** Throws IntentMismatchError if the returned intent contradicts `params`. */
  async prepareCreateUtxos(
    params: PrepareCreateUtxosParams,
    idempotencyKey?: string,
  ): Promise<PreparedOp> {
    const prepared = await this.post<PreparedOp>(
      '/v1/onchain/create-utxos/prepare',
      params,
      idempotencyKey,
    );
    return checkedIntent(prepared, 'create_utxos', (intent, mismatch) => {
      if (intent.recipients.length !== 0) {
        mismatch(`create-utxos intent carries ${intent.recipients.length} recipient(s)`);
      }
      if (intent.asset !== null) mismatch('create-utxos intent carries an asset');
      const utxos = intent.utxos;
      if (utxos === null || typeof utxos !== 'object') {
        mismatch('create-utxos intent carries no utxo shape');
        return;
      }
      for (const detail of [
        checkPinned('num', params.num, utxos.num),
        checkPinned('size', params.size, utxos.size),
        checkPinned('upTo', params.upTo, utxos.upTo),
        checkPinned('feeRateSatPerVb', params.feeRateSatPerVb, intent.feeRateSatPerVb),
      ]) {
        if (detail !== null) mismatch(detail);
      }
    });
  }

  completeCreateUtxos(
    params: CompleteParams,
    idempotencyKey?: string,
  ): Promise<{ txid: string | null; utxosCreated: number }> {
    return this.post('/v1/onchain/create-utxos/complete', params, idempotencyKey);
  }

  /**
   * Read back a prepared operation. Safe to poll: it is a read, takes no
   * idempotency key, and is not queued behind the gateway's wallet work, so it
   * answers even while a `complete` for the same user is still running.
   */
  getOnchainOperation(opId: string): Promise<OperationStatus> {
    return this.request({
      method: 'GET',
      path: `/v1/onchain/operations/${encodeURIComponent(opId)}`,
    });
  }

  /** Pin the idempotency key to retry a prepare without minting a second intent. */
  prepareLnDeposit(
    params: LnDepositPrepareParams,
    idempotencyKey?: string,
  ): Promise<LnDepositPrepareResult> {
    return this.post('/v1/ln/deposit/prepare', params, idempotencyKey);
  }

  /** Money-moving: pin the idempotency key to retry the SAME payment safely. */
  payLnInvoice(params: LnPayParams, idempotencyKey?: string): Promise<LnPayResult> {
    return this.post('/v1/ln/pay', params, idempotencyKey);
  }

  createLnInvoice(
    params: LnInvoiceCreateParams,
  ): Promise<{ invoice: string; paymentHash: string }> {
    return this.request({ method: 'POST', path: '/v1/ln/invoice', body: params });
  }

  getLnInvoice(paymentHash: string): Promise<LnInvoiceInfo> {
    return this.request({
      method: 'GET',
      path: `/v1/ln/invoice/${encodeURIComponent(paymentHash)}`,
    });
  }

  listLnPayments(): Promise<{ payments: LnPaymentEntry[] }> {
    return this.request({ method: 'GET', path: '/v1/ln/payments' });
  }

  getLnBalance(): Promise<LnBalance> {
    return this.request({ method: 'GET', path: '/v1/ln/balance' });
  }

  withdrawLn(params: LnWithdrawParams, idempotencyKey?: string): Promise<LnWithdrawResult> {
    return this.post('/v1/ln/withdraw', params, idempotencyKey);
  }
}
