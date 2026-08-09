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
  import type { Node as ProseNode, Schema } from "@milkdown/kit/prose/model";
  import { invoke, convertFileSrc } from "@tauri-apps/api/core";
  import { get } from "svelte/store";
  import { vaultPath, vaultError, activeVault } from "$lib/stores/vault";
  import { notifyNoteSaved } from "$lib/stores/indexEvents";
  import { isRelativeUrl } from "$lib/utils/url";
  import {
    MAX_TEXT_FILE_BYTES,
    decodeTextFile,
    languageForFilename,
    normalizeText,
  } from "$lib/utils/textFile";

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
   * DOM display resolver for Crepe's ImageBlock. A vault-relative `src` is
   * resolved for display only (the stored markdown keeps the relative path);
   * absolute/scheme URLs (remote https, data:) pass through untouched.
   *
   * Plain vaults use a `convertFileSrc` asset URL to the on-disk file. Encrypted
   * vaults can't expose files to the webview, so the bytes come back as a
   * `data:` URI from the backend (Crepe's `proxyDomURL` accepts a Promise).
   */
  async function proxyImageURL(url: string): Promise<string> {
    if (!isRelativeUrl(url)) return url;
    let rel = url;
    try {
      rel = decodeURI(url);
    } catch {
      rel = url;
    }
    if (get(activeVault)?.kind !== "plain") {
      try {
        return await invoke<string>("read_attachment_data_url", {
          relPath: rel,
        });
      } catch {
        return url;
      }
    }
    const root = get(vaultPath);
    if (!root) return url;
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

  // ---------------------------------------------------------------------------
  // Text file support: paste / drag embeds the contents inline
  //
  // A dropped `.json` / `.env` / `README.txt` is read as UTF-8 and its contents
  // go straight into the note as a fenced code block — with a language tag when
  // the type is recognized, bare otherwise (see `utils/textFile.ts`). Inline
  // rather than a saved `attachments/` file so the data stays searchable by the
  // FTS index, greppable, and portable to any other markdown tool — and, in an
  // encrypted vault, encrypted with the note itself. Binary files are refused:
  // images are the only file type saved as a real attachment.

  /** Collects non-image files from a clipboard/drag payload. */
  function extractDataFiles(dt: DataTransfer | null): File[] {
    if (!dt) return [];
    return Array.from(dt.files).filter((f) => !f.type.startsWith("image/"));
  }

  /**
   * Nodes for one embedded file: a `filename` label paragraph, then the
   * contents as a code block tagged with the file's language (if known).
   */
  function buildTextFileNodes(
    schema: Schema,
    fileName: string,
    raw: string,
  ): ProseNode[] {
    const paragraph = schema.nodes.paragraph;
    const codeBlock = schema.nodes.code_block;
    if (!paragraph || !codeBlock) return [];
    const text = normalizeText(raw);
    const codeMark = schema.marks.inlineCode;
    return [
      paragraph.create(
        null,
        schema.text(fileName, codeMark ? [codeMark.create()] : undefined),
      ),
      codeBlock.create(
        { language: languageForFilename(fileName) },
        // `schema.text("")` throws — an empty file gets an empty block.
        text === "" ? null : schema.text(text),
      ),
    ];
  }

  /** Reads one file as text, or reports why it can't be embedded. */
  async function readTextFileNodes(
    schema: Schema,
    file: File,
  ): Promise<ProseNode[] | null> {
    const name = file.name || "pasted-file";
    if (file.size > MAX_TEXT_FILE_BYTES) {
      vaultError.set(
        `'${name}' is too large to embed in a note (${Math.round(file.size / 1024)}KB, limit ${MAX_TEXT_FILE_BYTES / 1024}KB)`,
      );
      return null;
    }
    let text: string | null;
    try {
      text = decodeTextFile(new Uint8Array(await file.arrayBuffer()));
    } catch (e) {
      vaultError.set(`Could not read '${name}': ${e}`);
      return null;
    }
    if (text === null) {
      vaultError.set(
        `'${name}' isn't a text file — only images and text files can be added to a note`,
      );
      return null;
    }
    return buildTextFileNodes(schema, name, text);
  }

  /**
   * Embeds every readable file in one transaction, so a multi-file drop lands
   * in the order it was dropped instead of racing on a shared position.
   */
  async function insertTextFiles(
    view: EditorView,
    files: File[],
    pos: number | null,
  ): Promise<void> {
    const nodes: ProseNode[] = [];
    for (const file of files) {
      const built = await readTextFileNodes(view.state.schema, file);
      if (built) nodes.push(...built);
    }
    if (nodes.length === 0) return;
    const { from, to } = view.state.selection;
    const tr =
      pos === null
        ? view.state.tr.replaceWith(from, to, nodes)
        : view.state.tr.insert(pos, nodes);
    view.dispatch(tr);
  }

  const imageDropPasteKey = new PluginKey("jaynotes-image-drop-paste");

  const imagePastePlugin = proseComposable(
    () =>
      new Plugin({
        key: imageDropPasteKey,
        props: {
          handlePaste(view: EditorView, event: ClipboardEvent) {
            const images = extractImageFiles(event.clipboardData);
            const texts = extractDataFiles(event.clipboardData);
            if (images.length === 0 && texts.length === 0) return false;
            event.preventDefault();
            for (const file of images) void insertImageFile(view, file, null);
            if (texts.length > 0) void insertTextFiles(view, texts, null);
            return true;
          },
          handleDrop(view: EditorView, event: DragEvent) {
            const images = extractImageFiles(event.dataTransfer);
            const texts = extractDataFiles(event.dataTransfer);
            if (images.length === 0 && texts.length === 0) return false;
            event.preventDefault();
            const pos =
              view.posAtCoords({ left: event.clientX, top: event.clientY })
                ?.pos ?? null;
            for (const file of images) void insertImageFile(view, file, pos);
            if (texts.length > 0) void insertTextFiles(view, texts, pos);
            return true;
          },
        },
      }),
  );
</script>

<script lang="ts">
  import { onDestroy } from "svelte";
  import { Crepe } from "@milkdown/crepe";
  import { editorViewCtx } from "@milkdown/kit/core";
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

  /**
   * Persist the current editor content if it differs from what's on disk.
   * Exported so the AI chat can flush the open note before the model reads it.
   */
  export async function flush(): Promise<void> {
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

  /** The live ProseMirror view, or `null` before the editor finishes building. */
  function currentView(): EditorView | null {
    if (!crepe) return null;
    try {
      return crepe.editor.ctx.get(editorViewCtx);
    } catch {
      return null;
    }
  }

  // ---------------------------------------------------------------------------
  // File drops from Finder
  //
  // `dragDropEnabled: false` in tauri.conf.json hands drag/drop to the webview,
  // so a Finder drop arrives as a normal DOM DragEvent — but it does not
  // reliably reach ProseMirror's `handleDrop` prop, which only runs for drops
  // landing inside the contenteditable and behind Crepe's plugin stack. A
  // capture-phase listener on the host element runs first and unconditionally,
  // so it catches drops anywhere in the editor pane. The plugin's `handleDrop`
  // stays for in-app drags; this handler only claims events carrying files.
  $effect(() => {
    const el = host;
    if (!el) return;

    const carriesFiles = (dt: DataTransfer | null): boolean =>
      !!dt && Array.from(dt.types).includes("Files");

    // The drop event only fires if dragover is canceled.
    const onDragOver = (event: DragEvent): void => {
      if (!carriesFiles(event.dataTransfer)) return;
      event.preventDefault();
      if (event.dataTransfer) event.dataTransfer.dropEffect = "copy";
    };

    const onDrop = (event: DragEvent): void => {
      const dt = event.dataTransfer;
      if (!carriesFiles(dt)) return;
      event.preventDefault();
      event.stopPropagation();
      const view = currentView();
      if (!view) return;
      const images = extractImageFiles(dt);
      const texts = extractDataFiles(dt);
      if (images.length === 0 && texts.length === 0) {
        // The drag advertised files but exposed none — surface it rather than
        // silently doing nothing, since there's no console in a release build.
        vaultError.set(
          `Could not read the dropped file (drag types: ${Array.from(dt?.types ?? []).join(", ") || "none"})`,
        );
        return;
      }
      const pos =
        view.posAtCoords({ left: event.clientX, top: event.clientY })?.pos ??
        null;
      for (const file of images) void insertImageFile(view, file, pos);
      if (texts.length > 0) void insertTextFiles(view, texts, pos);
    };

    el.addEventListener("dragover", onDragOver, true);
    el.addEventListener("drop", onDrop, true);
    return () => {
      el.removeEventListener("dragover", onDragOver, true);
      el.removeEventListener("drop", onDrop, true);
    };
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
