// Words for add-on permissions, schedules and job runs (design system: AddonConsent and
// AddonsSettings). The consent dialog and the add-on's page use the same labels.

import type { AddonPermission, AddonPermissionView, JobRun, JobSchedule, MissedRunRule } from "../ipc/types";

export const PERMISSION_ORDER: AddonPermission[] = [
  "chats.read",
  "memory",
  "knowledge.search",
  "tools.call",
  "notify",
  "network",
  "jobs",
];

export function permissionLabel(p: AddonPermission): string {
  switch (p) {
    case "chats.read":
      return "Read your chats";
    case "memory":
      return "Remember things between chats";
    case "knowledge.search":
      return "Search your project files";
    case "tools.call":
      return "Use other tools";
    case "notify":
      return "Show notifications";
    case "network":
      return "Send data to the internet";
    case "jobs":
      return "Run background jobs";
  }
}

/** The help under a permission, naming exactly what it reads or does. */
export function permissionHelp(v: AddonPermissionView): string {
  switch (v.permission) {
    case "chats.read":
      return "Its messages and titles, in the chats you choose.";
    case "memory":
      return "You can see and delete everything it remembers in Settings › Add-ons.";
    case "knowledge.search":
      return "Only in the projects you choose.";
    case "tools.call":
      return `These tools, which still ask you before they change anything: ${v.tools.join(", ")}`;
    case "notify":
      return "Through your system's notifications.";
    case "network":
      return `Only to ${v.hosts.join(", ")}. What it sends there leaves this computer, and Delete all local data can't remove it.`;
    case "jobs":
      return "Each job stays off until you turn it on. Jobs never approve anything on their own.";
  }
}

export function sortPermissions(v: AddonPermissionView[]): AddonPermissionView[] {
  return [...v].sort((a, b) => PERMISSION_ORDER.indexOf(a.permission) - PERMISSION_ORDER.indexOf(b.permission));
}

const pad = (n: number) => String(n).padStart(2, "0");

export function scheduleText(s: JobSchedule): string {
  if (s.kind === "daily_at") return `Every day at ${pad(s.hour)}:${pad(s.minute)}`;
  const h = s.secs / 3600;
  if (Number.isInteger(h)) return h === 1 ? "Every hour" : `Every ${h} hours`;
  const m = Math.round(s.secs / 60);
  return m === 1 ? "Every minute" : `Every ${m} minutes`;
}

/** Local time of a Unix time, as 07:00 today, or a short date before that. */
export function timeText(unix: number, now: number = Date.now() / 1000): string {
  const d = new Date(unix * 1000);
  const t = `${pad(d.getHours())}:${pad(d.getMinutes())}`;
  const today = new Date(now * 1000);
  if (d.toDateString() === today.toDateString()) return t;
  return `${d.toLocaleDateString(undefined, { day: "numeric", month: "short" })} ${t}`;
}

export function hoursText(secs: number): string {
  const h = Math.round(secs / 3600);
  if (h >= 1) return h === 1 ? "within 1 hour" : `within ${h} hours`;
  return `within ${Math.max(1, Math.round(secs / 60))} minutes`;
}

export type RuleChoice = "catch_up_once" | "ask" | "skip";

export function ruleLabel(r: MissedRunRule): string {
  switch (r.rule) {
    case "catch_up_once":
      return `Run once, if still useful (${hoursText(r.fresh_for_secs)})`;
    case "ask":
      return "Ask me";
    case "skip":
      return "Skip";
  }
}

/** One line for the latest run, as the design's status line says it. */
export function runText(r: JobRun): string {
  switch (r.outcome) {
    case "running":
      return r.late ? `Missed at ${timeText(r.scheduled_for)} · Running late now` : "Running now";
    case "succeeded":
      return r.late
        ? `Missed at ${timeText(r.scheduled_for)} · Ran late at ${timeText(r.started_at)}`
        : `Last ran ${timeText(r.started_at)}`;
    case "skipped":
      return r.not_fresh
        ? `Skipped ${timeText(r.scheduled_for)}: it was no longer useful by the time Demios was open`
        : `Skipped ${timeText(r.scheduled_for)}`;
    case "failed":
      return `Last run failed at ${timeText(r.started_at)}${r.error === null ? "" : `: ${r.error}`}`;
    case "timed_out":
      return `Last run at ${timeText(r.started_at)} took too long and was stopped`;
    case "cancelled":
      return `Last run at ${timeText(r.started_at)} was stopped`;
    case "interrupted":
      return `Last run at ${timeText(r.started_at)} was interrupted when Demios closed`;
  }
}
