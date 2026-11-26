import { useEffect, useId, useState } from "react";
import type { Api } from "../ipc/commands";
import {
  errorMessage,
  type AddonConsentRequest,
  type AddonPermission,
  type Id,
  type ProjectSummary,
} from "../ipc/types";
import { permissionHelp, permissionLabel, sortPermissions } from "./addonText";
import { Dialog } from "./Dialog";
import { Button, InlineMessage, Select } from "./primitives";
import { useToast } from "./toast";

/**
 * What an add-on may do, one permission at a time (design system: AddonConsent).
 * Nothing is preselected and there is no "Allow all". "Not now" allows nothing and the
 * dialog comes back next start; the add-on runs either way, without what wasn't allowed.
 */
export function AddonConsent({
  api,
  request,
  onDone,
}: {
  api: Api;
  request: AddonConsentRequest;
  /** Closed: answered (true) or "Not now" (false). */
  onDone: (answered: boolean) => void;
}) {
  const toast = useToast();
  const base = useId();
  const perms = sortPermissions(request.permissions);
  const [allowed, setAllowed] = useState<Set<AddonPermission>>(new Set());
  const [chatsAll, setChatsAll] = useState(false);
  const [chatProjects, setChatProjects] = useState<Set<Id>>(new Set());
  const [knowledge, setKnowledge] = useState<Set<Id>>(new Set());
  const [projects, setProjects] = useState<ProjectSummary[] | null>(null);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const needsProjects = perms.some((p) => p.permission === "chats.read" || p.permission === "knowledge.search");

  useEffect(() => {
    if (!needsProjects) return;
    api
      .listProjects()
      .then(setProjects)
      .catch((e: unknown) => setError(`Couldn't load your projects. ${errorMessage(e)}`));
  }, [api, needsProjects]);

  const toggle = (p: AddonPermission, on: boolean) =>
    setAllowed((s) => {
      const n = new Set(s);
      if (on) n.add(p);
      else n.delete(p);
      return n;
    });

  const save = async () => {
    setSaving(true);
    setError(null);
    try {
      await api.addonConsent(
        request.id,
        perms.map((p) => p.permission),
        {
          allowed: [...allowed],
          chats_all: chatsAll,
          chats_projects: [...chatProjects],
          knowledge_projects: [...knowledge],
        },
      );
      if (allowed.size === 0) toast({ tone: "info", title: `${request.name} runs without extra permissions.` });
      onDone(true);
    } catch (e) {
      setError(`Couldn't save your choices. Try again. ${errorMessage(e)}`);
    } finally {
      setSaving(false);
    }
  };

  const projectBoxes = (chosen: Set<Id>, set: (s: Set<Id>) => void, label: string) =>
    projects === null ? null : projects.length === 0 ? (
      <p className="dn-help">You have no projects yet.</p>
    ) : (
      <fieldset className="addon-projects">
        <legend className="dn-visually-hidden">{label}</legend>
        {projects.map((p) => (
          <label key={p.id} className="addon-check">
            <input
              type="checkbox"
              checked={chosen.has(p.id)}
              onChange={(e) => {
                const n = new Set(chosen);
                if (e.target.checked) n.add(p.id);
                else n.delete(p.id);
                set(n);
              }}
            />
            {p.name}
          </label>
        ))}
      </fieldset>
    );

  return (
    <Dialog
      open
      onOpenChange={(open) => {
        if (!open) onDone(false);
      }}
      size="medium"
      title={request.first ? `What may ${request.name} do?` : `${request.name} ${request.version} asks for more`}
      description={
        request.first
          ? `${request.name} is built into this copy of Demios. Choose what it may use. You can change this later in Settings › Add-ons.`
          : "What you allowed before stays as it is. Choose whether it may also use this."
      }
      testId="addon-consent"
    >
      {error !== null && <InlineMessage tone="error">{error}</InlineMessage>}
      <ul className="addon-consent-list">
        {perms.map((v, i) => {
          const id = `${base}-${v.permission}`;
          const on = allowed.has(v.permission);
          return (
            <li key={v.permission} className="addon-check-item">
              <input
                type="checkbox"
                id={id}
                checked={on}
                aria-describedby={`${id}-d`}
                autoFocus={i === 0}
                onChange={(e) => toggle(v.permission, e.target.checked)}
              />
              <label htmlFor={id}>{permissionLabel(v.permission)}</label>
              <div className="addon-check-more">
                <span id={`${id}-d`} className="dn-help">
                  {permissionHelp(v)}
                </span>
                {v.permission === "chats.read" && on && (
                  <>
                    <Select
                      id={`${id}-scope`}
                      value={chatsAll ? "all" : "projects"}
                      placeholder="Which chats"
                      emptyText=""
                      groups={[
                        {
                          label: "Which chats",
                          options: [
                            { value: "all", label: "All chats" },
                            { value: "projects", label: "Chats in chosen projects" },
                          ],
                        },
                      ]}
                      onValueChange={(x) => setChatsAll(x === "all")}
                    />
                    {!chatsAll && projectBoxes(chatProjects, setChatProjects, "Projects whose chats it may read")}
                  </>
                )}
                {v.permission === "knowledge.search" && on &&
                  projectBoxes(knowledge, setKnowledge, "Projects it may search")}
              </div>
            </li>
          );
        })}
      </ul>
      <div className="dn-dialog-actions">
        <Button variant="secondary" onClick={() => onDone(false)} disabled={saving}>
          Not now
        </Button>
        <Button variant="primary" onClick={() => void save()} loading={saving}>
          Allow selected
        </Button>
      </div>
    </Dialog>
  );
}
