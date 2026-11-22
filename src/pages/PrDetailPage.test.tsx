import { beforeEach, describe, expect, it, vi } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { createMemoryRouter, RouterProvider } from "react-router";
import { PrDetailPage, TicketsSection } from "./PrDetailPage";
import { prDetailPath } from "../lib/useStartReview";
import type { PrTicketInfo } from "../lib/bindings/PrTicketInfo";
import type { TicketLoad } from "../lib/bindings/TicketLoad";

vi.mock("@tauri-apps/plugin-opener", () => ({ openUrl: vi.fn() }));
const loadPrTickets = vi.fn();
const getPrDetail = vi.fn();
const cancelTicketLoad = vi.fn();
const loadTicketImage = vi.fn();
vi.mock("../lib/api", () => ({
  api: {
    loadPrTickets: (...a: unknown[]) => loadPrTickets(...a),
    getPrDetail: (...a: unknown[]) => getPrDetail(...a),
    cancelTicketLoad: (...a: unknown[]) => cancelTicketLoad(...a),
    loadTicketImage: (...a: unknown[]) => loadTicketImage(...a),
  },
  errorMessage: (e: unknown) => String(e),
}));

const loaded: TicketLoad[] = [
  {
    key: "ABC-7",
    ticket: {
      key: "ABC-7",
      url: "https://acme.atlassian.net/browse/ABC-7",
      summary: "Reset password",
      issueType: "Story",
      status: "In Progress",
      description: "*FFT:* As a user I want to reset my password.\n!flow.png|thumbnail!\n!https://t.example/pixel.png!",
      parent: null,
      fields: [["Acceptance criteria", "Email is sent"]],
      comments: [],
      commentTotal: 0,
      textFormat: "jira_wiki",
      attachments: [{ id: "10001", filename: "flow.png", mimeType: "image/png", size: 10 }],
    },
    error: null,
  },
  { key: "ABC-8", ticket: null, error: "not found" },
];

function renderSection(info: PrTicketInfo) {
  return render(
    <QueryClientProvider client={new QueryClient()}>
      <TicketsSection serverId="s1" repo="acme/app" info={info} />
    </QueryClientProvider>,
  );
}

describe("TicketsSection", () => {
  beforeEach(() => {
    loadPrTickets.mockReset();
    loadPrTickets.mockResolvedValue(loaded);
    loadTicketImage.mockReset();
    loadTicketImage.mockResolvedValue("data:image/png;base64,AAAA");
  });

  it("loads Jira REST tickets right away", async () => {
    renderSection({ sourceName: "Acme Jira", sourceKind: "jira_cloud", keys: ["ABC-7", "ABC-8"], message: null });
    expect(await screen.findByText("Reset password")).toBeInTheDocument();
    expect(loadPrTickets).toHaveBeenCalledWith("s1", "acme/app", ["ABC-7", "ABC-8"], expect.any(String));
    expect(screen.getByText("Email is sent")).toBeInTheDocument();
    expect(screen.getByText(/could not be loaded: not found/)).toBeInTheDocument();
  });

  it("renders Jira wiki markup and loads attached images through the backend", async () => {
    const { container } = renderSection({ sourceName: "Acme Jira", sourceKind: "jira_cloud", keys: ["ABC-7"], message: null });
    expect((await screen.findByText("FFT:")).tagName).toBe("STRONG");
    const img = await screen.findByRole("img", { name: "flow.png" });
    expect(img).toHaveAttribute("src", "data:image/png;base64,AAAA");
    expect(loadTicketImage).toHaveBeenCalledWith("s1", "acme/app", "10001");
    // Images that are not attachments stay links and are never loaded.
    expect(container.querySelectorAll("img")).toHaveLength(1);
    expect(screen.getByRole("link", { name: "[image: pixel.png]" })).toBeInTheDocument();
  });

  it("waits for a click with an MCP source", async () => {
    renderSection({ sourceName: "Atlassian MCP", sourceKind: "mcp", keys: ["ABC-7"], message: null });
    expect(screen.getByText("ABC-7")).toBeInTheDocument();
    expect(loadPrTickets).not.toHaveBeenCalled();
    await userEvent.click(screen.getByRole("button", { name: "Load tickets" }));
    await waitFor(() => expect(loadPrTickets).toHaveBeenCalledTimes(1));
    expect(await screen.findByText("Reset password")).toBeInTheDocument();
  });

  it("cancels the backend load when the page is left", async () => {
    let requestId = "";
    loadPrTickets.mockImplementation((_s, _r, _k, id: string) => {
      requestId = id;
      return new Promise(() => {});
    });
    cancelTicketLoad.mockResolvedValue(true);
    const { unmount } = renderSection({ sourceName: "Atlassian MCP", sourceKind: "mcp", keys: ["ABC-7"], message: null });
    await userEvent.click(screen.getByRole("button", { name: "Load tickets" }));
    await waitFor(() => expect(loadPrTickets).toHaveBeenCalledTimes(1));
    unmount();
    await waitFor(() => expect(cancelTicketLoad).toHaveBeenCalledWith(requestId));
    expect(requestId).not.toBe("");
  });

  it("explains why nothing is loaded", () => {
    renderSection({
      sourceName: "Acme Jira",
      sourceKind: "jira_cloud",
      keys: [],
      message: "No ticket key for ABC found in the branch, title or description",
    });
    expect(screen.getByText(/No ticket key for ABC/)).toBeInTheDocument();
    expect(loadPrTickets).not.toHaveBeenCalled();
  });
});

describe("PrDetailPage", () => {
  it("decodes slashed project ids and renders the description", async () => {
    getPrDetail.mockResolvedValue({
      pr: {
        serverId: "s1",
        repo: "acme/app",
        projectId: "acme/app",
        number: 42,
        title: "Add login",
        description: "Implements **login**.",
        author: "dev",
        webUrl: "https://github.com/acme/app/pull/42",
        cloneUrl: null,
        headSha: "abc",
        baseSha: null,
        startSha: null,
        headRef: "feature/login",
        baseRef: "main",
        draft: false,
        state: "open",
        updatedAt: new Date().toISOString(),
        reviewRequested: true,
        authoredByMe: false,
      },
      tracked: null,
      reviews: [],
      tickets: null,
    });
    const router = createMemoryRouter([{ path: "/servers/:serverId/prs/:projectId/:number", element: <PrDetailPage /> }], {
      initialEntries: [prDetailPath("s1", "acme/app", 42)],
    });
    render(
      <QueryClientProvider client={new QueryClient()}>
        <RouterProvider router={router} />
      </QueryClientProvider>,
    );
    expect(await screen.findByText("login")).toBeInTheDocument();
    expect(getPrDetail).toHaveBeenCalledWith("s1", "acme/app", 42);
    expect(screen.queryByText(/Tickets/)).toBeNull();
    expect(screen.getByText("Not reviewed yet.")).toBeInTheDocument();
  });
});
