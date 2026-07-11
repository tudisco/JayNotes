<script lang="ts">
  import { themeMode, cycleTheme, type ThemeMode } from "$lib/stores/theme";

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
</script>

<aside class="sidebar">
  <div class="brand">
    <span class="brand-name">JayNotes</span>
  </div>

  <nav class="notes">
    <div class="section-label">Notes</div>
    <div class="empty-tree">Open a vault to get started</div>
  </nav>

  <div class="footer">
    <button class="footer-item" type="button">
      <span class="footer-icon">⚙</span>
      <span>Settings</span>
    </button>
    <button class="footer-item" type="button" onclick={cycleTheme}>
      <span class="footer-icon">{themeIcons[$themeMode]}</span>
      <span>Theme: {themeLabels[$themeMode]}</span>
    </button>
  </div>
</aside>

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

  .notes {
    flex: 1;
    overflow-y: auto;
    padding: 12px 8px;
  }

  .section-label {
    font-size: 11px;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--text-muted);
    padding: 4px 8px;
  }

  .empty-tree {
    margin-top: 8px;
    padding: 12px 8px;
    font-size: 13px;
    color: var(--text-muted);
    font-style: italic;
  }

  .footer {
    border-top: 1px solid var(--border);
    padding: 8px;
    display: flex;
    flex-direction: column;
    gap: 2px;
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
    text-align: left;
    cursor: pointer;
  }

  .footer-item:hover {
    background-color: var(--code-bg);
    color: var(--accent);
  }

  .footer-icon {
    width: 16px;
    text-align: center;
    color: var(--text-muted);
  }

  .footer-item:hover .footer-icon {
    color: var(--accent);
  }
</style>
