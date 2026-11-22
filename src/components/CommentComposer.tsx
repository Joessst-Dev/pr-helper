import { useEffect, useRef, useState } from "react";
import { MessageSquare, Pencil, Sparkles, Trash2 } from "lucide-react";
import { api, errorMessage } from "../lib/api";
import type { CommentAdvice } from "../lib/bindings/CommentAdvice";
import type { CommentAssessment } from "../lib/bindings/CommentAssessment";
import type { Finding } from "../lib/bindings/Finding";
import type { Severity } from "../lib/bindings/Severity";
import type { Side } from "../lib/bindings/Side";
import { formatCost, SEVERITIES, SEVERITY_LABEL } from "../lib/format";
import { SeverityBadge } from "./badges";
import { RulesWarning, useCommentCheck, type CheckFn } from "./FindingCard";
import { Badge, Button, Select, TextArea } from "./ui";

/** Lines of a critical file a comment is attached to. */
export type CommentAnchor = { file: string; lineStart: number; lineEnd: number; side: Side };

/** What the code views need to let the user comment on lines. */
export type CommentActions = {
  reviewId: string;
  /** The user's own comments (findings with `source = user`). */
  comments: Finding[];
  check: CheckFn;
  onAdd: (anchor: CommentAnchor, severity: Severity, text: string) => Promise<void>;
  onEdit: (id: string, text: string) => void;
  onDelete: (id: string) => Promise<void>;
};

const ASSESSMENT: Record<CommentAssessment, { label: string; tone: "ok" | "warn" | "danger" }> = {
  valid: { label: "Holds up", tone: "ok" },
  partly_valid: { label: "Partly holds", tone: "warn" },
  not_valid: { label: "Does not hold", tone: "danger" },
};

export function anchorLabel(a: Pick<CommentAnchor, "lineStart" | "lineEnd" | "side">) {
  const lines = a.lineEnd !== a.lineStart ? `lines ${a.lineStart}-${a.lineEnd}` : `line ${a.lineStart}`;
  return a.side === "left" ? `${lines} (removed)` : lines;
}

/** A comment being written, kept by the code view so it survives changing the selected lines. */
export type Draft = { text: string; severity: Severity };
export const EMPTY_DRAFT: Draft = { text: "", severity: "medium" };

export type Advice = ReturnType<typeof useAdvice>;

/** Runs the AI check of a draft; cancels it when the owner goes away or a new check starts. */
export function useAdvice(reviewId: string) {
  const running = useRef<string | null>(null);
  const [state, setState] = useState<{ busy: boolean; advice: CommentAdvice | null; error: string | null }>({
    busy: false,
    advice: null,
    error: null,
  });
  const cancel = () => {
    if (running.current) void api.cancelCommentCheck(running.current).catch(() => {});
    running.current = null;
  };
  useEffect(() => cancel, []);

  const ask = async (anchor: CommentAnchor, text: string, severity: Severity) => {
    cancel();
    const id = crypto.randomUUID();
    running.current = id;
    setState({ busy: true, advice: null, error: null });
    try {
      const advice = await api.checkUserComment(
        {
          reviewId,
          file: anchor.file,
          lineStart: anchor.lineStart,
          lineEnd: anchor.lineEnd !== anchor.lineStart ? anchor.lineEnd : null,
          side: anchor.side,
          severity,
          body: text,
        },
        id,
      );
      if (running.current === id) setState({ busy: false, advice, error: null });
    } catch (e) {
      if (running.current === id) setState({ busy: false, advice: null, error: errorMessage(e) });
    } finally {
      if (running.current === id) running.current = null;
    }
  };
  const stop = () => {
    cancel();
    setState({ busy: false, advice: null, error: null });
  };
  const dismiss = () => setState((s) => ({ ...s, advice: null }));
  return { ...state, ask, stop, dismiss };
}

function AdvicePanel({ advice, onUse, onDismiss }: { advice: CommentAdvice; onUse: () => void; onDismiss: () => void }) {
  const a = ASSESSMENT[advice.assessment];
  return (
    <div className="space-y-2 rounded-md border border-line bg-panel-2 p-3" aria-label="AI feedback">
      <div className="flex items-center gap-2 text-xs">
        <Sparkles className="size-3.5 text-muted" />
        <Badge tone={a.tone}>{a.label}</Badge>
        {advice.costUsd != null && <span className="ml-auto text-muted">{formatCost(advice.costUsd)}</span>}
      </div>
      {advice.feedback && <p className="text-[13px] whitespace-pre-wrap text-fg/85">{advice.feedback}</p>}
      {advice.suggestedComment && (
        <div>
          <div className="mb-1 text-xs font-medium text-muted">Suggested comment</div>
          <div className="rounded-md border border-line bg-bg px-3 py-2 font-sans text-[13px] whitespace-pre-wrap">
            {advice.suggestedComment}
          </div>
        </div>
      )}
      <div className="flex gap-2">
        {advice.suggestedComment && (
          <Button size="sm" onClick={onUse}>
            Use suggestion
          </Button>
        )}
        <Button size="sm" variant="ghost" onClick={onDismiss}>
          Keep mine
        </Button>
      </div>
    </div>
  );
}

/** Writes a new comment or edits a saved one, with an optional AI check of the draft. */
export function CommentComposer({
  reviewId,
  anchor,
  initialText = "",
  initialSeverity = "medium",
  editing = false,
  outsideDiff = false,
  check,
  draft,
  onDraftChange,
  ai: sharedAi,
  onSave,
  onCancel,
}: {
  reviewId: string;
  anchor: CommentAnchor;
  initialText?: string;
  initialSeverity?: Severity;
  /** Editing a saved comment: the severity stays as it is. */
  editing?: boolean;
  outsideDiff?: boolean;
  check: CheckFn;
  /** Where the text lives when it must outlive this composer (a new comment whose lines change). */
  draft?: Draft;
  onDraftChange?: (draft: Draft) => void;
  /** An AI check owned by the caller, so a running check survives a remount too. */
  ai?: Advice;
  onSave: (text: string, severity: Severity) => Promise<void> | void;
  onCancel: () => void;
}) {
  const [text, setTextState] = useState(draft?.text ?? initialText);
  const [severity, setSeverityState] = useState<Severity>(draft?.severity ?? initialSeverity);
  const setText = (t: string) => {
    setTextState(t);
    onDraftChange?.({ text: t, severity });
  };
  const setSeverity = (s: Severity) => {
    setSeverityState(s);
    onDraftChange?.({ text, severity: s });
  };
  const [saving, setSaving] = useState(false);
  const [saveError, setSaveError] = useState<string | null>(null);
  const result = useCommentCheck(text, check);
  const ownAi = useAdvice(reviewId);
  const ai = sharedAi ?? ownAi;
  const empty = !text.trim();

  const save = async () => {
    setSaving(true);
    setSaveError(null);
    try {
      await onSave(text, severity);
    } catch (e) {
      setSaveError(errorMessage(e));
      setSaving(false);
    }
  };

  return (
    <div className="space-y-2 border-y border-line bg-panel px-3 py-2.5 font-sans text-[13px]" data-composer>
      <div className="flex items-center gap-2 text-xs text-muted">
        <MessageSquare className="size-3.5" />
        <span>Your comment on {anchorLabel(anchor)}</span>
        {outsideDiff && <span className="text-warn">Not part of the diff, will be posted as a general comment.</span>}
      </div>
      <TextArea
        aria-label="Your comment"
        autoFocus
        rows={Math.min(10, Math.max(3, text.split("\n").length + 1))}
        value={text}
        onChange={(e) => setText(e.target.value)}
      />
      <RulesWarning text={text} result={result} />
      {ai.advice && (
        <AdvicePanel
          advice={ai.advice}
          onUse={() => {
            setText(ai.advice!.suggestedComment);
            ai.dismiss();
          }}
          onDismiss={ai.dismiss}
        />
      )}
      {ai.error && (
        <div role="alert" className="text-xs text-danger">
          The AI check failed: {ai.error}
        </div>
      )}
      {saveError && (
        <div role="alert" className="text-xs text-danger">
          Not saved: {saveError}
        </div>
      )}
      <div className="flex items-center gap-2">
        {!editing && (
          <Select
            aria-label="Severity"
            className="w-32"
            value={severity}
            onChange={(e) => setSeverity(e.target.value as Severity)}
          >
            {SEVERITIES.map((s) => (
              <option key={s} value={s}>
                {SEVERITY_LABEL[s]}
              </option>
            ))}
          </Select>
        )}
        {ai.busy ? (
          <Button size="sm" variant="ghost" onClick={ai.stop}>
            Stop AI check
          </Button>
        ) : (
          <Button size="sm" variant="ghost" disabled={empty} onClick={() => ai.ask(anchor, text, severity)}>
            <Sparkles className="size-3" /> Ask AI
          </Button>
        )}
        {ai.busy && <span className="text-xs text-muted">Checking your comment…</span>}
        <div className="ml-auto flex gap-2">
          <Button size="sm" variant="ghost" onClick={onCancel}>
            Cancel
          </Button>
          <Button size="sm" variant="primary" busy={saving} disabled={empty} onClick={save}>
            {editing ? "Save" : "Add comment"}
          </Button>
        </div>
      </div>
    </div>
  );
}

/** A saved comment of the user, shown under the line it is attached to. */
export function UserComment({ comment, actions }: { comment: Finding; actions: CommentActions }) {
  const [editing, setEditing] = useState(false);
  const [deleting, setDeleting] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const posted = !!comment.postedAt;

  if (editing) {
    return (
      <CommentComposer
        reviewId={actions.reviewId}
        anchor={{
          file: comment.file ?? "",
          lineStart: comment.lineStart ?? 1,
          lineEnd: comment.lineEnd ?? comment.lineStart ?? 1,
          side: comment.side,
        }}
        initialText={comment.proposedComment}
        initialSeverity={comment.severity}
        editing
        outsideDiff={!comment.inDiff}
        check={actions.check}
        onSave={(text) => {
          actions.onEdit(comment.id, text);
          setEditing(false);
        }}
        onCancel={() => setEditing(false)}
      />
    );
  }
  return (
    <div className="border-y border-line bg-panel px-3 py-2 font-sans text-[13px]" data-user-comment={comment.id}>
      <div className="mb-1 flex items-center gap-2 text-xs text-muted">
        <MessageSquare className="size-3.5" />
        <SeverityBadge severity={comment.severity} />
        <span>Your comment</span>
        {posted && <Badge tone="ok">Posted</Badge>}
        {!posted && !comment.selected && <span>(not selected for posting)</span>}
        {!posted && (
          <span className="ml-auto flex gap-1">
            <Button size="sm" variant="ghost" aria-label="Edit comment" onClick={() => setEditing(true)}>
              <Pencil className="size-3" /> Edit
            </Button>
            <Button
              size="sm"
              variant="ghost"
              aria-label="Delete comment"
              busy={deleting}
              onClick={async () => {
                setDeleting(true);
                setError(null);
                try {
                  await actions.onDelete(comment.id);
                } catch (e) {
                  setError(errorMessage(e));
                  setDeleting(false);
                }
              }}
            >
              <Trash2 className="size-3" /> Delete
            </Button>
          </span>
        )}
      </div>
      <p className="whitespace-pre-wrap">{comment.proposedComment}</p>
      {error && (
        <div role="alert" className="mt-1 text-xs text-danger">
          {error}
        </div>
      )}
    </div>
  );
}
