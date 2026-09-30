/**
 * GatewayClient: auth/idempotency headers, timeout and error mapping — all
 * via the injectable fetchFn, no real network.
 */
import { describe, expect, it } from 'vitest';
import {
  GatewayClient,
  GatewayError,
  generateIdempotencyKey,
  IntentMismatchError,
} from '../src/gateway.js';

interface SeenRequest {
  url: string;
  method: string;
  headers: Record<string, string>;
  body: string | null;
}

/** fetchFn stub that records requests and returns the queued responses. */
function stubFetch(responses: Response[]): { seen: SeenRequest[]; fetchFn: typeof fetch } {
  const seen: SeenRequest[] = [];
  const fetchFn = ((url: string | URL | Request, init?: RequestInit) => {
    seen.push({
      url: String(url),
      method: init?.method ?? 'GET',
      headers: Object.fromEntries(
        Object.entries((init?.headers ?? {}) as Record<string, string>).map(([k, v]) => [
          k.toLowerCase(),
          v,
        ]),
      ),
      body: typeof init?.body === 'string' ? init.body : null,
    });
    const response = responses.shift();
    if (response === undefined) throw new Error('stubFetch: no response queued');
    return Promise.resolve(response);
  }) as typeof fetch;
  return { seen, fetchFn };
}

function jsonResponse(status: number, body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { 'content-type': 'application/json' },
  });
}

const UUID_V4 = /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;

describe('generateIdempotencyKey', () => {
  it('produces unique RFC-4122 v4 ids', () => {
    const keys = new Set(Array.from({ length: 32 }, () => generateIdempotencyKey()));
    expect(keys.size).toBe(32);
    for (const key of keys) expect(key).toMatch(UUID_V4);
  });

  it('falls back to getRandomValues where crypto.randomUUID is missing', () => {
    // crypto.randomUUID is unavailable in non-secure browser contexts, which
    // is exactly this SDK's target; under Node the fallback never otherwise
    // runs, so its version/variant bit-twiddling would go unverified.
    const real = globalThis.crypto;
    Object.defineProperty(globalThis, 'crypto', {
      configurable: true,
      value: { getRandomValues: (array: Uint8Array) => real.getRandomValues(array) },
    });
    try {
      const keys = new Set(Array.from({ length: 32 }, () => generateIdempotencyKey()));
      expect(keys.size).toBe(32);
      for (const key of keys) expect(key).toMatch(UUID_V4);
    } finally {
      Object.defineProperty(globalThis, 'crypto', { configurable: true, value: real });
    }
  });
});

describe('GatewayClient', () => {
  it('percent-encodes caller-supplied path and query values', async () => {
    // RGB asset ids carry ':' and payment hashes come straight from callers:
    // an unencoded value could retarget the request to a different route.
    const { seen, fetchFn } = stubFetch([
      jsonResponse(200, { transfers: [] }),
      jsonResponse(200, { transfers: [] }),
      jsonResponse(200, {}),
    ]);
    const client = new GatewayClient({ baseUrl: 'http://gw.local', token: 't', fetchFn });
    await client.getTransfers('rgb:abc-123');
    await client.getTransfers();
    await client.getLnInvoice('../v1/ln/balance');
    expect(seen[0]?.url).toBe('http://gw.local/v1/wallet/transfers?assetId=rgb%3Aabc-123');
    expect(seen[1]?.url).toBe('http://gw.local/v1/wallet/transfers');
    expect(seen[2]?.url).toBe('http://gw.local/v1/ln/invoice/..%2Fv1%2Fln%2Fbalance');
  });

  it('sends the bearer token and joins paths on a trailing-slash base URL', async () => {
    const { seen, fetchFn } = stubFetch([jsonResponse(200, { address: 'bcrt1p...' })]);
    const client = new GatewayClient({ baseUrl: 'http://gw.local/', token: 'tok-1', fetchFn });
    await client.getAddress();
    expect(seen[0]?.url).toBe('http://gw.local/v1/wallet/address');
    expect(seen[0]?.headers['authorization']).toBe('Bearer tok-1');
  });

  it('refuses authenticated calls without a token, before any network IO', async () => {
    const { seen, fetchFn } = stubFetch([]);
    const client = new GatewayClient({ baseUrl: 'http://gw.local', fetchFn });
    await expect(client.getBalances()).rejects.toMatchObject({ code: 'NO_TOKEN' });
    expect(seen).toHaveLength(0);
  });

  it('createUser authenticates with the operator token only', async () => {
    const { seen, fetchFn } = stubFetch([
      jsonResponse(201, { userId: 'u1', token: 't', createdAt: 1 }),
    ]);
    const client = new GatewayClient({ baseUrl: 'http://gw.local', fetchFn });
    await client.createUser('op-secret');
    expect(seen[0]?.headers['x-operator-token']).toBe('op-secret');
    expect(seen[0]?.headers['authorization']).toBeUndefined();
  });

  it('generates a fresh idempotency key per money-moving call', async () => {
    // The prepare methods bind the returned intent to the request, so both
    // stubs have to answer with the intent the call actually asked for.
    const op = (intent: unknown) => ({ opId: 'op', psbt: 'cHNidP8=', intent, expiresAt: 0 });
    const { seen, fetchFn } = stubFetch([
      jsonResponse(
        200,
        op({
          kind: 'send_btc',
          feeRateSatPerVb: 2,
          recipients: [{ address: 'bcrt1p...', scriptHex: '5120ab', amountSat: 1_000 }],
          asset: null,
          utxos: null,
        }),
      ),
      jsonResponse(
        200,
        op({
          kind: 'create_utxos',
          feeRateSatPerVb: 2,
          recipients: [],
          asset: null,
          utxos: { upTo: false, num: 5, size: 1_000 },
        }),
      ),
    ]);
    const client = new GatewayClient({ baseUrl: 'http://gw.local', token: 't', fetchFn });
    await client.prepareSendBtc({ address: 'bcrt1p...', amountSat: 1_000 });
    await client.prepareCreateUtxos({});
    const keys = seen.map((r) => r.headers['idempotency-key']);
    expect(keys[0]).toMatch(UUID_V4);
    expect(keys[1]).toMatch(UUID_V4);
    expect(keys[0]).not.toBe(keys[1]);
    expect(seen[0]?.headers['content-type']).toBe('application/json');
  });

  it('honors a caller-pinned idempotency key (safe retry of the SAME op)', async () => {
    const { seen, fetchFn } = stubFetch([
      jsonResponse(200, { txid: 'aa' }),
      jsonResponse(200, { txid: 'aa' }),
    ]);
    const client = new GatewayClient({ baseUrl: 'http://gw.local', token: 't', fetchFn });
    const key = generateIdempotencyKey();
    await client.completeSendBtc({ opId: 'op', signedPsbt: 'cHNidP8=' }, key);
    await client.completeSendBtc({ opId: 'op', signedPsbt: 'cHNidP8=' }, key);
    expect(seen.map((r) => r.headers['idempotency-key'])).toEqual([key, key]);
  });

  it('maps a gateway error body to GatewayError with status and code', async () => {
    const { fetchFn } = stubFetch([
      jsonResponse(409, { error: { code: 'IDEMPOTENCY_CONFLICT', message: 'key reused' } }),
    ]);
    const client = new GatewayClient({ baseUrl: 'http://gw.local', token: 't', fetchFn });
    const error = await client.sync().catch((e: unknown) => e);
    expect(error).toBeInstanceOf(GatewayError);
    expect(error).toMatchObject({
      status: 409,
      code: 'IDEMPOTENCY_CONFLICT',
      message: 'key reused',
    });
  });

  it('maps a non-JSON error response to UNKNOWN', async () => {
    const { fetchFn } = stubFetch([new Response('<html>bad gateway</html>', { status: 502 })]);
    const client = new GatewayClient({ baseUrl: 'http://gw.local', token: 't', fetchFn });
    await expect(client.me()).rejects.toMatchObject({ status: 502, code: 'UNKNOWN' });
  });

  it('aborts a hung request after timeoutMs and reports TIMEOUT', async () => {
    const fetchFn = ((_url: string | URL | Request, init?: RequestInit) =>
      new Promise<Response>((_resolve, reject) => {
        init?.signal?.addEventListener('abort', () =>
          reject(new DOMException('aborted', 'AbortError')),
        );
      })) as typeof fetch;
    const client = new GatewayClient({
      baseUrl: 'http://gw.local',
      token: 't',
      timeoutMs: 25,
      fetchFn,
    });
    await expect(client.me()).rejects.toMatchObject({ code: 'TIMEOUT' });
  });

  it('maps a network failure to NETWORK_ERROR', async () => {
    const fetchFn = (() => Promise.reject(new TypeError('fetch failed'))) as typeof fetch;
    const client = new GatewayClient({ baseUrl: 'http://gw.local', token: 't', fetchFn });
    await expect(client.me()).rejects.toMatchObject({ code: 'NETWORK_ERROR' });
  });

  it('getOnchainOperation reads the op route with no idempotency key', async () => {
    const opId = '11111111-2222-4333-8444-555555555555';
    const intent = {
      kind: 'send_btc' as const,
      feeRateSatPerVb: 2,
      recipients: [],
      asset: null,
      utxos: null,
    };
    const { seen, fetchFn } = stubFetch([
      jsonResponse(200, {
        opId,
        kind: 'send_btc',
        state: 'pending',
        txid: 'd'.repeat(64),
        mayHaveBroadcast: true,
        intent,
        createdAt: 1,
        expiresAt: 2,
      }),
    ]);
    const client = new GatewayClient({ baseUrl: 'http://gw.local', token: 't', fetchFn });
    const status = await client.getOnchainOperation(opId);
    expect(seen[0]?.method).toBe('GET');
    expect(seen[0]?.url).toBe(`http://gw.local/v1/onchain/operations/${opId}`);
    // A read: polling it must never consume or require an idempotency key.
    expect(seen[0]?.headers['idempotency-key']).toBeUndefined();
    // The recovery signal after a lost complete.
    expect(status.mayHaveBroadcast).toBe(true);
    expect(status.txid).toBe('d'.repeat(64));
  });

  it('LN money-moving calls (pay, withdraw) carry an idempotency key; reads do not', async () => {
    const { seen, fetchFn } = stubFetch([
      jsonResponse(200, { paymentHash: 'a'.repeat(64), status: 'pending' }),
      jsonResponse(201, { withdrawalId: 'w', txid: 'b'.repeat(64) }),
      jsonResponse(200, { btcMsat: 0, assets: {} }),
      jsonResponse(200, { payments: [] }),
    ]);
    const client = new GatewayClient({ baseUrl: 'http://gw.local', token: 't', fetchFn });
    const key = generateIdempotencyKey();
    await client.payLnInvoice({ invoice: 'lnbcrt1...' }, key);
    await client.withdrawLn({ kind: 'btc', address: 'bcrt1q...', amountSat: 1_000 });
    await client.getLnBalance();
    await client.listLnPayments();
    expect(seen[0]?.url).toBe('http://gw.local/v1/ln/pay');
    expect(seen[0]?.headers['idempotency-key']).toBe(key);
    expect(seen[1]?.headers['idempotency-key']).toMatch(UUID_V4);
    expect(seen[2]?.headers['idempotency-key']).toBeUndefined();
    expect(seen[3]?.headers['idempotency-key']).toBeUndefined();
  });

  it('LN deposit prepare and invoice endpoints hit the expected routes', async () => {
    const hash = 'c'.repeat(64);
    const { seen, fetchFn } = stubFetch([
      jsonResponse(201, {
        depositId: 'd',
        kind: 'btc',
        address: 'bcrt1q...',
        invoice: null,
        recipientId: null,
      }),
      jsonResponse(201, { invoice: 'lnbcrt1...', paymentHash: hash }),
      jsonResponse(200, {
        paymentHash: hash,
        invoice: 'lnbcrt1...',
        state: 'pending',
        amtMsat: 1000,
        assetId: null,
        assetAmount: null,
        createdAt: 1,
      }),
    ]);
    const client = new GatewayClient({ baseUrl: 'http://gw.local', token: 't', fetchFn });
    const prepared = await client.prepareLnDeposit({ kind: 'btc', amountMsat: 1_000_000 });
    expect(prepared.address).toBe('bcrt1q...');
    await client.createLnInvoice({ amtMsat: 1000 });
    const info = await client.getLnInvoice(hash);
    expect(info.state).toBe('pending');
    expect(seen.map((r) => r.url)).toEqual([
      'http://gw.local/v1/ln/deposit/prepare',
      'http://gw.local/v1/ln/invoice',
      `http://gw.local/v1/ln/invoice/${hash}`,
    ]);
  });
});

describe('prepare responses are bound to the request', () => {
  // verify.ts check 2 matches the PSBT against the intent. A server-echoed
  // intent makes that check vacuous against a hostile gateway — it can put an
  // attacker output in the PSBT and the SAME attacker output in the intent,
  // and every one of the 5 checks passes. These bind the intent to what the
  // caller actually asked for, before it can reach verifyPsbt.
  const ADDRESS = 'bcrt1qw508d6qejxtdg4y5r3zarvary0c5xw7kygt080';
  const ATTACKER = 'bcrt1qrp33g0q5c5txsp9arysrx4k6zdkfs4nce4xj0gdcccefvpysxf3qccfmv3';

  function client(prepared: unknown): GatewayClient {
    const { fetchFn } = stubFetch([jsonResponse(200, prepared)]);
    return new GatewayClient({ baseUrl: 'http://gw.local', token: 't', fetchFn });
  }

  function sendBtcIntent(overrides: Partial<Record<string, unknown>> = {}): unknown {
    return {
      opId: 'op-1',
      psbt: 'cHNidP8=',
      expiresAt: 1,
      intent: {
        kind: 'send_btc',
        feeRateSatPerVb: 2,
        recipients: [{ address: ADDRESS, scriptHex: '0014deadbeef', amountSat: 40_000 }],
        asset: null,
        utxos: null,
        ...overrides,
      },
    };
  }

  it('accepts an intent that matches the request', async () => {
    const prepared = await client(sendBtcIntent()).prepareSendBtc({
      address: ADDRESS,
      amountSat: 40_000,
    });
    expect(prepared.opId).toBe('op-1');
  });

  it('rejects a redirected recipient address', async () => {
    const swapped = sendBtcIntent({
      recipients: [{ address: ATTACKER, scriptHex: '0014deadbeef', amountSat: 40_000 }],
    });
    await expect(
      client(swapped).prepareSendBtc({ address: ADDRESS, amountSat: 40_000 }),
    ).rejects.toThrow(IntentMismatchError);
  });

  it('rejects an inflated recipient amount', async () => {
    const inflated = sendBtcIntent({
      recipients: [{ address: ADDRESS, scriptHex: '0014deadbeef', amountSat: 4_000_000 }],
    });
    await expect(
      client(inflated).prepareSendBtc({ address: ADDRESS, amountSat: 40_000 }),
    ).rejects.toThrow(IntentMismatchError);
  });

  it('rejects an extra recipient smuggled into a send-btc intent', async () => {
    const extra = sendBtcIntent({
      recipients: [
        { address: ADDRESS, scriptHex: '0014deadbeef', amountSat: 40_000 },
        { address: ATTACKER, scriptHex: '0014cafe', amountSat: 100_000 },
      ],
    });
    await expect(
      client(extra).prepareSendBtc({ address: ADDRESS, amountSat: 40_000 }),
    ).rejects.toThrow(IntentMismatchError);
  });

  it('rejects a witness amount attached to a send-btc intent', async () => {
    // asset.witnessAmountSat is how check 2 accounts for a FOREIGN output;
    // on a plain BTC send there is no such approval to grant.
    const witness = sendBtcIntent({
      asset: {
        assetId: 'rgb:x',
        amount: 1,
        recipientId: 'wvout:y',
        witnessAmountSat: 5_000_000,
        transportEndpoints: [],
      },
    });
    await expect(
      client(witness).prepareSendBtc({ address: ADDRESS, amountSat: 40_000 }),
    ).rejects.toThrow(IntentMismatchError);
  });

  it('rejects a pinned fee rate the server did not honour', async () => {
    await expect(
      client(sendBtcIntent()).prepareSendBtc({
        address: ADDRESS,
        amountSat: 40_000,
        feeRateSatPerVb: 5,
      }),
    ).rejects.toThrow(IntentMismatchError);
  });

  it('leaves server-chosen defaults alone', async () => {
    // feeRateSatPerVb was not pinned by the caller, so the server's 2 stands.
    const prepared = await client(sendBtcIntent()).prepareSendBtc({
      address: ADDRESS,
      amountSat: 40_000,
    });
    expect(prepared.intent.feeRateSatPerVb).toBe(2);
  });

  it('rejects a send-asset intent that changes the asset, amount or recipient', async () => {
    const base = {
      opId: 'op-2',
      psbt: 'cHNidP8=',
      expiresAt: 1,
      intent: {
        kind: 'send_asset',
        feeRateSatPerVb: 2,
        recipients: [],
        asset: {
          assetId: 'rgb:good',
          amount: 10,
          recipientId: 'utxob:me',
          witnessAmountSat: null,
          transportEndpoints: ['http://proxy'],
        },
        utxos: null,
      },
    };
    const request = { assetId: 'rgb:good', amount: 10, recipientId: 'utxob:me' };
    await expect(client(base).prepareSendAsset(request)).resolves.toMatchObject({ opId: 'op-2' });

    for (const patch of [
      { assetId: 'rgb:evil' },
      { amount: 1_000 },
      { recipientId: 'utxob:attacker' },
      { witnessAmountSat: 5_000_000 },
    ]) {
      const tampered = {
        ...base,
        intent: { ...base.intent, asset: { ...base.intent.asset, ...patch } },
      };
      await expect(client(tampered).prepareSendAsset(request)).rejects.toThrow(IntentMismatchError);
    }
    const withRecipient = {
      ...base,
      intent: {
        ...base.intent,
        recipients: [{ address: ATTACKER, scriptHex: '0014cafe', amountSat: 100_000 }],
      },
    };
    await expect(client(withRecipient).prepareSendAsset(request)).rejects.toThrow(
      IntentMismatchError,
    );
  });

  it('rejects a create-utxos intent that changes a pinned shape', async () => {
    const base = {
      opId: 'op-3',
      psbt: 'cHNidP8=',
      expiresAt: 1,
      intent: {
        kind: 'create_utxos',
        feeRateSatPerVb: 2,
        recipients: [],
        asset: null,
        utxos: { upTo: false, num: 4, size: 1_000 },
      },
    };
    await expect(client(base).prepareCreateUtxos({ num: 4 })).resolves.toMatchObject({
      opId: 'op-3',
    });
    await expect(client(base).prepareCreateUtxos({ num: 8 })).rejects.toThrow(IntentMismatchError);
    await expect(client(base).prepareCreateUtxos({ size: 32_000 })).rejects.toThrow(
      IntentMismatchError,
    );
  });

  it('rejects a prepare response with no intent at all', async () => {
    await expect(
      client({ opId: 'op-4', psbt: 'cHNidP8=', expiresAt: 1 }).prepareSendBtc({
        address: ADDRESS,
        amountSat: 40_000,
      }),
    ).rejects.toThrow(IntentMismatchError);
  });

  it('rejects an intent prepared for a different flow', async () => {
    await expect(
      client(sendBtcIntent({ kind: 'create_utxos' })).prepareSendBtc({
        address: ADDRESS,
        amountSat: 40_000,
      }),
    ).rejects.toThrow(IntentMismatchError);
  });
});
