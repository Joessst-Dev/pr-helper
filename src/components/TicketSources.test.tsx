import { beforeEach, describe, expect, it, vi } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { TicketSourceForm } from "./TicketSources";
import { ticketSources } from "../test/fixtures";
import type { TicketSource } from "../lib/bindings/TicketSource";

const saveTicketSource = vi.fn();
vi.mock("../lib/api", () => ({
  api: { saveTicketSource: (...a: unknown[]) => saveTicketSource(...a) },
  errorMessage: (e: unknown) => String(e),
}));

function renderForm(source: TicketSource | undefined, onDone = vi.fn()) {
  render(
    <QueryClientProvider client={new QueryClient()}>
      <TicketSourceForm source={source} onDone={onDone} />
    </QueryClientProvider>,
  );
  return onDone;
}

describe("TicketSourceForm", () => {
  beforeEach(() => saveTicketSource.mockReset().mockResolvedValue(ticketSources[0]));

  it("keeps the stored token when only the name changes", async () => {
    const onDone = renderForm(ticketSources[0]);
    const name = screen.getByDisplayValue("Acme Jira");
    await userEvent.clear(name);
    await userEvent.type(name, "Jira");
    await userEvent.click(screen.getByRole("button", { name: "Verify and save" }));
    await waitFor(() => expect(onDone).toHaveBeenCalled());
    expect(saveTicketSource).toHaveBeenCalledWith("jira-1", expect.objectContaining({ name: "Jira", token: null }));
  });

  it("sends MCP settings without a token", async () => {
    renderForm(undefined);
    await userEvent.click(screen.getByRole("button", { name: "MCP server" }));
    await userEvent.type(screen.getByLabelText(/^Name/), "Atlassian MCP");
    await userEvent.type(screen.getByLabelText(/^MCP server name/), "atlassian");
    expect(screen.queryByLabelText(/token/i)).toBeNull();
    await userEvent.click(screen.getByRole("button", { name: "Save" }));
    await waitFor(() =>
      expect(saveTicketSource).toHaveBeenCalledWith(
        null,
        expect.objectContaining({ kind: "mcp", mcpServer: "atlassian", token: null, email: null }),
      ),
    );
  });
});
