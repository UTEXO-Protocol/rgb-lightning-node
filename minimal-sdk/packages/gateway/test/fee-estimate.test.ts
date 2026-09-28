/**
 * Fee-estimate route and estimator.
 *
 * Clients cannot reach esplora — it sits behind the gateway with RLN and the RGB
 * proxy — so without this route a client either hardcodes a fee rate or sends
 * none and takes the gateway's 2 sat/vB default. Two roundings are deliberate
 * and tested here: up to a whole sat/vB (never below the estimate the caller
 * asked for), and down the esplora target ladder to the nearest published target
 * at or below the request (never cheaper than the target asked for).
 */
import type { FastifyInstance } from 'fastify';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { CACHE_TTL_MS, FeeEstimator } from '../src/fees.js';
import { HttpError } from '../src/errors.js';
import { createTestUser, testServer, type TestUser } from './helpers.js';

/** The ladder esplora actually publishes, trimmed to the interesting rungs. */
const LADDER = {
  '1': 24.61,
  '2': 18.3,
  '3': 12.0,
  '6': 5.42,
  '10': 3.1,
  '144': 1.9,
  '504': 1.2,
  '1008': 1.0,
};

function esploraStub(
  body: unknown,
  init: { status?: number; json?: () => Promise<unknown> } = {},
): { fetchImpl: typeof fetch; calls: string[] } {
  const calls: string[] = [];
  const fetchImpl = (async (input: string | URL) => {
    calls.push(String(input));
    return {
      ok: (init.status ?? 200) < 400,
      status: init.status ?? 200,
      json: init.json ?? (async () => body),
    } as unknown as Response;
  }) as unknown as typeof fetch;
  return { fetchImpl, calls };
}

describe('FeeEstimator', () => {
  it('rounds a fractional rate UP to a whole sat/vB', async () => {
    const { fetchImpl } = esploraStub(LADDER);
    const estimator = new FeeEstimator({ esploraUrl: 'http://esplora', fetchImpl });
    // 5.42 -> 6, not 5: a client signing at exactly this rate must not land
    // below the estimate it asked for.
    expect(await estimator.estimate(6)).toEqual({
      blocks: 6,
      feeRateSatPerVb: 6,
      sourceBlocks: 6,
    });
  });

  it('falls back to the nearest published target at or below the request', async () => {
    const { fetchImpl } = esploraStub(LADDER);
    const estimator = new FeeEstimator({ esploraUrl: 'http://esplora', fetchImpl });
    // 5 is not published; 3 is, and it is the more urgent (so never cheaper)
    // neighbour. Answering with the 6-block rate would under-pay for 5 blocks.
    expect(await estimator.estimate(5)).toEqual({
      blocks: 5,
      feeRateSatPerVb: 12,
      sourceBlocks: 3,
    });
  });

  it('clamps into the range every fee-taking route accepts', async () => {
    const { fetchImpl } = esploraStub({ '1': 4000.5, '1008': 0.2 });
    const estimator = new FeeEstimator({ esploraUrl: 'http://esplora', fetchImpl });
    expect((await estimator.estimate(1)).feeRateSatPerVb).toBe(1000);
    // 0.2 rounds up to 1, which is also the floor — a 0 would be rejected by
    // every route this value is meant to be passed to.
    expect((await estimator.estimate(1008)).feeRateSatPerVb).toBe(1);
  });

  it('answers a target below the whole ladder with the fastest published rung', async () => {
    const { fetchImpl } = esploraStub({ '144': 1.9, '1008': 1.0 });
    const estimator = new FeeEstimator({ esploraUrl: 'http://esplora', fetchImpl });
    expect(await estimator.estimate(6)).toEqual({
      blocks: 6,
      feeRateSatPerVb: 2,
      sourceBlocks: 144,
    });
  });

  it('ignores unusable rungs', async () => {
    const { fetchImpl } = esploraStub({
      '1': 'not a number',
      '2': 0,
      '3': -1,
      '4': null,
      'not-a-target': 5,
      '0': 99,
      '2016': 99,
      '6': 5.0,
    });
    const estimator = new FeeEstimator({ esploraUrl: 'http://esplora', fetchImpl });
    expect(await estimator.estimate(6)).toEqual({
      blocks: 6,
      feeRateSatPerVb: 5,
      sourceBlocks: 6,
    });
  });

  it('caches for the TTL so a burst of clients is one indexer call', async () => {
    const { fetchImpl, calls } = esploraStub(LADDER);
    let now = 1_000_000;
    const estimator = new FeeEstimator({
      esploraUrl: 'http://esplora/',
      fetchImpl,
      now: () => now,
    });
    await estimator.estimate(6);
    await estimator.estimate(1);
    expect(calls).toEqual(['http://esplora/fee-estimates']);
    now += CACHE_TTL_MS;
    await estimator.estimate(6);
    expect(calls).toHaveLength(2);
  });

  it('reports an indexer failure as 502, not as a made-up rate', async () => {
    for (const stub of [
      esploraStub(undefined, { status: 503 }),
      esploraStub([1, 2, 3]),
      esploraStub({}),
      esploraStub(undefined, {
        json: async () => {
          throw new Error('malformed body');
        },
      }),
    ]) {
      const estimator = new FeeEstimator({
        esploraUrl: 'http://esplora',
        fetchImpl: stub.fetchImpl,
      });
      await expect(estimator.estimate(6)).rejects.toThrow(HttpError);
      await expect(estimator.estimate(6)).rejects.toMatchObject({ statusCode: 502 });
    }
  });

  it('does not cache a failure', async () => {
    let attempt = 0;
    const fetchImpl = (async () => {
      attempt += 1;
      if (attempt === 1) return { ok: false, status: 503 } as unknown as Response;
      return { ok: true, status: 200, json: async () => LADDER } as unknown as Response;
    }) as unknown as typeof fetch;
    const estimator = new FeeEstimator({ esploraUrl: 'http://esplora', fetchImpl });
    await expect(estimator.estimate(6)).rejects.toThrow(HttpError);
    expect((await estimator.estimate(6)).feeRateSatPerVb).toBe(6);
  });

  it('times out rather than holding the caller open on a stalled indexer', async () => {
    const fetchImpl = (async (_input: string | URL, init?: { signal?: AbortSignal }) =>
      new Promise<Response>((_resolve, reject) => {
        init?.signal?.addEventListener('abort', () => reject(new Error('aborted')));
      })) as unknown as typeof fetch;
    const estimator = new FeeEstimator({ esploraUrl: 'http://esplora', fetchImpl, timeoutMs: 5 });
    await expect(estimator.estimate(6)).rejects.toMatchObject({
      statusCode: 504,
      code: 'UPSTREAM_TIMEOUT',
    });
  });
});

describe('GET /v1/onchain/fee-estimate', () => {
  let app: FastifyInstance;
  let user: TestUser;

  beforeEach(async () => {
    const { fetchImpl } = esploraStub(LADDER);
    app = await testServer({ esploraFetch: fetchImpl });
    user = await createTestUser(app);
  });

  afterEach(async () => {
    await app.close();
  });

  function authed() {
    return { authorization: `Bearer ${user.token}` };
  }

  it('requires authentication', async () => {
    const response = await app.inject({ method: 'GET', url: '/v1/onchain/fee-estimate' });
    expect(response.statusCode).toBe(401);
  });

  it('answers a whole sat/vB a client can pass straight back as feeRateSatPerVb', async () => {
    const response = await app.inject({
      method: 'GET',
      url: '/v1/onchain/fee-estimate?blocks=6',
      headers: authed(),
    });
    expect(response.statusCode).toBe(200);
    expect(response.json()).toEqual({ blocks: 6, feeRateSatPerVb: 6, sourceBlocks: 6 });
  });

  it('defaults to a six-block target', async () => {
    const response = await app.inject({
      method: 'GET',
      url: '/v1/onchain/fee-estimate',
      headers: authed(),
    });
    expect(response.json()).toEqual({ blocks: 6, feeRateSatPerVb: 6, sourceBlocks: 6 });
  });

  it('rejects a target outside the esplora ladder', async () => {
    for (const blocks of ['0', '1009', 'six', '1.5']) {
      const response = await app.inject({
        method: 'GET',
        url: `/v1/onchain/fee-estimate?blocks=${blocks}`,
        headers: authed(),
      });
      expect(response.statusCode).toBe(400);
      expect(response.json().error.code).toBe('BAD_REQUEST');
    }
  });

  it('needs no wallet registration and no Idempotency-Key', async () => {
    // A client needs a fee rate BEFORE it can prepare anything, so this must not
    // depend on wallet state or on the per-user wallet queue.
    const response = await app.inject({
      method: 'GET',
      url: '/v1/onchain/fee-estimate?blocks=1',
      headers: authed(),
    });
    expect(response.statusCode).toBe(200);
    expect(response.json().feeRateSatPerVb).toBe(25);
  });

  it('keeps the indexer failure detail server-side', async () => {
    const { fetchImpl } = esploraStub(undefined, { status: 503 });
    const failing = await testServer({ esploraFetch: fetchImpl });
    const failingUser = await createTestUser(failing);
    const response = await failing.inject({
      method: 'GET',
      url: '/v1/onchain/fee-estimate',
      headers: { authorization: `Bearer ${failingUser.token}` },
    });
    expect(response.statusCode).toBe(502);
    expect(response.json()).toEqual({
      error: { code: 'FEE_ESTIMATE_UNAVAILABLE', message: expect.any(String) },
    });
    expect(response.body).not.toContain('503');
    await failing.close();
  });
});

describe('x-request-id', () => {
  let app: FastifyInstance;

  beforeEach(async () => {
    app = await testServer();
  });

  afterEach(async () => {
    await app.close();
  });

  it('is present on success and on errors, and is a fresh random id', async () => {
    // Client-facing error bodies carry only code and message (I4); the failure
    // they were mapped from is logged under this id and nowhere else, so without
    // the header a client holding a 502 has nothing an operator can grep for.
    const seen = new Set<string>();
    for (const url of ['/v1/health', '/v1/me']) {
      const response = await app.inject({ method: 'GET', url });
      const id = response.headers['x-request-id'];
      expect(typeof id).toBe('string');
      expect(id).toMatch(/^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/);
      seen.add(id as string);
    }
    expect(seen.size).toBe(2);
  });

  it('never adopts a caller-supplied id', async () => {
    // Otherwise a client could choose the value that ties together every log
    // line for its own requests.
    const response = await app.inject({
      method: 'GET',
      url: '/v1/health',
      headers: { 'request-id': 'attacker-chosen', 'x-request-id': 'attacker-chosen' },
    });
    expect(response.headers['x-request-id']).not.toBe('attacker-chosen');
  });

  it('is set on an idempotency replay too', async () => {
    const user = await createTestUser(app);
    const headers = {
      authorization: `Bearer ${user.token}`,
      'idempotency-key': 'replayed-key',
    };
    const payload = { address: 'bcrt1qw508d6qejxtdg4y5r3zarvary0c5xw7kygt080', amountSat: 1000 };
    const first = await app.inject({
      method: 'POST',
      url: '/v1/onchain/send-btc/prepare',
      headers,
      payload,
    });
    const replay = await app.inject({
      method: 'POST',
      url: '/v1/onchain/send-btc/prepare',
      headers,
      payload,
    });
    expect(replay.statusCode).toBe(first.statusCode);
    expect(replay.headers['x-idempotent-replay']).toBe('true');
    expect(typeof replay.headers['x-request-id']).toBe('string');
    expect(replay.headers['x-request-id']).not.toBe(first.headers['x-request-id']);
  });
});
