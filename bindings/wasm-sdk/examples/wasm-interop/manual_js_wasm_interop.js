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

function utf8ToHex(value) {
  const bytes = new TextEncoder().encode(value);
  return Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join("");
}

function applyReadEventPayload(node, payload, label) {
  const payloadHex = utf8ToHex(payload);
  const updated = node.ingestReadEventPayloadHex(payloadHex);
  log(label, { payload, updated });
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
  const proxyUrl = proxyInput && proxyInput.value ? proxyInput.value.trim() : "";
  const peerAddrInput = document.getElementById("peerAddr");
  const peerAddr =
    peerAddrInput && peerAddrInput.value ? peerAddrInput.value.trim() : "";
  const peerPubkeyInput = document.getElementById("peerPubkey");
  const peerPubkey =
    peerPubkeyInput && peerPubkeyInput.value
      ? peerPubkeyInput.value.trim()
      : "";

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

  if (proxyUrl.length > 0) {
    try {
      await checkProxyUrl(proxyUrl);
      log("Proxy check", { ok: true, proxyUrl });
    } catch (err) {
      log("Proxy check failed", String(err));
    }
  } else {
    log(
      "Proxy check skipped",
      "Set proxy URL in the input field to test checkProxyUrl"
    );
  }

  if (proxyUrl.length > 0 && peerAddr.length > 0 && peerPubkey.length > 0) {
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
  } else {
    log(
      "Bridge peer-session skipped",
      "Set proxy URL + peerAddr + peerPubkey to run the peer-session bootstrap path"
    );
  }

  const nodeProxyUrl = proxyUrl.length > 0 ? proxyUrl : "ws://127.0.0.1:3001";
  const node = new RlnWasmNode(nodeProxyUrl);
  log("Node created", { proxyUrl: nodeProxyUrl });

  const keysend = node.keysendValue(
    "02aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    3_000_000,
    undefined,
    undefined
  );
  log("Node keysend #1 (creates payment record)", keysend);

  const keysend2 = node.keysendValue(
    "02bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
    3_000_000,
    undefined,
    undefined
  );
  log("Node keysend #2 (creates payment record)", keysend2);

  const keysend3 = node.keysendValue(
    "02cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
    3_000_000,
    undefined,
    undefined
  );
  log("Node keysend #3 (creates payment record)", keysend3);
  log("Node payments (before callback payloads)", node.listPaymentsValue());

  applyReadEventPayload(
    node,
    JSON.stringify({
      payment_hash: keysend.payment_hash,
      status: "succeeded",
    }),
    "Callback payload applied (explicit status JSON)"
  );
  applyReadEventPayload(
    node,
    JSON.stringify({
      event: "PaymentFailed",
      payment_hash: keysend2.payment_hash,
    }),
    "Callback payload applied (event alias JSON)"
  );
  applyReadEventPayload(
    node,
    `payment_expired:${keysend3.payment_hash}`,
    "Callback payload applied (text alias)"
  );
  log("Node payments (after callback payloads)", node.listPaymentsValue());
  log("Runtime events (after callback payloads)", node.listRuntimeEventsValue());

  log("Example finished", { ok: true });
}

const runBtn = document.getElementById("run");
if (runBtn) {
  runBtn.addEventListener("click", () => {
    run().catch((err) => log("Fatal error", String(err)));
  });
}
