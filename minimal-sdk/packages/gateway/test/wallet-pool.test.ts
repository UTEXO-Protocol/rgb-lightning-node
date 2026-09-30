import { describe, expect, it } from 'vitest';
import type { WalletIdentity } from '../src/wallets/backend.js';
import { WalletPool } from '../src/wallets/pool.js';
import { WalletOpQueues } from '../src/wallets/queue.js';
import { deferred } from './helpers.js';
import { MockWalletBackend } from './wallet-mocks.js';

function identity(userId: string): WalletIdentity {
  return {
    userId,
    xpubs: { vanilla: `van-${userId}`, colored: `col-${userId}`, fingerprint: 'aabbccdd' },
  };
}

function makePool(maxOpen: number) {
  const backend = new MockWalletBackend();
  const queues = new WalletOpQueues();
  const pool = new WalletPool(backend, queues, { maxOpen });
  return { backend, queues, pool };
}

describe('WalletPool', () => {
  it('opens lazily and reuses the open handle', async () => {
    const { backend, pool } = makePool(4);
    await pool.withWallet(identity('u1'), (w) => w.getAddress());
    await pool.withWallet(identity('u1'), (w) => w.getAddress());
    expect(backend.openCalls).toHaveLength(1);
    expect(pool.openCount()).toBe(1);
  });

  it('evicts the least-recently-used wallet beyond maxOpen and closes it', async () => {
    const { backend, pool } = makePool(2);
    await pool.withWallet(identity('u1'), (w) => w.getAddress());
    await pool.withWallet(identity('u2'), (w) => w.getAddress());
    // Touch u1 so u2 becomes the LRU.
    await pool.withWallet(identity('u1'), (w) => w.getAddress());
    await pool.withWallet(identity('u3'), (w) => w.getAddress());

    expect(pool.openCount()).toBe(2);
    // Eviction close is queued; give it a tick.
    await new Promise((resolve) => setImmediate(resolve));
    expect(backend.handleFor('u2')?.closed).toBe(true);
    expect(backend.handleFor('u1')?.closed).toBe(false);
    expect(backend.handleFor('u3')?.closed).toBe(false);
  });

  it('reopens an evicted wallet on next use', async () => {
    const { backend, pool } = makePool(1);
    await pool.withWallet(identity('u1'), (w) => w.getAddress());
    await pool.withWallet(identity('u2'), (w) => w.getAddress());
    await pool.withWallet(identity('u1'), (w) => w.getAddress());
    const u1Opens = backend.openCalls.filter((call) => call.userId === 'u1');
    expect(u1Opens).toHaveLength(2);
  });

  it('serializes operations per wallet (single-flight through the queue)', async () => {
    const { backend, pool } = makePool(4);
    await pool.withWallet(identity('u1'), (w) => w.getAddress());
    const handle = backend.handleFor('u1');
    if (handle === undefined) throw new Error('no handle');

    const gate = deferred<void>();
    let firstDone = false;
    handle.gate = () => gate.promise;
    const first = pool.withWallet(identity('u1'), async (w) => {
      await w.sync();
      firstDone = true;
    });
    const second = pool.withWallet(identity('u1'), async (w) => {
      // Must observe the first operation completed (strict FIFO).
      expect(firstDone).toBe(true);
      await w.refresh();
    });
    gate.resolve();
    await Promise.all([first, second]);
    expect(handle.syncCount).toBe(1);
    expect(handle.refreshCount).toBe(1);
  });

  it('does not interrupt an in-flight operation when evicting', async () => {
    const { backend, pool } = makePool(1);
    await pool.withWallet(identity('u1'), (w) => w.getAddress());
    const handle = backend.handleFor('u1');
    if (handle === undefined) throw new Error('no handle');

    const gate = deferred<void>();
    handle.gate = () => gate.promise;
    const slow = pool.withWallet(identity('u1'), (w) => w.sync());
    // Trigger eviction of u1 while its sync is queued/running.
    const other = pool.withWallet(identity('u2'), (w) => w.getAddress());
    gate.resolve();
    await Promise.all([slow, other]);
    // close must come after the in-flight sync on the operation log.
    const closeIndex = handle.operations.indexOf('close');
    const syncIndex = handle.operations.indexOf('sync');
    expect(closeIndex).toBeGreaterThan(syncIndex);
    expect(handle.syncCount).toBe(1);
  });

  it('evict closes the cached wallet and forces a reopen', async () => {
    // WalletService.registerXpubs relies on this after a failed persist: the
    // handle was opened for xpubs that were never stored, so a retry with
    // different xpubs must not be served from it.
    const { backend, pool } = makePool(4);
    await pool.withWallet(identity('u1'), (w) => w.getAddress());
    const first = backend.handleFor('u1');

    pool.evict('u1');
    expect(pool.openCount()).toBe(0);
    await new Promise((resolve) => setImmediate(resolve));
    expect(first?.closed).toBe(true);

    await pool.withWallet(identity('u1'), (w) => w.getAddress());
    expect(backend.openCalls.filter((call) => call.userId === 'u1')).toHaveLength(2);
    expect(backend.handleFor('u1')).not.toBe(first);
  });

  it('evict is a no-op for a user with no open wallet', () => {
    const { backend, pool } = makePool(4);
    expect(() => pool.evict('never-opened')).not.toThrow();
    expect(backend.openCalls).toHaveLength(0);
  });

  it('closeAll closes every open wallet', async () => {
    const { backend, pool } = makePool(4);
    await pool.withWallet(identity('u1'), (w) => w.getAddress());
    await pool.withWallet(identity('u2'), (w) => w.getAddress());
    await pool.closeAll();
    expect(pool.openCount()).toBe(0);
    expect(backend.handleFor('u1')?.closed).toBe(true);
    expect(backend.handleFor('u2')?.closed).toBe(true);
  });
});
