<!--
  PropertiesBar.svelte — tag + metadata editor for a note's frontmatter.

  Given the note's verbatim frontmatter string, it parses out `tags` + `fields`
  (via metadata.ts) and lets the user edit them. On any edit it re-serializes
  and emits a new frontmatter string through `onChange`; EditorPane stores that
  as the single shared frontmatter and asks the Editor to persist it.

  Data safety:
   - It never emits unless the user edits something, so a note whose properties
     are untouched round-trips byte-for-byte.
   - Untouched fields keep their original parsed YAML value (types preserved);
     only a field the user actually edits becomes the typed string.
   - Malformed YAML disables editing entirely (a read-only notice) so we never
     overwrite frontmatter we couldn't understand.
-->
<script lang="ts">
  import { parseFrontmatter, serializeFrontmatter } from "$lib/utils/metadata";

  let {
    frontmatter,
    onChange,
  }: {
    frontmatter: string | null;
    onChange: (fm: string | null) => void;
  } = $props();

  interface FieldRow {
    id: number;
    key: string;
    /** String shown in the value input. */
    raw: string;
    /** Original parsed value, used verbatim while `edited` is false. */
    original: unknown;
    /** True once the user has typed into the value (raw replaces original). */
    edited: boolean;
  }

  let uid = 0;

  let tags = $state<string[]>([]);
  let fields = $state<FieldRow[]>([]);
  let parseError = $state(false);
  let expanded = $state(false);
  let tagDraft = $state("");

  /** Identity of the frontmatter we last initialized from / emitted, so our own
   *  emissions don't re-initialize the model and clobber in-progress edits. */
  let syncedFrontmatter: string | null | undefined = undefined;

  function formatValue(v: unknown): string {
    if (typeof v === "string") return v;
    if (v === null || v === undefined) return "";
    if (typeof v === "number" || typeof v === "boolean") return String(v);
    return JSON.stringify(v);
  }

  function initFrom(fm: string | null): void {
    const parsed = parseFrontmatter(fm);
    parseError = parsed.parseError;
    tags = [...parsed.tags];
    fields = Object.entries(parsed.fields).map(([key, value]) => ({
      id: ++uid,
      key,
      raw: formatValue(value),
      original: value,
      edited: false,
    }));
    tagDraft = "";
    syncedFrontmatter = fm;
  }

  // Re-initialize when the frontmatter changes externally (note switch, async
  // load) but NOT when the new value is one we just emitted ourselves.
  $effect(() => {
    const fm = frontmatter;
    if (fm !== syncedFrontmatter) {
      initFrom(fm);
    }
  });

  function emit(): void {
    const outFields: Record<string, unknown> = {};
    for (const row of fields) {
      const key = row.key.trim();
      if (!key) continue; // rows without a key don't serialize
      outFields[key] = row.edited ? row.raw : row.original;
    }
    const fm = serializeFrontmatter(tags, outFields);
    syncedFrontmatter = fm;
    onChange(fm);
  }

  // --- tags -----------------------------------------------------------------

  function addTagFromDraft(): void {
    const value = tagDraft.trim();
    tagDraft = "";
    if (!value) return;
    if (tags.includes(value)) return;
    tags = [...tags, value];
    emit();
  }

  function removeTag(index: number): void {
    tags = tags.filter((_, i) => i !== index);
    emit();
  }

  function onTagKey(event: KeyboardEvent): void {
    if (event.key === "Enter" || event.key === ",") {
      event.preventDefault();
      addTagFromDraft();
    } else if (
      event.key === "Backspace" &&
      tagDraft === "" &&
      tags.length > 0
    ) {
      removeTag(tags.length - 1);
    }
  }

  // --- fields ---------------------------------------------------------------

  function addRow(): void {
    fields = [
      ...fields,
      { id: ++uid, key: "", raw: "", original: "", edited: true },
    ];
    expanded = true;
  }

  function removeRow(id: number): void {
    fields = fields.filter((row) => row.id !== id);
    emit();
  }

  function onKeyInput(row: FieldRow, value: string): void {
    row.key = value;
    emit();
  }

  function onValueInput(row: FieldRow, value: string): void {
    row.raw = value;
    row.edited = true;
    emit();
  }

  let hasProps = $derived(tags.length > 0 || fields.length > 0);
</script>

<div class="props-bar">
  <div class="props-inner">
    {#if parseError}
      <p class="parse-error" role="status">
        Frontmatter couldn't be parsed — properties editing is disabled to avoid
        overwriting it. Fix the YAML in the file to re-enable.
      </p>
    {:else if !expanded}
      <div class="collapsed">
        {#if hasProps}
          <div class="chip-row">
            {#each tags as tag (tag)}
              <span class="chip">#{tag}</span>
            {/each}
            <button
              type="button"
              class="toggle"
              aria-expanded="false"
              onclick={() => (expanded = true)}
            >
              <span class="chevron">›</span> Properties
            </button>
          </div>
        {:else}
          <button
            type="button"
            class="add-props"
            onclick={() => (expanded = true)}
          >
            + Add properties
          </button>
        {/if}
      </div>
    {:else}
      <div class="expanded">
        <div class="section">
          <div class="section-head">
            <span class="section-label">Tags</span>
            <button
              type="button"
              class="toggle"
              aria-expanded="true"
              onclick={() => (expanded = false)}
            >
              <span class="chevron open">›</span> Collapse
            </button>
          </div>
          <div class="tag-editor">
            {#each tags as tag, i (tag)}
              <span class="chip removable">
                #{tag}
                <button
                  type="button"
                  class="chip-x"
                  aria-label={`Remove tag ${tag}`}
                  onclick={() => removeTag(i)}>×</button
                >
              </span>
            {/each}
            <input
              class="tag-input"
              type="text"
              placeholder="Add tag…"
              spellcheck="false"
              bind:value={tagDraft}
              onkeydown={onTagKey}
              onblur={addTagFromDraft}
            />
          </div>
        </div>

        <div class="section">
          <span class="section-label">Fields</span>
          <div class="fields">
            {#each fields as row (row.id)}
              <div class="field-row">
                <input
                  class="field-key"
                  type="text"
                  placeholder="key"
                  spellcheck="false"
                  value={row.key}
                  oninput={(e) => onKeyInput(row, e.currentTarget.value)}
                />
                <input
                  class="field-value"
                  type="text"
                  placeholder="value"
                  spellcheck="false"
                  value={row.raw}
                  oninput={(e) => onValueInput(row, e.currentTarget.value)}
                />
                <button
                  type="button"
                  class="row-x"
                  aria-label="Remove field"
                  onclick={() => removeRow(row.id)}>×</button
                >
              </div>
            {/each}
            <button type="button" class="add-row" onclick={addRow}>
              + Add field
            </button>
          </div>
        </div>
      </div>
    {/if}
  </div>
</div>

<style>
  .props-bar {
    flex-shrink: 0;
    width: 100%;
  }

  .props-inner {
    max-width: 46rem;
    width: 100%;
    margin: 0 auto;
    padding: 0 16px;
    font-family: var(--font-ui);
  }

  /* Collapsed row -------------------------------------------------------- */
  .collapsed {
    min-height: 24px;
    display: flex;
    align-items: center;
  }

  .chip-row {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px;
  }

  .chip {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    padding: 1px 8px;
    border-radius: 999px;
    background-color: color-mix(in srgb, var(--accent) 16%, transparent);
    color: var(--accent);
    border: 1px solid color-mix(in srgb, var(--accent) 28%, transparent);
    font-size: 12px;
    line-height: 1.6;
    white-space: nowrap;
  }

  .chip.removable {
    padding-right: 3px;
  }

  .chip-x {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 15px;
    height: 15px;
    padding: 0;
    border: none;
    border-radius: 50%;
    background: transparent;
    color: var(--accent);
    font-size: 13px;
    line-height: 1;
    cursor: pointer;
    opacity: 0.7;
  }

  .chip-x:hover {
    opacity: 1;
    background-color: color-mix(in srgb, var(--accent) 22%, transparent);
  }

  .toggle {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    padding: 1px 6px;
    border: none;
    border-radius: 4px;
    background: transparent;
    color: var(--text-muted);
    font-size: 12px;
    cursor: pointer;
  }

  .toggle:hover {
    color: var(--text);
    background-color: var(--hover);
  }

  .chevron {
    display: inline-block;
    transition: transform 0.15s ease;
  }

  .chevron.open {
    transform: rotate(90deg);
  }

  .add-props {
    padding: 1px 6px;
    border: none;
    border-radius: 4px;
    background: transparent;
    color: var(--text-muted);
    font-size: 12px;
    cursor: pointer;
  }

  .add-props:hover {
    color: var(--text);
    background-color: var(--hover);
  }

  /* Expanded editor ------------------------------------------------------ */
  .expanded {
    display: flex;
    flex-direction: column;
    gap: 12px;
    padding: 8px 0 10px;
    border-bottom: 1px solid var(--border);
  }

  .section {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }

  .section-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
  }

  .section-label {
    font-size: 11px;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    color: var(--text-muted);
  }

  .tag-editor {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px;
  }

  .tag-input {
    flex: 1;
    min-width: 120px;
    padding: 3px 6px;
    border: 1px solid transparent;
    border-radius: 4px;
    background: transparent;
    color: var(--text);
    font-family: inherit;
    font-size: 13px;
    outline: none;
  }

  .tag-input:focus {
    border-color: var(--border);
    background-color: var(--bg-app);
  }

  .fields {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }

  .field-row {
    display: flex;
    align-items: center;
    gap: 6px;
  }

  .field-key,
  .field-value {
    padding: 4px 8px;
    border: 1px solid var(--border);
    border-radius: 4px;
    background-color: var(--bg-app);
    color: var(--text);
    font-family: inherit;
    font-size: 13px;
    outline: none;
  }

  .field-key {
    flex: 0 0 30%;
    min-width: 0;
    font-weight: 500;
    color: var(--text-muted);
  }

  .field-value {
    flex: 1;
    min-width: 0;
  }

  .field-key:focus,
  .field-value:focus {
    border-color: var(--accent);
  }

  .row-x {
    flex-shrink: 0;
    width: 22px;
    height: 22px;
    padding: 0;
    border: none;
    border-radius: 4px;
    background: transparent;
    color: var(--text-muted);
    font-size: 16px;
    line-height: 1;
    cursor: pointer;
  }

  .row-x:hover {
    color: var(--text);
    background-color: var(--hover);
  }

  .add-row {
    align-self: flex-start;
    padding: 3px 8px;
    border: none;
    border-radius: 4px;
    background: transparent;
    color: var(--accent);
    font-size: 12px;
    font-weight: 500;
    cursor: pointer;
  }

  .add-row:hover {
    background-color: color-mix(in srgb, var(--accent) 12%, transparent);
  }

  .parse-error {
    margin: 6px 0;
    padding: 8px 10px;
    border: 1px solid color-mix(in srgb, var(--danger) 50%, var(--border));
    border-radius: 6px;
    background-color: color-mix(in srgb, var(--danger) 8%, transparent);
    color: var(--text-muted);
    font-size: 12px;
    line-height: 1.5;
  }
</style>
