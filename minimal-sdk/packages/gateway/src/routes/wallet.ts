/**
 * Wallet routes: every handler is authenticated and funneled through the
 * request-level per-user FIFO (Task 2); wallet calls additionally serialize
 * on the per-wallet operation queue inside the pool.
 */
import type { FastifyInstance } from 'fastify';
import type { UserXpubs } from '../wallets/backend.js';
import type { ReceiveParams } from '../wallets/service.js';
import {
  registerXpubsRouteSchema,
  walletAddressRouteSchema,
  walletBalancesRouteSchema,
  walletReceiveRouteSchema,
  walletSyncRouteSchema,
  walletTransfersRouteSchema,
  walletUnspentsRouteSchema,
} from '../schemas/wallet.js';

export function registerWalletRoutes(app: FastifyInstance): void {
  // onRequest (not preHandler) so unauthenticated calls are rejected before
  // body parsing and schema validation ever run.
  const auth = { onRequest: [app.authenticate] };

  app.post('/v1/wallet/xpubs', { schema: registerXpubsRouteSchema, ...auth }, (request, reply) => {
    const userId = request.userId as string;
    const xpubs = request.body as UserXpubs;
    return app.queues.enqueue(userId, async () => {
      const result = await app.wallets.registerXpubs(userId, xpubs);
      return reply.code(201).send({ fingerprint: xpubs.fingerprint, address: result.address });
    });
  });

  app.get('/v1/wallet/address', { schema: walletAddressRouteSchema, ...auth }, (request) => {
    const userId = request.userId as string;
    return app.queues.enqueue(userId, () => app.wallets.getAddress(userId));
  });

  app.get('/v1/wallet/balances', { schema: walletBalancesRouteSchema, ...auth }, (request) => {
    const userId = request.userId as string;
    return app.queues.enqueue(userId, () => app.wallets.getBalances(userId));
  });

  app.get('/v1/wallet/unspents', { schema: walletUnspentsRouteSchema, ...auth }, (request) => {
    const userId = request.userId as string;
    return app.queues.enqueue(userId, async () => ({
      unspents: await app.wallets.listUnspents(userId),
    }));
  });

  app.get('/v1/wallet/transfers', { schema: walletTransfersRouteSchema, ...auth }, (request) => {
    const userId = request.userId as string;
    const { assetId } = request.query as { assetId?: string };
    return app.queues.enqueue(userId, async () => ({
      transfers: await app.wallets.listTransfers(userId, assetId),
    }));
  });

  app.post(
    '/v1/wallet/receive',
    { schema: walletReceiveRouteSchema, ...auth },
    (request, reply) => {
      const userId = request.userId as string;
      const params = request.body as ReceiveParams;
      return app.queues.enqueue(userId, async () => {
        const result = await app.wallets.receive(userId, params);
        return reply.code(201).send(result);
      });
    },
  );

  app.post('/v1/wallet/sync', { schema: walletSyncRouteSchema, ...auth }, (request) => {
    const userId = request.userId as string;
    return app.queues.enqueue(userId, async () => {
      await app.wallets.sync(userId);
      return { status: 'ok' };
    });
  });
}
