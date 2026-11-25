import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import type { Api } from "../ipc/commands";
import type { SkillsOverview } from "../ipc/types";
import { TooltipProvider } from "./primitives";
import { SkillsCard } from "./Skills";
import { ToastProvider } from "./toast";

afterEach(cleanup);

const builtin = { name: "summarize", label: "Summarize", description: "Key points.", allowed_tools: [], source: "builtin" as const };
const mine = { name: "haiku", label: "Haiku", description: "Answers in haiku.", allowed_tools: ["artifact.*"], source: "user" as const };

function fake(first: SkillsOverview, afterReload: SkillsOverview) {
  const calls: string[] = [];
  const api = {
    skillsOverview: async () => first,
    reloadSkills: async () => {
      calls.push("reload");
      return afterReload;
    },
    openSkillsFolder: async () => {
      calls.push("open");
      return null;
    },
  } as unknown as Api;
  return { api, calls };
}

function ui(api: Api, onChanged?: () => void) {
  return render(
    <TooltipProvider>
      <ToastProvider>
        <SkillsCard api={api} {...(onChanged !== undefined ? { onChanged } : {})} />
      </ToastProvider>
    </TooltipProvider>,
  );
}

describe("SkillsCard", () => {
  it("lists built-in and own skills and shows each file that didn't load with its error", async () => {
    const { api } = fake(
      {
        skills: [builtin, mine],
        problems: [{ file: "Draft/SKILL.md", message: "name: `draft` must match its folder name `Draft`" }],
        folder: "/data/skills",
      },
      { skills: [], problems: [], folder: "/data/skills" },
    );
    ui(api);
    const rows = await screen.findAllByTestId("skill-row");
    expect(rows.map((r) => within(r).getByText(/Summarize|Haiku/).textContent)).toEqual(["Summarize Built in", "Haiku Yours"]);
    expect(within(rows[1]!).getByText("Tools: artifact.*")).toBeTruthy();
    expect(within(rows[0]!).getByText("No tools")).toBeTruthy();
    const problems = screen.getByTestId("skill-problems");
    expect(within(problems).getByRole("heading", { name: "1 skill didn't load" })).toBeTruthy();
    expect(problems.textContent).toContain("Draft/SKILL.md: name: `draft` must match its folder name `Draft`");
    expect(screen.getByTestId("skills-card").textContent).toContain("/data/skills");
  });

  it("reloads the folder, clears fixed problems and tells the composer", async () => {
    const { api, calls } = fake(
      { skills: [builtin], problems: [{ file: "haiku/SKILL.md", message: "the instructions are empty" }], folder: "/d" },
      { skills: [builtin, mine], problems: [], folder: "/d" },
    );
    let changed = 0;
    ui(api, () => changed++);
    await screen.findByTestId("skill-problems");
    fireEvent.click(screen.getByRole("button", { name: "Reload" }));
    await waitFor(() => expect(screen.getAllByTestId("skill-row")).toHaveLength(2));
    expect(screen.queryByTestId("skill-problems")).toBeNull();
    expect(calls).toEqual(["reload"]);
    expect(changed).toBe(1);
    expect(await screen.findByText("Skills reloaded")).toBeTruthy();
  });

  it("opens the skills folder through the backend", async () => {
    const { api, calls } = fake({ skills: [], problems: [], folder: "/d" }, { skills: [], problems: [], folder: "/d" });
    ui(api);
    fireEvent.click(await screen.findByRole("button", { name: "Open skills folder" }));
    await waitFor(() => expect(calls).toEqual(["open"]));
  });
});
