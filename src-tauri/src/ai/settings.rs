//! AI provider settings, stored under the `ai` key of the shared `settings.json`
//! (the vault module owns that file and preserves unknown keys, so this slots in
//! without disturbing existing settings).
//!
//! The API key is write-only from the UI's perspective: [`get_ai_settings`]
//! returns only whether a key is set plus its last four characters, never the
//! raw secret.

use serde::{Deserialize, Serialize};

use super::client::ChatClient;
use crate::vault;

const AI_KEY: &str = "ai";

/// Full AI settings as persisted. `api_key` is stored verbatim in the local
/// settings file but never returned to the frontend.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AiSettings {
    #[serde(default)]
    pub preset: String,
    #[serde(default)]
    pub base_url: String,
    #[serde(default)]
    pub api_key: String,
    #[serde(default)]
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
}

/// Masked view sent to the UI — the raw key is replaced by a presence flag and
/// its last four characters (for "•••• 1234" style display).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiSettingsMasked {
    pub preset: String,
    pub base_url: String,
    pub model: String,
    pub temperature: Option<f32>,
    pub api_key_set: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key_last4: Option<String>,
}

impl AiSettings {
    fn mask(&self) -> AiSettingsMasked {
        let key = self.api_key.trim();
        let last4 = if key.chars().count() >= 4 {
            Some(key.chars().rev().take(4).collect::<Vec<_>>().into_iter().rev().collect())
        } else {
            None
        };
        AiSettingsMasked {
            preset: self.preset.clone(),
            base_url: self.base_url.clone(),
            model: self.model.clone(),
            temperature: self.temperature,
            api_key_set: !key.is_empty(),
            api_key_last4: last4,
        }
    }
}

/// Loads the persisted AI settings (defaults when unset). Also usable by the
/// chat loop to build a client.
pub fn load(app: &tauri::AppHandle) -> Result<AiSettings, String> {
    let settings = vault::load_settings(app)?;
    match settings.extra.get(AI_KEY) {
        Some(v) => serde_json::from_value(v.clone())
            .map_err(|e| format!("Stored AI settings are malformed: {e}")),
        None => Ok(AiSettings::default()),
    }
}

fn store(app: &tauri::AppHandle, ai: &AiSettings) -> Result<(), String> {
    let mut settings = vault::load_settings(app)?;
    let value = serde_json::to_value(ai).map_err(|e| format!("Could not serialize AI settings: {e}"))?;
    settings.extra.insert(AI_KEY.to_string(), value);
    vault::save_settings(app, &settings)
}

/// An empty incoming key means "keep the existing one" (the UI never reads the
/// secret back, so it can't re-send it when saving other fields).
fn resolve_api_key(incoming: String, existing: String) -> String {
    if incoming.is_empty() {
        existing
    } else {
        incoming
    }
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

/// Returns AI settings with the API key masked (never the raw secret).
#[tauri::command]
pub async fn get_ai_settings(app: tauri::AppHandle) -> Result<AiSettingsMasked, String> {
    Ok(load(&app)?.mask())
}

/// Persists AI settings. An empty `api_key` keeps the existing stored key
/// (so the UI can save other fields without ever having read the secret back).
#[tauri::command]
pub async fn set_ai_settings(
    app: tauri::AppHandle,
    preset: String,
    base_url: String,
    api_key: String,
    model: String,
    temperature: Option<f32>,
) -> Result<(), String> {
    let existing = load(&app)?;
    let api_key = resolve_api_key(api_key, existing.api_key);
    store(
        &app,
        &AiSettings {
            preset,
            base_url,
            api_key,
            model,
            temperature,
        },
    )
}

/// Lists model ids from the provider (`GET {baseUrl}/models`).
///
/// The settings panel calls this before anything is saved, so the form's
/// current values arrive as overrides: `base_url` (if non-empty) replaces the
/// stored one, and `api_key` (if non-empty) replaces the stored key.
#[tauri::command]
pub async fn list_ai_models(
    app: tauri::AppHandle,
    base_url: Option<String>,
    api_key: Option<String>,
) -> Result<Vec<String>, String> {
    let mut settings = load(&app)?;
    if let Some(url) = base_url.filter(|u| !u.trim().is_empty()) {
        settings.base_url = url;
    }
    if let Some(key) = api_key.filter(|k| !k.trim().is_empty()) {
        settings.api_key = key;
    }
    if settings.base_url.trim().is_empty() {
        return Err("Set an AI base URL first.".into());
    }
    // `list_models` only needs base_url + api_key; the model may be blank here.
    let client = ChatClient::for_listing(&settings)?;
    client.list_models().await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masks_key_to_last_four() {
        let s = AiSettings {
            api_key: "sk-secret-1234".into(),
            ..Default::default()
        };
        let m = s.mask();
        assert!(m.api_key_set);
        assert_eq!(m.api_key_last4.as_deref(), Some("1234"));
    }

    #[test]
    fn masks_absent_and_short_keys() {
        let empty = AiSettings::default().mask();
        assert!(!empty.api_key_set);
        assert!(empty.api_key_last4.is_none());

        let short = AiSettings {
            api_key: "ab".into(),
            ..Default::default()
        }
        .mask();
        assert!(short.api_key_set);
        assert!(short.api_key_last4.is_none());
    }

    #[test]
    fn empty_incoming_key_keeps_existing() {
        assert_eq!(resolve_api_key(String::new(), "keep-me".into()), "keep-me");
        assert_eq!(resolve_api_key("new-key".into(), "old".into()), "new-key");
    }
}
