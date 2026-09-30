import { describe, expect, it } from 'vitest';
import { WalletOpQueues } from '../src/wallets/queue.js';
import { deferred, sleep } from './helpers.js';

describe('WalletOpQueues', () => {
  it('runs operations for one wallet strictly one at a time, in FIFO order', async () => {
    const queues = new WalletOpQueues();
    const events: string[] = [];
    let running = 0;
    let maxRunning = 0;

    const tasks = [0, 1, 2, 3, 4].map((index) =>
      queues.run('w1', async () => {
        running += 1;
        maxRunning = Math.max(maxRunning, running);
        events.push(`start-${index}`);
        await sleep(5 - index);
        events.push(`end-${index}`);
        running -= 1;
      }),
    );
    await Promise.all(tasks);

    expect(maxRunning).toBe(1);
    expect(events).toEqual([
      'start-0',
      'end-0',
      'start-1',
      'end-1',
      'start-2',
      'end-2',
      'start-3',
      'end-3',
      'start-4',
      'end-4',
    ]);
  });

  it('lets different wallets run concurrently', async () => {
    const queues = new WalletOpQueues();
    const gate = deferred<void>();
    let otherFinished = false;

    const blocked = queues.run('w1', async () => {
      await gate.promise;
    });
    await queues.run('w2', async () => {
      otherFinished = true;
    });

    expect(otherFinished).toBe(true);
    gate.resolve();
    await blocked;
  });

  it('keeps the chain alive after a failing operation', async () => {
    const queues = new WalletOpQueues();
    await expect(
      queues.run('w1', async () => {
        throw new Error('boom');
      }),
    ).rejects.toThrow('boom');
    await expect(queues.run('w1', async () => 'recovered')).resolves.toBe('recovered');
  });

  it('drains queued work even when enqueued while an operation is in flight', async () => {
    const queues = new WalletOpQueues();
    const gate = deferred<void>();
    const order: string[] = [];
    const first = queues.run('w1', async () => {
      await gate.promise;
      order.push('first');
    });
    const second = queues.run('w1', async () => {
      order.push('second');
    });
    gate.resolve();
    await Promise.all([first, second]);
    expect(order).toEqual(['first', 'second']);
    // A later run on the drained queue starts immediately.
    await expect(queues.run('w1', async () => 'fresh')).resolves.toBe('fresh');
  });
});
