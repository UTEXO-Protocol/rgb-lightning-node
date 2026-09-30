/**
 * Friendly aliases over the generated RLN OpenAPI types (./openapi.ts) for the
 * endpoint subset the gateway uses. Regenerate the underlying file with
 * `pnpm --filter @utexo/minimal-gateway generate:rln-types`.
 */
import type { components, paths } from './openapi.js';

export type RlnComponents = components;
export type RlnPaths = paths;

/** Error body every RLN endpoint returns on failure (src/error.rs APIErrorResponse). */
export interface RlnErrorBody {
  error: string;
  code: number;
  /** APIError variant name (e.g. 'PaymentNotFound') — the only reliable discriminator. */
  name?: string;
}

export type NodeInfoResponse = components['schemas']['NodeInfoResponse'];
export type AddressResponse = components['schemas']['AddressResponse'];
export type BtcBalanceRequest = components['schemas']['BtcBalanceRequest'];
export type BtcBalanceResponse = components['schemas']['BtcBalanceResponse'];
export type SendBtcRequest = components['schemas']['SendBtcRequest'];
export type SendBtcResponse = components['schemas']['SendBtcResponse'];
export type CreateUtxosRequest = components['schemas']['CreateUtxosRequest'];
export type EmptyResponse = components['schemas']['EmptyResponse'];
export type RgbInvoiceRequest = components['schemas']['RgbInvoiceRequest'];
export type RgbInvoiceResponse = components['schemas']['RgbInvoiceResponse'];
export type DecodeRgbInvoiceRequest = components['schemas']['DecodeRGBInvoiceRequest'];
export type DecodeRgbInvoiceResponse = components['schemas']['DecodeRGBInvoiceResponse'];
export type SendRgbRequest = components['schemas']['SendRgbRequest'];
export type SendRgbResponse = components['schemas']['SendRgbResponse'];
export type ListTransfersRequest = components['schemas']['ListTransfersRequest'];
export type ListTransfersResponse = components['schemas']['ListTransfersResponse'];
export type Transfer = components['schemas']['Transfer'];
export type RefreshRequest = components['schemas']['RefreshRequest'];
export type RefreshResponse = components['schemas']['RefreshResponse'];
export type LnInvoiceRequest = components['schemas']['LNInvoiceRequest'];
export type LnInvoiceResponse = components['schemas']['LNInvoiceResponse'];
export type DecodeLnInvoiceRequest = components['schemas']['DecodeLNInvoiceRequest'];
export type DecodeLnInvoiceResponse = components['schemas']['DecodeLNInvoiceResponse'];
export type InvoiceStatusRequest = components['schemas']['InvoiceStatusRequest'];
export type InvoiceStatusResponse = components['schemas']['InvoiceStatusResponse'];
export type SendPaymentRequest = components['schemas']['SendPaymentRequest'];
export type SendPaymentResponse = components['schemas']['SendPaymentResponse'];
export type GetPaymentRequest = components['schemas']['GetPaymentRequest'];
export type GetPaymentResponse = components['schemas']['GetPaymentResponse'];
export type Payment = components['schemas']['Payment'];
export type HtlcStatus = components['schemas']['HTLCStatus'];
export type LnInvoiceStatus = components['schemas']['InvoiceStatus'];
export type ListPaymentsResponse = components['schemas']['ListPaymentsResponse'];
export type ListPaymentsQuery = NonNullable<paths['/listpayments']['get']['parameters']['query']>;
export type ListChannelsResponse = components['schemas']['ListChannelsResponse'];
export type Channel = components['schemas']['Channel'];
