<!--
  Editor.svelte — Milkdown Crepe live-preview editor for a single note.

  Given a `path` (vault-relative), it loads the note, strips any frontmatter
  block (kept verbatim in memory), and mounts Crepe on the body. Edits are
  autosaved with a 600ms debounce; a save is also flushed immediately on note
  switch, component teardown, window blur, and tab hide.

  Crepe has no cheap "set markdown" API, so switching notes tears the editor
  down and recreates it — simple and reliable.

  The verbatim frontmatter string is a `$bindable` prop so it can be lifted to
  EditorPane and shared with PropertiesBar: the editor loads it, owns the body,
  and both write through this single source of truth. When the properties bar
  mutates the frontmatter it calls the exported `requestSave()`, which persists
  `joinFrontmatter(frontmatter, body)` on the same path the autosave uses — so
  neither side can clobber the other's changes.
-->
<script lang="ts">
  import { onDestroy } from "svelte";
  import { Crepe } from "@milkdown/crepe";
  import { readNote, writeNote, vaultError } from "$lib/stores/vault";
  import { joinFrontmatter, splitFrontmatter } from "$lib/utils/frontmatter";

  let {
    path,
    frontmatter = $bindable(null),
  }: { path: string; frontmatter?: string | null } = $props();

  const SAVE_DEBOUNCE_MS = 600;

  let host: HTMLDivElement;
  let crepe: Crepe | null = null;

  /** Path of the note currently mounted in the editor. */
  let currentPath: string | null = null;
  /** Body content as last persisted to disk (Crepe-serialized form). */
  let lastSavedBody = "";
  /** Frontmatter as last persisted to disk — lets us detect properties edits. */
  let lastSavedFrontmatter: string | null = null;
  /** True only once the editor is fully created — guards initial-load events. */
  let loaded = false;
  /** Monotonic token to discard stale async load/teardown work. */
  let opToken = 0;

  let saveTimer: ReturnType<typeof setTimeout> | null = null;
  let status = $state<"idle" | "saving" | "saved">("idle");
  let loadError = $state<string | null>(null);

  function clearSaveTimer(): void {
    if (saveTimer !== null) {
      clearTimeout(saveTimer);
      saveTimer = null;
    }
  }

  /** Persist the current editor content if it differs from what's on disk. */
  async function flush(): Promise<void> {
    clearSaveTimer();
    if (!crepe || !loaded || !currentPath) return;
    const body = crepe.getMarkdown();
    if (body === lastSavedBody && frontmatter === lastSavedFrontmatter) {
      status = "saved";
      return;
    }
    const target = currentPath;
    const fm = frontmatter;
    try {
      await writeNote(target, joinFrontmatter(fm, body));
      lastSavedBody = body;
      lastSavedFrontmatter = fm;
      status = "saved";
    } catch (e) {
      status = "idle";
      vaultError.set(String(e));
    }
  }

  function scheduleSave(): void {
    status = "saving";
    clearSaveTimer();
    saveTimer = setTimeout(() => void flush(), SAVE_DEBOUNCE_MS);
  }

  /**
   * Persist a frontmatter change made outside the editor (the properties bar).
   * Uses the same debounced save path so tag/field edits and body edits share
   * one writer and can't overwrite each other.
   */
  export function requestSave(): void {
    if (!loaded) return;
    scheduleSave();
  }

  /**
   * True when the editor holds unsaved changes (body or frontmatter differ from
   * what's on disk). Used to decide whether an external file change may safely
   * reload the note without clobbering the user's edits.
   */
  export function isDirty(): boolean {
    if (!crepe || !loaded) return false;
    return crepe.getMarkdown() !== lastSavedBody || frontmatter !== lastSavedFrontmatter;
  }

  /** Flush + destroy the current editor instance. */
  async function teardown(): Promise<void> {
    if (!crepe) return;
    await flush();
    const dying = crepe;
    crepe = null;
    loaded = false;
    currentPath = null;
    await dying.destroy();
  }

  async function load(p: string, token: number): Promise<void> {
    let raw: string;
    try {
      raw = await readNote(p);
    } catch (e) {
      if (token === opToken) loadError = String(e);
      return;
    }
    if (token !== opToken) return;

    loadError = null;
    const split = splitFrontmatter(raw);
    frontmatter = split.frontmatter;
    lastSavedFrontmatter = split.frontmatter;
    currentPath = p;

    // Recreate into a clean host in case any prior DOM survived teardown.
    host.innerHTML = "";
    const instance = new Crepe({
      root: host,
      defaultValue: split.body,
      features: { [Crepe.Feature.TopBar]: false },
      featureConfigs: {
        [Crepe.Feature.Placeholder]: { text: "Start writing…", mode: "block" },
      },
    });
    instance.on((listener) => {
      listener.markdownUpdated((_ctx, markdown) => {
        // Ignore events fired before the editor finished loading, and no-op
        // re-serializations that match the loaded content.
        if (!loaded || currentPath !== p) return;
        if (markdown === lastSavedBody) return;
        scheduleSave();
      });
    });

    await instance.create();
    if (token !== opToken) {
      await instance.destroy();
      return;
    }
    crepe = instance;
    lastSavedBody = instance.getMarkdown();
    loaded = true;
    status = "idle";
  }

  async function switchTo(p: string): Promise<void> {
    const token = ++opToken;
    await teardown();
    if (token !== opToken) return;
    await load(p, token);
  }

  // React to note changes: whenever `path` changes, flush the old note and
  // mount the new one. `host` is bound before this effect first runs.
  $effect(() => {
    const p = path;
    void switchTo(p);
  });

  // Flush on window blur and when the tab/window is hidden.
  $effect(() => {
    const onBlur = (): void => void flush();
    const onVisibility = (): void => {
      if (document.visibilityState === "hidden") void flush();
    };
    window.addEventListener("blur", onBlur);
    document.addEventListener("visibilitychange", onVisibility);
    return () => {
      window.removeEventListener("blur", onBlur);
      document.removeEventListener("visibilitychange", onVisibility);
    };
  });

  onDestroy(() => {
    clearSaveTimer();
    void teardown();
  });
</script>

<div class="editor-shell">
  {#if loadError}
    <p class="load-error" role="alert">{loadError}</p>
  {/if}
  <div class="editor-host" bind:this={host}></div>
  <span class="save-status" class:visible={status !== "idle"} aria-live="polite">
    {status === "saving" ? "Saving…" : status === "saved" ? "Saved" : ""}
  </span>
</div>

<style>
  .editor-shell {
    position: relative;
    flex: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
  }

  .editor-host {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
  }

  .load-error {
    margin: 0 0 12px;
    padding: 12px;
    border: 1px solid #b91c1c;
    border-radius: 8px;
    font-size: 13px;
    color: #b91c1c;
  }

  .save-status {
    position: absolute;
    right: 12px;
    bottom: 10px;
    padding: 2px 8px;
    border-radius: 6px;
    background-color: var(--code-bg);
    color: var(--text-muted);
    font-size: 11px;
    font-family: var(--font-ui);
    pointer-events: none;
    opacity: 0;
    transition: opacity 0.2s ease;
  }

  .save-status.visible {
    opacity: 0.85;
  }
</style>
