"use strict";

const { pollUntil } = require("./poll");

/**
 * Node-side regtest controller: tests talk to bitcoind RPC and the Esplora
 * indexer directly, instead of going through the gateway's `/dev/regtest/*`
 * endpoints.
 *
 * Why? In production the WASM SDK never calls `/dev/regtest/*`. Keeping that
 * dependency in test code makes the suite "honest" only for the gateway, not
 * for end users. By moving funding/mining/confirmation polling to a
 * dedicated Node helper we:
 *   - run the same gateway binary that ships (dev-http can stay off in the
 *     gateway path that the WASM client exercises),
 *   - have a single, typed surface for all regtest plumbing,
 *   - make the spec body purely about user-visible flows.
 */

class RegtestController {
  constructor(opts = {}) {
    this.bitcoinRpcUrl =
      opts.bitcoinRpcUrl ||
      process.env.E2E_BITCOIN_RPC_URL ||
      "http://127.0.0.1:19443";
    this.bitcoinUser =
      opts.bitcoinUser || process.env.E2E_BITCOIN_RPC_USER || "admin";
    this.bitcoinPassword =
      opts.bitcoinPassword ||
      process.env.E2E_BITCOIN_RPC_PASSWORD ||
      "passw";
    this.walletName =
      opts.walletName || process.env.E2E_BITCOIN_RPC_WALLET || "bdk-test";
    this.indexerUrl =
      opts.indexerUrl || process.env.E2E_INDEXER_URL || "http://127.0.0.1:3002";
  }

  async _rpc(method, params = [], { wallet = false } = {}) {
    const url = wallet
      ? `${this.bitcoinRpcUrl}/wallet/${encodeURIComponent(this.walletName)}`
      : this.bitcoinRpcUrl;
    const auth = Buffer.from(
      `${this.bitcoinUser}:${this.bitcoinPassword}`
    ).toString("base64");
    const r = await fetch(url, {
      method: "POST",
      headers: {
        "content-type": "application/json",
        authorization: `Basic ${auth}`,
      },
      body: JSON.stringify({
        jsonrpc: "1.0",
        id: "e2e",
        method,
        params,
      }),
    });
    const text = await r.text();
    if (!r.ok) {
      throw new Error(`bitcoin RPC ${method} HTTP ${r.status}: ${text.slice(0, 400)}`);
    }
    let body;
    try {
      body = JSON.parse(text);
    } catch (_e) {
      throw new Error(
        `bitcoin RPC ${method} non-JSON response: ${text.slice(0, 400)}`
      );
    }
    if (body && body.error) {
      throw new Error(
        `bitcoin RPC ${method} error: ${JSON.stringify(body.error).slice(0, 400)}`
      );
    }
    return body && body.result;
  }

  async ensureWalletReady(minMaturedBlocks = 101) {
    try {
      const wallets = await this._rpc("listwallets");
      if (Array.isArray(wallets) && !wallets.includes(this.walletName)) {
        // Try to load (may already exist on disk); if that fails, create.
        try {
          await this._rpc("loadwallet", [this.walletName]);
        } catch (_e) {
          await this._rpc("createwallet", [this.walletName]);
        }
      }
    } catch (_e) {
      // listwallets can fail on very fresh nodes; fall through and try create.
      try {
        await this._rpc("createwallet", [this.walletName]);
      } catch (_e2) {
        /* possibly already exists */
      }
    }

    // Make sure we have spendable mature outputs.
    const info = await this._rpc("getblockchaininfo").catch(() => null);
    const height = info && typeof info.blocks === "number" ? info.blocks : 0;
    if (height < minMaturedBlocks) {
      const addr = await this._rpc("getnewaddress", [], { wallet: true });
      await this._rpc("generatetoaddress", [
        minMaturedBlocks - height + 1,
        addr,
      ]);
    }
  }

  /**
   * Build (but do not broadcast) a regtest funding tx that pays `amountSat` to
   * `outputScriptHex`. Mirrors the shape returned by the gateway's
   * `/dev/regtest/funding-tx` for backward compatibility:
   *   { funding_tx_hex, txid, address, amount_sat }
   */
  async buildFundingTx(outputScriptHex, amountSat) {
    if (!outputScriptHex || amountSat <= 0) {
      throw new Error("buildFundingTx requires outputScriptHex and amountSat");
    }
    await this.ensureWalletReady();

    // Convert script -> address (regtest) via decodescript.
    const decoded = await this._rpc("decodescript", [outputScriptHex]);
    const address =
      (decoded && (decoded.address || (decoded.addresses && decoded.addresses[0]))) ||
      null;
    if (!address) {
      throw new Error(
        `cannot derive address from output_script_hex (decoded=${JSON.stringify(decoded).slice(0, 200)})`
      );
    }
    const amountBtc = (Number(amountSat) / 1e8).toFixed(8);
    const rawHex = await this._rpc(
      "createrawtransaction",
      [[], [{ [address]: amountBtc }]],
      { wallet: true }
    );
    const funded = await this._rpc(
      "fundrawtransaction",
      [rawHex, { changePosition: 1 }],
      { wallet: true }
    );
    const signed = await this._rpc(
      "signrawtransactionwithwallet",
      [funded.hex],
      { wallet: true }
    );
    if (!signed.complete) {
      throw new Error(`signrawtransactionwithwallet incomplete: ${JSON.stringify(signed)}`);
    }
    const decodedTx = await this._rpc("decoderawtransaction", [signed.hex]);
    return {
      funding_tx_hex: signed.hex,
      txid: decodedTx.txid,
      address,
      amount_sat: amountSat,
    };
  }

  async broadcast(rawHex) {
    return this._rpc("sendrawtransaction", [rawHex]);
  }

  /**
   * Send `amountBtc` BTC from the regtest miner wallet to `address`. Used to
   * top up the WASM SDK's BDK wallet so it can build its own funding tx in
   * Phase 1.5+.
   *
   * If `mineBlocks > 0`, mines that many blocks immediately after the send so
   * the receiver can spend the funds without waiting for an external block
   * source. Returns `{ txid, address, amount_btc, mined_blocks }`.
   */
  async fundAddress(address, amountBtc, mineBlocks = 6) {
    if (!address || typeof address !== "string") {
      throw new Error("fundAddress requires a destination address");
    }
    const amount = Number(amountBtc);
    if (!Number.isFinite(amount) || amount <= 0) {
      throw new Error(`fundAddress requires positive amount_btc, got ${amountBtc}`);
    }
    await this.ensureWalletReady();
    const txid = await this._rpc(
      "sendtoaddress",
      [address, amount.toFixed(8)],
      { wallet: true }
    );
    let blockHashes = [];
    if (mineBlocks > 0) {
      blockHashes = await this.mineBlocks(mineBlocks);
    }
    return {
      txid,
      address,
      amount_btc: amount,
      mined_blocks: mineBlocks,
      block_hashes: blockHashes,
    };
  }

  async mineBlocks(n = 1) {
    await this.ensureWalletReady();
    const addr = await this._rpc("getnewaddress", [], { wallet: true });
    return this._rpc("generatetoaddress", [n, addr]);
  }

  async broadcastAndMine(rawHex, blocks = 6) {
    const txid = await this.broadcast(rawHex);
    const minedHashes = blocks > 0 ? await this.mineBlocks(blocks) : [];
    return { txid, mined_blocks: blocks, block_hashes: minedHashes };
  }

  async getEsploraTipHeight() {
    const r = await fetch(`${this.indexerUrl}/blocks/tip/height`);
    const t = (await r.text()).trim();
    const n = Number.parseInt(t, 10);
    if (!Number.isFinite(n)) {
      throw new Error(`invalid esplora tip height: ${t.slice(0, 80)}`);
    }
    return n;
  }

  async waitForEsploraTip(targetHeight, opts = {}) {
    return pollUntil(
      async () => {
        const h = await this.getEsploraTipHeight().catch(() => -1);
        return h >= targetHeight ? h : null;
      },
      {
        timeoutMs: 90_000,
        intervalMs: 500,
        label: `esplora tip >= ${targetHeight}`,
        ...opts,
      }
    );
  }

  async waitForEsploraTxConfirmed(txid, opts = {}) {
    return pollUntil(
      async () => {
        const r = await fetch(`${this.indexerUrl}/tx/${txid}/status`);
        if (!r.ok) return null;
        const st = await r.json().catch(() => null);
        return st && st.confirmed === true ? st : null;
      },
      {
        timeoutMs: 90_000,
        intervalMs: 500,
        label: `esplora confirms ${txid}`,
        ...opts,
      }
    );
  }

  /**
   * Wait until the indexer has *accepted* a transaction (mempool or confirmed),
   * regardless of confirmation status. We use this in Phase 1.6 to verify that
   * the WASM SDK has broadcast the funding tx itself via its own LDK
   * `BroadcasterInterface` → `chain_sync::broadcast_tx` pipeline (POST `/tx`).
   * Returns the raw `/tx/{txid}/status` body on success.
   *
   * Optional `pump` callback is invoked between polls so callers can drive the
   * WASM chain-sync loop while waiting (each `chainSyncTickValue` pulls items
   * from the broadcaster queue and POSTs them to the indexer).
   */
  async waitForTxAcceptedByIndexer(txid, opts = {}) {
    const pump = opts.pump;
    return pollUntil(
      async () => {
        if (typeof pump === "function") {
          try { await pump(); } catch (_e) { /* ignore */ }
        }
        const r = await fetch(`${this.indexerUrl}/tx/${txid}/status`);
        if (!r.ok) return null;
        const st = await r.json().catch(() => null);
        return st ? st : null;
      },
      {
        timeoutMs: 60_000,
        intervalMs: 500,
        label: `indexer accepted tx ${txid}`,
        ...opts,
      }
    );
  }

  /**
   * Wait until bitcoind reports the tx in its mempool.
   *
   * This is stronger than indexer polling: it guarantees that a subsequent
   * `generatetoaddress` will actually be able to mine the transaction.
   */
  async waitForTxInMempool(txid, opts = {}) {
    await this.ensureWalletReady();
    return pollUntil(
      async () => {
        try {
          const entry = await this._rpc("getmempoolentry", [txid]);
          return entry || null;
        } catch (_e) {
          return null;
        }
      },
      {
        timeoutMs: 60_000,
        intervalMs: 250,
        label: `bitcoind mempool has ${txid}`,
        ...opts,
      }
    );
  }
}

module.exports = { RegtestController };
