<!--
  PermissionCard.svelte — a prominent approval card for a gated tool action.

  Handles the generalized permission request: plain delete, folder delete,
  move, and (defensively) any future action, which degrades to a neutral
  "requests permission" card. Only deletes get a danger-styled Allow button.
  Once answered, the card collapses to a one-line record of the decision.
-->
<script lang="ts">
  import type { PermissionEntry } from "$lib/stores/chatReducer";
  import { respondPermission } from "$lib/stores/chat";

  let { entry }: { entry: PermissionEntry } = $props();

  const PREVIEW = 10;

  let expanded = $state(false);
  let isDelete = $derived(entry.action === "delete");
  let visiblePaths = $derived(
    expanded ? entry.paths : entry.paths.slice(0, PREVIEW),
  );
  let hiddenCount = $derived(Math.max(0, entry.paths.length - PREVIEW));

  let heading = $derived(buildHeading(entry));

  function buildHeading(e: PermissionEntry): string {
    const n = e.paths.length;
    const notes = `${n} note${n === 1 ? "" : "s"}`;
    if (e.action === "delete") {
      if (e.folder) {
        return `Assistant wants to delete the folder “${e.folder}” and ${notes} inside:`;
      }
      return `Assistant wants to delete ${notes}:`;
    }
    if (e.action === "move") {
      return `Assistant wants to move ${notes}:`;
    }
    return `Assistant requests permission: ${e.action}`;
  }

  let destinationLabel = $derived(
    entry.destination === "" ? "vault root" : entry.destination,
  );

  function decide(approved: boolean): void {
    void respondPermission(entry.id, approved);
  }
</script>

{#if entry.status === "pending"}
  <div class="perm" class:danger={isDelete} role="group" aria-label="Permission request">
    <p class="heading">{heading}</p>

    <ul class="paths">
      {#each visiblePaths as p (p)}
        <li>{p}</li>
      {/each}
    </ul>
    {#if hiddenCount > 0 && !expanded}
      <button type="button" class="more" onclick={() => (expanded = true)}>
        …and {hiddenCount} more
      </button>
    {/if}

    {#if entry.action === "move"}
      <p class="dest">→ {destinationLabel}</p>
    {/if}

    <div class="actions">
      <button type="button" class="btn" onclick={() => decide(false)}>Deny</button>
      <button
        type="button"
        class="btn allow"
        class:danger={isDelete}
        onclick={() => decide(true)}
      >
        Allow
      </button>
    </div>
  </div>
{:else}
  <div class="record" class:approved={entry.status === "approved"}>
    <span class="dot" aria-hidden="true"></span>
    {entry.status === "approved" ? "Allowed" : "Denied"}
    {entry.action}
    {#if entry.folder}folder “{entry.folder}”{:else}{entry.paths.length} note{entry.paths.length === 1 ? "" : "s"}{/if}
  </div>
{/if}

<style>
  .perm {
    padding: 12px;
    border: 1px solid var(--border);
    border-radius: 10px;
    background-color: var(--bg-panel);
  }

  .perm.danger {
    border-color: color-mix(in srgb, var(--danger) 55%, var(--border));
    background-color: color-mix(in srgb, var(--danger) 7%, var(--bg-panel));
  }

  .heading {
    margin: 0 0 8px;
    font-size: 13px;
    font-weight: 600;
    color: var(--text);
    line-height: 1.4;
  }

  .paths {
    list-style: none;
    margin: 0;
    padding: 6px 8px;
    max-height: 132px;
    overflow-y: auto;
    border-radius: 6px;
    background-color: var(--code-bg);
    font-family: var(--font-mono);
    font-size: 12px;
    color: var(--text);
  }
  .paths li {
    padding: 1px 0;
    word-break: break-all;
  }

  .more {
    margin: 6px 0 0;
    padding: 0;
    border: none;
    background: none;
    color: var(--accent);
    font-size: 12px;
    cursor: pointer;
  }
  .more:hover {
    text-decoration: underline;
  }

  .dest {
    margin: 8px 0 0;
    font-size: 12px;
    color: var(--text-muted);
    word-break: break-word;
  }

  .actions {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
    margin-top: 12px;
  }

  .btn {
    padding: 6px 14px;
    border: 1px solid var(--border);
    border-radius: 6px;
    background-color: var(--bg-panel);
    color: var(--text);
    font-family: var(--font-ui);
    font-size: 12px;
    font-weight: 500;
    cursor: pointer;
  }
  .btn:hover {
    background-color: var(--hover);
  }

  .btn.allow {
    border-color: transparent;
    background-color: var(--accent);
    color: var(--accent-contrast);
  }
  .btn.allow:hover {
    background-color: var(--accent-hover);
  }
  .btn.allow.danger {
    background-color: var(--danger);
    color: var(--danger-contrast);
  }
  .btn.allow.danger:hover {
    background-color: color-mix(in srgb, var(--danger) 82%, black);
  }

  .record {
    display: flex;
    align-items: center;
    gap: 7px;
    padding: 4px 8px;
    font-size: 12px;
    color: var(--text-muted);
  }
  .dot {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background-color: var(--text-muted);
    flex-shrink: 0;
  }
  .record.approved .dot {
    background-color: var(--accent);
  }
</style>
