import { beforeEach, describe, expect, it, vi } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { ReviewResults } from "./ReviewPage";
import { finding } from "../test/fixtures";
import type { CriticalFile } from "../lib/bindings/CriticalFile";

vi.mock("@tauri-apps/plugin-opener", () => ({ openUrl: vi.fn() }));
const postFindings = vi.fn();
const updateFinding = vi.fn();
const deleteUserComment = vi.fn();
const addUserComment = vi.fn();
const getReviewFile = vi.fn();
vi.mock("../lib/api", () => ({
  api: {
    addUserComment: (...a: unknown[]) => addUserComment(...a),
    getReviewFile: (...a: unknown[]) => getReviewFile(...a),
    postFindings: (...a: unknown[]) => postFindings(...a),
    updateFinding: (...a: unknown[]) => updateFinding(...a),
    deleteUserComment: (...a: unknown[]) => deleteUserComment(...a),
    checkCommentText: async (_s: string, _r: string, text: string) => ({
      dashViolations: [],
      attributionViolations: [],
      sanitized: text,
    }),
  },
  errorMessage: (e: unknown) => String(e),
}));

function renderResults(extra: ReturnType<typeof finding>[] = [], criticalFiles: CriticalFile[] = []) {
  const findings = [
    finding({ id: "a", severity: "critical", title: "SQL injection" }),
    finding({ id: "b", severity: "low", title: "Naming", selected: false }),
    finding({ id: "c", severity: "nit", title: "Typo", selected: false }),
    ...extra,
  ];
  return render(
    <QueryClientProvider client={new QueryClient()}>
      <ReviewResults
        reviewId="r1"
        serverId="s1"
        repo="acme/app"
        summary="Two issues."
        verdict="request_changes"
        findings={findings}
        criticalFiles={criticalFiles}
        onChanged={vi.fn()}
      />
    </QueryClientProvider>,
  );
}

describe("ReviewResults", () => {
  beforeEach(() => {
    postFindings.mockReset().mockResolvedValue({ posted: 2, folded: 0, reviewUrl: null, headMoved: false });
    updateFinding.mockReset().mockResolvedValue({});
    deleteUserComment.mockReset().mockResolvedValue(undefined);
  });

  it("adds an own comment in severity order", async () => {
    getReviewFile.mockResolvedValue({
      content: "fn a() {}\nfn b() {}\n",
      diff: [{ kind: "added", old: null, new: 2, text: "fn b() {}" }],
      diffTruncated: false,
    });
    addUserComment.mockImplementation(async (input) =>
      finding({ id: "u1", source: "user", title: "Mine", severity: input.severity, proposedComment: input.body }),
    );
    renderResults([], [{ path: "src/b.rs", reason: "r", focusLines: [], note: null }]);
    await userEvent.click(screen.getByRole("button", { name: /src\/b\.rs/ }));
    await userEvent.click(await screen.findByRole("button", { name: "Comment on line 2" }));
    await userEvent.type(screen.getByRole("textbox", { name: "Your comment" }), "Mine");
    await userEvent.selectOptions(screen.getByRole("combobox", { name: "Severity" }), "high");
    await userEvent.click(screen.getByRole("button", { name: "Add comment" }));
    expect(addUserComment).toHaveBeenCalledWith({
      reviewId: "r1",
      file: "src/b.rs",
      lineStart: 2,
      lineEnd: null,
      side: "right",
      severity: "high",
      body: "Mine",
    });
    // After the critical finding, before the low and nit ones.
    const titles = await screen.findAllByRole("checkbox", { name: /Select finding/ });
    expect(titles.map((c) => c.getAttribute("aria-label"))).toEqual([
      "Select finding: SQL injection",
      "Select finding: Mine",
      "Select finding: Naming",
      "Select finding: Typo",
    ]);
  });

  it("posts the user's own comments with the findings and can delete them", async () => {
    const mine = finding({ id: "u1", source: "user", category: "comment", title: "Rename this", severity: "medium" });
    renderResults([mine]);
    expect(screen.getByText(/of 4 selected/)).toHaveTextContent("2 of 4 selected");
    await userEvent.click(screen.getByRole("button", { name: "Delete comment" }));
    expect(deleteUserComment).toHaveBeenCalledWith("u1");
    await waitFor(() => expect(screen.queryByText("Rename this")).toBeNull());
    expect(screen.getByText(/of 3 selected/)).toHaveTextContent("1 of 3 selected");
  });

  it("posts exactly the selected findings with the chosen mode", async () => {
    renderResults();
    expect(screen.getByText(/of 3 selected/)).toHaveTextContent("1 of 3 selected");
    expect(screen.getByRole("combobox", { name: "Submit as" })).toHaveValue("request_changes");

    await userEvent.click(screen.getByRole("checkbox", { name: /Naming/ }));
    expect(updateFinding).toHaveBeenCalledWith("b", true, expect.any(String));
    await userEvent.selectOptions(screen.getByRole("combobox", { name: "Submit as" }), "comment");
    await userEvent.click(screen.getByRole("button", { name: "Post selected" }));

    await waitFor(() => expect(postFindings).toHaveBeenCalledTimes(1));
    expect(postFindings).toHaveBeenCalledWith({
      reviewId: "r1",
      findingIds: ["a", "b"],
      mode: "comment",
      summary: "Two issues.",
    });
  });

  it("can omit the summary and hide severities", async () => {
    renderResults();
    await userEvent.click(screen.getByRole("button", { name: /Nit 1/ }));
    expect(screen.queryByText("Typo")).not.toBeInTheDocument();
    await userEvent.click(screen.getByRole("checkbox", { name: /Post the summary/ }));
    await userEvent.click(screen.getByRole("button", { name: "Post selected" }));
    await waitFor(() => expect(postFindings).toHaveBeenCalled());
    expect(postFindings.mock.calls[0][0].summary).toBeNull();
  });

  it("leaves hidden findings out of the post", async () => {
    renderResults();
    await userEvent.click(screen.getByRole("button", { name: /Critical 1/ }));
    expect(screen.getByText(/of 3 selected/)).toHaveTextContent("0 of 3 selected (1 hidden, not posted)");
    await userEvent.click(screen.getByRole("button", { name: "Post selected" }));
    await waitFor(() => expect(postFindings).toHaveBeenCalled());
    expect(postFindings.mock.calls[0][0].findingIds).toEqual([]);
  });

  it("waits for comment edits to save and refuses to post when a save failed", async () => {
    let finishSave: () => void = () => {};
    updateFinding.mockImplementationOnce(() => new Promise<void>((r) => (finishSave = r)));
    renderResults();
    const box = screen.getAllByRole("textbox", { name: "Proposed comment" })[0];
    await userEvent.type(box, " Edited.");
    await userEvent.click(screen.getByRole("button", { name: "Post selected" }));
    expect(updateFinding).toHaveBeenCalledWith("a", true, expect.stringContaining("Edited."));
    expect(postFindings).not.toHaveBeenCalled();
    finishSave();
    await waitFor(() => expect(postFindings).toHaveBeenCalledTimes(1));

    updateFinding.mockRejectedValueOnce("database is locked");
    await userEvent.type(box, " Again.");
    await userEvent.click(screen.getByRole("button", { name: "Post selected" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("Not saved: database is locked");
    expect(screen.getByText(/could not be saved/)).toBeInTheDocument();
    expect(postFindings).toHaveBeenCalledTimes(1);
  });

  it("does not send the summary again after a successful post", async () => {
    renderResults();
    await userEvent.click(screen.getByRole("button", { name: "Post selected" }));
    await waitFor(() => expect(postFindings).toHaveBeenCalledTimes(1));
    expect(screen.getByRole("checkbox", { name: /Post the summary/ })).not.toBeChecked();
  });
});
