//! Vault management: settings persistence, vault scanning, and file commands.
//!
//! Every command that takes a `rel_path` joins it against the vault root via
//! [`safe_join`], which rejects absolute paths and any `..` components so a
//! caller can never escape the vault.

use std::fs;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use tauri::Manager;
use tauri_plugin_dialog::DialogExt;
use walkdir::WalkDir;

use crate::index::{self, register_write, AppState};

const SETTINGS_FILE: &str = "settings.json";

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// The kind of a vault. `Plain` is the built-in default; `EncryptedDb` (M14) is
/// the first non-plain kind, gated behind the `provider-encrypted-db` feature at
/// the provider layer (the enum variant is always present so a settings file
/// written by a full build still deserializes in a plain-only build — such a
/// vault simply shows "unsupported"; see `providers::kind_supported`).
///
/// Kept as an enum (rather than a bare string) so downstream `match` statements
/// are exhaustive and the compiler flags every site that must learn about a new
/// kind. Serializes kebab-case (`"plain"`, `"encrypted-db"`) to match the
/// frontend/provider `kind()` contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum VaultKind {
    #[default]
    Plain,
    EncryptedDb,
    /// M15: rclone-crypt file-per-note vault (Syncthing-friendly). Gated behind
    /// `provider-encrypted-files` at the provider layer; the variant is always
    /// present so a settings file written by a full build still deserializes in
    /// a build without the feature (shows "unsupported").
    EncryptedFiles,
}

impl VaultKind {
    /// The stable string id used by the provider registry and the frontend.
    pub fn as_str(self) -> &'static str {
        match self {
            VaultKind::Plain => "plain",
            VaultKind::EncryptedDb => "encrypted-db",
            VaultKind::EncryptedFiles => "encrypted-files",
        }
    }
}

/// One configured vault: a stable id, a display name, an absolute path (a folder
/// for plain vaults, or the `<name>.jaynotes` container file for encrypted-db),
/// its kind, and provider-specific config.
///
/// `config` holds non-secret provider data persisted in plaintext — e.g. an
/// encrypted-db vault's KDF `salt` (a salt is not secret) — so the shared struct
/// stays kind-agnostic while providers stash what they need. Persisted verbatim
/// in `settings.json` under `vaults`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Vault {
    pub id: String,
    pub name: String,
    pub path: String,
    #[serde(default)]
    pub kind: VaultKind,
    #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub config: serde_json::Map<String, serde_json::Value>,
}

/// Persisted app settings. Unknown keys are preserved via `extra` so future
/// milestones can add fields without clobbering older ones.
///
/// The legacy single-vault `vault_path` field is still deserialized so the
/// [`migrate`] step can convert it into a `vaults` entry, after which it is
/// dropped (never re-serialized once `vaults` is populated).
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vault_path: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub vaults: Vec<Vault>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_vault_id: Option<String>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// Generates a short, collision-resistant, nanoid-style id (hex).
///
/// Combines the current time in nanoseconds, the process id, and a per-process
/// monotonic counter so ids are unique within and across app runs without
/// pulling in a uuid/rand dependency.
pub(crate) fn new_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let pid = std::process::id() as u64;
    let c = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{nanos:x}{pid:x}{c:x}")
}

/// The folder basename of a path, used as a vault's default display name.
/// Falls back to "Vault" for a path with no final component.
pub(crate) fn basename_of(path: &str) -> String {
    Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "Vault".to_string())
}

/// In-memory migration applied on every [`load_settings`]: if the legacy
/// `vault_path` is set and no `vaults` exist yet, convert it into a single
/// active vault entry. The legacy field is always cleared afterward so it is
/// never re-serialized. All other keys (`ai`, unknowns) are untouched.
fn migrate(settings: &mut Settings) {
    if settings.vaults.is_empty() {
        if let Some(path) = settings.vault_path.take() {
            if !path.trim().is_empty() {
                let id = new_id();
                settings.vaults.push(Vault {
                    id: id.clone(),
                    name: basename_of(&path),
                    path,
                    kind: VaultKind::Plain,
                    config: Default::default(),
                });
                settings.active_vault_id = Some(id);
            }
        }
    }
    // Legacy key is fully superseded by `vaults`; never keep it around.
    settings.vault_path = None;
}

/// Returns the active vault entry, if one is set and present in the list.
pub(crate) fn active_vault(settings: &Settings) -> Option<&Vault> {
    let id = settings.active_vault_id.as_ref()?;
    settings.vaults.iter().find(|v| &v.id == id)
}

/// One node of the vault file tree sent to the frontend.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TreeNode {
    pub name: String,
    /// Path relative to the vault root, always forward-slash separated.
    pub path: String,
    pub is_dir: bool,
    pub children: Vec<TreeNode>,
}

// ---------------------------------------------------------------------------
// Settings helpers
// ---------------------------------------------------------------------------

fn settings_path(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    let dir = app
        .path()
        .app_config_dir()
        .map_err(|e| format!("Could not resolve app config dir: {e}"))?;
    Ok(dir.join(SETTINGS_FILE))
}

pub(crate) fn load_settings(app: &tauri::AppHandle) -> Result<Settings, String> {
    let path = settings_path(app)?;
    if !path.exists() {
        return Ok(Settings::default());
    }
    let raw = fs::read_to_string(&path)
        .map_err(|e| format!("Could not read settings file: {e}"))?;
    let mut settings: Settings =
        serde_json::from_str(&raw).map_err(|e| format!("Settings file is not valid JSON: {e}"))?;
    migrate(&mut settings);
    Ok(settings)
}

pub(crate) fn save_settings(app: &tauri::AppHandle, settings: &Settings) -> Result<(), String> {
    let path = settings_path(app)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("Could not create config dir: {e}"))?;
    }
    let raw = serde_json::to_string_pretty(settings)
        .map_err(|e| format!("Could not serialize settings: {e}"))?;
    // Atomic write so a crash or a Syncthing race can never leave a partial
    // settings.json (which would wipe the vault list).
    atomic_write(&path, raw.as_bytes())
}

/// Returns the canonicalized active vault root, erroring if no vault is
/// configured or the directory no longer exists (e.g. an offline drive).
pub(crate) fn vault_root(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    let settings = load_settings(app)?;
    let vault = active_vault(&settings).ok_or_else(|| "No vault is configured".to_string())?;
    let root = PathBuf::from(&vault.path);
    if !root.is_dir() {
        return Err(format!("Vault directory no longer exists: {}", vault.path));
    }
    root.canonicalize()
        .map_err(|e| format!("Could not resolve vault directory: {e}"))
}

// ---------------------------------------------------------------------------
// Path safety
// ---------------------------------------------------------------------------

/// Joins `rel` onto `root`, rejecting absolute paths and any path component
/// that is not a plain name (`..`, drive prefixes, root dirs). `.` components
/// and an empty string (meaning the root itself) are allowed.
pub(crate) fn safe_join(root: &Path, rel: &str) -> Result<PathBuf, String> {
    let rel_path = Path::new(rel);
    if rel_path.is_absolute() {
        return Err(format!("Absolute paths are not allowed: {rel}"));
    }
    let mut out = root.to_path_buf();
    for comp in rel_path.components() {
        match comp {
            Component::Normal(part) => out.push(part),
            Component::CurDir => {}
            _ => return Err(format!("Path escapes the vault: {rel}")),
        }
    }
    Ok(out)
}

/// Converts an absolute path inside the vault back to a forward-slash
/// relative path.
pub(crate) fn to_rel_string(root: &Path, abs: &Path) -> Result<String, String> {
    let rel = abs
        .strip_prefix(root)
        .map_err(|_| format!("Path is outside the vault: {}", abs.display()))?;
    Ok(rel
        .components()
        .filter_map(|c| match c {
            Component::Normal(part) => Some(part.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/"))
}

// ---------------------------------------------------------------------------
// Tree scanning
// ---------------------------------------------------------------------------

fn is_hidden(entry: &walkdir::DirEntry) -> bool {
    entry.file_name().to_string_lossy().starts_with('.')
}

fn is_markdown(path: &Path) -> bool {
    path.extension()
        .map(|ext| ext.eq_ignore_ascii_case("md"))
        .unwrap_or(false)
}

/// Walks `root` and builds a tree of folders and `.md` files. Hidden
/// (dot-prefixed) files and directories are skipped entirely. Children are
/// sorted folders-first, then case-insensitive alphabetical.
pub(crate) fn scan_tree(root: &Path) -> Result<TreeNode, String> {
    let mut root_node = TreeNode {
        name: root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| root.display().to_string()),
        path: String::new(),
        is_dir: true,
        children: Vec::new(),
    };

    for entry in WalkDir::new(root)
        .min_depth(1)
        .into_iter()
        .filter_entry(|e| !is_hidden(e))
    {
        let entry = entry.map_err(|e| format!("Error scanning vault: {e}"))?;
        let is_dir = entry.file_type().is_dir();
        if !is_dir && !is_markdown(entry.path()) {
            continue;
        }
        let rel = entry
            .path()
            .strip_prefix(root)
            .map_err(|e| format!("Error scanning vault: {e}"))?
            .to_path_buf();
        insert_node(&mut root_node, &rel, is_dir);
    }

    sort_tree(&mut root_node);
    Ok(root_node)
}

/// Builds an empty root `TreeNode` named after `root`'s basename (its `path` is
/// the empty string). Used by the encrypted-files provider, which populates it
/// from decrypted plaintext paths.
#[cfg(feature = "provider-encrypted-files")]
pub(crate) fn empty_root_node(root: &Path) -> TreeNode {
    TreeNode {
        name: root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| root.display().to_string()),
        path: String::new(),
        is_dir: true,
        children: Vec::new(),
    }
}

pub(crate) fn insert_node(root: &mut TreeNode, rel: &Path, is_dir: bool) {
    let parts: Vec<String> = rel
        .components()
        .filter_map(|c| match c {
            Component::Normal(part) => Some(part.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect();

    let mut cur = root;
    for (i, name) in parts.iter().enumerate() {
        let is_last = i == parts.len() - 1;
        let existing = cur.children.iter().position(|c| c.name == *name);
        let idx = match existing {
            Some(idx) => idx,
            None => {
                cur.children.push(TreeNode {
                    name: name.clone(),
                    path: parts[..=i].join("/"),
                    is_dir: if is_last { is_dir } else { true },
                    children: Vec::new(),
                });
                cur.children.len() - 1
            }
        };
        cur = &mut cur.children[idx];
    }
}

pub(crate) fn sort_tree(node: &mut TreeNode) {
    node.children.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    for child in &mut node.children {
        sort_tree(child);
    }
}

/// Finds the first available "Untitled" name in `dir`:
/// `Untitled.md`, `Untitled 1.md`, `Untitled 2.md`, ...
fn unique_untitled(dir: &Path) -> Result<PathBuf, String> {
    let first = dir.join("Untitled.md");
    if !first.exists() {
        return Ok(first);
    }
    for n in 1..10_000 {
        let candidate = dir.join(format!("Untitled {n}.md"));
        if !candidate.exists() {
            return Ok(candidate);
        }
    }
    Err("Could not find a free Untitled name".to_string())
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

/// Opens a native folder picker. Returns the chosen absolute path, or null
/// if the user cancelled.
#[tauri::command]
pub async fn pick_vault(app: tauri::AppHandle) -> Result<Option<String>, String> {
    match app.dialog().file().blocking_pick_folder() {
        Some(folder) => {
            let path = folder
                .into_path()
                .map_err(|e| format!("Invalid folder selection: {e}"))?;
            Ok(Some(path.to_string_lossy().into_owned()))
        }
        None => Ok(None),
    }
}

/// Returns the active vault's path, or null if none is set (or its directory
/// no longer exists). A thin wrapper kept so untouched callers keep working;
/// the vault switcher uses `list_vaults` instead.
#[tauri::command]
pub async fn get_vault(app: tauri::AppHandle) -> Result<Option<String>, String> {
    let settings = load_settings(&app)?;
    Ok(active_vault(&settings)
        .map(|v| v.path.clone())
        .filter(|p| Path::new(p).is_dir()))
}

/// Adds `path` as a vault (or reuses an existing entry with the same canonical
/// path), makes it active, and (re)opens the index + watcher for it. Kept as a
/// thin wrapper over the multi-vault model so existing callers (e.g. the
/// first-run "Open vault" flow) don't churn.
#[tauri::command]
pub async fn set_vault(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    path: String,
) -> Result<(), String> {
    let dir = PathBuf::from(&path);
    if !dir.is_dir() {
        return Err(format!("Not a directory: {path}"));
    }
    let canonical = dir
        .canonicalize()
        .map_err(|e| format!("Could not resolve directory: {e}"))?;
    let canonical_str = canonical.to_string_lossy().into_owned();

    let mut settings = load_settings(&app)?;
    // Reuse an existing entry with the same canonical path, else create one.
    let id = match settings
        .vaults
        .iter()
        .find(|v| crate::vaults::same_path(&v.path, &canonical_str))
    {
        Some(v) => v.id.clone(),
        None => {
            let id = new_id();
            let name = crate::vaults::dedup_name(&basename_of(&canonical_str), &settings.vaults);
            settings.vaults.push(Vault {
                id: id.clone(),
                name,
                path: canonical_str,
                kind: VaultKind::Plain,
                config: Default::default(),
            });
            id
        }
    };
    settings.active_vault_id = Some(id);
    save_settings(&app, &settings)?;

    // Swap the index/watcher over to the new vault. A failure here is
    // non-fatal — the vault is still usable without search.
    if let Err(e) = index::init_for_vault(&app, &state, &canonical) {
        eprintln!("Index init failed for {}: {e}", canonical.display());
    }
    Ok(())
}

/// Runs `f` against the active vault's opened handle, erroring if no vault is
/// currently open (e.g. an encrypted vault that hasn't been unlocked). This is
/// the single dispatch seam: every storage command below is a one-liner over
/// the active [`crate::providers::VaultHandle`], plain or encrypted alike.
pub(crate) fn with_active<T>(
    state: &AppState,
    f: impl FnOnce(&dyn crate::providers::VaultHandle) -> Result<T, String>,
) -> Result<T, String> {
    let guard = state.active.lock().unwrap();
    let handle = guard
        .as_deref()
        .ok_or("No vault is open — it may need to be unlocked")?;
    f(handle)
}

/// Scans the vault and returns the full folder/.md-file tree.
#[tauri::command]
pub async fn scan_vault(state: tauri::State<'_, AppState>) -> Result<TreeNode, String> {
    with_active(state.inner(), |h| h.scan_tree())
}

/// Reads a note's contents.
#[tauri::command]
pub async fn read_note(
    state: tauri::State<'_, AppState>,
    rel_path: String,
) -> Result<String, String> {
    with_active(state.inner(), |h| h.read_note(&rel_path))
}

/// Writes a note's contents, creating parent directories if needed.
#[tauri::command]
pub async fn write_note(
    state: tauri::State<'_, AppState>,
    rel_path: String,
    content: String,
) -> Result<(), String> {
    let st = state.inner();
    with_active(st, |h| h.write_note(st, &rel_path, &content))
}

/// Creates an empty note. If `rel_path` is an existing directory (or empty,
/// meaning the vault root), an unused "Untitled" name is chosen inside it.
/// Otherwise `rel_path` is used as the file path (".md" appended if missing)
/// and it is an error if the file already exists. Returns the created
/// relative path.
#[tauri::command]
pub async fn create_note(
    state: tauri::State<'_, AppState>,
    rel_path: String,
) -> Result<String, String> {
    let st = state.inner();
    with_active(st, |h| h.create_note(st, &rel_path))
}

/// Creates a folder (and any missing parents). Errors if it already exists.
#[tauri::command]
pub async fn create_folder(
    state: tauri::State<'_, AppState>,
    rel_path: String,
) -> Result<(), String> {
    with_active(state.inner(), |h| h.create_folder(&rel_path))
}

/// Renames/moves a file or folder within the vault. Errors if the target
/// already exists.
#[tauri::command]
pub async fn rename_path(
    state: tauri::State<'_, AppState>,
    old_rel: String,
    new_rel: String,
) -> Result<(), String> {
    let st = state.inner();
    with_active(st, |h| h.rename(st, &old_rel, &new_rel))
}

/// Moves a file or folder to the OS Trash. Never hard-deletes.
#[tauri::command]
pub async fn trash_path(
    state: tauri::State<'_, AppState>,
    rel_path: String,
) -> Result<(), String> {
    let st = state.inner();
    with_active(st, |h| h.trash(st, &rel_path))
}

/// Core for creating a note at `rel` (untitled semantics), used by the plain
/// provider handle. See the `create_note` command for the semantics.
pub(crate) fn create_note_core(
    root: &Path,
    state: &AppState,
    rel: &str,
) -> Result<String, String> {
    let target = safe_join(root, rel)?;
    let file_path = if rel.is_empty() || target.is_dir() {
        unique_untitled(&target)?
    } else {
        let mut path = target;
        if !is_markdown(&path) {
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .ok_or_else(|| format!("Invalid note path: {rel}"))?;
            path.set_file_name(format!("{name}.md"));
        }
        if path.exists() {
            return Err(format!(
                "A file named '{}' already exists",
                path.file_name().unwrap_or_default().to_string_lossy()
            ));
        }
        path
    };
    let created_rel = to_rel_string(root, &file_path)?;
    write_note_at(root, state, &created_rel, "")?;
    Ok(created_rel)
}

/// Core for reading a note file's text (plain provider handle).
pub(crate) fn read_note_core(root: &Path, rel: &str) -> Result<String, String> {
    let path = safe_join(root, rel)?;
    if !path.is_file() {
        return Err(format!("Note does not exist: {rel}"));
    }
    fs::read_to_string(&path).map_err(|e| format!("Could not read note '{rel}': {e}"))
}

/// Core for reading raw attachment bytes (plain provider handle).
pub(crate) fn read_attachment_core(root: &Path, rel: &str) -> Result<Vec<u8>, String> {
    let path = safe_join(root, rel)?;
    if !path.is_file() {
        return Err(format!("Attachment does not exist: {rel}"));
    }
    fs::read(&path).map_err(|e| format!("Could not read attachment '{rel}': {e}"))
}

/// Core for revealing a file/folder in the OS file manager (plain handle only).
pub(crate) fn reveal_core(root: &Path, rel: &str) -> Result<(), String> {
    let path = safe_join(root, rel)?;
    if !path.exists() {
        return Err(format!("'{rel}' does not exist"));
    }
    tauri_plugin_opener::reveal_item_in_dir(&path)
        .map_err(|e| format!("Could not reveal '{rel}': {e}"))
}

/// Resolves a `[[wikilink]]` target `name` to an existing note's relative path
/// via the search index, or `None` if nothing matches. See `Index::resolve`.
#[tauri::command]
pub async fn resolve_note(
    state: tauri::State<'_, AppState>,
    name: String,
) -> Result<Option<String>, String> {
    resolve_via_active(state.inner(), &name)
}

/// Resolves a wikilink target through whichever index the active vault uses: a
/// self-indexing handle (encrypted-db) resolves via its container; a plain
/// vault via the separate FTS `state.index`.
pub(crate) fn resolve_via_active(state: &AppState, name: &str) -> Result<Option<String>, String> {
    {
        let active = state.active.lock().unwrap();
        if let Some(h) = active.as_deref() {
            if h.owns_index() {
                return h.resolve(name);
            }
        }
    }
    let guard = state.index.lock().unwrap();
    let index = guard.as_ref().ok_or("No vault is indexed")?;
    index.resolve(name)
}

/// Resolves a `[[wikilink]]` target to an existing note, or creates an empty
/// `<name>.md` when nothing matches. The name is sanitized into a safe single
/// filename. Returns the resolved/created relative path. Dispatches through the
/// active handle so it works for both plain and encrypted vaults.
#[tauri::command]
pub async fn resolve_or_create_note(
    state: tauri::State<'_, AppState>,
    name: String,
) -> Result<String, String> {
    if let Some(path) = resolve_via_active(state.inner(), &name)? {
        return Ok(path);
    }
    let base = sanitize_note_name(&name);
    let rel = format!("{base}.md");
    let st = state.inner();
    with_active(st, |h| {
        // If a same-named note already exists but wasn't indexed yet, open it;
        // otherwise create it empty.
        if h.read_note(&rel).is_err() {
            h.write_note(st, &rel, "")?;
        }
        Ok(rel.clone())
    })
}

/// Turns an arbitrary wikilink target into a safe single filename: any path
/// separators or characters illegal in a filename become spaces, a trailing
/// `.md` is dropped, and the result is trimmed. Empties fall back to
/// "Untitled".
fn sanitize_note_name(name: &str) -> String {
    let base = crate::index::strip_md_suffix(name.trim());
    let cleaned: String = base
        .chars()
        .map(|c| {
            if c == '/' || c == '\\' || c.is_control() || "<>:\"|?*".contains(c) {
                ' '
            } else {
                c
            }
        })
        .collect();
    let trimmed = cleaned.trim().trim_end_matches('.').trim();
    if trimmed.is_empty() {
        "Untitled".to_string()
    } else {
        trimmed.to_string()
    }
}

/// Image extensions accepted by [`save_attachment`]. Anything else is rejected
/// so the vault's `attachments/` folder never fills with arbitrary binaries.
const ALLOWED_IMAGE_EXTS: &[&str] = &["png", "jpg", "jpeg", "gif", "webp", "svg", "avif"];

/// Sanitizes a clipboard/drop-supplied image file name into a safe
/// `(base, extension)` pair. Directory components and leading dots are stripped,
/// characters illegal in a filename become `-`, and the extension is validated
/// against [`ALLOWED_IMAGE_EXTS`]. The base falls back to `pasted-image` when
/// nothing usable remains. Returns an error for missing or disallowed types.
pub(crate) fn sanitize_attachment_name(raw: &str) -> Result<(String, String), String> {
    // Keep only the final path segment so `../../evil.png` can't escape.
    let last = raw.rsplit(|c| c == '/' || c == '\\').next().unwrap_or(raw);
    // Strip leading dots so `.png` / `.hidden` can't produce a dotfile.
    let trimmed = last.trim().trim_start_matches('.');

    // Split off the extension (the part after the final `.`).
    let (base_raw, ext_raw) = match trimmed.rsplit_once('.') {
        Some((b, e)) if !e.is_empty() => (b, e),
        _ => {
            return Err(format!(
                "Cannot save '{raw}': an image file name like image.png is required"
            ))
        }
    };

    let ext = ext_raw.to_ascii_lowercase();
    if !ALLOWED_IMAGE_EXTS.contains(&ext.as_str()) {
        return Err(format!(
            "Unsupported image type '.{ext}'. Allowed: {}",
            ALLOWED_IMAGE_EXTS.join(", ")
        ));
    }

    // Replace filename-illegal characters in the base with `-`, then trim.
    let cleaned: String = base_raw
        .chars()
        .map(|c| {
            if c.is_control() || "/\\<>:\"|?*".contains(c) {
                '-'
            } else {
                c
            }
        })
        .collect();
    let base = cleaned.trim().trim_matches('-').trim();
    let base = if base.is_empty() {
        "pasted-image".to_string()
    } else {
        base.to_string()
    };
    Ok((base, ext))
}

/// Finds the first free `base.ext` in `dir`, then `base-1.ext`, `base-2.ext`, …
fn unique_attachment_path(dir: &Path, base: &str, ext: &str) -> Result<PathBuf, String> {
    let first = dir.join(format!("{base}.{ext}"));
    if !first.exists() {
        return Ok(first);
    }
    for n in 1..100_000 {
        let candidate = dir.join(format!("{base}-{n}.{ext}"));
        if !candidate.exists() {
            return Ok(candidate);
        }
    }
    Err("Could not find a free attachment name".to_string())
}

/// Saves image bytes as a real file under `attachments/` in the vault root and
/// returns its vault-relative path (e.g. `attachments/pasted-image.png`), which
/// the editor drops into the note as a standard `![](…)` link. The name is
/// sanitized and uniquified; only common image types are accepted.
#[tauri::command]
pub async fn save_attachment(
    state: tauri::State<'_, AppState>,
    file_name: String,
    data: Vec<u8>,
) -> Result<String, String> {
    let st = state.inner();
    with_active(st, |h| h.save_attachment(st, &file_name, &data))
}

/// Core for saving image bytes under `attachments/` (plain provider handle).
pub(crate) fn save_attachment_core(
    root: &Path,
    state: &AppState,
    file_name: &str,
    data: &[u8],
) -> Result<String, String> {
    let (base, ext) = sanitize_attachment_name(file_name)?;

    let dir = root.join("attachments");
    fs::create_dir_all(&dir)
        .map_err(|e| format!("Could not create attachments folder: {e}"))?;

    let path = unique_attachment_path(&dir, &base, &ext)?;
    let rel = to_rel_string(root, &path)?;
    // Register the write so the watcher ignores it. Harmless: the watcher skips
    // non-.md files anyway, but this keeps the suppression path uniform.
    register_write(&state.recent_writes, &rel);
    atomic_write(&path, data)?;
    Ok(rel)
}

/// Returns an attachment's bytes as a `data:` URI. Used by the editor image
/// resolver for vaults where `convertFileSrc` can't reach the bytes (encrypted
/// containers). Capped so a pathological blob can't wedge the webview.
#[tauri::command]
pub async fn read_attachment_data_url(
    state: tauri::State<'_, AppState>,
    rel_path: String,
) -> Result<String, String> {
    const MAX_BYTES: usize = 20 * 1024 * 1024; // ~20MB display cap
    let bytes = with_active(state.inner(), |h| h.read_attachment(&rel_path))?;
    if bytes.len() > MAX_BYTES {
        return Err(format!(
            "Attachment '{rel_path}' is too large to display ({} MB)",
            bytes.len() / (1024 * 1024)
        ));
    }
    let mime = mime_for(&rel_path);
    Ok(format!("data:{mime};base64,{}", base64_encode(&bytes)))
}

/// Guesses an image MIME type from a path's extension (attachments are always
/// one of [`ALLOWED_IMAGE_EXTS`]).
fn mime_for(rel: &str) -> &'static str {
    let ext = rel.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "avif" => "image/avif",
        _ => "application/octet-stream",
    }
}

/// Minimal standard-alphabet base64 encoder (no external dep in the main crate).
fn base64_encode(bytes: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            T[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            T[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

/// Reveals a file or folder in the OS file manager (Finder on macOS). Errors on
/// vaults whose handle doesn't support it (encrypted-db) — the frontend hides
/// the action via `capabilities.reveal_in_finder`, but the guard is enforced
/// here too.
#[tauri::command]
pub async fn reveal_in_finder(
    state: tauri::State<'_, AppState>,
    rel_path: String,
) -> Result<(), String> {
    with_active(state.inner(), |h| h.reveal_in_finder(&rel_path))
}

// ---------------------------------------------------------------------------
// Index helper
// ---------------------------------------------------------------------------

/// Atomically writes `bytes` to `path`.
///
/// Writes to a uniquely-named temp file in the **same directory** (so the
/// rename stays on one filesystem and is therefore atomic), then renames it
/// over `path`. A partially-written file can never be observed by a reader or
/// by Syncthing — they see either the old contents or the complete new ones.
/// The temp file is dot-prefixed so the vault scanner and watcher ignore it,
/// and it is removed if the write or rename fails so no `.tmp` litter remains.
pub(crate) fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);

    let parent = path
        .parent()
        .ok_or_else(|| format!("Cannot write to a path with no parent: {}", path.display()))?;
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .ok_or_else(|| format!("Cannot write to a path with no file name: {}", path.display()))?;
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let tmp = parent.join(format!(".{file_name}.tmp-{}-{n}", std::process::id()));

    if let Err(e) = fs::write(&tmp, bytes) {
        let _ = fs::remove_file(&tmp);
        return Err(format!("Could not write '{}': {e}", path.display()));
    }
    if let Err(e) = fs::rename(&tmp, path) {
        let _ = fs::remove_file(&tmp);
        return Err(format!("Could not finalize write to '{}': {e}", path.display()));
    }
    Ok(())
}

/// Upserts a single note into the search index, if one is open. Best-effort:
/// an indexing failure never fails the underlying file operation.
pub(crate) fn index_upsert(state: &AppState, rel: &str, content: &str) {
    if let Ok(guard) = state.index.lock() {
        if let Some(idx) = guard.as_ref() {
            let _ = idx.index_file(rel, content);
        }
    }
}

// ---------------------------------------------------------------------------
// Reusable write cores
//
// These operate on an already-resolved vault `root` and a plain `&AppState`,
// so both the `#[tauri::command]` wrappers below and the AI tool dispatcher
// (`crate::ai::tools`) share one implementation of every mutating file op —
// each registers a self-write and keeps the search index fresh, exactly like
// the commands always did.
// ---------------------------------------------------------------------------

/// Writes `content` to `rel` (creating parent folders), registering the write
/// and reindexing. Full-replace semantics.
pub(crate) fn write_note_at(
    root: &Path,
    state: &AppState,
    rel: &str,
    content: &str,
) -> Result<(), String> {
    let path = safe_join(root, rel)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("Could not create parent folders: {e}"))?;
    }
    register_write(&state.recent_writes, rel);
    atomic_write(&path, content.as_bytes())?;
    index_upsert(state, rel, content);
    Ok(())
}

/// Creates a folder (and any missing parents) at `rel`. Errors if it exists.
pub(crate) fn create_folder_at(root: &Path, rel: &str) -> Result<(), String> {
    if rel.trim().is_empty() {
        return Err("Folder name cannot be empty".to_string());
    }
    let path = safe_join(root, rel)?;
    if path.exists() {
        return Err(format!("'{rel}' already exists"));
    }
    fs::create_dir_all(&path).map_err(|e| format!("Could not create folder '{rel}': {e}"))
}

/// Renames/moves `old_rel` to `new_rel` within the vault, registering both
/// writes and updating the index. Errors if the source is missing or the
/// target already exists.
pub(crate) fn rename_at(
    root: &Path,
    state: &AppState,
    old_rel: &str,
    new_rel: &str,
) -> Result<(), String> {
    let old_path = safe_join(root, old_rel)?;
    let new_path = safe_join(root, new_rel)?;
    if !old_path.exists() {
        return Err(format!("'{old_rel}' does not exist"));
    }
    if new_path.exists() {
        return Err(format!("'{new_rel}' already exists"));
    }
    if let Some(parent) = new_path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("Could not create parent folders: {e}"))?;
    }
    register_write(&state.recent_writes, old_rel);
    register_write(&state.recent_writes, new_rel);
    fs::rename(&old_path, &new_path)
        .map_err(|e| format!("Could not rename '{old_rel}' to '{new_rel}': {e}"))?;
    if let Ok(guard) = state.index.lock() {
        if let Some(idx) = guard.as_ref() {
            let _ = idx.rename(old_rel, new_rel);
        }
    }
    Ok(())
}

/// Moves `rel` to the OS Trash (or, under `cfg(test)`, to a `.trash/` folder in
/// the vault so tests can assert removal without touching the real Trash),
/// registering the write and pruning the index. Never hard-deletes.
pub(crate) fn trash_at(root: &Path, state: &AppState, rel: &str) -> Result<(), String> {
    let path = safe_join(root, rel)?;
    if !path.exists() {
        return Err(format!("'{rel}' does not exist"));
    }
    let was_dir = path.is_dir();
    register_write(&state.recent_writes, rel);
    move_to_trash(root, &path).map_err(|e| format!("Could not move '{rel}' to Trash: {e}"))?;
    if let Ok(guard) = state.index.lock() {
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

/// The terminal "send to Trash" step, wrapped so tests are deterministic. In a
/// normal build it delegates to the `trash` crate (real OS Trash). Under test
/// it relocates the file into a hidden `.trash/` directory inside the vault —
/// which the scanner ignores — so a test can verify the file left the vault
/// tree without polluting the developer's actual Trash.
#[cfg(not(test))]
fn move_to_trash(_root: &Path, abs: &Path) -> Result<(), String> {
    trash::delete(abs).map_err(|e| e.to_string())
}

#[cfg(test)]
fn move_to_trash(root: &Path, abs: &Path) -> Result<(), String> {
    let trash_dir = root.join(".trash");
    fs::create_dir_all(&trash_dir).map_err(|e| e.to_string())?;
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
    fs::rename(abs, &dest).map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    /// Creates a unique empty temp dir; caller removes it when done.
    fn make_temp_dir(tag: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "jaynotes-test-{tag}-{}-{nanos}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn safe_join_accepts_normal_relative_paths() {
        let root = Path::new("/vault");
        assert_eq!(
            safe_join(root, "notes/foo.md").unwrap(),
            PathBuf::from("/vault/notes/foo.md")
        );
        // Empty rel path means the vault root itself.
        assert_eq!(safe_join(root, "").unwrap(), PathBuf::from("/vault"));
        // `.` components are harmless.
        assert_eq!(
            safe_join(root, "./a/./b.md").unwrap(),
            PathBuf::from("/vault/a/b.md")
        );
    }

    #[test]
    fn safe_join_rejects_escaping_paths() {
        let root = Path::new("/vault");
        assert!(safe_join(root, "..").is_err());
        assert!(safe_join(root, "../outside.md").is_err());
        assert!(safe_join(root, "a/../../outside.md").is_err());
        // Even a net-safe `..` is rejected.
        assert!(safe_join(root, "a/../b.md").is_err());
        assert!(safe_join(root, "/etc/passwd").is_err());
    }

    #[test]
    fn scan_tree_builds_sorted_filtered_tree() {
        let root = make_temp_dir("scan");

        fs::create_dir_all(root.join("b folder/nested")).unwrap();
        fs::create_dir_all(root.join("Alpha")).unwrap();
        fs::create_dir_all(root.join(".hidden-dir")).unwrap();
        fs::write(root.join("Zeta.md"), "z").unwrap();
        fs::write(root.join("apple.md"), "a").unwrap();
        fs::write(root.join("notes.txt"), "not md").unwrap();
        fs::write(root.join(".hidden.md"), "hidden").unwrap();
        fs::write(root.join(".hidden-dir/inside.md"), "hidden").unwrap();
        fs::write(root.join("b folder/inner.md"), "i").unwrap();
        fs::write(root.join("b folder/nested/deep.md"), "d").unwrap();

        let tree = scan_tree(&root).unwrap();

        // Root children: folders first (Alpha, b folder), then files sorted
        // case-insensitively (apple.md before Zeta.md). Hidden entries and
        // non-.md files are absent.
        let names: Vec<(&str, bool)> = tree
            .children
            .iter()
            .map(|c| (c.name.as_str(), c.is_dir))
            .collect();
        assert_eq!(
            names,
            vec![
                ("Alpha", true),
                ("b folder", true),
                ("apple.md", false),
                ("Zeta.md", false),
            ]
        );

        // Nested structure and relative paths.
        let b = &tree.children[1];
        assert_eq!(b.path, "b folder");
        let b_names: Vec<&str> = b.children.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(b_names, vec!["nested", "inner.md"]);
        let nested = &b.children[0];
        assert_eq!(nested.children.len(), 1);
        assert_eq!(nested.children[0].path, "b folder/nested/deep.md");
        assert!(!nested.children[0].is_dir);

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn sanitize_note_name_strips_separators_and_extension() {
        assert_eq!(sanitize_note_name("Note One"), "Note One");
        // Trailing .md dropped.
        assert_eq!(sanitize_note_name("Note One.md"), "Note One");
        // Path separators and illegal chars become spaces, then trimmed/collapsed at edges.
        assert_eq!(sanitize_note_name("folder/Note"), "folder Note");
        assert_eq!(sanitize_note_name("a:b*c?"), "a b c");
        // Empty / whitespace falls back to Untitled.
        assert_eq!(sanitize_note_name("   "), "Untitled");
        assert_eq!(sanitize_note_name("/"), "Untitled");
    }

    #[test]
    fn unique_untitled_skips_existing_names() {
        let dir = make_temp_dir("untitled");
        assert_eq!(unique_untitled(&dir).unwrap(), dir.join("Untitled.md"));
        fs::write(dir.join("Untitled.md"), "").unwrap();
        assert_eq!(unique_untitled(&dir).unwrap(), dir.join("Untitled 1.md"));
        fs::write(dir.join("Untitled 1.md"), "").unwrap();
        assert_eq!(unique_untitled(&dir).unwrap(), dir.join("Untitled 2.md"));
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn sanitize_attachment_name_cleans_and_keeps_extension() {
        // Plain name passes through, extension lowercased.
        assert_eq!(
            sanitize_attachment_name("Photo.PNG").unwrap(),
            ("Photo".to_string(), "png".to_string())
        );
        // Directory components are stripped (defense against path escapes).
        assert_eq!(
            sanitize_attachment_name("../../secrets/pic.jpg").unwrap(),
            ("pic".to_string(), "jpg".to_string())
        );
        assert_eq!(
            sanitize_attachment_name("a\\b\\c.gif").unwrap(),
            ("c".to_string(), "gif".to_string())
        );
        // Illegal characters in the base become '-', edges trimmed.
        assert_eq!(
            sanitize_attachment_name("my:cool*shot?.webp").unwrap(),
            ("my-cool-shot".to_string(), "webp".to_string())
        );
        // Interior dots in the base are preserved.
        assert_eq!(
            sanitize_attachment_name("v1.2.final.jpeg").unwrap(),
            ("v1.2.final".to_string(), "jpeg".to_string())
        );
        // Leading dots are stripped so a dotfile can't be produced.
        assert_eq!(
            sanitize_attachment_name(".hidden.png").unwrap(),
            ("hidden".to_string(), "png".to_string())
        );
        // A base that is nothing but illegal characters falls back to default.
        assert_eq!(
            sanitize_attachment_name("??.png").unwrap(),
            ("pasted-image".to_string(), "png".to_string())
        );
    }

    #[test]
    fn sanitize_attachment_name_rejects_bad_types() {
        // Disallowed extensions.
        assert!(sanitize_attachment_name("evil.exe").is_err());
        assert!(sanitize_attachment_name("doc.pdf").is_err());
        assert!(sanitize_attachment_name("archive.tar.gz").is_err());
        // No extension at all.
        assert!(sanitize_attachment_name("noext").is_err());
        assert!(sanitize_attachment_name("trailingdot.").is_err());
    }

    #[test]
    fn migrate_converts_legacy_vault_path_and_preserves_ai_key() {
        // A legacy settings.json shape: single `vaultPath`, plus an `ai` block
        // and an unknown future key that must both survive untouched.
        let raw = r#"{
            "vaultPath": "/Users/jay/MyVault",
            "ai": { "preset": "custom", "apiKey": "secret" },
            "futureThing": { "keep": true }
        }"#;
        let mut settings: Settings = serde_json::from_str(raw).unwrap();
        migrate(&mut settings);

        // Legacy key is gone; exactly one active vault took its place.
        assert!(settings.vault_path.is_none());
        assert_eq!(settings.vaults.len(), 1);
        let v = &settings.vaults[0];
        assert_eq!(v.name, "MyVault");
        assert_eq!(v.path, "/Users/jay/MyVault");
        assert_eq!(v.kind, VaultKind::Plain);
        assert_eq!(settings.active_vault_id.as_deref(), Some(v.id.as_str()));

        // Unknown keys (ai, futureThing) are preserved verbatim.
        assert_eq!(
            settings.extra.get("ai").and_then(|v| v.get("preset")),
            Some(&serde_json::json!("custom"))
        );
        assert_eq!(
            settings.extra.get("futureThing"),
            Some(&serde_json::json!({ "keep": true }))
        );

        // Round-trips without resurrecting the legacy key and stays stable.
        let json = serde_json::to_string(&settings).unwrap();
        assert!(!json.contains("vaultPath"));
        let mut again: Settings = serde_json::from_str(&json).unwrap();
        let before = again.vaults.clone();
        migrate(&mut again);
        assert_eq!(again.vaults, before, "migration is idempotent");
    }

    #[test]
    fn migrate_noop_when_vaults_already_present() {
        // A settings file already on the new schema is left alone.
        let raw = r#"{
            "vaults": [{ "id": "abc", "name": "V", "path": "/v", "kind": "plain" }],
            "activeVaultId": "abc"
        }"#;
        let mut settings: Settings = serde_json::from_str(raw).unwrap();
        migrate(&mut settings);
        assert_eq!(settings.vaults.len(), 1);
        assert_eq!(settings.vaults[0].id, "abc");
        assert_eq!(settings.active_vault_id.as_deref(), Some("abc"));
    }

    #[test]
    fn atomic_write_writes_content_and_leaves_no_tmp() {
        let dir = make_temp_dir("atomic");
        let path = dir.join("note.md");

        atomic_write(&path, b"hello world").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "hello world");

        // Overwrite in place.
        atomic_write(&path, b"second version").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "second version");

        // No `.tmp` litter remains after successful writes.
        let leftovers: Vec<String> = fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "unexpected temp files: {leftovers:?}");

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn unique_attachment_path_uniquifies_on_collision() {
        let dir = make_temp_dir("attach");
        assert_eq!(
            unique_attachment_path(&dir, "img", "png").unwrap(),
            dir.join("img.png")
        );
        fs::write(dir.join("img.png"), b"x").unwrap();
        assert_eq!(
            unique_attachment_path(&dir, "img", "png").unwrap(),
            dir.join("img-1.png")
        );
        fs::write(dir.join("img-1.png"), b"x").unwrap();
        assert_eq!(
            unique_attachment_path(&dir, "img", "png").unwrap(),
            dir.join("img-2.png")
        );
        fs::remove_dir_all(&dir).ok();
    }
}
