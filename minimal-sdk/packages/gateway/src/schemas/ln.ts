/**
 * JSON schemas for the LN float routes. Responses are strict
 * (additionalProperties: false) so nothing undeclared — payment secrets,
 * preimages, RLN internals — can ever leak (I1/I4 backstop).
 */
import { errorBodySchema } from './index.js';

const paymentHashSchema = { type: 'string', pattern: '^[0-9a-f]{64}$' } as const;
const uuidSchema = { type: 'string', pattern: '^[0-9a-f-]{36}$' } as const;
const feeRateSchema = { type: 'integer', minimum: 1, maximum: 1000 } as const;
const paymentStatusSchema = {
  type: 'string',
  enum: ['pending', 'succeeded', 'failed'],
} as const;
const invoiceStateSchema = {
  type: 'string',
  enum: ['pending', 'settled', 'expired'],
} as const;

const commonErrorResponses = {
  400: errorBodySchema,
  401: errorBodySchema,
  409: errorBodySchema,
  502: errorBodySchema,
  504: errorBodySchema,
} as const;

export const depositPrepareRouteSchema = {
  body: {
    type: 'object',
    properties: {
      kind: { type: 'string', enum: ['btc', 'rgb'] },
      amountMsat: { type: 'integer', minimum: 1000 },
      assetId: { type: 'string', minLength: 1 },
      amount: { type: 'integer', minimum: 1 },
    },
    required: ['kind'],
    additionalProperties: false,
  },
  response: {
    201: {
      type: 'object',
      properties: {
        depositId: uuidSchema,
        kind: { type: 'string', enum: ['btc', 'rgb'] },
        address: { type: ['string', 'null'] },
        invoice: { type: ['string', 'null'] },
        recipientId: { type: ['string', 'null'] },
      },
      required: ['depositId', 'kind', 'address', 'invoice', 'recipientId'],
      additionalProperties: false,
    },
    ...commonErrorResponses,
  },
} as const;

export const payRouteSchema = {
  body: {
    type: 'object',
    properties: {
      invoice: { type: 'string', minLength: 20, maxLength: 8192 },
      amtMsat: { type: 'integer', minimum: 1 },
      assetAmount: { type: 'integer', minimum: 1 },
    },
    required: ['invoice'],
    additionalProperties: false,
  },
  response: {
    200: {
      type: 'object',
      properties: {
        paymentHash: paymentHashSchema,
        status: paymentStatusSchema,
      },
      required: ['paymentHash', 'status'],
      additionalProperties: false,
    },
    ...commonErrorResponses,
  },
} as const;

export const invoiceCreateRouteSchema = {
  body: {
    type: 'object',
    properties: {
      amtMsat: { type: 'integer', minimum: 1 },
      expirySec: { type: 'integer', minimum: 60, maximum: 86_400 },
      assetId: { type: 'string', minLength: 1 },
      assetAmount: { type: 'integer', minimum: 1 },
      description: { type: 'string', maxLength: 256 },
    },
    additionalProperties: false,
  },
  response: {
    201: {
      type: 'object',
      properties: {
        invoice: { type: 'string' },
        paymentHash: paymentHashSchema,
      },
      required: ['invoice', 'paymentHash'],
      additionalProperties: false,
    },
    ...commonErrorResponses,
  },
} as const;

export const invoiceGetRouteSchema = {
  params: {
    type: 'object',
    properties: { hash: paymentHashSchema },
    required: ['hash'],
    additionalProperties: false,
  },
  response: {
    200: {
      type: 'object',
      properties: {
        paymentHash: paymentHashSchema,
        invoice: { type: 'string' },
        state: invoiceStateSchema,
        amtMsat: { type: ['integer', 'null'] },
        assetId: { type: ['string', 'null'] },
        assetAmount: { type: ['integer', 'null'] },
        createdAt: { type: 'integer' },
      },
      required: [
        'paymentHash',
        'invoice',
        'state',
        'amtMsat',
        'assetId',
        'assetAmount',
        'createdAt',
      ],
      additionalProperties: false,
    },
    401: errorBodySchema,
    404: errorBodySchema,
    502: errorBodySchema,
    504: errorBodySchema,
  },
} as const;

export const paymentsListRouteSchema = {
  response: {
    200: {
      type: 'object',
      properties: {
        payments: {
          type: 'array',
          items: {
            type: 'object',
            properties: {
              paymentHash: paymentHashSchema,
              direction: { type: 'string', enum: ['inbound', 'outbound'] },
              status: paymentStatusSchema,
              amtMsat: { type: ['integer', 'null'] },
              assetId: { type: ['string', 'null'] },
              assetAmount: { type: ['integer', 'null'] },
              createdAt: { type: 'integer' },
              updatedAt: { type: 'integer' },
            },
            required: [
              'paymentHash',
              'direction',
              'status',
              'amtMsat',
              'assetId',
              'assetAmount',
              'createdAt',
              'updatedAt',
            ],
            additionalProperties: false,
          },
        },
      },
      required: ['payments'],
      additionalProperties: false,
    },
    401: errorBodySchema,
    502: errorBodySchema,
    504: errorBodySchema,
  },
} as const;

export const lnBalanceRouteSchema = {
  response: {
    200: {
      type: 'object',
      properties: {
        btcMsat: { type: 'integer' },
        assets: { type: 'object', additionalProperties: { type: 'integer' } },
      },
      required: ['btcMsat', 'assets'],
      additionalProperties: false,
    },
    401: errorBodySchema,
  },
} as const;

export const withdrawRouteSchema = {
  body: {
    type: 'object',
    properties: {
      kind: { type: 'string', enum: ['btc', 'rgb'] },
      address: { type: 'string', pattern: '^[a-zA-Z0-9]{14,90}$' },
      // Upper bound keeps amountSat * 1000 a safe integer: the ledger works in
      // msat and rejects unsafe amounts with a RangeError, which would surface
      // as a 500 rather than the 400 a malformed request deserves.
      amountSat: { type: 'integer', minimum: 294, maximum: 9_007_199_254_740 },
      assetId: { type: 'string', minLength: 1 },
      amount: { type: 'integer', minimum: 1 },
      recipientId: { type: 'string', minLength: 1 },
      // Sat value of the new output that pays a `wvout:` recipient; required
      // for one, rejected for a blind (`utxob:`) one. Dust bound matches the
      // on-chain send-asset route. Unlike that route — where the user's own
      // wallet funds the output — here the NODE funds it, so the gateway
      // debits it from the user's msat float; the upper bound keeps
      // witnessAmountSat * 1000 a safe integer, like amountSat above.
      witnessAmountSat: { type: 'integer', minimum: 294, maximum: 9_007_199_254_740 },
      transportEndpoints: {
        type: 'array',
        items: { type: 'string', minLength: 1 },
        minItems: 1,
        maxItems: 4,
      },
      feeRateSatPerVb: feeRateSchema,
    },
    required: ['kind'],
    additionalProperties: false,
  },
  response: {
    201: {
      type: 'object',
      properties: {
        // sha256(userId, Idempotency-Key): the withdrawal's replay guard.
        withdrawalId: { type: 'string', pattern: '^[0-9a-f]{64}$' },
        txid: { type: 'string' },
      },
      required: ['withdrawalId', 'txid'],
      additionalProperties: false,
    },
    ...commonErrorResponses,
  },
} as const;
