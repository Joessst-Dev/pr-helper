import type { Finding } from "../lib/bindings/Finding";
import type { EffectiveSettings } from "../lib/bindings/EffectiveSettings";
import type { TicketSource } from "../lib/bindings/TicketSource";

export function finding(overrides: Partial<Finding> = {}): Finding {
  return {
    id: "f1",
    reviewId: "r1",
    position: 0,
    severity: "high",
    category: "bug",
    title: "Off-by-one in pagination",
    file: "src/page.rs",
    lineStart: 10,
    lineEnd: null,
    side: "right",
    explanation: "The loop skips the last page.",
    proposedComment: "This skips the last page; use <= here.",
    suggestion: null,
    inDiff: true,
    diffExcerpt: "+   10 for i in 0..n {",
    selected: true,
    postedAt: null,
    postedRef: null,
    ...overrides,
  };
}

export const effective: EffectiveSettings = {
  commentLanguage: { value: "German", source: "server" },
  forbidAiAttribution: { value: true, source: "default" },
  attributionFooter: { value: "", source: "default" },
  skipNits: { value: false, source: "global" },
  autoReviewOnNewCommits: { value: false, source: "default" },
  autoApproveOnApproveVerdict: { value: false, source: "default" },
  pollIntervalSecs: { value: 300, source: "default" },
  model: { value: null, source: "default" },
  effort: { value: null, source: "default" },
  maxBudgetUsd: { value: null, source: "default" },
  defaultSelectedMinSeverity: { value: "medium", source: "default" },
  ticketSourceId: { value: "jira-1", source: "global" },
  ticketProjectKeys: { value: "", source: "default" },
  ticketScopeCheck: { value: true, source: "default" },
};

export const ticketSources: TicketSource[] = [
  {
    id: "jira-1",
    kind: "jira_cloud",
    name: "Acme Jira",
    baseUrl: "https://acme.atlassian.net",
    email: "me@acme.test",
    mcpServer: null,
    mcpTools: null,
    model: null,
    extraFields: null,
    username: "Me",
    createdAt: "2026-01-01T00:00:00Z",
  },
];
