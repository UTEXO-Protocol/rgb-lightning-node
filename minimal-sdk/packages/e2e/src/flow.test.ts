/**
 * Scripted user journey over the full stack (regtest + shared RLN +
 * counterparty RLN + gateway). The journey uses ONLY the client SDK against
 * the gateway's real HTTP port — never RLN directly; the counterparty node
 * and the faucet are driven by the harness as "the outside world".
 *
 * Trust invariants are asserted while the journey runs:
 *  - I1: every gateway response is scanned for key-material field names and
 *    for the client mnemonics (scanning fetch installed in every client).
 *  - I3: a second user cannot see the first user's invoices/payments/
 *    transfers.
 *  - I4: the RLN admin-token sentinel and operator token appear in no
 *    response and no captured gateway log line.
 *  - A replayed pay with the same idempotency key does not double-debit.
 *
 * The send-asset leg is load-bearing beyond the journey itself: it is the only
 * place verifyAndSignPsbt runs against a PSBT rgb-lib actually emitted for a
 * COLORED spend (opret output, colored-account key origins, RGB change).
 *
 * Run with `pnpm -C minimal-sdk e2e` (see packages/e2e/README.md).
 */
import { afterAll, beforeAll, describe, expect, it } from 'vitest';
import {
  ClientKeys,
  GatewayClient,
  GatewayError,
  generateIdempotencyKey,
  verifyAndSignPsbt,
  type PreparedOp,
} from '@utexo/minimal-client-sdk';
import {
  forbidInLogs,
  forbidMnemonic,
  GATEWAY_URL,
  makeScanningFetch,
  mine,
  OPERATOR_TOKEN,
  regtest,
  retryUntil,
  rln,
  setupHarness,
  sleep,
  type Harness,
} from './setup.js';

const VAULT_FUND_SAT = 100_000_000; // 1 BTC from the faucet
const UTXO_NUM = 5;
const UTXO_SIZE_SAT = 5_000;
const RGB_RECEIVE_UNITS = 100;
const RGB_SEND_BACK_UNITS = 40;
const DEPOSIT_SAT = 200_000;
const DEPOSIT_MSAT = DEPOSIT_SAT * 1000;
const PAY_MSAT = 50_000_000;
const INVOICE_MSAT = 30_000_000;
const WITHDRAW_SAT = 180_000; // deposit - pay + invoice, in whole sats
const MAX_FEE_SAT = 2_000;
const FEE_RATE = 2;

interface JourneyUser {
  client: GatewayClient;
  keys: ClientKeys;
  userId: string;
  token: string;
}

let harness: Harness;
let userA: JourneyUser;
let userB: JourneyUser;
let vaultAddress: string;
let payHash: string;
let inboundHash: string;

/** Sign a prepared op with the journey user's keys — verify-before-sign. */
function sign(user: JourneyUser, prepared: PreparedOp): string {
  const { signedPsbt, verdict } = verifyAndSignPsbt(user.keys, prepared.psbt, {
    intent: prepared.intent,
    xpubs: user.keys.xpubs,
    maxFeeSat: MAX_FEE_SAT,
  });
  expect(verdict.ok).toBe(true);
  return signedPsbt;
}

async function createJourneyUser(name: string): Promise<JourneyUser> {
  const fetchFn = makeScanningFetch(harness.recorder);
  const bootstrap = new GatewayClient({ baseUrl: GATEWAY_URL, fetchFn });
  const created = await bootstrap.createUser(OPERATOR_TOKEN);
  forbidInLogs(harness.recorder, `${name}-token`, created.token);
  const { mnemonic, keys } = ClientKeys.generate('Regtest');
  forbidMnemonic(harness.recorder, `${name}-mnemonic`, mnemonic);
  return {
    client: new GatewayClient({ baseUrl: GATEWAY_URL, token: created.token, fetchFn }),
    keys,
    userId: created.userId,
    token: created.token,
  };
}

describe('minimal-sdk e2e journey on regtest', () => {
  beforeAll(async () => {
    harness = await setupHarness();
  }, 900_000);

  afterAll(async () => {
    await harness?.teardown();
  }, 60_000);

  it('creates a user, registers client-side xpubs, funds the vault', async () => {
    userA = await createJourneyUser('user-a');
    const xpubs = userA.keys.xpubs;
    const registered = await userA.client.registerXpubs({
      vanilla: xpubs.vanilla,
      colored: xpubs.colored,
      fingerprint: xpubs.fingerprint,
    });
    expect(registered.fingerprint).toBe(xpubs.fingerprint);
    vaultAddress = registered.address;
    // The gateway derived the address from the registered xpubs; the client
    // must re-derive the identical script (watch-only parity).
    expect(vaultAddress).toMatch(/^bcrt1p/);

    regtest(`sendtoaddress ${vaultAddress} 1`);
    mine(1);
    await retryUntil(
      'vault funded',
      async () => {
        await userA.client.sync();
        const balances = await userA.client.getBalances();
        return balances.btc.vanilla.settled >= VAULT_FUND_SAT ? true : undefined;
      },
      90_000,
    );
  }, 180_000);

  it('creates colorable UTXOs via prepare -> verify -> sign -> complete', async () => {
    const prepared = await userA.client.prepareCreateUtxos({
      num: UTXO_NUM,
      size: UTXO_SIZE_SAT,
      feeRateSatPerVb: FEE_RATE,
    });
    expect(prepared.intent.kind).toBe('create_utxos');
    const completed = await userA.client.completeCreateUtxos({
      opId: prepared.opId,
      signedPsbt: sign(userA, prepared),
    });
    expect(completed.utxosCreated).toBe(UTXO_NUM);
    mine(1);
    await retryUntil(
      'colorable utxos settled',
      async () => {
        await userA.client.sync();
        const { unspents } = await userA.client.getUnspents();
        return unspents.filter((u) => u.colorable).length >= UTXO_NUM ? true : undefined;
      },
      90_000,
    );
  }, 180_000);

  it('receives an RGB asset into the vault from the counterparty', async () => {
    const receive = await userA.client.receive({ mode: 'blind' });
    expect(receive.invoice.length).toBeGreaterThan(20);

    await rln(harness.cp.base, '/sendrgb', {
      donation: false,
      fee_rate: FEE_RATE,
      min_confirmations: 1,
      recipient_map: {
        [harness.assetId]: [
          {
            recipient_id: receive.recipientId,
            assignment: { type: 'Fungible', value: RGB_RECEIVE_UNITS },
            transport_endpoints: ['rpc://127.0.0.1:3000/json-rpc'],
          },
        ],
      },
    });

    // Settlement dance (donation=false): receiver ACKs via refresh, the
    // sender's refresh broadcasts, one confirmation settles both sides.
    await retryUntil(
      'RGB asset settled in the vault',
      async () => {
        await userA.client.sync();
        await rln(harness.cp.base, '/refreshtransfers', { filter: [], skip_sync: false });
        const balances = await userA.client.getBalances();
        const asset = balances.assets.find((entry) => entry.assetId === harness.assetId);
        if (asset !== undefined && asset.balance.settled >= RGB_RECEIVE_UNITS) return true;
        mine(1);
        return undefined;
      },
      180_000,
      2000,
    );

    const { transfers } = await userA.client.getTransfers(harness.assetId);
    expect(transfers.some((t) => t.status === 'Settled' && t.amount === RGB_RECEIVE_UNITS)).toBe(
      true,
    );
  }, 240_000);

  it('sends the RGB asset back on chain via prepare -> verify -> sign -> complete', async () => {
    // The only path that runs verify-before-sign against a real rgb-lib
    // COLORED spend: the opret commitment output, the colored-account key
    // origins and the RGB change output all have to satisfy verifyPsbt, which
    // hand-built fixtures cannot prove.
    const back = await rln<{ recipient_id: string }>(harness.cp.base, '/rgbinvoice', {
      min_confirmations: 1,
      asset_id: harness.assetId,
      assignment: { type: 'Fungible', value: RGB_SEND_BACK_UNITS },
      witness: false,
      transport_endpoints: ['rpc://127.0.0.1:3000/json-rpc'],
    });

    const prepared = await userA.client.prepareSendAsset(
      {
        assetId: harness.assetId,
        amount: RGB_SEND_BACK_UNITS,
        recipientId: back.recipient_id,
        feeRateSatPerVb: FEE_RATE,
      },
      generateIdempotencyKey(),
    );
    expect(prepared.intent.kind).toBe('send_asset');
    expect(prepared.intent.asset?.amount).toBe(RGB_SEND_BACK_UNITS);

    const completed = await userA.client.completeSendAsset(
      { opId: prepared.opId, signedPsbt: sign(userA, prepared) },
      generateIdempotencyKey(),
    );
    expect(completed.txid).toMatch(/^[0-9a-f]{64}$/);

    await retryUntil(
      'RGB send settled on both sides',
      async () => {
        await userA.client.sync();
        await rln(harness.cp.base, '/refreshtransfers', { filter: [], skip_sync: false });
        const balances = await userA.client.getBalances();
        const asset = balances.assets.find((entry) => entry.assetId === harness.assetId);
        if (
          asset !== undefined &&
          asset.balance.settled === RGB_RECEIVE_UNITS - RGB_SEND_BACK_UNITS
        ) {
          return true;
        }
        mine(1);
        return undefined;
      },
      180_000,
      2000,
    );
  }, 240_000);

  it('deposits BTC from the vault into the LN float', async () => {
    const deposit = await userA.client.prepareLnDeposit({ kind: 'btc', amountMsat: DEPOSIT_MSAT });
    expect(deposit.kind).toBe('btc');
    const target = deposit.address as string;
    expect(target).toMatch(/^bcrt1/);

    const prepared = await userA.client.prepareSendBtc({
      address: target,
      amountSat: DEPOSIT_SAT,
      feeRateSatPerVb: FEE_RATE,
    });
    expect(prepared.intent.recipients[0]?.amountSat).toBe(DEPOSIT_SAT);
    const completed = await userA.client.completeSendBtc({
      opId: prepared.opId,
      signedPsbt: sign(userA, prepared),
    });
    expect(completed.txid).toMatch(/^[0-9a-f]{64}$/);
    mine(1);

    await retryUntil(
      'deposit credited to the float ledger',
      async () => {
        const balance = await userA.client.getLnBalance();
        return balance.btcMsat >= DEPOSIT_MSAT ? true : undefined;
      },
      90_000,
    );
    const balance = await userA.client.getLnBalance();
    expect(balance.btcMsat).toBe(DEPOSIT_MSAT);
  }, 180_000);

  it('pays a BOLT11 invoice from the counterparty; replay does not double-debit', async () => {
    const cpInvoice = await rln<{ invoice: string }>(harness.cp.base, '/lninvoice', {
      amt_msat: PAY_MSAT,
      expiry_sec: 3600,
      asset_id: null,
      asset_amount: null,
    });

    const key = generateIdempotencyKey();
    const paid = await userA.client.payLnInvoice({ invoice: cpInvoice.invoice }, key);
    expect(paid.status).not.toBe('failed');
    payHash = paid.paymentHash;

    // Same idempotency key + same body: cached response, no second debit.
    const replayed = await userA.client.payLnInvoice({ invoice: cpInvoice.invoice }, key);
    expect(replayed.paymentHash).toBe(payHash);

    await retryUntil(
      'counterparty saw the payment settle',
      async () => {
        const { status } = await rln<{ status: string }>(harness.cp.base, '/invoicestatus', {
          invoice: cpInvoice.invoice,
        });
        return status === 'Succeeded' ? true : undefined;
      },
      90_000,
    );
    await retryUntil(
      'gateway reconciled the outbound payment',
      async () => {
        const { payments } = await userA.client.listLnPayments();
        const entry = payments.find((p) => p.paymentHash === payHash);
        return entry?.status === 'succeeded' && entry.direction === 'outbound' ? true : undefined;
      },
      90_000,
    );
    // Debited exactly once, despite the replay.
    const balance = await userA.client.getLnBalance();
    expect(balance.btcMsat).toBe(DEPOSIT_MSAT - PAY_MSAT);
  }, 180_000);

  it('creates an invoice and is paid by the counterparty', async () => {
    const created = await userA.client.createLnInvoice({ amtMsat: INVOICE_MSAT });
    inboundHash = created.paymentHash;

    await rln(harness.cp.base, '/sendpayment', { invoice: created.invoice });

    await retryUntil(
      'inbound invoice settled and credited',
      async () => {
        const info = await userA.client.getLnInvoice(inboundHash);
        return info.state === 'settled' ? true : undefined;
      },
      90_000,
    );
    const balance = await userA.client.getLnBalance();
    expect(balance.btcMsat).toBe(DEPOSIT_MSAT - PAY_MSAT + INVOICE_MSAT);
  }, 180_000);

  it('scopes user A resources away from user B (I3)', async () => {
    userB = await createJourneyUser('user-b');

    const foreignInvoice = await userB.client.getLnInvoice(inboundHash).catch((e: unknown) => e);
    expect(foreignInvoice).toBeInstanceOf(GatewayError);
    expect((foreignInvoice as GatewayError).status).toBe(404);

    const { payments } = await userB.client.listLnPayments();
    expect(payments).toEqual([]);

    const balance = await userB.client.getLnBalance();
    expect(balance.btcMsat).toBe(0);
    expect(balance.assets).toEqual({});

    // B's own vault is empty: A's transfers/unspents are simply not there.
    const xpubs = userB.keys.xpubs;
    await userB.client.registerXpubs({
      vanilla: xpubs.vanilla,
      colored: xpubs.colored,
      fingerprint: xpubs.fingerprint,
    });
    expect((await userB.client.getTransfers()).transfers).toEqual([]);
    expect((await userB.client.getUnspents()).unspents).toEqual([]);
    const balances = await userB.client.getBalances();
    expect(balances.btc.vanilla.settled).toBe(0);
    expect(balances.assets).toEqual([]);
  }, 120_000);

  it('withdraws the remaining float back to the vault; balances reconcile', async () => {
    const before = await userA.client.getLnBalance();
    expect(before.btcMsat).toBe(WITHDRAW_SAT * 1000);
    const vaultBefore = (await userA.client.getBalances()).btc.vanilla.settled;

    const withdrawn = await userA.client.withdrawLn({
      kind: 'btc',
      address: vaultAddress,
      amountSat: WITHDRAW_SAT,
      feeRateSatPerVb: FEE_RATE,
    });
    expect(withdrawn.txid).toMatch(/^[0-9a-f]{64}$/);
    mine(1);

    await retryUntil(
      'withdrawal arrived in the vault',
      async () => {
        await userA.client.sync();
        const balances = await userA.client.getBalances();
        return balances.btc.vanilla.settled >= vaultBefore + WITHDRAW_SAT ? true : undefined;
      },
      90_000,
    );

    // Full reconciliation: deposit - pay + invoice - withdraw == 0.
    const after = await userA.client.getLnBalance();
    expect(after.btcMsat).toBe(DEPOSIT_MSAT - PAY_MSAT + INVOICE_MSAT - WITHDRAW_SAT * 1000);
    expect(after.btcMsat).toBe(0);
  }, 180_000);

  it('never leaked key material or credentials (I1/I4)', async () => {
    // Let in-flight worker log lines land before scanning.
    await sleep(1500);
    expect(harness.recorder.scanned).toBeGreaterThan(20);
    expect(harness.recorder.violations).toEqual([]);
    for (const [name, secret] of harness.recorder.logForbidden) {
      const hit = harness.recorder.logs.find((line) => line.includes(secret));
      expect(hit, `secret "${name}" leaked into gateway logs`).toBeUndefined();
    }
  }, 30_000);
});
