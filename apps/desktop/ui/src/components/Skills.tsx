import { FolderOpen, RefreshCw } from "lucide-react";
import { useCallback, useEffect, useState } from "react";
import type { Api } from "../ipc/commands";
import { errorMessage, type SkillsOverview } from "../ipc/types";
import { Badge, Button, ICON, InlineMessage, Skeleton } from "./primitives";
import { useToast } from "./toast";

/**
 * Settings › General › Skills (T-SKL-02): the built-in and the user's own skills, the
 * user's files that didn't load with their error, and the folder to put new ones in.
 */
export function SkillsCard({ api, onChanged }: { api: Api; onChanged?: () => void }) {
  const toast = useToast();
  const [overview, setOverview] = useState<SkillsOverview | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [reloading, setReloading] = useState(false);

  useEffect(() => {
    let alive = true;
    api
      .skillsOverview()
      .then((o) => alive && setOverview(o))
      .catch((e: unknown) => alive && setError(errorMessage(e)));
    return () => {
      alive = false;
    };
  }, [api]);

  const reload = useCallback(async () => {
    setReloading(true);
    setError(null);
    try {
      const o = await api.reloadSkills();
      setOverview(o);
      onChanged?.();
      toast({
        tone: o.problems.length > 0 ? "warning" : "success",
        title:
          o.problems.length > 0
            ? `Skills reloaded. ${o.problems.length} ${o.problems.length === 1 ? "file has" : "files have"} a problem.`
            : "Skills reloaded",
      });
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setReloading(false);
    }
  }, [api, onChanged, toast]);

  const openFolder = async () => {
    try {
      await api.openSkillsFolder();
    } catch (e) {
      toast({ tone: "error", title: errorMessage(e) });
    }
  };

  const tools = (t: string[]) => (t.length === 0 ? "No tools" : `Tools: ${t.join(", ")}`);

  return (
    <section className="card" aria-labelledby="skills-h" data-testid="skills-card">
      <h2 id="skills-h">Skills</h2>
      <p className="dn-help">
        A skill changes how the assistant works in a chat. Pick one in the message box. A skill can only limit which
        tools are used; tools still ask for your OK as set under Tools.
      </p>
      {error !== null && <InlineMessage tone="error">{error}</InlineMessage>}
      {overview === null && error === null && <Skeleton />}
      {overview !== null && overview.problems.length > 0 && (
        <div role="group" aria-labelledby="skills-problems-h" data-testid="skill-problems">
          <h3 id="skills-problems-h" className="card-subhead">
            {overview.problems.length === 1 ? "1 skill didn't load" : `${overview.problems.length} skills didn't load`}
          </h3>
          {overview.problems.map((p) => (
            <InlineMessage key={`${p.source}:${p.file}`} tone={p.source === "builtin" ? "error" : "warning"}>
              {p.source === "builtin" && "Built-in skill, please report this problem: "}
              <strong>{p.file}</strong>: {p.message}
            </InlineMessage>
          ))}
        </div>
      )}
      {overview !== null &&
        overview.skills.map((s) => (
          <div className="row" key={s.name} data-testid="skill-row">
            <div className="row-main">
              <span className="row-title">
                {s.label} <Badge>{s.source === "user" ? "Yours" : "Built in"}</Badge>
              </span>
              <span className="dn-help">{s.description}</span>
              <span className="dn-help">{tools(s.allowed_tools)}</span>
            </div>
          </div>
        ))}
      <div className="row">
        <div className="row-main">
          <span className="row-title">Your own skills</span>
          <span className="dn-help">
            Add a folder with a <code>SKILL.md</code> file to {overview?.folder ?? "your skills folder"}, then select
            Reload. The folder name and the <code>name</code> in the file must match.
          </span>
        </div>
        <div className="row-actions">
          <Button icon={<FolderOpen {...ICON} aria-hidden="true" />} onClick={() => void openFolder()}>
            Open skills folder
          </Button>
          <Button
            icon={<RefreshCw {...ICON} aria-hidden="true" />}
            onClick={() => void reload()}
            aria-busy={reloading || undefined}
            disabled={reloading}
          >
            Reload
          </Button>
        </div>
      </div>
    </section>
  );
}
