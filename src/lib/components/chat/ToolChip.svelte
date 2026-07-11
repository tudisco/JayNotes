<!--
  ToolChip.svelte — one compact line of tool activity.

  Shows a category icon + what the assistant is doing (verb + argsSummary) while
  running, swapping to the backend's human result summary when it finishes.
  `detail` (if any) expands on click; an undoable write offers a Revert link.
-->
<script lang="ts">
  import type { ToolEntry } from "$lib/stores/chatReducer";
  import { revert } from "$lib/stores/chat";

  let { entry }: { entry: ToolEntry } = $props();

  type Category = "search" | "read" | "edit" | "move" | "delete" | "generic";

  const CATEGORY: Record<string, Category> = {
    search_notes: "search",
    notes_by_tag: "search",
    list_notes: "search",
    list_folders: "search",
    list_tags: "search",
    read_note: "read",
    note_links: "read",
    create_note: "edit",
    update_note: "edit",
    append_to_note: "edit",
    create_folder: "edit",
    rename_note: "move",
    move_note: "move",
    move_folder: "move",
    delete_note: "delete",
    delete_folder: "delete",
    batch_delete: "delete",
  };

  const VERB: Record<string, string> = {
    search_notes: "Searching",
    notes_by_tag: "Filtering by tag",
    list_notes: "Listing notes",
    list_folders: "Listing folders",
    list_tags: "Listing tags",
    read_note: "Reading",
    note_links: "Links for",
    create_note: "Creating",
    update_note: "Updating",
    append_to_note: "Appending to",
    rename_note: "Renaming",
    move_note: "Moving",
    move_folder: "Moving folder",
    create_folder: "Creating folder",
    delete_note: "Deleting",
    delete_folder: "Deleting folder",
    batch_delete: "Deleting",
  };

  let category = $derived<Category>(CATEGORY[entry.name] ?? "generic");
  let runningLabel = $derived(
    [VERB[entry.name] ?? "Working", entry.argsSummary].filter(Boolean).join(" "),
  );

  let expanded = $state(false);
  let reverting = $state(false);
  let revertedPath = $state<string | null>(null);
  let revertError = $state<string | null>(null);

  async function doRevert(): Promise<void> {
    if (!entry.revisionId || reverting) return;
    reverting = true;
    revertError = null;
    try {
      revertedPath = await revert(entry.revisionId);
    } catch (e) {
      revertError = String(e);
    } finally {
      reverting = false;
    }
  }
</script>

<div class="tool" class:running={entry.status === "running"}>
  <span class="icon" aria-hidden="true">
    {#if category === "search"}
      <svg viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="11" cy="11" r="7" /><path d="m21 21-4.3-4.3" /></svg>
    {:else if category === "read"}
      <svg viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M2 4h7a3 3 0 0 1 3 3v13a2.5 2.5 0 0 0-2.5-2.5H2z" /><path d="M22 4h-7a3 3 0 0 0-3 3v13a2.5 2.5 0 0 1 2.5-2.5H22z" /></svg>
    {:else if category === "edit"}
      <svg viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M12 20h9" /><path d="M16.5 3.5a2.1 2.1 0 0 1 3 3L7 19l-4 1 1-4z" /></svg>
    {:else if category === "move"}
      <svg viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M5 9l-3 3 3 3" /><path d="M9 5l3-3 3 3" /><path d="M15 19l-3 3-3-3" /><path d="M19 9l3 3-3 3" /><path d="M2 12h20" /><path d="M12 2v20" /></svg>
    {:else if category === "delete"}
      <svg viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M3 6h18" /><path d="M8 6V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2" /><path d="M6 6v14a2 2 0 0 0 2 2h8a2 2 0 0 0 2-2V6" /></svg>
    {:else}
      <svg viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="3" /></svg>
    {/if}
  </span>

  <span class="body">
    <span class="text">
      {#if entry.status === "running"}
        {runningLabel}
      {:else}
        {entry.summary ?? runningLabel}
      {/if}
    </span>

    <span class="meta">
      {#if entry.status === "running"}
        <span class="dots" aria-hidden="true"><span></span><span></span><span></span></span>
      {:else}
        {#if entry.detail}
          <button type="button" class="link" onclick={() => (expanded = !expanded)}>
            {expanded ? "hide" : "details"}
          </button>
        {/if}
        {#if entry.revisionId}
          {#if revertedPath}
            <span class="reverted">Reverted {revertedPath}</span>
          {:else}
            <button type="button" class="link" onclick={doRevert} disabled={reverting}>
              {reverting ? "reverting…" : "Revert"}
            </button>
          {/if}
        {/if}
      {/if}
    </span>

    {#if expanded && entry.detail}
      <pre class="detail">{entry.detail}</pre>
    {/if}
    {#if revertError}
      <span class="err">{revertError}</span>
    {/if}
  </span>
</div>

<style>
  .tool {
    display: flex;
    align-items: flex-start;
    gap: 7px;
    padding: 4px 8px;
    border-radius: 7px;
    color: var(--text-muted);
    font-size: 12px;
    line-height: 1.4;
  }

  .icon {
    flex-shrink: 0;
    display: flex;
    align-items: center;
    height: 17px;
    color: var(--text-muted);
  }

  .running .icon {
    color: var(--accent);
  }

  .body {
    min-width: 0;
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    gap: 4px 8px;
  }

  .text {
    color: var(--text);
    word-break: break-word;
  }

  .running .text {
    color: var(--text-muted);
  }

  .meta {
    display: inline-flex;
    align-items: center;
    gap: 8px;
  }

  .link {
    padding: 0;
    border: none;
    background: none;
    color: var(--accent);
    font-family: var(--font-ui);
    font-size: 11px;
    cursor: pointer;
  }
  .link:hover:not(:disabled) {
    text-decoration: underline;
  }
  .link:disabled {
    color: var(--text-muted);
    cursor: default;
  }

  .reverted {
    color: var(--text-muted);
    font-size: 11px;
  }

  .err {
    flex-basis: 100%;
    color: var(--danger);
    font-size: 11px;
  }

  .detail {
    flex-basis: 100%;
    margin: 4px 0 2px;
    padding: 8px;
    border-radius: 6px;
    background-color: var(--code-bg);
    color: var(--text);
    font-family: var(--font-mono);
    font-size: 11px;
    white-space: pre-wrap;
    word-break: break-word;
  }

  /* Streaming ellipsis. */
  .dots {
    display: inline-flex;
    gap: 3px;
  }
  .dots span {
    width: 4px;
    height: 4px;
    border-radius: 50%;
    background-color: var(--text-muted);
    animation: pulse 1.2s ease-in-out infinite;
  }
  .dots span:nth-child(2) {
    animation-delay: 0.2s;
  }
  .dots span:nth-child(3) {
    animation-delay: 0.4s;
  }

  @keyframes pulse {
    0%,
    100% {
      opacity: 0.25;
    }
    50% {
      opacity: 1;
    }
  }

  @media (prefers-reduced-motion: reduce) {
    .dots span {
      animation: none;
      opacity: 0.6;
    }
  }
</style>
