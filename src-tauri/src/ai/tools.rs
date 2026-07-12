//! Tool schemas and the dispatcher that executes them against the vault.
//!
//! Every tool dispatches through the **same layer the storage commands use**:
//! mutating tools go through the active [`crate::providers::VaultHandle`] (via
//! [`crate::vault::with_active`]) and read/search tools through the shared index
//! dispatch (`crate::index::dispatch_*`). That means the AI works identically on
//! a plain vault, an encrypted-files vault (separate keyed index), and a
//! self-indexing encrypted-db vault — and a locked vault surfaces one clean
//! "vault is locked" error rather than raw filesystem failures. Tool failures
//! are returned to the model as `Error: …` results rather than propagated, so a
//! bad argument never crashes the agent loop.

use serde_json::{json, Value};
use tauri::ipc::Channel;
use tauri::Emitter;

use super::revisions::Revisions;
use super::{AiEvent, AppAiState, Gate};
use crate::index::{self, AppState};
use crate::vault::{with_active, TreeNode};

/// How a tool's undo snapshots are stored for the active vault: plaintext files
/// in app-data (plain vaults) or encrypted inside the vault under `.revisions/`
/// (encrypted vaults, so snapshot content never leaks to app-data).
pub enum RevisionSink {
    Fs(Revisions),
    #[cfg(any(feature = "encryption", feature = "provider-tinylord"))]
    Handle,
}

/// Everything a tool needs to run.
pub struct ToolContext<'a> {
    pub state: &'a AppState,
    pub ai: &'a AppAiState,
    pub channel: &'a Channel<AiEvent>,
    pub revisions: RevisionSink,
    /// App handle for firing global UI side-effect events (e.g. `open_note`).
    /// `None` in unit tests, where emission is a no-op.
    pub app: Option<tauri::AppHandle>,
}

impl ToolContext<'_> {
    /// Snapshots `content` as an undo point of `rel`, choosing the storage
    /// backend for the active vault.
    fn snapshot(&self, rel: &str, content: &str) -> Result<String, String> {
        match &self.revisions {
            RevisionSink::Fs(r) => r.snapshot(rel, content),
            #[cfg(any(feature = "encryption", feature = "provider-tinylord"))]
            RevisionSink::Handle => super::revisions::handle_snapshot(self.state, rel, content),
        }
    }
}

/// Result of running one tool.
pub struct ToolOutcome {
    /// Compact JSON (or `Error: …`) returned to the model as the tool result.
    pub result: String,
    /// Short human summary for the `ToolResult` UI event.
    pub summary: String,
    /// Optional longer detail (e.g. affected paths) for the UI.
    pub detail: Option<String>,
    /// Revision id, when this tool created an undo snapshot.
    pub revision_id: Option<String>,
}

impl ToolOutcome {
    fn ok(result: Value, summary: impl Into<String>) -> Self {
        ToolOutcome {
            result: result.to_string(),
            summary: summary.into(),
            detail: None,
            revision_id: None,
        }
    }
    fn err(msg: impl std::fmt::Display) -> Self {
        ToolOutcome {
            result: format!("Error: {msg}"),
            summary: format!("Error: {msg}"),
            detail: None,
            revision_id: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Schemas
// ---------------------------------------------------------------------------

/// The OpenAI-format function schema array advertised to the model. Tool
/// descriptions are written for the model to disambiguate by — note especially
/// that `rename_note` is for renaming *in place* while `move_note` *relocates*.
pub fn tool_schemas() -> Vec<Value> {
    fn tool(name: &str, description: &str, params: Value) -> Value {
        json!({
            "type": "function",
            "function": { "name": name, "description": description, "parameters": params }
        })
    }
    fn obj(props: Value, required: &[&str]) -> Value {
        json!({ "type": "object", "properties": props, "required": required })
    }

    vec![
        tool(
            "search_notes",
            "Full-text search across the vault. Supports `tag:name` filters mixed with words (e.g. `roadmap tag:project`). Use this before answering questions about the vault's contents.",
            obj(json!({ "query": { "type": "string", "description": "Search text; may include tag: filters." } }), &["query"]),
        ),
        tool(
            "list_notes",
            "List notes in the vault (most recently modified first), returning path and title.",
            obj(json!({ "limit": { "type": "integer", "description": "Max notes to return (default 50)." } }), &[]),
        ),
        tool(
            "list_folders",
            "List all folders in the vault (relative paths).",
            obj(json!({}), &[]),
        ),
        tool(
            "list_tags",
            "List every tag in the vault with how many notes carry it.",
            obj(json!({}), &[]),
        ),
        tool(
            "notes_by_tag",
            "List notes carrying an exact tag.",
            obj(json!({ "tag": { "type": "string" } }), &["tag"]),
        ),
        tool(
            "read_note",
            "Read a note's full markdown content by its vault-relative path.",
            obj(json!({ "path": { "type": "string" } }), &["path"]),
        ),
        tool(
            "note_links",
            "Get a note's outgoing wikilinks (resolved to paths) and its backlinks (notes that link to it).",
            obj(json!({ "path": { "type": "string" } }), &["path"]),
        ),
        tool(
            "open_note",
            "Open a note in the user's editor so they can see it. Use when the user asks to open/show/go to a note.",
            obj(json!({ "path": { "type": "string" } }), &["path"]),
        ),
        tool(
            "create_note",
            "Create a NEW note at the given path with markdown content. Errors if a note already exists there — pick another name. Include YAML frontmatter with tags when appropriate.",
            obj(json!({
                "path": { "type": "string", "description": "Vault-relative path, e.g. `ideas/New Idea.md`." },
                "content": { "type": "string" }
            }), &["path", "content"]),
        ),
        tool(
            "update_note",
            "Replace an existing note's entire content. A revision snapshot is taken first so the user can revert. Only call this after reading the note.",
            obj(json!({ "path": { "type": "string" }, "content": { "type": "string" } }), &["path", "content"]),
        ),
        tool(
            "append_to_note",
            "Append markdown to the end of an existing note (separated by a newline). A revision snapshot is taken first.",
            obj(json!({ "path": { "type": "string" }, "content": { "type": "string" } }), &["path", "content"]),
        ),
        tool(
            "rename_note",
            "Rename a note IN PLACE within its current folder (or change its file name). To move a note to a different folder, use move_note instead.",
            obj(json!({
                "old_path": { "type": "string" },
                "new_path": { "type": "string", "description": "New vault-relative path (same folder for a pure rename)." }
            }), &["old_path", "new_path"]),
        ),
        tool(
            "move_note",
            "RELOCATE a note into a different folder, keeping its file name. The destination folder is created if missing. Requires explicit user approval before it runs. Use this (not rename_note) to organize notes into folders.",
            obj(json!({
                "path": { "type": "string" },
                "destination_folder": { "type": "string", "description": "Target folder relative path; empty string means the vault root." }
            }), &["path", "destination_folder"]),
        ),
        tool(
            "move_folder",
            "RELOCATE an entire folder (and everything inside it) into a different parent folder. The destination parent is created if missing. A folder cannot be moved into itself or one of its own subfolders. Requires explicit user approval before it runs.",
            obj(json!({
                "path": { "type": "string", "description": "The folder to move (vault-relative)." },
                "destination_folder": { "type": "string", "description": "Target parent folder relative path; empty string means the vault root." }
            }), &["path", "destination_folder"]),
        ),
        tool(
            "create_folder",
            "Create a new folder (and any missing parents) for organizing notes.",
            obj(json!({ "path": { "type": "string" } }), &["path"]),
        ),
        tool(
            "delete_note",
            "Move a note to the Trash. Requires explicit user approval before it runs.",
            obj(json!({ "path": { "type": "string" } }), &["path"]),
        ),
        tool(
            "batch_delete",
            "Move several notes to the Trash at once. Requires a single explicit user approval covering all of them.",
            obj(json!({ "paths": { "type": "array", "items": { "type": "string" } } }), &["paths"]),
        ),
        tool(
            "delete_folder",
            "Move an entire folder (and everything inside it) to the Trash as one restorable unit. Requires a single explicit user approval covering all contained notes.",
            obj(json!({ "path": { "type": "string", "description": "The folder to delete (vault-relative)." } }), &["path"]),
        ),
    ]
}

/// A short human summary of a tool call's arguments, for the `ToolCall` event.
pub fn summarize_args(name: &str, args: &Value) -> String {
    let s = |k: &str| args.get(k).and_then(|v| v.as_str()).unwrap_or("");
    match name {
        "search_notes" => format!("\"{}\"", s("query")),
        "notes_by_tag" => format!("#{}", s("tag")),
        "read_note" | "note_links" | "open_note" | "create_note" | "update_note"
        | "append_to_note" | "delete_note" | "delete_folder" | "create_folder" => {
            s("path").to_string()
        }
        "rename_note" => format!("{} → {}", s("old_path"), s("new_path")),
        "move_note" | "move_folder" => {
            let dest = s("destination_folder");
            format!("{} → {}/", s("path"), if dest.is_empty() { "(root)" } else { dest })
        }
        "batch_delete" => {
            let n = args.get("paths").and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(0);
            format!("{n} note(s)")
        }
        _ => String::new(),
    }
}

// ---------------------------------------------------------------------------
// Dispatch
// ---------------------------------------------------------------------------

/// Executes tool `name` with `args`. Never fails: any error is packaged as an
/// `Error: …` tool result the model can recover from.
pub async fn dispatch(ctx: &ToolContext<'_>, name: &str, args: &Value) -> ToolOutcome {
    match try_dispatch(ctx, name, args).await {
        Ok(o) => o,
        Err(e) => ToolOutcome::err(e),
    }
}

async fn try_dispatch(ctx: &ToolContext<'_>, name: &str, args: &Value) -> Result<ToolOutcome, String> {
    match name {
        "search_notes" => search_notes(ctx, arg_str(args, "query")?),
        "list_notes" => list_notes(ctx, args.get("limit").and_then(|v| v.as_u64()).map(|n| n as u32)),
        "list_folders" => list_folders(ctx),
        "list_tags" => list_tags(ctx),
        "notes_by_tag" => notes_by_tag(ctx, arg_str(args, "tag")?),
        "read_note" => read_note(ctx, arg_str(args, "path")?),
        "note_links" => note_links(ctx, arg_str(args, "path")?),
        "open_note" => open_note(ctx, arg_str(args, "path")?),
        "create_note" => create_note(ctx, arg_str(args, "path")?, arg_str(args, "content")?),
        "update_note" => update_note(ctx, arg_str(args, "path")?, arg_str(args, "content")?),
        "append_to_note" => append_to_note(ctx, arg_str(args, "path")?, arg_str(args, "content")?),
        "rename_note" => rename_note(ctx, arg_str(args, "old_path")?, arg_str(args, "new_path")?),
        "move_note" => {
            move_note(ctx, arg_str(args, "path")?, arg_str(args, "destination_folder").unwrap_or("")).await
        }
        "move_folder" => {
            move_folder(ctx, arg_str(args, "path")?, arg_str(args, "destination_folder").unwrap_or("")).await
        }
        "create_folder" => create_folder(ctx, arg_str(args, "path")?),
        "delete_note" => delete_note(ctx, arg_str(args, "path")?).await,
        "batch_delete" => batch_delete(ctx, args).await,
        "delete_folder" => delete_folder(ctx, arg_str(args, "path")?).await,
        other => Err(format!("Unknown tool '{other}'")),
    }
}

fn arg_str<'a>(args: &'a Value, key: &str) -> Result<&'a str, String> {
    args.get(key)
        .and_then(|v| v.as_str())
        .ok_or_else(|| format!("Missing required string argument '{key}'"))
}

// ---------------------------------------------------------------------------
// Storage dispatch through the active handle (plain / encrypted-db / encrypted-
// files alike). Each helper takes one short lock on `state.active`, so revision
// writes that also touch the handle never nest a lock.
// ---------------------------------------------------------------------------

fn h_scan(ctx: &ToolContext<'_>) -> Result<TreeNode, String> {
    with_active(ctx.state, |h| h.scan_tree())
}
fn h_read(ctx: &ToolContext<'_>, rel: &str) -> Result<String, String> {
    with_active(ctx.state, |h| h.read_note(rel))
}
fn h_write(ctx: &ToolContext<'_>, rel: &str, content: &str) -> Result<(), String> {
    with_active(ctx.state, |h| h.write_note(ctx.state, rel, content))
}
fn h_create_folder(ctx: &ToolContext<'_>, rel: &str) -> Result<(), String> {
    with_active(ctx.state, |h| h.create_folder(rel))
}
fn h_rename(ctx: &ToolContext<'_>, old: &str, new: &str) -> Result<(), String> {
    with_active(ctx.state, |h| h.rename(ctx.state, old, new))
}
fn h_trash(ctx: &ToolContext<'_>, rel: &str) -> Result<(), String> {
    with_active(ctx.state, |h| h.trash(ctx.state, rel))
}

/// True if a note (file) exists at `rel` in the active vault.
fn note_exists(ctx: &ToolContext<'_>, rel: &str) -> bool {
    h_read(ctx, rel).is_ok()
}

/// Finds a node by vault-relative path in a scanned tree.
fn find_node<'a>(node: &'a TreeNode, path: &str) -> Option<&'a TreeNode> {
    if path == node.path {
        return Some(node);
    }
    for child in &node.children {
        if path == child.path || path.starts_with(&format!("{}/", child.path)) {
            return find_node(child, path);
        }
    }
    None
}

/// Collects every folder's relative path from a scanned tree.
fn collect_folders(node: &TreeNode, out: &mut Vec<String>) {
    for child in &node.children {
        if child.is_dir {
            out.push(child.path.clone());
            collect_folders(child, out);
        }
    }
}

/// Collects every `.md` note under (and including) `node`.
fn collect_notes(node: &TreeNode, out: &mut Vec<String>) {
    for child in &node.children {
        if child.is_dir {
            collect_notes(child, out);
        } else {
            out.push(child.path.clone());
        }
    }
}

/// Normalizes a note path to end in `.md`.
fn with_md(path: &str) -> String {
    if path.to_ascii_lowercase().ends_with(".md") {
        path.to_string()
    } else {
        format!("{path}.md")
    }
}

// ---- read tools ----

fn search_notes(ctx: &ToolContext<'_>, query: &str) -> Result<ToolOutcome, String> {
    let hits = index::dispatch_search(ctx.state, query, 25)?;
    let arr: Vec<Value> = hits
        .iter()
        .map(|h| json!({ "path": h.path, "title": h.title, "snippet": strip_marks(&h.snippet), "tags": h.tags }))
        .collect();
    let summary = format!("Searched \"{query}\" — {} result(s)", arr.len());
    Ok(ToolOutcome::ok(json!({ "results": arr }), summary))
}

fn list_notes(ctx: &ToolContext<'_>, limit: Option<u32>) -> Result<ToolOutcome, String> {
    let mut notes = index::dispatch_list_notes(ctx.state)?;
    let limit = limit.unwrap_or(50) as usize;
    notes.truncate(limit);
    let arr: Vec<Value> = notes.iter().map(|n| json!({ "path": n.path, "title": n.title })).collect();
    let summary = format!("Listed {} note(s)", arr.len());
    Ok(ToolOutcome::ok(json!({ "notes": arr }), summary))
}

fn list_folders(ctx: &ToolContext<'_>) -> Result<ToolOutcome, String> {
    let tree = h_scan(ctx)?;
    let mut folders = Vec::new();
    collect_folders(&tree, &mut folders);
    folders.sort();
    let summary = format!("Listed {} folder(s)", folders.len());
    Ok(ToolOutcome::ok(json!({ "folders": folders }), summary))
}

fn list_tags(ctx: &ToolContext<'_>) -> Result<ToolOutcome, String> {
    let tags = index::dispatch_list_tags(ctx.state)?;
    let arr: Vec<Value> = tags.iter().map(|t| json!({ "tag": t.tag, "count": t.count })).collect();
    let summary = format!("Listed {} tag(s)", arr.len());
    Ok(ToolOutcome::ok(json!({ "tags": arr }), summary))
}

fn notes_by_tag(ctx: &ToolContext<'_>, tag: &str) -> Result<ToolOutcome, String> {
    let hits = index::dispatch_notes_by_tag(ctx.state, tag, 200)?;
    let arr: Vec<Value> = hits.iter().map(|h| json!({ "path": h.path, "title": h.title })).collect();
    let summary = format!("#{tag} — {} note(s)", arr.len());
    Ok(ToolOutcome::ok(json!({ "notes": arr }), summary))
}

fn read_note(ctx: &ToolContext<'_>, path: &str) -> Result<ToolOutcome, String> {
    let content = h_read(ctx, path).map_err(|_| format!("Note does not exist: {path}"))?;
    Ok(ToolOutcome::ok(json!({ "path": path, "content": content }), format!("Read {path}")))
}

fn note_links(ctx: &ToolContext<'_>, path: &str) -> Result<ToolOutcome, String> {
    let (outgoing, backlinks) = index::dispatch_links_for(ctx.state, path)?;
    let summary = format!("{path}: {} out, {} back", outgoing.len(), backlinks.len());
    Ok(ToolOutcome::ok(json!({ "outgoing": outgoing, "backlinks": backlinks }), summary))
}

/// Ungated: validates that the note exists (through the active handle), then
/// fires a global `ai-open-note` app event so the UI opens it in the editor.
/// Emission is best-effort and a no-op when no app handle is present (tests).
fn open_note(ctx: &ToolContext<'_>, path: &str) -> Result<ToolOutcome, String> {
    if !note_exists(ctx, path) {
        return Err(format!("Note does not exist: {path}"));
    }
    if let Some(app) = &ctx.app {
        let _ = app.emit("ai-open-note", json!({ "path": path }));
    }
    Ok(ToolOutcome::ok(json!({ "path": path, "opened": true }), format!("Opened {path}")))
}

// ---- write tools ----

fn create_note(ctx: &ToolContext<'_>, path: &str, content: &str) -> Result<ToolOutcome, String> {
    let rel = with_md(path);
    if note_exists(ctx, &rel) {
        return Err(format!(
            "A file named '{rel}' already exists — pick another name"
        ));
    }
    // Create the exact note through the handle. `create_note("")`/dir-target has
    // untitled semantics, so write the content directly at the resolved path.
    h_write(ctx, &rel, content)?;
    Ok(ToolOutcome::ok(json!({ "path": rel, "created": true }), format!("Created {rel}")))
}

fn update_note(ctx: &ToolContext<'_>, path: &str, content: &str) -> Result<ToolOutcome, String> {
    let current = h_read(ctx, path)
        .map_err(|_| format!("Note does not exist: {path} (use create_note to make it)"))?;
    let revision_id = ctx.snapshot(path, &current)?;
    h_write(ctx, path, content)?;
    Ok(ToolOutcome {
        result: json!({ "path": path, "updated": true }).to_string(),
        summary: format!("Updated {path}"),
        detail: None,
        revision_id: Some(revision_id),
    })
}

fn append_to_note(ctx: &ToolContext<'_>, path: &str, content: &str) -> Result<ToolOutcome, String> {
    let current = h_read(ctx, path)
        .map_err(|_| format!("Note does not exist: {path} (use create_note to make it)"))?;
    let revision_id = ctx.snapshot(path, &current)?;
    let mut next = current;
    if !next.is_empty() && !next.ends_with('\n') {
        next.push('\n');
    }
    next.push_str(content);
    h_write(ctx, path, &next)?;
    Ok(ToolOutcome {
        result: json!({ "path": path, "appended": true }).to_string(),
        summary: format!("Appended to {path}"),
        detail: None,
        revision_id: Some(revision_id),
    })
}

fn rename_note(ctx: &ToolContext<'_>, old_path: &str, new_path: &str) -> Result<ToolOutcome, String> {
    h_rename(ctx, old_path, new_path)?;
    Ok(ToolOutcome::ok(
        json!({ "old_path": old_path, "new_path": new_path }),
        format!("Renamed {old_path} → {new_path}"),
    ))
}

/// Gated: emits a "move" permission request and awaits approval before moving.
/// Validation (source exists, no collision) happens BEFORE the gate so the user
/// is never asked to approve an operation that would fail anyway.
async fn move_note(
    ctx: &ToolContext<'_>,
    path: &str,
    dest_folder: &str,
) -> Result<ToolOutcome, String> {
    let file_name = path.rsplit('/').next().filter(|s| !s.is_empty()).ok_or_else(|| {
        format!("Invalid note path: {path}")
    })?;
    if !note_exists(ctx, path) {
        return Err(format!("Note does not exist: {path}"));
    }
    let dest = dest_folder.trim().trim_matches('/').to_string();
    let new_path = if dest.is_empty() {
        file_name.to_string()
    } else {
        format!("{dest}/{file_name}")
    };
    if new_path == path {
        return Err("The note is already in that folder".into());
    }
    // Collision: an existing note, or a folder of that name in the scanned tree.
    if note_exists(ctx, &new_path) || path_is_dir(ctx, &new_path)? {
        return Err(format!("'{new_path}' already exists"));
    }

    if !ctx
        .ai
        .request_permission(Gate::moving(vec![path.to_string()], dest.clone()), ctx.channel)
        .await
    {
        return Ok(denied(&[path]));
    }
    // The handle's rename creates the destination folder (parent dirs) as needed.
    h_rename(ctx, path, &new_path)?;
    let shown = if dest.is_empty() { "(vault root)".to_string() } else { format!("{dest}/") };
    Ok(ToolOutcome::ok(
        json!({ "old_path": path, "new_path": new_path }),
        format!("Moved {path} → {shown}"),
    ))
}

/// True if `path` is a folder in the active vault (via a tree scan).
fn path_is_dir(ctx: &ToolContext<'_>, path: &str) -> Result<bool, String> {
    let tree = h_scan(ctx)?;
    Ok(find_node(&tree, path).map(|n| n.is_dir).unwrap_or(false))
}

/// Gated: moves an entire folder under a new parent. Refuses self-nesting
/// (moving a folder into itself or a descendant), validates collisions before
/// asking for permission, and reuses `rename_at`, whose index rename already
/// rewrites every contained note path by prefix.
async fn move_folder(
    ctx: &ToolContext<'_>,
    path: &str,
    dest_folder: &str,
) -> Result<ToolOutcome, String> {
    let path = path.trim().trim_matches('/').to_string();
    if path.is_empty() {
        return Err("A folder path is required (the vault root cannot be moved)".into());
    }
    if !path_is_dir(ctx, &path)? {
        return Err(format!("Folder does not exist: {path}"));
    }
    let name = path.rsplit('/').next().unwrap_or(&path).to_string();
    let dest = dest_folder.trim().trim_matches('/').to_string();
    if dest == path || dest.starts_with(&format!("{path}/")) {
        return Err(format!(
            "Cannot move '{path}' into itself or one of its own subfolders"
        ));
    }
    let new_path = if dest.is_empty() {
        name.clone()
    } else {
        format!("{dest}/{name}")
    };
    if new_path == path {
        return Err("The folder is already in that location".into());
    }
    if note_exists(ctx, &new_path) || path_is_dir(ctx, &new_path)? {
        return Err(format!("'{new_path}' already exists"));
    }

    if !ctx
        .ai
        .request_permission(Gate::moving(vec![path.clone()], dest.clone()), ctx.channel)
        .await
    {
        return Ok(denied(&[&path]));
    }
    // The handle's rename registers self-writes, creates the destination parent,
    // renames the directory, and updates every `path/…` note in the index.
    h_rename(ctx, &path, &new_path)?;
    let shown = if dest.is_empty() { "(vault root)".to_string() } else { format!("{dest}/") };
    Ok(ToolOutcome::ok(
        json!({ "old_path": path, "new_path": new_path }),
        format!("Moved folder {path} → {shown}"),
    ))
}

fn create_folder(ctx: &ToolContext<'_>, path: &str) -> Result<ToolOutcome, String> {
    h_create_folder(ctx, path)?;
    Ok(ToolOutcome::ok(json!({ "path": path, "created": true }), format!("Created folder {path}")))
}

// ---- gated (destructive) tools ----

async fn delete_note(ctx: &ToolContext<'_>, path: &str) -> Result<ToolOutcome, String> {
    if !ctx
        .ai
        .request_permission(Gate::delete(vec![path.to_string()]), ctx.channel)
        .await
    {
        return Ok(denied(&[path]));
    }
    h_trash(ctx, path)?;
    Ok(ToolOutcome::ok(json!({ "path": path, "deleted": true }), format!("Deleted {path}")))
}

async fn batch_delete(ctx: &ToolContext<'_>, args: &Value) -> Result<ToolOutcome, String> {
    let paths: Vec<String> = args
        .get("paths")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
        .unwrap_or_default();
    if paths.is_empty() {
        return Err("No paths provided to batch_delete".into());
    }
    if !ctx
        .ai
        .request_permission(Gate::delete(paths.clone()), ctx.channel)
        .await
    {
        let refs: Vec<&str> = paths.iter().map(|s| s.as_str()).collect();
        return Ok(denied(&refs));
    }
    let mut deleted = Vec::new();
    let mut errors = Vec::new();
    for p in &paths {
        match h_trash(ctx, p) {
            Ok(()) => deleted.push(p.clone()),
            Err(e) => errors.push(format!("{p}: {e}")),
        }
    }
    let summary = format!("Deleted {} of {} note(s)", deleted.len(), paths.len());
    Ok(ToolOutcome {
        result: json!({ "deleted": deleted, "errors": errors }).to_string(),
        summary,
        detail: (!errors.is_empty()).then(|| errors.join("; ")),
        revision_id: None,
    })
}

/// Gated: deletes an entire folder. Presented to the user like a batch delete —
/// the permission request enumerates every contained `.md` note in `paths` and
/// carries the folder itself in the `folder` field (the UI truncates long
/// lists). On approval the folder is trashed AS ONE UNIT via
/// [`vault::trash_at`]: a single `trash::delete` on the directory produces one
/// cleanly-restorable Trash entry, and `trash_at` then prunes the whole subtree
/// from the index with `remove_prefix`. Reuse `trash_at` the same way for any
/// future batch-ish gated delete — it is the one-call "trash + reindex" helper
/// for both files and directories.
async fn delete_folder(ctx: &ToolContext<'_>, path: &str) -> Result<ToolOutcome, String> {
    let path = path.trim().trim_matches('/').to_string();
    if path.is_empty() {
        return Err("A folder path is required (the vault root cannot be deleted)".into());
    }
    let tree = h_scan(ctx)?;
    let folder = find_node(&tree, &path).filter(|n| n.is_dir);
    let folder = match folder {
        Some(f) => f,
        None => return Err(format!("Folder does not exist: {path}")),
    };

    // Enumerate contained notes so the user sees exactly what approval covers.
    let mut notes: Vec<String> = Vec::new();
    collect_notes(folder, &mut notes);
    notes.sort();

    if !ctx
        .ai
        .request_permission(Gate::delete_folder(path.clone(), notes.clone()), ctx.channel)
        .await
    {
        return Ok(denied(&[&path]));
    }
    h_trash(ctx, &path)?;
    Ok(ToolOutcome {
        result: json!({ "folder": path, "deleted": true, "notes": notes }).to_string(),
        summary: format!("Deleted folder {path} ({} note(s))", notes.len()),
        detail: (!notes.is_empty()).then(|| notes.join(", ")),
        revision_id: None,
    })
}

fn denied(paths: &[&str]) -> ToolOutcome {
    ToolOutcome {
        result: json!({ "denied": true, "message": "User denied the request", "paths": paths }).to_string(),
        summary: "User denied the request".into(),
        detail: None,
        revision_id: None,
    }
}

// ---- helpers ----

/// Removes `<mark>`/`</mark>` snippet highlight markers for tool output.
fn strip_marks(s: &str) -> String {
    s.replace("<mark>", "").replace("</mark>", "")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schemas_cover_every_dispatch_arm() {
        let names: Vec<String> = tool_schemas()
            .iter()
            .map(|t| t["function"]["name"].as_str().unwrap().to_string())
            .collect();
        for expected in [
            "search_notes", "list_notes", "list_folders", "list_tags", "notes_by_tag",
            "read_note", "note_links", "open_note", "create_note", "update_note",
            "append_to_note", "rename_note", "move_note", "move_folder", "create_folder",
            "delete_note", "batch_delete", "delete_folder",
        ] {
            assert!(names.contains(&expected.to_string()), "missing schema: {expected}");
        }
    }

    #[test]
    fn summarize_args_is_human_readable() {
        assert_eq!(summarize_args("search_notes", &json!({"query":"cats"})), "\"cats\"");
        assert_eq!(
            summarize_args("move_note", &json!({"path":"a.md","destination_folder":"archive"})),
            "a.md → archive/"
        );
        assert_eq!(
            summarize_args("move_note", &json!({"path":"a.md","destination_folder":""})),
            "a.md → (root)/"
        );
        assert_eq!(summarize_args("batch_delete", &json!({"paths":["a","b","c"]})), "3 note(s)");
    }

    // ---- dispatcher integration ----

    use crate::ai::client::{consume_stream, ApiMessage, ChatClient};
    use crate::ai::settings::AiSettings;
    use crate::index::Index;
    use std::path::{Path, PathBuf};

    /// Reads all indexed note paths straight from the in-memory index (test-only
    /// helper that replaced the old `with_index`).
    fn index_note_paths(ctx: &ToolContext<'_>) -> Vec<crate::index::NoteRef> {
        ctx.state
            .index
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .list_notes()
            .unwrap()
    }
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, SystemTime, UNIX_EPOCH};
    use tokio::sync::Notify;

    fn temp_vault(tag: &str) -> PathBuf {
        let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let dir = std::env::temp_dir().join(format!("jaynotes-tools-{tag}-{nanos}"));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Builds an `AppState` whose index is an in-memory database rooted at
    /// `root`, optionally pre-indexing `(rel, content)` notes, and whose active
    /// handle is a `PlainHandle` over `root` (so tools dispatch through the same
    /// handle layer the commands use).
    fn state_with_index(root: &Path, notes: &[(&str, &str)]) -> AppState {
        let idx = Index::from_conn(rusqlite::Connection::open_in_memory().unwrap(), root).unwrap();
        for (rel, content) in notes {
            idx.index_file(rel, content).unwrap();
        }
        let state = AppState::default();
        *state.index.lock().unwrap() = Some(idx);
        *state.active.lock().unwrap() =
            Some(Box::new(crate::providers::plain::PlainHandle::new(root)));
        state
    }

    fn noop_channel() -> Channel<AiEvent> {
        Channel::new(|_| Ok(()))
    }

    fn ctx_for<'a>(
        state: &'a AppState,
        ai: &'a AppAiState,
        channel: &'a Channel<AiEvent>,
        _root: &Path,
        rev_dir: PathBuf,
    ) -> ToolContext<'a> {
        ToolContext {
            state,
            ai,
            channel,
            revisions: RevisionSink::Fs(Revisions::new(rev_dir)),
            app: None,
        }
    }

    /// Polls the pending-permission map until a request appears, then resolves
    /// it. Used to drive the gate from the test side.
    async fn answer_next_permission(ai: &AppAiState, approved: bool) {
        for _ in 0..200 {
            let id = ai.pending.lock().unwrap().keys().next().cloned();
            if let Some(id) = id {
                ai.resolve_permission(&id, approved);
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("no permission request appeared");
    }

    #[tokio::test]
    async fn create_and_update_snapshot_then_append() {
        let root = temp_vault("write");
        let rev_dir = root.join(".rev");
        let state = state_with_index(&root, &[]);
        let ai = AppAiState::default();
        let ch = noop_channel();
        let ctx = ctx_for(&state, &ai, &ch, &root, rev_dir.clone());

        // create
        let out = dispatch(&ctx, "create_note", &json!({"path":"n.md","content":"# One\n"})).await;
        assert!(out.result.contains("\"created\":true"), "{}", out.result);
        assert_eq!(std::fs::read_to_string(root.join("n.md")).unwrap(), "# One\n");

        // create again → collision error, file untouched
        let dup = dispatch(&ctx, "create_note", &json!({"path":"n.md","content":"x"})).await;
        assert!(dup.result.starts_with("Error:"));
        assert_eq!(std::fs::read_to_string(root.join("n.md")).unwrap(), "# One\n");

        // update → snapshots the old content, writes the new
        let up = dispatch(&ctx, "update_note", &json!({"path":"n.md","content":"# Two\n"})).await;
        assert!(up.revision_id.is_some(), "update must produce a revision");
        assert_eq!(std::fs::read_to_string(root.join("n.md")).unwrap(), "# Two\n");
        let (rev_path, rev_content) = Revisions::new(rev_dir.clone())
            .get(up.revision_id.as_ref().unwrap())
            .unwrap();
        assert_eq!(rev_path, "n.md");
        assert_eq!(rev_content, "# One\n", "snapshot holds pre-update content");

        // append → separating newline + snapshot
        let ap = dispatch(&ctx, "append_to_note", &json!({"path":"n.md","content":"more"})).await;
        assert!(ap.revision_id.is_some());
        assert_eq!(std::fs::read_to_string(root.join("n.md")).unwrap(), "# Two\nmore");

        std::fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn open_note_validates_existence() {
        let root = temp_vault("open");
        let state = state_with_index(&root, &[]);
        std::fs::write(root.join("here.md"), "hi").unwrap();
        let ai = AppAiState::default();
        let ch = noop_channel();
        let ctx = ctx_for(&state, &ai, &ch, &root, root.join(".rev"));

        // Existing note → success (event emission is a no-op without an app handle).
        let ok = dispatch(&ctx, "open_note", &json!({"path":"here.md"})).await;
        assert!(ok.result.contains("\"opened\":true"), "{}", ok.result);
        assert_eq!(ok.summary, "Opened here.md");

        // Missing note → error.
        let miss = dispatch(&ctx, "open_note", &json!({"path":"nope.md"})).await;
        assert!(miss.result.starts_with("Error:"), "{}", miss.result);

        std::fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn delete_denied_leaves_file_untouched() {
        let root = temp_vault("deny");
        let state = state_with_index(&root, &[]);
        std::fs::write(root.join("keep.md"), "body").unwrap();
        let ai = AppAiState::default();
        let ch = noop_channel();
        let ctx = ctx_for(&state, &ai, &ch, &root, root.join(".rev"));

        let del_args = json!({"path":"keep.md"});
        let (out, _) = tokio::join!(
            dispatch(&ctx, "delete_note", &del_args),
            answer_next_permission(&ai, false),
        );
        assert!(out.result.contains("\"denied\":true"), "{}", out.result);
        assert!(root.join("keep.md").exists(), "denied delete must not remove the file");

        std::fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn batch_delete_approved_trashes_all() {
        let root = temp_vault("batch");
        let state = state_with_index(&root, &[]);
        std::fs::write(root.join("a.md"), "a").unwrap();
        std::fs::write(root.join("b.md"), "b").unwrap();
        let ai = AppAiState::default();
        let ch = noop_channel();
        let ctx = ctx_for(&state, &ai, &ch, &root, root.join(".rev"));

        let batch_args = json!({"paths":["a.md","b.md"]});
        let (out, _) = tokio::join!(
            dispatch(&ctx, "batch_delete", &batch_args),
            answer_next_permission(&ai, true),
        );
        assert!(out.result.contains("\"deleted\""), "{}", out.result);
        // Files left the vault tree (moved into the test .trash sink).
        assert!(!root.join("a.md").exists());
        assert!(!root.join("b.md").exists());
        assert!(root.join(".trash/a.md").exists());

        std::fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn move_note_approved_to_existing_new_and_collision() {
        let root = temp_vault("move");
        let state = state_with_index(&root, &[]);
        std::fs::create_dir_all(root.join("existing")).unwrap();
        std::fs::write(root.join("a.md"), "a").unwrap();
        std::fs::write(root.join("b.md"), "b").unwrap();
        std::fs::write(root.join("existing/c.md"), "c").unwrap();
        let ai = AppAiState::default();
        let ch = noop_channel();
        let ctx = ctx_for(&state, &ai, &ch, &root, root.join(".rev"));

        // move into an existing folder (approved)
        let m1_args = json!({"path":"a.md","destination_folder":"existing"});
        let (m1, _) = tokio::join!(
            dispatch(&ctx, "move_note", &m1_args),
            answer_next_permission(&ai, true),
        );
        assert!(m1.summary.starts_with("Moved a.md → existing/"), "{}", m1.summary);
        assert!(root.join("existing/a.md").exists());
        assert!(!root.join("a.md").exists());

        // move into a new folder (auto-created, approved)
        let m2_args = json!({"path":"b.md","destination_folder":"fresh/deep"});
        let (m2, _) = tokio::join!(
            dispatch(&ctx, "move_note", &m2_args),
            answer_next_permission(&ai, true),
        );
        assert!(m2.result.contains("fresh/deep/b.md"), "{}", m2.result);
        assert!(root.join("fresh/deep/b.md").exists());

        // collision: fails validation BEFORE any permission request.
        std::fs::write(root.join("c.md"), "c2").unwrap();
        let m3 = dispatch(&ctx, "move_note", &json!({"path":"c.md","destination_folder":"existing"})).await;
        assert!(m3.result.starts_with("Error:"), "{}", m3.result);
        assert!(root.join("c.md").exists(), "collision must not move the source");
        assert!(ai.pending.lock().unwrap().is_empty(), "no gate for invalid moves");

        std::fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn move_note_denied_leaves_file_in_place() {
        let root = temp_vault("move-deny");
        let state = state_with_index(&root, &[]);
        std::fs::write(root.join("stay.md"), "body").unwrap();
        let ai = AppAiState::default();
        let ch = noop_channel();
        let ctx = ctx_for(&state, &ai, &ch, &root, root.join(".rev"));

        let args = json!({"path":"stay.md","destination_folder":"elsewhere"});
        let (out, _) = tokio::join!(
            dispatch(&ctx, "move_note", &args),
            answer_next_permission(&ai, false),
        );
        assert!(out.result.contains("\"denied\":true"), "{}", out.result);
        assert!(root.join("stay.md").exists(), "denied move must not relocate the file");
        assert!(!root.join("elsewhere").exists(), "denied move must not create the folder");

        std::fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn move_folder_approved_updates_tree_and_index() {
        let root = temp_vault("move-folder");
        std::fs::create_dir_all(root.join("proj/sub")).unwrap();
        std::fs::write(root.join("proj/x.md"), "x").unwrap();
        std::fs::write(root.join("proj/sub/y.md"), "y").unwrap();
        let state = state_with_index(&root, &[("proj/x.md", "x"), ("proj/sub/y.md", "y")]);
        let ai = AppAiState::default();
        let ch = noop_channel();
        let ctx = ctx_for(&state, &ai, &ch, &root, root.join(".rev"));

        let args = json!({"path":"proj","destination_folder":"archive"});
        let (out, _) = tokio::join!(
            dispatch(&ctx, "move_folder", &args),
            answer_next_permission(&ai, true),
        );
        assert!(out.result.contains("archive/proj"), "{}", out.result);
        assert!(root.join("archive/proj/x.md").exists());
        assert!(root.join("archive/proj/sub/y.md").exists());
        assert!(!root.join("proj").exists());

        // Index paths were prefix-renamed.
        let mut paths: Vec<String> = index_note_paths(&ctx)
            .into_iter()
            .map(|n| n.path)
            .collect();
        paths.sort();
        assert_eq!(paths, vec!["archive/proj/sub/y.md", "archive/proj/x.md"]);

        std::fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn move_folder_rejects_self_nesting_and_collision() {
        let root = temp_vault("move-folder-bad");
        std::fs::create_dir_all(root.join("a/sub")).unwrap();
        std::fs::create_dir_all(root.join("dest/a")).unwrap();
        let state = state_with_index(&root, &[]);
        let ai = AppAiState::default();
        let ch = noop_channel();
        let ctx = ctx_for(&state, &ai, &ch, &root, root.join(".rev"));

        // Into itself.
        let s1 = dispatch(&ctx, "move_folder", &json!({"path":"a","destination_folder":"a"})).await;
        assert!(s1.result.starts_with("Error:"), "{}", s1.result);
        // Into its own descendant.
        let s2 = dispatch(&ctx, "move_folder", &json!({"path":"a","destination_folder":"a/sub"})).await;
        assert!(s2.result.starts_with("Error:"), "{}", s2.result);
        // Target collision (dest/a already exists).
        let s3 = dispatch(&ctx, "move_folder", &json!({"path":"a","destination_folder":"dest"})).await;
        assert!(s3.result.starts_with("Error:"), "{}", s3.result);
        // All rejected during validation — no permission requests, tree intact.
        assert!(ai.pending.lock().unwrap().is_empty());
        assert!(root.join("a/sub").exists());

        std::fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn delete_folder_approved_removes_dir_and_index_rows() {
        let root = temp_vault("del-folder");
        std::fs::create_dir_all(root.join("old/sub")).unwrap();
        std::fs::write(root.join("old/a.md"), "a").unwrap();
        std::fs::write(root.join("old/sub/b.md"), "b").unwrap();
        std::fs::write(root.join("keep.md"), "k").unwrap();
        let state = state_with_index(
            &root,
            &[("old/a.md", "a"), ("old/sub/b.md", "b"), ("keep.md", "k")],
        );
        let ai = AppAiState::default();
        // A capturing channel so we can assert the permission payload shape.
        let events: std::sync::Arc<std::sync::Mutex<Vec<String>>> = Default::default();
        let sink = events.clone();
        let ch: Channel<AiEvent> = Channel::new(move |body| {
            if let tauri::ipc::InvokeResponseBody::Json(s) = body {
                sink.lock().unwrap().push(s);
            }
            Ok(())
        });
        let ctx = ctx_for(&state, &ai, &ch, &root, root.join(".rev"));

        let args = json!({"path":"old"});
        let (out, _) = tokio::join!(
            dispatch(&ctx, "delete_folder", &args),
            answer_next_permission(&ai, true),
        );
        assert!(out.result.contains("\"deleted\":true"), "{}", out.result);
        assert!(!root.join("old").exists(), "folder gone from the vault");
        assert!(root.join(".trash/old/a.md").exists(), "trashed as one unit");
        assert!(root.join("keep.md").exists());

        // Permission event enumerated the contained notes + folder field.
        let perm = events
            .lock()
            .unwrap()
            .iter()
            .find(|e| e.contains("permissionRequest"))
            .cloned()
            .expect("a permissionRequest event was emitted");
        let v: Value = serde_json::from_str(&perm).unwrap();
        assert_eq!(v["action"], "delete");
        assert_eq!(v["folder"], "old");
        let paths: Vec<&str> = v["paths"].as_array().unwrap().iter().map(|p| p.as_str().unwrap()).collect();
        assert_eq!(paths, vec!["old/a.md", "old/sub/b.md"]);

        // Index rows for all children removed; unrelated note remains.
        let remaining: Vec<String> = index_note_paths(&ctx)
            .into_iter()
            .map(|n| n.path)
            .collect();
        assert_eq!(remaining, vec!["keep.md"]);

        std::fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn delete_folder_denied_leaves_everything_untouched() {
        let root = temp_vault("del-folder-deny");
        std::fs::create_dir_all(root.join("old")).unwrap();
        std::fs::write(root.join("old/a.md"), "a").unwrap();
        let state = state_with_index(&root, &[("old/a.md", "a")]);
        let ai = AppAiState::default();
        let ch = noop_channel();
        let ctx = ctx_for(&state, &ai, &ch, &root, root.join(".rev"));

        let args = json!({"path":"old"});
        let (out, _) = tokio::join!(
            dispatch(&ctx, "delete_folder", &args),
            answer_next_permission(&ai, false),
        );
        assert!(out.result.contains("\"denied\":true"), "{}", out.result);
        assert!(root.join("old/a.md").exists(), "denied delete must not touch the folder");
        let count: Vec<_> = index_note_paths(&ctx);
        assert_eq!(count.len(), 1, "index untouched on denial");

        std::fs::remove_dir_all(&root).ok();
    }

    // ---- agent loop against a fake OpenAI endpoint ----

    struct ScriptResponder {
        calls: AtomicUsize,
        tool_sse: Vec<u8>,
        final_sse: Vec<u8>,
    }

    impl wiremock::Respond for ScriptResponder {
        fn respond(&self, _req: &wiremock::Request) -> wiremock::ResponseTemplate {
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            let body = if n == 0 { &self.tool_sse } else { &self.final_sse };
            wiremock::ResponseTemplate::new(200)
                .set_body_raw(body.clone(), "text/event-stream")
        }
    }

    #[tokio::test]
    async fn agent_loop_runs_tool_then_streams_answer() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer};

        let tool_sse = concat!(
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"function\":{\"name\":\"search_notes\",\"arguments\":\"{\\\"query\\\":\\\"cats\\\"}\"}}]}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
            "data: [DONE]\n\n",
        )
        .as_bytes()
        .to_vec();
        let final_sse = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"Found \"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"1 note.\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
            "data: [DONE]\n\n",
        )
        .as_bytes()
        .to_vec();

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ScriptResponder {
                calls: AtomicUsize::new(0),
                tool_sse,
                final_sse,
            })
            .mount(&server)
            .await;

        // A vault index containing the note the tool should find.
        let root = temp_vault("loop");
        let state = state_with_index(&root, &[("cats.md", "# Cats\nCats are wonderful.")]);
        let ai = AppAiState::default();
        let ch = noop_channel();
        let ctx = ctx_for(&state, &ai, &ch, &root, root.join(".rev"));

        let client = ChatClient::new(&AiSettings {
            base_url: server.uri(),
            model: "test-model".into(),
            ..Default::default()
        })
        .unwrap();
        let schemas = tool_schemas();
        let cancel = Notify::new();

        // Round 1: model asks to search.
        let mut messages = vec![ApiMessage::system("s"), ApiMessage::user("find cats")];
        let resp = client.stream(&messages, &schemas, true).await.unwrap();
        let turn = consume_stream(resp, |_| {}, &cancel).await.unwrap();
        assert!(turn.wants_tools());
        assert_eq!(turn.tool_calls[0].function.name, "search_notes");

        // Dispatch the tool against the real temp-vault index.
        messages.push(ApiMessage::assistant(None, turn.tool_calls.clone()));
        for tc in &turn.tool_calls {
            let args: Value = serde_json::from_str(&tc.function.arguments).unwrap();
            let out = dispatch(&ctx, &tc.function.name, &args).await;
            assert!(out.result.contains("cats.md"), "tool saw the note: {}", out.result);
            messages.push(ApiMessage::tool(tc.id.clone(), out.result));
        }

        // Round 2: final answer streamed.
        let resp2 = client.stream(&messages, &schemas, true).await.unwrap();
        let mut streamed = String::new();
        let turn2 = consume_stream(
            resp2,
            |p| {
                if let crate::ai::client::StreamPiece::Content(t) = p {
                    streamed.push_str(&t);
                }
            },
            &cancel,
        )
        .await
        .unwrap();
        assert_eq!(streamed, "Found 1 note.");
        assert_eq!(turn2.content, "Found 1 note.");
        assert!(!turn2.wants_tools());

        // The follow-up request carried the tool result back to the provider.
        let reqs = server.received_requests().await.unwrap();
        assert_eq!(reqs.len(), 2);
        let second = String::from_utf8_lossy(&reqs[1].body);
        assert!(second.contains("\"role\":\"tool\""));
        assert!(second.contains("cats.md"));

        std::fs::remove_dir_all(&root).ok();
    }
}
