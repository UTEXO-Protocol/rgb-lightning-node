"use strict";

const path = require("path");
const { test, expect } = require("@playwright/test");

const {
  snapshot,
  dumpSdkArtifacts,
  pumpRuntime,
  waitForChannelReadyToPeer,
} = require("./helpers/sdk");
const { RegtestController } = require("./helpers/regtest");
const { RegularRlnClient } = require("./helpers/regular-rln");
const flow = require("./helpers/flow");

/**
 * Proxy WebSocket fault-injection.
 *
 * The LN proxy WebSocket between the browser SDK and the gateway is a real
 * failure mode in production (mobile network changes, proxy restarts, …).
 * The SDK has built-in reconnect (`ln_transport.rs::try_reconnect_socket`,
 * 3 attempts by default with 250ms→4s backoff).
 *
 * Important LN protocol caveat we have to honor here:
 *
 *   The gateway's WS↔TCP relay closes the upstream TCP socket to the LN
 *   counterparty whenever the browser-side WS dies. From the counterparty's
 *   point of view this is a `DisconnectedPeer` event. For channels that
 *   are *mid-handshake* (Accepted inbound → FundingCreated not yet seen),
 *   LDK on the counterparty correctly aborts the channel — that's
 *   protocol-correct, not a recovery bug. The reconnect-path test we can
 *   actually write is therefore "yank the WS *after* the channel is fully
 *   open and assert the SDK reconnects without losing channel state".
 *
 * What this spec asserts:
 *   1. Full happy path completes (channel ready + first keysend).
 *   2. We yank the LN proxy WS.
 *   3. The SDK observes the close and the auto-reconnect path opens a
 *      fresh socket against the gateway (≥ 1 new socket capture).
 *   4. After reconnect, the channel still appears `ready` / `is_usable`
 *      from `listChannelsJson()` — i.e. LDK's in-memory channel state
 *      survived the proxy churn.
 */
test("WASM channel survives proxy WebSocket disconnect after open", async ({ page }, testInfo) => {
  test.setTimeout(540_000);

  const browserConsoleErrors = [];
  page.on("console", (msg) => {
    if (msg.type() === "error") browserConsoleErrors.push(msg.text());
  });

  // Capture every LN proxy WebSocket the SDK opens. After the first close,
  // the SDK reconnects and a fresh `WebSocketRoute` is captured here, so the
  // variable always points at the *current* live socket (not a dangling
  // closed one).
  let lnProxyRoute = null;
  let lnProxySocketCount = 0;
  await page.routeWebSocket("ws://127.0.0.1:3001/**", (route) => {
    const url = route.url();
    if (url.includes("/v1/") || url.includes("/ln/v1/")) {
      lnProxyRoute = route;
      lnProxySocketCount += 1;
    }
    route.connectToServer();
  });

  const regtest = new RegtestController();
  const regularRln = new RegularRlnClient();

  const regularPubkey = await test.step("regular RLN reachable", async () => {
    const info = await regularRln.nodeInfo();
    expect(info && typeof info.pubkey === "string").toBeTruthy();
    return info.pubkey;
  });

  const sdkReady = await test.step("load harness and wait for SDK ready", async () => {
    return flow.loadHarness(page);
  });
  expect(sdkReady.pubkey).toMatch(/^[0-9a-f]{66}$/i);
  const wasmPubkey = sdkReady.pubkey;

  const result = await test.step("run full happy path before fault injection", async () => {
    return flow.runFullHappyPath(page, {
      regtest,
      regularRln,
      regularPubkey,
      wasmPubkey,
    });
  });
  expect(result.readyChannel.peer_pubkey.toLowerCase()).toBe(
    regularPubkey.toLowerCase()
  );
  expect(result.payment && result.payment.status).toBe("succeeded");

  await test.step("LN proxy WebSocket has been opened", async () => {
    await expect.poll(() => lnProxyRoute !== null, { timeout: 60_000 }).toBeTruthy();
  });
  const socketsBeforeClose = lnProxySocketCount;

  await test.step("yank the LN proxy WebSocket", async () => {
    await lnProxyRoute.close();
  });

  await test.step("SDK reconnect kicks in", async () => {
    // After close, `try_reconnect_socket` reopens a new WS, which our route
    // handler captures and increments `lnProxySocketCount`. Pump the runtime
    // so any reconnect timers fire promptly.
    await expect
      .poll(
        async () => {
          await pumpRuntime(page);
          return lnProxySocketCount;
        },
        { timeout: 60_000, intervals: [250, 500, 1000] }
      )
      .toBeGreaterThan(socketsBeforeClose);
  });

  await test.step("channel survives the WS bounce (is_usable on WASM side)", async () => {
    // Pump for a beat so PeerManager's read_event drains any pending
    // channel_reestablish noise after reconnect, then re-poll until the
    // channel is reported back as usable on our side. We *don't* require
    // the regular RLN side to be re-listed via REST here because the
    // regular RLN side may take longer to re-emit "is_usable" via its
    // public REST view.
    for (let i = 0; i < 10; i += 1) {
      await pumpRuntime(page);
      await new Promise((r) => setTimeout(r, 100));
    }
    const ch = await waitForChannelReadyToPeer(page, regularPubkey, {
      timeoutMs: 60_000,
    });
    expect(ch).toBeTruthy();
    expect(ch.peer_pubkey.toLowerCase()).toBe(regularPubkey.toLowerCase());
    expect(ch.capacity_sat).toBe(flow.DEFAULTS.capacitySat);
  });

  const artifactDir = path.join(testInfo.outputDir, "sdk-snapshot-success");
  await dumpSdkArtifacts(page, artifactDir, "final");

  if (browserConsoleErrors.length) {
    test.info().annotations.push({
      type: "browser-console-errors",
      description: browserConsoleErrors.slice(0, 20).join("\n"),
    });
  }
});

test.afterEach(async ({ page }, testInfo) => {
  if (testInfo.status !== testInfo.expectedStatus) {
    const dir = path.join(testInfo.outputDir, "sdk-snapshot-failure");
    try {
      await dumpSdkArtifacts(page, dir, "failure");
    } catch (_e) {
      /* best-effort */
    }
    try {
      const snap = await snapshot(page);
      // eslint-disable-next-line no-console
      console.log("[e2e] failure snapshot:", JSON.stringify(snap, null, 2));
    } catch (_e) {
      /* best-effort */
    }
  }
});
