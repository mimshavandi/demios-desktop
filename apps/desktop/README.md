# Demios Desktop (Tauri 2 shell)

Rust side in `src/` (typed IPC commands, chat streaming, settings), UI in `ui/` (React 19 + strict TypeScript, Radix primitives, `--dn-*` design tokens from `ui/design-tokens`). ADR-0002, design standard `docs/design/design-system.md`.

```sh
cd apps/desktop
npm ci
npm run dev:app            # dev server + app window, with the dev fake model
npm run tauri dev          # same, without a model (shows "Connect a model")
npm run typecheck && npm test
npx tauri build --debug --bundles app   # macOS .app in target/debug/bundle/macos
```

- `DEMIOS_DATA_DIR=/tmp/demios-profile` runs against a throwaway database.
- Cargo feature `dev-fake-model` (off by default) registers `local:dev-fake` (T-SHELL-03), a canned streaming model, so the app answers before real backends land. `build.rs` only enables it in debug builds (`cfg(fake_model)`), so a release build never contains it.
- IPC: every command is listed in `build.rs` and granted in `capabilities/default.json`; the webview gets no plugins. The TS signatures are in `ui/src/ipc/commands.ts`; `dn-core` types come from `crates/dn-core/bindings` (ts-rs).
