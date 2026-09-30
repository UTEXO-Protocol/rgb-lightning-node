// Preflight A (Task 1, ran green 2026-09-09): verify @utexo/rgb-lib@0.3.0-beta.18
// supports a watch-only (mnemonic-less) wallet for: getAddress, listUnspents,
// blindReceive, witnessReceive, createUtxosBegin/End, sendBegin/End — signing done
// by a separate wallet instance standing in for the client-side signer.
//
// NOTE: the published wrapper.js is stale (1-arg new_wallet, 6-arg sendBegin);
// this script calls the native module directly with the compiled signatures.
// See minimal-sdk/README.md ("rgb-lib binding status") for the marshalling rules.
//
// To run: bring up regtest (README), then from minimal-sdk/ (which ships the
// dependency): `node scripts/preflight-a.cjs`. Linux-x64 only (the require
// below loads the linux-x64 native package directly).
// Task 6 reuses this script to generate rgb-lib derivation fixtures.
const lib = require('@utexo/rgb-lib-linux-x64/rgblib');
const fs = require('fs');
const { execSync } = require('child_process');

const ELECTRUM = 'localhost:50001';
const PROXY = 'rpc://localhost:3000/json-rpc';
const path = require('path');
const RLN_REPO = path.join(__dirname, '..', '..');
const BASE = '/tmp/preflight-rgb-lib/run';

fs.rmSync(BASE, { recursive: true, force: true });
for (const d of ['watch', 'signer', 'recipient']) fs.mkdirSync(`${BASE}/${d}`, { recursive: true });

const sh = (cmd) => execSync(cmd, { cwd: RLN_REPO, encoding: 'utf8' }).trim();
const step = (msg) => console.log(`\n== ${msg}`);

const wd = (dir) =>
  JSON.stringify({
    dataDir: dir,
    bitcoinNetwork: 'Regtest',
    databaseType: 'Sqlite',
    maxAllocationsPerUtxo: '1',
    accountXpubVanilla: k1.accountXpubVanilla,
    accountXpubColored: k1.accountXpubColored,
    vanillaKeychain: null,
    supportedSchemas: ['Nia', 'Cfa', 'Uda', 'Ifa'],
  });
const keysJson = (k, withMnemonic) =>
  JSON.stringify({
    accountXpubVanilla: k.accountXpubVanilla,
    accountXpubColored: k.accountXpubColored,
    vanillaKeychain: null,
    masterFingerprint: k.masterFingerprint,
    mnemonic: withMnemonic ? k.mnemonic : null,
  });

step('generate keys (two users)');
const k1 = JSON.parse(lib.rgblib_generate_keys('Regtest'));
const k2 = JSON.parse(lib.rgblib_generate_keys('Regtest'));
console.log('k1 fingerprint:', k1.masterFingerprint);

step('construct wallets: watch-only (server), signer (client stand-in), recipient');
const W = lib.rgblib_new_wallet(wd(`${BASE}/watch`), keysJson(k1, false));
const S = lib.rgblib_new_wallet(wd(`${BASE}/signer`), keysJson(k1, true));
const wdR = JSON.stringify({
  dataDir: `${BASE}/recipient`,
  bitcoinNetwork: 'Regtest',
  databaseType: 'Sqlite',
  maxAllocationsPerUtxo: '1',
  accountXpubVanilla: k2.accountXpubVanilla,
  accountXpubColored: k2.accountXpubColored,
  vanillaKeychain: null,
  supportedSchemas: ['Nia', 'Cfa', 'Uda', 'Ifa'],
});
const R = lib.rgblib_new_wallet(wdR, keysJson(k2, true));
console.log('wallets constructed OK (watch-only construction: PASS)');

step('goOnline');
const onW = lib.rgblib_go_online(W, false, ELECTRUM);
const onR = lib.rgblib_go_online(R, false, ELECTRUM);
console.log('online OK');

step('getAddress (watch-only)');
const addrW = lib.rgblib_get_address(W);
const addrR = lib.rgblib_get_address(R);
console.log('addrW:', addrW);

step('fund both wallets from regtest miner');
sh(`./regtest.sh sendtoaddress ${addrW} 1`);
sh(`./regtest.sh sendtoaddress ${addrR} 1`);
sh(`./regtest.sh mine 1`);

step('listUnspents (watch-only, with sync)');
const unspents = JSON.parse(lib.rgblib_list_unspents(W, onW, false, false));
console.log('watch-only unspents:', unspents.length);
if (unspents.length < 1) throw new Error('no unspents after funding');

step('createUtxosBegin (watch-only) -> external sign -> createUtxosEnd');
const cuPsbt = lib.rgblib_create_utxos_begin(W, onW, false, '5', '10000', '2', false);
console.log('unsigned PSBT (base64) length:', cuPsbt.length);
const cuSigned = lib.rgblib_sign_psbt(S, cuPsbt);
const cuCount = lib.rgblib_create_utxos_end(W, onW, cuSigned, false);
console.log('createUtxos count:', cuCount, '(PASS)');
sh(`./regtest.sh mine 1`);

step('recipient createUtxos (full wallet, internal signing)');
lib.rgblib_create_utxos(R, onR, false, '5', '10000', '2', false);
sh(`./regtest.sh mine 1`);

step('issueAssetNIA on watch-only wallet');
const asset = JSON.parse(
  lib.rgblib_issue_asset_nia(W, 'PRE', 'Preflight', '0', JSON.stringify(['1000'])),
);
const assetId = asset.assetId;
console.log('assetId:', assetId);

step('blindReceive + witnessReceive on watch-only wallet');
const exp = String(Math.floor(Date.now() / 1000) + 86400); // SWIG rejects JS null for char*: optionals must be materialized
const blindW = JSON.parse(
  lib.rgblib_blind_receive(
    W,
    assetId,
    JSON.stringify({ Fungible: 1 }),
    exp,
    JSON.stringify([PROXY]),
    '1',
  ),
);
console.log('blindReceive OK:', blindW.recipientId);
const witnessW = JSON.parse(
  lib.rgblib_witness_receive(
    W,
    assetId,
    JSON.stringify({ Fungible: 1 }),
    exp,
    JSON.stringify([PROXY]),
    '1',
  ),
);
console.log('witnessReceive OK:', witnessW.recipientId);

step('recipient blindReceive (target for the send)');
// recipient does not know the asset yet -> asset-agnostic invoice (assetId null works via SWIG)
const blindR = JSON.parse(
  lib.rgblib_blind_receive(
    R,
    null,
    JSON.stringify({ Fungible: 100 }),
    exp,
    JSON.stringify([PROXY]),
    '1',
  ),
);
console.log('recipient blind invoice:', blindR.invoice.slice(0, 60), '...');

step('sendBegin (watch-only) -> external sign -> sendEnd');
const recipientMap = {
  [assetId]: [
    {
      recipientId: blindR.recipientId,
      witnessData: null,
      assignment: { Fungible: 100 },
      transportEndpoints: [PROXY],
    },
  ],
};
// NB: last arg is dry_run (not skip_sync); result is JSON-serialized (unlike create_utxos_begin)
const sendBeginRaw = lib.rgblib_send_begin(
  W,
  onW,
  JSON.stringify(recipientMap),
  false,
  '2',
  '1',
  exp,
  false,
);
const sendBeginRes = JSON.parse(sendBeginRaw);
const sendPsbt = typeof sendBeginRes === 'string' ? sendBeginRes : sendBeginRes.psbt;
console.log('send_begin result type:', typeof sendBeginRes, '| PSBT length:', sendPsbt.length);
const sendSigned = lib.rgblib_sign_psbt(S, sendPsbt);
const sendResult = JSON.parse(lib.rgblib_send_end(W, onW, sendSigned, false));
console.log('sendEnd OK, txid:', sendResult.txid, '(PASS)');

step('settle: refresh both sides, mine, refresh again');
lib.rgblib_refresh(R, onR, null, JSON.stringify([]), false);
lib.rgblib_refresh(W, onW, null, JSON.stringify([]), false);
sh(`./regtest.sh mine 1`);
lib.rgblib_refresh(R, onR, null, JSON.stringify([]), false);
lib.rgblib_refresh(W, onW, null, JSON.stringify([]), false);
const balR = JSON.parse(lib.rgblib_get_asset_balance(R, assetId));
console.log('recipient asset balance:', JSON.stringify(balR));

console.log('\nPREFLIGHT A: ALL CHECKS PASSED');
