<script lang="ts">
  import Editor from "./Editor.svelte";
  import PropertiesBar from "./PropertiesBar.svelte";
  import { renameNote, selected, vaultError } from "$lib/stores/vault";

  let fileSelected = $derived($selected !== null && !$selected.isDir);
  let notePath = $derived(fileSelected ? ($selected as { path: string }).path : null);

  // Single shared source of truth for the current note's verbatim frontmatter.
  // Editor loads it (and owns the body); PropertiesBar edits it. Both persist
  // through the same Editor writer so their saves can't clobber each other.
  let editor = $state<Editor | undefined>();
  let frontmatter = $state<string | null>(null);

  function onPropertiesChange(fm: string | null): void {
    frontmatter = fm;
    editor?.requestSave();
  }

  function baseName(path: string): string {
    return (path.split("/").pop() ?? "").replace(/\.md$/i, "");
  }

  // Editable title draft, resynced whenever the selected note changes.
  let titleDraft = $state("");
  $effect(() => {
    const p = notePath;
    titleDraft = p ? baseName(p) : "";
  });

  async function commitTitle(): Promise<void> {
    const p = notePath;
    if (!p) return;
    const current = baseName(p);
    const next = titleDraft.trim();
    if (!next || next === current) {
      titleDraft = current; // revert empties / no-ops
      return;
    }
    try {
      await renameNote(p, next);
    } catch (e) {
      vaultError.set(String(e));
      titleDraft = current;
    }
  }

  function onTitleKey(event: KeyboardEvent): void {
    const input = event.currentTarget as HTMLInputElement;
    if (event.key === "Enter") {
      event.preventDefault();
      input.blur(); // triggers commit via onblur
    } else if (event.key === "Escape") {
      event.preventDefault();
      titleDraft = notePath ? baseName(notePath) : "";
      input.blur();
    }
  }
</script>

<section class="editor-pane" class:has-note={fileSelected}>
  {#if fileSelected && notePath}
    <div class="note-view">
      {#key notePath}
        <div class="note-meta">
          <header class="note-header">
            <input
              class="note-title"
              type="text"
              bind:value={titleDraft}
              spellcheck="false"
              aria-label="Note title"
              onkeydown={onTitleKey}
              onblur={commitTitle}
            />
          </header>
          <PropertiesBar {frontmatter} onChange={onPropertiesChange} />
        </div>
        <Editor bind:this={editor} path={notePath} bind:frontmatter />
      {/key}
    </div>
  {:else}
    <div class="empty-state">
      <div class="empty-icon">✎</div>
      <p class="empty-title">No note open</p>
      <p class="empty-hint">Select a note from the sidebar to start editing.</p>
    </div>
  {/if}
</section>

<style>
  .editor-pane {
    flex: 1;
    height: 100%;
    display: flex;
    align-items: center;
    justify-content: center;
    background-color: var(--bg-panel);
    overflow: hidden;
  }

  .editor-pane.has-note {
    align-items: stretch;
    justify-content: stretch;
  }

  .note-view {
    flex: 1;
    min-width: 0;
    min-height: 0;
    display: flex;
    flex-direction: column;
  }

  .note-meta {
    flex-shrink: 0;
  }

  .note-header {
    max-width: 46rem;
    width: 100%;
    margin: 0 auto;
    padding: 28px 16px 4px;
  }

  /* The bare "+ Add properties" affordance stays out of the way until the
     title/properties area is hovered or focused. */
  .note-meta :global(.add-props) {
    opacity: 0;
    transition: opacity 0.15s ease;
  }

  .note-meta:hover :global(.add-props),
  .note-meta:focus-within :global(.add-props) {
    opacity: 1;
  }

  .note-title {
    display: block;
    width: 100%;
    margin: 0;
    padding: 2px 0;
    border: none;
    border-radius: 4px;
    background: transparent;
    color: var(--text);
    font-family: var(--font-content);
    font-size: 30px;
    font-weight: 700;
    line-height: 1.2;
    outline: none;
  }

  .note-title:focus {
    background-color: var(--code-bg);
  }

  .empty-state {
    text-align: center;
    color: var(--text-muted);
    padding: 24px;
  }

  .empty-icon {
    font-size: 40px;
    line-height: 1;
    margin-bottom: 16px;
    opacity: 0.5;
  }

  .empty-title {
    margin: 0 0 6px;
    font-size: 16px;
    font-weight: 600;
    color: var(--text);
  }

  .empty-hint {
    margin: 0;
    font-size: 13px;
    color: var(--text-muted);
  }
</style>
