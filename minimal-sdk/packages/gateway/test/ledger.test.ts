import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { openDb, type GatewayDb } from '../src/db.js';
import { BTC_ASSET, InsufficientBalanceError, Ledger } from '../src/ledger.js';

// Float caps are NOT a ledger concern (see the ledger.ts header): they are a
// policy on NEW exposure, enforced by LnFlows.assertBtcHeadroom, and covered
// per-user and globally in ln-routes.test.ts. Once funds have arrived, a
// credit must always succeed, so there is nothing to cap here.
const ASSET = 'rgb:testAsset-000';

describe('Ledger', () => {
  let db: GatewayDb;
  let ledger: Ledger;

  beforeEach(() => {
    db = openDb(':memory:');
    for (const id of ['user-a', 'user-b', 'user-c']) {
      db.prepare('INSERT INTO users (id, token_hash, created_at) VALUES (?, ?, ?)').run(
        id,
        `hash-${id}`,
        Date.now(),
      );
    }
    ledger = new Ledger(db);
  });

  afterEach(() => {
    db.close();
  });

  const credit = (userId: string, amount: number, ref?: string) =>
    ledger.credit({
      userId,
      asset: BTC_ASSET,
      amount,
      kind: 'deposit',
      ...(ref !== undefined ? { ref } : {}),
    });

  it('tracks balances as the sum of deltas', () => {
    credit('user-a', 600_000);
    ledger.debit({ userId: 'user-a', asset: BTC_ASSET, amount: 250_000, kind: 'ln_out' });
    expect(ledger.balance('user-a', BTC_ASSET)).toBe(350_000);
    expect(ledger.balance('user-b', BTC_ASSET)).toBe(0);
  });

  it('rejects debits beyond the balance', () => {
    credit('user-a', 100_000);
    expect(() =>
      ledger.debit({ userId: 'user-a', asset: BTC_ASSET, amount: 100_001, kind: 'ln_out' }),
    ).toThrowError(InsufficientBalanceError);
    expect(ledger.balance('user-a', BTC_ASSET)).toBe(100_000);
  });

  it('keeps per-asset balances independent', () => {
    credit('user-a', 100_000);
    ledger.credit({ userId: 'user-a', asset: ASSET, amount: 42, kind: 'deposit', ref: 'd2' });
    expect(() =>
      ledger.debit({ userId: 'user-a', asset: ASSET, amount: 43, kind: 'ln_out' }),
    ).toThrowError(InsufficientBalanceError);
    expect(ledger.balances('user-a')).toEqual({ [BTC_ASSET]: 100_000, [ASSET]: 42 });
  });

  it('applies creditOnce exactly once per ref', () => {
    const params = {
      userId: 'user-a',
      asset: BTC_ASSET,
      amount: 50_000,
      kind: 'deposit' as const,
      ref: 'dep-1',
    };
    expect(ledger.creditOnce(params)).toBe(true);
    expect(ledger.creditOnce(params)).toBe(false);
    expect(ledger.balance('user-a', BTC_ASSET)).toBe(50_000);
  });

  it('requires a ref on creditOnce', () => {
    expect(() =>
      ledger.creditOnce({ userId: 'user-a', asset: BTC_ASSET, amount: 1, kind: 'deposit' }),
    ).toThrowError(TypeError);
  });

  it('rejects zero, negative and non-integer amounts', () => {
    for (const amount of [0, -1, 1.5, Number.NaN, Number.POSITIVE_INFINITY, 2 ** 53]) {
      expect(() =>
        ledger.credit({ userId: 'user-a', asset: BTC_ASSET, amount, kind: 'deposit' }),
      ).toThrowError(RangeError);
      expect(() =>
        ledger.debit({ userId: 'user-a', asset: BTC_ASSET, amount, kind: 'withdraw' }),
      ).toThrowError(RangeError);
    }
    expect(ledger.entries('user-a')).toEqual([]);
  });

  it('backstops exactly-once at the database level (unique index)', () => {
    credit('user-a', 10_000, 'dup-ref');
    expect(() =>
      db
        .prepare(
          `INSERT INTO ledger (user_id, asset, delta_msat_or_units, kind, ref, created_at)
           VALUES ('user-a', ?, 10000, 'deposit', 'dup-ref', 0)`,
        )
        .run(BTC_ASSET),
    ).toThrowError(/UNIQUE/);
  });

  it('property: the balance always equals the sum of deltas', () => {
    // Deterministic LCG so failures are reproducible.
    let seed = 0xdecafbad;
    const rand = () => {
      seed = (seed * 1_664_525 + 1_013_904_223) >>> 0;
      return seed / 0x1_0000_0000;
    };
    const users = ['user-a', 'user-b'];
    const assets = [BTC_ASSET, ASSET];
    const expected = new Map<string, number>();
    for (let i = 0; i < 400; i += 1) {
      const userId = users[Math.floor(rand() * users.length)] as string;
      const asset = assets[Math.floor(rand() * assets.length)] as string;
      const key = `${userId}/${asset}`;
      const current = expected.get(key) ?? 0;
      const amount = 1 + Math.floor(rand() * 10_000);
      if (rand() < 0.5 && current >= amount) {
        ledger.debit({ userId, asset, amount, kind: 'ln_out' });
        expected.set(key, current - amount);
      } else {
        ledger.credit({ userId, asset, amount, kind: 'ln_in' });
        expected.set(key, current + amount);
      }
    }
    for (const [key, value] of expected) {
      const [userId, asset] = key.split('/') as [string, string];
      expect(ledger.balance(userId, asset)).toBe(value);
    }
    for (const asset of assets) {
      const total = users.reduce((sum, u) => sum + (expected.get(`${u}/${asset}`) ?? 0), 0);
      expect(ledger.globalBalance(asset)).toBe(total);
    }
  });
});
