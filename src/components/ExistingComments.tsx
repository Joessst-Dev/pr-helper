import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { ExternalLink } from "lucide-react";
import { openUrl } from "@tauri-apps/plugin-opener";
import { api, errorMessage } from "../lib/api";
import type { PrComment } from "../lib/bindings/PrComment";
import type { PrThread } from "../lib/bindings/PrThread";
import { location, relativeTime, REVIEWER_STATE_LABEL } from "../lib/format";
import { Markdown } from "./Markdown";
import { Badge, Card, ErrorBanner, Spinner } from "./ui";

/** `webUrl` only links the "see all" note; the comments carry their own links. */
type Props = { serverId: string; projectId: string; number: number; webUrl?: string | null };

function usePrComments({ serverId, projectId, number }: Props) {
  return useQuery({
    queryKey: ["pr-comments", serverId, projectId, number],
    queryFn: () => api.listPrComments(serverId, projectId, number),
    enabled: !!serverId && !!projectId,
    staleTime: 60_000,
  });
}

/**
 * Everyone's comments on a PR, read-only. The text is author-controlled: it is rendered as
 * markdown without HTML and kept apart from findings, so it can never be posted.
 */
export function ExistingComments(props: Props) {
  const comments = usePrComments(props);
  const [hideResolved, setHideResolved] = useState(true);

  if (comments.isLoading) {
    return (
      <div className="flex items-center gap-2 text-[13px] text-muted">
        <Spinner /> Loading comments…
      </div>
    );
  }
  if (comments.error) return <ErrorBanner error={errorMessage(comments.error)} />;
  if (!comments.data) return null;

  const { threads, truncated } = comments.data;
  const resolved = threads.filter((t) => t.resolved === true).length;
  const shown = hideResolved ? threads.filter((t) => t.resolved !== true) : threads;

  return (
    <div className="space-y-3">
      {threads.length === 0 ? (
        <p className="text-[13px] text-muted">No comments yet.</p>
      ) : (
        resolved > 0 && (
          <label className="flex items-center gap-2 text-xs text-muted">
            <input
              type="checkbox"
              className="accent-[var(--accent)]"
              checked={hideResolved}
              onChange={(e) => setHideResolved(e.target.checked)}
            />
            Hide resolved threads ({resolved})
          </label>
        )
      )}
      {shown.map((t) => (
        <ThreadCard key={t.id} thread={t} />
      ))}
      {truncated && <TruncatedNote webUrl={props.webUrl} />}
    </div>
  );
}

/** Collapsed version for the review page. Loads the comments only once opened. */
export function ExistingCommentsDisclosure(props: Props) {
  const [open, setOpen] = useState(false);
  return (
    <details className="rounded-lg border border-line bg-panel" onToggle={(e) => setOpen(e.currentTarget.open)}>
      <summary className="cursor-pointer px-4 py-2.5 text-xs font-medium text-muted">Existing comments on the PR</summary>
      {open && (
        <div className="border-t border-line p-4">
          <ExistingComments {...props} />
        </div>
      )}
    </details>
  );
}

function TruncatedNote({ webUrl }: { webUrl?: string | null }) {
  return (
    <p className="text-xs text-muted">
      Only the newest comments were loaded.
      {webUrl && (
        <>
          {" "}
          <button type="button" className="text-accent hover:underline" onClick={() => openUrl(webUrl)}>
            See all in the browser
          </button>
        </>
      )}
    </p>
  );
}

function ThreadCard({ thread }: { thread: PrThread }) {
  return (
    <Card className="space-y-3 p-4 text-[13px]">
      {(thread.file || thread.resolved != null || thread.outdated) && (
        <div className="flex flex-wrap items-center gap-2">
          {thread.file && <span className="font-mono text-xs">{location(thread.file, thread.line, null)}</span>}
          {thread.resolved === true && <Badge tone="ok">Resolved</Badge>}
          {thread.resolved === false && <Badge>Unresolved</Badge>}
          {thread.outdated && <Badge tone="warn">Outdated</Badge>}
        </div>
      )}
      <ul className="space-y-3">
        {thread.comments.map((c) => (
          <CommentItem key={c.id} comment={c} />
        ))}
      </ul>
    </Card>
  );
}

function CommentItem({ comment }: { comment: PrComment }) {
  const url = comment.url;
  return (
    <li className="border-l-2 border-line pl-3">
      <div className="mb-1 flex flex-wrap items-center gap-1.5 text-xs text-muted">
        <span className="font-medium text-fg">{comment.author}</span>
        {comment.isMe && <Badge>you</Badge>}
        {comment.reviewState && (
          <Badge tone={comment.reviewState === "approved" ? "ok" : comment.reviewState === "changes_requested" ? "warn" : "neutral"}>
            {REVIEWER_STATE_LABEL[comment.reviewState]}
          </Badge>
        )}
        <span>· {relativeTime(comment.createdAt)}</span>
        {url && (
          <button
            type="button"
            className="ml-auto text-muted hover:text-fg"
            title="Open in browser"
            aria-label="Open comment in browser"
            onClick={() => openUrl(url)}
          >
            <ExternalLink className="size-3" />
          </button>
        )}
      </div>
      {comment.body.trim() ? <Markdown>{comment.body}</Markdown> : <p className="text-muted">No text.</p>}
    </li>
  );
}
