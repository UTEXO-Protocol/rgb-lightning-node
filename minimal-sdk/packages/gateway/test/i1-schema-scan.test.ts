import { afterEach, describe, expect, it } from 'vitest';
import type { FastifyInstance } from 'fastify';
import { findKeyMaterialFields, findKeyMaterialInValue } from '../src/schemas/scan.js';
import { testServer } from './helpers.js';

let app: FastifyInstance | undefined;

// Only the first test builds a server; the pure-function tests below must not
// trip over an unassigned (or already-closed) instance.
afterEach(async () => {
  await app?.close();
  app = undefined;
});

describe('invariant I1: no key material in the gateway API surface', () => {
  it('no registered route schema declares a key-material field', async () => {
    app = await testServer();
    await app.ready();
    expect(app.routeSchemas.length).toBeGreaterThan(0);
    for (const entry of app.routeSchemas) {
      const violations = findKeyMaterialFields(entry.schema);
      expect.soft(violations, `route ${JSON.stringify(entry.method)} ${entry.url}`).toEqual([]);
    }
  });

  it('the scanner itself catches key-material fields (self-check)', () => {
    const poisoned = {
      body: {
        type: 'object',
        properties: {
          mnemonic: { type: 'string' },
          nested: { type: 'object', properties: { privateKey: { type: 'string' } } },
        },
        required: ['mnemonic'],
      },
      response: {
        200: {
          type: 'object',
          properties: { signing_key: { type: 'string' } },
        },
      },
    };
    const violations = findKeyMaterialFields(poisoned);
    expect(violations).toContain('$.body.properties.mnemonic');
    expect(violations).toContain('$.body.properties.nested.properties.privateKey');
    expect(violations).toContain('$.body.required[mnemonic]');
    expect(violations).toContain('$.response.200.properties.signing_key');
  });

  it('the runtime value scanner catches key material in live payloads', () => {
    expect(findKeyMaterialInValue({ ok: true, data: [{ seed: 'abc' }] })).toEqual([
      '$.data[0].seed',
    ]);
    expect(findKeyMaterialInValue({ userId: 'u_1', xpub: 'tpub...', amountMsat: 5 })).toEqual([]);
  });
});
