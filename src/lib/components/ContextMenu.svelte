<script lang="ts">
  import {
    closeContextMenu,
    contextMenu,
    deleteToTrash,
    newFolder,
    newNote,
    revealInFinder,
    startRename,
    vaultError,
    type TreeNode,
  } from "$lib/stores/vault";

  const MENU_WIDTH = 200;
  const MENU_MAX_HEIGHT = 220;

  let confirmingDelete = $state(false);

  // Reset the delete-confirm step whenever the menu opens/closes.
  $effect(() => {
    $contextMenu;
    confirmingDelete = false;
  });

  function clampX(x: number): number {
    return Math.min(x, window.innerWidth - MENU_WIDTH - 8);
  }

  function clampY(y: number): number {
    return Math.min(y, window.innerHeight - MENU_MAX_HEIGHT - 8);
  }

  async function run(action: (node: TreeNode) => Promise<void> | void) {
    const menu = $contextMenu;
    closeContextMenu();
    if (!menu) return;
    try {
      await action(menu.node);
    } catch (e) {
      vaultError.set(String(e));
    }
  }

  function handleWindowKeydown(event: KeyboardEvent) {
    if (event.key === "Escape" && $contextMenu) {
      closeContextMenu();
    }
  }

  function handleWindowPointerDown(event: MouseEvent) {
    if (!$contextMenu) return;
    const target = event.target as HTMLElement;
    if (!target.closest(".context-menu")) {
      closeContextMenu();
    }
  }
</script>

<svelte:window
  onkeydown={handleWindowKeydown}
  onmousedown={handleWindowPointerDown}
  oncontextmenu={handleWindowPointerDown}
/>

{#if $contextMenu}
  {@const menu = $contextMenu}
  <div
    class="context-menu"
    role="menu"
    tabindex="-1"
    style:left="{clampX(menu.x)}px"
    style:top="{clampY(menu.y)}px"
  >
    {#if confirmingDelete}
      <div class="confirm">
        <p class="confirm-text">
          Move <strong>{menu.node.name}</strong> to Trash?
        </p>
        <div class="confirm-actions">
          <button
            type="button"
            class="confirm-btn danger"
            onclick={() => run(deleteToTrash)}
          >
            Move to Trash
          </button>
          <button
            type="button"
            class="confirm-btn"
            onclick={() => closeContextMenu()}
          >
            Cancel
          </button>
        </div>
      </div>
    {:else}
      {#if menu.node.isDir}
        <button
          type="button"
          class="item"
          role="menuitem"
          onclick={() => run((n) => newNote(n.path))}
        >
          New note
        </button>
        <button
          type="button"
          class="item"
          role="menuitem"
          onclick={() => run((n) => newFolder(n.path))}
        >
          New folder
        </button>
        <div class="separator"></div>
      {/if}
      <button
        type="button"
        class="item"
        role="menuitem"
        onclick={() => run((n) => startRename(n))}
      >
        Rename
      </button>
      <button
        type="button"
        class="item"
        role="menuitem"
        onclick={() => (confirmingDelete = true)}
      >
        Delete
      </button>
      <div class="separator"></div>
      <button
        type="button"
        class="item"
        role="menuitem"
        onclick={() => run(revealInFinder)}
      >
        Reveal in Finder
      </button>
    {/if}
  </div>
{/if}

<style>
  .context-menu {
    position: fixed;
    z-index: 1000;
    min-width: 180px;
    max-width: 240px;
    padding: 4px;
    border: 1px solid var(--border);
    border-radius: 8px;
    background-color: var(--bg-panel);
    box-shadow: var(--shadow-menu);
  }

  .item {
    display: block;
    width: 100%;
    padding: 6px 10px;
    border: none;
    border-radius: 5px;
    background: transparent;
    color: var(--text);
    font-size: 13px;
    font-family: var(--font-ui);
    text-align: left;
    cursor: pointer;
  }

  .item:hover {
    background-color: var(--accent);
    color: var(--accent-contrast);
  }

  .separator {
    height: 1px;
    margin: 4px 6px;
    background-color: var(--border);
  }

  .confirm {
    padding: 8px 10px;
  }

  .confirm-text {
    margin: 0 0 8px;
    font-size: 13px;
    color: var(--text);
    word-break: break-word;
  }

  .confirm-actions {
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
</style>
