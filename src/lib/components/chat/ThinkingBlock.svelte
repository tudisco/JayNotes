<!--
  ThinkingBlock.svelte — a slim, collapsed-by-default row above an assistant
  bubble that surfaces a reasoning model's chain-of-thought.

  While reasoning streams and no visible answer has begun (`active`), the label
  shimmers "Thinking…". Once the answer starts or the turn ends it reads
  "Thought for a moment" and stays click-to-expand. The reasoning is rendered as
  plain (escaped) text in a muted, scrollable box — never as live markup.
-->
<script lang="ts">
  let { reasoning, active }: { reasoning: string; active: boolean } = $props();
  let open = $state(false);
</script>

<div class="think">
  <button
    type="button"
    class="think-toggle"
    aria-expanded={open}
    onclick={() => (open = !open)}
  >
    <span class="spark" aria-hidden="true">✻</span>
    <span class="label" class:shimmer={active}>
      {active ? "Thinking…" : "Thought for a moment"}
    </span>
    <span class="chev" class:open aria-hidden="true">›</span>
  </button>
  {#if open}
    <div class="think-body">{reasoning}</div>
  {/if}
</div>

<style>
  .think {
    margin-bottom: 4px;
  }
  .think-toggle {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    padding: 2px 4px;
    border: none;
    background: transparent;
    color: var(--text-muted);
    font-family: var(--font-ui);
    font-size: 11.5px;
    cursor: pointer;
    border-radius: 5px;
  }
  .think-toggle:hover {
    color: var(--text);
  }
  .spark {
    font-size: 11px;
    opacity: 0.8;
  }
  .chev {
    display: inline-block;
    transition: transform 0.15s ease;
    opacity: 0.7;
  }
  .chev.open {
    transform: rotate(90deg);
  }

  /* Shimmer while reasoning is still streaming with no answer yet. */
  .shimmer {
    background: linear-gradient(
      90deg,
      var(--text-muted) 0%,
      var(--text) 50%,
      var(--text-muted) 100%
    );
    background-size: 200% 100%;
    -webkit-background-clip: text;
    background-clip: text;
    color: transparent;
    animation: think-shimmer 1.6s linear infinite;
  }
  @keyframes think-shimmer {
    0% {
      background-position: 200% 0;
    }
    100% {
      background-position: -200% 0;
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .shimmer {
      animation: none;
    }
  }

  .think-body {
    margin-top: 4px;
    padding: 8px 10px;
    max-height: 200px;
    overflow-y: auto;
    border-left: 2px solid var(--border);
    background-color: var(--code-bg);
    border-radius: 0 6px 6px 0;
    color: var(--text-muted);
    font-size: 12px;
    line-height: 1.5;
    white-space: pre-wrap;
    word-break: break-word;
  }
</style>
