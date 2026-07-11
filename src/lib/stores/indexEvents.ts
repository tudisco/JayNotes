// Backend index / file-watcher events.
//
// The Rust file watcher emits `vault-changed` (payload `{ paths: [...] }`)
// after each debounced batch of external filesystem changes it has folded into
// the search index. We react by refreshing the file tree, and by publishing the
// changed paths so the editor can reload the open note when it's safe to do so.
//
// Self-writes never reach here: vault.rs suppresses watcher events for paths the
// app itself just wrote, so this only fires for changes made outside JayNotes.

import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { writable } from "svelte/store";
import { refreshTree } from "./vault";

interface VaultChangedPayload {
  paths: string[];
}

/**
 * The most recent external vault change. `seq` increments on every event so
 * consumers can distinguish a fresh event from a re-render even when `paths`
 * repeats; `[]`/`0` is the initial (no-change-yet) state.
 */
export const vaultChanged = writable<{ paths: string[]; seq: number }>({
  paths: [],
  seq: 0,
});

let unlisten: UnlistenFn | null = null;
let seq = 0;

/** Registers the `vault-changed` listener once. Safe to call repeatedly. */
export async function initIndexEvents(): Promise<void> {
  if (unlisten) return;
  unlisten = await listen<VaultChangedPayload>("vault-changed", async (event) => {
    await refreshTree();
    vaultChanged.set({ paths: event.payload?.paths ?? [], seq: ++seq });
  });
}
