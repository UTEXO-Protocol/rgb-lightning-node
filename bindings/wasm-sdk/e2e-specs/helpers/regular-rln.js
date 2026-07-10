"use strict";

const { pollUntil } = require("./poll");

/**
 * Direct REST client for a regular `rgb-lightning-node` (counterparty in the
 * E2E). Tests call this instead of going through the gateway's
 * `/dev/regular-rln/*` proxy endpoints so:
 *   - tests stay valid even when gateway dev-http is off,
 *   - failures point clearly at "regular RLN side" vs "gateway side".
 */

class RegularRlnClient {
  constructor(opts = {}) {
    this.baseUrl =
      opts.baseUrl || process.env.E2E_REGULAR_RLN_API_BASE || "http://127.0.0.1:3101";
    this.label = opts.label || "regular-rln";
  }

  async _post(path, body) {
    const r = await fetch(`${this.baseUrl}${path}`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(body || {}),
    });
    const text = await r.text();
    if (!r.ok) {
      throw new Error(
        `${this.label} POST ${path} HTTP ${r.status}: ${text.slice(0, 400)}`
      );
    }
    if (!text) return {};
    try {
      return JSON.parse(text);
    } catch (_e) {
      return { _raw: text };
    }
  }

  async _get(path) {
    const r = await fetch(`${this.baseUrl}${path}`);
    const text = await r.text();
    if (!r.ok) {
      throw new Error(
        `${this.label} GET ${path} HTTP ${r.status}: ${text.slice(0, 400)}`
      );
    }
    if (!text) return {};
    try {
      return JSON.parse(text);
    } catch (_e) {
      return { _raw: text };
    }
  }

  async nodeInfo() {
    return this._get("/nodeinfo");
  }

  async listChannels() {
    return this._get("/listchannels");
  }

  async listPayments() {
    return this._get("/listpayments");
  }

  async listPeers() {
    return this._get("/listpeers");
  }

  /**
   * Wait until `regular RLN listchannels` shows a channel from `peerPubkey`
   * with a non-temporary channel_id (i.e. it has migrated past funding-creation).
   *
   * `pump` is an optional callback the helper invokes between polls. Tests
   * pass `pumpRuntime(page)` so the WASM runtime keeps draining outbound
   * frames while we're polling the counterparty — otherwise the funding-signed
   * exchange may stall in the browser.
   */
  async waitForFinalizedChannelFromPeer(peerPubkey, tempChannelId, opts = {}) {
    const pump = opts.pump;
    return pollUntil(
      async () => {
        if (typeof pump === "function") {
          try {
            await pump();
          } catch (_e) {
            /* ignore: don't let pump errors break the poll */
          }
        }
        const res = await this.listChannels().catch(() => null);
        const list = res && Array.isArray(res.channels) ? res.channels : [];
        const target = (peerPubkey || "").toLowerCase();
        const fromPeer = list.filter(
          (c) =>
            typeof c.peer_pubkey === "string" &&
            c.peer_pubkey.toLowerCase() === target
        );
        const finalized = fromPeer.find(
          (c) =>
            typeof c.channel_id === "string" &&
            c.channel_id !== tempChannelId
        );
        return finalized || null;
      },
      {
        timeoutMs: 5 * 60_000,
        intervalMs: 250,
        label: `regular RLN finalized channel from ${peerPubkey} (temp=${tempChannelId})`,
        ...opts,
      }
    );
  }
}

module.exports = { RegularRlnClient };
