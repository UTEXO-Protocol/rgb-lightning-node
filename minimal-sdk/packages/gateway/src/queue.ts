/**
 * Per-user FIFO queue with a global concurrency cap (Block 3 item 1).
 *
 * Every downstream operation (RLN call, wallet-service call) is funneled
 * through enqueue(userId, fn). Guarantees:
 *  - per user: tasks run one at a time, strictly in enqueue order;
 *  - globally: at most `globalConcurrency` tasks run at once, granted in
 *    arrival order so no user can starve the others;
 *  - backpressure: a user with `perUserDepth` queued-or-running tasks gets an
 *    immediate QueueFullError (mapped to 429 + Retry-After by the server).
 */

export class QueueFullError extends Error {
  constructor(readonly retryAfterSeconds: number) {
    super('per-user queue is full');
    this.name = 'QueueFullError';
  }
}

class Semaphore {
  private available: number;
  private readonly waiters: Array<() => void> = [];

  constructor(capacity: number) {
    this.available = capacity;
  }

  async acquire(): Promise<void> {
    if (this.available > 0) {
      this.available -= 1;
      return;
    }
    await new Promise<void>((resolve) => this.waiters.push(resolve));
  }

  release(): void {
    const next = this.waiters.shift();
    if (next !== undefined) {
      next();
    } else {
      this.available += 1;
    }
  }
}

export interface UserQueuesOptions {
  globalConcurrency: number;
  perUserDepth: number;
}

export class UserQueues {
  private readonly tails = new Map<string, Promise<unknown>>();
  private readonly depths = new Map<string, number>();
  private readonly semaphore: Semaphore;

  constructor(private readonly options: UserQueuesOptions) {
    this.semaphore = new Semaphore(options.globalConcurrency);
  }

  /** Queued-or-running task count for a user (0 when idle). */
  depth(userId: string): number {
    return this.depths.get(userId) ?? 0;
  }

  enqueue<T>(userId: string, fn: () => Promise<T>): Promise<T> {
    const depth = this.depth(userId);
    if (depth >= this.options.perUserDepth) {
      throw new QueueFullError(1);
    }
    this.depths.set(userId, depth + 1);
    const previous = this.tails.get(userId) ?? Promise.resolve();
    const run: Promise<T> = previous
      .catch(() => undefined)
      .then(async () => {
        await this.semaphore.acquire();
        try {
          return await fn();
        } finally {
          this.semaphore.release();
          const remaining = this.depth(userId) - 1;
          if (remaining <= 0) {
            this.depths.delete(userId);
            if (this.tails.get(userId) === run) this.tails.delete(userId);
          } else {
            this.depths.set(userId, remaining);
          }
        }
      });
    this.tails.set(userId, run);
    return run;
  }
}
