// Regenerates test/fixtures/rgblib-parity.json from the REAL rgb-lib native
// binding (successor of Task 1's preflight script; see scripts/preflight-a.cjs
// at the workspace root). Everything here runs OFFLINE — no regtest needed:
// wallet construction, address derivation and BDK signing are all local.
//
// The fixture is the ground truth the client SDK must reproduce exactly:
//  - account xpubs + master fingerprint per network (restore_keys)
//  - vanilla- and colored-keychain addresses at indexes 0..4 (get_address;
//    colored addresses via a wallet whose *vanilla* side is the colored xpub —
//    both keychains use derivation child 0, verified in rgb-lib src/utils.rs)
//  - a PSBT hand-built with @scure, signed+finalized by rgb-lib's own BDK
//    signer (sign_psbt), with its txid and witness
//
// Run (needs the gateway package installed for the native module):
//   pnpm --filter @utexo/minimal-client-sdk generate:fixtures
import { createRequire } from 'node:module';
import { rmSync, mkdirSync, writeFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

const HERE = fileURLToPath(new URL('.', import.meta.url));
const OUT = new URL('../test/fixtures/rgblib-parity.json', import.meta.url);
const gatewayRequire = createRequire(new URL('../../gateway/package.json', import.meta.url));
const lib = gatewayRequire('@utexo/rgb-lib-linux-x64/rgblib');

const sdkRequire = createRequire(new URL('../package.json', import.meta.url));
const { Transaction, p2tr } = await import(sdkRequire.resolve('@scure/btc-signer'));
const { base64, hex } = await import(sdkRequire.resolve('@scure/base'));
const { HDKey } = await import(sdkRequire.resolve('@scure/bip32'));
const { mnemonicToSeedSync } = await import(sdkRequire.resolve('@scure/bip39'));

// TEST fixture only — same BIP-39 reference mnemonic the gateway integration
// suite uses. Mnemonics are never a gateway input (I1/I2).
const MNEMONIC =
  'abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about';
// Distinct mnemonic for the wallets' unused "other side" (BDK rejects
// identical external/internal descriptors) and for foreign-key test material.
const OTHER_MNEMONIC = 'zoo zoo zoo zoo zoo zoo zoo zoo zoo zoo zoo wrong';

const BASE = `${HERE}/../.fixture-tmp`;
rmSync(BASE, { recursive: true, force: true });

const networks = {};
for (const net of ['Regtest', 'Testnet', 'Signet', 'Mainnet']) {
  const keys = JSON.parse(lib.rgblib_restore_keys(net, MNEMONIC));
  networks[net] = {
    xpub: keys.xpub,
    accountXpubVanilla: keys.accountXpubVanilla,
    accountXpubColored: keys.accountXpubColored,
    masterFingerprint: keys.masterFingerprint,
  };
}
const k = networks.Regtest;
const other = JSON.parse(lib.rgblib_restore_keys('Regtest', OTHER_MNEMONIC));

function newWallet(dir, vanillaXpub, coloredXpub, mnemonic) {
  mkdirSync(dir, { recursive: true });
  const walletData = JSON.stringify({
    dataDir: dir,
    bitcoinNetwork: 'Regtest',
    databaseType: 'Sqlite',
    maxAllocationsPerUtxo: '1',
    accountXpubVanilla: vanillaXpub,
    accountXpubColored: coloredXpub,
    vanillaKeychain: null,
    supportedSchemas: ['Nia', 'Cfa', 'Uda', 'Ifa'],
  });
  const keysJson = JSON.stringify({
    accountXpubVanilla: vanillaXpub,
    accountXpubColored: coloredXpub,
    vanillaKeychain: null,
    masterFingerprint: k.masterFingerprint,
    mnemonic,
  });
  return lib.rgblib_new_wallet(walletData, keysJson);
}

const addresses = (wallet, count) =>
  Array.from({ length: count }, () => lib.rgblib_get_address(wallet));

const W = newWallet(`${BASE}/vanilla`, k.accountXpubVanilla, k.accountXpubColored, null);
const vanillaAddresses = addresses(W, 5);
const C = newWallet(`${BASE}/colored`, k.accountXpubColored, other.accountXpubColored, null);
const coloredAddresses = addresses(C, 5);

// Signing fixture: spend vanilla idx0, pay a foreign recipient, change to
// vanilla idx1, zero-value OP_RETURN — the exact shape verify-before-sign
// accepts. rgb-lib (BDK) signs and finalizes it; the SDK must produce the
// same txid from the same unsigned PSBT.
const REGTEST = { bech32: 'bcrt', pubKeyHash: 0x6f, scriptHash: 0xc4, wif: 0xef };
const master = HDKey.fromMasterSeed(mnemonicToSeedSync(MNEMONIC));
const account = master.derive("m/86'/1'/0'");
const HARD = 0x80000000;
const pathFor = (index) => [HARD + 86, HARD + 1, HARD, 0, index];
const keyAt = (index) => account.deriveChild(0).deriveChild(index);
const spendAt = (index) => p2tr(keyAt(index).publicKey.slice(1), undefined, REGTEST);

const otherMaster = HDKey.fromMasterSeed(mnemonicToSeedSync(OTHER_MNEMONIC));
const foreignKey = otherMaster.derive("m/86'/1'/0'/0/0");
const foreignSpend = p2tr(foreignKey.publicKey.slice(1), undefined, REGTEST);

const tx = new Transaction({ allowUnknownOutputs: true });
tx.addInput({
  txid: '5e2b3c1f8a9d4e6b7c0f1a2d3e4b5c6d7e8f9a0b1c2d3e4f5a6b7c8d9e0f1a2b',
  index: 1,
  witnessUtxo: { script: spendAt(0).script, amount: 100_000n },
  tapInternalKey: keyAt(0).publicKey.slice(1),
  tapBip32Derivation: [
    [
      keyAt(0).publicKey.slice(1),
      { hashes: [], der: { fingerprint: master.fingerprint, path: pathFor(0) } },
    ],
  ],
});
tx.addOutput({ script: foreignSpend.script, amount: 40_000n });
tx.addOutputAddress(spendAt(1).address, 59_000n, REGTEST);
tx.updateOutput(1, {
  tapInternalKey: keyAt(1).publicKey.slice(1),
  tapBip32Derivation: [
    [
      keyAt(1).publicKey.slice(1),
      { hashes: [], der: { fingerprint: master.fingerprint, path: pathFor(1) } },
    ],
  ],
});
tx.addOutput({ script: new Uint8Array([0x6a, 0x05, 1, 2, 3, 4, 5]), amount: 0n });
const unsignedPsbt = base64.encode(tx.toPSBT());

const S = newWallet(`${BASE}/signer`, k.accountXpubVanilla, k.accountXpubColored, MNEMONIC);
const signedPsbt = lib.rgblib_sign_psbt(S, unsignedPsbt);
const signedTx = Transaction.fromPSBT(base64.decode(signedPsbt), {
  allowUnknownInputs: true,
  allowUnknownOutputs: true,
});
const witness = signedTx.getInput(0).finalScriptWitness;
if (witness === undefined || witness.length !== 1 || witness[0].length !== 64) {
  throw new Error('rgb-lib did not key-path-sign the fixture PSBT');
}

// Real witness invoice produced OFFLINE by the watch-only wallet (blind
// invoices need colorable UTXOs, so those stay synthetic in the tests).
const witnessInvoice = JSON.parse(
  lib.rgblib_witness_receive(
    W,
    null,
    JSON.stringify('Any'),
    '2000000000',
    JSON.stringify(['rpc://localhost:3000/json-rpc']),
    '1',
  ),
);

const fixture = {
  generatedBy: 'scripts/generate-rgblib-fixtures.mjs (@utexo/rgb-lib 0.3.0-beta.18)',
  mnemonic: MNEMONIC,
  otherMnemonic: OTHER_MNEMONIC,
  networks,
  regtest: { vanillaAddresses, coloredAddresses },
  signing: {
    unsignedPsbt,
    signedPsbt,
    txid: signedTx.id,
    witnessSignature: hex.encode(witness[0]),
  },
  witnessReceive: {
    invoice: witnessInvoice.invoice,
    recipientId: witnessInvoice.recipientId,
    expirationTimestamp: witnessInvoice.expirationTimestamp,
  },
};

mkdirSync(new URL('../test/fixtures', import.meta.url), { recursive: true });
writeFileSync(OUT, `${JSON.stringify(fixture, null, 2)}\n`);
rmSync(BASE, { recursive: true, force: true });
console.log(`fixture written: txid ${signedTx.id}`);
console.log(`witness invoice: ${witnessInvoice.invoice}`);
