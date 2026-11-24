import { beforeEach, describe, expect, it, vi } from "vitest";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { ExistingComments, ExistingCommentsDisclosure } from "./ExistingComments";
import type { PrComment } from "../lib/bindings/PrComment";
import type { PrThread } from "../lib/bindings/PrThread";

vi.mock("@tauri-apps/plugin-opener", () => ({ openUrl: vi.fn() }));
const listPrComments = vi.fn();
vi.mock("../lib/api", () => ({
  api: { listPrComments: (...a: unknown[]) => listPrComments(...a) },
  errorMessage: (e: unknown) => String(e),
}));

const comment = (id: string, overrides: Partial<PrComment> = {}): PrComment => ({
  id,
  author: "bob",
  isMe: false,
  body: `Comment ${id}`,
  createdAt: new Date().toISOString(),
  url: null,
  reviewState: null,
  ...overrides,
});

const thread = (id: string, overrides: Partial<PrThread> = {}): PrThread => ({
  id,
  file: null,
  line: null,
  resolved: null,
  outdated: false,
  comments: [comment(`${id}-1`)],
  ...overrides,
});

const props = { serverId: "s1", projectId: "42", number: 7, webUrl: "https://gl/mr/7" };

function renderWith(ui: React.ReactElement) {
  return render(<QueryClientProvider client={new QueryClient()}>{ui}</QueryClientProvider>);
}

describe("ExistingComments", () => {
  beforeEach(() => listPrComments.mockReset());

  it("shows threads and hides resolved ones until asked", async () => {
    listPrComments.mockResolvedValue({
      threads: [
        thread("general", { comments: [comment("g", { author: "alice", reviewState: "approved", body: "LGTM" })] }),
        thread("inline", { file: "src/a.rs", line: 3, resolved: false, outdated: true }),
        thread("done", { file: "src/b.rs", line: 9, resolved: true, comments: [comment("d", { isMe: true })] }),
      ],
      truncated: true,
    });
    renderWith(<ExistingComments {...props} />);
    expect(await screen.findByText("LGTM")).toBeInTheDocument();
    expect(screen.getByText("Approved")).toBeInTheDocument();
    expect(screen.getByText("src/a.rs:3")).toBeInTheDocument();
    expect(screen.getByText("Outdated")).toBeInTheDocument();
    expect(screen.queryByText("src/b.rs:9")).toBeNull();
    expect(screen.getByText(/Only the newest comments were loaded/)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "See all in the browser" })).toBeInTheDocument();

    await userEvent.click(screen.getByRole("checkbox", { name: /Hide resolved threads \(1\)/ }));
    expect(screen.getByText("src/b.rs:9")).toBeInTheDocument();
    expect(screen.getByText("you")).toBeInTheDocument();
    expect(listPrComments).toHaveBeenCalledWith("s1", "42", 7);
  });

  it("says when there are no comments", async () => {
    listPrComments.mockResolvedValue({ threads: [], truncated: false });
    renderWith(<ExistingComments {...props} />);
    expect(await screen.findByText("No comments yet.")).toBeInTheDocument();
  });

  it("never renders comment HTML", async () => {
    listPrComments.mockResolvedValue({
      threads: [thread("x", { comments: [comment("x", { body: 'Hi <img src="x" onerror="alert(1)">' })] })],
      truncated: false,
    });
    const { container } = renderWith(<ExistingComments {...props} />);
    await screen.findByText(/Hi/);
    expect(container.querySelector("img")).toBeNull();
  });

  it("loads only once the disclosure is opened", async () => {
    listPrComments.mockResolvedValue({ threads: [thread("a")], truncated: false });
    renderWith(<ExistingCommentsDisclosure {...props} />);
    expect(listPrComments).not.toHaveBeenCalled();
    await userEvent.click(screen.getByText("Existing comments on the PR"));
    expect(await screen.findByText("Comment a-1")).toBeInTheDocument();
  });
});
