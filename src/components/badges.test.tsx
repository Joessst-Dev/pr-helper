import { describe, expect, it } from "vitest";
import { render, screen } from "@testing-library/react";
import { InReviewBadge, PrReviewBadges } from "./badges";
import { pr } from "../test/fixtures";

describe("InReviewBadge", () => {
  it("is hidden when only the user reviews", () => {
    const { container } = render(<InReviewBadge reviewers={[{ login: "me", state: "requested", isMe: true }]} />);
    expect(container).toBeEmptyDOMElement();
  });

  it("names two other reviewers and counts the rest", () => {
    render(
      <InReviewBadge
        reviewers={[
          { login: "me", state: "requested", isMe: true },
          { login: "alice", state: "approved", isMe: false },
          { login: "bob", state: "requested", isMe: false },
          { login: "carol", state: "commented", isMe: false },
        ]}
      />,
    );
    const badge = screen.getByText(/In review: alice, bob/);
    expect(badge).toHaveTextContent("In review: alice, bob +1");
    expect(badge).toHaveAttribute("title", "alice: Approved\nbob: Requested\ncarol: Commented");
  });
});

describe("PrReviewBadges", () => {
  it("shows approval and changes requested by others", () => {
    render(
      <PrReviewBadges
        pr={pr({
          approved: true,
          reviewers: [
            { login: "me", state: "changes_requested", isMe: true },
            { login: "bob", state: "changes_requested", isMe: false },
          ],
        })}
      />,
    );
    expect(screen.getByText("Approved")).toBeInTheDocument();
    expect(screen.getByText("Changes requested")).toBeInTheDocument();
  });

  it("ignores the user's own request for changes", () => {
    render(<PrReviewBadges pr={pr({ reviewers: [{ login: "me", state: "changes_requested", isMe: true }] })} />);
    expect(screen.queryByText("Changes requested")).toBeNull();
    expect(screen.queryByText("Approved")).toBeNull();
    expect(screen.queryByText(/In review/)).toBeNull();
  });
});
