// @ts-check
const path = require("path");
const { defineConfig } = require("@playwright/test");

// This suite lives at `bindings/wasm-sdk/e2e-specs`, so the repo root is three levels up.
const repoRoot = path.join(__dirname, "..", "..", "..");

/**
 * Serves the repository root so the harness page can load the built wasm package under
 * `/bindings/wasm-sdk/pkg/` and the example HTML under `/bindings/wasm-sdk/examples/`.
 */
module.exports = defineConfig({
  testDir: ".",
  testMatch: "**/wasm_regular_rln*.spec.js",
  timeout: 240000,
  retries: 0,
  workers: 1,
  reporter: process.env.CI ? [["github"], ["list"]] : [["list"]],
  use: {
    baseURL: process.env.E2E_HTTP_BASE_URL || "http://127.0.0.1:8080",
    headless: true,
    trace: process.env.CI ? "retain-on-failure" : "off",
  },
  webServer: {
    command:
      'python3 -m http.server 8080 --bind 127.0.0.1 --directory "' +
      repoRoot.replace(/"/g, '\\"') +
      '"',
    url: "http://127.0.0.1:8080/",
    timeout: 120000,
    reuseExistingServer: !process.env.CI,
  },
});
