<script lang="ts">
  import FileTree from "./FileTree.svelte";
  import {
    commitRename,
    expandedDirs,
    openContextMenu,
    renamingPath,
    selected,
    startRename,
    toggleDir,
    vaultError,
    type TreeNode,
  } from "$lib/stores/vault";

  let { nodes, depth = 0 }: { nodes: TreeNode[]; depth?: number } = $props();

  function displayName(node: TreeNode): string {
    if (node.isDir) return node.name;
    return node.name.replace(/\.md$/i, "");
  }

  function handleClick(node: TreeNode): void {
    selected.set({ path: node.path, isDir: node.isDir });
    if (node.isDir) {
      toggleDir(node.path);
    }
  }

  function handleContextMenu(event: MouseEvent, node: TreeNode): void {
    event.preventDefault();
    // Keep the window-level close-on-contextmenu handler from immediately
    // closing the menu we are about to open.
    event.stopPropagation();
    selected.set({ path: node.path, isDir: node.isDir });
    openContextMenu(event.clientX, event.clientY, node);
  }

  async function handleRenameKey(
    event: KeyboardEvent,
    node: TreeNode,
  ): Promise<void> {
    const input = event.currentTarget as HTMLInputElement;
    if (event.key === "Enter") {
      event.preventDefault();
      try {
        await commitRename(node, input.value);
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
</script>

<ul class="tree" role="group">
  {#each nodes as node (node.path)}
    {@const isExpanded = node.isDir && $expandedDirs.has(node.path)}
    {@const isSelected = $selected?.path === node.path}
    <li>
      {#if $renamingPath === node.path}
        <div class="row renaming" style:padding-left="{depth * 14 + 8}px">
          {#if node.isDir}
            <svg
              class="chevron"
              class:expanded={isExpanded}
              viewBox="0 0 16 16"
              width="12"
              height="12"
              aria-hidden="true"
            >
              <path
                d="M6 4l4 4-4 4"
                fill="none"
                stroke="currentColor"
                stroke-width="1.5"
                stroke-linecap="round"
                stroke-linejoin="round"
              />
            </svg>
          {:else}
            <span class="chevron-spacer"></span>
          {/if}
          <input
            class="rename-input"
            type="text"
            value={displayName(node)}
            use:focusAndSelect
            onkeydown={(e) => handleRenameKey(e, node)}
            onblur={() => renamingPath.set(null)}
          />
        </div>
      {:else}
        <button
          type="button"
          class="row"
          class:selected={isSelected}
          style:padding-left="{depth * 14 + 8}px"
          onclick={() => handleClick(node)}
          ondblclick={() => startRename(node)}
          oncontextmenu={(e) => handleContextMenu(e, node)}
          title={node.path}
        >
          {#if node.isDir}
            <svg
              class="chevron"
              class:expanded={isExpanded}
              viewBox="0 0 16 16"
              width="12"
              height="12"
              aria-hidden="true"
            >
              <path
                d="M6 4l4 4-4 4"
                fill="none"
                stroke="currentColor"
                stroke-width="1.5"
                stroke-linecap="round"
                stroke-linejoin="round"
              />
            </svg>
          {:else}
            <span class="chevron-spacer"></span>
          {/if}
          <span class="name">{displayName(node)}</span>
        </button>
      {/if}

      {#if isExpanded}
        <FileTree nodes={node.children} depth={depth + 1} />
      {/if}
    </li>
  {/each}
</ul>

<style>
  .tree {
    list-style: none;
    margin: 0;
    padding: 0;
  }

  .row {
    display: flex;
    align-items: center;
    gap: 5px;
    width: 100%;
    padding: 3px 8px;
    border: none;
    border-radius: 5px;
    background: transparent;
    color: var(--text);
    font-size: 13px;
    font-family: var(--font-ui);
    text-align: left;
    cursor: pointer;
    white-space: nowrap;
    overflow: hidden;
  }

  .row:hover {
    background-color: var(--code-bg);
  }

  .row.selected {
    background-color: var(--accent);
    color: var(--accent-contrast);
  }

  .row.selected:hover {
    background-color: var(--accent-hover);
  }

  .row.renaming {
    cursor: default;
  }

  .chevron {
    flex-shrink: 0;
    color: var(--text-muted);
    transition: transform 0.12s ease;
  }

  .row.selected .chevron {
    color: var(--accent-contrast);
  }

  .chevron.expanded {
    transform: rotate(90deg);
  }

  .chevron-spacer {
    flex-shrink: 0;
    width: 12px;
  }

  .name {
    overflow: hidden;
    text-overflow: ellipsis;
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
</style>
