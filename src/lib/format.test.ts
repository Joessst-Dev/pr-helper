import { describe, expect, it } from "vitest";
import { commentTitle, formatCost, location, relativeTime, severityRank } from "./format";

describe("format", () => {
  it("formats relative times", () => {
    const now = new Date("2026-01-10T12:00:00Z");
    expect(relativeTime("2026-01-10T11:59:50Z", now)).toBe("just now");
    expect(relativeTime("2026-01-10T11:30:00Z", now)).toBe("30m ago");
    expect(relativeTime("2026-01-10T07:00:00Z", now)).toBe("5h ago");
    expect(relativeTime("2026-01-07T12:00:00Z", now)).toBe("3d ago");
    expect(relativeTime(null, now)).toBe("");
  });

  it("formats locations", () => {
    expect(location(null, null, null)).toBe("PR-wide");
    expect(location("a.rs", null, null)).toBe("a.rs");
    expect(location("a.rs", 3, null)).toBe("a.rs:3");
    expect(location("a.rs", 3, 7)).toBe("a.rs:3-7");
  });

  it("ranks severities critical first", () => {
    expect(severityRank("critical")).toBeLessThan(severityRank("nit"));
  });

  it("formats cost", () => {
    expect(formatCost(0.004)).toBe("<$0.01");
    expect(formatCost(1.234)).toBe("$1.23");
    expect(formatCost(null)).toBe("");
  });

  it("titles a comment by its first non-empty line, shortened like the backend", () => {
    expect(commentTitle("\n  First line \nmore")).toBe("First line");
    expect([...commentTitle("x".repeat(200))]).toHaveLength(80);
    expect(commentTitle("   ")).toBe("");
  });
});
