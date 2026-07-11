<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { revealItemInDir } from "@tauri-apps/plugin-opener";
  import Editor from "./Editor.svelte";
  import PropertiesBar from "./PropertiesBar.svelte";
  import { renameNote, selected, vaultError } from "$lib/stores/vault";
  import { vaultChanged } from "$lib/stores/indexEvents";

  let fileSelected = $derived($selected !== null && !$selected.isDir);
  let notePath = $derived(fileSelected ? ($selected as { path: string }).path : null);

  // Single shared source of truth for the current note's verbatim frontmatter.
  // Editor loads it (and owns the body); PropertiesBar edits it. Both persist
  // through the same Editor writer so their saves can't clobber each other.
  let editor = $state<Editor | undefined>();
  let frontmatter = $state<string | null>(null);

  // Bumping this remounts the Editor (via the {#key} below), forcing a fresh
  // read from disk. Used to reload the open note after an external change.
  let reloadNonce = $state(0);
  let lastSeq = 0;

  // When the watcher reports the open note changed on disk, reload it — but
  // only if the editor has no unsaved edits, so we never stomp the user's work.
  $effect(() => {
    const change = $vaultChanged;
    if (change.seq === lastSeq) return;
    lastSeq = change.seq;
    const p = notePath;
    if (p && change.paths.includes(p) && editor && !editor.isDirty()) {
      reloadNonce += 1;
    }
  });

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

  // "Export as PDF…": save the current note to a PDF the user picks, then
  // reveal it in Finder. Reuses SettingsMenu's transient-status pattern.
  let exportStatus = $state<"idle" | "exporting" | "done" | "error">("idle");

  async function exportPdf(): Promise<void> {
    const p = notePath;
    if (!p || exportStatus === "exporting") return;
    exportStatus = "exporting";
    try {
      const out = await invoke<string>("export_note_pdf", { relPath: p });
      if (!out) {
        exportStatus = "idle"; // user cancelled the save dialog
        return;
      }
      exportStatus = "done";
      try {
        await revealItemInDir(out);
      } catch {
        // Revealing is a nicety; a failure here shouldn't surface as an error.
      }
      setTimeout(() => {
        if (exportStatus === "done") exportStatus = "idle";
      }, 2500);
    } catch (e) {
      exportStatus = "error";
      vaultError.set(String(e));
      setTimeout(() => {
        if (exportStatus === "error") exportStatus = "idle";
      }, 2500);
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
      {#key `${notePath}:${reloadNonce}`}
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
            <div class="note-actions">
              {#if exportStatus === "exporting"}
                <span class="export-status">Exporting…</span>
              {:else if exportStatus === "done"}
                <span class="export-status">Exported</span>
              {/if}
              <button
                type="button"
                class="icon-button"
                title="Export as PDF…"
                aria-label="Export as PDF"
                disabled={exportStatus === "exporting"}
                onclick={exportPdf}
              >
                <svg
                  width="16"
                  height="16"
                  viewBox="0 0 24 24"
                  fill="none"
                  stroke="currentColor"
                  stroke-width="2"
                  stroke-linecap="round"
                  stroke-linejoin="round"
                  aria-hidden="true"
                >
                  <path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z" />
                  <path d="M14 2v6h6" />
                  <path d="M12 18v-6" />
                  <path d="M9 15l3 3 3-3" />
                </svg>
              </button>
            </div>
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
    display: flex;
    align-items: flex-start;
    gap: 8px;
    max-width: 46rem;
    width: 100%;
    margin: 0 auto;
    padding: 28px 16px 4px;
  }

  .note-actions {
    display: flex;
    align-items: center;
    gap: 8px;
    flex-shrink: 0;
    padding-top: 4px;
  }

  .export-status {
    font-size: 12px;
    color: var(--text-muted);
    white-space: nowrap;
  }

  /* Subtle inline-SVG icon button, revealed like the "+ Add properties"
     affordance when the title/properties area is hovered or focused. */
  .icon-button {
    display: flex;
    align-items: center;
    justify-content: center;
    width: 28px;
    height: 28px;
    padding: 0;
    border: none;
    border-radius: 6px;
    background: transparent;
    color: var(--text-muted);
    cursor: pointer;
    opacity: 0;
    transition:
      opacity 0.15s ease,
      background-color 0.15s ease,
      color 0.15s ease;
  }

  .note-meta:hover .icon-button,
  .note-meta:focus-within .icon-button,
  .icon-button:focus-visible {
    opacity: 1;
  }

  .icon-button:hover:not(:disabled) {
    background-color: var(--hover);
    color: var(--accent);
  }

  .icon-button:disabled {
    opacity: 1;
    color: var(--text-muted);
    cursor: default;
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
    font-size: 28px;
    font-weight: 600;
    line-height: 1.25;
    letter-spacing: -0.015em;
    outline: none;
  }

  .note-title:focus {
    background-color: var(--hover);
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
