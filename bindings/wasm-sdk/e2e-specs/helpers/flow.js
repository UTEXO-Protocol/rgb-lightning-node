"use strict";

/**
 * High-level WASM SDK ↔ regular RLN E2E flow building blocks.
 *
 * Every spec drives the same `wasm_e2e_harness.html` and the same
 * `RegtestController` / `RegularRlnClient` plumbing, then composes its own
 * scenario from these primitives. Keeps individual specs short and focused
 * on what they're actually testing (happy path, reload, ws disconnect, …).
 *
 * Contracts:
 *   - All steps assume the harness is already loaded for the given page
 *     unless otherwise noted (see `loadHarness`).
 *   - Steps never broadcast funding txs themselves: the WASM SDK's LDK
 *     `BroadcasterInterface` does that via Esplora; we just observe it.
 *   - Steps never mine on the way to "channel ready" except via the
 *     external `regtest` controller (regtest needs an external miner).
 */

const sdk = require("./sdk");

const HARNESS_PATH =
  "/bindings/wasm-sdk/examples/wasm-interop/wasm_e2e_harness.html";

const DEFAULTS = Object.freeze({
  capacitySat: 500_000,
  // Top up the BDK wallet with ~10x the channel capacity so it has plenty of
  // room for fees, change, and any concurrent test reuse without retopping.
  walletFundBtc: 0.05,
  walletFundConfirmBlocks: 6,
  fundingFeeRateSatPerVb: 2,
  fundingConfirmBlocks: 12,
  keysendAmountMsat: 3_000_000,
  regularRlnLnAddr: process.env.E2E_REGULAR_RLN_LN_ADDR || "127.0.0.1:9802",
});

function harnessUrl({ freshRuntime = false, runtimeId, mnemonic } = {}) {
  const params = new URLSearchParams();
  if (freshRuntime) params.set("freshRuntime", "1");
  if (runtimeId) params.set("runtimeId", runtimeId);
  if (mnemonic) params.set("mnemonic", mnemonic);
  const qs = params.toString();
  return qs ? `${HARNESS_PATH}?${qs}` : HARNESS_PATH;
}

/**
 * Load the harness page and wait until `window.__sdk` is fully wired
 * (node + wallet + online). Returns the readiness payload from
 * `waitForSdkReady` (`{ pubkey }`).
 */
async function loadHarness(page, { freshRuntime = false, runtimeId, mnemonic, timeoutMs } = {}) {
  await page.goto(harnessUrl({ freshRuntime, runtimeId, mnemonic }));
  return sdk.waitForSdkReady(page, timeoutMs ? { timeoutMs } : undefined);
}

/**
 * Top up the WASM SDK's BDK wallet via the regtest miner wallet, then wait
 * until the WASM wallet's vanilla balance reflects the deposit.
 *
 * Returns `{ walletAddress, minSpendableSat }`.
 */
async function fundWasmWallet(page, regtest, opts = {}) {
  const cfg = { ...DEFAULTS, ...opts };
  const walletAddress = await sdk.getWalletAddress(page);
  if (!walletAddress) {
    throw new Error("harness did not expose wallet address (window.__sdk.walletAddress)");
  }
  await regtest.fundAddress(
    walletAddress,
    cfg.walletFundBtc,
    cfg.walletFundConfirmBlocks
  );
  const minSpendableSat = Math.floor(cfg.walletFundBtc * 1e8) - 50_000;
  await sdk.waitForWalletSpendable(page, minSpendableSat);
  return { walletAddress, minSpendableSat };
}

/**
 * Connect the WASM node to the regular RLN, open a non-virtual channel, and
 * wait for the LDK `FundingGenerationReady` event. Returns the parsed
 * pending-funding entry (`{ temporary_channel_id, output_script_hex,
 * channel_value_satoshis, counterparty_node_id }`).
 */
async function openChannelAndWaitForPending(page, opts) {
  const { regularPubkey, capacitySat = DEFAULTS.capacitySat, regularRlnLnAddr =
    DEFAULTS.regularRlnLnAddr } = opts;
  await page.evaluate(
    async ({ peerAddr, peerPubkey }) => {
      await window.__sdk.node.connectPeer(peerAddr, peerPubkey);
    },
    { peerAddr: regularRlnLnAddr, peerPubkey: regularPubkey }
  );
  await page.evaluate(
    ({ peerPubkey, capacity }) => {
      return JSON.parse(
        window.__sdk.node.openChannelJson(
          peerPubkey,
          BigInt(capacity),
          false,
          undefined,
          undefined
        )
      );
    },
    { peerPubkey: regularPubkey, capacity: capacitySat }
  );
  return sdk.waitForPendingFunding(page);
}

/**
 * Drive the LDK funding handshake to completion:
 *  1. ask the WASM BDK wallet to build + sign the funding tx,
 *  2. submit the signed hex to LDK so it sends `FundingCreated`,
 *  3. wait until the regular RLN reports the migrated channel id
 *     (i.e. it processed `FundingCreated` and replied with `FundingSigned`),
 *  4. assert the WASM SDK broadcast the funding tx itself via its LDK
 *     `BroadcasterInterface` → Esplora pipeline,
 *  5. mine confirmations + wait for the indexer to see the tx as confirmed,
 *  6. wait for `listChannelsJson()` on the WASM side to flip to ready.
 *
 * Returns `{ funding, readyChannel }`.
 */
async function fundChannelAndWaitForReady(page, opts) {
  const {
    regtest,
    regularRln,
    pending,
    regularPubkey,
    wasmPubkey,
    fundingFeeRateSatPerVb = DEFAULTS.fundingFeeRateSatPerVb,
    fundingConfirmBlocks = DEFAULTS.fundingConfirmBlocks,
  } = opts;

  const tipBefore = await regtest.getEsploraTipHeight();

  const funding = await sdk.buildLightningFundingTx(page, {
    outputScriptHex: pending.output_script_hex,
    amountSat: Number(pending.channel_value_satoshis),
    feeRate: fundingFeeRateSatPerVb,
  });
  if (!/^[0-9a-f]+$/i.test(funding.funding_tx_hex)) {
    throw new Error(
      `WASM wallet returned invalid funding hex: ${String(funding.funding_tx_hex).slice(0, 80)}`
    );
  }
  if (!/^[0-9a-f]{64}$/i.test(funding.txid)) {
    throw new Error(`WASM wallet returned invalid txid: ${funding.txid}`);
  }

  await page.evaluate(
    ({ temp, peer, hex }) => {
      window.__sdk.node.submitFundingTransactionValue({
        temporary_channel_id: temp,
        counterparty_node_id: peer,
        funding_tx_hex: hex,
      });
    },
    {
      temp: pending.temporary_channel_id,
      peer: pending.counterparty_node_id,
      hex: funding.funding_tx_hex,
    }
  );

  await regularRln.waitForFinalizedChannelFromPeer(
    wasmPubkey,
    pending.temporary_channel_id,
    { pump: () => sdk.pumpRuntime(page) }
  );

  await regtest.waitForTxAcceptedByIndexer(funding.txid, {
    pump: () => sdk.pumpRuntime(page),
  });
  await regtest.waitForTxInMempool(funding.txid);

  await regtest.mineBlocks(fundingConfirmBlocks);
  await regtest.waitForEsploraTip(tipBefore + fundingConfirmBlocks);
  await regtest.waitForEsploraTxConfirmed(funding.txid);
  for (let i = 0; i < 20; i += 1) {
    await sdk.pumpRuntime(page);
    await new Promise((r) => setTimeout(r, 100));
  }

  const readyChannel = await sdk.waitForChannelReadyToPeer(page, regularPubkey);
  return { funding, readyChannel };
}

/**
 * Submit a keysend payment from the WASM SDK to the regular RLN and wait for
 * it to settle to "succeeded". Returns the final payment record.
 */
async function payKeysend(page, opts) {
  const { regularPubkey, amountMsat = DEFAULTS.keysendAmountMsat } = opts;
  const attempt = await sdk.submitKeysend(page, regularPubkey, amountMsat);
  if (attempt && attempt.status === "succeeded") return attempt;
  if (!attempt || !attempt.payment_hash) {
    throw new Error(
      `keysend returned no payment_hash and not succeeded: ${JSON.stringify(attempt).slice(0, 400)}`
    );
  }
  return sdk.waitForPaymentSucceeded(page, attempt.payment_hash);
}

/**
 * Convenience: run the full WASM ↔ regular RLN happy path, returning a
 * single object with all interesting handles. Used by the smoke spec and as
 * a building block for fault-injection specs that want to verify the full
 * flow still works after a fault.
 */
async function runFullHappyPath(page, opts) {
  const { regtest, regularRln, regularPubkey, wasmPubkey } = opts;
  await fundWasmWallet(page, regtest, opts);
  const pending = await openChannelAndWaitForPending(page, {
    regularPubkey,
    capacitySat: opts.capacitySat,
    regularRlnLnAddr: opts.regularRlnLnAddr,
  });
  const { funding, readyChannel } = await fundChannelAndWaitForReady(page, {
    regtest,
    regularRln,
    pending,
    regularPubkey,
    wasmPubkey,
    fundingFeeRateSatPerVb: opts.fundingFeeRateSatPerVb,
    fundingConfirmBlocks: opts.fundingConfirmBlocks,
  });
  const payment = await payKeysend(page, {
    regularPubkey,
    amountMsat: opts.keysendAmountMsat,
  });
  return { pending, funding, readyChannel, payment };
}

module.exports = {
  HARNESS_PATH,
  DEFAULTS,
  harnessUrl,
  loadHarness,
  fundWasmWallet,
  openChannelAndWaitForPending,
  fundChannelAndWaitForReady,
  payKeysend,
  runFullHappyPath,
};
