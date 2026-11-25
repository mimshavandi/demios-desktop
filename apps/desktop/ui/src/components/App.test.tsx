import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { Api } from "../ipc/commands";
import type {
  AddModelLink,
  ApprovalPrompt,
  ArtifactDetail,
  ChatEvent,
  Conversation,
  ModelCatalog,
  Settings,
  StoredMessage,
  LocalModelEvent,
  LocalModelRow,
  ToolEvent,
  UnknownOutcome,
} from "../ipc/types";
import { App } from "./App";

afterEach(cleanup);

// The real renderer needs a webview (constructable stylesheets, sandboxed frames); record
// what the pane hands it instead. Its own behaviour is tested in ui/artifact-sandbox.
const shown = vi.hoisted(() => ({
  artifacts: [] as { id: string; versions: unknown[] }[],
  madeBy: [] as (string | null)[],
  opts: null as null | { allowNetwork?: (id: string) => boolean; onAllowNetwork?: (id: string) => void },
}));
vi.mock("@dn-artifacts", () => ({
  ArtifactView: class {
    constructor(
      private el: HTMLElement,
      private opts: { madeBy?: (v: { author: string; model?: string | null }) => string | null },
    ) {
      this.el.append(Object.assign(document.createElement("div"), { textContent: "sandbox view" }));
      shown.opts = this.opts as typeof shown.opts;
    }
    setArtifact(a: { id: string; versions: { author: string; model?: string | null }[] }) {
      shown.artifacts.push(a);
      const last = a.versions[a.versions.length - 1];
      shown.madeBy.push(last ? (this.opts.madeBy?.(last) ?? null) : null);
      return Promise.resolve();
    }
    destroy() {}
  },
}));

/** In-memory stand-in for the Rust commands. */
function fakeApi() {
  const convs: Conversation[] = [];
  const trash: Conversation[] = [];
  let approvalSink: ((e: ToolEvent) => void) | null = null;
  const responses: [string, string][] = [];
  const resolved: [string, boolean][] = [];
  const unknowns: UnknownOutcome[] = [];
  let localSink: ((e: LocalModelEvent) => void) | null = null;
  const downloads: string[] = [];
  const pauses: string[] = [];
  const localRows: LocalModelRow[] = [
    {
      id: "m1",
      origin: "catalog",
      location: null,
      model_ref: "local:m1",
      name: "Tiny Model",
      repo: "org/tiny",
      quant: "Q4_K_M",
      size: 2_000_000_000,
      params_b: 1,
      capabilities: ["chat"],
      license: "cc-by-nc-4.0",
      license_warning: true,
      card_url: "https://huggingface.co/org/tiny",
      notes: "",
      state: { state: "not_downloaded" },
      fit: { status: "caution", estimated: true, reason: "Fits, with little memory left for other apps.", suggested_quant: null },
    },
  ];
  const msgs = new Map<string, StoredMessage[]>();
  const artifacts: ArtifactDetail[] = [];
  let settings: Settings = { slots: {}, telemetry_enabled: false };
  let n = 0;
  const reports: [string, string | null][] = [];
  const links: string[] = [];
  let listener: ((ev: ChatEvent) => void) | null = null;
  const id = () => `id-${++n}`;
  const m = (cid: string, role: "user" | "assistant", text: string, status: StoredMessage["status"]): StoredMessage => ({
    id: id(),
    conversation_id: cid,
    role,
    content: text === "" ? [] : [{ type: "text", text }],
    tool_calls: [],
    tool_call_id: null,
    model: "local:dev-fake",
    status,
    finish_reason: null,
    error: null,
    created_at: n,
    updated_at: n,
  });
  const catalog = (): ModelCatalog => ({
    slots: [{ slot_id: "chat", label: "Chat", description: "Answers", capability: "chat" }],
    models: [
      { model_ref: "local:dev-fake", label: "Dev fake", source: "dev", capabilities: ["chat"], available: true, note: null },
    ],
    providers: [{ id: "openai", name: "OpenAI", has_key: false }],
    local_models: localRows,
    key_store: "memory",
    settings,
    chat_model: null,
    chat_model_tools: true,
  });
  const api: Api = {
    appInfo: async () => ({ version: "0.1.0", dev_fake_model: true, data_dir: "/tmp", models_dir: "/tmp/models", updates: "off" }),
    checkForUpdate: async () => ({ status: "up_to_date", current: "0.1.0" }) as const,
    installUpdate: async () => null,
    hubSearch: async () => ({ items: [], next: null }),
    hubRepo: async () => {
      throw { code: "model_unavailable", message: "not in this test" };
    },
    hubPull: async () => "local:x",
    hubCancel: async () => true,
    hubPullStatus: async () => ({}),
    watchAddLinks: async () => null,
    addLinkDone: async () => null,
    hubResolveLink: async () => {
      throw { code: "model_unavailable", message: "not in this test" };
    },
    hfTokenStatus: async () => false,
    setHfToken: async () => true,
    deleteHfToken: async () => false,
    listMcpServers: async () => ({ servers: [], local_servers_allowed: true }),
    saveMcpServer: async () => ({ servers: [], local_servers_allowed: true }),
    removeMcpServer: async () => ({ servers: [], local_servers_allowed: true }),
    reconnectMcpServer: async () => null,
    signInMcpServer: async () => null,
    signOutMcpServer: async () => ({ servers: [], local_servers_allowed: true }),
    listMcpTools: async (serverId) => ({ server_id: serverId, connected: false, tools: [] }),
    setMcpToolChoice: async (serverId) => ({ server_id: serverId, connected: false, tools: [] }),
    markMcpToolsReviewed: async (serverId) => ({ server_id: serverId, connected: false, tools: [] }),
    openLink: async (url) => {
      links.push(url);
      return true;
    },
    reportProblem: async (kind, messageId) => {
      reports.push([kind, messageId ?? null]);
      return null;
    },
    localDataSummary: async () => ({
      conversations: convs.length,
      artifacts: artifacts.length,
      data_dir: "/tmp",
      models_dir: "/tmp/models",
      models_bytes: 4_200_000_000,
      keychain: ["OpenAI key"],
    }),
    deleteAllLocalData: async () => {
      convs.length = 0;
      return null;
    },
    listConversations: async () => [...convs],
    createConversation: async () => {
      const c = { id: id(), title: "New chat", model: null, created_at: n, updated_at: n };
      convs.unshift(c);
      msgs.set(c.id, []);
      return c;
    },
    renameConversation: async () => null,
    deleteConversation: async (cid) => {
      const i = convs.findIndex((c) => c.id === cid);
      const [c] = convs.splice(i, 1);
      if (c) trash.push(c);
      return null;
    },
    restoreConversation: async (cid) => {
      const i = trash.findIndex((c) => c.id === cid);
      const [c] = trash.splice(i, 1);
      if (c) convs.unshift(c);
      return null;
    },
    listMessages: async (cid) => [...(msgs.get(cid) ?? [])],
    sendMessage: async (cid, text, onEvent) => {
      listener = onEvent;
      const u = m(cid, "user", text, "complete");
      const a = m(cid, "assistant", "", "streaming");
      msgs.get(cid)?.push(u, a);
      const c = convs.find((x) => x.id === cid);
      if (c) c.title = text;
      return { user_message: u, assistant_message: a, title: text };
    },
    regenerate: async () => {
      throw { code: "invalid_input", message: "Nothing to regenerate." };
    },
    stopGeneration: async () => true,
    getSettings: async () => settings,
    updateSettings: async (s) => {
      settings = s;
      return s;
    },
    listModels: async () => catalog(),
    setProviderKey: async () => catalog(),
    deleteProviderKey: async () => catalog(),
    restoreProviderKey: async () => catalog(),
    watchLocalModels: async (onEvent) => {
      localSink = onEvent;
      return localRows;
    },
    downloadModel: async (id) => {
      downloads.push(id);
      return null;
    },
    pauseDownload: async (id) => {
      pauses.push(id);
      return null;
    },
    watchTools: async (onEvent) => {
      approvalSink = onEvent;
      return { approvals: [], servers: [] };
    },
    respondApproval: async (requestId, decision) => {
      responses.push([requestId, decision]);
      return null;
    },
    listArtifacts: async (cid) =>
      artifacts
        .filter((a) => a.conversation_id === cid)
        .map((a) => ({ id: a.id, conversation_id: cid, artifact_type: a.artifact_type, title: a.title, latest_n: a.versions.length, created_at: "", updated_at: "" })),
    getArtifact: async (_cid, aid) => {
      const a = artifacts.find((x) => x.id === aid);
      if (!a) throw { code: "not_found", message: "gone" };
      return structuredClone(a);
    },
    rollbackArtifact: async (_cid, aid, to) => {
      const a = artifacts.find((x) => x.id === aid);
      const src = a?.versions.find((v) => v.n === to);
      if (!a || !src) throw { code: "not_found", message: "gone" };
      a.versions.push({ ...src, n: a.versions.length + 1, parent_n: to, author: "user", model: null, message_id: null });
      return structuredClone(a);
    },
    listUnknownOutcomes: async () => [...unknowns],
    resolveUnknownOutcome: async (key, happened) => {
      resolved.push([key, happened]);
      unknowns.splice(0);
      return null;
    },
  };
  return {
    api,
    emit: (ev: ChatEvent) => listener?.(ev),
    msgs,
    approve: (e: ToolEvent) => approvalSink?.(e),
    responses,
    resolved,
    unknowns,
    artifacts,
    local: (e: LocalModelEvent) => localSink?.(e),
    downloads,
    pauses,
    localRows,
    reports,
    links,
  };
}

describe("App", () => {
  it("shows a card for an artifact the answer made and opens it in the pane", async () => {
    const { api, emit, artifacts } = fakeApi();
    render(<App api={api} />);
    const box = await screen.findByLabelText("Message");
    fireEvent.change(box, { target: { value: "Make an HTML artifact" } });
    fireEvent.keyDown(box, { key: "Enter" });
    expect(await screen.findByRole("button", { name: "Stop answer" })).toBeTruthy();
    const conv = "id-1";
    const assistantId = "id-3";
    artifacts.push({
      id: "art-1",
      conversation_id: conv,
      artifact_type: { kind: "html" },
      title: "Fake artifact",
      versions: [
        { n: 1, content: "<h1>Version one</h1>", author: "model", parent_n: null, message_id: assistantId, created_at: "2026-11-24T10:00:00Z", model: "local:dev-fake" },
      ],
    });
    act(() => {
      emit({ type: "artifact_changed", message_id: assistantId, conversation_id: conv, artifact_id: "art-1", title: "Fake artifact", version: 1 });
      emit({ type: "text_delta", message_id: assistantId, offset: 0, text: "Made it." });
      emit({ type: "done", message_id: assistantId, finish_reason: "stop", usage: { input_tokens: 1, output_tokens: 2 } });
    });
    const card = await screen.findByTestId("artifact-card");
    expect(card.textContent).toContain("Fake artifact");
    expect(card.textContent).toContain("HTML · v1");
    // The pane opened on it and named the model that made it.
    await waitFor(() => expect(shown.artifacts.at(-1)?.id).toBe("art-1"));
    expect(shown.madeBy.at(-1)).toBe("Dev fake");
    fireEvent.click(screen.getByRole("button", { name: "Open Fake artifact" }));
    expect(screen.getByTestId("artifact-host").hidden).toBe(false);

    // Network allowed for v1 doesn't carry over to a new version the model writes.
    shown.opts?.onAllowNetwork?.("art-1");
    expect(shown.opts?.allowNetwork?.("art-1")).toBe(true);
    const art = artifacts[0];
    if (!art) throw new Error("no artifact");
    art.versions.push({ ...art.versions[0]!, n: 2, parent_n: 1, content: "<h1>Version two</h1>" });
    act(() => {
      emit({ type: "artifact_changed", message_id: assistantId, conversation_id: conv, artifact_id: "art-1", title: "Fake artifact", version: 2 });
    });
    await waitFor(() => expect(shown.artifacts.at(-1)?.versions.length).toBe(2));
    expect(shown.opts?.allowNetwork?.("art-1")).toBe(false);
  });

  it("sends a message and renders the streamed answer as markdown", async () => {
    const { api, emit } = fakeApi();
    render(<App api={api} />);
    expect(await screen.findByText("What can I help with?")).toBeTruthy();
    expect(screen.getByTestId("artifact-pane")).toBeTruthy();
    expect(screen.getByText("No chats yet")).toBeTruthy();

    const box = screen.getByLabelText("Message");
    fireEvent.change(box, { target: { value: "Hello" } });
    fireEvent.keyDown(box, { key: "Enter" });

    expect(await screen.findByRole("button", { name: "Stop answer" })).toBeTruthy();
    const assistantId = "id-3";
    act(() => {
      emit({ type: "text_delta", message_id: assistantId, offset: 0, text: "Hi **there**" });
    });
    expect((await screen.findByText("there")).tagName).toBe("STRONG");
    act(() => {
      emit({ type: "done", message_id: assistantId, finish_reason: "stop", usage: { input_tokens: 1, output_tokens: 2 } });
    });
    expect(await screen.findByRole("button", { name: "Retry" })).toBeTruthy();
    await waitFor(() => expect(screen.getByRole("button", { name: "Hello" })).toBeTruthy());
    // The model badge says where the answer ran.
    expect(screen.getAllByText(/On this device/).length).toBeGreaterThan(0);
  });

  it("discloses AI answers: first-run notice, composer line, labelled answers", async () => {
    const { api, emit } = fakeApi();
    let saved: Settings | null = null;
    const update = api.updateSettings;
    api.updateSettings = async (s) => {
      saved = s;
      return update(s);
    };
    render(<App api={api} />);
    expect(await screen.findByText(/Demios lets you chat with AI models you choose/)).toBeTruthy();
    // The persistent line is part of the composer's description for screen readers.
    const box = screen.getByLabelText("Message");
    expect(box.getAttribute("aria-describedby")).toContain("composer-disclosure");
    expect(document.getElementById("composer-disclosure")?.textContent).toContain("can be wrong");

    fireEvent.click(screen.getByRole("button", { name: "Got it" }));
    await waitFor(() => expect(screen.queryByText(/Demios lets you chat with AI models you choose/)).toBeNull());
    expect(document.activeElement).toBe(box);
    expect(saved).toMatchObject({ ai_notice_seen: true });
    // The line by the composer stays.
    expect(document.getElementById("composer-disclosure")).toBeTruthy();

    fireEvent.change(box, { target: { value: "Hello" } });
    fireEvent.keyDown(box, { key: "Enter" });
    await screen.findByRole("button", { name: "Stop answer" });
    act(() => {
      emit({ type: "text_delta", message_id: "id-3", offset: 0, text: "Hi" });
      emit({ type: "done", message_id: "id-3", finish_reason: "stop", usage: { input_tokens: 1, output_tokens: 1 } });
    });
    const answer = await screen.findByTestId("message-assistant");
    expect(answer.getAttribute("data-ai-generated")).toBe("true");
    expect(answer.getAttribute("data-generator")).toBe("local:dev-fake");
    expect(answer.getAttribute("aria-label")).toMatch(/^AI answer from /);
  });

  it("reports a problem or an answer without sending chat content", async () => {
    const { api, emit, reports, msgs } = fakeApi();
    render(<App api={api} />);
    const box = await screen.findByLabelText("Message");
    fireEvent.change(box, { target: { value: "private question" } });
    fireEvent.keyDown(box, { key: "Enter" });
    await screen.findByRole("button", { name: "Stop answer" });
    // The backend stores the finished answer; the list reloads from it.
    const stored = [...msgs.values()].flat().find((x) => x.role === "assistant");
    if (stored) {
      stored.content = [{ type: "text", text: "an answer" }];
      stored.status = "complete";
    }
    act(() => {
      emit({ type: "text_delta", message_id: "id-3", offset: 0, text: "an answer" });
      emit({ type: "done", message_id: "id-3", finish_reason: "stop", usage: { input_tokens: 1, output_tokens: 1 } });
    });
    // Wait until the chat has settled (the list reloads after the answer ends).
    await waitFor(() => expect(screen.getByRole("button", { name: "private question" })).toBeTruthy());
    fireEvent.click(await screen.findByRole("button", { name: "Report this answer" }));
    await waitFor(() => expect(reports).toEqual([["answer", "id-3"]]));
    expect(await screen.findByText("Opened the report form in your browser")).toBeTruthy();

    fireEvent.click(await screen.findByRole("button", { name: "Models" }));
    fireEvent.click(await screen.findByRole("tab", { name: "General" }));
    fireEvent.click(await screen.findByRole("button", { name: "Report a problem" }));
    await waitFor(() => expect(reports).toEqual([["answer", "id-3"], ["problem", null]]));
    // Only kinds and message ids went to the backend: no chat text.
    expect(JSON.stringify(reports)).not.toContain("private question");
  });

  it("a demios:// link opens the add-model confirmation and downloads nothing by itself", async () => {
    const { api } = fakeApi();
    const calls: string[] = [];
    api.watchAddLinks = async () => ({ repo: "org/Tiny-GGUF", file: null, quant: "Q8_0", revision: null });
    api.hubResolveLink = async (link) => {
      calls.push(`resolve ${link.repo} ${link.quant ?? ""}`);
      return {
        id: "org/Tiny-GGUF",
        sha: "c".repeat(40),
        gated: false,
        license: { id: "apache-2.0", name: null, link: null, warning: null },
        card_url: "https://huggingface.co/org/Tiny-GGUF",
        selected: "Tiny-Q8_0",
        fit_unknown: false,
        variants: [
          { name: "Tiny-Q4_K_M", quant: "Q4_K_M", total_size: 1e9, parts: 1, verifiable: true, fit: null, downloaded: null, progress: null },
          { name: "Tiny-Q8_0", quant: "Q8_0", total_size: 2e9, parts: 1, verifiable: true, fit: null, downloaded: null, progress: null },
        ],
      };
    };
    api.hubPull = async () => {
      calls.push("pull");
      return "local:x";
    };
    api.addLinkDone = async () => {
      calls.push("done");
      return null;
    };
    render(<App api={api} />);
    expect(await screen.findByText(/A link asked to add this model/)).toBeTruthy();
    const variants = await screen.findAllByTestId("hub-variant");
    expect(variants[0]!.textContent).toContain("Chosen by the link");
    expect(variants[0]!.getAttribute("data-selected")).toBe("true");
    expect(calls).toEqual(["resolve org/Tiny-GGUF Q8_0"]);
    expect(calls).not.toContain("pull");

    fireEvent.click(screen.getByRole("button", { name: "Not now" }));
    await waitFor(() => expect(calls).toContain("done"));
    expect(screen.queryByText(/A link asked to add this model/)).toBeNull();
    expect(calls).not.toContain("pull");
  });

  it("a link mid-chat keeps the composer draft", async () => {
    const { api } = fakeApi();
    let deliver: ((l: AddModelLink) => void) | null = null;
    api.watchAddLinks = async (on) => {
      deliver = on;
      return null;
    };
    api.hubResolveLink = async () => {
      throw { code: "model_unavailable", message: "offline in this test" };
    };
    render(<App api={api} />);
    const box = (await screen.findByLabelText("Message")) as HTMLTextAreaElement;
    fireEvent.change(box, { target: { value: "half-written question" } });
    await waitFor(() => expect(deliver).not.toBeNull());
    act(() => deliver?.({ repo: "org/Tiny-GGUF", file: null, quant: null, revision: null }));
    fireEvent.click(await screen.findByRole("button", { name: "Not now" }));
    fireEvent.click(screen.getAllByRole("button", { name: /New chat/ })[0]!);
    const back = (await screen.findByLabelText("Message")) as HTMLTextAreaElement;
    expect(back.value).toBe("half-written question");
  });

  it("About opens the privacy policy and accessibility statement through the link confirm", async () => {
    const { api, links } = fakeApi();
    let fail = false;
    api.openLink = async (url) => {
      links.push(url);
      if (fail) throw { code: "unavailable", message: "Couldn't open your browser." };
      return false; // the user cancelled the native confirm
    };
    render(<App api={api} />);
    fireEvent.click(await screen.findByRole("button", { name: "Models" }));
    fireEvent.click(await screen.findByRole("tab", { name: "General" }));
    fireEvent.click(await screen.findByRole("button", { name: "Privacy policy" }));
    fireEvent.click(await screen.findByRole("button", { name: "Accessibility statement" }));
    await waitFor(() => expect(links).toEqual(["https://demios.io/privacy", "https://demios.io/accessibility"]));
    expect(screen.queryByText(/Couldn't open the page/)).toBeNull();
    expect(screen.queryByRole("button", { name: /Impressum|Imprint|Legal notice/ })).toBeNull();
    fail = true;
    fireEvent.click(screen.getByRole("button", { name: "Privacy policy" }));
    expect(await screen.findByText("Couldn't open the page. Couldn't open your browser.")).toBeTruthy();
  });

  it("links in answers go to the backend to confirm and open, never navigate the app", async () => {
    const { api, emit, msgs, links } = fakeApi();
    let refuse = false;
    api.openLink = async (url) => {
      links.push(url);
      if (refuse) throw { code: "invalid_input", message: "Only web links (http and https) open from answers." };
      return true;
    };
    render(<App api={api} />);
    const box = await screen.findByLabelText("Message");
    fireEvent.change(box, { target: { value: "Where?" } });
    fireEvent.keyDown(box, { key: "Enter" });
    await screen.findByRole("button", { name: "Stop answer" });
    const text =
      "See [the docs](https://example.com/docs) or [mail us](mailto:a@example.com), https://auto.example/x and [rel](/x).";
    const stored = [...msgs.values()].flat().find((x) => x.role === "assistant");
    if (stored) {
      stored.content = [{ type: "text", text }];
      stored.status = "complete";
    }
    act(() => {
      emit({ type: "text_delta", message_id: "id-3", offset: 0, text });
      emit({ type: "done", message_id: "id-3", finish_reason: "stop", usage: { input_tokens: 1, output_tokens: 1 } });
    });
    await waitFor(() => expect(screen.getByRole("button", { name: "Where?" })).toBeTruthy());
    const before = window.location.href;
    const docs = await screen.findByRole("link", { name: "the docs" });
    const notPrevented = fireEvent.click(docs);
    expect(notPrevented).toBe(false); // default navigation cancelled
    expect(links).toEqual(["https://example.com/docs"]);
    expect(window.location.href).toBe(before);

    // Ctrl/Cmd-click is a normal click: same confirmed path, no new window.
    expect(fireEvent.click(docs, { ctrlKey: true })).toBe(false);
    expect(fireEvent.click(docs, { metaKey: true })).toBe(false);
    expect(links).toEqual(["https://example.com/docs", "https://example.com/docs", "https://example.com/docs"]);
    // Middle-click and drag can't bypass the confirmation.
    expect(fireEvent(docs, new MouseEvent("auxclick", { bubbles: true, cancelable: true, button: 1 }))).toBe(false);
    expect(docs.getAttribute("draggable")).toBe("false");
    expect(fireEvent.dragStart(docs)).toBe(false);
    expect(links.length).toBe(3);
    // A bare URL is autolinked and takes the same path.
    fireEvent.click(screen.getByRole("link", { name: "https://auto.example/x" }));
    expect(links[3]).toBe("https://auto.example/x");
    // A relative link loses its href when sanitised: not a link, clicking does nothing.
    expect(screen.queryByRole("link", { name: "rel" })).toBeNull();
    fireEvent.click(screen.getByText("rel"));
    expect(links.length).toBe(4);
    links.splice(0);

    // Only http(s) opens: the backend refuses anything else and the user is told why.
    refuse = true;
    fireEvent.click(screen.getByRole("link", { name: "mail us" }));
    expect(links[0]).toBe("mailto:a@example.com");
    expect(await screen.findByText("Only web links (http and https) open from answers.")).toBeTruthy();
  });

  it("does not show the first-run notice once seen", async () => {
    const { api } = fakeApi();
    await api.updateSettings({ slots: {}, telemetry_enabled: false, ai_notice_seen: true });
    render(<App api={api} />);
    expect(await screen.findByText("What can I help with?")).toBeTruthy();
    expect(screen.queryByText(/Demios lets you chat with AI models you choose/)).toBeNull();
    expect(document.getElementById("composer-disclosure")).toBeTruthy();
  });

  it("keeps the prompt when sending fails", async () => {
    const { api } = fakeApi();
    api.sendMessage = async () => {
      throw { code: "busy", message: "An answer is already streaming in this chat." };
    };
    render(<App api={api} />);
    const box = (await screen.findByLabelText("Message")) as HTMLTextAreaElement;
    fireEvent.change(box, { target: { value: "keep me" } });
    fireEvent.keyDown(box, { key: "Enter" });
    expect(await screen.findByText("An answer is already streaming in this chat.")).toBeTruthy();
    expect(box.value).toBe("keep me");
  });

  it("deletes a chat at once and restores it with undo", async () => {
    const { api } = fakeApi();
    await api.createConversation();
    let deletes = 0;
    let restores = 0;
    const del = api.deleteConversation;
    const res = api.restoreConversation;
    api.deleteConversation = async (id) => {
      deletes++;
      return del(id);
    };
    api.restoreConversation = async (id) => {
      restores++;
      return res(id);
    };
    render(<App api={api} />);
    fireEvent.click(await screen.findByRole("button", { name: "Delete “New chat”" }));
    expect(await screen.findByText("Deleted “New chat”")).toBeTruthy();
    expect(deletes).toBe(1);
    await waitFor(() => expect(screen.queryByRole("button", { name: "Delete “New chat”" })).toBeNull());
    fireEvent.click(screen.getByRole("button", { name: "Undo" }));
    expect(await screen.findByRole("button", { name: "Delete “New chat”" })).toBeTruthy();
    expect(restores).toBe(1);
    expect(deletes).toBe(1);
  });

  it("shows models and settings with telemetry off", async () => {
    const { api } = fakeApi();
    render(<App api={api} />);
    fireEvent.click(await screen.findByRole("button", { name: "Models" }));
    expect(await screen.findByText("Model for each task")).toBeTruthy();
    expect(screen.getByLabelText("Chat")).toBeTruthy();
    const key = screen.getByLabelText("OpenAI") as HTMLInputElement;
    expect(key.type).toBe("password");
    expect(screen.getByText(/Messages in chats that use OpenAI models are sent to OpenAI/)).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Save key" }));
    expect(await screen.findByText("Paste the key first.")).toBeTruthy();
    expect(key.getAttribute("aria-invalid")).toBe("true");

    fireEvent.click(screen.getByRole("tab", { name: "General" }));
    expect(screen.getByRole("tabpanel").getAttribute("aria-labelledby")).toBe("tab-general");
    expect(screen.getByText(/Read-only tools run without asking/)).toBeTruthy();
    // No inert telemetry switch; a plain statement instead (T-STORE-11).
    expect(await screen.findByText(/Demios collects no usage data/)).toBeTruthy();
    expect(screen.queryByRole("switch")).toBeNull();
    expect(screen.getByText("Where your data goes")).toBeTruthy();
    expect(screen.getByText("Downloaded models are stored in /tmp/models")).toBeTruthy();
  });

  it("deletes all local data only after a confirm, and cancel changes nothing", async () => {
    const { api } = fakeApi();
    let calls = 0;
    let fail = true;
    api.deleteAllLocalData = async () => {
      calls++;
      if (fail) throw { code: "partial", message: "Some data could not be deleted: keys in the system keychain. Try again." };
      return null;
    };
    render(<App api={api} />);
    fireEvent.click(await screen.findByRole("button", { name: "Models" }));
    fireEvent.click(await screen.findByRole("tab", { name: "General" }));
    const trigger = await screen.findByRole("button", { name: "Delete all local data" });

    fireEvent.click(trigger);
    const dialog = await screen.findByRole("alertdialog", { name: "Delete all local data?" });
    expect(dialog.textContent).toContain("can't be undone");
    await waitFor(() => expect(dialog.textContent).toContain("4.2 GB"));
    expect(dialog.textContent).toContain("/tmp/models");
    expect(dialog.textContent).toContain("OpenAI key");
    // Focus starts on Cancel; the red button is armed only after a short guard.
    expect(document.activeElement?.textContent).toBe("Cancel");
    const confirm = screen.getByRole("button", { name: "Delete all data" }) as HTMLButtonElement;
    expect(confirm.disabled).toBe(true);
    fireEvent.click(confirm);
    expect(calls).toBe(0);
    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
    await waitFor(() => expect(screen.queryByRole("alertdialog")).toBeNull());
    expect(calls).toBe(0);
    await waitFor(() => expect(document.activeElement).toBe(trigger));

    // Esc closes it too, and deletes nothing.
    fireEvent.click(trigger);
    const again = await screen.findByRole("alertdialog");
    fireEvent.keyDown(again, { key: "Escape" });
    await waitFor(() => expect(screen.queryByRole("alertdialog")).toBeNull());
    expect(calls).toBe(0);

    fireEvent.click(trigger);
    const armed = (await screen.findByRole("button", { name: "Delete all data" })) as HTMLButtonElement;
    await waitFor(() => expect(armed.disabled).toBe(false), { timeout: 1500 });
    fireEvent.click(armed);
    // A partial failure stays in the dialog and says what is left.
    expect(await screen.findByText(/keys in the system keychain/)).toBeTruthy();
    expect(screen.getByRole("alertdialog")).toBeTruthy();

    fail = false;
    fireEvent.click(screen.getByRole("button", { name: "Delete all data" }));
    expect(await screen.findByText("All local data deleted")).toBeTruthy();
    expect(calls).toBe(2);
  });

  it("keeps the delete button disabled until it knows what will be deleted", async () => {
    const { api } = fakeApi();
    let finish: (() => void) | null = null;
    const summary = api.localDataSummary;
    api.localDataSummary = () =>
      new Promise((resolve) => {
        finish = () => void summary().then(resolve);
      });
    render(<App api={api} />);
    fireEvent.click(await screen.findByRole("button", { name: "Models" }));
    fireEvent.click(await screen.findByRole("tab", { name: "General" }));
    fireEvent.click(await screen.findByRole("button", { name: "Delete all local data" }));
    const confirm = (await screen.findByRole("button", { name: "Delete all data" })) as HTMLButtonElement;
    // Well past the 500 ms guard, still disabled: the list isn't known yet.
    await new Promise((r) => setTimeout(r, 700));
    expect(confirm.disabled).toBe(true);
    act(() => finish?.());
    await waitFor(() => expect(confirm.disabled).toBe(false));
  });

  it("shows the build channel and commit on the About screen", async () => {
    const { api } = fakeApi();
    api.appInfo = async () => ({
      version: "0.1.0",
      dev_fake_model: false,
      data_dir: null,
      models_dir: null,
      commit: "1a2b3c4",
      channel: "preview",
      updates: "off",
    });
    render(<App api={api} />);
    fireEvent.click(await screen.findByRole("button", { name: "Models" }));
    fireEvent.click(await screen.findByRole("tab", { name: "General" }));
    expect(await screen.findByText("Preview build (unsigned), commit 1a2b3c4")).toBeTruthy();
  });

  it("asks to connect a model when none is available", async () => {
    const { api } = fakeApi();
    api.appInfo = async () => ({ version: "0.1.0", dev_fake_model: false, data_dir: null, models_dir: null, updates: "off" });
    render(<App api={api} />);
    expect(await screen.findByText(/No model connected/)).toBeTruthy();
    expect((screen.getByLabelText("Message") as HTMLTextAreaElement).disabled).toBe(true);
    expect(screen.getAllByRole("button", { name: "Connect a model" }).length).toBeGreaterThan(0);
  });

  it("shows an approval card with the exact arguments and never auto-allows", async () => {
    const f = fakeApi();
    const c = await f.api.createConversation();
    render(<App api={f.api} />);
    await screen.findByRole("button", { name: "New chat" });
    const prompt = (risk: ApprovalPrompt["risk"]): ApprovalPrompt => ({
      request_id: `r-${risk}`,
      scope: `conv:${c.id}`,
      capability_id: "mcp.files.write_file",
      title: "Write to notes/todo.md",
      description: "Replaces the file.",
      risk,
      policy: "ask_once_per_conversation",
      arguments: { path: "notes/todo.md", content: "milk" },
    });
    act(() => f.approve({ type: "requested", prompt: prompt("low_write") }));
    const card = await screen.findByTestId("approval-card");
    expect(card.textContent).toContain('"path": "notes/todo.md"');
    expect(screen.getByText("Changes local data")).toBeTruthy();
    expect(screen.getByRole("button", { name: /Always allow in this chat/ })).toBeTruthy();
    expect(f.responses).toEqual([]);
    // Keys in the first 500 ms after the card appears are ignored.
    fireEvent.keyDown(card, { key: "Enter", metaKey: true, ctrlKey: true });
    expect(f.responses).toEqual([]);
    await act(() => new Promise((r) => setTimeout(r, 550)));
    // ⌘↵ / Ctrl+↵ allows once.
    fireEvent.keyDown(card, { key: "Enter", metaKey: true, ctrlKey: true });
    await waitFor(() => expect(f.responses).toEqual([["r-low_write", "approve"]]));
    await waitFor(() => expect(screen.queryByTestId("approval-card")).toBeNull());

    // Destructive: no "always allow"; Esc denies.
    act(() => f.approve({ type: "requested", prompt: prompt("destructive") }));
    const card2 = await screen.findByTestId("approval-card");
    expect(screen.queryByRole("button", { name: /Always allow in this chat/ })).toBeNull();
    await act(() => new Promise((r) => setTimeout(r, 550)));
    fireEvent.keyDown(card2, { key: "Escape" });
    await waitFor(() => expect(f.responses[1]).toEqual(["r-destructive", "deny"]));
  });

  it("asks the user to resolve an unknown outcome", async () => {
    const f = fakeApi();
    await f.api.createConversation();
    f.unknowns.push({
      key: "k1",
      capability_id: "mcp.mail.send",
      arguments: { to: "pat@example.com" },
      error: "interrupted",
      updated_at_ms: 1,
    });
    render(<App api={f.api} />);
    expect(await screen.findByText("Did this go through?")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "It didn't happen" }));
    await waitFor(() => expect(f.resolved).toEqual([["k1", false]]));
    await waitFor(() => expect(screen.queryByText("Did this go through?")).toBeNull());
  });

  it("renders stored tool calls as cards with their results", async () => {
    const f = fakeApi();
    const c = await f.api.createConversation();
    const base = {
      conversation_id: c.id,
      tool_call_id: null,
      model: "byok:anthropic:claude-sonnet-5-5",
      status: "complete" as const,
      error: null,
      created_at: 1,
      updated_at: 1,
    };
    f.msgs.set(c.id, [
      { ...base, id: "u", role: "user", content: [{ type: "text", text: "save it" }], tool_calls: [], finish_reason: null },
      {
        ...base,
        id: "a1",
        role: "assistant",
        content: [],
        tool_calls: [{ id: "call_1", name: "mcp__files__write_file", arguments: { path: "x.md" } }],
        finish_reason: "tool_calls",
      },
      { ...base, id: "t", role: "tool", tool_call_id: "call_1", content: [{ type: "text", text: "Wrote 1 file." }], tool_calls: [], finish_reason: null },
      { ...base, id: "a2", role: "assistant", content: [{ type: "text", text: "Saved." }], tool_calls: [], finish_reason: "stop" },
    ]);
    render(<App api={f.api} />);
    const card = await screen.findByTestId("tool-call-card");
    expect(card.textContent).toContain("files · write_file");
    expect(card.textContent).toContain("Done");
    expect(screen.queryByText("Wrote 1 file.")).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: /write_file/ }));
    expect(await screen.findByText("Wrote 1 file.")).toBeTruthy();
    expect(screen.getByText("Saved.")).toBeTruthy();
  });

  it("never approves when the user is typing in the composer as a card appears", async () => {
    const f = fakeApi();
    const c = await f.api.createConversation();
    render(<App api={f.api} />);
    const box = (await screen.findByLabelText("Message")) as HTMLTextAreaElement;
    box.focus();
    fireEvent.change(box, { target: { value: "next message" } });
    act(() =>
      f.approve({
        type: "requested",
        prompt: {
          request_id: "r1",
          scope: `conv:${c.id}`,
          capability_id: "mcp.files.write_file",
          title: "Write to notes/todo.md",
          description: "",
          risk: "low_write",
          policy: "ask_once_per_conversation",
          arguments: { path: "notes/todo.md" },
        },
      }),
    );
    await screen.findByTestId("approval-card");
    expect(document.activeElement).toBe(box);
    fireEvent.keyDown(box, { key: "Enter", metaKey: true, ctrlKey: true });
    await act(() => new Promise((r) => setTimeout(r, 50)));
    expect(f.responses).toEqual([]);
    expect(screen.getByTestId("approval-card")).toBeTruthy();
    expect(await screen.findByText("Approval needed: Write to notes/todo.md")).toBeTruthy();
  });

  it("tells the user when an approval expired and when a tool server failed", async () => {
    const f = fakeApi();
    const c = await f.api.createConversation();
    render(<App api={f.api} />);
    await screen.findByRole("button", { name: "New chat" });
    act(() =>
      f.approve({
        type: "requested",
        prompt: {
          request_id: "r2",
          scope: `conv:${c.id}`,
          capability_id: "mcp.files.write_file",
          title: "Write",
          description: "",
          risk: "low_write",
          policy: "always_ask",
          arguments: {},
        },
      }),
    );
    await screen.findByTestId("approval-card");
    act(() => f.approve({ type: "resolved", request_id: "r2", expired: true }));
    expect(await screen.findByText(/approval request expired/)).toBeTruthy();
    expect(screen.queryByTestId("approval-card")).toBeNull();

    act(() =>
      f.approve({
        type: "mcp_server",
        status: { server_id: "fs", name: "Files", state: "failed", message: "It didn't start.", tools: 0, review: 0 },
      }),
    );
    expect(await screen.findByText("Files couldn't connect. It didn't start.")).toBeTruthy();
  });

  it("ignores clicks on a card's buttons in its first half second", async () => {
    const f = fakeApi();
    const c = await f.api.createConversation();
    render(<App api={f.api} />);
    await screen.findByRole("button", { name: "New chat" });
    act(() =>
      f.approve({
        type: "requested",
        prompt: {
          request_id: "r3",
          scope: `conv:${c.id}`,
          capability_id: "mcp.files.write_file",
          title: "Write",
          description: "",
          risk: "low_write",
          policy: "always_ask",
          arguments: {},
        },
      }),
    );
    const allow = await screen.findByRole("button", { name: /Allow once/ });
    // e.g. the second click of a double-click aimed at the previous card.
    fireEvent.click(allow);
    await act(() => new Promise((r) => setTimeout(r, 20)));
    expect(f.responses).toEqual([]);
    await act(() => new Promise((r) => setTimeout(r, 550)));
    fireEvent.click(allow);
    await waitFor(() => expect(f.responses).toEqual([["r3", "approve"]]));
  });

  it("downloads a local model with progress, pause and a licence warning", async () => {
    const f = fakeApi();
    render(<App api={f.api} />);
    fireEvent.click(await screen.findByRole("button", { name: "Models" }));
    const row = await screen.findByTestId("local-model-row");
    expect(row.textContent).toContain("Caution (estimate)");
    expect(row.textContent).toContain("Licence: cc-by-nc-4.0");
    expect(screen.getByText(/Check the licence before downloading/)).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Download model" }));
    await waitFor(() => expect(f.downloads).toEqual(["m1"]));

    const base = f.localRows[0]!;
    act(() =>
      f.local({
        type: "changed",
        row: { ...base, state: { state: "downloading", done: 500_000_000, total: 2_000_000_000, bytes_per_sec: 10_000_000 } },
      }),
    );
    const bar = await screen.findByRole("progressbar");
    expect(bar.getAttribute("aria-valuenow")).toBe("500000000");
    expect(screen.getByText(/500 MB of 2.0 GB · 3 min left/)).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Pause" }));
    await waitFor(() => expect(f.pauses).toEqual(["m1"]));

    act(() =>
      f.local({ type: "changed", row: { ...base, state: { state: "failed", message: "Couldn't reach Hugging Face.", done: 0, total: 2_000_000_000 } } }),
    );
    expect(await screen.findByText("Couldn't reach Hugging Face.")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Retry" })).toBeTruthy();

    act(() => f.local({ type: "changed", row: { ...base, state: { state: "queued", resuming: true } } }));
    expect(await screen.findByText("Checking the partial download…")).toBeTruthy();

    act(() => f.local({ type: "changed", row: { ...base, state: { state: "ready" } } }));
    expect(await screen.findByText("On this device", { selector: ".dn-badge" })).toBeTruthy();
    expect(await screen.findByText("Tiny Model downloaded")).toBeTruthy();
  });
});
