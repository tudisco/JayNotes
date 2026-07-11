<script lang="ts">
  import { onMount } from "svelte";
  import { get } from "svelte/store";
  import Sidebar from "$lib/components/Sidebar.svelte";
  import EditorPane from "$lib/components/EditorPane.svelte";
  import QuickSwitcher from "$lib/components/QuickSwitcher.svelte";
  import ChatSidebar from "$lib/components/ChatSidebar.svelte";
  import { initVault, newNote, vaultError, vaultPath } from "$lib/stores/vault";
  import { initIndexEvents } from "$lib/stores/indexEvents";
  import { initAiOpenNote } from "$lib/stores/chat";
  import {
    quickSwitcherOpen,
    searchFocusNonce,
    sidebarMode,
    toggleChat,
  } from "$lib/stores/ui";

  onMount(() => {
    initVault();
    initIndexEvents();
    initAiOpenNote();
    window.addEventListener("keydown", onKeydown);
    return () => window.removeEventListener("keydown", onKeydown);
  });

  // Single global shortcut handler. Uses metaKey (macOS) or ctrlKey (portable).
  function onKeydown(event: KeyboardEvent): void {
    const mod = event.metaKey || event.ctrlKey;
    if (!mod) return;
    // While the quick switcher is open it owns the keyboard.
    if (get(quickSwitcherOpen)) return;

    const key = event.key.toLowerCase();
    if (key === "a" && event.shiftKey) {
      // Cmd/Ctrl+Shift+A toggles the AI assistant panel.
      event.preventDefault();
      toggleChat();
    } else if (key === "p" || key === "o") {
      event.preventDefault();
      quickSwitcherOpen.set(true);
    } else if (key === "f" && event.shiftKey) {
      event.preventDefault();
      sidebarMode.set("search");
      searchFocusNonce.update((n) => n + 1);
    } else if (key === "e" && !event.shiftKey) {
      event.preventDefault();
      sidebarMode.set("files");
    } else if (key === "n" && !event.shiftKey) {
      event.preventDefault();
      if (!get(vaultPath)) return;
      sidebarMode.set("files");
      newNote("").catch((e) => vaultError.set(String(e)));
    }
  }
</script>

<div class="app-shell">
  <Sidebar />
  <EditorPane />
  <ChatSidebar />
</div>

<QuickSwitcher />

<style>
  .app-shell {
    display: flex;
    width: 100vw;
    height: 100vh;
    overflow: hidden;
  }
</style>
