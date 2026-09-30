/**
 * E2e harness: brings up the repo's regtest stack (bitcoind + electrs + RGB
 * proxy + esplora), two RLN nodes (the shared node the gateway fronts and a
 * counterparty), opens a plain-BTC channel between them and starts the gateway
 * in-process on a real HTTP port.
 *
 * The user JOURNEY (flow.test.ts) talks ONLY to the gateway through the client
 * SDK. This harness may drive the counterparty RLN node and the regtest faucet
 * directly — that models the outside world, not the user.
 *
 * Env knobs (see README): E2E_SKIP_STACK=1 reuses an already-running regtest
 * stack, E2E_STOP_STACK=1 tears the containers down afterwards.
 */
import { execSync, spawn, type ChildProcess } from 'node:child_process';
import { mkdtempSync, openSync, closeSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import type { FastifyInstance } from 'fastify';
import { buildServer, findKeyMaterialInValue, loadConfig } from '@utexo/minimal-gateway';

export const REPO_ROOT = fileURLToPath(new URL('../../../..', import.meta.url));
const RLN_BINARY = join(REPO_ROOT, 'target', 'debug', 'rgb-lightning-node');

export const ELECTRUM_URL = '127.0.0.1:50001';
export const ESPLORA_URL = 'http://127.0.0.1:3002';
export const PROXY_URL = 'rpc://127.0.0.1:3000/json-rpc';

export const SHARED_RLN_PORT = 3201;
export const SHARED_PEER_PORT = 9901;
export const CP_RLN_PORT = 3202;
export const CP_PEER_PORT = 9902;
export const GATEWAY_PORT = 8490;

export const SHARED_RLN_URL = `http://127.0.0.1:${SHARED_RLN_PORT}`;
export const CP_RLN_URL = `http://127.0.0.1:${CP_RLN_PORT}`;
export const GATEWAY_URL = `http://127.0.0.1:${GATEWAY_PORT}`;

export const OPERATOR_TOKEN = 'e2e-operator-token-0123456789abcdef';
/**
 * Sentinel credential (I4): RLN runs with --disable-authentication and ignores
 * the Authorization header, so the gateway can carry a known sentinel that
 * must never surface in any gateway response or log.
 */
export const RLN_ADMIN_TOKEN_SENTINEL = 'e2e-rln-admin-token-sentinel-do-not-leak';

const NODE_PASSWORD = 'e2epassword';
const CHANNEL_CAPACITY_SAT = 1_000_000;
const CHANNEL_PUSH_MSAT = 300_000_000;

export function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

export function regtest(command: string): string {
  return execSync(`./regtest.sh ${command}`, {
    cwd: REPO_ROOT,
    encoding: 'utf8',
    stdio: ['ignore', 'pipe', 'pipe'],
  }).trim();
}

export function mine(blocks: number): void {
  regtest(`mine ${blocks}`);
}

export async function retryUntil<T>(
  what: string,
  probe: () => Promise<T | undefined>,
  timeoutMs: number,
  intervalMs = 1000,
): Promise<T> {
  const deadline = Date.now() + timeoutMs;
  for (;;) {
    const value = await probe();
    if (value !== undefined) return value;
    if (Date.now() > deadline) throw new Error(`timed out waiting for: ${what}`);
    await sleep(intervalMs);
  }
}

/**
 * Raw RLN call for HARNESS use only (faucet/counterparty side of the world).
 * GET when body is undefined, POST otherwise.
 */
export async function rln<T = unknown>(base: string, path: string, body?: unknown): Promise<T> {
  const response = await fetch(`${base}${path}`, {
    method: body === undefined ? 'GET' : 'POST',
    ...(body === undefined
      ? {}
      : { headers: { 'content-type': 'application/json' }, body: JSON.stringify(body) }),
  });
  const text = await response.text();
  if (!response.ok) {
    throw new Error(`RLN ${base}${path} -> ${response.status}: ${text}`);
  }
  return JSON.parse(text) as T;
}

/** Every secret that must never appear in a gateway response or log line. */
export interface InvariantRecorder {
  /** Violations found while scanning gateway responses; must stay empty. */
  violations: string[];
  /** Number of gateway responses scanned (sanity: the scan actually ran). */
  scanned: number;
  /** name -> secret scanned against every RESPONSE body. */
  responseForbidden: Map<string, string>;
  /** name -> secret scanned against captured gateway LOGS. */
  logForbidden: Map<string, string>;
  /** Captured gateway log lines. */
  logs: string[];
}

export function makeRecorder(): InvariantRecorder {
  const recorder: InvariantRecorder = {
    violations: [],
    scanned: 0,
    responseForbidden: new Map(),
    logForbidden: new Map(),
    logs: [],
  };
  recorder.responseForbidden.set('rln-admin-token', RLN_ADMIN_TOKEN_SENTINEL);
  recorder.responseForbidden.set('operator-token', OPERATOR_TOKEN);
  recorder.logForbidden.set('rln-admin-token', RLN_ADMIN_TOKEN_SENTINEL);
  recorder.logForbidden.set('operator-token', OPERATOR_TOKEN);
  return recorder;
}

/** Register a client-side mnemonic: forbidden in every response and log. */
export function forbidMnemonic(recorder: InvariantRecorder, name: string, mnemonic: string): void {
  recorder.responseForbidden.set(name, mnemonic);
  recorder.logForbidden.set(name, mnemonic);
}

/** User bearer tokens are returned ONCE by createUser but must never be logged. */
export function forbidInLogs(recorder: InvariantRecorder, name: string, secret: string): void {
  recorder.logForbidden.set(name, secret);
}

/**
 * fetch wrapper for ALL journey clients: scans every gateway response for
 * key-material field names (mirrors the I1 schema-scan test at runtime) and
 * for forbidden secret values (I4).
 */
export function makeScanningFetch(recorder: InvariantRecorder): typeof fetch {
  const scanningFetch = (async (input: string | URL | Request, init?: RequestInit) => {
    const response = await fetch(input as string | URL | Request, init);
    const url = String(input);
    let text = '';
    try {
      text = await response.clone().text();
    } catch {
      return response;
    }
    recorder.scanned += 1;
    for (const [name, secret] of recorder.responseForbidden) {
      if (secret !== '' && text.includes(secret)) {
        recorder.violations.push(`secret "${name}" found in response of ${url}`);
      }
    }
    try {
      const parsed: unknown = JSON.parse(text);
      for (const path of findKeyMaterialInValue(parsed)) {
        recorder.violations.push(`key-material field ${path} in response of ${url}`);
      }
    } catch {
      // non-JSON response: substring scan above is all we can do
    }
    return response;
  }) as typeof fetch;
  return scanningFetch;
}

export interface RlnNode {
  name: string;
  base: string;
  peerPort: number;
  dataDir: string;
  process: ChildProcess;
  pubkey: string;
}

async function startRlnNode(name: string, daemonPort: number, peerPort: number): Promise<RlnNode> {
  const dataDir = mkdtempSync(join(tmpdir(), `utexo-e2e-${name}-`));
  const logFd = openSync(join(dataDir, 'rln.log'), 'w');
  const child = spawn(
    RLN_BINARY,
    [
      dataDir,
      '--daemon-listening-port',
      String(daemonPort),
      '--ldk-peer-listening-port',
      String(peerPort),
      '--network',
      'regtest',
      '--disable-authentication',
    ],
    { stdio: ['ignore', logFd, logFd] },
  );
  closeSync(logFd);
  const base = `http://127.0.0.1:${daemonPort}`;

  await retryUntil(
    `${name} RLN HTTP up`,
    async () => {
      try {
        await fetch(`${base}/nodeinfo`);
        return true;
      } catch {
        return undefined;
      }
    },
    60_000,
    500,
  );

  await rln(base, '/init', { password: NODE_PASSWORD });
  await rln(base, '/unlock', {
    password: NODE_PASSWORD,
    ldk_chain_sync: {
      mode: 'BlockSync',
      config: {
        bitcoind_rpc_username: 'user',
        bitcoind_rpc_password: 'password',
        bitcoind_rpc_host: 'localhost',
        bitcoind_rpc_port: 18443,
      },
    },
    indexer_url: ELECTRUM_URL,
    proxy_endpoint: PROXY_URL,
    announce_addresses: [],
  });
  const info = await retryUntil(
    `${name} RLN unlocked`,
    async () => {
      try {
        return await rln<{ pubkey: string }>(base, '/nodeinfo', undefined);
      } catch {
        return undefined;
      }
    },
    60_000,
  );
  return { name, base, peerPort, dataDir, process: child, pubkey: info.pubkey };
}

async function fundNode(node: RlnNode, amountBtc: string, targetSat: number): Promise<void> {
  const { address } = await rln<{ address: string }>(node.base, '/address', {});
  regtest(`sendtoaddress ${address} ${amountBtc}`);
  mine(1);
  await retryUntil(
    `${node.name} on-chain funds`,
    async () => {
      const balance = await rln<{ vanilla: { settled: number } }>(node.base, '/btcbalance', {
        skip_sync: false,
      });
      return balance.vanilla.settled >= targetSat ? true : undefined;
    },
    60_000,
  );
}

interface ChannelEntry {
  ready: boolean;
  is_usable: boolean;
  peer_pubkey: string;
}

async function openBtcChannel(shared: RlnNode, cp: RlnNode): Promise<void> {
  const peerUri = `${cp.pubkey}@127.0.0.1:${cp.peerPort}`;
  await rln(shared.base, '/connectpeer', { peer_pubkey_and_addr: peerUri });
  await rln(shared.base, '/openchannel', {
    peer_pubkey_and_opt_addr: peerUri,
    capacity_sat: CHANNEL_CAPACITY_SAT,
    push_msat: CHANNEL_PUSH_MSAT,
    public: false,
    with_anchors: true,
  });
  await retryUntil(
    'channel usable on both nodes',
    async () => {
      mine(1);
      const [a, b] = await Promise.all([
        rln<{ channels: ChannelEntry[] }>(shared.base, '/listchannels', undefined),
        rln<{ channels: ChannelEntry[] }>(cp.base, '/listchannels', undefined),
      ]);
      const usable = (channels: ChannelEntry[], peer: string) =>
        channels.some((c) => c.peer_pubkey === peer && c.ready && c.is_usable);
      return usable(a.channels, cp.pubkey) && usable(b.channels, shared.pubkey) ? true : undefined;
    },
    180_000,
    2000,
  );
}

async function issueCounterpartyAsset(cp: RlnNode): Promise<string> {
  await rln(cp.base, '/createutxos', {
    up_to: false,
    num: 4,
    size: 32_500,
    fee_rate: 2,
    skip_sync: false,
  });
  mine(1);
  const issued = await rln<{ asset: { asset_id: string } }>(cp.base, '/issueassetnia', {
    ticker: 'E2E',
    name: 'E2E Asset',
    precision: 0,
    amounts: [1000],
  });
  return issued.asset.asset_id;
}

export interface Harness {
  shared: RlnNode;
  cp: RlnNode;
  app: FastifyInstance;
  recorder: InvariantRecorder;
  /** NIA asset issued on the counterparty, ready to be sent to the vault. */
  assetId: string;
  teardown(): Promise<void>;
}

async function startStack(): Promise<void> {
  if (process.env['E2E_SKIP_STACK'] === '1') {
    return;
  }
  execSync('ESPLORA=1 ./regtest.sh start', {
    cwd: REPO_ROOT,
    encoding: 'utf8',
    stdio: ['ignore', 'pipe', 'pipe'],
    env: { ...process.env, ESPLORA: '1' },
  });
}

async function startGateway(
  recorder: InvariantRecorder,
  tempDirs: string[],
): Promise<FastifyInstance> {
  const dbDir = mkdtempSync(join(tmpdir(), 'utexo-e2e-gw-db-'));
  const walletsDir = mkdtempSync(join(tmpdir(), 'utexo-e2e-gw-wallets-'));
  tempDirs.push(dbDir, walletsDir);
  const config = loadConfig({
    GATEWAY_PORT: String(GATEWAY_PORT),
    RLN_URL: SHARED_RLN_URL,
    RLN_ADMIN_TOKEN: RLN_ADMIN_TOKEN_SENTINEL,
    ESPLORA_URL,
    RGB_PROXY_URL: PROXY_URL,
    GATEWAY_SQLITE_PATH: join(dbDir, 'gateway.sqlite'),
    GATEWAY_WALLETS_DIR: walletsDir,
    GATEWAY_WALLET_INDEXER_URL: ELECTRUM_URL,
    GATEWAY_OPERATOR_TOKEN: OPERATOR_TOKEN,
    GATEWAY_DEPOSIT_MIN_CONFIRMATIONS: '1',
    GATEWAY_DEPOSITS_INTERVAL_MS: '1000',
    GATEWAY_RECONCILER_INTERVAL_MS: '1000',
  });
  const app = await buildServer({
    config,
    loggerStream: {
      write(msg: string) {
        recorder.logs.push(msg);
      },
    },
    startWorkers: true,
  });
  await app.listen({ host: '127.0.0.1', port: GATEWAY_PORT });
  return app;
}

function stopProcess(child: ChildProcess): Promise<void> {
  return new Promise((resolve) => {
    if (child.exitCode !== null || child.killed) {
      resolve();
      return;
    }
    const killTimer = setTimeout(() => {
      child.kill('SIGKILL');
    }, 5000);
    child.once('exit', () => {
      clearTimeout(killTimer);
      resolve();
    });
    child.kill('SIGTERM');
  });
}

export async function setupHarness(): Promise<Harness> {
  await startStack();
  const recorder = makeRecorder();

  // Anything already started must be torn down if a later step throws:
  // beforeAll never returns a harness in that case, so the test file's
  // afterAll cannot clean up, and orphaned daemons hold the fixed ports —
  // the next run would then fail on a bind error instead of the real cause.
  const started: RlnNode[] = [];
  const tempDirs: string[] = [];
  let app: FastifyInstance | undefined;
  const cleanup = async (): Promise<void> => {
    await app?.close();
    await Promise.all(started.map((node) => stopProcess(node.process)));
    for (const dir of tempDirs) {
      rmSync(dir, { recursive: true, force: true });
    }
  };

  try {
    // Node data dirs are deliberately NOT registered for removal: they hold
    // rln.log, which is the first thing to read after a failed run.
    const shared = await startRlnNode('shared', SHARED_RLN_PORT, SHARED_PEER_PORT);
    started.push(shared);
    const cp = await startRlnNode('cp', CP_RLN_PORT, CP_PEER_PORT);
    started.push(cp);

    // The shared node funds the channel + withdraw fees; the counterparty funds
    // colored UTXOs for issuance and its side of on-chain fees.
    await fundNode(shared, '0.5', 45_000_000);
    await fundNode(cp, '0.5', 45_000_000);
    await rln(shared.base, '/createutxos', {
      up_to: false,
      num: 4,
      size: 32_500,
      fee_rate: 2,
      skip_sync: false,
    });
    mine(1);
    const assetId = await issueCounterpartyAsset(cp);
    await openBtcChannel(shared, cp);

    app = await startGateway(recorder, tempDirs);

    return {
      shared,
      cp,
      app,
      recorder,
      assetId,
      async teardown() {
        await cleanup();
        if (process.env['E2E_STOP_STACK'] === '1') {
          regtest('stop');
        }
      },
    };
  } catch (error) {
    await cleanup();
    throw error;
  }
}
