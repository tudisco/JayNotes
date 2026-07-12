//! Shared unlock/lock commands for every provider that must be *opened* before
//! use — the encrypted providers (behind the `encryption` umbrella) and the
//! hosted `tinylord` provider (whose unlock is a server login).
//!
//! ## One command set, many kinds
//!
//! M14 shipped these bound to encrypted-db; M15 added a second encrypted kind
//! (different KDF) and M16 adds a hosted kind whose unlock is a username +
//! password *login*, deliberately outside the encryption umbrella. Rather than
//! give each kind its own command names (which the frontend would have to branch
//! on), the command layer lives here and dispatches by [`VaultKind`]. It is gated
//! on `any(encryption, provider-tinylord)` so a build with only one such provider
//! still gets a working unlock flow, and it takes an `extra` map so a provider
//! can require fields beyond the password (encrypted-files' `password2`,
//! tinylord's `username`) without changing the signature per kind.
//!
//! ## No per-command session parameter
//!
//! Each kind uses a *different* Tauri-managed session type — [`SecretsSession`]
//! (opaque key material, encryption only) vs. tinylord's login-session store —
//! and a given build may compile in only one of them. So the commands never take
//! a session `State` parameter; they resolve whichever session the matched kind
//! needs from the `AppHandle` inside the (feature-gated) match arm.

use std::collections::HashMap;

use tauri::Manager;

use crate::index::AppState;
use crate::vault::{load_settings, VaultKind};

#[cfg(feature = "encryption")]
use crate::providers::crypto::{self, SecretsSession};

/// True if `id` names a vault that needs unlocking and isn't currently unlocked.
#[tauri::command]
pub async fn vault_needs_unlock(app: tauri::AppHandle, id: String) -> Result<bool, String> {
    let settings = load_settings(&app)?;
    let vault = match settings.vaults.iter().find(|v| v.id == id) {
        Some(v) => v,
        None => return Ok(false),
    };
    match vault.kind {
        #[cfg(feature = "encryption")]
        VaultKind::EncryptedDb | VaultKind::EncryptedFiles => {
            let session = app.state::<SecretsSession>();
            Ok(!session.is_unlocked(&id))
        }
        #[cfg(feature = "provider-tinylord")]
        VaultKind::Tinylord => {
            let session = app.state::<crate::providers::tinylord::TinyLordSessions>();
            Ok(!session.is_unlocked(&id))
        }
        _ => Ok(false),
    }
}

/// Unlocks a vault with a password (+ provider-specific `extra` fields), opening
/// it as the active backend. A wrong password / bad login errors. On success,
/// optionally remembers the credential for silent future unlocks.
#[tauri::command]
pub async fn unlock_vault(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    id: String,
    password: String,
    extra: Option<HashMap<String, String>>,
    remember: bool,
) -> Result<(), String> {
    let settings = load_settings(&app)?;
    let vault = settings
        .vaults
        .iter()
        .find(|v| v.id == id)
        .ok_or("No such vault")?
        .clone();
    let _ = &extra; // used only by kinds that need it (below)

    match vault.kind {
        #[cfg(feature = "provider-encrypted-db")]
        VaultKind::EncryptedDb => {
            let session = app.state::<SecretsSession>();
            crate::providers::encrypted_db::unlock_with_password(
                &app, &state, &session, &vault, &password, remember,
            )
        }
        #[cfg(feature = "provider-encrypted-files")]
        VaultKind::EncryptedFiles => {
            let password2 = extra
                .as_ref()
                .and_then(|m| m.get("password2"))
                .cloned()
                .unwrap_or_default();
            let session = app.state::<SecretsSession>();
            crate::providers::encrypted_files::unlock_with_password(
                &app, &state, &session, &vault, &password, &password2, remember,
            )
        }
        #[cfg(feature = "provider-tinylord")]
        VaultKind::Tinylord => {
            // Username: an explicit extra field wins; otherwise the one stored in
            // the vault's config at creation (the common case — the unlock panel
            // only asks for the password).
            let username = extra
                .as_ref()
                .and_then(|m| m.get("username"))
                .cloned()
                .filter(|u| !u.trim().is_empty())
                .or_else(|| {
                    vault
                        .config
                        .get("username")
                        .and_then(|v| v.as_str())
                        .map(str::to_string)
                })
                .unwrap_or_default();
            let session = app.state::<crate::providers::tinylord::TinyLordSessions>();
            crate::providers::tinylord::unlock_with_login(
                &app, &state, &session, &vault, &username, &password, remember,
            )
        }
        _ => Err("This vault doesn't require unlocking (or its provider isn't in this build)".into()),
    }
}

/// Tries to unlock `id` silently from remembered credentials (no prompt).
/// Returns true if the vault became active. Used right after switching to it.
#[tauri::command]
pub async fn unlock_remembered(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    id: String,
) -> Result<bool, String> {
    let settings = load_settings(&app)?;
    let vault = match settings.vaults.iter().find(|v| v.id == id) {
        Some(v) => v.clone(),
        None => return Ok(false),
    };
    Ok(crate::providers::try_auto_open(&app, &state, &vault))
}

/// Locks the active vault: clears the in-memory credential, drops the backend
/// (the tinylord handle's `Drop` also stops its SSE tasks), the index, and the
/// watcher, and forgets any remembered credential.
#[tauri::command]
pub async fn lock_vault(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    id: String,
) -> Result<(), String> {
    let settings = load_settings(&app)?;
    let kind = settings
        .vaults
        .iter()
        .find(|v| v.id == id)
        .map(|v| v.kind);

    match kind {
        #[cfg(feature = "encryption")]
        Some(VaultKind::EncryptedDb) | Some(VaultKind::EncryptedFiles) => {
            let session = app.state::<SecretsSession>();
            session.lock(&id);
            crypto::keyring_delete(&id);
        }
        #[cfg(feature = "provider-tinylord")]
        Some(VaultKind::Tinylord) => {
            let session = app.state::<crate::providers::tinylord::TinyLordSessions>();
            session.lock(&id);
            crate::providers::tinylord::keyring_delete(&id);
        }
        _ => {}
    }

    *state.active.lock().unwrap() = None;
    *state.index.lock().unwrap() = None;
    *state.watcher.lock().unwrap() = None;
    Ok(())
}
