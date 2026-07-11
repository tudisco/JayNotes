// The live AI chat store: turns the pure reducer (chatReducer.ts) into a
// running conversation wired to the Tauri backend.
//
// Responsibilities:
//   * hold the rendered message list + streaming/turn state
//   * run a turn — flush the open editor, open an IPC Channel, fold every
//     AiEvent through the reducer as it streams in
//   * restore prior history on first open
//   * expose permission responses, cancel, new-chat, revert, and the canned
//     quick-action prompts
//   * after an AI-authored write, refresh the tree + nudge the editor to reload
//
// Provider settings live here too (masked; the raw key never leaves Rust).

import { Channel, invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { get, writable } from "svelte/store";

import {
  mapHistory,
  nextId,
  reduceChat,
  type AiEvent,
  type ChatEntry,
  type DisplayMessage,
} from "./chatReducer";
import { ensureVisible, fileTree, refreshTree, selected, type TreeNode } from "./vault";
import { flushOpenEditor, requestEditorReload } from "./editorBridge";

// ---------------------------------------------------------------------------
// Provider settings (masked view mirrored from the backend)
// ---------------------------------------------------------------------------

export interface AiSettingsMasked {
  preset: string;
  baseUrl: string;
  model: string;
  temperature?: number;
  apiKeySet: boolean;
  apiKeyLast4?: string;
}

/** Latest masked provider settings, or null until first loaded. */
export const aiSettings = writable<AiSettingsMasked | null>(null);

export async function loadAiSettings(): Promise<void> {
  try {
    aiSettings.set(await invoke<AiSettingsMasked>("get_ai_settings"));
  } catch {
    // Leave null; the setup card handles the unconfigured state.
  }
}

// ---------------------------------------------------------------------------
// Conversation state
// ---------------------------------------------------------------------------

export const chatMessages = writable<ChatEntry[]>([]);

/** True while a turn is streaming. */
export const chatStreaming = writable(false);

/**
 * Whether the open note is attached to the next message. Defaults on; resets to
 * on whenever the open note changes (see the subscription below).
 */
export const contextEnabled = writable(true);

let historyLoaded = false;

/** Tool names that mutate the vault — used to decide on a post-turn reload. */
const WRITE_TOOLS = new Set([
  "create_note",
  "update_note",
  "append_to_note",
  "rename_note",
  "move_note",
  "move_folder",
  "create_folder",
  "delete_note",
  "delete_folder",
  "batch_delete",
]);

/** Set during a turn if any vault-mutating tool ran, so we reload afterwards. */
let turnDidWrite = false;

// Reset the context toggle back on whenever the open note changes.
let lastNotePath: string | null = null;
selected.subscribe((sel) => {
  const path = sel && !sel.isDir ? sel.path : null;
  if (path !== lastNotePath) {
    lastNotePath = path;
    contextEnabled.set(true);
  }
});

// ---------------------------------------------------------------------------
// Loading / clearing
// ---------------------------------------------------------------------------

/** Loads persisted history once (on the sidebar's first open). */
export async function ensureHistoryLoaded(): Promise<void> {
  if (historyLoaded) return;
  historyLoaded = true;
  try {
    const history = await invoke<DisplayMessage[]>("ai_get_history");
    if (history.length > 0) chatMessages.set(mapHistory(history));
  } catch {
    // A failed restore just starts an empty conversation.
  }
}

/** Clears the conversation (memory + disk). */
export async function newChat(): Promise<void> {
  try {
    await invoke("ai_new_chat");
  } catch {
    // Even if the backend clear fails, drop the local view.
  }
  chatMessages.set([]);
}

// ---------------------------------------------------------------------------
// Sending a turn
// ---------------------------------------------------------------------------

function apply(event: AiEvent): void {
  chatMessages.update((m) => reduceChat(m, event));

  if (event.type === "toolCall" && WRITE_TOOLS.has(event.name)) turnDidWrite = true;
  if (event.type === "toolResult" && event.revisionId) turnDidWrite = true;

  if (event.type === "done") {
    chatStreaming.set(false);
    if (turnDidWrite) {
      void refreshTree().catch(() => {});
      requestEditorReload();
    }
  }
}

/**
 * Runs one chat turn for `text`. Flushes the open editor first so the model
 * reads the user's latest content, attaches the open note when the context
 * toggle is on, and streams events through the reducer. A concurrent-send
 * rejection surfaces as an inline notice rather than an error.
 */
export async function sendMessage(text: string): Promise<void> {
  const body = text.trim();
  if (!body || get(chatStreaming)) return;

  try {
    await flushOpenEditor();
  } catch {
    // A flush failure shouldn't block the message; the editor surfaces its own.
  }

  const sel = get(selected);
  const currentNote = get(contextEnabled) && sel && !sel.isDir ? sel.path : undefined;

  chatMessages.update((m) => [...m, { kind: "user", id: nextId(), text: body }]);
  chatStreaming.set(true);
  turnDidWrite = false;

  const channel = new Channel<AiEvent>();
  channel.onmessage = apply;

  try {
    await invoke("ai_chat_send", { userMessage: body, currentNote, channel });
  } catch (e) {
    chatMessages.update((m) => [...m, { kind: "notice", id: nextId(), text: String(e) }]);
  } finally {
    chatStreaming.set(false);
  }
}

/** Aborts the in-flight turn (partial reply is kept). */
export async function cancel(): Promise<void> {
  try {
    await invoke("ai_cancel");
  } catch {
    // Nothing in flight, or already finishing.
  }
}

/** Answers a pending permission request and reflects the decision in the card. */
export async function respondPermission(requestId: string, approved: boolean): Promise<void> {
  chatMessages.update((m) =>
    reduceChat(m, { type: "permissionDecision", requestId, approved }),
  );
  try {
    await invoke("ai_permission_respond", { requestId, approved });
  } catch {
    // The turn will time out and deny on its own if this never lands.
  }
}

/**
 * Reverts a note to an AI-write snapshot. Returns the reverted path so the UI
 * can confirm; reloads the editor when the reverted note is the one open.
 */
export async function revert(revisionId: string): Promise<string> {
  const path = await invoke<string>("ai_revert", { revisionId });
  await refreshTree().catch(() => {});
  const sel = get(selected);
  if (sel && !sel.isDir && sel.path === path) requestEditorReload();
  return path;
}

// ---------------------------------------------------------------------------
// Opening notes (clickable links in chat + the ai-open-note tool event)
// ---------------------------------------------------------------------------

/** True if `path` is an exact note path in the current tree. */
function pathInTree(path: string): boolean {
  const walk = (n: TreeNode): boolean =>
    (!n.isDir && n.path === path) || n.children.some(walk);
  const root = get(fileTree);
  return root ? walk(root) : false;
}

/** Opens `path` in the editor, expanding ancestors so it's visible. */
function openNotePath(path: string): void {
  ensureVisible(path);
  selected.set({ path, isDir: false });
}

/** Shows a short-lived muted notice (auto-dismissed), e.g. an unresolved link. */
function transientNotice(text: string): void {
  const id = nextId();
  chatMessages.update((m) => [...m, { kind: "notice", id, text }]);
  setTimeout(() => {
    chatMessages.update((m) => m.filter((e) => e.id !== id));
  }, 3500);
}

/**
 * Opens a note referenced by a clickable chat link. Prefers an exact rel-path
 * hit in the tree; otherwise resolves the bare name (sans `.md`) via the index;
 * a miss surfaces a transient "note not found" notice rather than a modal.
 */
export async function openNoteLink(target: string): Promise<void> {
  const path = target.replace(/^\.?\//, "");
  if (pathInTree(path)) {
    openNotePath(path);
    return;
  }
  const name = (path.split("/").pop() ?? path).replace(/\.md$/i, "");
  try {
    const resolved = await invoke<string | null>("resolve_note", { name });
    if (resolved) {
      openNotePath(resolved);
      return;
    }
  } catch {
    // Fall through to the not-found notice.
  }
  transientNotice(`Note not found: ${name}`);
}

let aiOpenNoteUnlisten: UnlistenFn | null = null;

/**
 * Registers the one-shot listener for the backend `ai-open-note` event (fired
 * by the `open_note` tool) → opens the note in the editor. Safe to call
 * repeatedly. Mirrors the listen-once pattern in indexEvents.ts.
 */
export async function initAiOpenNote(): Promise<void> {
  if (aiOpenNoteUnlisten) return;
  aiOpenNoteUnlisten = await listen<{ path: string }>("ai-open-note", (event) => {
    const path = event.payload?.path;
    if (path) openNotePath(path);
  });
}

// ---------------------------------------------------------------------------
// Canned quick-action prompts
// ---------------------------------------------------------------------------

export const QUICK_ACTIONS = {
  proofread:
    "Proofread the current note: fix spelling, grammar and punctuation. Keep the meaning, " +
    "formatting, frontmatter and code blocks unchanged. Apply the changes with update_note.",
  improve:
    "Improve the writing in the current note: tighten wording, improve flow and clarity, and " +
    "fix awkward phrasing. Preserve the meaning, structure, frontmatter and any code blocks. " +
    "Apply the changes with update_note.",
  summarize:
    "Add a concise summary to the current note. Read it first, then prepend a short " +
    '"## Summary" section (2–4 sentences) capturing the key points, leaving the rest ' +
    "unchanged. Apply the change with update_note.",
  noteFromChat:
    "Turn our conversation so far into a well-structured note. Choose a clear filename and " +
    "create it with create_note, then tell me where you put it.",
} as const;
