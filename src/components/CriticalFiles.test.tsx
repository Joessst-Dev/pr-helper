import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import type { CriticalFile } from "../lib/bindings/CriticalFile";
import type { CriticalFileContent } from "../lib/bindings/CriticalFileContent";
import { CriticalFiles } from "./CriticalFiles";
import type { CommentActions } from "./CommentComposer";
import { finding } from "../test/fixtures";

const getReviewFile = vi.fn();
const checkUserComment = vi.fn();
const cancelCommentCheck = vi.fn();
vi.mock("../lib/api", () => ({
  api: {
    getReviewFile: (...a: unknown[]) => getReviewFile(...a),
    checkUserComment: (...a: unknown[]) => checkUserComment(...a),
    cancelCommentCheck: (...a: unknown[]) => cancelCommentCheck(...a),
  },
  errorMessage: (e: unknown) => String(e),
}));
// Highlighting is covered by shiki itself; here every line gets one token, so we can check
// that tokens are rendered as plain spans.
vi.mock("../lib/highlight", async (importOriginal) => {
  const tokenize = async (code: string) =>
    code.split("\n").map((line) => [{ content: line, style: { "--shiki-light": "#111" } }]);
  const actual = await importOriginal<typeof import("../lib/highlight")>();
  return {
    isHunkGap: actual.isHunkGap,
    tokenize,
    tokenizeDiff: async (diff: { text: string }[]) => tokenize(diff.map((d) => d.text).join("\n")),
  };
});

const files: CriticalFile[] = [
  {
    path: "src/auth.rs",
    reason: "The token check moved into the middleware.",
    focusLines: [{ start: 2, end: 3 }],
    note: null,
  },
  { path: "assets/logo.png", reason: "Replaced logo.", focusLines: [], note: "binary" },
  { path: "src/old.rs", reason: "Older review.", focusLines: [], note: "pruned" },
];

const authContent: CriticalFileContent = {
  content: "fn a() {}\nfn check() {}\nfn b() {}\n<img src=x onerror=alert(1)>\n",
  diff: [
    { kind: "context", old: 1, new: 1, text: "fn a() {}" },
    { kind: "removed", old: 2, new: null, text: "fn old_check() {}" },
    { kind: "added", old: null, new: 2, text: "fn check() {}" },
    { kind: "context", old: 3, new: 3, text: "fn b() {}" },
  ],
  diffTruncated: false,
};

function renderFiles() {
  return render(
    <QueryClientProvider client={new QueryClient({ defaultOptions: { queries: { retry: false } } })}>
      <CriticalFiles reviewId="r1" files={files} />
    </QueryClientProvider>,
  );
}

describe("CriticalFiles", () => {
  beforeEach(() => {
    getReviewFile.mockReset().mockImplementation(async (_r: string, path: string) => {
      if (path === "src/auth.rs") return authContent;
      if (path === "src/old.rs") return { content: null, diff: [], diffTruncated: false };
      if (path === "assets/logo.png") {
        return { content: null, diff: [{ kind: "added", old: null, new: 1, text: "Binary files differ" }], diffTruncated: false };
      }
      throw new Error("not found: file x of review r1");
    });
  });

  it("lists files with the reviewer's reason and focus ranges", () => {
    renderFiles();
    expect(screen.getByText("Files to review yourself")).toBeInTheDocument();
    expect(screen.getByText("The token check moved into the middleware.")).toBeInTheDocument();
    expect(screen.getByText("L2-3")).toBeInTheDocument();
    expect(getReviewFile).not.toHaveBeenCalled();
  });

  it("renders nothing without files", () => {
    const { container } = render(
      <QueryClientProvider client={new QueryClient()}>
        <CriticalFiles reviewId="r1" files={[]} />
      </QueryClientProvider>,
    );
    expect(container).toBeEmptyDOMElement();
  });

  it("expands the full file with changed and focus lines marked, and toggles to the diff", async () => {
    const { container } = renderFiles();
    await userEvent.click(screen.getByRole("button", { name: /src\/auth\.rs/ }));
    expect(getReviewFile).toHaveBeenCalledWith("r1", "src/auth.rs");

    const line2 = await screen.findByText("fn check() {}");
    const row = line2.closest("[data-line]")!;
    expect(row).toHaveAttribute("data-line", "2");
    expect(row).toHaveAttribute("data-changed", "true");
    expect(row).toHaveAttribute("data-focus", "true");
    expect(container.querySelector('[data-line="1"]')).not.toHaveAttribute("data-changed");
    expect(container.querySelector('[data-line="3"]')).toHaveAttribute("data-focus", "true");
    // Author-controlled code is text, never markup.
    expect(screen.getByText("<img src=x onerror=alert(1)>")).toBeInTheDocument();
    expect(container.querySelector("img")).toBeNull();
    expect(line2.closest(".shiki-token")).not.toBeNull();

    await userEvent.click(screen.getByRole("button", { name: "Diff" }));
    expect(await screen.findByText("fn old_check() {}")).toBeInTheDocument();
    expect(container.querySelector('[data-kind="removed"]')).toHaveTextContent("fn old_check() {}");
    expect(container.querySelector('[data-kind="added"]')).toHaveTextContent("fn check() {}");
  });

  it("explains pruned files instead of showing an empty diff", async () => {
    renderFiles();
    await userEvent.click(screen.getByRole("button", { name: /src\/old\.rs/ }));
    expect(await screen.findByText(/only kept for the latest review/)).toBeInTheDocument();
    expect(screen.queryByText("No diff lines for this file.")).toBeNull();
  });

  it("explains a failed load and retries", async () => {
    render(
      <QueryClientProvider client={new QueryClient({ defaultOptions: { queries: { retry: false } } })}>
        <CriticalFiles reviewId="r1" files={[{ path: "gone.rs", reason: "r", focusLines: [], note: null }]} />
      </QueryClientProvider>,
    );
    await userEvent.click(screen.getByRole("button", { name: /gone\.rs/ }));
    expect(await screen.findByText("The code of this file could not be loaded.")).toBeInTheDocument();
    getReviewFile.mockResolvedValueOnce(authContent);
    await userEvent.click(screen.getByRole("button", { name: "Retry" }));
    expect(await screen.findByText("fn check() {}")).toBeInTheDocument();
  });

  it("shows only the diff for binary files", async () => {
    renderFiles();
    await userEvent.click(screen.getByRole("button", { name: /assets\/logo\.png/ }));
    expect(await screen.findByText(/Binary file, only the diff is shown/)).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "File" })).toBeNull();
    expect(screen.getByText("Binary files differ")).toBeInTheDocument();
  });

  describe("own comments", () => {
    function actions(overrides: Partial<CommentActions> = {}): CommentActions {
      return {
        reviewId: "r1",
        comments: [],
        check: async (text) => ({ dashViolations: [], attributionViolations: [], sanitized: text }),
        onAdd: vi.fn().mockResolvedValue(undefined),
        onEdit: vi.fn(),
        onDelete: vi.fn().mockResolvedValue(undefined),
        ...overrides,
      };
    }

    function renderWith(a: CommentActions) {
      return render(
        <QueryClientProvider client={new QueryClient({ defaultOptions: { queries: { retry: false } } })}>
          <CriticalFiles reviewId="r1" files={files} comments={a} />
        </QueryClientProvider>,
      );
    }

    async function openAuth() {
      await userEvent.click(screen.getByRole("button", { name: /src\/auth\.rs/ }));
      await screen.findByText("fn check() {}");
    }

    beforeEach(() => {
      checkUserComment.mockReset();
      cancelCommentCheck.mockReset().mockResolvedValue(true);
    });

    it("comments on a range of the file picked with click and shift-click", async () => {
      const a = actions();
      const { container } = renderWith(a);
      await openAuth();
      await userEvent.click(screen.getByRole("button", { name: "Comment on line 2" }));
      fireEvent.click(screen.getByRole("button", { name: "Comment on line 3" }), { shiftKey: true });
      expect(container.querySelectorAll("[data-selected]")).toHaveLength(2);
      expect(screen.getByText("Your comment on lines 2-3")).toBeInTheDocument();
      expect(screen.queryByText(/Not part of the diff/)).toBeNull();

      await userEvent.type(screen.getByRole("textbox", { name: "Your comment" }), "Check the lock order.");
      await userEvent.selectOptions(screen.getByRole("combobox", { name: "Severity" }), "high");
      await userEvent.click(screen.getByRole("button", { name: "Add comment" }));
      expect(a.onAdd).toHaveBeenCalledWith(
        { file: "src/auth.rs", lineStart: 2, lineEnd: 3, side: "right" },
        "high",
        "Check the lock order.",
      );
      expect(screen.queryByRole("textbox", { name: "Your comment" })).toBeNull();
    });

    it("keeps the draft and a running AI check when the selection changes", async () => {
      checkUserComment.mockReturnValue(new Promise(() => {}));
      const a = actions();
      renderWith(a);
      await openAuth();
      await userEvent.click(screen.getByRole("button", { name: "Comment on line 2" }));
      await userEvent.type(screen.getByRole("textbox", { name: "Your comment" }), "Long thought");
      await userEvent.selectOptions(screen.getByRole("combobox", { name: "Severity" }), "low");
      await userEvent.click(screen.getByRole("button", { name: /Ask AI/ }));
      fireEvent.click(screen.getByRole("button", { name: "Comment on line 4" }), { shiftKey: true });
      expect(screen.getByText("Your comment on lines 2-4")).toBeInTheDocument();
      expect(screen.getByRole("textbox", { name: "Your comment" })).toHaveValue("Long thought");
      expect(screen.getByRole("combobox", { name: "Severity" })).toHaveValue("low");
      expect(screen.getByText("Checking your comment…")).toBeInTheDocument();
      expect(cancelCommentCheck).not.toHaveBeenCalled();
      await userEvent.click(screen.getByRole("button", { name: "Add comment" }));
      expect(a.onAdd).toHaveBeenCalledWith(
        { file: "src/auth.rs", lineStart: 2, lineEnd: 4, side: "right" },
        "low",
        "Long thought",
      );
      // Saving ends the check and starts the next comment empty.
      expect(cancelCommentCheck).toHaveBeenCalled();
      await userEvent.click(screen.getByRole("button", { name: "Comment on line 1" }));
      expect(screen.getByRole("textbox", { name: "Your comment" })).toHaveValue("");
    });

    it("keeps the draft when switching between file and diff view", async () => {
      renderWith(actions());
      await openAuth();
      await userEvent.click(screen.getByRole("button", { name: "Comment on line 2" }));
      await userEvent.type(screen.getByRole("textbox", { name: "Your comment" }), "Keep me");
      await userEvent.click(screen.getByRole("button", { name: "Diff" }));
      // Line 2 is an added line in the diff, so the composer follows it there.
      expect(await screen.findByRole("textbox", { name: "Your comment" })).toHaveValue("Keep me");
      await userEvent.click(screen.getByRole("button", { name: "File" }));
      expect(screen.getByRole("textbox", { name: "Your comment" })).toHaveValue("Keep me");
    });

    it("does not offer AI advice for other lines than it checked", async () => {
      checkUserComment.mockResolvedValue({
        assessment: "valid",
        feedback: "Right.",
        suggestedComment: "About line 2.",
        costUsd: null,
      });
      renderWith(actions());
      await openAuth();
      await userEvent.click(screen.getByRole("button", { name: "Comment on line 2" }));
      await userEvent.type(screen.getByRole("textbox", { name: "Your comment" }), "x");
      await userEvent.click(screen.getByRole("button", { name: /Ask AI/ }));
      expect(await screen.findByRole("button", { name: "Use suggestion" })).toBeInTheDocument();
      await userEvent.click(screen.getByRole("button", { name: "Comment on line 4" }));
      expect(screen.queryByRole("button", { name: "Use suggestion" })).toBeNull();
      expect(screen.getByText(/The AI feedback was for line 2/)).toBeInTheDocument();
    });

    it("blocks deleting while a post is running", async () => {
      const mine = finding({ id: "u1", source: "user", file: "src/auth.rs", lineStart: 2 });
      renderWith(actions({ comments: [mine], posting: true }));
      await openAuth();
      expect(screen.getByRole("button", { name: "Delete comment" })).toBeDisabled();
    });

    it("points to the diff view for comments on removed lines", async () => {
      const removed = finding({ id: "u2", source: "user", file: "src/auth.rs", lineStart: 2, side: "left" });
      renderWith(actions({ comments: [removed] }));
      await openAuth();
      expect(screen.getByText("1 comment on removed lines is shown in the Diff view.")).toBeInTheDocument();
      await userEvent.click(screen.getByRole("button", { name: "Diff" }));
      expect(await screen.findByText("Your comment")).toBeInTheDocument();
      expect(screen.queryByText(/shown in the Diff view/)).toBeNull();
    });

    it("warns that lines outside the diff become a general comment", async () => {
      renderWith(actions());
      await openAuth();
      await userEvent.click(screen.getByRole("button", { name: "Comment on line 4" }));
      expect(screen.getByText(/Not part of the diff, will be posted as a general comment/)).toBeInTheDocument();
    });

    it("comments on removed lines on the old side in the diff", async () => {
      const a = actions();
      renderWith(a);
      await openAuth();
      await userEvent.click(screen.getByRole("button", { name: "Diff" }));
      await userEvent.click(await screen.findByRole("button", { name: "Comment on removed line 2" }));
      expect(screen.getByText("Your comment on line 2 (removed)")).toBeInTheDocument();
      await userEvent.type(screen.getByRole("textbox", { name: "Your comment" }), "Why remove it?");
      await userEvent.click(screen.getByRole("button", { name: "Add comment" }));
      expect(a.onAdd).toHaveBeenCalledWith(
        { file: "src/auth.rs", lineStart: 2, lineEnd: 2, side: "left" },
        "medium",
        "Why remove it?",
      );
    });

    it("asks the AI about a draft and takes its suggestion", async () => {
      checkUserComment.mockResolvedValue({
        assessment: "partly_valid",
        feedback: "The lock is taken in the caller.",
        suggestedComment: "Is the caller always holding the lock here?",
        costUsd: 0.02,
      });
      renderWith(actions());
      await openAuth();
      await userEvent.click(screen.getByRole("button", { name: "Comment on line 2" }));
      await userEvent.type(screen.getByRole("textbox", { name: "Your comment" }), "Missing lock");
      await userEvent.click(screen.getByRole("button", { name: /Ask AI/ }));
      expect(checkUserComment).toHaveBeenCalledWith(
        {
          reviewId: "r1",
          file: "src/auth.rs",
          lineStart: 2,
          lineEnd: null,
          side: "right",
          severity: "medium",
          body: "Missing lock",
        },
        expect.any(String),
      );
      expect(await screen.findByText("Partly holds")).toBeInTheDocument();
      expect(screen.getByText("The lock is taken in the caller.")).toBeInTheDocument();
      await userEvent.click(screen.getByRole("button", { name: "Use suggestion" }));
      expect(screen.getByRole("textbox", { name: "Your comment" })).toHaveValue(
        "Is the caller always holding the lock here?",
      );
      expect(screen.queryByText("Partly holds")).toBeNull();
    });

    it("cancels a running AI check when the composer closes", async () => {
      checkUserComment.mockReturnValue(new Promise(() => {}));
      renderWith(actions());
      await openAuth();
      await userEvent.click(screen.getByRole("button", { name: "Comment on line 2" }));
      await userEvent.type(screen.getByRole("textbox", { name: "Your comment" }), "x");
      await userEvent.click(screen.getByRole("button", { name: /Ask AI/ }));
      const requestId = checkUserComment.mock.calls[0][1];
      await userEvent.click(screen.getByRole("button", { name: "Cancel" }));
      expect(cancelCommentCheck).toHaveBeenCalledWith(requestId);
    });

    it("shows saved comments under their line and deletes them", async () => {
      const mine = finding({
        id: "u1",
        source: "user",
        file: "src/auth.rs",
        lineStart: 2,
        lineEnd: null,
        severity: "low",
        proposedComment: "Rename this.",
      });
      const a = actions({ comments: [mine] });
      const { container } = renderWith(a);
      // The file row counts the comments.
      expect(screen.getByRole("button", { name: /src\/auth\.rs/ })).toHaveTextContent("1");
      await openAuth();
      const saved = container.querySelector('[data-user-comment="u1"]')!;
      expect(saved).toHaveTextContent("Rename this.");
      // It sits right after line 2.
      expect(saved.parentElement!.previousElementSibling).toHaveAttribute("data-line", "2");
      await userEvent.click(screen.getByRole("button", { name: "Delete comment" }));
      expect(a.onDelete).toHaveBeenCalledWith("u1");
    });

    it("edits a saved comment", async () => {
      const mine = finding({ id: "u1", source: "user", file: "src/auth.rs", lineStart: 2, proposedComment: "Old" });
      const a = actions({ comments: [mine] });
      renderWith(a);
      await openAuth();
      await userEvent.click(screen.getByRole("button", { name: "Edit comment" }));
      expect(screen.queryByRole("combobox", { name: "Severity" })).toBeNull();
      const box = screen.getByRole("textbox", { name: "Your comment" });
      await userEvent.clear(box);
      await userEvent.type(box, "New");
      await userEvent.click(screen.getByRole("button", { name: "Save" }));
      expect(a.onEdit).toHaveBeenCalledWith("u1", "New");
    });

    it("is read-only without comment actions", async () => {
      renderFiles();
      await openAuth();
      expect(screen.queryByRole("button", { name: "Comment on line 2" })).toBeNull();
    });
  });
});
