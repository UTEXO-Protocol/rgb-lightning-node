/**
 * On-chain prepare/complete routes: server prepares, client signs, server
 * completes. All money-moving, so every route requires an Idempotency-Key;
 * auth runs onRequest (before body parsing) and all downstream work goes
 * through the per-user FIFO queue.
 */
import type { FastifyInstance, FastifySchema } from 'fastify';
import {
  createUtxosCompleteRouteSchema,
  createUtxosPrepareRouteSchema,
  feeEstimateRouteSchema,
  operationGetRouteSchema,
  sendAssetCompleteRouteSchema,
  sendAssetPrepareRouteSchema,
  sendBtcCompleteRouteSchema,
  sendBtcPrepareRouteSchema,
} from '../schemas/onchain.js';
import type {
  OnchainOpKind,
  PrepareCreateUtxosParams,
  PrepareSendAssetParams,
  PrepareSendBtcParams,
} from '../wallets/prepare.js';

interface CompleteBody {
  opId: string;
  signedPsbt: string;
}

/** Confirmation target used when a fee-estimate request names none. */
const DEFAULT_FEE_TARGET_BLOCKS = 6;

export function registerOnchainRoutes(app: FastifyInstance): void {
  const routeOptions = (schema: FastifySchema) => ({
    schema,
    onRequest: [app.authenticate],
    preHandler: [app.idempotency.preHandler],
    onSend: app.idempotency.onSend,
  });

  const completeRoute = (url: string, schema: FastifySchema, kind: OnchainOpKind): void => {
    app.post(url, routeOptions(schema), (request) => {
      const userId = request.userId as string;
      const { opId, signedPsbt } = request.body as CompleteBody;
      return app.queues.enqueue(userId, async () => {
        const completed = await app.onchain.complete(userId, kind, opId, signedPsbt);
        if (kind === 'create_utxos') {
          return { txid: completed.txid, utxosCreated: completed.utxosCreated };
        }
        if (kind === 'send_asset') {
          return { txid: completed.txid, batchTransferIdx: completed.batchTransferIdx };
        }
        return { txid: completed.txid };
      });
    });
  };

  // Fee source for clients, which cannot reach esplora themselves. Read-only,
  // no Idempotency-Key and not queued behind the per-user wallet work: it makes
  // no wallet call, and a client needs a fee rate BEFORE it can prepare
  // anything — queueing it behind that user's stalled wallet operation would
  // deny it precisely when it is about to be used. Cached in the estimator, so
  // a burst of clients does not become a burst of indexer calls.
  app.get(
    '/v1/onchain/fee-estimate',
    { schema: feeEstimateRouteSchema, onRequest: [app.authenticate] },
    (request) => {
      const { blocks } = request.query as { blocks?: number };
      return app.fees.estimate(blocks ?? DEFAULT_FEE_TARGET_BLOCKS);
    },
  );

  // Read-only recovery route: the outcome of `complete` can be lost to a
  // timeout, a crash, or the 502 COMPLETE_AMBIGUOUS that rgb-lib's
  // broadcast-before-bookkeeping ordering makes possible, and the txid is
  // recorded on the op in exactly that case. Deliberately NOT on the per-user
  // queue and with no Idempotency-Key: it is a single SQLite read, and queueing
  // it behind the very wallet call that is stuck would make it unavailable
  // precisely when a client needs it.
  app.get(
    '/v1/onchain/operations/:opId',
    { schema: operationGetRouteSchema, onRequest: [app.authenticate] },
    (request) => {
      const userId = request.userId as string;
      const { opId } = request.params as { opId: string };
      return app.onchain.getOperation(userId, opId);
    },
  );

  app.post(
    '/v1/onchain/send-btc/prepare',
    routeOptions(sendBtcPrepareRouteSchema),
    (request, reply) => {
      const userId = request.userId as string;
      const params = request.body as PrepareSendBtcParams;
      return app.queues.enqueue(userId, async () => {
        const prepared = await app.onchain.prepareSendBtc(userId, params);
        return reply.code(201).send(prepared);
      });
    },
  );
  completeRoute('/v1/onchain/send-btc/complete', sendBtcCompleteRouteSchema, 'send_btc');

  app.post(
    '/v1/onchain/send-asset/prepare',
    routeOptions(sendAssetPrepareRouteSchema),
    (request, reply) => {
      const userId = request.userId as string;
      const params = request.body as PrepareSendAssetParams;
      return app.queues.enqueue(userId, async () => {
        const prepared = await app.onchain.prepareSendAsset(userId, params);
        return reply.code(201).send(prepared);
      });
    },
  );
  completeRoute('/v1/onchain/send-asset/complete', sendAssetCompleteRouteSchema, 'send_asset');

  app.post(
    '/v1/onchain/create-utxos/prepare',
    routeOptions(createUtxosPrepareRouteSchema),
    (request, reply) => {
      const userId = request.userId as string;
      const params = request.body as PrepareCreateUtxosParams;
      return app.queues.enqueue(userId, async () => {
        const prepared = await app.onchain.prepareCreateUtxos(userId, params);
        return reply.code(201).send(prepared);
      });
    },
  );
  completeRoute(
    '/v1/onchain/create-utxos/complete',
    createUtxosCompleteRouteSchema,
    'create_utxos',
  );
}
