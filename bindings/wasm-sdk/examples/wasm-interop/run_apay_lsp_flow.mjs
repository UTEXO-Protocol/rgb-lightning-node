// Headless driver for the WASM async-payments-with-LSP E2E flow.
//
// - serves the repo root over a tiny static server (correct .wasm / .js MIME types),
// - launches system Chrome (headless) via puppeteer-core,
// - runs the "run" phase (apayNewWithAddress + apayNew refill against a real utexo-lsp),
// - asserts the phase reported ok:true.
//
// Prereqs (see README "Async payments with LSP"):
//   1. compose.wasm.yaml infra (proxy/esplora/electrum/gateway),
//   2. a native invoice-host RLN node (peer 9802 / REST 3101) started with
//        --lsp-base-url http://127.0.0.1:8080 --lsp-bearer-token <token>,
//   3. utexo-lsp on :8080 with APAY_BEARER_TOKEN=<token> and LSP_BASE_URL pointing at the node.
//   4. the wasm pkg built:
//        cd bindings/wasm-sdk && wasm-pack build --target web --dev --out-dir pkg
//
// Usage:
//   PUPPETEER_EXECUTABLE_PATH=/usr/bin/google-chrome \
//   E2E_PUPPETEER=/tmp/e2e-driver/node_modules/puppeteer-core \
//   node bindings/wasm-sdk/examples/wasm-interop/run_apay_lsp_flow.mjs

import http from "node:http";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
// repo root = .../bindings/wasm-sdk/examples/wasm-interop -> up 4
const REPO_ROOT = path.resolve(__dirname, "../../../..");
const PAGE_PATH = "/bindings/wasm-sdk/examples/wasm-interop/apay_lsp_flow.html";

const PUPPETEER_DIR = process.env.E2E_PUPPETEER || "/tmp/e2e-driver/node_modules/puppeteer-core";
const CHROME = process.env.PUPPETEER_EXECUTABLE_PATH || "/usr/bin/google-chrome";
const RUN_TIMEOUT_MS = Number(process.env.E2E_RUN_TIMEOUT_MS || 600_000);

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

async function runPhase(page, baseUrl, phase, runtimeId, timeoutMs) {
  const url = `${baseUrl}${PAGE_PATH}?phase=${phase}&runtimeId=${encodeURIComponent(runtimeId)}`;
  console.log(`\n=== navigating: ${phase} ===\n${url}`);
  await page.goto(url, { waitUntil: "domcontentloaded", timeout: 60_000 });
  await page.waitForFunction("window.__E2E_DONE === true", { timeout: timeoutMs, polling: 500 });
  return page.evaluate("window.__E2E_RESULT");
}

async function main() {
  const { server, port } = await startStaticServer(REPO_ROOT);
  const baseUrl = `http://127.0.0.1:${port}`;
  console.log(`static server: ${baseUrl} (root=${REPO_ROOT})`);

  const puppeteer = await loadPuppeteer();
  const userDataDir = fs.mkdtempSync(path.join(os.tmpdir(), "apay-e2e-chrome-"));
  const browser = await puppeteer.launch({
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

  let exitCode = 0;
  try {
    const page = await browser.newPage();
    page.on("console", (msg) => {
      const t = msg.text();
      if (t.startsWith("[e2e]")) console.log(t);
    });
    page.on("pageerror", (err) => console.log(`[pageerror] ${err}`));

    const runtimeId = `apay-${Date.now().toString(16)}`;
    const runResult = await runPhase(page, baseUrl, "run", runtimeId, RUN_TIMEOUT_MS);
    console.log("\n--- RUN RESULT ---");
    console.log(JSON.stringify(runResult, null, 2));
    if (!runResult || !runResult.ok) throw new Error("run phase failed");

    console.log("\n✅✅✅ ASYNC-PAYMENTS-WITH-LSP FLOW PASSED ✅✅✅");
  } catch (e) {
    console.error(`\n❌ E2E FAILED: ${e}`);
    exitCode = 1;
  } finally {
    await browser.close().catch(() => {});
    server.close();
  }
  process.exit(exitCode);
}

main();
