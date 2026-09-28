/**
 * Completions worker: the gateway finishing the bookkeeping of an on-chain
 * operation that `complete` left ambiguous.
 *
 * The behaviour under test is the whole reason the worker exists. A client that
 * receives 502 COMPLETE_AMBIGUOUS is entitled never to retry — a 5xx is not
 * proof the send failed, and a retry it cannot prove safe could, as far as it
 * knows, send twice. Before this worker such an op stayed `pending` forever with
 * its transaction on the network, then aged into `expired`, at which point
 * `complete` refused it with 410 and nothing could ever resolve it.
 */
import type { FastifyInstance } from 'fastify';
import { randomUUID } from 'node:crypto';
import { base64, hex } from '@scure/base';
import { Transaction } from '@scure/btc-signer';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { WalletBackendError } from '../src/wallets/backend.js';
import { txidFromPsbt } from '../src/wallets/prepare.js';
import { createTestUser, testServer, type TestUser } from './helpers.js';
import { MockWalletBackend } from './wallet-mocks.js';

const XPUBS = {
  vanilla:
    'tpubDDfvzhdVV4unsoKt5aE6dcsNsfeWbTgmLZPi8LQDYU2xixrYemMfWJ3BaVneH3u7DBQePdTwhpybaKRU95pi6PMUtLPBJLVQRpzEnjfjZzX',
  colored:
    'tpubDCtpoJs6YJcjLnr9gq6jYriYNMuWEu8mSDvEQU5st3ZkJbFqqzwpHUiPvxqD2366ciFAfpehk1k2d7Tyk7AJEr8uZva7KfnX4RpsiVSoEcZ',
  fingerprint: '73c5da0a',
};

const RECIPIENT_ADDRESS = 'bcrt1p8wpt9v4frpf3tkn0srd97pksgsxc5hs52lafxwru9kgeephvs7rqjeprhg';
const RECIPIENT_SCRIPT_HEX = '51203b82b2b2a9185315da6f80da5f06d0440d8a5e1457fa93387c2d919c86ec8786';

/** Minimal parsable PSBT spending outpoint `char.repeat(64)`:0. */
function parsablePsbt(char: string): string {
  const tx = new Transaction({ allowUnknownInputs: true, allowUnknownOutputs: true });
  tx.addInput({ txid: char.repeat(64), index: 0 });
  tx.addOutput({ script: hex.decode(RECIPIENT_SCRIPT_HEX), amount: 40_000n });
  return base64.encode(tx.toPSBT());
}
const PSBT_A = parsablePsbt('a');
const PSBT_B = parsablePsbt('b');

/** rgb-lib failure that cannot be pinned on the PSBT: may already have broadcast. */
function postBroadcastFailure(): WalletBackendError {
  return new WalletBackendError(
    'wallet sendBtcEnd failed',
    'RgbLib(Database { details: "error returned from database: disk I/O error" })',
  );
}

describe('completions worker', () => {
  let app: FastifyInstance;
  let backend: MockWalletBackend;
  let user: TestUser;

  beforeEach(async () => {
    backend = new MockWalletBackend();
    backend.dataFor = () => ({ preparedPsbt: PSBT_A });
    app = await testServer({ walletBackend: backend });
    user = await createTestUser(app);
    const registered = await app.inject({
      method: 'POST',
      url: '/v1/wallet/xpubs',
      headers: { authorization: `Bearer ${user.token}` },
      payload: XPUBS,
    });
    expect(registered.statusCode).toBe(201);
  });

  afterEach(async () => {
    await app.close();
  });

  function headers(key: string = randomUUID()) {
    return { authorization: `Bearer ${user.token}`, 'idempotency-key': key };
  }

  async function prepareSendBtc() {
    const response = await app.inject({
      method: 'POST',
      url: '/v1/onchain/send-btc/prepare',
      headers: headers(),
      payload: { address: RECIPIENT_ADDRESS, amountSat: 40_000, feeRateSatPerVb: 2 },
    });
    expect(response.statusCode).toBe(201);
    return response.json() as { opId: string };
  }

  function opRow(opId: string) {
    return app.db.prepare('SELECT * FROM pending_ops WHERE id = ?').get(opId) as
      | {
          state: string;
          txid: string | null;
          signed_psbt: string | null;
          completion_attempts: number;
        }
      | undefined;
  }

  async function getOp(opId: string) {
    return app.inject({
      method: 'GET',
      url: `/v1/onchain/operations/${opId}`,
      headers: { authorization: `Bearer ${user.token}` },
    });
  }

  /** Drive an op to the ambiguous state the worker is meant to resolve. */
  async function makeAmbiguous(): Promise<string> {
    // Prepare must succeed, so clear any failure a previous call left armed.
    const open = backend.handleFor(user.userId);
    if (open !== undefined) open.failWith = undefined;
    const { opId } = await prepareSendBtc();
    backend.handleFor(user.userId)!.failWith = postBroadcastFailure();
    const response = await app.inject({
      method: 'POST',
      url: '/v1/onchain/send-btc/complete',
      headers: headers(),
      payload: { opId, signedPsbt: PSBT_A },
    });
    expect(response.statusCode).toBe(502);
    expect(response.json().error.code).toBe('COMPLETE_AMBIGUOUS');
    return opId;
  }

  it('retains the signed PSBT with the txid when a completion goes ambiguous', async () => {
    const opId = await makeAmbiguous();
    const row = opRow(opId);
    expect(row?.state).toBe('pending');
    expect(row?.txid).toBe(txidFromPsbt(PSBT_A));
    // Without this the gateway cannot finish the job itself. It is not a secret:
    // signatures are public and the transaction is already broadcast.
    expect(row?.signed_psbt).toBe(PSBT_A);
  });

  it('finishes an ambiguous operation without the client ever coming back', async () => {
    const opId = await makeAmbiguous();
    // The wallet recovers (rgb-lib re-broadcast of a known transaction succeeds).
    backend.handleFor(user.userId)!.failWith = undefined;

    await app.completionsWorker.runOnce();

    const row = opRow(opId);
    expect(row?.state).toBe('completed');
    // The txid comes from the wallet, exactly as in `complete` (the mock returns
    // a constant; rgb-lib returns the broadcast txid of the retained PSBT).
    expect(row?.txid).toBe('mock-btc-txid');
    // Retained PSBT dropped once it has no further purpose.
    expect(row?.signed_psbt).toBeNull();
    // And the client's own recovery read now terminalizes instead of reporting
    // an ambiguity forever.
    const status = (await getOp(opId)).json();
    expect(status.state).toBe('completed');
    expect(status.mayHaveBroadcast).toBe(false);
  });

  it('resolves an ambiguous operation that has already passed its TTL', async () => {
    // The TTL exists so an UNSIGNED PSBT stops being completable. This one is
    // signed and on the network: refusing to record it would strand the
    // bookkeeping for a transaction that confirmed anyway, which is exactly the
    // dead end the old "retry complete" recovery had (complete answers 410).
    const opId = await makeAmbiguous();
    app.db.prepare('UPDATE pending_ops SET expires_at = ? WHERE id = ?').run(Date.now() - 1, opId);
    expect((await getOp(opId)).json().state).toBe('expired');
    backend.handleFor(user.userId)!.failWith = undefined;

    await app.completionsWorker.runOnce();

    expect(opRow(opId)?.state).toBe('completed');
  });

  it('leaves an op alone while it is still failing, and counts the attempt', async () => {
    const opId = await makeAmbiguous();
    expect(opRow(opId)?.completion_attempts).toBe(0);

    await app.completionsWorker.runOnce();

    expect(opRow(opId)?.state).toBe('pending');
    expect(opRow(opId)?.completion_attempts).toBe(1);
    // Still eligible: the next pass tries again.
    expect(app.completionsWorker.pending().map((row) => row.id)).toEqual([opId]);
  });

  it('gives up after the attempt cap and reports the op as needing an operator', async () => {
    const opId = await makeAmbiguous();
    for (let pass = 0; pass < 6; pass += 1) await app.completionsWorker.runOnce();

    const row = opRow(opId);
    expect(row?.state).toBe('pending');
    // Capped at 5 attempts (GATEWAY_COMPLETIONS_MAX_ATTEMPTS): a permanently
    // unfinishable op must stop consuming the user's wallet queue every pass.
    expect(row?.completion_attempts).toBe(5);
    expect(app.completionsWorker.pending()).toEqual([]);
    expect(app.completionsWorker.unresolved().map((entry) => entry.id)).toEqual([opId]);
  });

  it('ignores ops that never reached broadcast', async () => {
    // No txid recorded means rgb-lib failed before broadcast (or the op was
    // merely prepared): there is nothing to finish, and replaying a PSBT here
    // would be the gateway sending on its own initiative.
    const { opId } = await prepareSendBtc();
    expect(app.completionsWorker.pending()).toEqual([]);

    await app.completionsWorker.runOnce();

    expect(opRow(opId)?.state).toBe('pending');
    expect(opRow(opId)?.completion_attempts).toBe(0);
  });

  it('ignores an ambiguous op with no retained PSBT (pre-upgrade rows)', async () => {
    const opId = await makeAmbiguous();
    app.db.prepare('UPDATE pending_ops SET signed_psbt = NULL WHERE id = ?').run(opId);
    expect(app.completionsWorker.pending()).toEqual([]);

    await app.completionsWorker.runOnce();

    expect(opRow(opId)?.state).toBe('pending');
  });

  it('refuses to broadcast a retained PSBT that is not the recorded transaction', async () => {
    // A retained PSBT whose txid disagrees with the recorded one would mean the
    // row's intent describes a different transaction. The check runs on retained
    // state BEFORE any wallet call, so nothing is broadcast.
    const opId = await makeAmbiguous();
    app.db.prepare('UPDATE pending_ops SET signed_psbt = ? WHERE id = ?').run(PSBT_B, opId);
    const handle = backend.handleFor(user.userId)!;
    handle.failWith = undefined;
    const before = handle.operations.length;

    await app.completionsWorker.runOnce();

    expect(handle.operations.slice(before)).toEqual([]);
    expect(opRow(opId)?.state).toBe('pending');
    expect(opRow(opId)?.completion_attempts).toBe(1);

    // And it walks out to the operator queue instead of being retried forever.
    for (let pass = 0; pass < 5; pass += 1) await app.completionsWorker.runOnce();
    expect(app.completionsWorker.pending()).toEqual([]);
    expect(app.completionsWorker.unresolved().map((entry) => entry.id)).toEqual([opId]);
  });

  it('skips a row a client finished after the batch was selected', async () => {
    // The worker selects a batch and then works through it; a client's own
    // retry can land in between. Acting on the stale row would spend a wallet
    // slot and an attempt re-broadcasting a transaction already recorded done.
    const opId = await makeAmbiguous();
    const batch = app.completionsWorker.pending();
    expect(batch).toHaveLength(1);
    const handle = backend.handleFor(user.userId)!;
    handle.failWith = undefined;
    app.db
      .prepare("UPDATE pending_ops SET state = 'completed', signed_psbt = NULL WHERE id = ?")
      .run(opId);
    const before = handle.operations.length;

    expect(await app.onchain.resumeAmbiguous(batch[0]!)).toBe(false);

    expect(handle.operations.slice(before)).toEqual([]);
    expect(opRow(opId)?.completion_attempts).toBe(0);
  });

  it('never touches a completed op', async () => {
    const { opId } = await prepareSendBtc();
    const completed = await app.inject({
      method: 'POST',
      url: '/v1/onchain/send-btc/complete',
      headers: headers(),
      payload: { opId, signedPsbt: PSBT_A },
    });
    expect(completed.statusCode).toBe(200);
    expect(app.completionsWorker.pending()).toEqual([]);

    await app.completionsWorker.runOnce();

    expect(opRow(opId)?.completion_attempts).toBe(0);
  });

  it('isolates a failing op from the rest of the pass', async () => {
    const first = await makeAmbiguous();
    const second = await makeAmbiguous();
    const handle = backend.handleFor(user.userId)!;
    // A non-WalletBackendError escaping resumeAmbiguous must not abort the pass;
    // start() calls runOnce with `void`, so an escape would kill the process.
    let calls = 0;
    handle.failWith = undefined;
    handle.gate = async () => {
      calls += 1;
      if (calls === 1) throw new Error('unexpected wallet explosion');
    };

    await app.completionsWorker.runOnce();

    expect(calls).toBe(2);
    const states = [first, second].map((id) => opRow(id)?.state);
    expect(states).toContain('completed');
    expect(states).toContain('pending');
  });
});
