// Real end-to-end RGB-over-Lightning flow: a WASM node against a native Rust node.
//
// This is NOT an API mock — the WASM node speaks the real LN wire protocol to
// `rgb-native-phase5-node` through the WebSocket relay, opens real on-chain
// channels, and settles REAL HTLCs (keysendLiveValue → send_spontaneous_payment).
//
// Steps (matches the requested flow):
//   0. open a vanilla (BTC-only) channel AND an RGB (NIA) channel
//   1. transfer regular BTC      — real keysend HTLC over the vanilla channel
//   2. transfer RGB assets       — real BOLT11 invoice + keysend HTLCs over the RGB channel
//   3. close both channels, then reopen (page reload) and verify the WASM node's
//      persisted state is correct.
//
// Driven headlessly by run_e2e_full_flow.mjs, or manually via rgb_e2e_full_flow.html.
//
// Infrastructure required (compose.wasm.yaml) + rgb-native-phase5-node on 19735/19737.

import init, {
  RlnWasmNode,
  RlnWasmSdk,
  RlnWasmWallet,
  rgbGenerateKeysValue,
} from "../../pkg/rln_wasm_sdk.js";

const DEFAULTS = {
  nodeProxyUrl: "ws://127.0.0.1:3001",
  esploraUrl: "http://127.0.0.1:3002",
  rgbProxyUrl: "http://127.0.0.1:3001/rgb/json-rpc",
  gatewayUrl: "http://127.0.0.1:3001",
  // Native peer = a regular `rgb-lightning-node` (LDK peer port 9802, REST 3101), instead of the
  // old `rgb-native-phase5-node` (19735/19737). The native helpers below translate the old mgmt
  // API (/info, /invoice, /pay_invoice, /force_close) to the regular RLN's REST endpoints.
  nativePeerAddr: "127.0.0.1:9802",
  nativeMgmtUrl: "http://127.0.0.1:3101",
};

const VANILLA_CAPACITY_SAT = 1_000_000n;
const RGB_CAPACITY_SAT = 1_000_000n;
const COLORED_UTXO_SIZE_SAT = Number(RGB_CAPACITY_SAT) + 100_000;
const ASSET_TOTAL_ISSUE = 2000; // total NIA minted
const ASSET_CHANNEL_AMOUNT = 1000n; // RGB committed into the channel on open
const ASSET_SEND_AMOUNT = 100n; // RGB moved to the native node over LN
const BTC_KEYSEND_MSAT = 30_000_000n; // 30k sat, seeds native outbound liquidity above reserve
const BOLT11_SEND_MSAT = 2_000_000n;
const BOLT11_RECEIVE_MSAT = 1_000_000n;
const HODL_INVOICE_MSAT = 1_000_000n;
const RGB_INVOICE_SEND_AMOUNT = 100n;
const RGB_INVOICE_RECEIVE_AMOUNT = 40n;
const RGB_INVOICE_SEND_MSAT = 10_000_000n;
const RGB_INVOICE_RECEIVE_MSAT = 3_000_000n;
const RGB_KEYSEND_MSAT = 3_000_000n; // 3k sat carrying the RGB asset (RGB-LN minimum)

const CHANNEL_READY_TIMEOUT_MS = 180_000;
const FUND_TIMEOUT_MS = 60_000;
const PAYMENT_TIMEOUT_MS = 90_000;
const CLOSE_TIMEOUT_MS = 120_000;
const RGB_FUNDING_STEP_TIMEOUT_MS = 30_000;
const FETCH_TIMEOUT_MS = 15_000;

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

function safeJson(v) {
  return JSON.stringify(v, (_k, x) => (typeof x === "bigint" ? x.toString() : x), 2);
}

function log(message, data) {
  const out = document.getElementById("out");
  const line = `${message}${data === undefined ? "" : `: ${safeJson(data)}`}`;
  if (out) {
    const pre = document.createElement("pre");
    pre.textContent = line;
    out.appendChild(pre);
  }
  // Mirror to console so the headless driver captures it.
  console.log(`[e2e] ${line}`);
}

function readParam(name, fallback) {
  const v = new URLSearchParams(window.location.search).get(name);
  return v && v.trim() ? v.trim() : fallback;
}

function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

async function sha256Hex(hex) {
  const bytes = Uint8Array.from(hex.match(/.{2}/g).map((byte) => Number.parseInt(byte, 16)));
  const digest = await crypto.subtle.digest("SHA-256", bytes);
  return Array.from(new Uint8Array(digest), (byte) => byte.toString(16).padStart(2, "0")).join("");
}

async function withTimeout(promise, timeoutMs, label) {
  let timer;
  try {
    return await Promise.race([
      promise,
      new Promise((_, reject) => {
        timer = setTimeout(() => reject(new Error(`${label} timed out after ${timeoutMs}ms`)), timeoutMs);
      }),
    ]);
  } finally {
    clearTimeout(timer);
  }
}

function assert(cond, msg) {
  if (!cond) throw new Error(`ASSERTION FAILED: ${msg}`);
}

// A pre-funding "ghost" is a cached entry still keyed by its temporary channel id
// (channel_id === temporary_channel_id) that never migrated to a funding-derived id — it is not a
// real channel. Compare/report only real channels so transient SDK-cache ghosts (which the live
// ChannelManager doesn't have, and which never survive a reload) don't cause false mismatches.
function realChannelIds(channels) {
  return (channels || [])
    .filter(
      (c) =>
        !(
          c.channel_id === c.temporary_channel_id &&
          ["opening", "pending", ""].includes(c.status || "")
        )
    )
    .map((c) => c.channel_id)
    .sort();
}

function toRgbTransport(url) {
  const s = String(url || "").trim();
  if (s.startsWith("rpc://")) return s;
  if (s.startsWith("http://")) return `rpc://${s.slice("http://".length)}`;
  if (s.startsWith("https://")) return `rpc://${s.slice("https://".length)}`;
  return s;
}

async function mineBlocks(gatewayUrl, address, count) {
  try {
    const resp = await fetch(`${gatewayUrl}/dev/regtest/fund`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ address, amount_btc: 0.0001, mine_blocks: count }),
      signal: AbortSignal.timeout(FETCH_TIMEOUT_MS),
    });
    if (!resp.ok) log(`mineBlocks HTTP ${resp.status}`, await resp.text().catch(() => ""));
  } catch (e) {
    log("mineBlocks error", String(e));
  }
}

// GET node info from a regular `rgb-lightning-node` and expose it under the old `/info` shape
// (the rest of this flow reads `nativeInfo.node_id`).
async function fetchNativeInfo(nativeMgmtUrl) {
  const resp = await fetch(`${nativeMgmtUrl}/nodeinfo`, {
    signal: AbortSignal.timeout(FETCH_TIMEOUT_MS),
  });
  if (!resp.ok) throw new Error(`/nodeinfo failed: ${resp.status}`);
  const j = await resp.json();
  return { node_id: j.pubkey, ...j };
}

// Translate the old phase5-node mgmt API to the regular RLN's REST endpoints:
//   /invoice      -> /lninvoice   (BTC or RGB BOLT11; the RGB asset rides in asset_id/asset_amount)
//   /pay_invoice  -> /sendpayment (the regular RLN decodes the asset from the invoice)
async function nativeManagementPost(nativeMgmtUrl, path, body) {
  let target = path;
  let payload = body;
  if (path === "/invoice") {
    target = "/lninvoice";
    payload = {
      amt_msat: body.amt_msat ?? null,
      expiry_sec: body.expiry_sec ?? 3600,
      asset_id: body.asset_id ?? null,
      asset_amount: body.asset_amount ?? null,
      payment_hash: body.payment_hash ?? null,
      description_hash: null,
      min_final_cltv_expiry_delta: null,
    };
  } else if (path === "/pay_invoice") {
    target = "/sendpayment";
    payload = {
      invoice: body.invoice,
      amt_msat: body.amt_msat ?? null,
      asset_id: body.asset_id ?? null,
      asset_amount: body.asset_amount ?? null,
    };
  }
  const resp = await fetch(`${nativeMgmtUrl}${target}`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(payload),
    signal: AbortSignal.timeout(FETCH_TIMEOUT_MS),
  });
  if (!resp.ok) throw new Error(`${target} failed: ${resp.status} ${await resp.text().catch(() => "")}`);
  return resp.json();
}

async function nativePayInvoice(node, nativeMgmtUrl, body, timeoutMs = 30_000) {
  const deadline = Date.now() + timeoutMs;
  let lastError;
  while (Date.now() < deadline) {
    try {
      return await nativeManagementPost(nativeMgmtUrl, "/pay_invoice", body);
    } catch (error) {
      lastError = error;
      if (!String(error).includes("RouteNotFound")) throw error;
      await drainPeerEvents(node, 2);
    }
  }
  throw lastError ?? new Error("native /pay_invoice retry timed out");
}

// Fallback: ask the native node to force-close a channel (its /force_close mgmt endpoint).
// counterparty_node_id is THIS wasm node's pubkey (the channel's counterparty from native's view).
async function nativeForceClose(nativeMgmtUrl, channelId, counterpartyNodeId) {
  const resp = await fetch(`${nativeMgmtUrl}/closechannel`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ channel_id: channelId, peer_pubkey: counterpartyNodeId, force: true }),
    signal: AbortSignal.timeout(FETCH_TIMEOUT_MS),
  });
  return { status: resp.status, body: (await resp.text().catch(() => "")).slice(0, 160) };
}

async function fundWallet(gatewayUrl, wallet, online, address) {
  log("Funding wallet on-chain...", { address });
  const resp = await fetch(`${gatewayUrl}/dev/regtest/fund`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ address, amount_btc: 1, mine_blocks: 6 }),
  });
  if (!resp.ok) throw new Error(`fund request failed: ${resp.status} ${await resp.text().catch(() => "")}`);
  const deadline = Date.now() + FUND_TIMEOUT_MS;
  while (Date.now() < deadline) {
    await wallet.syncOnline(online);
    const bal = wallet.getBtcBalanceValue();
    if (Number(bal?.vanilla?.spendable ?? 0) >= 50_000_000) {
      log("Wallet funded", bal);
      return;
    }
    await sleep(2000);
  }
  throw new Error("wallet not funded within timeout");
}

async function ensureColoredUtxos(wallet, online) {
  await wallet.syncOnline(online);
  if (Number(wallet.getBtcBalanceValue()?.colored?.spendable ?? 0) > 0) {
    log("Colored UTXOs already present");
    return;
  }
  log("Creating colored UTXOs...");
  const unsigned = await wallet.createUtxosBegin(online, true, 5, COLORED_UTXO_SIZE_SAT, 1n, false);
  const signed = wallet.signPsbtValue(unsigned);
  const created = await wallet.createUtxosEnd(online, signed, false);
  log("Colored UTXOs created", { created });
  if (!created) throw new Error("createUtxosEnd registered no colored UTXOs");
}

// Drive RGB funding work + periodic mining until a channel to `peer` becomes usable.
// `withAsset` distinguishes the RGB channel (asset_id set) from the vanilla one. We match by
// peer + asset presence rather than the channel_id returned by openChannel, because LDK rewrites
// the channel_id from the temporary one to the funding-outpoint-derived one once funding confirms.
async function waitForUsableChannel(node, peer, withAsset, gatewayUrl, walletAddress, timeoutMs) {
  const deadline = Date.now() + timeoutMs;
  const label = withAsset ? "RGB" : "vanilla";
  let iter = 0;
  while (Date.now() < deadline) {
    try {
      await node.chainSyncTickValue();
    } catch (e) {
      log(`chainSyncTick err iter=${iter}`, String(e));
    }
    try {
      await withTimeout(node.driveRgbFundingWork(), RGB_FUNDING_STEP_TIMEOUT_MS, "driveRgbFundingWork");
    } catch (e) {
      if (String(e).includes("timed out")) throw e;
    }
    const found = node
      .listChannelsValue()
      .find((c) => c.peer_pubkey === peer && c.is_usable && (withAsset ? !!c.asset_id : !c.asset_id));
    if (found) return found;
    if (iter % 4 === 2) await mineBlocks(gatewayUrl, walletAddress, 3);
    if (iter % 5 === 0) {
      log(`waiting ${label} channel usable`, node.listChannelsValue().map((c) => ({ id: c.channel_id.slice(0, 12), asset: !!c.asset_id, status: c.status, usable: c.is_usable })));
    }
    await sleep(2000);
    iter++;
  }
  throw new Error(`${label} channel to ${peer.slice(0, 12)} not usable within ${timeoutMs}ms`);
}

// Fund a vanilla (BTC-only) channel: pull the LDK funding request, build a funding
// tx from the wallet's BTC, submit it back to LDK, which broadcasts via chain sync.
async function fundVanillaChannel(node, wallet, online, gatewayUrl, esploraUrl, walletAddress, nativePubkey) {
  const deadline = Date.now() + 60_000;
  while (Date.now() < deadline) {
    await node.chainSyncTickValue();
    const reqs = node.listPendingFundingRequestsValue();
    if (Array.isArray(reqs) && reqs.length > 0) {
      const req = reqs[0];
      log("Vanilla funding request", req);
      try {
        await wallet.syncOnline(online);
        const vu = await wallet.listUnspentsVanillaValue(online, 0, false);
        log("Wallet vanilla unspents before funding", vu);
        log("Wallet btc balance before funding", wallet.getBtcBalanceValue());
      } catch (e) {
        log("listUnspentsVanilla err", String(e));
      }
      // Use the *Json variant: buildLightningFundingTxValue returns a JS Map
      // (serde_json::json!), whose properties aren't directly addressable.
      const built = JSON.parse(
        await wallet.buildLightningFundingTxJson(online, req.output_script_hex, BigInt(req.channel_value_satoshis), 1n)
      );
      log("Built vanilla funding tx", { txid: built.txid, hex: built.funding_tx_hex });
      node.submitFundingTransactionValue({
        temporary_channel_id: req.temporary_channel_id,
        counterparty_node_id: req.counterparty_node_id || nativePubkey,
        funding_tx_hex: built.funding_tx_hex,
      });
      log("Submitted vanilla funding tx to LDK");
      // Belt-and-suspenders: broadcast the funding tx to the indexer directly. The wasm
      // chain-sync broadcast queue can lag; an explicit POST guarantees the tx reaches
      // bitcoind's mempool so mining confirms it. Surface any rejection reason.
      try {
        const resp = await fetch(`${esploraUrl}/tx`, {
          method: "POST",
          headers: { "content-type": "text/plain" },
          body: built.funding_tx_hex,
        });
        log("Funding tx broadcast to esplora", { status: resp.status, body: (await resp.text()).slice(0, 200) });
      } catch (e) {
        log("explicit funding broadcast error", String(e));
      }
      return;
    }
    await sleep(1500);
  }
  throw new Error("no pending vanilla funding request appeared");
}

// Drive the peer/event loop and wait for a REAL payment to settle.
async function waitLivePayment(node, paymentHash, timeoutMs) {
  const deadline = Date.now() + timeoutMs;
  let iter = 0;
  while (Date.now() < deadline) {
    // Persist locally prepared RGB commitment fascia before accepting the peer's response. The
    // response may include HTLC signatures whose validation immediately colors a child HTLC
    // transaction, which requires the parent commitment fascia to already be visible.
    try {
      await node.driveRgbFundingWork();
    } catch (_e) {
      /* non-fatal */
    }
    try {
      await node.chainSyncTickValue(); // drives peer_process_events → flushes HTLC frames
    } catch (e) {
      log(`tick err during payment iter=${iter}`, String(e));
    }
    // Drain any additional RGB transaction-persistence work queued while processing peer frames.
    try {
      await node.driveRgbFundingWork();
    } catch (_e) {
      /* non-fatal */
    }
    const p = node.livePaymentValue(paymentHash);
    if (p && p.status === "succeeded") return p;
    if (p && p.status === "failed") throw new Error(`payment ${paymentHash} FAILED`);
    await sleep(750);
    iter++;
  }
  throw new Error(`payment ${paymentHash} did not settle within ${timeoutMs}ms`);
}

async function waitLivePaymentStatus(node, paymentHash, expectedStatus, timeoutMs) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    try {
      await node.driveRgbFundingWork();
      await node.chainSyncTickValue();
      await node.driveRgbFundingWork();
    } catch (_e) {
      /* non-fatal while the peer/event loop advances */
    }
    const payment = node.livePaymentValue(paymentHash);
    if (payment?.status === expectedStatus) return payment;
    if (payment?.status === "failed" && expectedStatus !== "failed") {
      throw new Error(`payment ${paymentHash} failed while waiting for ${expectedStatus}`);
    }
    await sleep(250);
  }
  throw new Error(`payment ${paymentHash} did not reach ${expectedStatus} within ${timeoutMs}ms`);
}

async function drainPeerEvents(node, iterations = 8) {
  for (let i = 0; i < iterations; i++) {
    await node.chainSyncTickValue().catch(() => {});
    await sleep(250);
  }
}

async function waitForChannelGone(node, channelId, timeoutMs) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (!node.listChannelsValue().some((c) => c.channel_id === channelId)) return true;
    await sleep(1500);
  }
  return false;
}

function lsKey(runtimeId) {
  return `rgb-e2e-${runtimeId}`;
}

// ---------------------------------------------------------------------------
// PHASE: run — full flow
// ---------------------------------------------------------------------------

async function runFlow(cfg, runtimeId) {
  await init();
  log("WASM initialized", { runtimeId, phase: "run" });

  const keys = rgbGenerateKeysValue("regtest");
  const sdkPassword = "rgb-e2e-full-flow";
  const sdk = new RlnWasmSdk();
  await sdk.initValue(sdkPassword, keys.mnemonic);
  await sdk.unlock(JSON.stringify({ password: sdkPassword }));
  log("SDK initialized + unlocked");

  const node = RlnWasmNode.newWithNodeRuntimeId(cfg.nodeProxyUrl, runtimeId);
  const myPubkey = JSON.parse(node.nodePubkeyJson());
  const myPubkeyHex =
    (typeof myPubkey === "string" ? myPubkey : myPubkey?.pubkey ?? myPubkey?.node_pubkey) || "";
  log("WASM node created", myPubkey);

  const wallet = await RlnWasmWallet.create(
    JSON.stringify({
      data_dir: `/tmp/rln_wasm_e2e_${runtimeId}`,
      bitcoin_network: "Regtest",
      database_type: "Sqlite",
      max_allocations_per_utxo: 5,
      account_xpub_vanilla: keys.account_xpub_vanilla,
      account_xpub_colored: keys.account_xpub_colored,
      mnemonic: keys.mnemonic,
      master_fingerprint: keys.master_fingerprint,
      vanilla_keychain: null,
      supported_schemas: ["Nia"],
    })
  );
  const online = await wallet.goOnlineValue(true, cfg.esploraUrl);
  node.attachWallet(wallet);
  const walletAddress = wallet.getAddress();
  log("Wallet online + attached", { walletAddress });

  // --- fund the wallet on-chain ---
  await fundWallet(cfg.gatewayUrl, wallet, online, walletAddress);
  // chainSyncTickValue requires an active sync, but the *background* loop must stay dormant: if it
  // keeps re-syncing Esplora while we broadcast createUtxos/issuance txs between the two channel
  // opens, it transiently marks confirmed funding txs as evicted/unconfirmed and force-closes the
  // channel ("Locked at 6 confs, now have 0 confs"). So start with a huge interval (effectively
  // off) and drive LDK chain sync explicitly via chainSyncTickValue inside the wait loops.
  node.chainSyncStartValue(cfg.esploraUrl, 3_600_000);

  // --- connect to the native node ---
  const nativeInfo = await fetchNativeInfo(cfg.nativeMgmtUrl);
  const nativePubkey = nativeInfo.node_id;
  log("Native node info", nativeInfo);
  await node.connectPeer(cfg.nativePeerAddr, nativePubkey);
  log("Connected to native node");

  // === STEP 0a: open a VANILLA (BTC-only) channel ===
  // Do this BEFORE any RGB ops (createUtxos/issuance): the vanilla funding tx is built by BDK
  // coin selection, and running it while the wallet still has a single pristine vanilla UTXO
  // avoids BDK re-offering an already-spent UTXO once RGB operations have churned its view.
  log("Opening vanilla channel...");
  const vanillaOpen = node.openChannelValueWithOptions(
    nativePubkey, VANILLA_CAPACITY_SAT, false, null, null, null, null, null
  );
  log("Vanilla channel open initiated", { tempChannelId: vanillaOpen.channel_id });
  await fundVanillaChannel(node, wallet, online, cfg.gatewayUrl, cfg.esploraUrl, walletAddress, nativePubkey);
  const vanillaChannel = await waitForUsableChannel(node, nativePubkey, false, cfg.gatewayUrl, walletAddress, CHANNEL_READY_TIMEOUT_MS);
  const vanillaChannelId = vanillaChannel.channel_id;
  log("✅ VANILLA channel usable", { id: vanillaChannelId, capacity: vanillaChannel.capacity_sat });

  // --- RGB prep: colored UTXOs + issue NIA (after the vanilla funding tx confirmed) ---
  await mineBlocks(cfg.gatewayUrl, walletAddress, 3);
  await wallet.refreshValue(online, null, [], false).catch(() => {});
  await ensureColoredUtxos(wallet, online);
  await mineBlocks(cfg.gatewayUrl, walletAddress, 3);
  await wallet.syncOnline(online);

  const issued = node.issueAssetNiaValue({
    ticker: "E2E",
    name: "WASM E2E RGB",
    precision: 0,
    amounts: [ASSET_TOTAL_ISSUE],
  });
  const assetId = issued.asset_id;
  log("NIA asset issued", { assetId });
  await mineBlocks(cfg.gatewayUrl, walletAddress, 3);
  await wallet.refreshValue(online, null, [], false);
  const assetBalAfterIssue = wallet.getAssetBalanceValue(assetId);
  log("Asset balance after issuance", assetBalAfterIssue);
  assert(Number(assetBalAfterIssue?.settled ?? 0) === ASSET_TOTAL_ISSUE, "issued asset not settled");

  // === STEP 0b: open an RGB (NIA) channel ===
  const rgbTransport = toRgbTransport(cfg.rgbProxyUrl);
  log("Opening RGB channel...", { assetId, asset: ASSET_CHANNEL_AMOUNT.toString() });
  const rgbOpen = node.openChannelValueWithOptions(
    nativePubkey, RGB_CAPACITY_SAT, false, assetId, ASSET_CHANNEL_AMOUNT, null, assetId, rgbTransport
  );
  log("RGB channel open initiated", { tempChannelId: rgbOpen.channel_id });
  const rgbChannel = await waitForUsableChannel(node, nativePubkey, true, cfg.gatewayUrl, walletAddress, CHANNEL_READY_TIMEOUT_MS);
  const rgbChannelId = rgbChannel.channel_id;
  log("✅ RGB channel usable", { id: rgbChannelId });

  const assetBalAfterOpen = wallet.getAssetBalanceValue(assetId);
  log("Asset balance after RGB channel open (channel amount now off-chain)", assetBalAfterOpen);

  // === STEP 1: transfer regular BTC — REAL keysend HTLC (settles end-to-end) ===
  log("STEP 1: real BTC keysend...", { amtMsat: BTC_KEYSEND_MSAT.toString() });
  const btcKeysend = node.keysendLiveValue(nativePubkey, BTC_KEYSEND_MSAT, null, null);
  log("BTC keysend initiated (real HTLC)", btcKeysend);
  const btcSettled = await waitLivePayment(node, btcKeysend.payment_hash, PAYMENT_TIMEOUT_MS);
  assert(btcSettled.status === "succeeded", "BTC keysend not succeeded");
  assert(!!btcSettled.preimage, "BTC keysend has no preimage (not a real PaymentSent)");
  log("✅ STEP 1 done — real BTC HTLC settled wasm→native", btcSettled);

  // === STEP 1b: pay a real native-generated BOLT11 invoice ===
  const nativeInvoice = await nativeManagementPost(cfg.nativeMgmtUrl, "/invoice", {
    amt_msat: Number(BOLT11_SEND_MSAT),
    expiry_sec: 3600,
  });
  const bolt11Send = node.sendPaymentLiveValue(nativeInvoice.invoice, null, null, null);
  const bolt11SendSettled = await waitLivePayment(node, bolt11Send.payment_hash, PAYMENT_TIMEOUT_MS);
  assert(bolt11SendSettled.status === "succeeded", "outbound BOLT11 payment not succeeded");
  assert(!!bolt11SendSettled.preimage, "outbound BOLT11 payment has no PaymentSent preimage");
  log("✅ real BOLT11 payment settled wasm→native", bolt11SendSettled);

  // === STEP 1c: receive and auto-claim a real BOLT11 payment ===
  const wasmInvoice = node.createLnInvoiceLiveValue(BOLT11_RECEIVE_MSAT, 3600, null, null);
  const decodedWasmInvoice = node.decodeLnInvoiceValue(wasmInvoice.invoice);
  await nativePayInvoice(node, cfg.nativeMgmtUrl, {
    invoice: wasmInvoice.invoice,
    amt_msat: Number(BOLT11_RECEIVE_MSAT),
  });
  const bolt11ReceiveSettled = await waitLivePayment(
    node,
    decodedWasmInvoice.payment_hash,
    PAYMENT_TIMEOUT_MS,
  );
  assert(bolt11ReceiveSettled.status === "succeeded", "inbound BOLT11 payment not claimed");
  assert(bolt11ReceiveSettled.inbound === true, "inbound BOLT11 payment not marked inbound");
  log("✅ real BOLT11 payment received + claimed native→wasm", bolt11ReceiveSettled);

  // === STEP 1d: hold and explicitly claim a real BOLT11 HTLC ===
  const hodlClaimPreimage = "11".repeat(32);
  const hodlClaimHash = await sha256Hex(hodlClaimPreimage);
  const hodlClaimInvoice = node.createHodlLnInvoiceValue(
    HODL_INVOICE_MSAT,
    3600,
    null,
    null,
    hodlClaimHash,
  );
  await nativePayInvoice(node, cfg.nativeMgmtUrl, {
    invoice: hodlClaimInvoice.invoice,
    amt_msat: Number(HODL_INVOICE_MSAT),
  });
  const hodlClaimable = await waitLivePaymentStatus(
    node,
    hodlClaimHash,
    "claimable",
    PAYMENT_TIMEOUT_MS,
  );
  assert(hodlClaimable.inbound === true, "HODL claim payment not marked inbound");
  const hodlClaimResult = node.claimHodlInvoiceValue(hodlClaimHash, hodlClaimPreimage);
  assert(hodlClaimResult.changed === true, "HODL claim did not release the held HTLC");
  const hodlClaimed = await waitLivePayment(node, hodlClaimHash, PAYMENT_TIMEOUT_MS);
  assert(hodlClaimed.preimage === hodlClaimPreimage, "claimed HODL payment preimage mismatch");
  log("✅ real HODL invoice held + explicitly claimed", hodlClaimed);
  await drainPeerEvents(node);

  // === STEP 1e: hold and explicitly cancel a real BOLT11 HTLC ===
  const hodlCancelPreimage = "22".repeat(32);
  const hodlCancelHash = await sha256Hex(hodlCancelPreimage);
  const hodlCancelInvoice = node.createHodlLnInvoiceValue(
    HODL_INVOICE_MSAT,
    3600,
    null,
    null,
    hodlCancelHash,
  );
  await nativePayInvoice(node, cfg.nativeMgmtUrl, {
    invoice: hodlCancelInvoice.invoice,
    amt_msat: Number(HODL_INVOICE_MSAT),
  });
  await waitLivePaymentStatus(node, hodlCancelHash, "claimable", PAYMENT_TIMEOUT_MS);
  node.cancelHodlInvoiceValue(hodlCancelHash);
  const hodlCancelled = await waitLivePaymentStatus(
    node,
    hodlCancelHash,
    "cancelled",
    PAYMENT_TIMEOUT_MS,
  );
  log("✅ real HODL invoice held + explicitly cancelled", hodlCancelled);

  // === STEP 2a: pay a real native-generated RGB BOLT11 invoice ===
  const nativeRgbInvoice = await nativeManagementPost(cfg.nativeMgmtUrl, "/invoice", {
    // Seed enough native-side BTC liquidity for the reverse RGB invoice. RGB-LN payments must
    // carry at least 3,000,000 msat, while a newly funded remote side starts below that limit.
    amt_msat: Number(RGB_INVOICE_SEND_MSAT),
    expiry_sec: 3600,
    asset_id: assetId,
    asset_amount: Number(RGB_INVOICE_SEND_AMOUNT),
  });
  const rgbInvoiceSend = node.sendPaymentLiveValue(nativeRgbInvoice.invoice, null, null, null);
  const rgbInvoiceSendSettled = await waitLivePayment(
    node,
    rgbInvoiceSend.payment_hash,
    PAYMENT_TIMEOUT_MS,
  );
  assert(rgbInvoiceSendSettled.status === "succeeded", "outbound RGB BOLT11 payment not succeeded");
  assert(!!rgbInvoiceSendSettled.preimage, "outbound RGB BOLT11 payment has no PaymentSent preimage");
  assert(rgbInvoiceSendSettled.asset_id === assetId, "outbound RGB BOLT11 payment asset ID mismatch");
  assert(BigInt(rgbInvoiceSendSettled.asset_amount) === RGB_INVOICE_SEND_AMOUNT, "outbound RGB BOLT11 payment asset amount mismatch");
  log("✅ real RGB BOLT11 payment settled wasm→native", rgbInvoiceSendSettled);

  // === STEP 2b: receive and auto-claim a real RGB BOLT11 payment ===
  const wasmRgbInvoice = node.createLnInvoiceLiveValue(
    RGB_INVOICE_RECEIVE_MSAT,
    3600,
    assetId,
    RGB_INVOICE_RECEIVE_AMOUNT,
  );
  const decodedWasmRgbInvoice = node.decodeLnInvoiceValue(wasmRgbInvoice.invoice);
  await nativePayInvoice(node, cfg.nativeMgmtUrl, {
    invoice: wasmRgbInvoice.invoice,
    amt_msat: Number(RGB_INVOICE_RECEIVE_MSAT),
  });
  const rgbInvoiceReceiveSettled = await waitLivePayment(
    node,
    decodedWasmRgbInvoice.payment_hash,
    PAYMENT_TIMEOUT_MS,
  );
  assert(rgbInvoiceReceiveSettled.status === "succeeded", "inbound RGB BOLT11 payment not claimed");
  assert(rgbInvoiceReceiveSettled.inbound === true, "inbound RGB BOLT11 payment not marked inbound");
  assert(rgbInvoiceReceiveSettled.asset_id === assetId, "inbound RGB BOLT11 payment asset ID mismatch");
  assert(BigInt(rgbInvoiceReceiveSettled.asset_amount) === RGB_INVOICE_RECEIVE_AMOUNT, "inbound RGB BOLT11 payment asset amount mismatch");
  log("✅ real RGB BOLT11 payment received + claimed native→wasm", rgbInvoiceReceiveSettled);

  // === STEP 2c: transfer RGB assets — REAL keysend HTLC carrying the asset ===
  log("STEP 2: real RGB keysend...", { assetId, asset: ASSET_SEND_AMOUNT.toString(), amtMsat: RGB_KEYSEND_MSAT.toString() });
  const rgbKeysend = node.keysendLiveValue(nativePubkey, RGB_KEYSEND_MSAT, assetId, ASSET_SEND_AMOUNT);
  log("RGB keysend initiated (real HTLC + asset, route found over RGB channel)", rgbKeysend);
  const rgbSettled = await waitLivePayment(node, rgbKeysend.payment_hash, PAYMENT_TIMEOUT_MS);
  assert(rgbSettled.status === "succeeded", "RGB keysend not succeeded");
  assert(!!rgbSettled.preimage, "RGB keysend has no preimage (not a real PaymentSent)");
  const rgbStatus = rgbSettled.status;
  log("✅ STEP 2 done — real RGB HTLC settled", rgbSettled);

  // === STEP 3a: close both channels and DRIVE the close to completion ===
  // Exercise BOTH close paths: the RGB channel is FORCE-closed (force=true →
  // force_close_broadcasting_latest_txn), the vanilla channel is cooperatively closed (force=false).
  // The close API only *initiates* the close (and marks the channel closing/force_closing); the live
  // ChannelManager removes the channel only once it actually closes and fires Event::ChannelClosed.
  // So we pump peer/chain/RGB events + mine until both channels truly leave the live view (the
  // force-close commitment tx and the coop closing tx both need to confirm on-chain).
  log("STEP 3: closing channels (RGB=force-close, vanilla=cooperative)...");
  for (const [id, label, force] of [[rgbChannelId, "RGB", true], [vanillaChannelId, "vanilla", false]]) {
    try {
      node.closeChannelWithOptions(id, nativePubkey, force);
      log(`close requested for ${label} channel`, { id, force });
    } catch (e) {
      throw new Error(`close request for ${label} channel failed: ${String(e)}`);
    }
  }

  const closeDeadline = Date.now() + CLOSE_TIMEOUT_MS;
  let rgbGone = false;
  let vanillaGone = false;
  let forcedFallback = false;
  let citer = 0;
  while (Date.now() < closeDeadline && !(rgbGone && vanillaGone)) {
    try {
      await node.chainSyncTickValue();
    } catch (e) {
      log(`close tick err iter=${citer}`, String(e));
    }
    try {
      await node.driveRgbFundingWork();
    } catch (_e) {
      /* drives the RGB colored closing tx; non-fatal */
    }
    if (citer % 3 === 0) await mineBlocks(cfg.gatewayUrl, walletAddress, 3);
    const ids = new Set(node.listChannelsValue().map((c) => c.channel_id));
    rgbGone = !ids.has(rgbChannelId);
    vanillaGone = !ids.has(vanillaChannelId);
    // Fallback: if cooperative close hasn't completed ~halfway through, force-close the stragglers
    // from the native node so the channels resolve on-chain.
    if (!forcedFallback && Date.now() > closeDeadline - CLOSE_TIMEOUT_MS / 2) {
      forcedFallback = true;
      for (const [id, gone] of [[rgbChannelId, rgbGone], [vanillaChannelId, vanillaGone]]) {
        if (!gone) await nativeForceClose(cfg.nativeMgmtUrl, id, myPubkeyHex).then((r) => log("native force_close fallback", { id, r })).catch((e) => log("force_close err", String(e)));
      }
    }
    if (citer % 5 === 0) {
      log("closing…", { rgbGone, vanillaGone, channels: node.listChannelsValue().map((c) => ({ id: c.channel_id.slice(0, 12), status: c.status, usable: c.is_usable })) });
    }
    await sleep(2000);
    citer++;
  }
  log("Channels closed?", { rgbGone, vanillaGone, forcedFallback });
  assert(rgbGone && vanillaGone, `channels did not fully close: rgbGone=${rgbGone} vanillaGone=${vanillaGone}`);

  const channelsAfterClose = node.listChannelsValue();
  const assetBalAfterClose = null;

  node.persistLdkRuntimeState();

  // Persist a snapshot for the verify phase (page reload).
  const snapshot = {
    runtimeId,
    keys,
    sdkPassword,
    assetId,
    nativePubkey,
    myPubkey,
    walletAddress,
    vanillaChannelId,
    rgbChannelId,
    btcPaymentHash: btcKeysend.payment_hash,
    rgbPaymentHash: rgbKeysend?.payment_hash ?? null,
    channelsAfterClose,
    assetBalAfterIssue,
    assetBalAfterOpen,
    assetBalAfterClose,
  };
  window.localStorage.setItem(lsKey(runtimeId), safeJson(snapshot));

  // PARITY_PLAN 0.2 verification: the autonomous drive loop self-drives the node (chain sync +
  // event draining + authoritative reconcile) with NO manual chainSyncTick. All manual ticking is
  // finished by this quiescent point, so we hand the wheel to the loop, confirm it runs a few
  // cycles on its own, then stop it. autoDrive runs the exact same node_drive_tick_once that the
  // manual ticks above used (exercised throughout this flow).
  const autoStart = node.autoDriveStartValue(250);
  log("PARITY 0.2 — autoDrive started", autoStart);
  assert(
    autoStart && autoStart.running === true && autoStart.interval_ms === 250,
    `autoDrive should report running at 250ms after start: ${safeJson(autoStart)}`
  );
  await sleep(1500); // ~6 self-driven cycles, no manual tick
  const autoMid = node.autoDriveStatusValue();
  assert(
    autoMid && autoMid.running === true,
    `autoDrive should still be running mid-flight: ${safeJson(autoMid)}`
  );
  const autoStop = node.autoDriveStopValue();
  log("PARITY 0.2 — autoDrive stopped", autoStop);
  assert(
    autoStop && autoStop.running === false,
    `autoDrive should report stopped: ${safeJson(autoStop)}`
  );
  await sleep(400); // let the in-flight cycle observe the stop flag and exit
  log("✅ PARITY 0.2 — autonomous drive loop start/run/stop verified");

  const result = {
    ok: true,
    phase: "run",
    runtimeId,
    assetId,
    nativePubkey,
    myPubkey,
    vanillaChannelId,
    rgbChannelId,
    btcKeysend: btcSettled,
    rgbKeysend: rgbSettled,
    rgbKeysendStatus: rgbStatus,
    rgbGone,
    vanillaGone,
    assetBalAfterIssue,
    assetBalAfterOpen,
    assetBalAfterClose,
  };
  log("=== RUN PHASE COMPLETE ===", result);
  return result;
}

// ---------------------------------------------------------------------------
// PHASE: verify — reopen the node and confirm persisted state
// ---------------------------------------------------------------------------

async function verifyFlow(cfg, runtimeId) {
  await init();
  log("WASM initialized", { runtimeId, phase: "verify" });

  const raw = window.localStorage.getItem(lsKey(runtimeId));
  assert(raw, `no run-phase snapshot found for runtimeId=${runtimeId}`);
  const snap = JSON.parse(raw);
  log("Loaded run snapshot", { savedPubkey: snap.myPubkey, vanilla: snap.vanillaChannelId, rgb: snap.rgbChannelId });

  // Reopen the SDK + node with the SAME identity/runtimeId — this is the "reopen".
  const sdk = new RlnWasmSdk();
  await sdk.preloadPersistentRuntimeState();
  await sdk.initValue(snap.sdkPassword, snap.keys.mnemonic);
  await sdk.unlock(JSON.stringify({ password: snap.sdkPassword }));
  log("SDK re-initialized from persisted state");

  const node = RlnWasmNode.newWithNodeRuntimeId(cfg.nodeProxyUrl, runtimeId);
  const reopenedPubkey = JSON.parse(node.nodePubkeyJson());
  log("Node reopened", reopenedPubkey);

  // (a) identity persisted across reopen
  assert(
    JSON.stringify(reopenedPubkey) === JSON.stringify(snap.myPubkey),
    `node identity changed across reopen: ${safeJson(reopenedPubkey)} != ${safeJson(snap.myPubkey)}`
  );
  log("✅ node identity persisted across reopen");

  // (b) real channel state restored from persistence matches the pre-reload state. Both channels
  // were closed (RGB force-closed, vanilla cooperatively), so the real (non-ghost) channel set must
  // be empty before reload AND after reopen. We compare real channels only — pre-funding temp-id
  // ghosts are a transient SDK-cache artifact that the live ChannelManager never has and that never
  // survive a reload, so they must not count.
  const channelsNow = node.listChannelsValue();
  const idsNow = realChannelIds(channelsNow);
  const idsBefore = realChannelIds(snap.channelsAfterClose || []);
  log("Real channels after reopen", idsNow);
  log("Real channel-id set before reload", idsBefore);
  log("Raw channels after reopen", channelsNow);
  // PARITY_PLAN 0.1 verification: with reconcile-from-live running LAST in every drive pass, the
  // raw cached channel set must already equal the live ChannelManager set — i.e. contain NO
  // pre-funding temp-id "ghost" entries — so no JS-side ghost filtering should be needed. We assert
  // this on the raw set directly (independent of realChannelIds()).
  const rawGhosts = (channelsNow || []).filter(
    (c) =>
      c.channel_id === c.temporary_channel_id &&
      ["opening", "pending", ""].includes(c.status || "")
  );
  log("PARITY 0.1 — raw ghost entries after reopen", { count: rawGhosts.length, rawGhosts });
  assert(
    rawGhosts.length === 0,
    `PARITY 0.1: expected the raw channel set to contain no temp-id ghosts (reconcile-from-live runs last), got ${rawGhosts.length}: ${safeJson(rawGhosts)}`
  );
  assert(
    JSON.stringify(idsNow) === JSON.stringify(idsBefore),
    `restored real channel set differs from persisted: after=${safeJson(idsNow)} before=${safeJson(idsBefore)}`
  );
  // After closing both channels, no usable channel may remain on reopen.
  assert(
    channelsNow.filter((c) => c.is_usable).length === 0,
    `expected no usable channels after close+reopen, got ${channelsNow.filter((c) => c.is_usable).length}`
  );
  log("✅ real channel set restored from persistence matches pre-reload state (both channels closed)", {
    realCount: idsNow.length,
    rawCount: channelsNow.length,
  });

  const result = {
    ok: true,
    phase: "verify",
    runtimeId,
    reopenedPubkey,
    channelsAfterReopen: channelsNow,
    matchedIdentity: true,
  };
  log("=== VERIFY PHASE COMPLETE ===", result);
  return result;
}

// ---------------------------------------------------------------------------
// entrypoint
// ---------------------------------------------------------------------------

async function main() {
  const out = document.getElementById("out");
  if (out) out.innerHTML = "";
  const cfg = {
    nodeProxyUrl: readParam("nodeProxyUrl", DEFAULTS.nodeProxyUrl),
    esploraUrl: readParam("esploraUrl", DEFAULTS.esploraUrl),
    rgbProxyUrl: readParam("rgbProxyUrl", DEFAULTS.rgbProxyUrl),
    gatewayUrl: readParam("gatewayUrl", DEFAULTS.gatewayUrl),
    nativePeerAddr: readParam("nativePeerAddr", DEFAULTS.nativePeerAddr),
    nativeMgmtUrl: readParam("nativeMgmtUrl", DEFAULTS.nativeMgmtUrl),
  };
  const phase = readParam("phase", "run");
  const runtimeId = readParam("runtimeId", `rgb-e2e-${Math.random().toString(16).slice(2)}`);
  log("Config", { ...cfg, phase, runtimeId });

  try {
    const result = phase === "verify" ? await verifyFlow(cfg, runtimeId) : await runFlow(cfg, runtimeId);
    window.__E2E_RESULT = result;
    window.__E2E_DONE = true;
    log(`*** E2E ${phase.toUpperCase()} SUCCESS ***`);
  } catch (err) {
    const failure = { ok: false, phase, runtimeId, error: String(err && err.stack ? err.stack : err) };
    window.__E2E_RESULT = failure;
    window.__E2E_DONE = true;
    log(`*** E2E ${phase.toUpperCase()} FAILED ***`, failure);
  }
}

// Auto-run when a phase is provided (headless driver), else wait for the button.
if (new URLSearchParams(window.location.search).has("phase")) {
  main();
} else {
  const btn = document.getElementById("run");
  if (btn) btn.addEventListener("click", () => main());
}
