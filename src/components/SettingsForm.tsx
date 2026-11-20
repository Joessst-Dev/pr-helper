import type { ReactNode } from "react";
import type { EffectiveSettings } from "../lib/bindings/EffectiveSettings";
import type { Severity } from "../lib/bindings/Severity";
import type { SettingsOverrides } from "../lib/bindings/SettingsOverrides";
import type { Sourced } from "../lib/bindings/Sourced";
import type { TicketSource } from "../lib/bindings/TicketSource";
import { SEVERITIES, SEVERITY_LABEL, SOURCE_LABEL } from "../lib/format";
import { Input, Select } from "./ui";

const LANGUAGES = ["English", "German", "French", "Spanish", "Italian", "Dutch", "Portuguese", "Polish", "Japanese"];
const EFFORTS = ["low", "medium", "high", "xhigh", "max"];

function inheritLabel<T>(s: Sourced<T> | undefined, fmt: (v: T) => string) {
  if (!s) return "Inherit";
  return `Inherit: ${fmt(s.value)} (${SOURCE_LABEL[s.source]})`;
}

const yesNo = (v: boolean) => (v ? "Yes" : "No");
const orDefault = (v: string | number | null) => (v == null || v === "" ? "CLI default" : String(v));

function Row({ label, description, children }: { label: string; description: string; children: ReactNode }) {
  return (
    <div className="grid grid-cols-[minmax(0,1fr)_280px] items-center gap-6 border-b border-line py-3 last:border-b-0">
      <div>
        <div className="text-[13px] font-medium">{label}</div>
        <div className="text-xs text-muted">{description}</div>
      </div>
      <div>{children}</div>
    </div>
  );
}

function BoolSelect({
  value,
  inherited,
  onChange,
  label,
}: {
  value: boolean | null | undefined;
  inherited?: Sourced<boolean>;
  onChange: (v: boolean | null) => void;
  label: string;
}) {
  return (
    <Select
      aria-label={label}
      value={value == null ? "" : value ? "yes" : "no"}
      onChange={(e) => onChange(e.target.value === "" ? null : e.target.value === "yes")}
    >
      <option value="">{inheritLabel(inherited, yesNo)}</option>
      <option value="yes">Yes</option>
      <option value="no">No</option>
    </Select>
  );
}

/**
 * Edits one level of the settings hierarchy. Empty values mean "inherit"; `inherited` shows what
 * that resolves to.
 */
/** Select value for "explicitly no ticket source" (stored as an empty string). */
const NO_TICKETS = "__none__";

export function SettingsForm({
  value,
  inherited,
  ticketSources = [],
  onChange,
}: {
  value: SettingsOverrides;
  inherited?: EffectiveSettings;
  ticketSources?: TicketSource[];
  onChange: (v: SettingsOverrides) => void;
}) {
  const set = <K extends keyof SettingsOverrides>(key: K, v: SettingsOverrides[K] | null | undefined) =>
    onChange({ ...value, [key]: v === undefined || v === "" ? null : v });
  const sourceName = (id: string | null) =>
    id == null ? "None" : (ticketSources.find((t) => t.id === id)?.name ?? "Unknown source");
  const ticketSelectValue =
    value.ticketSourceId == null ? "" : value.ticketSourceId === "" ? NO_TICKETS : value.ticketSourceId;

  return (
    <div>
      <Row label="Comment language" description="Language for proposed comments and the review summary.">
        <Input
          aria-label="Comment language"
          list="pr-helper-languages"
          placeholder={inheritLabel(inherited?.commentLanguage, String)}
          value={value.commentLanguage ?? ""}
          onChange={(e) => set("commentLanguage", e.target.value)}
        />
        <datalist id="pr-helper-languages">
          {LANGUAGES.map((l) => (
            <option key={l} value={l} />
          ))}
        </datalist>
      </Row>
      <Row
        label="Forbid AI attribution"
        description="Never mention Claude or AI in posted comments. Enforced before anything is posted."
      >
        <BoolSelect
          label="Forbid AI attribution"
          value={value.forbidAiAttribution}
          inherited={inherited?.forbidAiAttribution}
          onChange={(v) => set("forbidAiAttribution", v)}
        />
      </Row>
      <Row label="Attribution footer" description="Appended to posted reviews when attribution is allowed.">
        <Input
          aria-label="Attribution footer"
          placeholder={inheritLabel(inherited?.attributionFooter, (v) => v || "none")}
          value={value.attributionFooter ?? ""}
          onChange={(e) => set("attributionFooter", e.target.value)}
        />
      </Row>
      <Row label="Skip nits" description="Don't report cosmetic or preference-level findings at all.">
        <BoolSelect
          label="Skip nits"
          value={value.skipNits}
          inherited={inherited?.skipNits}
          onChange={(v) => set("skipNits", v)}
        />
      </Row>
      <Row
        label="Auto-review new commits"
        description="Start a new review when a reviewed PR changes and the last verdict wasn't approve."
      >
        <BoolSelect
          label="Auto-review new commits"
          value={value.autoReviewOnNewCommits}
          inherited={inherited?.autoReviewOnNewCommits}
          onChange={(v) => set("autoReviewOnNewCommits", v)}
        />
      </Row>
      <Row
        label="Auto-approve"
        description="Post an approval (summary only) when the verdict is approve, then notify you."
      >
        <BoolSelect
          label="Auto-approve"
          value={value.autoApproveOnApproveVerdict}
          inherited={inherited?.autoApproveOnApproveVerdict}
          onChange={(v) => set("autoApproveOnApproveVerdict", v)}
        />
      </Row>
      <Row label="Pre-select findings from" description="Findings at or above this severity start selected.">
        <Select
          aria-label="Pre-select findings from"
          value={value.defaultSelectedMinSeverity ?? ""}
          onChange={(e) => set("defaultSelectedMinSeverity", (e.target.value || null) as Severity | null)}
        >
          <option value="">{inheritLabel(inherited?.defaultSelectedMinSeverity, (v) => SEVERITY_LABEL[v])}</option>
          {SEVERITIES.map((s) => (
            <option key={s} value={s}>
              {SEVERITY_LABEL[s]}
            </option>
          ))}
        </Select>
      </Row>
      <Row label="Poll interval (seconds)" description="How often reviewed PRs are checked for new commits (min 60).">
        <Input
          aria-label="Poll interval"
          type="number"
          min={60}
          placeholder={inheritLabel(inherited?.pollIntervalSecs, String)}
          value={value.pollIntervalSecs ?? ""}
          onChange={(e) => set("pollIntervalSecs", e.target.value ? Number(e.target.value) : null)}
        />
      </Row>
      <Row label="Model" description="Claude model alias or id, e.g. opus or sonnet.">
        <Input
          aria-label="Model"
          placeholder={inheritLabel(inherited?.model, orDefault)}
          value={value.model ?? ""}
          onChange={(e) => set("model", e.target.value.trim())}
        />
      </Row>
      <Row label="Effort" description="Reasoning effort for the reviewer.">
        <Select aria-label="Effort" value={value.effort ?? ""} onChange={(e) => set("effort", e.target.value)}>
          <option value="">{inheritLabel(inherited?.effort, orDefault)}</option>
          {EFFORTS.map((e) => (
            <option key={e} value={e}>
              {e}
            </option>
          ))}
        </Select>
      </Row>
      <Row label="Budget per review (USD)" description="Stops a review that exceeds this API spend.">
        <Input
          aria-label="Budget per review"
          type="number"
          min={0}
          step={0.5}
          placeholder={inheritLabel(inherited?.maxBudgetUsd, (v) => (v == null ? "no limit" : `$${v}`))}
          value={value.maxBudgetUsd ?? ""}
          onChange={(e) => set("maxBudgetUsd", e.target.value ? Number(e.target.value) : null)}
        />
      </Row>
      <Row
        label="Ticket source"
        description="Load the linked Jira ticket as context for the reviewer. Sources are managed in Settings."
      >
        <Select
          aria-label="Ticket source"
          value={ticketSelectValue}
          onChange={(e) => {
            const v = e.target.value;
            // "" inherits; NO_TICKETS stores an explicit "off" for this level.
            onChange({ ...value, ticketSourceId: v === "" ? null : v === NO_TICKETS ? "" : v });
          }}
        >
          <option value="">{inheritLabel(inherited?.ticketSourceId, sourceName)}</option>
          <option value={NO_TICKETS}>None</option>
          {ticketSources.map((t) => (
            <option key={t.id} value={t.id}>
              {t.name}
            </option>
          ))}
          {value.ticketSourceId && !ticketSources.some((t) => t.id === value.ticketSourceId) && (
            <option value={value.ticketSourceId}>Deleted source</option>
          )}
        </Select>
      </Row>
      <Row
        label="Jira project keys"
        description="Only tickets from these projects are loaded, e.g. ABC, PAY. Keys are found in the branch, title or description."
      >
        <Input
          aria-label="Jira project keys"
          placeholder={inheritLabel(inherited?.ticketProjectKeys, (v) => v || "none")}
          value={value.ticketProjectKeys ?? ""}
          onChange={(e) => set("ticketProjectKeys", e.target.value)}
        />
      </Row>
      <Row
        label="Check PR against ticket"
        description="Report requirements or acceptance criteria from the ticket that the PR misses (at most medium)."
      >
        <BoolSelect
          label="Check PR against ticket"
          value={value.ticketScopeCheck}
          inherited={inherited?.ticketScopeCheck}
          onChange={(v) => set("ticketScopeCheck", v)}
        />
      </Row>
    </div>
  );
}
