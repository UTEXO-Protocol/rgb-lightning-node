/**
 * Opaque per-user bearer tokens, hashed at rest (invariant I4: only hashes are
 * persisted; the plaintext token is returned exactly once at creation).
 *
 * Tokens carry 256 bits of entropy, so a single unsalted SHA-256 is a safe
 * at-rest representation and gives O(1) indexed lookup.
 */
import { createHash, randomBytes, timingSafeEqual } from 'node:crypto';
import type { FastifyReply, FastifyRequest } from 'fastify';
import type { GatewayDb } from './db.js';
import { HttpError } from './errors.js';

export interface CreatedUser {
  userId: string;
  token: string;
  createdAt: number;
}

export interface UserRecord {
  id: string;
  createdAt: number;
}

export function generateToken(): string {
  return `utxg_${randomBytes(32).toString('base64url')}`;
}

export function hashToken(token: string): string {
  return createHash('sha256').update(token, 'utf8').digest('hex');
}

/** Constant-time string comparison that does not leak length. */
export function safeEqual(a: string, b: string): boolean {
  const ha = createHash('sha256').update(a, 'utf8').digest();
  const hb = createHash('sha256').update(b, 'utf8').digest();
  return timingSafeEqual(ha, hb);
}

export function createUser(db: GatewayDb, now: number = Date.now()): CreatedUser {
  const userId = `u_${randomBytes(16).toString('hex')}`;
  const token = generateToken();
  db.prepare('INSERT INTO users (id, token_hash, created_at) VALUES (?, ?, ?)').run(
    userId,
    hashToken(token),
    now,
  );
  return { userId, token, createdAt: now };
}

export function findUserByToken(db: GatewayDb, token: string): UserRecord | undefined {
  const row = db
    .prepare('SELECT id, created_at FROM users WHERE token_hash = ?')
    .get(hashToken(token)) as { id: string; created_at: number } | undefined;
  return row === undefined ? undefined : { id: row.id, createdAt: row.created_at };
}

export type PreHandler = (request: FastifyRequest, reply: FastifyReply) => Promise<void>;

/** Resolves the authenticated user on every non-bootstrap route. */
export function makeAuthHook(db: GatewayDb): PreHandler {
  return async (request: FastifyRequest, _reply: FastifyReply): Promise<void> => {
    const header = request.headers.authorization;
    if (typeof header !== 'string' || !header.startsWith('Bearer ')) {
      throw new HttpError(401, 'UNAUTHORIZED', 'missing bearer token');
    }
    const token = header.slice('Bearer '.length).trim();
    const user = token === '' ? undefined : findUserByToken(db, token);
    if (user === undefined) {
      throw new HttpError(401, 'UNAUTHORIZED', 'invalid token');
    }
    request.userId = user.id;
  };
}

/** Guards the bootstrap-only user-creation route with the operator token. */
export function makeOperatorGuard(operatorToken: string): PreHandler {
  return async (request: FastifyRequest, _reply: FastifyReply): Promise<void> => {
    const provided = request.headers['x-operator-token'];
    if (typeof provided !== 'string' || !safeEqual(provided, operatorToken)) {
      throw new HttpError(401, 'OPERATOR_UNAUTHORIZED', 'missing or invalid operator token');
    }
  };
}
