/**
 * Lazy-open LRU pool of per-user watch-only wallets.
 *
 * A wallet opens on first use (construction + goOnline) and stays cached; when
 * more than maxOpen wallets are open, the least-recently-used one is closed.
 * All access — including open and close — runs on the wallet's single-flight
 * queue, so an eviction never interrupts an in-flight operation and a reopen
 * queued behind a close observes the closed state, not a stale handle.
 */
import type { WalletBackend, WalletHandle, WalletIdentity } from './backend.js';
import type { WalletOpQueues } from './queue.js';

export interface WalletPoolOptions {
  maxOpen: number;
}

export class WalletPool {
  /** Insertion order doubles as LRU order (oldest first). */
  private readonly open = new Map<string, WalletHandle>();

  constructor(
    private readonly backend: WalletBackend,
    private readonly queues: WalletOpQueues,
    private readonly options: WalletPoolOptions,
  ) {}

  /** Number of currently open wallets (for tests and metrics). */
  openCount(): number {
    return this.open.size;
  }

  /**
   * Run one operation against the user's wallet, opening it if needed. The
   * whole acquire+operate sequence holds the wallet's queue slot.
   */
  async withWallet<T>(
    identity: WalletIdentity,
    fn: (wallet: WalletHandle) => Promise<T>,
  ): Promise<T> {
    return this.queues.run(identity.userId, async () => {
      const handle = await this.acquire(identity);
      return fn(handle);
    });
  }

  /** Must only be called from within the wallet's queue slot. */
  private async acquire(identity: WalletIdentity): Promise<WalletHandle> {
    const existing = this.open.get(identity.userId);
    if (existing !== undefined) {
      this.open.delete(identity.userId);
      this.open.set(identity.userId, existing);
      return existing;
    }
    const handle = await this.backend.open(identity);
    this.open.set(identity.userId, handle);
    this.evictOverflow(identity.userId);
    return handle;
  }

  private evictOverflow(inUseUserId: string): void {
    while (this.open.size > this.options.maxOpen) {
      const lru = this.open.keys().next().value;
      if (lru === undefined || lru === inUseUserId) return;
      const handle = this.open.get(lru);
      this.open.delete(lru);
      if (handle === undefined) continue;
      // Close behind any queued work for that wallet; a later request reopens.
      void this.queues.run(lru, () => handle.close()).catch(() => undefined);
    }
  }

  /**
   * Drop a user's cached wallet (closed behind any queued work). Used when a
   * registration attempt fails after the smoke-open: the cached handle would
   * otherwise serve addresses derived from xpubs that were never persisted.
   */
  evict(userId: string): void {
    const handle = this.open.get(userId);
    if (handle === undefined) return;
    this.open.delete(userId);
    void this.queues.run(userId, () => handle.close()).catch(() => undefined);
  }

  /** Close every open wallet (server shutdown). */
  async closeAll(): Promise<void> {
    const entries = [...this.open.entries()];
    this.open.clear();
    await Promise.all(
      entries.map(([userId, handle]) =>
        this.queues.run(userId, () => handle.close()).catch(() => undefined),
      ),
    );
  }
}
