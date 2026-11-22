import { describe, expect, it, vi } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { FindingCard } from "./FindingCard";
import { finding } from "../test/fixtures";

vi.mock("@tauri-apps/plugin-opener", () => ({ openUrl: vi.fn() }));

const cleanCheck = vi.fn(async (text: string) => ({
  dashViolations: [],
  attributionViolations: [],
  sanitized: text.trim(),
}));

describe("FindingCard", () => {
  it("toggles selection and saves edits on blur", async () => {
    const onToggle = vi.fn();
    const onEdit = vi.fn();
    render(<FindingCard finding={finding()} check={cleanCheck} onToggle={onToggle} onEdit={onEdit} />);

    await userEvent.click(screen.getByRole("checkbox", { name: /Off-by-one/ }));
    expect(onToggle).toHaveBeenCalledWith(false);

    const box = screen.getByRole("textbox", { name: "Proposed comment" });
    await userEvent.clear(box);
    await userEvent.type(box, "Use an inclusive range.");
    await userEvent.tab();
    expect(onEdit).toHaveBeenCalledWith("Use an inclusive range.");
  });

  it("warns when the text breaks the comment rules", async () => {
    const check = vi.fn(async () => ({
      dashViolations: ["wrong — fix"],
      attributionViolations: [],
      sanitized: "wrong, fix",
    }));
    render(<FindingCard finding={finding({ proposedComment: "wrong — fix" })} check={check} onToggle={vi.fn()} onEdit={vi.fn()} />);
    await waitFor(() => expect(screen.getByText(/Breaks the comment rules/)).toBeInTheDocument());
    expect(check).toHaveBeenCalledWith("wrong — fix");
  });

  it("marks findings outside the diff and locks posted ones", () => {
    render(
      <FindingCard
        finding={finding({ inDiff: false, postedAt: "2026-01-01T00:00:00Z" })}
        check={cleanCheck}
        onToggle={vi.fn()}
        onEdit={vi.fn()}
      />,
    );
    expect(screen.getByText(/outside the diff/)).toBeInTheDocument();
    expect(screen.getByText("Posted")).toBeInTheDocument();
    expect(screen.getByRole("checkbox")).toBeDisabled();
    expect(screen.getByRole("textbox", { name: "Proposed comment" })).toBeDisabled();
  });

  it("marks the user's own comments and deletes them until posted", async () => {
    const onDelete = vi.fn().mockRejectedValueOnce("gone").mockResolvedValue(undefined);
    const mine = finding({ source: "user", category: "comment", explanation: "" });
    const { rerender } = render(
      <FindingCard finding={mine} check={cleanCheck} onToggle={vi.fn()} onEdit={vi.fn()} onDelete={onDelete} />,
    );
    expect(screen.getAllByText("Your comment")).toHaveLength(2);
    expect(screen.queryByText("comment")).toBeNull();
    await userEvent.click(screen.getByRole("button", { name: "Delete comment" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("Not deleted: gone");
    await userEvent.click(screen.getByRole("button", { name: "Delete comment" }));
    expect(onDelete).toHaveBeenCalledTimes(2);

    rerender(
      <FindingCard
        finding={{ ...mine, postedAt: "2026-01-01T00:00:00Z" }}
        check={cleanCheck}
        onToggle={vi.fn()}
        onEdit={vi.fn()}
        onDelete={onDelete}
      />,
    );
    expect(screen.queryByRole("button", { name: "Delete comment" })).toBeNull();
  });

  it("offers no delete for reviewer findings", () => {
    render(<FindingCard finding={finding()} check={cleanCheck} onToggle={vi.fn()} onEdit={vi.fn()} />);
    expect(screen.queryByRole("button", { name: "Delete comment" })).toBeNull();
  });
});
