/**
 * Fastify JSON schemas for every gateway route. Response schemas are strict
 * (additionalProperties: false) so serialization strips anything undeclared —
 * a structural backstop for invariants I1/I4.
 */

export const errorBodySchema = {
  type: 'object',
  properties: {
    error: {
      type: 'object',
      properties: {
        code: { type: 'string' },
        message: { type: 'string' },
      },
      required: ['code', 'message'],
      additionalProperties: false,
    },
  },
  required: ['error'],
  additionalProperties: false,
} as const;

export const healthRouteSchema = {
  response: {
    200: {
      type: 'object',
      properties: { status: { type: 'string', enum: ['ok'] } },
      required: ['status'],
      additionalProperties: false,
    },
  },
} as const;

/**
 * Bootstrap-only user creation, guarded by the operator token. The bearer
 * token is returned exactly once here and stored only as a hash.
 */
export const createUserRouteSchema = {
  headers: {
    type: 'object',
    properties: { 'x-operator-token': { type: 'string' } },
  },
  response: {
    201: {
      type: 'object',
      properties: {
        userId: { type: 'string' },
        token: { type: 'string' },
        createdAt: { type: 'integer' },
      },
      required: ['userId', 'token', 'createdAt'],
      additionalProperties: false,
    },
    400: errorBodySchema,
    401: errorBodySchema,
  },
} as const;

export const meRouteSchema = {
  response: {
    200: {
      type: 'object',
      properties: {
        userId: { type: 'string' },
        createdAt: { type: 'integer' },
      },
      required: ['userId', 'createdAt'],
      additionalProperties: false,
    },
    401: errorBodySchema,
  },
} as const;
