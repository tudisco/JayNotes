<!--
  SettingsMenu.svelte — the sidebar footer "Settings" control.

  Renders a single footer button that opens a small popover (upward, since the
  footer sits at the bottom) with:
    - theme cycle (reuses the shared theme store)
    - "Manage vaults…" (opens the header vault switcher popover)
    - "Rebuild index" (reindex_vault) with transient status
    - the app version

  Self-contained: owns its trigger, popover, click-outside/Escape handling, and
  styles, so the Sidebar footer is just <SettingsMenu />.
-->
<script lang="ts">
  import { onMount } from "svelte";
  import { getVersion } from "@tauri-apps/api/app";
  import { invoke } from "@tauri-apps/api/core";
  import { themeMode, cycleTheme, type ThemeMode } from "$lib/stores/theme";
  import { editorWidth, toggleEditorWidth } from "$lib/stores/ui";
  import {
    vaultError,
    vaultPath,
    activeVault,
    vaultLocked,
    lockVault,
  } from "$lib/stores/vault";
  import { vaultSwitcherOpen } from "$lib/stores/ui";

  const themeLabels: Record<ThemeMode, string> = {
    light: "Light",
    dark: "Dark",
    system: "System",
  };
  const themeIcons: Record<ThemeMode, string> = {
    light: "☀",
    dark: "☾",
    system: "◐",
  };

  let open = $state(false);
  let reindexStatus = $state<"idle" | "running" | "done" | "error">("idle");
  let version = $state("");

  onMount(async () => {
    try {
      version = await getVersion();
    } catch {
      version = "0.1.0";
    }
  });

  function toggle(): void {
    open = !open;
  }

  function close(): void {
    open = false;
  }

  function manageVaults(): void {
    close();
    vaultSwitcherOpen.set(true);
  }

  // "Lock vault" is offered only for an unlocked encrypted vault.
  let canLock = $derived(
    $activeVault !== null && $activeVault.kind !== "plain" && !$vaultLocked,
  );

  async function onLock(): Promise<void> {
    const v = $activeVault;
    close();
    if (!v) return;
    try {
      await lockVault(v.id);
    } catch (e) {
      vaultError.set(String(e));
    }
  }

  async function rebuildIndex(): Promise<void> {
    reindexStatus = "running";
    try {
      await invoke<number>("reindex_vault");
      reindexStatus = "done";
      setTimeout(() => {
        if (reindexStatus === "done") reindexStatus = "idle";
      }, 2000);
    } catch (e) {
      reindexStatus = "error";
      vaultError.set(String(e));
    }
  }

  function onWindowKeydown(event: KeyboardEvent): void {
    if (event.key === "Escape" && open) close();
  }

  function onWindowPointerDown(event: MouseEvent): void {
    if (!open) return;
    const target = event.target as HTMLElement;
    if (!target.closest(".settings-menu")) close();
  }
</script>

<svelte:window onkeydown={onWindowKeydown} onmousedown={onWindowPointerDown} />

<div class="settings-menu">
  {#if open}
    <div class="popover" role="menu" aria-label="Settings">
      <button
        type="button"
        class="menu-item"
        role="menuitem"
        onclick={cycleTheme}
      >
        <span class="menu-icon">{themeIcons[$themeMode]}</span>
        <span class="menu-label">Theme</span>
        <span class="menu-value">{themeLabels[$themeMode]}</span>
      </button>

      <button
        type="button"
        class="menu-item"
        role="menuitem"
        onclick={toggleEditorWidth}
      >
        <span class="menu-icon">⇔</span>
        <span class="menu-label">Note width</span>
        <span class="menu-value">{$editorWidth === "full" ? "Full" : "Comfortable"}</span>
      </button>

      <div class="separator"></div>

      <button
        type="button"
        class="menu-item"
        role="menuitem"
        onclick={manageVaults}
      >
        <span class="menu-icon">⇄</span>
        <span class="menu-label">Manage vaults…</span>
      </button>

      {#if canLock}
        <button type="button" class="menu-item" role="menuitem" onclick={onLock}>
          <span class="menu-icon">🔒</span>
          <span class="menu-label">Lock vault</span>
        </button>
      {/if}

      <button
        type="button"
        class="menu-item"
        role="menuitem"
        disabled={!$vaultPath || reindexStatus === "running"}
        onclick={rebuildIndex}
      >
        <span class="menu-icon">↻</span>
        <span class="menu-label">Rebuild index</span>
        {#if reindexStatus === "running"}
          <span class="menu-value">Rebuilding…</span>
        {:else if reindexStatus === "done"}
          <span class="menu-value">Done</span>
        {/if}
      </button>

      <div class="separator"></div>

      <div class="version">JayNotes v{version}</div>
    </div>
  {/if}

  <button
    class="footer-item"
    class:active={open}
    type="button"
    aria-haspopup="menu"
    aria-expanded={open}
    onclick={toggle}
  >
    <span class="footer-icon">⚙</span>
    <span>Settings</span>
  </button>
</div>

<style>
  .settings-menu {
    position: relative;
  }

  .footer-item {
    display: flex;
    align-items: center;
    gap: 10px;
    width: 100%;
    padding: 8px 8px;
    border: none;
    border-radius: 6px;
    background: transparent;
    color: var(--text);
    font-size: 13px;
    font-family: var(--font-ui);
    text-align: left;
    cursor: pointer;
  }

  .footer-item:hover,
  .footer-item.active {
    background-color: var(--hover);
    color: var(--accent);
  }

  .footer-icon {
    width: 16px;
    text-align: center;
    color: var(--text-muted);
  }

  .footer-item:hover .footer-icon,
  .footer-item.active .footer-icon {
    color: var(--accent);
  }

  .popover {
    position: absolute;
    left: 0;
    right: 0;
    bottom: calc(100% + 6px);
    z-index: 200;
    padding: 4px;
    border: 1px solid var(--border);
    border-radius: 8px;
    background-color: var(--bg-panel);
    box-shadow: var(--shadow-menu);
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

  .menu-item:hover:not(:disabled) {
    background-color: var(--hover);
  }

  .menu-item:disabled {
    opacity: 0.5;
    cursor: default;
  }

  .menu-icon {
    width: 16px;
    text-align: center;
    color: var(--text-muted);
    flex-shrink: 0;
  }

  .menu-label {
    flex: 1;
  }

  .menu-value {
    font-size: 12px;
    color: var(--text-muted);
  }

  .separator {
    height: 1px;
    margin: 4px 6px;
    background-color: var(--border);
  }

  .version {
    padding: 6px 8px 4px;
    font-size: 11px;
    color: var(--text-muted);
  }
</style>
