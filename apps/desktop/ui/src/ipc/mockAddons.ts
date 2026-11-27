// A made-up add-on for the browser mock (`npm run dev` without Tauri), so Settings › Add-ons
// and its consent dialog can be seen without an add-on build. The real app has none unless
// an add-on build adds them.

import type { Api } from "./commands";
import type { AddonJob, AddonPermissionView, AddonSummary, HeldCall } from "./types";

type AddonApi = Pick<
  Api,
  | "addonsList"
  | "addonsWatch"
  | "addonDetail"
  | "addonsPendingConsent"
  | "addonConsent"
  | "addonSetPermission"
  | "addonSetEnabled"
  | "addonsStartJobs"
  | "addonsPaused"
  | "addonsSetPaused"
  | "addonSetJobEnabled"
  | "addonSetMissedRule"
  | "addonAnswerQuestion"
  | "addonAnswerHeld"
>;

export function mockAddons(): AddonApi {
  const now = Math.floor(Date.now() / 1000);
  let watch: (() => void) | null = null;
  let paused = false;
  let enabled = true;
  const perms: AddonPermissionView[] = [
    { permission: "chats.read", asked: false, allowed: false, tools: [], hosts: [] },
    { permission: "tools.call", asked: false, allowed: false, tools: ["mcp.calendar.*"], hosts: [] },
    { permission: "jobs", asked: false, allowed: false, tools: [], hosts: [] },
  ];
  const job: AddonJob = {
    id: "morning",
    name: "Morning summary",
    schedule: { kind: "daily_at", hour: 7, minute: 0 },
    enabled: true,
    missed_rule: { rule: "catch_up_once", fresh_for_secs: 6 * 3600 },
    missed_rule_is_default: true,
    proposed_rule: { rule: "catch_up_once", fresh_for_secs: 6 * 3600 },
    last_success: now - 3 * 3600,
    history: [
      {
        id: "00000000-0000-7000-8000-000000000001",
        job: { plugin: "brief", job: "morning" },
        scheduled_for: now - 6 * 3600,
        started_at: now - 3 * 3600,
        finished_at: now - 3 * 3600 + 20,
        late: true,
        covers_runs: 1,
        outcome: "succeeded",
        not_fresh: false,
        error: null,
      },
    ],
    question: null,
    running: false,
  };
  let held: HeldCall[] = [
    {
      id: "00000000-0000-7000-8000-000000000002",
      job_id: "morning",
      tool: "mcp.calendar.create_event",
      summary: "Add a reminder to your calendar",
      arguments: JSON.stringify({ title: "Review the summary", at: "09:00" }),
      arguments_sha256: "mock",
      created_at: now - 600,
    },
  ];
  const summary = (): AddonSummary => ({
    id: "brief",
    name: "Daily brief",
    version: "1.2.0",
    state: enabled ? "running" : "off",
    reason: null,
    needs_consent: perms.some((p) => !p.asked),
    waiting: held.length,
    has_jobs: true,
  });
  return {
    addonsList: async () => [summary()],
    addonsWatch: async (f) => {
      watch = f;
      return null;
    },
    addonDetail: async () => ({
      summary: summary(),
      permissions: perms.map((p) => ({ ...p })),
      chats_all: true,
      chats_projects: [],
      knowledge_projects: [],
      jobs: [{ ...job }],
      held,
    }),
    addonsPendingConsent: async () =>
      perms.some((p) => !p.asked)
        ? [
            {
              id: "brief",
              name: "Daily brief",
              version: "1.2.0",
              first: perms.every((p) => !p.asked),
              permissions: perms.filter((p) => !p.asked).map((p) => ({ ...p })),
            },
          ]
        : [],
    addonConsent: async (_id, asked, answer) => {
      for (const p of perms) {
        if (asked.includes(p.permission)) {
          p.asked = true;
          p.allowed = answer.allowed.includes(p.permission);
        }
      }
      return null;
    },
    addonSetPermission: async (_id, permission, on) => {
      const p = perms.find((x) => x.permission === permission);
      if (p !== undefined) p.allowed = on;
      return null;
    },
    addonSetEnabled: async (_id, on) => {
      enabled = on;
      return null;
    },
    addonsStartJobs: async () => null,
    addonsPaused: async () => paused,
    addonsSetPaused: async (p) => {
      paused = p;
      return null;
    },
    addonSetJobEnabled: async (_id, _job, on) => {
      job.enabled = on;
      return null;
    },
    addonSetMissedRule: async (_id, _job, rule) => {
      job.missed_rule = rule ?? job.proposed_rule;
      job.missed_rule_is_default = rule === null;
      return null;
    },
    addonAnswerQuestion: async () => null,
    addonAnswerHeld: async (id) => {
      held = held.filter((h) => h.id !== id);
      watch?.();
      return null;
    },
  };
}
