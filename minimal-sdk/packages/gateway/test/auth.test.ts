import { createHash } from 'node:crypto';
import { afterEach, describe, expect, it } from 'vitest';
import type { FastifyInstance } from 'fastify';
import { OPERATOR_TOKEN, createTestUser, testServer } from './helpers.js';

let app: FastifyInstance;

afterEach(async () => {
  await app.close();
});

describe('user bootstrap (POST /v1/users)', () => {
  it('rejects a missing operator token', async () => {
    app = await testServer();
    const response = await app.inject({ method: 'POST', url: '/v1/users' });
    expect(response.statusCode).toBe(401);
    expect(response.json().error.code).toBe('OPERATOR_UNAUTHORIZED');
  });

  it('rejects a wrong operator token', async () => {
    app = await testServer();
    const response = await app.inject({
      method: 'POST',
      url: '/v1/users',
      headers: { 'x-operator-token': 'wrong-operator-token-123' },
    });
    expect(response.statusCode).toBe(401);
  });

  it('creates a user and stores only the token hash', async () => {
    app = await testServer();
    const user = await createTestUser(app);
    expect(user.userId).toMatch(/^u_/);
    expect(user.token).toMatch(/^utxg_/);
    const row = app.db.prepare('SELECT token_hash FROM users WHERE id = ?').get(user.userId) as {
      token_hash: string;
    };
    expect(row.token_hash).not.toBe(user.token);
    expect(row.token_hash).toBe(createHash('sha256').update(user.token).digest('hex'));
  });
});

describe('bearer auth (GET /v1/me)', () => {
  it('accepts a valid token and resolves the right user', async () => {
    app = await testServer();
    const user = await createTestUser(app);
    const response = await app.inject({
      method: 'GET',
      url: '/v1/me',
      headers: { authorization: `Bearer ${user.token}` },
    });
    expect(response.statusCode).toBe(200);
    expect(response.json()).toEqual({ userId: user.userId, createdAt: user.createdAt });
  });

  it('rejects missing, malformed and unknown tokens', async () => {
    app = await testServer();
    await createTestUser(app);
    for (const headers of [
      {},
      { authorization: 'Bearer ' },
      { authorization: 'Basic abc' },
      { authorization: 'Bearer utxg_definitely-not-a-real-token' },
      { authorization: `Bearer ${OPERATOR_TOKEN}` },
    ]) {
      const response = await app.inject({ method: 'GET', url: '/v1/me', headers });
      expect(response.statusCode).toBe(401);
      expect(response.json().error.code).toBe('UNAUTHORIZED');
    }
  });

  it('never resolves one user from another user token', async () => {
    app = await testServer();
    const alice = await createTestUser(app);
    const bob = await createTestUser(app);
    expect(alice.token).not.toBe(bob.token);
    const asBob = await app.inject({
      method: 'GET',
      url: '/v1/me',
      headers: { authorization: `Bearer ${bob.token}` },
    });
    expect(asBob.json().userId).toBe(bob.userId);
    expect(asBob.json().userId).not.toBe(alice.userId);
  });
});
