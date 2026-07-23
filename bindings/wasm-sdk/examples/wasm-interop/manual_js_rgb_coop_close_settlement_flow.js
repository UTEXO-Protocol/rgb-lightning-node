// Repro / regression flow for: "On-chain RGB balance does not settle on the WASM side after
// channel close" (hackmd bug report).
//
// A native hub opens a REAL, on-chain-funded, anchors RGB channel to the wasm node with
// push_asset_amount (seeding the wasm side's in-channel RGB), one RGB keysend lands hub→wasm to
// make the wasm balance distinct from the pushed amount, and then the WASM side cooperatively
// closes the channel. After the colored closing transaction confirms:
//
//   - the NATIVE side recovers its RGB share on-chain automatically (SpendableOutputs →
//     OutputSweeper → RgbOutputSpender colored sweep) — used here as the infra control;
//   - the WASM side, on the bug, never settles: ChainMonitor events are never drained, there is
//     no Event::SpendableOutputs handler, and no sweep pipeline exists, so
//     wallet.getAssetBalance stays 0 forever even though the closing tx is colored and the
//     transfer info is recorded.
//
// The flow fails with a REPRO marker if the wasm side's on-chain RGB balance does not become
// >= the expected in-channel amount within the settlement timeout, while passing end-to-end once
// the wasm sweep pipeline exists.
//
// The flow: the hub funds its wallet, issues NIA and opens the real RGB channel to us with
// push (300 RGB); a hub → wasm RGB keysend (50 RGB) brings the wasm in-channel RGB to 350;
// pre-close wallet balances are snapshotted (on-chain RGB expected 0 — everything is
// in-channel); the wasm side cooperatively closes and both sides negotiate the colored closing
// tx; finally chain sync + RGB work are pumped while mining until the native side settles
// (control) and the wasm side's getAssetBalance reaches the expected 350 on-chain.
//
// Driven headlessly by run_coop_close_settlement_flow.mjs, or manually via
// rgb_coop_close_settlement_flow.html.

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
  nativePeerAddr: "127.0.0.1:9802",
  nativeMgmtUrl: "http://127.0.0.1:3101",
};

const CHANNEL_CAPACITY_SAT = 1_000_000;
const CHANNEL_PUSH_MSAT = 500_000_000; // 500k sat of BTC pushed to the wasm side
const COLORED_UTXO_SIZE_SAT = CHANNEL_CAPACITY_SAT + 100_000;
const ASSET_TOTAL_ISSUE = 2000; // NIA minted on the hub
const ASSET_CHANNEL_AMOUNT = 600; // RGB committed into the channel on open
const ASSET_PUSH_AMOUNT = 300; // RGB pushed to the wasm side on open (hub keeps 300)
const RGB_KEYSEND_AMOUNT = 50; // hub → wasm keysend; wasm expects 350 on-chain after close
const RGB_HTLC_MSAT = 3_000_000; // RGB-LN minimum BTC carried per HTLC

const EXPECTED_WASM_RGB = ASSET_PUSH_AMOUNT + RGB_KEYSEND_AMOUNT; // 350
const EXPECTED_HUB_RGB = ASSET_CHANNEL_AMOUNT - ASSET_PUSH_AMOUNT - RGB_KEYSEND_AMOUNT; // 250

const CHANNEL_READY_TIMEOUT_MS = 240_000;
const FUND_TIMEOUT_MS = 60_000;
const PAYMENT_TIMEOUT_MS = 90_000;
const RGB_FUNDING_WORK_TIMEOUT_MS = 30_000;
const FETCH_TIMEOUT_MS = 15_000;
// Settlement needs: closing tx broadcast + confirm, SpendableOutputs maturity (ANTI_REORG_DELAY
// = 6 confs), sweep broadcast + confirm, refresh. We mine continuously, so this bounds the
// whole tail of the flow.
const SETTLEMENT_TIMEOUT_MS = Number(240_000);

// ---------------------------------------------------------------------------
// helpers (shared shape with manual_js_rgb_e2e_full_flow.js)
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
  const resp = await fetch(`${nativeMgmtUrl}${path}`, { signal: AbortSignal.timeout(FETCH_TIMEOUT_MS) });
  if (!resp.ok) throw new Error(`${path} failed: ${resp.status} ${await resp.text().catch(() => "")}`);
  return resp.json();
}

async function nativePost(nativeMgmtUrl, path, body, timeoutMs = FETCH_TIMEOUT_MS) {
  const resp = await fetch(`${nativeMgmtUrl}${path}`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(body ?? {}),
    signal: AbortSignal.timeout(timeoutMs),
  });
  const text = await resp.text().catch(() => "");
  if (!resp.ok) throw new Error(`${path} failed: ${resp.status} ${text.slice(0, 300)}`);
  return text ? JSON.parse(text) : {};
}

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

async function fundAddress(gatewayUrl, address, amountBtc, mineBlocksCount) {
  const resp = await fetch(`${gatewayUrl}/dev/regtest/fund`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ address, amount_btc: amountBtc, mine_blocks: mineBlocksCount }),
    signal: AbortSignal.timeout(FETCH_TIMEOUT_MS),
  });
  if (!resp.ok) throw new Error(`fund ${address} failed: ${resp.status} ${await resp.text().catch(() => "")}`);
}

// Wait until the native hub's on-chain wallet actually sees at least `minSat` of vanilla funds.
async function waitNativeFunds(cfg, minSat, timeoutMs = 60_000) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const bal = await nativePost(cfg.nativeMgmtUrl, "/btcbalance", { skip_sync: false }).catch(() => null);
    const spendable = Number(bal?.vanilla?.spendable ?? 0);
    if (spendable >= minSat) return bal;
    await sleep(2000);
  }
  throw new Error(`native hub wallet did not see ${minSat} sat within ${timeoutMs}ms`);
}

// Bootstrap the native hub as the RGB issuer: fund its wallet, create colored UTXOs, issue NIA.
async function nativeBootstrapRgbAsset(cfg, walletAddress) {
  const { address: nativeAddr } = await nativePost(cfg.nativeMgmtUrl, "/address", {});
  log("Native hub wallet address", { nativeAddr });
  await fundAddress(cfg.gatewayUrl, nativeAddr, 1, 6);
  await waitNativeFunds(cfg, COLORED_UTXO_SIZE_SAT * 5 + 1_000_000);
  await nativePost(cfg.nativeMgmtUrl, "/refreshtransfers", { asset_id: null, filter: [], skip_sync: false }).catch(() => {});

  await withRetry(
    () => nativePost(cfg.nativeMgmtUrl, "/createutxos", {
      up_to: false, num: 5, size: COLORED_UTXO_SIZE_SAT, fee_rate: 1, skip_sync: false,
    }),
    5,
    () => sleep(3000),
  );
  await mineBlocks(cfg.gatewayUrl, walletAddress, 3);
  await nativePost(cfg.nativeMgmtUrl, "/refreshtransfers", { asset_id: null, filter: [], skip_sync: false }).catch(() => {});

  const issued = await nativePost(cfg.nativeMgmtUrl, "/issueassetnia", {
    ticker: "CLS", name: "Coop Close Settlement Repro", precision: 0, amounts: [ASSET_TOTAL_ISSUE],
  });
  const assetId = issued.asset.asset_id;
  log("Native hub issued NIA asset", { assetId });
  await mineBlocks(cfg.gatewayUrl, walletAddress, 3);
  await nativePost(cfg.nativeMgmtUrl, "/refreshtransfers", { asset_id: null, filter: [], skip_sync: false }).catch(() => {});
  return assetId;
}

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
    await node.chainSyncTickValue().catch(() => {});
    let nativePeers = [];
    try {
      const resp = await fetch(`${nativeMgmtUrl}/listpeers`, { signal: AbortSignal.timeout(FETCH_TIMEOUT_MS) });
      if (resp.ok) nativePeers = (await resp.json()).peers || [];
    } catch (_e) {
      /* keep polling */
    }
    if (nativePeers.some((p) => p.pubkey === wasmPubkeyHex)) return true;
    if (iter % 4 === 0) {
      log(`waitForNativePeer[${iter}]`, {
        wasmSees: wasmPeerView(node),
        lspSees: nativePeers.map((p) => (p.pubkey || "").slice(0, 12)),
      });
    }
    iter++;
    await sleep(500);
  }
  throw new Error(`native hub did not see us (${wasmPubkeyHex.slice(0, 12)}) as a peer within ${timeoutMs}ms`);
}

async function fundWallet(gatewayUrl, wallet, online, address) {
  log("Funding wasm wallet on-chain...", { address });
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

// Drive the wasm node (chain sync + RGB funding work) and mine periodically until the REAL RGB
// channel to `peer` is usable on the wasm side.
async function waitForUsableChannel(node, peer, gatewayUrl, walletAddress, timeoutMs) {
  const deadline = Date.now() + timeoutMs;
  let iter = 0;
  while (Date.now() < deadline) {
    try {
      await node.chainSyncTickValue();
    } catch (e) {
      log(`chainSyncTick err iter=${iter}`, String(e));
    }
    try {
      await withTimeout(node.driveRgbFundingWork(), RGB_FUNDING_WORK_TIMEOUT_MS, "driveRgbFundingWork");
    } catch (e) {
      if (String(e).includes("timed out")) throw e;
    }
    const found = node
      .listChannelsValue()
      .find((c) => c.peer_pubkey === peer && c.is_usable);
    if (found) return found;
    if (iter % 3 === 2) await mineBlocks(gatewayUrl, walletAddress, 3);
    if (iter % 5 === 0) {
      log("waiting for the real RGB channel to become usable", node.listChannelsValue().map((c) => ({ id: c.channel_id.slice(0, 12), asset: !!c.asset_id, status: c.status, usable: c.is_usable })));
    }
    await sleep(2000);
    iter++;
  }
  throw new Error(`real RGB channel to ${peer.slice(0, 12)} not usable within ${timeoutMs}ms`);
}

// Wait until the native hub reports the channel to us as ready.
async function waitForNativeChannelReady(nativeMgmtUrl, wasmPubkeyHex, timeoutMs) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const { channels = [] } = await nativeGet(nativeMgmtUrl, "/listchannels").catch(() => ({}));
    const ch = channels.find((c) => c.peer_pubkey === wasmPubkeyHex && c.asset_id);
    if (ch && ch.ready) return ch;
    await sleep(1500);
  }
  throw new Error(`native hub channel to us not ready within ${timeoutMs}ms`);
}

// getAssetBalance throws "Asset ... not found" while the wallet has never seen the asset
// on-chain (an inbound hub-funded channel keeps all RGB in-channel until close settlement) —
// treat that as an all-zero balance.
function assetBalanceOrZero(wallet, assetId) {
  try {
    return wallet.getAssetBalanceValue(assetId);
  } catch (_e) {
    return { settled: 0, future: 0, spendable: 0, unknown_asset: true };
  }
}

async function settleChainConvergence(node, rounds = 6, gapMs = 900) {
  for (let i = 0; i < rounds; i++) {
    try {
      await node.chainSyncTickValue();
    } catch (_e) {
      /* non-fatal */
    }
    await sleep(gapMs);
  }
}

async function pumpOnce(node) {
  try {
    await node.driveRgbFundingWork();
  } catch (_e) { /* non-fatal */ }
  try {
    await node.chainSyncTickValue();
  } catch (_e) { /* non-fatal */ }
  try {
    await node.driveRgbFundingWork();
  } catch (_e) { /* non-fatal */ }
}

// Poll the native hub's view of an outbound payment until Succeeded.
async function waitNativePaymentSettled(node, cfg, paymentHash, label, timeoutMs) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    await pumpOnce(node);
    const { payments = [] } = await nativeGet(cfg.nativeMgmtUrl, "/listpayments").catch(() => ({}));
    const p = payments.find((x) => x.payment_hash === paymentHash);
    const status = String(p?.status ?? "").toLowerCase();
    if (status === "succeeded") return p;
    if (status === "failed") throw new Error(`${label}: native payment ${paymentHash.slice(0, 16)} FAILED`);
    await sleep(750);
  }
  throw new Error(`${label}: native payment ${paymentHash.slice(0, 16)} did not settle within ${timeoutMs}ms`);
}

// ---------------------------------------------------------------------------
// the flow
// ---------------------------------------------------------------------------

async function runFlow(cfg, runtimeId) {
  await init();
  log("WASM initialized", { runtimeId, flow: "rgb-coop-close-settlement" });

  const keys = rgbGenerateKeysValue("regtest");
  const sdkPassword = "rgb-coop-close-settlement";
  const sdk = new RlnWasmSdk();
  await sdk.initValue(sdkPassword, keys.mnemonic);
  await sdk.unlock(JSON.stringify({ password: sdkPassword }));
  log("SDK initialized + unlocked");

  const node = RlnWasmNode.newWithNodeRuntimeId(cfg.nodeProxyUrl, runtimeId, "Regtest");
  const myPubkey = JSON.parse(node.nodePubkeyJson());
  let myPubkeyHex =
    (typeof myPubkey === "string" ? myPubkey : myPubkey?.pubkey ?? myPubkey?.node_pubkey) || "";
  assert(myPubkeyHex, "could not determine wasm node pubkey");
  log("WASM node created", myPubkey);

  const wallet = await RlnWasmWallet.create(
    JSON.stringify({
      data_dir: `/tmp/rln_wasm_coop_close_${runtimeId}`,
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
  // The sweep's witness_receive + post_consignment resolve their transport endpoint from the
  // wallet's RGB proxy transport config — configure it exactly like a production app would.
  wallet.setRgbProxyTransport(cfg.rgbProxyUrl, null, null);
  const online = await wallet.goOnlineValue(true, cfg.esploraUrl);
  node.attachWallet(wallet);
  const walletAddress = wallet.getAddress();
  log("Wallet online + attached", { walletAddress });

  await fundWallet(cfg.gatewayUrl, wallet, online, walletAddress);
  node.chainSyncStartValue(cfg.esploraUrl, 3_600_000);

  const nativeInfo = await nativeGet(cfg.nativeMgmtUrl, "/nodeinfo");
  const nativePubkey = nativeInfo.pubkey;
  log("Native hub info", { pubkey: nativePubkey });

  // === bootstrap: hub issues the asset ===
  const assetId = await nativeBootstrapRgbAsset(cfg, walletAddress);

  // === bootstrap: connect to the hub ===
  await node.connectPeer(cfg.nativePeerAddr, nativePubkey);
  {
    const live = JSON.parse(node.nodePubkeyJson());
    const liveHex = (typeof live === "string" ? live : live?.pubkey ?? live?.node_pubkey) || "";
    if (liveHex && liveHex !== myPubkeyHex) {
      log("on-wire pubkey refreshed after connect", { initial: myPubkeyHex.slice(0, 16), live: liveHex.slice(0, 16) });
      myPubkeyHex = liveHex;
    }
  }
  await waitForNativePeer(node, cfg.nativeMgmtUrl, myPubkeyHex, 60_000);
  log("✅ connected to the hub; it reports us as a peer");
  const reconnectAndWaitForPeer = async () => {
    await node.connectPeer(cfg.nativePeerAddr, nativePubkey).catch(() => {});
    await waitForNativePeer(node, cfg.nativeMgmtUrl, myPubkeyHex, 20_000).catch(() => {});
  };

  // === bootstrap: hub opens the REAL RGB channel to us ===
  log("Requesting native hub to open a REAL RGB channel to us...", {
    assetId, assetAmount: ASSET_CHANNEL_AMOUNT, pushAssetAmount: ASSET_PUSH_AMOUNT,
  });
  const openRes = await withRetry(
    () =>
      nativePost(cfg.nativeMgmtUrl, "/openchannel", {
        peer_pubkey_and_opt_addr: `${myPubkeyHex}@127.0.0.1:9735`,
        capacity_sat: CHANNEL_CAPACITY_SAT,
        push_msat: CHANNEL_PUSH_MSAT,
        asset_id: assetId,
        asset_amount: ASSET_CHANNEL_AMOUNT,
        push_asset_amount: ASSET_PUSH_AMOUNT,
        public: false,
        with_anchors: true, // RGB channels require anchors
        fee_base_msat: null,
        fee_proportional_millionths: null,
        temporary_channel_id: null,
        // NO virtual_open_mode: this is a real, on-chain-funded, broadcast channel.
      }, 30_000),
    5,
    reconnectAndWaitForPeer,
  );
  log("Native hub /openchannel accepted", openRes);

  const rgbChannel = await waitForUsableChannel(node, nativePubkey, cfg.gatewayUrl, walletAddress, CHANNEL_READY_TIMEOUT_MS);
  const rgbChannelId = rgbChannel.channel_id;
  assert(rgbChannel.is_usable === true, "real RGB channel did not become usable on the wasm side");
  const nativeChannel = await waitForNativeChannelReady(cfg.nativeMgmtUrl, myPubkeyHex, 60_000);
  assert(
    Number(nativeChannel.asset_local_amount) === ASSET_CHANNEL_AMOUNT - ASSET_PUSH_AMOUNT,
    `hub-side asset_local_amount should be ${ASSET_CHANNEL_AMOUNT - ASSET_PUSH_AMOUNT}, got ${nativeChannel.asset_local_amount}`,
  );
  log("✅ REAL RGB channel open + ready on both sides", {
    id: rgbChannelId,
    wasm_asset_local: rgbChannel.asset_local_amount,
    hub_asset_local: nativeChannel.asset_local_amount,
  });

  await settleChainConvergence(node);

  // === hub → wasm RGB keysend (makes the wasm in-channel balance ≠ the pushed amount) ===
  log("hub → wasm RGB keysend...", { assetId, asset: RGB_KEYSEND_AMOUNT, amtMsat: RGB_HTLC_MSAT });
  const hubKeysend = await nativePost(cfg.nativeMgmtUrl, "/keysend", {
    dest_pubkey: myPubkeyHex,
    amt_msat: RGB_HTLC_MSAT,
    asset_id: assetId,
    asset_amount: RGB_KEYSEND_AMOUNT,
  }, 30_000);
  const hubKeysendSettled = await waitNativePaymentSettled(
    node, cfg, hubKeysend.payment_hash, "hub→wasm RGB keysend", PAYMENT_TIMEOUT_MS,
  );
  log("✅ hub→wasm RGB keysend settled", hubKeysendSettled);

  // === pre-close snapshot ===
  await settleChainConvergence(node, 4);
  const preCloseChannel = node.listChannelsValue().find((c) => c.channel_id === rgbChannelId);
  const preCloseWasmRgb = assetBalanceOrZero(wallet, assetId);
  const preCloseWasmBtc = wallet.getBtcBalanceValue();
  log("pre-close snapshot", {
    channel_asset_local: preCloseChannel?.asset_local_amount,
    channel_asset_remote: preCloseChannel?.asset_remote_amount,
    wasm_onchain_rgb: preCloseWasmRgb,
    wasm_onchain_btc_vanilla_spendable: preCloseWasmBtc?.vanilla?.spendable,
  });
  assert(
    Number(preCloseChannel?.asset_local_amount ?? 0) === EXPECTED_WASM_RGB,
    `wasm in-channel RGB should be ${EXPECTED_WASM_RGB} before close, got ${preCloseChannel?.asset_local_amount}`,
  );

  // === WASM side cooperatively closes the channel ===
  log("wasm initiates cooperative close...", { channel: rgbChannelId.slice(0, 16) });
  node.closeChannelWithOptions(rgbChannelId, nativePubkey, false);
  // Pump until both sides agree the channel is gone (shutdown + closing_signed negotiation
  // rides the normal peer pump; the colored closing tx needs process_pending_rgb_transactions,
  // which driveRgbFundingWork flushes).
  {
    const deadline = Date.now() + 120_000;
    let closedOnWasm = false;
    let closedOnNative = false;
    while (Date.now() < deadline && !(closedOnWasm && closedOnNative)) {
      await pumpOnce(node);
      closedOnWasm = !node.listChannelsValue().some((c) => c.channel_id === rgbChannelId);
      const { channels = [] } = await nativeGet(cfg.nativeMgmtUrl, "/listchannels").catch(() => ({}));
      closedOnNative = !channels.some((c) => c.peer_pubkey === myPubkeyHex && c.asset_id);
      await sleep(1000);
    }
    assert(closedOnWasm, "channel still listed on the wasm side after cooperative close");
    assert(closedOnNative, "channel still listed on the native side after cooperative close");
  }
  await mineBlocks(cfg.gatewayUrl, walletAddress, 3);
  log("✅ cooperative close negotiated, closing tx mined");

  // === settlement — mine + pump until BOTH sides recover their RGB on-chain ===
  // Native side first (control: proves the closing tx really is colored and the infra works —
  // the native SpendableOutputs → RgbOutputSpender pipeline settles automatically).
  log("waiting for on-chain RGB settlement (native control first, then wasm)...", {
    expected_hub: EXPECTED_HUB_RGB, expected_wasm: EXPECTED_WASM_RGB,
  });
  let nativeSettled = null;
  let wasmSettled = null;
  const deadline = Date.now() + SETTLEMENT_TIMEOUT_MS;
  let iter = 0;
  while (Date.now() < deadline && !(nativeSettled && wasmSettled)) {
    // Keep blocks flowing: closing-tx maturity (ANTI_REORG_DELAY=6) and sweep confirmations.
    await mineBlocks(cfg.gatewayUrl, walletAddress, 2);
    await pumpOnce(node);

    if (!nativeSettled) {
      await nativePost(cfg.nativeMgmtUrl, "/refreshtransfers", { asset_id: null, filter: [], skip_sync: false }).catch(() => {});
      const bal = await nativePost(cfg.nativeMgmtUrl, "/assetbalance", { asset_id: assetId }).catch(() => null);
      const total = Number(bal?.settled ?? 0) + Number(bal?.future ?? 0);
      if (iter % 4 === 0) log(`  [native control] on-chain RGB`, bal);
      if (total >= EXPECTED_HUB_RGB) {
        nativeSettled = bal;
        log("✅ native control settled its RGB share on-chain", bal);
      }
    }

    if (!wasmSettled) {
      await wallet.refreshValue(online, null, [], false).catch((e) => {
        if (iter % 4 === 0) log("  wasm refresh error (non-fatal)", String(e));
      });
      const bal = assetBalanceOrZero(wallet, assetId);
      const total = Number(bal?.settled ?? 0) + Number(bal?.future ?? 0);
      if (iter % 4 === 0) log(`  [wasm] on-chain RGB`, bal);
      if (total >= EXPECTED_WASM_RGB) {
        wasmSettled = bal;
        log("✅ wasm side settled its RGB share on-chain", bal);
      }
    }

    iter++;
    await sleep(2000);
  }

  if (!nativeSettled) {
    throw new Error(
      `native control did not settle ${EXPECTED_HUB_RGB} RGB on-chain within ${SETTLEMENT_TIMEOUT_MS}ms — infra problem, not the wasm bug`,
    );
  }
  if (!wasmSettled) {
    const bal = assetBalanceOrZero(wallet, assetId);
    throw new Error(
      `REPRO: wasm on-chain RGB balance did not settle after cooperative close — ` +
      `expected >= ${EXPECTED_WASM_RGB}, getAssetBalance=${safeJson(bal)} while the native side ` +
      `recovered ${EXPECTED_HUB_RGB}. This is the "On-chain RGB balance does not settle on the ` +
      `WASM side after channel close" bug (no ChainMonitor drain / SpendableOutputs handler / sweep pipeline).`,
    );
  }

  const postCloseWasmBtc = wallet.getBtcBalanceValue();
  log("post-close wasm BTC balance", {
    vanilla_spendable: postCloseWasmBtc?.vanilla?.spendable,
    colored_spendable: postCloseWasmBtc?.colored?.spendable,
  });

  const result = {
    ok: true,
    flow: "rgb-coop-close-settlement",
    runtimeId,
    assetId,
    rgbChannelId,
    wasmOnchainRgb: wasmSettled,
    nativeOnchainRgb: nativeSettled,
  };
  log("=== COOP-CLOSE RGB SETTLEMENT FLOW COMPLETE ===", result);
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
  const runtimeId = readParam("runtimeId", `coop-close-${Math.random().toString(16).slice(2)}`);
  log("Config", { ...cfg, runtimeId });

  try {
    const result = await runFlow(cfg, runtimeId);
    window.__E2E_RESULT = result;
    window.__E2E_DONE = true;
    log("*** COOP-CLOSE RGB SETTLEMENT E2E SUCCESS ***");
  } catch (err) {
    const failure = { ok: false, runtimeId, error: String(err && err.stack ? err.stack : err) };
    window.__E2E_RESULT = failure;
    window.__E2E_DONE = true;
    log("*** COOP-CLOSE RGB SETTLEMENT E2E FAILED ***", failure);
  }
}

if (new URLSearchParams(window.location.search).has("autorun")) {
  main();
} else {
  const btn = document.getElementById("run");
  if (btn) btn.addEventListener("click", () => main());
}
