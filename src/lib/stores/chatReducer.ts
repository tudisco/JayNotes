// Pure chat state: the AiEvent -> message-list reducer plus history mapping.
//
// This module is deliberately free of Tauri and Svelte imports so it can be
// unit-tested in a plain node environment and reasoned about in isolation. The
// live store (`chat.ts`) wires these pure transitions to the IPC channel.
//
// A "turn" streams events in this shape (per the M11a backend):
//   tokens interleave; each tool call -> `toolCall` then maybe a
//   `permissionRequest` (awaited) then `toolResult`; the turn ends with `done`
//   (an `error` is always followed by `done`). Cancelled turns keep their
//   partial assistant text.

// ---------------------------------------------------------------------------
// Streaming events (mirror of the Rust `AiEvent`, camelCase on the wire)
// ---------------------------------------------------------------------------

export type AiEvent =
  | { type: "token"; text: string }
  | { type: "reasoning"; text: string }
  | { type: "toolCall"; id: string; name: string; argsSummary: string }
  | {
      type: "toolResult";
      id: string;
      summary: string;
      detail?: string;
      revisionId?: string;
    }
  | {
      type: "permissionRequest";
      requestId: string;
      action: string;
      paths: string[];
      /** Present for `move` actions: the destination folder ("" = vault root). */
      destination?: string;
      /** Present for folder deletions: the folder being removed. */
      folder?: string;
    }
  | { type: "done"; rounds: number; cancelled: boolean }
  | { type: "error"; message: string };

/** A locally-originated decision on a pending permission request. */
export interface PermissionDecisionAction {
  type: "permissionDecision";
  requestId: string;
  approved: boolean;
}

export type ChatAction = AiEvent | PermissionDecisionAction;

// ---------------------------------------------------------------------------
// Rendered message entries (the UI's discriminated union)
// ---------------------------------------------------------------------------

export interface UserEntry {
  kind: "user";
  id: string;
  text: string;
}

export interface AssistantEntry {
  kind: "assistant";
  id: string;
  text: string;
  /**
   * Accumulated model reasoning (chain-of-thought) for this turn, shown in a
   * collapsed row above the bubble. Empty when the model didn't reason.
   */
  reasoning: string;
  /** True while tokens are still streaming into this bubble. */
  streaming: boolean;
}

export interface ToolEntry {
  kind: "tool";
  /** The tool-call id (used to pair a `toolResult` with its `toolCall`). */
  id: string;
  name: string;
  argsSummary: string;
  status: "running" | "done";
  summary?: string;
  detail?: string;
  revisionId?: string;
}

export type PermissionStatus = "pending" | "approved" | "denied";

export interface PermissionEntry {
  kind: "permission";
  /** The permission request id. */
  id: string;
  action: string;
  paths: string[];
  destination?: string;
  folder?: string;
  status: PermissionStatus;
}

export interface ErrorEntry {
  kind: "error";
  id: string;
  text: string;
}

/** A transient inline notice (e.g. "a request is already in progress"). */
export interface NoticeEntry {
  kind: "notice";
  id: string;
  text: string;
}

export type ChatEntry =
  | UserEntry
  | AssistantEntry
  | ToolEntry
  | PermissionEntry
  | ErrorEntry
  | NoticeEntry;

// ---------------------------------------------------------------------------
// Id generation (monotonic; entries that don't carry a natural id get one)
// ---------------------------------------------------------------------------

let seq = 0;

/** A fresh, process-unique entry id. Exported for the store's direct pushes. */
export function nextId(): string {
  seq += 1;
  return `e${seq}`;
}

// ---------------------------------------------------------------------------
// Reducer
// ---------------------------------------------------------------------------

/** Seals the trailing streaming assistant bubble, if any. */
function sealStreaming(messages: ChatEntry[]): ChatEntry[] {
  const last = messages[messages.length - 1];
  if (last && last.kind === "assistant" && last.streaming) {
    return [...messages.slice(0, -1), { ...last, streaming: false }];
  }
  return messages;
}

/**
 * Applies one streaming event (or local permission decision) to the message
 * list, returning a new array. Pure: no side effects, no shared mutation.
 */
export function reduceChat(messages: ChatEntry[], action: ChatAction): ChatEntry[] {
  switch (action.type) {
    case "token": {
      const last = messages[messages.length - 1];
      if (last && last.kind === "assistant" && last.streaming) {
        return [
          ...messages.slice(0, -1),
          { ...last, text: last.text + action.text },
        ];
      }
      return [
        ...messages,
        { kind: "assistant", id: nextId(), text: action.text, reasoning: "", streaming: true },
      ];
    }

    case "reasoning": {
      // Reasoning may arrive before any visible content — accumulate it on the
      // current streaming bubble, or open a fresh (text-empty) one for it.
      const last = messages[messages.length - 1];
      if (last && last.kind === "assistant" && last.streaming) {
        return [
          ...messages.slice(0, -1),
          { ...last, reasoning: last.reasoning + action.text },
        ];
      }
      return [
        ...messages,
        { kind: "assistant", id: nextId(), text: "", reasoning: action.text, streaming: true },
      ];
    }

    case "toolCall": {
      const sealed = sealStreaming(messages);
      return [
        ...sealed,
        {
          kind: "tool",
          id: action.id,
          name: action.name,
          argsSummary: action.argsSummary,
          status: "running",
        },
      ];
    }

    case "toolResult":
      return messages.map((m) =>
        m.kind === "tool" && m.id === action.id
          ? {
              ...m,
              status: "done",
              summary: action.summary,
              detail: action.detail,
              revisionId: action.revisionId,
            }
          : m,
      );

    case "permissionRequest":
      return [
        ...sealStreaming(messages),
        {
          kind: "permission",
          id: action.requestId,
          action: action.action,
          paths: action.paths,
          destination: action.destination,
          folder: action.folder,
          status: "pending",
        },
      ];

    case "permissionDecision":
      return messages.map((m) =>
        m.kind === "permission" && m.id === action.requestId && m.status === "pending"
          ? { ...m, status: action.approved ? "approved" : "denied" }
          : m,
      );

    case "error":
      return [
        ...sealStreaming(messages),
        { kind: "error", id: nextId(), text: action.message },
      ];

    case "done":
      return sealStreaming(messages);

    default:
      return messages;
  }
}

// ---------------------------------------------------------------------------
// History mapping (rehydrate the UI from `ai_get_history`)
// ---------------------------------------------------------------------------

export interface DisplayToolCall {
  name: string;
  summary: string;
}

export interface DisplayMessage {
  role: "user" | "assistant" | "tool";
  content: string;
  toolCalls?: DisplayToolCall[];
  reasoning?: string;
}

/**
 * Turns persisted display history into rendered entries. Assistant tool-call
 * annotations are dropped in favour of the following `tool` messages, which
 * carry the human-readable result summaries; no revert links are offered for
 * historical tool activity (the revision ids aren't part of history).
 */
export function mapHistory(history: DisplayMessage[]): ChatEntry[] {
  const out: ChatEntry[] = [];
  for (const m of history) {
    if (m.role === "user") {
      out.push({ kind: "user", id: nextId(), text: m.content });
    } else if (m.role === "assistant") {
      if (m.content.trim() || m.reasoning) {
        out.push({
          kind: "assistant",
          id: nextId(),
          text: m.content,
          reasoning: m.reasoning ?? "",
          streaming: false,
        });
      }
    } else if (m.role === "tool") {
      out.push({
        kind: "tool",
        id: nextId(),
        name: "",
        argsSummary: "",
        status: "done",
        summary: m.content,
      });
    }
  }
  return out;
}
