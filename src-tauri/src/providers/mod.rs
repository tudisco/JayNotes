//! Vault provider registry — a compile-time plugin architecture.
//!
//! Every vault *kind* (plain folder, encrypted database, …) is a
//! [`VaultProvider`]: a small, self-describing module that declares its
//! metadata (display name, config-field schema, capabilities) and knows how to
//! turn a config (or an unlocked secret) into a live [`VaultHandle`] — the
//! object every storage command dispatches through.
//!
//! ## Design: trait-object handle, not an enum
//!
//! The active vault is held in [`crate::index::AppState`] as
//! `Option<Box<dyn VaultHandle>>`. `vault.rs`'s commands resolve that handle and
//! call one method on it, so each command is a thin one-liner and adding a
//! provider never edits a `match` in the command layer. The **plain** provider
//! ([`plain::PlainHandle`]) is the built-in default and simply forwards to the
//! existing file-operation cores in `vault.rs`, so every pre-M14 test keeps
//! passing unchanged.
//!
//! ## Feature gating
//!
//! Non-plain providers each sit behind a Cargo feature (`provider-encrypted-db`,
//! …). The registry assembles its entries with `#[cfg(feature = …)]`, and the
//! frontend renders its vault-type picker purely from [`provider_metas`], so a
//! provider compiled out leaves zero UI trace. A saved vault whose kind has no
//! compiled provider is reported "unsupported" (never pruned) — see
//! [`provider_for_kind`] and `vaults::list_vaults`.

use serde::Serialize;

use crate::index::{AppState, NoteRef, SearchHit, TagCount};
use crate::vault::TreeNode;

pub mod plain;

#[cfg(feature = "provider-encrypted-db")]
pub mod encrypted_db;

#[cfg(feature = "provider-encrypted-files")]
pub mod encrypted_files;

#[cfg(feature = "provider-tinylord")]
pub mod tinylord;

#[cfg(feature = "encryption")]
pub mod crypto;

// The shared unlock/lock command layer exists whenever *any* provider needs
// unlocking — every encrypted provider (the `encryption` umbrella) and the
// hosted tinylord provider (whose unlock is a login, deliberately outside the
// encryption umbrella).
#[cfg(any(feature = "encryption", feature = "provider-tinylord"))]
pub mod unlock;

// ---------------------------------------------------------------------------
// Metadata the frontend renders (vault-type picker + config forms)
// ---------------------------------------------------------------------------

/// One field in a provider's creation form. `field_type` drives which control
/// the frontend renders: `"folder"` → a folder picker, `"password"` → a masked
/// input, `"text"`/`"url"` → a text input.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigField {
    pub key: String,
    pub label: String,
    pub field_type: String,
    pub required: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub placeholder: Option<String>,
    /// Pre-filled default value for the input (e.g. tinylord's `database` =
    /// "jaynotes"). The frontend seeds the field with this when the form opens.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
}

impl ConfigField {
    fn new(key: &str, label: &str, field_type: &str, required: bool, placeholder: &str) -> Self {
        ConfigField {
            key: key.to_string(),
            label: label.to_string(),
            field_type: field_type.to_string(),
            required,
            placeholder: if placeholder.is_empty() {
                None
            } else {
                Some(placeholder.to_string())
            },
            default: None,
        }
    }

    /// Builder tweak: attach a pre-filled default value.
    #[cfg(feature = "provider-tinylord")]
    fn with_default(mut self, default: &str) -> Self {
        self.default = Some(default.to_string());
        self
    }
}

/// What a vault of this kind can and cannot do. Surfaced to the frontend so
/// capability-driven UI (e.g. hiding "Reveal in Finder" for a DB vault) is
/// data-driven rather than hardcoded per kind.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Capabilities {
    /// Notes are real files on disk that can be revealed in the OS file manager.
    pub reveal_in_finder: bool,
    /// The vault must be unlocked (password/keyring) before it can be opened.
    pub needs_unlock: bool,
    /// Notes/attachments are individual files (plain, encrypted-files) vs. rows
    /// inside a single container (encrypted-db, tinylord).
    pub folder_backed: bool,
}

/// A provider's self-description, sent to the frontend by `list_providers`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderMeta {
    pub kind: String,
    pub display_name: String,
    pub description: String,
    pub config_fields: Vec<ConfigField>,
    pub capabilities: Capabilities,
    /// Verb the unlock panel should use for this kind ("Unlock" by default, but
    /// "Sign in" for a hosted vault whose unlock is a login). Optional; the
    /// frontend falls back to "Unlock" when absent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unlock_label: Option<String>,
}

// ---------------------------------------------------------------------------
// Provider + handle traits
// ---------------------------------------------------------------------------

/// A vault kind. Registered instances are unit structs held as `&'static dyn`.
pub trait VaultProvider: Send + Sync {
    /// Stable kind id, matching [`crate::vault::VaultKind`]'s serialization.
    fn kind(&self) -> &'static str;
    /// Metadata for the type picker + config form.
    fn metadata(&self) -> ProviderMeta;
}

/// The live, opened storage for one vault. Every `vault.rs` command dispatches
/// through the active handle. Storage-mutating methods take `&AppState` so they
/// can register self-writes and keep the search index consistent, exactly as
/// the standalone cores always did.
///
/// The search methods have default `Err` bodies: a folder-backed handle
/// (plain) leaves search to the separate `state.index`, whereas a
/// self-indexing handle (encrypted-db, whose container *is* the index)
/// overrides them and reports [`VaultHandle::owns_index`] = `true`.
pub trait VaultHandle: Send + Sync {
    fn capabilities(&self) -> Capabilities;

    fn scan_tree(&self) -> Result<TreeNode, String>;
    fn read_note(&self, rel: &str) -> Result<String, String>;
    fn write_note(&self, state: &AppState, rel: &str, content: &str) -> Result<(), String>;
    /// Creates an auto-named "Untitled" note in `rel` (a folder or the root),
    /// or the exact file when `rel` names one. Returns the created rel path.
    fn create_note(&self, state: &AppState, rel: &str) -> Result<String, String>;
    fn create_folder(&self, rel: &str) -> Result<(), String>;
    fn rename(&self, state: &AppState, old_rel: &str, new_rel: &str) -> Result<(), String>;
    fn trash(&self, state: &AppState, rel: &str) -> Result<(), String>;
    fn save_attachment(
        &self,
        state: &AppState,
        file_name: &str,
        data: &[u8],
    ) -> Result<String, String>;
    /// Raw bytes of an attachment (for building a `data:` URL in the editor).
    fn read_attachment(&self, rel: &str) -> Result<Vec<u8>, String>;
    /// Reveals a note/folder in the OS file manager. Errors on non-folder-backed
    /// vaults (guarded by `capabilities().reveal_in_finder`).
    fn reveal_in_finder(&self, rel: &str) -> Result<(), String>;

    /// True when this handle owns its own search index (the container), so the
    /// search commands must dispatch here instead of `state.index`.
    fn owns_index(&self) -> bool {
        false
    }
    /// True when a manual "reindex" must be driven through this handle rather
    /// than `state.index.full_scan()`. Defaults to [`Self::owns_index`]. The
    /// encrypted-files handle overrides this to `true` while keeping
    /// `owns_index() == false`: it *populates* the separate `state.index`, but a
    /// plain filesystem `full_scan` over its ciphertext backing would be wrong —
    /// only the handle can decrypt names and content to rebuild the index.
    fn owns_reindex(&self) -> bool {
        self.owns_index()
    }
    fn search(&self, _query: &str, _limit: u32) -> Result<Vec<SearchHit>, String> {
        Err("This vault does not provide search directly".into())
    }
    fn list_notes(&self) -> Result<Vec<NoteRef>, String> {
        Err("This vault does not provide search directly".into())
    }
    fn list_tags(&self) -> Result<Vec<TagCount>, String> {
        Err("This vault does not provide search directly".into())
    }
    fn notes_by_tag(&self, _tag: &str, _limit: u32) -> Result<Vec<SearchHit>, String> {
        Err("This vault does not provide search directly".into())
    }
    fn resolve(&self, _name: &str) -> Result<Option<String>, String> {
        Ok(None)
    }
    fn status(&self) -> Result<crate::index::IndexStatus, String> {
        Err("This vault does not provide index status".into())
    }
    /// A self-indexing container has no external files to rescan; the default
    /// reports "nothing reindexed".
    fn reindex(&self) -> Result<usize, String> {
        Ok(0)
    }
}

// ---------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------

const PLAIN: plain::PlainProvider = plain::PlainProvider;
#[cfg(feature = "provider-encrypted-db")]
const ENCRYPTED_DB: encrypted_db::EncryptedDbProvider = encrypted_db::EncryptedDbProvider;
#[cfg(feature = "provider-encrypted-files")]
const ENCRYPTED_FILES: encrypted_files::EncryptedFilesProvider =
    encrypted_files::EncryptedFilesProvider;
#[cfg(feature = "provider-tinylord")]
const TINYLORD: tinylord::TinylordProvider = tinylord::TinylordProvider;

/// Every provider compiled into this build, plain first. Assembled with
/// `#[cfg]` so an omitted feature drops both the module and its entry.
pub fn providers() -> Vec<&'static dyn VaultProvider> {
    #[allow(unused_mut)]
    let mut list: Vec<&'static dyn VaultProvider> = vec![&PLAIN];
    #[cfg(feature = "provider-encrypted-db")]
    list.push(&ENCRYPTED_DB);
    #[cfg(feature = "provider-encrypted-files")]
    list.push(&ENCRYPTED_FILES);
    #[cfg(feature = "provider-tinylord")]
    list.push(&TINYLORD);
    list
}

/// The provider for `kind`, or `None` if that kind isn't compiled into this
/// build (→ the vault shows "unsupported").
pub fn provider_for_kind(kind: &str) -> Option<&'static dyn VaultProvider> {
    providers().into_iter().find(|p| p.kind() == kind)
}

/// Metadata for every compiled provider — drives the frontend type picker.
pub fn provider_metas() -> Vec<ProviderMeta> {
    providers().into_iter().map(|p| p.metadata()).collect()
}

/// True when `kind` has a compiled provider in this build.
pub fn kind_supported(kind: &str) -> bool {
    provider_for_kind(kind).is_some()
}

// ---------------------------------------------------------------------------
// Activation dispatch (kept feature-agnostic so vaults.rs/lib.rs don't `#[cfg]`)
// ---------------------------------------------------------------------------

/// Attempts to open a **non-plain** vault's backend automatically (from an
/// unlocked session or a remembered keyring key), installing it as the active
/// backend on success. Returns true if it became active. Always false in a build
/// whose provider for that kind is compiled out.
pub fn try_auto_open(
    app: &tauri::AppHandle,
    state: &AppState,
    vault: &crate::vault::Vault,
) -> bool {
    #[cfg(feature = "provider-encrypted-db")]
    if vault.kind == crate::vault::VaultKind::EncryptedDb {
        return encrypted_db::auto_open(app, state, vault);
    }
    #[cfg(feature = "provider-encrypted-files")]
    if vault.kind == crate::vault::VaultKind::EncryptedFiles {
        return encrypted_files::auto_open(app, state, vault);
    }
    #[cfg(feature = "provider-tinylord")]
    if vault.kind == crate::vault::VaultKind::Tinylord {
        return tinylord::auto_open(app, state, vault);
    }
    let _ = (app, state, vault);
    false
}

/// Opens a **temporary secondary handle** for a vault that is *not* the active
/// one, for a cross-vault transfer (see [`crate::transfer`]) or to list its
/// folders. The active backend in `state` is left untouched; the returned handle
/// is dropped by the caller when it is done (an encrypted-db handle flushes its
/// snapshot on drop).
///
/// - **plain** opens directly (no secret needed).
/// - **encrypted** kinds open only from an already-unlocked session or a
///   remembered keyring key; when neither exists the vault is locked and this
///   returns the sentinel error `"dest-locked"` so the caller can prompt an
///   unlock and retry.
/// - **tinylord** and any kind whose provider is compiled out are rejected with
///   a clear message (a hosted vault would need its full realtime runtime spun
///   up just to accept one note, which isn't supported as a transfer target).
pub fn open_secondary_handle(
    app: &tauri::AppHandle,
    vault: &crate::vault::Vault,
) -> Result<Box<dyn VaultHandle>, String> {
    match vault.kind {
        crate::vault::VaultKind::Plain => {
            let root = std::path::Path::new(&vault.path);
            if !root.is_dir() {
                return Err(format!(
                    "Vault '{}' is not reachable — is the drive connected?",
                    vault.name
                ));
            }
            let canonical = root
                .canonicalize()
                .map_err(|e| format!("Could not resolve vault directory: {e}"))?;
            Ok(Box::new(plain::PlainHandle::new(&canonical)))
        }
        #[cfg(feature = "provider-encrypted-db")]
        crate::vault::VaultKind::EncryptedDb => encrypted_db::open_secondary(app, vault),
        #[cfg(feature = "provider-encrypted-files")]
        crate::vault::VaultKind::EncryptedFiles => encrypted_files::open_secondary(app, vault),
        _ => {
            let _ = app;
            Err(format!(
                "'{}' can't receive transferred notes — its vault type isn't supported as a destination.",
                vault.name
            ))
        }
    }
}

/// Opens the active vault's backend on startup: a plain vault always opens; a
/// non-plain vault opens only if it can be unlocked silently (session/keyring),
/// otherwise it stays locked for the UI to prompt. Best-effort — failures leave
/// no active backend rather than aborting launch.
pub fn open_active_on_startup(app: &tauri::AppHandle, state: &AppState) {
    let settings = match crate::vault::load_settings(app) {
        Ok(s) => s,
        Err(_) => return,
    };
    let vault = match crate::vault::active_vault(&settings) {
        Some(v) => v.clone(),
        None => return,
    };
    match vault.kind {
        crate::vault::VaultKind::Plain => {
            let root = std::path::Path::new(&vault.path);
            if let Ok(canonical) = root.canonicalize() {
                if let Err(e) = crate::index::init_for_vault(app, state, &canonical) {
                    eprintln!("Index init failed: {e}");
                }
            }
        }
        _ => {
            // Encrypted / other: only auto-open if a key is already available.
            let _ = try_auto_open(app, state, &vault);
        }
    }
}

/// Lists the compiled providers' metadata for the vault-type picker and its
/// generically-rendered config forms.
#[tauri::command]
pub async fn list_providers() -> Result<Vec<ProviderMeta>, String> {
    Ok(provider_metas())
}

/// Capabilities of the currently open vault handle (or `None` when no vault is
/// open / it's still locked). Drives capability-gated UI like hiding "Reveal in
/// Finder" for a database vault.
#[tauri::command]
pub async fn active_capabilities(
    state: tauri::State<'_, AppState>,
) -> Result<Option<Capabilities>, String> {
    let guard = state.active.lock().unwrap();
    Ok(guard.as_deref().map(|h| h.capabilities()))
}

// ---------------------------------------------------------------------------
// Shared config-field builders (reused by provider metadata)
// ---------------------------------------------------------------------------

pub(crate) fn field(
    key: &str,
    label: &str,
    field_type: &str,
    required: bool,
    placeholder: &str,
) -> ConfigField {
    ConfigField::new(key, label, field_type, required, placeholder)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_provider_always_registered() {
        let metas = provider_metas();
        assert!(metas.iter().any(|m| m.kind == "plain"));
        assert!(kind_supported("plain"));
        assert!(provider_for_kind("plain").is_some());
    }

    #[test]
    fn unknown_kind_is_unsupported() {
        assert!(!kind_supported("does-not-exist"));
        assert!(provider_for_kind("does-not-exist").is_none());
    }

    #[cfg(feature = "provider-encrypted-db")]
    #[test]
    fn encrypted_db_present_when_feature_on() {
        assert!(kind_supported("encrypted-db"));
        let meta = provider_for_kind("encrypted-db").unwrap().metadata();
        assert!(meta.capabilities.needs_unlock);
        assert!(!meta.capabilities.reveal_in_finder);
        // Config form asks for a location, a name, and a password (+ confirm).
        let keys: Vec<&str> = meta.config_fields.iter().map(|f| f.key.as_str()).collect();
        assert!(keys.contains(&"location"));
        assert!(keys.contains(&"name"));
        assert!(keys.contains(&"password"));
    }

    #[cfg(feature = "provider-encrypted-files")]
    #[test]
    fn encrypted_files_present_when_feature_on() {
        assert!(kind_supported("encrypted-files"));
        let meta = provider_for_kind("encrypted-files").unwrap().metadata();
        assert!(meta.capabilities.needs_unlock);
        assert!(!meta.capabilities.reveal_in_finder);
        assert!(meta.capabilities.folder_backed);
        // Config form: location, name, password (+ confirm), and an optional
        // advanced password2/salt field.
        let fields: Vec<(&str, bool)> = meta
            .config_fields
            .iter()
            .map(|f| (f.key.as_str(), f.required))
            .collect();
        assert!(fields.contains(&("location", true)));
        assert!(fields.contains(&("password", true)));
        assert!(fields.contains(&("password2", false)), "password2 is optional");
    }

    #[cfg(not(feature = "provider-encrypted-files"))]
    #[test]
    fn encrypted_files_absent_without_feature() {
        assert!(!kind_supported("encrypted-files"));
    }

    #[test]
    fn plain_capabilities_are_folder_backed() {
        let meta = provider_for_kind("plain").unwrap().metadata();
        assert!(meta.capabilities.reveal_in_finder);
        assert!(meta.capabilities.folder_backed);
        assert!(!meta.capabilities.needs_unlock);
    }
}
