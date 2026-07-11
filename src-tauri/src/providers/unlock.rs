//! Shared unlock/lock commands for every encrypted provider.
//!
//! M14 shipped these bound to the encrypted-db provider. M15 adds a second
//! encrypted kind whose KDF is completely different (rclone `Keys::derive`
//! rather than scrypt→SQLCipher), so the command layer is lifted here — gated on
//! the `encryption` umbrella (present whenever *any* encrypted provider is
//! built) — and dispatches by [`VaultKind`] to the right provider's unlock. The
//! frontend calls one command set regardless of kind; a build with only one
//! encrypted provider still gets a working unlock flow.
//!
//! `unlock_vault` takes an `extra` map so a provider can require additional
//! fields beyond the password (encrypted-files' optional `password2`/salt)
//! without changing the command signature per kind.

use std::collections::HashMap;

use crate::index::AppState;
use crate::providers::crypto::{self, SecretsSession};
use crate::vault::{load_settings, VaultKind};

/// True if `id` names an encrypted vault that isn't currently unlocked.
#[tauri::command]
pub async fn vault_needs_unlock(
    app: tauri::AppHandle,
    session: tauri::State<'_, SecretsSession>,
    id: String,
) -> Result<bool, String> {
    let settings = load_settings(&app)?;
    let vault = match settings.vaults.iter().find(|v| v.id == id) {
        Some(v) => v,
        None => return Ok(false),
    };
    let needs = matches!(
        vault.kind,
        VaultKind::EncryptedDb | VaultKind::EncryptedFiles
    );
    if !needs {
        return Ok(false);
    }
    Ok(!session.is_unlocked(&id))
}

/// Unlocks an encrypted vault with a password (+ provider-specific `extra`
/// fields), opening it as the active backend. A wrong password errors. On
/// success, optionally remembers the derived key material in the OS keyring.
#[tauri::command]
pub async fn unlock_vault(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    session: tauri::State<'_, SecretsSession>,
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
        VaultKind::EncryptedDb => crate::providers::encrypted_db::unlock_with_password(
            &app, &state, &session, &vault, &password, remember,
        ),
        #[cfg(feature = "provider-encrypted-files")]
        VaultKind::EncryptedFiles => {
            let password2 = extra
                .as_ref()
                .and_then(|m| m.get("password2"))
                .cloned()
                .unwrap_or_default();
            crate::providers::encrypted_files::unlock_with_password(
                &app, &state, &session, &vault, &password, &password2, remember,
            )
        }
        _ => Err("This vault is not encrypted (or its provider isn't in this build)".into()),
    }
}

/// Tries to unlock `id` silently from the OS keyring (no prompt). Returns true
/// if the vault became active. Used right after switching to an encrypted vault.
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

/// Locks the active encrypted vault: clears the in-memory key material, drops
/// the backend (flushing any final snapshot on drop), the index, and the
/// watcher, and forgets any keyring copy.
#[tauri::command]
pub async fn lock_vault(
    state: tauri::State<'_, AppState>,
    session: tauri::State<'_, SecretsSession>,
    id: String,
) -> Result<(), String> {
    session.lock(&id);
    crypto::keyring_delete(&id);
    *state.active.lock().unwrap() = None;
    *state.index.lock().unwrap() = None;
    *state.watcher.lock().unwrap() = None;
    Ok(())
}
