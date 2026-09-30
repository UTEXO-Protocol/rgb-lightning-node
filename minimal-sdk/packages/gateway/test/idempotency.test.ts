import { afterEach, describe, expect, it } from 'vitest';
import type { FastifyInstance } from 'fastify';
import type { GatewayConfig } from '../src/config.js';
import { IN_FLIGHT_RECLAIM_MS, RETENTION_MS, requestHash } from '../src/idempotency.js';
import { createTestUser, deferred, sleep, testServer, type TestUser } from './helpers.js';

let app: FastifyInstance;

afterEach(async () => {
  await app.close();
});

interface PayFixture {
  app: FastifyInstance;
  user: TestUser;
  calls: () => number;
  gate: () => Promise<void> | undefined;
  setGate: (p: Promise<void>) => void;
  failNext: () => void;
}

/**
 * Registers a fake money-moving route wired exactly like the real ones will
 * be: auth preHandler → idempotency preHandler → per-user FIFO enqueue,
 * with the idempotency onSend hook capturing the response.
 */
async function payFixture(configOverrides: Partial<GatewayConfig> = {}): Promise<PayFixture> {
  const server = await testServer({ configOverrides });
  let calls = 0;
  let gate: Promise<void> | undefined;
  let shouldFail = false;
  server.post(
    '/v1/test/pay',
    {
      schema: {
        body: {
          type: 'object',
          properties: { amountMsat: { type: 'integer' }, memo: { type: 'string' } },
          required: ['amountMsat'],
          additionalProperties: false,
        },
        response: {
          200: {
            type: 'object',
            properties: { paid: { type: 'boolean' }, execution: { type: 'integer' } },
            required: ['paid', 'execution'],
            additionalProperties: false,
          },
        },
      },
      preHandler: [server.authenticate, server.idempotency.preHandler],
      onSend: server.idempotency.onSend,
    },
    async (request) =>
      server.queues.enqueue(request.userId as string, async () => {
        if (shouldFail) {
          shouldFail = false;
          throw new Error('downstream exploded');
        }
        calls += 1;
        if (gate !== undefined) await gate;
        return { paid: true, execution: calls };
      }),
  );
  await server.ready();
  const user = await createTestUser(server);
  return {
    app: server,
    user,
    calls: () => calls,
    gate: () => gate,
    setGate: (p) => (gate = p),
    failNext: () => (shouldFail = true),
  };
}

function pay(
  fixture: PayFixture,
  options: { key?: string; amountMsat?: number; token?: string } = {},
) {
  const headers: Record<string, string> = {
    authorization: `Bearer ${options.token ?? fixture.user.token}`,
  };
  if (options.key !== undefined) headers['idempotency-key'] = options.key;
  return fixture.app.inject({
    method: 'POST',
    url: '/v1/test/pay',
    headers,
    payload: { amountMsat: options.amountMsat ?? 1000 },
  });
}

describe('idempotency middleware', () => {
  it('requires the Idempotency-Key header', async () => {
    const fixture = await payFixture();
    app = fixture.app;
    const response = await pay(fixture);
    expect(response.statusCode).toBe(400);
    expect(response.json().error.code).toBe('IDEMPOTENCY_KEY_REQUIRED');
    expect(fixture.calls()).toBe(0);
  });

  it('replays the cached response for the same key and request', async () => {
    const fixture = await payFixture();
    app = fixture.app;
    const first = await pay(fixture, { key: 'k1' });
    expect(first.statusCode).toBe(200);
    expect(first.json()).toEqual({ paid: true, execution: 1 });
    const replay = await pay(fixture, { key: 'k1' });
    expect(replay.statusCode).toBe(200);
    expect(replay.json()).toEqual({ paid: true, execution: 1 });
    expect(replay.headers['x-idempotent-replay']).toBe('true');
    expect(fixture.calls()).toBe(1);
  });

  it('returns 409 for the same key with a different request', async () => {
    const fixture = await payFixture();
    app = fixture.app;
    await pay(fixture, { key: 'k1', amountMsat: 1000 });
    const conflict = await pay(fixture, { key: 'k1', amountMsat: 2000 });
    expect(conflict.statusCode).toBe(409);
    expect(conflict.json().error.code).toBe('IDEMPOTENCY_KEY_REUSED');
    expect(fixture.calls()).toBe(1);
  });

  it('scopes keys per user: two users may use the same key', async () => {
    const fixture = await payFixture();
    app = fixture.app;
    const other = await createTestUser(fixture.app);
    const first = await pay(fixture, { key: 'shared' });
    const second = await pay(fixture, { key: 'shared', token: other.token });
    expect(first.statusCode).toBe(200);
    expect(second.statusCode).toBe(200);
    expect(fixture.calls()).toBe(2);
  });

  it('rejects a concurrent duplicate while the first request is in flight', async () => {
    const fixture = await payFixture();
    app = fixture.app;
    const barrier = deferred<void>();
    fixture.setGate(barrier.promise);
    const firstPromise = pay(fixture, { key: 'k1' });
    // Wait until the first request has claimed the key and entered the handler.
    while (fixture.calls() === 0) await sleep(2);
    const concurrent = await pay(fixture, { key: 'k1' });
    expect(concurrent.statusCode).toBe(409);
    expect(concurrent.json().error.code).toBe('IDEMPOTENCY_IN_FLIGHT');
    barrier.resolve();
    const first = await firstPromise;
    expect(first.statusCode).toBe(200);
    expect(fixture.calls()).toBe(1);
  });

  it('does not cache 5xx responses: a retry re-executes', async () => {
    const fixture = await payFixture();
    app = fixture.app;
    fixture.failNext();
    const failed = await pay(fixture, { key: 'k1' });
    expect(failed.statusCode).toBe(500);
    expect(failed.json().error.code).toBe('INTERNAL');
    const retried = await pay(fixture, { key: 'k1' });
    expect(retried.statusCode).toBe(200);
    expect(retried.json()).toEqual({ paid: true, execution: 1 });
  });

  it('returns 429 with Retry-After when the per-user queue is full (and does not burn the key)', async () => {
    const fixture = await payFixture({ queuePerUserDepth: 1, queueGlobalConcurrency: 1 });
    app = fixture.app;
    const barrier = deferred<void>();
    fixture.setGate(barrier.promise);
    const firstPromise = pay(fixture, { key: 'k1' });
    while (fixture.calls() === 0) await sleep(2);
    const overflow = await pay(fixture, { key: 'k2' });
    expect(overflow.statusCode).toBe(429);
    expect(overflow.headers['retry-after']).toBeDefined();
    expect(overflow.json().error.code).toBe('QUEUE_FULL');
    barrier.resolve();
    await firstPromise;
    // The rejected key was released, so a later retry succeeds.
    const retried = await pay(fixture, { key: 'k2' });
    expect(retried.statusCode).toBe(200);
  });

  it('rejects a key longer than 128 characters', async () => {
    const fixture = await payFixture();
    app = fixture.app;
    const response = await pay(fixture, { key: 'k'.repeat(129) });
    expect(response.statusCode).toBe(400);
    expect(response.json().error.code).toBe('IDEMPOTENCY_KEY_REQUIRED');
    expect(fixture.calls()).toBe(0);
  });

  it('replays when the same body arrives with reordered JSON keys', async () => {
    const fixture = await payFixture();
    app = fixture.app;
    const inject = (payload: string) =>
      fixture.app.inject({
        method: 'POST',
        url: '/v1/test/pay',
        headers: {
          authorization: `Bearer ${fixture.user.token}`,
          'idempotency-key': 'k1',
          'content-type': 'application/json',
        },
        payload,
      });
    const first = await inject('{"amountMsat":1000,"memo":"m"}');
    expect(first.statusCode).toBe(200);
    // Same request, different key order: stableStringify canonicalizes, so
    // this is a replay — not an IDEMPOTENCY_KEY_REUSED conflict.
    const replay = await inject('{"memo":"m","amountMsat":1000}');
    expect(replay.statusCode).toBe(200);
    expect(replay.headers['x-idempotent-replay']).toBe('true');
    expect(replay.json()).toEqual(first.json());
    expect(fixture.calls()).toBe(1);
  });

  it('reclaims a stale in-flight claim but keeps a fresh one locked', async () => {
    const fixture = await payFixture();
    app = fixture.app;
    const insert = app.db.prepare(
      'INSERT INTO idempotency (user_id, key, request_hash, response, created_at) VALUES (?, ?, ?, NULL, ?)',
    );
    // Crash leftover for THIS request: response never stored, older than the
    // reclaim window. Retrying the same request must go through.
    const hash = requestHash('POST', '/v1/test/pay', { amountMsat: 1000 });
    insert.run(fixture.user.userId, 'k-stale', hash, Date.now() - IN_FLIGHT_RECLAIM_MS - 1_000);
    const reclaimed = await pay(fixture, { key: 'k-stale' });
    expect(reclaimed.statusCode).toBe(200);
    expect(fixture.calls()).toBe(1);

    // A claim younger than the window with a matching request hash is still
    // treated as in flight.
    insert.run(fixture.user.userId, 'k-young', hash, Date.now() - 1_000);
    const blocked = await pay(fixture, { key: 'k-young' });
    expect(blocked.statusCode).toBe(409);
    expect(blocked.json().error.code).toBe('IDEMPOTENCY_IN_FLIGHT');
    expect(fixture.calls()).toBe(1);
  });

  it('never reclaims a stale claim for a DIFFERENT request', async () => {
    const fixture = await payFixture();
    app = fixture.app;
    // Staleness is a crash-recovery allowance for retrying the same request,
    // not a licence to repurpose a key: a different body must still conflict,
    // however old the abandoned claim is.
    app.db
      .prepare(
        'INSERT INTO idempotency (user_id, key, request_hash, response, created_at) VALUES (?, ?, ?, NULL, ?)',
      )
      .run(
        fixture.user.userId,
        'k-stale-other',
        requestHash('POST', '/v1/test/pay', { amountMsat: 999_999 }),
        Date.now() - IN_FLIGHT_RECLAIM_MS - 1_000,
      );
    const conflict = await pay(fixture, { key: 'k-stale-other' });
    expect(conflict.statusCode).toBe(409);
    expect(conflict.json().error.code).toBe('IDEMPOTENCY_KEY_REUSED');
    expect(fixture.calls()).toBe(0);
  });

  it('caches Buffer payloads and replays them intact', async () => {
    app = await testServer();
    app.post(
      '/v1/test/binary',
      {
        preHandler: [app.authenticate, app.idempotency.preHandler],
        onSend: app.idempotency.onSend,
      },
      async (_request, reply) => reply.type('application/octet-stream').send(Buffer.from('blob')),
    );
    const user = await createTestUser(app);
    const inject = () =>
      app.inject({
        method: 'POST',
        url: '/v1/test/binary',
        headers: { authorization: `Bearer ${user.token}`, 'idempotency-key': 'kb' },
      });
    const first = await inject();
    expect(first.statusCode).toBe(200);
    expect(first.body).toBe('blob');
    const replay = await inject();
    expect(replay.headers['x-idempotent-replay']).toBe('true');
    expect(replay.body).toBe('blob');
  });

  it('releases the claim (fails loud, not corrupt) for an uncacheable stream payload', async () => {
    app = await testServer();
    const { Readable } = await import('node:stream');
    let executions = 0;
    app.post(
      '/v1/test/stream',
      {
        preHandler: [app.authenticate, app.idempotency.preHandler],
        onSend: app.idempotency.onSend,
      },
      async (_request, reply) => {
        executions += 1;
        return reply.type('application/octet-stream').send(Readable.from(['chunk']));
      },
    );
    const user = await createTestUser(app);
    const inject = () =>
      app.inject({
        method: 'POST',
        url: '/v1/test/stream',
        headers: { authorization: `Bearer ${user.token}`, 'idempotency-key': 'ks' },
      });
    const first = await inject();
    expect(first.statusCode).toBe(200);
    // No cached row: the claim was released rather than storing an empty body.
    expect(
      app.db.prepare('SELECT response FROM idempotency WHERE key = ?').get('ks'),
    ).toBeUndefined();
    // A retry re-executes instead of replaying a corrupt empty response.
    await inject();
    expect(executions).toBe(2);
  });

  it('purges completed rows past retention when a new key is claimed', async () => {
    const fixture = await payFixture();
    app = fixture.app;
    app.db
      .prepare(
        'INSERT INTO idempotency (user_id, key, request_hash, response, created_at) VALUES (?, ?, ?, ?, ?)',
      )
      .run(
        fixture.user.userId,
        'k-old',
        'h',
        '{"statusCode":200,"contentType":null,"body":""}',
        Date.now() - RETENTION_MS - 1_000,
      );
    const response = await pay(fixture, { key: 'k-new' });
    expect(response.statusCode).toBe(200);
    expect(
      app.db.prepare('SELECT key FROM idempotency WHERE key = ?').get('k-old'),
    ).toBeUndefined();
    // The fresh row itself survives.
    expect(app.db.prepare('SELECT key FROM idempotency WHERE key = ?').get('k-new')).toBeDefined();
  });
});
