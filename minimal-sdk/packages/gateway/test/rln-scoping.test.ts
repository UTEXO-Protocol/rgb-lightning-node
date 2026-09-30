import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { createUser } from '../src/auth.js';
import { openDb, type GatewayDb } from '../src/db.js';
import {
  filterOwned,
  ownerOf,
  OwnershipConflictError,
  recordOwnership,
  scopePayments,
  scopeTransfers,
  updateResourceState,
} from '../src/rln/scoping.js';
import type { Payment, Transfer } from '../src/rln/types.js';

function payment(hash: string, paymentType: Payment['payment_type'] = 'Outbound'): Payment {
  return {
    payment_hash: hash,
    payment_type: paymentType,
    status: 'Succeeded',
    created_at: 1_700_000_000,
    updated_at: 1_700_000_100,
    payee_pubkey: '03b79a4bc1ec365524b4fab9a39eb133753646babb5a1da5c4bc94c53110b7795d',
  };
}

function transfer(
  idx: number,
  ids: { recipientId?: string | null; txid?: string | null },
): Transfer {
  return {
    idx,
    created_at: 1_700_000_000,
    updated_at: 1_700_000_100,
    status: 'Settled',
    assignments: [],
    kind: 'ReceiveBlind',
    transport_endpoints: [],
    recipient_id: ids.recipientId ?? null,
    txid: ids.txid ?? null,
  };
}

describe('rln scoping', () => {
  let db: GatewayDb;
  let alice: string;
  let bob: string;

  beforeEach(() => {
    db = openDb(':memory:');
    alice = createUser(db).userId;
    bob = createUser(db).userId;
  });

  afterEach(() => {
    db.close();
  });

  it('records ownership at creation time and resolves the owner', () => {
    recordOwnership(db, { kind: 'invoice', resourceId: 'hash1', userId: alice, state: 'pending' });
    expect(ownerOf(db, 'invoice', 'hash1')).toBe(alice);
    expect(ownerOf(db, 'invoice', 'hash1')).not.toBe(bob);
    expect(ownerOf(db, 'payment_hash', 'hash1')).toBeUndefined();
  });

  it('lets the owner re-record with a new state but never transfers ownership', () => {
    recordOwnership(db, { kind: 'invoice', resourceId: 'hash1', userId: alice, state: 'pending' });
    recordOwnership(db, { kind: 'invoice', resourceId: 'hash1', userId: alice, state: 'settled' });
    const row = db
      .prepare("SELECT state FROM resource_map WHERE kind = 'invoice' AND resource_id = 'hash1'")
      .get() as { state: string };
    expect(row.state).toBe('settled');
    expect(() =>
      recordOwnership(db, { kind: 'invoice', resourceId: 'hash1', userId: bob, state: 'pending' }),
    ).toThrow(OwnershipConflictError);
    expect(ownerOf(db, 'invoice', 'hash1')).toBe(alice);
  });

  it('updates state only for the owner', () => {
    recordOwnership(db, {
      kind: 'payment_hash',
      resourceId: 'hash2',
      userId: alice,
      state: 'pending',
    });
    expect(updateResourceState(db, 'payment_hash', 'hash2', bob, 'settled')).toBe(false);
    expect(updateResourceState(db, 'payment_hash', 'hash2', alice, 'settled')).toBe(true);
    const row = db
      .prepare(
        "SELECT state FROM resource_map WHERE kind = 'payment_hash' AND resource_id = 'hash2'",
      )
      .get() as { state: string };
    expect(row.state).toBe('settled');
  });

  it('scopes payments: a foreign payment_hash is invisible to another user', () => {
    recordOwnership(db, { kind: 'payment_hash', resourceId: 'aaa', userId: alice, state: 'sent' });
    recordOwnership(db, { kind: 'invoice', resourceId: 'bbb', userId: alice, state: 'pending' });
    recordOwnership(db, { kind: 'payment_hash', resourceId: 'ccc', userId: bob, state: 'sent' });
    const nodeWide = [
      payment('aaa'),
      payment('bbb', 'InboundAutoClaim'),
      payment('ccc'),
      payment('unowned'),
    ];

    const aliceView = scopePayments(db, alice, nodeWide);
    expect(aliceView.map((p) => p.payment_hash)).toEqual(['aaa', 'bbb']);

    const bobView = scopePayments(db, bob, nodeWide);
    expect(bobView.map((p) => p.payment_hash)).toEqual(['ccc']);
    expect(bobView.some((p) => p.payment_hash === 'aaa')).toBe(false);
  });

  it('scopes payments per direction, so an outbound claim cannot read an inbound one', () => {
    // Alice created the invoice; Bob claimed the same hash under
    // 'payment_hash' (decodeLnInvoice accepts any self-signed BOLT11, so the
    // hash is his to choose). Bob must still see nothing.
    recordOwnership(db, { kind: 'invoice', resourceId: 'aaa', userId: alice, state: 'pending' });
    recordOwnership(db, { kind: 'payment_hash', resourceId: 'aaa', userId: bob, state: 'failed' });
    const inbound = [payment('aaa', 'InboundAutoClaim')];

    expect(scopePayments(db, alice, inbound).map((p) => p.payment_hash)).toEqual(['aaa']);
    expect(scopePayments(db, bob, inbound)).toEqual([]);
    // ...and symmetrically, Alice's invoice claim does not expose Bob's send.
    const outbound = [payment('aaa', 'Outbound')];
    expect(scopePayments(db, bob, outbound).map((p) => p.payment_hash)).toEqual(['aaa']);
    expect(scopePayments(db, alice, outbound)).toEqual([]);
  });

  it('scopes transfers by recipient_id and by txid, dropping unattributable ones', () => {
    recordOwnership(db, {
      kind: 'asset_transfer',
      resourceId: 'recipient-a',
      userId: alice,
      state: 'pending',
    });
    recordOwnership(db, {
      kind: 'asset_transfer',
      resourceId: 'txid-a',
      userId: alice,
      state: 'sent',
    });
    recordOwnership(db, {
      kind: 'asset_transfer',
      resourceId: 'recipient-b',
      userId: bob,
      state: 'pending',
    });
    const nodeWide = [
      transfer(1, { recipientId: 'recipient-a' }),
      transfer(2, { txid: 'txid-a' }),
      transfer(3, { recipientId: 'recipient-b', txid: 'txid-b' }),
      transfer(4, {}),
    ];

    expect(scopeTransfers(db, alice, nodeWide).map((t) => t.idx)).toEqual([1, 2]);
    expect(scopeTransfers(db, bob, nodeWide).map((t) => t.idx)).toEqual([3]);
  });

  it('filterOwned drops items with missing or empty ids', () => {
    recordOwnership(db, { kind: 'invoice', resourceId: 'x', userId: alice, state: 'pending' });
    const items = [{ id: 'x' }, { id: '' }, { id: null }, { id: undefined }];
    const kept = filterOwned(db, alice, ['invoice'], items, (item) => item.id);
    expect(kept).toEqual([{ id: 'x' }]);
  });
});
