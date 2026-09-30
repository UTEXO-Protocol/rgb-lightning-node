/**
 * Full prepare→sign→complete round-trip in-process with a fixture signer:
 * the mocked backend serves a REAL taproot PSBT (key origins included, as
 * BDK/rgb-lib produce), the "client" signs it from a test-only mnemonic via
 * @scure — exactly what the Task-6 SDK will do — and completion verifies a
 * finalized schnorr signature is present before reporting the txid.
 *
 * The fixture mnemonic is test-only: mnemonics are never a gateway input.
 */
import type { FastifyInstance } from 'fastify';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { randomUUID } from 'node:crypto';
import type { HDKey } from '@scure/bip32';
import { base64, hex } from '@scure/base';
import { p2tr, Transaction } from '@scure/btc-signer';
import { WalletBackendError } from '../src/wallets/backend.js';
import { masterFromMnemonic, signPsbtWithMaster, PSBT_PARSE_OPTIONS } from './fixture-signer.js';
import { createTestUser, testServer, type TestUser } from './helpers.js';
import { MockWalletBackend } from './wallet-mocks.js';

const FIXTURE_MNEMONIC =
  'abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about';
const REGTEST = { bech32: 'bcrt', pubKeyHash: 0x6f, scriptHash: 0xc4, wif: 0xef };
/** Vanilla keychain account for regtest (BIP-86, coin type 1). */
const ACCOUNT_PATH = "m/86'/1'/0'";

const XPUBS = {
  vanilla:
    'tpubDDfvzhdVV4unsoKt5aE6dcsNsfeWbTgmLZPi8LQDYU2xixrYemMfWJ3BaVneH3u7DBQePdTwhpybaKRU95pi6PMUtLPBJLVQRpzEnjfjZzX',
  colored:
    'tpubDCtpoJs6YJcjLnr9gq6jYriYNMuWEu8mSDvEQU5st3ZkJbFqqzwpHUiPvxqD2366ciFAfpehk1k2d7Tyk7AJEr8uZva7KfnX4RpsiVSoEcZ',
  fingerprint: '73c5da0a',
};

const master = masterFromMnemonic(FIXTURE_MNEMONIC);

function accountKey(change: number, index: number): HDKey {
  return master.derive(`${ACCOUNT_PATH}/${change}/${index}`);
}

function xOnly(key: HDKey): Uint8Array {
  return key.publicKey!.slice(1);
}

function pathOf(change: number, index: number): number[] {
  const h = 0x80000000;
  return [86 + h, 1 + h, 0 + h, change, index];
}

/**
 * Build the unsigned PSBT the wallet service would prepare: one own p2tr
 * input (with key origin), one recipient output, one own change output.
 */
function buildUnsignedPsbt(recipientScriptHex: string, amountSat: bigint): string {
  const own = accountKey(0, 0);
  const ownSpend = p2tr(xOnly(own), undefined, REGTEST);
  const change = accountKey(1, 0);
  const changeSpend = p2tr(xOnly(change), undefined, REGTEST);
  const tx = new Transaction({ allowUnknownOutputs: true });
  tx.addInput({
    txid: new Uint8Array(32).fill(7),
    index: 0,
    witnessUtxo: { script: ownSpend.script, amount: 100_000n },
    tapInternalKey: xOnly(own),
    tapBip32Derivation: [
      [xOnly(own), { hashes: [], der: { fingerprint: master.fingerprint, path: pathOf(0, 0) } }],
    ],
  });
  tx.addOutput({ script: hex.decode(recipientScriptHex), amount: amountSat });
  tx.addOutput({
    script: changeSpend.script,
    amount: 100_000n - amountSat - 500n,
  });
  return base64.encode(tx.toPSBT());
}

function signPsbt(psbtBase64: string): string {
  return signPsbtWithMaster(master, psbtBase64);
}

describe('prepare→sign→complete round-trip with a fixture signer', () => {
  let app: FastifyInstance;
  let backend: MockWalletBackend;
  let user: TestUser;
  const recipientScriptHex = '51203b82b2b2a9185315da6f80da5f06d0440d8a5e1457fa93387c2d919c86ec8786';
  const recipientAddress = 'bcrt1p8wpt9v4frpf3tkn0srd97pksgsxc5hs52lafxwru9kgeephvs7rqjeprhg';

  beforeEach(async () => {
    backend = new MockWalletBackend();
    app = await testServer({ walletBackend: backend });
    user = await createTestUser(app);
    await app.inject({
      method: 'POST',
      url: '/v1/wallet/xpubs',
      headers: { authorization: `Bearer ${user.token}` },
      payload: XPUBS,
    });
  });

  afterEach(async () => {
    await app.close();
  });

  function headers() {
    return {
      authorization: `Bearer ${user.token}`,
      'idempotency-key': randomUUID(),
    };
  }

  it('signs the prepared PSBT from PSBT key origins and completes with the real txid', async () => {
    const unsignedPsbt = buildUnsignedPsbt(recipientScriptHex, 40_000n);
    const expectedTxid = Transaction.fromPSBT(base64.decode(unsignedPsbt), PSBT_PARSE_OPTIONS).id;
    backend.dataFor = () => ({ preparedPsbt: unsignedPsbt });
    // Reopen with fixture data: evict the smoke-open handle by closing all.
    await app.walletPool.closeAll();

    const prepared = await app.inject({
      method: 'POST',
      url: '/v1/onchain/send-btc/prepare',
      headers: headers(),
      payload: { address: recipientAddress, amountSat: 40_000 },
    });
    expect(prepared.statusCode).toBe(201);
    const { opId, psbt, intent } = prepared.json();
    expect(psbt).toBe(unsignedPsbt);

    // Client-side: the recipient script in the intent matches the PSBT output.
    const parsed = Transaction.fromPSBT(base64.decode(psbt), PSBT_PARSE_OPTIONS);
    expect(hex.encode(parsed.getOutput(0).script!)).toBe(intent.recipients[0].scriptHex);
    expect(parsed.getOutput(0).amount).toBe(BigInt(intent.recipients[0].amountSat));

    const signedPsbt = signPsbt(psbt);

    // "Broadcast": assert a finalized 64/65-byte schnorr witness is present.
    const handle = backend.handleFor(user.userId)!;
    const originalEnd = handle.sendBtcEnd.bind(handle);
    handle.sendBtcEnd = async (received: string) => {
      const done = Transaction.fromPSBT(base64.decode(received), PSBT_PARSE_OPTIONS);
      const witness = done.getInput(0).finalScriptWitness;
      if (witness === undefined || witness.length !== 1 || witness[0]!.length < 64) {
        throw new WalletBackendError('wallet sendBtcEnd failed', 'missing schnorr signature');
      }
      await originalEnd(received);
      return done.id;
    };

    const completed = await app.inject({
      method: 'POST',
      url: '/v1/onchain/send-btc/complete',
      headers: headers(),
      payload: { opId, signedPsbt },
    });
    expect(completed.statusCode).toBe(200);
    expect(completed.json()).toEqual({ txid: expectedTxid });
  });

  it('rejects an unsigned PSBT at the finalize boundary as a clean 400', async () => {
    const unsignedPsbt = buildUnsignedPsbt(recipientScriptHex, 40_000n);
    backend.dataFor = () => ({ preparedPsbt: unsignedPsbt });
    await app.walletPool.closeAll();

    const prepared = await app.inject({
      method: 'POST',
      url: '/v1/onchain/send-btc/prepare',
      headers: headers(),
      payload: { address: recipientAddress, amountSat: 40_000 },
    });
    const { opId } = prepared.json();

    const handle = backend.handleFor(user.userId)!;
    handle.sendBtcEnd = async (received: string) => {
      const done = Transaction.fromPSBT(base64.decode(received), PSBT_PARSE_OPTIONS);
      if (done.getInput(0).finalScriptWitness === undefined) {
        // What real rgb-lib raises here: extract_tx succeeds on a witness-less
        // PSBT, the indexer refuses the transaction, and broadcast_tx confirms
        // the txid has no confirmations before reporting FailedBroadcast
        // (rgb-lib wallet/online.rs:47-86).
        throw new WalletBackendError(
          'wallet sendBtcEnd failed',
          'RgbLib(FailedBroadcast { details: "sendrawtransaction: non-mandatory-script-verify-flag" })',
        );
      }
      return done.id;
    };

    // Skip signing entirely: send the unsigned PSBT back.
    const completed = await app.inject({
      method: 'POST',
      url: '/v1/onchain/send-btc/complete',
      headers: headers(),
      payload: { opId, signedPsbt: unsignedPsbt },
    });
    expect(completed.statusCode).toBe(400);
    expect(completed.json().error.code).toBe('PSBT_REJECTED');
  });
});
