<script lang="ts">
  import { listTags, notesByTag, type SearchHit, type TagCount } from "$lib/stores/search";
  import { openContextMenu, selected, vaultError, type TreeNode } from "$lib/stores/vault";
  import { noteSaved, vaultChanged } from "$lib/stores/indexEvents";

  let tags = $state<TagCount[]>([]);
  let loaded = $state(false);

  // The tag currently drilled into, or null for the full tag list.
  let activeTag = $state<string | null>(null);
  let hits = $state<SearchHit[]>([]);

  function loadTags(): void {
    listTags()
      .then((r) => {
        tags = r;
        loaded = true;
      })
      .catch((e) => vaultError.set(String(e)));
  }

  // Initial load, refresh on external vault changes, and on in-app index edits
  // (self-writes never emit `vault-changed`; `noteSaved` is bumped after a
  // context-menu delete so an emptied tag drops out and its note list updates).
  let lastSeq = -1;
  let lastSaveSeq = -1;
  $effect(() => {
    const seq = $vaultChanged.seq;
    const saveSeq = $noteSaved;
    if (seq !== lastSeq || saveSeq !== lastSaveSeq) {
      lastSeq = seq;
      lastSaveSeq = saveSeq;
      loadTags();
      // If a tag is open, refresh its note list too (it may have changed).
      if (activeTag) openTag(activeTag);
    }
  });

  function openTag(tag: string): void {
    activeTag = tag;
    notesByTag(tag)
      .then((r) => {
        // A stale index can report a tag that no note actually carries any
        // more: fall back to the full list and re-fetch tags.
        if (r.length === 0) {
          activeTag = null;
          loadTags();
          return;
        }
        hits = r;
      })
      .catch((e) => vaultError.set(String(e)));
  }

  function back(): void {
    activeTag = null;
    hits = [];
  }

  function open(hit: SearchHit): void {
    selected.set({ path: hit.path, isDir: false });
  }

  function fileName(path: string): string {
    const idx = path.lastIndexOf("/");
    return idx === -1 ? path : path.slice(idx + 1);
  }

  /** A minimal TreeNode so the shared ContextMenu can act on a tag-list row. */
  function nodeOf(hit: SearchHit): TreeNode {
    return { name: fileName(hit.path), path: hit.path, isDir: false, children: [] };
  }

  // Right-click opens the same shared ContextMenu as the tree/recent rows
  // (rename / delete-with-confirm / reveal-in-Finder, capability-gated). Rename
  // from here sets the shared `renamingPath`; there is no inline rename row in
  // this panel, so the rename input appears in the file tree (Files tab).
  function handleContextMenu(event: MouseEvent, hit: SearchHit): void {
    event.preventDefault();
    event.stopPropagation();
    selected.set({ path: hit.path, isDir: false });
    openContextMenu(event.clientX, event.clientY, nodeOf(hit));
  }

  function relDir(path: string): string {
    const idx = path.lastIndexOf("/");
    return idx === -1 ? "" : path.slice(0, idx);
  }
</script>

<div class="tags-panel">
  {#if activeTag}
    <button type="button" class="back" onclick={back}>
      <svg viewBox="0 0 16 16" width="13" height="13" aria-hidden="true">
        <path
          d="M9.5 3.5L5 8l4.5 4.5"
          fill="none"
          stroke="currentColor"
          stroke-width="1.4"
          stroke-linecap="round"
          stroke-linejoin="round"
        />
      </svg>
      <span class="back-label">#{activeTag}</span>
    </button>

    <div class="results">
      <ul class="hit-list">
        {#each hits as hit (hit.path)}
          <li>
            <button
              type="button"
              class="hit"
              onclick={() => open(hit)}
              oncontextmenu={(e) => handleContextMenu(e, hit)}
              title={hit.path}
            >
              <span class="hit-title">{hit.title}</span>
              {#if relDir(hit.path)}
                <span class="hit-path">{relDir(hit.path)}</span>
              {/if}
              {#if hit.snippet}
                <span class="hit-snippet">{hit.snippet}</span>
              {/if}
            </button>
          </li>
        {/each}
      </ul>
    </div>
  {:else}
    <div class="results">
      {#if loaded && tags.length === 0}
        <p class="hint">No tags yet.</p>
      {:else}
        <ul class="tag-list">
          {#each tags as t (t.tag)}
            <li>
              <button type="button" class="tag-row" onclick={() => openTag(t.tag)}>
                <span class="tag-name">#{t.tag}</span>
                <span class="tag-count">{t.count}</span>
              </button>
            </li>
          {/each}
        </ul>
      {/if}
    </div>
  {/if}
</div>

<style>
  .tags-panel {
    display: flex;
    flex-direction: column;
    height: 100%;
    min-height: 0;
  }

  .results {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
  }

  .hint {
    margin: 8px 4px;
    font-size: 12px;
    line-height: 1.5;
    color: var(--text-muted);
  }

  .back {
    display: flex;
    align-items: center;
    gap: 6px;
    width: 100%;
    margin: 0 0 6px;
    padding: 6px 8px;
    border: none;
    border-radius: 6px;
    background: transparent;
    color: var(--text-muted);
    font-family: var(--font-ui);
    font-size: 13px;
    font-weight: 600;
    text-align: left;
    cursor: pointer;
  }

  .back:hover {
    background-color: var(--hover);
    color: var(--text);
  }

  .back-label {
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .tag-list,
  .hit-list {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 2px;
  }

  .tag-row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
    width: 100%;
    padding: 7px 8px;
    border: none;
    border-radius: 6px;
    background: transparent;
    color: var(--text);
    font-family: var(--font-ui);
    font-size: 13px;
    text-align: left;
    cursor: pointer;
  }

  .tag-row:hover {
    background-color: var(--hover);
  }

  .tag-name {
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .tag-count {
    flex-shrink: 0;
    min-width: 20px;
    padding: 1px 6px;
    border-radius: 999px;
    background-color: var(--code-bg);
    color: var(--text-muted);
    font-size: 11px;
    font-variant-numeric: tabular-nums;
    text-align: center;
  }

  .hit {
    display: flex;
    flex-direction: column;
    gap: 2px;
    width: 100%;
    padding: 7px 8px;
    border: none;
    border-radius: 6px;
    background: transparent;
    color: var(--text);
    font-family: var(--font-ui);
    text-align: left;
    cursor: pointer;
  }

  .hit:hover {
    background-color: var(--hover);
  }

  .hit-title {
    font-size: 13px;
    font-weight: 600;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .hit-path {
    font-size: 11px;
    color: var(--text-muted);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .hit-snippet {
    font-size: 12px;
    line-height: 1.45;
    color: var(--text-muted);
    display: -webkit-box;
    -webkit-line-clamp: 2;
    line-clamp: 2;
    -webkit-box-orient: vertical;
    overflow: hidden;
  }
</style>
