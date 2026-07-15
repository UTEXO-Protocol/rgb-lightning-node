// Phase 4 — multi-hop routing: a WASM node as a forwarding intermediary.
//
// Topology:  native-A  --chan-->  WASM  --chan-->  native-B
//             (payer)          (forwarder)          (payee)
//
// This is NOT an API mock. The WASM node speaks the real LN wire protocol to BOTH native nodes
// through the WebSocket relay, opens two real on-chain (vanilla/BTC) channels, and then FORWARDS a
// real HTLC: native-A pays a BOLT11 invoice created by native-B, a node it has no direct channel to
// and only knows via the private-channel route hint (WASM->B) carried in the invoice. The HTLC is
// routed native-A -> WASM -> native-B and settles end-to-end, proving the WASM node routes/forwards
// as a multi-hop intermediary.
//
// Driven headlessly by run_multihop_flow.mjs (which boots native-A + native-B), or manually via
// rgb_multihop_flow.html. Infrastructure required: compose.wasm.yaml infra (esplora 3002, relay
// gateway 3001) + two rgb-native-phase5-node instances (A on 19735/19737, B on 19745/19747).

import init, {
  RlnWasmNode,
  RlnWasmSdk,
  RlnWasmWallet,
  rgbGenerateKeysValue,
} from "../../pkg/rln_wasm_sdk.js";

const DEFAULTS = {
  nodeProxyUrl: "ws://127.0.0.1:3001",
  esploraUrl: "http://127.0.0.1:3002",
  gatewayUrl: "http://127.0.0.1:3001",
  // native-A = payer, native-B = payee
  nativeAPeerAddr: "127.0.0.1:19735",
  nativeAMgmtUrl: "http://127.0.0.1:19737",
  nativeBPeerAddr: "127.0.0.1:19745",
  nativeBMgmtUrl: "http://127.0.0.1:19747",
};

const CHANNEL_CAPACITY_SAT = 1_000_000n;
// Seed native-A's outbound liquidity on the A<->WASM channel so it can originate the multi-hop
// payment. WASM funds both channels, so without this native-A would have 0 outbound toward WASM.
const SEED_KEYSEND_MSAT = 80_000_000n; // Below the channel's 100m-msat per-HTLC maximum.
const MULTIHOP_PAYMENT_MSAT = 20_000_000n; // 20k sat: native-A -> WASM -> native-B

const CHANNEL_READY_TIMEOUT_MS = 180_000;
const FUND_TIMEOUT_MS = 60_000;
const PAYMENT_TIMEOUT_MS = 120_000;
const FETCH_TIMEOUT_MS = 15_000;
const AUTO_DRIVE_INTERVAL_MS = 400;

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
  console.log(`[e2e] ${line}`);
}

function readParam(name, fallback) {
  const v = new URLSearchParams(window.location.search).get(name);
  return v && v.trim() ? v.trim() : fallback;
}

function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

function assert(cond, msg) {
  if (!cond) throw new Error(`assertion failed: ${msg}`);
}

async function withTimeout(promise, timeoutMs, label) {
  let timer;
  const timeout = new Promise((_resolve, reject) => {
    timer = setTimeout(() => reject(new Error(`${label} timed out after ${timeoutMs}ms`)), timeoutMs);
  });
  try {
    return await Promise.race([promise, timeout]);
  } finally {
    clearTimeout(timer);
  }
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

async function nativeGet(nativeMgmtUrl, path) {
  const resp = await fetch(`${nativeMgmtUrl}${path}`, {
    method: "GET",
    signal: AbortSignal.timeout(FETCH_TIMEOUT_MS),
  });
  if (!resp.ok) throw new Error(`GET ${path} failed: ${resp.status} ${await resp.text().catch(() => "")}`);
  return resp.json();
}

async function nativePost(nativeMgmtUrl, path, body) {
  const resp = await fetch(`${nativeMgmtUrl}${path}`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(body),
    signal: AbortSignal.timeout(FETCH_TIMEOUT_MS),
  });
  if (!resp.ok) throw new Error(`POST ${path} failed: ${resp.status} ${await resp.text().catch(() => "")}`);
  return resp.json();
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

// Pull the LDK funding request for a freshly-initiated vanilla channel, build the funding tx from
// the wallet's BTC, submit it back to LDK, and broadcast it so mining confirms the channel.
async function fundVanillaChannel(node, wallet, online, gatewayUrl, esploraUrl, walletAddress, peerPubkey) {
  const deadline = Date.now() + 60_000;
  while (Date.now() < deadline) {
    await node.chainSyncTickValue();
    const reqs = node.listPendingFundingRequestsValue();
    const req = Array.isArray(reqs)
      ? reqs.find((r) => (r.counterparty_node_id || peerPubkey) === peerPubkey) || reqs[0]
      : undefined;
    if (req) {
      log("Vanilla funding request", req);
      await wallet.syncOnline(online);
      const built = JSON.parse(
        await wallet.buildLightningFundingTxJson(
          online,
          req.output_script_hex,
          BigInt(req.channel_value_satoshis),
          1n
        )
      );
      log("Built vanilla funding tx", { txid: built.txid });
      node.submitFundingTransactionValue({
        temporary_channel_id: req.temporary_channel_id,
        counterparty_node_id: req.counterparty_node_id || peerPubkey,
        funding_tx_hex: built.funding_tx_hex,
      });
      try {
        const resp = await fetch(`${esploraUrl}/tx`, {
          method: "POST",
          headers: { "content-type": "text/plain" },
          body: built.funding_tx_hex,
        });
        log("Funding tx broadcast to esplora", { status: resp.status, body: (await resp.text()).slice(0, 120) });
      } catch (e) {
        log("explicit funding broadcast error", String(e));
      }
      return;
    }
    await sleep(1500);
  }
  throw new Error(`no pending vanilla funding request appeared for ${peerPubkey.slice(0, 12)}`);
}

// Drive chain sync + periodic mining until a vanilla channel to `peer` becomes usable.
async function waitForUsableChannel(node, peer, gatewayUrl, walletAddress, timeoutMs) {
  const deadline = Date.now() + timeoutMs;
  let iter = 0;
  while (Date.now() < deadline) {
    try {
      await node.chainSyncTickValue();
    } catch (e) {
      log(`chainSyncTick err iter=${iter}`, String(e));
    }
    const found = node
      .listChannelsValue()
      .find((c) => c.peer_pubkey === peer && c.is_usable && !c.asset_id);
    if (found) return found;
    if (iter % 4 === 2) await mineBlocks(gatewayUrl, walletAddress, 3);
    if (iter % 5 === 0) {
      log(`waiting channel usable to ${peer.slice(0, 12)}`,
        node.listChannelsValue().map((c) => ({ id: c.channel_id.slice(0, 12), peer: c.peer_pubkey.slice(0, 12), status: c.status, usable: c.is_usable })));
    }
    await sleep(2000);
    iter++;
  }
  throw new Error(`channel to ${peer.slice(0, 12)} not usable within ${timeoutMs}ms`);
}

// Drive the WASM peer/event loop and wait for a WASM-originated payment to settle.
async function waitWasmPayment(node, paymentHash, timeoutMs) {
  const deadline = Date.now() + timeoutMs;
  let iter = 0;
  while (Date.now() < deadline) {
    try {
      await node.chainSyncTickValue();
    } catch (e) {
      log(`tick err iter=${iter}`, String(e));
    }
    const p = node.livePaymentValue(paymentHash);
    if (p && (p.status === "succeeded" || p.status === "failed")) return p;
    await sleep(1500);
    iter++;
  }
  throw new Error(`WASM payment ${paymentHash.slice(0, 12)} did not settle within ${timeoutMs}ms`);
}

// Poll a native node's /payment/{hash} until it reaches a terminal status. NO WASM ticking here:
// the WASM node forwards autonomously via the Phase 0.2 autoDrive loop, so this verifies that the
// self-driving intermediary actually moves the HTLC end-to-end without any manual prodding.
async function waitNativePayment(nativeMgmtUrl, paymentHash, timeoutMs) {
  const deadline = Date.now() + timeoutMs;
  let iter = 0;
  while (Date.now() < deadline) {
    const rec = await nativeGet(nativeMgmtUrl, `/payment/${paymentHash}`);
    if (rec && (rec.status === "succeeded" || rec.status === "failed")) return rec;
    if (iter % 5 === 0) log(`waiting native payment ${paymentHash.slice(0, 12)}`, rec);
    await sleep(1500);
    iter++;
  }
  throw new Error(`native payment ${paymentHash.slice(0, 12)} did not settle within ${timeoutMs}ms`);
}

// ---------------------------------------------------------------------------
// flow
// ---------------------------------------------------------------------------

async function runFlow(cfg, runtimeId) {
  await init();
  log("WASM initialized", { runtimeId });

  const keys = rgbGenerateKeysValue("regtest");
  const sdkPassword = "rgb-multihop-flow";
  const sdk = new RlnWasmSdk();
  await sdk.initValue(sdkPassword, keys.mnemonic);
  await sdk.unlock(JSON.stringify({ password: sdkPassword }));
  log("SDK initialized + unlocked");

  const node = RlnWasmNode.newWithNodeRuntimeId(cfg.nodeProxyUrl, runtimeId, "Regtest");
  const myPubkey = JSON.parse(node.nodePubkeyJson());
  log("WASM node created", myPubkey);

  const wallet = await RlnWasmWallet.create(
    JSON.stringify({
      data_dir: `/tmp/rln_wasm_mh_${runtimeId}`,
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

  await fundWallet(cfg.gatewayUrl, wallet, online, walletAddress);
  // Keep the background chain-sync loop dormant; drive ticks explicitly (see full-flow rationale).
  node.chainSyncStartValue(cfg.esploraUrl, 3_600_000);

  // --- discover + connect to both native nodes ---
  const infoA = await nativeGet(cfg.nativeAMgmtUrl, "/info");
  const infoB = await nativeGet(cfg.nativeBMgmtUrl, "/info");
  const pubkeyA = infoA.node_id;
  const pubkeyB = infoB.node_id;
  log("native-A (payer) info", infoA);
  log("native-B (payee) info", infoB);
  assert(pubkeyA !== pubkeyB, "native-A and native-B must be distinct nodes");
  await node.connectPeer(cfg.nativeAPeerAddr, pubkeyA);
  await node.connectPeer(cfg.nativeBPeerAddr, pubkeyB);
  log("Connected to native-A and native-B");

  // === open WASM<->A channel (WASM funds) ===
  log("Opening WASM<->native-A channel...");
  node.openChannelValueWithOptions(pubkeyA, CHANNEL_CAPACITY_SAT, false, null, null, null, null, null);
  await fundVanillaChannel(node, wallet, online, cfg.gatewayUrl, cfg.esploraUrl, walletAddress, pubkeyA);
  const chanA = await waitForUsableChannel(node, pubkeyA, cfg.gatewayUrl, walletAddress, CHANNEL_READY_TIMEOUT_MS);
  log("✅ WASM<->native-A channel usable", { id: chanA.channel_id });

  // === open WASM<->B channel (WASM funds; gives WASM outbound liquidity toward B for forwarding) ===
  log("Opening WASM<->native-B channel...");
  node.openChannelValueWithOptions(pubkeyB, CHANNEL_CAPACITY_SAT, false, null, null, null, null, null);
  await fundVanillaChannel(node, wallet, online, cfg.gatewayUrl, cfg.esploraUrl, walletAddress, pubkeyB);
  const chanB = await waitForUsableChannel(node, pubkeyB, cfg.gatewayUrl, walletAddress, CHANNEL_READY_TIMEOUT_MS);
  log("✅ WASM<->native-B channel usable", { id: chanB.channel_id });

  // === seed native-A outbound liquidity on the A<->WASM channel ===
  // WASM funded that channel, so native-A starts with 0 outbound. Push it some sats so it can
  // originate the multi-hop payment back through WASM.
  log("Seeding native-A outbound liquidity (WASM keysend -> A)...", { msat: SEED_KEYSEND_MSAT.toString() });
  const seed = node.keysendLiveValue(pubkeyA, SEED_KEYSEND_MSAT, null, null);
  const seedSettled = await waitWasmPayment(node, seed.payment_hash, PAYMENT_TIMEOUT_MS);
  assert(seedSettled.status === "succeeded", "seed keysend to native-A did not settle");
  log("✅ native-A seeded with outbound liquidity", seedSettled);

  // === hand the wheel to the autonomous drive loop (Phase 0.2) so WASM forwards on its own ===
  const auto = node.autoDriveStartValue(AUTO_DRIVE_INTERVAL_MS);
  assert(auto && auto.running === true, `autoDrive should be running: ${safeJson(auto)}`);
  log("✅ autoDrive started — WASM will forward the multi-hop HTLC autonomously", auto);

  // === native-B creates an invoice; native-A pays it routed THROUGH WASM ===
  // native-A has no channel to native-B and no gossip; it can only reach B via the private-channel
  // route hint (WASM->B) that LDK embeds in B's invoice.
  const invoice = await nativePost(cfg.nativeBMgmtUrl, "/invoice", {
    amt_msat: Number(MULTIHOP_PAYMENT_MSAT),
    expiry_sec: 3600,
  });
  log("native-B created invoice", invoice);
  const decoded = node.decodeLnInvoiceValue(invoice.invoice);
  log("Invoice decoded by WASM (route hints carry WASM->B)", {
    payment_hash: decoded.payment_hash,
    route_hints: decoded.route_hints ?? decoded.hints ?? "(see raw)",
  });

  const pay = await nativePost(cfg.nativeAMgmtUrl, "/pay_invoice", {
    invoice: invoice.invoice,
    amt_msat: Number(MULTIHOP_PAYMENT_MSAT),
  });
  log("native-A initiated multi-hop payment (native-A -> WASM -> native-B)", pay);
  const paymentHash = pay.payment_hash || invoice.payment_hash;

  // Payer's PaymentSent fires only after the FULL route (incl. the WASM forward) settles.
  const payerStatus = await waitNativePayment(cfg.nativeAMgmtUrl, paymentHash, PAYMENT_TIMEOUT_MS);
  log("native-A payment status", payerStatus);
  assert(payerStatus.status === "succeeded", `multi-hop payment did not succeed: ${safeJson(payerStatus)}`);
  assert(!!payerStatus.preimage, "multi-hop payment has no preimage (not a real PaymentSent)");

  // Payee confirms it actually received the funds.
  const payeeStatus = await waitNativePayment(cfg.nativeBMgmtUrl, paymentHash, 30_000);
  log("native-B receive status", payeeStatus);
  assert(payeeStatus.status === "succeeded", `native-B did not claim the payment: ${safeJson(payeeStatus)}`);
  assert(payeeStatus.inbound === true, "native-B payment not marked inbound");

  log("✅✅ MULTI-HOP SETTLED: native-A -> WASM (forward) -> native-B", {
    paymentHash,
    amountMsat: MULTIHOP_PAYMENT_MSAT.toString(),
    preimage: payerStatus.preimage,
  });

  node.autoDriveStopValue();

  const result = {
    ok: true,
    phase: "run",
    runtimeId,
    wasmPubkey: myPubkey,
    payerPubkey: pubkeyA,
    payeePubkey: pubkeyB,
    channelToA: chanA.channel_id,
    channelToB: chanB.channel_id,
    paymentHash,
    multiHopAmountMsat: MULTIHOP_PAYMENT_MSAT.toString(),
    preimage: payerStatus.preimage,
  };
  log("=== MULTI-HOP RUN COMPLETE ===", result);
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
    gatewayUrl: readParam("gatewayUrl", DEFAULTS.gatewayUrl),
    nativeAPeerAddr: readParam("nativeAPeerAddr", DEFAULTS.nativeAPeerAddr),
    nativeAMgmtUrl: readParam("nativeAMgmtUrl", DEFAULTS.nativeAMgmtUrl),
    nativeBPeerAddr: readParam("nativeBPeerAddr", DEFAULTS.nativeBPeerAddr),
    nativeBMgmtUrl: readParam("nativeBMgmtUrl", DEFAULTS.nativeBMgmtUrl),
  };
  const runtimeId = readParam("runtimeId", `rgb-mh-${Math.random().toString(16).slice(2)}`);
  log("Config", { ...cfg, runtimeId });

  try {
    const result = await runFlow(cfg, runtimeId);
    window.__E2E_RESULT = result;
    window.__E2E_DONE = true;
    log("*** MULTI-HOP SUCCESS ***");
  } catch (err) {
    const failure = { ok: false, phase: "run", runtimeId, error: String(err && err.stack ? err.stack : err) };
    window.__E2E_RESULT = failure;
    window.__E2E_DONE = true;
    log("*** MULTI-HOP FAILED ***", failure);
  }
}

if (new URLSearchParams(window.location.search).has("phase")) {
  main();
} else {
  const btn = document.getElementById("run");
  if (btn) btn.addEventListener("click", () => main());
}
