import init, {
  rgbGenerateKeysJson,
  rgbRestoreKeysJson,
  rgbRestoreKeysValue,
  RlnWasmInvoice,
  checkProxyUrl,
  installPeerManagerHooksFromJs,
  clearPeerManagerHooks,
  hasPeerManagerHooks,
  RlnWasmRustPeerManagerBridge,
  RlnWasmNode,
} from "../../pkg/rln_wasm_sdk.js";

const DEFAULT_PROXY_URL = "ws://127.0.0.1:3001";
const DEFAULT_PEER_ADDR = "127.0.0.1:9746";
const DEFAULT_PEER_PUBKEY =
  "02399514a480a9b9d041651fd408c1483a2a3dff33a74a158dc948d120930fa011";
const DEMO_KEYSEND_MSAT = 3_000_000n;
const DEMO_PAYEE_PUBKEY_A =
  "03d860e19dac1741d2353d3953cd9f9d07f39c922cde0ca810f0aa33437bb81e23";
const DEMO_PAYEE_PUBKEY_B =
  "02399514a480a9b9d041651fd408c1483a2a3dff33a74a158dc948d120930fa011";

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

async function run() {
  await init();
  log("WASM initialized");

  const keysJson = rgbGenerateKeysJson("regtest");
  const keysFromJson = JSON.parse(keysJson);
  log("Generated keys (json)", keysFromJson);

  const restoredJson = rgbRestoreKeysJson("regtest", keysFromJson.mnemonic);
  const restored = JSON.parse(restoredJson);
  const restoredValue = rgbRestoreKeysValue("regtest", keysFromJson.mnemonic);
  log("Restored keys", restored);

  if (restored.xpub !== keysFromJson.xpub) {
    throw new Error("xpub mismatch between generated and restored keys");
  }
  if (restoredValue.xpub !== keysFromJson.xpub) {
    throw new Error("xpub mismatch between json/value restore flow");
  }
  log("Restore parity", { ok: true });

  try {
    // Expected failure: demonstrates stable error contract.
    new RlnWasmInvoice("");
  } catch (err) {
    log("Invoice parse expected error", String(err));
  }

  const proxyInput = document.getElementById("proxyUrl");
  const proxyUrl =
    proxyInput && proxyInput.value
      ? proxyInput.value.trim()
      : DEFAULT_PROXY_URL;
  const peerAddrInput = document.getElementById("peerAddr");
  const peerAddr =
    peerAddrInput && peerAddrInput.value
      ? peerAddrInput.value.trim()
      : DEFAULT_PEER_ADDR;
  const peerPubkeyInput = document.getElementById("peerPubkey");
  const peerPubkey =
    peerPubkeyInput && peerPubkeyInput.value
      ? peerPubkeyInput.value.trim()
      : DEFAULT_PEER_PUBKEY;
  log("Using defaults/runtime values", { proxyUrl, peerAddr, peerPubkey });

  clearPeerManagerHooks();
  installPeerManagerHooksFromJs(
    (peerPubkeyValue) => {
      log("hook.new_outbound_connection", { peerPubkey: peerPubkeyValue });
      // placeholder init bytes (hex), to be replaced by real LDK init bytes
      return "";
    },
    (payloadHex) => {
      log("hook.read_event", { payloadHex });
    },
    () => {
      log("hook.process_events", { ok: true });
    },
    () => {
      log("hook.socket_disconnected", { ok: true });
    },
    (message) => {
      log("hook.report_error", String(message));
    }
  );
  log("Peer manager hooks installed", { hasPeerManagerHooks: hasPeerManagerHooks() });

  try {
    await checkProxyUrl(proxyUrl);
    log("Proxy check", { ok: true, proxyUrl });
  } catch (err) {
    log("Proxy check failed", String(err));
  }

  try {
    const bridge = new RlnWasmRustPeerManagerBridge();
    const session = await bridge.connectSession(proxyUrl, peerAddr, peerPubkey);
    log("Bridge session created", { websocketUrl: session.websocketUrl() });
    log("Bridge stats (before start)", bridge.statsValue());

    try {
      await session.start();
      log("Bridge session started", { isStarted: session.isStarted() });
    } catch (err) {
      // expected if proxy or peer is unreachable
      log("Bridge session start failed (non-fatal)", String(err));
    }

    log("Bridge stats (after start attempt)", bridge.statsValue());
    await session.close().catch((err) => {
      log("Bridge session close warning", String(err));
    });
    log("Bridge session closed", { isStarted: session.isStarted() });
    log("Bridge stats (after close)", bridge.statsValue());
  } catch (err) {
    log("Bridge connect failed (non-fatal)", String(err));
  }
  const node = new RlnWasmNode(proxyUrl);
  log("Node created", { proxyUrl });
  const runtimeStatus = node.ldkRuntimeStatusValue();
  log("Runtime status", runtimeStatus);
  log("Runtime components (initial)", node.ldkRuntimeComponentsValue());

  try {
    node.chainSyncStartValue("http://127.0.0.1:3002", 5000);
    log("Chain sync started", node.chainSyncStatusValue());
    node.chainSyncStopValue();
    log("Chain sync stopped", node.chainSyncStatusValue());
  } catch (err) {
    log("Chain sync demo failed (non-fatal)", String(err));
  }

  const keysend = node.keysendValue(
    DEMO_PAYEE_PUBKEY_A,
    DEMO_KEYSEND_MSAT,
    undefined,
    undefined
  );
  log("Node keysend #1 (creates payment record)", keysend);

  const keysend2 = node.keysendValue(
    DEMO_PAYEE_PUBKEY_B,
    DEMO_KEYSEND_MSAT,
    undefined,
    undefined
  );
  log("Node keysend #2 (creates payment record)", keysend2);

  const keysend3 = node.keysendValue(
    DEMO_PAYEE_PUBKEY_A,
    DEMO_KEYSEND_MSAT,
    undefined,
    undefined
  );
  log("Node keysend #3 (creates payment record)", keysend3);
  const rgbKeysend = node.keysendValue(
    DEMO_PAYEE_PUBKEY_B,
    DEMO_KEYSEND_MSAT,
    "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    5n
  );
  log("Node keysend #4 RGB (creates RGB-LN transfer record)", rgbKeysend);
  log("Node payments (runtime-driven)", node.listPaymentsValue());
  log("RGB-LN transfers (runtime-driven)", node.listRgbLnTransfersValue());
  log("Manual status updates skipped", {
    reason:
      "wasm_native_ldk owns payment state transitions; statuses come from runtime event stream",
    backend: runtimeStatus && typeof runtimeStatus.backend === "string"
      ? runtimeStatus.backend
      : "unknown",
  });
  log("Runtime events (runtime-driven)", node.listRuntimeEventsValue());
  log("Runtime components (final)", node.ldkRuntimeComponentsValue());

  log("Example finished", { ok: true });
}

const runBtn = document.getElementById("run");
if (runBtn) {
  runBtn.addEventListener("click", () => {
    run().catch((err) => log("Fatal error", String(err)));
  });
}
