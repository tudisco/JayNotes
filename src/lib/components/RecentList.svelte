<script lang="ts">
  import { listNotes, type NoteRef } from "$lib/stores/search";
  import { vaultChanged } from "$lib/stores/indexEvents";
  import {
    commitRename,
    openContextMenu,
    renamingPath,
    selected,
    vaultError,
    type TreeNode,
  } from "$lib/stores/vault";
  import { relativeTime } from "$lib/utils/time";

  let notes = $state<NoteRef[]>([]);
  let loaded = $state(false);

  function load(): void {
    listNotes()
      .then((r) => {
        notes = r;
        loaded = true;
      })
      .catch((e) => vaultError.set(String(e)));
  }

  // Initial load (also fires when this list becomes the active view, since the
  // component mounts only then) and refresh on every external vault change.
  let lastSeq = -1;
  $effect(() => {
    const seq = $vaultChanged.seq;
    if (seq !== lastSeq) {
      lastSeq = seq;
      load();
    }
  });

  function relDir(path: string): string {
    const idx = path.lastIndexOf("/");
    return idx === -1 ? "" : path.slice(0, idx);
  }

  function fileName(path: string): string {
    const idx = path.lastIndexOf("/");
    return idx === -1 ? path : path.slice(idx + 1);
  }

  /** A minimal TreeNode so the shared ContextMenu can act on a recent row. */
  function nodeOf(note: NoteRef): TreeNode {
    return { name: fileName(note.path), path: note.path, isDir: false, children: [] };
  }

  function open(note: NoteRef): void {
    selected.set({ path: note.path, isDir: false });
  }

  function handleContextMenu(event: MouseEvent, note: NoteRef): void {
    event.preventDefault();
    event.stopPropagation();
    selected.set({ path: note.path, isDir: false });
    openContextMenu(event.clientX, event.clientY, nodeOf(note));
  }

  async function handleRenameKey(event: KeyboardEvent, note: NoteRef): Promise<void> {
    const input = event.currentTarget as HTMLInputElement;
    if (event.key === "Enter") {
      event.preventDefault();
      try {
        await commitRename(nodeOf(note), input.value);
      } catch (e) {
        vaultError.set(String(e));
      }
    } else if (event.key === "Escape") {
      event.preventDefault();
      renamingPath.set(null);
    }
  }

  function focusAndSelect(el: HTMLInputElement): void {
    el.focus();
    el.select();
  }

  function displayTitle(note: NoteRef): string {
    return fileName(note.path).replace(/\.md$/i, "");
  }
</script>

{#if loaded && notes.length === 0}
  <div class="empty">No notes yet</div>
{:else}
  <ul class="recent" role="group">
    {#each notes as note (note.path)}
      {@const isSelected = $selected?.path === note.path}
      <li>
        {#if $renamingPath === note.path}
          <div class="row renaming">
            <input
              class="rename-input"
              type="text"
              value={displayTitle(note)}
              use:focusAndSelect
              onkeydown={(e) => handleRenameKey(e, note)}
              onblur={() => renamingPath.set(null)}
            />
          </div>
        {:else}
          <button
            type="button"
            class="row"
            class:selected={isSelected}
            onclick={() => open(note)}
            oncontextmenu={(e) => handleContextMenu(e, note)}
            title={note.path}
          >
            <span class="main">
              <span class="name">{note.title}</span>
              {#if relDir(note.path)}
                <span class="dir">{relDir(note.path)}</span>
              {/if}
            </span>
            <span class="time">{relativeTime(note.mtime)}</span>
          </button>
        {/if}
      </li>
    {/each}
  </ul>
{/if}

<style>
  .recent {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 1px;
  }

  .row {
    display: flex;
    align-items: baseline;
    gap: 8px;
    width: 100%;
    padding: 4px 8px;
    border: none;
    border-radius: 5px;
    background: transparent;
    color: var(--text);
    font-family: var(--font-ui);
    text-align: left;
    cursor: pointer;
  }

  .row:hover {
    background-color: var(--hover);
  }

  .row.selected {
    background-color: var(--accent);
    color: var(--accent-contrast);
  }

  .row.selected:hover {
    background-color: var(--accent-hover);
  }

  .main {
    display: flex;
    flex-direction: column;
    gap: 1px;
    min-width: 0;
    flex: 1;
  }

  .name {
    font-size: 13px;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .dir {
    font-size: 11px;
    color: var(--text-muted);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .row.selected .dir {
    color: var(--accent-contrast);
    opacity: 0.75;
  }

  .time {
    flex-shrink: 0;
    font-size: 11px;
    color: var(--text-muted);
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }

  .row.selected .time {
    color: var(--accent-contrast);
    opacity: 0.85;
  }

  .row.renaming {
    cursor: default;
  }

  .rename-input {
    flex: 1;
    min-width: 0;
    padding: 1px 4px;
    border: 1px solid var(--accent);
    border-radius: 4px;
    background-color: var(--bg-panel);
    color: var(--text);
    font-size: 13px;
    font-family: var(--font-ui);
    outline: none;
  }

  .empty {
    margin-top: 8px;
    padding: 12px 8px;
    font-size: 13px;
    color: var(--text-muted);
    font-style: italic;
  }
</style>
