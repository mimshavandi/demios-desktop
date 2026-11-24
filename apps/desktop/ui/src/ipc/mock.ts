// Dev-only stand-in for the Rust commands, used when the UI runs in a plain browser
// (`npm run dev` without Tauri). Never bundled into the app: main.tsx imports it only
// when `import.meta.env.DEV` and no Tauri runtime is present.

import type { Api } from "./commands";
import type { ChatEvent, Conversation, ModelCatalog, Settings, StoredMessage } from "./types";

const REPLY = (q: string, turn: number) =>
  `**Dev fake model** (turn ${turn}). No real model is connected yet, so this reply is canned.\n\n` +
  `You said:\n\n> ${q}\n\nA code block, to check highlighting:\n\n` +
  "```rust\nfn main() {\n    let answer = 6 * 7;\n    println!(\"the answer is {answer}\");\n}\n```\n\n" +
  "- Pick a real model in **Models** once local inference or a provider key is available.\n" +
  "- *Stop* and *Retry* work on this reply too.";

export function createMockApi(): Api {
  const convs: Conversation[] = [];
  const msgs = new Map<string, StoredMessage[]>();
  const keys = new Set<string>();
  const deleted = new Set<string>();
  const removedKeys = new Set<string>();
  let settings: Settings = { slots: {}, telemetry_enabled: false };
  const timers = new Map<string, number>();
  const id = () => crypto.randomUUID();
  const now = () => Date.now();

  const message = (cid: string, role: StoredMessage["role"], text: string, status: StoredMessage["status"]): StoredMessage => ({
    id: id(),
    conversation_id: cid,
    role,
    content: text === "" ? [] : [{ type: "text", text }],
    tool_calls: [],
    tool_call_id: null,
    model: role === "assistant" ? "local:dev-fake" : null,
    status,
    finish_reason: null,
    error: null,
    created_at: now(),
    updated_at: now(),
  });

  const catalog = (): ModelCatalog => ({
    slots: [
      { slot_id: "chat", label: "Chat", description: "Answers in conversations.", capability: "chat" },
      { slot_id: "fast", label: "Fast", description: "Titles, summaries and other quick background tasks.", capability: "chat" },
      { slot_id: "embed", label: "Embeddings", description: "Knowledge search (arrives with dn-knowledge).", capability: "embedding" },
    ],
    models: [
      { model_ref: "local:dev-fake", label: "Dev fake model (canned replies)", source: "dev", capabilities: ["chat"], available: true, note: null },
      ...[...keys].flatMap((p) =>
        (p === "anthropic" ? ["claude-sonnet-5-5", "claude-haiku-5-5"] : ["gpt-5"]).map((m) => ({
          model_ref: `byok:${p}:${m}`,
          label: `${p === "anthropic" ? "Anthropic" : p === "openai" ? "OpenAI" : "Google Gemini"} · ${m}`,
          source: "byok" as const,
          capabilities: ["chat" as const],
          available: false,
          note: "Provider backend not in this build yet (dn-providers).",
        })),
      ),
    ],
    providers: [
      { id: "anthropic", name: "Anthropic", has_key: keys.has("anthropic") },
      { id: "openai", name: "OpenAI", has_key: keys.has("openai") },
      { id: "gemini", name: "Google Gemini", has_key: keys.has("gemini") },
    ],
    local_models: [
      { model_id: "qwen3-8b-q4_k_m", label: "Qwen3 8B · Q4_K_M", size: "5.0 GB", note: "Downloads and local inference arrive with dn-models / dn-inference." },
      { model_id: "llama-3.2-3b-q4_k_m", label: "Llama 3.2 3B · Q4_K_M", size: "2.0 GB", note: "Downloads and local inference arrive with dn-models / dn-inference." },
    ],
    key_store: "session only (browser mock)",
    settings,
  });

  const stream = (cid: string, a: StoredMessage, q: string, onEvent: (e: ChatEvent) => void) => {
    const turn = (msgs.get(cid) ?? []).filter((m) => m.role === "user").length;
    const words = REPLY(q, turn).split(/(?<=[ \n])/);
    let i = 0;
    let text = "";
    const tick = () => {
      if (i >= words.length) {
        timers.delete(cid);
        a.content = [{ type: "text", text }];
        a.status = "complete";
        onEvent({ type: "done", message_id: a.id, finish_reason: "stop", usage: { input_tokens: 1, output_tokens: i } });
        return;
      }
      const w = words[i++] ?? "";
      const offset = text.length;
      text += w;
      a.content = [{ type: "text", text }];
      onEvent({ type: "text_delta", message_id: a.id, offset, text: w });
      timers.set(cid, window.setTimeout(tick, 18));
    };
    timers.set(cid, window.setTimeout(tick, 120));
    return () => {
      window.clearTimeout(timers.get(cid));
      timers.delete(cid);
      a.status = "cancelled";
      onEvent({ type: "done", message_id: a.id, finish_reason: "cancelled", usage: { input_tokens: 0, output_tokens: 0 } });
    };
  };
  const stops = new Map<string, () => void>();

  return {
    appInfo: async () => ({ version: "0.1.0 (browser mock)", dev_fake_model: true, data_dir: null }),
    listConversations: async () =>
      convs.filter((c) => !deleted.has(c.id)).sort((x, y) => y.updated_at - x.updated_at),
    createConversation: async () => {
      const c = { id: id(), title: "New chat", model: null, created_at: now(), updated_at: now() };
      convs.push(c);
      msgs.set(c.id, []);
      return c;
    },
    renameConversation: async () => null,
    deleteConversation: async (cid) => {
      deleted.add(cid);
      return null;
    },
    restoreConversation: async (cid) => {
      deleted.delete(cid);
      return null;
    },
    listMessages: async (cid) => structuredClone(msgs.get(cid) ?? []),
    sendMessage: async (cid, text, onEvent) => {
      const c = convs.find((x) => x.id === cid);
      const title = c !== undefined && c.title === "New chat" ? text.slice(0, 48) : null;
      if (c !== undefined) {
        if (title !== null) c.title = title;
        c.updated_at = now();
      }
      const u = message(cid, "user", text, "complete");
      const a = message(cid, "assistant", "", "streaming");
      msgs.get(cid)?.push(u, a);
      stops.set(cid, stream(cid, a, text, onEvent));
      // IPC serialises; hand out copies so the UI never shares objects with the mock.
      return structuredClone({ user_message: u, assistant_message: a, title });
    },
    regenerate: async (cid, onEvent) => {
      const list = msgs.get(cid) ?? [];
      const lastUser = list.map((m) => m.role).lastIndexOf("user");
      list.splice(lastUser + 1);
      const a = message(cid, "assistant", "", "streaming");
      list.push(a);
      stops.set(cid, stream(cid, a, list[lastUser] ? (list[lastUser].content[0] as { text: string }).text : "", onEvent));
      return structuredClone({ user_message: null, assistant_message: a, title: null });
    },
    stopGeneration: async (cid) => {
      const s = stops.get(cid);
      stops.delete(cid);
      if (s !== undefined && timers.has(cid)) {
        s();
        return true;
      }
      return false;
    },
    getSettings: async () => settings,
    updateSettings: async (s) => {
      settings = s;
      return s;
    },
    listModels: async () => catalog(),
    setProviderKey: async (p) => {
      keys.add(p);
      return catalog();
    },
    deleteProviderKey: async (p) => {
      keys.delete(p);
      removedKeys.add(p);
      return catalog();
    },
    restoreProviderKey: async (p) => {
      if (!removedKeys.delete(p)) throw { code: "expired", message: "The key can no longer be restored. Paste it again." };
      keys.add(p);
      return catalog();
    },
  };
}
