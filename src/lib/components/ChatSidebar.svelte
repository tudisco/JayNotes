<!--
  ChatSidebar.svelte — the collapsible right-hand AI assistant panel.

  Collapsed, it shows only a slim edge toggle. Expanded, it is a fixed-width
  column: header → (optional settings) → messages → context chip → quick actions
  → input. It owns auto-scroll, first-open history/settings loading, the setup
  card, and the send/stop/permission plumbing (delegated to the chat store).
-->
<script lang="ts">
  import { tick } from "svelte";
  import { chatOpen, toggleChat } from "$lib/stores/ui";
  import { selected } from "$lib/stores/vault";
  import {
    aiSettings,
    chatMessages,
    chatStreaming,
    contextEnabled,
    cancel,
    ensureHistoryLoaded,
    loadAiSettings,
    newChat,
    sendMessage,
    QUICK_ACTIONS,
  } from "$lib/stores/chat";
  import MarkdownView from "./chat/MarkdownView.svelte";
  import ThinkingBlock from "./chat/ThinkingBlock.svelte";
  import ToolChip from "./chat/ToolChip.svelte";
  import PermissionCard from "./chat/PermissionCard.svelte";
  import AiSettingsPanel from "./chat/AiSettingsPanel.svelte";

  let draft = $state("");
  let showSettings = $state(false);
  let messagesEl = $state<HTMLDivElement>();
  let textareaEl = $state<HTMLTextAreaElement>();
  let pinned = $state(true);
  let opened = false;

  let entries = $derived($chatMessages);
  let streaming = $derived($chatStreaming);
  let pendingPermission = $derived(
    entries.some((e) => e.kind === "permission" && e.status === "pending"),
  );
  let noteOpen = $derived($selected !== null && !$selected.isDir);
  let noteTitle = $derived(
    noteOpen ? baseName(($selected as { path: string }).path) : "",
  );
  let hasConversation = $derived(
    entries.some((e) => e.kind === "user" || e.kind === "assistant"),
  );

  function isLocal(url: string): boolean {
    return /localhost|127\.0\.0\.1/.test(url);
  }

  let needsSetup = $derived.by(() => {
    const s = $aiSettings;
    if (!s) return true;
    if (!s.model.trim()) return true;
    return !s.apiKeySet && !isLocal(s.baseUrl);
  });

  function baseName(path: string): string {
    return (path.split("/").pop() ?? "").replace(/\.md$/i, "");
  }

  // On first expand, restore history and load provider settings.
  $effect(() => {
    if ($chatOpen && !opened) {
      opened = true;
      void ensureHistoryLoaded();
      void loadAiSettings();
    }
  });

  // Auto-scroll to the newest content while pinned to the bottom.
  $effect(() => {
    entries; // track
    if (!pinned) return;
    void tick().then(() => {
      if (messagesEl) messagesEl.scrollTop = messagesEl.scrollHeight;
    });
  });

  function onScroll(): void {
    if (!messagesEl) return;
    const gap = messagesEl.scrollHeight - messagesEl.scrollTop - messagesEl.clientHeight;
    pinned = gap < 40;
  }

  function jumpToLatest(): void {
    if (!messagesEl) return;
    pinned = true;
    messagesEl.scrollTop = messagesEl.scrollHeight;
  }

  function autoGrow(): void {
    const el = textareaEl;
    if (!el) return;
    el.style.height = "auto";
    el.style.height = `${Math.min(el.scrollHeight, 168)}px`;
  }

  async function send(): Promise<void> {
    const text = draft.trim();
    if (!text || streaming || pendingPermission) return;
    draft = "";
    await tick();
    autoGrow();
    await sendMessage(text);
  }

  function onKeydown(event: KeyboardEvent): void {
    if (event.key === "Enter" && !event.shiftKey) {
      event.preventDefault();
      void send();
    }
  }

  function runQuickAction(prompt: string): void {
    if (streaming || pendingPermission) return;
    void sendMessage(prompt);
  }
</script>

{#if $chatOpen}
  <aside class="chat-panel" aria-label="AI assistant">
    <header class="panel-header">
      <span class="title">Assistant</span>
      <div class="header-actions">
        <button
          type="button"
          class="hbtn"
          title="New chat"
          aria-label="New chat"
          disabled={streaming || entries.length === 0}
          onclick={() => void newChat()}
        >
          <svg viewBox="0 0 24 24" width="15" height="15" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M12 5v14" /><path d="M5 12h14" /></svg>
        </button>
        <button
          type="button"
          class="hbtn"
          class:active={showSettings}
          title="Provider settings"
          aria-label="Provider settings"
          onclick={() => (showSettings = !showSettings)}
        >
          <svg viewBox="0 0 24 24" width="15" height="15" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="3" /><path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 1 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 0 1-4 0v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 1 1-2.83-2.83l.06-.06a1.65 1.65 0 0 0 .33-1.82 1.65 1.65 0 0 0-1.51-1H3a2 2 0 0 1 0-4h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 1 1 2.83-2.83l.06.06a1.65 1.65 0 0 0 1.82.33H9a1.65 1.65 0 0 0 1-1.51V3a2 2 0 0 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 1 1 2.83 2.83l-.06.06a1.65 1.65 0 0 0-.33 1.82V9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 0 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1z" /></svg>
        </button>
        <button
          type="button"
          class="hbtn"
          title="Close (⌘⇧A)"
          aria-label="Close assistant"
          onclick={toggleChat}
        >
          <svg viewBox="0 0 24 24" width="15" height="15" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M18 6 6 18" /><path d="m6 6 12 12" /></svg>
        </button>
      </div>
    </header>

    {#if showSettings}
      <AiSettingsPanel onClose={() => (showSettings = false)} />
    {/if}

    <div class="messages" bind:this={messagesEl} onscroll={onScroll}>
      {#if entries.length === 0}
        {#if needsSetup && !showSettings}
          <div class="card setup">
            <p class="card-title">Set up your AI provider</p>
            <p class="card-body">
              Connect an OpenAI-compatible provider to chat with your vault. Local
              providers like Ollama need no API key.
            </p>
            <button type="button" class="card-btn" onclick={() => (showSettings = true)}>
              Open settings
            </button>
          </div>
        {:else}
          <div class="intro">
            <p class="intro-title">Ask about your notes</p>
            <p class="intro-body">
              Search, summarize, rewrite, organize, or create notes across your
              vault. The assistant asks before deleting anything.
            </p>
          </div>
        {/if}
      {:else}
        {#each entries as entry (entry.id)}
          {#if entry.kind === "user"}
            <div class="msg user"><div class="bubble">{entry.text}</div></div>
          {:else if entry.kind === "assistant"}
            <div class="msg assistant" class:streaming={entry.streaming}>
              {#if entry.reasoning}
                <ThinkingBlock
                  reasoning={entry.reasoning}
                  active={entry.streaming && !entry.text}
                />
              {/if}
              {#if entry.text}<MarkdownView source={entry.text} />{/if}
              {#if entry.streaming && entry.text}<span class="cursor" aria-hidden="true"></span>{/if}
            </div>
          {:else if entry.kind === "tool"}
            <ToolChip {entry} />
          {:else if entry.kind === "permission"}
            <PermissionCard {entry} />
          {:else if entry.kind === "error"}
            <p class="error-line">{entry.text}</p>
          {:else if entry.kind === "notice"}
            <p class="notice-line">{entry.text}</p>
          {/if}
        {/each}
      {/if}
    </div>

    {#if !pinned && entries.length > 0}
      <button type="button" class="jump" onclick={jumpToLatest}>Jump to latest ↓</button>
    {/if}

    <div class="composer">
      {#if noteOpen}
        <div class="chip-row">
          <button
            type="button"
            class="chip"
            class:off={!$contextEnabled}
            title={$contextEnabled
              ? "The assistant can see the open note. Click to exclude it from your next message."
              : "The open note is not shared. Click to include it."}
            onclick={() => contextEnabled.update((v) => !v)}
          >
            <svg viewBox="0 0 24 24" width="12" height="12" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z" /><path d="M14 2v6h6" /></svg>
            <span class="chip-text">{noteTitle}</span>
            <span class="chip-state">{$contextEnabled ? "shared" : "off"}</span>
          </button>
        </div>
      {/if}

      {#if noteOpen || hasConversation}
        <div class="quick-row">
          {#if noteOpen}
            <button type="button" class="quick" disabled={streaming || pendingPermission} onclick={() => runQuickAction(QUICK_ACTIONS.proofread)}>Fix spelling &amp; grammar</button>
            <button type="button" class="quick" disabled={streaming || pendingPermission} onclick={() => runQuickAction(QUICK_ACTIONS.improve)}>Improve writing</button>
            <button type="button" class="quick" disabled={streaming || pendingPermission} onclick={() => runQuickAction(QUICK_ACTIONS.summarize)}>Summarize</button>
          {/if}
          {#if hasConversation}
            <button type="button" class="quick" disabled={streaming || pendingPermission} onclick={() => runQuickAction(QUICK_ACTIONS.noteFromChat)}>Note from chat</button>
          {/if}
        </div>
      {/if}

      {#if pendingPermission}
        <p class="waiting">Waiting for your decision…</p>
      {/if}

      <div class="input-area">
        <textarea
          bind:this={textareaEl}
          bind:value={draft}
          class="input"
          rows="1"
          placeholder="Ask the assistant…"
          disabled={pendingPermission}
          oninput={autoGrow}
          onkeydown={onKeydown}
        ></textarea>
        {#if streaming}
          <button type="button" class="send stop" title="Stop" aria-label="Stop" onclick={() => void cancel()}>
            <svg viewBox="0 0 24 24" width="16" height="16" fill="currentColor"><rect x="6" y="6" width="12" height="12" rx="2" /></svg>
          </button>
        {:else}
          <button
            type="button"
            class="send"
            title="Send (Enter)"
            aria-label="Send"
            disabled={!draft.trim() || pendingPermission}
            onclick={() => void send()}
          >
            <svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M22 2 11 13" /><path d="M22 2 15 22l-4-9-9-4Z" /></svg>
          </button>
        {/if}
      </div>
    </div>
  </aside>
{:else}
  <button
    type="button"
    class="edge-toggle"
    title="Assistant (⌘⇧A)"
    aria-label="Open assistant"
    onclick={toggleChat}
  >
    <svg viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M12 3a3 3 0 0 0 3 3 3 3 0 0 0-3 3 3 3 0 0 0-3-3 3 3 0 0 0 3-3Z" /><path d="M19 11a2 2 0 0 0 2 2 2 2 0 0 0-2 2 2 2 0 0 0-2-2 2 2 0 0 0 2-2Z" /><path d="M5 13a2 2 0 0 0 2 2 2 2 0 0 0-2 2 2 2 0 0 0-2-2 2 2 0 0 0 2-2Z" /></svg>
  </button>
{/if}

<style>
  .chat-panel {
    flex-shrink: 0;
    width: 360px;
    max-width: 46vw;
    height: 100%;
    display: flex;
    flex-direction: column;
    min-height: 0;
    background-color: var(--bg-sidebar);
    border-left: 1px solid var(--border);
  }

  .panel-header {
    flex-shrink: 0;
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 10px 10px 10px 14px;
    border-bottom: 1px solid var(--border);
  }

  .title {
    font-size: 13px;
    font-weight: 600;
    color: var(--text);
  }

  .header-actions {
    display: flex;
    gap: 2px;
  }

  .hbtn {
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
  }
  .hbtn:hover:not(:disabled),
  .hbtn.active {
    background-color: var(--hover);
    color: var(--accent);
  }
  .hbtn:disabled {
    opacity: 0.4;
    cursor: default;
  }

  .messages {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    padding: 12px;
    display: flex;
    flex-direction: column;
    gap: 10px;
  }

  /* Messages */
  .msg.user {
    display: flex;
    justify-content: flex-end;
  }
  .msg.user .bubble {
    max-width: 85%;
    padding: 7px 11px;
    border-radius: 12px 12px 3px 12px;
    background-color: color-mix(in srgb, var(--accent) 16%, var(--bg-panel));
    color: var(--text);
    font-size: 13px;
    line-height: 1.5;
    white-space: pre-wrap;
    word-break: break-word;
  }

  .msg.assistant {
    position: relative;
  }

  .cursor {
    display: inline-block;
    width: 7px;
    height: 14px;
    margin-left: 1px;
    vertical-align: text-bottom;
    background-color: var(--accent);
    animation: blink 1s steps(2) infinite;
  }
  @keyframes blink {
    0%,
    100% {
      opacity: 1;
    }
    50% {
      opacity: 0;
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .cursor {
      animation: none;
    }
  }

  .error-line {
    margin: 0;
    padding: 2px 4px;
    font-size: 12px;
    color: var(--danger);
    word-break: break-word;
  }
  .notice-line {
    margin: 0;
    padding: 6px 8px;
    border-radius: 6px;
    background-color: var(--code-bg);
    font-size: 12px;
    color: var(--text-muted);
  }

  /* Empty / setup states */
  .intro,
  .card {
    margin: auto 0;
    text-align: center;
    padding: 8px;
  }
  .intro-title,
  .card-title {
    margin: 0 0 6px;
    font-size: 14px;
    font-weight: 600;
    color: var(--text);
  }
  .intro-body,
  .card-body {
    margin: 0;
    font-size: 12px;
    line-height: 1.5;
    color: var(--text-muted);
  }
  .card {
    border: 1px solid var(--border);
    border-radius: 10px;
    background-color: var(--bg-panel);
    padding: 16px;
  }
  .card-btn {
    margin-top: 12px;
    padding: 7px 14px;
    border: none;
    border-radius: 6px;
    background-color: var(--accent);
    color: var(--accent-contrast);
    font-family: var(--font-ui);
    font-size: 12px;
    font-weight: 500;
    cursor: pointer;
  }
  .card-btn:hover {
    background-color: var(--accent-hover);
  }

  .jump {
    position: absolute;
    align-self: center;
    margin-top: -40px;
    padding: 5px 12px;
    border: 1px solid var(--border);
    border-radius: 999px;
    background-color: var(--bg-panel);
    color: var(--text);
    font-family: var(--font-ui);
    font-size: 12px;
    box-shadow: var(--shadow-menu);
    cursor: pointer;
    z-index: 5;
  }
  .jump:hover {
    border-color: var(--accent);
    color: var(--accent);
  }

  /* Composer */
  .composer {
    flex-shrink: 0;
    border-top: 1px solid var(--border);
    padding: 10px 12px 12px;
  }

  .chip-row {
    margin-bottom: 8px;
  }
  .chip {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    max-width: 100%;
    padding: 3px 8px;
    border: 1px solid var(--border);
    border-radius: 999px;
    background-color: var(--bg-panel);
    color: var(--text-muted);
    font-family: var(--font-ui);
    font-size: 11px;
    cursor: pointer;
  }
  .chip:hover {
    border-color: var(--accent);
  }
  .chip.off {
    opacity: 0.6;
    text-decoration: line-through;
  }
  .chip-text {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    max-width: 180px;
    color: var(--text);
  }
  .chip.off .chip-text {
    color: var(--text-muted);
  }
  .chip-state {
    font-variant: small-caps;
    letter-spacing: 0.02em;
  }

  .quick-row {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    margin-bottom: 8px;
  }
  .quick {
    padding: 4px 9px;
    border: 1px solid var(--border);
    border-radius: 999px;
    background-color: var(--bg-panel);
    color: var(--text);
    font-family: var(--font-ui);
    font-size: 11px;
    cursor: pointer;
  }
  .quick:hover:not(:disabled) {
    border-color: var(--accent);
    color: var(--accent);
  }
  .quick:disabled {
    opacity: 0.5;
    cursor: default;
  }

  .waiting {
    margin: 0 0 8px;
    font-size: 11px;
    color: var(--text-muted);
  }

  .input-area {
    display: flex;
    align-items: flex-end;
    gap: 8px;
  }
  .input {
    flex: 1;
    min-width: 0;
    resize: none;
    max-height: 168px;
    padding: 8px 10px;
    border: 1px solid var(--border);
    border-radius: 8px;
    background-color: var(--bg-panel);
    color: var(--text);
    font-family: var(--font-ui);
    font-size: 13px;
    line-height: 1.45;
    outline: none;
  }
  .input:focus {
    border-color: var(--accent);
  }
  .input:disabled {
    opacity: 0.6;
  }
  .input::placeholder {
    color: var(--text-muted);
  }

  .send {
    flex-shrink: 0;
    display: flex;
    align-items: center;
    justify-content: center;
    width: 34px;
    height: 34px;
    border: none;
    border-radius: 8px;
    background-color: var(--accent);
    color: var(--accent-contrast);
    cursor: pointer;
  }
  .send:hover:not(:disabled) {
    background-color: var(--accent-hover);
  }
  .send:disabled {
    opacity: 0.45;
    cursor: default;
  }
  .send.stop {
    background-color: var(--danger);
    color: var(--danger-contrast);
  }

  /* Collapsed edge tab — pinned to the right edge, vertically centered. */
  .edge-toggle {
    position: fixed;
    top: 50%;
    right: 0;
    transform: translateY(-50%);
    z-index: 50;
    display: flex;
    align-items: center;
    justify-content: center;
    width: 30px;
    height: 46px;
    border: 1px solid var(--border);
    border-right: none;
    border-radius: 8px 0 0 8px;
    background-color: var(--bg-panel);
    color: var(--text-muted);
    box-shadow: var(--shadow-menu);
    cursor: pointer;
  }
  .edge-toggle:hover {
    color: var(--accent);
    border-color: var(--accent);
    background-color: var(--bg-sidebar);
  }
</style>
