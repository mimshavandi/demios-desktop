import { Folder, FolderInput, FolderPlus, MessageSquarePlus, MoreHorizontal, Pencil, Settings, Trash2 } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { errorMessage, type Conversation, type Id, type ProjectSummary } from "../ipc/types";
import { mod } from "../platform";
import { DropdownMenu } from "./DropdownMenu";
import { Button, EmptyState, ICON, IconButton, InlineMessage, Skeleton } from "./primitives";

export type ListState = { kind: "loading" } | { kind: "ready" } | { kind: "error"; message: string };

interface Props {
  conversations: Conversation[];
  state: ListState;
  activeId: Id | null;
  projects: ProjectSummary[];
  projectsState: ListState;
  /** The open project page ("new" while creating one), if any. */
  activeProject: Id | "new" | null;
  onNewProject: () => void;
  onOpenProject: (id: Id) => void;
  onRetryProjects: () => void;
  /** Settings is open (any page). */
  settingsOpen: boolean;
  onNew: () => void;
  onOpen: (id: Id) => void;
  onDelete: (c: Conversation) => void;
  /** Moves a chat into a project, or out of one with `null` (with Undo). */
  onMove: (c: Conversation, projectId: Id | null) => Promise<void>;
  /** Renames a chat; rejects with the reason if it couldn't be saved. */
  onRename: (c: Conversation, title: string) => Promise<void>;
  onRetry: () => void;
  /** Opens Settings on the page used last. */
  onSettings: () => void;
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

      <div className="conv-scroll">
        <div className="group-head">
          <h2 className="group" id="projects-h">
            Projects
          </h2>
          <IconButton
            size="sm"
            label="New project"
            onClick={p.onNewProject}
            aria-current={p.activeProject === "new" ? "page" : undefined}
            data-testid="new-project-button"
          >
            <FolderPlus {...ICON} aria-hidden="true" />
          </IconButton>
        </div>
        {p.projectsState.kind === "loading" && (
          <ul className="conv-list" aria-busy="true" aria-label="Loading projects">
            {[60, 40].map((w) => (
              <li key={w} className="conv conv-skeleton">
                <Skeleton width={`${w}%`} />
              </li>
            ))}
          </ul>
        )}
        {p.projectsState.kind === "error" && (
          <InlineMessage
            tone="error"
            action={
              <Button size="sm" onClick={p.onRetryProjects}>
                Retry
              </Button>
            }
          >
            Couldn't load projects. {p.projectsState.message}
          </InlineMessage>
        )}
        {p.projectsState.kind === "ready" && p.projects.length === 0 && (
          <p className="dn-help group-empty">Group chats with their own files and instructions.</p>
        )}
        {p.projectsState.kind === "ready" && p.projects.length > 0 && (
          <ul className="conv-list" aria-labelledby="projects-h">
            {p.projects.map((x) => (
              <li key={x.id} className={x.id === p.activeProject ? "conv active" : "conv"}>
                <button
                  type="button"
                  className="conv-open project-open"
                  data-testid="project-item"
                  aria-current={x.id === p.activeProject ? "page" : undefined}
                  onClick={() => p.onOpenProject(x.id)}
                  title={x.name}
                >
                  <Folder {...ICON} aria-hidden="true" />
                  <span className="project-open-name">{x.name}</span>
                </button>
              </li>
            ))}
          </ul>
        )}

        <h2 className="group" id="recent-h">
          Recent
        </h2>
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
          <ul className="conv-list" aria-labelledby="recent-h">
            {p.conversations.map((c) => (
              <ChatRow
                key={c.id}
                c={c}
                active={c.id === p.activeId}
                projects={p.projects}
                onOpen={p.onOpen}
                onDelete={p.onDelete}
                onMove={p.onMove}
                onRename={p.onRename}
              />
            ))}
          </ul>
        )}
      </div>

      <div className="sidebar-foot">
        <button
          type="button"
          className={p.settingsOpen ? "nav active" : "nav"}
          aria-current={p.settingsOpen ? "page" : undefined}
          onClick={() => p.onSettings()}
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

const TITLE_MAX = 200;

/** A chat in the sidebar, with its menu: move to project, rename (inline), delete (#166). */
function ChatRow({
  c,
  active,
  projects,
  onOpen,
  onDelete,
  onMove,
  onRename,
}: {
  c: Conversation;
  active: boolean;
  projects: ProjectSummary[];
  onOpen: (id: Id) => void;
  onDelete: (c: Conversation) => void;
  onMove: (c: Conversation, projectId: Id | null) => Promise<void>;
  onRename: (c: Conversation, title: string) => Promise<void>;
}) {
  const [renaming, setRenaming] = useState(false);
  const [draft, setDraft] = useState(c.title);
  const [error, setError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const inputRef = useRef<HTMLInputElement>(null);
  const openRef = useRef<HTMLButtonElement>(null);
  const errorId = `rename-${c.id}-error`;

  useEffect(() => {
    if (renaming) inputRef.current?.select();
  }, [renaming]);

  const stop = () => {
    setRenaming(false);
    setError(null);
    // Focus back on the row, whose name is now the (new) title.
    requestAnimationFrame(() => openRef.current?.focus());
  };

  const save = async () => {
    const t = draft.trim();
    if (t === c.title) return stop();
    if (t === "" || t.length > TITLE_MAX) {
      setError(`A chat name is 1–${TITLE_MAX} characters.`);
      return;
    }
    setSaving(true);
    try {
      await onRename(c, t);
      stop();
    } catch (e) {
      // The text stays, so nothing typed is lost.
      setError(`Couldn't rename the chat. ${errorMessage(e)}`);
    } finally {
      setSaving(false);
    }
  };

  if (renaming) {
    return (
      <li className={active ? "conv active" : "conv"}>
        <form
          className="conv-rename"
          onSubmit={(e) => {
            e.preventDefault();
            void save();
          }}
        >
          <input
            ref={inputRef}
            aria-label="Chat name"
            value={draft}
            maxLength={TITLE_MAX + 1}
            disabled={saving}
            aria-invalid={error !== null || undefined}
            aria-describedby={error !== null ? errorId : undefined}
            onChange={(e) => {
              setDraft(e.target.value);
              setError(null);
            }}
            onKeyDown={(e) => {
              if (e.key === "Escape") {
                e.preventDefault();
                setDraft(c.title);
                stop();
              }
            }}
            onBlur={() => {
              if (!saving && error === null) void save();
            }}
          />
          {error !== null && (
            <span id={errorId} className="dn-field-error conv-rename-error" role="alert">
              {error}
            </span>
          )}
        </form>
      </li>
    );
  }

  return (
    <li className={active ? "conv active" : "conv"}>
      <button
        ref={openRef}
        type="button"
        className="conv-open"
        data-testid="conversation-item"
        aria-current={active ? "page" : undefined}
        onClick={() => onOpen(c.id)}
        title={c.title}
      >
        {c.title}
      </button>
      <DropdownMenu
        label={`More actions for “${c.title}”`}
        triggerClassName="conv-menu"
        testId="conversation-menu"
        trigger={<MoreHorizontal {...ICON} aria-hidden="true" />}
        entries={[
          ...(projects.length > 0
            ? [
                {
                  kind: "choice" as const,
                  label: "Move to project",
                  icon: <FolderInput {...ICON} aria-hidden="true" />,
                  groupLabel: "Project",
                  value: c.project_id ?? "",
                  options: [{ value: "", label: "No project" }, ...projects.map((x) => ({ value: x.id, label: x.name }))],
                  failure: "Couldn't move the chat",
                  onChoose: (v: string) => onMove(c, v === "" ? null : v),
                },
              ]
            : []),
          {
            kind: "item",
            label: "Rename",
            icon: <Pencil {...ICON} aria-hidden="true" />,
            failure: "Couldn't rename the chat",
            run: () => {
              setDraft(c.title);
              setRenaming(true);
            },
            focusAfter: () => inputRef.current,
          },
          { kind: "separator" },
          {
            kind: "item",
            label: "Delete",
            icon: <Trash2 {...ICON} aria-hidden="true" />,
            danger: true,
            failure: "Couldn't delete the chat",
            run: () => onDelete(c),
            // The row goes; focus moves to New chat rather than being lost.
            focusAfter: () => document.querySelector<HTMLElement>('[data-testid="new-chat"]'),
          },
        ]}
      />
    </li>
  );
}
