"use strict";

const path = require("path");
const { test, expect } = require("@playwright/test");

const {
  snapshot,
  dumpSdkArtifacts,
} = require("./helpers/sdk");
const { RegtestController } = require("./helpers/regtest");
const { RegularRlnClient } = require("./helpers/regular-rln");
const flow = require("./helpers/flow");

/**
 * WASM SDK E2E — native channel happy path.
 *
 * The test owns the whole flow end-to-end:
 *   - drives the SDK directly via `window.__sdk.{node, wallet, online}`,
 *   - funds the BDK wallet via Node-side bitcoind RPC (no gateway
 *     `/dev/regtest/*` involvement),
 *   - has the WASM SDK's own BDK wallet build + sign the channel funding
 *     transaction (`buildLightningFundingTxValue`) so the smoke spec
 *     exercises the same code path real WASM clients will use,
 *   - asserts on typed JSON state for channels and payments.
 *
 * The actual flow lives in `helpers/flow.js`; this spec is intentionally
 * thin so fault-injection specs can compose the same primitives.
 */
test("WASM opens native channel to regular RLN and pays over it", async ({ page }, testInfo) => {
  test.setTimeout(420_000);

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

  const sdkReady = await test.step("load harness and wait for SDK ready", async () => {
    return flow.loadHarness(page);
  });
  expect(sdkReady && sdkReady.pubkey).toMatch(/^[0-9a-f]{66}$/i);
  const wasmPubkey = sdkReady.pubkey;

  const result = await test.step("run full happy path (fund → open → pay)", async () => {
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
  expect(result.readyChannel.capacity_sat).toBe(flow.DEFAULTS.capacitySat);
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
