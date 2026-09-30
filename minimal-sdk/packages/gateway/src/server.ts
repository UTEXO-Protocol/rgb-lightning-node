/**
 * Gateway server skeleton: Fastify + SQLite + auth + per-user FIFO queue +
 * idempotency hooks. Later tasks register RLN, wallet, on-chain and LN routes
 * on top of the decorators wired here.
 */
import { randomUUID } from 'node:crypto';
import Fastify, { type FastifyInstance } from 'fastify';
import type { GatewayConfig } from './config.js';
import { openDb, type GatewayDb } from './db.js';
import { createUser, makeAuthHook, makeOperatorGuard, type PreHandler } from './auth.js';
import {
  makeIdempotencyHooks,
  type IdempotencyClaim,
  type IdempotencyHooks,
} from './idempotency.js';
import { QueueFullError, UserQueues } from './queue.js';
import { HttpError } from './errors.js';
import { createUserRouteSchema, healthRouteSchema, meRouteSchema } from './schemas/index.js';
import type { WalletBackend } from './wallets/backend.js';
import { NativeWalletBackend } from './wallets/rgblib.js';
import { WalletOpQueues } from './wallets/queue.js';
import { WalletPool } from './wallets/pool.js';
import { WalletService } from './wallets/service.js';
import { OnchainService } from './wallets/prepare.js';
import { registerWalletRoutes } from './routes/wallet.js';
import { registerOnchainRoutes } from './routes/onchain.js';
import { FloatCapExceededError, InsufficientBalanceError, Ledger } from './ledger.js';
import { RlnClient, type RlnApi } from './rln/client.js';
import { LnFlows, registerLnRoutes } from './routes/ln.js';
import { DepositsWorker } from './workers/deposits.js';
import { Reconciler } from './workers/reconciler.js';
import { CompletionsWorker } from './workers/completions.js';
import { FeeEstimator } from './fees.js';

export interface RouteSchemaEntry {
  method: string | string[];
  url: string;
  schema: unknown;
}

declare module 'fastify' {
  interface FastifyInstance {
    db: GatewayDb;
    gatewayConfig: GatewayConfig;
    queues: UserQueues;
    authenticate: PreHandler;
    idempotency: IdempotencyHooks;
    routeSchemas: RouteSchemaEntry[];
    wallets: WalletService;
    walletPool: WalletPool;
    onchain: OnchainService;
    rln: RlnApi;
    ledger: Ledger;
    ln: LnFlows;
    depositsWorker: DepositsWorker;
    reconciler: Reconciler;
    completionsWorker: CompletionsWorker;
    fees: FeeEstimator;
  }
  interface FastifyRequest {
    userId?: string;
    idempotency?: IdempotencyClaim;
  }
}

export interface BuildServerOptions {
  config: GatewayConfig;
  /** Pre-opened database (tests pass ':memory:'-backed instances). */
  db?: GatewayDb;
  /** Capture stream for log output (tests assert secrets never appear). */
  loggerStream?: { write(msg: string): void };
  loggerLevel?: string;
  /** Wallet backend override (unit tests inject a mock; defaults to rgb-lib). */
  walletBackend?: WalletBackend;
  /** RLN client override (unit tests inject a fake; defaults to RlnClient). */
  rlnClient?: RlnApi;
  /** Esplora fetch override for the deposits watcher and fee estimator. */
  esploraFetch?: typeof fetch;
  /** Start the background workers (deposits watcher + reconciler). Off in tests. */
  startWorkers?: boolean;
}

export async function buildServer(options: BuildServerOptions): Promise<FastifyInstance> {
  const { config } = options;
  const db = options.db ?? openDb(config.sqlitePath);

  const app = Fastify({
    // Random request ids: the value is echoed to the client as `x-request-id`
    // (below), so it must not be a guessable per-process counter — it is the
    // only handle a client has on a failure whose detail stays server-side.
    genReqId: () => randomUUID(),
    // Never adopt a caller-supplied id: it would let a client choose the value
    // that ties together every log line for its own requests. Fastify v5
    // already defaults this off; pinning it keeps that true across upgrades.
    requestIdHeader: false,
    logger: {
      level: options.loggerLevel ?? 'info',
      // Safety net: these headers carry credentials and must never be logged.
      redact: {
        paths: [
          'req.headers.authorization',
          'req.headers["x-operator-token"]',
          'reqHeaders.authorization',
          'reqHeaders["x-operator-token"]',
        ],
        censor: '[redacted]',
      },
      ...(options.loggerStream !== undefined ? { stream: options.loggerStream } : {}),
    },
    bodyLimit: 64 * 1024,
  });

  const routeSchemas: RouteSchemaEntry[] = [];
  app.addHook('onRoute', (route) => {
    routeSchemas.push({ method: route.method, url: route.url, schema: route.schema });
  });

  // Correlation handle. Client-facing error bodies carry only `code` and
  // `message` (invariant I4) — the RLN or wallet failure they were mapped from
  // is logged under this same id and nowhere else. Without the header a client
  // holding a 502 COMPLETE_AMBIGUOUS or WITHDRAWAL_UNRESOLVED has nothing an
  // operator can grep for. Set on the request hook so it is present on error
  // replies and on idempotency replays alike (the replay path re-runs hooks and
  // caches only status/content-type/body, so a replay gets a fresh id).
  app.addHook('onRequest', async (request, reply) => {
    void reply.header('x-request-id', request.id);
  });

  app.decorate('db', db);
  app.decorate('gatewayConfig', config);
  app.decorate(
    'queues',
    new UserQueues({
      globalConcurrency: config.queueGlobalConcurrency,
      perUserDepth: config.queuePerUserDepth,
    }),
  );
  app.decorate('authenticate', makeAuthHook(db));
  app.decorate('idempotency', makeIdempotencyHooks(db));
  app.decorate('routeSchemas', routeSchemas);

  const walletBackend =
    options.walletBackend ??
    new NativeWalletBackend({
      baseDir: config.walletsDir,
      network: config.bitcoinNetwork,
      indexerUrl: config.walletIndexerUrl,
    });
  const walletPool = new WalletPool(walletBackend, new WalletOpQueues(), {
    maxOpen: config.walletMaxOpen,
  });
  app.decorate('walletPool', walletPool);
  const walletService = new WalletService(db, walletPool, config);
  app.decorate('wallets', walletService);
  const onchainService = new OnchainService(db, walletPool, walletService, config);
  app.decorate('onchain', onchainService);
  app.decorate(
    'fees',
    new FeeEstimator({
      esploraUrl: config.esploraUrl,
      ...(options.esploraFetch !== undefined ? { fetchImpl: options.esploraFetch } : {}),
    }),
  );

  const rln =
    options.rlnClient ??
    new RlnClient({ baseUrl: config.rlnUrl, adminToken: config.rlnAdminToken });
  app.decorate('rln', rln);
  const ledger = new Ledger(db);
  app.decorate('ledger', ledger);
  app.decorate('ln', new LnFlows(db, ledger, rln, config));
  const depositsWorker = new DepositsWorker({
    db,
    ledger,
    rln,
    esploraUrl: config.esploraUrl,
    minConfirmations: config.depositMinConfirmations,
    ...(options.esploraFetch !== undefined ? { fetchImpl: options.esploraFetch } : {}),
    log: app.log,
  });
  app.decorate('depositsWorker', depositsWorker);
  const reconciler = new Reconciler({
    db,
    ledger,
    rln,
    outboundGraceSeconds: config.reconcilerGraceSeconds,
    log: app.log,
  });
  app.decorate('reconciler', reconciler);
  const completionsWorker = new CompletionsWorker({
    db,
    onchain: onchainService,
    maxAttempts: config.completionsMaxAttempts,
    log: app.log,
  });
  app.decorate('completionsWorker', completionsWorker);

  app.setErrorHandler((error, request, reply) => {
    if (error instanceof QueueFullError) {
      void reply
        .header('retry-after', String(error.retryAfterSeconds))
        .code(429)
        .send({ error: { code: 'QUEUE_FULL', message: 'too many pending requests, retry later' } });
      return;
    }
    if (error instanceof HttpError) {
      // The client-facing body stays opaque (invariant I4), so the `cause` —
      // the RLN or wallet failure this was mapped from — is the only record
      // an operator has of why a money-moving call failed. It must reach the
      // log, or an 'ambiguous' withdrawal is unresolvable.
      if (error.statusCode >= 500 || error.cause !== undefined) {
        request.log.error(
          { err: error.cause ?? error, code: error.code, statusCode: error.statusCode },
          'request failed',
        );
      }
      void reply
        .code(error.statusCode)
        .send({ error: { code: error.code, message: error.message } });
      return;
    }
    if (error instanceof FloatCapExceededError) {
      void reply.code(409).send({ error: { code: 'FLOAT_CAP_EXCEEDED', message: error.message } });
      return;
    }
    if (error instanceof InsufficientBalanceError) {
      void reply
        .code(409)
        .send({ error: { code: 'INSUFFICIENT_BALANCE', message: error.message } });
      return;
    }
    const maybeValidation = error as { validation?: unknown; message?: unknown };
    if (maybeValidation.validation !== undefined) {
      const message =
        typeof maybeValidation.message === 'string' ? maybeValidation.message : 'invalid request';
      void reply.code(400).send({ error: { code: 'BAD_REQUEST', message } });
      return;
    }
    // Unknown failure: log server-side, return an opaque error (no internals leak).
    request.log.error({ err: error }, 'unhandled error');
    void reply.code(500).send({ error: { code: 'INTERNAL', message: 'internal error' } });
  });

  app.get('/v1/health', { schema: healthRouteSchema }, async () => ({ status: 'ok' }));

  const operatorGuard = makeOperatorGuard(config.operatorToken);
  app.post(
    '/v1/users',
    { schema: createUserRouteSchema, preHandler: [operatorGuard] },
    async (request, reply) => {
      const created = createUser(db);
      request.log.info({ userId: created.userId }, 'user created');
      return reply.code(201).send(created);
    },
  );

  app.get('/v1/me', { schema: meRouteSchema, preHandler: [app.authenticate] }, async (request) => {
    const row = db.prepare('SELECT created_at FROM users WHERE id = ?').get(request.userId) as
      { created_at: number } | undefined;
    if (row === undefined) {
      throw new HttpError(401, 'UNAUTHORIZED', 'unknown user');
    }
    return { userId: request.userId, createdAt: row.created_at };
  });

  registerWalletRoutes(app);
  registerOnchainRoutes(app);
  registerLnRoutes(app);

  if (options.startWorkers === true) {
    depositsWorker.start(config.depositsIntervalMs);
    reconciler.start(config.reconcilerIntervalMs);
    completionsWorker.start(config.completionsIntervalMs);
  }

  app.addHook('onClose', async () => {
    depositsWorker.stop();
    reconciler.stop();
    completionsWorker.stop();
    await walletPool.closeAll();
    db.close();
  });

  return app;
}
