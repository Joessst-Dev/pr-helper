import { Link, useNavigate } from "react-router";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Bell, EyeOff, Play } from "lucide-react";
import { api, errorMessage } from "../lib/api";
import type { TrackedPr } from "../lib/bindings/TrackedPr";
import { relativeTime, shortSha } from "../lib/format";
import { ReviewStatusBadge, VerdictBadge } from "../components/badges";
import { Badge, Button, Card, EmptyState, PageHeader } from "../components/ui";

function TrackedRow({ t }: { t: TrackedPr }) {
  const qc = useQueryClient();
  const navigate = useNavigate();
  const review = useMutation({
    mutationFn: () => api.startReview(t.serverId, t.projectId, t.number),
    onSuccess: (r) => navigate(`/reviews/${r.id}`),
  });
  const untrack = useMutation({
    mutationFn: () => api.untrackPr(t.serverId, t.repo, t.number),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["tracked"] }),
  });
  return (
    <div className="flex items-center gap-4 border-b border-line px-4 py-3 last:border-b-0">
      <div className="min-w-0 flex-1">
        <div className="flex items-center gap-2">
          <span className="truncate font-medium">{t.title}</span>
          {t.hasNewChanges && <Badge tone="warn">New changes</Badge>}
          <VerdictBadge verdict={t.lastVerdict} />
        </div>
        <div className="mt-0.5 text-xs text-muted">
          <span className="font-mono">
            {t.repo}#{t.number}
          </span>{" "}
          · reviewed {shortSha(t.lastReviewedSha) || "never"} · head {shortSha(t.lastSeenHeadSha)} · checked{" "}
          {relativeTime(t.updatedAt)}
        </div>
        {review.error && <div className="text-xs text-danger">{errorMessage(review.error)}</div>}
      </div>
      <Button size="sm" variant={t.hasNewChanges ? "primary" : "secondary"} busy={review.isPending} onClick={() => review.mutate()}>
        <Play className="size-3" /> Review again
      </Button>
      <Button size="sm" variant="ghost" title="Stop watching" busy={untrack.isPending} onClick={() => untrack.mutate()}>
        <EyeOff className="size-3.5" />
      </Button>
    </div>
  );
}

export function WatchingPage() {
  const tracked = useQuery({ queryKey: ["tracked"], queryFn: api.listTracked });
  const recent = useQuery({ queryKey: ["reviews", "recent"], queryFn: () => api.listRecentReviews(15) });
  const sorted = [...(tracked.data ?? [])].sort((a, b) => Number(b.hasNewChanges) - Number(a.hasNewChanges));

  return (
    <div>
      <PageHeader
        title="Watching"
        subtitle="Open PRs you've reviewed. They're polled for new commits; auto-review and auto-approve follow your settings."
      />
      <div className="space-y-6 px-6 py-5">
        {sorted.length === 0 ? (
          <EmptyState icon={<Bell className="size-8" />} title="Nothing watched yet">
            Review a pull request and it shows up here.
          </EmptyState>
        ) : (
          <Card className="overflow-hidden">
            {sorted.map((t) => (
              <TrackedRow key={`${t.serverId}:${t.repo}#${t.number}`} t={t} />
            ))}
          </Card>
        )}

        {recent.data && recent.data.length > 0 && (
          <section>
            <h2 className="mb-2 text-sm font-semibold">Recent reviews</h2>
            <Card className="overflow-hidden">
              {recent.data.map((r) => (
                <Link
                  key={r.id}
                  to={`/reviews/${r.id}`}
                  className="flex items-center gap-3 border-b border-line px-4 py-2.5 text-[13px] last:border-b-0 hover:bg-panel-2/50"
                >
                  <span className="font-mono text-xs text-muted">
                    {r.repo}#{r.number}
                  </span>
                  <span className="min-w-0 flex-1 truncate">{r.prTitle}</span>
                  {r.trigger === "auto" && <Badge>auto</Badge>}
                  <span className="text-xs text-muted">{relativeTime(r.createdAt)}</span>
                  <ReviewStatusBadge review={r} />
                </Link>
              ))}
            </Card>
          </section>
        )}
      </div>
    </div>
  );
}
