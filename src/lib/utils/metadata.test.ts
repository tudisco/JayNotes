import { describe, expect, it } from "vitest";
import {
  extractInlineTags,
  parseFrontmatter,
  serializeFrontmatter,
} from "./metadata";

describe("parseFrontmatter", () => {
  it("returns empty results for null input", () => {
    expect(parseFrontmatter(null)).toEqual({
      tags: [],
      fields: {},
      parseError: false,
    });
  });

  it("returns empty results for an empty block", () => {
    expect(parseFrontmatter("---\n---\n")).toEqual({
      tags: [],
      fields: {},
      parseError: false,
    });
  });

  it("parses a flow tag array", () => {
    const { tags, fields, parseError } = parseFrontmatter(
      "---\ntags: [a, b]\n---\n",
    );
    expect(tags).toEqual(["a", "b"]);
    expect(fields).toEqual({});
    expect(parseError).toBe(false);
  });

  it("parses a block (YAML list) of tags", () => {
    const { tags } = parseFrontmatter("---\ntags:\n  - a\n  - b\n---\n");
    expect(tags).toEqual(["a", "b"]);
  });

  it("normalizes a single-string tags value", () => {
    expect(parseFrontmatter("---\ntags: solo\n---\n").tags).toEqual(["solo"]);
    expect(parseFrontmatter("---\ntags: a b c\n---\n").tags).toEqual([
      "a",
      "b",
      "c",
    ]);
  });

  it("trims, strips leading #, and dedupes tags", () => {
    const { tags } = parseFrontmatter("---\ntags: ['#a', 'a', ' b ']\n---\n");
    expect(tags).toEqual(["a", "b"]);
  });

  it("keeps non-tag keys in fields, preserving order and type", () => {
    const { tags, fields } = parseFrontmatter(
      "---\ntitle: Note\ntags: [x]\ncount: 3\ndone: true\n---\n",
    );
    expect(tags).toEqual(["x"]);
    expect(Object.keys(fields)).toEqual(["title", "count", "done"]);
    expect(fields).toEqual({ title: "Note", count: 3, done: true });
  });

  it("flags malformed YAML as a parse error without throwing", () => {
    const { parseError, tags, fields } = parseFrontmatter(
      "---\ntitle: : bad\n  weird: indent\n\t- tab\n---\n",
    );
    expect(parseError).toBe(true);
    expect(tags).toEqual([]);
    expect(fields).toEqual({});
  });
});

describe("serializeFrontmatter", () => {
  it("returns null when there are no tags and no fields", () => {
    expect(serializeFrontmatter([], {})).toBeNull();
  });

  it("serializes tags first as a flow sequence, then fields", () => {
    const out = serializeFrontmatter(["a", "b"], { title: "Note", count: 3 });
    expect(out).toBe("---\ntags: [a, b]\ntitle: Note\ncount: 3\n---\n");
  });

  it("omits the tags key when there are none", () => {
    expect(serializeFrontmatter([], { title: "Note" })).toBe(
      "---\ntitle: Note\n---\n",
    );
  });

  it("emits only tags when there are no fields", () => {
    expect(serializeFrontmatter(["a"], {})).toBe("---\ntags: [a]\n---\n");
  });

  it("preserves non-string field types round-trip", () => {
    const block = serializeFrontmatter(["t"], {
      count: 3,
      done: true,
      nested: { a: 1 },
    });
    const parsed = parseFrontmatter(block);
    expect(parsed.tags).toEqual(["t"]);
    expect(parsed.fields).toEqual({ count: 3, done: true, nested: { a: 1 } });
  });

  it("round-trips parse -> serialize -> parse", () => {
    const raw = "---\ntags: [a, b]\ntitle: Hello\ncount: 42\n---\n";
    const p1 = parseFrontmatter(raw);
    const s = serializeFrontmatter(p1.tags, p1.fields);
    expect(s).toBe(raw);
  });
});

describe("extractInlineTags", () => {
  it("finds simple hashtags in order, deduped", () => {
    expect(extractInlineTags("a #foo b #bar c #foo")).toEqual(["foo", "bar"]);
  });

  it("supports -, _, / and digits after the first letter", () => {
    expect(extractInlineTags("#a-b #c_d #e/f #g2")).toEqual([
      "a-b",
      "c_d",
      "e/f",
      "g2",
    ]);
  });

  it("ignores a # that follows a word character (foo#bar)", () => {
    expect(extractInlineTags("foo#bar and http://x#frag")).toEqual([]);
  });

  it("ignores tags that do not start with a letter (#123)", () => {
    expect(extractInlineTags("#123 #_x #-y")).toEqual([]);
  });

  it("ignores tags inside a fenced code block", () => {
    const body = "before #real\n```\n#nope\ncode #alsonope\n```\nafter #also";
    expect(extractInlineTags(body)).toEqual(["real", "also"]);
  });

  it("ignores tags inside ~~~ fences", () => {
    const body = "~~~\n#nope\n~~~\n#yes";
    expect(extractInlineTags(body)).toEqual(["yes"]);
  });

  it("ignores tags inside inline code spans", () => {
    expect(extractInlineTags("text `#nope` and #yes")).toEqual(["yes"]);
  });

  it("returns an empty array when there are no tags", () => {
    expect(extractInlineTags("just plain text")).toEqual([]);
  });
});
