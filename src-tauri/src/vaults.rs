//! Multi-vault lifecycle: listing, status/offline detection, startup pruning,
//! and the add/create/remove/rename/switch commands.
//!
//! The persisted model (`Settings`, `Vault`, `VaultKind`) lives in
//! [`crate::vault`] alongside the settings file it belongs to; this module owns
//! everything *about* the set of vaults — how their on-disk status is
//! classified, how missing ones are pruned, and how the frontend switcher
//! drives them.
//!
//! ## Adding a new vault kind (M14)
//!
//! [`crate::vault::VaultKind`] is an enum precisely so a new variant
//! (`Encrypted`) makes every `match` on it a compile error until handled.
//! Adding an encrypted kind will touch: the enum, `add_vault`/`create_vault`
//! (which currently hard-code `VaultKind::Plain`), and any open/unlock step
//! `switch_vault` needs before it inits the index. Everything else here
//! (status, pruning, naming, listing) is kind-agnostic and needs no change.

use std::path::Path;

use serde::Serialize;

use crate::index::{self, AppState};
use crate::vault::{
    active_vault, basename_of, load_settings, new_id, save_settings, Vault, VaultKind,
};

// ---------------------------------------------------------------------------
// Status heuristic
// ---------------------------------------------------------------------------

/// On-disk status of a vault folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum VaultStatus {
    /// The folder/container exists and is reachable.
    Ok,
    /// The folder is gone but its volume is unmounted (e.g. an external drive
    /// is unplugged). The vault is intact — it just isn't reachable right now.
    Offline,
    /// The folder is gone on a mounted volume — it was moved or deleted.
    Missing,
    /// The vault's kind has no provider compiled into this build (a public build
    /// that omitted the feature). Kept forever, never pruned; the switcher row is
    /// disabled with "unsupported in this build".
    Unsupported,
}

/// Extracts `/Volumes/<name>` from a macOS external-volume path.
///
/// Returns `None` for any path not under `/Volumes/` (home directories,
/// internal-disk paths, and — for now — Windows drive letters, all of which
/// are treated as `missing` rather than `offline` when their folder is gone).
/// This is the single documented seam for the volume heuristic; keeping it a
/// pure string function makes the ok/offline/missing classification testable
/// without mounting real drives.
fn volume_root(path: &str) -> Option<String> {
    let rest = path.strip_prefix("/Volumes/")?;
    let name = rest.split('/').next().filter(|s| !s.is_empty())?;
    Some(format!("/Volumes/{name}"))
}

/// Classifies a vault path as ok / offline / missing.
///
/// `exists` decides whether a given path is present; the real caller passes
/// `|p| p.exists()` (a plain vault's folder or an encrypted-db vault's container
/// file), tests inject a fake so `/Volumes/...` paths can be simulated
/// deterministically.
///
/// - path exists                          → [`VaultStatus::Ok`]
/// - path missing, volume root missing    → [`VaultStatus::Offline`]
/// - path missing, volume root present    → [`VaultStatus::Missing`]
/// - path missing, not under `/Volumes/`  → [`VaultStatus::Missing`]
fn classify_status(path: &str, exists: &dyn Fn(&Path) -> bool) -> VaultStatus {
    if exists(Path::new(path)) {
        return VaultStatus::Ok;
    }
    if let Some(vol) = volume_root(path) {
        if !exists(Path::new(&vol)) {
            return VaultStatus::Offline;
        }
    }
    VaultStatus::Missing
}

/// Real-filesystem reachability status of a vault, kind-aware: a vault whose
/// kind has no compiled provider is [`VaultStatus::Unsupported`] regardless of
/// disk state; otherwise its path presence is classified.
fn status_of_vault(vault: &Vault) -> VaultStatus {
    if !crate::providers::kind_supported(vault.kind.as_str()) {
        return VaultStatus::Unsupported;
    }
    // A TinyLord vault's `path` is a server URL, not a local directory, so the
    // filesystem ok/offline/missing heuristic doesn't apply — it must never be
    // pruned for a "missing folder". Reachability is a runtime concern surfaced
    // by the reconnecting banner, so it always classifies as Ok when supported.
    if vault.kind == VaultKind::Tinylord {
        return VaultStatus::Ok;
    }
    classify_status(&vault.path, &|p| p.exists())
}

// ---------------------------------------------------------------------------
// Naming / dedup helpers
// ---------------------------------------------------------------------------

/// Canonical-ish path equality used to reject duplicate adds. Both paths are
/// canonicalized when possible (resolves symlinks, `.`/`..`, trailing slashes);
/// falls back to a trimmed string compare when a path can't be canonicalized.
pub(crate) fn same_path(a: &str, b: &str) -> bool {
    let norm = |p: &str| {
        Path::new(p)
            .canonicalize()
            .map(|c| c.to_string_lossy().into_owned())
            .unwrap_or_else(|_| p.trim_end_matches('/').to_string())
    };
    norm(a) == norm(b)
}

/// Returns `base`, or `base 2`, `base 3`, … so no two vaults share a display
/// name. Case-insensitive to avoid confusing near-duplicates.
pub(crate) fn dedup_name(base: &str, existing: &[Vault]) -> String {
    let taken: std::collections::HashSet<String> =
        existing.iter().map(|v| v.name.to_lowercase()).collect();
    if !taken.contains(&base.to_lowercase()) {
        return base.to_string();
    }
    for n in 2..10_000 {
        let candidate = format!("{base} {n}");
        if !taken.contains(&candidate.to_lowercase()) {
            return candidate;
        }
    }
    base.to_string()
}

// ---------------------------------------------------------------------------
// Wire types
// ---------------------------------------------------------------------------

/// A vault plus its live status, as sent to the frontend switcher.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VaultInfo {
    pub id: String,
    pub name: String,
    pub path: String,
    pub kind: VaultKind,
    pub status: VaultStatus,
}

/// Payload of [`list_vaults`]: the (post-pruning) vault list, the active id, and
/// the names of any vaults pruned this call so the UI can show a one-time notice.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VaultList {
    pub vaults: Vec<VaultInfo>,
    pub active_id: Option<String>,
    pub removed: Vec<String>,
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

/// Lists all configured vaults with their live status, after pruning any whose
/// status is `missing` (folder gone on a *mounted* volume). Offline vaults are
/// **never** pruned — an unplugged external drive must not forget the vault.
///
/// If pruning removes the active vault, the active pointer moves to the first
/// remaining vault (or none) and the index/watcher are re-inited for it. The
/// names of pruned vaults are returned once so the frontend can surface a
/// dismissable "Removed vault X" notice.
#[tauri::command]
pub async fn list_vaults(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
) -> Result<VaultList, String> {
    let mut settings = load_settings(&app)?;

    let mut removed: Vec<String> = Vec::new();
    let prev_active = settings.active_vault_id.clone();
    settings.vaults.retain(|v| {
        // Only prune vaults whose folder vanished on a *mounted* volume. Offline
        // (drive unplugged) and Unsupported (kind not in this build) are kept.
        if status_of_vault(v) == VaultStatus::Missing {
            removed.push(v.name.clone());
            false
        } else {
            true
        }
    });

    // If the active vault was pruned, fall back to the first remaining one.
    let active_pruned = settings
        .active_vault_id
        .as_ref()
        .map(|id| !settings.vaults.iter().any(|v| &v.id == id))
        .unwrap_or(false);
    if active_pruned {
        settings.active_vault_id = settings.vaults.first().map(|v| v.id.clone());
    }

    // Persist whenever we pruned or moved the active pointer. (Migration from a
    // legacy `vault_path` is also flushed to disk here on first run.)
    save_settings(&app, &settings)?;

    // Re-init the index for the new active vault if the pointer changed.
    if active_pruned && settings.active_vault_id != prev_active {
        reinit_active(&app, &state, &settings);
    }

    let vaults = settings
        .vaults
        .iter()
        .map(|v| VaultInfo {
            id: v.id.clone(),
            name: v.name.clone(),
            path: v.path.clone(),
            kind: v.kind,
            status: status_of_vault(v),
        })
        .collect();

    Ok(VaultList {
        vaults,
        active_id: settings.active_vault_id,
        removed,
    })
}

/// Adds an existing folder as a new vault. Validates the directory exists and
/// rejects duplicates by canonical path. The display name defaults to the
/// folder basename, deduped ("Name 2") against existing vaults. Does not switch
/// to it — the frontend calls `switch_vault` next.
#[tauri::command]
pub async fn add_vault(app: tauri::AppHandle, path: String) -> Result<Vault, String> {
    let dir = Path::new(&path);
    if !dir.is_dir() {
        return Err(format!("Not a folder: {path}"));
    }
    let canonical = dir
        .canonicalize()
        .map_err(|e| format!("Could not resolve folder: {e}"))?
        .to_string_lossy()
        .into_owned();

    let mut settings = load_settings(&app)?;
    if let Some(existing) = settings
        .vaults
        .iter()
        .find(|v| same_path(&v.path, &canonical))
    {
        return Err(format!("That folder is already a vault: {}", existing.name));
    }

    let vault = Vault {
        id: new_id(),
        name: dedup_name(&basename_of(&canonical), &settings.vaults),
        path: canonical,
        kind: VaultKind::Plain,
        config: Default::default(),
    };
    settings.vaults.push(vault.clone());
    save_settings(&app, &settings)?;
    Ok(vault)
}

/// Creates a new folder named `name` under `parent_path` and adds it as a vault.
/// Errors if the folder already exists.
#[tauri::command]
pub async fn create_vault(
    app: tauri::AppHandle,
    parent_path: String,
    name: String,
) -> Result<Vault, String> {
    let clean = name.trim();
    if clean.is_empty() {
        return Err("A vault name is required".to_string());
    }
    if clean.contains('/') || clean.contains('\\') {
        return Err("Vault name cannot contain path separators".to_string());
    }
    let parent = Path::new(&parent_path);
    if !parent.is_dir() {
        return Err(format!("Parent folder does not exist: {parent_path}"));
    }
    let target = parent.join(clean);
    if target.exists() {
        return Err(format!("A folder named '{clean}' already exists here"));
    }
    std::fs::create_dir(&target).map_err(|e| format!("Could not create vault folder: {e}"))?;

    // Delegate to add_vault for canonicalization + dedup + persistence.
    add_vault(app, target.to_string_lossy().into_owned()).await
}

/// Forgets a vault (the folder on disk is never touched). If the removed vault
/// was active, switches to the first remaining vault — re-initing the index —
/// or clears the index entirely when none remain. Returns the path now active
/// (or null), so the frontend can refresh its tree.
#[tauri::command]
pub async fn remove_vault(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    id: String,
) -> Result<Option<String>, String> {
    let mut settings = load_settings(&app)?;
    if !settings.vaults.iter().any(|v| v.id == id) {
        return Err("No such vault".to_string());
    }
    let was_active = settings.active_vault_id.as_deref() == Some(id.as_str());
    settings.vaults.retain(|v| v.id != id);

    if was_active {
        settings.active_vault_id = settings.vaults.first().map(|v| v.id.clone());
    }
    save_settings(&app, &settings)?;

    if was_active {
        reinit_active(&app, &state, &settings);
    }
    Ok(active_vault(&settings).map(|v| v.path.clone()))
}

/// Renames a vault's display name only (never its folder). The name is trimmed
/// and must be non-empty.
#[tauri::command]
pub async fn rename_vault(
    app: tauri::AppHandle,
    id: String,
    name: String,
) -> Result<(), String> {
    let clean = name.trim();
    if clean.is_empty() {
        return Err("A vault name is required".to_string());
    }
    let mut settings = load_settings(&app)?;
    let vault = settings
        .vaults
        .iter_mut()
        .find(|v| v.id == id)
        .ok_or("No such vault")?;
    vault.name = clean.to_string();
    save_settings(&app, &settings)
}

/// Switches the active vault to `id`, persists it, and re-opens the index +
/// watcher + asset-protocol scope for its folder (exactly as `set_vault` does).
/// Returns the canonicalized path. Errors if the vault is unknown or its folder
/// isn't currently reachable (offline/missing) — the caller stays put.
#[tauri::command]
pub async fn switch_vault(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    id: String,
) -> Result<String, String> {
    let mut settings = load_settings(&app)?;
    let vault = settings
        .vaults
        .iter()
        .find(|v| v.id == id)
        .ok_or("No such vault")?
        .clone();

    if !crate::providers::kind_supported(vault.kind.as_str()) {
        return Err(format!(
            "Vault '{}' is unsupported in this build",
            vault.name
        ));
    }

    if vault.kind == VaultKind::Plain {
        let root = Path::new(&vault.path);
        if !root.is_dir() {
            return Err(format!(
                "Vault '{}' is not reachable — is the drive connected?",
                vault.name
            ));
        }
        let canonical = root
            .canonicalize()
            .map_err(|e| format!("Could not resolve vault directory: {e}"))?;

        settings.active_vault_id = Some(id);
        save_settings(&app, &settings)?;

        if let Err(e) = index::init_for_vault(&app, &state, &canonical) {
            eprintln!("Index init failed for {}: {e}", canonical.display());
        }
        return Ok(canonical.to_string_lossy().into_owned());
    }

    // A TinyLord vault has no local file to check — reachability is decided when
    // we (silently) connect below; if that fails it stays locked and the unlock
    // panel prompts for the login.
    if vault.kind != VaultKind::Tinylord {
        // Non-plain, non-tinylord (encrypted-db/-files): the container must exist.
        let file = Path::new(&vault.path);
        if !file.exists() {
            return Err(format!(
                "Vault '{}' is not reachable — is the drive connected?",
                vault.name
            ));
        }
    }
    settings.active_vault_id = Some(id);
    save_settings(&app, &settings)?;

    // Try to open it silently (unlocked session / remembered key). If that
    // fails it stays locked, and the frontend shows the unlock prompt — clear
    // any previous backend so a stale vault isn't left active.
    if !crate::providers::try_auto_open(&app, &state, &vault) {
        *state.index.lock().unwrap() = None;
        *state.watcher.lock().unwrap() = None;
        *state.active.lock().unwrap() = None;
    }
    Ok(vault.path.clone())
}

/// Re-inits the index/watcher for the currently active vault in `settings`, or
/// clears them when no vault is active. Best-effort — index failures are
/// non-fatal (the vault is still usable without search).
fn reinit_active(app: &tauri::AppHandle, state: &AppState, settings: &crate::vault::Settings) {
    match active_vault(settings) {
        Some(v) if v.kind == VaultKind::Plain => {
            let root = Path::new(&v.path);
            if let Ok(canonical) = root.canonicalize() {
                if let Err(e) = index::init_for_vault(app, state, &canonical) {
                    eprintln!("Index init failed for {}: {e}", canonical.display());
                }
            }
        }
        Some(v) => {
            // Non-plain: open silently if possible, else leave locked.
            if !crate::providers::try_auto_open(app, state, v) {
                *state.index.lock().unwrap() = None;
                *state.watcher.lock().unwrap() = None;
                *state.active.lock().unwrap() = None;
            }
        }
        None => {
            // No active vault: drop the index, watcher, and backend.
            *state.index.lock().unwrap() = None;
            *state.watcher.lock().unwrap() = None;
            *state.active.lock().unwrap() = None;
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    /// Builds a `dir_exists` closure over a fixed set of existing paths.
    fn exists_set(paths: &[&str]) -> impl Fn(&Path) -> bool {
        let set: HashSet<String> = paths.iter().map(|s| s.to_string()).collect();
        move |p: &Path| set.contains(&p.to_string_lossy().into_owned())
    }

    #[test]
    fn volume_root_extracts_only_volumes_paths() {
        assert_eq!(
            volume_root("/Volumes/WorkDrive/Notes"),
            Some("/Volumes/WorkDrive".to_string())
        );
        // Vault sitting at the volume root itself.
        assert_eq!(
            volume_root("/Volumes/WorkDrive"),
            Some("/Volumes/WorkDrive".to_string())
        );
        // Non-volume paths yield nothing.
        assert_eq!(volume_root("/Users/jay/Notes"), None);
        assert_eq!(volume_root("/Volumes/"), None);
        assert_eq!(volume_root("C:/Users/jay/Notes"), None);
    }

    #[test]
    fn classify_status_ok_offline_missing() {
        // Folder present → ok.
        let exists = exists_set(&["/Volumes/WorkDrive/Notes", "/Volumes/WorkDrive"]);
        assert_eq!(
            classify_status("/Volumes/WorkDrive/Notes", &exists),
            VaultStatus::Ok
        );

        // Folder gone, volume gone → offline (drive unplugged).
        let exists = exists_set(&[]);
        assert_eq!(
            classify_status("/Volumes/WorkDrive/Notes", &exists),
            VaultStatus::Offline
        );

        // Folder gone, volume present → missing (deleted/moved).
        let exists = exists_set(&["/Volumes/WorkDrive"]);
        assert_eq!(
            classify_status("/Volumes/WorkDrive/Notes", &exists),
            VaultStatus::Missing
        );

        // Non-volume path gone → missing (never offline).
        let exists = exists_set(&[]);
        assert_eq!(
            classify_status("/Users/jay/Notes", &exists),
            VaultStatus::Missing
        );
    }

    #[test]
    fn dedup_name_appends_suffix_case_insensitively() {
        let mk = |name: &str| Vault {
            id: name.to_string(),
            name: name.to_string(),
            path: format!("/x/{name}"),
            kind: VaultKind::Plain,
            config: Default::default(),
        };
        let existing = vec![mk("Notes"), mk("Work 2")];
        // Fresh name is unchanged.
        assert_eq!(dedup_name("Archive", &existing), "Archive");
        // Collision (case-insensitive) bumps the suffix.
        assert_eq!(dedup_name("notes", &existing), "notes 2");
        // Skips a taken suffix.
        assert_eq!(dedup_name("Work", &existing), "Work"); // "Work" itself is free
    }

    /// Emulates the prune step of `list_vaults` over injected statuses, so the
    /// "prune missing but keep offline" rule is verified without a Tauri app.
    fn prune(vaults: Vec<Vault>, status: &dyn Fn(&str) -> VaultStatus) -> (Vec<Vault>, Vec<String>) {
        let mut removed = Vec::new();
        let kept = vaults
            .into_iter()
            .filter(|v| {
                if status(&v.path) == VaultStatus::Missing {
                    removed.push(v.name.clone());
                    false
                } else {
                    true
                }
            })
            .collect();
        (kept, removed)
    }

    #[test]
    fn pruning_removes_missing_keeps_offline_and_ok() {
        let mk = |name: &str, path: &str| Vault {
            id: name.to_string(),
            name: name.to_string(),
            path: path.to_string(),
            kind: VaultKind::Plain,
            config: Default::default(),
        };
        let vaults = vec![
            mk("Ok", "/ok"),
            mk("Offline", "/off"),
            mk("Missing", "/gone"),
        ];
        let status = |p: &str| match p {
            "/ok" => VaultStatus::Ok,
            "/off" => VaultStatus::Offline,
            _ => VaultStatus::Missing,
        };
        let (kept, removed) = prune(vaults, &status);
        let names: Vec<&str> = kept.iter().map(|v| v.name.as_str()).collect();
        assert_eq!(names, vec!["Ok", "Offline"]);
        assert_eq!(removed, vec!["Missing"]);
    }

    /// An `encrypted-db` vault reports [`VaultStatus::Unsupported`] when the
    /// provider feature is compiled out (public plain-only build) — and is
    /// therefore never pruned regardless of disk state.
    #[cfg(not(feature = "provider-encrypted-db"))]
    #[test]
    fn encrypted_vault_unsupported_without_feature() {
        let v = Vault {
            id: "e".into(),
            name: "Enc".into(),
            path: "/nope/secret.jaynotes".into(),
            kind: VaultKind::EncryptedDb,
            config: Default::default(),
        };
        assert_eq!(status_of_vault(&v), VaultStatus::Unsupported);
    }

    /// With the feature on, an `encrypted-db` vault is a supported kind and is
    /// classified by its container file's presence like any other vault.
    #[cfg(feature = "provider-encrypted-db")]
    #[test]
    fn encrypted_vault_supported_with_feature() {
        let present = Vault {
            id: "e".into(),
            name: "Enc".into(),
            path: "/nope/secret.jaynotes".into(),
            kind: VaultKind::EncryptedDb,
            config: Default::default(),
        };
        // Not on a /Volumes/ path and missing → Missing (a supported kind).
        assert_eq!(status_of_vault(&present), VaultStatus::Missing);
    }
}
