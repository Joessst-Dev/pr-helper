import { type ReactNode, useState } from "react";
import { Link, useParams } from "react-router";
import { useQuery } from "@tanstack/react-query";
import { ArrowLeft, ExternalLink, FileText, Play, RefreshCw, Ticket as TicketIcon } from "lucide-react";
import { openUrl } from "@tauri-apps/plugin-opener";
import { api, errorMessage } from "../lib/api";
import type { PrTicketInfo } from "../lib/bindings/PrTicketInfo";
import type { Review } from "../lib/bindings/Review";
import type { Ticket } from "../lib/bindings/Ticket";
import type { TicketLoad } from "../lib/bindings/TicketLoad";
import { relativeTime } from "../lib/format";
import { useStartReview } from "../lib/useStartReview";
import { Markdown } from "../components/Markdown";
import { ReviewStatusBadge } from "../components/badges";
import { Badge, Button, Card, ErrorBanner, PageHeader, Spinner } from "../components/ui";

export function PrDetailPage() {
  const { serverId = "", projectId = "", number: numberParam = "" } = useParams();
  const number = Number(numberParam);
  const detail = useQuery({
    queryKey: ["pr", serverId, projectId, number],
    queryFn: () => api.getPrDetail(serverId, projectId, number),
    enabled: !!serverId && !!projectId && Number.isInteger(number),
    staleTime: 60_000,
  });
  const start = useStartReview(serverId, projectId, number);

  if (detail.isLoading) {
    return (
      <div className="flex items-center gap-2 p-8 text-muted">
        <Spinner /> Loading pull request…
      </div>
    );
  }
  if (detail.error || !detail.data) {
    return (
      <div className="p-6">
        <ErrorBanner error={errorMessage(detail.error ?? "Pull request not found")} />
      </div>
    );
  }

  const { pr, tracked, reviews, tickets } = detail.data;
  const latest = reviews[0];
  const running = latest && (latest.status === "running" || latest.status === "queued");

  return (
    <div>
      <PageHeader
        title={
          <span>
            <span className="font-mono text-muted">
              {pr.repo}#{pr.number}
            </span>{" "}
            {pr.title}
          </span>
        }
        subtitle={
          <span className="flex flex-wrap items-center gap-1.5">
            {pr.draft && <Badge>Draft</Badge>}
            {pr.reviewRequested && <Badge tone="accent">Review requested</Badge>}
            {tracked?.hasNewChanges && <Badge tone="warn">New changes</Badge>}
            <span>{pr.author}</span>
            <span>·</span>
            <span className="font-mono">
              {pr.headRef} → {pr.baseRef}
            </span>
            <span>·</span>
            <span>updated {relativeTime(pr.updatedAt)}</span>
          </span>
        }
        actions={
          <>
            <Link to={`/servers/${serverId}`} className="flex h-8 items-center gap-1 px-2 text-[13px] text-muted hover:text-fg">
              <ArrowLeft className="size-3.5" /> Back
            </Link>
            <Button variant="ghost" onClick={() => detail.refetch()} busy={detail.isFetching} title="Refresh" aria-label="Refresh">
              <RefreshCw className="size-3.5" />
            </Button>
            <Button variant="ghost" onClick={() => openUrl(pr.webUrl)}>
              <ExternalLink className="size-3.5" /> Open in browser
            </Button>
            <Button
              variant={latest ? "secondary" : "primary"}
              busy={start.isPending}
              disabled={!!running}
              onClick={() => start.mutate()}
            >
              <Play className="size-3" />
              {latest ? "Re-review" : "Review"}
            </Button>
          </>
        }
      />

      <div className="grid grid-cols-[minmax(0,1fr)_240px] gap-6 px-6 py-5">
        <div className="min-w-0 space-y-6">
          {start.error && <ErrorBanner error={errorMessage(start.error)} />}
          <section>
            <SectionTitle icon={<FileText className="size-3.5" />}>Description</SectionTitle>
            <Card className="p-4">
              {pr.description.trim() ? (
                <Markdown>{pr.description}</Markdown>
              ) : (
                <p className="text-[13px] text-muted">No description.</p>
              )}
            </Card>
          </section>
          {tickets && (
            // Keyed by source so a changed source (e.g. Jira to MCP) resets the load-on-request state.
            <TicketsSection
              key={`${tickets.sourceKind ?? ""}:${tickets.sourceName}`}
              serverId={serverId}
              repo={pr.repo}
              info={tickets}
            />
          )}
        </div>

        <aside className="space-y-2">
          <SectionTitle>Reviews</SectionTitle>
          {reviews.length === 0 ? (
            <p className="text-[13px] text-muted">Not reviewed yet.</p>
          ) : (
            <ul className="space-y-1">
              {reviews.map((r) => (
                <ReviewItem key={r.id} review={r} />
              ))}
            </ul>
          )}
        </aside>
      </div>
    </div>
  );
}

function SectionTitle({ icon, children }: { icon?: ReactNode; children: ReactNode }) {
  return (
    <h2 className="mb-2 flex items-center gap-1.5 text-[11px] font-medium tracking-wide text-muted uppercase">
      {icon}
      {children}
    </h2>
  );
}

function ReviewItem({ review }: { review: Review }) {
  return (
    <li>
      <Link
        to={`/reviews/${review.id}`}
        className="flex items-center justify-between gap-2 rounded-md px-2 py-1.5 text-[13px] hover:bg-panel-2"
      >
        <ReviewStatusBadge review={review} />
        <span className="text-xs text-muted">{relativeTime(review.createdAt)}</span>
      </Link>
    </li>
  );
}

/**
 * Loads tickets and stops the backend fetch (an MCP fetcher run is a paid Claude run) when
 * React Query aborts the query, e.g. because the page was left.
 */
async function loadTickets(serverId: string, repo: string, keys: string[], signal: AbortSignal) {
  const requestId = crypto.randomUUID();
  const cancel = () => void api.cancelTicketLoad(requestId).catch(() => {});
  if (signal.aborted) throw signal.reason;
  signal.addEventListener("abort", cancel, { once: true });
  try {
    return await api.loadPrTickets(serverId, repo, keys, requestId);
  } finally {
    signal.removeEventListener("abort", cancel);
  }
}

export function TicketsSection({ serverId, repo, info }: { serverId: string; repo: string; info: PrTicketInfo }) {
  // MCP fetches spawn a Claude run, so they only start on request. Jira REST loads right away.
  const [requested, setRequested] = useState(info.sourceKind !== "mcp");
  const tickets = useQuery({
    queryKey: ["pr-tickets", serverId, repo, info.sourceKind, info.sourceName, info.keys],
    queryFn: ({ signal }) => loadTickets(serverId, repo, info.keys, signal),
    enabled: requested && info.keys.length > 0,
    staleTime: 5 * 60_000,
  });

  return (
    <section>
      <SectionTitle icon={<TicketIcon className="size-3.5" />}>
        Tickets{info.sourceName && <span className="normal-case"> · {info.sourceName}</span>}
      </SectionTitle>
      {info.message && <p className="text-[13px] text-muted">{info.message}</p>}
      {info.keys.length > 0 && !requested && (
        <Card className="flex items-center gap-3 p-4">
          <span className="flex flex-wrap gap-1">
            {info.keys.map((k) => (
              <Badge key={k} className="font-mono">
                {k}
              </Badge>
            ))}
          </span>
          <Button size="sm" className="ml-auto" onClick={() => setRequested(true)}>
            Load tickets
          </Button>
        </Card>
      )}
      {tickets.isFetching && !tickets.data && (
        <div className="flex items-center gap-2 text-[13px] text-muted">
          <Spinner /> Loading {info.keys.join(", ")}…
        </div>
      )}
      {tickets.error && <ErrorBanner error={errorMessage(tickets.error)} />}
      {tickets.data && (
        <div className="space-y-3">
          {tickets.data.map((t) => (
            <TicketResult key={t.key} load={t} />
          ))}
        </div>
      )}
    </section>
  );
}

function TicketResult({ load }: { load: TicketLoad }) {
  if (!load.ticket) {
    return (
      <Card className="p-4 text-[13px]">
        <span className="font-mono">{load.key}</span>
        <span className="text-danger"> could not be loaded: {load.error ?? "unknown error"}</span>
      </Card>
    );
  }
  return <TicketCard ticket={load.ticket} />;
}

/** Ticket text is shown as plain text: Jira returns wiki markup or flattened rich text, not markdown. */
function TicketCard({ ticket }: { ticket: Ticket }) {
  const url = ticket.url;
  return (
    <Card className="space-y-3 p-4 text-[13px]">
      <div className="flex flex-wrap items-center gap-2">
        {url ? (
          <button type="button" className="font-mono text-accent hover:underline" onClick={() => openUrl(url)}>
            {ticket.key}
          </button>
        ) : (
          <span className="font-mono">{ticket.key}</span>
        )}
        <span className="font-medium">{ticket.summary}</span>
        {ticket.issueType && <Badge>{ticket.issueType}</Badge>}
        {ticket.status && <Badge tone="accent">{ticket.status}</Badge>}
      </div>
      {ticket.parent && <div className="text-xs text-muted">Parent: {ticket.parent}</div>}
      {ticket.description ? (
        <p className="whitespace-pre-wrap">{ticket.description}</p>
      ) : (
        <p className="text-muted">No description.</p>
      )}
      {ticket.fields.map(([name, text]) => (
        <div key={name}>
          <div className="text-xs font-medium text-muted">{name}</div>
          <p className="whitespace-pre-wrap">{text}</p>
        </div>
      ))}
      {ticket.comments.length > 0 && (
        <details>
          <summary className="cursor-pointer text-xs text-muted">
            {ticket.comments.length === ticket.commentTotal
              ? `${ticket.commentTotal} comments`
              : `Last ${ticket.comments.length} of ${ticket.commentTotal} comments`}
          </summary>
          <ul className="mt-2 space-y-2">
            {ticket.comments.map((c, i) => (
              <li key={i} className="border-l-2 border-line pl-3">
                <div className="text-xs text-muted">
                  {c.author} · {c.created}
                </div>
                <p className="whitespace-pre-wrap">{c.body}</p>
              </li>
            ))}
          </ul>
        </details>
      )}
    </Card>
  );
}
