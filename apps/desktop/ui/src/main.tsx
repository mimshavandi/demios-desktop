import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { App } from "./components/App";
import { api, type Api } from "./ipc/commands";
import "./styles.css";

async function pickApi(): Promise<Api> {
  const inTauri = "__TAURI_INTERNALS__" in window;
  if (!inTauri && import.meta.env.DEV) {
    // `npm run dev` in a plain browser: talk to an in-memory mock instead of Rust.
    const { createMockApi } = await import("./ipc/mock");
    return createMockApi();
  }
  return api;
}

const root = document.getElementById("root");
if (root === null) throw new Error("missing #root");
const theme = new URLSearchParams(window.location.search).get("theme");
if (import.meta.env.DEV && (theme === "light" || theme === "dark")) {
  document.documentElement.dataset["theme"] = theme;
}
void pickApi().then((chosen) => {
  createRoot(root).render(
    <StrictMode>
      <App api={chosen} />
    </StrictMode>,
  );
});
