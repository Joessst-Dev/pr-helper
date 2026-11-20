import { useMemo, useState } from "react";
import { Link, useParams } from "react-router";
import { useQuery } from "@tanstack/react-query";
import { ExternalLink, GitPullRequest, Play, RefreshCw, Search } from "lucide-react";
import { openUrl } from "@tauri-apps/plugin-opener";
import { api, errorMessage } from "../lib/api";
import type { PrFilter } from "../lib/bindings/PrFilter";
import type { PrListItem } from "../lib/bindings/PrListItem";
import { relativeTime } from "../lib/format";
import { prDetailPath, useStartReview } from "../lib/useStartReview";
import { ReviewStatusBadge } from "../components/badges";
import { Badge, Button, EmptyState, ErrorBanner, Input, PageHeader, Segmented, Spinner } from "../components/ui";

const FILTERS: { value: PrFilter; label: string }[] = [
  { value: "review_requested", label: "Review requested" },
  { value: "involved", label: "Involved" },
  { value: "authored", label: "Authored" },
  { value: "all", label: "All visible" },
];

export function PullRequestsPage() {
  const { serverId = "" } = useParams();
  const [filter, setFilter] = useState<PrFilter>("review_requested");
  const [search, setSearch] = useState("");
  const servers = useQuery({ queryKey: ["servers"], queryFn: api.listServers });
  const server = servers.data?.find((s) => s.id === serverId);

  const prs = useQuery({
    queryKey: ["prs", serverId, filter],
    queryFn: () => api.listPrs(serverId, filter),
    enabled: !!serverId,
    staleTime: 60_000,
  });

  const visible = useMemo(() => {
    const q = search.trim().toLowerCase();
    const items = prs.data ?? [];
    if (!q) return items;
    return items.filter(({ pr }) =>
      [pr.repo, pr.title, pr.author, `#${pr.number}`].some((s) => s.toLowerCase().includes(q)),
    );
  }, [prs.data, search]);

  return (
    <div>
      <PageHeader
        title={server?.name ?? "Pull requests"}
        subtitle={server && `${server.baseUrl}${server.username ? ` · signed in as ${server.username}` : ""}`}
        actions={
          <Button onClick={() => prs.refetch()} busy={prs.isFetching} variant="ghost" title="Refresh" aria-label="Refresh">
            <RefreshCw className="size-3.5" />
            Refresh
          </Button>
        }
      />
      <div className="flex flex-wrap items-center gap-3 px-6 py-3">
        <Segmented value={filter} options={FILTERS} onChange={setFilter} />
        <div className="relative ml-auto w-72">
          <Search className="pointer-events-none absolute top-2 left-2.5 size-4 text-muted" />
          <Input
            placeholder="Filter by repo, title, author"
            value={search}
            onChange={(e) => setSearch(e.target.value)}
            className="pl-8"
          />
        </div>
      </div>

      <div className="px-6 pb-8">
        {prs.error && <ErrorBanner error={errorMessage(prs.error)} />}
        {prs.isLoading && (
          <div className="flex items-center gap-2 py-10 text-muted">
            <Spinner /> Loading pull requests…
          </div>
        )}
        {prs.data && visible.length === 0 && (
          <EmptyState icon={<GitPullRequest className="size-8" />} title="No open pull requests">
            {search ? "Nothing matches your filter." : "Try another filter, or widen the server scope in its settings."}
          </EmptyState>
        )}
        {visible.length > 0 && (
          <div className="overflow-hidden rounded-lg border border-line bg-panel">
            {visible.map((item) => (
              <PrRow key={`${item.pr.projectId}#${item.pr.number}`} item={item} serverId={serverId} />
            ))}
          </div>
        )}
      </div>
    </div>
  );
}

function PrRow({ item, serverId }: { item: PrListItem; serverId: string }) {
  const { pr, tracked, latestReview } = item;
  const start = useStartReview(serverId, pr.projectId, pr.number);
  const running = latestReview && (latestReview.status === "running" || latestReview.status === "queued");

  return (
    <div className="flex items-center gap-4 border-b border-line px-4 py-3 last:border-b-0 hover:bg-panel-2/50">
      <GitPullRequest className={pr.draft ? "size-4 shrink-0 text-muted" : "size-4 shrink-0 text-ok"} />
      <div className="min-w-0 flex-1">
        <div className="flex items-center gap-2">
          <Link to={prDetailPath(serverId, pr.projectId, pr.number)} className="truncate font-medium hover:underline">
            {pr.title}
          </Link>
          {pr.draft && <Badge>Draft</Badge>}
          {pr.reviewRequested && <Badge tone="accent">Review requested</Badge>}
          {tracked?.hasNewChanges && <Badge tone="warn">New changes</Badge>}
        </div>
        <div className="mt-0.5 flex items-center gap-1.5 text-xs text-muted">
          <span className="font-mono">
            {pr.repo}#{pr.number}
          </span>
          <span>·</span>
          <span>{pr.author}</span>
          <span>·</span>
          <span>
            {pr.headRef} → {pr.baseRef}
          </span>
          <span>·</span>
          <span>updated {relativeTime(pr.updatedAt)}</span>
        </div>
        {start.error && <div className="mt-1 text-xs text-danger">{errorMessage(start.error)}</div>}
      </div>
      <div className="flex shrink-0 items-center gap-2">
        {latestReview && (
          <Link to={`/reviews/${latestReview.id}`} className="flex items-center gap-1.5 hover:underline">
            <ReviewStatusBadge review={latestReview} />
          </Link>
        )}
        <Button variant="ghost" size="sm" title="Open in browser" aria-label="Open in browser" onClick={() => openUrl(pr.webUrl)}>
          <ExternalLink className="size-3.5" />
        </Button>
        <Button
          size="sm"
          variant={latestReview ? "secondary" : "primary"}
          busy={start.isPending}
          disabled={!!running}
          onClick={() => start.mutate()}
        >
          <Play className="size-3" />
          {latestReview ? "Re-review" : "Review"}
        </Button>
      </div>
    </div>
  );
}
