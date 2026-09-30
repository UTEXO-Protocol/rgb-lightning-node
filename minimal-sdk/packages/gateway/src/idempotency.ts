/**
 * Idempotency-key middleware for money-moving routes (Block 3 item 2).
 *
 * Contract per route that declares it:
 *  - `Idempotency-Key` header is required (400 without it);
 *  - first request claims the (user, key) row, executes, and caches the final
 *    response (status < 500 and not 429 — retryable outcomes are not cached);
 *  - replay with the same key + same request hash returns the cached response;
 *  - same key + different request hash returns 409 IDEMPOTENCY_KEY_REUSED;
 *  - a concurrent duplicate while the first is in flight returns 409
 *    IDEMPOTENCY_IN_FLIGHT (never a second execution);
 *  - an in-flight claim older than IN_FLIGHT_RECLAIM_MS is treated as a crash
 *    leftover (the response was never stored) and is reclaimed by the next
 *    replay OF THE SAME REQUEST, so a crashed request does not brick its key
 *    forever while a different request under that key still gets 409. Resource
 *    guards (resource_map, pending_ops) still prevent double execution;
 *  - completed rows older than RETENTION_MS are garbage-collected lazily.
 */
import { createHash } from 'node:crypto';
import type { FastifyReply, FastifyRequest } from 'fastify';
import type { GatewayDb } from './db.js';
import { HttpError } from './errors.js';

export interface IdempotencyClaim {
  key: string;
  requestHash: string;
  fresh: boolean;
}

export interface IdempotencyHooks {
  preHandler: (request: FastifyRequest, reply: FastifyReply) => Promise<void>;
  onSend: (request: FastifyRequest, reply: FastifyReply, payload: unknown) => Promise<unknown>;
}

function stableStringify(value: unknown): string {
  if (value === null || typeof value !== 'object') return JSON.stringify(value) ?? 'null';
  if (Array.isArray(value)) return `[${value.map(stableStringify).join(',')}]`;
  const entries = Object.entries(value as Record<string, unknown>)
    .filter(([, v]) => v !== undefined)
    .sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0))
    .map(([k, v]) => `${JSON.stringify(k)}:${stableStringify(v)}`);
  return `{${entries.join(',')}}`;
}

export function requestHash(method: string, url: string, body: unknown): string {
  return createHash('sha256')
    .update(method)
    .update('\n')
    .update(url)
    .update('\n')
    .update(stableStringify(body ?? null))
    .digest('hex');
}

interface IdempotencyRow {
  request_hash: string;
  response: string | null;
  created_at: number;
}

function isStale(createdAt: number, now: number): boolean {
  return now - createdAt >= IN_FLIGHT_RECLAIM_MS;
}

interface CachedResponse {
  statusCode: number;
  contentType: string | null;
  body: string;
}

/** In-flight claims older than this are crash leftovers and may be reclaimed. */
export const IN_FLIGHT_RECLAIM_MS = 15 * 60_000;
/** Completed rows older than this are garbage-collected. */
export const RETENTION_MS = 7 * 24 * 60 * 60_000;

export function makeIdempotencyHooks(db: GatewayDb): IdempotencyHooks {
  const insert = db.prepare(
    'INSERT OR IGNORE INTO idempotency (user_id, key, request_hash, response, created_at) VALUES (?, ?, ?, NULL, ?)',
  );
  const select = db.prepare(
    'SELECT request_hash, response, created_at FROM idempotency WHERE user_id = ? AND key = ?',
  );
  const store = db.prepare('UPDATE idempotency SET response = ? WHERE user_id = ? AND key = ?');
  const release = db.prepare('DELETE FROM idempotency WHERE user_id = ? AND key = ?');
  const reclaim = db.prepare(
    `UPDATE idempotency SET created_at = ?
     WHERE user_id = ? AND key = ? AND request_hash = ? AND response IS NULL AND created_at <= ?`,
  );
  const purge = db.prepare(
    'DELETE FROM idempotency WHERE response IS NOT NULL AND created_at <= ?',
  );

  const preHandler = async (request: FastifyRequest, reply: FastifyReply): Promise<void> => {
    const userId = request.userId;
    if (userId === undefined) {
      throw new HttpError(500, 'INTERNAL', 'idempotency requires an authenticated route');
    }
    const key = request.headers['idempotency-key'];
    if (typeof key !== 'string' || key.length === 0 || key.length > 128) {
      throw new HttpError(400, 'IDEMPOTENCY_KEY_REQUIRED', 'Idempotency-Key header is required');
    }
    const now = Date.now();
    const hash = requestHash(request.method, request.routeOptions.url ?? request.url, request.body);
    const claimed = insert.run(userId, key, hash, now).changes === 1;
    if (claimed) {
      purge.run(now - RETENTION_MS);
      request.idempotency = { key, requestHash: hash, fresh: true };
      return;
    }
    const row = select.get(userId, key) as IdempotencyRow | undefined;
    if (row === undefined) {
      throw new HttpError(500, 'INTERNAL', 'idempotency claim vanished');
    }
    // A different request under the same key is key reuse, full stop — the
    // reclaim path below is for retrying the SAME request after a crash, not
    // for repurposing a key.
    if (row.request_hash !== hash) {
      throw new HttpError(
        409,
        'IDEMPOTENCY_KEY_REUSED',
        'Idempotency-Key was already used with a different request',
      );
    }
    if (row.response === null) {
      // Reclaim a crash leftover atomically; if another request beat us to it
      // (created_at moved), fall through to IN_FLIGHT.
      if (
        isStale(row.created_at, now) &&
        reclaim.run(now, userId, key, hash, now - IN_FLIGHT_RECLAIM_MS).changes === 1
      ) {
        request.idempotency = { key, requestHash: hash, fresh: true };
        return;
      }
      throw new HttpError(
        409,
        'IDEMPOTENCY_IN_FLIGHT',
        'a request with this Idempotency-Key is still in flight',
      );
    }
    const cached = JSON.parse(row.response) as CachedResponse;
    reply.header('x-idempotent-replay', 'true');
    if (cached.contentType !== null) reply.type(cached.contentType);
    await reply.code(cached.statusCode).send(cached.body);
  };

  const onSend = async (
    request: FastifyRequest,
    reply: FastifyReply,
    payload: unknown,
  ): Promise<unknown> => {
    const claim = request.idempotency;
    if (claim === undefined || !claim.fresh) return payload;
    const retryable = reply.statusCode >= 500 || reply.statusCode === 429;
    if (retryable) {
      release.run(request.userId, claim.key);
      return payload;
    }
    // Fastify serializes JSON routes before onSend, so payload is a string
    // (or Buffer). A stream cannot be cached: release the claim loudly rather
    // than store an empty body that a replay would serve as the "response".
    const body =
      typeof payload === 'string'
        ? payload
        : Buffer.isBuffer(payload)
          ? payload.toString('utf8')
          : null;
    if (body === null) {
      request.log.error(
        { idempotencyKey: claim.key },
        'idempotency: uncacheable response payload type; claim released',
      );
      release.run(request.userId, claim.key);
      return payload;
    }
    const cached: CachedResponse = {
      statusCode: reply.statusCode,
      contentType: (reply.getHeader('content-type') as string | undefined) ?? null,
      body,
    };
    store.run(JSON.stringify(cached), request.userId, claim.key);
    return payload;
  };

  return { preHandler, onSend };
}
