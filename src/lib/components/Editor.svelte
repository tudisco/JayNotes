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
<script lang="ts" module>
  // ---------------------------------------------------------------------------
  // Wikilink decoration plugin (module-scoped: one definition, reused per note)
  //
  // Milkdown Crepe has no wikilink parser, so `[[Note Name]]` stays as literal
  // paragraph text. A raw ProseMirror plugin (registered through Crepe's
  // `editor.use`) scans the doc and wraps every `[[...]]` run in an inline
  // `.jaynotes-wikilink` decoration so it reads as an accent-colored link. A
  // Cmd/Ctrl+Click handler on the host then reads the span's text and navigates
  // (see `onEditorClick`). This is the most robust option Crepe allows without
  // forking its markdown parser, and Cmd+Click is the Obsidian-style trigger.
  // Aliased: Svelte reserves the `$` prefix for local identifiers.
  import { $prose as proseComposable } from "@milkdown/kit/utils";
  import { Plugin, PluginKey, type EditorState } from "@milkdown/kit/prose/state";
  import { Decoration, DecorationSet, type EditorView } from "@milkdown/kit/prose/view";
  import type { Node as ProseNode } from "@milkdown/kit/prose/model";
  import { invoke, convertFileSrc } from "@tauri-apps/api/core";
  import { get } from "svelte/store";
  import { vaultPath, vaultError } from "$lib/stores/vault";
  import { notifyNoteSaved } from "$lib/stores/indexEvents";
  import { isRelativeUrl } from "$lib/utils/url";

  /** A `[[...]]` run that stays on one line and holds no brackets itself. */
  const WIKILINK_RE = /\[\[[^[\]\n]+?\]\]/g;
  const wikilinkKey = new PluginKey<DecorationSet>("jaynotes-wikilink");

  function buildWikilinkDecorations(doc: ProseNode): DecorationSet {
    const decorations: Decoration[] = [];
    doc.descendants((node: ProseNode, pos: number) => {
      if (!node.isText || typeof node.text !== "string") return;
      WIKILINK_RE.lastIndex = 0;
      let m: RegExpExecArray | null;
      while ((m = WIKILINK_RE.exec(node.text)) !== null) {
        const from = pos + m.index;
        decorations.push(
          Decoration.inline(from, from + m[0].length, {
            class: "jaynotes-wikilink",
          }),
        );
      }
    });
    return DecorationSet.create(doc, decorations);
  }

  const wikilinkPlugin = proseComposable(
    () =>
      new Plugin<DecorationSet>({
        key: wikilinkKey,
        state: {
          init: (_config, state) => buildWikilinkDecorations(state.doc),
          apply: (tr, prev) =>
            tr.docChanged ? buildWikilinkDecorations(tr.doc) : prev,
        },
        props: {
          decorations(state: EditorState) {
            return wikilinkKey.getState(state);
          },
        },
      }),
  );

  // ---------------------------------------------------------------------------
  // Local image support: paste / drag save to attachments/, relative render
  //
  // Crepe's ImageBlock feature only routes its upload *button* through
  // `onUpload`, and its bundled clipboard handler ignores image files entirely.
  // So a raw ProseMirror plugin (same `editor.use` path as the wikilink one)
  // catches image paste/drop, saves the bytes as a real file via the
  // `save_attachment` command, and inserts a standard inline image node whose
  // `src` is the vault-relative path — keeping the markdown on disk clean
  // (`![](attachments/…)`, never an asset URL). `proxyDomURL` (below, in the
  // ImageBlock config) rewrites that relative path to a loadable asset URL for
  // DOM display only.

  /** Ensures a clipboard image blob carries a real filename with an extension. */
  function namedImageFile(file: File): File {
    if (file.name && file.name.includes(".")) return file;
    const ext = (file.type.split("/")[1] || "png").toLowerCase();
    return new File([file], `pasted-image.${ext}`, { type: file.type });
  }

  /** Collects image files from a clipboard/drag payload (files first, then items). */
  function extractImageFiles(dt: DataTransfer | null): File[] {
    if (!dt) return [];
    const out: File[] = [];
    for (const f of Array.from(dt.files)) {
      if (f.type.startsWith("image/")) out.push(f);
    }
    if (out.length > 0) return out;
    // Screenshot paste often exposes the blob only via `items`, not `files`.
    for (const item of Array.from(dt.items ?? [])) {
      if (item.kind === "file" && item.type.startsWith("image/")) {
        const f = item.getAsFile();
        if (f) out.push(namedImageFile(f));
      }
    }
    return out;
  }

  /** Saves an image File under the vault's `attachments/`; returns its rel path. */
  async function uploadImage(file: File): Promise<string> {
    const bytes = Array.from(new Uint8Array(await file.arrayBuffer()));
    return invoke<string>("save_attachment", {
      fileName: file.name || "pasted-image.png",
      data: bytes,
    });
  }

  /**
   * DOM display resolver for Crepe's ImageBlock. A vault-relative `src` becomes
   * a `convertFileSrc` asset URL so the webview can load the on-disk file;
   * absolute/scheme URLs (remote https, data:) pass through untouched. This only
   * affects rendering — the stored markdown keeps the relative path.
   */
  function proxyImageURL(url: string): string {
    if (!isRelativeUrl(url)) return url;
    const root = get(vaultPath);
    if (!root) return url;
    let rel = url;
    try {
      rel = decodeURI(url);
    } catch {
      rel = url;
    }
    return convertFileSrc(`${root}/${rel}`);
  }

  /** Saves `file`, then inserts an inline image node (clean `![](rel)` markdown). */
  async function insertImageFile(
    view: EditorView,
    file: File,
    pos: number | null,
  ): Promise<void> {
    let src: string;
    try {
      src = await uploadImage(file);
    } catch (e) {
      vaultError.set(String(e));
      return;
    }
    const imageType = view.state.schema.nodes.image;
    if (!imageType) return;
    const node = imageType.create({ src, alt: "", title: "" });
    const tr =
      pos === null
        ? view.state.tr.replaceSelectionWith(node, false)
        : view.state.tr.insert(pos, node);
    view.dispatch(tr);
  }

  const imageDropPasteKey = new PluginKey("jaynotes-image-drop-paste");

  const imagePastePlugin = proseComposable(
    () =>
      new Plugin({
        key: imageDropPasteKey,
        props: {
          handlePaste(view: EditorView, event: ClipboardEvent) {
            const files = extractImageFiles(event.clipboardData);
            if (files.length === 0) return false;
            event.preventDefault();
            for (const file of files) void insertImageFile(view, file, null);
            return true;
          },
          handleDrop(view: EditorView, event: DragEvent) {
            const files = extractImageFiles(event.dataTransfer);
            if (files.length === 0) return false;
            event.preventDefault();
            const pos =
              view.posAtCoords({ left: event.clientX, top: event.clientY })
                ?.pos ?? null;
            for (const file of files) void insertImageFile(view, file, pos);
            return true;
          },
        },
      }),
  );
</script>

<script lang="ts">
  import { onDestroy } from "svelte";
  import { Crepe } from "@milkdown/crepe";
  import {
    readNote,
    writeNote,
    selected,
    ensureVisible,
  } from "$lib/stores/vault";
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
      notifyNoteSaved();
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
        // `onUpload` (used by the image upload button) and `proxyDomURL` (used
        // for DOM rendering) apply to both the block and inline image variants —
        // Crepe forwards these top-level options to each internally.
        [Crepe.Feature.ImageBlock]: {
          onUpload: uploadImage,
          proxyDomURL: proxyImageURL,
        },
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

    // Register the wikilink decoration + image paste/drop plugins before build.
    instance.editor.use(wikilinkPlugin);
    instance.editor.use(imagePastePlugin);

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

  /**
   * Navigate a `[[wikilink]]`: resolve `name` to an existing note (or create
   * one in the vault root on a miss), then open it. Flushes the current note
   * first so its edits are never lost on the switch.
   */
  async function openWikilink(name: string): Promise<void> {
    try {
      let path = await invoke<string | null>("resolve_note", { name });
      if (!path) {
        path = await invoke<string>("resolve_or_create_note", { name });
      }
      await flush();
      ensureVisible(path);
      selected.set({ path, isDir: false });
    } catch (e) {
      vaultError.set(String(e));
    }
  }

  /** Cmd/Ctrl+Click on a `[[...]]` decoration span opens the linked note. */
  function onEditorClick(event: MouseEvent): void {
    if (!(event.metaKey || event.ctrlKey)) return;
    const target = event.target as HTMLElement | null;
    const span = target?.closest?.(".jaynotes-wikilink") as HTMLElement | null;
    if (!span) return;
    const match = (span.textContent ?? "").match(/\[\[([^[\]\n]+?)\]\]/);
    if (!match) return;
    event.preventDefault();
    event.stopPropagation();
    const name = match[1].split("|")[0].trim();
    if (name) void openWikilink(name);
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
  <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
  <div class="editor-host" bind:this={host} onclick={onEditorClick}></div>
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
    border: 1px solid var(--danger);
    border-radius: 8px;
    font-size: 13px;
    color: var(--danger);
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
