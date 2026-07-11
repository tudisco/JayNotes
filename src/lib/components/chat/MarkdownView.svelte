<!--
  MarkdownView.svelte — renders assistant markdown safely.

  The HTML comes from `renderMarkdown`, which neutralizes raw HTML and unsafe
  URL schemes (see utils/markdown.ts), so `{@html}` here is safe by construction.
  After each render a small copy button is grafted onto every code block.
-->
<script lang="ts">
  import { renderMarkdown } from "$lib/utils/markdown";
  import { openNoteLink } from "$lib/stores/chat";

  let { source }: { source: string } = $props();

  let container = $state<HTMLDivElement>();
  let html = $derived(renderMarkdown(source));

  // Delegated handler: clicking an internal note link opens the note.
  function onClick(event: MouseEvent): void {
    const target = event.target as HTMLElement | null;
    const anchor = target?.closest?.("a[data-note]") as HTMLElement | null;
    if (!anchor) return;
    event.preventDefault();
    const path = anchor.getAttribute("data-note");
    if (path) void openNoteLink(path);
  }

  // Enhance code blocks with a copy button after every (re)render.
  $effect(() => {
    html; // track
    const root = container;
    if (!root) return;
    for (const pre of Array.from(root.querySelectorAll("pre"))) {
      if (pre.querySelector(".code-copy")) continue;
      const btn = document.createElement("button");
      btn.type = "button";
      btn.className = "code-copy";
      btn.textContent = "Copy";
      btn.addEventListener("click", () => {
        const code = pre.querySelector("code")?.textContent ?? pre.textContent ?? "";
        void navigator.clipboard.writeText(code).then(() => {
          btn.textContent = "Copied";
          setTimeout(() => (btn.textContent = "Copy"), 1200);
        });
      });
      pre.appendChild(btn);
    }
  });
</script>

<!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
<div class="md" bind:this={container} onclick={onClick}>{@html html}</div>

<style>
  .md {
    font-size: 13px;
    line-height: 1.55;
    color: var(--text);
    word-break: break-word;
    overflow-wrap: anywhere;
  }

  /* Tight vertical rhythm suited to a narrow panel. */
  .md :global(> :first-child) {
    margin-top: 0;
  }
  .md :global(> :last-child) {
    margin-bottom: 0;
  }
  .md :global(p),
  .md :global(ul),
  .md :global(ol),
  .md :global(blockquote),
  .md :global(pre),
  .md :global(table) {
    margin: 0 0 8px;
  }
  .md :global(h1),
  .md :global(h2),
  .md :global(h3),
  .md :global(h4) {
    margin: 14px 0 6px;
    font-weight: 600;
    line-height: 1.3;
  }
  /* Kept modest — headings shouldn't shout in a narrow chat column. */
  .md :global(h1) {
    font-size: 1.15em;
  }
  .md :global(h2) {
    font-size: 1.08em;
  }
  .md :global(h3),
  .md :global(h4) {
    font-size: 1em;
  }
  .md :global(ul),
  .md :global(ol) {
    padding-left: 20px;
  }
  .md :global(li) {
    margin: 2px 0;
  }
  /* GFM task lists: drop the bullet, keep the (disabled) checkbox aligned. */
  .md :global(li:has(> input[type="checkbox"])) {
    list-style: none;
    margin-left: -16px;
  }
  .md :global(li > input[type="checkbox"]) {
    margin-right: 6px;
    vertical-align: middle;
  }
  .md :global(del) {
    color: var(--text-muted);
  }
  .md :global(a) {
    color: var(--accent);
    text-decoration: none;
  }
  .md :global(a:hover) {
    text-decoration: underline;
  }
  /* Internal note links (from [[wikilinks]] and relative .md links). */
  .md :global(a.note-link) {
    color: var(--accent);
    text-decoration: none;
    border-bottom: 1px dotted color-mix(in srgb, var(--accent) 55%, transparent);
    cursor: pointer;
  }
  .md :global(a.note-link:hover) {
    border-bottom-style: solid;
    text-decoration: none;
  }
  .md :global(code) {
    padding: 1px 4px;
    border-radius: 3px;
    background-color: var(--code-bg);
    font-family: var(--font-mono);
    font-size: 12px;
  }
  .md :global(pre) {
    position: relative;
    padding: 10px 12px;
    border: 1px solid var(--border);
    border-radius: 8px;
    background-color: var(--code-bg);
    overflow-x: auto;
  }
  .md :global(pre code) {
    padding: 0;
    background: transparent;
    font-size: 12px;
    line-height: 1.5;
  }
  .md :global(blockquote) {
    padding-left: 10px;
    border-left: 2px solid var(--border);
    color: var(--text-muted);
  }
  .md :global(img) {
    max-width: 100%;
    border-radius: 6px;
  }
  /* Tables scroll horizontally rather than forcing the panel to widen. */
  .md :global(table) {
    display: block;
    width: max-content;
    max-width: 100%;
    overflow-x: auto;
    border-collapse: collapse;
    font-size: 12px;
  }
  .md :global(th),
  .md :global(td) {
    padding: 4px 8px;
    border: 1px solid var(--border);
    text-align: left;
  }
  .md :global(thead th) {
    background-color: var(--hover);
    font-weight: 600;
  }
  .md :global(hr) {
    border: none;
    border-top: 1px solid var(--border);
    margin: 10px 0;
  }

  /* Copy button, injected per code block. */
  .md :global(.code-copy) {
    position: absolute;
    top: 6px;
    right: 6px;
    padding: 2px 8px;
    border: 1px solid var(--border);
    border-radius: 5px;
    background-color: var(--bg-panel);
    color: var(--text-muted);
    font-family: var(--font-ui);
    font-size: 11px;
    cursor: pointer;
    opacity: 0;
    transition: opacity 0.15s ease;
  }
  .md :global(pre:hover .code-copy) {
    opacity: 1;
  }
  .md :global(.code-copy:hover) {
    color: var(--accent);
    border-color: var(--accent);
  }
</style>
