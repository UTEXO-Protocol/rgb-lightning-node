// Headless driver for the cooperative-close RGB settlement flow
// (manual_js_rgb_coop_close_settlement_flow.js).
//
// Same harness shape as run_e2e_full_flow.mjs: static server over the repo root + system Chrome
// via puppeteer-core. Single phase. The console filter additionally surfaces close / sweep /
// SpendableOutputs lines, because the behaviour under test is post-close on-chain settlement.
//
// Prereqs: compose.wasm-infra.yaml infra, wasm-proxy-gateway (dev-http) on 3001, a FRESH native
// rgb-lightning-node on 9802/3101, and the wasm pkg built:
//   cd bindings/wasm-sdk && wasm-pack build --target web --dev --out-dir pkg
//
// Usage:
//   node bindings/wasm-sdk/examples/wasm-interop/run_coop_close_settlement_flow.mjs

import http from "node:http";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const REPO_ROOT = path.resolve(__dirname, "../../../..");
const PAGE_PATH = "/bindings/wasm-sdk/examples/wasm-interop/rgb_coop_close_settlement_flow.html";

const PUPPETEER_DIR = process.env.E2E_PUPPETEER || "/tmp/e2e-driver/node_modules/puppeteer-core";
const CHROME = process.env.PUPPETEER_EXECUTABLE_PATH || "/usr/bin/google-chrome";
const RUN_TIMEOUT_MS = Number(process.env.E2E_RUN_TIMEOUT_MS || 900_000);

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

async function main() {
  const { server, port } = await startStaticServer(REPO_ROOT);
  const baseUrl = `http://127.0.0.1:${port}`;
  console.log(`static server: ${baseUrl} (root=${REPO_ROOT})`);

  const puppeteer = await loadPuppeteer();
  const userDataDir = fs.mkdtempSync(path.join(os.tmpdir(), "e2e-chrome-"));
  const browser = await puppeteer.launch({
    executablePath: CHROME,
    headless: true,
    // The settlement wait keeps the page's JS busy with long wallet refresh calls; the default
    // 180s protocol timeout can kill the final __E2E_RESULT evaluate on an unsettled (repro) run.
    protocolTimeout: 300_000,
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
      if (t.startsWith("[e2e]")) {
        console.log(t);
      } else if (
        // The behaviour under test: always surface close/sweep/spendable-output lines.
        /SpendableOutputs|sweep|ChannelClosed|force.?clos|ProcessingError|closed due to|height-only best block/i.test(t)
      ) {
        console.log(`[wasm!] ${t.slice(0, 400)}`);
      } else if (
        process.env.E2E_VERBOSE &&
        /rgb|witness|consignment|transfer/i.test(t)
      ) {
        console.log(`[wasm] ${t.slice(0, 280)}`);
      }
    });
    page.on("pageerror", (err) => console.log(`[pageerror] ${err}`));

    const runtimeId = `coop-close-${Date.now().toString(16)}`;
    const url = `${baseUrl}${PAGE_PATH}?autorun=1&runtimeId=${encodeURIComponent(runtimeId)}`;
    console.log(`\n=== navigating ===\n${url}`);
    await page.goto(url, { waitUntil: "domcontentloaded", timeout: 60_000 });
    await page.waitForFunction("window.__E2E_DONE === true", { timeout: RUN_TIMEOUT_MS, polling: 500 });
    const result = await page.evaluate("window.__E2E_RESULT");

    console.log("\n--- RESULT ---");
    console.log(JSON.stringify(result, null, 2));
    if (!result || !result.ok) throw new Error("coop-close RGB settlement flow failed");

    console.log("\n✅✅✅ COOP-CLOSE RGB SETTLEMENT FLOW PASSED ✅✅✅");
  } catch (e) {
    console.error(`\n❌ FLOW FAILED: ${e}`);
    exitCode = 1;
  } finally {
    await browser.close().catch(() => {});
    server.close();
  }
  process.exit(exitCode);
}

main();
