/**
 * Per-wallet single-flight operation queue (Block 3 item 3).
 *
 * rgb-lib wallet handles are not concurrency-safe: every operation on a
 * user's wallet — request-driven calls and background syncs alike — runs
 * through run(userId, fn), which chains strictly FIFO per wallet. Unlike the
 * request-level UserQueues (../queue.ts) there is no depth cap or global
 * semaphore here: backpressure is applied at the request layer, and workers
 * enqueue at most one background operation per wallet at a time.
 */

export class WalletOpQueues {
  private readonly tails = new Map<string, Promise<unknown>>();

  run<T>(walletId: string, fn: () => Promise<T>): Promise<T> {
    const previous = this.tails.get(walletId) ?? Promise.resolve();
    const run: Promise<T> = previous.catch(() => undefined).then(fn);
    this.tails.set(walletId, run);
    // Drop the tail entry once the chain drains (a newer run replaces it).
    void run
      .catch(() => undefined)
      .finally(() => {
        if (this.tails.get(walletId) === run) this.tails.delete(walletId);
      });
    return run;
  }
}
