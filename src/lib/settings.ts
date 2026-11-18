import type { SettingsOverrides } from "./bindings/SettingsOverrides";

/** An override level that sets nothing (inherits everything). */
export const EMPTY_OVERRIDES: SettingsOverrides = {
  commentLanguage: null,
  forbidAiAttribution: null,
  attributionFooter: null,
  skipNits: null,
  autoReviewOnNewCommits: null,
  autoApproveOnApproveVerdict: null,
  pollIntervalSecs: null,
  model: null,
  effort: null,
  maxBudgetUsd: null,
  defaultSelectedMinSeverity: null,
};
