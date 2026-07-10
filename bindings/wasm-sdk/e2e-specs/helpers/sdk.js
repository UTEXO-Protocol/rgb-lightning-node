"use strict";

const { pollUntil } = require("./poll");

/**
 * Helpers that interact with the WASM SDK via Playwright's `page.evaluate`.
 *
 * Contract: the SDK page is expected to expose `window.__sdk = { node }` after
 * construction. All helpers go through `evaluate(fn, args)` so they always
 * read the *current* SDK state from the running browser tab — never the
 * test process's stale view.
 *
 * Why structured (not log scraping)?
 *   - `#out` text matching is fragile and ambiguous across specs.
 *   - Real flows need predicates over typed JSON state (channels, payments,
 *     peers, runtime), not over human-readable strings.
 */

async function getNodePubkey(page) {
  return page.evaluate(() => {
    if (!window.__sdk || !window.__sdk.node) return null;
    try {
      return JSON.parse(window.__sdk.node.nodePubkeyJson()).pubkey || null;
    } catch (_e) {
      return null;
    }
  });
}

async function snapshot(page) {
  return page.evaluate(() => {
    const node = window.__sdk && window.__sdk.node;
    if (!node) return { ready: false };
    const safe = (fn, fallback) => {
      try {
        return fn();
      } catch (_e) {
        return fallback;
      }
    };
    return {
      ready: true,
      pubkey: safe(() => JSON.parse(node.nodePubkeyJson()).pubkey, null),
      runtime: safe(() => JSON.parse(node.ldkRuntimeStatusJson()), null),
      channels: safe(() => JSON.parse(node.listChannelsJson()), []),
      payments: safe(() => JSON.parse(node.listPaymentsJson()), []),
      pending_funding: safe(
        () => JSON.parse(node.listPendingFundingRequestsJson()),
        []
      ),
      chain_sync: safe(() => node.chainSyncStatusValue(), null),
    };
  });
}

async function pumpRuntime(page) {
  return page.evaluate(async () => {
    const node = window.__sdk && window.__sdk.node;
    if (!node) return { pumped: false };
    try {
      await node.chainSyncTickValue();
    } catch (_e) {
      /* ignore */
    }
    let drained = 0;
    try {
      const r = node.processNativeRuntimeQueueValue();
      drained = (r && typeof r.drained === "number" && r.drained) || 0;
    } catch (_e) {
      /* ignore */
    }
    return { pumped: true, drained };
  });
}

async function waitForSdkReady(page, opts = {}) {
  return pollUntil(
    async () => {
      const pk = await getNodePubkey(page);
      return pk ? { pubkey: pk } : null;
    },
    { timeoutMs: 60_000, intervalMs: 200, label: "sdk ready (window.__sdk.node)", ...opts }
  );
}

async function waitForPendingFunding(page, opts = {}) {
  return pollUntil(
    async () => {
      await pumpRuntime(page);
      const list = await page.evaluate(
        () => JSON.parse(window.__sdk.node.listPendingFundingRequestsJson())
      );
      return Array.isArray(list) && list.length > 0 ? list[0] : null;
    },
    {
      timeoutMs: 90_000,
      intervalMs: 200,
      label: "WASM pending funding request",
      ...opts,
    }
  );
}

function findChannelByPeer(channels, peerPubkey) {
  if (!Array.isArray(channels)) return null;
  const target = (peerPubkey || "").toLowerCase();
  return (
    channels.find(
      (c) =>
        typeof c.peer_pubkey === "string" &&
        c.peer_pubkey.toLowerCase() === target
    ) || null
  );
}

async function waitForChannelReadyToPeer(page, peerPubkey, opts = {}) {
  return pollUntil(
    async () => {
      await pumpRuntime(page);
      const channels = await page.evaluate(() =>
        JSON.parse(window.__sdk.node.listChannelsJson())
      );
      const ch = findChannelByPeer(channels, peerPubkey);
      if (!ch) return null;
      const ready =
        ch.ready === true ||
        ch.is_usable === true ||
        ch.status === "ready" ||
        ch.status === "open";
      return ready ? ch : null;
    },
    {
      timeoutMs: 5 * 60_000,
      intervalMs: 250,
      label: `WASM channel to ${peerPubkey} ready`,
      ...opts,
    }
  );
}

async function waitForChannelUsableToPeer(page, peerPubkey, opts = {}) {
  return pollUntil(
    async () => {
      await pumpRuntime(page);
      const channels = await page.evaluate(() =>
        JSON.parse(window.__sdk.node.listChannelsJson())
      );
      const ch = findChannelByPeer(channels, peerPubkey);
      return ch && ch.is_usable === true ? ch : null;
    },
    {
      timeoutMs: 5 * 60_000,
      intervalMs: 250,
      label: `WASM channel to ${peerPubkey} usable`,
      ...opts,
    }
  );
}

async function submitKeysend(page, peerPubkey, amountMsat) {
  return page.evaluate(
    ({ peer, amt }) => {
      const node = window.__sdk.node;
      const raw = node.keysendJson(peer, BigInt(amt), undefined, undefined);
      return JSON.parse(raw);
    },
    { peer: peerPubkey, amt: amountMsat }
  );
}

async function waitForPaymentSucceeded(page, paymentHash, opts = {}) {
  return pollUntil(
    async () => {
      await pumpRuntime(page);
      const payments = await page.evaluate(() =>
        JSON.parse(window.__sdk.node.listPaymentsJson())
      );
      if (!Array.isArray(payments)) return null;
      const match = payments.find((p) => p.payment_hash === paymentHash);
      if (!match) return null;
      if (match.status === "succeeded") return match;
      return null;
    },
    {
      timeoutMs: 5 * 60_000,
      intervalMs: 300,
      label: `payment ${paymentHash} succeeded`,
      ...opts,
    }
  );
}

async function getWalletAddress(page) {
  return page.evaluate(() => {
    if (!window.__sdk) return null;
    if (typeof window.__sdk.walletAddress === "string") return window.__sdk.walletAddress;
    if (window.__sdk.wallet && typeof window.__sdk.wallet.getAddress === "function") {
      try { return window.__sdk.wallet.getAddress(); } catch (_e) { return null; }
    }
    return null;
  });
}

async function getWalletBtcBalance(page) {
  return page.evaluate(() => {
    if (!window.__sdk || !window.__sdk.wallet) return null;
    try {
      return window.__sdk.wallet.getBtcBalanceValue();
    } catch (e) {
      return { error: String(e) };
    }
  });
}

function readVanillaSpendable(balance) {
  if (!balance || typeof balance !== "object") return 0;
  const v = balance.vanilla;
  if (!v || typeof v !== "object") return 0;
  const n = Number(v.spendable);
  return Number.isFinite(n) ? n : 0;
}

/**
 * Block until the wallet's vanilla spendable balance is at least `minSat`.
 * Triggers a wallet sync on every poll iteration so the indexer's view is
 * surfaced quickly.
 */
async function waitForWalletSpendable(page, minSat, opts = {}) {
  return pollUntil(
    async () => {
      try {
        await page.evaluate(async () => {
          if (!window.__sdk || !window.__sdk.wallet || !window.__sdk.online) return;
          await window.__sdk.wallet.syncOnline(window.__sdk.online);
        });
      } catch (_e) {
        /* ignore — wallet may not be ready yet */
      }
      const bal = await getWalletBtcBalance(page);
      const spendable = readVanillaSpendable(bal);
      return spendable >= minSat ? { spendable, balance: bal } : null;
    },
    {
      timeoutMs: 90_000,
      intervalMs: 500,
      label: `wallet vanilla spendable >= ${minSat} sat`,
      ...opts,
    }
  );
}

/**
 * Drive the WASM SDK's BDK wallet to construct (and sign) a Lightning channel
 * funding transaction paying `amountSat` to `outputScriptHex`. Returns
 * `{ funding_tx_hex, txid, address, signed_psbt }`.
 *
 * The transaction is *not* broadcast — the test broadcasts it via the regtest
 * controller (or relies on LDK's chain interface in production paths).
 */
async function buildLightningFundingTx(
  page,
  { outputScriptHex, amountSat, feeRate = 1 }
) {
  return page.evaluate(
    async ({ scriptHex, amount, fee }) => {
      const wallet = window.__sdk && window.__sdk.wallet;
      const online = window.__sdk && window.__sdk.online;
      if (!wallet || !online) {
        throw new Error("window.__sdk.wallet / online not initialized");
      }
      const raw = await wallet.buildLightningFundingTxJson(
        online,
        scriptHex,
        BigInt(amount),
        BigInt(fee)
      );
      return JSON.parse(raw);
    },
    { scriptHex: outputScriptHex, amount: amountSat, fee: feeRate }
  );
}

async function dumpSdkArtifacts(page, outDir, prefix = "sdk") {
  const fs = require("fs");
  const path = require("path");
  fs.mkdirSync(outDir, { recursive: true });

  const snap = await snapshot(page).catch((err) => ({ error: String(err) }));
  fs.writeFileSync(
    path.join(outDir, `${prefix}-snapshot.json`),
    JSON.stringify(snap, null, 2)
  );

  const html = await page.content().catch(() => null);
  if (html) fs.writeFileSync(path.join(outDir, `${prefix}-page.html`), html);

  return snap;
}

module.exports = {
  getNodePubkey,
  snapshot,
  pumpRuntime,
  waitForSdkReady,
  waitForPendingFunding,
  findChannelByPeer,
  waitForChannelReadyToPeer,
  waitForChannelUsableToPeer,
  submitKeysend,
  waitForPaymentSucceeded,
  getWalletAddress,
  getWalletBtcBalance,
  waitForWalletSpendable,
  buildLightningFundingTx,
  dumpSdkArtifacts,
};
