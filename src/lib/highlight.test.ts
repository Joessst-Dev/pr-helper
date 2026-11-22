import { describe, expect, it } from "vitest";
import type { FileDiffLine } from "./bindings/FileDiffLine";
import { isHunkGap, languageFor, MAX_HIGHLIGHT_LINES, tokenize, tokenizeDiff } from "./highlight";

describe("languageFor", () => {
  it("maps extensions and well-known file names", () => {
    expect(languageFor("src/review/pipeline.rs")).toBe("rust");
    expect(languageFor("src/App.TSX")).toBe("tsx");
    expect(languageFor("deploy/Dockerfile")).toBe("docker");
    expect(languageFor("Makefile")).toBe("make");
    expect(languageFor(".github/workflows/ci.yml")).toBe("yaml");
    expect(languageFor("LICENSE")).toBeNull();
    expect(languageFor("data.unknownext")).toBeNull();
  });
});

describe("tokenize", () => {
  it("highlights with both theme colors as custom properties", async () => {
    const lines = await tokenize('fn main() {\n    let s = "hi";\n}\n', "main.rs");
    expect(lines).not.toBeNull();
    expect(lines!.length).toBe(4);
    expect(lines![1].map((t) => t.content).join("")).toBe('    let s = "hi";');
    const styled = lines!.flat().find((t) => t.content === "let");
    expect(styled?.style?.["--shiki-light"]).toMatch(/^#/);
    expect(styled?.style?.["--shiki-dark"]).toMatch(/^#/);
  });

  it("leaves unknown and huge files as plain text", async () => {
    expect(await tokenize("hello", "README")).toBeNull();
    expect(await tokenize("x\n".repeat(MAX_HIGHLIGHT_LINES + 1), "a.ts")).toBeNull();
  });
});

describe("tokenizeDiff", () => {
  const line = (kind: FileDiffLine["kind"], old: number | null, nw: number | null, text: string): FileDiffLine => ({
    kind,
    old,
    new: nw,
    text,
  });

  it("does not let a removed line's open comment color the added lines", async () => {
    const diff = [
      line("removed", 1, null, "/* legacy"),
      line("added", null, 1, "let x = 1;"),
      line("context", 2, 2, "let y = 2;"),
    ];
    const tokens = await tokenizeDiff(diff, "a.ts");
    const plain = await tokenize("let x = 1;", "a.ts");
    const comment = (await tokenize("/* legacy", "a.ts"))![0][0].style;
    expect(tokens![1]).toEqual(plain![0]);
    expect(tokens![2]!.some((t) => t.content === "let")).toBe(true);
    expect(tokens![2]!.find((t) => t.content === "let")?.style).not.toEqual(comment);
  });

  it("highlights each hunk separately", async () => {
    const diff = [line("added", null, 1, "const s = `open"), line("added", null, 40, "let z = 3;")];
    expect(isHunkGap(diff[0], diff[1])).toBe(true);
    const tokens = await tokenizeDiff(diff, "a.ts");
    expect(tokens![1]).toEqual((await tokenize("let z = 3;", "a.ts"))![0]);
  });

  it("stays plain text for unknown languages", async () => {
    expect(await tokenizeDiff([line("added", null, 1, "x")], "README")).toBeNull();
  });
});
