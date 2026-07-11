// A tiny bridge between the AI chat and the open editor instance.
//
// EditorPane owns the live Crepe editor, so two capabilities the chat needs are
// registered here rather than threaded through props:
//
//  * flush — persist the open note before the assistant reads it from disk, so
//    the model always sees the user's latest text (mirrors the editor's own
//    flush-on-switch/blur behaviour).
//  * reload — after the assistant writes to a note, the file-watcher stays quiet
//    (self-writes are suppressed), so the editor won't auto-reload. Bumping this
//    nonce lets EditorPane re-read the open note the same way it reacts to an
//    external change (and, as there, only when the buffer isn't dirty).

import { writable } from "svelte/store";

/** Bumped to ask EditorPane to re-read the open note from disk (if safe). */
export const editorReloadNonce = writable(0);

/** Requests a reload of the open note after an AI-authored write/revert. */
export function requestEditorReload(): void {
  editorReloadNonce.update((n) => n + 1);
}

type FlushFn = () => Promise<void>;

let flushFn: FlushFn | null = null;

/**
 * Registers the open editor's flush. Returns an unregister function; EditorPane
 * calls it on teardown so a stale closure can't be invoked after unmount.
 */
export function registerEditorFlush(fn: FlushFn): () => void {
  flushFn = fn;
  return () => {
    if (flushFn === fn) flushFn = null;
  };
}

/** Flushes the open editor, if one is mounted. Safe to call when none is. */
export async function flushOpenEditor(): Promise<void> {
  if (flushFn) await flushFn();
}
