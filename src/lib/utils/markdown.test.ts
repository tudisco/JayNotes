import { describe, expect, it } from "vitest";
import { escapeHtml, renderMarkdown } from "./markdown";

describe("escapeHtml", () => {
  it("escapes all five significant characters", () => {
    expect(escapeHtml(`<a href="x">'&'</a>`)).toBe(
      "&lt;a href=&quot;x&quot;&gt;&#39;&amp;&#39;&lt;/a&gt;",
    );
  });
});

describe("renderMarkdown — basic formatting", () => {
  it("renders markdown to HTML tags", () => {
    const html = renderMarkdown("# Title\n\nSome **bold** and `code`.");
    expect(html).toContain("<h1");
    expect(html).toContain("<strong>bold</strong>");
    expect(html).toContain("<code>code</code>");
  });

  it("renders fenced code blocks", () => {
    const html = renderMarkdown("```js\nconst x = 1;\n```");
    expect(html).toContain("<pre>");
    expect(html).toContain("const x = 1;");
  });

  it("keeps safe links but opens them externally", () => {
    const html = renderMarkdown("[docs](https://example.com)");
    expect(html).toContain('href="https://example.com"');
    expect(html).toContain('rel="noopener noreferrer nofollow"');
  });
});

describe("renderMarkdown — safety (raw HTML never survives)", () => {
  const attacks = [
    `<script>alert(1)</script>`,
    `<img src=x onerror="alert(1)">`,
    `<iframe src="javascript:alert(1)"></iframe>`,
    `<div onclick="steal()">hi</div>`,
    `<svg/onload=alert(1)>`,
    `Hello <b>world</b> <a href="javascript:alert(1)">x</a>`,
  ];

  for (const src of attacks) {
    it(`neutralizes: ${src.slice(0, 32)}`, () => {
      const html = renderMarkdown(src);
      // No live tags from the source may appear (they must be escaped to text).
      expect(html).not.toMatch(/<script/i);
      expect(html).not.toMatch(/<iframe/i);
      expect(html).not.toMatch(/<svg/i);
      // No live element may carry an inline event handler (escaped text is fine).
      expect(html).not.toMatch(/<[a-z][^>]*\son\w+=/i);
    });
  }

  it("strips javascript: URLs from markdown links", () => {
    const html = renderMarkdown("[click me](javascript:alert(document.cookie))");
    expect(html).not.toMatch(/javascript:/i);
    expect(html).toContain('href="#"');
  });

  it("strips javascript: URLs from markdown images", () => {
    const html = renderMarkdown("![x](javascript:alert(1))");
    expect(html).not.toMatch(/javascript:/i);
  });

  it("strips data: URLs", () => {
    const html = renderMarkdown("[x](data:text/html,<script>alert(1)</script>)");
    expect(html).not.toMatch(/data:text\/html/i);
    expect(html).not.toMatch(/<script/i);
  });

  it("escapes raw HTML into visible text, preserving the words", () => {
    const html = renderMarkdown("A <b>bold claim</b> here");
    expect(html).toContain("&lt;b&gt;");
    expect(html).toContain("bold claim");
    expect(html).not.toMatch(/<b>/);
  });
});
