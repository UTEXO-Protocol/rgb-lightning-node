// Real end-to-end RGB-over-Lightning flow over TRUSTED VIRTUAL channels: a WASM node against a
// native Rust LSP node.
//
// This is NOT an API mock — the WASM node speaks the real LN wire protocol to the native
// `rgb-lightning-node` LSP through the WebSocket relay and settles REAL HTLCs
// (keysendLiveValue → send_spontaneous_payment).
//
// Channel model: the native LSP OPENS both channels to this wasm node as trusted virtual channels
// (0-conf, scid-privacy, never-broadcast dust=1 funding); the wasm node ACCEPTS them via its
// Event::OpenChannelRequest handler (accept_inbound_channel_from_trusted_peer_0conf, Virtual — the
// fix this example exercises). The LSP funds + issues everything and pushes BTC + RGB liquidity to
// us so we have outbound capacity for the wasm→native payment steps.
//
// Steps:
//   0. the LSP issues NIA + opens ONE RGB (NIA) virtual channel to us (native allows one virtual
//      channel per peer pair); we accept it 0-conf. Also (0d) a regression check that invoiceStatus
//      does not trap the wasm node (is_expired() -> would_expire(unix_now_secs()) fix).
//   1. transfer regular BTC — real keysend / BOLT11 / HODL HTLCs over the virtual channel
//   2. transfer RGB assets — real BOLT11 invoice + keysend HTLCs over the virtual channel
//   3. the LSP abandons the virtual channel; reopen (page reload) and verify persisted state
//
// Driven headlessly by run_e2e_full_flow.mjs, or manually via rgb_e2e_full_flow.html.
//
// Infrastructure required (compose.wasm.yaml) + a native rgb-lightning-node LSP (peer 9802, REST 3101)
// started with `--enable-virtual-channels-v0`.

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

// Both channels are TRUSTED VIRTUAL channels (0-conf, scid-privacy, never-broadcast dust=1 funding)
// that the native LSP OPENS to this wasm node; the wasm node ACCEPTS them via its
// Event::OpenChannelRequest handler (accept_inbound_channel_from_trusted_peer_0conf, Virtual). Because
// the LSP is the opener/funder, it ALSO pushes BTC + RGB liquidity to us so we have OUTBOUND capacity
// for the wasm→native payment steps below. Native REST takes plain numbers (not BigInt) here.
const VANILLA_CAPACITY_SAT = 1_000_000; // LSP-funded vanilla (BTC-only) virtual channel
const RGB_CAPACITY_SAT = 1_000_000; // LSP-funded RGB (NIA) virtual channel
const VANILLA_PUSH_MSAT = 500_000_000; // 500k sat pushed to us → our outbound BTC on the vanilla channel
const RGB_CHANNEL_PUSH_MSAT = 500_000_000; // 500k sat pushed to us → our outbound BTC on the RGB channel
const COLORED_UTXO_SIZE_SAT = RGB_CAPACITY_SAT + 100_000;
const ASSET_TOTAL_ISSUE = 2000; // total NIA minted ON THE LSP (the LSP funds the RGB channel now)
const ASSET_CHANNEL_AMOUNT = 1000; // RGB the LSP commits into the RGB channel on open
const ASSET_PUSH_AMOUNT = 500; // RGB the LSP pushes to us on open → our outbound RGB
const ASSET_SEND_AMOUNT = 100n; // RGB moved wasm→native over LN
const BTC_KEYSEND_MSAT = 30_000_000n; // 30k sat, wasm→native over the pushed vanilla liquidity
// Channels opened by the wasm node enforce the native-parity 3M-msat HTLC floor
// (our_htlc_minimum_msat = HTLC_MIN_MSAT), so any HTLC the wasm *receives* must be >= 3M.
const BOLT11_SEND_MSAT = 3_000_000n;
const BOLT11_RECEIVE_MSAT = 3_000_000n;
const HODL_INVOICE_MSAT = 3_000_000n;
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

// ---------------------------------------------------------------------------
// native LSP helpers — the native node is the LSP that OPENS virtual channels to us
// ---------------------------------------------------------------------------

// Generic POST to the native RLN REST API (no old-mgmt-API path translation).
async function nativePost(nativeMgmtUrl, path, body, timeoutMs = FETCH_TIMEOUT_MS) {
  const resp = await fetch(`${nativeMgmtUrl}${path}`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(body ?? {}),
    signal: AbortSignal.timeout(timeoutMs),
  });
  const text = await resp.text().catch(() => "");
  if (!resp.ok) throw new Error(`${path} failed: ${resp.status} ${text.slice(0, 200)}`);
  return text ? JSON.parse(text) : {};
}

// Retry an async op a few times, running `onRetry` (e.g. pump our event loop) between attempts.
async function withRetry(fn, attempts, onRetry) {
  let lastErr;
  for (let i = 1; i <= attempts; i++) {
    try {
      return await fn();
    } catch (e) {
      lastErr = e;
      log(`attempt ${i}/${attempts} failed`, String(e));
      if (i < attempts && onRetry) await onRetry();
    }
  }
  throw lastErr;
}

// Fund an arbitrary address from the regtest faucet (used to seed the LSP's on-chain wallet).
async function fundAddress(gatewayUrl, address, amountBtc, mineBlocksCount) {
  const resp = await fetch(`${gatewayUrl}/dev/regtest/fund`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ address, amount_btc: amountBtc, mine_blocks: mineBlocksCount }),
    signal: AbortSignal.timeout(FETCH_TIMEOUT_MS),
  });
  if (!resp.ok) throw new Error(`fund ${address} failed: ${resp.status} ${await resp.text().catch(() => "")}`);
}

// Bootstrap the native LSP as the RGB issuer: fund its wallet, create colored UTXOs, issue NIA.
// Returns the asset_id the LSP will commit into the RGB virtual channel. (In the previous version of
// this example the *wasm* node issued the asset and opened the RGB channel; now the LSP is the
// opener/funder, so it must own the asset.)
async function nativeBootstrapRgbAsset(cfg, walletAddress) {
  // 1. Fund the LSP's on-chain wallet: BTC for both channel fundings + the colored UTXO + fees.
  const { address: nativeAddr } = await nativePost(cfg.nativeMgmtUrl, "/address", {});
  log("Native LSP wallet address", { nativeAddr });
  await fundAddress(cfg.gatewayUrl, nativeAddr, 1, 6);
  await nativePost(cfg.nativeMgmtUrl, "/refreshtransfers", {}).catch(() => {});

  // 2. Create colored UTXOs on the LSP (to hold the issued asset + fund the RGB channel).
  await nativePost(cfg.nativeMgmtUrl, "/createutxos", {
    up_to: false, num: 5, size: COLORED_UTXO_SIZE_SAT, fee_rate: 1, skip_sync: false,
  }).catch((e) => log("native createutxos (may already have colored UTXOs)", String(e)));
  await mineBlocks(cfg.gatewayUrl, walletAddress, 3);
  await nativePost(cfg.nativeMgmtUrl, "/refreshtransfers", {}).catch(() => {});

  // 3. Issue the NIA asset ON THE LSP.
  const issued = await nativePost(cfg.nativeMgmtUrl, "/issueassetnia", {
    ticker: "E2E", name: "LSP E2E RGB", precision: 0, amounts: [ASSET_TOTAL_ISSUE],
  });
  const assetId = issued.asset.asset_id;
  log("Native LSP issued NIA asset", { assetId });
  await mineBlocks(cfg.gatewayUrl, walletAddress, 3);
  await nativePost(cfg.nativeMgmtUrl, "/refreshtransfers", {}).catch(() => {});
  return assetId;
}

// Ask the native LSP to OPEN a trusted virtual channel to this wasm node; the wasm node accepts it
// automatically (Event::OpenChannelRequest → accept_inbound_channel_from_trusted_peer_0conf, Virtual).
// `peer_pubkey_and_opt_addr` carries a DUMMY address: we already connected inbound to the LSP, so
// native's connect_peer_if_necessary finds us in list_peers and returns early — the dummy addr is
// never dialed, and the LSP opens the channel over the existing peer link. Pass `assetId` for the RGB
// channel (with `assetAmount`/`pushAssetAmount`), or omit for the vanilla (BTC-only) channel.
async function nativeOpenVirtualChannel(cfg, wasmPubkeyHex, { assetId, assetAmount, pushAssetAmount } = {}) {
  const body = {
    peer_pubkey_and_opt_addr: `${wasmPubkeyHex}@127.0.0.1:9735`,
    capacity_sat: assetId ? RGB_CAPACITY_SAT : VANILLA_CAPACITY_SAT,
    push_msat: assetId ? RGB_CHANNEL_PUSH_MSAT : VANILLA_PUSH_MSAT,
    asset_id: assetId ?? null,
    asset_amount: assetId ? assetAmount : null,
    push_asset_amount: assetId ? pushAssetAmount : null,
    public: false, // trusted virtual channels must be private
    with_anchors: !!assetId, // colored (RGB) channels require anchors
    fee_base_msat: null,
    fee_proportional_millionths: null,
    temporary_channel_id: null,
    virtual_open_mode: "trusted_no_broadcast",
  };
  const res = await nativePost(cfg.nativeMgmtUrl, "/openchannel", body);
  log(`Native LSP opened ${assetId ? "RGB" : "vanilla"} virtual channel`, res);
  return res;
}

// Wait until the native LSP node reports the WASM node as a connected peer. Our LN connection to the
// LSP only completes (and stays alive) while we pump our event loop, so we drive chainSyncTick while
// polling the LSP's /listpeers. This must hold BEFORE the LSP's /openchannel runs, because its
// connect_peer_if_necessary short-circuits only when we are already in its list_peers — otherwise it
// tries to dial our dummy address and fails with FailedPeerConnection.
function wasmPeerView(node) {
  try {
    return JSON.parse(node.listPeersJson()).map((p) => ({ pk: (p.pubkey || "").slice(0, 12), started: p.started }));
  } catch (_e) {
    return "n/a";
  }
}

async function waitForNativePeer(node, nativeMgmtUrl, wasmPubkeyHex, timeoutMs) {
  const deadline = Date.now() + timeoutMs;
  let iter = 0;
  while (Date.now() < deadline) {
    await node.chainSyncTickValue().catch(() => {}); // pump peer events → completes/keeps the LN handshake
    let nativePeers = [];
    try {
      const resp = await fetch(`${nativeMgmtUrl}/listpeers`, {
        signal: AbortSignal.timeout(FETCH_TIMEOUT_MS),
      });
      if (resp.ok) nativePeers = (await resp.json()).peers || [];
    } catch (_e) {
      /* keep polling */
    }
    if (nativePeers.some((p) => p.pubkey === wasmPubkeyHex)) return true;
    if (iter % 4 === 0) {
      // DIAGNOSTIC: compare what the WASM node thinks (its own peers + `started`) vs what the LSP sees.
      log(`waitForNativePeer[${iter}]`, {
        wasmSees: wasmPeerView(node),
        lspSees: nativePeers.map((p) => (p.pubkey || "").slice(0, 12)),
      });
    }
    iter++;
    await sleep(500);
  }
  throw new Error(`native LSP did not see us (${wasmPubkeyHex.slice(0, 12)}) as a peer within ${timeoutMs}ms`);
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

// Converge the native LSP (payer) and the wasm node (payee) on the same chain tip before an inbound
// payment. The wasm node syncs to the chain tip aggressively (on-demand `chainSyncTickValue` ticks),
// while the native LSP's `SpvClient` polls once per second. Right after an earlier block-mining burst
// the native payer can still be a couple blocks behind the tip the wasm node already applied. If it
// pays while behind, it sets the HTLC `cltv_expiry` from its lower height, and the wasm node then
// rejects the inbound HTLC with `PaymentClaimBuffer` ("final CLTV expiry too soon"), because LDK
// requires `cltv_expiry > current_height + HTLC_FAIL_BACK_BUFFER + 1` and the ~2-block default margin
// is exhausted by the skew. STEP 1c mines no blocks, so a brief settle (native polls every ~1s and
// catches up to the tip in a single poll) drives the skew to zero. This mirrors a real deployment,
// where a well-run LSP payer tracks the tip rather than lagging it.
async function settleChainConvergence(node, rounds = 6, gapMs = 900) {
  for (let i = 0; i < rounds; i++) {
    try {
      await node.chainSyncTickValue(); // keep the wasm node pinned to the stable tip
    } catch (_e) {
      /* non-fatal: the peer/event loop is still advancing */
    }
    await sleep(gapMs); // let the native LSP's 1s SpvClient poll catch up to the same tip
  }
}

// Pay our spendable BTC balance on a channel back to the counterparty (the LSP). A never-broadcast
// virtual channel is torn down by the LSP abandoning it, which forfeits whatever BTC the client still
// holds — so the LSP's guard refuses to abandon while our counterparty BTC floor is > 0. We drain by
// keysending the LSP up to `next_outbound_htlc_limit_msat` (LDK's max single HTLC, already net of the
// channel reserve + fee buffer) each round until nothing meaningful is left to send.
async function drainOutboundToNative(node, nativePubkey, channelId) {
  // The RGB virtual channel enforces a minimum HTLC of `rgb_htlc_min_msat` (3,000,000 msat here), so
  // we can never send the last sub-3k-sat sliver over LN — but the commitment-fee accounting drives
  // the counterparty BTC floor to 0 once only that dust remains, which is what the LSP's guard needs.
  const MIN_HTLC_MSAT = 3_000_000n;
  for (let round = 0; round < 60; round++) {
    const ch = node.listChannelsValue().find((c) => c.channel_id === channelId);
    if (!ch) {
      log("drain: channel no longer present", { round });
      break;
    }
    const outbound = BigInt(ch.outbound_msat ?? 0);
    const limit = BigInt(ch.next_outbound_htlc_limit_msat ?? 0);
    // Whatever remains below one HTLC minimum is unspendable via LN — stop cleanly.
    if (limit < MIN_HTLC_MSAT) {
      log("drain: remaining outbound below HTLC minimum", { round, outbound_msat: outbound.toString(), limit_msat: limit.toString() });
      break;
    }
    // Send the FULL available HTLC. The virtual channel has no counterparty reserve, so paying the
    // exact `next_outbound_htlc_limit_msat` drains our spendable balance to 0 (leaving nothing to keep
    // the counterparty BTC floor above 0). If a feerate shift between query and send rejects the exact
    // amount, retry one notch smaller (but never below the HTLC minimum).
    let amount = limit;
    let ks;
    try {
      ks = node.keysendLiveValue(nativePubkey, amount, null, null);
    } catch (_e) {
      amount = limit > MIN_HTLC_MSAT + 1_000_000n ? limit - 1_000_000n : MIN_HTLC_MSAT;
      ks = node.keysendLiveValue(nativePubkey, amount, null, null);
    }
    const settled = await waitLivePayment(node, ks.payment_hash, PAYMENT_TIMEOUT_MS).catch(() => null);
    log(`drain round ${round}`, {
      sent_msat: amount.toString(),
      outbound_before_msat: outbound.toString(),
      status: settled?.status ?? "unsettled",
    });
    if (!settled || settled.status !== "succeeded") break;
  }
}

// Pay our RGB asset balance on a channel back to the LSP. This must run BEFORE the BTC drain (an RGB
// keysend carries BTC, so we cannot move the asset once BTC is gone) and before the abandon (the LSP's
// guard also refuses while our counterparty RGB balance is > 0). Each keysend carries the asset plus
// RGB_KEYSEND_MSAT of BTC; we send in chunks of a proven-routable size (STEP 2c moved 100 units).
async function drainRgbToNative(node, nativePubkey, channelId, assetId) {
  const RGB_CHUNK = 100n;
  for (let round = 0; round < 40; round++) {
    const ch = node.listChannelsValue().find((c) => c.channel_id === channelId);
    if (!ch) {
      log("drain-rgb: channel no longer present", { round });
      break;
    }
    const rgb = BigInt(ch.asset_local_amount ?? 0);
    if (rgb <= 0n) {
      log("drain-rgb: no RGB left to send", { round });
      break;
    }
    const amount = rgb < RGB_CHUNK ? rgb : RGB_CHUNK;
    const ks = node.keysendLiveValue(nativePubkey, RGB_KEYSEND_MSAT, assetId, amount);
    const settled = await waitLivePayment(node, ks.payment_hash, PAYMENT_TIMEOUT_MS).catch(() => null);
    log(`drain-rgb round ${round}`, {
      sent_rgb: amount.toString(),
      rgb_before: rgb.toString(),
      status: settled?.status ?? "unsettled",
    });
    if (!settled || settled.status !== "succeeded") break;
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

  const node = RlnWasmNode.newWithNodeRuntimeId(cfg.nodeProxyUrl, runtimeId, "Regtest");
  const myPubkey = JSON.parse(node.nodePubkeyJson());
  // NOTE: before the live LDK backend is initialized (which connectPeer does), nodePubkeyJson returns
  // a fallback signing identity that does NOT match the node's on-wire LN pubkey. We refresh this
  // after connectPeer below, so `let` (not `const`).
  let myPubkeyHex =
    (typeof myPubkey === "string" ? myPubkey : myPubkey?.pubkey ?? myPubkey?.node_pubkey) || "";
  // The LSP opens channels TO us by pubkey, so we must know our own pubkey up front.
  assert(myPubkeyHex, "could not determine wasm node pubkey (needed for the LSP to open channels to us)");
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
  // Opt in to trusted virtual channels v0 so this node ACCEPTS the LSP's inbound virtual channels:
  // the `Event::OpenChannelRequest` handler gates the 0-conf virtual accept on this flag (default
  // off, mirroring the native node). Set it before attachWallet, which seeds the backend's flag registry.
  node.setEnableVirtualChannelsV0(true);
  // Part 3 coverage: accepting inbound virtual channels is opt-in (flag-gated in the
  // OpenChannelRequest handler); confirm the flag reads back on before we rely on it.
  const vcFlag = JSON.parse(node.enableVirtualChannelsV0Json());
  assert(vcFlag.enabled === true, `enable_virtual_channels_v0 should be on, got ${safeJson(vcFlag)}`);
  node.attachWallet(wallet);
  const walletAddress = wallet.getAddress();
  log("Wallet online + attached", { walletAddress, virtualChannelsEnabled: vcFlag.enabled });

  // --- fund the wallet on-chain ---
  await fundWallet(cfg.gatewayUrl, wallet, online, walletAddress);
  // chainSyncTickValue requires an active sync, but the *background* loop must stay dormant: if it
  // keeps re-syncing Esplora underneath us it can transiently mark channel state stale. So start with
  // a huge interval (effectively off) and drive LDK chain sync explicitly via chainSyncTickValue
  // inside the wait loops.
  node.chainSyncStartValue(cfg.esploraUrl, 3_600_000);

  // --- native node info (we connect right before the opens, in 0b below) ---
  const nativeInfo = await fetchNativeInfo(cfg.nativeMgmtUrl);
  const nativePubkey = nativeInfo.node_id;
  log("Native node info", nativeInfo);

  // === STEP 0: the native LSP OPENS both channels to us as TRUSTED VIRTUAL channels ===
  // This is the whole point of this example. The LSP opens 0-conf, scid-privacy, never-broadcast
  // (dust=1) channels, and THIS wasm node accepts them via its Event::OpenChannelRequest handler
  // (accept_inbound_channel_from_trusted_peer_0conf with ChannelFundingType::Virtual — the bug fix
  // this example exercises). The LSP also pushes BTC + RGB liquidity so we have OUTBOUND capacity for
  // the wasm→native payment steps. NOTE: the LSP must run with `--enable-virtual-channels-v0`.

  // 0a: the LSP issues the RGB asset it will commit into the RGB channel. Done BEFORE we connect so
  // there is no idle gap between establishing our LN connection and the LSP opening channels to us —
  // our connection to the LSP only completes/stays alive while we pump our event loop.
  const assetId = await nativeBootstrapRgbAsset(cfg, walletAddress);
  const assetBalAfterIssue = null; // the asset is issued on the LSP now, not on this wasm wallet

  // 0b: connect to the LSP and pump until IT reports us as a peer — required before its /openchannel,
  // whose connect_peer_if_necessary short-circuits only when we are already in its list_peers.
  await node.connectPeer(cfg.nativePeerAddr, nativePubkey);
  // Refresh our on-wire pubkey now that the live LDK backend is initialized: this is the pubkey the
  // LSP actually sees for us and must target when it opens channels back to us (the pre-connect
  // nodePubkeyJson was a fallback identity that does not match).
  {
    const live = JSON.parse(node.nodePubkeyJson());
    const liveHex = (typeof live === "string" ? live : live?.pubkey ?? live?.node_pubkey) || "";
    if (liveHex && liveHex !== myPubkeyHex) {
      log("on-wire pubkey refreshed after connect", { initial: myPubkeyHex.slice(0, 16), live: liveHex.slice(0, 16) });
      myPubkeyHex = liveHex;
    }
  }
  log("connectPeer returned; WASM peer view", wasmPeerView(node)); // DIAGNOSTIC
  await waitForNativePeer(node, cfg.nativeMgmtUrl, myPubkeyHex, 60_000);
  log("✅ connected to the LSP; it now reports us as a peer");
  // Reconnect + re-establish if a brief idle around a /openchannel retry dropped the connection.
  const reconnectAndWaitForPeer = async () => {
    await node.connectPeer(cfg.nativePeerAddr, nativePubkey).catch(() => {});
    await waitForNativePeer(node, cfg.nativeMgmtUrl, myPubkeyHex, 20_000).catch(() => {});
  };

  // 0c: the LSP opens a SINGLE RGB (NIA) virtual channel to us, committing 1000 RGB and pushing 500
  // RGB + BTC liquidity to us. Native allows only ONE virtual channel per peer pair, so this one
  // channel carries BOTH the BTC and the RGB payments below (its sat capacity handles the BTC HTLCs).
  // We learn the asset from the channel-open consignment the LSP posts (pulled via driveRgbFundingWork
  // inside waitForUsableChannel), so no wasm-side issuance is needed.
  log("Requesting native LSP to open an RGB virtual channel to us...", { assetId, asset: String(ASSET_CHANNEL_AMOUNT) });
  await withRetry(
    () => nativeOpenVirtualChannel(cfg, myPubkeyHex, { assetId, assetAmount: ASSET_CHANNEL_AMOUNT, pushAssetAmount: ASSET_PUSH_AMOUNT }),
    5,
    reconnectAndWaitForPeer,
  );
  const rgbChannel = await waitForUsableChannel(node, nativePubkey, true, cfg.gatewayUrl, walletAddress, CHANNEL_READY_TIMEOUT_MS);
  const rgbChannelId = rgbChannel.channel_id;
  // Regression gate for the virtual-channel accept fix: an accepted 0-conf scid-privacy RGB channel
  // must reach `usable` (accept + anchor negotiation + consignment pull all succeeded).
  assert(rgbChannel.is_usable === true, "RGB virtual channel did not become usable (accept/anchor/consignment?)");
  // Part 4 regression gate: the INBOUND (LSP-opened) RGB channel must surface the RGB asset at the
  // SDK layer. asset_id is now read from the RGB kv store for every live channel (not just the
  // outbound-open cache), so an ACCEPTED RGB channel is recognized as RGB — previously it showed up
  // as vanilla (asset_id = null). asset_local_amount is our local RGB, i.e. what the LSP pushed to us.
  assert(
    rgbChannel.asset_id === assetId,
    `inbound RGB channel asset_id mismatch: got ${rgbChannel.asset_id}, want ${assetId}`,
  );
  assert(
    Number(rgbChannel.asset_local_amount) === ASSET_PUSH_AMOUNT,
    `inbound RGB channel asset_local_amount should equal pushed ${ASSET_PUSH_AMOUNT}, got ${rgbChannel.asset_local_amount}`,
  );
  log("✅ RGB virtual channel usable + recognized as RGB (Part 4: asset_id/asset_local_amount surfaced for inbound channel)", {
    id: rgbChannelId,
    assetId,
    assetLocalAmount: rgbChannel.asset_local_amount,
  });

  // The asset was ISSUED on the LSP; we know it as an off-chain *channel* asset (proven above via
  // rgbChannel.asset_id / asset_local_amount from the LN backend), but our rgb-lib WALLET has no
  // on-chain holding of it — so a wallet-level asset-balance lookup can legitimately fail. Non-fatal.
  let assetBalAfterOpen = null;
  try {
    assetBalAfterOpen = wallet.getAssetBalanceValue(assetId);
  } catch (e) {
    log("wallet asset-balance lookup for LSP-issued asset unavailable (expected; it is a channel asset)", String(e));
  }
  log("Asset balance after RGB channel open (off-chain channel RGB)", assetBalAfterOpen);

  // === STEP 0d: invoiceStatus regression coverage (wasm SystemTime-trap fix) ===
  // Before the fix, `invoiceStatus` on a PENDING invoice called `Bolt11Invoice::is_expired()`, which
  // reads `SystemTime::now()` — unimplemented on wasm32 — so it TRAPPED and poisoned the whole node
  // ("time not implemented on this platform"; every later call became `RuntimeError: unreachable`).
  // The fix uses `would_expire(unix_now_secs())`. That expiry check is only reached while the invoice
  // is still "pending", so we drive both outcomes of it on pending invoices:
  // NOTE: use the non-live `createLnInvoiceValue` here — it registers the invoice in the same
  // runtime payment view that `invoiceStatus` reads (the live variant registers only in the backend,
  // so invoiceStatus would report "unknown LN invoice"). This step doesn't pay the invoice; it only
  // needs it registered + pending so invoiceStatus reaches the would_expire check.
  {
    // (a) fresh invoice → invoiceStatus must NOT trap and must report "pending" (would_expire=false).
    const pendingInv = node.createLnInvoiceValue(BOLT11_RECEIVE_MSAT, 3600, null, null);
    const pendingStatus = JSON.parse(node.invoiceStatusJson(pendingInv.invoice));
    assert(
      pendingStatus.status === "pending",
      `invoiceStatus on a fresh invoice should be 'pending', got ${safeJson(pendingStatus)}`,
    );
    // (b) 1s-expiry invoice, after it lapses → would_expire=true → status transitions to "expired".
    const expiringInv = node.createLnInvoiceValue(BOLT11_RECEIVE_MSAT, 1, null, null);
    await sleep(3000); // let the 1s-expiry invoice lapse
    const expiredStatus = JSON.parse(node.invoiceStatusJson(expiringInv.invoice));
    assert(
      expiredStatus.status === "expired",
      `invoiceStatus on a lapsed invoice should be 'expired', got ${safeJson(expiredStatus)}`,
    );
    log("✅ STEP 0d — invoiceStatus regression coverage passed (would_expire on pending; no SystemTime trap on wasm)");
  }

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
  // Let the payer (native LSP) and payee (wasm) settle on the same chain tip first. Both track the
  // same chain, but the LSP's SpvClient polls once per second while the wasm node syncs on demand, so
  // right after an earlier mining burst the payer can briefly lag the payee. If it pays while behind,
  // the inbound HTLC's cltv is too close to our height and LDK rejects it with PaymentClaimBuffer
  // ("final CLTV expiry too soon"). A brief settle drives that transient skew to zero.
  await settleChainConvergence(node);
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
  // WS2: decodeLnInvoice must surface the RGB asset fields (previously hardcoded null).
  assert(
    decodedWasmRgbInvoice.asset_id === assetId,
    `decoded RGB invoice asset_id mismatch (got ${decodedWasmRgbInvoice.asset_id}, want ${assetId})`,
  );
  assert(
    BigInt(decodedWasmRgbInvoice.asset_amount ?? 0n) === RGB_INVOICE_RECEIVE_AMOUNT,
    `decoded RGB invoice asset_amount mismatch (got ${decodedWasmRgbInvoice.asset_amount}, want ${RGB_INVOICE_RECEIVE_AMOUNT})`,
  );
  log("✅ WS2: decodeLnInvoice surfaced RGB asset fields", {
    asset_id: decodedWasmRgbInvoice.asset_id,
    asset_amount: decodedWasmRgbInvoice.asset_amount,
  });
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

  // === STEP 3: tear down the VIRTUAL channel from BOTH sides ===
  // Virtual channels have never-broadcast (dust=1) funding, so there is no on-chain cooperative/force
  // close. The LSP is the opener: IT abandons its side (native /closechannel → abandon_virtual_channel).
  // That abandon is silent (ErrorAction::IgnoreError) — it never notifies us — so, once we have drained
  // our value, WE abandon our own side (node.closeChannelWithOptions → client-side
  // abandon_virtual_channel) to drop the channel from our view. force=true is intentionally NOT used —
  // it is unsupported for virtual channels.
  // Drain our residual RGB and BTC back to the LSP first: it pushed us both on open, and the
  // never-broadcast virtual channel is torn down by the LSP *abandoning* it (which would forfeit any
  // value we still hold). The LSP's guard therefore refuses to abandon while our counterparty RGB or
  // BTC balance is > 0. RGB must go first — an RGB keysend carries BTC, so we cannot move the asset
  // once BTC is gone.
  log("STEP 3: draining our RGB + BTC balances back to the LSP before teardown...");
  await drainRgbToNative(node, nativePubkey, rgbChannelId, assetId);
  await drainOutboundToNative(node, nativePubkey, rgbChannelId);

  log("STEP 3: LSP abandons the virtual channel...");
  for (const [id, label] of [[rgbChannelId, "RGB"]]) {
    try {
      const r = await nativePost(cfg.nativeMgmtUrl, "/closechannel", {
        channel_id: id, peer_pubkey: myPubkeyHex, force: false,
      });
      log(`LSP abandon requested for ${label} virtual channel`, { id, r });
    } catch (e) {
      // The abandon may report the session already gone on retries; keep draining regardless.
      log(`LSP abandon for ${label} channel returned`, String(e));
    }
  }

  // The LSP's abandon is silent, so drop our own side too (guarded by the drained-to-zero check).
  try {
    node.closeChannelWithOptions(rgbChannelId, nativePubkey, false);
    log("client-side abandon requested for RGB virtual channel");
  } catch (e) {
    log("client-side abandon returned", String(e));
  }

  const closeDeadline = Date.now() + CLOSE_TIMEOUT_MS;
  let rgbGone = false;
  let citer = 0;
  while (Date.now() < closeDeadline && !rgbGone) {
    try {
      await node.chainSyncTickValue(); // pump peer/event loop so the abandonment propagates to us
    } catch (e) {
      log(`close tick err iter=${citer}`, String(e));
    }
    try {
      await node.driveRgbFundingWork();
    } catch (_e) {
      /* non-fatal */
    }
    const ids = new Set(node.listChannelsValue().map((c) => c.channel_id));
    rgbGone = !ids.has(rgbChannelId);
    if (citer % 5 === 0) {
      log("closing…", { rgbGone, channels: node.listChannelsValue().map((c) => ({ id: c.channel_id.slice(0, 12), status: c.status, usable: c.is_usable })) });
    }
    await sleep(1500);
    citer++;
  }
  log("Virtual channel closed?", { rgbGone });
  assert(rgbGone, `virtual channel did not fully close: rgbGone=${rgbGone}`);

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
    rgbChannelId,
    btcKeysend: btcSettled,
    rgbKeysend: rgbSettled,
    rgbKeysendStatus: rgbStatus,
    rgbGone,
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
  log("Loaded run snapshot", { savedPubkey: snap.myPubkey, rgb: snap.rgbChannelId });

  // Reopen the SDK + node with the SAME identity/runtimeId — this is the "reopen".
  const sdk = new RlnWasmSdk();
  await sdk.preloadPersistentRuntimeState();
  await sdk.initValue(snap.sdkPassword, snap.keys.mnemonic);
  await sdk.unlock(JSON.stringify({ password: snap.sdkPassword }));
  log("SDK re-initialized from persisted state");

  const node = RlnWasmNode.newWithNodeRuntimeId(cfg.nodeProxyUrl, runtimeId, "Regtest");
  const reopenedPubkey = JSON.parse(node.nodePubkeyJson());
  log("Node reopened", reopenedPubkey);

  // (a) identity persisted across reopen
  assert(
    JSON.stringify(reopenedPubkey) === JSON.stringify(snap.myPubkey),
    `node identity changed across reopen: ${safeJson(reopenedPubkey)} != ${safeJson(snap.myPubkey)}`
  );
  log("✅ node identity persisted across reopen");

  // (b) real channel state restored from persistence matches the pre-reload state. Both virtual
  // channels were abandoned by the LSP, so the real (non-ghost) channel set must be empty before
  // reload AND after reopen. We compare real channels only — pre-funding temp-id ghosts are a
  // transient SDK-cache artifact that the live ChannelManager never has and that never survive a
  // reload, so they must not count.
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
