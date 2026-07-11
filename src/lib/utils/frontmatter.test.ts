import { describe, expect, it } from "vitest";
import { joinFrontmatter, splitFrontmatter } from "./frontmatter";

describe("splitFrontmatter", () => {
  it("returns null frontmatter when there is none", () => {
    const raw = "# Hello\n\nJust a body.\n";
    expect(splitFrontmatter(raw)).toEqual({ frontmatter: null, body: raw });
  });

  it("splits a standard frontmatter block from the body", () => {
    const raw = "---\ntitle: Note\ntags: [a, b]\n---\n# Body\n";
    const { frontmatter, body } = splitFrontmatter(raw);
    expect(frontmatter).toBe("---\ntitle: Note\ntags: [a, b]\n---\n");
    expect(body).toBe("# Body\n");
  });

  it("handles an empty body after the closing fence", () => {
    const raw = "---\ntitle: Note\n---\n";
    const { frontmatter, body } = splitFrontmatter(raw);
    expect(frontmatter).toBe("---\ntitle: Note\n---\n");
    expect(body).toBe("");
  });

  it("handles a closing fence at EOF with no trailing newline", () => {
    const raw = "---\ntitle: Note\n---";
    const { frontmatter, body } = splitFrontmatter(raw);
    expect(frontmatter).toBe("---\ntitle: Note\n---");
    expect(body).toBe("");
  });

  it("handles an empty frontmatter block", () => {
    const raw = "---\n---\nbody\n";
    const { frontmatter, body } = splitFrontmatter(raw);
    expect(frontmatter).toBe("---\n---\n");
    expect(body).toBe("body\n");
  });

  it("does not treat a `---` later in the body as frontmatter", () => {
    const raw = "# Title\n\nsome text\n\n---\n\nmore text\n";
    expect(splitFrontmatter(raw)).toEqual({ frontmatter: null, body: raw });
  });

  it("only captures the first frontmatter block, leaving later `---` in body", () => {
    const raw = "---\ntitle: Note\n---\nbody\n---\nfooter\n";
    const { frontmatter, body } = splitFrontmatter(raw);
    expect(frontmatter).toBe("---\ntitle: Note\n---\n");
    expect(body).toBe("body\n---\nfooter\n");
  });

  it("treats an unterminated opening fence as body (no frontmatter)", () => {
    const raw = "---\ntitle: Note\nno closing fence here\n";
    expect(splitFrontmatter(raw)).toEqual({ frontmatter: null, body: raw });
  });

  it("does not treat an inline `foo---` as a closing fence", () => {
    const raw = "---\ntitle: Note\nfoo---bar\n";
    expect(splitFrontmatter(raw)).toEqual({ frontmatter: null, body: raw });
  });

  it("tolerates CRLF newlines", () => {
    const raw = "---\r\ntitle: Note\r\n---\r\nbody\r\n";
    const { frontmatter, body } = splitFrontmatter(raw);
    expect(frontmatter).toBe("---\r\ntitle: Note\r\n---\r\n");
    expect(body).toBe("body\r\n");
  });
});

describe("joinFrontmatter", () => {
  it("reassembles to the original string byte-for-byte", () => {
    const cases = [
      "# no frontmatter\n",
      "---\ntitle: Note\n---\n# Body\n",
      "---\ntitle: Note\n---\n",
      "---\ntitle: Note\n---",
      "---\n---\nbody\n",
      "---\ntitle: Note\n---\nbody\n---\nfooter\n",
      "---\r\ntitle: Note\r\n---\r\nbody\r\n",
      "",
    ];
    for (const raw of cases) {
      const { frontmatter, body } = splitFrontmatter(raw);
      expect(joinFrontmatter(frontmatter, body)).toBe(raw);
    }
  });

  it("passes the body through unchanged when frontmatter is null", () => {
    expect(joinFrontmatter(null, "just body")).toBe("just body");
  });
});
