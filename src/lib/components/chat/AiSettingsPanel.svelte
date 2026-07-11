<!--
  AiSettingsPanel.svelte — provider configuration, inline within the chat panel.

  Preset picks a base URL (OpenAI / OpenRouter / Ollama / Custom); the API key is
  write-only (empty submit keeps the stored key); "Fetch" lists models from the
  provider. Save persists through `set_ai_settings` and refreshes the masked view.
-->
<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { aiSettings, loadAiSettings } from "$lib/stores/chat";

  let { onClose }: { onClose: () => void } = $props();

  const PRESETS: Record<string, string> = {
    openai: "https://api.openai.com/v1",
    openrouter: "https://openrouter.ai/api/v1",
    ollama: "http://localhost:11434/v1",
    custom: "",
  };

  let preset = $state("openai");
  let baseUrl = $state("");
  let apiKey = $state("");
  let model = $state("");
  let temperature = $state("");
  let apiKeySet = $state(false);
  let apiKeyLast4 = $state<string | undefined>(undefined);

  let models = $state<string[]>([]);
  let fetchStatus = $state<"idle" | "loading" | "error">("idle");
  let fetchError = $state("");
  let saveStatus = $state<"idle" | "saving" | "saved" | "error">("idle");
  let saveError = $state("");

  // A local provider (Ollama, or a localhost base URL) needs no API key.
  let localProvider = $derived(
    preset === "ollama" || /localhost|127\.0\.0\.1/.test(baseUrl),
  );

  // Seed the form from the current masked settings on mount.
  $effect(() => {
    const s = $aiSettings;
    if (!s) {
      void loadAiSettings();
      return;
    }
    preset = s.preset || "openai";
    baseUrl = s.baseUrl || PRESETS[preset] || "";
    model = s.model || "";
    temperature = s.temperature != null ? String(s.temperature) : "";
    apiKeySet = s.apiKeySet;
    apiKeyLast4 = s.apiKeyLast4;
  });

  function onPresetChange(): void {
    if (preset !== "custom") baseUrl = PRESETS[preset];
  }

  async function fetchModels(): Promise<void> {
    fetchStatus = "loading";
    fetchError = "";
    try {
      // Pass the form's current values so Fetch works before Save; empty
      // strings fall back to whatever is already stored.
      models = await invoke<string[]>("list_ai_models", { baseUrl, apiKey });
      fetchStatus = "idle";
      if (models.length === 0) {
        fetchStatus = "error";
        fetchError = "No models returned by the provider.";
      }
    } catch (e) {
      fetchStatus = "error";
      fetchError = String(e);
    }
  }

  async function save(): Promise<void> {
    saveStatus = "saving";
    saveError = "";
    const temp = temperature.trim();
    const parsed = temp === "" ? null : Number(temp);
    if (parsed != null && (Number.isNaN(parsed) || parsed < 0 || parsed > 2)) {
      saveStatus = "error";
      saveError = "Temperature must be a number between 0 and 2.";
      return;
    }
    try {
      await invoke("set_ai_settings", {
        preset,
        baseUrl: baseUrl.trim(),
        apiKey, // empty keeps the stored key
        model: model.trim(),
        temperature: parsed,
      });
      apiKey = "";
      await loadAiSettings();
      saveStatus = "saved";
      setTimeout(() => {
        if (saveStatus === "saved") saveStatus = "idle";
      }, 2000);
    } catch (e) {
      saveStatus = "error";
      saveError = String(e);
    }
  }
</script>

<div class="settings">
  <div class="row">
    <label for="ai-preset">Provider</label>
    <select id="ai-preset" bind:value={preset} onchange={onPresetChange}>
      <option value="openai">OpenAI</option>
      <option value="openrouter">OpenRouter</option>
      <option value="ollama">Ollama (local)</option>
      <option value="custom">Custom</option>
    </select>
  </div>

  <div class="row">
    <label for="ai-base">Base URL</label>
    <input
      id="ai-base"
      type="text"
      spellcheck="false"
      autocomplete="off"
      placeholder="https://…/v1"
      bind:value={baseUrl}
    />
  </div>

  <div class="row">
    <label for="ai-key">API key {#if localProvider}<span class="muted">(optional for local)</span>{/if}</label>
    <input
      id="ai-key"
      type="password"
      spellcheck="false"
      autocomplete="off"
      placeholder={apiKeySet ? `•••• ${apiKeyLast4 ?? "(kept)"}` : localProvider ? "not required" : "sk-…"}
      bind:value={apiKey}
    />
  </div>

  <div class="row">
    <label for="ai-model">Model</label>
    <div class="model-field">
      <input
        id="ai-model"
        type="text"
        spellcheck="false"
        autocomplete="off"
        list="ai-model-list"
        placeholder="e.g. gpt-4o-mini"
        bind:value={model}
      />
      <button type="button" class="fetch" onclick={fetchModels} disabled={fetchStatus === "loading"}>
        {fetchStatus === "loading" ? "…" : "Fetch"}
      </button>
      <datalist id="ai-model-list">
        {#each models as m (m)}
          <option value={m}></option>
        {/each}
      </datalist>
    </div>
  </div>
  {#if fetchStatus === "error"}
    <p class="field-error">{fetchError}</p>
  {:else if models.length > 0}
    <p class="field-hint">{models.length} model(s) available.</p>
  {/if}

  <div class="row">
    <label for="ai-temp">Temperature <span class="muted">(0–2, blank = default)</span></label>
    <input
      id="ai-temp"
      type="number"
      min="0"
      max="2"
      step="0.1"
      placeholder="default"
      bind:value={temperature}
    />
  </div>

  {#if saveStatus === "error"}
    <p class="field-error">{saveError}</p>
  {/if}

  <div class="footer">
    <button type="button" class="link" onclick={onClose}>Close</button>
    <div class="save-group">
      {#if saveStatus === "saved"}<span class="saved">Saved</span>{/if}
      <button type="button" class="save" onclick={save} disabled={saveStatus === "saving"}>
        {saveStatus === "saving" ? "Saving…" : "Save"}
      </button>
    </div>
  </div>
</div>

<style>
  .settings {
    display: flex;
    flex-direction: column;
    gap: 10px;
    padding: 12px;
    border-bottom: 1px solid var(--border);
    background-color: var(--bg-panel);
  }

  .row {
    display: flex;
    flex-direction: column;
    gap: 4px;
  }

  label {
    font-size: 12px;
    font-weight: 500;
    color: var(--text);
  }

  .muted {
    font-weight: 400;
    color: var(--text-muted);
  }

  input,
  select {
    width: 100%;
    padding: 6px 8px;
    border: 1px solid var(--border);
    border-radius: 6px;
    background-color: var(--bg-app);
    color: var(--text);
    font-family: var(--font-ui);
    font-size: 13px;
    outline: none;
  }
  input:focus,
  select:focus {
    border-color: var(--accent);
  }

  .model-field {
    display: flex;
    gap: 6px;
  }
  .model-field input {
    flex: 1;
    min-width: 0;
  }

  .fetch {
    flex-shrink: 0;
    padding: 6px 10px;
    border: 1px solid var(--border);
    border-radius: 6px;
    background-color: var(--bg-panel);
    color: var(--text);
    font-family: var(--font-ui);
    font-size: 12px;
    cursor: pointer;
  }
  .fetch:hover:not(:disabled) {
    background-color: var(--hover);
  }
  .fetch:disabled {
    opacity: 0.6;
    cursor: default;
  }

  .field-hint {
    margin: -4px 0 0;
    font-size: 11px;
    color: var(--text-muted);
  }
  .field-error {
    margin: -4px 0 0;
    font-size: 11px;
    color: var(--danger);
  }

  .footer {
    display: flex;
    align-items: center;
    justify-content: space-between;
    margin-top: 2px;
  }

  .save-group {
    display: flex;
    align-items: center;
    gap: 8px;
  }

  .saved {
    font-size: 12px;
    color: var(--text-muted);
  }

  .link {
    padding: 0;
    border: none;
    background: none;
    color: var(--text-muted);
    font-family: var(--font-ui);
    font-size: 12px;
    cursor: pointer;
  }
  .link:hover {
    color: var(--text);
    text-decoration: underline;
  }

  .save {
    padding: 6px 16px;
    border: none;
    border-radius: 6px;
    background-color: var(--accent);
    color: var(--accent-contrast);
    font-family: var(--font-ui);
    font-size: 12px;
    font-weight: 500;
    cursor: pointer;
  }
  .save:hover:not(:disabled) {
    background-color: var(--accent-hover);
  }
  .save:disabled {
    opacity: 0.6;
    cursor: default;
  }
</style>
