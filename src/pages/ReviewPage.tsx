import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { Link, useParams } from "react-router";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import clsx from "clsx";
import { Copy, ExternalLink, Square, TerminalSquare } from "lucide-react";
import { openUrl } from "@tauri-apps/plugin-opener";
import { api, errorMessage } from "../lib/api";
import type { CriticalFile } from "../lib/bindings/CriticalFile";
import type { Finding } from "../lib/bindings/Finding";
import type { ProgressEvent } from "../lib/bindings/ProgressEvent";
import type { Severity } from "../lib/bindings/Severity";
import type { SubmitMode } from "../lib/bindings/SubmitMode";
import type { Verdict } from "../lib/bindings/Verdict";
import { useReviewProgress } from "../lib/events";
import { formatCost, relativeTime, SEVERITIES, SEVERITY_LABEL, severityRank, shortSha } from "../lib/format";
import { ReviewStatusBadge, VerdictBadge } from "../components/badges";
import type { CommentActions } from "../components/CommentComposer";
import { CriticalFiles } from "../components/CriticalFiles";
import { FindingCard, RulesWarning, useCommentCheck } from "../components/FindingCard";
import { TicketChips } from "../components/TicketChips";
import { Badge, Button, Card, EmptyState, ErrorBanner, PageHeader, Select, Spinner, TextArea } from "../components/ui";

export function ReviewPage() {
  const { reviewId = "" } = useParams();
  const qc = useQueryClient();
  const detail = useQuery({ queryKey: ["review", reviewId], queryFn: () => api.getReview(reviewId) });
  const review = detail.data?.review;

  const history = useQuery({
    queryKey: ["reviews", review?.serverId, review?.repo, review?.number],
    queryFn: () => api.listReviewsForPr(review!.serverId, review!.repo, review!.number),
    enabled: !!review,
  });

  const cancel = useMutation({
    mutationFn: async () => {
      if (!(await api.cancelReview(reviewId))) throw new Error("The review is no longer running.");
    },
    onSettled: () => qc.invalidateQueries({ queryKey: ["review", reviewId] }),
  });

  if (detail.isLoading) {
    return (
      <div className="flex items-center gap-2 p-8 text-muted">
        <Spinner /> Loading review…
      </div>
    );
  }
  if (detail.error || !detail.data || !review) {
    return (
      <div className="p-6">
        <ErrorBanner error={errorMessage(detail.error ?? "Review not found")} />
      </div>
    );
  }

  const running = review.status === "running" || review.status === "queued";
  const prUrl = detail.data.webUrl;

  return (
    <div>
      <PageHeader
        title={
          <span>
            <span className="font-mono text-muted">
              {review.repo}#{review.number}
            </span>{" "}
            {review.prTitle}
          </span>
        }
        subtitle={
          <span className="flex flex-wrap items-center gap-2">
            <ReviewStatusBadge review={review} />
            <span>{review.trigger === "auto" ? "Auto-review" : "Manual review"}</span>
            <span>· head {shortSha(review.headSha)}</span>
            <span>· {relativeTime(review.createdAt)}</span>
            {review.costUsd != null && <span>· {formatCost(review.costUsd)}</span>}
            {review.approvalPosted && <Badge tone="ok">Approval posted</Badge>}
            <TicketChips tickets={review.tickets} />
          </span>
        }
        actions={
          <>
            {prUrl && (
              <Button variant="ghost" onClick={() => openUrl(prUrl)}>
                <ExternalLink className="size-3.5" /> Open PR
              </Button>
            )}
            {running && (
              <Button variant="danger" busy={cancel.isPending} onClick={() => cancel.mutate()}>
                <Square className="size-3" /> Cancel
              </Button>
            )}
          </>
        }
      />

      <div className="grid grid-cols-[minmax(0,1fr)_240px] gap-6 px-6 py-5">
        <div className="min-w-0 space-y-4">
          {review.status === "failed" && <ErrorBanner error={review.error ?? "The review failed"} />}
          {cancel.error && <ErrorBanner error={errorMessage(cancel.error)} />}
          {review.status !== "done" && <ProgressLog key={reviewId} reviewId={reviewId} initial={detail.data.log} live={running} />}
          {review.status === "done" && (
            <ReviewResults
              key={review.id}
              reviewId={review.id}
              serverId={review.serverId}
              repo={review.repo}
              summary={review.summary ?? ""}
              verdict={review.verdict}
              findings={detail.data.findings}
              criticalFiles={detail.data.criticalFiles}
              onChanged={() => qc.invalidateQueries({ queryKey: ["review", reviewId] })}
            />
          )}
        </div>

        <aside className="space-y-4">
          <Card className="p-3">
            <div className="mb-2 text-xs font-medium text-muted">Review history</div>
            {history.data?.map((r) => (
              <Link
                key={r.id}
                to={`/reviews/${r.id}`}
                className={clsx(
                  "flex items-center justify-between gap-2 rounded px-2 py-1.5 text-xs hover:bg-panel-2",
                  r.id === reviewId && "bg-panel-2",
                )}
              >
                <span className="font-mono text-muted">{shortSha(r.headSha)}</span>
                <span className="text-muted">{relativeTime(r.createdAt)}</span>
                <ReviewStatusBadge review={r} />
              </Link>
            ))}
          </Card>
          {review.sessionId && review.worktreePath && <ResumeCard reviewId={review.id} />}
        </aside>
      </div>
    </div>
  );
}

function ProgressLog({ reviewId, initial, live }: { reviewId: string; initial: ProgressEvent[]; live: boolean }) {
  const [lines, setLines] = useState<ProgressEvent[]>(initial);
  const bottom = useRef<HTMLDivElement>(null);
  const onEvent = useCallback((e: ProgressEvent) => setLines((l) => [...l, e]), []);
  useReviewProgress(live ? reviewId : undefined, onEvent);
  useEffect(() => bottom.current?.scrollIntoView({ block: "nearest" }), [lines.length]);

  if (lines.length === 0 && !live) return null;
  return (
    <Card>
      <div className="flex items-center gap-2 border-b border-line px-4 py-2 text-xs font-medium text-muted">
        {live && <Spinner className="size-3.5" />} Reviewer activity
      </div>
      <div className="max-h-[420px] overflow-y-auto px-4 py-2 font-mono text-[12px] leading-5">
        {lines.map((l, i) => (
          <div
            key={i}
            className={clsx(
              "truncate",
              l.kind === "error" && "text-danger",
              l.kind === "status" && "font-semibold",
              l.kind === "text" && "whitespace-pre-wrap text-fg/70",
              l.kind === "tool" && "text-muted",
            )}
          >
            {l.kind === "tool" ? `→ ${l.message}` : l.message}
          </div>
        ))}
        <div ref={bottom} />
      </div>
    </Card>
  );
}

function ResumeCard({ reviewId }: { reviewId: string }) {
  const cmd = useQuery({ queryKey: ["resume", reviewId], queryFn: () => api.resumeCommand(reviewId) });
  const [copied, setCopied] = useState(false);
  if (!cmd.data) return null;
  return (
    <Card className="p-3">
      <div className="mb-2 flex items-center gap-1.5 text-xs font-medium text-muted">
        <TerminalSquare className="size-3.5" /> Continue in terminal
      </div>
      <code className="block rounded bg-bg p-2 text-[11px] break-all">{cmd.data}</code>
      <Button
        size="sm"
        variant="ghost"
        className="mt-2"
        onClick={() => {
          navigator.clipboard.writeText(cmd.data).then(() => setCopied(true));
        }}
      >
        <Copy className="size-3" /> {copied ? "Copied" : "Copy command"}
      </Button>
    </Card>
  );
}

export function ReviewResults({
  reviewId,
  serverId,
  repo,
  summary: initialSummary,
  verdict,
  findings: initialFindings,
  criticalFiles = [],
  onChanged,
}: {
  reviewId: string;
  serverId: string;
  repo: string;
  summary: string;
  verdict: Verdict | null;
  findings: Finding[];
  criticalFiles?: CriticalFile[];
  onChanged: () => void;
}) {
  const [findings, setFindings] = useState(initialFindings);
  const [hidden, setHidden] = useState<Set<Severity>>(new Set());
  const [summary, setSummary] = useState(initialSummary);
  const [includeSummary, setIncludeSummary] = useState(true);
  const [mode, setMode] = useState<SubmitMode>(verdict === "request_changes" ? "request_changes" : "comment");

  useEffect(() => setFindings(initialFindings), [initialFindings]);

  const check = useCallback((text: string) => api.checkCommentText(serverId, repo, text), [serverId, repo]);

  // Edits are saved in the background; posting waits for them so it never sends stale text.
  const pendingSaves = useRef(new Map<string, Promise<void>>());
  const [saveErrors, setSaveErrors] = useState<Record<string, string>>({});

  const update = (id: string, patch: Partial<Finding>) => {
    setFindings((fs) => fs.map((f) => (f.id === id ? { ...f, ...patch } : f)));
    const f = findings.find((x) => x.id === id)!;
    const next = { ...f, ...patch };
    const save = api
      .updateFinding(id, next.selected, next.proposedComment)
      .then(() => setSaveErrors(({ [id]: _, ...rest }) => rest))
      .catch((e) => {
        setSaveErrors((errs) => ({ ...errs, [id]: errorMessage(e) }));
        throw e;
      })
      .finally(() => {
        if (pendingSaves.current.get(id) === save) pendingSaves.current.delete(id);
      });
    pendingSaves.current.set(id, save);
  };

  const userComments = useMemo(() => findings.filter((f) => f.source === "user"), [findings]);
  const commentActions: CommentActions = {
    reviewId,
    comments: userComments,
    check,
    onAdd: async (anchor, severity, body) => {
      const added = await api.addUserComment({
        reviewId,
        file: anchor.file,
        lineStart: anchor.lineStart,
        lineEnd: anchor.lineEnd !== anchor.lineStart ? anchor.lineEnd : null,
        side: anchor.side,
        severity,
        body,
      });
      // Same place the backend gives it: after every finding of the same or a higher severity.
      setFindings((fs) => {
        const at = fs.reduce((i, f, k) => (severityRank(f.severity) <= severityRank(added.severity) ? k + 1 : i), 0);
        return [...fs.slice(0, at), added, ...fs.slice(at)];
      });
    },
    onEdit: (id, proposedComment) => update(id, { proposedComment }),
    onDelete: async (id) => {
      await api.deleteUserComment(id);
      pendingSaves.current.delete(id);
      setSaveErrors(({ [id]: _, ...rest }) => rest);
      setFindings((fs) => fs.filter((f) => f.id !== id));
    },
  };

  const counts = useMemo(() => {
    const c = Object.fromEntries(SEVERITIES.map((s) => [s, 0])) as Record<Severity, number>;
    findings.forEach((f) => c[f.severity]++);
    return c;
  }, [findings]);

  const selectable = findings.filter((f) => !f.postedAt);
  // Only what is on screen gets posted: hiding a severity also leaves it out of the post.
  const selected = selectable.filter((f) => f.selected && !hidden.has(f.severity));
  const hiddenSelected = selectable.filter((f) => f.selected && hidden.has(f.severity)).length;
  const summaryCheck = useCommentCheck(summary, check);

  const post = useMutation({
    mutationFn: async () => {
      const results = await Promise.allSettled(pendingSaves.current.values());
      if (results.some((r) => r.status === "rejected")) {
        throw new Error("Some edits could not be saved. Fix them before posting.");
      }
      return api.postFindings({
        reviewId,
        findingIds: selected.map((f) => f.id),
        mode,
        summary: includeSummary ? summary : null,
      });
    },
    onSuccess: () => {
      // The summary (and any approval) went out with this post; don't send it twice.
      setIncludeSummary(false);
      setMode("comment");
      onChanged();
    },
    onError: onChanged,
  });

  const setAll = (value: boolean) => {
    selectable.forEach((f) => {
      if (f.selected !== value && !hidden.has(f.severity)) update(f.id, { selected: value });
    });
  };

  return (
    <div className="space-y-4">
      <Card className="p-4">
        <div className="mb-2 flex items-center gap-2">
          <span className="text-xs font-medium text-muted">Verdict</span>
          <VerdictBadge verdict={verdict} />
          <span className="ml-auto text-xs text-muted">{findings.length} finding(s)</span>
        </div>
        <TextArea aria-label="Review summary" rows={3} value={summary} onChange={(e) => setSummary(e.target.value)} />
        <RulesWarning text={summary} result={summaryCheck} />
        <label className="mt-2 flex items-center gap-2 text-xs text-muted">
          <input
            type="checkbox"
            className="accent-[var(--accent)]"
            checked={includeSummary}
            onChange={(e) => setIncludeSummary(e.target.checked)}
          />
          Post the summary as the review body
        </label>
      </Card>

      <CriticalFiles reviewId={reviewId} files={criticalFiles} comments={commentActions} />

      {findings.length === 0 ? (
        <EmptyState title="No findings">
          The reviewer found nothing worth commenting on.
          {criticalFiles.length > 0 && " You can still add your own comments in the files above."}
        </EmptyState>
      ) : (
        <>
          <div className="flex flex-wrap items-center gap-2">
            {SEVERITIES.filter((s) => counts[s] > 0).map((s) => (
              <button
                key={s}
                type="button"
                aria-pressed={!hidden.has(s)}
                onClick={() =>
                  setHidden((h) => {
                    const n = new Set(h);
                    if (n.has(s)) n.delete(s);
                    else n.add(s);
                    return n;
                  })
                }
                className={clsx(
                  "h-6 shrink-0 rounded-full border px-2.5 text-xs whitespace-nowrap",
                  hidden.has(s) ? "border-line text-muted line-through" : "border-line-strong bg-panel",
                )}
              >
                {SEVERITY_LABEL[s]} {counts[s]}
              </button>
            ))}
            <div className="ml-auto flex gap-1">
              <Button size="sm" variant="ghost" onClick={() => setAll(true)}>
                Select all
              </Button>
              <Button size="sm" variant="ghost" onClick={() => setAll(false)}>
                Select none
              </Button>
            </div>
          </div>

          <div className="space-y-2">
            {findings
              .filter((f) => !hidden.has(f.severity))
              .map((f) => (
                <FindingCard
                  key={f.id}
                  finding={f}
                  check={check}
                  onToggle={(selected) => update(f.id, { selected })}
                  onEdit={(proposedComment) => update(f.id, { proposedComment })}
                  onDelete={f.source === "user" ? () => commentActions.onDelete(f.id) : undefined}
                  saveError={saveErrors[f.id]}
                />
              ))}
          </div>
        </>
      )}

      <div className="sticky bottom-0 -mx-1 flex items-center gap-3 rounded-lg border border-line bg-panel/95 px-4 py-3 shadow-sm backdrop-blur">
        <span className="text-[13px]">
          <strong>{selected.length}</strong> of {selectable.length} selected
          {hiddenSelected > 0 && <span className="text-muted"> ({hiddenSelected} hidden, not posted)</span>}
        </span>
        {post.data && (
          <span className="text-xs text-ok">
            Posted {post.data.posted}
            {post.data.folded > 0 && `, ${post.data.folded} as general comments`}
            {post.data.headMoved && " (PR has newer commits)"}
          </span>
        )}
        {post.error && <span className="truncate text-xs text-danger">{errorMessage(post.error)}</span>}
        <div className="ml-auto flex items-center gap-2">
          <Select
            aria-label="Submit as"
            value={mode}
            onChange={(e) => setMode(e.target.value as SubmitMode)}
            className="w-44"
          >
            <option value="comment">Comment</option>
            <option value="request_changes">Request changes</option>
            <option value="approve">Approve</option>
          </Select>
          <Button
            variant="primary"
            busy={post.isPending}
            disabled={selected.length === 0 && !(includeSummary && summary.trim()) && mode === "comment"}
            onClick={() => post.mutate()}
          >
            Post selected
          </Button>
        </div>
      </div>
    </div>
  );
}
