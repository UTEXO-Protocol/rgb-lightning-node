/**
 * Typed gateway configuration loaded from environment variables.
 *
 * The RLN admin token and operator token are credentials: they must never be
 * logged or echoed in any response (invariant I4).
 */

export interface GatewayConfig {
  /** Interface the gateway listens on. Defaults to loopback. */
  host: string;
  port: number;
  /** Base URL of the single shared RLN instance (reachable only from the gateway). */
  rlnUrl: string;
  /** Biscuit admin token; empty when RLN runs with --disable-authentication on localhost. */
  rlnAdminToken: string;
  esploraUrl: string;
  rgbProxyUrl: string;
  /** Path to the gateway SQLite database (':memory:' for tests). */
  sqlitePath: string;
  /** Gateway-owned directory holding one watch-only wallet data-dir per user. */
  walletsDir: string;
  /**
   * Indexer URL passed to rgb-lib `goOnline`. Either an Electrum server
   * (`host:port`, `ssl://host:port`) or an esplora REST base URL
   * (`https://…`) — the published rgb-lib native module is built with BOTH the
   * `electrum` and `esplora` features, and rgb-lib picks the client by probing
   * Electrum first and falling back to esplora.
   *
   * They are NOT equivalent. rgb-lib's Electrum resolver additionally requires
   * verbose-transaction support and, on the default signet, the presence of a
   * specific probe transaction; its esplora resolver checks only the genesis
   * hash. So a Blockstream/mempool `electrs` is rejected over its Electrum port
   * but works over its esplora REST port. See the README's indexer section.
   */
  walletIndexerUrl: string;
  /** rgb-lib BitcoinNetwork for user wallets. */
  bitcoinNetwork: 'Mainnet' | 'Testnet' | 'Testnet4' | 'Signet' | 'SignetCustom' | 'Regtest';
  /** LRU capacity of the open-wallet pool. */
  walletMaxOpen: number;
  /** Seconds an unsigned prepare-step PSBT stays completable before expiring. */
  onchainOpTtlSeconds: number;
  /** Guards the bootstrap-only POST /v1/users route. */
  operatorToken: string;
  /** Custodial LN float caps (Block 3): enforced at ledger credit/debit time. */
  floatCapPerUserMsat: number;
  floatCapGlobalMsat: number;
  /** Confirmations before a BTC/RGB deposit is credited to the float ledger. */
  depositMinConfirmations: number;
  /** Seconds an unfunded deposit intent stays pending (and counts against caps). */
  depositTtlSeconds: number;
  /**
   * HTLC carrier value (msat) put on asset invoices that declare no msat
   * amount. RLN refuses an asset invoice below the channel HTLC minimum, so
   * this must match the node's `channels.htlc_min_msat` (RLN default 3_000_000)
   * or the higher `inbound_htlc_minimum_msat` of the asset's channels.
   */
  assetInvoiceMinMsat: number;
  /**
   * RGB consignment-proxy endpoints users may name in withdraw/send-asset
   * requests (SSRF guard — the node dials these). Always includes rgbProxyUrl.
   */
  rgbTransportAllowlist: string[];
  /** Poll interval of the deposits watcher (ms). */
  depositsIntervalMs: number;
  /** Poll interval of the ambiguous-completion worker (ms). */
  completionsIntervalMs: number;
  /**
   * Attempts the completions worker makes at finishing one ambiguous operation
   * before leaving it to an operator.
   */
  completionsMaxAttempts: number;
  /** Poll interval of the payments reconciler (ms). */
  reconcilerIntervalMs: number;
  /**
   * Seconds a debited-pending outbound payment RLN does not know about must
   * age before the reconciler refunds it (the send may still be queued behind
   * RLN's global lock — refunding too early would double-spend the float).
   */
  reconcilerGraceSeconds: number;
  /** Max downstream operations running at once across all users. */
  queueGlobalConcurrency: number;
  /** Max queued+running operations per user before 429. */
  queuePerUserDepth: number;
}

export class ConfigError extends Error {
  constructor(message: string) {
    super(message);
    this.name = 'ConfigError';
  }
}

function requireString(env: Record<string, string | undefined>, name: string): string {
  const value = env[name];
  if (value === undefined || value.trim() === '') {
    throw new ConfigError(`missing required environment variable ${name}`);
  }
  return value.trim();
}

function requireUrl(env: Record<string, string | undefined>, name: string): string {
  const value = requireString(env, name);
  try {
    new URL(value);
  } catch {
    throw new ConfigError(`environment variable ${name} is not a valid URL: ${value}`);
  }
  return value;
}

function intWithDefault(
  env: Record<string, string | undefined>,
  name: string,
  fallback: number,
): number {
  const raw = env[name];
  if (raw === undefined || raw.trim() === '') return fallback;
  const value = Number(raw);
  if (!Number.isSafeInteger(value) || value <= 0) {
    throw new ConfigError(`environment variable ${name} must be a positive integer, got: ${raw}`);
  }
  return value;
}

/**
 * Every rgb-lib `BitcoinNetwork` variant (src/utils.rs). `Signet` means Bitcoin's
 * DEFAULT signet; a signet with its own challenge is `SignetCustom` and is a
 * different chain to RGB, even though every signet shares the same genesis block
 * hash. Telling the gateway `Signet` for a custom signet fails at first wallet
 * use with an `InvalidIndexer`/"resolver is for another chain-network pair" that
 * names neither the network nor the fix, so the option has to exist here.
 */
const BITCOIN_NETWORKS = [
  'Mainnet',
  'Testnet',
  'Testnet4',
  'Signet',
  'SignetCustom',
  'Regtest',
] as const;

function bitcoinNetwork(
  env: Record<string, string | undefined>,
): (typeof BITCOIN_NETWORKS)[number] {
  const raw = env['GATEWAY_BITCOIN_NETWORK']?.trim();
  if (raw === undefined || raw === '') return 'Regtest';
  const match = BITCOIN_NETWORKS.find((name) => name.toLowerCase() === raw.toLowerCase());
  if (match === undefined) {
    throw new ConfigError(
      `GATEWAY_BITCOIN_NETWORK must be one of ${BITCOIN_NETWORKS.join(', ')}, got: ${raw}`,
    );
  }
  return match;
}

export function loadConfig(env: Record<string, string | undefined> = process.env): GatewayConfig {
  const operatorToken = requireString(env, 'GATEWAY_OPERATOR_TOKEN');
  if (operatorToken.length < 16) {
    throw new ConfigError('GATEWAY_OPERATOR_TOKEN must be at least 16 characters');
  }
  const rgbProxyUrl = requireUrl(env, 'RGB_PROXY_URL');
  const extraTransports = (env['GATEWAY_RGB_TRANSPORT_ALLOWLIST'] ?? '')
    .split(',')
    .map((entry) => entry.trim())
    .filter((entry) => entry !== '');
  return {
    host: env['GATEWAY_HOST']?.trim() || '127.0.0.1',
    port: intWithDefault(env, 'GATEWAY_PORT', 8480),
    rlnUrl: requireUrl(env, 'RLN_URL'),
    rlnAdminToken: env['RLN_ADMIN_TOKEN']?.trim() ?? '',
    esploraUrl: requireUrl(env, 'ESPLORA_URL'),
    rgbProxyUrl,
    sqlitePath: requireString(env, 'GATEWAY_SQLITE_PATH'),
    walletsDir: requireString(env, 'GATEWAY_WALLETS_DIR'),
    walletIndexerUrl: requireString(env, 'GATEWAY_WALLET_INDEXER_URL'),
    bitcoinNetwork: bitcoinNetwork(env),
    walletMaxOpen: intWithDefault(env, 'GATEWAY_WALLET_MAX_OPEN', 32),
    onchainOpTtlSeconds: intWithDefault(env, 'GATEWAY_ONCHAIN_OP_TTL_SECONDS', 600),
    operatorToken,
    floatCapPerUserMsat: intWithDefault(env, 'GATEWAY_FLOAT_CAP_PER_USER_MSAT', 1_000_000_000),
    floatCapGlobalMsat: intWithDefault(env, 'GATEWAY_FLOAT_CAP_GLOBAL_MSAT', 10_000_000_000),
    depositMinConfirmations: intWithDefault(env, 'GATEWAY_DEPOSIT_MIN_CONFIRMATIONS', 1),
    depositTtlSeconds: intWithDefault(env, 'GATEWAY_DEPOSIT_TTL_SECONDS', 86_400),
    assetInvoiceMinMsat: intWithDefault(env, 'GATEWAY_ASSET_INVOICE_MIN_MSAT', 3_000_000),
    rgbTransportAllowlist: [rgbProxyUrl, ...extraTransports.filter((e) => e !== rgbProxyUrl)],
    depositsIntervalMs: intWithDefault(env, 'GATEWAY_DEPOSITS_INTERVAL_MS', 10_000),
    completionsIntervalMs: intWithDefault(env, 'GATEWAY_COMPLETIONS_INTERVAL_MS', 30_000),
    completionsMaxAttempts: intWithDefault(env, 'GATEWAY_COMPLETIONS_MAX_ATTEMPTS', 5),
    reconcilerIntervalMs: intWithDefault(env, 'GATEWAY_RECONCILER_INTERVAL_MS', 10_000),
    reconcilerGraceSeconds: intWithDefault(env, 'GATEWAY_RECONCILER_GRACE_SECONDS', 600),
    queueGlobalConcurrency: intWithDefault(env, 'GATEWAY_QUEUE_GLOBAL_CONCURRENCY', 4),
    queuePerUserDepth: intWithDefault(env, 'GATEWAY_QUEUE_PER_USER_DEPTH', 16),
  };
}
