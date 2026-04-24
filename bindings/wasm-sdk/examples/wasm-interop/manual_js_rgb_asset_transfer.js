import init, {
  RlnWasmInvoice,
  RlnWasmSdk,
  rgbRestoreKeysValue,
} from "../../pkg/rln_wasm_sdk.js";

const DEFAULT_INDEXER_URL = "http://127.0.0.1:3002";
const DEFAULT_NODE_PROXY_URL = "ws://127.0.0.1:3001";
const DEFAULT_TRANSPORT_ENDPOINT = "http://127.0.0.1:3001/rgb/json-rpc";
const DEFAULT_FUND_AMOUNT_BTC = 1;
const DEFAULT_FUND_MINE_BLOCKS = 6;
const MIN_VANILLA_SPENDABLE_FOR_RGB_SEND_SAT = 5000;
const MIN_VANILLA_SETTLED_FOR_RGB_SEND_SAT = 2000;
const VERBOSE_LOGS = false;
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

function logVerbose(message, data = undefined) {
  if (!VERBOSE_LOGS) return;
  log(message, data);
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

function toAmountNumber(value) {
  if (typeof value === "number") return Number.isFinite(value) ? value : 0;
  if (typeof value === "bigint") return Number(value);
  if (typeof value === "string" && value.trim().length > 0) {
    const parsed = Number(value.trim());
    return Number.isFinite(parsed) ? parsed : 0;
  }
  return 0;
}

function readBalanceAmount(balanceObj, bucket, field) {
  if (!balanceObj || typeof balanceObj !== "object") return 0;
  const bucketObj = balanceObj[bucket];
  if (!bucketObj || typeof bucketObj !== "object") return 0;
  return toAmountNumber(bucketObj[field]);
}

function readVanillaStats(balanceObj) {
  return {
    settled: readBalanceAmount(balanceObj, "vanilla", "settled"),
    spendable: readBalanceAmount(balanceObj, "vanilla", "spendable"),
    future: readBalanceAmount(balanceObj, "vanilla", "future"),
  };
}

function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

function senderFundingHint(senderAddress) {
  return {
    sender_address: senderAddress,
    action: "Fund this sender address on regtest and mine >= 1 block (recommended: 6).",
    example_amount_btc: 1,
    auto_fund_endpoint:
      "POST <gateway>/dev/regtest/fund (enabled by default in wasm-proxy-gateway local dev)",
    js_hook: "Optional override: window.regtestFund({ address, amountBtc, mineBlocks })",
    manual_fallback: "./regtest.sh sendtoaddress <address> 1 && ./regtest.sh mine 6",
    indexer_note:
      "WASM wallet online mode requires a valid Esplora URL (default: http://127.0.0.1:3002).",
  };
}

function toHttpOrigin(url) {
  if (!url) return null;
  try {
    const parsed = new URL(url);
    if (parsed.protocol === "ws:") parsed.protocol = "http:";
    if (parsed.protocol === "wss:") parsed.protocol = "https:";
    return parsed.origin;
  } catch (_err) {
    return null;
  }
}

function toRgbTransportEndpoint(url) {
  if (!url) return "";
  const trimmed = String(url).trim();
  if (trimmed.startsWith("rpc://")) {
    return trimmed;
  }
  if (trimmed.startsWith("http://")) {
    return `rpc://${trimmed.slice("http://".length)}`;
  }
  if (trimmed.startsWith("https://")) {
    return `rpc://${trimmed.slice("https://".length)}`;
  }
  return trimmed;
}

async function tryGatewayAutoFund(nodeProxyUrl, senderAddress) {
  const base = toHttpOrigin(nodeProxyUrl);
  if (!base) {
    return false;
  }

  const endpoint = `${base}/dev/regtest/fund`;
  const response = await fetch(endpoint, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({
      address: senderAddress,
      amount_btc: DEFAULT_FUND_AMOUNT_BTC,
      mine_blocks: DEFAULT_FUND_MINE_BLOCKS,
    }),
  });
  if (!response.ok) {
    log("Gateway auto-fund request failed", {
      endpoint,
      status: response.status,
    });
    return false;
  }
  const body = await response.json().catch(() => ({}));
  log("Gateway auto-fund result", body);
  return true;
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

async function createRlnInstance(
  role,
  walletMnemonic,
  nodeProxyUrl,
  transportEndpoint,
  lifecycle
) {
  const keys = rgbRestoreKeysValue("regtest", walletMnemonic);
  const walletData = buildWalletDataFromGeneratedKeys(keys, role);
  const walletDataJson = JSON.stringify(walletData);

  const sdk = new RlnWasmSdk();
  await sdk.preloadPersistentRuntimeState();
  sdk.setDefaultRgbProxyTransport(transportEndpoint, null, null);
  await sdk.initValue(lifecycle.password, lifecycle.mnemonic);
  await sdk.unlock(JSON.stringify({ password: lifecycle.password }));

  const node = sdk.createNodeHandle(nodeProxyUrl);
  const wallet = await sdk.createWallet(walletDataJson);
  node.attachWallet(wallet);

  return { sdk, node, wallet };
}

async function signPsbt(walletHandle, unsignedPsbt) {
  if (
    walletHandle &&
    typeof walletHandle.signPsbtValue === "function"
  ) {
    const signed = walletHandle.signPsbtValue(unsignedPsbt);
    if (!signed || typeof signed !== "string") {
      throw new Error("wallet.signPsbtValue returned an invalid signed PSBT");
    }
    return signed;
  }

  if (typeof window.signPsbt === "function") {
    const signed = await window.signPsbt(unsignedPsbt);
    if (!signed || typeof signed !== "string") {
      throw new Error("window.signPsbt returned an invalid signed PSBT");
    }
    return signed;
  }

  throw new Error(
    "No signer configured. Use wallet.signPsbtValue or inject window.signPsbt(unsignedPsbt) => signedPsbt."
  );
}

async function ensureRgbAllocations(walletHandle, online) {
  const before = walletHandle.getBtcBalanceValue();
  const coloredSpendable = readBalanceAmount(before, "colored", "spendable");
  if (coloredSpendable > 0) {
    return before;
  }

  logVerbose("No colored allocations, creating UTXOs", { before });
  const unsignedPsbt = await walletHandle.createUtxosBegin(
    online,
    true,
    5,
    undefined,
    1n,
    false
  );
  logVerbose("createUtxosBegin ready", { unsigned_psbt_length: unsignedPsbt.length });

  const signedPsbt = await signPsbt(walletHandle, unsignedPsbt);
  const created = await walletHandle.createUtxosEnd(online, signedPsbt, false);
  logVerbose("createUtxosEnd result", { created });

  await walletHandle.syncOnline(online);
  const after = walletHandle.getBtcBalanceValue();
  logVerbose("BTC balances after createUtxos", after);
  return after;
}

async function tryAutoFundSender(
  senderAddress,
  senderWallet,
  senderOnline,
  nodeProxyUrl,
  minSpendableSat = 1,
  minSettledSat = 1
) {
  if (typeof window.regtestFund === "function") {
    await window.regtestFund({
      address: senderAddress,
      amountBtc: DEFAULT_FUND_AMOUNT_BTC,
      mineBlocks: DEFAULT_FUND_MINE_BLOCKS,
    });
  } else {
    const funded = await tryGatewayAutoFund(nodeProxyUrl, senderAddress);
    if (!funded) {
      return false;
    }
  }
  for (let i = 0; i < 60; i += 1) {
    await senderWallet.syncOnline(senderOnline);
    const refreshed = senderWallet.getBtcBalanceValue();
    const vanilla = readVanillaStats(refreshed);
    logVerbose("Wallet BTC balance after funding sync", { attempt: i + 1, vanilla });
    if (vanilla.spendable >= minSpendableSat && vanilla.settled >= minSettledSat) {
      return true;
    }
    await sleep(1000);
  }
  return false;
}

async function waitForSettledVanilla(senderWallet, senderOnline, minSettledSat, attempts) {
  for (let i = 0; i < attempts; i += 1) {
    await senderWallet.syncOnline(senderOnline);
    const refreshed = senderWallet.getBtcBalanceValue();
    const vanilla = readVanillaStats(refreshed);
    logVerbose("Waiting for settled vanilla balance", { attempt: i + 1, vanilla });
    if (vanilla.settled >= minSettledSat) {
      return true;
    }
    await sleep(1000);
  }
  return false;
}

async function ensureSenderVanillaFeeBudget(
  sender,
  senderOnline,
  nodeProxyUrl,
  minSpendableSat,
  minSettledSat
) {
  const before = sender.getBtcBalanceValue();
  const vanilla = readVanillaStats(before);
  logVerbose("Wallet vanilla balance check", { vanilla, minSpendableSat, minSettledSat });
  if (vanilla.spendable >= minSpendableSat && vanilla.settled >= minSettledSat) {
    return;
  }

  if (vanilla.spendable >= minSpendableSat && vanilla.settled < minSettledSat) {
    const settledReady = await waitForSettledVanilla(sender, senderOnline, minSettledSat, 30);
    if (settledReady) {
      return;
    }
  }

  const senderAddress = sender.getAddress();
  const funded = await tryAutoFundSender(
    senderAddress,
    sender,
    senderOnline,
    nodeProxyUrl,
    minSpendableSat,
    minSettledSat
  );
  if (!funded) {
    throw new Error(
      `Sender vanilla budget is insufficient after autofund (need spendable>=${minSpendableSat} and settled>=${minSettledSat})`
    );
  }
}

async function waitForAssetBalance(walletHandle, online, assetId, label, refreshAssetId = null) {
  for (let i = 0; i < 20; i += 1) {
    try {
      await walletHandle.refreshValue(online, refreshAssetId, [], false);
    } catch (_err) {
      // refresh can fail transiently while transport/indexer catches up
    }
    await walletHandle.syncOnline(online);
    try {
      const balance = walletHandle.getAssetBalanceValue(assetId);
      logVerbose(`${label} asset balance ready`, { attempt: i + 1, balance });
      return balance;
    } catch (_err) {
      await sleep(1000);
    }
  }
  throw new Error(`${label} asset balance not available yet for ${assetId}`);
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

  const indexerUrl = readText("indexerUrl") || DEFAULT_INDEXER_URL;
  const nodeProxyUrl = readText("nodeProxyUrl") || DEFAULT_NODE_PROXY_URL;
  const transportEndpoint =
    readText("transportEndpoint") || DEFAULT_TRANSPORT_ENDPOINT;
  const transportEndpointRgb = toRgbTransportEndpoint(transportEndpoint);
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
    nodeProxyUrl,
    transportEndpoint,
    lifecycle
  );
  const receiverRln = await createRlnInstance(
    "receiver",
    FIXED_RECEIVER_MNEMONIC,
    nodeProxyUrl,
    transportEndpoint,
    lifecycle
  );
  const sender = senderRln.wallet;
  const receiver = receiverRln.wallet;
  const senderNode = senderRln.node;
  const receiverNode = receiverRln.node;
  log("Two RLN instances initialized and unlocked", { ok: true });
  log("Using fixed endpoints", {
    indexerUrl,
    nodeProxyUrl,
    transportEndpoint,
    transportEndpointRgb,
  });
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
  try {
    await ensureSenderVanillaFeeBudget(
      sender,
      senderOnline,
      nodeProxyUrl,
      MIN_VANILLA_SPENDABLE_FOR_RGB_SEND_SAT,
      MIN_VANILLA_SETTLED_FOR_RGB_SEND_SAT
    );
  } catch (err) {
    const senderAddress = sender.getAddress();
    log("Sender wallet requires regtest funding", senderFundingHint(senderAddress));
    throw err;
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
  await sender.syncOnline(senderOnline);
  const afterIssueBtc = sender.getBtcBalanceValue();
  log("Sender BTC balance after issue", afterIssueBtc);
  await ensureSenderVanillaFeeBudget(
    sender,
    senderOnline,
    nodeProxyUrl,
    MIN_VANILLA_SPENDABLE_FOR_RGB_SEND_SAT,
    MIN_VANILLA_SETTLED_FOR_RGB_SEND_SAT
  );

  try {
    await ensureSenderVanillaFeeBudget(
      receiver,
      receiverOnline,
      nodeProxyUrl,
      MIN_VANILLA_SPENDABLE_FOR_RGB_SEND_SAT,
      MIN_VANILLA_SETTLED_FOR_RGB_SEND_SAT
    );
    await ensureRgbAllocations(receiver, receiverOnline);
  } catch (err) {
    log("Receiver wallet requires regtest funding", {
      receiver_address: receiver.getAddress(),
      action: "Fund receiver address and rerun",
      error: String(err),
    });
    throw err;
  }

  const receiveData = receiver.blindReceiveValue(
    null,
    { Fungible: sendAmount },
    3600,
    [transportEndpointRgb],
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
    transport_endpoints: pickTransportEndpoints(invoiceData, transportEndpointRgb),
  };
  if (!recipient.recipient_id) {
    throw new Error("Recipient id missing in decoded RGB invoice");
  }

  const recipientMap = {
    [assetId]: [recipient],
  };

  const unsignedPsbt = await sender.sendBegin(senderOnline, recipientMap, false, 1n, 1);
  log("Unsigned PSBT ready", { unsigned_psbt_length: unsignedPsbt.length });

  const signedPsbt = await signPsbt(sender, unsignedPsbt);
  log("Signed PSBT obtained", { signed_psbt_length: signedPsbt.length });

  const sendResult = await sender.sendEndValue(senderOnline, signedPsbt, false);
  log("sendEnd result", sendResult);

  await sender.syncOnline(senderOnline);
  await receiver.syncOnline(receiverOnline);

  const senderBalance = await waitForAssetBalance(
    sender,
    senderOnline,
    assetId,
    "Sender",
    assetId
  );
  const receiverBalance = await waitForAssetBalance(
    receiver,
    receiverOnline,
    assetId,
    "Receiver",
    null
  );
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
