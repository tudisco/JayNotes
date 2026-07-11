// Small pieces of cross-component UI state driven by keyboard shortcuts.

import { writable } from "svelte/store";

/** Which view the sidebar shows below its tab strip. */
export type SidebarMode = "files" | "search" | "tags";

export const sidebarMode = writable<SidebarMode>("files");

/** How the Files tab renders the vault: folder tree, or a flat recent list. */
export type FilesView = "tree" | "recent";

const FILES_VIEW_KEY = "jaynotes:filesView";

function initialFilesView(): FilesView {
  if (typeof localStorage === "undefined") return "tree";
  return localStorage.getItem(FILES_VIEW_KEY) === "recent" ? "recent" : "tree";
}

/**
 * Whether the Files tab shows the folder tree (default) or a flat list of all
 * notes ordered newest-modified first. Persisted to localStorage so it survives
 * restarts.
 */
export const filesView = writable<FilesView>(initialFilesView());

if (typeof localStorage !== "undefined") {
  filesView.subscribe((v) => localStorage.setItem(FILES_VIEW_KEY, v));
}

/** Flips the Files tab between the folder tree and the recent list. */
export function toggleFilesView(): void {
  filesView.update((v) => (v === "tree" ? "recent" : "tree"));
}

/** Whether the Cmd+P quick switcher modal is open. */
export const quickSwitcherOpen = writable(false);

/**
 * Incremented whenever the search panel should grab focus (e.g. Cmd+Shift+F
 * pressed while it's already visible). The panel watches this and focuses its
 * input; the value itself is meaningless.
 */
export const searchFocusNonce = writable(0);
