import { beforeEach, describe, expect, it, vi } from "vitest";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import type { CriticalFile } from "../lib/bindings/CriticalFile";
import type { CriticalFileContent } from "../lib/bindings/CriticalFileContent";
import { CriticalFiles } from "./CriticalFiles";

const getReviewFile = vi.fn();
vi.mock("../lib/api", () => ({
  api: { getReviewFile: (...a: unknown[]) => getReviewFile(...a) },
  errorMessage: (e: unknown) => String(e),
}));
// Highlighting is covered by shiki itself; here every line gets one token, so we can check
// that tokens are rendered as plain spans.
vi.mock("../lib/highlight", () => ({
  tokenize: async (code: string) =>
    code.split("\n").map((line) => [{ content: line, style: { "--shiki-light": "#111" } }]),
}));

const files: CriticalFile[] = [
  {
    path: "src/auth.rs",
    reason: "The token check moved into the middleware.",
    focusLines: [{ start: 2, end: 3 }],
    binary: false,
    truncated: false,
  },
  { path: "assets/logo.png", reason: "Replaced logo.", focusLines: [], binary: true, truncated: false },
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
    getReviewFile.mockReset().mockImplementation(async (_r: string, path: string) =>
      path === "src/auth.rs"
        ? authContent
        : { content: null, diff: [{ kind: "added", old: null, new: 1, text: "Binary files differ" }], diffTruncated: false },
    );
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

  it("shows only the diff for binary files", async () => {
    renderFiles();
    await userEvent.click(screen.getByRole("button", { name: /assets\/logo\.png/ }));
    expect(await screen.findByText(/Binary file, only the diff is shown/)).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "File" })).toBeNull();
    expect(screen.getByText("Binary files differ")).toBeInTheDocument();
  });
});
