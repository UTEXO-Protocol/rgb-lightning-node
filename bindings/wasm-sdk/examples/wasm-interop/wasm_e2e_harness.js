// Minimal browser entry used by E2E and manual DevTools driving.
//
// Goals:
//   - construct the SDK once (RlnWasmNode + RlnWasmWallet),
//   - bring the BDK wallet online against the regtest indexer,
//   - expose everything on `window.__sdk`,
//   - do *not* drive any flow (open/fund/pay) here — caller owns the flow.
//
// Wallet model:
//   - `?freshRuntime=1` → fresh runtime id AND fresh mnemonic (fully isolated per run).
//   - `?runtimeId=...&mnemonic=...` → explicit stable identity for restart-style tests.
//   - default → stable runtime id + mnemonic across reloads in the same tab.

import init, {
  RlnWasmSdk,
  RlnWasmNode,
  RlnWasmWallet,
  rgbGenerateKeysValue,
  rgbRestoreKeysValue,
} from "../../pkg/rln_wasm_sdk.js";

const DEFAULT_INDEXER_URL = "http://127.0.0.1:3002";
const DEFAULT_NODE_PROXY_URL = "ws://127.0.0.1:3001";

function getQueryParam(name, fallback) {
  try {
    const u = new URL(window.location.href);
    const v = u.searchParams.get(name);
    return v && v.length > 0 ? v : fallback;
  } catch (_e) {
    return fallback;
  }
}

function setStatus(text) {
  const el = document.getElementById("status");
  if (el) el.textContent = text;
}

function buildWalletData(keys, runtimeId) {
  return {
    data_dir: `/tmp/rln_wasm_e2e_${runtimeId}`,
    bitcoin_network: "Regtest",
    database_type: "Sqlite",
    max_allocations_per_utxo: 5,
    account_xpub_vanilla: keys.account_xpub_vanilla,
    account_xpub_colored: keys.account_xpub_colored,
    mnemonic: keys.mnemonic,
    master_fingerprint: keys.master_fingerprint,
    vanilla_keychain: null,
    supported_schemas: ["Nia"],
  };
}

(async function main() {
  try {
    await init();

    const wantFresh = getQueryParam("freshRuntime", "") === "1";
    let rid;
    let mnemonic;
    const explicitRuntimeId = getQueryParam("runtimeId", "");
    const explicitMnemonic = getQueryParam("mnemonic", "");
    if (explicitRuntimeId && explicitMnemonic) {
      rid = explicitRuntimeId;
      mnemonic = explicitMnemonic;
      sessionStorage.setItem("rln_wasm_runtime_id", rid);
      sessionStorage.setItem("rln_wasm_e2e_mnemonic", mnemonic);
    } else if (wantFresh) {
      rid = Math.random().toString(16).slice(2) + Math.random().toString(16).slice(2);
      mnemonic = rgbGenerateKeysValue("regtest").mnemonic;
    } else {
      rid = sessionStorage.getItem("rln_wasm_runtime_id");
      if (!rid) {
        rid = Math.random().toString(16).slice(2);
        sessionStorage.setItem("rln_wasm_runtime_id", rid);
      }
      mnemonic = sessionStorage.getItem("rln_wasm_e2e_mnemonic");
      if (!mnemonic) {
        mnemonic = rgbGenerateKeysValue("regtest").mnemonic;
        sessionStorage.setItem("rln_wasm_e2e_mnemonic", mnemonic);
      }
    }

    const sdk = new RlnWasmSdk();
    const password = "rln-wasm-e2e";
    await sdk.initValue(password, mnemonic);
    await sdk.unlock(JSON.stringify({ password }));

    const baseProxyUrl = getQueryParam("nodeProxyUrl", DEFAULT_NODE_PROXY_URL);
    const indexerUrl = getQueryParam("indexerUrl", DEFAULT_INDEXER_URL);
    const nodeProxyUrl = baseProxyUrl.split("#runtime:")[0];

    const node = RlnWasmNode.newWithNodeRuntimeId(nodeProxyUrl, rid);
    node.installAutoPeerManagerHooks();
    node.chainSyncStartValue(indexerUrl, 5000);
    try {
      node.reconnectManagerStartValue();
    } catch (_e) {
      /* optional */
    }

    const keys = rgbRestoreKeysValue("regtest", mnemonic);
    const walletData = buildWalletData(keys, rid);
    const wallet = await RlnWasmWallet.create(JSON.stringify(walletData));
    const online = await wallet.goOnlineValue(true, indexerUrl);

    if (typeof node.attachWallet === "function") {
      try { node.attachWallet(wallet); } catch (_e) { /* optional */ }
    }

    const walletAddress = wallet.getAddress();

    Object.freeze(
      Object.assign((window.__sdk = {}), {
        node,
        wallet,
        online,
        walletAddress,
        mnemonic,
        runtimeId: rid,
        nodeProxyUrl,
        indexerUrl,
        ready: true,
        version: 1,
      })
    );

    const flushLdkState = () => {
      try {
        node.persistLdkRuntimeState();
      } catch (_e) {
        /* best-effort */
      }
    };
    window.addEventListener("pagehide", flushLdkState);
    window.addEventListener("beforeunload", flushLdkState);

    const pubkey = (JSON.parse(node.nodePubkeyJson()) || {}).pubkey || "?";
    setStatus(`ready · pubkey=${pubkey} · wallet=${walletAddress.slice(0, 18)}…`);
  } catch (err) {
    window.__sdk = { ready: false, error: String(err) };
    setStatus(`init failed: ${err}`);
    throw err;
  }
})();
