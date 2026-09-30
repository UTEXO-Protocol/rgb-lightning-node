import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { openDb, type GatewayDb } from '../src/db.js';
import { BTC_ASSET, Ledger } from '../src/ledger.js';
import { DepositsWorker } from '../src/workers/deposits.js';
import { fakeRln, type RlnOverrides } from './ln-mocks.js';

const USER = 'user-a';
const ADDRESS = 'bcrt1qdeposittarget';
const RECIPIENT = 'bcrt:utxob:testRecipient';
const ASSET = 'rgb:testAsset-000';

interface EsploraFixture {
  tipHeight: number;
  txs: unknown[];
}

function esploraFetch(fixture: EsploraFixture): typeof fetch {
  return (async (input: string | URL | Request) => {
    const url = String(input);
    const body = url.endsWith('/blocks/tip/height')
      ? JSON.stringify(fixture.tipHeight)
      : JSON.stringify(fixture.txs);
    return new Response(body, { status: 200, headers: { 'content-type': 'application/json' } });
  }) as typeof fetch;
}

function confirmedTx(txid: string, blockHeight: number, address: string, valueSat: number) {
  return {
    txid,
    status: { confirmed: true, block_height: blockHeight },
    vout: [
      { scriptpubkey_address: address, value: valueSat },
      { scriptpubkey_address: 'bcrt1qsomeoneelse', value: 999 },
    ],
  };
}

describe('DepositsWorker', () => {
  let db: GatewayDb;
  let ledger: Ledger;

  beforeEach(() => {
    db = openDb(':memory:');
    db.prepare('INSERT INTO users (id, token_hash, created_at) VALUES (?, ?, ?)').run(
      USER,
      'hash-a',
      Date.now(),
    );
    ledger = new Ledger(db);
  });

  afterEach(() => {
    db.close();
  });

  function insertDeposit(
    kind: 'btc' | 'rgb',
    asset: string,
    amount: number,
    target: string,
    expiresAt: number = Date.now() + 86_400_000,
  ): string {
    const id = `dep-${kind}-${target}`;
    db.prepare(
      `INSERT INTO pending_deposits (id, user_id, kind, asset, amount, target, invoice, state, created_at, expires_at)
       VALUES (?, ?, ?, ?, ?, ?, NULL, 'pending', ?, ?)`,
    ).run(id, USER, kind, asset, amount, target, Date.now(), expiresAt);
    return id;
  }

  function worker(fixture: EsploraFixture, overrides: RlnOverrides = {}, minConf = 1) {
    return new DepositsWorker({
      db,
      ledger,
      rln: fakeRln(overrides),
      esploraUrl: 'http://esplora.test',
      minConfirmations: minConf,
      fetchImpl: esploraFetch(fixture),
    });
  }

  function depositState(id: string): { state: string; credited_amount: number | null } {
    return db
      .prepare('SELECT state, credited_amount FROM pending_deposits WHERE id = ?')
      .get(id) as { state: string; credited_amount: number | null };
  }

  it('credits a confirmed btc deposit exactly once across replayed runs', async () => {
    const id = insertDeposit('btc', BTC_ASSET, 500_000, ADDRESS);
    const w = worker({ tipHeight: 105, txs: [confirmedTx('tx1', 103, ADDRESS, 700)] });
    await w.runOnce();
    expect(ledger.balance(USER, BTC_ASSET)).toBe(700_000);
    expect(depositState(id)).toEqual({ state: 'credited', credited_amount: 700_000 });

    // Replays (same watcher and a fresh one) never double-credit.
    await w.runOnce();
    const fresh = worker({ tipHeight: 110, txs: [confirmedTx('tx1', 103, ADDRESS, 700)] });
    await fresh.runOnce();
    expect(ledger.balance(USER, BTC_ASSET)).toBe(700_000);
  });

  it('ignores unconfirmed or under-confirmed transactions', async () => {
    const id = insertDeposit('btc', BTC_ASSET, 500_000, ADDRESS);
    const unconfirmed = {
      txid: 'tx-mempool',
      status: { confirmed: false },
      vout: [{ scriptpubkey_address: ADDRESS, value: 700 }],
    };
    const w = worker({ tipHeight: 105, txs: [unconfirmed] }, {}, 1);
    await w.runOnce();
    expect(ledger.balance(USER, BTC_ASSET)).toBe(0);
    expect(depositState(id).state).toBe('pending');

    // Confirmed but below the required depth.
    const shallow = worker({ tipHeight: 105, txs: [confirmedTx('tx1', 104, ADDRESS, 700)] }, {}, 3);
    await shallow.runOnce();
    expect(ledger.balance(USER, BTC_ASSET)).toBe(0);
  });

  it('credits a settled rgb deposit exactly once across replayed runs', async () => {
    const id = insertDeposit('rgb', ASSET, 42, RECIPIENT);
    const overrides: RlnOverrides = {
      refreshTransfers: () => ({ transfers: [] }),
      listTransfers: () => ({
        transfers: [
          {
            idx: 1,
            created_at: 0,
            updated_at: 0,
            status: 'Settled',
            requested_assignment: null,
            assignments: [{ type: 'Fungible', value: 42 }],
            kind: 'ReceiveBlind',
            txid: 'rgb-txid',
            recipient_id: RECIPIENT,
            transport_endpoints: [],
          },
        ],
        first_index_offset: 1,
        last_index_offset: 1,
      }),
    };
    const w = worker({ tipHeight: 0, txs: [] }, overrides);
    await w.runOnce();
    expect(ledger.balance(USER, ASSET)).toBe(42);
    expect(depositState(id)).toEqual({ state: 'credited', credited_amount: 42 });

    await w.runOnce();
    expect(ledger.balance(USER, ASSET)).toBe(42);
  });

  it('expires a stale unfunded deposit', async () => {
    const id = insertDeposit('btc', BTC_ASSET, 500_000, ADDRESS, Date.now() - 1);
    const w = worker({ tipHeight: 105, txs: [] });
    await w.runOnce();
    expect(depositState(id).state).toBe('expired');
    expect(ledger.balance(USER, BTC_ASSET)).toBe(0);
  });

  it('credits a funded past-due deposit on the last-chance poll instead of expiring it', async () => {
    // Funding confirmed right at the TTL boundary: the pass polls BEFORE the
    // expiry sweep, so funds the node already holds are credited, not lost.
    const id = insertDeposit('btc', BTC_ASSET, 500_000, ADDRESS, Date.now() - 1);
    const w = worker({ tipHeight: 105, txs: [confirmedTx('tx1', 103, ADDRESS, 500)] });
    await w.runOnce();
    expect(depositState(id)).toEqual({ state: 'credited', credited_amount: 500_000 });
    expect(ledger.balance(USER, BTC_ASSET)).toBe(500_000);
  });

  it('credits an underpayment at its confirmed amount and closes the intent (one-shot)', async () => {
    const id = insertDeposit('btc', BTC_ASSET, 500_000, ADDRESS);
    const w = worker({ tipHeight: 105, txs: [confirmedTx('tx1', 103, ADDRESS, 300)] });
    await w.runOnce();
    // Documented one-shot semantics: the first confirmed poll credits what
    // arrived (even below the declared amount) and closes the intent — a
    // second payment needs a fresh deposit prepare.
    expect(depositState(id)).toEqual({ state: 'credited', credited_amount: 300_000 });
    expect(ledger.balance(USER, BTC_ASSET)).toBe(300_000);
  });

  it('keeps a deposit pending and logs when esplora answers non-OK', async () => {
    const id = insertDeposit('btc', BTC_ASSET, 500_000, ADDRESS);
    const warnings: unknown[] = [];
    const w = new DepositsWorker({
      db,
      ledger,
      rln: fakeRln({}),
      esploraUrl: 'http://esplora.test',
      minConfirmations: 1,
      fetchImpl: (async () => new Response('boom', { status: 500 })) as typeof fetch,
      log: { warn: (obj) => warnings.push(obj) },
    });
    await w.runOnce();
    expect(depositState(id).state).toBe('pending');
    expect(warnings).toHaveLength(1);
  });

  it('skips an overlapping pass while one is already running', async () => {
    insertDeposit('btc', BTC_ASSET, 500_000, ADDRESS);
    let resolveFetch: (() => void) | undefined;
    let fetches = 0;
    const w = new DepositsWorker({
      db,
      ledger,
      rln: fakeRln({}),
      esploraUrl: 'http://esplora.test',
      minConfirmations: 1,
      fetchImpl: (async (input: string | URL | Request) => {
        fetches += 1;
        // Only the first fetch parks; later ones answer so the pass finishes.
        if (fetches === 1) {
          await new Promise<void>((resolve) => {
            resolveFetch = resolve;
          });
        }
        const body = String(input).endsWith('/blocks/tip/height') ? '0' : '[]';
        return new Response(body, { status: 200 });
      }) as typeof fetch,
    });
    const first = w.runOnce();
    // Wait until the first pass is parked inside its fetch.
    while (resolveFetch === undefined) await new Promise((r) => setTimeout(r, 1));
    await w.runOnce(); // must return immediately without a second fetch
    expect(fetches).toBe(1);
    resolveFetch();
    await first;
  });

  it('sums every confirmed payment to the address and records the first txid', async () => {
    const id = insertDeposit('btc', BTC_ASSET, 500_000, ADDRESS);
    const w = worker({
      tipHeight: 105,
      txs: [confirmedTx('tx-newer', 104, ADDRESS, 200), confirmedTx('tx-older', 101, ADDRESS, 500)],
    });
    await w.runOnce();
    expect(depositState(id)).toEqual({ state: 'credited', credited_amount: 700_000 });
    const txid = db.prepare('SELECT txid FROM pending_deposits WHERE id = ?').get(id);
    expect((txid as { txid: string }).txid).toBe('tx-newer');
  });

  it('pages past newer settled transfers of the same asset to find the deposit', async () => {
    // RLN's /listtransfers is node-wide and capped at 100 per page: without
    // pagination this deposit is invisible once 100 newer settled transfers
    // exist for the asset, and would never be credited.
    const id = insertDeposit('rgb', ASSET, 42, RECIPIENT);
    const filler = (idx: number) => ({
      idx,
      created_at: 0,
      updated_at: 0,
      status: 'Settled',
      requested_assignment: null,
      assignments: [{ type: 'Fungible', value: 1 }],
      kind: 'ReceiveBlind',
      txid: `filler-${idx}`,
      recipient_id: `other-${idx}`,
      transport_endpoints: [],
    });
    const seenOffsets: (number | undefined)[] = [];
    const w = worker(
      { tipHeight: 0, txs: [] },
      {
        refreshTransfers: () => ({ transfers: [] }),
        listTransfers: (request: { index_offset?: number; status?: string }) => {
          seenOffsets.push(request.index_offset);
          expect(request.status).toBe('Settled');
          if (request.index_offset === undefined) {
            return {
              transfers: Array.from({ length: 100 }, (_, i) => filler(200 - i)),
              first_index_offset: 200,
              last_index_offset: 101,
            };
          }
          return {
            transfers: [
              {
                ...filler(1),
                recipient_id: RECIPIENT,
                txid: 'rgb-txid',
                assignments: [{ type: 'Fungible', value: 42 }],
              },
            ],
            first_index_offset: 1,
            last_index_offset: 1,
          };
        },
      },
    );
    await w.runOnce();
    expect(seenOffsets).toEqual([undefined, 101]);
    expect(ledger.balance(USER, ASSET)).toBe(42);
    expect(depositState(id)).toEqual({ state: 'credited', credited_amount: 42 });
  });

  it('does not credit a settled transfer that carries no fungible assignment', async () => {
    // Crediting the user-declared amount here would mint units the node may
    // not hold: RGB credits are exempt from the float caps, so the only safe
    // answer is to leave the intent pending and surface it.
    const id = insertDeposit('rgb', ASSET, 42, RECIPIENT);
    const warnings: unknown[] = [];
    const w = new DepositsWorker({
      db,
      ledger,
      rln: fakeRln({
        refreshTransfers: () => ({ transfers: [] }),
        listTransfers: () => ({
          transfers: [
            {
              idx: 1,
              created_at: 0,
              updated_at: 0,
              status: 'Settled',
              requested_assignment: null,
              assignments: [],
              kind: 'ReceiveBlind',
              txid: 'rgb-txid',
              recipient_id: RECIPIENT,
              transport_endpoints: [],
            },
          ],
          first_index_offset: 1,
          last_index_offset: 1,
        }),
      }),
      esploraUrl: 'http://esplora.test',
      minConfirmations: 1,
      fetchImpl: esploraFetch({ tipHeight: 0, txs: [] }),
      log: { warn: (obj) => warnings.push(obj) },
    });
    await w.runOnce();
    expect(ledger.balance(USER, ASSET)).toBe(0);
    expect(depositState(id).state).toBe('pending');
    expect(warnings).toHaveLength(1);
  });

  it('leaves an rgb deposit pending until its transfer settles', async () => {
    const id = insertDeposit('rgb', ASSET, 42, RECIPIENT);
    const w = worker(
      { tipHeight: 0, txs: [] },
      {
        refreshTransfers: () => ({ transfers: [] }),
        listTransfers: () => ({
          transfers: [
            {
              idx: 1,
              created_at: 0,
              updated_at: 0,
              status: 'WaitingConfirmations',
              requested_assignment: null,
              assignments: [],
              kind: 'ReceiveBlind',
              txid: null,
              recipient_id: RECIPIENT,
              transport_endpoints: [],
            },
          ],
          first_index_offset: 1,
          last_index_offset: 1,
        }),
      },
    );
    await w.runOnce();
    expect(ledger.balance(USER, ASSET)).toBe(0);
    expect(depositState(id).state).toBe('pending');
  });
});
