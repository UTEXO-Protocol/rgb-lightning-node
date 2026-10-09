/**
 * JSON schemas for the on-chain prepare/complete routes. Strict responses
 * (additionalProperties: false) so nothing undeclared can leak (I1 backstop).
 */
import { errorBodySchema } from './index.js';

/** Base64 PSBT; the body limit (64KB) bounds overall size. */
const psbtSchema = { type: 'string', pattern: '^[A-Za-z0-9+/=]+$', minLength: 20 } as const;
const opIdSchema = { type: 'string', pattern: '^[0-9a-f-]{36}$' } as const;
const feeRateSchema = { type: 'integer', minimum: 1, maximum: 1000 } as const;

const intentSchema = {
  type: 'object',
  properties: {
    kind: { type: 'string', enum: ['send_btc', 'send_asset', 'create_utxos'] },
    feeRateSatPerVb: { type: 'integer' },
    recipients: {
      type: 'array',
      items: {
        type: 'object',
        properties: {
          address: { type: 'string' },
          scriptHex: { type: 'string' },
          amountSat: { type: 'integer' },
        },
        required: ['address', 'scriptHex', 'amountSat'],
        additionalProperties: false,
      },
    },
    asset: {
      type: ['object', 'null'],
      properties: {
        assetId: { type: 'string' },
        amount: { type: 'integer' },
        recipientId: { type: 'string' },
        witnessAmountSat: { type: ['integer', 'null'] },
        transportEndpoints: { type: 'array', items: { type: 'string' } },
      },
      required: ['assetId', 'amount', 'recipientId', 'witnessAmountSat', 'transportEndpoints'],
      additionalProperties: false,
    },
    utxos: {
      type: ['object', 'null'],
      properties: {
        upTo: { type: 'boolean' },
        num: { type: 'integer' },
        size: { type: 'integer' },
      },
      required: ['upTo', 'num', 'size'],
      additionalProperties: false,
    },
  },
  required: ['kind', 'feeRateSatPerVb', 'recipients', 'asset', 'utxos'],
  additionalProperties: false,
} as const;

const preparedResponseSchema = {
  type: 'object',
  properties: {
    opId: opIdSchema,
    psbt: { type: 'string' },
    intent: intentSchema,
    expiresAt: { type: 'integer' },
  },
  required: ['opId', 'psbt', 'intent', 'expiresAt'],
  additionalProperties: false,
} as const;

const completeBodySchema = {
  type: 'object',
  properties: {
    opId: opIdSchema,
    signedPsbt: psbtSchema,
  },
  required: ['opId', 'signedPsbt'],
  additionalProperties: false,
} as const;

const prepareErrorResponses = {
  400: errorBodySchema,
  401: errorBodySchema,
  404: errorBodySchema,
  409: errorBodySchema,
} as const;

const completeErrorResponses = {
  400: errorBodySchema,
  401: errorBodySchema,
  404: errorBodySchema,
  409: errorBodySchema,
  410: errorBodySchema,
} as const;

/**
 * Read-only operation status. Strict like every other response here: the PSBT
 * is deliberately NOT echoed — the client already holds it from `prepare`, and
 * a 64KB blob has no place in a recovery poll.
 */
export const operationGetRouteSchema = {
  params: {
    type: 'object',
    properties: { opId: opIdSchema },
    required: ['opId'],
    additionalProperties: false,
  },
  response: {
    200: {
      type: 'object',
      properties: {
        opId: opIdSchema,
        kind: { type: 'string', enum: ['send_btc', 'send_asset', 'create_utxos'] },
        state: { type: 'string', enum: ['pending', 'completed', 'expired'] },
        txid: { type: ['string', 'null'] },
        mayHaveBroadcast: { type: 'boolean' },
        intent: intentSchema,
        createdAt: { type: 'integer' },
        expiresAt: { type: 'integer' },
      },
      required: [
        'opId',
        'kind',
        'state',
        'txid',
        'mayHaveBroadcast',
        'intent',
        'createdAt',
        'expiresAt',
      ],
      additionalProperties: false,
    },
    401: errorBodySchema,
    404: errorBodySchema,
  },
} as const;

/**
 * Fee estimate from the shared indexer. Clients cannot reach esplora directly,
 * so without this they have no fee source; the answer is a whole sat/vB in the
 * same 1..1000 range every fee-taking route accepts, so it can be passed
 * straight back as `feeRateSatPerVb`.
 */
export const feeEstimateRouteSchema = {
  querystring: {
    type: 'object',
    properties: {
      // 1008 is the widest target esplora publishes.
      blocks: { type: 'integer', minimum: 1, maximum: 1008 },
    },
    additionalProperties: false,
  },
  response: {
    200: {
      type: 'object',
      properties: {
        blocks: { type: 'integer' },
        feeRateSatPerVb: feeRateSchema,
        /** Esplora ladder target the estimate was read from (≤ `blocks`). */
        sourceBlocks: { type: 'integer' },
      },
      required: ['blocks', 'feeRateSatPerVb', 'sourceBlocks'],
      additionalProperties: false,
    },
    400: errorBodySchema,
    401: errorBodySchema,
    502: errorBodySchema,
    504: errorBodySchema,
  },
} as const;

export const sendBtcPrepareRouteSchema = {
  body: {
    type: 'object',
    properties: {
      address: { type: 'string', pattern: '^[a-zA-Z0-9]{14,90}$' },
      amountSat: { type: 'integer', minimum: 294 },
      feeRateSatPerVb: feeRateSchema,
    },
    required: ['address', 'amountSat'],
    additionalProperties: false,
  },
  response: { 201: preparedResponseSchema, ...prepareErrorResponses },
} as const;

export const sendBtcCompleteRouteSchema = {
  body: completeBodySchema,
  response: {
    200: {
      type: 'object',
      properties: { txid: { type: 'string' } },
      required: ['txid'],
      additionalProperties: false,
    },
    ...completeErrorResponses,
  },
} as const;

export const sendAssetPrepareRouteSchema = {
  body: {
    type: 'object',
    properties: {
      assetId: { type: 'string', minLength: 1 },
      // Integer, not number: an RGB fungible amount is a u64 on the rgb-lib
      // side, and a fractional one fails serde deserialization there — a 500
      // for what is really a malformed request.
      amount: { type: 'integer', minimum: 1 },
      recipientId: { type: 'string', minLength: 1 },
      witnessAmountSat: { type: 'integer', minimum: 294 },
      transportEndpoints: {
        type: 'array',
        items: { type: 'string', minLength: 1 },
        minItems: 1,
        maxItems: 4,
      },
      donation: { type: 'boolean' },
      minConfirmations: { type: 'integer', minimum: 0, maximum: 100 },
      feeRateSatPerVb: feeRateSchema,
    },
    required: ['assetId', 'amount', 'recipientId'],
    additionalProperties: false,
  },
  response: { 201: preparedResponseSchema, ...prepareErrorResponses },
} as const;

export const sendAssetCompleteRouteSchema = {
  body: completeBodySchema,
  response: {
    200: {
      type: 'object',
      properties: {
        txid: { type: 'string' },
        /**
         * rgb-lib's batch transfer index for the transfer this created. refresh,
         * fail and delete address a transfer by this, not by its txid, so a
         * client cannot manage what it just sent without it.
         */
        batchTransferIdx: { type: ['integer', 'null'] },
      },
      required: ['txid', 'batchTransferIdx'],
      additionalProperties: false,
    },
    ...completeErrorResponses,
  },
} as const;

export const createUtxosPrepareRouteSchema = {
  body: {
    type: 'object',
    properties: {
      num: { type: 'integer', minimum: 1, maximum: 50 },
      size: { type: 'integer', minimum: 294 },
      upTo: { type: 'boolean' },
      feeRateSatPerVb: feeRateSchema,
    },
    additionalProperties: false,
  },
  response: { 201: preparedResponseSchema, ...prepareErrorResponses },
} as const;

export const createUtxosCompleteRouteSchema = {
  body: completeBodySchema,
  response: {
    200: {
      type: 'object',
      properties: {
        txid: { type: ['string', 'null'] },
        utxosCreated: { type: 'integer' },
      },
      required: ['txid', 'utxosCreated'],
      additionalProperties: false,
    },
    ...completeErrorResponses,
  },
} as const;
