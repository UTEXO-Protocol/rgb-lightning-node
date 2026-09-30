import { createServer, type IncomingMessage, type Server, type ServerResponse } from 'node:http';
import type { AddressInfo } from 'node:net';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import {
  RlnClient,
  RlnHttpError,
  RlnNetworkError,
  RlnTimeoutError,
  sanitizeRlnError,
} from '../src/rln/client.js';
import { HttpError } from '../src/errors.js';

interface SeenRequest {
  method: string;
  url: string;
  headers: IncomingMessage['headers'];
  body: string;
}

type Handler = (req: SeenRequest, res: ServerResponse) => void;

const ADMIN_TOKEN = 'test-rln-admin-token-abcdef';

describe('RlnClient', () => {
  let server: Server;
  let baseUrl: string;
  let seen: SeenRequest[];
  let handler: Handler;
  const pendingTimers: NodeJS.Timeout[] = [];

  beforeEach(async () => {
    seen = [];
    handler = (_req, res) => {
      res.writeHead(200, { 'content-type': 'application/json' });
      res.end('{}');
    };
    server = createServer((req, res) => {
      let body = '';
      req.on('data', (chunk: Buffer) => (body += chunk.toString()));
      req.on('end', () => {
        const request: SeenRequest = {
          method: req.method ?? '',
          url: req.url ?? '',
          headers: req.headers,
          body,
        };
        seen.push(request);
        res.on('error', () => {});
        handler(request, res);
      });
    });
    await new Promise<void>((resolve) => server.listen(0, '127.0.0.1', resolve));
    baseUrl = `http://127.0.0.1:${(server.address() as AddressInfo).port}`;
  });

  afterEach(async () => {
    for (const timer of pendingTimers) clearTimeout(timer);
    pendingTimers.length = 0;
    server.closeAllConnections();
    await new Promise<void>((resolve) => server.close(() => resolve()));
  });

  function client(overrides: { adminToken?: string; timeoutMs?: number } = {}): RlnClient {
    return new RlnClient({
      baseUrl,
      adminToken: overrides.adminToken ?? ADMIN_TOKEN,
      ...(overrides.timeoutMs !== undefined ? { timeoutMs: overrides.timeoutMs } : {}),
    });
  }

  it('sends the admin token as a bearer header and parses the response', async () => {
    handler = (_req, res) => {
      res.writeHead(200, { 'content-type': 'application/json' });
      res.end(JSON.stringify({ pubkey: 'abc', num_channels: 0 }));
    };
    const info = await client().nodeInfo();
    expect(info.pubkey).toBe('abc');
    expect(seen[0]?.method).toBe('GET');
    expect(seen[0]?.url).toBe('/nodeinfo');
    expect(seen[0]?.headers['authorization']).toBe(`Bearer ${ADMIN_TOKEN}`);
  });

  it('omits the authorization header when the admin token is empty', async () => {
    await client({ adminToken: '' }).nodeInfo();
    expect(seen[0]?.headers['authorization']).toBeUndefined();
  });

  it('POSTs a JSON body with content-type', async () => {
    handler = (_req, res) => {
      res.writeHead(200, { 'content-type': 'application/json' });
      res.end(JSON.stringify({ payment_hash: 'aa', status: 'Pending' }));
    };
    await client().sendPayment({ invoice: 'lnbcrt1...' });
    expect(seen[0]?.method).toBe('POST');
    expect(seen[0]?.url).toBe('/sendpayment');
    expect(seen[0]?.headers['content-type']).toBe('application/json');
    expect(JSON.parse(seen[0]?.body ?? '')).toEqual({ invoice: 'lnbcrt1...' });
  });

  it('sends an empty JSON object for body-less POST endpoints', async () => {
    handler = (_req, res) => {
      res.writeHead(200, { 'content-type': 'application/json' });
      res.end(JSON.stringify({ address: 'bcrt1q...' }));
    };
    await client().address();
    expect(seen[0]?.body).toBe('{}');
  });

  it('serializes listpayments query parameters and skips undefined ones', async () => {
    handler = (_req, res) => {
      res.writeHead(200, { 'content-type': 'application/json' });
      res.end(JSON.stringify({ payments: [], first_index_offset: 0, last_index_offset: 0 }));
    };
    await client().listPayments({ index_offset: 42, status: 'Succeeded' });
    expect(seen[0]?.url).toBe('/listpayments?index_offset=42&status=Succeeded');
    await client().listPayments();
    expect(seen[1]?.url).toBe('/listpayments');
  });

  it('times out slow calls with RlnTimeoutError', async () => {
    handler = (_req, res) => {
      const timer = setTimeout(() => {
        res.writeHead(200, { 'content-type': 'application/json' });
        res.end('{}');
      }, 5_000);
      pendingTimers.push(timer);
    };
    const error = await client({ timeoutMs: 50 })
      .nodeInfo()
      .then(
        () => undefined,
        (e: unknown) => e,
      );
    expect(error).toBeInstanceOf(RlnTimeoutError);
    expect((error as RlnTimeoutError).path).toBe('/nodeinfo');
    expect((error as RlnTimeoutError).timeoutMs).toBe(50);
  });

  it('passes the RLN error body through on RlnHttpError', async () => {
    handler = (_req, res) => {
      res.writeHead(400, { 'content-type': 'application/json' });
      res.end(JSON.stringify({ error: 'Invalid invoice: bad checksum', code: 400 }));
    };
    const error = await client()
      .decodeLnInvoice({ invoice: 'nope' })
      .then(
        () => undefined,
        (e: unknown) => e,
      );
    expect(error).toBeInstanceOf(RlnHttpError);
    const httpError = error as RlnHttpError;
    expect(httpError.status).toBe(400);
    expect(httpError.body).toEqual({ error: 'Invalid invoice: bad checksum', code: 400 });
  });

  it('keeps the raw body when an error response is not RLN-shaped JSON', async () => {
    handler = (_req, res) => {
      res.writeHead(502, { 'content-type': 'text/plain' });
      res.end('Bad Gateway');
    };
    const error = await client()
      .nodeInfo()
      .then(
        () => undefined,
        (e: unknown) => e,
      );
    expect(error).toBeInstanceOf(RlnHttpError);
    expect((error as RlnHttpError).body).toBeUndefined();
    expect((error as RlnHttpError).rawBody).toBe('Bad Gateway');
  });

  it('reports connection failures as RlnNetworkError', async () => {
    const port = (server.address() as AddressInfo).port;
    server.closeAllConnections();
    await new Promise<void>((resolve) => server.close(() => resolve()));
    const dead = new RlnClient({ baseUrl: `http://127.0.0.1:${port}`, adminToken: '' });
    await expect(dead.nodeInfo()).rejects.toBeInstanceOf(RlnNetworkError);
    // Recreate so afterEach can close it.
    server = createServer(() => {});
    await new Promise<void>((resolve) => server.listen(0, '127.0.0.1', resolve));
  });

  it('sanitizes RLN failures so no upstream detail reaches clients', () => {
    const upstream = new RlnHttpError(
      '/sendpayment',
      400,
      { error: 'secret internal detail', code: 400 },
      '',
    );
    const sanitized = sanitizeRlnError(upstream);
    expect(sanitized).toBeInstanceOf(HttpError);
    expect(sanitized.statusCode).toBe(502);
    expect(sanitized.code).toBe('UPSTREAM_ERROR');
    expect(sanitized.message).not.toContain('secret');

    const timeout = sanitizeRlnError(new RlnTimeoutError('/nodeinfo', 30_000));
    expect(timeout.statusCode).toBe(504);
    expect(timeout.code).toBe('UPSTREAM_TIMEOUT');

    const network = sanitizeRlnError(new RlnNetworkError('/nodeinfo', new Error('ECONNREFUSED')));
    expect(network.statusCode).toBe(502);
    expect(network.message).not.toContain('ECONNREFUSED');
  });
});
