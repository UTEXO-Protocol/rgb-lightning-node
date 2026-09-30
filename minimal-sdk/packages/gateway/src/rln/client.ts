/**
 * Thin typed fetch client for the single shared RLN instance.
 *
 * Every call carries a timeout. Failures surface as structured errors that
 * keep the RLN error body for gateway-internal handling; nothing from an RLN
 * error may reach an end user un-sanitized — call sanitizeRlnError() at the
 * route boundary (invariant I4: the admin token and node internals stay inside
 * the gateway).
 */
import { HttpError } from '../errors.js';
import type {
  AddressResponse,
  DecodeLnInvoiceRequest,
  DecodeLnInvoiceResponse,
  GetPaymentRequest,
  GetPaymentResponse,
  InvoiceStatusRequest,
  InvoiceStatusResponse,
  ListPaymentsQuery,
  ListPaymentsResponse,
  ListTransfersRequest,
  ListTransfersResponse,
  LnInvoiceRequest,
  LnInvoiceResponse,
  NodeInfoResponse,
  RefreshRequest,
  RefreshResponse,
  RgbInvoiceRequest,
  RgbInvoiceResponse,
  RlnErrorBody,
  SendBtcRequest,
  SendBtcResponse,
  SendPaymentRequest,
  SendPaymentResponse,
  SendRgbRequest,
  SendRgbResponse,
} from './types.js';

export const DEFAULT_RLN_TIMEOUT_MS = 30_000;

/**
 * The RLN surface the gateway's LN flows and workers depend on. Structural
 * subset of RlnClient so tests can inject a plain-object fake.
 */
export type RlnApi = Pick<
  RlnClient,
  | 'address'
  | 'rgbInvoice'
  | 'sendBtc'
  | 'sendRgb'
  | 'lnInvoice'
  | 'decodeLnInvoice'
  | 'invoiceStatus'
  | 'sendPayment'
  | 'getPayment'
  | 'listPayments'
  | 'listTransfers'
  | 'refreshTransfers'
>;

/** RLN answered with a non-2xx status. Internal use only — sanitize at the boundary. */
export class RlnHttpError extends Error {
  constructor(
    readonly path: string,
    readonly status: number,
    /** Parsed RLN error body when the response was `{error, code}`-shaped. */
    readonly body: RlnErrorBody | undefined,
    /** Raw response text fallback for non-JSON error responses. */
    readonly rawBody: string,
  ) {
    super(`RLN ${path} failed with status ${status}${body !== undefined ? `: ${body.error}` : ''}`);
    this.name = 'RlnHttpError';
  }
}

/** The RLN call did not complete within the timeout. */
export class RlnTimeoutError extends Error {
  constructor(
    readonly path: string,
    readonly timeoutMs: number,
  ) {
    super(`RLN ${path} timed out after ${timeoutMs}ms`);
    this.name = 'RlnTimeoutError';
  }
}

/** The RLN call failed before an HTTP response existed (DNS, refused, reset). */
export class RlnNetworkError extends Error {
  constructor(
    readonly path: string,
    cause: unknown,
  ) {
    super(`RLN ${path} network failure`, { cause });
    this.name = 'RlnNetworkError';
  }
}

/**
 * Map any RLN client failure to a client-safe HttpError. RLN messages, status
 * codes and bodies never pass through: end users learn only that the upstream
 * call failed. The original error is attached as `cause` so the central error
 * handler can log what actually went wrong — without it, a failed withdrawal
 * left in 'ambiguous' state would have no gateway-side record at all.
 */
export function sanitizeRlnError(error: unknown): HttpError {
  if (error instanceof RlnTimeoutError) {
    return new HttpError(504, 'UPSTREAM_TIMEOUT', 'upstream node timed out', { cause: error });
  }
  return new HttpError(502, 'UPSTREAM_ERROR', 'upstream node error', { cause: error });
}

export interface RlnClientOptions {
  baseUrl: string;
  /** Biscuit admin token; empty when RLN runs with --disable-authentication. */
  adminToken: string;
  timeoutMs?: number;
  /** Injectable for tests; defaults to global fetch. */
  fetchImpl?: typeof fetch;
}

export class RlnClient {
  private readonly baseUrl: string;
  private readonly adminToken: string;
  private readonly timeoutMs: number;
  private readonly fetchImpl: typeof fetch;

  constructor(options: RlnClientOptions) {
    this.baseUrl = options.baseUrl.replace(/\/+$/, '');
    this.adminToken = options.adminToken;
    this.timeoutMs = options.timeoutMs ?? DEFAULT_RLN_TIMEOUT_MS;
    this.fetchImpl = options.fetchImpl ?? fetch;
  }

  private headers(hasBody: boolean): Record<string, string> {
    const headers: Record<string, string> = {};
    if (hasBody) headers['content-type'] = 'application/json';
    if (this.adminToken !== '') headers['authorization'] = `Bearer ${this.adminToken}`;
    return headers;
  }

  private async request<T>(method: 'GET' | 'POST', path: string, body?: unknown): Promise<T> {
    const controller = new AbortController();
    const timer = setTimeout(() => controller.abort(), this.timeoutMs);
    // The timer stays armed until the BODY is read too: a response whose body
    // stalls must not pin a queue slot forever.
    let text: string;
    let response: Response;
    try {
      try {
        response = await this.fetchImpl(`${this.baseUrl}${path}`, {
          method,
          headers: this.headers(body !== undefined),
          ...(body !== undefined ? { body: JSON.stringify(body) } : {}),
          signal: controller.signal,
        });
        text = await response.text();
      } catch (error) {
        if (controller.signal.aborted) throw new RlnTimeoutError(path, this.timeoutMs);
        throw new RlnNetworkError(path, error);
      }
    } finally {
      clearTimeout(timer);
    }

    if (!response.ok) {
      let parsed: RlnErrorBody | undefined;
      try {
        const candidate: unknown = JSON.parse(text);
        if (
          typeof candidate === 'object' &&
          candidate !== null &&
          typeof (candidate as { error?: unknown }).error === 'string' &&
          typeof (candidate as { code?: unknown }).code === 'number'
        ) {
          parsed = candidate as unknown as RlnErrorBody;
        }
      } catch {
        // non-JSON error body; keep rawBody only
      }
      throw new RlnHttpError(path, response.status, parsed, text);
    }
    return JSON.parse(text) as T;
  }

  private get<T>(path: string, query?: Record<string, string | number | undefined>): Promise<T> {
    let suffix = '';
    if (query !== undefined) {
      const params = new URLSearchParams();
      for (const [key, value] of Object.entries(query)) {
        if (value !== undefined) params.set(key, String(value));
      }
      const encoded = params.toString();
      if (encoded !== '') suffix = `?${encoded}`;
    }
    return this.request<T>('GET', `${path}${suffix}`);
  }

  private post<T>(path: string, body?: unknown): Promise<T> {
    // RLN POST handlers expect a JSON body even when all fields are optional.
    return this.request<T>('POST', path, body ?? {});
  }

  nodeInfo(): Promise<NodeInfoResponse> {
    return this.get('/nodeinfo');
  }

  address(): Promise<AddressResponse> {
    return this.post('/address');
  }

  sendBtc(request: SendBtcRequest): Promise<SendBtcResponse> {
    return this.post('/sendbtc', request);
  }

  rgbInvoice(request: RgbInvoiceRequest): Promise<RgbInvoiceResponse> {
    return this.post('/rgbinvoice', request);
  }

  sendRgb(request: SendRgbRequest): Promise<SendRgbResponse> {
    return this.post('/sendrgb', request);
  }

  listTransfers(request: ListTransfersRequest): Promise<ListTransfersResponse> {
    return this.post('/listtransfers', request);
  }

  refreshTransfers(request: RefreshRequest): Promise<RefreshResponse> {
    return this.post('/refreshtransfers', request);
  }

  lnInvoice(request: LnInvoiceRequest): Promise<LnInvoiceResponse> {
    return this.post('/lninvoice', request);
  }

  decodeLnInvoice(request: DecodeLnInvoiceRequest): Promise<DecodeLnInvoiceResponse> {
    return this.post('/decodelninvoice', request);
  }

  invoiceStatus(request: InvoiceStatusRequest): Promise<InvoiceStatusResponse> {
    return this.post('/invoicestatus', request);
  }

  sendPayment(request: SendPaymentRequest): Promise<SendPaymentResponse> {
    return this.post('/sendpayment', request);
  }

  getPayment(request: GetPaymentRequest): Promise<GetPaymentResponse> {
    return this.post('/getpayment', request);
  }

  listPayments(query: ListPaymentsQuery = {}): Promise<ListPaymentsResponse> {
    return this.get('/listpayments', query);
  }
}
