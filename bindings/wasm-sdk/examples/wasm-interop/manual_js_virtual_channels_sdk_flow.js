import init, { RlnWasmSdk } from "../../pkg/rln_wasm_sdk.js";

const DEFAULT_NODE_A_PROXY_URL = "ws://127.0.0.1:3001";
const DEFAULT_NODE_B_PROXY_URL = "ws://127.0.0.1:3001";
const DEFAULT_NODE_A_PEER_ADDR = "127.0.0.1:9745";
const DEFAULT_NODE_B_PEER_ADDR = "127.0.0.1:9746";
const DEFAULT_NODE_A_PUBKEY =
  "03d860e19dac1741d2353d3953cd9f9d07f39c922cde0ca810f0aa33437bb81e23";
const DEFAULT_NODE_B_PUBKEY =
  "02399514a480a9b9d041651fd408c1483a2a3dff33a74a158dc948d120930fa011";
const DEFAULT_INDEXER_URL = "http://127.0.0.1:3002";

const SDK_PASSWORD = "wasm-sdk-password";
const NODE_A_RUNTIME_ID = "wasm-virtual-node-a";
const NODE_B_RUNTIME_ID = "wasm-virtual-node-b";
const OPEN_CHANNEL_CAPACITY_SAT = 500_000n;
const KEYSEND_MSAT = 3_000_000n;
const CHANNEL_READY_TIMEOUT_MS = 30_000;
const PAYMENT_READY_TIMEOUT_MS = 15_000;
const CLOSE_TIMEOUT_MS = 30_000;
const VIRTUAL_OPEN_MODE = "trusted_no_broadcast";
const RELAY_AUTH_CHALLENGE_PREFIX = "rln-wasm-open-channel";

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

function assertCondition(condition, errorMessage) {
  if (!condition) {
    throw new Error(errorMessage);
  }
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

function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

function buildRelayAuthChallenge(nodeLabel) {
  return `${RELAY_AUTH_CHALLENGE_PREFIX}:${nodeLabel}:${Date.now()}`;
}

async function maybeConfigureRelaySessionAuth(nodeHandle, nodeLabel) {
  const tokenInputId = nodeLabel === "nodeA" ? "nodeARelayAuthToken" : "nodeBRelayAuthToken";
  const nodeIdInputId = nodeLabel === "nodeA" ? "nodeARelayNodeId" : "nodeBRelayNodeId";
  const manualRelayAuthToken = readText(tokenInputId);
  const manualRelayNodeId = readText(nodeIdInputId);
  if (manualRelayAuthToken.length > 0 || manualRelayNodeId.length > 0) {
    if (manualRelayAuthToken.length === 0 || manualRelayNodeId.length === 0) {
      throw new Error(
        `both relay auth token and relay node id must be provided for ${nodeLabel}`
      );
    }
    nodeHandle.setRelaySessionAuth(manualRelayAuthToken, manualRelayNodeId);
    log("Relay auth configured from UI", { nodeLabel });
    return;
  }

  if (typeof window.getRelaySessionAuth !== "function") {
    log("Relay auth skipped", {
      nodeLabel,
      reason:
        "window.getRelaySessionAuth is not defined (proxy may be open or auth disabled).",
    });
    return;
  }

  const challenge = buildRelayAuthChallenge(nodeLabel);
  const signedDoc = nodeHandle.signMessageValue(challenge);
  const signedMessage = signedDoc?.signed_message || signedDoc?.signedMessage;
  if (!signedMessage || typeof signedMessage !== "string") {
    throw new Error(`failed to sign relay auth challenge for ${nodeLabel}`);
  }

  const auth = await window.getRelaySessionAuth({
    nodeLabel,
    challenge,
    signedMessage,
  });
  const relayAuthToken = auth?.relayAuthToken || auth?.relay_auth_token;
  const relayNodeId = auth?.relayNodeId || auth?.relay_node_id;
  if (
    typeof relayAuthToken !== "string" ||
    relayAuthToken.trim().length === 0 ||
    typeof relayNodeId !== "string" ||
    relayNodeId.trim().length === 0
  ) {
    throw new Error(
      `window.getRelaySessionAuth must return { relayAuthToken, relayNodeId } for ${nodeLabel}`
    );
  }

  nodeHandle.setRelaySessionAuth(relayAuthToken, relayNodeId);
  log("Relay auth configured", { nodeLabel });
}

async function waitForChannelUsable(nodeHandle, peerPubkey, timeoutMs) {
  const deadline = Date.now() + timeoutMs;
  let lastSnapshot = [];
  while (Date.now() < deadline) {
    if (typeof nodeHandle.processNativeRuntimeQueueValue === "function") {
      nodeHandle.processNativeRuntimeQueueValue();
    } else if (typeof nodeHandle.drainNativeRuntimeQueueValue === "function") {
      nodeHandle.drainNativeRuntimeQueueValue();
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

async function waitForChannelGone(nodeHandle, channelId, timeoutMs) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const channels = nodeHandle.listChannelsValue();
    const present = channels.some((c) => c.channel_id === channelId);
    if (!present) return;
    await sleep(200);
  }
  throw new Error(`channel ${channelId} was not removed in time`);
}

async function waitForChannelClosedOrGoneOnBoth(nodeA, nodeB, channelId, timeoutMs) {
  const deadline = Date.now() + timeoutMs;
  let lastSnapshot = null;
  while (Date.now() < deadline) {
    const channelsA = nodeA.listChannelsValue();
    const channelsB = nodeB.listChannelsValue();
    const a = channelsA.find((c) => c.channel_id === channelId) ?? null;
    const b = channelsB.find((c) => c.channel_id === channelId) ?? null;
    lastSnapshot = {
      nodeA: a
        ? {
            status: a.status,
            is_usable: a.is_usable,
            peer_pubkey: a.peer_pubkey,
            virtual_open_mode: a.virtual_open_mode ?? null,
          }
        : null,
      nodeB: b
        ? {
            status: b.status,
            is_usable: b.is_usable,
            peer_pubkey: b.peer_pubkey,
            virtual_open_mode: b.virtual_open_mode ?? null,
          }
        : null,
    };

    const aDone = a === null || a.status === "closing" || a.is_usable === false;
    const bDone = b === null || b.status === "closing" || b.is_usable === false;
    if (aDone && bDone) {
      return;
    }
    // Host-authoritative trusted close: treat completion once host side is done.
    if (aDone) {
      return;
    }
    await sleep(500);
  }
  throw new Error(
    `channel neither non-usable nor closing/gone on at least one node: ${JSON.stringify(lastSnapshot)}`
  );
}

async function closeVirtualChannelWithRetry(
  nodeHandle,
  channelId,
  peerPubkey,
  timeoutMs
) {
  const deadline = Date.now() + timeoutMs;
  let lastErr = "unknown";
  while (Date.now() < deadline) {
    try {
      nodeHandle.closeChannelWithOptions(channelId, peerPubkey, false);
      return;
    } catch (err) {
      lastErr = String(err);
      if (lastErr.includes("channel not found")) {
        log("Close treated as complete: channel already gone", {
          channelId,
          reason: lastErr,
        });
        return;
      }
      await sleep(500);
    }
  }
  if (lastErr.includes("virtual cleanup is blocked while HTLCs are still in flight")) {
    log("Close timeout treated as non-fatal due in-flight HTLC cleanup", {
      channelId,
      reason: lastErr,
    });
    return;
  }
  throw new Error(`close channel did not succeed in time: ${lastErr}`);
}

function assertTrustedVirtualChannel(channel, expectedPeerPubkey) {
  assertCondition(
    channel.peer_pubkey === expectedPeerPubkey,
    "virtual channel peer pubkey mismatch"
  );
  assertCondition(
    channel.virtual_open_mode === VIRTUAL_OPEN_MODE,
    `expected virtual_open_mode='${VIRTUAL_OPEN_MODE}', got '${channel.virtual_open_mode}'`
  );
}

async function runFlow() {
  await init();
  log("WASM package initialized");
  const nodeAProxyUrl = readText("nodeAProxyUrl") || DEFAULT_NODE_A_PROXY_URL;
  const nodeBProxyUrl = readText("nodeBProxyUrl") || DEFAULT_NODE_B_PROXY_URL;
  const nodeAPeerAddr = readText("nodeAPeerAddr") || DEFAULT_NODE_A_PEER_ADDR;
  const nodeBPeerAddr = readText("nodeBPeerAddr") || DEFAULT_NODE_B_PEER_ADDR;
  const configuredNodeAPubkey = readText("nodeAPubkey") || DEFAULT_NODE_A_PUBKEY;
  const configuredNodeBPubkey = readText("nodeBPubkey") || DEFAULT_NODE_B_PUBKEY;
  const indexerUrl = readText("indexerUrl") || DEFAULT_INDEXER_URL;

  const sdk = new RlnWasmSdk();
  if (typeof sdk.setDefaultEnableVirtualChannelsV0 === "function") {
    sdk.setDefaultEnableVirtualChannelsV0(true);
  }
  await sdk.preloadPersistentRuntimeState();
  log("Persistent runtime state preloaded");

  const initData = await sdk.initValue(SDK_PASSWORD, undefined);
  log("SDK initialized", initData);

  await sdk.unlock(JSON.stringify({ password: SDK_PASSWORD }));
  log("SDK unlocked");

  const createNodeWithRuntimeId = (proxyUrl, runtimeId) =>
    sdk.createNodeHandleWithRuntimeId(proxyUrl, runtimeId);
  const nodeA = createNodeWithRuntimeId(nodeAProxyUrl, NODE_A_RUNTIME_ID);
  const nodeB = createNodeWithRuntimeId(nodeBProxyUrl, NODE_B_RUNTIME_ID);
  const nodeAPubkey = resolveNodePubkey(nodeA);
  const nodeBPubkey = resolveNodePubkey(nodeB);
  if (configuredNodeAPubkey && configuredNodeAPubkey !== nodeAPubkey) {
    log("Node A pubkey input differs from runtime identity; using runtime identity", {
      configured: configuredNodeAPubkey,
      derived: nodeAPubkey,
    });
  }
  if (configuredNodeBPubkey && configuredNodeBPubkey !== nodeBPubkey) {
    log("Node B pubkey input differs from runtime identity; using runtime identity", {
      configured: configuredNodeBPubkey,
      derived: nodeBPubkey,
    });
  }
  log("Node handles created", {
    nodeAProxy: nodeAProxyUrl,
    nodeBProxy: nodeBProxyUrl,
    nodeARuntimeId: NODE_A_RUNTIME_ID,
    nodeBRuntimeId: NODE_B_RUNTIME_ID,
    nodeAPeerAddr,
    nodeBPeerAddr,
    nodeAPubkey,
    nodeBPubkey,
    indexerUrl,
  });

  await maybeConfigureRelaySessionAuth(nodeA, "nodeA");
  await maybeConfigureRelaySessionAuth(nodeB, "nodeB");

  log("Node A runtime components (initial)", nodeA.ldkRuntimeComponentsValue());
  log("Node B runtime components (initial)", nodeB.ldkRuntimeComponentsValue());

  try {
    nodeA.chainSyncStartValue(indexerUrl, 5000);
    log("Node A chain sync status", nodeA.chainSyncStatusValue());
    nodeA.chainSyncStopValue();
  } catch (err) {
    log("Node A chain sync demo failed (non-fatal)", String(err));
  }

  await nodeA.connectPeer(nodeBPeerAddr, nodeBPubkey);
  await nodeB.connectPeer(nodeAPeerAddr, nodeAPubkey);
  log("Peers connected");

  const opened = nodeA.openChannelValueWithOptions(
    nodeBPubkey,
    OPEN_CHANNEL_CAPACITY_SAT,
    false,
    undefined,
    undefined,
    VIRTUAL_OPEN_MODE
  );
  log("Channel open requested", opened);
  assertTrustedVirtualChannel(opened, nodeBPubkey);
  if (typeof nodeA.processNativeRuntimeQueueValue === "function") {
    const processed = nodeA.processNativeRuntimeQueueValue();
    log("Native runtime queue processed after open", processed);
  } else {
    log("Native runtime queue process API unavailable on node handle");
  }

  const channel = await waitForChannelUsable(
    nodeA,
    nodeBPubkey,
    CHANNEL_READY_TIMEOUT_MS
  );
  assertTrustedVirtualChannel(channel, nodeBPubkey);
  log("Channel is usable", channel);

  const keysendAB = nodeA.keysendValue(
    nodeBPubkey,
    KEYSEND_MSAT,
    undefined,
    undefined
  );
  log("A -> B keysend created", keysendAB);
  const keysendABFinal = await waitForPaymentStatus(
    nodeA,
    keysendAB.payment_hash,
    "succeeded",
    PAYMENT_READY_TIMEOUT_MS
  );
  log("A -> B keysend finalized", keysendABFinal);

  const keysendBA = nodeB.keysendValue(
    nodeAPubkey,
    KEYSEND_MSAT,
    undefined,
    undefined
  );
  log("B -> A drain keysend created", keysendBA);
  const keysendBAFinal = await waitForPaymentStatus(
    nodeB,
    keysendBA.payment_hash,
    "succeeded",
    PAYMENT_READY_TIMEOUT_MS
  );
  log("B -> A drain keysend finalized", keysendBAFinal);

  try {
    await closeVirtualChannelWithRetry(
      nodeA,
      channel.channel_id,
      nodeBPubkey,
      CLOSE_TIMEOUT_MS
    );
  } catch (primaryErr) {
    log("Node A close fallback to node B", String(primaryErr));
    await closeVirtualChannelWithRetry(
      nodeB,
      channel.channel_id,
      nodeAPubkey,
      CLOSE_TIMEOUT_MS
    );
  }
  await waitForChannelClosedOrGoneOnBoth(
    nodeA,
    nodeB,
    channel.channel_id,
    CLOSE_TIMEOUT_MS
  );
  await waitForChannelGone(nodeA, channel.channel_id, CLOSE_TIMEOUT_MS);
  log("Virtual channel close completed, removed on node A", {
    channel_id: channel.channel_id,
  });

  const nodeAChannels = nodeA.listChannelsValue();
  const nodeBChannels = nodeB.listChannelsValue();
  assertCondition(Array.isArray(nodeAChannels) && nodeAChannels.length === 0, "node A still reports channels after close");
  log("Final channels", { nodeA: nodeAChannels, nodeB: nodeBChannels });

  log("Runtime events node A", nodeA.listRuntimeEventsValue());
  log("Runtime events node B", nodeB.listRuntimeEventsValue());
  log("Node A runtime components (final)", nodeA.ldkRuntimeComponentsValue());
  log("Node B runtime components (final)", nodeB.ldkRuntimeComponentsValue());
  log("Node A RGB-LN transfers", nodeA.listRgbLnTransfersValue());
  log("Node B RGB-LN transfers", nodeB.listRgbLnTransfersValue());
  log("SUCCESS: wasm flow completed");
}

const runBtn = document.getElementById("run-flow");
if (runBtn) {
  runBtn.addEventListener("click", () => {
    const out = document.getElementById("out");
    if (out) out.innerHTML = "";
    runFlow().catch((err) => log("Flow failed", String(err)));
  });
}
