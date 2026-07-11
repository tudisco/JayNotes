<script lang="ts">
</script>

<section class="editor-pane">
  <div class="empty-state">
    <div class="empty-icon">✎</div>
    <p class="empty-title">No note open</p>
    <p class="empty-hint">Select a note from the sidebar to start editing.</p>
  </div>
</section>

<style>
  .editor-pane {
    flex: 1;
    height: 100%;
    display: flex;
    align-items: center;
    justify-content: center;
    background-color: var(--bg-panel);
    overflow-y: auto;
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
</style>
