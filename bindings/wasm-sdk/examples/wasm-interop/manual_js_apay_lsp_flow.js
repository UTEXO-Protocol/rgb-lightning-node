// WASM async-payments-with-LSP E2E flow — TWO independent wasm recipients.
//
// Drives the wasm SDK's `apayNew` / `apayNewWithAddress` against a REAL utexo-lsp:
//
//   wasm recipient A ─┐
//                     ├─(LN custom msg 37915: async_order.new)─▶ native invoice-host RLN node
//   wasm recipient B ─┘                                          │  (started with --lsp-base-url)
//                                                                ▼ HTTP /internal/async_order/new
//                                                              utexo-lsp ──▶ sqlite (async_orders,
//                                                                              apay_hash_batch, ...)
//
// Each recipient is a fully independent wasm node (own SDK identity, wallet, runtime scope) and:
//   1. boots SDK + wallet (online) + LDK runtime,
//   2. connects to the shared native invoice-host node and opens a vanilla (BTC) channel
//      (apay_new requires a *live* channel with the host),
//   3. derives a 200-hash batch from its seed, signs the Merkle batch commitment + a
//      `username@domain` Lightning-Address attestation with its node key, ships it to the host,
//      and awaits the LSP's order acknowledgement,
//   4. refills with a second batch and asserts the host-advertised `next_index_expected`
//      advanced (proving the per-host KV cursor persisted).
//
// The flow then asserts the two recipients are distinct LN peers and the LSP tracked them as
// separate orders (distinct order_id, independent hash cursors that both start at index 1).
//
// Finally it exercises the payment path: node B resolves node A's Lightning Address via the LSP's
// LNURL-pay endpoints and pays it. The host holds B's HTLC and asks A for an invoice
// (`async_order.request_invoice` over msg 37915); A's wasm backend re-derives the preimage and
// mints the invoice. That A-mints-the-invoice step is HARD-asserted (it's the new wasm capability).
//
// The final host→A forward + B settle is SOFT (best-effort): it requires the host node to pay an
// invoice whose hash it already holds as a HODL inbound — utexo-lsp's intended single-node forward.
// A stock rgb-lightning-node blocks that with `PaymentHashAlreadyUsed`, so the example logs whether
// full settlement completed but does not fail without it. (Node A first keysends the host some
// outbound liquidity so the host→A leg can pay when the node permits the forward.)
//
// Prereqs — see this directory's README "Async payments with LSP" section:
//   - compose.wasm.yaml infra (proxy/esplora/electrum/gateway on 3000-3002/3001),
//   - a native invoice-host RLN node (peer 9802 / REST 3101) started with
//       --lsp-base-url http://127.0.0.1:8080 --lsp-bearer-token <token>,
//   - utexo-lsp running on :8080 with APAY_BEARER_TOKEN=<token> and LSP_BASE_URL pointing
//     back at the host node's REST.
//
// Driven headlessly by run_apay_lsp_flow.mjs (auto-runs when `?phase=` is present).

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
  // Native invoice-host = a regular `rgb-lightning-node` (LDK peer port 9802, REST 3101),
  // started with --lsp-base-url so it forwards async_order.new to utexo-lsp.
  nativePeerAddr: "127.0.0.1:9802",
  nativeMgmtUrl: "http://127.0.0.1:3101",
  // The Lightning-Address domain the LSP serves (its LIGHTNING_ADDRESS_DOMAIN_URL host).
  lnAddressDomain: "127.0.0.1:8080",
};

const EXPECTED_BATCH_SIZE = 200; // ASYNC_ORDER_MAX_HASH_BATCH_SIZE
// Recipient A keysends this to the host so the host has outbound liquidity to later pay A's
// `request_invoice` invoice (the host→A leg of the async settlement).
const SEED_HOST_LIQUIDITY_MSAT = 50_000_000n;
// Amount node B pays to A's Lightning Address (LSP min/max sendable defaults to 3_000_000 msat).
const LN_ADDRESS_PAY_MSAT = 3_000_000;

const CHANNEL_READY_TIMEOUT_MS = 180_000;
const FUND_TIMEOUT_MS = 60_000;
const FETCH_TIMEOUT_MS = 15_000;
const RGB_FUNDING_STEP_TIMEOUT_MS = 30_000;
const PAYMENT_TIMEOUT_MS = 90_000;
// Time for the host to relay B's HTLC and ask A for an invoice (a few utexo-lsp cron cycles).
const REQUEST_INVOICE_TIMEOUT_MS = 120_000;
// Best-effort window for full settlement. On a stock node that blocks the host-side hodl-forward
// this will always elapse, so keep it short; a node that permits the forward settles well within it.
const SETTLE_TIMEOUT_MS = 45_000;

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
  return v === null || v === "" ? fallback : v;
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
  // The gateway exposes regtest mining only via /dev/regtest/fund (it mines `mine_blocks` after
  // sending a small amount). A tiny amount to our own address advances the chain harmlessly.
  const resp = await fetch(`${gatewayUrl}/dev/regtest/fund`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ address, amount_btc: 0.001, mine_blocks: count }),
  });
  if (!resp.ok) log("mineBlocks non-200", { status: resp.status });
}

async function fetchNativeInfo(nativeMgmtUrl) {
  const resp = await fetch(`${nativeMgmtUrl}/nodeinfo`, {
    signal: AbortSignal.timeout(FETCH_TIMEOUT_MS),
  });
  if (!resp.ok) throw new Error(`/nodeinfo failed: ${resp.status}`);
  const j = await resp.json();
  return { node_id: j.pubkey, ...j };
}

async function fundWallet(gatewayUrl, wallet, online, address) {
  log("Funding wallet on-chain...", { address });
  const resp = await fetch(`${gatewayUrl}/dev/regtest/fund`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ address, amount_btc: 1, mine_blocks: 6 }),
  });
  if (!resp.ok) throw new Error(`fund request failed: ${resp.status}`);
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

// Pull the LDK funding request for the just-opened channel, build a funding tx from the wallet's
// BTC, submit it back to LDK, and broadcast it to the indexer so mining confirms it.
// Wait until the native invoice-host reports us (`wasmPubkeyHex`) as a connected peer, so the LSP's
// reconcile cron targets us when it auto-opens a trusted virtual channel. Drives our event loop so
// the LN handshake completes.
async function waitForHostPeer(node, nativeMgmtUrl, wasmPubkeyHex, timeoutMs) {
  const deadline = Date.now() + timeoutMs;
  let iter = 0;
  while (Date.now() < deadline) {
    await node.chainSyncTickValue().catch(() => {}); // pump peer events → completes the LN handshake
    let hostPeers = [];
    try {
      const resp = await fetch(`${nativeMgmtUrl}/listpeers`, {
        signal: AbortSignal.timeout(FETCH_TIMEOUT_MS),
      });
      if (resp.ok) hostPeers = (await resp.json()).peers || [];
    } catch (_e) {
      /* keep polling */
    }
    if (hostPeers.some((p) => p.pubkey === wasmPubkeyHex)) return true;
    if (iter % 6 === 0) {
      log(`waitForHostPeer[${iter}]`, {
        hostSees: hostPeers.map((p) => (p.pubkey || "").slice(0, 12)),
      });
    }
    iter++;
    await sleep(500);
  }
  throw new Error(
    `native invoice-host did not see us (${wasmPubkeyHex.slice(0, 12)}) as a peer within ${timeoutMs}ms`
  );
}

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
      await withTimeout(node.driveRgbFundingWork(), RGB_FUNDING_STEP_TIMEOUT_MS, "driveRgbFundingWork");
    } catch (e) {
      if (String(e).includes("timed out")) throw e;
    }
    const found = node
      .listChannelsValue()
      .find((c) => c.peer_pubkey === peer && c.is_usable && !c.asset_id);
    if (found) return found;
    if (iter % 4 === 2) await mineBlocks(gatewayUrl, walletAddress, 3);
    if (iter % 5 === 0) {
      log("waiting for the LSP to open a usable virtual channel", node.listChannelsValue().map((c) => ({
        id: c.channel_id.slice(0, 12),
        status: c.status,
        usable: c.is_usable,
      })));
    }
    await sleep(2000);
    iter++;
  }
  throw new Error(`LSP virtual channel to ${peer.slice(0, 12)} not usable within ${timeoutMs}ms`);
}

// Compact view of an apay order response (the full `hashes` array is 200 entries — too noisy to log).
function orderSummary(resp) {
  return {
    order_id: resp.order_id,
    status: resp.status,
    first_hash_index: resp.first_hash_index,
    last_hash_index: resp.last_hash_index,
    next_index_expected: resp.next_index_expected,
    hashes_count: Array.isArray(resp.hashes) ? resp.hashes.length : 0,
  };
}

function assertOrderResponse(resp, { expectedFirstIndex, hostPubkey }) {
  assert(resp && typeof resp === "object", "apay response is not an object");
  assert(resp.status === "active", `unexpected order status: ${resp.status}`);
  assert(resp.host_node_id === hostPubkey, "response host_node_id mismatch");
  assert(Array.isArray(resp.hashes), "response.hashes is not an array");
  assert(resp.hashes.length === EXPECTED_BATCH_SIZE, `expected ${EXPECTED_BATCH_SIZE} hashes, got ${resp.hashes.length}`);
  assert(Number(resp.first_hash_index) === expectedFirstIndex, `first_hash_index ${resp.first_hash_index} != ${expectedFirstIndex}`);
  const expectedLast = expectedFirstIndex + EXPECTED_BATCH_SIZE - 1;
  assert(Number(resp.last_hash_index) === expectedLast, `last_hash_index ${resp.last_hash_index} != ${expectedLast}`);
  // The LSP commits through the highest accepted index and hints the next one.
  assert(Number(resp.accepted_through_index) === expectedLast, `accepted_through_index ${resp.accepted_through_index} != ${expectedLast}`);
  assert(Number(resp.next_index_expected) === expectedLast + 1, `next_index_expected ${resp.next_index_expected} != ${expectedLast + 1}`);
  // Every hash is 32-byte hex and indices are contiguous from first_hash_index.
  resp.hashes.forEach((h, i) => {
    assert(Number(h.hash_index) === expectedFirstIndex + i, `hash[${i}] index ${h.hash_index} not contiguous`);
    assert(/^[0-9a-f]{64}$/.test(h.payment_hash), `hash[${i}] payment_hash not 32-byte hex`);
  });
}

// Drive one node's peer/event loop a notch (chainSyncTick flushes peer frames → services inbound
// request_invoice on the recipient and advances HTLCs on the sender).
async function tickNode(node) {
  try {
    await node.driveRgbFundingWork();
  } catch (_e) {
    /* non-fatal */
  }
  try {
    await node.chainSyncTickValue();
  } catch (_e) {
    /* non-fatal */
  }
}

// Poll `check()` (returns a truthy value or null) while driving all `nodes`, until it yields a
// value or the timeout elapses. Returns the value, or null on timeout.
async function waitForCondition(check, nodes, timeoutMs, label) {
  const deadline = Date.now() + timeoutMs;
  let iter = 0;
  while (Date.now() < deadline) {
    for (const n of nodes) await tickNode(n);
    const v = check();
    if (v) return v;
    if (iter % 8 === 0) log(`waiting: ${label}`);
    await sleep(1000);
    iter++;
  }
  return null;
}

// Wait for `node`'s payment to settle, driving BOTH nodes each iteration: the sender advances its
// HTLC while the recipient services the host's request_invoice and auto-claims.
async function waitLivePaymentBothDriven(node, paymentHash, nodes, timeoutMs) {
  const deadline = Date.now() + timeoutMs;
  let iter = 0;
  while (Date.now() < deadline) {
    for (const n of nodes) await tickNode(n);
    const p = node.livePaymentValue(paymentHash);
    if (p && p.status === "succeeded") return p;
    if (p && p.status === "failed") throw new Error(`payment ${paymentHash} FAILED`);
    if (iter % 8 === 0) log("waiting payment settle", { paymentHash: paymentHash.slice(0, 12), status: p?.status });
    await sleep(1000);
    iter++;
  }
  throw new Error(`payment ${paymentHash} did not settle within ${timeoutMs}ms`);
}

// Resolve a recipient's Lightning Address via the LSP's LNURL-pay endpoints and return the BOLT11
// to pay. The LSP assigns its own handle per node pubkey (the client-supplied username is only an
// attestation), so we first resolve that handle by pubkey, then run LNURL-pay discovery.
async function resolveLnAddressInvoice(lnAddressDomain, recipientPubkey, amountMsat) {
  const base = lnAddressDomain.startsWith("http") ? lnAddressDomain : `http://${lnAddressDomain}`;
  const byResp = await fetch(`${base}/lightning_address/by_pubkey/${recipientPubkey}`, {
    signal: AbortSignal.timeout(FETCH_TIMEOUT_MS),
  });
  if (!byResp.ok) throw new Error(`by_pubkey lookup failed: ${byResp.status} ${await byResp.text().catch(() => "")}`);
  const { username } = await byResp.json();
  log("LSP-assigned Lightning Address handle", { recipientPubkey: recipientPubkey.slice(0, 16), username });
  const discoResp = await fetch(`${base}/.well-known/lnurlp/${encodeURIComponent(username)}`, {
    signal: AbortSignal.timeout(FETCH_TIMEOUT_MS),
  });
  if (!discoResp.ok) throw new Error(`lnurlp discovery failed: ${discoResp.status} ${await discoResp.text().catch(() => "")}`);
  const disco = await discoResp.json();
  log("LNURL-pay discovery", { callback: disco.callback, min: disco.minSendable, max: disco.maxSendable });
  if (!disco.callback) throw new Error("lnurlp discovery missing callback");
  const sep = disco.callback.includes("?") ? "&" : "?";
  const cbResp = await fetch(`${disco.callback}${sep}amount=${amountMsat}`, {
    signal: AbortSignal.timeout(FETCH_TIMEOUT_MS),
  });
  if (!cbResp.ok) throw new Error(`lnurlp callback failed: ${cbResp.status} ${await cbResp.text().catch(() => "")}`);
  const cb = await cbResp.json();
  if (!cb.pr) throw new Error(`lnurlp callback returned no invoice: ${JSON.stringify(cb).slice(0, 200)}`);
  log("LNURL-pay invoice", { pr: cb.pr.slice(0, 40), hasProof: !!cb.proof });
  return cb.pr;
}

// ---------------------------------------------------------------------------
// PHASE: run — async-payments-with-LSP registration
// ---------------------------------------------------------------------------

// Boot one wasm recipient node (its own SDK identity, wallet and runtime scope), fund it, connect
// to the shared native invoice-host and wait for the LSP to auto-open a trusted virtual channel to
// it. Returns the handles the registration step needs. `label` ("a"/"b") keeps the two recipients
// isolated: distinct runtime id (→ distinct persistence scope + node seed), distinct wallet keys.
async function provisionRecipient(cfg, runtimeId, label, hostPubkey) {
  const scopedRuntimeId = `${runtimeId}-${label}`;
  log(`[${label}] provisioning recipient`, { runtimeId: scopedRuntimeId });

  // The RlnWasmSdk identity is a page-global singleton (initialized once in runFlow). Each node's
  // LDK identity is still distinct because `node_signing_identity` hashes in the node_runtime_id,
  // so a shared SDK seed + a per-recipient runtime id yields two independent nodes.
  const keys = rgbGenerateKeysValue("regtest");
  const node = RlnWasmNode.newWithNodeRuntimeId(cfg.nodeProxyUrl, scopedRuntimeId, "Regtest");
  const myPubkey = JSON.parse(node.nodePubkeyJson());
  log(`[${label}] wasm node created`, myPubkey);

  const wallet = await RlnWasmWallet.create(
    JSON.stringify({
      data_dir: `/tmp/rln_wasm_apay_${scopedRuntimeId}`,
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
  // Accept the LSP's trusted virtual-channel opens (0-conf, scid-privacy, never-broadcast). Set before
  // attachWallet, which seeds the backend's flag registry (mirrors the RGB full-flow).
  node.setEnableVirtualChannelsV0(true);
  node.attachWallet(wallet);
  const walletAddress = wallet.getAddress();
  log(`[${label}] wallet online + attached`, { walletAddress });

  await fundWallet(cfg.gatewayUrl, wallet, online, walletAddress);
  node.chainSyncStartValue(cfg.esploraUrl, 3_600_000);

  await node.connectPeer(cfg.nativePeerAddr, hostPubkey);
  log(`[${label}] connected to native invoice-host`);

  // Re-read the pubkey now that the LDK runtime is up (connectPeer initializes the live backend):
  // before that nodePubkeyJson falls back to the signing-identity key, but the LSP peers/opens under
  // this real LDK node id (what apay_new signs with and what the host sees on the wire).
  const livePubkey = JSON.parse(node.nodePubkeyJson());
  const livePubkeyHex =
    typeof livePubkey === "string" ? livePubkey : livePubkey?.pubkey ?? livePubkey?.node_pubkey;
  log(`[${label}] live LDK node id`, { pubkey: livePubkeyHex });

  // apay_new requires a live channel with the host. Rather than opening a vanilla channel ourselves,
  // let the LSP auto-open a *trusted virtual* channel to us: its reconcile cron
  // (DEFAULT_VIRTUAL_OPEN_MODE=trusted_no_broadcast) opens a 0-conf, never-broadcast channel to every
  // peer the host is connected to, which we accept (setEnableVirtualChannelsV0 above) — no on-chain
  // funding by us. First make sure the host sees us as a peer so the cron targets us.
  await waitForHostPeer(node, cfg.nativeMgmtUrl, livePubkeyHex, CHANNEL_READY_TIMEOUT_MS);
  log(`[${label}] waiting for the LSP to auto-open a virtual channel to us...`);
  const channel = await waitForUsableChannel(node, hostPubkey, cfg.gatewayUrl, walletAddress, CHANNEL_READY_TIMEOUT_MS);
  log(`[${label}] ✅ virtual channel usable (LSP-opened)`, {
    id: channel.channel_id,
    capacity: channel.capacity_sat,
    virtual_open_mode: channel.virtual_open_mode,
  });

  return { label, node, myPubkey: livePubkeyHex, walletAddress };
}

// Register two batches with the LSP from one recipient: an address-attested first batch, then a
// plain refill, asserting the per-node KV cursor advanced (no hash reuse).
async function registerBatches(recipient, cfg, hostPubkey) {
  const { label, node } = recipient;
  const username = `wasm-${label}-${Date.now().toString(16)}`.slice(0, 32);

  log(`[${label}] STEP 1: apayNewWithAddress (first batch)...`, { username, domain: cfg.lnAddressDomain });
  const first = await withTimeout(
    node.apayNewWithAddressValue(hostPubkey, username, cfg.lnAddressDomain),
    60_000,
    `[${label}] apayNewWithAddress`
  );
  log(`[${label}] apayNewWithAddress response`, orderSummary(first));
  assertOrderResponse(first, { expectedFirstIndex: 1, hostPubkey });
  log(`[${label}] ✅ first batch registered`, {
    order_id: first.order_id,
    next_index_expected: first.next_index_expected,
  });

  log(`[${label}] STEP 2: apayNew (refill batch)...`);
  const second = await withTimeout(
    node.apayNewValue(hostPubkey),
    60_000,
    `[${label}] apayNew refill`
  );
  log(`[${label}] apayNew refill response`, orderSummary(second));
  assertOrderResponse(second, { expectedFirstIndex: Number(first.next_index_expected), hostPubkey });
  assert(
    Number(second.first_hash_index) === Number(first.next_index_expected),
    `[${label}] refill batch did not continue from the persisted cursor`
  );
  assert(
    Number(second.next_index_expected) > Number(first.next_index_expected),
    `[${label}] next_index_expected did not advance on refill`
  );
  log(`[${label}] ✅ refill continued from the persisted cursor`, {
    first_next: first.next_index_expected,
    second_first: second.first_hash_index,
  });

  return {
    label,
    username,
    recipient_node_id: JSON.parse(node.nodePubkeyJson()),
    order_id: first.order_id,
    first_batch: { first: first.first_hash_index, last: first.last_hash_index, next: first.next_index_expected },
    refill_batch: { first: second.first_hash_index, last: second.last_hash_index, next: second.next_index_expected },
  };
}

async function runFlow(cfg, runtimeId) {
  await init();
  log("WASM initialized", { runtimeId, phase: "run" });

  // The SDK is a page-global singleton; initialize it once for the whole page (both recipients).
  const sdkPassword = "apay-lsp-flow";
  const sdk = new RlnWasmSdk();
  await sdk.initValue(sdkPassword, rgbGenerateKeysValue("regtest").mnemonic);
  await sdk.unlock(JSON.stringify({ password: sdkPassword }));
  log("SDK initialized + unlocked");

  // Shared native invoice-host (both recipients open a channel to it and register through it).
  const nativeInfo = await fetchNativeInfo(cfg.nativeMgmtUrl);
  const hostPubkey = nativeInfo.node_id;
  log("Native invoice-host info", { node_id: hostPubkey });

  // Provision two independent wasm recipients sequentially (keeps regtest funding/mining simple),
  // then have each register its own batches with the LSP through the shared host.
  const recipientA = await provisionRecipient(cfg, runtimeId, "a", hostPubkey);
  const recipientB = await provisionRecipient(cfg, runtimeId, "b", hostPubkey);

  const resultA = await registerBatches(recipientA, cfg, hostPubkey);
  const resultB = await registerBatches(recipientB, cfg, hostPubkey);

  // The two recipients are distinct LN peers, so the LSP tracks them as separate orders with
  // independent hash cursors — both legitimately start at hash_index 1.
  const idA = JSON.stringify(recipientA.myPubkey);
  const idB = JSON.stringify(recipientB.myPubkey);
  assert(idA !== idB, "the two wasm recipients share a node id (not independent)");
  assert(
    String(resultA.order_id) !== String(resultB.order_id),
    "the LSP assigned the same order_id to two distinct recipients"
  );
  log("✅ two independent recipients registered with the LSP", {
    a: { order_id: resultA.order_id, username: resultA.username },
    b: { order_id: resultB.order_id, username: resultB.username },
  });

  // === STEP 3: node B pays node A's Lightning Address; the payment settles via the recipient
  //            (node A) answering the host's request_invoice and auto-claiming. ===
  // Seed host→A outbound liquidity first so the host can pay A's request_invoice invoice.
  log("STEP 3a: recipient A seeds host outbound liquidity (keysend)...");
  const seed = recipientA.node.keysendLiveValue(hostPubkey, SEED_HOST_LIQUIDITY_MSAT, null, null);
  await waitLivePaymentBothDriven(recipientA.node, seed.payment_hash, [recipientA.node], PAYMENT_TIMEOUT_MS);
  log("✅ host outbound liquidity toward A seeded");

  const recipientAPubkey =
    typeof recipientA.myPubkey === "string"
      ? recipientA.myPubkey
      : recipientA.myPubkey?.pubkey ?? recipientA.myPubkey?.node_pubkey;
  log("STEP 3b: node B resolves A's Lightning Address and pays it...", {
    recipient_pubkey: recipientAPubkey,
    amountMsat: LN_ADDRESS_PAY_MSAT,
  });
  const bolt11 = await resolveLnAddressInvoice(cfg.lnAddressDomain, recipientAPubkey, LN_ADDRESS_PAY_MSAT);
  const payment = recipientB.node.sendPaymentLiveValue(bolt11, null, null, null);
  log("payment initiated by B", payment);
  // The LN-address invoice reuses one of A's registered hashes, so B's payment hash IS the hash the
  // host will ask A to invoice. We use it to detect A's responder firing and to track settlement.
  const sharedHash = payment.payment_hash;

  // === HARD milestone: A's request_invoice responder mints an invoice for this payment ===
  // Once the host receives B's HTLC it asks A (async_order.request_invoice over msg 37915); A's
  // wasm backend re-derives the preimage and mints an inbound invoice for `sharedHash`. That insert
  // into A's live-payment ledger is the observable proof the recipient responder ran.
  log("STEP 3c: waiting for recipient A to service the host's request_invoice...");
  const aResponded = await waitForCondition(
    () => {
      const r = recipientA.node.livePaymentValue(sharedHash);
      return r && r.inbound ? r : null;
    },
    [recipientA.node, recipientB.node],
    REQUEST_INVOICE_TIMEOUT_MS,
    "recipient A request_invoice"
  );
  assert(aResponded, "recipient A never serviced the host's request_invoice (responder did not fire)");
  log("✅ STEP 3c — recipient A serviced request_invoice (responder minted the invoice)", {
    payment_hash: sharedHash,
    a_inbound_status: aResponded.status,
  });

  // === SOFT settlement: the final host→A forward + B settle. ===
  // This last leg needs the host node to pay an invoice whose hash it already holds as a HODL
  // inbound — utexo-lsp's intended single-node forward. A stock rgb-lightning-node blocks that with
  // `PaymentHashAlreadyUsed`, so settlement only completes when the node allows the hodl-forward.
  // We therefore treat full settlement as best-effort and do not fail the flow without it.
  log("STEP 3d: best-effort waiting for full B → A settlement...");
  let fullSettlement = { settled: false, status: payment.status };
  try {
    const settled = await waitLivePaymentBothDriven(
      recipientB.node,
      sharedHash,
      [recipientA.node, recipientB.node],
      SETTLE_TIMEOUT_MS
    );
    fullSettlement = { settled: settled.status === "succeeded", status: settled.status, preimage: settled.preimage };
    log("✅ STEP 3d — B → A Lightning-Address payment SETTLED end-to-end via the LSP", fullSettlement);
  } catch (e) {
    log(
      "ℹ️ STEP 3d — full settlement did not complete (expected on a stock node that blocks the " +
        "host-side hodl-forward with PaymentHashAlreadyUsed; the wasm responder already ran)",
      String(e)
    );
  }

  return {
    ok: true,
    request_invoice_serviced: true,
    full_settlement: fullSettlement.settled,
    settlement_status: fullSettlement.status,
    runtimeId,
    host_node_id: hostPubkey,
    recipients: [resultA, resultB],
    payment: { payment_hash: sharedHash, status: fullSettlement.status, amount_msat: LN_ADDRESS_PAY_MSAT },
  };
}

// ---------------------------------------------------------------------------
// entry
// ---------------------------------------------------------------------------

async function main() {
  const cfg = {
    nodeProxyUrl: readParam("nodeProxyUrl", DEFAULTS.nodeProxyUrl),
    esploraUrl: readParam("esploraUrl", DEFAULTS.esploraUrl),
    gatewayUrl: readParam("gatewayUrl", DEFAULTS.gatewayUrl),
    nativePeerAddr: readParam("nativePeerAddr", DEFAULTS.nativePeerAddr),
    nativeMgmtUrl: readParam("nativeMgmtUrl", DEFAULTS.nativeMgmtUrl),
    lnAddressDomain: readParam("lnAddressDomain", DEFAULTS.lnAddressDomain),
  };
  const runtimeId = readParam("runtimeId", `apay-${Date.now().toString(16)}`);
  log("config", cfg);

  try {
    const result = await runFlow(cfg, runtimeId);
    log("RESULT", result);
    window.__E2E_RESULT = result;
    window.__E2E_DONE = true;
  } catch (e) {
    const failure = { ok: false, error: String(e?.stack || e) };
    log("FAILED", failure);
    window.__E2E_RESULT = failure;
    window.__E2E_DONE = true;
  }
}

document.getElementById("run")?.addEventListener("click", () => {
  main();
});

if (new URLSearchParams(window.location.search).has("phase")) {
  main();
}
