import { describe, expect, it } from "vitest";
import { languageFor, MAX_HIGHLIGHT_LINES, tokenize } from "./highlight";

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
