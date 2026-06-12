// Real RGB channel flow using WasmRgbBackend (new architecture).
//
// Connects to rgb-native-phase5-node via the WASM proxy gateway, opens a
// real on-chain NIA channel, sends a keysend payment, then cooperatively
// closes.  Replaces the old virtual-channel flow that used RlnWasmSdk.
//
// Infrastructure required (from compose.wasm.yaml or compose.wasm-infra.yaml):
//   - Esplora at DEFAULT_ESPLORA_URL
//   - RGB proxy server at DEFAULT_RGB_PROXY_URL
//   - WASM proxy gateway at DEFAULT_NODE_PROXY_URL
//   - rgb-native-phase5-node binary (from contrib/rgb-cross-variant-harness)
//     listening at DEFAULT_NATIVE_PEER_ADDR / DEFAULT_NATIVE_MGMT_URL

import init, {
  RlnWasmNode,
  RlnWasmSdk,
  RlnWasmWallet,
  rgbGenerateKeysValue,
} from "../../pkg/rln_wasm_sdk.js";

const DEFAULT_NODE_PROXY_URL = "ws://127.0.0.1:3001";
const DEFAULT_ESPLORA_URL = "http://127.0.0.1:3002";
// Route RGB proxy through the gateway so CORS headers are included in responses.
// Direct access to http://127.0.0.1:3000/json-rpc lacks CORS headers and the
// browser blocks the preflight, causing prepare_funding_transfer to hang.
const DEFAULT_RGB_PROXY_URL = "http://127.0.0.1:3001/rgb/json-rpc";
const DEFAULT_GATEWAY_URL = "http://127.0.0.1:3001";
const DEFAULT_NATIVE_PEER_ADDR = "127.0.0.1:19735";
const DEFAULT_NATIVE_MGMT_URL = "http://127.0.0.1:19737";

const CHANNEL_CAPACITY_SAT = 1_000_000n;
// The RGB channel funding tx is funded ONLY from colored (External-keychain) UTXOs —
// rgb-lib's send_begin cannot pull vanilla BTC. So each colored UTXO must cover the full
// channel capacity (plus fees) on its own; otherwise funding fails with
// "Insufficient allocations". Size each one a bit above capacity so a single colored UTXO
// funds the channel (which also minimises how many full prev-txs BDK must hold for add_utxos).
const COLORED_UTXO_SIZE_SAT = Number(CHANNEL_CAPACITY_SAT) + 100_000;
const ASSET_LOCAL_AMOUNT = 1000n;
const KEYSEND_MSAT = 10_000_000n;
const CHANNEL_READY_TIMEOUT_MS = 120_000;
const FUND_TIMEOUT_MS = 60_000;
const FETCH_TIMEOUT_MS = 15_000;
const RGB_FUNDING_STEP_TIMEOUT_MS = 30_000;

function safeJson(v) {
  return JSON.stringify(v, (_k, x) => (typeof x === "bigint" ? x.toString() : x), 2);
}

function log(message, data) {
  const out = document.getElementById("out");
  if (!out) return;
  const line = document.createElement("pre");
  line.textContent = data === undefined ? String(message) : `${message}: ${safeJson(data)}`;
  out.appendChild(line);
}

function readText(id) {
  const el = document.getElementById(id);
  return el && typeof el.value === "string" ? el.value.trim() : "";
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

// Convert http:// URL to the rpc:// scheme expected by rgb-lib transport parsing.
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
    });
    if (!resp.ok) {
      const txt = await resp.text().catch(() => "");
      log(`mineBlocks FAILED: HTTP ${resp.status}`, txt);
    } else {
      log(`mineBlocks: mined ${count} blocks ok`);
    }
  } catch (e) {
    log(`mineBlocks FAILED: network error`, String(e));
  }
}

async function fundWallet(walletAddress, gatewayUrl, wallet, online) {
  log("Requesting regtest funding...", { walletAddress, gatewayUrl });
  const resp = await fetch(`${gatewayUrl}/dev/regtest/fund`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ address: walletAddress, amount_btc: 1, mine_blocks: 6 }),
  });
  if (!resp.ok) {
    const txt = await resp.text().catch(() => "");
    throw new Error(`fund request failed: ${resp.status} ${txt}`);
  }
  const result = await resp.json().catch(() => ({}));
  log("Fund response", result);

  // Poll until vanilla balance is spendable
  const deadline = Date.now() + FUND_TIMEOUT_MS;
  while (Date.now() < deadline) {
    await wallet.syncOnline(online);
    const bal = wallet.getBtcBalanceValue();
    const spendable = Number(bal?.vanilla?.spendable ?? 0);
    const settled = Number(bal?.vanilla?.settled ?? 0);
    if (spendable >= 5000 && settled >= 2000) {
      log("Wallet funded", bal);
      return;
    }
    await sleep(2000);
  }
  throw new Error("wallet did not receive funds within timeout");
}

async function ensureColoredUtxos(wallet, online) {
  await wallet.syncOnline(online);
  const bal = wallet.getBtcBalanceValue();
  const coloredSpendable = Number(bal?.colored?.spendable ?? 0);
  if (coloredSpendable > 0) {
    log("Colored UTXOs already present", { coloredSpendable });
    return;
  }
  log("Creating colored UTXOs...");
  const unsignedPsbt = await wallet.createUtxosBegin(online, true, 5, COLORED_UTXO_SIZE_SAT, 1n, false);
  const signedPsbt = wallet.signPsbtValue(unsignedPsbt);
  const created = await wallet.createUtxosEnd(online, signedPsbt, false);
  const after = wallet.getBtcBalanceValue();
  const coloredAfter = Number(after?.colored?.spendable ?? 0);
  log("Colored UTXOs created", { created, balance: after });
  if (created === 0 || coloredAfter === 0) {
    throw new Error(
      "createUtxosEnd completed without registering colored UTXOs in BDK; rebuild the WASM package with the rgb-lib-wasm broadcast transaction-graph fix"
    );
  }
}

async function waitForChannelUsable(node, peerPubkey, timeoutMs, onIterCb) {
  const deadline = Date.now() + timeoutMs;
  let lastSnapshot = [];
  let iteration = 0;
  while (Date.now() < deadline) {
    // Explicitly tick live LDK on every iteration. chainSyncStartValue's background loop only
    // updates generic chain state; chainSyncTickValue also applies it to live LDK and drains
    // FundingGenerationReady/ChannelPending events into the RGB funding work queue.
    if (iteration % 5 === 0) log(`channel poll heartbeat iter=${iteration}`);
    try {
      await node.chainSyncTickValue();
    } catch (e) {
      log(`chainSyncTickValue error iter=${iteration}`, String(e));
    }

    // Drive prepare/complete funding work queued by the explicit live-LDK tick.
    const driveStart = Date.now();
    log(`driveRgbFundingWork: calling iter=${iteration}`);
    try {
      await withTimeout(node.driveRgbFundingWork(), RGB_FUNDING_STEP_TIMEOUT_MS, "driveRgbFundingWork");
      const elapsed = Date.now() - driveStart;
      log(`driveRgbFundingWork: returned ok iter=${iteration} elapsed=${elapsed}ms`);
    } catch (e) {
      const elapsed = Date.now() - driveStart;
      log(`driveRgbFundingWork error iter=${iteration} elapsed=${elapsed}ms`, String(e));
      if (String(e).includes("driveRgbFundingWork timed out")) throw e;
    }
    const channels = node.listChannelsValue();
    lastSnapshot = channels.map((c) => ({
      channel_id: c.channel_id,
      peer_pubkey: c.peer_pubkey,
      status: c.status,
      is_usable: c.is_usable,
    }));
    if (iteration % 5 === 0) log("Channel status", lastSnapshot);
    const found = channels.find((c) => c.peer_pubkey === peerPubkey);
    if (found?.is_usable) return found;
    if (onIterCb) await onIterCb(iteration);
    await sleep(2000);
    iteration++;
  }
  throw new Error(
    `channel did not become usable within ${timeoutMs}ms. Last: ${JSON.stringify(lastSnapshot)}`
  );
}

async function waitForChannelGone(node, channelId, timeoutMs) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const present = node.listChannelsValue().some((c) => c.channel_id === channelId);
    if (!present) return true;
    await sleep(1000);
  }
  log("Channel close timed out (non-fatal for this demo)", { channelId });
  return false;
}

async function run() {
  const out = document.getElementById("out");
  if (out) out.innerHTML = "";

  const nodeProxyUrl = readText("nodeProxyUrl") || DEFAULT_NODE_PROXY_URL;
  const esploraUrl = readText("esploraUrl") || DEFAULT_ESPLORA_URL;
  const rgbProxyUrl = readText("rgbProxyUrl") || DEFAULT_RGB_PROXY_URL;
  const gatewayUrl = readText("gatewayUrl") || DEFAULT_GATEWAY_URL;
  const nativePeerAddr = readText("nativePeerAddr") || DEFAULT_NATIVE_PEER_ADDR;
  const nativeMgmtUrl = readText("nativeMgmtUrl") || DEFAULT_NATIVE_MGMT_URL;

  await init();
  log("WASM initialized");
  log("Config", {
    nodeProxyUrl,
    esploraUrl,
    rgbProxyUrl,
    nativePeerAddr,
    nativeMgmtUrl,
  });

  // Unique runtime ID per run so each run gets isolated browser storage.
  const runtimeId = `rgb-real-${Math.random().toString(16).slice(2)}`;
  const keys = rgbGenerateKeysValue("regtest");
  log("Keys generated (ephemeral for this run)", { runtimeId });

  // Initialize the SDK lifecycle with the mnemonic.  This sets the stable
  // node identity seed that openChannelValueWithOptions requires.
  const sdk = new RlnWasmSdk();
  const sdkPassword = "rgb-real-channel-demo";
  await sdk.initValue(sdkPassword, keys.mnemonic);
  await sdk.unlock(JSON.stringify({ password: sdkPassword }));
  log("SDK lifecycle initialized");

  // Create node — proxy URL is used for LN peer transport (WebSocket relay).
  const node = RlnWasmNode.newWithNodeRuntimeId(nodeProxyUrl, runtimeId);
  log("Node created", JSON.parse(node.nodePubkeyJson()));

  // Build and bring the RGB wallet online.
  const walletData = {
    data_dir: `/tmp/rln_wasm_rgb_demo_${runtimeId}`,
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
  const wallet = await RlnWasmWallet.create(JSON.stringify(walletData));
  const online = await wallet.goOnlineValue(true, esploraUrl);
  log("Wallet online", { id: online.id });

  // Attach wallet to node — this wires up WasmRgbBackend which the channel
  // manager will use for prepare/complete_funding_transfer on channel open.
  node.attachWallet(wallet);
  log("Wallet attached to node (WasmRgbBackend active)");

  const walletAddress = wallet.getAddress();
  log("Wallet address", walletAddress);

  // Fund wallet with on-chain BTC via the regtest gateway.
  await fundWallet(walletAddress, gatewayUrl, wallet, online);

  // Create colored UTXOs so rgb-lib can allocate the NIA asset.
  await ensureColoredUtxos(wallet, online);

  // Mine to confirm the colored UTXOs — prepare_funding_transfer uses
  // min_confirmations=1, so the allocation inputs must be on-chain.
  log("Mining to confirm colored UTXOs...");
  await mineBlocks(gatewayUrl, walletAddress, 3);
  await wallet.syncOnline(online);
  log("Colored UTXOs confirmed", wallet.getBtcBalanceValue());

  // Issue NIA asset.
  const issued = node.issueAssetNiaValue({
    ticker: "TST",
    name: "WASM RGB Real Channel Demo",
    precision: 0,
    amounts: [Number(ASSET_LOCAL_AMOUNT) * 2],
  });
  const assetId = issued.asset_id;
  log("NIA asset issued", issued);

  // Mine to confirm the issuance transaction, then call wallet.refresh() so
  // rgb-lib advances the issuance transfer from "pending_broadcast" to
  // "settled".  syncOnline alone only updates BDK's UTXO view; it does NOT
  // advance rgb-lib's transfer state machine, so send_begin would still see
  // zero allocations for the issued asset.  refresh(skip_sync=false) does
  // both BDK sync and rgb-lib transfer state refresh in one call.
  log("Mining to confirm issuance...");
  await mineBlocks(gatewayUrl, walletAddress, 3);
  log("Refreshing RGB transfer states (BDK sync + rgb-lib state machine)...");
  const refreshResult = await wallet.refreshValue(online, null, [], false);
  log("Post-issuance refresh done", refreshResult);
  log("Post-issuance balance", wallet.getBtcBalanceValue());

  // Sanity check: the issued asset should be settled and spendable before we
  // open the channel.  (The colored-UTXO full-tx presence in BDK is now
  // guaranteed by the rgb-lib-wasm _broadcast_psbt insert_tx fix.)
  try {
    log("Asset balance before channel open", wallet.getAssetBalanceValue(assetId));
  } catch (e) {
    log("Asset balance check error", String(e));
  }

  // Start recurring node sync only after wallet setup is complete. Before the create-UTXO and
  // issuance transactions are mined, an immediate Esplora sync can temporarily mark those
  // locally broadcast transactions as evicted because Esplora has not indexed them yet.
  node.chainSyncStartValue(esploraUrl, 5000);
  log("Chain sync started (5 s interval)");

  // Fetch the native node's pubkey from its management API.
  log("Fetching native node info...", { nativeMgmtUrl });
  const infoResp = await fetch(`${nativeMgmtUrl}/info`);
  if (!infoResp.ok) {
    throw new Error(`native node info: ${infoResp.status}`);
  }
  const nativeInfo = await infoResp.json();
  const nativePubkey = nativeInfo.node_id;
  log("Native node info", nativeInfo);

  // Connect to native node through the WS proxy relay.
  log("Connecting to native node...", { nativePeerAddr, nativePubkey });
  await node.connectPeer(nativePeerAddr, nativePubkey);
  log("Connected");

  // Open a real RGB on-chain channel.
  // WasmRgbBackend drives the whole funding flow:
  //   FundingGenerationReady → prepare_funding_transfer (creates RGB+BTC PSBT)
  //   ChannelPending         → complete_funding_transfer (broadcasts tx)
  //   (mine blocks)
  //   ChannelReady           → is_usable becomes true
  const rgbTransport = toRgbTransport(rgbProxyUrl);
  log("Opening real RGB channel...", {
    nativePubkey,
    capacitySat: CHANNEL_CAPACITY_SAT.toString(),
    assetId,
    assetLocalAmount: ASSET_LOCAL_AMOUNT.toString(),
    rgbTransport,
  });
  const opened = node.openChannelValueWithOptions(
    nativePubkey,
    CHANNEL_CAPACITY_SAT,
    false,
    assetId,
    ASSET_LOCAL_AMOUNT,
    null,         // null virtual_open_mode = real on-chain channel
    assetId,      // contract_id is the same as asset_id for NIA
    rgbTransport
  );
  log("Channel open initiated", opened);

  // The WasmRgbBackend funding flow:
  //   FundingGenerationReady → driveRgbFundingWork() → prepare_funding_transfer
  //   → funding_transaction_generated → ChannelPending
  //   → driveRgbFundingWork() → complete_funding_transfer (broadcasts tx)
  //   → mine blocks → ChannelReady → is_usable = true
  //
  // driveRgbFundingWork() and block mining are called inside the poll loop.
  log("Waiting for channel to become usable (driveRgbFundingWork + periodic mining)...");
  const channel = await waitForChannelUsable(node, nativePubkey, CHANNEL_READY_TIMEOUT_MS, async (iter) => {
    // Mine 3 blocks every 4 iterations (~8 s) to confirm the funding tx once broadcast.
    if (iter % 4 === 2) {
      try {
        await fetch(`${gatewayUrl}/dev/regtest/fund`, {
          method: "POST",
          headers: { "content-type": "application/json" },
          body: JSON.stringify({ address: walletAddress, amount_btc: 0.0001, mine_blocks: 3 }),
          signal: AbortSignal.timeout(FETCH_TIMEOUT_MS),
        });
      } catch (e) {
        log(`periodic mining error iter=${iter}`, String(e));
      }
    }
  });
  log("Channel READY", channel);

  // Send a small keysend (BTC only) to verify the channel is live.
  log("Sending keysend payment...", { amtMsat: KEYSEND_MSAT.toString() });
  const keysend = node.keysendValue(nativePubkey, KEYSEND_MSAT, undefined, undefined);
  log("Keysend sent", keysend);

  // Cooperative close.
  log("Requesting cooperative close...");
  node.closeChannelWithOptions(channel.channel_id, nativePubkey, false);

  const closed = await waitForChannelGone(node, channel.channel_id, 60_000);
  log(closed ? "Channel closed" : "Channel close remains pending");

  log("=== REAL RGB CHANNEL FLOW COMPLETE ===");
  log("Summary", {
    assetId,
    channelId: channel.channel_id,
    nativePubkey,
    runtimeId,
  });
}

const runBtn = document.getElementById("run");
if (runBtn) {
  runBtn.addEventListener("click", () => {
    run().catch((err) => log("Flow failed", String(err)));
  });
}
