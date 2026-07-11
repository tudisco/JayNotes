<script lang="ts">
  import {
    fileTree,
    newFolder,
    newItemTargetDir,
    newNote,
    openVault,
    vaultError,
    vaultLoading,
    vaultPath,
  } from "$lib/stores/vault";
  import FileTree from "./FileTree.svelte";
  import ContextMenu from "./ContextMenu.svelte";
  import SearchPanel from "./SearchPanel.svelte";
  import TagsPanel from "./TagsPanel.svelte";
  import SettingsMenu from "./SettingsMenu.svelte";
  import { sidebarMode, searchFocusNonce } from "$lib/stores/ui";

  function showFiles(): void {
    sidebarMode.set("files");
  }

  function showSearch(): void {
    sidebarMode.set("search");
    searchFocusNonce.update((n) => n + 1);
  }

  function showTags(): void {
    sidebarMode.set("tags");
  }

  async function handleNewNote(): Promise<void> {
    try {
      await newNote(newItemTargetDir());
    } catch (e) {
      vaultError.set(String(e));
    }
  }

  async function handleNewFolder(): Promise<void> {
    try {
      await newFolder(newItemTargetDir());
    } catch (e) {
      vaultError.set(String(e));
    }
  }
</script>

<aside class="sidebar">
  <div class="brand">
    <span class="brand-name">JayNotes</span>
  </div>

  {#if $vaultPath}
    <div class="tabs" role="tablist" aria-label="Sidebar view">
      <button
        type="button"
        role="tab"
        class="tab"
        class:active={$sidebarMode === "files"}
        aria-selected={$sidebarMode === "files"}
        title="Files (Cmd+E)"
        onclick={showFiles}
      >
        <svg viewBox="0 0 16 16" width="15" height="15" aria-hidden="true">
          <path
            d="M1.5 3.5a1 1 0 0 1 1-1h3l1.5 2h6.5a1 1 0 0 1 1 1v7a1 1 0 0 1-1 1h-11a1 1 0 0 1-1-1v-9z"
            fill="none"
            stroke="currentColor"
            stroke-width="1.3"
            stroke-linejoin="round"
          />
        </svg>
        <span>Files</span>
      </button>
      <button
        type="button"
        role="tab"
        class="tab"
        class:active={$sidebarMode === "search"}
        aria-selected={$sidebarMode === "search"}
        title="Search (Cmd+Shift+F)"
        onclick={showSearch}
      >
        <svg viewBox="0 0 16 16" width="15" height="15" aria-hidden="true">
          <circle
            cx="7"
            cy="7"
            r="4.5"
            fill="none"
            stroke="currentColor"
            stroke-width="1.3"
          />
          <path
            d="M10.5 10.5L14 14"
            fill="none"
            stroke="currentColor"
            stroke-width="1.3"
            stroke-linecap="round"
          />
        </svg>
        <span>Search</span>
      </button>
      <button
        type="button"
        role="tab"
        class="tab"
        class:active={$sidebarMode === "tags"}
        aria-selected={$sidebarMode === "tags"}
        title="Tags"
        onclick={showTags}
      >
        <svg viewBox="0 0 16 16" width="15" height="15" aria-hidden="true">
          <path
            d="M2.5 2.5h4.2a1 1 0 0 1 .7.3l6 6a1 1 0 0 1 0 1.4l-3.5 3.5a1 1 0 0 1-1.4 0l-6-6a1 1 0 0 1-.3-.7V3.5a1 1 0 0 1 1-1z"
            fill="none"
            stroke="currentColor"
            stroke-width="1.3"
            stroke-linejoin="round"
          />
          <circle cx="5.2" cy="5.2" r="1.05" fill="currentColor" />
        </svg>
        <span>Tags</span>
      </button>
    </div>
  {/if}

  <nav class="notes">
    {#if $vaultPath && $sidebarMode === "search"}
      <SearchPanel />
    {:else if $vaultPath && $sidebarMode === "tags"}
      <TagsPanel />
    {:else}
    <div class="section-header">
      <span class="section-label">Notes</span>
      {#if $vaultPath}
        <div class="toolbar">
          <button
            type="button"
            class="tool-btn"
            title="New note"
            aria-label="New note"
            onclick={handleNewNote}
          >
            <svg viewBox="0 0 16 16" width="14" height="14" aria-hidden="true">
              <path
                d="M9.5 1.5H4a1 1 0 0 0-1 1v11a1 1 0 0 0 1 1h8a1 1 0 0 0 1-1V5m-3.5-3.5L13 5m-3.5-3.5V4a1 1 0 0 0 1 1H13"
                fill="none"
                stroke="currentColor"
                stroke-width="1.3"
                stroke-linecap="round"
                stroke-linejoin="round"
              />
              <path
                d="M8 7.5v4M6 9.5h4"
                fill="none"
                stroke="currentColor"
                stroke-width="1.3"
                stroke-linecap="round"
              />
            </svg>
          </button>
          <button
            type="button"
            class="tool-btn"
            title="New folder"
            aria-label="New folder"
            onclick={handleNewFolder}
          >
            <svg viewBox="0 0 16 16" width="14" height="14" aria-hidden="true">
              <path
                d="M1.5 3.5a1 1 0 0 1 1-1h3l1.5 2h6.5a1 1 0 0 1 1 1v7a1 1 0 0 1-1 1h-11a1 1 0 0 1-1-1v-9z"
                fill="none"
                stroke="currentColor"
                stroke-width="1.3"
                stroke-linecap="round"
                stroke-linejoin="round"
              />
              <path
                d="M8 7.5v3.5M6.25 9.25h3.5"
                fill="none"
                stroke="currentColor"
                stroke-width="1.3"
                stroke-linecap="round"
              />
            </svg>
          </button>
        </div>
      {/if}
    </div>

    {#if $vaultLoading}
      <div class="empty-tree">Loading…</div>
    {:else if !$vaultPath}
      <div class="empty-tree">
        <p>Open a vault to get started</p>
        <button type="button" class="open-vault-btn" onclick={openVault}>
          Open Vault
        </button>
      </div>
    {:else if $fileTree}
      {#if $fileTree.children.length === 0}
        <div class="empty-tree">No notes yet</div>
      {:else}
        <FileTree nodes={$fileTree.children} />
      {/if}
    {/if}
    {/if}

    {#if $vaultError}
      <div class="error" role="alert">{$vaultError}</div>
    {/if}
  </nav>

  <div class="footer">
    <SettingsMenu />
  </div>
</aside>

<ContextMenu />

<style>
  .sidebar {
    width: 250px;
    min-width: 250px;
    height: 100%;
    display: flex;
    flex-direction: column;
    background-color: var(--bg-sidebar);
    border-right: 1px solid var(--border);
  }

  .brand {
    padding: 16px 16px 12px;
    border-bottom: 1px solid var(--border);
  }

  .brand-name {
    font-size: 16px;
    font-weight: 600;
    color: var(--text);
    letter-spacing: -0.01em;
  }

  .tabs {
    display: flex;
    gap: 2px;
    padding: 6px 8px 0;
    border-bottom: 1px solid var(--border);
  }

  .tab {
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 6px;
    flex: 1;
    padding: 7px 8px;
    border: none;
    border-bottom: 2px solid transparent;
    margin-bottom: -1px;
    background: transparent;
    color: var(--text-muted);
    font-family: var(--font-ui);
    font-size: 12px;
    font-weight: 500;
    cursor: pointer;
  }

  .tab:hover {
    color: var(--text);
  }

  .tab.active {
    color: var(--accent);
    border-bottom-color: var(--accent);
  }

  .notes {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    padding: 12px 8px;
  }

  .section-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 4px 8px;
  }

  .section-label {
    font-size: 11px;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--text-muted);
  }

  .toolbar {
    display: flex;
    gap: 2px;
  }

  .tool-btn {
    display: flex;
    align-items: center;
    justify-content: center;
    padding: 3px;
    border: none;
    border-radius: 4px;
    background: transparent;
    color: var(--text-muted);
    cursor: pointer;
  }

  .tool-btn:hover {
    background-color: var(--hover);
    color: var(--accent);
  }

  .empty-tree {
    margin-top: 8px;
    padding: 12px 8px;
    font-size: 13px;
    color: var(--text-muted);
    font-style: italic;
  }

  .empty-tree p {
    margin: 0 0 10px;
  }

  .open-vault-btn {
    padding: 6px 12px;
    border: none;
    border-radius: 6px;
    background-color: var(--accent);
    color: var(--accent-contrast);
    font-size: 13px;
    font-weight: 500;
    font-family: var(--font-ui);
    font-style: normal;
    cursor: pointer;
  }

  .open-vault-btn:hover {
    background-color: var(--accent-hover);
  }

  .error {
    margin: 8px;
    padding: 8px;
    border: 1px solid var(--danger);
    border-radius: 6px;
    font-size: 12px;
    color: var(--danger);
    word-break: break-word;
  }

  .footer {
    border-top: 1px solid var(--border);
    padding: 8px;
  }
</style>
