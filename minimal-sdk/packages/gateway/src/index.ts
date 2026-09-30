/**
 * @utexo/minimal-gateway — API gateway in front of one shared RLN instance.
 */
export const GATEWAY_NAME = '@utexo/minimal-gateway';

export { loadConfig, ConfigError, type GatewayConfig } from './config.js';
export { openDb, migrate, schemaVersion, type GatewayDb } from './db.js';
export {
  createUser,
  findUserByToken,
  generateToken,
  hashToken,
  safeEqual,
  makeAuthHook,
  makeOperatorGuard,
  type CreatedUser,
  type PreHandler,
} from './auth.js';
export { UserQueues, QueueFullError, type UserQueuesOptions } from './queue.js';
export {
  makeIdempotencyHooks,
  requestHash,
  type IdempotencyHooks,
  type IdempotencyClaim,
} from './idempotency.js';
export { HttpError } from './errors.js';
export { buildServer, type BuildServerOptions, type RouteSchemaEntry } from './server.js';
export {
  findKeyMaterialFields,
  findKeyMaterialInValue,
  KEY_MATERIAL_FIELD_NAMES,
} from './schemas/scan.js';
export {
  RlnClient,
  RlnHttpError,
  RlnNetworkError,
  RlnTimeoutError,
  sanitizeRlnError,
  DEFAULT_RLN_TIMEOUT_MS,
  type RlnApi,
  type RlnClientOptions,
} from './rln/client.js';
export {
  BTC_ASSET,
  FloatCapExceededError,
  InsufficientBalanceError,
  Ledger,
  type LedgerEntryParams,
  type LedgerKind,
  type LedgerRow,
} from './ledger.js';
export {
  LnFlows,
  mapHtlcStatus,
  refundOutboundPayment,
  refundWithdrawal,
  settleInboundInvoice,
  type GatewayPaymentStatus,
  type LnInvoiceRow,
} from './routes/ln.js';
export { DepositsWorker, type DepositsWorkerOptions } from './workers/deposits.js';
export { Reconciler, type ReconcilerOptions } from './workers/reconciler.js';
export {
  recordOwnership,
  ownerOf,
  updateResourceState,
  filterOwned,
  scopePayments,
  scopeTransfers,
  OwnershipConflictError,
  type ResourceKind,
  type ResourceRecord,
} from './rln/scoping.js';
export type * from './rln/types.js';
export {
  WalletBackendError,
  type Balance,
  type CreateUtxosParams,
  type ReceiveData,
  type ReceiveRequest,
  type SendAssetBeginRequest,
  type UserXpubs,
  type WalletAsset,
  type WalletBackend,
  type WalletBtcBalance,
  type WalletHandle,
  type WalletIdentity,
  type WalletTransfer,
  type WalletUnspent,
} from './wallets/backend.js';
export {
  NativeWalletBackend,
  loadNativeRgbLib,
  restoreKeysForTests,
  type NativeWalletBackendOptions,
} from './wallets/rgblib.js';
export { WalletOpQueues } from './wallets/queue.js';
export { WalletPool, type WalletPoolOptions } from './wallets/pool.js';
export {
  WalletService,
  type ReceiveParams,
  type ReceiveResult,
  type WalletBalances,
} from './wallets/service.js';
export {
  OnchainService,
  addressToScriptHex,
  txidFromPsbt,
  type CompletedOp,
  type IntentAsset,
  type IntentRecipient,
  type IntentUtxos,
  type OnchainIntent,
  type OnchainOpKind,
  type PreparedOp,
  type PrepareCreateUtxosParams,
  type PrepareSendAssetParams,
  type PrepareSendBtcParams,
} from './wallets/prepare.js';
