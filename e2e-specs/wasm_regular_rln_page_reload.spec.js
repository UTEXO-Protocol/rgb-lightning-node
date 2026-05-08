"use strict";

const path = require("path");
const { test, expect } = require("@playwright/test");

const {
  snapshot,
  dumpSdkArtifacts,
  pumpRuntime,
  buildLightningFundingTx,
  waitForChannelReadyToPeer,
  waitForChannelUsableToPeer,
  waitForSdkReady,
  submitKeysend,
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
 *   - abandoned mid-flow sessions do not wedge the gateway,
 *   - same-runtime reloads restore LDK state when persistence exists,
 *   - browser restart-style boots can hydrate runtime state from IndexedDB.
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

async function getHarnessIdentity(page) {
  return page.evaluate(() => ({
    runtimeId: window.__sdk && window.__sdk.runtimeId,
    mnemonic: window.__sdk && window.__sdk.mnemonic,
    pubkey:
      window.__sdk && window.__sdk.node
        ? JSON.parse(window.__sdk.node.nodePubkeyJson()).pubkey
        : null,
  }));
}

async function reconnectPersistedPeers(page) {
  await page.evaluate(async () => {
    await window.__sdk.node.reconnectPersistedPeersValue();
  });
}

async function waitForIndexedDbRuntimeEntries(page, opts = {}) {
  const timeoutMs = opts.timeoutMs || 60_000;
  const deadline = Date.now() + timeoutMs;
  for (;;) {
    const entries = await page.evaluate(async () => {
      const db = await new Promise((resolve, reject) => {
        const req = indexedDB.open("rln_wasm_sdk_runtime", 1);
        req.onupgradeneeded = () => {
          const opened = req.result;
          if (!opened.objectStoreNames.contains("state")) {
            opened.createObjectStore("state");
          }
        };
        req.onerror = () => reject(req.error || new Error("indexedDB open failed"));
        req.onsuccess = () => resolve(req.result);
      });
      try {
        return await new Promise((resolve, reject) => {
          const tx = db.transaction("state", "readonly");
          const store = tx.objectStore("state");
          const keysReq = store.getAllKeys();
          keysReq.onerror = () => reject(keysReq.error || new Error("indexedDB keys failed"));
          tx.oncomplete = () => resolve((keysReq.result || []).map(String));
          tx.onerror = () => reject(tx.error || new Error("indexedDB tx failed"));
          tx.onabort = () => reject(tx.error || new Error("indexedDB tx aborted"));
        });
      } finally {
        db.close();
      }
    });
    const matching = entries.filter(
      (key) =>
        key.startsWith("rln:wasm:ldk-runtime:") ||
        key.startsWith("rln:wasm:ldk-monitors:") ||
        key.startsWith("rln:wasm:chain-sync:") ||
        key.startsWith("rln:wasm:peer-sessions:")
    );
    if (matching.length > 0) return matching;
    if (Date.now() >= deadline) {
      throw new Error("timed out waiting for runtime IndexedDB entries");
    }
    await new Promise((r) => setTimeout(r, 250));
  }
}

async function clearBrowserVolatileRuntimeStorage(page) {
  await page.evaluate(() => {
    localStorage.clear();
    sessionStorage.clear();
  });
}

test("WASM funding confirmation drives ChannelReady after same-runtime reload", async ({ page }, testInfo) => {
  test.setTimeout(540_000);

  const regtest = new RegtestController();
  const regularRln = new RegularRlnClient();

  const regularPubkey = await test.step("regular RLN reachable", async () => {
    const info = await regularRln.nodeInfo();
    expect(info && typeof info.pubkey === "string").toBeTruthy();
    return info.pubkey;
  });

  const firstReady = await test.step("first boot: load stable harness", async () => {
    return flow.loadHarness(page);
  });
  expect(firstReady.pubkey).toMatch(/^[0-9a-f]{66}$/i);
  const wasmPubkey = firstReady.pubkey;

  const pending = await test.step("open channel and submit funding tx", async () => {
    await flow.fundWasmWallet(page, regtest);
    const pendingFunding = await flow.openChannelAndWaitForPending(page, {
      regularPubkey,
    });
    const funding = await buildLightningFundingTx(page, {
      outputScriptHex: pendingFunding.output_script_hex,
      amountSat: Number(pendingFunding.channel_value_satoshis),
      feeRate: flow.DEFAULTS.fundingFeeRateSatPerVb,
    });

    await page.evaluate(
      ({ temp, peer, hex }) => {
        window.__sdk.node.submitFundingTransactionValue({
          temporary_channel_id: temp,
          counterparty_node_id: peer,
          funding_tx_hex: hex,
        });
      },
      {
        temp: pendingFunding.temporary_channel_id,
        peer: pendingFunding.counterparty_node_id,
        hex: funding.funding_tx_hex,
      }
    );

    await regularRln.waitForFinalizedChannelFromPeer(
      wasmPubkey,
      pendingFunding.temporary_channel_id,
      { pump: () => pumpRuntime(page) }
    );
    await regtest.waitForTxAcceptedByIndexer(funding.txid, {
      pump: () => pumpRuntime(page),
    });
    await regtest.waitForTxInMempool(funding.txid);
    await page.evaluate(() => window.__sdk.node.persistLdkRuntimeState());
    return { pendingFunding, funding };
  });

  await test.step("reload same runtime before confirmations", async () => {
    await page.reload();
  });

  const secondReady = await test.step("second boot: same node identity returns", async () => {
    return waitForSdkReady(page);
  });
  expect(secondReady.pubkey).toBe(wasmPubkey);

  await test.step("reconnect persisted peer", async () => {
    await page.evaluate(async () => {
      await window.__sdk.node.reconnectPersistedPeersValue();
    });
  });

  const readyChannel = await test.step("mine funding confirmations and wait for ready", async () => {
    const tipBefore = await regtest.getEsploraTipHeight();
    await regtest.mineBlocks(flow.DEFAULTS.fundingConfirmBlocks);
    await regtest.waitForEsploraTip(tipBefore + flow.DEFAULTS.fundingConfirmBlocks);
    await regtest.waitForEsploraTxConfirmed(pending.funding.txid);
    for (let i = 0; i < 20; i += 1) {
      await pumpRuntime(page);
      await new Promise((r) => setTimeout(r, 100));
    }
    return waitForChannelReadyToPeer(page, regularPubkey);
  });
  expect(readyChannel.peer_pubkey.toLowerCase()).toBe(regularPubkey.toLowerCase());

  const artifactDir = path.join(testInfo.outputDir, "sdk-snapshot-success");
  await dumpSdkArtifacts(page, artifactDir, "same-runtime-reload-final");
});

test("WASM same-runtime reload preserves a usable channel and can pay", async ({ page }, testInfo) => {
  test.setTimeout(540_000);

  const regtest = new RegtestController();
  const regularRln = new RegularRlnClient();

  const regularPubkey = await test.step("regular RLN reachable", async () => {
    const info = await regularRln.nodeInfo();
    expect(info && typeof info.pubkey === "string").toBeTruthy();
    return info.pubkey;
  });

  const firstReady = await test.step("first boot: load stable harness", async () => {
    return flow.loadHarness(page);
  });
  expect(firstReady.pubkey).toMatch(/^[0-9a-f]{66}$/i);
  const wasmPubkey = firstReady.pubkey;

  await test.step("open channel and wait until usable", async () => {
    await flow.fundWasmWallet(page, regtest);
    const pending = await flow.openChannelAndWaitForPending(page, {
      regularPubkey,
    });
    const { readyChannel } = await flow.fundChannelAndWaitForReady(page, {
      regtest,
      regularRln,
      pending,
      regularPubkey,
      wasmPubkey,
    });
    expect(readyChannel.peer_pubkey.toLowerCase()).toBe(regularPubkey.toLowerCase());
    await page.evaluate(() => window.__sdk.node.persistLdkRuntimeState());
  });

  await test.step("reload same runtime and restore peer", async () => {
    await page.reload();
    const secondReady = await waitForSdkReady(page);
    expect(secondReady.pubkey).toBe(wasmPubkey);
    await reconnectPersistedPeers(page);
  });

  const restoredChannel = await test.step("restored channel is usable", async () => {
    return waitForChannelUsableToPeer(page, regularPubkey);
  });
  expect(restoredChannel.peer_pubkey.toLowerCase()).toBe(regularPubkey.toLowerCase());

  const payment = await test.step("post-reload keysend succeeds", async () => {
    return flow.payKeysend(page, { regularPubkey });
  });
  expect(payment && payment.status).toBe("succeeded");

  const artifactDir = path.join(testInfo.outputDir, "sdk-snapshot-success");
  await dumpSdkArtifacts(page, artifactDir, "same-runtime-usable-payment-final");
});

test("WASM mid-handshake same-runtime reload aborts instead of pretending usable", async ({ page }, testInfo) => {
  test.setTimeout(360_000);

  const regtest = new RegtestController();
  const regularRln = new RegularRlnClient();

  const regularPubkey = await test.step("regular RLN reachable", async () => {
    const info = await regularRln.nodeInfo();
    expect(info && typeof info.pubkey === "string").toBeTruthy();
    return info.pubkey;
  });

  const firstReady = await test.step("first boot: load stable harness", async () => {
    return flow.loadHarness(page);
  });
  expect(firstReady.pubkey).toMatch(/^[0-9a-f]{66}$/i);
  const wasmPubkey = firstReady.pubkey;

  await test.step("start channel handshake and stop before funding", async () => {
    await flow.fundWasmWallet(page, regtest);
    const pending = await flow.openChannelAndWaitForPending(page, {
      regularPubkey,
    });
    expect(pending.temporary_channel_id).toMatch(/^[0-9a-f]{64}$/i);
    await page.evaluate(() => window.__sdk.node.persistLdkRuntimeState());
  });

  await test.step("reload same runtime during the handshake", async () => {
    await page.reload();
    const secondReady = await waitForSdkReady(page);
    expect(secondReady.pubkey).toBe(wasmPubkey);
    await reconnectPersistedPeers(page);
  });

  await test.step("abandoned in-flight channel is not exposed as usable", async () => {
    for (let i = 0; i < 30; i += 1) {
      await pumpRuntime(page);
      await new Promise((r) => setTimeout(r, 200));
    }
    const channels = await page.evaluate(() =>
      JSON.parse(window.__sdk.node.listChannelsJson())
    );
    const channel = channels.find(
      (ch) =>
        typeof ch.peer_pubkey === "string" &&
        ch.peer_pubkey.toLowerCase() === regularPubkey.toLowerCase()
    );
    expect(channel && channel.is_usable === true).toBeFalsy();
  });

  await test.step("payment does not succeed over the aborted channel", async () => {
    let result = null;
    let error = null;
    try {
      result = await submitKeysend(
        page,
        regularPubkey,
        flow.DEFAULTS.keysendAmountMsat
      );
    } catch (e) {
      error = String(e);
    }
    expect(error || (result && result.status !== "succeeded")).toBeTruthy();
  });

  const artifactDir = path.join(testInfo.outputDir, "sdk-snapshot-success");
  await dumpSdkArtifacts(page, artifactDir, "mid-handshake-abort-final");
});

test("WASM browser restart-style boot hydrates LDK state from IndexedDB", async ({ page }, testInfo) => {
  test.setTimeout(540_000);

  const regtest = new RegtestController();
  const regularRln = new RegularRlnClient();

  const regularPubkey = await test.step("regular RLN reachable", async () => {
    const info = await regularRln.nodeInfo();
    expect(info && typeof info.pubkey === "string").toBeTruthy();
    return info.pubkey;
  });

  const firstReady = await test.step("first boot: load stable harness", async () => {
    return flow.loadHarness(page);
  });
  expect(firstReady.pubkey).toMatch(/^[0-9a-f]{66}$/i);
  const wasmPubkey = firstReady.pubkey;

  await test.step("open usable channel and persist to IndexedDB", async () => {
    await flow.fundWasmWallet(page, regtest);
    const pending = await flow.openChannelAndWaitForPending(page, {
      regularPubkey,
    });
    const { readyChannel } = await flow.fundChannelAndWaitForReady(page, {
      regtest,
      regularRln,
      pending,
      regularPubkey,
      wasmPubkey,
    });
    expect(readyChannel.is_usable).toBeTruthy();
    await page.evaluate(() => window.__sdk.node.persistLdkRuntimeState());
    await waitForIndexedDbRuntimeEntries(page);
  });

  const identity = await getHarnessIdentity(page);
  expect(identity.pubkey).toBe(wasmPubkey);
  expect(identity.runtimeId).toBeTruthy();
  expect(identity.mnemonic).toBeTruthy();

  await test.step("clear volatile browser storage and restart harness", async () => {
    await clearBrowserVolatileRuntimeStorage(page);
    await flow.loadHarness(page, {
      runtimeId: identity.runtimeId,
      mnemonic: identity.mnemonic,
    });
  });

  const secondReady = await test.step("same node identity hydrates from persistent store", async () => {
    return waitForSdkReady(page);
  });
  expect(secondReady.pubkey).toBe(wasmPubkey);

  await test.step("reconnect peer and use restored channel", async () => {
    await reconnectPersistedPeers(page);
    const ch = await waitForChannelUsableToPeer(page, regularPubkey);
    expect(ch.peer_pubkey.toLowerCase()).toBe(regularPubkey.toLowerCase());
  });

  const payment = await test.step("post-restart keysend succeeds", async () => {
    return flow.payKeysend(page, { regularPubkey });
  });
  expect(payment && payment.status).toBe("succeeded");

  const artifactDir = path.join(testInfo.outputDir, "sdk-snapshot-success");
  await dumpSdkArtifacts(page, artifactDir, "indexeddb-restart-final");
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
