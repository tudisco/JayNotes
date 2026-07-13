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

/** Whether the header vault-switcher popover is open (also openable from Settings). */
export const vaultSwitcherOpen = writable(false);

const CHAT_OPEN_KEY = "jaynotes:chatOpen";

function initialChatOpen(): boolean {
  if (typeof localStorage === "undefined") return false;
  return localStorage.getItem(CHAT_OPEN_KEY) === "true";
}

/**
 * Whether the AI assistant sidebar is expanded. Collapsed by default; the state
 * is persisted to localStorage so it survives restarts.
 */
export const chatOpen = writable<boolean>(initialChatOpen());

if (typeof localStorage !== "undefined") {
  chatOpen.subscribe((v) => localStorage.setItem(CHAT_OPEN_KEY, String(v)));
}

/** Toggles the AI assistant sidebar (Cmd+Shift+A). */
export function toggleChat(): void {
  chatOpen.update((v) => !v);
}

/**
 * Incremented whenever the search panel should grab focus (e.g. Cmd+Shift+F
 * pressed while it's already visible). The panel watches this and focuses its
 * input; the value itself is meaningless.
 */
export const searchFocusNonce = writable(0);

/** How wide the note editor column is. */
export type EditorWidth = "full" | "comfortable";

const EDITOR_WIDTH_KEY = "jaynotes:editorWidth";

function initialEditorWidth(): EditorWidth {
  if (typeof localStorage === "undefined") return "full";
  return localStorage.getItem(EDITOR_WIDTH_KEY) === "comfortable"
    ? "comfortable"
    : "full";
}

/**
 * Notes grow with the window by default ("full", generous side padding);
 * "comfortable" opts into the classic centered 46rem reading column. Applied
 * as `data-editor-width` on <html> so plain CSS switches both the editor
 * content and the note header. Persisted across restarts.
 */
export const editorWidth = writable<EditorWidth>(initialEditorWidth());

if (typeof localStorage !== "undefined") {
  editorWidth.subscribe((v) => {
    localStorage.setItem(EDITOR_WIDTH_KEY, v);
    document.documentElement.dataset.editorWidth = v;
  });
}

/** Flips between full-width and comfortable reading column. */
export function toggleEditorWidth(): void {
  editorWidth.update((v) => (v === "full" ? "comfortable" : "full"));
}
