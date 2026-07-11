<script lang="ts">
  import { tick } from "svelte";
  import { searchNotes, type SearchHit } from "$lib/stores/search";
  import { selected, vaultError } from "$lib/stores/vault";
  import { vaultChanged } from "$lib/stores/indexEvents";
  import { searchFocusNonce } from "$lib/stores/ui";

  const DEBOUNCE_MS = 200;

  let query = $state("");
  let hits = $state<SearchHit[]>([]);
  let searched = $state(false);
  let input = $state<HTMLInputElement | undefined>();

  function runSearch(): void {
    const q = query.trim();
    if (!q) {
      hits = [];
      searched = false;
      return;
    }
    searchNotes(q)
      .then((r) => {
        hits = r;
        searched = true;
      })
      .catch((e) => vaultError.set(String(e)));
  }

  // Debounced search on every keystroke.
  $effect(() => {
    query; // track
    const timer = setTimeout(runSearch, DEBOUNCE_MS);
    return () => clearTimeout(timer);
  });

  // Re-run the active search when the vault changes on disk.
  let lastSeq = 0;
  $effect(() => {
    const change = $vaultChanged;
    if (change.seq !== lastSeq) {
      lastSeq = change.seq;
      if (query.trim()) runSearch();
    }
  });

  // Focus the input on mount and whenever Cmd+Shift+F is pressed again.
  $effect(() => {
    $searchFocusNonce; // track
    tick().then(() => input?.focus());
  });

  function open(hit: SearchHit): void {
    selected.set({ path: hit.path, isDir: false });
  }

  function relDir(path: string): string {
    const idx = path.lastIndexOf("/");
    return idx === -1 ? "" : path.slice(0, idx);
  }

  // Split a snippet into plain / highlighted segments. The backend only ever
  // injects <mark>…</mark>; every other character is treated as literal text
  // and rendered through Svelte's escaping, so raw HTML in a note can't leak.
  function snippetSegments(snip: string): { text: string; mark: boolean }[] {
    const parts = snip.split(/(<mark>|<\/mark>)/);
    const out: { text: string; mark: boolean }[] = [];
    let mark = false;
    for (const p of parts) {
      if (p === "<mark>") {
        mark = true;
      } else if (p === "</mark>") {
        mark = false;
      } else if (p) {
        out.push({ text: p, mark });
      }
    }
    return out;
  }
</script>

<div class="search-panel">
  <input
    bind:this={input}
    class="search-input"
    type="text"
    placeholder="Search notes…  (try tag:idea)"
    spellcheck="false"
    autocomplete="off"
    bind:value={query}
  />

  <div class="results">
    {#if !query.trim()}
      <p class="hint">
        Search titles and content. Filter by tag with
        <code>tag:name</code>.
      </p>
    {:else if searched && hits.length === 0}
      <p class="hint">No results for “{query.trim()}”.</p>
    {:else}
      <ul class="hit-list">
        {#each hits as hit (hit.path)}
          <li>
            <button type="button" class="hit" onclick={() => open(hit)} title={hit.path}>
              <span class="hit-title">{hit.title}</span>
              {#if relDir(hit.path)}
                <span class="hit-path">{relDir(hit.path)}</span>
              {/if}
              {#if hit.snippet}
                <span class="hit-snippet">
                  {#each snippetSegments(hit.snippet) as seg}
                    {#if seg.mark}<mark>{seg.text}</mark>{:else}{seg.text}{/if}
                  {/each}
                </span>
              {/if}
            </button>
          </li>
        {/each}
      </ul>
    {/if}
  </div>
</div>

<style>
  .search-panel {
    display: flex;
    flex-direction: column;
    height: 100%;
    min-height: 0;
  }

  .search-input {
    margin: 0 0 8px;
    padding: 7px 10px;
    border: 1px solid var(--border);
    border-radius: 6px;
    background-color: var(--bg-panel);
    color: var(--text);
    font-family: var(--font-ui);
    font-size: 13px;
    outline: none;
  }

  .search-input:focus {
    border-color: var(--accent);
  }

  .search-input::placeholder {
    color: var(--text-muted);
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

  .hint code {
    padding: 1px 4px;
    border-radius: 3px;
    background-color: var(--code-bg);
    font-size: 11px;
  }

  .hit-list {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 2px;
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
    background-color: var(--code-bg);
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

  .hit-snippet :global(mark) {
    padding: 0 1px;
    border-radius: 2px;
    background-color: color-mix(in srgb, var(--accent) 22%, transparent);
    color: var(--text);
  }
</style>
