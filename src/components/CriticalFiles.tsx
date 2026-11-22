import { useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import clsx from "clsx";
import { ChevronDown, ChevronRight, FileSearch, MessageSquare } from "lucide-react";
import { useQuery } from "@tanstack/react-query";
import { api, errorMessage } from "../lib/api";
import type { CriticalFile } from "../lib/bindings/CriticalFile";
import type { FileDiffLine } from "../lib/bindings/FileDiffLine";
import type { LineRange } from "../lib/bindings/LineRange";
import type { FileNote } from "../lib/bindings/FileNote";
import type { Finding } from "../lib/bindings/Finding";
import type { Side } from "../lib/bindings/Side";
import { isHunkGap, tokenize, tokenizeDiff, type Token } from "../lib/highlight";
import { Badge, Button, Card, Spinner } from "./ui";
import { CommentComposer, UserComment, type CommentActions } from "./CommentComposer";

/** Files the reviewer recommends reading by hand, each expandable to its highlighted code. */
export function CriticalFiles({
  reviewId,
  files,
  comments,
}: {
  reviewId: string;
  files: CriticalFile[];
  /** Lets the user comment on lines; without it the code is read-only. */
  comments?: CommentActions;
}) {
  const [open, setOpen] = useState<string | null>(null);
  if (files.length === 0) return null;
  return (
    <Card>
      <div className="flex items-center gap-2 border-b border-line px-4 py-2 text-xs font-medium text-muted">
        <FileSearch className="size-3.5" /> Files to review yourself
        {comments && (
          <span className="ml-auto font-normal">Click a line number to comment, shift-click to select a range.</span>
        )}
      </div>
      <ul className="divide-y divide-line">
        {files.map((f) => {
          const expanded = open === f.path;
          const count = comments?.comments.filter((c) => c.file === f.path).length ?? 0;
          return (
            <li key={f.path}>
              <button
                type="button"
                aria-expanded={expanded}
                onClick={() => setOpen(expanded ? null : f.path)}
                className="flex w-full items-start gap-2 px-4 py-2.5 text-left hover:bg-panel-2"
              >
                {expanded ? (
                  <ChevronDown className="mt-0.5 size-3.5 shrink-0 text-muted" />
                ) : (
                  <ChevronRight className="mt-0.5 size-3.5 shrink-0 text-muted" />
                )}
                <span className="min-w-0 flex-1">
                  <span className="flex flex-wrap items-center gap-1.5">
                    <span className="font-mono text-[13px] break-all">{f.path}</span>
                    {f.focusLines.map((r) => (
                      <Badge key={`${r.start}-${r.end}`} tone="accent">
                        {rangeLabel(r)}
                      </Badge>
                    ))}
                    {count > 0 && (
                      <Badge tone="info">
                        <MessageSquare className="size-3" /> {count}
                      </Badge>
                    )}
                  </span>
                  {f.reason && <span className="mt-1 block text-[13px] whitespace-pre-wrap text-fg/80">{f.reason}</span>}
                </span>
              </button>
              {expanded && (
                <div className="px-4 pb-3">
                  <CodeView reviewId={reviewId} file={f} comments={comments} />
                </div>
              )}
            </li>
          );
        })}
      </ul>
    </Card>
  );
}

function rangeLabel(r: LineRange) {
  return r.start === r.end ? `L${r.start}` : `L${r.start}-${r.end}`;
}

type Mode = "file" | "diff";

const NOTES: Record<FileNote, string> = {
  binary: "Binary file, only the diff is shown.",
  tooLarge: "The file is too large to show in full, only the diff is shown.",
  deleted: "The PR deletes this file, only the diff is shown.",
  notAFile: "Not a regular file (for example a submodule), only the diff is shown.",
  unreadable: "The file could not be read from git (see the review log), only the diff is shown.",
  pruned: "The code is only kept for the latest review of this PR. Open that review to see it.",
};

export function CodeView({
  reviewId,
  file,
  comments,
}: {
  reviewId: string;
  file: CriticalFile;
  comments?: CommentActions;
}) {
  const query = useQuery({
    queryKey: ["reviewFile", reviewId, file.path],
    queryFn: () => api.getReviewFile(reviewId, file.path),
    staleTime: Infinity,
  });
  const content = query.data?.content ?? null;
  const [mode, setMode] = useState<Mode | null>(null);
  const effective: Mode = mode ?? (content == null ? "diff" : "file");

  if (query.isLoading) {
    return (
      <div className="flex items-center gap-2 py-3 text-xs text-muted">
        <Spinner className="size-3.5" /> Loading file…
      </div>
    );
  }
  if (query.error || !query.data) {
    return (
      <div className="flex items-center gap-3 py-2 text-xs">
        <span className="text-danger">The code of this file could not be loaded.</span>
        <span className="truncate text-muted">{errorMessage(query.error ?? "")}</span>
        <Button size="sm" variant="ghost" busy={query.isFetching} onClick={() => query.refetch()}>
          Retry
        </Button>
      </div>
    );
  }

  const { diff, diffTruncated } = query.data;
  const notice = file.note ? NOTES[file.note] : null;

  return (
    <div className="space-y-2">
      <div className="flex items-center gap-2">
        {content != null && (
          <div role="group" aria-label="View" className="flex overflow-hidden rounded-md border border-line text-xs">
            {(["file", "diff"] as const).map((m) => (
              <button
                key={m}
                type="button"
                aria-pressed={effective === m}
                onClick={() => setMode(m)}
                className={clsx("px-2.5 py-1", effective === m ? "bg-panel-2 font-medium" : "text-muted hover:bg-panel-2")}
              >
                {m === "file" ? "File" : "Diff"}
              </button>
            ))}
          </div>
        )}
        {notice && <span className="text-xs text-muted">{notice}</span>}
        {effective === "diff" && diffTruncated && <span className="text-xs text-muted">The diff is cut off.</span>}
      </div>
      {effective === "file" && content != null ? (
        <FileLines path={file.path} content={content} diff={diff} focus={file.focusLines} comments={comments} />
      ) : file.note === "pruned" ? null : (
        <DiffLines path={file.path} diff={diff} focus={file.focusLines} comments={comments} />
      )}
    </div>
  );
}

/** Tokens for `code`, or `null` while loading or when it stays plain text. */
function useTokens(code: string, path: string) {
  const [tokens, setTokens] = useState<{ code: string; lines: Token[][] } | null>(null);
  useEffect(() => {
    let cancelled = false;
    tokenize(code, path).then((lines) => {
      if (!cancelled && lines) setTokens({ code, lines });
    });
    return () => {
      cancelled = true;
    };
  }, [code, path]);
  return tokens?.code === code ? tokens.lines : null;
}

function useDiffTokens(diff: FileDiffLine[], path: string) {
  const [tokens, setTokens] = useState<{ diff: FileDiffLine[]; lines: (Token[] | undefined)[] } | null>(null);
  useEffect(() => {
    let cancelled = false;
    tokenizeDiff(diff, path).then((lines) => {
      if (!cancelled && lines) setTokens({ diff, lines });
    });
    return () => {
      cancelled = true;
    };
  }, [diff, path]);
  return tokens?.diff === diff ? tokens.lines : null;
}

function inRanges(line: number, ranges: LineRange[]) {
  return ranges.some((r) => line >= r.start && line <= r.end);
}

function Code({ text, tokens }: { text: string; tokens?: Token[] }) {
  if (!tokens) return <>{text || " "}</>;
  if (tokens.length === 0) return <> </>;
  return (
    <>
      {tokens.map((t, i) => (
        <span key={i} className="shiki-token" style={t.style}>
          {t.content}
        </span>
      ))}
    </>
  );
}

/** Scrolls the box so the marked row is near the top, once per `deps` change. */
function useScrollTo(deps: unknown[]) {
  const box = useRef<HTMLDivElement>(null);
  const target = useRef<HTMLDivElement>(null);
  useLayoutEffect(() => {
    if (box.current && target.current) {
      box.current.scrollTop = Math.max(0, target.current.offsetTop - 48);
    }
  }, deps);
  return { box, target };
}

type Selection = { side: Side; start: number; end: number };

/** Lines picked for a new comment: a click starts a selection, a shift-click on the same side extends it. */
function useLineSelection() {
  const [sel, setSel] = useState<{ side: Side; from: number; to: number } | null>(null);
  const pick = (side: Side, line: number, extend: boolean) =>
    setSel((s) => (extend && s && s.side === side ? { ...s, to: line } : { side, from: line, to: line }));
  const range: Selection | null = sel && {
    side: sel.side,
    start: Math.min(sel.from, sel.to),
    end: Math.max(sel.from, sel.to),
  };
  return { range, pick, clear: () => setSel(null) };
}

const inSelection = (sel: Selection | null, side: Side, line: number | null) =>
  !!sel && line != null && sel.side === side && line >= sel.start && line <= sel.end;

/** The user's comments on a file, by the side and last line they are attached to. */
function commentsByLine(path: string, actions?: CommentActions) {
  const map = new Map<string, Finding[]>();
  for (const c of actions?.comments ?? []) {
    if (c.file !== path || c.lineStart == null) continue;
    const key = `${c.side}:${c.lineEnd ?? c.lineStart}`;
    map.set(key, [...(map.get(key) ?? []), c]);
  }
  return map;
}

function LineNumber({
  line,
  side,
  className,
  onPick,
}: {
  line: number | null;
  side: Side;
  className: string;
  onPick?: (side: Side, line: number, extend: boolean) => void;
}) {
  if (line == null || !onPick) return <span className={clsx(className, "text-muted select-none")}>{line ?? ""}</span>;
  return (
    <button
      type="button"
      aria-label={`Comment on ${side === "left" ? "removed " : ""}line ${line}`}
      className={clsx(className, "cursor-pointer text-muted select-none hover:text-accent hover:underline")}
      onClick={(e) => onPick(side, line, e.shiftKey)}
    >
      {line}
    </button>
  );
}

/** Saved comments and the composer that belong under one code row. */
function RowExtras({
  path,
  side,
  line,
  saved,
  selection,
  outsideDiff,
  actions,
  onDone,
}: {
  path: string;
  side: Side;
  line: number | null;
  saved: Map<string, Finding[]>;
  selection: Selection | null;
  outsideDiff: boolean;
  actions?: CommentActions;
  onDone: () => void;
}) {
  if (!actions || line == null) return null;
  const mine = saved.get(`${side}:${line}`) ?? [];
  const compose = selection?.side === side && selection.end === line;
  if (mine.length === 0 && !compose) return null;
  return (
    <div className="sticky left-0 w-full">
      {mine.map((c) => (
        <UserComment key={c.id} comment={c} actions={actions} />
      ))}
      {compose && (
        <CommentComposer
          key={`${selection.side}:${selection.start}-${selection.end}`}
          reviewId={actions.reviewId}
          anchor={{ file: path, lineStart: selection.start, lineEnd: selection.end, side }}
          outsideDiff={outsideDiff}
          check={actions.check}
          onSave={async (text, severity) => {
            await actions.onAdd({ file: path, lineStart: selection.start, lineEnd: selection.end, side }, severity, text);
            onDone();
          }}
          onCancel={onDone}
        />
      )}
    </div>
  );
}

function FileLines({
  path,
  content,
  diff,
  focus,
  comments,
}: {
  path: string;
  content: string;
  diff: FileDiffLine[];
  focus: LineRange[];
  comments?: CommentActions;
}) {
  const lines = useMemo(() => {
    const l = content.split("\n");
    if (l.length > 1 && l[l.length - 1] === "") l.pop();
    return l;
  }, [content]);
  const changed = useMemo(
    () => new Set(diff.filter((d) => d.kind === "added" && d.new != null).map((d) => d.new!)),
    [diff],
  );
  const inDiff = useMemo(
    () => new Set(diff.filter((d) => d.kind !== "removed" && d.new != null).map((d) => d.new!)),
    [diff],
  );
  const tokens = useTokens(content, path);
  const { range, pick, clear } = useLineSelection();
  const saved = useMemo(() => commentsByLine(path, comments), [path, comments]);
  // Jump to what the reviewer pointed at, else to the first change.
  const first = focus[0]?.start ?? [...changed].sort((a, b) => a - b)[0] ?? 1;
  const { box, target } = useScrollTo([path, first]);

  return (
    <div
      ref={box}
      className="relative max-h-[560px] overflow-auto rounded-md border border-line bg-bg py-1 font-mono text-[12px] leading-5"
    >
      {lines.map((text, i) => {
        const no = i + 1;
        const isChanged = changed.has(no);
        const isFocus = inRanges(no, focus);
        const isSelected = inSelection(range, "right", no);
        return (
          <div key={no}>
            <div
              ref={no === first ? target : undefined}
              data-line={no}
              data-changed={isChanged || undefined}
              data-focus={isFocus || undefined}
              data-selected={isSelected || undefined}
              className={clsx(
                "flex min-w-max",
                isSelected ? "bg-warn-soft" : isFocus ? "bg-accent-soft" : isChanged && "bg-diff-add",
              )}
            >
              <span
                className={clsx("w-1 shrink-0", isChanged && "bg-ok")}
                aria-label={isChanged ? "changed" : undefined}
              />
              <LineNumber
                line={no}
                side="right"
                className="w-12 shrink-0 pr-3 text-right"
                onPick={comments && pick}
              />
              <span className="pr-4 whitespace-pre">
                <Code text={text} tokens={tokens?.[i]} />
              </span>
            </div>
            <RowExtras
              path={path}
              side="right"
              line={no}
              saved={saved}
              selection={range}
              outsideDiff={!inDiff.has(range?.end ?? no)}
              actions={comments}
              onDone={clear}
            />
          </div>
        );
      })}
    </div>
  );
}

function DiffLines({
  path,
  diff,
  focus,
  comments,
}: {
  path: string;
  diff: FileDiffLine[];
  focus: LineRange[];
  comments?: CommentActions;
}) {
  const tokens = useDiffTokens(diff, path);
  const { range, pick, clear } = useLineSelection();
  const saved = useMemo(() => commentsByLine(path, comments), [path, comments]);
  const firstFocus = diff.findIndex((d) => d.new != null && inRanges(d.new, focus));
  const first = firstFocus >= 0 ? firstFocus : diff.findIndex((d) => d.kind !== "context");
  const { box, target } = useScrollTo([path, first]);

  if (diff.length === 0) return <div className="py-2 text-xs text-muted">No diff lines for this file.</div>;
  return (
    <div
      ref={box}
      className="relative max-h-[560px] overflow-auto rounded-md border border-line bg-bg py-1 font-mono text-[12px] leading-5"
    >
      {diff.map((d, i) => {
        const prev = diff[i - 1];
        const gap = isHunkGap(prev, d);
        const mark = d.kind === "added" ? "+" : d.kind === "removed" ? "-" : " ";
        // Removed lines are commented on the old side, everything else on the new side.
        const side: Side = d.kind === "removed" ? "left" : "right";
        const line = side === "left" ? d.old : d.new;
        const isSelected = inSelection(range, side, line);
        // A context line can also end a range of removed lines on the old side.
        const leftAlso = d.kind === "context" ? d.old : null;
        return (
          <div key={i}>
            {gap && <div className="px-3 text-muted select-none">⋯</div>}
            <div
              ref={i === first ? target : undefined}
              data-kind={d.kind}
              data-selected={isSelected || undefined}
              className={clsx(
                "flex min-w-max",
                isSelected && "bg-warn-soft",
                !isSelected && d.kind === "added" && "bg-diff-add",
                !isSelected && d.kind === "removed" && "bg-diff-del",
                d.new != null && inRanges(d.new, focus) && "outline outline-1 -outline-offset-1 outline-accent/40",
              )}
            >
              <LineNumber
                line={d.old}
                side="left"
                className="w-10 shrink-0 pr-2 text-right"
                onPick={comments && d.kind === "removed" ? pick : undefined}
              />
              <LineNumber
                line={d.new}
                side="right"
                className="w-10 shrink-0 pr-2 text-right"
                onPick={comments && d.kind !== "removed" ? pick : undefined}
              />
              <span className="w-4 shrink-0 text-muted select-none">{mark}</span>
              <span className="pr-4 whitespace-pre">
                <Code text={d.text} tokens={tokens?.[i]} />
              </span>
            </div>
            <RowExtras
              path={path}
              side={side}
              line={line}
              saved={saved}
              selection={range}
              outsideDiff={false}
              actions={comments}
              onDone={clear}
            />
            {leftAlso != null && (
              <RowExtras
                path={path}
                side="left"
                line={leftAlso}
                saved={saved}
                selection={range}
                outsideDiff={false}
                actions={comments}
                onDone={clear}
              />
            )}
          </div>
        );
      })}
    </div>
  );
}
