import init, { RlnWasmSdk } from "../../pkg/rln_wasm_sdk.js";

const NODE_A_PROXY_URL = "ws://node-a.runtime";
const NODE_B_PROXY_URL = "ws://node-b.runtime";

const NODE_A_PEER_ADDR = "127.0.0.1:9745";
const NODE_B_PEER_ADDR = "127.0.0.1:9746";

const NODE_A_PUBKEY =
  "03d860e19dac1741d2353d3953cd9f9d07f39c922cde0ca810f0aa33437bb81e23";
const NODE_B_PUBKEY =
  "02399514a480a9b9d041651fd408c1483a2a3dff33a74a158dc948d120930fa011";

const SDK_PASSWORD = "wasm-sdk-password";
const OPEN_CHANNEL_CAPACITY_SAT = 500_000n;
const KEYSEND_MSAT = 3_000_000n;
const CHANNEL_READY_TIMEOUT_MS = 8_000;
const PAYMENT_READY_TIMEOUT_MS = 8_000;

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

function assertCondition(condition, errorMessage) {
  if (!condition) {
    throw new Error(errorMessage);
  }
}

function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

async function waitForChannelUsable(nodeHandle, peerPubkey, timeoutMs) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const channels = nodeHandle.listChannelsValue();
    const found = channels.find((c) => c.peer_pubkey === peerPubkey);
    if (found && found.is_usable) {
      return found;
    }
    await sleep(200);
  }
  throw new Error("channel did not become usable in time");
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

async function runFlow() {
  await init();
  log("WASM package initialized");

  const sdk = new RlnWasmSdk();

  const initData = await sdk.initValue(SDK_PASSWORD, undefined);
  log("SDK initialized", initData);

  await sdk.unlock(JSON.stringify({ password: SDK_PASSWORD }));
  log("SDK unlocked");

  const nodeA = sdk.createNodeHandleWithRuntimeBackend(
    NODE_A_PROXY_URL,
    "ldk_bridge"
  );
  const nodeB = sdk.createNodeHandleWithRuntimeBackend(
    NODE_B_PROXY_URL,
    "ldk_bridge"
  );
  log("Node handles created", {
    nodeAProxy: NODE_A_PROXY_URL,
    nodeBProxy: NODE_B_PROXY_URL,
  });

  await nodeA.connectPeer(NODE_B_PEER_ADDR, NODE_B_PUBKEY);
  await nodeB.connectPeer(NODE_A_PEER_ADDR, NODE_A_PUBKEY);
  log("Peers connected");

  const opened = nodeA.openChannelValue(
    NODE_B_PUBKEY,
    OPEN_CHANNEL_CAPACITY_SAT,
    false,
    undefined,
    undefined
  );
  log("Channel open requested", opened);

  const channel = await waitForChannelUsable(
    nodeA,
    NODE_B_PUBKEY,
    CHANNEL_READY_TIMEOUT_MS
  );
  log("Channel is usable", channel);

  const keysendAB = nodeA.keysendValue(
    NODE_B_PUBKEY,
    KEYSEND_MSAT,
    undefined,
    undefined
  );
  log("A -> B keysend created", keysendAB);
  nodeA.updatePaymentStatus(keysendAB.payment_hash, "succeeded");
  const keysendABFinal = await waitForPaymentStatus(
    nodeA,
    keysendAB.payment_hash,
    "succeeded",
    PAYMENT_READY_TIMEOUT_MS
  );
  log("A -> B keysend finalized", keysendABFinal);

  const keysendBA = nodeB.keysendValue(
    NODE_A_PUBKEY,
    KEYSEND_MSAT,
    undefined,
    undefined
  );
  log("B -> A drain keysend created", keysendBA);
  nodeB.updatePaymentStatus(keysendBA.payment_hash, "succeeded");
  const keysendBAFinal = await waitForPaymentStatus(
    nodeB,
    keysendBA.payment_hash,
    "succeeded",
    PAYMENT_READY_TIMEOUT_MS
  );
  log("B -> A drain keysend finalized", keysendBAFinal);

  nodeA.closeChannel(channel.channel_id);
  await waitForChannelGone(nodeA, channel.channel_id, CHANNEL_READY_TIMEOUT_MS);
  log("Channel closed and removed on node A", { channel_id: channel.channel_id });

  const nodeAChannels = nodeA.listChannelsValue();
  const nodeBChannels = nodeB.listChannelsValue();
  assertCondition(
    Array.isArray(nodeAChannels) && nodeAChannels.length === 0,
    "node A should have no channels after close"
  );
  log("Final channels", { nodeA: nodeAChannels, nodeB: nodeBChannels });

  log("Runtime events node A", nodeA.listRuntimeEventsValue());
  log("Runtime events node B", nodeB.listRuntimeEventsValue());
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
