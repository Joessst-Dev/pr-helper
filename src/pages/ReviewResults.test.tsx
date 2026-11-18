import { beforeEach, describe, expect, it, vi } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { ReviewResults } from "./ReviewPage";
import { finding } from "../test/fixtures";

vi.mock("@tauri-apps/plugin-opener", () => ({ openUrl: vi.fn() }));
const postFindings = vi.fn();
const updateFinding = vi.fn();
vi.mock("../lib/api", () => ({
  api: {
    postFindings: (...a: unknown[]) => postFindings(...a),
    updateFinding: (...a: unknown[]) => updateFinding(...a),
    checkCommentText: async (_s: string, _r: string, text: string) => ({
      dashViolations: [],
      attributionViolations: [],
      sanitized: text,
    }),
  },
  errorMessage: (e: unknown) => String(e),
}));

function renderResults() {
  const findings = [
    finding({ id: "a", severity: "critical", title: "SQL injection" }),
    finding({ id: "b", severity: "low", title: "Naming", selected: false }),
    finding({ id: "c", severity: "nit", title: "Typo", selected: false }),
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
        onChanged={vi.fn()}
      />
    </QueryClientProvider>,
  );
}

describe("ReviewResults", () => {
  beforeEach(() => {
    postFindings.mockReset().mockResolvedValue({ posted: 2, folded: 0, reviewUrl: null, headMoved: false });
    updateFinding.mockReset().mockResolvedValue({});
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
});
