<script lang="ts">
  import { tick } from "svelte";
  import { fuzzyScore } from "$lib/utils/fuzzy";
  import { listNotes, type NoteRef } from "$lib/stores/search";
  import { createNamedNote, selected, vaultError } from "$lib/stores/vault";
  import { quickSwitcherOpen } from "$lib/stores/ui";

  const MAX_RESULTS = 50;

  interface Scored {
    note: NoteRef;
    /** Matched indices within the title, for highlighting. */
    titlePositions: number[];
  }

  let notes = $state<NoteRef[]>([]);
  let query = $state("");
  let activeIndex = $state(0);
  let input = $state<HTMLInputElement | undefined>();
  let listEl = $state<HTMLElement | undefined>();

  // Load the note list and reset state whenever the switcher opens.
  let wasOpen = false;
  $effect(() => {
    const open = $quickSwitcherOpen;
    if (open && !wasOpen) {
      query = "";
      activeIndex = 0;
      notes = [];
      listNotes()
        .then((n) => (notes = n))
        .catch((e) => vaultError.set(String(e)));
      tick().then(() => input?.focus());
    }
    wasOpen = open;
  });

  // Fuzzy-ranked results. Empty query → recent notes (already mtime-sorted).
  let results = $derived.by<Scored[]>(() => {
    const q = query.trim();
    if (!q) {
      return notes.slice(0, MAX_RESULTS).map((note) => ({ note, titlePositions: [] }));
    }
    const scored: { note: NoteRef; score: number; titlePositions: number[] }[] = [];
    for (const note of notes) {
      const ts = fuzzyScore(q, note.title);
      const ps = fuzzyScore(q, note.path);
      if (!ts && !ps) continue;
      // Prefer title matches; a path-only match still qualifies.
      const titleScore = ts ? ts.score + 5 : -Infinity;
      const pathScore = ps ? ps.score : -Infinity;
      scored.push({
        note,
        score: Math.max(titleScore, pathScore),
        titlePositions: ts ? ts.positions : [],
      });
    }
    scored.sort((a, b) => b.score - a.score);
    return scored.slice(0, MAX_RESULTS);
  });

  // Whether an exact (case-insensitive) title match already exists — gates the
  // "create note" affordance.
  let exactExists = $derived(
    notes.some((n) => n.title.toLowerCase() === query.trim().toLowerCase()),
  );
  let canCreate = $derived(query.trim().length > 0 && !exactExists);
  // The create row sits just past the last result; selecting it triggers create.
  let createIndex = $derived(canCreate ? results.length : -1);
  let itemCount = $derived(results.length + (canCreate ? 1 : 0));

  // Keep the active index in range as results change.
  $effect(() => {
    if (activeIndex >= itemCount) activeIndex = Math.max(0, itemCount - 1);
  });

  function close(): void {
    quickSwitcherOpen.set(false);
  }

  function openNote(path: string): void {
    selected.set({ path, isDir: false });
    close();
  }

  async function createNote(): Promise<void> {
    const name = query.trim();
    if (!name) return;
    try {
      await createNamedNote(name);
      close();
    } catch (e) {
      vaultError.set(String(e));
    }
  }

  function choose(index: number): void {
    if (index === createIndex) {
      createNote();
    } else if (index >= 0 && index < results.length) {
      openNote(results[index].note.path);
    }
  }

  async function scrollActiveIntoView(): Promise<void> {
    await tick();
    const el = listEl?.querySelector<HTMLElement>('[data-active="true"]');
    el?.scrollIntoView({ block: "nearest" });
  }

  function onKeydown(event: KeyboardEvent): void {
    if (event.key === "Escape") {
      event.preventDefault();
      close();
    } else if (event.key === "ArrowDown") {
      event.preventDefault();
      if (itemCount > 0) activeIndex = (activeIndex + 1) % itemCount;
      scrollActiveIntoView();
    } else if (event.key === "ArrowUp") {
      event.preventDefault();
      if (itemCount > 0) activeIndex = (activeIndex - 1 + itemCount) % itemCount;
      scrollActiveIntoView();
    } else if (event.key === "Enter") {
      event.preventDefault();
      if (event.shiftKey && canCreate) {
        createNote();
      } else {
        choose(activeIndex);
      }
    }
  }

  /** Splits a title into highlighted / plain segments for rendering. */
  function segments(title: string, positions: number[]): { text: string; hit: boolean }[] {
    if (positions.length === 0) return [{ text: title, hit: false }];
    const hit = new Set(positions);
    const out: { text: string; hit: boolean }[] = [];
    let buf = "";
    let bufHit = hit.has(0);
    for (let i = 0; i < title.length; i++) {
      const h = hit.has(i);
      if (h !== bufHit) {
        out.push({ text: buf, hit: bufHit });
        buf = "";
        bufHit = h;
      }
      buf += title[i];
    }
    if (buf) out.push({ text: buf, hit: bufHit });
    return out;
  }

  function relDir(path: string): string {
    const idx = path.lastIndexOf("/");
    return idx === -1 ? "" : path.slice(0, idx);
  }
</script>

{#if $quickSwitcherOpen}
  <div
    class="overlay"
    role="presentation"
    onclick={close}
    onkeydown={onKeydown}
  >
    <div
      class="switcher"
      role="dialog"
      aria-modal="true"
      aria-label="Quick switcher"
      tabindex="-1"
      onkeydown={onKeydown}
      onclick={(e) => e.stopPropagation()}
    >
      <input
        bind:this={input}
        class="switcher-input"
        type="text"
        placeholder="Search notes by name…"
        spellcheck="false"
        autocomplete="off"
        bind:value={query}
        onkeydown={onKeydown}
      />
      <ul class="results" bind:this={listEl} role="listbox" aria-label="Notes">
        {#each results as item, i (item.note.path)}
          <li>
            <button
              type="button"
              class="result"
              class:active={i === activeIndex}
              data-active={i === activeIndex}
              role="option"
              aria-selected={i === activeIndex}
              onmousemove={() => (activeIndex = i)}
              onclick={() => choose(i)}
            >
              <span class="title">
                {#each segments(item.note.title, item.titlePositions) as seg}
                  {#if seg.hit}<span class="hit">{seg.text}</span>{:else}{seg.text}{/if}
                {/each}
              </span>
              {#if relDir(item.note.path)}
                <span class="path">{relDir(item.note.path)}</span>
              {/if}
            </button>
          </li>
        {/each}

        {#if canCreate}
          <li>
            <button
              type="button"
              class="result create"
              class:active={activeIndex === createIndex}
              data-active={activeIndex === createIndex}
              role="option"
              aria-selected={activeIndex === createIndex}
              onmousemove={() => (activeIndex = createIndex)}
              onclick={() => choose(createIndex)}
            >
              <span class="title">Create “{query.trim()}”</span>
              <span class="path">Enter</span>
            </button>
          </li>
        {:else if results.length === 0}
          <li class="empty">No matching notes</li>
        {/if}
      </ul>
    </div>
  </div>
{/if}

<style>
  .overlay {
    position: fixed;
    inset: 0;
    z-index: 100;
    display: flex;
    justify-content: center;
    align-items: flex-start;
    padding-top: 20vh;
    background-color: rgba(0, 0, 0, 0.32);
  }

  .switcher {
    width: 560px;
    max-width: calc(100vw - 32px);
    background-color: var(--bg-panel);
    border: 1px solid var(--border);
    border-radius: 12px;
    box-shadow: var(--shadow-modal);
    overflow: hidden;
  }

  .switcher-input {
    width: 100%;
    padding: 14px 16px;
    border: none;
    border-bottom: 1px solid var(--border);
    background: transparent;
    color: var(--text);
    font-family: var(--font-ui);
    font-size: 16px;
    outline: none;
  }

  .switcher-input::placeholder {
    color: var(--text-muted);
  }

  .results {
    list-style: none;
    margin: 0;
    padding: 6px;
    max-height: calc(10 * 44px);
    overflow-y: auto;
  }

  .result {
    display: flex;
    align-items: baseline;
    gap: 10px;
    width: 100%;
    padding: 8px 10px;
    border: none;
    border-radius: 7px;
    background: transparent;
    color: var(--text);
    font-family: var(--font-ui);
    font-size: 14px;
    text-align: left;
    cursor: pointer;
  }

  .result.active {
    background-color: var(--accent);
    color: var(--accent-contrast);
  }

  .title {
    flex-shrink: 0;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    max-width: 60%;
  }

  .hit {
    font-weight: 700;
    color: var(--accent);
  }

  .result.active .hit {
    color: var(--accent-contrast);
    text-decoration: underline;
  }

  .path {
    flex: 1;
    min-width: 0;
    font-size: 12px;
    color: var(--text-muted);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    text-align: right;
  }

  .result.active .path {
    color: var(--accent-contrast);
    opacity: 0.85;
  }

  .create .title {
    font-style: italic;
  }

  .empty {
    padding: 14px 10px;
    color: var(--text-muted);
    font-size: 13px;
    text-align: center;
  }
</style>
