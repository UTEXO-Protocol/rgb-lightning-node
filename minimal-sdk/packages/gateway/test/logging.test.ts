import { afterEach, describe, expect, it } from 'vitest';
import type { FastifyInstance } from 'fastify';
import { OPERATOR_TOKEN, RLN_ADMIN_TOKEN, createTestUser, testServer } from './helpers.js';
import { fakeRln, rlnTimeout } from './ln-mocks.js';
import { BTC_ASSET } from '../src/ledger.js';

let app: FastifyInstance;

afterEach(async () => {
  await app.close();
});

describe('invariant I4: credentials never reach the logs', () => {
  it('user tokens, the operator token and the RLN admin token are absent from log output', async () => {
    const chunks: string[] = [];
    app = await testServer({
      loggerStream: { write: (msg: string) => void chunks.push(msg) },
    });
    const user = await createTestUser(app);
    await app.inject({
      method: 'GET',
      url: '/v1/me',
      headers: { authorization: `Bearer ${user.token}` },
    });
    // Error paths log more; make sure they do not leak headers either.
    await app.inject({
      method: 'POST',
      url: '/v1/users',
      headers: { 'x-operator-token': 'wrong-operator-token-123' },
    });
    const logs = chunks.join('');
    expect(logs.length).toBeGreaterThan(0);
    expect(logs).not.toContain(user.token);
    expect(logs).not.toContain(OPERATOR_TOKEN);
    expect(logs).not.toContain('wrong-operator-token-123');
    expect(logs).not.toContain(RLN_ADMIN_TOKEN);
    // The user id is fine to log; the token hash is not stored in logs either.
    const row = app.db.prepare('SELECT token_hash FROM users WHERE id = ?').get(user.userId) as {
      token_hash: string;
    };
    expect(logs).not.toContain(row.token_hash);
  });
});

describe('failed money-moving calls leave an operator-readable record', () => {
  it('logs the upstream cause behind an opaque client error', async () => {
    const chunks: string[] = [];
    app = await testServer({
      loggerStream: { write: (msg: string) => void chunks.push(msg) },
      rlnClient: fakeRln({
        sendBtc: () => {
          throw rlnTimeout();
        },
      }),
    });
    const user = await createTestUser(app);
    app.ledger.credit({
      userId: user.userId,
      asset: BTC_ASSET,
      amount: 1_000_000,
      kind: 'deposit',
      ref: 'd1',
    });
    const response = await app.inject({
      method: 'POST',
      url: '/v1/ln/withdraw',
      headers: { authorization: `Bearer ${user.token}`, 'idempotency-key': 'w-1' },
      payload: { kind: 'btc', address: 'bcrt1qvaultaddr000000', amountSat: 300 },
    });
    // The client learns nothing about the node...
    expect(response.statusCode).toBe(504);
    expect(JSON.stringify(response.json())).not.toContain('/sendbtc');
    // ...but the withdrawal is left 'ambiguous', so the operator must be able
    // to see what RLN actually said.
    const withdrawal = app.db.prepare('SELECT state FROM withdrawals').get() as { state: string };
    expect(withdrawal.state).toBe('ambiguous');
    const logs = chunks.join('');
    expect(logs).toContain('RlnTimeoutError');
    expect(logs).toContain('UPSTREAM_TIMEOUT');
    expect(logs).not.toContain(RLN_ADMIN_TOKEN);
    expect(logs).not.toContain(user.token);
  });
});
