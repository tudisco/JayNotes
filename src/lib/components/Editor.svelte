<!--
  Editor.svelte — Milkdown Crepe live-preview editor for a single note.

  Given a `path` (vault-relative), it loads the note, strips any frontmatter
  block (kept verbatim in memory), and mounts Crepe on the body. Edits are
  autosaved with a 600ms debounce; a save is also flushed immediately on note
  switch, component teardown, window blur, and tab hide.

  Crepe has no cheap "set markdown" API, so switching notes tears the editor
  down and recreates it — simple and reliable.
-->
<script lang="ts">
  import { onDestroy } from "svelte";
  import { Crepe } from "@milkdown/crepe";
  import { readNote, writeNote, vaultError } from "$lib/stores/vault";
  import { joinFrontmatter, splitFrontmatter } from "$lib/utils/frontmatter";

  let { path }: { path: string } = $props();

  const SAVE_DEBOUNCE_MS = 600;

  let host: HTMLDivElement;
  let crepe: Crepe | null = null;

  /** Verbatim frontmatter of the currently loaded note (null when none). */
  let frontmatter: string | null = null;
  /** Path of the note currently mounted in the editor. */
  let currentPath: string | null = null;
  /** Body content as last persisted to disk (Crepe-serialized form). */
  let lastSavedBody = "";
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
    if (body === lastSavedBody) {
      status = "saved";
      return;
    }
    const target = currentPath;
    try {
      await writeNote(target, joinFrontmatter(frontmatter, body));
      lastSavedBody = body;
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
