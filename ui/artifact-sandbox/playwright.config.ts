import { defineConfig, devices } from "@playwright/test";

// Chromium stands in for WebView2 (Windows); WebKit for WKWebView (macOS) and WebKitGTK (Linux).
export default defineConfig({
  testDir: "tests",
  fullyParallel: false,
  workers: 1,
  retries: 0,
  reporter: [["list"]],
  use: {
    baseURL: "http://127.0.0.1:4173",
    trace: "retain-on-failure",
  },
  projects: [
    { name: "chromium", use: { ...devices["Desktop Chrome"] } },
    { name: "webkit", use: { ...devices["Desktop Safari"] } },
  ],
  webServer: {
    command: "node scripts/serve.mjs",
    url: "http://127.0.0.1:4173/__hits",
    reuseExistingServer: false,
    stdout: "ignore",
  },
});
