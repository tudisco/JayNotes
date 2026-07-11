<!--
  VaultSwitcher.svelte — the sidebar header vault selector.

  Renders the brand row as a button showing the active vault's name + a chevron.
  Clicking opens a popover listing every configured vault:
    - each row: name, dimmed shortened path, status dot (accent = ok, amber =
      offline), a checkmark on the active vault, and hover actions rename/remove.
    - offline rows are disabled for switching (the drive is unplugged).
    - bottom actions: "Add existing folder…" and "New vault…".

  Reuses the popover/menu styling patterns from SettingsMenu/ContextMenu. The
  popover open state is shared via the `vaultSwitcherOpen` ui store so the
  Settings menu's "Manage vaults…" item can open it too. Escape closes.
-->
<script lang="ts">
  import {
    activeVault,
    addVault,
    createVault,
    removeVault,
    renameVault,
    switchVault,
    vaultError,
    vaults,
    activeVaultId,
    type VaultInfo,
  } from "$lib/stores/vault";
  import { vaultSwitcherOpen } from "$lib/stores/ui";
  import { shortenPath } from "$lib/utils/path";

  // Inline edit/confirm sub-states, keyed by vault id.
  let renamingId = $state<string | null>(null);
  let renameValue = $state("");
  let confirmingRemoveId = $state<string | null>(null);

  // "New vault" inline flow: once a parent folder is picked, prompt for a name.
  let newVaultParent = $state<string | null>(null);
  let newVaultName = $state("");

  function open(): void {
    vaultSwitcherOpen.set(true);
  }

  function close(): void {
    vaultSwitcherOpen.set(false);
    renamingId = null;
    confirmingRemoveId = null;
    newVaultParent = null;
    newVaultName = "";
  }

  function toggle(): void {
    if ($vaultSwitcherOpen) close();
    else open();
  }

  async function pick(vault: VaultInfo): Promise<void> {
    if (vault.status !== "ok" || vault.id === $activeVaultId) {
      if (vault.id === $activeVaultId) close();
      return;
    }
    close();
    try {
      await switchVault(vault.id);
    } catch (e) {
      vaultError.set(String(e));
    }
  }

  function beginRename(vault: VaultInfo): void {
    renamingId = vault.id;
    renameValue = vault.name;
    confirmingRemoveId = null;
  }

  async function commitRename(): Promise<void> {
    const id = renamingId;
    const name = renameValue.trim();
    renamingId = null;
    if (!id || !name) return;
    try {
      await renameVault(id, name);
    } catch (e) {
      vaultError.set(String(e));
    }
  }

  async function confirmRemove(id: string): Promise<void> {
    confirmingRemoveId = null;
    try {
      await removeVault(id);
    } catch (e) {
      vaultError.set(String(e));
    }
  }

  async function onAddExisting(): Promise<void> {
    close();
    try {
      await addVault();
    } catch (e) {
      vaultError.set(String(e));
    }
  }

  async function onPickNewParent(): Promise<void> {
    try {
      const { invoke } = await import("@tauri-apps/api/core");
      const picked = await invoke<string | null>("pick_vault");
      if (picked) {
        newVaultParent = picked;
        newVaultName = "";
      }
    } catch (e) {
      vaultError.set(String(e));
    }
  }

  async function commitNewVault(): Promise<void> {
    const parent = newVaultParent;
    const name = newVaultName.trim();
    if (!parent || !name) return;
    close();
    try {
      await createVault(parent, name);
    } catch (e) {
      vaultError.set(String(e));
    }
  }

  function onWindowKeydown(event: KeyboardEvent): void {
    if (event.key !== "Escape" || !$vaultSwitcherOpen) return;
    if (renamingId) {
      renamingId = null;
    } else if (confirmingRemoveId) {
      confirmingRemoveId = null;
    } else if (newVaultParent !== null) {
      newVaultParent = null;
    } else {
      close();
    }
  }

  function onWindowPointerDown(event: MouseEvent): void {
    if (!$vaultSwitcherOpen) return;
    const target = event.target as HTMLElement;
    if (!target.closest(".vault-switcher")) close();
  }
</script>

<svelte:window onkeydown={onWindowKeydown} onmousedown={onWindowPointerDown} />

<div class="vault-switcher">
  <button
    type="button"
    class="brand-trigger"
    class:active={$vaultSwitcherOpen}
    aria-haspopup="menu"
    aria-expanded={$vaultSwitcherOpen}
    onclick={toggle}
  >
    <span class="brand-name">{$activeVault ? $activeVault.name : "JayNotes"}</span>
    <svg class="chevron" viewBox="0 0 16 16" width="12" height="12" aria-hidden="true">
      <path
        d="M4 6l4 4 4-4"
        fill="none"
        stroke="currentColor"
        stroke-width="1.4"
        stroke-linecap="round"
        stroke-linejoin="round"
      />
    </svg>
  </button>

  {#if $vaultSwitcherOpen}
    <div class="popover" role="menu" aria-label="Vaults">
      {#if $vaults.length === 0}
        <div class="empty">No vaults yet</div>
      {/if}

      {#each $vaults as vault (vault.id)}
        {#if renamingId === vault.id}
          <div class="row rename-row">
            <!-- svelte-ignore a11y_autofocus -->
            <input
              class="rename-input"
              autofocus
              bind:value={renameValue}
              onkeydown={(e) => {
                if (e.key === "Enter") commitRename();
              }}
              onblur={commitRename}
            />
          </div>
        {:else if confirmingRemoveId === vault.id}
          <div class="confirm">
            <p class="confirm-text">Forget this vault? The folder is not deleted.</p>
            <div class="confirm-actions">
              <button
                type="button"
                class="confirm-btn danger"
                onclick={() => confirmRemove(vault.id)}
              >
                Forget vault
              </button>
              <button
                type="button"
                class="confirm-btn"
                onclick={() => (confirmingRemoveId = null)}
              >
                Cancel
              </button>
            </div>
          </div>
        {:else}
          <div class="row" class:offline={vault.status === "offline"}>
            <button
              type="button"
              class="row-main"
              role="menuitemradio"
              aria-checked={vault.id === $activeVaultId}
              disabled={vault.status !== "ok"}
              title={vault.status === "offline"
                ? "Drive not connected"
                : vault.path}
              onclick={() => pick(vault)}
            >
              <span
                class="dot"
                class:dot-ok={vault.status === "ok"}
                class:dot-offline={vault.status === "offline"}
              ></span>
              <span class="row-text">
                <span class="row-name">
                  {vault.name}
                  {#if vault.status === "offline"}<span class="tag">offline</span>{/if}
                </span>
                <span class="row-path">{shortenPath(vault.path)}</span>
              </span>
              {#if vault.id === $activeVaultId}
                <span class="check" aria-hidden="true">✓</span>
              {/if}
            </button>
            <div class="row-actions">
              <button
                type="button"
                class="row-action"
                title="Rename"
                aria-label="Rename vault"
                onclick={() => beginRename(vault)}
              >
                ✎
              </button>
              <button
                type="button"
                class="row-action"
                title="Forget vault"
                aria-label="Forget vault"
                onclick={() => (confirmingRemoveId = vault.id)}
              >
                ✕
              </button>
            </div>
          </div>
        {/if}
      {/each}

      <div class="separator"></div>

      {#if newVaultParent !== null}
        <div class="new-vault">
          <p class="new-vault-parent" title={newVaultParent}>
            In {shortenPath(newVaultParent)}
          </p>
          <!-- svelte-ignore a11y_autofocus -->
          <input
            class="rename-input"
            autofocus
            placeholder="New vault name"
            bind:value={newVaultName}
            onkeydown={(e) => {
              if (e.key === "Enter") commitNewVault();
            }}
          />
          <div class="confirm-actions">
            <button type="button" class="confirm-btn primary" onclick={commitNewVault}>
              Create
            </button>
            <button
              type="button"
              class="confirm-btn"
              onclick={() => (newVaultParent = null)}
            >
              Cancel
            </button>
          </div>
        </div>
      {:else}
        <button type="button" class="menu-item" role="menuitem" onclick={onAddExisting}>
          <span class="menu-icon">＋</span>
          <span>Add existing folder…</span>
        </button>
        <button type="button" class="menu-item" role="menuitem" onclick={onPickNewParent}>
          <span class="menu-icon">✦</span>
          <span>New vault…</span>
        </button>
      {/if}
    </div>
  {/if}
</div>

<style>
  .vault-switcher {
    position: relative;
    padding: 12px 12px 10px;
    border-bottom: 1px solid var(--border);
  }

  .brand-trigger {
    display: flex;
    align-items: center;
    gap: 6px;
    width: 100%;
    padding: 4px 6px;
    border: none;
    border-radius: 6px;
    background: transparent;
    color: var(--text);
    font-family: var(--font-ui);
    text-align: left;
    cursor: pointer;
  }

  .brand-trigger:hover,
  .brand-trigger.active {
    background-color: var(--hover);
  }

  .brand-name {
    flex: 1;
    font-size: 15px;
    font-weight: 600;
    letter-spacing: -0.01em;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .chevron {
    flex-shrink: 0;
    color: var(--text-muted);
  }

  .popover {
    position: absolute;
    left: 8px;
    right: 8px;
    top: calc(100% - 2px);
    z-index: 200;
    padding: 4px;
    border: 1px solid var(--border);
    border-radius: 8px;
    background-color: var(--bg-panel);
    box-shadow: var(--shadow-menu);
  }

  .empty {
    padding: 8px 10px;
    font-size: 12px;
    color: var(--text-muted);
    font-style: italic;
  }

  .row {
    display: flex;
    align-items: stretch;
    border-radius: 5px;
  }

  .row:hover {
    background-color: var(--hover);
  }

  .row-main {
    display: flex;
    align-items: center;
    gap: 8px;
    flex: 1;
    min-width: 0;
    padding: 6px 8px;
    border: none;
    background: transparent;
    color: var(--text);
    font-family: var(--font-ui);
    text-align: left;
    cursor: pointer;
  }

  .row-main:disabled {
    cursor: default;
  }

  .row.offline .row-main {
    opacity: 0.65;
  }

  .dot {
    flex-shrink: 0;
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background-color: var(--text-muted);
  }

  .dot-ok {
    background-color: var(--accent);
  }

  .dot-offline {
    background-color: #d9a441;
  }

  .row-text {
    display: flex;
    flex-direction: column;
    min-width: 0;
    flex: 1;
  }

  .row-name {
    display: flex;
    align-items: center;
    gap: 6px;
    font-size: 13px;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .tag {
    font-size: 10px;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    color: #d9a441;
  }

  .row-path {
    font-size: 11px;
    color: var(--text-muted);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .check {
    flex-shrink: 0;
    color: var(--accent);
    font-size: 12px;
  }

  .row-actions {
    display: none;
    align-items: center;
    gap: 2px;
    padding-right: 4px;
  }

  .row:hover .row-actions {
    display: flex;
  }

  .row-action {
    display: flex;
    align-items: center;
    justify-content: center;
    width: 22px;
    height: 22px;
    border: none;
    border-radius: 4px;
    background: transparent;
    color: var(--text-muted);
    font-size: 12px;
    cursor: pointer;
  }

  .row-action:hover {
    background-color: var(--bg-sidebar);
    color: var(--accent);
  }

  .rename-row {
    padding: 4px;
  }

  .rename-input {
    width: 100%;
    padding: 5px 8px;
    border: 1px solid var(--accent);
    border-radius: 5px;
    background-color: var(--bg-input, var(--bg-panel));
    color: var(--text);
    font-size: 13px;
    font-family: var(--font-ui);
    box-sizing: border-box;
  }

  .menu-item {
    display: flex;
    align-items: center;
    gap: 8px;
    width: 100%;
    padding: 7px 8px;
    border: none;
    border-radius: 5px;
    background: transparent;
    color: var(--text);
    font-size: 13px;
    font-family: var(--font-ui);
    text-align: left;
    cursor: pointer;
  }

  .menu-item:hover {
    background-color: var(--hover);
  }

  .menu-icon {
    width: 16px;
    text-align: center;
    color: var(--text-muted);
    flex-shrink: 0;
  }

  .separator {
    height: 1px;
    margin: 4px 6px;
    background-color: var(--border);
  }

  .confirm,
  .new-vault {
    padding: 6px 8px 8px;
  }

  .confirm-text {
    margin: 0 0 8px;
    font-size: 12px;
    color: var(--text);
    line-height: 1.4;
  }

  .new-vault-parent {
    margin: 2px 0 6px;
    font-size: 11px;
    color: var(--text-muted);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .confirm-actions {
    display: flex;
    gap: 6px;
    margin-top: 8px;
  }

  .confirm-btn {
    flex: 1;
    padding: 6px 8px;
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

  .confirm-btn.primary {
    border-color: transparent;
    background-color: var(--accent);
    color: var(--accent-contrast);
  }

  .confirm-btn.primary:hover {
    background-color: var(--accent-hover);
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
