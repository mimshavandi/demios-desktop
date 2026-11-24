import { Boxes, MessageSquarePlus, Settings, Trash2 } from "lucide-react";
import type { Conversation, Id } from "../ipc/types";
import { mod } from "../platform";
import { Button, EmptyState, ICON, IconButton, InlineMessage, Skeleton } from "./primitives";
import type { SettingsSection } from "./SettingsView";

export type ListState = { kind: "loading" } | { kind: "ready" } | { kind: "error"; message: string };

interface Props {
  conversations: Conversation[];
  state: ListState;
  activeId: Id | null;
  settings: SettingsSection | null;
  onNew: () => void;
  onOpen: (id: Id) => void;
  onDelete: (c: Conversation) => void;
  onRetry: () => void;
  onSettings: (s: SettingsSection) => void;
}

export function Sidebar(p: Props) {
  return (
    <nav className="sidebar" aria-label="Chats">
      <button
        type="button"
        className="nav nav-new"
        onClick={p.onNew}
        aria-keyshortcuts={`${mod}N`}
        data-testid="new-chat"
      >
        <MessageSquarePlus {...ICON} aria-hidden="true" />
        New chat
        <kbd>{mod}N</kbd>
      </button>

      <h2 className="group">Recent</h2>
      <div className="conv-scroll">
        {p.state.kind === "loading" && (
          <ul className="conv-list" aria-busy="true" aria-label="Loading chats">
            {[70, 55, 80, 45].map((w) => (
              <li key={w} className="conv conv-skeleton">
                <Skeleton width={`${w}%`} />
              </li>
            ))}
          </ul>
        )}
        {p.state.kind === "error" && (
          <InlineMessage
            tone="error"
            action={
              <Button size="sm" onClick={p.onRetry}>
                Retry
              </Button>
            }
          >
            Couldn't load chats. {p.state.message}
          </InlineMessage>
        )}
        {p.state.kind === "ready" && p.conversations.length === 0 && (
          <EmptyState title="No chats yet">Your conversations stay on this computer.</EmptyState>
        )}
        {p.state.kind === "ready" && p.conversations.length > 0 && (
          <ul className="conv-list">
            {p.conversations.map((c) => (
              <li key={c.id} className={c.id === p.activeId ? "conv active" : "conv"}>
                <button
                  type="button"
                  className="conv-open"
                  data-testid="conversation-item"
                  aria-current={c.id === p.activeId ? "page" : undefined}
                  onClick={() => p.onOpen(c.id)}
                  title={c.title}
                >
                  {c.title}
                </button>
                <IconButton
                  size="sm"
                  className="conv-delete"
                  label={`Delete “${c.title}”`}
                  onClick={() => p.onDelete(c)}
                >
                  <Trash2 {...ICON} aria-hidden="true" />
                </IconButton>
              </li>
            ))}
          </ul>
        )}
      </div>

      <div className="sidebar-foot">
        <button
          type="button"
          className={p.settings === "models" ? "nav active" : "nav"}
          aria-current={p.settings === "models" ? "page" : undefined}
          onClick={() => p.onSettings("models")}
        >
          <Boxes {...ICON} aria-hidden="true" />
          Models
        </button>
        <button
          type="button"
          className={p.settings === "general" ? "nav active" : "nav"}
          aria-current={p.settings === "general" ? "page" : undefined}
          onClick={() => p.onSettings("general")}
          aria-keyshortcuts={`${mod},`}
        >
          <Settings {...ICON} aria-hidden="true" />
          Settings
          <kbd>{mod},</kbd>
        </button>
      </div>
    </nav>
  );
}
