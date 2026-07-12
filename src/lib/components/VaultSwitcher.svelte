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
    createEncryptedVault,
    createTinylordVault,
    removeVault,
    renameVault,
    switchVault,
    vaultError,
    vaults,
    activeVaultId,
    providers,
    createEncryptedFilesVault,
    type VaultInfo,
    type ProviderMeta,
  } from "$lib/stores/vault";
  import { vaultSwitcherOpen } from "$lib/stores/ui";
  import { shortenPath } from "$lib/utils/path";

  // Inline edit/confirm sub-states, keyed by vault id.
  let renamingId = $state<string | null>(null);
  let renameValue = $state("");
  let confirmingRemoveId = $state<string | null>(null);

  // "New vault" flow: pick a provider type, then fill its config fields.
  let newStep = $state<"type" | "config" | null>(null);
  let newProvider = $state<ProviderMeta | null>(null);
  // Generic config values keyed by ConfigField.key, plus the remember toggle.
  let configValues = $state<Record<string, string>>({});
  let rememberPassword = $state(false);
  let createError = $state("");
  let creating = $state(false);

  function open(): void {
    vaultSwitcherOpen.set(true);
  }

  function resetNew(): void {
    newStep = null;
    newProvider = null;
    configValues = {};
    rememberPassword = false;
    createError = "";
    creating = false;
  }

  function close(): void {
    vaultSwitcherOpen.set(false);
    renamingId = null;
    confirmingRemoveId = null;
    resetNew();
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

  // Start the "New vault" flow: skip straight to the config form when only one
  // provider is compiled in, otherwise show the type picker.
  function startNewVault(): void {
    createError = "";
    if ($providers.length <= 1) {
      chooseProvider($providers[0] ?? null);
    } else {
      newStep = "type";
    }
  }

  function chooseProvider(p: ProviderMeta | null): void {
    if (!p) return;
    newProvider = p;
    // Seed the form with any field defaults from the provider metadata
    // (e.g. tinylord's database = "jaynotes").
    const seeded: Record<string, string> = {};
    for (const f of p.configFields) {
      if (f.default) seeded[f.key] = f.default;
    }
    configValues = seeded;
    rememberPassword = false;
    createError = "";
    newStep = "config";
  }

  async function pickFolderInto(key: string): Promise<void> {
    try {
      const { invoke } = await import("@tauri-apps/api/core");
      const picked = await invoke<string | null>("pick_vault");
      if (picked) configValues = { ...configValues, [key]: picked };
    } catch (e) {
      createError = String(e);
    }
  }

  async function submitNewVault(): Promise<void> {
    const p = newProvider;
    if (!p || creating) return;
    // Required-field + password-confirm validation.
    for (const f of p.configFields) {
      if (f.required && !(configValues[f.key] ?? "").trim()) {
        createError = `${f.label} is required`;
        return;
      }
    }
    if (
      "password" in configValues &&
      "confirm" in configValues &&
      configValues.password !== configValues.confirm
    ) {
      createError = "Passwords do not match";
      return;
    }
    creating = true;
    createError = "";
    try {
      if (p.kind === "encrypted-db") {
        await createEncryptedVault(
          configValues.location,
          configValues.name,
          configValues.password,
          rememberPassword,
        );
      } else if (p.kind === "encrypted-files") {
        await createEncryptedFilesVault(
          configValues.location,
          configValues.name,
          configValues.password,
          configValues.password2 ?? "",
          rememberPassword,
        );
      } else if (p.kind === "tinylord") {
        await createTinylordVault(
          configValues.url,
          configValues.database,
          configValues.username,
          configValues.password,
          rememberPassword,
        );
      } else {
        await createVault(configValues.location, configValues.name);
      }
      close();
    } catch (e) {
      createError = String(e);
      creating = false;
    }
  }

  function onWindowKeydown(event: KeyboardEvent): void {
    if (event.key !== "Escape" || !$vaultSwitcherOpen) return;
    if (renamingId) {
      renamingId = null;
    } else if (confirmingRemoveId) {
      confirmingRemoveId = null;
    } else if (newStep === "config") {
      newStep = $providers.length <= 1 ? null : "type";
    } else if (newStep === "type") {
      newStep = null;
    } else {
      close();
    }
  }

  const statusLabel: Record<string, string> = {
    offline: "offline",
    unsupported: "unsupported",
  };

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
          <div
            class="row"
            class:offline={vault.status === "offline"}
            class:unsupported={vault.status === "unsupported"}
          >
            <button
              type="button"
              class="row-main"
              role="menuitemradio"
              aria-checked={vault.id === $activeVaultId}
              disabled={vault.status !== "ok"}
              title={vault.status === "offline"
                ? "Drive not connected"
                : vault.status === "unsupported"
                  ? "This vault type is unsupported in this build"
                  : vault.path}
              onclick={() => pick(vault)}
            >
              {#if vault.kind === "tinylord"}
                <span class="dot dot-lock" aria-hidden="true">🌐</span>
              {:else if vault.kind !== "plain"}
                <span class="dot dot-lock" aria-hidden="true">🔒</span>
              {:else}
                <span
                  class="dot"
                  class:dot-ok={vault.status === "ok"}
                  class:dot-offline={vault.status === "offline"}
                ></span>
              {/if}
              <span class="row-text">
                <span class="row-name">
                  {vault.name}
                  {#if statusLabel[vault.status]}
                    <span class="tag" class:tag-warn={vault.status === "unsupported"}>
                      {statusLabel[vault.status]}
                    </span>
                  {/if}
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

      {#if newStep === "type"}
        <div class="new-vault">
          <p class="new-vault-heading">Choose a vault type</p>
          {#each $providers as p (p.kind)}
            <button
              type="button"
              class="type-option"
              onclick={() => chooseProvider(p)}
            >
              <span class="type-icon"
                >{p.kind === "plain" ? "📁" : p.kind === "tinylord" ? "🌐" : "🔒"}</span
              >
              <span class="type-text">
                <span class="type-name">{p.displayName}</span>
                <span class="type-desc">{p.description}</span>
              </span>
            </button>
          {/each}
          <div class="confirm-actions">
            <button type="button" class="confirm-btn" onclick={resetNew}>Cancel</button>
          </div>
        </div>
      {:else if newStep === "config" && newProvider}
        <div class="new-vault">
          <p class="new-vault-heading">New {newProvider.displayName.toLowerCase()}</p>
          {#each newProvider.configFields as f (f.key)}
            <label class="field-label" for={`cfg-${f.key}`}>{f.label}</label>
            {#if f.fieldType === "folder"}
              <button
                type="button"
                class="folder-pick"
                onclick={() => pickFolderInto(f.key)}
              >
                {configValues[f.key]
                  ? shortenPath(configValues[f.key])
                  : (f.placeholder ?? "Choose folder…")}
              </button>
            {:else}
              <input
                id={`cfg-${f.key}`}
                class="rename-input"
                type={f.fieldType === "password" ? "password" : "text"}
                autocomplete="off"
                placeholder={f.placeholder ?? ""}
                bind:value={configValues[f.key]}
                onkeydown={(e) => {
                  if (e.key === "Enter") submitNewVault();
                }}
              />
            {/if}
          {/each}
          {#if newProvider.capabilities.needsUnlock}
            <label class="remember">
              <input type="checkbox" bind:checked={rememberPassword} />
              Remember password
            </label>
          {/if}
          {#if createError}
            <p class="create-error">{createError}</p>
          {/if}
          <div class="confirm-actions">
            <button
              type="button"
              class="confirm-btn primary"
              disabled={creating}
              onclick={submitNewVault}
            >
              {creating ? "Creating…" : "Create"}
            </button>
            <button type="button" class="confirm-btn" onclick={resetNew}>Cancel</button>
          </div>
        </div>
      {:else}
        <button type="button" class="menu-item" role="menuitem" onclick={onAddExisting}>
          <span class="menu-icon">＋</span>
          <span>Add existing folder…</span>
        </button>
        <button type="button" class="menu-item" role="menuitem" onclick={startNewVault}>
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

  /* The encrypted-vault lock glyph replaces the status dot. */
  .dot-lock {
    width: auto;
    height: auto;
    background: transparent;
    border-radius: 0;
    font-size: 10px;
    line-height: 1;
  }

  .row.unsupported .row-main {
    opacity: 0.55;
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

  .tag-warn {
    color: var(--text-muted);
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

  .new-vault-heading {
    margin: 2px 2px 8px;
    font-size: 12px;
    font-weight: 600;
    color: var(--text);
  }

  .type-option {
    display: flex;
    align-items: center;
    gap: 8px;
    width: 100%;
    padding: 8px;
    margin-bottom: 4px;
    border: 1px solid var(--border);
    border-radius: 6px;
    background: transparent;
    color: var(--text);
    text-align: left;
    font-family: var(--font-ui);
    cursor: pointer;
  }

  .type-option:hover {
    background-color: var(--hover);
    border-color: var(--accent);
  }

  .type-icon {
    flex-shrink: 0;
    font-size: 16px;
  }

  .type-text {
    display: flex;
    flex-direction: column;
    min-width: 0;
  }

  .type-name {
    font-size: 13px;
    font-weight: 500;
  }

  .type-desc {
    font-size: 11px;
    color: var(--text-muted);
  }

  .field-label {
    display: block;
    margin: 6px 2px 3px;
    font-size: 11px;
    color: var(--text-muted);
  }

  .folder-pick {
    width: 100%;
    padding: 6px 8px;
    border: 1px dashed var(--border);
    border-radius: 5px;
    background: transparent;
    color: var(--text);
    font-size: 12px;
    font-family: var(--font-ui);
    text-align: left;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    cursor: pointer;
  }

  .folder-pick:hover {
    border-color: var(--accent);
    background-color: var(--hover);
  }

  .remember {
    display: flex;
    align-items: center;
    gap: 6px;
    margin-top: 8px;
    font-size: 12px;
    color: var(--text-muted);
    cursor: pointer;
  }

  .create-error {
    margin: 8px 2px 0;
    font-size: 12px;
    color: var(--danger);
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
