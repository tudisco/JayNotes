import { describe, expect, it } from "vitest";
import {
  reduceChat,
  mapHistory,
  type ChatEntry,
  type AssistantEntry,
  type ToolEntry,
  type PermissionEntry,
} from "./chatReducer";

/** Folds a sequence of actions from an empty list. */
function run(...actions: Parameters<typeof reduceChat>[1][]): ChatEntry[] {
  return actions.reduce<ChatEntry[]>((acc, a) => reduceChat(acc, a), []);
}

describe("reduceChat — token accumulation", () => {
  it("accumulates consecutive tokens into one streaming bubble", () => {
    const msgs = run(
      { type: "token", text: "Hel" },
      { type: "token", text: "lo " },
      { type: "token", text: "world" },
    );
    expect(msgs).toHaveLength(1);
    const a = msgs[0] as AssistantEntry;
    expect(a.kind).toBe("assistant");
    expect(a.text).toBe("Hello world");
    expect(a.streaming).toBe(true);
  });

  it("seals the bubble on done, keeping partial text", () => {
    const msgs = run(
      { type: "token", text: "partial" },
      { type: "done", rounds: 1, cancelled: true },
    );
    const a = msgs[0] as AssistantEntry;
    expect(a.text).toBe("partial");
    expect(a.streaming).toBe(false);
  });

  it("starts a fresh bubble after a tool call splits the stream", () => {
    const msgs = run(
      { type: "token", text: "first" },
      { type: "toolCall", id: "t1", name: "search_notes", argsSummary: '"cats"' },
      { type: "toolResult", id: "t1", summary: "Searched — 1" },
      { type: "token", text: "second" },
    );
    const assistants = msgs.filter((m) => m.kind === "assistant") as AssistantEntry[];
    expect(assistants.map((a) => a.text)).toEqual(["first", "second"]);
    expect(assistants[0].streaming).toBe(false); // sealed by the tool call
    expect(assistants[1].streaming).toBe(true);
  });
});

describe("reduceChat — tool call/result pairing", () => {
  it("pairs a result to its call by id and marks it done", () => {
    const msgs = run(
      { type: "toolCall", id: "t1", name: "read_note", argsSummary: "a.md" },
      {
        type: "toolResult",
        id: "t1",
        summary: "Read a.md",
        detail: "42 lines",
        revisionId: "rev-1",
      },
    );
    const tool = msgs.find((m) => m.kind === "tool") as ToolEntry;
    expect(tool.status).toBe("done");
    expect(tool.summary).toBe("Read a.md");
    expect(tool.detail).toBe("42 lines");
    expect(tool.revisionId).toBe("rev-1");
  });

  it("leaves an unmatched result a no-op and keeps the call running", () => {
    const msgs = run(
      { type: "toolCall", id: "t1", name: "read_note", argsSummary: "a.md" },
      { type: "toolResult", id: "OTHER", summary: "nope" },
    );
    const tool = msgs.find((m) => m.kind === "tool") as ToolEntry;
    expect(tool.status).toBe("running");
    expect(tool.summary).toBeUndefined();
  });
});

describe("reduceChat — permission lifecycle", () => {
  it("creates a pending delete card then resolves it on decision", () => {
    let msgs = run({
      type: "permissionRequest",
      requestId: "perm-0",
      action: "delete",
      paths: ["a.md", "b.md"],
    });
    let card = msgs.find((m) => m.kind === "permission") as PermissionEntry;
    expect(card.status).toBe("pending");
    expect(card.action).toBe("delete");
    expect(card.paths).toEqual(["a.md", "b.md"]);

    msgs = reduceChat(msgs, {
      type: "permissionDecision",
      requestId: "perm-0",
      approved: true,
    });
    card = msgs.find((m) => m.kind === "permission") as PermissionEntry;
    expect(card.status).toBe("approved");
  });

  it("marks a denied decision", () => {
    let msgs = run({
      type: "permissionRequest",
      requestId: "perm-1",
      action: "delete",
      paths: ["x.md"],
    });
    msgs = reduceChat(msgs, {
      type: "permissionDecision",
      requestId: "perm-1",
      approved: false,
    });
    const card = msgs.find((m) => m.kind === "permission") as PermissionEntry;
    expect(card.status).toBe("denied");
  });

  it("carries destination for move actions", () => {
    const msgs = run({
      type: "permissionRequest",
      requestId: "perm-2",
      action: "move",
      paths: ["notes/a.md"],
      destination: "archive",
    });
    const card = msgs.find((m) => m.kind === "permission") as PermissionEntry;
    expect(card.action).toBe("move");
    expect(card.destination).toBe("archive");
  });

  it("carries folder for folder deletions", () => {
    const msgs = run({
      type: "permissionRequest",
      requestId: "perm-3",
      action: "delete",
      folder: "old",
      paths: ["old/a.md", "old/b.md"],
    });
    const card = msgs.find((m) => m.kind === "permission") as PermissionEntry;
    expect(card.folder).toBe("old");
    expect(card.paths).toHaveLength(2);
  });

  it("only resolves the matching pending card", () => {
    let msgs = run(
      { type: "permissionRequest", requestId: "p1", action: "delete", paths: ["a"] },
      { type: "permissionRequest", requestId: "p2", action: "delete", paths: ["b"] },
    );
    msgs = reduceChat(msgs, { type: "permissionDecision", requestId: "p2", approved: true });
    const cards = msgs.filter((m) => m.kind === "permission") as PermissionEntry[];
    expect(cards.find((c) => c.id === "p1")!.status).toBe("pending");
    expect(cards.find((c) => c.id === "p2")!.status).toBe("approved");
  });
});

describe("reduceChat — error + done", () => {
  it("appends an error entry and seals any streaming bubble", () => {
    const msgs = run(
      { type: "token", text: "half" },
      { type: "error", message: "Provider exploded" },
      { type: "done", rounds: 0, cancelled: false },
    );
    const assistant = msgs.find((m) => m.kind === "assistant") as AssistantEntry;
    expect(assistant.streaming).toBe(false);
    const err = msgs.find((m) => m.kind === "error");
    expect(err).toBeDefined();
    expect((err as { text: string }).text).toBe("Provider exploded");
  });
});

describe("mapHistory", () => {
  it("maps roles into rendered entries and drops empty assistant chatter", () => {
    const entries = mapHistory([
      { role: "user", content: "hi" },
      { role: "assistant", content: "", toolCalls: [{ name: "search_notes", summary: '"x"' }] },
      { role: "tool", content: "Searched — 1 result" },
      { role: "assistant", content: "Found it." },
    ]);
    expect(entries.map((e) => e.kind)).toEqual(["user", "tool", "assistant"]);
    const tool = entries[1] as ToolEntry;
    expect(tool.status).toBe("done");
    expect(tool.summary).toBe("Searched — 1 result");
    expect(tool.revisionId).toBeUndefined();
  });
});
