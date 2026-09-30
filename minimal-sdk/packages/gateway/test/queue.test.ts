import { describe, expect, it } from 'vitest';
import { QueueFullError, UserQueues } from '../src/queue.js';
import { sleep } from './helpers.js';

describe('UserQueues', () => {
  it('runs one user tasks strictly in FIFO order under parallel load', async () => {
    const queues = new UserQueues({ globalConcurrency: 8, perUserDepth: 100 });
    const completed: number[] = [];
    const tasks: Promise<void>[] = [];
    for (let i = 0; i < 25; i++) {
      tasks.push(
        queues.enqueue('alice', async () => {
          // Random delay: with concurrency > 1 across users, only per-user
          // chaining can keep this order stable.
          await sleep(Math.floor(11 * ((i * 7) % 5)) / 5);
          completed.push(i);
        }),
      );
    }
    await Promise.all(tasks);
    expect(completed).toEqual([...Array(25).keys()]);
  });

  it('interleaves users but preserves per-user order', async () => {
    const queues = new UserQueues({ globalConcurrency: 4, perUserDepth: 100 });
    const perUser = new Map<string, number[]>();
    const tasks: Promise<void>[] = [];
    for (const user of ['a', 'b', 'c']) perUser.set(user, []);
    for (let i = 0; i < 15; i++) {
      for (const user of ['a', 'b', 'c']) {
        tasks.push(
          queues.enqueue(user, async () => {
            await sleep((i + user.charCodeAt(0)) % 3);
            perUser.get(user)?.push(i);
          }),
        );
      }
    }
    await Promise.all(tasks);
    for (const order of perUser.values()) {
      expect(order).toEqual([...Array(15).keys()]);
    }
  });

  it('never exceeds the global concurrency cap', async () => {
    const cap = 3;
    const queues = new UserQueues({ globalConcurrency: cap, perUserDepth: 100 });
    let running = 0;
    let peak = 0;
    const tasks: Promise<void>[] = [];
    for (let i = 0; i < 40; i++) {
      tasks.push(
        queues.enqueue(`user-${i}`, async () => {
          running += 1;
          peak = Math.max(peak, running);
          await sleep(2);
          running -= 1;
        }),
      );
    }
    await Promise.all(tasks);
    expect(peak).toBeLessThanOrEqual(cap);
    expect(peak).toBeGreaterThan(1);
  });

  it('rejects with QueueFullError once per-user depth is reached, then recovers', async () => {
    const queues = new UserQueues({ globalConcurrency: 2, perUserDepth: 2 });
    let release: () => void = () => {};
    const gate = new Promise<void>((resolve) => (release = resolve));
    const first = queues.enqueue('alice', () => gate);
    const second = queues.enqueue('alice', async () => 'second');
    expect(() => queues.enqueue('alice', async () => 'third')).toThrow(QueueFullError);
    try {
      queues.enqueue('alice', async () => 'fourth');
    } catch (error) {
      expect((error as QueueFullError).retryAfterSeconds).toBeGreaterThanOrEqual(1);
    }
    // A different user must not be affected by another user's full queue.
    await expect(queues.enqueue('bob', async () => 'ok')).resolves.toBe('ok');
    release();
    await first;
    await expect(second).resolves.toBe('second');
    await expect(queues.enqueue('alice', async () => 'fifth')).resolves.toBe('fifth');
  });

  it('keeps serving a user after one of their tasks throws', async () => {
    const queues = new UserQueues({ globalConcurrency: 2, perUserDepth: 10 });
    const failing = queues.enqueue('alice', async () => {
      throw new Error('boom');
    });
    await expect(failing).rejects.toThrow('boom');
    await expect(queues.enqueue('alice', async () => 42)).resolves.toBe(42);
    expect(queues.depth('alice')).toBe(0);
  });
});
