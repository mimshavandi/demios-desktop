import { Boxes, Folder, FolderPlus, MessageSquarePlus, Settings, Trash2 } from "lucide-react";
import type { Conversation, Id, ProjectSummary } from "../ipc/types";
import { mod } from "../platform";
import { Button, EmptyState, ICON, IconButton, InlineMessage, Skeleton } from "./primitives";
import type { SettingsSection } from "./SettingsView";

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
