/// <reference types="vitest/config" />
import { fileURLToPath } from "node:url";
import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

const repo = (p: string) => fileURLToPath(new URL(`../../${p}`, import.meta.url));

// The Tauri window loads the dev server in `tauri dev` and `dist/` in `tauri build`.
export default defineConfig({
  root: "ui",
  plugins: [react()],
  clearScreen: false,
  resolve: {
    alias: {
      // Generated from dn-core by ts-rs (crates/dn-core/bindings, T-CORE-04).
      "@dn-core": repo("crates/dn-core/bindings"),
      // Shared design tokens (ui/design-tokens, ADR-0010).
      "@dn-tokens": repo("ui/design-tokens"),
      // Artifact renderer (ui/artifact-sandbox, ADR-0004), as built by `npm run sandbox`:
      // its in-frame runtimes are prebuilt text, and each renderer is a lazy chunk.
      "@dn-artifacts": repo("ui/artifact-sandbox/dist/index.js"),
    },
  },
  server: {
    port: 1420,
    strictPort: true,
    host: "127.0.0.1",
    fs: {
      allow: [
        repo("apps/desktop"),
        repo("ui/design-tokens"),
        repo("crates/dn-core/bindings"),
        repo("ui/artifact-sandbox/dist"),
      ],
    },
  },
  build: { outDir: "../dist", emptyOutDir: true, target: "es2022" },
  test: {
    environment: "jsdom",
    // No built sandbox needed for unit tests (CI's ui job doesn't build it).
    alias: { "@dn-artifacts": fileURLToPath(new URL("./ui/src/test/dn-artifacts-stub.ts", import.meta.url)) },
    include: ["src/**/*.test.{ts,tsx}"],
  },
});
