import { CircleCheck, CircleX, Eye, LoaderCircle, MessageSquare } from "lucide-react";
import type { PrReviewer } from "../lib/bindings/PrReviewer";
import type { PullRequest } from "../lib/bindings/PullRequest";
import type { Review } from "../lib/bindings/Review";
import type { Severity } from "../lib/bindings/Severity";
import type { Verdict } from "../lib/bindings/Verdict";
import { REVIEWER_STATE_LABEL, SEVERITY_LABEL, VERDICT_LABEL } from "../lib/format";
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

const IN_REVIEW_NAMES = 2;

/** "In review: alice, bob +1" when anyone other than the user is reviewing. */
export function InReviewBadge({ reviewers }: { reviewers: PrReviewer[] }) {
  const others = reviewers.filter((r) => !r.isMe);
  if (others.length === 0) return null;
  const names = others.slice(0, IN_REVIEW_NAMES).map((r) => r.login);
  const more = others.length - names.length;
  return (
    <Badge tone="info" title={others.map((r) => `${r.login}: ${REVIEWER_STATE_LABEL[r.state]}`).join("\n")}>
      <Eye className="size-3" />
      In review: {names.join(", ")}
      {more > 0 && ` +${more}`}
    </Badge>
  );
}

/** Approval and other people's review state of a PR. */
export function PrReviewBadges({ pr }: { pr: PullRequest }) {
  const changesRequested = pr.reviewers.some((r) => !r.isMe && r.state === "changes_requested");
  return (
    <>
      {pr.approved && (
        <Badge tone="ok">
          <CircleCheck className="size-3" />
          Approved
        </Badge>
      )}
      {changesRequested && (
        <Badge tone="warn">
          <CircleX className="size-3" />
          Changes requested
        </Badge>
      )}
      <InReviewBadge reviewers={pr.reviewers} />
    </>
  );
}
