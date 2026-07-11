import { describe, expect, it } from "vitest";
import { fuzzyScore } from "./fuzzy";

describe("fuzzyScore", () => {
  it("matches an empty query with score 0 and no positions", () => {
    expect(fuzzyScore("", "anything")).toEqual({ score: 0, positions: [] });
  });

  it("returns null when the query is not a subsequence", () => {
    expect(fuzzyScore("xyz", "meeting notes")).toBeNull();
    expect(fuzzyScore("gnm", "meeting")).toBeNull(); // right chars, wrong order
  });

  it("is case-insensitive and records matched positions", () => {
    const r = fuzzyScore("MT", "meeting");
    expect(r).not.toBeNull();
    expect(r!.positions).toEqual([0, 3]); // m…ee_t
  });

  it("scores a word-start / consecutive match above a scattered one", () => {
    // "meet" as a consecutive run at a word start...
    const prefix = fuzzyScore("meet", "meeting notes")!;
    // ...beats the same letters buried mid-word with no boundaries or runs.
    const scattered = fuzzyScore("meet", "xmxexextx")!;
    expect(prefix).not.toBeNull();
    expect(scattered).not.toBeNull();
    expect(prefix.score).toBeGreaterThan(scattered.score);
  });

  it("rewards matches at a path segment boundary", () => {
    // "note" as a whole word-start segment beats the same letters mid-word.
    const boundary = fuzzyScore("note", "projects/note")!;
    const midword = fuzzyScore("note", "annotenoise")!;
    expect(boundary.score).toBeGreaterThan(midword.score);
  });

  it("prefers an earlier match position on otherwise equal input", () => {
    const early = fuzzyScore("a", "abc")!;
    const late = fuzzyScore("a", "xxxa")!;
    expect(early.score).toBeGreaterThan(late.score);
  });

  it("prefers the shorter target when matches are otherwise equal", () => {
    const short = fuzzyScore("ab", "ab")!;
    const long = fuzzyScore("ab", "ab" + "x".repeat(50))!;
    expect(short.score).toBeGreaterThan(long.score);
  });
});
