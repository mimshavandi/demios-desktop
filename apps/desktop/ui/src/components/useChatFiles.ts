import { useCallback, useEffect, useRef, useState } from "react";
import type { Api } from "../ipc/commands";
import { errorMessage, type ChatFile, type Id } from "../ipc/types";
import type { Attachments } from "./ChatView";
import type { useToast } from "./toast";

/** How long a removed file can be brought back (design: ChatAttachments). */
export const UNDO_MS = 10_000;

/**
 * The chat's data files for the composer (#370): loads them with the chat, attaches
 * through the system picker, and removes with Undo. With no chat open, attaching creates
 * the chat only after a real pick, and `onCreated` opens it.
 */
export function useChatFiles({
  api,
  conversationId,
  projectId,
  provider,
  toast,
  onCreated,
  focusComposer,
}: {
  api: Api;
  conversationId: Id | null;
  /** The project a new chat would start in. */
  projectId: Id | null;
  provider: string | null;
  toast: ReturnType<typeof useToast>;
  onCreated: (id: Id) => Promise<void>;
  focusComposer: () => void;
}): Attachments {
  const [files, setFiles] = useState<ChatFile[]>([]);
  const [skipped, setSkipped] = useState<{ name: string; reason: string }[]>([]);
  const [busy, setBusy] = useState(false);
  // Names picked and still being copied: one chip each until the copies are made.
  const [copying, setCopying] = useState<string[]>([]);
  const [status, setStatus] = useState("");
  const openId = useRef(conversationId);
  openId.current = conversationId;
  // The chat an attach just created: switching to it keeps what that attach reported.
  const created = useRef<Id | null>(null);

  const load = useCallback(
    async (id: Id) => {
      try {
        const list = await api.listChatFiles(id);
        if (openId.current === id) setFiles(list);
      } catch (e) {
        if (openId.current === id) toast({ tone: "error", title: `Couldn't load this chat's files. ${errorMessage(e)}` });
      }
    },
    [api, toast],
  );

  useEffect(() => {
    if (conversationId === null || conversationId !== created.current) {
      setFiles([]);
      setSkipped([]);
      setStatus("");
    }
    created.current = null;
    if (conversationId !== null) void load(conversationId);
  }, [conversationId, load]);

  const onAttach = useCallback(() => {
    void (async () => {
      setBusy(true);
      setSkipped([]);
      try {
        const r = await api.addChatFiles(conversationId, conversationId === null ? projectId : null, (p) => {
          setCopying(p.names);
          const [one] = p.names;
          setStatus(p.names.length === 1 && one !== undefined ? `Adding ${one}` : `Adding ${p.names.length} files`);
        });
        if (r.cancelled) return;
        if (conversationId === null && r.conversation_id !== null) {
          created.current = r.conversation_id;
          await onCreated(r.conversation_id);
        }
        setFiles((f) => [...f, ...r.added]);
        setSkipped(r.skipped);
        const [only] = r.added;
        setStatus(
          r.added.length === 0
            ? ""
            : r.added.length === 1 && only !== undefined
              ? `${only.name} attached`
              : `${r.added.length} files attached`,
        );
        focusComposer();
      } catch (e) {
        toast({ tone: "error", title: `Couldn't attach files. ${errorMessage(e)}` });
      } finally {
        setCopying([]);
        setBusy(false);
      }
    })();
  }, [api, conversationId, projectId, onCreated, focusComposer, toast]);

  const onRemove = useCallback(
    (f: ChatFile) => {
      const id = f.conversation_id;
      setFiles((list) => list.filter((x) => x.id !== f.id));
      void api
        .removeChatFile(id, f.id)
        .then(() => {
          // The Toast is the announcement; the composer's status says only what attaching did.
          toast({
            tone: "info",
            title: `${f.name} removed`,
            duration: UNDO_MS,
            action: {
              label: "Undo",
              onAction: () => {
                void api
                  .restoreChatFile(id, f.id)
                  .then(() => {
                    if (openId.current === id) return load(id);
                    return undefined;
                  })
                  .catch((e: unknown) =>
                    toast({ tone: "error", title: `Couldn't bring ${f.name} back. ${errorMessage(e)}` }),
                  );
              },
            },
          });
        })
        .catch((e: unknown) => {
          // Nothing was removed: the chip comes back, with the reason.
          if (openId.current === id) void load(id);
          toast({ tone: "error", title: `Couldn't remove ${f.name}. ${errorMessage(e)}` });
        });
    },
    [api, load, toast],
  );

  return {
    files,
    skipped,
    busy,
    copying,
    provider,
    status,
    onAttach,
    onRemove,
    onDismissSkipped: () => setSkipped([]),
  };
}
