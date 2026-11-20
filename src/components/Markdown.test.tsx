import { describe, expect, it, vi } from "vitest";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { openUrl } from "@tauri-apps/plugin-opener";
import { Markdown, safeHref } from "./Markdown";

vi.mock("@tauri-apps/plugin-opener", () => ({ openUrl: vi.fn() }));

describe("Markdown", () => {
  it("renders gfm markdown", () => {
    render(<Markdown>{"## Changes\n\n- [x] done\n- **bold** item\n\n| a | b |\n|---|---|\n| 1 | 2 |"}</Markdown>);
    expect(screen.getByRole("heading", { name: "Changes" })).toBeInTheDocument();
    expect(screen.getByText("bold").tagName).toBe("STRONG");
    expect(screen.getByRole("table")).toBeInTheDocument();
  });

  it("drops raw html and never loads images", () => {
    const { container } = render(
      <Markdown>{'<script>alert(1)</script>\n\n<img src="https://t.example/p.gif">\n\n![logo](https://t.example/logo.png)'}</Markdown>,
    );
    expect(container.querySelector("script")).toBeNull();
    expect(container.querySelector("img")).toBeNull();
    expect(screen.getByRole("link", { name: "[image: logo]" })).toBeInTheDocument();
  });

  it("opens web links in the system browser and neutralizes other schemes", async () => {
    render(<Markdown>{"[docs](https://example.com/docs) and [bad](javascript:alert(1))"}</Markdown>);
    await userEvent.click(screen.getByRole("link", { name: "docs" }));
    expect(openUrl).toHaveBeenCalledWith("https://example.com/docs");
    expect(screen.queryByRole("link", { name: "bad" })).toBeNull();
    expect(screen.getByText("bad")).toBeInTheDocument();
  });

  it("only allows http and https", () => {
    expect(safeHref("https://a.example/x")).toBe("https://a.example/x");
    expect(safeHref("file:///etc/passwd")).toBeNull();
    expect(safeHref("relative/path")).toBeNull();
    expect(safeHref(undefined)).toBeNull();
  });
});
