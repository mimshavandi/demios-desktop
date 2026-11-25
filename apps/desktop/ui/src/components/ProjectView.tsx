import { FilePlus, FileText, MessageSquarePlus, Trash2 } from "lucide-react";
import { useCallback, useEffect, useId, useRef, useState } from "react";
import type { Api } from "../ipc/commands";
import {
  errorMessage,
  PROJECT_INSTRUCTIONS_MAX,
  PROJECT_NAME_MAX,
  type Conversation,
  type Id,
  type Project,
  type ProjectFile,
} from "../ipc/types";
import { formatBytes } from "./LocalModels";
import { Button, ICON, IconButton, InlineMessage, Skeleton, TextField } from "./primitives";
import { useToast } from "./toast";

/** Unsaved edits of a project's name and instructions, kept while the user looks elsewhere. */
export interface ProjectDraft {
  name: string;
  instructions: string;
}

type Load<T> = { kind: "loading" } | { kind: "ready"; value: T } | { kind: "error"; message: string };

/**
 * A new project: only a name is asked for; nothing is stored until "Create project".
 */
export function NewProjectView({ api, onCreated }: { api: Api; onCreated: (p: Project) => void }) {
  const [name, setName] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const inputRef = useRef<HTMLInputElement>(null);
  useEffect(() => inputRef.current?.focus(), []);

  const create = async () => {
    const n = name.trim();
    if (n === "") {
      setError("Enter a name for the project.");
      inputRef.current?.focus();
      return;
    }
    setBusy(true);
    try {
      onCreated(await api.createProject(n));
    } catch (e) {
      setError(errorMessage(e));
      inputRef.current?.focus();
    } finally {
      setBusy(false);
    }
  };

  return (
    <section className="settings" aria-labelledby="new-project-h">
      <div className="settings-column project-page" data-testid="new-project">
        <h1 id="new-project-h">New project</h1>
        <p className="dn-help">
          A project keeps related chats together, with files and instructions that apply to every chat in it.
        </p>
        <form
          className="card project-form"
          noValidate
          onSubmit={(e) => {
            e.preventDefault();
            void create();
          }}
        >
          <TextField
            ref={inputRef}
            label="Name"
            value={name}
            maxLength={PROJECT_NAME_MAX}
            error={error}
            onChange={(e) => {
              setName(e.target.value);
              if (error !== null) setError(null);
            }}
          />
          <div className="project-form-actions">
            <Button type="submit" variant="primary" loading={busy} disabled={busy}>
              Create project
            </Button>
          </div>
        </form>
      </div>
    </section>
  );
}

/**
 * The project page (T-PRJ-01): name and standing instructions, files, and the
 * project's chats. Deleting the project acts at once and offers Undo (handled by the app).
 */
export function ProjectView({
  api,
  project,
  chats,
  draft,
  onDraft,
  onSaved,
  onNewChat,
  onOpenChat,
  onDelete,
  onFilesChanged,
}: {
  api: Api;
  project: Project;
  /** Live chats of this project, newest first. */
  chats: Conversation[];
  draft: ProjectDraft | null;
  onDraft: (d: ProjectDraft | null) => void;
  onSaved: (p: Project) => void;
  onNewChat: () => void;
  onOpenChat: (id: Id) => void;
  onDelete: () => void;
  onFilesChanged: () => void;
}) {
  const toast = useToast();
  const name = draft?.name ?? project.name;
  const instructions = draft?.instructions ?? project.instructions;
  const dirty = draft !== null && (draft.name !== project.name || draft.instructions !== project.instructions);
  const [nameError, setNameError] = useState<string | null>(null);
  const [saveError, setSaveError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const nameRef = useRef<HTMLInputElement>(null);
  const instructionsId = useId();
  const countId = useId();
  const over = instructions.length > PROJECT_INSTRUCTIONS_MAX;
  const headingRef = useRef<HTMLHeadingElement>(null);

  // A project opened: reset messages and move focus to its heading (the view changed).
  useEffect(() => {
    setNameError(null);
    setSaveError(null);
    headingRef.current?.focus();
  }, [project.id]);

  const edit = (d: Partial<ProjectDraft>) => {
    setSaveError(null);
    onDraft({ name, instructions, ...d });
  };

  const save = async () => {
    if (name.trim() === "") {
      setNameError("Enter a name for the project.");
      nameRef.current?.focus();
      return;
    }
    if (over) {
      setSaveError(`Instructions can be at most ${PROJECT_INSTRUCTIONS_MAX.toLocaleString()} characters.`);
      return;
    }
    setSaving(true);
    try {
      const p = await api.updateProject(project.id, name, instructions);
      onDraft(null);
      onSaved(p);
      toast({ tone: "success", title: "Project saved" });
    } catch (e) {
      // The draft stays, so nothing typed is lost.
      setSaveError(errorMessage(e));
    } finally {
      setSaving(false);
    }
  };

  return (
    <section className="settings" aria-labelledby={`${instructionsId}-title`}>
      <div className="settings-column project-page" data-testid="project-view">
        <div className="project-head">
          <h1 id={`${instructionsId}-title`} className="project-title" ref={headingRef} tabIndex={-1}>
            {project.name}
          </h1>
          <Button variant="primary" icon={<MessageSquarePlus {...ICON} aria-hidden="true" />} onClick={onNewChat}>
            New chat
          </Button>
        </div>

        <form
          className="card project-form"
          aria-labelledby={`${instructionsId}-h`}
          noValidate
          onSubmit={(e) => {
            e.preventDefault();
            void save();
          }}
        >
          <h2 id={`${instructionsId}-h`}>Details</h2>
          <TextField
            ref={nameRef}
            label="Name"
            value={name}
            maxLength={PROJECT_NAME_MAX}
            error={nameError}
            onChange={(e) => {
              setNameError(null);
              edit({ name: e.target.value });
            }}
          />
          <div className={`dn-field${over ? " is-invalid" : ""}`}>
            <label htmlFor={instructionsId}>Instructions</label>
            <textarea
              id={instructionsId}
              className="project-instructions"
              rows={6}
              value={instructions}
              aria-invalid={over || undefined}
              aria-describedby={countId}
              placeholder="For example: Answer in British English and cite the files you use."
              onChange={(e) => edit({ instructions: e.target.value })}
            />
            <span id={countId} className={over ? "dn-field-error" : "dn-field-help"}>
              Sent with every chat in this project. {instructions.length.toLocaleString()} of{" "}
              {PROJECT_INSTRUCTIONS_MAX.toLocaleString()} characters.
            </span>
          </div>
          {saveError !== null && <InlineMessage tone="error">Couldn't save the project. {saveError}</InlineMessage>}
          <div className="project-form-actions">
            {dirty && (
              <span className="dn-help" role="status">
                Unsaved changes
              </span>
            )}
            <Button type="submit" variant={dirty ? "primary" : "secondary"} loading={saving} disabled={saving}>
              Save changes
            </Button>
          </div>
        </form>

        <ProjectFiles api={api} projectId={project.id} onChanged={onFilesChanged} />

        <section className="card" aria-labelledby={`${instructionsId}-chats`}>
          <h2 id={`${instructionsId}-chats`}>Chats</h2>
          {chats.length === 0 ? (
            <p className="dn-help project-empty">
              No chats yet. Chats you start with New chat on this page get the project's instructions.
            </p>
          ) : (
            <ul className="project-list" data-testid="project-chats">
              {chats.map((c) => (
                <li key={c.id}>
                  <button type="button" className="project-chat" onClick={() => onOpenChat(c.id)}>
                    {c.title}
                  </button>
                </li>
              ))}
            </ul>
          )}
        </section>

        <section className="card project-danger">
          <div className="row">
            <div className="row-main">
              <span className="dn-help">
                Deleting the project also deletes its chats and files. You can undo it until Demios restarts.
              </span>
            </div>
            <div className="row-actions">
              <Button variant="danger" icon={<Trash2 {...ICON} aria-hidden="true" />} onClick={onDelete}>
                Delete project
              </Button>
            </div>
          </div>
        </section>
      </div>
    </section>
  );
}

function ProjectFiles({ api, projectId, onChanged }: { api: Api; projectId: Id; onChanged: () => void }) {
  const toast = useToast();
  const [files, setFiles] = useState<Load<ProjectFile[]>>({ kind: "loading" });
  const [adding, setAdding] = useState(false);
  const [skipped, setSkipped] = useState<{ name: string; reason: string }[]>([]);
  const addRef = useRef<HTMLButtonElement>(null);
  const headingId = useId();

  const load = useCallback(async () => {
    setFiles({ kind: "loading" });
    try {
      setFiles({ kind: "ready", value: await api.listProjectFiles(projectId) });
    } catch (e) {
      setFiles({ kind: "error", message: errorMessage(e) });
    }
  }, [api, projectId]);

  useEffect(() => {
    setSkipped([]);
    void load();
  }, [load]);

  const add = async () => {
    setAdding(true);
    setSkipped([]);
    try {
      const r = await api.addProjectFiles(projectId);
      if (r.cancelled) return;
      setSkipped(r.skipped);
      if (r.added.length > 0) {
        setFiles((f) => (f.kind === "ready" ? { kind: "ready", value: [...f.value, ...r.added] } : f));
        toast({
          tone: "success",
          title: r.added.length === 1 ? `Added “${r.added[0]?.name ?? ""}”` : `Added ${r.added.length} files`,
        });
        onChanged();
      }
    } catch (e) {
      toast({ tone: "error", title: `Couldn't add files. ${errorMessage(e)}` });
    } finally {
      setAdding(false);
      addRef.current?.focus();
    }
  };

  const remove = async (f: ProjectFile) => {
    try {
      await api.removeProjectFile(projectId, f.id);
    } catch (e) {
      toast({ tone: "error", title: `Couldn't remove the file. ${errorMessage(e)}` });
      return;
    }
    setFiles((s) => (s.kind === "ready" ? { kind: "ready", value: s.value.filter((x) => x.id !== f.id) } : s));
    // The row is gone: keep focus in the section.
    addRef.current?.focus();
    onChanged();
    toast({
      tone: "info",
      title: `Removed “${f.name}”`,
      duration: 10_000,
      action: {
        label: "Undo",
        onAction: () => {
          void api
            .restoreProjectFile(projectId, f.id)
            .then(() => {
              onChanged();
              return load();
            })
            .catch((e: unknown) => toast({ tone: "error", title: `Couldn't restore the file. ${errorMessage(e)}` }));
        },
      },
    });
  };

  return (
    <section className="card" aria-labelledby={headingId} data-testid="project-files">
      <div className="project-section-head">
        <h2 id={headingId}>Files</h2>
        <Button
          ref={addRef}
          icon={<FilePlus {...ICON} aria-hidden="true" />}
          loading={adding}
          disabled={adding}
          onClick={() => void add()}
        >
          Add files
        </Button>
      </div>
      <p className="dn-help">
        Demios keeps a copy on this computer, so moving or changing the original doesn't affect it. Up to 50 MB per
        file.
      </p>
      {skipped.length > 0 && (
        <InlineMessage tone="warning">
          {skipped.length === 1 ? "1 file wasn't added:" : `${skipped.length} files weren't added:`}
          <ul className="project-skipped">
            {skipped.map((s, i) => (
              <li key={`${s.name}-${i}`}>
                {s.name}: {s.reason}
              </li>
            ))}
          </ul>
        </InlineMessage>
      )}
      {files.kind === "loading" && (
        <ul className="project-list" aria-busy="true" aria-label="Loading files">
          {[60, 45].map((w) => (
            <li key={w} className="project-file">
              <Skeleton width={`${w}%`} />
            </li>
          ))}
        </ul>
      )}
      {files.kind === "error" && (
        <InlineMessage
          tone="error"
          action={
            <Button size="sm" onClick={() => void load()}>
              Retry
            </Button>
          }
        >
          Couldn't load the files. {files.message}
        </InlineMessage>
      )}
      {files.kind === "ready" && files.value.length === 0 && (
        <p className="dn-help project-empty">No files yet.</p>
      )}
      {files.kind === "ready" && files.value.length > 0 && (
        <ul className="project-list">
          {files.value.map((f) => (
            <li key={f.id} className="project-file" data-testid="project-file">
              <FileText {...ICON} aria-hidden="true" />
              <div className="row-main">
                <span className="project-file-name" title={f.name}>
                  {f.name}
                </span>
                <span className="dn-help" title={f.source_path ?? undefined}>
                  {formatBytes(f.size)}
                </span>
              </div>
              <IconButton size="sm" label={`Remove “${f.name}”`} onClick={() => void remove(f)}>
                <Trash2 {...ICON} aria-hidden="true" />
              </IconButton>
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}
