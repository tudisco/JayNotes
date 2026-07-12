//! The `encrypted-db` provider: a SQLCipher container vault.
//!
//! ## Live / snapshot split (safe-degrade design)
//!
//! The **live** working database lives in `app_data_dir/enc-vaults/<id>.db`
//! (WAL, never synced). The user-visible `<name>.jaynotes` at the vault location
//! is a **snapshot** — a consistent, same-key encrypted copy exported with
//! `VACUUM INTO` + atomic rename, debounced ~3 s after the last edit. Keeping the
//! WAL out of the synced location means an external sync tool only ever sees a
//! whole, consistent file.
//!
//! On open we hydrate the live DB from the snapshot if it's missing, then — if
//! the snapshot changed since our last export (another device, a Syncthing
//! sync, or a `*.sync-conflict-*` sibling) — run the pure merge in
//! [`merge`], re-export, and delete consumed conflict siblings.

pub mod container;
pub mod merge;

use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use tauri::Manager;

use crate::index::AppState;
use crate::providers::crypto::{self, SecretsSession};
use crate::providers::{field, Capabilities, ProviderMeta, VaultHandle, VaultProvider};
use crate::vault::{load_settings, new_id, save_settings, TreeNode, Vault, VaultKind};
use crate::vaults::{dedup_name, VaultInfo, VaultStatus};

use container::Container;

/// Debounce quiet-period before a snapshot export.
const SNAPSHOT_DEBOUNCE: Duration = Duration::from_secs(3);

// ---------------------------------------------------------------------------
// Provider
// ---------------------------------------------------------------------------

pub struct EncryptedDbProvider;

impl EncryptedDbProvider {
    pub const CAPS: Capabilities = Capabilities {
        reveal_in_finder: false,
        needs_unlock: true,
        folder_backed: false,
    };
}

impl VaultProvider for EncryptedDbProvider {
    fn kind(&self) -> &'static str {
        "encrypted-db"
    }

    fn metadata(&self) -> ProviderMeta {
        ProviderMeta {
            kind: "encrypted-db".into(),
            display_name: "Encrypted database".into(),
            description: "One password-locked file holding all notes, images, and search.".into(),
            config_fields: vec![
                field("location", "Location", "folder", true, "Where to save the vault file"),
                field("name", "Vault name", "text", true, "My Private Notes"),
                field("password", "Password", "password", true, "Choose a strong password"),
                field("confirm", "Confirm password", "password", true, "Re-enter the password"),
            ],
            capabilities: Self::CAPS,
            unlock_label: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Handle
// ---------------------------------------------------------------------------

/// The opened handle for an encrypted-db vault. Owns the live container behind a
/// mutex and a background worker that debounces snapshot exports.
pub struct EncryptedDbHandle {
    container: Arc<Mutex<Container>>,
    tx: Option<Sender<()>>,
    worker: Option<JoinHandle<()>>,
}

impl EncryptedDbHandle {
    fn new(container: Container, snapshot: PathBuf) -> Self {
        let container = Arc::new(Mutex::new(container));
        let (tx, rx) = channel::<()>();
        let worker = spawn_snapshot_worker(container.clone(), snapshot, rx);
        EncryptedDbHandle {
            container,
            tx: Some(tx),
            worker: Some(worker),
        }
    }

    /// Signals the worker that a write happened (schedules a debounced export).
    fn touch(&self) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(());
        }
    }

    fn with<T>(&self, f: impl FnOnce(&Container) -> Result<T, String>) -> Result<T, String> {
        let c = self.container.lock().map_err(|_| "container lock poisoned")?;
        f(&c)
    }
}

impl Drop for EncryptedDbHandle {
    fn drop(&mut self) {
        // Dropping the sender disconnects the worker, which does a final flush
        // and exits; join it so the snapshot reflects the latest state before
        // the vault is switched away.
        self.tx.take();
        if let Some(w) = self.worker.take() {
            let _ = w.join();
        }
    }
}

impl VaultHandle for EncryptedDbHandle {
    fn capabilities(&self) -> Capabilities {
        EncryptedDbProvider::CAPS
    }

    fn scan_tree(&self) -> Result<TreeNode, String> {
        self.with(|c| c.scan_tree())
    }
    fn read_note(&self, rel: &str) -> Result<String, String> {
        self.with(|c| c.read_note(rel))
    }
    fn write_note(&self, _state: &AppState, rel: &str, content: &str) -> Result<(), String> {
        self.with(|c| c.write_note(rel, content))?;
        self.touch();
        Ok(())
    }
    fn create_note(&self, _state: &AppState, rel: &str) -> Result<String, String> {
        let r = self.with(|c| c.create_note(rel))?;
        self.touch();
        Ok(r)
    }
    fn create_folder(&self, rel: &str) -> Result<(), String> {
        self.with(|c| c.create_folder(rel))?;
        self.touch();
        Ok(())
    }
    fn rename(&self, _state: &AppState, old_rel: &str, new_rel: &str) -> Result<(), String> {
        self.with(|c| c.rename(old_rel, new_rel))?;
        self.touch();
        Ok(())
    }
    fn trash(&self, _state: &AppState, rel: &str) -> Result<(), String> {
        self.with(|c| c.trash(rel))?;
        self.touch();
        Ok(())
    }
    fn save_attachment(
        &self,
        _state: &AppState,
        file_name: &str,
        data: &[u8],
    ) -> Result<String, String> {
        let r = self.with(|c| c.save_attachment(file_name, data))?;
        self.touch();
        Ok(r)
    }
    fn read_attachment(&self, rel: &str) -> Result<Vec<u8>, String> {
        self.with(|c| c.read_attachment(rel))
    }
    fn reveal_in_finder(&self, _rel: &str) -> Result<(), String> {
        Err("This vault stores notes inside an encrypted file — there's nothing to reveal in Finder".into())
    }

    fn owns_index(&self) -> bool {
        true
    }
    fn search(&self, query: &str, limit: u32) -> Result<Vec<crate::index::SearchHit>, String> {
        self.with(|c| c.search(query, limit))
    }
    fn list_notes(&self) -> Result<Vec<crate::index::NoteRef>, String> {
        self.with(|c| c.list_notes())
    }
    fn list_tags(&self) -> Result<Vec<crate::index::TagCount>, String> {
        self.with(|c| c.list_tags())
    }
    fn notes_by_tag(&self, tag: &str, limit: u32) -> Result<Vec<crate::index::SearchHit>, String> {
        self.with(|c| c.notes_by_tag(tag, limit))
    }
    fn resolve(&self, name: &str) -> Result<Option<String>, String> {
        self.with(|c| c.resolve(name))
    }
    fn status(&self) -> Result<crate::index::IndexStatus, String> {
        self.with(|c| c.status())
    }
}

fn spawn_snapshot_worker(
    container: Arc<Mutex<Container>>,
    snapshot: PathBuf,
    rx: Receiver<()>,
) -> JoinHandle<()> {
    std::thread::spawn(move || loop {
        match rx.recv() {
            Ok(_) => loop {
                // Coalesce a burst of writes: only export after a quiet gap.
                match rx.recv_timeout(SNAPSHOT_DEBOUNCE) {
                    Ok(_) => continue,
                    Err(RecvTimeoutError::Timeout) => {
                        export_now(&container, &snapshot);
                        break;
                    }
                    Err(RecvTimeoutError::Disconnected) => {
                        export_now(&container, &snapshot);
                        return;
                    }
                }
            },
            Err(_) => return, // handle dropped with no pending writes
        }
    })
}

fn export_now(container: &Arc<Mutex<Container>>, snapshot: &Path) {
    if let Ok(c) = container.lock() {
        if let Err(e) = c.export_snapshot(snapshot) {
            eprintln!("Snapshot export failed for {}: {e}", snapshot.display());
        }
    }
}

// ---------------------------------------------------------------------------
// Activation (open + import + install as the active backend)
// ---------------------------------------------------------------------------

/// The app-data path of the live working DB for `vault_id`.
fn live_db_path(app: &tauri::AppHandle, vault_id: &str) -> Result<PathBuf, String> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("Could not resolve app data dir: {e}"))?
        .join("enc-vaults");
    Ok(dir.join(format!("{vault_id}.db")))
}

/// `*.sync-conflict-*` siblings of the snapshot (Syncthing's conflict copies).
fn sync_conflict_siblings(snapshot: &Path) -> Vec<PathBuf> {
    let (dir, stem) = match (snapshot.parent(), snapshot.file_stem()) {
        (Some(d), Some(s)) => (d, s.to_string_lossy().into_owned()),
        _ => return Vec::new(),
    };
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for entry in rd.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with(&stem) && name.contains(".sync-conflict-") {
                out.push(entry.path());
            }
        }
    }
    out
}

/// Opens (hydrating + importing as needed) an encrypted-db vault with `key`,
/// installs it as the active backend, and returns. A wrong key surfaces as an
/// error from [`Container::open`].
fn open_and_activate(
    app: &tauri::AppHandle,
    state: &AppState,
    vault: &Vault,
    key: [u8; 32],
) -> Result<(), String> {
    let snapshot = PathBuf::from(&vault.path);
    let live = live_db_path(app, &vault.id)?;
    if let Some(parent) = live.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("Could not create live dir: {e}"))?;
    }

    // Hydrate the live DB from the snapshot on first open on this device.
    let fresh_hydrate = if !live.exists() {
        if !snapshot.exists() {
            return Err(format!("Vault file is missing: {}", vault.path));
        }
        std::fs::copy(&snapshot, &live).map_err(|e| format!("Could not open vault: {e}"))?;
        true
    } else {
        false
    };

    let container = Container::open(&live, &key)?; // verifies the key
    if fresh_hydrate {
        // The copied live DB's export marker matches the snapshot → no import.
        container.mark_synced(&snapshot)?;
    } else if container.snapshot_changed_externally(&snapshot) {
        // The snapshot moved under us (another device / sync) → merge it in.
        let date = today();
        container.import_from(&snapshot, &key, &date)?;
        for sib in sync_conflict_siblings(&snapshot) {
            if container.import_from(&sib, &key, &date).is_ok() {
                let _ = std::fs::remove_file(&sib);
            }
        }
        container.export_snapshot(&snapshot)?; // re-export the merged state
    }

    // Encrypted vaults have no plain fs index/watcher; the container is both.
    *state.index.lock().unwrap() = None;
    *state.watcher.lock().unwrap() = None;
    *state.active.lock().unwrap() = Some(Box::new(EncryptedDbHandle::new(container, snapshot)));
    Ok(())
}

/// Attempts to open the vault automatically using an already-unlocked session
/// key or a remembered keyring key. Returns true if it became active. Never
/// prompts. Used by startup and vault switching.
pub fn auto_open(app: &tauri::AppHandle, state: &AppState, vault: &Vault) -> bool {
    let session = app.state::<SecretsSession>();
    let key = session
        .get(&vault.id)
        .or_else(|| crypto::keyring_get(&vault.id));
    if let Some(key) = key {
        if open_and_activate(app, state, vault, key).is_ok() {
            session.store(&vault.id, key);
            return true;
        }
    }
    false
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

/// Creates a new encrypted-db vault: a `<name>.jaynotes` container at
/// `location`, keyed by a scrypt-derived key. Adds it to settings, makes it
/// active, and (optionally) remembers the key in the OS keyring. Returns the new
/// vault's info for the switcher.
#[tauri::command]
pub async fn create_encrypted_vault(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    session: tauri::State<'_, SecretsSession>,
    location: String,
    name: String,
    password: String,
    remember: bool,
) -> Result<VaultInfo, String> {
    let clean = name.trim();
    if clean.is_empty() {
        return Err("A vault name is required".into());
    }
    if clean.contains('/') || clean.contains('\\') {
        return Err("Vault name cannot contain path separators".into());
    }
    if password.is_empty() {
        return Err("A password is required".into());
    }
    let dir = Path::new(&location);
    if !dir.is_dir() {
        return Err(format!("Location folder does not exist: {location}"));
    }
    let file = dir.join(format!("{clean}.jaynotes"));
    if file.exists() {
        return Err(format!("A vault file named '{clean}.jaynotes' already exists here"));
    }

    let salt = crypto::random_bytes(16)?;
    let key = crypto::derive_vault_key(&password, &salt)?;

    // Build the vault entry (salt is non-secret config, stored in plaintext).
    let mut settings = load_settings(&app)?;
    let id = new_id();
    let mut config = serde_json::Map::new();
    config.insert(
        "salt".into(),
        serde_json::Value::String(crypto::to_hex(&salt)),
    );
    let vault = Vault {
        id: id.clone(),
        name: dedup_name(clean, &settings.vaults),
        path: file.to_string_lossy().into_owned(),
        kind: VaultKind::EncryptedDb,
        config,
    };

    // Create the live DB + seed the initial snapshot at the vault location.
    {
        let live = live_db_path(&app, &id)?;
        if let Some(parent) = live.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("Could not create live dir: {e}"))?;
        }
        let container = Container::create(&live, &key, &vault.name)?;
        container.export_snapshot(&file)?;
    }

    settings.vaults.push(vault.clone());
    settings.active_vault_id = Some(id.clone());
    save_settings(&app, &settings)?;

    open_and_activate(&app, &state, &vault, key)?;
    session.store(&id, key);
    if remember {
        let _ = crypto::keyring_store(&id, &key);
    }

    Ok(VaultInfo {
        id: vault.id,
        name: vault.name,
        path: vault.path,
        kind: vault.kind,
        status: VaultStatus::Ok,
    })
}

/// Unlocks an encrypted-db vault with `password`, opening it as the active
/// backend. A wrong password errors (SQLCipher fails to open). On success the
/// derived key is cached in the session and, if `remember`, the OS keyring.
/// Called by the shared unlock command in [`crate::providers::unlock`].
pub fn unlock_with_password(
    app: &tauri::AppHandle,
    state: &AppState,
    session: &SecretsSession,
    vault: &Vault,
    password: &str,
    remember: bool,
) -> Result<(), String> {
    let salt_hex = vault
        .config
        .get("salt")
        .and_then(|v| v.as_str())
        .ok_or("Vault is missing its key salt")?;
    let salt = crypto::from_hex(salt_hex)?;
    let key = crypto::derive_vault_key(password, &salt)?;

    open_and_activate(app, state, vault, key)?;
    session.store(&vault.id, key);
    if remember {
        let _ = crypto::keyring_store(&vault.id, &key);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Date helper (YYYY-MM-DD, UTC) — no chrono dependency
// ---------------------------------------------------------------------------

/// Today's date as `YYYY-MM-DD` (UTC), for conflict-copy naming.
pub fn today() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let days = secs.div_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Howard Hinnant's `civil_from_days`: days-since-epoch → (year, month, day).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_from_days_known_dates() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        // 2026-07-11 is 20645 days after the epoch.
        assert_eq!(civil_from_days(20_645), (2026, 7, 11));
    }
}
