<script lang="ts">
  import { onMount } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import { revealItemInDir } from "@tauri-apps/plugin-opener";
  import Editor from "./Editor.svelte";
  import PropertiesBar from "./PropertiesBar.svelte";
  import {
    renameNote,
    moveNote,
    deleteToTrash,
    selected,
    fileTree,
    vaultError,
    vaultLocked,
    activeVault,
    unlockVault,
    providers,
    vaults,
    activeVaultId,
    ensureVisible,
    transferNote,
    listVaultFolders,
    unlockTransferDest,
    type VaultInfo,
  } from "$lib/stores/vault";
  import { collectFolderPaths } from "$lib/utils/path";
  import { sidebarMode, filesView } from "$lib/stores/ui";
  import { vaultChanged } from "$lib/stores/indexEvents";
  import {
    editorReloadNonce,
    registerEditorFlush,
  } from "$lib/stores/editorBridge";

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

  // The AI writes through a suppressed self-write path, so the watcher stays
  // quiet; the chat bumps this nonce to reload the open note after an edit or
  // revert. Same safety rule: never reload over unsaved edits.
  let lastAiReload = 0;
  $effect(() => {
    const n = $editorReloadNonce;
    if (n === lastAiReload) return;
    lastAiReload = n;
    if (notePath && editor && !editor.isDirty()) {
      reloadNonce += 1;
    }
  });

  // Let the AI chat flush the open note before the model reads it from disk.
  onMount(() => registerEditorFlush(async () => { await editor?.flush(); }));

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

  // "Move to Trash": a small inline confirm popover (no native alert) anchored
  // to the trashcan button. Confirming trashes the open note through the shared
  // store helper, which clears the selection (editor returns to "No note open")
  // and refreshes the tree; errors surface via the usual vaultError path.
  let confirmingTrash = $state(false);

  // "Move to folder…": a small folder-picker popover anchored under the move
  // button. Lists "(vault root)" plus every folder in the vault; picking one
  // moves the open note there (filename kept) via the store's move helper.
  let pickingMove = $state(false);
  let moveFilter = $state("");
  let moveActiveIndex = $state(0);

  // The move popover is a small step machine: "folders" is the in-vault folder
  // list plus an "Other vaults" section (Job A); choosing another vault steps to
  // "vault-folders" (that vault's folder list, or an inline unlock when it's
  // locked), and choosing a folder there steps to "confirm".
  type MoveView = "folders" | "vault-folders" | "confirm";
  let moveView = $state<MoveView>("folders");
  let transferTarget = $state<VaultInfo | null>(null);
  let transferFolders = $state<string[]>([]);
  let transferFoldersLoading = $state(false);
  let transferError = $state("");
  let transferDestFolder = $state<string | null>(null);
  let transferBusy = $state(false);
  // When set, the target vault is locked and this row shows an inline unlock.
  let unlockingTargetId = $state<string | null>(null);
  let tUnlockPw = $state("");
  let tUnlockPw2 = $state("");
  let tUnlockRemember = $state(false);
  let tUnlockError = $state("");
  let tUnlocking = $state(false);
  // Transient "Moved to <vault>" notice shown after the note leaves this vault.
  let movedNotice = $state<string | null>(null);

  // Kind glyphs for the "Other vaults" rows (mirrors the unlock-pane icons).
  const kindGlyph: Record<string, string> = {
    plain: "📁",
    "encrypted-db": "🔒",
    "encrypted-files": "🗄️",
    tinylord: "🌐",
  };

  interface OtherVault {
    vault: VaultInfo;
    glyph: string;
    disabled: boolean;
    reason: string; // shown dimmed when disabled
  }

  // Every configured vault except the active (source) one, with a readiness
  // verdict. Encrypted vaults aren't pre-checked for unlock here — clicking one
  // tries to open it and falls into the inline unlock only if it reports locked.
  let otherVaults = $derived.by<OtherVault[]>(() =>
    $vaults
      .filter((v) => v.id !== $activeVaultId)
      .map((v) => {
        let disabled = false;
        let reason = "";
        if (v.status === "offline") {
          disabled = true;
          reason = "drive offline";
        } else if (v.status === "unsupported") {
          disabled = true;
          reason = "unsupported build";
        } else if (v.kind === "tinylord") {
          disabled = true;
          reason = "hosted — can't receive";
        }
        return { vault: v, glyph: kindGlyph[v.kind] ?? "📁", disabled, reason };
      }),
  );

  // The plaintext-Trash warning applies when moving OUT of a plain vault INTO an
  // encrypted one: the OS Trash copy of the original stays readable in the clear.
  let showPlaintextWarning = $derived(
    $activeVault?.kind === "plain" &&
      (transferTarget?.kind === "encrypted-db" ||
        transferTarget?.kind === "encrypted-files"),
  );

  function resetTransferState(): void {
    moveView = "folders";
    transferTarget = null;
    transferFolders = [];
    transferFoldersLoading = false;
    transferError = "";
    transferDestFolder = null;
    transferBusy = false;
    unlockingTargetId = null;
    tUnlockPw = "";
    tUnlockPw2 = "";
    tUnlockError = "";
    tUnlocking = false;
  }

  async function chooseTargetVault(v: VaultInfo): Promise<void> {
    transferTarget = v;
    transferError = "";
    transferFolders = [];
    unlockingTargetId = null;
    moveView = "vault-folders";
    await loadTargetFolders();
  }

  async function loadTargetFolders(): Promise<void> {
    if (!transferTarget) return;
    transferFoldersLoading = true;
    transferError = "";
    try {
      transferFolders = await listVaultFolders(transferTarget.id);
      unlockingTargetId = null;
    } catch (e) {
      const msg = String(e);
      if (msg.includes("dest-locked")) {
        unlockingTargetId = transferTarget.id; // show the inline unlock
      } else {
        transferError = msg;
      }
    } finally {
      transferFoldersLoading = false;
    }
  }

  async function submitTargetUnlock(event: SubmitEvent): Promise<void> {
    event.preventDefault();
    const v = transferTarget;
    if (!v || tUnlocking || !tUnlockPw) return;
    tUnlocking = true;
    tUnlockError = "";
    try {
      const extra =
        v.kind === "encrypted-files" && tUnlockPw2
          ? { password2: tUnlockPw2 }
          : undefined;
      await unlockTransferDest(v.id, tUnlockPw, tUnlockRemember, extra);
      tUnlockPw = "";
      tUnlockPw2 = "";
      await loadTargetFolders(); // now unlocked → its folders
    } catch (e) {
      tUnlockError = String(e);
    } finally {
      tUnlocking = false;
    }
  }

  function chooseTargetFolder(folder: string): void {
    transferDestFolder = folder;
    moveView = "confirm";
  }

  function backToVaultList(): void {
    moveView = "folders";
    transferTarget = null;
    transferError = "";
    unlockingTargetId = null;
  }

  async function doTransfer(): Promise<void> {
    const p = notePath;
    const v = transferTarget;
    if (!p || !v || transferDestFolder === null || transferBusy) return;
    transferBusy = true;
    transferError = "";
    try {
      // Flush pending edits before the note leaves this vault (same rule the
      // in-vault move follows) so a debounced autosave can't race the transfer.
      await editor?.flush();
      await transferNote(p, v.id, transferDestFolder);
      pickingMove = false;
      resetTransferState();
      showMovedNotice(v.name);
    } catch (e) {
      const msg = String(e);
      if (msg.includes("dest-locked")) {
        // The key expired between steps — drop back to the unlock prompt.
        unlockingTargetId = v.id;
        moveView = "vault-folders";
      } else {
        transferError = msg;
      }
    } finally {
      transferBusy = false;
    }
  }

  function showMovedNotice(vaultName: string): void {
    movedNotice = `Moved to ${vaultName}`;
    setTimeout(() => {
      if (movedNotice === `Moved to ${vaultName}`) movedNotice = null;
    }, 2800);
  }

  // Job B: reveal the open note in the file tree — switch the sidebar to Files
  // (tree mode) and expand its ancestor folders. Cheap; the note is already the
  // selection so no scroll bookkeeping is needed.
  function revealInTree(): void {
    const p = notePath;
    if (!p) return;
    sidebarMode.set("files");
    filesView.set("tree");
    ensureVisible(p);
    selected.set({ path: p, isDir: false });
  }

  function parentDir(path: string): string {
    const i = path.lastIndexOf("/");
    return i === -1 ? "" : path.slice(0, i);
  }

  interface MoveOption {
    path: string; // "" = vault root
    label: string; // "(vault root)" or a slash-separated folder path
    depth: number; // indentation level
    disabled: boolean; // the note's current folder
  }

  // The folder the open note currently lives in — marked/disabled in the list.
  let currentFolder = $derived(notePath ? parentDir(notePath) : "");

  // All destinations: root first, then every folder (filtered when searching).
  let moveOptions = $derived.by<MoveOption[]>(() => {
    const folders = collectFolderPaths($fileTree);
    const q = moveFilter.trim().toLowerCase();
    const matched = q
      ? folders.filter((p) => p.toLowerCase().includes(q))
      : folders;
    const opts: MoveOption[] = [
      {
        path: "",
        label: "(vault root)",
        depth: 0,
        disabled: currentFolder === "",
      },
    ];
    for (const p of matched) {
      opts.push({
        path: p,
        label: p,
        depth: p.split("/").length,
        disabled: p === currentFolder,
      });
    }
    return opts;
  });

  // Show a filter box only once the vault has enough folders to warrant it.
  let showMoveFilter = $derived(collectFolderPaths($fileTree).length > 10);

  // Keep the highlighted row in range as the filtered list shrinks/grows.
  $effect(() => {
    const n = moveOptions.length;
    if (moveActiveIndex > n - 1) moveActiveIndex = Math.max(0, n - 1);
  });

  function openMovePicker(): void {
    confirmingTrash = false; // one popover at a time
    moveFilter = "";
    moveActiveIndex = 0;
    resetTransferState();
    pickingMove = true;
  }

  function closeMovePicker(): void {
    pickingMove = false;
    resetTransferState();
  }

  // Reset both popovers whenever the open note changes.
  $effect(() => {
    void notePath;
    confirmingTrash = false;
    pickingMove = false;
    resetTransferState();
  });

  async function moveTo(dest: string): Promise<void> {
    const p = notePath;
    pickingMove = false;
    if (!p) return;
    try {
      // Flush pending edits BEFORE the rename so a debounced autosave can't
      // race the move and write to the note's old path after it has moved.
      await editor?.flush();
      await moveNote(p, dest);
    } catch (e) {
      // Collision (a note with that name already exists in the destination) or
      // any IO error: surface it and leave the note where it was.
      vaultError.set(String(e));
    }
  }

  function onMoveKeydown(event: KeyboardEvent): void {
    if (!pickingMove) return;
    if (event.key === "ArrowDown") {
      event.preventDefault();
      moveActiveIndex = Math.min(moveActiveIndex + 1, moveOptions.length - 1);
    } else if (event.key === "ArrowUp") {
      event.preventDefault();
      moveActiveIndex = Math.max(moveActiveIndex - 1, 0);
    } else if (event.key === "Enter") {
      event.preventDefault();
      const opt = moveOptions[moveActiveIndex];
      if (opt && !opt.disabled) void moveTo(opt.path);
    }
  }

  async function trashNote(): Promise<void> {
    const p = notePath;
    confirmingTrash = false;
    if (!p) return;
    try {
      await deleteToTrash({ name: baseName(p), path: p, isDir: false, children: [] });
    } catch (e) {
      vaultError.set(String(e));
    }
  }

  // ---- locked-vault unlock prompt (shown in the main pane) ----
  let unlockPassword = $state("");
  let unlockPassword2 = $state("");
  let unlockRemember = $state(false);
  let unlockError = $state("");
  let unlocking = $state(false);

  // Provider metadata drives the panel copy: a hosted vault's unlock is a
  // login, so its metadata carries unlockLabel "Sign in"; encrypted vaults
  // fall back to "Unlock".
  let unlockMeta = $derived(
    $providers.find((p) => p.kind === $activeVault?.kind) ?? null,
  );
  let unlockLabel = $derived(unlockMeta?.unlockLabel ?? "Unlock");
  let unlockBusyLabel = $derived(
    unlockMeta?.unlockLabel ? "Signing in…" : "Unlocking…",
  );
  let unlockHint = $derived(
    $activeVault?.kind === "tinylord"
      ? "Sign in to your TinyLord server to open this vault."
      : "Enter the password to open this encrypted vault.",
  );

  // Clear the prompt whenever the active (locked) vault changes.
  $effect(() => {
    void $activeVault?.id;
    unlockPassword = "";
    unlockPassword2 = "";
    unlockError = "";
  });

  async function submitUnlock(event: SubmitEvent): Promise<void> {
    event.preventDefault();
    const v = $activeVault;
    if (!v || unlocking || !unlockPassword) return;
    unlocking = true;
    unlockError = "";
    try {
      // encrypted-files unlock also needs the rclone salt/second password to
      // re-derive the same keys; other encrypted kinds ignore `extra`.
      const extra =
        v.kind === "encrypted-files" && unlockPassword2
          ? { password2: unlockPassword2 }
          : undefined;
      await unlockVault(v.id, unlockPassword, unlockRemember, extra);
      unlockPassword = "";
      unlockPassword2 = "";
    } catch (e) {
      unlockError = String(e);
    } finally {
      unlocking = false;
    }
  }

  function onWindowKeydown(event: KeyboardEvent): void {
    if (event.key === "Escape" && confirmingTrash) {
      confirmingTrash = false;
    }
    if (pickingMove) {
      if (event.key === "Escape") {
        // Escape steps back one level, then closes from the vault list.
        if (moveView === "confirm") {
          moveView = "vault-folders";
        } else if (moveView === "vault-folders") {
          backToVaultList();
        } else {
          closeMovePicker();
        }
      } else if (moveView === "folders") {
        onMoveKeydown(event);
      }
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

<svelte:window onkeydown={onWindowKeydown} />

<section class="editor-pane" class:has-note={fileSelected}>
  {#if movedNotice}
    <div class="moved-notice" role="status">{movedNotice}</div>
  {/if}
  {#if fileSelected && notePath}
    <div class="note-view">
      {#key `${notePath}:${reloadNonce}`}
        <div class="note-meta">
          <header class="note-header">
            <div class="note-title-wrap">
              {#if currentFolder}
                <button
                  type="button"
                  class="note-path"
                  title="Reveal in file tree"
                  onclick={revealInTree}
                >
                  {#each currentFolder.split("/") as seg, i (i)}
                    {#if i > 0}<span class="note-path-sep">/</span>{/if}<span
                      class="note-path-seg">{seg}</span
                    >
                  {/each}
                </button>
              {/if}
              <input
                class="note-title"
                type="text"
                bind:value={titleDraft}
                spellcheck="false"
                aria-label="Note title"
                onkeydown={onTitleKey}
                onblur={commitTitle}
              />
            </div>
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
              <div class="move-wrap">
                <button
                  type="button"
                  class="icon-button"
                  title="Move to folder…"
                  aria-label="Move to folder"
                  aria-haspopup="menu"
                  aria-expanded={pickingMove}
                  onclick={() => (pickingMove ? closeMovePicker() : openMovePicker())}
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
                    <path d="M4 20a2 2 0 0 1-2-2V6a2 2 0 0 1 2-2h5l2 3h7a2 2 0 0 1 2 2v3" />
                    <path d="M14 17h7" />
                    <path d="M18 14l3 3-3 3" />
                  </svg>
                </button>
                {#if pickingMove}
                  <!-- Click-away backdrop + Escape both close the picker. -->
                  <button
                    type="button"
                    class="popover-backdrop"
                    aria-label="Cancel move to folder"
                    onclick={closeMovePicker}
                  ></button>
                  <div class="move-picker" role="menu" aria-label="Move note">
                    {#if moveView === "folders"}
                      {#if showMoveFilter}
                        <!-- svelte-ignore a11y_autofocus -->
                        <input
                          class="move-filter"
                          type="text"
                          placeholder="Filter folders…"
                          autocomplete="off"
                          spellcheck="false"
                          autofocus
                          bind:value={moveFilter}
                          oninput={() => (moveActiveIndex = 0)}
                        />
                      {/if}
                      <ul class="move-list">
                        {#each moveOptions as opt, i (opt.path)}
                          <li>
                            <button
                              type="button"
                              class="move-item"
                              class:active={i === moveActiveIndex}
                              class:current={opt.disabled}
                              role="menuitem"
                              disabled={opt.disabled}
                              style:padding-left="{8 + opt.depth * 12}px"
                              onmouseenter={() => (moveActiveIndex = i)}
                              onclick={() => moveTo(opt.path)}
                            >
                              <span class="move-item-label">{opt.label}</span>
                              {#if opt.disabled}
                                <span class="move-item-hint">current</span>
                              {/if}
                            </button>
                          </li>
                        {:else}
                          <li class="move-empty">No folders match</li>
                        {/each}
                      </ul>

                      {#if otherVaults.length > 0}
                        <div class="move-sep" role="separator"></div>
                        <p class="move-section-label">Other vaults</p>
                        <ul class="move-list">
                          {#each otherVaults as ov (ov.vault.id)}
                            <li>
                              <button
                                type="button"
                                class="move-item vault-row"
                                role="menuitem"
                                disabled={ov.disabled}
                                onclick={() => chooseTargetVault(ov.vault)}
                              >
                                <span class="vault-glyph" aria-hidden="true"
                                  >{ov.glyph}</span
                                >
                                <span class="move-item-label">{ov.vault.name}</span>
                                {#if ov.disabled}
                                  <span class="move-item-hint">{ov.reason}</span>
                                {:else}
                                  <span class="vault-chevron" aria-hidden="true"
                                    >›</span
                                  >
                                {/if}
                              </button>
                            </li>
                          {/each}
                        </ul>
                      {/if}
                    {:else if moveView === "vault-folders"}
                      <div class="move-subhead">
                        <button
                          type="button"
                          class="back-btn"
                          aria-label="Back to vault list"
                          onclick={backToVaultList}>‹</button
                        >
                        <span class="move-subhead-title"
                          >{transferTarget?.name}</span
                        >
                      </div>
                      {#if unlockingTargetId}
                        <form class="t-unlock" onsubmit={submitTargetUnlock}>
                          <p class="t-unlock-hint">Unlock this vault first.</p>
                          <!-- svelte-ignore a11y_autofocus -->
                          <input
                            class="move-filter"
                            type="password"
                            placeholder="Password"
                            autocomplete="off"
                            autofocus
                            bind:value={tUnlockPw}
                          />
                          {#if transferTarget?.kind === "encrypted-files"}
                            <input
                              class="move-filter"
                              type="text"
                              placeholder="Salt / second password (optional)"
                              autocomplete="off"
                              bind:value={tUnlockPw2}
                            />
                          {/if}
                          <label class="t-remember">
                            <input type="checkbox" bind:checked={tUnlockRemember} />
                            Remember
                          </label>
                          {#if tUnlockError}
                            <p class="t-error">{tUnlockError}</p>
                          {/if}
                          <button
                            type="submit"
                            class="confirm-btn primary"
                            disabled={tUnlocking || !tUnlockPw}
                          >
                            {tUnlocking ? "Unlocking…" : "Unlock"}
                          </button>
                        </form>
                      {:else if transferFoldersLoading}
                        <p class="move-empty">Loading…</p>
                      {:else if transferError}
                        <p class="t-error">{transferError}</p>
                      {:else}
                        <ul class="move-list">
                          <li>
                            <button
                              type="button"
                              class="move-item"
                              role="menuitem"
                              onclick={() => chooseTargetFolder("")}
                            >
                              <span class="move-item-label">(vault root)</span>
                            </button>
                          </li>
                          {#each transferFolders as f (f)}
                            <li>
                              <button
                                type="button"
                                class="move-item"
                                role="menuitem"
                                style:padding-left="{8 +
                                  f.split('/').length * 12}px"
                                onclick={() => chooseTargetFolder(f)}
                              >
                                <span class="move-item-label">{f}</span>
                              </button>
                            </li>
                          {/each}
                        </ul>
                      {/if}
                    {:else if moveView === "confirm"}
                      <div class="move-subhead">
                        <button
                          type="button"
                          class="back-btn"
                          aria-label="Back to folder list"
                          onclick={() => (moveView = "vault-folders")}>‹</button
                        >
                        <span class="move-subhead-title">Confirm move</span>
                      </div>
                      <div class="t-confirm">
                        <p class="t-confirm-text">
                          Move to <strong>{transferTarget?.name}</strong>? The
                          original will be moved to this vault's trash.
                        </p>
                        {#if transferDestFolder}
                          <p class="t-dest">Into: {transferDestFolder}</p>
                        {/if}
                        {#if showPlaintextWarning}
                          <p class="t-warn">
                            Original remains in the OS Trash in plain text — empty
                            the Trash to fully remove it.
                          </p>
                        {/if}
                        {#if transferError}
                          <p class="t-error">{transferError}</p>
                        {/if}
                        <div class="trash-confirm-actions">
                          <button
                            type="button"
                            class="confirm-btn primary"
                            disabled={transferBusy}
                            onclick={doTransfer}
                          >
                            {transferBusy ? "Moving…" : "Move"}
                          </button>
                          <button
                            type="button"
                            class="confirm-btn"
                            disabled={transferBusy}
                            onclick={() => (moveView = "vault-folders")}
                          >
                            Cancel
                          </button>
                        </div>
                      </div>
                    {/if}
                  </div>
                {/if}
              </div>
              <div class="trash-wrap">
                <button
                  type="button"
                  class="icon-button"
                  title="Move to Trash"
                  aria-label="Move to Trash"
                  onclick={() => (confirmingTrash = !confirmingTrash)}
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
                    <polyline points="3 6 5 6 21 6" />
                    <path d="M19 6v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6m3 0V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2" />
                    <line x1="10" y1="11" x2="10" y2="17" />
                    <line x1="14" y1="11" x2="14" y2="17" />
                  </svg>
                </button>
                {#if confirmingTrash}
                  <!-- Click-away backdrop + Escape both cancel the confirm. -->
                  <button
                    type="button"
                    class="trash-backdrop"
                    aria-label="Cancel move to Trash"
                    onclick={() => (confirmingTrash = false)}
                  ></button>
                  <div class="trash-confirm" role="dialog" aria-label="Move to Trash">
                    <p class="trash-confirm-text">Move to Trash?</p>
                    <div class="trash-confirm-actions">
                      <button
                        type="button"
                        class="confirm-btn danger"
                        onclick={trashNote}
                      >
                        Trash
                      </button>
                      <button
                        type="button"
                        class="confirm-btn"
                        onclick={() => (confirmingTrash = false)}
                      >
                        Cancel
                      </button>
                    </div>
                  </div>
                {/if}
              </div>
            </div>
          </header>
          <PropertiesBar {frontmatter} onChange={onPropertiesChange} />
        </div>
        <Editor bind:this={editor} path={notePath} bind:frontmatter />
      {/key}
    </div>
  {:else if $vaultLocked && $activeVault}
    <div class="unlock-pane">
      <div class="lock-icon" aria-hidden="true">
        {$activeVault.kind === "tinylord" ? "🌐" : "🔒"}
      </div>
      <p class="unlock-title">{$activeVault.name} is locked</p>
      <p class="unlock-hint">{unlockHint}</p>
      <form class="unlock-form" onsubmit={submitUnlock}>
        <!-- svelte-ignore a11y_autofocus -->
        <input
          class="unlock-input"
          type="password"
          autofocus
          autocomplete="off"
          placeholder="Password"
          bind:value={unlockPassword}
        />
        {#if $activeVault.kind === "encrypted-files"}
          <input
            class="unlock-input"
            type="text"
            autocomplete="off"
            placeholder="Salt / second password (optional)"
            bind:value={unlockPassword2}
          />
        {/if}
        <label class="unlock-remember">
          <input type="checkbox" bind:checked={unlockRemember} />
          Remember password
        </label>
        {#if unlockError}
          <p class="unlock-error">{unlockError}</p>
        {/if}
        <button
          class="unlock-button"
          type="submit"
          disabled={unlocking || !unlockPassword}
        >
          {unlocking ? unlockBusyLabel : unlockLabel}
        </button>
      </form>
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
    position: relative;
    flex: 1;
    height: 100%;
    display: flex;
    align-items: center;
    justify-content: center;
    background-color: var(--bg-panel);
    overflow: hidden;
  }

  /* Transient "Moved to <vault>" confirmation after a cross-vault transfer. */
  .moved-notice {
    position: absolute;
    top: 12px;
    left: 50%;
    transform: translateX(-50%);
    z-index: 1200;
    padding: 6px 12px;
    border: 1px solid var(--border);
    border-radius: 999px;
    background-color: var(--bg-panel);
    color: var(--text);
    font-size: 12px;
    font-family: var(--font-ui);
    box-shadow: var(--shadow-menu);
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

  /* Keep the trashcan visible while its confirm popover is open. */
  .trash-wrap {
    position: relative;
    display: flex;
  }

  .trash-wrap:has(.trash-confirm) .icon-button {
    opacity: 1;
  }

  .trash-backdrop {
    position: fixed;
    inset: 0;
    z-index: 999;
    border: none;
    background: transparent;
    cursor: default;
  }

  .trash-confirm {
    position: absolute;
    top: calc(100% + 6px);
    right: 0;
    z-index: 1000;
    width: 168px;
    padding: 10px;
    border: 1px solid var(--border);
    border-radius: 8px;
    background-color: var(--bg-panel);
    box-shadow: var(--shadow-menu);
  }

  .trash-confirm-text {
    margin: 0 0 8px;
    font-size: 13px;
    color: var(--text);
  }

  .trash-confirm-actions {
    display: flex;
    gap: 6px;
  }

  .confirm-btn {
    flex: 1;
    padding: 5px 8px;
    border: 1px solid var(--border);
    border-radius: 5px;
    background-color: var(--bg-panel);
    color: var(--text);
    font-size: 12px;
    font-family: var(--font-ui);
    cursor: pointer;
  }

  .confirm-btn:hover {
    background-color: var(--hover);
  }

  .confirm-btn.danger {
    border-color: var(--danger);
    color: var(--danger);
  }

  .confirm-btn.danger:hover {
    background-color: var(--danger);
    color: var(--danger-contrast);
  }

  /* ---- Move-to-folder picker (mirrors the trash-confirm popover) ---- */
  .move-wrap {
    position: relative;
    display: flex;
  }

  .move-wrap:has(.move-picker) .icon-button {
    opacity: 1;
  }

  .popover-backdrop {
    position: fixed;
    inset: 0;
    z-index: 999;
    border: none;
    background: transparent;
    cursor: default;
  }

  .move-picker {
    position: absolute;
    top: calc(100% + 6px);
    right: 0;
    z-index: 1000;
    width: 240px;
    padding: 6px;
    border: 1px solid var(--border);
    border-radius: 8px;
    background-color: var(--bg-panel);
    box-shadow: var(--shadow-menu);
  }

  .move-filter {
    width: 100%;
    margin-bottom: 6px;
    padding: 6px 8px;
    border: 1px solid var(--border);
    border-radius: 6px;
    background-color: var(--bg-input, var(--bg-panel));
    color: var(--text);
    font-size: 13px;
    font-family: var(--font-ui);
    box-sizing: border-box;
  }

  .move-filter:focus {
    outline: none;
    border-color: var(--accent);
  }

  .move-list {
    list-style: none;
    margin: 0;
    padding: 0;
    max-height: 260px;
    overflow-y: auto;
  }

  .move-item {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: 8px;
    width: 100%;
    padding: 6px 8px;
    border: none;
    border-radius: 5px;
    background: transparent;
    color: var(--text);
    font-size: 13px;
    font-family: var(--font-ui);
    text-align: left;
    cursor: pointer;
  }

  .move-item-label {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .move-item.active:not(:disabled) {
    background-color: var(--accent);
    color: var(--accent-contrast);
  }

  .move-item:disabled,
  .move-item.current {
    color: var(--text-muted);
    cursor: default;
  }

  .move-item-hint {
    flex-shrink: 0;
    font-size: 11px;
    color: var(--text-muted);
  }

  .move-empty {
    padding: 8px;
    font-size: 12px;
    color: var(--text-muted);
    text-align: center;
  }

  /* ---- Transfer to another vault (steps inside the move popover) ---- */
  .move-sep {
    height: 1px;
    margin: 6px 4px;
    background-color: var(--border);
  }

  .move-section-label {
    margin: 2px 0 4px;
    padding: 0 8px;
    font-size: 11px;
    font-weight: 600;
    letter-spacing: 0.03em;
    text-transform: uppercase;
    color: var(--text-muted);
  }

  .vault-glyph {
    flex-shrink: 0;
    font-size: 13px;
    line-height: 1;
  }

  .vault-row {
    gap: 8px;
  }

  .vault-chevron {
    flex-shrink: 0;
    color: var(--text-muted);
    font-size: 15px;
  }

  .move-subhead {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 2px 4px 6px;
  }

  .back-btn {
    display: flex;
    align-items: center;
    justify-content: center;
    width: 22px;
    height: 22px;
    padding: 0;
    border: none;
    border-radius: 5px;
    background: transparent;
    color: var(--text-muted);
    font-size: 18px;
    line-height: 1;
    cursor: pointer;
  }

  .back-btn:hover {
    background-color: var(--hover);
    color: var(--text);
  }

  .move-subhead-title {
    font-size: 12px;
    font-weight: 600;
    color: var(--text);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .t-unlock {
    display: flex;
    flex-direction: column;
    gap: 8px;
    padding: 4px;
  }

  .t-unlock-hint {
    margin: 0;
    font-size: 12px;
    color: var(--text-muted);
  }

  .t-remember {
    display: flex;
    align-items: center;
    gap: 6px;
    font-size: 12px;
    color: var(--text-muted);
    cursor: pointer;
  }

  .t-error {
    margin: 0;
    padding: 4px 4px 0;
    font-size: 12px;
    color: var(--danger);
  }

  .t-confirm {
    padding: 4px;
  }

  .t-confirm-text {
    margin: 0 0 6px;
    font-size: 13px;
    color: var(--text);
    line-height: 1.4;
  }

  .t-dest {
    margin: 0 0 6px;
    font-size: 12px;
    color: var(--text-muted);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .t-warn {
    margin: 0 0 8px;
    font-size: 11px;
    color: var(--text-muted);
    line-height: 1.4;
  }

  .confirm-btn.primary {
    border-color: var(--accent);
    background-color: var(--accent);
    color: var(--accent-contrast);
  }

  .confirm-btn.primary:hover:not(:disabled) {
    background-color: var(--accent-hover);
  }

  .confirm-btn:disabled {
    opacity: 0.6;
    cursor: default;
  }

  /* ---- Dimmed folder path above the title (Job B) ---- */
  .note-title-wrap {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
  }

  .note-path {
    align-self: flex-start;
    max-width: 100%;
    margin: 0 0 1px;
    padding: 0 2px;
    border: none;
    border-radius: 4px;
    background: transparent;
    color: var(--text-muted);
    font-family: var(--font-ui);
    font-size: 12px;
    line-height: 1.3;
    text-align: left;
    cursor: pointer;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .note-path:hover {
    color: var(--text);
  }

  .note-path-sep {
    margin: 0 3px;
    opacity: 0.6;
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

  .unlock-pane {
    display: flex;
    flex-direction: column;
    align-items: center;
    text-align: center;
    padding: 24px;
    max-width: 22rem;
  }

  .lock-icon {
    font-size: 34px;
    line-height: 1;
    margin-bottom: 14px;
    opacity: 0.7;
  }

  .unlock-title {
    margin: 0 0 6px;
    font-size: 16px;
    font-weight: 600;
    color: var(--text);
  }

  .unlock-hint {
    margin: 0 0 16px;
    font-size: 13px;
    color: var(--text-muted);
  }

  .unlock-form {
    display: flex;
    flex-direction: column;
    gap: 10px;
    width: 100%;
  }

  .unlock-input {
    width: 100%;
    padding: 8px 10px;
    border: 1px solid var(--border);
    border-radius: 6px;
    background-color: var(--bg-input, var(--bg-panel));
    color: var(--text);
    font-size: 14px;
    font-family: var(--font-ui);
    box-sizing: border-box;
  }

  .unlock-input:focus {
    outline: none;
    border-color: var(--accent);
  }

  .unlock-remember {
    display: flex;
    align-items: center;
    gap: 6px;
    font-size: 12px;
    color: var(--text-muted);
    cursor: pointer;
  }

  .unlock-error {
    margin: 0;
    font-size: 12px;
    color: var(--danger);
  }

  .unlock-button {
    padding: 8px 10px;
    border: none;
    border-radius: 6px;
    background-color: var(--accent);
    color: var(--accent-contrast);
    font-size: 14px;
    font-family: var(--font-ui);
    cursor: pointer;
  }

  .unlock-button:hover:not(:disabled) {
    background-color: var(--accent-hover);
  }

  .unlock-button:disabled {
    opacity: 0.6;
    cursor: default;
  }
</style>
