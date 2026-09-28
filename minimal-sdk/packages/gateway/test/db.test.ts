import { describe, expect, it } from 'vitest';
import { migrate, openDb, schemaVersion } from '../src/db.js';

/** Bump alongside a new MIGRATIONS entry in src/db.ts. */
const LATEST_SCHEMA_VERSION = 3;

describe('db migrations', () => {
  it('upgrades a v1 database in place, backfilling ln_invoices.expires_at', () => {
    const db = openDb(':memory:');
    // Simulate a database created before migration v2: every later migration's
    // columns have to go, or re-running them fails on the ones openDb added.
    db.exec('ALTER TABLE ln_invoices DROP COLUMN expires_at');
    db.exec('ALTER TABLE pending_ops DROP COLUMN signed_psbt');
    db.exec('ALTER TABLE pending_ops DROP COLUMN completion_attempts');
    db.exec('ALTER TABLE user_xpubs DROP COLUMN address_index_high_water');
    db.pragma('user_version = 1');
    db.prepare('INSERT INTO users (id, token_hash, created_at) VALUES (?, ?, ?)').run(
      'u_1',
      'hash',
      1,
    );
    db.prepare(
      `INSERT INTO ln_invoices (payment_hash, user_id, invoice, amt_msat, asset_id, asset_amount, state, created_at)
       VALUES ('h1', 'u_1', 'lnbc1', 1000, NULL, NULL, 'pending', 4242)`,
    ).run();

    migrate(db);

    expect(schemaVersion(db)).toBe(LATEST_SCHEMA_VERSION);
    // Legacy rows inherit created_at, i.e. they are already expired and stop
    // pinning float-cap headroom rather than pinning it forever.
    const row = db.prepare('SELECT expires_at FROM ln_invoices WHERE payment_hash = ?').get('h1');
    expect((row as { expires_at: number }).expires_at).toBe(4242);
    db.close();
  });

  it('creates the full schema on open', () => {
    const db = openDb(':memory:');
    expect(schemaVersion(db)).toBe(LATEST_SCHEMA_VERSION);
    const tables = db
      .prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
      .all()
      .map((row) => (row as { name: string }).name);
    for (const table of [
      'users',
      'user_xpubs',
      'resource_map',
      'idempotency',
      'ledger',
      'pending_ops',
      'pending_deposits',
      'ln_invoices',
      'withdrawals',
      'worker_state',
    ]) {
      expect(tables).toContain(table);
    }
    const depositColumns = db
      .prepare('PRAGMA table_info(pending_deposits)')
      .all()
      .map((row) => (row as { name: string }).name);
    expect(depositColumns).toContain('expires_at');
    const invoiceColumns = db
      .prepare('PRAGMA table_info(ln_invoices)')
      .all()
      .map((row) => (row as { name: string }).name);
    expect(invoiceColumns).toContain('expires_at');
    // v3: server-side completion recovery and address attribution state.
    const opColumns = db
      .prepare('PRAGMA table_info(pending_ops)')
      .all()
      .map((row) => (row as { name: string }).name);
    expect(opColumns).toContain('signed_psbt');
    expect(opColumns).toContain('completion_attempts');
    const xpubColumns = db
      .prepare('PRAGMA table_info(user_xpubs)')
      .all()
      .map((row) => (row as { name: string }).name);
    expect(xpubColumns).toContain('address_index_high_water');
    db.close();
  });

  it('upgrades a v2 database in place, adding the completion-recovery columns', () => {
    const db = openDb(':memory:');
    db.exec('ALTER TABLE pending_ops DROP COLUMN signed_psbt');
    db.exec('ALTER TABLE pending_ops DROP COLUMN completion_attempts');
    db.exec('ALTER TABLE user_xpubs DROP COLUMN address_index_high_water');
    db.pragma('user_version = 2');
    db.prepare('INSERT INTO users (id, token_hash, created_at) VALUES (?, ?, ?)').run(
      'u_1',
      'hash',
      1,
    );
    db.prepare(
      `INSERT INTO pending_ops (id, user_id, kind, psbt, intent, state, txid, created_at, expires_at)
       VALUES ('op-1', 'u_1', 'send_btc', 'psbt', '{}', 'pending', 'txid-1', 1, 2)`,
    ).run();

    migrate(db);

    expect(schemaVersion(db)).toBe(LATEST_SCHEMA_VERSION);
    // An op that was already ambiguous before the upgrade has no retained PSBT,
    // so the worker cannot finish it — it stays an operator job, not a crash.
    const row = db
      .prepare('SELECT signed_psbt, completion_attempts FROM pending_ops WHERE id = ?')
      .get('op-1') as { signed_psbt: string | null; completion_attempts: number };
    expect(row.signed_psbt).toBeNull();
    expect(row.completion_attempts).toBe(0);
    db.close();
  });

  it('is idempotent when re-run', () => {
    const db = openDb(':memory:');
    migrate(db);
    migrate(db);
    expect(schemaVersion(db)).toBe(LATEST_SCHEMA_VERSION);
    db.close();
  });

  it('enforces the ledger exactly-once index on refs', () => {
    const db = openDb(':memory:');
    db.prepare('INSERT INTO users (id, token_hash, created_at) VALUES (?, ?, ?)').run(
      'u_1',
      'hash',
      1,
    );
    const insert = db.prepare(
      `INSERT INTO ledger (user_id, asset, delta_msat_or_units, kind, ref, created_at)
       VALUES ('u_1', 'btc', 5, 'deposit', ?, 1)`,
    );
    expect(() => insert.run('ref-1')).not.toThrow();
    expect(() => insert.run('ref-1')).toThrow(/UNIQUE/);
    // NULL refs are exempt from deduplication.
    expect(() => insert.run(null)).not.toThrow();
    expect(() => insert.run(null)).not.toThrow();
    db.close();
  });

  it('rejects unknown pending-op kinds and states via CHECK constraints', () => {
    const db = openDb(':memory:');
    db.prepare('INSERT INTO users (id, token_hash, created_at) VALUES (?, ?, ?)').run(
      'u_1',
      'hash',
      1,
    );
    const insert = db.prepare(
      `INSERT INTO pending_ops (id, user_id, kind, psbt, intent, state, txid, created_at, expires_at)
       VALUES (?, 'u_1', ?, 'p', '{}', ?, NULL, 1, 2)`,
    );
    expect(() => insert.run('op1', 'send_btc', 'pending')).not.toThrow();
    expect(() => insert.run('op2', 'bogus', 'pending')).toThrow(/CHECK/);
    expect(() => insert.run('op3', 'send_asset', 'bogus')).toThrow(/CHECK/);
    db.close();
  });

  it('rejects unknown resource kinds via CHECK constraint', () => {
    const db = openDb(':memory:');
    db.prepare('INSERT INTO users (id, token_hash, created_at) VALUES (?, ?, ?)').run(
      'u_1',
      'hash',
      1,
    );
    const insert = db.prepare(
      'INSERT INTO resource_map (kind, resource_id, user_id, state, created_at) VALUES (?, ?, ?, ?, ?)',
    );
    expect(() => insert.run('invoice', 'r1', 'u_1', 'pending', 1)).not.toThrow();
    expect(() => insert.run('bogus', 'r2', 'u_1', 'pending', 1)).toThrow(/CHECK/);
    db.close();
  });

  it('enforces one idempotency row per (user, key)', () => {
    const db = openDb(':memory:');
    db.prepare('INSERT INTO users (id, token_hash, created_at) VALUES (?, ?, ?)').run(
      'u_1',
      'hash',
      1,
    );
    const insert = db.prepare(
      'INSERT INTO idempotency (user_id, key, request_hash, response, created_at) VALUES (?, ?, ?, NULL, ?)',
    );
    insert.run('u_1', 'k1', 'h1', 1);
    expect(() => insert.run('u_1', 'k1', 'h2', 2)).toThrow(/UNIQUE|PRIMARY/);
    db.close();
  });
});
