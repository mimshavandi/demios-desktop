import { useState } from "react";
import type { Conversation, Id } from "../ipc/types";

interface Props {
  conversations: Conversation[];
  activeId: Id | null;
  settingsOpen: boolean;
  devFake: boolean;
  onNew: () => void;
  onOpen: (id: Id) => void;
  onDelete: (id: Id) => void;
  onSettings: () => void;
}

export function Sidebar(p: Props) {
  // Deleting asks once, inline, on the row itself.
  const [confirmId, setConfirmId] = useState<Id | null>(null);
  return (
    <nav className="sidebar" aria-label="Conversations">
      <div className="sidebar-head">
        <span className="brand">Demios</span>
        <button type="button" className="btn btn-primary" onClick={p.onNew}>
          New chat
        </button>
      </div>
      {p.conversations.length === 0 ? (
        <p className="sidebar-empty">No conversations yet. Start one with New chat.</p>
      ) : (
        <ul className="conv-list">
          {p.conversations.map((c) => (
            <li key={c.id} className={c.id === p.activeId ? "conv active" : "conv"}>
              <button
                type="button"
                className="conv-open"
                aria-current={c.id === p.activeId ? "page" : undefined}
                onClick={() => p.onOpen(c.id)}
                title={c.title}
              >
                {c.title}
              </button>
              {confirmId === c.id ? (
                <span className="conv-confirm">
                  <button
                    type="button"
                    className="btn btn-danger btn-small"
                    onClick={() => {
                      setConfirmId(null);
                      p.onDelete(c.id);
                    }}
                  >
                    Delete
                  </button>
                  <button
                    type="button"
                    className="btn btn-ghost btn-small"
                    onClick={() => setConfirmId(null)}
                  >
                    Keep
                  </button>
                </span>
              ) : (
                <button
                  type="button"
                  className="conv-delete"
                  aria-label={`Delete “${c.title}”`}
                  title="Delete conversation"
                  onClick={() => setConfirmId(c.id)}
                >
                  ×
                </button>
              )}
            </li>
          ))}
        </ul>
      )}
      <div className="sidebar-foot">
        {p.devFake && <span className="badge">Dev fake model on</span>}
        <button
          type="button"
          className={p.settingsOpen ? "btn btn-ghost active" : "btn btn-ghost"}
          onClick={p.onSettings}
        >
          Settings
        </button>
      </div>
    </nav>
  );
}
