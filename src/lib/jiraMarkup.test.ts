import { describe, expect, it } from "vitest";
import { jiraToMarkdown } from "./jiraMarkup";

describe("jiraToMarkdown", () => {
  it("converts emphasis", () => {
    expect(jiraToMarkdown("*FFT:* the flag")).toBe("**FFT:** the flag");
    expect(jiraToMarkdown("a *bold* and _italic_ and -gone- and +under+ and {{a*b*c}}")).toBe(
      "a **bold** and _italic_ and ~~gone~~ and under and `a*b*c`",
    );
    expect(jiraToMarkdown("??Someone??")).toBe("*Someone*");
  });

  it("leaves arithmetic, dates, hyphens and URLs alone", () => {
    expect(jiraToMarkdown("5 * 3 = 15, 2026-01-01, re-use, a - b")).toBe("5 * 3 = 15, 2026-01-01, re-use, a - b");
    expect(jiraToMarkdown("see https://x.example/a-b-c/*x*_y_")).toBe("see https://x.example/a-b-c/*x*_y_");
  });

  it("converts headings, lists and rules", () => {
    expect(jiraToMarkdown("h2. Goal\n* one\n** nested\n# first\n## sub\n----")).toBe(
      "## Goal\n- one\n  - nested\n1. first\n   1. sub\n\n---",
    );
  });

  it("keeps single line breaks", () => {
    expect(jiraToMarkdown("line one\nline two\n\nnew paragraph")).toBe("line one  \nline two\n\nnew paragraph");
  });

  it("converts code and noformat blocks verbatim", () => {
    expect(jiraToMarkdown("Before\n{code:java}\nint *a* = 1;\n{code}\nAfter")).toBe(
      "Before\n\n~~~~java\nint *a* = 1;\n~~~~\nAfter",
    );
    expect(jiraToMarkdown("{noformat}raw *x*{noformat}")).toBe("~~~~\nraw *x*\n~~~~");
    expect(jiraToMarkdown("{code:title=A.kt|language=kotlin}\nval x = 1\n{code}")).toBe("~~~~kotlin\nval x = 1\n~~~~");
    // An unclosed block is closed at the end.
    expect(jiraToMarkdown("{code}\nx")).toBe("~~~~\nx\n~~~~");
  });

  it("converts links, mentions and images", () => {
    expect(jiraToMarkdown("[Docs|https://d.example/x] and [https://e.example] and [~jdoe] and [ABC-1]")).toBe(
      "[Docs](<https://d.example/x>) and <https://e.example> and @jdoe and [ABC-1]",
    );
    expect(jiraToMarkdown("!screen shot.png|thumbnail! and !https://i.example/a.gif!")).toBe(
      "![screen shot.png](<screen shot.png>) and ![a.gif](<https://i.example/a.gif>)",
    );
    expect(jiraToMarkdown("Great! Thanks!")).toBe("Great! Thanks!");
  });

  it("converts quotes and strips layout macros", () => {
    expect(jiraToMarkdown("{quote}quoted *text*{quote}\nafter")).toBe("> quoted **text**\n\nafter");
    expect(jiraToMarkdown("bq. short quote")).toBe("> short quote");
    expect(jiraToMarkdown("{color:red}red{color} {panel:title=X}in panel{panel}")).toBe("red in panel");
  });

  it("converts tables, adding a header row when there is none", () => {
    expect(jiraToMarkdown("||Name||Link||\n|a|[x|https://x.example]|")).toBe(
      "| Name | Link |\n| --- | --- |\n| a | [x](<https://x.example>) |\n",
    );
    expect(jiraToMarkdown("Intro\n|a|b|")).toBe("Intro\n\n|  |  |\n| --- | --- |\n| a | b |\n");
  });

  it("keeps html-like text and tildes literal", () => {
    expect(jiraToMarkdown("<script>x</script> ~sub~")).toBe("\\<script>x\\</script> \\~sub\\~");
    expect(jiraToMarkdown("> not a quote")).toBe("\\> not a quote");
  });
});
