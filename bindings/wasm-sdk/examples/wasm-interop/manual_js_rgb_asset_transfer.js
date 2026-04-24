import init, { rgbRestoreKeysValue, RlnWasmSdk } from "../../pkg/rln_wasm_sdk.js";

const DEFAULT_INDEXER_URL = "http://127.0.0.1:3002";
const DEFAULT_NODE_PROXY_URL = "ws://127.0.0.1:3001";
const DEFAULT_TRANSPORT_ENDPOINT = "http://127.0.0.1:3001/rgb/json-rpc";
const DEFAULT_NODE_A_PEER_ADDR = "127.0.0.1:9745";
const DEFAULT_NODE_B_PEER_ADDR = "127.0.0.1:9746";
const DEFAULT_FUND_AMOUNT_BTC = 1;
const DEFAULT_FUND_MINE_BLOCKS = 6;
const MIN_VANILLA_SPENDABLE_SAT = 5000;
const MIN_VANILLA_SETTLED_SAT = 2000;
const OPEN_CHANNEL_CAPACITY_SAT = 500_000n;
const LN_RGB_PAYMENT_MSAT = 3_000_000n;
const INVOICE_EXPIRY_SEC = 3600;
const CHANNEL_READY_TIMEOUT_MS = 30_000;
const PAYMENT_READY_TIMEOUT_MS = 20_000;
const FIXED_LIFECYCLE_MNEMONIC =
  "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
const FIXED_SENDER_MNEMONIC =
  "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
const FIXED_RECEIVER_MNEMONIC =
  "legal winner thank year wave sausage worth useful legal winner thank yellow";

function safeJson(value) {
  return JSON.stringify(
    value,
    (_k, v) => (typeof v === "bigint" ? v.toString() : v),
    2
  );
}

function log(message, data = undefined) {
  const out = document.getElementById("out");
  if (!out) return;
  const line = document.createElement("pre");
  line.textContent =
    data === undefined
      ? String(message)
      : `${message}: ${safeJson(data)}`;
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

function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

function assertCondition(condition, errorMessage) {
  if (!condition) {
    throw new Error(errorMessage);
  }
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

function senderFundingHint(senderAddress) {
  return {
    sender_address: senderAddress,
    action: "Fund this sender address on regtest and mine >= 1 block (recommended: 6).",
    example_amount_btc: 1,
    auto_fund_endpoint:
      "POST <gateway>/dev/regtest/fund (enabled by default in wasm-proxy-gateway local dev)",
    js_hook: "Optional override: window.regtestFund({ address, amountBtc, mineBlocks })",
    manual_fallback: "./regtest.sh sendtoaddress <address> 1 && ./regtest.sh mine 6",
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
  if (trimmed.startsWith("rpc://")) return trimmed;
  if (trimmed.startsWith("http://")) return `rpc://${trimmed.slice("http://".length)}`;
  if (trimmed.startsWith("https://")) return `rpc://${trimmed.slice("https://".length)}`;
  return trimmed;
}

async function tryGatewayAutoFund(nodeProxyUrl, senderAddress) {
  const base = toHttpOrigin(nodeProxyUrl);
  if (!base) return false;
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

async function tryAutoFundWallet(address, wallet, online, nodeProxyUrl) {
  if (typeof window.regtestFund === "function") {
    await window.regtestFund({
      address,
      amountBtc: DEFAULT_FUND_AMOUNT_BTC,
      mineBlocks: DEFAULT_FUND_MINE_BLOCKS,
    });
  } else {
    const funded = await tryGatewayAutoFund(nodeProxyUrl, address);
    if (!funded) return false;
  }

  for (let i = 0; i < 60; i += 1) {
    await wallet.syncOnline(online);
    const refreshed = wallet.getBtcBalanceValue();
    const vanilla = readVanillaStats(refreshed);
    if (
      vanilla.spendable >= MIN_VANILLA_SPENDABLE_SAT &&
      vanilla.settled >= MIN_VANILLA_SETTLED_SAT
    ) {
      return true;
    }
    await sleep(1000);
  }
  return false;
}

async function waitForSettledVanilla(wallet, online, minSettledSat, attempts) {
  for (let i = 0; i < attempts; i += 1) {
    await wallet.syncOnline(online);
    const refreshed = wallet.getBtcBalanceValue();
    const vanilla = readVanillaStats(refreshed);
    if (vanilla.settled >= minSettledSat) {
      return true;
    }
    await sleep(1000);
  }
  return false;
}

async function ensureWalletVanillaBudget(
  label,
  wallet,
  online,
  nodeProxyUrl,
  fundingAddress
) {
  const before = wallet.getBtcBalanceValue();
  const vanilla = readVanillaStats(before);
  if (
    vanilla.spendable >= MIN_VANILLA_SPENDABLE_SAT &&
    vanilla.settled >= MIN_VANILLA_SETTLED_SAT
  ) {
    return;
  }
  if (
    vanilla.spendable >= MIN_VANILLA_SPENDABLE_SAT &&
    vanilla.settled < MIN_VANILLA_SETTLED_SAT
  ) {
    const settledReady = await waitForSettledVanilla(
      wallet,
      online,
      MIN_VANILLA_SETTLED_SAT,
      45
    );
    if (settledReady) {
      return;
    }
  }

  const walletAddress = String(fundingAddress || "").trim();
  if (!walletAddress) {
    throw new Error(`${label} fundingAddress cannot be empty`);
  }
  const funded = await tryAutoFundWallet(walletAddress, wallet, online, nodeProxyUrl);
  if (!funded) {
    if (label === "sender") {
      log("Sender wallet requires regtest funding", senderFundingHint(walletAddress));
    } else {
      log("Receiver wallet requires regtest funding", {
        receiver_address: walletAddress,
        action: "Fund this receiver address on regtest and mine >= 1 block (recommended: 6).",
      });
    }
    throw new Error(
      `${label} vanilla budget is insufficient (need spendable>=${MIN_VANILLA_SPENDABLE_SAT}, settled>=${MIN_VANILLA_SETTLED_SAT})`
    );
  }
}

async function ensureRgbAllocations(walletHandle, online) {
  const before = walletHandle.getBtcBalanceValue();
  const coloredSpendable = readBalanceAmount(before, "colored", "spendable");
  if (coloredSpendable > 0) return;
  const unsignedPsbt = await walletHandle.createUtxosBegin(
    online,
    true,
    5,
    undefined,
    1n,
    false
  );
  const signedPsbt = walletHandle.signPsbtValue(unsignedPsbt);
  await walletHandle.createUtxosEnd(online, signedPsbt, false);
  await walletHandle.syncOnline(online);
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
  runtimeId,
  walletMnemonic,
  nodeProxyUrl,
  transportEndpoint,
  lifecycle
) {
  const keys = rgbRestoreKeysValue("regtest", walletMnemonic);
  const walletData = buildWalletDataFromGeneratedKeys(keys, role);
  const walletDataJson = JSON.stringify(walletData);

  const sdk = new RlnWasmSdk();
  if (typeof sdk.setDefaultEnableVirtualChannelsV0 === "function") {
    sdk.setDefaultEnableVirtualChannelsV0(true);
  }
  await sdk.preloadPersistentRuntimeState();
  sdk.setDefaultRgbProxyTransport(transportEndpoint, null, null);
  await sdk.initValue(lifecycle.password, lifecycle.mnemonic);
  await sdk.unlock(JSON.stringify({ password: lifecycle.password }));

  const node = sdk.createNodeHandleWithRuntimeId(nodeProxyUrl, runtimeId);
  const wallet = await sdk.createWallet(walletDataJson);
  node.attachWallet(wallet);

  return { sdk, node, wallet };
}

function normalizeNodePubkey(value) {
  if (typeof value === "string" && value.trim().length > 0) {
    return value.trim();
  }
  if (value && typeof value === "object") {
    const candidate = value.pubkey ?? value.node_pubkey ?? value.nodePubkey;
    if (typeof candidate === "string" && candidate.trim().length > 0) {
      return candidate.trim();
    }
  }
  throw new Error("failed to resolve node pubkey from nodePubkeyValue");
}

function resolveNodePubkey(nodeHandle) {
  if (typeof nodeHandle.nodePubkeyJson === "function") {
    const raw = nodeHandle.nodePubkeyJson();
    if (typeof raw === "string" && raw.trim().length > 0) {
      try {
        return normalizeNodePubkey(JSON.parse(raw));
      } catch (_err) {
        return normalizeNodePubkey(raw);
      }
    }
  }
  return normalizeNodePubkey(nodeHandle.nodePubkeyValue());
}

function assertChannelPeer(channel, expectedPeerPubkey) {
  assertCondition(channel.peer_pubkey === expectedPeerPubkey, "channel peer pubkey mismatch");
}

async function waitForChannelUsable(nodeHandle, peerPubkey, timeoutMs) {
  const deadline = Date.now() + timeoutMs;
  let lastSnapshot = [];
  while (Date.now() < deadline) {
    if (typeof nodeHandle.processNativeRuntimeQueueValue === "function") {
      nodeHandle.processNativeRuntimeQueueValue();
    }
    const channels = nodeHandle.listChannelsValue();
    lastSnapshot = channels.map((c) => ({
      channel_id: c.channel_id,
      peer_pubkey: c.peer_pubkey,
      status: c.status,
      is_usable: c.is_usable,
      virtual_open_mode: c.virtual_open_mode ?? null,
    }));
    const found = channels.find((c) => c.peer_pubkey === peerPubkey);
    if (found && found.is_usable) {
      return found;
    }
    await sleep(200);
  }
  throw new Error(
    `channel did not become usable in time, last=${JSON.stringify(lastSnapshot)}`
  );
}

async function waitForPaymentStatus(nodeHandle, paymentHash, expectedStatus, timeoutMs) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const payment = nodeHandle.getPaymentValue(paymentHash);
    if (payment.status === expectedStatus) {
      return payment;
    }
    await sleep(200);
  }
  throw new Error(
    `payment ${paymentHash} did not reach status '${expectedStatus}' in time`
  );
}

function pickInvoiceString(invoiceResponse) {
  if (typeof invoiceResponse === "string") return invoiceResponse;
  if (invoiceResponse && typeof invoiceResponse.invoice === "string") {
    return invoiceResponse.invoice;
  }
  if (invoiceResponse && typeof invoiceResponse.invoice_string === "string") {
    return invoiceResponse.invoice_string;
  }
  throw new Error("createLnInvoiceValue response does not contain invoice string");
}

function paymentHashFromResult(sendResult) {
  const hash = sendResult?.payment_hash ?? sendResult?.paymentHash;
  if (!hash || typeof hash !== "string") {
    throw new Error("payment response missing payment_hash");
  }
  return hash;
}

function transferMatchesPayment(transfer, paymentHash, assetId, assetAmount) {
  const transferHash = transfer?.payment_hash ?? transfer?.paymentHash;
  const transferAssetId = transfer?.asset_id ?? transfer?.assetId;
  const transferAssetAmount = Number(transfer?.asset_amount ?? transfer?.assetAmount ?? 0);
  return (
    transferHash === paymentHash &&
    transferAssetId === assetId &&
    transferAssetAmount === Number(assetAmount)
  );
}

async function run() {
  const out = document.getElementById("out");
  if (out) out.innerHTML = "";

  await init();
  log("WASM init", { ok: true });

  const indexerUrl = readText("indexerUrl") || DEFAULT_INDEXER_URL;
  const nodeProxyUrl = readText("nodeProxyUrl") || DEFAULT_NODE_PROXY_URL;
  const transportEndpoint = readText("transportEndpoint") || DEFAULT_TRANSPORT_ENDPOINT;
  const transportEndpointRgb = toRgbTransportEndpoint(transportEndpoint);
  const senderPeerAddr = readText("senderPeerAddr") || DEFAULT_NODE_A_PEER_ADDR;
  const receiverPeerAddr = readText("receiverPeerAddr") || DEFAULT_NODE_B_PEER_ADDR;
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
    "wasm-rgb-ln-sender",
    FIXED_SENDER_MNEMONIC,
    nodeProxyUrl,
    transportEndpoint,
    lifecycle
  );
  const receiverRln = await createRlnInstance(
    "receiver",
    "wasm-rgb-ln-receiver",
    FIXED_RECEIVER_MNEMONIC,
    nodeProxyUrl,
    transportEndpoint,
    lifecycle
  );
  const sender = senderRln.wallet;
  const receiver = receiverRln.wallet;
  const senderNode = senderRln.node;
  const receiverNode = receiverRln.node;
  const senderPubkey = resolveNodePubkey(senderNode);
  const receiverPubkey = resolveNodePubkey(receiverNode);
  const senderAddress = sender.getAddress();
  const receiverAddress = receiver.getAddress();

  log("Two RLN instances initialized and unlocked", { ok: true });
  log("Using fixed endpoints", {
    indexerUrl,
    nodeProxyUrl,
    transportEndpoint,
    transportEndpointRgb,
    senderPeerAddr,
    receiverPeerAddr,
    senderPubkey,
    receiverPubkey,
  });
  log("Wallet addresses", {
    sender_address: senderAddress,
    receiver_address: receiverAddress,
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
  log("BTC balances before flow", {
    sender: sender.getBtcBalanceValue(),
    receiver: receiver.getBtcBalanceValue(),
  });

  await ensureWalletVanillaBudget(
    "sender",
    sender,
    senderOnline,
    nodeProxyUrl,
    senderAddress
  );
  await ensureWalletVanillaBudget(
    "receiver",
    receiver,
    receiverOnline,
    nodeProxyUrl,
    receiverAddress
  );
  await ensureRgbAllocations(sender, senderOnline);
  await ensureRgbAllocations(receiver, receiverOnline);

  await senderNode.connectPeer(receiverPeerAddr, receiverPubkey);
  await receiverNode.connectPeer(senderPeerAddr, senderPubkey);
  log("Peers connected", {
    senderPeerAddr,
    receiverPeerAddr,
  });

  const opened = senderNode.openChannelValue(
    receiverPubkey,
    OPEN_CHANNEL_CAPACITY_SAT,
    false,
    undefined,
    undefined
  );
  assertChannelPeer(opened, receiverPubkey);
  log("Channel open requested", opened);

  const channel = await waitForChannelUsable(senderNode, receiverPubkey, CHANNEL_READY_TIMEOUT_MS);
  assertChannelPeer(channel, receiverPubkey);
  log("Channel is usable", channel);

  const issueReq = {
    ticker: "TST",
    name: "WASM RGB over LN Demo",
    precision: 0,
    amounts: [issueAmount],
  };
  const issued = senderNode.issueAssetNiaValue(issueReq);
  const assetId = issued.asset_id;
  log("Asset issued", issued);
  const lnInvoiceDoc = receiverNode.createLnInvoiceValue(
    LN_RGB_PAYMENT_MSAT,
    INVOICE_EXPIRY_SEC,
    assetId,
    BigInt(sendAmount)
  );
  const lnInvoice = pickInvoiceString(lnInvoiceDoc);
  log("Receiver RGB-LN invoice created", {
    invoice_preview: lnInvoice.slice(0, 24),
    invoice_length: lnInvoice.length,
    amt_msat: LN_RGB_PAYMENT_MSAT,
    asset_id: assetId,
    asset_amount: sendAmount,
  });

  const sendResult = senderNode.sendPaymentValue(
    lnInvoice,
    LN_RGB_PAYMENT_MSAT,
    assetId,
    BigInt(sendAmount)
  );
  log("Sender RGB-LN payment sent", sendResult);
  const paymentHash = paymentHashFromResult(sendResult);

  const senderPayment = await waitForPaymentStatus(
    senderNode,
    paymentHash,
    "succeeded",
    PAYMENT_READY_TIMEOUT_MS
  );
  log("Sender payment finalized", senderPayment);

  const senderTransfers = senderNode.listRgbLnTransfersValue();
  const receiverTransfers = receiverNode.listRgbLnTransfersValue();
  const senderTransfer = senderTransfers.find((t) =>
    transferMatchesPayment(t, paymentHash, assetId, sendAmount)
  );
  const receiverTransfer = receiverTransfers.find((t) =>
    transferMatchesPayment(t, paymentHash, assetId, sendAmount)
  );
  assertCondition(Boolean(senderTransfer), "sender RGB-LN transfer record not found");
  assertCondition(Boolean(receiverTransfer), "receiver RGB-LN transfer record not found");

  log("Sender RGB-LN transfers", senderTransfers);
  log("Receiver RGB-LN transfers", receiverTransfers);
  log("RGB over Lightning flow completed", {
    ok: true,
    asset_id: assetId,
    sent_amount: sendAmount,
    payment_hash: paymentHash,
    channel_id: channel.channel_id,
  });
}

const runBtn = document.getElementById("run");
if (runBtn) {
  runBtn.addEventListener("click", () => {
    run().catch((err) => {
      log("RGB over Lightning flow failed", String(err));
    });
  });
}
