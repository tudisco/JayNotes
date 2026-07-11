import { describe, expect, it } from "vitest";
import { escapeHtml, preprocessNoteLinks, renderMarkdown } from "./markdown";

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

describe("renderMarkdown — GFM (tables, task lists, strikethrough)", () => {
  it("renders a GFM table with header and body cells", () => {
    const html = renderMarkdown(
      "| Name | Qty |\n| --- | --- |\n| Apples | 3 |",
    );
    expect(html).toContain("<table");
    expect(html).toContain("<th");
    expect(html).toContain("<td");
    expect(html).toContain("Apples");
  });

  it("renders a task list with checkbox inputs and list items", () => {
    const html = renderMarkdown("- [x] done\n- [ ] todo");
    expect(html).toContain("<li");
    expect(html).toMatch(/<input[^>]*type="checkbox"/);
    // Task-list checkboxes are display-only.
    expect(html).toMatch(/<input[^>]*disabled/);
  });

  it("renders strikethrough", () => {
    expect(renderMarkdown("~~gone~~")).toContain("<del>gone</del>");
  });

  it("keeps embedded HTML in a table cell escaped (no reopened hole)", () => {
    const html = renderMarkdown(
      "| Col |\n| --- |\n| <img src=x onerror=alert(1)> |",
    );
    expect(html).toContain("<table");
    expect(html).not.toMatch(/<img/i);
    expect(html).not.toMatch(/<[a-z][^>]*\son\w+=/i);
    expect(html).toContain("&lt;img");
  });

  it("keeps a <script> in a table cell escaped", () => {
    const html = renderMarkdown("| C |\n| --- |\n| <script>alert(1)</script> |");
    expect(html).not.toMatch(/<script/i);
    expect(html).toContain("&lt;script");
  });
});

describe("preprocessNoteLinks — wikilink rewriting", () => {
  it("rewrites a bare wikilink to a note link labeled by filename", () => {
    const html = renderMarkdown("See [[folder/My Note.md]] please");
    expect(html).toContain('data-note="folder/My Note.md"');
    expect(html).toContain('class="note-link"');
    expect(html).toContain(">My Note</a>");
  });

  it("handles paths with spaces and slashes intact", () => {
    const html = renderMarkdown("[[a b/c d/Deep Note.md]]");
    expect(html).toContain('data-note="a b/c d/Deep Note.md"');
    expect(html).toContain(">Deep Note</a>");
  });

  it("uses the alias form for the label", () => {
    const html = renderMarkdown("[[folder/Note.md|Custom Label]]");
    expect(html).toContain('data-note="folder/Note.md"');
    expect(html).toContain(">Custom Label</a>");
  });

  it("leaves wikilinks inside inline code untouched", () => {
    const out = preprocessNoteLinks("use `[[Note.md]]` literally");
    expect(out).toBe("use `[[Note.md]]` literally");
    const html = renderMarkdown("use `[[Note.md]]` literally");
    expect(html).not.toContain("data-note");
    expect(html).toContain("[[Note.md]]");
  });

  it("leaves wikilinks inside fenced code blocks untouched", () => {
    const src = "```\n[[Secret.md]]\n```";
    expect(preprocessNoteLinks(src)).toBe(src);
    expect(renderMarkdown(src)).not.toContain("data-note");
  });

  it("turns a relative .md markdown link into a note link", () => {
    const html = renderMarkdown("[read](notes/Todo.md)");
    expect(html).toContain('data-note="notes/Todo.md"');
    expect(html).toContain(">read</a>");
  });

  it("does not treat external links as note links", () => {
    const html = renderMarkdown("[site](https://example.com/x.md)");
    expect(html).not.toContain("data-note");
    expect(html).toContain('href="https://example.com/x.md"');
  });

  it("stays escaped for a crafted [[<img>]] payload (no XSS)", () => {
    const html = renderMarkdown("[[<img src=x onerror=alert(1)>]]");
    expect(html).not.toMatch(/<img/i);
    expect(html).not.toMatch(/<[a-z][^>]*\son\w+=/i);
    // The angle-bracketed payload survives only as escaped text.
    expect(html).toContain("&lt;img");
  });

  it("escapes a crafted alias so it cannot break out of the link", () => {
    const html = renderMarkdown("[[Note.md|<img src=x onerror=alert(1)>]]");
    expect(html).not.toMatch(/<img/i);
    expect(html).not.toMatch(/<[a-z][^>]*\son\w+=/i);
  });
});
