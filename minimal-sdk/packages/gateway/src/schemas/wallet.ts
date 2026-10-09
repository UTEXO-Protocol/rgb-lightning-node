/**
 * JSON schemas for the wallet routes. Strict responses
 * (additionalProperties: false) strip anything undeclared — nothing from the
 * rgb-lib layer reaches a client unless declared here (I1 backstop).
 */
import { errorBodySchema } from './index.js';

const balanceSchema = {
  type: 'object',
  properties: {
    settled: { type: 'number' },
    future: { type: 'number' },
    spendable: { type: 'number' },
  },
  required: ['settled', 'future', 'spendable'],
  additionalProperties: false,
} as const;

/** Base58 account xpub (tpub/xpub/vpub...); length bounds, not full checksum. */
const xpubPattern = '^[a-zA-Z0-9]{100,120}$';

/**
 * An rgb-lib Assignment as a variant name plus its fungible value. `kind`
 * carries the variant verbatim, so a new rgb-lib assignment variant reaches a
 * client as itself rather than as a silently dropped field.
 */
const assignmentSchema = {
  type: 'object',
  properties: {
    kind: { type: 'string' },
    amount: { type: ['integer', 'null'] },
  },
  required: ['kind', 'amount'],
  additionalProperties: false,
} as const;

export const registerXpubsRouteSchema = {
  body: {
    type: 'object',
    properties: {
      vanilla: { type: 'string', pattern: xpubPattern },
      colored: { type: 'string', pattern: xpubPattern },
      fingerprint: { type: 'string', pattern: '^[0-9a-f]{8}$' },
    },
    required: ['vanilla', 'colored', 'fingerprint'],
    additionalProperties: false,
  },
  response: {
    201: {
      type: 'object',
      properties: {
        fingerprint: { type: 'string' },
        address: { type: 'string' },
      },
      required: ['fingerprint', 'address'],
      additionalProperties: false,
    },
    400: errorBodySchema,
    401: errorBodySchema,
    409: errorBodySchema,
  },
} as const;

export const walletAddressRouteSchema = {
  response: {
    200: {
      type: 'object',
      properties: {
        address: { type: 'string' },
        /**
         * Derivation the address came from, so a client can re-derive it from
         * its own seed before trusting it as a deposit target — the gateway
         * holds the xpubs and could otherwise return any address. Null when it
         * could not be attributed (see wallets/derivation.ts): the address is
         * still rgb-lib's, it just carries no proof.
         */
        derivation: {
          type: ['object', 'null'],
          properties: {
            account: { type: 'string', enum: ['vanilla'] },
            keychain: { type: 'integer' },
            index: { type: 'integer' },
            derivationPath: { type: 'string' },
            scriptHex: { type: 'string' },
          },
          required: ['account', 'keychain', 'index', 'derivationPath', 'scriptHex'],
          additionalProperties: false,
        },
      },
      required: ['address', 'derivation'],
      additionalProperties: false,
    },
    401: errorBodySchema,
    404: errorBodySchema,
    502: errorBodySchema,
  },
} as const;

export const walletBalancesRouteSchema = {
  response: {
    200: {
      type: 'object',
      properties: {
        btc: {
          type: 'object',
          properties: { vanilla: balanceSchema, colored: balanceSchema },
          required: ['vanilla', 'colored'],
          additionalProperties: false,
        },
        assets: {
          type: 'array',
          items: {
            type: 'object',
            properties: {
              assetId: { type: 'string' },
              schema: { type: 'string' },
              ticker: { type: ['string', 'null'] },
              name: { type: 'string' },
              details: { type: ['string', 'null'] },
              precision: { type: 'integer' },
              /** Total issued amount; null for schemas without one (UDA). */
              issuedSupply: { type: ['integer', 'null'] },
              /** Unix seconds of asset genesis. */
              timestamp: { type: 'integer' },
              /** Unix seconds this wallet imported the asset. */
              addedAt: { type: 'integer' },
              balance: balanceSchema,
            },
            required: [
              'assetId',
              'schema',
              'ticker',
              'name',
              'details',
              'precision',
              'issuedSupply',
              'timestamp',
              'addedAt',
              'balance',
            ],
            additionalProperties: false,
          },
        },
      },
      required: ['btc', 'assets'],
      additionalProperties: false,
    },
    401: errorBodySchema,
    404: errorBodySchema,
    502: errorBodySchema,
  },
} as const;

export const walletUnspentsRouteSchema = {
  response: {
    200: {
      type: 'object',
      properties: {
        unspents: {
          type: 'array',
          items: {
            type: 'object',
            properties: {
              txid: { type: 'string' },
              vout: { type: 'integer' },
              amountSat: { type: 'number' },
              colorable: { type: 'boolean' },
              /** True once the transaction creating this UTXO has been broadcast. */
              exists: { type: 'boolean' },
              /**
               * Blind receives already promised against this UTXO. A client
               * counting free allocation slots cannot derive this from
               * `allocations`: a pending blind receive reserves a slot before it
               * holds an allocation.
               */
              pendingBlinded: { type: 'integer' },
              allocations: {
                type: 'array',
                items: {
                  type: 'object',
                  properties: {
                    assetId: { type: ['string', 'null'] },
                    amount: { type: ['number', 'null'] },
                    assignment: assignmentSchema,
                    settled: { type: 'boolean' },
                  },
                  required: ['assetId', 'amount', 'assignment', 'settled'],
                  additionalProperties: false,
                },
              },
            },
            required: [
              'txid',
              'vout',
              'amountSat',
              'colorable',
              'exists',
              'pendingBlinded',
              'allocations',
            ],
            additionalProperties: false,
          },
        },
      },
      required: ['unspents'],
      additionalProperties: false,
    },
    401: errorBodySchema,
    404: errorBodySchema,
    502: errorBodySchema,
  },
} as const;

export const walletTransfersRouteSchema = {
  querystring: {
    type: 'object',
    properties: { assetId: { type: 'string', minLength: 1 } },
    additionalProperties: false,
  },
  response: {
    200: {
      type: 'object',
      properties: {
        transfers: {
          type: 'array',
          items: {
            type: 'object',
            properties: {
              idx: { type: 'integer' },
              /**
               * Batch this transfer belongs to. rgb-lib keys refresh, fail and
               * delete by the BATCH index, not by `idx`, so a client that only
               * sees `idx` cannot act on the transfer it just read.
               */
              batchTransferIdx: { type: ['integer', 'null'] },
              assetId: { type: ['string', 'null'] },
              amount: { type: ['number', 'null'] },
              /** Full assignment list; `amount` is only its fungible summary. */
              assignments: { type: 'array', items: assignmentSchema },
              kind: { type: 'string' },
              status: { type: 'string' },
              txid: { type: ['string', 'null'] },
              recipientId: { type: ['string', 'null'] },
              expiration: { type: ['number', 'null'] },
              createdAt: { type: 'number' },
              updatedAt: { type: 'number' },
            },
            required: [
              'idx',
              'batchTransferIdx',
              'assetId',
              'amount',
              'assignments',
              'kind',
              'status',
              'txid',
              'recipientId',
              'expiration',
              'createdAt',
              'updatedAt',
            ],
            additionalProperties: false,
          },
        },
      },
      required: ['transfers'],
      additionalProperties: false,
    },
    401: errorBodySchema,
    404: errorBodySchema,
    502: errorBodySchema,
  },
} as const;

export const walletReceiveRouteSchema = {
  body: {
    type: 'object',
    properties: {
      mode: { type: 'string', enum: ['blind', 'witness'] },
      assetId: { type: 'string', minLength: 1 },
      // Integer for the same reason as sendAssetPrepareRouteSchema: rgb-lib
      // deserializes a fungible assignment into a u64.
      amount: { type: 'integer', minimum: 1 },
      durationSeconds: { type: 'integer', minimum: 60, maximum: 2_592_000 },
      minConfirmations: { type: 'integer', minimum: 0, maximum: 100 },
    },
    required: ['mode'],
    additionalProperties: false,
  },
  response: {
    201: {
      type: 'object',
      properties: {
        invoice: { type: 'string' },
        recipientId: { type: 'string' },
        expirationTimestamp: { type: ['number', 'null'] },
        /**
         * Batch index of the created receive; rgb-lib keys refresh, fail and
         * delete by it, so without this a client cannot manage the receive it
         * just asked for. Null only on rgb-lib shape drift.
         */
        batchTransferIdx: { type: ['integer', 'null'] },
        mode: { type: 'string', enum: ['blind', 'witness'] },
      },
      required: ['invoice', 'recipientId', 'expirationTimestamp', 'batchTransferIdx', 'mode'],
      additionalProperties: false,
    },
    400: errorBodySchema,
    401: errorBodySchema,
    404: errorBodySchema,
    502: errorBodySchema,
  },
} as const;

export const walletSyncRouteSchema = {
  response: {
    200: {
      type: 'object',
      properties: { status: { type: 'string', enum: ['ok'] } },
      required: ['status'],
      additionalProperties: false,
    },
    401: errorBodySchema,
    404: errorBodySchema,
  },
} as const;
