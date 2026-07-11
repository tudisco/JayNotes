//! The `encrypted-files` provider: a Syncthing-friendly, file-per-note vault
//! whose backing directory holds rclone-crypt-compatible ciphertext.
//!
//! ## Shape (vs. encrypted-db)
//!
//! Where encrypted-db packs everything into one SQLCipher container, this
//! provider keeps **one ciphertext file per note** in a folder, exactly like a
//! plain vault but with encrypted names and content. That makes it safe to sync
//! with Syncthing (whole-file, atomic writes, per-file conflict handling) and
//! readable by rclone itself with the same password.
//!
//! * **Names** — every path segment is AES-256-EME name-encrypted (deterministic).
//! * **Content** — XSalsa20-Poly1305 chunked (authenticated → wrong key fails).
//! * **Search index** — a *separate* SQLCipher-keyed DB in app-data, keyed by a
//!   value derived from the vault's own key material (see [`crypto::derive_index_key`]),
//!   so decrypted note text never touches a plaintext index. The handle reports
//!   `owns_index() == false` (search still dispatches through `state.index`) but
//!   `owns_reindex() == true` (only the handle can decrypt to rebuild it).
//! * **Unlock** — the rclone KDF (`Keys::derive`) is different from encrypted-db's
//!   scrypt/SQLCipher key, so the vault stores no salt of its own; the derived
//!   80-byte `Keys` material lives in the shared [`SecretsSession`] (and, opt-in,
//!   the OS keyring as *derived material*, never the password). A wrong password
//!   is caught by the [`verify_key`] probe.
//!
//! ## AI revisions
//!
//! Undo snapshots for AI writes live under a dot-prefixed `.revisions/` prefix
//! *inside* the encrypted backing (encrypted like everything else, hidden from
//! the tree). The AI layer routes revision reads/writes through this handle for
//! encrypted vaults — see `crate::ai`.

pub mod cipher;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use tauri::Manager;
use walkdir::WalkDir;

use crate::index::{self, register_write, AppState, Index};
use crate::providers::crypto::{self, SecretsSession};
use crate::providers::{field, Capabilities, ProviderMeta, VaultHandle, VaultProvider};
use crate::vault::{
    self, atomic_write, load_settings, new_id, safe_join, save_settings, TreeNode, Vault, VaultKind,
};
use crate::vaults::{dedup_name, VaultInfo, VaultStatus};

use cipher::CryptCipher;

/// Literal (un-encrypted) probe file at the backing root whose authenticated
/// content validates a password on unlock. Dot-prefixed so it stays out of the
/// tree; rclone treats it as a harmless stray.
const PROBE_FILE: &str = ".jaynotes-check";
const PROBE_CONTENT: &[u8] = b"jaynotes-encrypted-files-v1";

// ---------------------------------------------------------------------------
// Provider
// ---------------------------------------------------------------------------

pub struct EncryptedFilesProvider;

impl EncryptedFilesProvider {
    pub const CAPS: Capabilities = Capabilities {
        // The backing folder shows only encrypted gibberish names, so revealing a
        // note in Finder is meaningless — the action is hidden in the UI.
        reveal_in_finder: false,
        needs_unlock: true,
        // Notes are individual (encrypted) files on disk.
        folder_backed: true,
    };
}

impl VaultProvider for EncryptedFilesProvider {
    fn kind(&self) -> &'static str {
        "encrypted-files"
    }

    fn metadata(&self) -> ProviderMeta {
        ProviderMeta {
            kind: "encrypted-files".into(),
            display_name: "Encrypted files (Syncthing-friendly)".into(),
            description:
                "A folder of individually-encrypted notes — rclone-compatible, safe to sync."
                    .into(),
            config_fields: vec![
                field("location", "Location", "folder", true, "Where to save the vault folder"),
                field("name", "Vault name", "text", true, "My Synced Notes"),
                field("password", "Password", "password", true, "Choose a strong password"),
                field("confirm", "Confirm password", "password", true, "Re-enter the password"),
                field(
                    "password2",
                    "Salt / second password (advanced, optional)",
                    "text",
                    false,
                    "Leave empty for rclone's default",
                ),
            ],
            capabilities: Self::CAPS,
        }
    }
}

// ---------------------------------------------------------------------------
// Handle
// ---------------------------------------------------------------------------

/// The opened handle for an encrypted-files vault.
pub struct EncryptedFilesHandle {
    /// The ciphertext backing directory (what Syncthing/rclone see).
    backing: PathBuf,
    cipher: Arc<CryptCipher>,
    /// Shared clone of `state.index` — the separate keyed FTS index this handle
    /// populates. `Arc` so the handle can rebuild it without `&AppState`.
    index: Arc<Mutex<Option<Index>>>,
    /// Shared clone of `state.recent_writes` for CIPHERTEXT self-write
    /// suppression (the watcher observes ciphertext paths).
    recent: Arc<Mutex<HashMap<String, std::time::Instant>>>,
}

impl EncryptedFilesHandle {
    /// Constructs the handle over an already-verified backing dir + cipher, and
    /// does a full decrypt-scan to (re)populate the keyed index. The key MUST be
    /// verified by [`verify_key`] before this is called.
    fn open_at(
        backing: &Path,
        cipher: Arc<CryptCipher>,
        index: Arc<Mutex<Option<Index>>>,
        recent: Arc<Mutex<HashMap<String, std::time::Instant>>>,
    ) -> Result<Self, String> {
        let handle = EncryptedFilesHandle {
            backing: backing.to_path_buf(),
            cipher,
            index,
            recent,
        };
        handle.reindex()?;
        Ok(handle)
    }
}

impl EncryptedFilesHandle {
    /// The backing (ciphertext) absolute path for a plaintext vault-relative
    /// `rel`, jailed to the backing dir.
    fn backing_path(&self, rel: &str) -> Result<PathBuf, String> {
        let ct_rel = self.cipher.encrypt_rel(rel)?;
        safe_join(&self.backing, &ct_rel)
    }

    /// True if a note file exists at plaintext `rel`.
    fn note_exists(&self, rel: &str) -> bool {
        self.backing_path(rel).map(|p| p.is_file()).unwrap_or(false)
    }

    /// True if a folder exists at plaintext `rel`.
    fn dir_exists(&self, rel: &str) -> bool {
        self.backing_path(rel).map(|p| p.is_dir()).unwrap_or(false)
    }

    /// Upserts one note into the keyed index (best-effort; hidden paths skipped).
    fn index_upsert(&self, rel: &str, content: &str) {
        if index::rel_is_hidden(rel) {
            return;
        }
        if let Ok(guard) = self.index.lock() {
            if let Some(idx) = guard.as_ref() {
                let _ = idx.index_file(rel, content);
            }
        }
    }

    /// Walks the backing dir and returns `(entries, stray_count)`, where each
    /// entry is `(plaintext_rel, is_dir)`. Strays (segments that don't decrypt)
    /// and hidden (dot-prefixed) decrypted paths are skipped.
    fn decrypted_entries(&self) -> (Vec<(String, bool)>, usize) {
        let mut out: Vec<(String, bool)> = Vec::new();
        let mut strays = 0usize;

        for entry in WalkDir::new(&self.backing)
            .min_depth(1)
            .into_iter()
            .filter_entry(|e| !e.file_name().to_string_lossy().starts_with('.'))
        {
            let entry = match entry {
                Ok(e) => e,
                Err(_) => {
                    strays += 1;
                    continue;
                }
            };
            let is_dir = entry.file_type().is_dir();
            let backing_rel = match entry.path().strip_prefix(&self.backing) {
                Ok(r) => r,
                Err(_) => continue,
            };
            match self.decrypt_backing_rel(backing_rel) {
                DecryptOutcome::Ok(plain_rel) => out.push((plain_rel, is_dir)),
                DecryptOutcome::Hidden => {}
                DecryptOutcome::Stray => {
                    strays += 1;
                }
            }
        }
        (out, strays)
    }

    /// Decrypts a full backing relative path segment-by-segment to a plaintext
    /// rel, or classifies it as hidden/stray.
    fn decrypt_backing_rel(&self, backing_rel: &Path) -> DecryptOutcome {
        let mut plain_parts: Vec<String> = Vec::new();
        for comp in backing_rel.components() {
            let seg = match comp {
                std::path::Component::Normal(s) => s.to_string_lossy().into_owned(),
                _ => return DecryptOutcome::Stray,
            };
            match self.cipher.decrypt_backing_name(&seg) {
                Ok(plain) => {
                    if plain.starts_with('.') {
                        return DecryptOutcome::Hidden;
                    }
                    plain_parts.push(plain);
                }
                Err(_) => return DecryptOutcome::Stray,
            }
        }
        DecryptOutcome::Ok(plain_parts.join("/"))
    }

    /// Finds a free `Untitled`/`Untitled n` note rel inside plaintext `dir`.
    fn unique_untitled(&self, dir: &str) -> Result<String, String> {
        let join = |name: &str| {
            if dir.is_empty() {
                name.to_string()
            } else {
                format!("{dir}/{name}")
            }
        };
        let first = join("Untitled.md");
        if !self.note_exists(&first) {
            return Ok(first);
        }
        for n in 1..10_000 {
            let cand = join(&format!("Untitled {n}.md"));
            if !self.note_exists(&cand) {
                return Ok(cand);
            }
        }
        Err("Could not find a free Untitled name".into())
    }

    /// Finds a free `attachments/base.ext` (then `base-1.ext`, …) plaintext rel.
    fn unique_attachment(&self, base: &str, ext: &str) -> Result<String, String> {
        let first = format!("attachments/{base}.{ext}");
        if !self.note_exists(&first) {
            return Ok(first);
        }
        for n in 1..100_000 {
            let cand = format!("attachments/{base}-{n}.{ext}");
            if !self.note_exists(&cand) {
                return Ok(cand);
            }
        }
        Err("Could not find a free attachment name".into())
    }

    /// Encrypts `bytes` to the backing path for plaintext `rel` (atomic write in
    /// the ciphertext dir), registering the CIPHERTEXT path for self-write
    /// suppression.
    fn write_encrypted(&self, rel: &str, bytes: &[u8]) -> Result<(), String> {
        let ct_rel = self.cipher.encrypt_rel(rel)?;
        let ct_abs = safe_join(&self.backing, &ct_rel)?;
        if let Some(parent) = ct_abs.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("Could not create parent folders: {e}"))?;
        }
        register_write(&self.recent, &ct_rel);
        let ciphertext = self.cipher.encrypt_content(bytes)?;
        atomic_write(&ct_abs, &ciphertext)
    }

    /// Reads and decrypts the raw bytes of the note/attachment at plaintext `rel`.
    fn read_decrypted(&self, rel: &str) -> Result<Vec<u8>, String> {
        let ct_abs = self.backing_path(rel)?;
        if !ct_abs.is_file() {
            return Err(format!("Does not exist: {rel}"));
        }
        let ct = std::fs::read(&ct_abs).map_err(|e| format!("Could not read '{rel}': {e}"))?;
        self.cipher.decrypt_content(&ct)
    }
}

/// Classification of a backing entry during a scan.
enum DecryptOutcome {
    Ok(String),
    Hidden,
    Stray,
}

impl VaultHandle for EncryptedFilesHandle {
    fn capabilities(&self) -> Capabilities {
        EncryptedFilesProvider::CAPS
    }

    fn scan_tree(&self) -> Result<TreeNode, String> {
        let (entries, strays) = self.decrypted_entries();
        if strays > 0 {
            eprintln!(
                "encrypted-files: skipped {strays} undecryptable stray entry(ies) in {}",
                self.backing.display()
            );
        }
        let mut root = vault::empty_root_node(&self.backing);
        for (rel, is_dir) in entries {
            // Only folders and `.md` notes appear in the tree (attachments live
            // under attachments/ and aren't shown), matching the plain scanner.
            let is_md = rel.rsplit('/').next().map(is_markdown_name).unwrap_or(false);
            if is_dir || is_md {
                vault::insert_node(&mut root, Path::new(&rel), is_dir);
            }
        }
        vault::sort_tree(&mut root);
        Ok(root)
    }

    fn read_note(&self, rel: &str) -> Result<String, String> {
        let bytes = self.read_decrypted(rel)?;
        String::from_utf8(bytes).map_err(|_| format!("Note '{rel}' is not valid UTF-8"))
    }

    fn write_note(&self, _state: &AppState, rel: &str, content: &str) -> Result<(), String> {
        self.write_encrypted(rel, content.as_bytes())?;
        self.index_upsert(rel, content);
        Ok(())
    }

    fn create_note(&self, state: &AppState, rel: &str) -> Result<String, String> {
        let created = if rel.is_empty() || self.dir_exists(rel) {
            self.unique_untitled(rel.trim_matches('/'))?
        } else {
            let mut r = rel.to_string();
            if !is_markdown_name(rel.rsplit('/').next().unwrap_or(rel)) {
                r.push_str(".md");
            }
            if self.note_exists(&r) {
                return Err(format!("A note named '{r}' already exists"));
            }
            r
        };
        self.write_note(state, &created, "")?;
        Ok(created)
    }

    fn create_folder(&self, rel: &str) -> Result<(), String> {
        if rel.trim().is_empty() {
            return Err("Folder name cannot be empty".into());
        }
        if self.dir_exists(rel) || self.note_exists(rel) {
            return Err(format!("'{rel}' already exists"));
        }
        let ct_abs = self.backing_path(rel)?;
        std::fs::create_dir_all(&ct_abs).map_err(|e| format!("Could not create folder '{rel}': {e}"))
    }

    fn rename(&self, _state: &AppState, old_rel: &str, new_rel: &str) -> Result<(), String> {
        let old_ct = self.backing_path(old_rel)?;
        let new_ct = self.backing_path(new_rel)?;
        if !old_ct.exists() {
            return Err(format!("'{old_rel}' does not exist"));
        }
        if new_ct.exists() {
            return Err(format!("'{new_rel}' already exists"));
        }
        if let Some(parent) = new_ct.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("Could not create parent folders: {e}"))?;
        }
        // Register both ciphertext paths for self-write suppression.
        if let Ok(cr) = self.cipher.encrypt_rel(old_rel) {
            register_write(&self.recent, &cr);
        }
        if let Ok(cr) = self.cipher.encrypt_rel(new_rel) {
            register_write(&self.recent, &cr);
        }
        std::fs::rename(&old_ct, &new_ct)
            .map_err(|e| format!("Could not rename '{old_rel}' to '{new_rel}': {e}"))?;
        if let Ok(guard) = self.index.lock() {
            if let Some(idx) = guard.as_ref() {
                let _ = idx.rename(old_rel, new_rel);
            }
        }
        Ok(())
    }

    fn trash(&self, _state: &AppState, rel: &str) -> Result<(), String> {
        let ct_abs = self.backing_path(rel)?;
        if !ct_abs.exists() {
            return Err(format!("'{rel}' does not exist"));
        }
        let was_dir = ct_abs.is_dir();
        if let Ok(cr) = self.cipher.encrypt_rel(rel) {
            register_write(&self.recent, &cr);
        }
        move_to_trash(&self.backing, &ct_abs)
            .map_err(|e| format!("Could not move '{rel}' to Trash: {e}"))?;
        if let Ok(guard) = self.index.lock() {
            if let Some(idx) = guard.as_ref() {
                let _ = if was_dir {
                    idx.remove_prefix(rel)
                } else {
                    idx.remove_file(rel)
                };
            }
        }
        Ok(())
    }

    fn save_attachment(
        &self,
        _state: &AppState,
        file_name: &str,
        data: &[u8],
    ) -> Result<String, String> {
        let (base, ext) = vault::sanitize_attachment_name(file_name)?;
        let rel = self.unique_attachment(&base, &ext)?;
        self.write_encrypted(&rel, data)?;
        Ok(rel)
    }

    fn read_attachment(&self, rel: &str) -> Result<Vec<u8>, String> {
        self.read_decrypted(rel)
    }

    fn reveal_in_finder(&self, _rel: &str) -> Result<(), String> {
        Err("This vault stores notes as encrypted files — there's nothing readable to reveal in Finder".into())
    }

    fn owns_index(&self) -> bool {
        false
    }
    fn owns_reindex(&self) -> bool {
        true
    }

    /// Full decrypt-scan: upsert every current note into the keyed index and
    /// drop index rows for notes no longer present. Returns the count indexed.
    fn reindex(&self) -> Result<usize, String> {
        let (entries, _strays) = self.decrypted_entries();
        let mut present: HashSet<String> = HashSet::new();
        let mut indexed = 0usize;
        for (rel, is_dir) in &entries {
            if *is_dir || !is_markdown_name(rel.rsplit('/').next().unwrap_or(rel)) {
                continue;
            }
            present.insert(rel.clone());
            let content = match self.read_note(rel) {
                Ok(c) => c,
                Err(_) => continue, // unreadable note (e.g. mid-sync) — skip
            };
            if let Ok(guard) = self.index.lock() {
                if let Some(idx) = guard.as_ref() {
                    idx.index_file(rel, &content)?;
                    indexed += 1;
                }
            }
        }
        // Prune index rows for notes that vanished.
        if let Ok(guard) = self.index.lock() {
            if let Some(idx) = guard.as_ref() {
                for note in idx.list_notes()? {
                    if !present.contains(&note.path) {
                        idx.remove_file(&note.path)?;
                    }
                }
            }
        }
        Ok(indexed)
    }
}

/// True for a `.md`/`.MD` filename.
fn is_markdown_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.ends_with(".md")
}

/// Moves a backing file/folder to the OS Trash (or, under test, a hidden
/// `.trash-enc/` inside the backing dir so tests can assert removal without
/// touching the real Trash). Never hard-deletes.
#[cfg(not(test))]
fn move_to_trash(_backing: &Path, abs: &Path) -> Result<(), String> {
    trash::delete(abs).map_err(|e| e.to_string())
}

#[cfg(test)]
fn move_to_trash(backing: &Path, abs: &Path) -> Result<(), String> {
    let trash_dir = backing.join(".trash-enc");
    std::fs::create_dir_all(&trash_dir).map_err(|e| e.to_string())?;
    let name = abs
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_else(|| "trashed".into());
    let mut dest = trash_dir.join(&name);
    let mut n = 1;
    while dest.exists() {
        dest = trash_dir.join(format!("{}-{n}", name.to_string_lossy()));
        n += 1;
    }
    std::fs::rename(abs, &dest).map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// Key verification (wrong-password probe)
// ---------------------------------------------------------------------------

/// Verifies `cipher`'s key against the backing dir, and on an empty/probe-less
/// vault establishes the probe for next time.
///
/// 1. If the literal `.jaynotes-check` probe exists, decrypt its authenticated
///    content — success proves the key; failure is a wrong password.
/// 2. Otherwise (externally-created rclone vault, or a fresh one), try to
///    content-decrypt existing notes: any success proves the key; if there are
///    notes but none decrypt, it's a wrong password; a truly empty vault is
///    accepted. On acceptance the probe file is written so future unlocks use
///    path 1.
fn verify_key(backing: &Path, cipher: &CryptCipher) -> Result<(), String> {
    let probe = backing.join(PROBE_FILE);
    if probe.is_file() {
        let ct = std::fs::read(&probe).map_err(|e| format!("Could not read probe: {e}"))?;
        return cipher
            .decrypt_content(&ct)
            .map(|_| ())
            .map_err(|_| wrong_password());
    }

    // No probe yet. Try existing notes.
    let mut examined = 0usize;
    let mut decrypted_any = false;
    for entry in WalkDir::new(backing)
        .min_depth(1)
        .into_iter()
        .filter_entry(|e| !e.file_name().to_string_lossy().starts_with('.'))
    {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        if !entry.file_type().is_file() {
            continue;
        }
        examined += 1;
        if let Ok(bytes) = std::fs::read(entry.path()) {
            if cipher.decrypt_content(&bytes).is_ok() {
                decrypted_any = true;
                break;
            }
        }
    }
    if examined > 0 && !decrypted_any {
        return Err(wrong_password());
    }

    // Accepted (empty vault or a note decrypted) — establish the probe.
    write_probe(backing, cipher)?;
    Ok(())
}

fn wrong_password() -> String {
    "Wrong password — could not decrypt this vault.".to_string()
}

/// Writes the literal `.jaynotes-check` probe (authenticated ciphertext of a
/// fixed marker) at the backing root.
fn write_probe(backing: &Path, cipher: &CryptCipher) -> Result<(), String> {
    std::fs::create_dir_all(backing).map_err(|e| format!("Could not create vault folder: {e}"))?;
    let ct = cipher.encrypt_content(PROBE_CONTENT)?;
    atomic_write(&backing.join(PROBE_FILE), &ct)
}

// ---------------------------------------------------------------------------
// Activation
// ---------------------------------------------------------------------------

/// Opens (verifying the key), installs the keyed index + handle + watcher as the
/// active backend. On any error the backend is left cleared (locked).
fn open_and_activate(
    app: &tauri::AppHandle,
    state: &AppState,
    vault: &Vault,
    cipher: CryptCipher,
) -> Result<(), String> {
    let backing = PathBuf::from(&vault.path);
    if !backing.is_dir() {
        return Err(format!("Vault folder is missing: {}", vault.path));
    }

    // Verify BEFORE touching shared state so a wrong password changes nothing.
    verify_key(&backing, &cipher)?;

    let material = cipher.key_material();
    let index_key = crypto::derive_index_key(&material)?;
    let idx = index::open_keyed_index(app, &backing, &index_key)?;
    *state.index.lock().unwrap() = Some(idx);

    let cipher = Arc::new(cipher);
    let handle = match EncryptedFilesHandle::open_at(
        &backing,
        cipher.clone(),
        state.index.clone(),
        state.recent_writes.clone(),
    ) {
        Ok(h) => h,
        Err(e) => {
            *state.index.lock().unwrap() = None;
            return Err(e);
        }
    };

    // Watch the ciphertext backing; the watcher maps events to plaintext.
    match crate::watcher::start_crypt_watcher(
        app.clone(),
        state.index.clone(),
        state.recent_writes.clone(),
        backing.clone(),
        cipher,
    ) {
        Ok(w) => *state.watcher.lock().unwrap() = Some(w),
        Err(e) => eprintln!("encrypted-files watcher failed: {e}"),
    }

    *state.active.lock().unwrap() = Some(Box::new(handle));
    Ok(())
}

/// Attempts to open the vault automatically from an already-unlocked session or
/// remembered keyring material. Never prompts. Returns true on success.
pub fn auto_open(app: &tauri::AppHandle, state: &AppState, vault: &Vault) -> bool {
    let session = app.state::<SecretsSession>();
    let material = session
        .get_bytes(&vault.id)
        .or_else(|| crypto::keyring_get_bytes(&vault.id));
    if let Some(material) = material {
        if let Ok(cipher) = CryptCipher::from_material(&material) {
            if open_and_activate(app, state, vault, cipher).is_ok() {
                session.store_bytes(&vault.id, material);
                return true;
            }
        }
    }
    false
}

/// Unlocks with a password (+ optional password2), installing the vault as
/// active. A wrong password errors via the probe. On success the derived key
/// material is cached in the session and, if `remember`, the keyring.
pub fn unlock_with_password(
    app: &tauri::AppHandle,
    state: &AppState,
    session: &SecretsSession,
    vault: &Vault,
    password: &str,
    password2: &str,
    remember: bool,
) -> Result<(), String> {
    let cipher = CryptCipher::derive(password, password2)?;
    let material = cipher.key_material().to_vec();
    open_and_activate(app, state, vault, cipher)?;
    session.store_bytes(&vault.id, material.clone());
    if remember {
        let _ = crypto::keyring_store_bytes(&vault.id, &material);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Create command
// ---------------------------------------------------------------------------

/// Creates a new encrypted-files vault: a fresh folder at `location/name` whose
/// backing holds only the encrypted `.jaynotes-check` probe, adds it to
/// settings, and opens it. The vault config stores **no secrets** — only a
/// `kdf: "rclone"` marker (the rclone KDF needs no stored salt).
#[tauri::command]
pub async fn create_encrypted_files_vault(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    session: tauri::State<'_, SecretsSession>,
    location: String,
    name: String,
    password: String,
    password2: Option<String>,
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
    let backing = dir.join(clean);
    if backing.exists() {
        return Err(format!("A folder named '{clean}' already exists here"));
    }
    std::fs::create_dir(&backing).map_err(|e| format!("Could not create vault folder: {e}"))?;

    let password2 = password2.unwrap_or_default();
    let cipher = CryptCipher::derive(&password, &password2)?;
    // Seed the probe so the vault is never empty-and-probe-less.
    write_probe(&backing, &cipher)?;

    let mut settings = load_settings(&app)?;
    let id = new_id();
    let mut config = serde_json::Map::new();
    config.insert("kdf".into(), serde_json::Value::String("rclone".into()));
    let vault = Vault {
        id: id.clone(),
        name: dedup_name(clean, &settings.vaults),
        path: backing.to_string_lossy().into_owned(),
        kind: VaultKind::EncryptedFiles,
        config,
    };
    settings.vaults.push(vault.clone());
    settings.active_vault_id = Some(id.clone());
    save_settings(&app, &settings)?;

    let material = cipher.key_material().to_vec();
    open_and_activate(&app, &state, &vault, cipher)?;
    session.store_bytes(&id, material.clone());
    if remember {
        let _ = crypto::keyring_store_bytes(&id, &material);
    }

    Ok(VaultInfo {
        id: vault.id,
        name: vault.name,
        path: vault.path,
        kind: vault.kind,
        status: VaultStatus::Ok,
    })
}

#[cfg(test)]
mod tests;
