<!--
  TODO M2: this is a temporary read-only stub. The real markdown editor
  arrives in Milestone 2; for now the selected note's raw contents are shown
  in a plain <pre>.
-->
<script lang="ts">
  import { readNote, selected } from "$lib/stores/vault";

  let content = $state<string | null>(null);
  let loadError = $state<string | null>(null);

  $effect(() => {
    const sel = $selected;
    if (!sel || sel.isDir) {
      content = null;
      loadError = null;
      return;
    }
    let stale = false;
    readNote(sel.path)
      .then((text) => {
        if (!stale) {
          content = text;
          loadError = null;
        }
      })
      .catch((e) => {
        if (!stale) {
          content = null;
          loadError = String(e);
        }
      });
    return () => {
      stale = true;
    };
  });

  let fileSelected = $derived($selected !== null && !$selected.isDir);
</script>

<section class="editor-pane" class:has-note={fileSelected}>
  {#if fileSelected}
    <div class="note-view">
      <header class="note-header">
        <h1 class="note-title">
          {$selected?.path.split("/").pop()?.replace(/\.md$/i, "")}
        </h1>
        <span class="note-path">{$selected?.path}</span>
      </header>
      {#if loadError}
        <p class="load-error" role="alert">{loadError}</p>
      {:else}
        <pre class="raw-content">{content ?? ""}</pre>
      {/if}
      <p class="stub-note">Read-only preview — editing arrives in M2.</p>
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
    overflow-y: auto;
  }

  .editor-pane.has-note {
    align-items: stretch;
    justify-content: stretch;
  }

  .note-view {
    flex: 1;
    max-width: 820px;
    margin: 0 auto;
    padding: 32px 40px;
    display: flex;
    flex-direction: column;
    min-width: 0;
  }

  .note-header {
    margin-bottom: 16px;
    padding-bottom: 12px;
    border-bottom: 1px solid var(--border);
  }

  .note-title {
    margin: 0 0 4px;
    font-size: 22px;
    font-weight: 650;
    color: var(--text);
    font-family: var(--font-content);
  }

  .note-path {
    font-size: 12px;
    color: var(--text-muted);
    font-family: var(--font-mono);
    background: transparent;
  }

  .raw-content {
    margin: 0;
    padding: 16px;
    border-radius: 8px;
    background-color: var(--code-bg);
    font-family: var(--font-mono);
    font-size: 13px;
    line-height: 1.6;
    color: var(--text);
    white-space: pre-wrap;
    word-break: break-word;
    overflow-x: auto;
  }

  .load-error {
    padding: 12px;
    border: 1px solid #b91c1c;
    border-radius: 8px;
    font-size: 13px;
    color: #b91c1c;
  }

  .stub-note {
    margin: 12px 0 0;
    font-size: 12px;
    font-style: italic;
    color: var(--text-muted);
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
