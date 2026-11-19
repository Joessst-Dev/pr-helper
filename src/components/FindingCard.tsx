import { useEffect, useState } from "react";
import clsx from "clsx";
import { ChevronDown, ChevronRight, CircleCheck, ExternalLink, TriangleAlert } from "lucide-react";
import { openUrl } from "@tauri-apps/plugin-opener";
import type { CommentCheck } from "../lib/bindings/CommentCheck";
import type { Finding } from "../lib/bindings/Finding";
import { location } from "../lib/format";
import { SeverityBadge } from "./badges";
import { Badge, TextArea } from "./ui";

export type CheckFn = (text: string) => Promise<CommentCheck>;

/** Debounced backend check of a comment against the posting rules. */
export function useCommentCheck(text: string, check: CheckFn, delay = 300) {
  const [result, setResult] = useState<CommentCheck | null>(null);
  useEffect(() => {
    let cancelled = false;
    const t = setTimeout(() => {
      check(text)
        .then((r) => !cancelled && setResult(r))
        .catch(() => !cancelled && setResult(null));
    }, delay);
    return () => {
      cancelled = true;
      clearTimeout(t);
    };
  }, [text, check, delay]);
  return result;
}

/** Live warning about text that breaks the comment rules and will be rewritten on posting. */
export function RulesWarning({ text, result }: { text: string; result: CommentCheck | null }) {
  if (!result) return null;
  const problems = [...result.dashViolations, ...result.attributionViolations];
  if (problems.length > 0) {
    return (
      <div className="mt-1.5 flex items-start gap-1.5 text-xs text-warn">
        <TriangleAlert className="mt-0.5 size-3.5 shrink-0" />
        <span>
          Breaks the comment rules ({problems.map((p) => `"${p}"`).join(", ")}). It will be rewritten before posting.
        </span>
      </div>
    );
  }
  if (result.sanitized !== text.trim()) {
    return <div className="mt-1.5 text-xs text-muted">Formatting will be normalized before posting.</div>;
  }
  return null;
}

function DiffExcerpt({ excerpt }: { excerpt: string }) {
  return (
    <pre className="overflow-x-auto rounded-md border border-line bg-bg py-1 font-mono text-[12px] leading-5">
      {excerpt.split("\n").map((line, i) => (
        <div
          key={i}
          className={clsx(
            "px-3 whitespace-pre",
            line.startsWith("+") && "bg-diff-add",
            line.startsWith("-") && "bg-diff-del",
          )}
        >
          {line}
        </div>
      ))}
    </pre>
  );
}

export function FindingCard({
  finding,
  check,
  onToggle,
  onEdit,
  saveError,
}: {
  finding: Finding;
  check: CheckFn;
  onToggle: (selected: boolean) => void;
  onEdit: (text: string) => void;
  /** Set when the last change to this finding could not be saved. */
  saveError?: string;
}) {
  const [text, setText] = useState(finding.proposedComment);
  const [open, setOpen] = useState(finding.severity !== "nit");
  const [showDiff, setShowDiff] = useState(false);
  const posted = !!finding.postedAt;
  const result = useCommentCheck(text, check);

  useEffect(() => setText(finding.proposedComment), [finding.id, finding.proposedComment]);

  return (
    <div
      className={clsx(
        "rounded-lg border bg-panel transition-colors",
        finding.selected && !posted ? "border-accent/50" : "border-line",
        posted && "opacity-70",
      )}
    >
      <div className="flex items-start gap-3 px-4 py-3">
        <input
          type="checkbox"
          aria-label={`Select finding: ${finding.title}`}
          className="mt-1 size-4 accent-[var(--accent)]"
          checked={finding.selected}
          disabled={posted}
          onChange={(e) => onToggle(e.target.checked)}
        />
        <button type="button" className="min-w-0 flex-1 text-left" onClick={() => setOpen(!open)}>
          <div className="flex flex-wrap items-center gap-2">
            {open ? <ChevronDown className="size-3.5 text-muted" /> : <ChevronRight className="size-3.5 text-muted" />}
            <SeverityBadge severity={finding.severity} />
            <span className="font-medium">{finding.title}</span>
            <Badge>{finding.category}</Badge>
            {posted && (
              <Badge tone="ok">
                <CircleCheck className="size-3" /> Posted
              </Badge>
            )}
          </div>
          {saveError && (
            <div role="alert" className="mt-1 pl-5 text-xs text-danger">
              Not saved: {saveError}
            </div>
          )}
          <div className="mt-1 flex items-center gap-2 pl-5 font-mono text-xs text-muted">
            {location(finding.file, finding.lineStart, finding.lineEnd)}
            {finding.file && !finding.inDiff && (
              <span className="font-sans text-[11px] text-warn">outside the diff, posted as a general comment</span>
            )}
          </div>
        </button>
        {posted && finding.postedRef && (
          <button
            type="button"
            className="rounded p-1 text-muted hover:text-fg"
            title="Open posted comment"
            aria-label="Open posted comment"
            onClick={() => openUrl(finding.postedRef!)}
          >
            <ExternalLink className="size-3.5" />
          </button>
        )}
      </div>

      {open && (
        <div className="space-y-3 border-t border-line px-4 py-3 pl-11">
          <p className="text-[13px] leading-relaxed whitespace-pre-wrap text-fg/85">{finding.explanation}</p>

          {finding.diffExcerpt && (
            <div>
              <button
                type="button"
                className="mb-1 text-xs text-muted hover:text-fg"
                onClick={() => setShowDiff(!showDiff)}
              >
                {showDiff ? "Hide" : "Show"} diff context
              </button>
              {showDiff && <DiffExcerpt excerpt={finding.diffExcerpt} />}
            </div>
          )}

          <div>
            <div className="mb-1 text-xs font-medium text-muted">Proposed comment</div>
            <TextArea
              aria-label="Proposed comment"
              rows={Math.min(10, Math.max(3, text.split("\n").length + 1))}
              value={text}
              disabled={posted}
              onChange={(e) => setText(e.target.value)}
              onBlur={() => text !== finding.proposedComment && onEdit(text)}
            />
            <RulesWarning text={text} result={result} />
          </div>

          {finding.suggestion && (
            <div>
              <div className="mb-1 text-xs font-medium text-muted">Suggested change</div>
              <pre className="overflow-x-auto rounded-md border border-line bg-bg px-3 py-2 font-mono text-[12px]">
                {finding.suggestion}
              </pre>
            </div>
          )}
        </div>
      )}
    </div>
  );
}
