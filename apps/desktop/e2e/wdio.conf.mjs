// Real-app e2e (D-015, T-SHELL-05): WebdriverIO talks to tauri-driver, which launches the
// built app through the platform's WebDriver (WebKitWebDriver on Linux, msedgedriver on
// Windows). macOS has no WKWebView WebDriver: see README.md for the manual checklist.
//
// Env:
//   DEMIOS_E2E_APP   path to the built app (default: target/debug/demios-desktop[.exe])
//   TAURI_DRIVER     path to tauri-driver (default: ~/.cargo/bin/tauri-driver[.exe])
//   MSEDGEDRIVER     Windows: path to msedgedriver matching the installed WebView2
//   DEMIOS_E2E_WIN_DEVTOOLS  Windows: how msedgedriver finds the WebView2 DevTools endpoint,
//                    any of `port`, `udd` joined by `+` (default `port`). See README.md.

import { spawn } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));
const win = process.platform === "win32";
const exe = win ? ".exe" : "";

const application =
  process.env.DEMIOS_E2E_APP ?? path.resolve(here, "../../../target/debug", `demios-desktop${exe}`);
const tauriDriver =
  process.env.TAURI_DRIVER ?? path.join(os.homedir(), ".cargo", "bin", `tauri-driver${exe}`);

// One data dir for the whole run, so the relaunched app sees the same history. The app
// inherits this environment through tauri-driver and the native driver.
const dataDir = fs.mkdtempSync(path.join(os.tmpdir(), "demios-e2e-"));
process.env.DEMIOS_DATA_DIR = dataDir;
// Slow the dev fake model down so Stop lands mid-answer.
process.env.DEMIOS_FAKE_DELAY_MS ??= "80";

// Windows (T-SHELL-06): msedgedriver starts WebView2 with `--remote-debugging-port=0` and then
// looks for the `DevToolsActivePort` file in the user-data folder it expects. Tauri gives
// WebView2 its own folder (%LOCALAPPDATA%\<identifier>\EBWebView), so the file lands
// elsewhere and session creation times out. Browser args reach WebView2 through
// tauri:options.args -> ms:edgeOptions.args:
//   port: a fixed --remote-debugging-port, so the driver connects to it directly;
//   udd:  --user-data-dir set to the app's real WebView2 folder, so the driver looks there.
const winDevtools = (process.env.DEMIOS_E2E_WIN_DEVTOOLS ?? "port").split("+");
const winArgs = [];
if (win) {
  if (winDevtools.includes("udd")) {
    const ebwebview = path.join(process.env.LOCALAPPDATA ?? "", "io.demios.app", "EBWebView");
    winArgs.push(`--user-data-dir=${ebwebview}`);
  }
  if (winDevtools.includes("port")) winArgs.push("--remote-debugging-port=9222");
}

let driver;

export const config = {
  runner: "local",
  specs: ["./specs/**/*.e2e.mjs"],
  maxInstances: 1,
  hostname: "127.0.0.1",
  port: 4444,
  capabilities: [
    {
      maxInstances: 1,
      // tauri-driver speaks classic WebDriver only.
      "wdio:enforceWebDriverClassic": true,
      "tauri:options": { application, ...(winArgs.length ? { args: winArgs } : {}) },
    },
  ],
  logLevel: "warn",
  framework: "mocha",
  reporters: ["spec"],
  mochaOpts: { ui: "bdd", timeout: 180_000 },
  waitforTimeout: 30_000,

  onPrepare() {
    if (!fs.existsSync(application)) {
      throw new Error(
        `App not built at ${application}. Build it with: npm --prefix apps/desktop run build && cargo build -p demios-desktop --features dev-fake-model,custom-protocol`,
      );
    }
    console.log(`e2e data dir: ${dataDir}`);
    if (win) console.log(`WebView2 DevTools strategy: ${winDevtools.join("+")} -> ${JSON.stringify(winArgs)}`);
  },

  // tauri-driver stays up for the whole worker, so reloadSession() relaunches the app
  // against the same data dir.
  async beforeSession() {
    const args = [];
    if (win && process.env.MSEDGEDRIVER) args.push("--native-driver", process.env.MSEDGEDRIVER);
    driver = spawn(tauriDriver, args, { stdio: [null, process.stdout, process.stderr] });
    await new Promise((r) => setTimeout(r, 1500));
  },

  afterSession() {
    driver?.kill();
  },

  onComplete() {
    fs.rmSync(dataDir, { recursive: true, force: true });
  },
};
