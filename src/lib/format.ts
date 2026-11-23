import type { Severity } from "./bindings/Severity";
import type { Verdict } from "./bindings/Verdict";
import type { SettingSource } from "./bindings/SettingSource";

export const SEVERITIES: Severity[] = ["critical", "high", "medium", "low", "nit"];

export const severityRank = (s: Severity) => SEVERITIES.indexOf(s);

export const SEVERITY_LABEL: Record<Severity, string> = {
  critical: "Critical",
  high: "High",
  medium: "Medium",
  low: "Low",
  nit: "Nit",
};

export const VERDICT_LABEL: Record<Verdict, string> = {
  approve: "Approve",
  request_changes: "Changes requested",
  comment: "Comment",
};

export const SOURCE_LABEL: Record<SettingSource, string> = {
  default: "default",
  global: "global",
  server: "server",
  repo: "repo",
};

export function relativeTime(iso: string | null | undefined, now: Date = new Date()): string {
  if (!iso) return "";
  const then = new Date(iso);
  if (Number.isNaN(then.getTime())) return "";
  const secs = Math.round((now.getTime() - then.getTime()) / 1000);
  if (secs < 45) return "just now";
  const mins = Math.round(secs / 60);
  if (mins < 60) return `${mins}m ago`;
  const hours = Math.round(mins / 60);
  if (hours < 24) return `${hours}h ago`;
  const days = Math.round(hours / 24);
  if (days < 30) return `${days}d ago`;
  return then.toLocaleDateString();
}

export function shortSha(sha: string | null | undefined): string {
  return sha ? sha.slice(0, 7) : "";
}

export function formatCost(usd: number | null | undefined): string {
  if (usd == null) return "";
  return usd < 0.01 ? "<$0.01" : `$${usd.toFixed(2)}`;
}

export function location(file: string | null, lineStart: number | null, lineEnd: number | null): string {
  if (!file) return "PR-wide";
  if (lineStart == null) return file;
  return lineEnd != null && lineEnd !== lineStart ? `${file}:${lineStart}-${lineEnd}` : `${file}:${lineStart}`;
}

const MAX_TITLE_CHARS = 80;

/** The title of a user comment: its first non-empty line, shortened. Mirrors `title_of` in Rust. */
export function commentTitle(text: string): string {
  const first =
    text
      .split("\n")
      .map((l) => l.trim())
      .find((l) => l) ?? "";
  const chars = [...first];
  return chars.length <= MAX_TITLE_CHARS ? first : `${chars.slice(0, MAX_TITLE_CHARS - 1).join("").trimEnd()}…`;
}
