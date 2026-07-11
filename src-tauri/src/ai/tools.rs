//! Tool schemas and the dispatcher that executes them against the vault.
//!
//! Every mutating tool goes through the shared cores in [`crate::vault`] (so it
//! registers a self-write and keeps the search index fresh, exactly like the
//! hand-driven commands) and every path is jailed by `safe_join`. Tool failures
//! are returned to the model as `Error: …` results rather than propagated, so a
//! bad argument never crashes the agent loop — the model can read the error and
//! try again.

use std::path::Path;

use serde_json::{json, Value};
use tauri::ipc::Channel;
use walkdir::WalkDir;

use super::revisions::Revisions;
use super::{AiEvent, AppAiState};
use crate::index::{AppState, Index};
use crate::vault;

/// Everything a tool needs to run.
pub struct ToolContext<'a> {
    pub state: &'a AppState,
    pub ai: &'a AppAiState,
    pub channel: &'a Channel<AiEvent>,
    pub root: std::path::PathBuf,
    pub revisions: Revisions,
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
            "RELOCATE a note into a different folder, keeping its file name. The destination folder is created if missing. Use this (not rename_note) to organize notes into folders.",
            obj(json!({
                "path": { "type": "string" },
                "destination_folder": { "type": "string", "description": "Target folder relative path; empty string means the vault root." }
            }), &["path", "destination_folder"]),
        ),
        tool(
            "create_folder",
            "Create a new folder (and any missing parents) for organizing notes.",
            obj(json!({ "path": { "type": "string" } }), &["path"]),
        ),
        tool(
            "delete_note",
            "Move a note to the Trash. This requires explicit user approval before it runs.",
            obj(json!({ "path": { "type": "string" } }), &["path"]),
        ),
        tool(
            "batch_delete",
            "Move several notes to the Trash at once. Requires a single explicit user approval covering all of them.",
            obj(json!({ "paths": { "type": "array", "items": { "type": "string" } } }), &["paths"]),
        ),
    ]
}

/// A short human summary of a tool call's arguments, for the `ToolCall` event.
pub fn summarize_args(name: &str, args: &Value) -> String {
    let s = |k: &str| args.get(k).and_then(|v| v.as_str()).unwrap_or("");
    match name {
        "search_notes" => format!("\"{}\"", s("query")),
        "notes_by_tag" => format!("#{}", s("tag")),
        "read_note" | "note_links" | "create_note" | "update_note" | "append_to_note"
        | "delete_note" | "create_folder" => s("path").to_string(),
        "rename_note" => format!("{} → {}", s("old_path"), s("new_path")),
        "move_note" => {
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
        "create_note" => create_note(ctx, arg_str(args, "path")?, arg_str(args, "content")?),
        "update_note" => update_note(ctx, arg_str(args, "path")?, arg_str(args, "content")?),
        "append_to_note" => append_to_note(ctx, arg_str(args, "path")?, arg_str(args, "content")?),
        "rename_note" => rename_note(ctx, arg_str(args, "old_path")?, arg_str(args, "new_path")?),
        "move_note" => move_note(ctx, arg_str(args, "path")?, arg_str(args, "destination_folder").unwrap_or("")),
        "create_folder" => create_folder(ctx, arg_str(args, "path")?),
        "delete_note" => delete_note(ctx, arg_str(args, "path")?).await,
        "batch_delete" => batch_delete(ctx, args).await,
        other => Err(format!("Unknown tool '{other}'")),
    }
}

fn arg_str<'a>(args: &'a Value, key: &str) -> Result<&'a str, String> {
    args.get(key)
        .and_then(|v| v.as_str())
        .ok_or_else(|| format!("Missing required string argument '{key}'"))
}

/// Locks the index and runs `f` against it.
fn with_index<T>(ctx: &ToolContext<'_>, f: impl FnOnce(&Index) -> Result<T, String>) -> Result<T, String> {
    let guard = ctx.state.index.lock().map_err(|_| "Index lock poisoned".to_string())?;
    let idx = guard.as_ref().ok_or("No vault is indexed")?;
    f(idx)
}

// ---- read tools ----

fn search_notes(ctx: &ToolContext<'_>, query: &str) -> Result<ToolOutcome, String> {
    let hits = with_index(ctx, |idx| idx.search(query, 25))?;
    let arr: Vec<Value> = hits
        .iter()
        .map(|h| json!({ "path": h.path, "title": h.title, "snippet": strip_marks(&h.snippet), "tags": h.tags }))
        .collect();
    let summary = format!("Searched \"{query}\" — {} result(s)", arr.len());
    Ok(ToolOutcome::ok(json!({ "results": arr }), summary))
}

fn list_notes(ctx: &ToolContext<'_>, limit: Option<u32>) -> Result<ToolOutcome, String> {
    let mut notes = with_index(ctx, |idx| idx.list_notes())?;
    let limit = limit.unwrap_or(50) as usize;
    notes.truncate(limit);
    let arr: Vec<Value> = notes.iter().map(|n| json!({ "path": n.path, "title": n.title })).collect();
    let summary = format!("Listed {} note(s)", arr.len());
    Ok(ToolOutcome::ok(json!({ "notes": arr }), summary))
}

fn list_folders(ctx: &ToolContext<'_>) -> Result<ToolOutcome, String> {
    let mut folders: Vec<String> = Vec::new();
    for entry in WalkDir::new(&ctx.root)
        .min_depth(1)
        .into_iter()
        .filter_entry(|e| !e.file_name().to_string_lossy().starts_with('.'))
    {
        let entry = entry.map_err(|e| format!("Error scanning vault: {e}"))?;
        if !entry.file_type().is_dir() {
            continue;
        }
        if let Ok(rel) = vault::to_rel_string(&ctx.root, entry.path()) {
            if !rel.is_empty() {
                folders.push(rel);
            }
        }
    }
    folders.sort();
    let summary = format!("Listed {} folder(s)", folders.len());
    Ok(ToolOutcome::ok(json!({ "folders": folders }), summary))
}

fn list_tags(ctx: &ToolContext<'_>) -> Result<ToolOutcome, String> {
    let tags = with_index(ctx, |idx| idx.list_tags())?;
    let arr: Vec<Value> = tags.iter().map(|t| json!({ "tag": t.tag, "count": t.count })).collect();
    let summary = format!("Listed {} tag(s)", arr.len());
    Ok(ToolOutcome::ok(json!({ "tags": arr }), summary))
}

fn notes_by_tag(ctx: &ToolContext<'_>, tag: &str) -> Result<ToolOutcome, String> {
    let hits = with_index(ctx, |idx| idx.notes_by_tag(tag, 200))?;
    let arr: Vec<Value> = hits.iter().map(|h| json!({ "path": h.path, "title": h.title })).collect();
    let summary = format!("#{tag} — {} note(s)", arr.len());
    Ok(ToolOutcome::ok(json!({ "notes": arr }), summary))
}

fn read_note(ctx: &ToolContext<'_>, path: &str) -> Result<ToolOutcome, String> {
    let abs = vault::safe_join(&ctx.root, path)?;
    if !abs.is_file() {
        return Err(format!("Note does not exist: {path}"));
    }
    let content = std::fs::read_to_string(&abs).map_err(|e| format!("Could not read '{path}': {e}"))?;
    Ok(ToolOutcome::ok(json!({ "path": path, "content": content }), format!("Read {path}")))
}

fn note_links(ctx: &ToolContext<'_>, path: &str) -> Result<ToolOutcome, String> {
    let (outgoing, backlinks) = with_index(ctx, |idx| idx.links_for(path))?;
    let summary = format!("{path}: {} out, {} back", outgoing.len(), backlinks.len());
    Ok(ToolOutcome::ok(json!({ "outgoing": outgoing, "backlinks": backlinks }), summary))
}

// ---- write tools ----

fn create_note(ctx: &ToolContext<'_>, path: &str, content: &str) -> Result<ToolOutcome, String> {
    let created = vault::create_note_exact(&ctx.root, ctx.state, path, content)?;
    Ok(ToolOutcome::ok(json!({ "path": created, "created": true }), format!("Created {created}")))
}

fn update_note(ctx: &ToolContext<'_>, path: &str, content: &str) -> Result<ToolOutcome, String> {
    let abs = vault::safe_join(&ctx.root, path)?;
    if !abs.is_file() {
        return Err(format!("Note does not exist: {path} (use create_note to make it)"));
    }
    let revision_id = snapshot_current(ctx, path, &abs)?;
    vault::write_note_at(&ctx.root, ctx.state, path, content)?;
    Ok(ToolOutcome {
        result: json!({ "path": path, "updated": true }).to_string(),
        summary: format!("Updated {path}"),
        detail: None,
        revision_id: Some(revision_id),
    })
}

fn append_to_note(ctx: &ToolContext<'_>, path: &str, content: &str) -> Result<ToolOutcome, String> {
    let abs = vault::safe_join(&ctx.root, path)?;
    if !abs.is_file() {
        return Err(format!("Note does not exist: {path} (use create_note to make it)"));
    }
    let current = std::fs::read_to_string(&abs).map_err(|e| format!("Could not read '{path}': {e}"))?;
    let revision_id = ctx.revisions.snapshot(path, &current)?;
    let mut next = current;
    if !next.is_empty() && !next.ends_with('\n') {
        next.push('\n');
    }
    next.push_str(content);
    vault::write_note_at(&ctx.root, ctx.state, path, &next)?;
    Ok(ToolOutcome {
        result: json!({ "path": path, "appended": true }).to_string(),
        summary: format!("Appended to {path}"),
        detail: None,
        revision_id: Some(revision_id),
    })
}

fn rename_note(ctx: &ToolContext<'_>, old_path: &str, new_path: &str) -> Result<ToolOutcome, String> {
    vault::rename_at(&ctx.root, ctx.state, old_path, new_path)?;
    Ok(ToolOutcome::ok(
        json!({ "old_path": old_path, "new_path": new_path }),
        format!("Renamed {old_path} → {new_path}"),
    ))
}

fn move_note(ctx: &ToolContext<'_>, path: &str, dest_folder: &str) -> Result<ToolOutcome, String> {
    let file_name = Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .ok_or_else(|| format!("Invalid note path: {path}"))?;
    let dest = dest_folder.trim().trim_matches('/');
    let new_path = if dest.is_empty() {
        file_name.clone()
    } else {
        format!("{dest}/{file_name}")
    };
    if new_path == path {
        return Err("The note is already in that folder".into());
    }
    // Create the destination folder if needed (harmless if it exists).
    if !dest.is_empty() {
        let dest_abs = vault::safe_join(&ctx.root, dest)?;
        if !dest_abs.exists() {
            vault::create_folder_at(&ctx.root, dest)?;
        }
    }
    vault::rename_at(&ctx.root, ctx.state, path, &new_path)?;
    let shown = if dest.is_empty() { "(vault root)".to_string() } else { format!("{dest}/") };
    Ok(ToolOutcome::ok(
        json!({ "old_path": path, "new_path": new_path }),
        format!("Moved {path} → {shown}"),
    ))
}

fn create_folder(ctx: &ToolContext<'_>, path: &str) -> Result<ToolOutcome, String> {
    vault::create_folder_at(&ctx.root, path)?;
    Ok(ToolOutcome::ok(json!({ "path": path, "created": true }), format!("Created folder {path}")))
}

// ---- gated (destructive) tools ----

async fn delete_note(ctx: &ToolContext<'_>, path: &str) -> Result<ToolOutcome, String> {
    if !ctx.ai.request_permission("delete", vec![path.to_string()], ctx.channel).await {
        return Ok(denied(&[path]));
    }
    vault::trash_at(&ctx.root, ctx.state, path)?;
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
    if !ctx.ai.request_permission("delete", paths.clone(), ctx.channel).await {
        let refs: Vec<&str> = paths.iter().map(|s| s.as_str()).collect();
        return Ok(denied(&refs));
    }
    let mut deleted = Vec::new();
    let mut errors = Vec::new();
    for p in &paths {
        match vault::trash_at(&ctx.root, ctx.state, p) {
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

fn denied(paths: &[&str]) -> ToolOutcome {
    ToolOutcome {
        result: json!({ "denied": true, "message": "User denied the request", "paths": paths }).to_string(),
        summary: "User denied the request".into(),
        detail: None,
        revision_id: None,
    }
}

// ---- helpers ----

/// Snapshots the current on-disk content of `path` before a mutation.
fn snapshot_current(ctx: &ToolContext<'_>, path: &str, abs: &Path) -> Result<String, String> {
    let current = std::fs::read_to_string(abs).map_err(|e| format!("Could not read '{path}': {e}"))?;
    ctx.revisions.snapshot(path, &current)
}

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
            "read_note", "note_links", "create_note", "update_note", "append_to_note",
            "rename_note", "move_note", "create_folder", "delete_note", "batch_delete",
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
    use std::path::PathBuf;
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
    /// `root`, optionally pre-indexing `(rel, content)` notes.
    fn state_with_index(root: &Path, notes: &[(&str, &str)]) -> AppState {
        let idx = Index::from_conn(rusqlite::Connection::open_in_memory().unwrap(), root).unwrap();
        for (rel, content) in notes {
            idx.index_file(rel, content).unwrap();
        }
        let state = AppState::default();
        *state.index.lock().unwrap() = Some(idx);
        state
    }

    fn noop_channel() -> Channel<AiEvent> {
        Channel::new(|_| Ok(()))
    }

    fn ctx_for<'a>(
        state: &'a AppState,
        ai: &'a AppAiState,
        channel: &'a Channel<AiEvent>,
        root: &Path,
        rev_dir: PathBuf,
    ) -> ToolContext<'a> {
        ToolContext {
            state,
            ai,
            channel,
            root: root.to_path_buf(),
            revisions: Revisions::new(rev_dir),
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
    async fn move_note_to_existing_new_and_collision() {
        let root = temp_vault("move");
        let state = state_with_index(&root, &[]);
        std::fs::create_dir_all(root.join("existing")).unwrap();
        std::fs::write(root.join("a.md"), "a").unwrap();
        std::fs::write(root.join("b.md"), "b").unwrap();
        std::fs::write(root.join("existing/c.md"), "c").unwrap();
        let ai = AppAiState::default();
        let ch = noop_channel();
        let ctx = ctx_for(&state, &ai, &ch, &root, root.join(".rev"));

        // move into an existing folder
        let m1 = dispatch(&ctx, "move_note", &json!({"path":"a.md","destination_folder":"existing"})).await;
        assert!(m1.summary.starts_with("Moved a.md → existing/"), "{}", m1.summary);
        assert!(root.join("existing/a.md").exists());
        assert!(!root.join("a.md").exists());

        // move into a new folder (auto-created)
        let m2 = dispatch(&ctx, "move_note", &json!({"path":"b.md","destination_folder":"fresh/deep"})).await;
        assert!(m2.result.contains("fresh/deep/b.md"), "{}", m2.result);
        assert!(root.join("fresh/deep/b.md").exists());

        // collision: existing/c.md already there
        std::fs::write(root.join("c.md"), "c2").unwrap();
        let m3 = dispatch(&ctx, "move_note", &json!({"path":"c.md","destination_folder":"existing"})).await;
        assert!(m3.result.starts_with("Error:"), "{}", m3.result);
        assert!(root.join("c.md").exists(), "collision must not move the source");

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
        let turn2 = consume_stream(resp2, |t| streamed.push_str(t), &cancel).await.unwrap();
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
