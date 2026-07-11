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

/// Persisted app settings. Unknown keys are preserved via `extra` so future
/// milestones can add fields without clobbering older ones.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vault_path: Option<String>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
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

fn load_settings(app: &tauri::AppHandle) -> Result<Settings, String> {
    let path = settings_path(app)?;
    if !path.exists() {
        return Ok(Settings::default());
    }
    let raw = fs::read_to_string(&path)
        .map_err(|e| format!("Could not read settings file: {e}"))?;
    serde_json::from_str(&raw).map_err(|e| format!("Settings file is not valid JSON: {e}"))
}

fn save_settings(app: &tauri::AppHandle, settings: &Settings) -> Result<(), String> {
    let path = settings_path(app)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("Could not create config dir: {e}"))?;
    }
    let raw = serde_json::to_string_pretty(settings)
        .map_err(|e| format!("Could not serialize settings: {e}"))?;
    fs::write(&path, raw).map_err(|e| format!("Could not write settings file: {e}"))
}

/// Returns the canonicalized vault root, erroring if no vault is configured
/// or the directory no longer exists.
fn vault_root(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    let settings = load_settings(app)?;
    let raw = settings
        .vault_path
        .ok_or_else(|| "No vault is configured".to_string())?;
    let root = PathBuf::from(&raw);
    if !root.is_dir() {
        return Err(format!("Vault directory no longer exists: {raw}"));
    }
    root.canonicalize()
        .map_err(|e| format!("Could not resolve vault directory: {e}"))
}

/// Returns the canonicalized saved vault root if one is configured and still
/// exists on disk, else `None`. Used by startup index initialization, where a
/// missing/invalid vault should simply mean "no index" rather than an error.
pub fn saved_vault_root(app: &tauri::AppHandle) -> Option<String> {
    let settings = load_settings(app).ok()?;
    let root = PathBuf::from(settings.vault_path?);
    if !root.is_dir() {
        return None;
    }
    root.canonicalize()
        .ok()
        .map(|p| p.to_string_lossy().into_owned())
}

// ---------------------------------------------------------------------------
// Path safety
// ---------------------------------------------------------------------------

/// Joins `rel` onto `root`, rejecting absolute paths and any path component
/// that is not a plain name (`..`, drive prefixes, root dirs). `.` components
/// and an empty string (meaning the root itself) are allowed.
fn safe_join(root: &Path, rel: &str) -> Result<PathBuf, String> {
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
fn to_rel_string(root: &Path, abs: &Path) -> Result<String, String> {
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
fn scan_tree(root: &Path) -> Result<TreeNode, String> {
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

fn insert_node(root: &mut TreeNode, rel: &Path, is_dir: bool) {
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

fn sort_tree(node: &mut TreeNode) {
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

/// Returns the saved vault path, or null if none is set (or the saved
/// directory no longer exists).
#[tauri::command]
pub async fn get_vault(app: tauri::AppHandle) -> Result<Option<String>, String> {
    let settings = load_settings(&app)?;
    Ok(settings
        .vault_path
        .filter(|p| Path::new(p).is_dir()))
}

/// Validates that `path` is an existing directory, persists it as the vault
/// root in settings.json, and (re)opens the search index + file watcher for it.
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
    let mut settings = load_settings(&app)?;
    settings.vault_path = Some(canonical.to_string_lossy().into_owned());
    save_settings(&app, &settings)?;

    // Swap the index/watcher over to the new vault. A failure here is
    // non-fatal — the vault is still usable without search.
    if let Err(e) = index::init_for_vault(&app, &state, &canonical) {
        eprintln!("Index init failed for {}: {e}", canonical.display());
    }
    Ok(())
}

/// Scans the vault and returns the full folder/.md-file tree.
#[tauri::command]
pub async fn scan_vault(app: tauri::AppHandle) -> Result<TreeNode, String> {
    let root = vault_root(&app)?;
    scan_tree(&root)
}

/// Reads a note's contents.
#[tauri::command]
pub async fn read_note(app: tauri::AppHandle, rel_path: String) -> Result<String, String> {
    let root = vault_root(&app)?;
    let path = safe_join(&root, &rel_path)?;
    if !path.is_file() {
        return Err(format!("Note does not exist: {rel_path}"));
    }
    fs::read_to_string(&path).map_err(|e| format!("Could not read note '{rel_path}': {e}"))
}

/// Writes a note's contents, creating parent directories if needed.
#[tauri::command]
pub async fn write_note(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    rel_path: String,
    content: String,
) -> Result<(), String> {
    let root = vault_root(&app)?;
    let path = safe_join(&root, &rel_path)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("Could not create parent folders: {e}"))?;
    }
    // Register the write BEFORE it lands so the watcher never races us.
    register_write(&state.recent_writes, &rel_path);
    fs::write(&path, &content).map_err(|e| format!("Could not write note '{rel_path}': {e}"))?;
    index_upsert(&state, &rel_path, &content);
    Ok(())
}

/// Creates an empty note. If `rel_path` is an existing directory (or empty,
/// meaning the vault root), an unused "Untitled" name is chosen inside it.
/// Otherwise `rel_path` is used as the file path (".md" appended if missing)
/// and it is an error if the file already exists. Returns the created
/// relative path.
#[tauri::command]
pub async fn create_note(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    rel_path: String,
) -> Result<String, String> {
    let root = vault_root(&app)?;
    let target = safe_join(&root, &rel_path)?;

    let file_path = if rel_path.is_empty() || target.is_dir() {
        unique_untitled(&target)?
    } else {
        let mut path = target;
        if !is_markdown(&path) {
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .ok_or_else(|| format!("Invalid note path: {rel_path}"))?;
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

    if let Some(parent) = file_path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("Could not create parent folders: {e}"))?;
    }
    let created_rel = to_rel_string(&root, &file_path)?;
    register_write(&state.recent_writes, &created_rel);
    fs::write(&file_path, "").map_err(|e| format!("Could not create note: {e}"))?;
    index_upsert(&state, &created_rel, "");
    Ok(created_rel)
}

/// Creates a folder (and any missing parents). Errors if it already exists.
#[tauri::command]
pub async fn create_folder(app: tauri::AppHandle, rel_path: String) -> Result<(), String> {
    let root = vault_root(&app)?;
    if rel_path.is_empty() {
        return Err("Folder name cannot be empty".to_string());
    }
    let path = safe_join(&root, &rel_path)?;
    if path.exists() {
        return Err(format!("'{rel_path}' already exists"));
    }
    fs::create_dir_all(&path).map_err(|e| format!("Could not create folder '{rel_path}': {e}"))
}

/// Renames/moves a file or folder within the vault. Errors if the target
/// already exists.
#[tauri::command]
pub async fn rename_path(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    old_rel: String,
    new_rel: String,
) -> Result<(), String> {
    let root = vault_root(&app)?;
    let old_path = safe_join(&root, &old_rel)?;
    let new_path = safe_join(&root, &new_rel)?;
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
    register_write(&state.recent_writes, &old_rel);
    register_write(&state.recent_writes, &new_rel);
    fs::rename(&old_path, &new_path)
        .map_err(|e| format!("Could not rename '{old_rel}' to '{new_rel}': {e}"))?;
    if let Ok(guard) = state.index.lock() {
        if let Some(idx) = guard.as_ref() {
            let _ = idx.rename(&old_rel, &new_rel);
        }
    }
    Ok(())
}

/// Moves a file or folder to the OS Trash. Never hard-deletes.
#[tauri::command]
pub async fn trash_path(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    rel_path: String,
) -> Result<(), String> {
    let root = vault_root(&app)?;
    let path = safe_join(&root, &rel_path)?;
    if !path.exists() {
        return Err(format!("'{rel_path}' does not exist"));
    }
    let was_dir = path.is_dir();
    register_write(&state.recent_writes, &rel_path);
    trash::delete(&path).map_err(|e| format!("Could not move '{rel_path}' to Trash: {e}"))?;
    if let Ok(guard) = state.index.lock() {
        if let Some(idx) = guard.as_ref() {
            let _ = if was_dir {
                idx.remove_prefix(&rel_path)
            } else {
                idx.remove_file(&rel_path)
            };
        }
    }
    Ok(())
}

/// Reveals a file or folder in the OS file manager (Finder on macOS).
#[tauri::command]
pub async fn reveal_in_finder(app: tauri::AppHandle, rel_path: String) -> Result<(), String> {
    let root = vault_root(&app)?;
    let path = safe_join(&root, &rel_path)?;
    if !path.exists() {
        return Err(format!("'{rel_path}' does not exist"));
    }
    tauri_plugin_opener::reveal_item_in_dir(&path)
        .map_err(|e| format!("Could not reveal '{rel_path}': {e}"))
}

// ---------------------------------------------------------------------------
// Index helper
// ---------------------------------------------------------------------------

/// Upserts a single note into the search index, if one is open. Best-effort:
/// an indexing failure never fails the underlying file operation.
fn index_upsert(state: &tauri::State<'_, AppState>, rel: &str, content: &str) {
    if let Ok(guard) = state.index.lock() {
        if let Some(idx) = guard.as_ref() {
            let _ = idx.index_file(rel, content);
        }
    }
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
    fn unique_untitled_skips_existing_names() {
        let dir = make_temp_dir("untitled");
        assert_eq!(unique_untitled(&dir).unwrap(), dir.join("Untitled.md"));
        fs::write(dir.join("Untitled.md"), "").unwrap();
        assert_eq!(unique_untitled(&dir).unwrap(), dir.join("Untitled 1.md"));
        fs::write(dir.join("Untitled 1.md"), "").unwrap();
        assert_eq!(unique_untitled(&dir).unwrap(), dir.join("Untitled 2.md"));
        fs::remove_dir_all(&dir).ok();
    }
}
