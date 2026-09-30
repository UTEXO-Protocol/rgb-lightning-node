/**
 * SQLite persistence with a tiny forward-only migration runner.
 *
 * Schema version is tracked in PRAGMA user_version; each migration applies in
 * one transaction so a crash mid-migration leaves the previous version intact.
 * The pre-release development history was squashed into a single v1 before
 * anything shipped; new migrations append from v2.
 */
import Database from 'better-sqlite3';

export type GatewayDb = Database.Database;

interface Migration {
  version: number;
  sql: string;
}

const MIGRATIONS: readonly Migration[] = [
  {
    version: 1,
    sql: `
      CREATE TABLE users (
        id TEXT PRIMARY KEY,
        token_hash TEXT NOT NULL UNIQUE,
        created_at INTEGER NOT NULL
      ) STRICT;

      CREATE TABLE user_xpubs (
        user_id TEXT PRIMARY KEY REFERENCES users(id),
        vanilla TEXT NOT NULL,
        colored TEXT NOT NULL,
        fingerprint TEXT NOT NULL,
        created_at INTEGER NOT NULL
      ) STRICT;

      CREATE TABLE resource_map (
        kind TEXT NOT NULL CHECK (kind IN ('invoice', 'payment_hash', 'asset_transfer', 'swap')),
        resource_id TEXT NOT NULL,
        user_id TEXT NOT NULL REFERENCES users(id),
        state TEXT NOT NULL,
        created_at INTEGER NOT NULL,
        PRIMARY KEY (kind, resource_id)
      ) STRICT;
      CREATE INDEX resource_map_user ON resource_map (user_id, kind);

      CREATE TABLE idempotency (
        user_id TEXT NOT NULL REFERENCES users(id),
        key TEXT NOT NULL,
        request_hash TEXT NOT NULL,
        response TEXT,
        created_at INTEGER NOT NULL,
        PRIMARY KEY (user_id, key)
      ) STRICT;

      CREATE TABLE ledger (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        user_id TEXT NOT NULL REFERENCES users(id),
        asset TEXT NOT NULL,
        delta_msat_or_units INTEGER NOT NULL,
        kind TEXT NOT NULL,
        ref TEXT,
        created_at INTEGER NOT NULL
      ) STRICT;
      CREATE INDEX ledger_user_asset ON ledger (user_id, asset);
      -- Exactly-once backstop for ledger entries that carry a ref: replayed
      -- workers/requests can never double-apply the same (user, asset, kind, ref).
      CREATE UNIQUE INDEX ledger_once
        ON ledger (user_id, asset, kind, ref) WHERE ref IS NOT NULL;

      CREATE TABLE pending_ops (
        id TEXT PRIMARY KEY,
        user_id TEXT NOT NULL REFERENCES users(id),
        kind TEXT NOT NULL CHECK (kind IN ('send_btc', 'send_asset', 'create_utxos')),
        psbt TEXT NOT NULL,
        intent TEXT NOT NULL,
        state TEXT NOT NULL CHECK (state IN ('pending', 'completed', 'expired')),
        txid TEXT,
        created_at INTEGER NOT NULL,
        expires_at INTEGER NOT NULL
      ) STRICT;
      CREATE INDEX pending_ops_user_state ON pending_ops (user_id, state);

      CREATE TABLE pending_deposits (
        id TEXT PRIMARY KEY,
        user_id TEXT NOT NULL REFERENCES users(id),
        kind TEXT NOT NULL CHECK (kind IN ('btc', 'rgb')),
        asset TEXT NOT NULL,
        amount INTEGER NOT NULL,
        target TEXT NOT NULL,
        invoice TEXT,
        state TEXT NOT NULL CHECK (state IN ('pending', 'credited', 'expired')),
        credited_amount INTEGER,
        txid TEXT,
        created_at INTEGER NOT NULL,
        expires_at INTEGER NOT NULL
      ) STRICT;
      CREATE INDEX pending_deposits_state ON pending_deposits (state, user_id);

      CREATE TABLE ln_invoices (
        payment_hash TEXT PRIMARY KEY,
        user_id TEXT NOT NULL REFERENCES users(id),
        invoice TEXT NOT NULL,
        amt_msat INTEGER,
        asset_id TEXT,
        asset_amount INTEGER,
        state TEXT NOT NULL CHECK (state IN ('pending', 'settled', 'expired')),
        created_at INTEGER NOT NULL
      ) STRICT;
      CREATE INDEX ln_invoices_user_state ON ln_invoices (user_id, state);

      CREATE TABLE withdrawals (
        id TEXT PRIMARY KEY,
        user_id TEXT NOT NULL REFERENCES users(id),
        asset TEXT NOT NULL,
        amount INTEGER NOT NULL,
        target TEXT NOT NULL,
        state TEXT NOT NULL CHECK (state IN ('pending', 'sent', 'failed', 'ambiguous')),
        txid TEXT,
        created_at INTEGER NOT NULL
      ) STRICT;
      CREATE INDEX withdrawals_user ON withdrawals (user_id);

      CREATE TABLE worker_state (
        key TEXT PRIMARY KEY,
        value TEXT NOT NULL
      ) STRICT;
    `,
  },
  {
    version: 2,
    // Invoice expiry, so a pending invoice that can no longer be paid stops
    // counting against float-cap headroom even while the reconciler is behind
    // (mirrors pending_deposits.expires_at). Rows written before this
    // migration get their created_at as expiry: they are older than any
    // supported BOLT11 expiry (max 86400s) by the time an upgrade lands, so
    // treating them as expired is the safe, non-blocking default.
    sql: `
      ALTER TABLE ln_invoices ADD COLUMN expires_at INTEGER NOT NULL DEFAULT 0;
      UPDATE ln_invoices SET expires_at = created_at WHERE expires_at = 0;
    `,
  },
  {
    version: 3,
    // Server-side completion recovery, plus address attribution state.
    //
    // `signed_psbt` is retained ONLY on the ambiguous path, where rgb-lib may
    // already have broadcast but its bookkeeping did not finish: without the
    // signed PSBT the gateway cannot finish that bookkeeping itself and the row
    // is stuck forever unless the client retries `complete` (which a client is
    // entitled not to do — a 502 is not proof the send failed). It carries no
    // secret: signatures are public and the transaction is already on the
    // network. `completion_attempts` bounds the retries so a permanently
    // unfinishable op stops burning wallet time and surfaces to an operator.
    //
    // `address_index_high_water` is the ceiling of the address-attribution scan
    // (see wallets/derivation.ts), so attribution keeps working as a user's
    // revealed index grows instead of silently falling off a fixed window.
    sql: `
      ALTER TABLE pending_ops ADD COLUMN signed_psbt TEXT;
      ALTER TABLE pending_ops ADD COLUMN completion_attempts INTEGER NOT NULL DEFAULT 0;
      ALTER TABLE user_xpubs ADD COLUMN address_index_high_water INTEGER NOT NULL DEFAULT 0;
    `,
  },
];

export function schemaVersion(db: GatewayDb): number {
  return db.pragma('user_version', { simple: true }) as number;
}

export function migrate(db: GatewayDb): void {
  for (const migration of MIGRATIONS) {
    if (migration.version <= schemaVersion(db)) continue;
    db.transaction(() => {
      db.exec(migration.sql);
      db.pragma(`user_version = ${migration.version}`);
    })();
  }
}

export function openDb(path: string): GatewayDb {
  const db = new Database(path);
  db.pragma('journal_mode = WAL');
  db.pragma('foreign_keys = ON');
  migrate(db);
  return db;
}
