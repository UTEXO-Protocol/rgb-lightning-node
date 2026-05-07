"use strict";

const path = require("path");
const { test, expect } = require("@playwright/test");

const {
  snapshot,
  dumpSdkArtifacts,
  pumpRuntime,
} = require("./helpers/sdk");
const { RegtestController } = require("./helpers/regtest");
const { RegularRlnClient } = require("./helpers/regular-rln");
const flow = require("./helpers/flow");

/**
 * Page reload fault-injection.
 *
 * Mid-flow page reloads are a real failure mode for browser-based clients
 * (user closes the tab and comes back, OS sleeps the tab, etc.). We can't
 * yet test "channel survives reload" because the WASM SDK does not persist
 * `ChannelManager` snapshots — but we *can* test that:
 *
 *   - the harness boots cleanly twice in the same browser context,
 *   - the gateway is not wedged by the abandoned first session,
 *   - regular RLN tolerates the abandoned-then-resumed pattern,
 *   - a second, fresh run completes the full happy path end-to-end.
 *
 * This is the same property the legacy demo-page spec asserted, expressed
 * via the typed harness instead of by scraping `#out` text.
 */
test("WASM channel flow survives a mid-flow page reload", async ({ page }, testInfo) => {
  test.setTimeout(540_000);

  const browserConsoleErrors = [];
  page.on("console", (msg) => {
    if (msg.type() === "error") browserConsoleErrors.push(msg.text());
  });

  const regtest = new RegtestController();
  const regularRln = new RegularRlnClient();

  const regularPubkey = await test.step("regular RLN reachable", async () => {
    const info = await regularRln.nodeInfo();
    expect(info && typeof info.pubkey === "string").toBeTruthy();
    return info.pubkey;
  });

  const firstReady = await test.step("first boot: load harness", async () => {
    return flow.loadHarness(page, { freshRuntime: true });
  });
  expect(firstReady.pubkey).toMatch(/^[0-9a-f]{66}$/i);

  await test.step("first boot: get partway into the flow", async () => {
    // Fund the wallet, connect the peer, request channel open. This drives
    // the SDK far enough that LDK has emitted state and the gateway has an
    // active session — which is exactly what we want to be testing the
    // recovery from when the page reload kills the in-memory state.
    await flow.fundWasmWallet(page, regtest);
    await flow.openChannelAndWaitForPending(page, { regularPubkey });
    // Pump a few times so the in-flight messages actually leave the browser
    // before we yank the page out from under the runtime.
    for (let i = 0; i < 10; i += 1) {
      await pumpRuntime(page);
      await new Promise((r) => setTimeout(r, 50));
    }
  });

  await test.step("reload the page mid-flow", async () => {
    await page.reload();
  });

  const secondReady = await test.step("second boot: harness comes back up", async () => {
    // The harness page is configured with `?freshRuntime=1`, so the second
    // boot generates a new runtime id + mnemonic — exactly what we want for
    // an "abandoned → fresh start" semantic test.
    const { waitForSdkReady } = require("./helpers/sdk");
    return waitForSdkReady(page);
  });
  expect(secondReady.pubkey).toMatch(/^[0-9a-f]{66}$/i);
  expect(secondReady.pubkey).not.toBe(firstReady.pubkey);
  const wasmPubkey = secondReady.pubkey;

  const result = await test.step("second boot: full happy path completes", async () => {
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
