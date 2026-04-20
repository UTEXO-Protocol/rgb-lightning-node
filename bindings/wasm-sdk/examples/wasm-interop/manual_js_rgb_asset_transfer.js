import init, {
  RlnWasmInvoice,
  RlnWasmSdk,
  rgbRestoreKeysValue,
} from "../../pkg/rln_wasm_sdk.js";

const DEFAULT_INDEXER_URL = "http://127.0.0.1:3002";
const DEFAULT_TRANSPORT_ENDPOINT = "rpc://127.0.0.1:3000/json-rpc";
const FIXED_LIFECYCLE_MNEMONIC =
  "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
const FIXED_SENDER_MNEMONIC =
  "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
const FIXED_RECEIVER_MNEMONIC =
  "legal winner thank year wave sausage worth useful legal winner thank yellow";

function log(message, data = undefined) {
  const out = document.getElementById("out");
  if (!out) return;
  const line = document.createElement("pre");
  line.textContent =
    data === undefined
      ? String(message)
      : `${message}: ${JSON.stringify(data, null, 2)}`;
  out.appendChild(line);
}

function readText(id) {
  const el = document.getElementById(id);
  return el && typeof el.value === "string" ? el.value.trim() : "";
}

function readPositiveInt(id, fallback) {
  const raw = readText(id);
  if (!raw) return fallback;
  const n = Number.parseInt(raw, 10);
  if (!Number.isFinite(n) || n <= 0) {
    throw new Error(`${id} must be a positive integer`);
  }
  return n;
}

function senderFundingHint(senderAddress) {
  return {
    sender_address: senderAddress,
    action: "Fund this sender address on regtest and mine >= 1 block (recommended: 6).",
    example_amount_btc: 1,
    js_hook: "Optionally define window.regtestFund({ address, amountBtc, mineBlocks })",
  };
}

function buildWalletDataFromGeneratedKeys(keys, role) {
  return {
    data_dir: `/tmp/rln_wasm_${role}_fixed`,
    bitcoin_network: "Regtest",
    database_type: "Sqlite",
    max_allocations_per_utxo: 5,
    account_xpub_vanilla: keys.account_xpub_vanilla,
    account_xpub_colored: keys.account_xpub_colored,
    mnemonic: keys.mnemonic,
    master_fingerprint: keys.master_fingerprint,
    vanilla_keychain: null,
    supported_schemas: ["Nia"],
  };
}

async function createRlnInstance(role, walletMnemonic, transportEndpoint, lifecycle) {
  const keys = rgbRestoreKeysValue("regtest", walletMnemonic);
  const walletData = buildWalletDataFromGeneratedKeys(keys, role);
  const walletDataJson = JSON.stringify(walletData);

  const sdk = new RlnWasmSdk();
  await sdk.initValue(lifecycle.password, lifecycle.mnemonic);
  await sdk.unlock(JSON.stringify({ password: lifecycle.password }));

  const node = sdk.createNodeHandle(transportEndpoint);
  const wallet = await sdk.createWallet(walletDataJson);
  node.attachWallet(wallet);

  return { sdk, node, wallet };
}

async function signPsbt(unsignedPsbt) {
  if (typeof window.signPsbt === "function") {
    const signed = await window.signPsbt(unsignedPsbt);
    if (!signed || typeof signed !== "string") {
      throw new Error("window.signPsbt returned an invalid signed PSBT");
    }
    return signed;
  }

  throw new Error(
    "No signer configured. Inject window.signPsbt(unsignedPsbt) => signedPsbt before running the flow."
  );
}

async function ensureRgbAllocations(walletHandle, online) {
  const before = walletHandle.getBtcBalanceValue();
  const coloredSpendable =
    before && before.colored && typeof before.colored.spendable === "number"
      ? before.colored.spendable
      : 0;
  if (coloredSpendable > 0) {
    return before;
  }

  log("No colored allocations, creating UTXOs", { before });
  const unsignedPsbt = await walletHandle.createUtxosBegin(
    online,
    true,
    5,
    undefined,
    1n,
    false
  );
  log("createUtxosBegin ready", { unsigned_psbt_length: unsignedPsbt.length });

  const signedPsbt = await signPsbt(unsignedPsbt);
  const created = await walletHandle.createUtxosEnd(online, signedPsbt, false);
  log("createUtxosEnd result", { created });

  await walletHandle.syncOnline(online);
  const after = walletHandle.getBtcBalanceValue();
  log("BTC balances after createUtxos", after);
  return after;
}

async function tryAutoFundSender(senderAddress, senderWallet, senderOnline) {
  if (typeof window.regtestFund !== "function") {
    return false;
  }

  await window.regtestFund({
    address: senderAddress,
    amountBtc: 1,
    mineBlocks: 6,
  });
  await senderWallet.syncOnline(senderOnline);

  const refreshed = senderWallet.getBtcBalanceValue();
  const spendable =
    refreshed &&
    refreshed.vanilla &&
    typeof refreshed.vanilla.spendable === "number"
      ? refreshed.vanilla.spendable
      : 0;
  return spendable > 0;
}

function pickInvoiceString(invoiceResponse) {
  if (typeof invoiceResponse === "string") return invoiceResponse;
  if (invoiceResponse && typeof invoiceResponse.invoice === "string") {
    return invoiceResponse.invoice;
  }
  if (invoiceResponse && typeof invoiceResponse.invoice_string === "string") {
    return invoiceResponse.invoice_string;
  }
  throw new Error("blindReceiveValue response does not contain invoice string");
}

function pickTransportEndpoints(invoiceData, fallbackEndpoint) {
  const endpoints =
    invoiceData.transport_endpoints || invoiceData.transportEndpoints || [];
  if (Array.isArray(endpoints) && endpoints.length > 0) {
    return endpoints;
  }
  if (fallbackEndpoint) {
    return [fallbackEndpoint];
  }
  throw new Error("No transport endpoints available for recipient");
}

async function run() {
  const out = document.getElementById("out");
  if (out) out.innerHTML = "";

  await init();
  log("WASM init", { ok: true });

  const indexerUrl = DEFAULT_INDEXER_URL;
  const transportEndpoint = DEFAULT_TRANSPORT_ENDPOINT;
  const issueAmount = readPositiveInt("issueAmount", 1000);
  const sendAmount = readPositiveInt("sendAmount", 100);
  if (sendAmount > issueAmount) {
    throw new Error("sendAmount cannot be greater than issueAmount");
  }

  const lifecycle = {
    password: "rln-fixed-password",
    mnemonic: FIXED_LIFECYCLE_MNEMONIC,
  };
  const senderRln = await createRlnInstance(
    "sender",
    FIXED_SENDER_MNEMONIC,
    transportEndpoint,
    lifecycle
  );
  const receiverRln = await createRlnInstance(
    "receiver",
    FIXED_RECEIVER_MNEMONIC,
    transportEndpoint,
    lifecycle
  );
  const sender = senderRln.wallet;
  const receiver = receiverRln.wallet;
  const senderNode = senderRln.node;
  const receiverNode = receiverRln.node;
  log("Two RLN instances initialized and unlocked", { ok: true });
  log("Using fixed endpoints", { indexerUrl, transportEndpoint });
  log("Wallet addresses", {
    sender_address: sender.getAddress(),
    receiver_address: receiver.getAddress(),
  });

  const senderOnline = await sender.goOnlineValue(false, indexerUrl);
  const receiverOnline = await receiver.goOnlineValue(false, indexerUrl);
  log("Wallets online", {
    sender_online_id: senderOnline.id,
    receiver_online_id: receiverOnline.id,
  });

  await sender.syncOnline(senderOnline);
  await receiver.syncOnline(receiverOnline);
  log("Initial sync complete", { ok: true });
  const btcBefore = {
    sender: sender.getBtcBalanceValue(),
    receiver: receiver.getBtcBalanceValue(),
  };
  log("BTC balances before issue", btcBefore);
  const senderSpendableBefore =
    btcBefore.sender &&
    btcBefore.sender.vanilla &&
    typeof btcBefore.sender.vanilla.spendable === "number"
      ? btcBefore.sender.vanilla.spendable
      : 0;
  if (senderSpendableBefore <= 0) {
    const senderAddress = sender.getAddress();
    const fundedViaHook = await tryAutoFundSender(senderAddress, sender, senderOnline);
    if (!fundedViaHook) {
      log("Sender wallet requires regtest funding", senderFundingHint(senderAddress));
      throw new Error("Sender spendable BTC is zero. Fund sender wallet and rerun.");
    }
    log("Sender auto-funded via window.regtestFund", { ok: true });
  }
  await ensureRgbAllocations(sender, senderOnline);

  const issueReq = {
    ticker: "TST",
    name: "WASM RLN Demo",
    precision: 0,
    amounts: [issueAmount],
  };
  const issued = senderNode.issueAssetNiaValue(issueReq);
  const assetId = issued.asset_id;
  log("Asset issued", issued);

  const receiveData = receiver.blindReceiveValue(
    assetId,
    { Fungible: sendAmount },
    3600,
    [transportEndpoint],
    1
  );
  log("Receiver blind invoice", receiveData);

  const invoiceString = pickInvoiceString(receiveData);
  const invoiceObj = new RlnWasmInvoice(invoiceString);
  const invoiceData = invoiceObj.invoiceDataValue();
  log("Decoded RGB invoice", invoiceData);
  log("Receiver node info", receiverNode.nodeInfoValue());

  const recipient = {
    recipient_id: invoiceData.recipient_id || invoiceData.recipientId,
    witness_data: null,
    assignment: { Fungible: sendAmount },
    transport_endpoints: pickTransportEndpoints(invoiceData, transportEndpoint),
  };
  if (!recipient.recipient_id) {
    throw new Error("Recipient id missing in decoded RGB invoice");
  }

  const recipientMap = {
    [assetId]: [recipient],
  };

  const unsignedPsbt = await sender.sendBegin(senderOnline, recipientMap, false, 1n, 1);
  log("Unsigned PSBT ready", { unsigned_psbt_length: unsignedPsbt.length });

  const signedPsbt = await signPsbt(unsignedPsbt);
  log("Signed PSBT obtained", { signed_psbt_length: signedPsbt.length });

  const sendResult = await sender.sendEndValue(senderOnline, signedPsbt, false);
  log("sendEnd result", sendResult);

  await sender.syncOnline(senderOnline);
  await receiver.syncOnline(receiverOnline);

  const senderBalance = sender.getAssetBalanceValue(assetId);
  const receiverBalance = receiver.getAssetBalanceValue(assetId);
  log("Sender asset balance", senderBalance);
  log("Receiver asset balance", receiverBalance);

  log("RGB transfer flow completed", {
    ok: true,
    asset_id: assetId,
    sent_amount: sendAmount,
  });
}

const runBtn = document.getElementById("run");
if (runBtn) {
  runBtn.addEventListener("click", () => {
    run().catch((err) => {
      log("RGB transfer flow failed", String(err));
    });
  });
}
