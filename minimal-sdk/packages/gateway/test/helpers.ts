import { tmpdir } from 'node:os';
import { join } from 'node:path';
import type { FastifyInstance } from 'fastify';
import { loadConfig, type GatewayConfig } from '../src/config.js';
import { buildServer } from '../src/server.js';
import type { WalletBackend } from '../src/wallets/backend.js';
import type { RlnApi } from '../src/rln/client.js';

export const OPERATOR_TOKEN = 'test-operator-token-0123456789';
export const RLN_ADMIN_TOKEN = 'test-rln-admin-token-abcdef';

export function testEnv(overrides: Record<string, string> = {}): Record<string, string> {
  return {
    RLN_URL: 'http://127.0.0.1:3001',
    RLN_ADMIN_TOKEN,
    ESPLORA_URL: 'http://127.0.0.1:3002',
    RGB_PROXY_URL: 'rpc://127.0.0.1:3000/json-rpc',
    GATEWAY_SQLITE_PATH: ':memory:',
    GATEWAY_WALLETS_DIR: join(tmpdir(), 'utexo-gateway-test-wallets'),
    GATEWAY_WALLET_INDEXER_URL: '127.0.0.1:50001',
    GATEWAY_OPERATOR_TOKEN: OPERATOR_TOKEN,
    ...overrides,
  };
}

export function testConfig(overrides: Partial<GatewayConfig> = {}): GatewayConfig {
  return { ...loadConfig(testEnv()), ...overrides };
}

export interface TestServerOptions {
  configOverrides?: Partial<GatewayConfig>;
  loggerStream?: { write(msg: string): void };
  walletBackend?: WalletBackend;
  rlnClient?: RlnApi;
  esploraFetch?: typeof fetch;
}

export async function testServer(options: TestServerOptions = {}): Promise<FastifyInstance> {
  const config = testConfig(options.configOverrides ?? {});
  return buildServer({
    config,
    ...(options.loggerStream !== undefined ? { loggerStream: options.loggerStream } : {}),
    ...(options.walletBackend !== undefined ? { walletBackend: options.walletBackend } : {}),
    ...(options.rlnClient !== undefined ? { rlnClient: options.rlnClient } : {}),
    ...(options.esploraFetch !== undefined ? { esploraFetch: options.esploraFetch } : {}),
  });
}

export interface TestUser {
  userId: string;
  token: string;
  createdAt: number;
}

export async function createTestUser(app: FastifyInstance): Promise<TestUser> {
  const response = await app.inject({
    method: 'POST',
    url: '/v1/users',
    headers: { 'x-operator-token': OPERATOR_TOKEN },
  });
  if (response.statusCode !== 201) {
    throw new Error(`user bootstrap failed: ${response.statusCode} ${response.body}`);
  }
  return response.json() as TestUser;
}

export function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

export interface Deferred<T> {
  promise: Promise<T>;
  resolve: (value: T) => void;
  reject: (error: unknown) => void;
}

export function deferred<T>(): Deferred<T> {
  let resolve!: (value: T) => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}
