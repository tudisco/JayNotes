//! The `tinylord` provider: a hosted vault backed by a self-hosted TinyLord
//! document server.
//!
//! ## Shape (vs. the other providers)
//!
//! Where plain/encrypted vaults are local files, this vault's notes are
//! **documents on a remote server** — the server is the source of truth. Notes
//! live in a `notes` collection (`{path, title, content, mtime}`), empty folders
//! in a `folders` collection (`{path}`), and attachments in an `attachments`
//! collection (`{path, bytes_b64, mtime}`). Every handle op maps onto document
//! CRUD, keyed by the note's `path` field (a local `path → document-id` map,
//! seeded by the initial sync, avoids a lookup round-trip on each write).
//!
//! ## Search index + realtime
//!
//! A **local, unkeyed** SQLite FTS index mirrors the notes so search/tags/quick-
//! switcher work exactly as for a plain vault. It reports `owns_index() == false`
//! (search dispatches through `state.index`) but `owns_reindex() == true` (only
//! this handle can rebuild it from the server). On connect it does a full sync
//! (query every note → rebuild the index), then opens one SSE `subscribe` stream
//! per collection (`notes`, `folders`, `attachments`) on the handle's runtime: change events
//! update the local index and emit the existing `vault-changed` event, so an edit
//! on another device appears here within seconds. The index is plaintext — a
//! deliberate tradeoff: the server already stores everything encrypted at rest,
//! and a local plaintext FTS mirror is what makes offline-free search instant.
//!
//! ## Deletes / concurrency (from TinyLord's `writer.rs` / `realtime.rs`)
//!
//! Server `DELETE` is authoritative and is broadcast as an SSE `delete` event to
//! every device, so notes are **hard-deleted** — no tombstones are needed. The
//! document envelope carries no version/revision field (only `updated_at`), so
//! multi-device conflicts are plain **last-writer-wins**.
//!
//! ## Tokens vs. the keyring (why we remember the *password*)
//!
//! TinyLord's refresh session **rotates on every refresh and expires** (default
//! 30 days) and can be revoked, so a stored refresh token is fragile. The
//! password, by contrast, always re-logs-in cleanly. So — unlike the encrypted
//! providers, which remember derived key material — this provider's "remember"
//! stores the **login password** in the OS keyring. Live access/refresh tokens
//! stay only in memory inside the connected client.

pub mod client;

#[cfg(test)]
mod tests;

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use tauri::{Emitter, Manager};
use tokio::runtime::Runtime;
use tokio::task::JoinHandle;
use zeroize::Zeroizing;

use crate::index::{
    self, open_unkeyed_index_for_key, register_write, title_of, was_recently_written, AppState,
    Index,
};
use crate::providers::{field, Capabilities, ProviderMeta, VaultHandle, VaultProvider};
use crate::vault::{load_settings, new_id, save_settings, TreeNode, Vault, VaultKind};
use crate::vaults::{dedup_name, VaultInfo, VaultStatus};

use client::{ChangeEvent, DocEnvelope, SseParser, TinyClient, TinyError};

/// Collection names.
const NOTES: &str = "notes";
const FOLDERS: &str = "folders";
const ATTACHMENTS: &str = "attachments";

/// Keyring service name (account is the vault id). Stores the login password.
const KEYRING_SERVICE: &str = "jaynotes-tinylord";

// ---------------------------------------------------------------------------
// Provider
// ---------------------------------------------------------------------------

pub struct TinylordProvider;

impl TinylordProvider {
    pub const CAPS: Capabilities = Capabilities {
        reveal_in_finder: false,
        needs_unlock: true,
        // Notes are documents inside a server, not individual local files.
        folder_backed: false,
    };
}

impl VaultProvider for TinylordProvider {
    fn kind(&self) -> &'static str {
        "tinylord"
    }

    fn metadata(&self) -> ProviderMeta {
        ProviderMeta {
            kind: "tinylord".into(),
            display_name: "TinyLord server".into(),
            description: "Notes hosted on your TinyLord server, live-synced across devices.".into(),
            config_fields: vec![
                field("url", "Server URL", "url", true, "https://notes.example.com"),
                field("database", "Database", "text", true, "jaynotes").with_default("jaynotes"),
                field("username", "Username", "text", true, "your-username"),
                field("password", "Password", "password", true, "your password"),
            ],
            capabilities: Self::CAPS,
            unlock_label: Some("Sign in".into()),
        }
    }
}

// ---------------------------------------------------------------------------
// Session state (Tauri-managed) — remembers the login password per vault
// ---------------------------------------------------------------------------

/// Per-vault login credential held in memory, so switching back to a tinylord
/// vault reconnects silently. `is_unlocked` means "we hold a password to
/// (re)connect with", mirroring the encrypted providers' key-material session.
#[derive(Default)]
pub struct TinyLordSessions {
    passwords: Mutex<HashMap<String, Zeroizing<String>>>,
}

impl TinyLordSessions {
    pub fn store(&self, vault_id: &str, password: &str) {
        self.passwords
            .lock()
            .unwrap()
            .insert(vault_id.to_string(), Zeroizing::new(password.to_string()));
    }

    pub fn get(&self, vault_id: &str) -> Option<String> {
        self.passwords
            .lock()
            .unwrap()
            .get(vault_id)
            .map(|p| p.to_string())
    }

    pub fn is_unlocked(&self, vault_id: &str) -> bool {
        self.passwords.lock().unwrap().contains_key(vault_id)
    }

    pub fn lock(&self, vault_id: &str) {
        self.passwords.lock().unwrap().remove(vault_id);
    }
}

/// Stores the login password in the OS keyring (best-effort). Unlike the
/// encrypted providers this persists the *password*, because TinyLord refresh
/// tokens rotate + expire and a password always re-logs-in cleanly.
fn keyring_store_password(vault_id: &str, password: &str) -> Result<(), String> {
    let entry =
        keyring::Entry::new(KEYRING_SERVICE, vault_id).map_err(|e| format!("Keyring: {e}"))?;
    entry
        .set_password(password)
        .map_err(|e| format!("Keyring: {e}"))
}

/// Fetches a remembered password from the OS keyring, or `None`.
fn keyring_get_password(vault_id: &str) -> Option<String> {
    let entry = keyring::Entry::new(KEYRING_SERVICE, vault_id).ok()?;
    entry.get_password().ok()
}

/// Removes a remembered password (on lock / vault removal). Best-effort.
pub fn keyring_delete(vault_id: &str) {
    if let Ok(entry) = keyring::Entry::new(KEYRING_SERVICE, vault_id) {
        let _ = entry.delete_credential();
    }
}

// ---------------------------------------------------------------------------
// Local path → document-id maps
// ---------------------------------------------------------------------------

/// Bidirectional caches so writes/renames/deletes can address a document by its
/// server id without a lookup, and SSE `delete` events (which carry only an id)
/// can resolve back to a path.
#[derive(Default)]
struct Maps {
    note_id: HashMap<String, String>,   // path → id
    note_path: HashMap<String, String>, // id → path
    folder_id: HashMap<String, String>,
    folder_path: HashMap<String, String>,
    attach_id: HashMap<String, String>,   // path → id
    attach_path: HashMap<String, String>, // id → path
}

impl Maps {
    fn insert_note(&mut self, path: &str, id: &str) {
        self.note_id.insert(path.to_string(), id.to_string());
        self.note_path.insert(id.to_string(), path.to_string());
    }
    fn remove_note_by_path(&mut self, path: &str) -> Option<String> {
        let id = self.note_id.remove(path)?;
        self.note_path.remove(&id);
        Some(id)
    }
    fn remove_note_by_id(&mut self, id: &str) -> Option<String> {
        let path = self.note_path.remove(id)?;
        self.note_id.remove(&path);
        Some(path)
    }
    fn insert_folder(&mut self, path: &str, id: &str) {
        self.folder_id.insert(path.to_string(), id.to_string());
        self.folder_path.insert(id.to_string(), path.to_string());
    }
    fn remove_folder_by_path(&mut self, path: &str) -> Option<String> {
        let id = self.folder_id.remove(path)?;
        self.folder_path.remove(&id);
        Some(id)
    }
    fn remove_folder_by_id(&mut self, id: &str) -> Option<String> {
        let path = self.folder_path.remove(id)?;
        self.folder_id.remove(&path);
        Some(path)
    }
    fn insert_attachment(&mut self, path: &str, id: &str) {
        self.attach_id.insert(path.to_string(), id.to_string());
        self.attach_path.insert(id.to_string(), path.to_string());
    }
    fn remove_attachment_by_id(&mut self, id: &str) -> Option<String> {
        let path = self.attach_path.remove(id)?;
        self.attach_id.remove(&path);
        Some(path)
    }
}

// ---------------------------------------------------------------------------
// Handle
// ---------------------------------------------------------------------------

/// The opened handle for a tinylord vault.
pub struct TinyLordHandle {
    client: Arc<TinyClient>,
    /// A dedicated runtime driving the reqwest reactor + SSE tasks. `Option` so
    /// `Drop` can shut it down in the background (dropping a runtime inline in an
    /// async context would panic).
    rt: Option<Runtime>,
    maps: Arc<Mutex<Maps>>,
    index: Arc<Mutex<Option<Index>>>,
    recent: Arc<Mutex<HashMap<String, Instant>>>,
    /// Root name shown for the tree (the database name).
    db: String,
    cancel: Arc<AtomicBool>,
    tasks: Vec<JoinHandle<()>>,
}

impl TinyLordHandle {
    /// Blocks on `fut` using the handle's runtime from a scratch thread. The
    /// handle's sync trait methods are invoked from inside Tauri's async runtime,
    /// where a direct `Runtime::block_on` would panic ("runtime within runtime");
    /// running it on a fresh thread (which has no entered runtime) is always safe.
    fn block_on<F>(&self, fut: F) -> F::Output
    where
        F: Future + Send,
        F::Output: Send,
    {
        let rt = self.rt.as_ref().expect("runtime present");
        std::thread::scope(|s| s.spawn(|| rt.block_on(fut)).join().unwrap())
    }

    /// True if a note is known at `rel`.
    fn note_exists(&self, rel: &str) -> bool {
        self.maps.lock().unwrap().note_id.contains_key(rel)
    }

    /// True if a folder is known at `rel`, or a note lives under `rel/`.
    fn dir_exists(&self, rel: &str) -> bool {
        let maps = self.maps.lock().unwrap();
        if maps.folder_id.contains_key(rel) {
            return true;
        }
        let prefix = format!("{rel}/");
        maps.note_id.keys().any(|p| p.starts_with(&prefix))
            || maps.folder_id.keys().any(|p| p.starts_with(&prefix))
    }

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

    fn index_remove(&self, rel: &str) {
        if let Ok(guard) = self.index.lock() {
            if let Some(idx) = guard.as_ref() {
                let _ = idx.remove_file(rel);
            }
        }
    }

    fn index_rename(&self, old_rel: &str, new_rel: &str) {
        if let Ok(guard) = self.index.lock() {
            if let Some(idx) = guard.as_ref() {
                let _ = idx.rename(old_rel, new_rel);
            }
        }
    }

    /// Finds a free `Untitled`/`Untitled n` note path inside plaintext `dir`.
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

    fn unique_attachment(&self, base: &str, ext: &str) -> Result<String, String> {
        let maps = self.maps.lock().unwrap();
        let taken = |p: &str| maps.attach_id.contains_key(p);
        let first = format!("attachments/{base}.{ext}");
        if !taken(&first) {
            return Ok(first);
        }
        for n in 1..100_000 {
            let cand = format!("attachments/{base}-{n}.{ext}");
            if !taken(&cand) {
                return Ok(cand);
            }
        }
        Err("Could not find a free attachment name".into())
    }

    /// Upserts one note document by path (PUT when the id is known, else POST),
    /// updating the id map. Returns the server id.
    async fn upsert_note(&self, rel: &str, content: &str) -> Result<String, TinyError> {
        let doc = note_doc(rel, content);
        let existing = self.maps.lock().unwrap().note_id.get(rel).cloned();
        let env = match existing {
            Some(id) => self.client.put_doc(NOTES, &id, &doc).await?,
            None => self.client.create_doc(NOTES, &doc).await?,
        };
        self.maps.lock().unwrap().insert_note(rel, &env.id);
        Ok(env.id)
    }
}

impl Drop for TinyLordHandle {
    fn drop(&mut self) {
        // Stop the SSE loops promptly, then shut the runtime down off-thread so
        // dropping it never blocks the caller (which may be inside a runtime).
        self.cancel.store(true, Ordering::SeqCst);
        for t in &self.tasks {
            t.abort();
        }
        if let Some(rt) = self.rt.take() {
            rt.shutdown_background();
        }
    }
}

impl VaultHandle for TinyLordHandle {
    fn capabilities(&self) -> Capabilities {
        TinylordProvider::CAPS
    }

    fn scan_tree(&self) -> Result<TreeNode, String> {
        let mut root = TreeNode {
            name: self.db.clone(),
            path: String::new(),
            is_dir: true,
            children: Vec::new(),
        };
        let maps = self.maps.lock().unwrap();
        // Hidden (dot-prefixed) paths — e.g. AI `.revisions/` documents — stay
        // out of the tree, matching every other provider's scanner.
        for path in maps.folder_id.keys() {
            if index::rel_is_hidden(path) {
                continue;
            }
            crate::vault::insert_node(&mut root, std::path::Path::new(path), true);
        }
        for path in maps.note_id.keys() {
            if index::rel_is_hidden(path) {
                continue;
            }
            crate::vault::insert_node(&mut root, std::path::Path::new(path), false);
        }
        drop(maps);
        crate::vault::sort_tree(&mut root);
        Ok(root)
    }

    fn read_note(&self, rel: &str) -> Result<String, String> {
        let env = self
            .block_on(self.client.find_by_path(NOTES, rel))
            .map_err(String::from)?
            .ok_or_else(|| format!("Note does not exist: {rel}"))?;
        Ok(env
            .doc
            .get("content")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string())
    }

    fn write_note(&self, state: &AppState, rel: &str, content: &str) -> Result<(), String> {
        register_write(&state.recent_writes, rel);
        self.block_on(self.upsert_note(rel, content))
            .map_err(String::from)?;
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
        let rel = rel.trim();
        if rel.is_empty() {
            return Err("Folder name cannot be empty".into());
        }
        if self.dir_exists(rel) || self.note_exists(rel) {
            return Err(format!("'{rel}' already exists"));
        }
        register_write(&self.recent, rel);
        let doc = serde_json::json!({ "path": rel });
        let env = self
            .block_on(self.client.create_doc(FOLDERS, &doc))
            .map_err(String::from)?;
        self.maps.lock().unwrap().insert_folder(rel, &env.id);
        Ok(())
    }

    fn rename(&self, state: &AppState, old_rel: &str, new_rel: &str) -> Result<(), String> {
        if self.note_exists(new_rel) || self.dir_exists(new_rel) {
            return Err(format!("'{new_rel}' already exists"));
        }
        register_write(&state.recent_writes, old_rel);
        register_write(&state.recent_writes, new_rel);

        if self.note_exists(old_rel) {
            self.block_on(self.rename_note(old_rel, new_rel))
                .map_err(String::from)?;
            self.index_rename(old_rel, new_rel);
            return Ok(());
        }
        // Folder rename: rewrite every note/folder under the prefix.
        self.block_on(self.rename_folder(old_rel, new_rel))
            .map_err(String::from)?;
        self.index_rename(old_rel, new_rel);
        Ok(())
    }

    fn trash(&self, state: &AppState, rel: &str) -> Result<(), String> {
        register_write(&state.recent_writes, rel);
        if self.note_exists(rel) {
            let id = self.maps.lock().unwrap().note_id.get(rel).cloned();
            if let Some(id) = id {
                self.block_on(self.client.delete_doc(NOTES, &id))
                    .map_err(String::from)?;
            }
            self.maps.lock().unwrap().remove_note_by_path(rel);
            self.index_remove(rel);
            return Ok(());
        }
        // Folder: delete the folder doc plus everything under it.
        self.block_on(self.trash_folder(rel)).map_err(String::from)?;
        if let Ok(guard) = self.index.lock() {
            if let Some(idx) = guard.as_ref() {
                let _ = idx.remove_prefix(rel);
            }
        }
        Ok(())
    }

    fn save_attachment(
        &self,
        state: &AppState,
        file_name: &str,
        data: &[u8],
    ) -> Result<String, String> {
        let (base, ext) = crate::vault::sanitize_attachment_name(file_name)?;
        let rel = self.unique_attachment(&base, &ext)?;
        register_write(&state.recent_writes, &rel);
        let doc = serde_json::json!({
            "path": rel,
            "bytes_b64": base64_encode(data),
            "mtime": now_ms(),
        });
        let env = self
            .block_on(self.client.create_doc(ATTACHMENTS, &doc))
            .map_err(String::from)?;
        self.maps.lock().unwrap().insert_attachment(&rel, &env.id);
        Ok(rel)
    }

    fn read_attachment(&self, rel: &str) -> Result<Vec<u8>, String> {
        let env = self
            .block_on(self.client.find_by_path(ATTACHMENTS, rel))
            .map_err(String::from)?
            .ok_or_else(|| format!("Attachment does not exist: {rel}"))?;
        let b64 = env
            .doc
            .get("bytes_b64")
            .and_then(|v| v.as_str())
            .ok_or_else(|| format!("Attachment '{rel}' has no data"))?;
        base64_decode(b64).ok_or_else(|| format!("Attachment '{rel}' is corrupt"))
    }

    fn reveal_in_finder(&self, _rel: &str) -> Result<(), String> {
        Err("This vault's notes live on a TinyLord server — there's nothing to reveal in Finder".into())
    }

    fn owns_index(&self) -> bool {
        false
    }
    fn owns_reindex(&self) -> bool {
        true
    }

    /// Full re-sync from the server: rebuild the local index + maps.
    fn reindex(&self) -> Result<usize, String> {
        let sync = self.block_on(sync_all(&self.client)).map_err(String::from)?;
        let count = sync.notes.len();
        apply_full_sync(&self.index, &self.maps, sync);
        Ok(count)
    }
}

// Handle-internal async helpers (kept off the trait so `block_on` can call them).
impl TinyLordHandle {
    async fn rename_note(&self, old_rel: &str, new_rel: &str) -> Result<(), TinyError> {
        let env = self
            .client
            .find_by_path(NOTES, old_rel)
            .await?
            .ok_or_else(|| TinyError::Api {
                status: 404,
                message: format!("Note not found: {old_rel}"),
            })?;
        let content = env
            .doc
            .get("content")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let doc = note_doc(new_rel, content);
        self.client.put_doc(NOTES, &env.id, &doc).await?;
        let mut maps = self.maps.lock().unwrap();
        maps.remove_note_by_path(old_rel);
        maps.insert_note(new_rel, &env.id);
        Ok(())
    }

    async fn rename_folder(&self, old_rel: &str, new_rel: &str) -> Result<(), TinyError> {
        let prefix = format!("{old_rel}/");
        // Snapshot affected paths (release the lock before awaiting).
        let (note_paths, folder_paths) = {
            let maps = self.maps.lock().unwrap();
            let notes: Vec<String> = maps
                .note_id
                .keys()
                .filter(|p| p.as_str() == old_rel || p.starts_with(&prefix))
                .cloned()
                .collect();
            let folders: Vec<String> = maps
                .folder_id
                .keys()
                .filter(|p| p.as_str() == old_rel || p.starts_with(&prefix))
                .cloned()
                .collect();
            (notes, folders)
        };
        let remap = |p: &str| format!("{new_rel}{}", &p[old_rel.len()..]);

        for old_p in note_paths {
            let new_p = remap(&old_p);
            self.rename_note(&old_p, &new_p).await?;
        }
        for old_p in folder_paths {
            let new_p = remap(&old_p);
            let id = self.maps.lock().unwrap().folder_id.get(&old_p).cloned();
            if let Some(id) = id {
                let doc = serde_json::json!({ "path": new_p });
                self.client.put_doc(FOLDERS, &id, &doc).await?;
                let mut maps = self.maps.lock().unwrap();
                maps.remove_folder_by_path(&old_p);
                maps.insert_folder(&new_p, &id);
            }
        }
        Ok(())
    }

    async fn trash_folder(&self, rel: &str) -> Result<(), TinyError> {
        let prefix = format!("{rel}/");
        let (note_ids, folder_ids, note_paths, folder_paths) = {
            let maps = self.maps.lock().unwrap();
            let matches = |p: &str| p == rel || p.starts_with(&prefix);
            let note_paths: Vec<String> =
                maps.note_id.keys().filter(|p| matches(p)).cloned().collect();
            let folder_paths: Vec<String> = maps
                .folder_id
                .keys()
                .filter(|p| matches(p))
                .cloned()
                .collect();
            let note_ids: Vec<String> = note_paths
                .iter()
                .filter_map(|p| maps.note_id.get(p).cloned())
                .collect();
            let folder_ids: Vec<String> = folder_paths
                .iter()
                .filter_map(|p| maps.folder_id.get(p).cloned())
                .collect();
            (note_ids, folder_ids, note_paths, folder_paths)
        };
        for id in note_ids {
            self.client.delete_doc(NOTES, &id).await?;
        }
        for id in folder_ids {
            self.client.delete_doc(FOLDERS, &id).await?;
        }
        let mut maps = self.maps.lock().unwrap();
        for p in note_paths {
            maps.remove_note_by_path(&p);
        }
        for p in folder_paths {
            maps.remove_folder_by_path(&p);
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Document helpers
// ---------------------------------------------------------------------------

fn note_doc(rel: &str, content: &str) -> serde_json::Value {
    serde_json::json!({
        "path": rel,
        "title": title_of(rel),
        "content": content,
        "mtime": now_ms(),
    })
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn is_markdown_name(name: &str) -> bool {
    name.to_ascii_lowercase().ends_with(".md")
}

fn doc_path(doc: &serde_json::Value) -> Option<String> {
    doc.get("path").and_then(|v| v.as_str()).map(str::to_string)
}

fn doc_content(doc: &serde_json::Value) -> String {
    doc.get("content")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

// ---------------------------------------------------------------------------
// Sync
// ---------------------------------------------------------------------------

/// Everything a full server sync returns.
struct FullSync {
    notes: Vec<DocEnvelope>,
    folders: Vec<DocEnvelope>,
    /// Attachment `(id, path)` pairs, path-projected (no `bytes_b64` transfer).
    attachments: Vec<(String, String)>,
}

/// Queries every note + folder document (and attachment paths) from the server.
async fn sync_all(client: &TinyClient) -> Result<FullSync, TinyError> {
    Ok(FullSync {
        notes: client.query_all(NOTES, None).await?,
        folders: client.query_all(FOLDERS, None).await?,
        attachments: client.query_paths(ATTACHMENTS).await?,
    })
}

/// Rebuilds the local index + maps from a full document sync: upserts everything
/// present and prunes index rows / map entries for anything no longer on the
/// server. Also seeds the attachment id maps (from path-projected pairs).
fn apply_full_sync(index: &Arc<Mutex<Option<Index>>>, maps: &Arc<Mutex<Maps>>, sync: FullSync) {
    let mut present: HashSet<String> = HashSet::new();
    let mut new_maps = Maps::default();

    // Notes → index + maps.
    for env in &sync.notes {
        if let Some(path) = doc_path(&env.doc) {
            present.insert(path.clone());
            new_maps.insert_note(&path, &env.id);
            if !index::rel_is_hidden(&path) {
                if let Ok(guard) = index.lock() {
                    if let Some(idx) = guard.as_ref() {
                        let _ = idx.index_file(&path, &doc_content(&env.doc));
                    }
                }
            }
        }
    }
    for env in &sync.folders {
        if let Some(path) = doc_path(&env.doc) {
            new_maps.insert_folder(&path, &env.id);
        }
    }
    for (id, path) in &sync.attachments {
        new_maps.insert_attachment(path, id);
    }

    // Prune index rows for notes that vanished server-side.
    if let Ok(guard) = index.lock() {
        if let Some(idx) = guard.as_ref() {
            if let Ok(existing) = idx.list_notes() {
                for note in existing {
                    if !present.contains(&note.path) {
                        let _ = idx.remove_file(&note.path);
                    }
                }
            }
        }
    }

    *maps.lock().unwrap() = new_maps;
}

// ---------------------------------------------------------------------------
// Connection-status reporter + realtime SSE tasks
// ---------------------------------------------------------------------------

#[derive(Serialize, Clone)]
struct ConnectionPayload {
    #[serde(rename = "vaultId")]
    vault_id: String,
    status: &'static str,
}

#[derive(Serialize, Clone)]
struct ChangePayload {
    paths: Vec<String>,
}

#[derive(Serialize, Clone)]
struct SessionExpiredPayload {
    #[serde(rename = "vaultId")]
    vault_id: String,
}

/// Tracks how many subscriber tasks are currently disconnected and emits a
/// single `tinylord-connection` transition (connected ↔ reconnecting) so the UI
/// shows one banner regardless of which stream dropped.
struct ConnReporter {
    app: tauri::AppHandle,
    vault_id: String,
    down: AtomicUsize,
}

impl ConnReporter {
    fn went_down(&self) {
        if self.down.fetch_add(1, Ordering::SeqCst) == 0 {
            let _ = self.app.emit(
                "tinylord-connection",
                ConnectionPayload {
                    vault_id: self.vault_id.clone(),
                    status: "reconnecting",
                },
            );
        }
    }
    fn came_up(&self) {
        if self.down.fetch_sub(1, Ordering::SeqCst) == 1 {
            let _ = self.app.emit(
                "tinylord-connection",
                ConnectionPayload {
                    vault_id: self.vault_id.clone(),
                    status: "connected",
                },
            );
        }
    }
    fn session_expired(&self) {
        let _ = self.app.emit(
            "tinylord-session-expired",
            SessionExpiredPayload {
                vault_id: self.vault_id.clone(),
            },
        );
    }
}

/// Everything a subscriber task needs, bundled so the spawn site stays readable.
struct SseCtx {
    client: Arc<TinyClient>,
    maps: Arc<Mutex<Maps>>,
    index: Arc<Mutex<Option<Index>>>,
    recent: Arc<Mutex<HashMap<String, Instant>>>,
    app: tauri::AppHandle,
    reporter: Arc<ConnReporter>,
    cancel: Arc<AtomicBool>,
}

/// The reconnecting SSE loop for one collection. Never panics; a dropped stream
/// backs off (1s → 30s cap) and resumes with `Last-Event-ID`. An auth failure
/// stops the loop and signals the UI to re-prompt for sign-in.
async fn subscribe_loop(coll: &'static str, ctx: SseCtx) {
    let mut last_id: Option<i64> = None;
    let mut backoff = Duration::from_secs(1);
    let mut is_down = false;

    loop {
        if ctx.cancel.load(Ordering::SeqCst) {
            break;
        }
        match ctx.client.open_subscribe(coll, last_id).await {
            Ok(mut resp) => {
                if is_down {
                    ctx.reporter.came_up();
                    is_down = false;
                }
                backoff = Duration::from_secs(1);
                let mut parser = SseParser::new();
                loop {
                    if ctx.cancel.load(Ordering::SeqCst) {
                        return;
                    }
                    match resp.chunk().await {
                        Ok(Some(bytes)) => {
                            for frame in parser.feed(&bytes) {
                                apply_frame(coll, &frame.event, &frame.data, &ctx, &mut last_id)
                                    .await;
                            }
                        }
                        Ok(None) => break, // server closed the stream
                        Err(_) => break,   // network error mid-stream
                    }
                }
            }
            Err(TinyError::SessionExpired) | Err(TinyError::Auth(_)) => {
                ctx.reporter.session_expired();
                return;
            }
            Err(_) => { /* transient: fall through to backoff */ }
        }

        if ctx.cancel.load(Ordering::SeqCst) {
            break;
        }
        if !is_down {
            ctx.reporter.went_down();
            is_down = true;
        }
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(Duration::from_secs(30));
    }
}

/// Applies one decoded SSE frame to the local index/tree and emits
/// `vault-changed` for affected note/folder paths.
async fn apply_frame(
    coll: &str,
    event: &str,
    data: &str,
    ctx: &SseCtx,
    last_id: &mut Option<i64>,
) {
    if event == "resync" {
        // We fell behind the changelog; re-read everything.
        if let Ok(sync) = sync_all(&ctx.client).await {
            let paths: Vec<String> = sync.notes.iter().filter_map(|e| doc_path(&e.doc)).collect();
            apply_full_sync(&ctx.index, &ctx.maps, sync);
            let _ = ctx.app.emit("vault-changed", ChangePayload { paths });
        }
        return;
    }
    if event != "change" {
        return; // keep-alive / unknown
    }
    let ev: ChangeEvent = match serde_json::from_str(data) {
        Ok(e) => e,
        Err(_) => return,
    };
    *last_id = Some(ev.seq);

    // Resolve the affected path.
    let path = match ev.op.as_str() {
        "insert" | "update" => match ev.doc.as_ref().and_then(doc_path) {
            Some(p) => p,
            None => return,
        },
        "delete" => {
            let maps = ctx.maps.lock().unwrap();
            let by = match coll {
                FOLDERS => &maps.folder_path,
                ATTACHMENTS => &maps.attach_path,
                _ => &maps.note_path,
            };
            match by.get(&ev.id).cloned() {
                Some(p) => p,
                None => return, // never seen this doc locally
            }
        }
        _ => return,
    };

    // Suppress the echo of our own writes.
    if was_recently_written(&ctx.recent, &path) {
        // Still make sure our map knows the server id (for a fresh create).
        let mut maps = ctx.maps.lock().unwrap();
        match coll {
            NOTES => maps.insert_note(&path, &ev.id),
            FOLDERS => maps.insert_folder(&path, &ev.id),
            ATTACHMENTS => maps.insert_attachment(&path, &ev.id),
            _ => {}
        }
        return;
    }

    match (coll, ev.op.as_str()) {
        (NOTES, "insert") | (NOTES, "update") => {
            let content = ev.doc.as_ref().map(doc_content).unwrap_or_default();
            ctx.maps.lock().unwrap().insert_note(&path, &ev.id);
            if !index::rel_is_hidden(&path) {
                if let Ok(guard) = ctx.index.lock() {
                    if let Some(idx) = guard.as_ref() {
                        let _ = idx.index_file(&path, &content);
                    }
                }
            }
        }
        (NOTES, "delete") => {
            ctx.maps.lock().unwrap().remove_note_by_id(&ev.id);
            if let Ok(guard) = ctx.index.lock() {
                if let Some(idx) = guard.as_ref() {
                    let _ = idx.remove_file(&path);
                }
            }
        }
        (FOLDERS, "insert") | (FOLDERS, "update") => {
            ctx.maps.lock().unwrap().insert_folder(&path, &ev.id);
        }
        (FOLDERS, "delete") => {
            ctx.maps.lock().unwrap().remove_folder_by_id(&ev.id);
        }
        (ATTACHMENTS, "insert") | (ATTACHMENTS, "update") => {
            ctx.maps.lock().unwrap().insert_attachment(&path, &ev.id);
        }
        (ATTACHMENTS, "delete") => {
            ctx.maps.lock().unwrap().remove_attachment_by_id(&ev.id);
        }
        _ => {}
    }

    let _ = ctx.app.emit(
        "vault-changed",
        ChangePayload {
            paths: vec![path],
        },
    );
}

// ---------------------------------------------------------------------------
// Activation
// ---------------------------------------------------------------------------

/// Runs `fut` on `rt` from a scratch thread (see `TinyLordHandle::block_on`).
fn block_on_rt<F>(rt: &Runtime, fut: F) -> F::Output
where
    F: Future + Send,
    F::Output: Send,
{
    std::thread::scope(|s| s.spawn(|| rt.block_on(fut)).join().unwrap())
}

/// A stable index-file key for a vault (URL + database).
fn index_key(vault: &Vault, database: &str) -> String {
    format!("tinylord:{}:{}", client::normalize_base(&vault.path), database)
}

/// Logs in, opens the local index, does the initial sync, and installs the
/// handle (with its SSE tasks) as the active backend. On any failure the backend
/// is left cleared (locked).
fn open_and_activate(
    app: &tauri::AppHandle,
    state: &AppState,
    vault: &Vault,
    username: &str,
    password: &str,
) -> Result<(), String> {
    let base = client::normalize_base(&vault.path);
    let database = vault
        .config
        .get("database")
        .and_then(|v| v.as_str())
        .unwrap_or("jaynotes")
        .to_string();

    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .map_err(|e| format!("Could not start network runtime: {e}"))?;

    // Log in (verifies credentials). A wrong password / unreachable server errors.
    let client = block_on_rt(&rt, TinyClient::login(&base, &database, username, password))
        .map_err(String::from)?;

    // Best-effort: ensure a path index exists on each collection (needs the
    // per-db admin grant; a plain write user's 403 is swallowed inside).
    block_on_rt(&rt, async {
        client.ensure_path_index(NOTES).await;
        client.ensure_path_index(FOLDERS).await;
        client.ensure_path_index(ATTACHMENTS).await;
    });

    // Open the local (plaintext, unkeyed) FTS mirror.
    let idx = open_unkeyed_index_for_key(app, &index_key(vault, &database))?;
    *state.index.lock().unwrap() = Some(idx);

    // Initial full sync.
    let maps = Arc::new(Mutex::new(Maps::default()));
    let sync = match block_on_rt(&rt, sync_all(&client)) {
        Ok(v) => v,
        Err(e) => {
            *state.index.lock().unwrap() = None;
            return Err(e.into());
        }
    };
    apply_full_sync(&state.index, &maps, sync);

    // Spawn the realtime subscribers.
    let client = Arc::new(client);
    let cancel = Arc::new(AtomicBool::new(false));
    let reporter = Arc::new(ConnReporter {
        app: app.clone(),
        vault_id: vault.id.clone(),
        down: AtomicUsize::new(0),
    });

    let mut tasks = Vec::new();
    for coll in [NOTES, FOLDERS, ATTACHMENTS] {
        let ctx = SseCtx {
            client: client.clone(),
            maps: maps.clone(),
            index: state.index.clone(),
            recent: state.recent_writes.clone(),
            app: app.clone(),
            reporter: reporter.clone(),
            cancel: cancel.clone(),
        };
        tasks.push(rt.spawn(subscribe_loop(coll, ctx)));
    }

    let handle = TinyLordHandle {
        client,
        rt: Some(rt),
        maps,
        index: state.index.clone(),
        recent: state.recent_writes.clone(),
        db: database,
        cancel,
        tasks,
    };

    *state.watcher.lock().unwrap() = None;
    *state.active.lock().unwrap() = Some(Box::new(handle));
    Ok(())
}

/// Attempts to open the vault silently from a remembered password (session or
/// keyring). Never prompts. Returns true on success.
pub fn auto_open(app: &tauri::AppHandle, state: &AppState, vault: &Vault) -> bool {
    let session = app.state::<TinyLordSessions>();
    let username = vault
        .config
        .get("username")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let password = session
        .get(&vault.id)
        .or_else(|| keyring_get_password(&vault.id));
    if let Some(pw) = password {
        if open_and_activate(app, state, vault, &username, &pw).is_ok() {
            session.store(&vault.id, &pw);
            return true;
        }
    }
    false
}

/// Unlocks a tinylord vault by logging in, installing it as active. On success
/// the password is cached in the session and (if `remember`) the OS keyring.
/// Called by the shared unlock command.
pub fn unlock_with_login(
    app: &tauri::AppHandle,
    state: &AppState,
    session: &TinyLordSessions,
    vault: &Vault,
    username: &str,
    password: &str,
    remember: bool,
) -> Result<(), String> {
    open_and_activate(app, state, vault, username, password)?;
    session.store(&vault.id, password);
    if remember {
        let _ = keyring_store_password(&vault.id, password);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Create command
// ---------------------------------------------------------------------------

/// Creates a new tinylord vault: verifies the login, stores the vault (URL +
/// database + username; **no password** persisted in settings), connects it as
/// active, and optionally remembers the password. On a failed connect the vault
/// entry is rolled back so a broken vault never lingers.
#[tauri::command]
pub async fn create_tinylord_vault(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    session: tauri::State<'_, TinyLordSessions>,
    url: String,
    database: String,
    username: String,
    password: String,
    remember: bool,
) -> Result<VaultInfo, String> {
    let base = client::normalize_base(&url);
    if base.is_empty() || !base.contains("://") {
        return Err("A valid server URL is required".into());
    }
    let database = if database.trim().is_empty() {
        "jaynotes".to_string()
    } else {
        database.trim().to_string()
    };
    if username.trim().is_empty() {
        return Err("A username is required".into());
    }
    if password.is_empty() {
        return Err("A password is required".into());
    }

    let mut settings = load_settings(&app)?;
    let id = new_id();
    let host = base.split("://").nth(1).unwrap_or(&base);
    let default_name = format!("{database} @ {host}");
    let mut config = serde_json::Map::new();
    config.insert("database".into(), serde_json::Value::String(database.clone()));
    config.insert("username".into(), serde_json::Value::String(username.clone()));
    let vault = Vault {
        id: id.clone(),
        name: dedup_name(&default_name, &settings.vaults),
        path: base,
        kind: VaultKind::Tinylord,
        config,
    };

    let prev_active = settings.active_vault_id.clone();
    settings.vaults.push(vault.clone());
    settings.active_vault_id = Some(id.clone());
    save_settings(&app, &settings)?;

    // Connect. On failure, roll the settings back so no broken vault persists.
    if let Err(e) = open_and_activate(&app, &state, &vault, &username, &password) {
        let mut settings = load_settings(&app)?;
        settings.vaults.retain(|v| v.id != id);
        settings.active_vault_id = prev_active;
        let _ = save_settings(&app, &settings);
        return Err(e);
    }

    session.store(&id, &password);
    if remember {
        let _ = keyring_store_password(&id, &password);
    }

    Ok(VaultInfo {
        id: vault.id,
        name: vault.name,
        path: vault.path,
        kind: vault.kind,
        status: VaultStatus::Ok,
    })
}

// ---------------------------------------------------------------------------
// Base64 (standard alphabet) — self-contained, no dep in the main crate
// ---------------------------------------------------------------------------

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn base64_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(B64[(n >> 18) as usize & 63] as char);
        out.push(B64[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            B64[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            B64[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

fn base64_decode(s: &str) -> Option<Vec<u8>> {
    let val = |c: u8| -> Option<u32> {
        match c {
            b'A'..=b'Z' => Some((c - b'A') as u32),
            b'a'..=b'z' => Some((c - b'a' + 26) as u32),
            b'0'..=b'9' => Some((c - b'0' + 52) as u32),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    };
    let mut out = Vec::new();
    let mut acc = 0u32;
    let mut bits = 0u32;
    for &c in s.as_bytes() {
        if c == b'=' || c.is_ascii_whitespace() {
            continue;
        }
        let v = val(c)?;
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}
