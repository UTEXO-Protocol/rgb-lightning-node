/**
 * Test fakes for the LN flows: a programmable RlnApi where every method not
 * explicitly provided fails loudly, plus error constructors matching the real
 * client's failure modes.
 */
import type { RlnApi } from '../src/rln/client.js';
import { RlnHttpError, RlnNetworkError, RlnTimeoutError } from '../src/rln/client.js';

export type RlnOverrides = {
  [K in keyof RlnApi]?: (...args: Parameters<RlnApi[K]>) => unknown;
};

export interface FakeRln extends RlnApi {
  /** Method-name log of every call, in order. */
  calls: string[];
}

const RLN_METHODS: ReadonlyArray<keyof RlnApi> = [
  'address',
  'rgbInvoice',
  'sendBtc',
  'sendRgb',
  'lnInvoice',
  'decodeLnInvoice',
  'invoiceStatus',
  'sendPayment',
  'getPayment',
  'listPayments',
  'listTransfers',
  'refreshTransfers',
];

export function fakeRln(overrides: RlnOverrides): FakeRln {
  const calls: string[] = [];
  const fake = { calls } as unknown as FakeRln;
  for (const method of RLN_METHODS) {
    const impl = overrides[method];
    (fake as unknown as Record<string, unknown>)[method] = async (...args: unknown[]) => {
      calls.push(method);
      if (impl === undefined) {
        throw new Error(`unexpected RLN call: ${method}(${JSON.stringify(args)})`);
      }
      return (impl as (...a: unknown[]) => unknown)(...args);
    };
  }
  return fake;
}

export function rlnRejection(status = 400): RlnHttpError {
  return new RlnHttpError('/test', status, { error: 'rejected by node', code: status }, '');
}

/** RLN's actual "payment does not exist" answer (403 + name discriminator). */
export function rlnPaymentNotFound(): RlnHttpError {
  return new RlnHttpError(
    '/getpayment',
    403,
    { error: 'Payment not found', code: 403, name: 'PaymentNotFound' },
    '',
  );
}

export function rlnTimeout(): RlnTimeoutError {
  return new RlnTimeoutError('/test', 30_000);
}

export function rlnNetworkFailure(): RlnNetworkError {
  return new RlnNetworkError('/test', new Error('connection refused'));
}

/** A syntactically plausible regtest BOLT11 string (fakes only decode it via RLN). */
export const TEST_INVOICE =
  'lnbcrt30u1pjv6yzntestinvoice00000000000000000000000000000000000000000000';

export const HASH_A = 'a'.repeat(64);
export const HASH_B = 'b'.repeat(64);

export function decodedInvoice(
  paymentHash: string,
  amtMsat: number | null,
  asset?: { assetId: string; amount: number | null },
) {
  return {
    amt_msat: amtMsat,
    expiry_sec: 3600,
    timestamp: 1_700_000_000,
    asset_id: asset?.assetId ?? null,
    asset_amount: asset?.amount ?? null,
    payment_hash: paymentHash,
    payment_secret: 'f'.repeat(64),
    payee_pubkey: null,
    min_final_cltv_expiry_delta: 144,
    network: 'Regtest' as const,
  };
}

/**
 * RLN reports payment timestamps in unix SECONDS from its own clock, and the
 * gateway's /listpayments walk uses them as an age floor against resource_map
 * rows stamped with Date.now(). Mock payments therefore default to "now", not
 * a frozen constant, or every fixture would look older than any owned resource.
 */
export function nowSec(): number {
  return Math.floor(Date.now() / 1000);
}

export function outboundPayment(
  paymentHash: string,
  status: string,
  amtMsat: number | null,
  createdAtSec: number = nowSec(),
) {
  return {
    amt_msat: amtMsat,
    asset_amount: null,
    asset_id: null,
    payment_hash: paymentHash,
    payment_type: 'Outbound' as const,
    status,
    created_at: createdAtSec,
    updated_at: createdAtSec + 100,
    payee_pubkey: '02' + '1'.repeat(64),
  };
}

export function inboundPayment(
  paymentHash: string,
  status: string,
  amtMsat: number | null,
  createdAtSec: number = nowSec(),
) {
  return {
    ...outboundPayment(paymentHash, status, amtMsat, createdAtSec),
    payment_type: 'InboundAutoClaim',
  };
}
