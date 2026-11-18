import { CircleCheck, CircleX, LoaderCircle, MessageSquare } from "lucide-react";
import type { Review } from "../lib/bindings/Review";
import type { Severity } from "../lib/bindings/Severity";
import type { Verdict } from "../lib/bindings/Verdict";
import { SEVERITY_LABEL, VERDICT_LABEL } from "../lib/format";
import { Badge } from "./ui";

const SEVERITY_TONE = {
  critical: "danger",
  high: "warn",
  medium: "info",
  low: "neutral",
  nit: "neutral",
} as const;

export function SeverityBadge({ severity }: { severity: Severity }) {
  return (
    <Badge tone={SEVERITY_TONE[severity]} className={severity === "nit" ? "opacity-70" : undefined}>
      {SEVERITY_LABEL[severity]}
    </Badge>
  );
}

export function VerdictBadge({ verdict }: { verdict: Verdict | null }) {
  if (!verdict) return null;
  const tone = verdict === "approve" ? "ok" : verdict === "request_changes" ? "danger" : "info";
  const Icon = verdict === "approve" ? CircleCheck : verdict === "request_changes" ? CircleX : MessageSquare;
  return (
    <Badge tone={tone}>
      <Icon className="size-3" />
      {VERDICT_LABEL[verdict]}
    </Badge>
  );
}

export function ReviewStatusBadge({ review }: { review: Review }) {
  switch (review.status) {
    case "queued":
      return <Badge>Queued</Badge>;
    case "running":
      return (
        <Badge tone="accent">
          <LoaderCircle className="size-3 animate-spin" />
          Reviewing
        </Badge>
      );
    case "failed":
      return <Badge tone="danger">Failed</Badge>;
    case "cancelled":
      return <Badge>Cancelled</Badge>;
    case "done":
      return <VerdictBadge verdict={review.verdict} />;
  }
}
