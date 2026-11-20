import { describe, expect, it, vi } from "vitest";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { openUrl } from "@tauri-apps/plugin-opener";
import { TicketChips } from "./TicketChips";

vi.mock("@tauri-apps/plugin-opener", () => ({ openUrl: vi.fn() }));

describe("TicketChips", () => {
  it("links tickets that have a URL", async () => {
    render(
      <TicketChips
        tickets={[
          { key: "ABC-1", url: "https://acme.atlassian.net/browse/ABC-1" },
          { key: "ABC-2", url: null },
        ]}
      />,
    );
    await userEvent.click(screen.getByRole("button", { name: /ABC-1/ }));
    expect(openUrl).toHaveBeenCalledWith("https://acme.atlassian.net/browse/ABC-1");
    expect(screen.getByText("ABC-2")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /ABC-2/ })).toBeNull();
  });

  it("renders nothing without tickets", () => {
    const { container } = render(<TicketChips tickets={[]} />);
    expect(container).toBeEmptyDOMElement();
  });
});
