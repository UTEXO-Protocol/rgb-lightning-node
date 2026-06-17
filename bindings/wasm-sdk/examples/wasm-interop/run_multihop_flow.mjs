// Headless driver for the Phase 4 multi-hop forwarding E2E (native-A -> WASM -> native-B).
//
// - boots TWO rgb-native-phase5-node instances (A=payer on 19735/19737, B=payee on 19745/19747),
// - serves the repo root over a tiny static server (correct .wasm / .js MIME types),
// - launches system Chrome (headless, web-security disabled) via puppeteer-core,
// - runs the "run" phase and asserts ok:true,
// - tears the native nodes down on exit.
//
// Prereqs: compose.wasm.yaml infra (esplora 3002, relay gateway 3001) running, and the harness
// binary built: cd ../rust-lightning/contrib/rgb-cross-variant-harness && cargo build --bin
// rgb-native-phase5-node. The wasm pkg must be built first:
//   cd bindings/wasm-sdk && wasm-pack build --target web --dev --out-dir pkg -- --features real-wasm-rgb
//
// Usage:
//   PUPPETEER_EXECUTABLE_PATH=/usr/bin/google-chrome \
//   E2E_PUPPETEER=/tmp/e2e-driver/node_modules/puppeteer-core \
//   node bindings/wasm-sdk/examples/wasm-interop/run_multihop_flow.mjs

import http from "node:http";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawn } from "node:child_process";
import { fileURLToPath, pathToFileURL } from "node:url";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const REPO_ROOT = path.resolve(__dirname, "../../../..");
const PAGE_PATH = "/bindings/wasm-sdk/examples/wasm-interop/rgb_multihop_flow.html";

const PUPPETEER_DIR = process.env.E2E_PUPPETEER || "/tmp/e2e-driver/node_modules/puppeteer-core";
const CHROME = process.env.PUPPETEER_EXECUTABLE_PATH || "/usr/bin/google-chrome";
const RUN_TIMEOUT_MS = Number(process.env.E2E_RUN_TIMEOUT_MS || 600_000);

const HARNESS_BIN = path.resolve(
  REPO_ROOT,
  "../rust-lightning/contrib/rgb-cross-variant-harness/target/debug/rgb-native-phase5-node"
);
const ESPLORA_URL = process.env.E2E_ESPLORA_URL || "http://127.0.0.1:3002";

// Distinct node-identity seeds so the two instances have distinct node ids (the harness defaults
// every node to [1;32]; passing the 5th NODE_SEED_HEX arg overrides it).
const NATIVES = [
  { name: "native-A", listen: "127.0.0.1:19735", mgmt: "127.0.0.1:19737", seed: "11".repeat(32) },
  { name: "native-B", listen: "127.0.0.1:19745", mgmt: "127.0.0.1:19747", seed: "22".repeat(32) },
];

const MIME = {
  ".html": "text/html; charset=utf-8",
  ".js": "text/javascript; charset=utf-8",
  ".mjs": "text/javascript; charset=utf-8",
  ".wasm": "application/wasm",
  ".json": "application/json; charset=utf-8",
  ".css": "text/css; charset=utf-8",
};

function startStaticServer(root) {
  const server = http.createServer((req, res) => {
    try {
      const urlPath = decodeURIComponent(new URL(req.url, "http://x").pathname);
      const filePath = path.join(root, urlPath);
      if (!filePath.startsWith(root) || !fs.existsSync(filePath) || fs.statSync(filePath).isDirectory()) {
        res.writeHead(404);
        res.end("not found");
        return;
      }
      const ext = path.extname(filePath).toLowerCase();
      res.writeHead(200, {
        "content-type": MIME[ext] || "application/octet-stream",
        "cache-control": "no-store",
      });
      fs.createReadStream(filePath).pipe(res);
    } catch (e) {
      res.writeHead(500);
      res.end(String(e));
    }
  });
  const port = Number(process.env.E2E_STATIC_PORT || 0);
  return new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(port, "127.0.0.1", () => resolve({ server, port: server.address().port }));
  });
}

async function loadPuppeteer() {
  const pkg = JSON.parse(fs.readFileSync(path.join(PUPPETEER_DIR, "package.json"), "utf8"));
  const rel = pkg.exports?.["."]?.import || pkg.module || pkg.main;
  const entry = pathToFileURL(path.join(PUPPETEER_DIR, rel)).href;
  return (await import(entry)).default;
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

async function waitNativeReady(mgmt, timeoutMs) {
  const deadline = Date.now() + timeoutMs;
  let lastErr;
  while (Date.now() < deadline) {
    try {
      const resp = await fetch(`http://${mgmt}/info`, { signal: AbortSignal.timeout(3000) });
      if (resp.ok) return await resp.json();
    } catch (e) {
      lastErr = e;
    }
    await sleep(500);
  }
  throw new Error(`native node at ${mgmt} not ready: ${lastErr}`);
}

function startNative(node) {
  if (!fs.existsSync(HARNESS_BIN)) {
    throw new Error(`harness binary not found at ${HARNESS_BIN} (build it first)`);
  }
  const stamp = Date.now().toString(16);
  const dataDir = path.join(os.tmpdir(), `mh-${node.name}-${stamp}`);
  const logPath = path.join(os.tmpdir(), `mh-${node.name}-${stamp}.log`);
  const logFd = fs.openSync(logPath, "a");
  const child = spawn(HARNESS_BIN, [dataDir, node.listen, ESPLORA_URL, node.mgmt, node.seed], {
    stdio: ["ignore", logFd, logFd],
    detached: false,
  });
  console.log(`started ${node.name}: pid=${child.pid} listen=${node.listen} mgmt=${node.mgmt} log=${logPath}`);
  return { ...node, child, logPath };
}

function stopNative(n) {
  try {
    n.child.kill("SIGKILL");
  } catch {
    /* already gone */
  }
}

async function runPhase(page, baseUrl, runtimeId, timeoutMs) {
  const url = `${baseUrl}${PAGE_PATH}?phase=run&runtimeId=${encodeURIComponent(runtimeId)}`;
  console.log(`\n=== navigating: run ===\n${url}`);
  await page.goto(url, { waitUntil: "domcontentloaded", timeout: 60_000 });
  await page.waitForFunction("window.__E2E_DONE === true", { timeout: timeoutMs, polling: 500 });
  return page.evaluate("window.__E2E_RESULT");
}

async function main() {
  const running = [];
  let browser;
  let server;
  let exitCode = 0;
  try {
    // --- boot the two native nodes ---
    for (const node of NATIVES) running.push(startNative(node));
    for (const n of running) {
      const info = await waitNativeReady(n.mgmt, 30_000);
      console.log(`${n.name} ready: node_id=${info.node_id}`);
    }

    const started = await startStaticServer(REPO_ROOT);
    server = started.server;
    const baseUrl = `http://127.0.0.1:${started.port}`;
    console.log(`static server: ${baseUrl} (root=${REPO_ROOT})`);

    const puppeteer = await loadPuppeteer();
    const userDataDir = fs.mkdtempSync(path.join(os.tmpdir(), "mh-chrome-"));
    browser = await puppeteer.launch({
      executablePath: CHROME,
      headless: true,
      userDataDir,
      args: [
        "--no-sandbox",
        "--disable-dev-shm-usage",
        "--disable-gpu",
        "--disable-web-security",
        "--disable-features=IsolateOrigins,site-per-process",
      ],
    });

    const page = await browser.newPage();
    page.on("console", (msg) => {
      const t = msg.text();
      if (t.startsWith("[e2e]")) console.log(t);
      else if (process.env.E2E_VERBOSE && /routing::router|RouteNotFound|Failed to find|forward/i.test(t)) {
        console.log(`[wasm] ${t.slice(0, 280)}`);
      }
    });
    page.on("pageerror", (err) => console.log(`[pageerror] ${err}`));

    const runtimeId = `mh-${Date.now().toString(16)}`;
    const runResult = await runPhase(page, baseUrl, runtimeId, RUN_TIMEOUT_MS);
    console.log("\n--- RUN RESULT ---");
    console.log(JSON.stringify(runResult, null, 2));
    if (!runResult || !runResult.ok) throw new Error("multi-hop run phase failed");

    console.log("\n✅✅✅ PHASE 4 MULTI-HOP PASSED (native-A → WASM → native-B) ✅✅✅");
  } catch (e) {
    console.error(`\n❌ MULTI-HOP E2E FAILED: ${e}`);
    for (const n of running) {
      try {
        console.error(`--- ${n.name} log tail (${n.logPath}) ---`);
        const txt = fs.readFileSync(n.logPath, "utf8");
        console.error(txt.split("\n").slice(-25).join("\n"));
      } catch {
        /* no log */
      }
    }
    exitCode = 1;
  } finally {
    if (browser) await browser.close().catch(() => {});
    if (server) server.close();
    for (const n of running) stopNative(n);
  }
  process.exit(exitCode);
}

main();
