import init, {
  RlnWasmInvoice,
  RlnWasmSdk,
  rgbGenerateKeysValue,
} from "../../pkg/rln_wasm_sdk.js";

const DEFAULT_INDEXER_URL = "http://127.0.0.1:3002";
const DEFAULT_TRANSPORT_ENDPOINT = "rpc://127.0.0.1:3000/json-rpc";

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

function parseWalletData(id) {
  const raw = readText(id);
  if (!raw) {
    throw new Error(`${id} cannot be empty`);
  }
  let parsed;
  try {
    parsed = JSON.parse(raw);
  } catch (err) {
    throw new Error(`${id} is not valid JSON: ${String(err)}`);
  }
  return JSON.stringify(parsed);
}

function buildWalletDataFromGeneratedKeys(keys, role, seed) {
  return {
    data_dir: `/tmp/rln_wasm_${role}_${seed}`,
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

function ensureWalletDataInputsFilled() {
  const senderWalletInput = document.getElementById("senderWalletData");
  const receiverWalletInput = document.getElementById("receiverWalletData");
  if (!senderWalletInput || !receiverWalletInput) {
    return;
  }
  if (senderWalletInput.value.trim() && receiverWalletInput.value.trim()) {
    return;
  }

  const seed = Date.now().toString();
  const senderKeys = rgbGenerateKeysValue("regtest");
  const receiverKeys = rgbGenerateKeysValue("regtest");
  const senderWalletData = buildWalletDataFromGeneratedKeys(senderKeys, "sender", seed);
  const receiverWalletData = buildWalletDataFromGeneratedKeys(receiverKeys, "receiver", seed);

  senderWalletInput.value = JSON.stringify(senderWalletData, null, 2);
  receiverWalletInput.value = JSON.stringify(receiverWalletData, null, 2);
  log("WalletData generated at runtime", {
    sender_mnemonic_words: senderKeys.mnemonic.split(" ").length,
    receiver_mnemonic_words: receiverKeys.mnemonic.split(" ").length,
  });
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
  ensureWalletDataInputsFilled();

  const indexerUrl = readText("indexerUrl");
  const transportEndpoint = readText("transportEndpoint");
  const issueAmount = readPositiveInt("issueAmount", 1000);
  const sendAmount = readPositiveInt("sendAmount", 100);

  if (!indexerUrl) {
    throw new Error("indexerUrl cannot be empty");
  }
  if (!transportEndpoint) {
    throw new Error("transportEndpoint cannot be empty");
  }
  if (sendAmount > issueAmount) {
    throw new Error("sendAmount cannot be greater than issueAmount");
  }

  const senderWalletDataJson = parseWalletData("senderWalletData");
  const receiverWalletDataJson = parseWalletData("receiverWalletData");

  const sdk = new RlnWasmSdk();
  const sender = await sdk.createWalletHandleAsync(senderWalletDataJson);
  const receiver = await sdk.createWalletHandleAsync(receiverWalletDataJson);
  log("Wallet handles created", { ok: true });
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
  log("BTC balances before issue", {
    sender: sender.getBtcBalanceValue(),
    receiver: receiver.getBtcBalanceValue(),
  });
  await ensureRgbAllocations(sender, senderOnline);

  const issueReq = {
    ticker: "TST",
    name: "WASM RGB Demo",
    precision: 0,
    amounts: [issueAmount],
  };
  const issued = sender.issueAssetNiaValue(issueReq);
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

const indexerInput = document.getElementById("indexerUrl");
if (indexerInput && !indexerInput.value.trim()) {
  indexerInput.value = DEFAULT_INDEXER_URL;
}

const transportInput = document.getElementById("transportEndpoint");
if (transportInput && !transportInput.value.trim()) {
  transportInput.value = DEFAULT_TRANSPORT_ENDPOINT;
}

const senderWalletInput = document.getElementById("senderWalletData");
const receiverWalletInput = document.getElementById("receiverWalletData");
if (
  senderWalletInput &&
  receiverWalletInput &&
  !senderWalletInput.value.trim() &&
  !receiverWalletInput.value.trim()
) {
  // Generate once on page load for convenience.
  // A new runtime-generated pair will also be created in run() when fields are empty.
  const seed = Date.now().toString();
  const senderKeys = rgbGenerateKeysValue("regtest");
  const receiverKeys = rgbGenerateKeysValue("regtest");
  senderWalletInput.value = JSON.stringify(
    buildWalletDataFromGeneratedKeys(senderKeys, "sender", seed),
    null,
    2
  );
  receiverWalletInput.value = JSON.stringify(
    buildWalletDataFromGeneratedKeys(receiverKeys, "receiver", seed),
    null,
    2
  );
}
