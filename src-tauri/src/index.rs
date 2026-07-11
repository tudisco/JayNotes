//! SQLite FTS5 search index for a vault.
//!
//! Each vault gets its own database at
//! `app_data_dir()/indexes/<hash-of-vault-path>.db`, keeping the vault folder
//! itself clean. The index mirrors the vault's `.md` files into three query
//! surfaces: full-text search (`notes_fts`), tags, and wikilinks.
//!
//! ## FTS content mode
//!
//! `notes_fts` is a **regular (content-owning) FTS5 table**, not the
//! `content=''` contentless variant from the milestone sketch. Contentless FTS
//! requires the caller to hand-manage rowid bookkeeping on every update and is
//! easy to get subtly wrong; a content-owning table lets us `DELETE ... WHERE
//! rowid = ?` then re-`INSERT` on each upsert, which is trivially correct. The
//! duplicated body text costs disk but never truth. `notes.id` is used as the
//! FTS `rowid`, so the two tables stay joined without a mapping table.
//!
//! ## Extraction
//!
//! The per-file extractors ([`split_frontmatter`], [`extract_inline_tags`],
//! [`extract_links`]) are deliberately dependency-light hand rolls that mirror
//! the TypeScript rules in `src/lib/utils/{frontmatter,metadata}.ts` so the
//! Rust index and the JS editor agree on what a note contains. Frontmatter YAML
//! `tags:` is parsed with `serde_yaml`.

use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use rusqlite::types::Value;
use rusqlite::{params, params_from_iter, Connection, OptionalExtension};
use serde::Serialize;
use tauri::{Emitter, Manager};
use walkdir::WalkDir;

/// Bumped whenever the schema below changes; a mismatch drops & rebuilds.
const SCHEMA_VERSION: i64 = 1;

// ---------------------------------------------------------------------------
// Managed state
// ---------------------------------------------------------------------------

/// Tauri-managed application state shared across commands and the watcher.
pub struct AppState {
    /// The open index for the current vault, or `None` when no vault is set.
    /// An `Arc` so the file watcher thread can hold its own handle.
    ///
    /// For a plain vault this is the separate on-disk FTS index. For a
    /// self-indexing vault (encrypted-db, whose container *is* the index) this
    /// stays `None` and the search commands dispatch through [`Self::active`]
    /// instead — see `VaultHandle::owns_index`.
    pub index: Arc<Mutex<Option<Index>>>,
    /// Paths (vault-relative) the app itself wrote recently, with a timestamp,
    /// so the watcher can ignore its own writes. Shared with the watcher.
    pub recent_writes: Arc<Mutex<HashMap<String, std::time::Instant>>>,
    /// The active file watcher; dropping it stops watching. Replaced on each
    /// vault change.
    pub watcher: Mutex<Option<crate::watcher::WatcherHandle>>,
    /// The active vault's opened storage handle, through which every `vault.rs`
    /// command dispatches. `None` when no vault is open (e.g. an encrypted vault
    /// that hasn't been unlocked yet).
    pub active: Mutex<Option<Box<dyn crate::providers::VaultHandle>>>,
}

impl Default for AppState {
    fn default() -> Self {
        AppState {
            index: Arc::new(Mutex::new(None)),
            recent_writes: Arc::new(Mutex::new(HashMap::new())),
            watcher: Mutex::new(None),
            active: Mutex::new(None),
        }
    }
}

/// Records that the app just wrote `rel`, so the watcher suppresses the echo.
pub fn register_write(recent: &Arc<Mutex<HashMap<String, std::time::Instant>>>, rel: &str) {
    if let Ok(mut map) = recent.lock() {
        map.insert(rel.to_string(), std::time::Instant::now());
    }
}

// ---------------------------------------------------------------------------
// Extraction (mirrors src/lib/utils/{frontmatter,metadata}.ts)
// ---------------------------------------------------------------------------

/// The indexable pieces of a single note.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Extracted {
    pub frontmatter: Option<String>,
    pub body: String,
    /// Frontmatter tags followed by inline `#tags`, deduped, order-preserving.
    pub tags: Vec<String>,
    /// `[[wikilink]]` targets (alias dropped, trimmed), deduped.
    pub links: Vec<String>,
}

/// True for a line that is nothing but `---` plus optional trailing spaces/tabs.
fn is_fence_line(line: &str) -> bool {
    line.starts_with("---") && line[3..].chars().all(|c| c == ' ' || c == '\t')
}

fn strip_cr(s: &str) -> &str {
    s.strip_suffix('\r').unwrap_or(s)
}

/// Splits a leading `---` frontmatter block from the body, mirroring the
/// `FRONTMATTER_RE` regex in `frontmatter.ts`. The returned block includes both
/// fences and the trailing newline; `None` means the note has no frontmatter.
pub fn split_frontmatter(raw: &str) -> (Option<String>, String) {
    let first_nl = match raw.find('\n') {
        Some(i) => i,
        None => return (None, raw.to_string()),
    };
    if !is_fence_line(strip_cr(&raw[..first_nl])) {
        return (None, raw.to_string());
    }
    let mut pos = first_nl + 1;
    while pos <= raw.len() {
        let rest = &raw[pos..];
        let (line, next_pos, had_nl) = match rest.find('\n') {
            Some(i) => (&rest[..i], pos + i + 1, true),
            None => (rest, raw.len(), false),
        };
        if is_fence_line(strip_cr(line)) {
            return (Some(raw[..next_pos].to_string()), raw[next_pos..].to_string());
        }
        if !had_nl {
            break;
        }
        pos = next_pos;
    }
    (None, raw.to_string())
}

/// Normalizes a raw `tags` YAML value into a tag list, mirroring the
/// `normalizeTags` rules in `metadata.ts` (array | scalar; split scalars on
/// whitespace/commas; strip a leading `#`; trim; dedupe; preserve order).
fn normalize_yaml_tags(value: &serde_yaml::Value, out: &mut Vec<String>, seen: &mut HashSet<String>) {
    let push = |raw: String, out: &mut Vec<String>, seen: &mut HashSet<String>| {
        let tag = raw.trim().trim_start_matches('#').to_string();
        if !tag.is_empty() && seen.insert(tag.clone()) {
            out.push(tag);
        }
    };
    match value {
        serde_yaml::Value::Sequence(seq) => {
            for item in seq {
                let s = yaml_scalar_to_string(item);
                push(s, out, seen);
            }
        }
        serde_yaml::Value::String(s) => {
            for part in s.split(|c: char| c == ',' || c.is_whitespace()) {
                push(part.to_string(), out, seen);
            }
        }
        serde_yaml::Value::Null => {}
        other => push(yaml_scalar_to_string(other), out, seen),
    }
}

fn yaml_scalar_to_string(v: &serde_yaml::Value) -> String {
    match v {
        serde_yaml::Value::String(s) => s.clone(),
        serde_yaml::Value::Bool(b) => b.to_string(),
        serde_yaml::Value::Number(n) => n.to_string(),
        _ => String::new(),
    }
}

/// Parses `tags:` out of a verbatim frontmatter block (fences included).
fn frontmatter_tags(block: &str) -> Vec<String> {
    let inner = strip_fences(block);
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    if let Ok(serde_yaml::Value::Mapping(map)) = serde_yaml::from_str::<serde_yaml::Value>(&inner) {
        if let Some(tags) = map.get(serde_yaml::Value::String("tags".to_string())) {
            normalize_yaml_tags(tags, &mut out, &mut seen);
        }
    }
    out
}

/// Removes the opening and closing `---` fence lines from a frontmatter block.
fn strip_fences(block: &str) -> String {
    let mut lines: Vec<&str> = block.split('\n').collect();
    // Drop trailing empty element produced by a trailing newline.
    if lines.last() == Some(&"") {
        lines.pop();
    }
    if !lines.is_empty() && is_fence_line(strip_cr(lines[0])) {
        lines.remove(0);
    }
    if !lines.is_empty() && is_fence_line(strip_cr(lines[lines.len() - 1])) {
        lines.pop();
    }
    lines.join("\n")
}

fn is_word_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// Detects a code-fence line: up to 3 leading whitespace then a run of >= 3
/// backticks or tildes. Returns `(marker_char, run_length)`.
fn fence_marker(line: &str) -> Option<(char, usize)> {
    let chars: Vec<char> = line.chars().collect();
    let mut nw = 0;
    while nw < chars.len() && chars[nw].is_whitespace() {
        nw += 1;
    }
    if nw > 3 || nw >= chars.len() {
        return None;
    }
    let c = chars[nw];
    if c != '`' && c != '~' {
        return None;
    }
    let len = chars[nw..].iter().take_while(|&&x| x == c).count();
    if len >= 3 {
        Some((c, len))
    } else {
        None
    }
}

/// Removes inline `` `code` `` spans from a single line, mirroring the
/// ``/`[^`\n]*`/g`` replace in `metadata.ts`.
fn strip_inline_code(line: &str) -> String {
    let chars: Vec<char> = line.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '`' {
            if let Some(close) = (i + 1..chars.len()).find(|&j| chars[j] == '`') {
                i = close + 1; // drop the whole span
                continue;
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

/// Extracts inline `#tags` from a note body, mirroring `extractInlineTags` in
/// `metadata.ts`: a tag is `#` + a letter + `[A-Za-z0-9_/-]*`, the `#` must not
/// follow a word char / `#` / `/`, and matches inside fenced code blocks and
/// inline code spans are ignored. Order-preserving and deduped.
pub fn extract_inline_tags(body: &str) -> Vec<String> {
    let mut scanned: Vec<String> = Vec::new();
    let mut fence: Option<(char, usize)> = None;
    for line in body.split('\n') {
        let line = strip_cr(line);
        let marker = fence_marker(line);
        if let Some((fc, flen)) = fence {
            if let Some((mc, ml)) = marker {
                if mc == fc && ml >= flen {
                    fence = None;
                }
            }
            continue; // fenced content never contributes tags
        }
        if let Some(m) = marker {
            fence = Some(m);
            continue;
        }
        scanned.push(strip_inline_code(line));
    }

    let text = scanned.join("\n");
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '#' {
            let prev_ok = i == 0 || {
                let p = chars[i - 1];
                !(is_word_char(p) || p == '#' || p == '/')
            };
            if prev_ok && i + 1 < chars.len() && chars[i + 1].is_ascii_alphabetic() {
                let mut j = i + 1;
                let mut tag = String::new();
                while j < chars.len() && (is_word_char(chars[j]) || chars[j] == '/' || chars[j] == '-') {
                    tag.push(chars[j]);
                    j += 1;
                }
                if seen.insert(tag.clone()) {
                    out.push(tag);
                }
                i = j;
                continue;
            }
        }
        i += 1;
    }
    out
}

/// Extracts `[[wikilink]]` targets from a body: text between `[[` and `]]`,
/// with any `|alias` suffix dropped, trimmed. Deduped, order-preserving.
pub fn extract_links(body: &str) -> Vec<String> {
    let chars: Vec<char> = body.chars().collect();
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    let mut i = 0;
    while i + 1 < chars.len() {
        if chars[i] == '[' && chars[i + 1] == '[' {
            let mut j = i + 2;
            let mut close = None;
            while j + 1 < chars.len() {
                if chars[j] == '\n' {
                    break; // wikilinks don't span lines
                }
                if chars[j] == '[' && chars[j + 1] == '[' {
                    break; // a new opener supersedes this unterminated one
                }
                if chars[j] == ']' && chars[j + 1] == ']' {
                    close = Some(j);
                    break;
                }
                j += 1;
            }
            if let Some(c) = close {
                let inner: String = chars[i + 2..c].iter().collect();
                let target = inner.split('|').next().unwrap_or("").trim().to_string();
                if !target.is_empty() && seen.insert(target.clone()) {
                    out.push(target);
                }
                i = c + 2;
                continue;
            }
        }
        i += 1;
    }
    out
}

/// Full per-file extraction from raw note content.
pub fn extract(raw: &str) -> Extracted {
    let (frontmatter, body) = split_frontmatter(raw);

    let mut tags = Vec::new();
    let mut seen = HashSet::new();
    if let Some(ref fm) = frontmatter {
        for t in frontmatter_tags(fm) {
            if seen.insert(t.clone()) {
                tags.push(t);
            }
        }
    }
    for t in extract_inline_tags(&body) {
        if seen.insert(t.clone()) {
            tags.push(t);
        }
    }

    let links = extract_links(&body);
    Extracted {
        frontmatter,
        body,
        tags,
        links,
    }
}

/// Splits a raw search query into `(text_terms, tag_filters)`. Whitespace
/// tokens of the form `tag:foo` become tag filters (leading `#` stripped);
/// everything else is a free-text term. Both are order-preserving.
pub(crate) fn parse_query(query: &str) -> (Vec<String>, Vec<String>) {
    let mut terms = Vec::new();
    let mut tags = Vec::new();
    for tok in query.split_whitespace() {
        if let Some(rest) = tok.strip_prefix("tag:") {
            let t = rest.trim_start_matches('#').trim();
            if !t.is_empty() {
                tags.push(t.to_string());
            }
        } else {
            terms.push(tok.to_string());
        }
    }
    (terms, tags)
}

/// Builds a safe FTS5 MATCH expression from free-text terms. Each term is
/// emitted as a double-quoted FTS string (internal `"` doubled) so arbitrary
/// user punctuation — `"`, `(`, `-`, `*` — can never be interpreted as FTS
/// query syntax. Terms carrying no alphanumeric content (e.g. a lone `*`) are
/// dropped so we never emit an empty-phrase (`""`) that FTS rejects. A trailing
/// `*` on the last surviving term gives prefix-as-you-type matching. Returns
/// `None` when nothing searchable remains.
pub(crate) fn build_match(terms: &[String]) -> Option<String> {
    let kept: Vec<&String> = terms
        .iter()
        .filter(|t| t.chars().any(char::is_alphanumeric))
        .collect();
    if kept.is_empty() {
        return None;
    }
    let last = kept.len() - 1;
    let parts: Vec<String> = kept
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let escaped = t.replace('"', "\"\"");
            if i == last {
                format!("\"{escaped}\"*")
            } else {
                format!("\"{escaped}\"")
            }
        })
        .collect();
    Some(parts.join(" "))
}

/// Drops a trailing `.md`/`.MD` extension from a string, if present.
pub(crate) fn strip_md_suffix(s: &str) -> &str {
    s.strip_suffix(".md")
        .or_else(|| s.strip_suffix(".MD"))
        .unwrap_or(s)
}

/// Title from a vault-relative path: the file stem (name without `.md`).
pub(crate) fn title_of(rel: &str) -> String {
    let name = rel.rsplit('/').next().unwrap_or(rel);
    name.strip_suffix(".md")
        .or_else(|| name.strip_suffix(".MD"))
        .unwrap_or(name)
        .to_string()
}

// ---------------------------------------------------------------------------
// Path helpers (local copies so this module is self-contained)
// ---------------------------------------------------------------------------

fn is_markdown(path: &Path) -> bool {
    path.extension()
        .map(|e| e.eq_ignore_ascii_case("md"))
        .unwrap_or(false)
}

fn to_rel_string(root: &Path, abs: &Path) -> Option<String> {
    let rel = abs.strip_prefix(root).ok()?;
    Some(
        rel.components()
            .filter_map(|c| match c {
                Component::Normal(part) => Some(part.to_string_lossy().into_owned()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("/"),
    )
}

/// True if any component of a relative path is dot-prefixed (hidden).
pub fn rel_is_hidden(rel: &str) -> bool {
    rel.split('/').any(|part| part.starts_with('.'))
}

// ---------------------------------------------------------------------------
// Index
// ---------------------------------------------------------------------------

/// A per-vault SQLite index. Holds an open connection plus the vault root so it
/// can stat and read files during scans.
pub struct Index {
    conn: Connection,
    vault_root: PathBuf,
    /// Duration of the most recent full scan, in milliseconds.
    last_scan_ms: AtomicU64,
}

/// Serializable snapshot for the `index_status` command.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexStatus {
    pub note_count: i64,
    pub last_scan_ms: u64,
}

/// A single search result. `snippet` may embed `<mark>…</mark>` around matched
/// terms (raw, un-escaped body text otherwise — the frontend escapes it).
/// `score` is the bm25 rank (lower = better) or 0.0 for tag-only queries.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchHit {
    pub path: String,
    pub title: String,
    pub snippet: String,
    pub score: f64,
    pub tags: Vec<String>,
}

/// A lightweight note reference for the quick switcher.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NoteRef {
    pub path: String,
    pub title: String,
    pub mtime: i64,
}

/// A distinct tag with the number of notes carrying it. Powers the Tags panel.
#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TagCount {
    pub tag: String,
    pub count: u32,
}

impl Index {
    /// Opens (or creates) the index database at `db_path` for `vault_root`,
    /// rebuilding the schema if the stored version doesn't match. Unkeyed: a
    /// plain SQLite index for a plain vault.
    pub fn open(db_path: &Path, vault_root: &Path) -> Result<Index, String> {
        Self::open_keyed(db_path, vault_root, None)
    }

    /// Opens (or creates) the index database, optionally as a SQLCipher-keyed
    /// container.
    ///
    /// An encrypted-files vault must never leak plaintext into its search index
    /// (the FTS bodies are decrypted note text), so its index is itself keyed
    /// with a 32-byte raw key applied via `PRAGMA key = "x'…'"` — the same
    /// SQLCipher raw-key mode the encrypted-db container uses. When `key` is
    /// `None` the DB opens as ordinary SQLite (plain vaults). The vendored build
    /// is SQLCipher, so a keyed reopen of an unkeyed DB (or a wrong key) fails at
    /// the first query in [`ensure_schema`], surfacing as an error.
    pub fn open_keyed(
        db_path: &Path,
        vault_root: &Path,
        key: Option<&[u8; 32]>,
    ) -> Result<Index, String> {
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("Could not create index dir: {e}"))?;
        }
        let conn = Connection::open(db_path)
            .map_err(|e| format!("Could not open index db: {e}"))?;
        if let Some(key) = key {
            let hex = to_hex_key(key);
            conn.execute_batch(&format!("PRAGMA key = \"x'{hex}'\";"))
                .map_err(|e| format!("Could not key index db: {e}"))?;
        }
        Self::from_conn(conn, vault_root)
    }

    /// Builds an index over an already-open connection (used by tests).
    pub fn from_conn(conn: Connection, vault_root: &Path) -> Result<Index, String> {
        conn.pragma_update(None, "journal_mode", "WAL").ok();
        let index = Index {
            conn,
            vault_root: vault_root.to_path_buf(),
            last_scan_ms: AtomicU64::new(0),
        };
        index.ensure_schema()?;
        Ok(index)
    }

    fn ensure_schema(&self) -> Result<(), String> {
        self.conn
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);",
            )
            .map_err(|e| format!("Could not init meta table: {e}"))?;

        let stored: Option<i64> = self
            .conn
            .query_row(
                "SELECT value FROM meta WHERE key = 'schema_version'",
                [],
                |r| r.get::<_, String>(0),
            )
            .optional()
            .map_err(|e| format!("Could not read schema version: {e}"))?
            .and_then(|s| s.parse().ok());

        if stored != Some(SCHEMA_VERSION) {
            self.conn
                .execute_batch(
                    "DROP TABLE IF EXISTS notes_fts;
                     DROP TABLE IF EXISTS notes;
                     DROP TABLE IF EXISTS tags;
                     DROP TABLE IF EXISTS links;",
                )
                .map_err(|e| format!("Could not drop stale schema: {e}"))?;
            self.create_schema()?;
            self.conn
                .execute(
                    "INSERT OR REPLACE INTO meta(key, value) VALUES('schema_version', ?1)",
                    params![SCHEMA_VERSION.to_string()],
                )
                .map_err(|e| format!("Could not write schema version: {e}"))?;
        }
        Ok(())
    }

    fn create_schema(&self) -> Result<(), String> {
        self.conn
            .execute_batch(
                "CREATE TABLE notes (
                    id INTEGER PRIMARY KEY,
                    path TEXT UNIQUE NOT NULL,
                    title TEXT NOT NULL,
                    mtime INTEGER NOT NULL,
                    size INTEGER NOT NULL,
                    frontmatter TEXT
                );
                CREATE VIRTUAL TABLE notes_fts USING fts5(
                    title, body, path UNINDEXED,
                    tokenize = \"unicode61 remove_diacritics 2\"
                );
                CREATE TABLE tags (
                    note_id INTEGER NOT NULL,
                    tag TEXT NOT NULL,
                    PRIMARY KEY (note_id, tag)
                );
                CREATE TABLE links (
                    source_id INTEGER NOT NULL,
                    target_path TEXT NOT NULL
                );
                CREATE INDEX idx_tags_tag ON tags(tag);
                CREATE INDEX idx_links_target ON links(target_path);",
            )
            .map_err(|e| format!("Could not create schema: {e}"))
    }

    /// Number of indexed notes.
    pub fn note_count(&self) -> Result<i64, String> {
        self.conn
            .query_row("SELECT COUNT(*) FROM notes", [], |r| r.get(0))
            .map_err(|e| format!("Could not count notes: {e}"))
    }

    pub fn status(&self) -> Result<IndexStatus, String> {
        Ok(IndexStatus {
            note_count: self.note_count()?,
            last_scan_ms: self.last_scan_ms.load(Ordering::Relaxed),
        })
    }

    /// Upserts a note's index rows from its raw content, keeping `notes`,
    /// `notes_fts`, `tags`, and `links` consistent.
    fn upsert(&self, rel: &str, raw: &str, mtime: i64, size: i64) -> Result<(), String> {
        let ex = extract(raw);
        let title = title_of(rel);

        let existing: Option<i64> = self
            .conn
            .query_row("SELECT id FROM notes WHERE path = ?1", params![rel], |r| {
                r.get(0)
            })
            .optional()
            .map_err(|e| format!("Could not look up note: {e}"))?;

        let id = match existing {
            Some(id) => {
                self.conn
                    .execute(
                        "UPDATE notes SET title = ?2, mtime = ?3, size = ?4, frontmatter = ?5 WHERE id = ?1",
                        params![id, title, mtime, size, ex.frontmatter],
                    )
                    .map_err(|e| format!("Could not update note: {e}"))?;
                id
            }
            None => {
                self.conn
                    .execute(
                        "INSERT INTO notes(path, title, mtime, size, frontmatter) VALUES(?1, ?2, ?3, ?4, ?5)",
                        params![rel, title, mtime, size, ex.frontmatter],
                    )
                    .map_err(|e| format!("Could not insert note: {e}"))?;
                self.conn.last_insert_rowid()
            }
        };

        // FTS: delete + reinsert keyed on notes.id as the rowid.
        self.conn
            .execute("DELETE FROM notes_fts WHERE rowid = ?1", params![id])
            .map_err(|e| format!("Could not clear fts row: {e}"))?;
        self.conn
            .execute(
                "INSERT INTO notes_fts(rowid, title, body, path) VALUES(?1, ?2, ?3, ?4)",
                params![id, title, ex.body, rel],
            )
            .map_err(|e| format!("Could not write fts row: {e}"))?;

        self.conn
            .execute("DELETE FROM tags WHERE note_id = ?1", params![id])
            .map_err(|e| format!("Could not clear tags: {e}"))?;
        for tag in &ex.tags {
            self.conn
                .execute(
                    "INSERT OR IGNORE INTO tags(note_id, tag) VALUES(?1, ?2)",
                    params![id, tag],
                )
                .map_err(|e| format!("Could not write tag: {e}"))?;
        }

        self.conn
            .execute("DELETE FROM links WHERE source_id = ?1", params![id])
            .map_err(|e| format!("Could not clear links: {e}"))?;
        for link in &ex.links {
            self.conn
                .execute(
                    "INSERT INTO links(source_id, target_path) VALUES(?1, ?2)",
                    params![id, link],
                )
                .map_err(|e| format!("Could not write link: {e}"))?;
        }
        Ok(())
    }

    /// Indexes (or re-indexes) a single note given its content. `mtime`/`size`
    /// are read from disk when available, else derived from the content.
    pub fn index_file(&self, rel: &str, content: &str) -> Result<(), String> {
        let (mtime, size) = self.stat(rel).unwrap_or((now_secs(), content.len() as i64));
        self.upsert(rel, content, mtime, size)
    }

    /// Removes a single note's rows from every table.
    pub fn remove_file(&self, rel: &str) -> Result<(), String> {
        let id: Option<i64> = self
            .conn
            .query_row("SELECT id FROM notes WHERE path = ?1", params![rel], |r| {
                r.get(0)
            })
            .optional()
            .map_err(|e| format!("Could not look up note for removal: {e}"))?;
        if let Some(id) = id {
            self.delete_by_id(id)?;
        }
        Ok(())
    }

    /// Removes every note whose path is `prefix` or lives under `prefix/`
    /// (used when a folder is trashed).
    pub fn remove_prefix(&self, prefix: &str) -> Result<(), String> {
        let like = format!("{}/%", prefix.trim_end_matches('/'));
        let ids: Vec<i64> = {
            let mut stmt = self
                .conn
                .prepare("SELECT id FROM notes WHERE path = ?1 OR path LIKE ?2")
                .map_err(|e| format!("Could not query prefix: {e}"))?;
            let rows = stmt
                .query_map(params![prefix, like], |r| r.get(0))
                .map_err(|e| format!("Could not query prefix: {e}"))?;
            rows.filter_map(|r| r.ok()).collect()
        };
        for id in ids {
            self.delete_by_id(id)?;
        }
        Ok(())
    }

    fn delete_by_id(&self, id: i64) -> Result<(), String> {
        self.conn
            .execute("DELETE FROM notes WHERE id = ?1", params![id])
            .map_err(|e| format!("Could not delete note: {e}"))?;
        self.conn
            .execute("DELETE FROM notes_fts WHERE rowid = ?1", params![id])
            .map_err(|e| format!("Could not delete fts row: {e}"))?;
        self.conn
            .execute("DELETE FROM tags WHERE note_id = ?1", params![id])
            .map_err(|e| format!("Could not delete tags: {e}"))?;
        self.conn
            .execute("DELETE FROM links WHERE source_id = ?1", params![id])
            .map_err(|e| format!("Could not delete links: {e}"))?;
        Ok(())
    }

    /// Updates paths for a rename/move. Handles both a single file and a folder
    /// (prefix) rename, updating `notes` and the `notes_fts` path column.
    pub fn rename(&self, old_rel: &str, new_rel: &str) -> Result<(), String> {
        let old = old_rel.trim_end_matches('/');
        let new = new_rel.trim_end_matches('/');
        let like = format!("{old}/%");
        let rows: Vec<(i64, String)> = {
            let mut stmt = self
                .conn
                .prepare("SELECT id, path FROM notes WHERE path = ?1 OR path LIKE ?2")
                .map_err(|e| format!("Could not query rename set: {e}"))?;
            let mapped = stmt
                .query_map(params![old, like], |r| Ok((r.get(0)?, r.get(1)?)))
                .map_err(|e| format!("Could not query rename set: {e}"))?;
            mapped.filter_map(|r| r.ok()).collect()
        };
        for (id, path) in rows {
            let new_path = if path == old {
                new.to_string()
            } else {
                format!("{new}{}", &path[old.len()..])
            };
            let title = title_of(&new_path);
            self.conn
                .execute(
                    "UPDATE notes SET path = ?2, title = ?3 WHERE id = ?1",
                    params![id, new_path, title],
                )
                .map_err(|e| format!("Could not update note path: {e}"))?;
            self.conn
                .execute(
                    "UPDATE notes_fts SET path = ?2, title = ?3 WHERE rowid = ?1",
                    params![id, new_path, title],
                )
                .map_err(|e| format!("Could not update fts path: {e}"))?;
        }
        Ok(())
    }

    /// Full rescan: upsert changed files (by mtime+size), delete vanished ones.
    /// Returns the number of files (re)indexed this pass — 0 means everything
    /// was already up to date. Skips hidden dirs/files and non-`.md` files.
    pub fn full_scan(&self) -> Result<usize, String> {
        let started = std::time::Instant::now();
        let root = self.vault_root.clone();

        let mut existing: HashMap<String, (i64, i64)> = HashMap::new();
        {
            let mut stmt = self
                .conn
                .prepare("SELECT path, mtime, size FROM notes")
                .map_err(|e| format!("Could not read existing notes: {e}"))?;
            let rows = stmt
                .query_map([], |r| {
                    Ok((r.get::<_, String>(0)?, (r.get::<_, i64>(1)?, r.get::<_, i64>(2)?)))
                })
                .map_err(|e| format!("Could not read existing notes: {e}"))?;
            for row in rows {
                let (path, meta) = row.map_err(|e| format!("Could not read note row: {e}"))?;
                existing.insert(path, meta);
            }
        }

        let mut seen: HashSet<String> = HashSet::new();
        let mut indexed = 0usize;

        for entry in WalkDir::new(&root)
            .min_depth(1)
            .into_iter()
            .filter_entry(|e| !e.file_name().to_string_lossy().starts_with('.'))
        {
            let entry = entry.map_err(|e| format!("Error scanning vault: {e}"))?;
            if entry.file_type().is_dir() || !is_markdown(entry.path()) {
                continue;
            }
            let rel = match to_rel_string(&root, entry.path()) {
                Some(r) => r,
                None => continue,
            };
            let meta = entry
                .metadata()
                .map_err(|e| format!("Could not stat '{rel}': {e}"))?;
            let mtime = meta
                .modified()
                .ok()
                .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            let size = meta.len() as i64;
            seen.insert(rel.clone());

            if existing.get(&rel) == Some(&(mtime, size)) {
                continue; // unchanged
            }
            let content = std::fs::read_to_string(entry.path())
                .map_err(|e| format!("Could not read '{rel}': {e}"))?;
            self.upsert(&rel, &content, mtime, size)?;
            indexed += 1;
        }

        for path in existing.keys() {
            if !seen.contains(path) {
                self.remove_file(path)?;
            }
        }

        self.last_scan_ms
            .store(started.elapsed().as_millis() as u64, Ordering::Relaxed);
        Ok(indexed)
    }

    /// Sorted tag list for one note id.
    fn tags_for(&self, id: i64) -> Result<Vec<String>, String> {
        let mut stmt = self
            .conn
            .prepare("SELECT tag FROM tags WHERE note_id = ?1 ORDER BY tag")
            .map_err(|e| format!("Could not query tags: {e}"))?;
        let rows = stmt
            .query_map([id], |r| r.get::<_, String>(0))
            .map_err(|e| format!("Could not query tags: {e}"))?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    /// Full-text + tag search. Parses `tag:` filters out of `query` (ANDed);
    /// the remainder becomes a sanitized FTS5 MATCH with title weighted over
    /// body. A tag-only query (no text) returns tagged notes sorted by title.
    /// An empty query with no tag filters returns nothing.
    pub fn search(&self, query: &str, limit: u32) -> Result<Vec<SearchHit>, String> {
        let (terms, tags) = parse_query(query);
        let match_expr = build_match(&terms);
        if match_expr.is_none() && tags.is_empty() {
            return Ok(Vec::new());
        }
        let limit = limit as i64;

        // Raw rows: (id, path, title, snippet_or_empty, score, body_prefix, body_len).
        let raw: Vec<(i64, String, String, String, f64, String, i64)> = if let Some(m) = match_expr
        {
            let mut sql = String::from(
                "SELECT n.id, n.path, n.title, \
                 snippet(notes_fts, 1, '<mark>', '</mark>', '…', 12), \
                 bm25(notes_fts, 5.0, 1.0), \
                 substr(notes_fts.body, 1, 100), length(notes_fts.body) \
                 FROM notes_fts JOIN notes n ON n.id = notes_fts.rowid \
                 WHERE notes_fts MATCH ?",
            );
            let mut binds: Vec<Value> = vec![Value::Text(m)];
            if !tags.is_empty() {
                let placeholders = vec!["?"; tags.len()].join(",");
                sql.push_str(&format!(
                    " AND n.id IN (SELECT note_id FROM tags WHERE tag IN ({placeholders}) \
                     GROUP BY note_id HAVING COUNT(DISTINCT tag) = ?)"
                ));
                for t in &tags {
                    binds.push(Value::Text(t.clone()));
                }
                binds.push(Value::Integer(tags.len() as i64));
            }
            sql.push_str(" ORDER BY bm25(notes_fts, 5.0, 1.0) LIMIT ?");
            binds.push(Value::Integer(limit));

            let mut stmt = self
                .conn
                .prepare(&sql)
                .map_err(|e| format!("Could not prepare search: {e}"))?;
            let rows = stmt
                .query_map(params_from_iter(binds), |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                        r.get(6)?,
                    ))
                })
                .map_err(|e| format!("Could not run search: {e}"))?;
            rows.collect::<Result<_, _>>()
                .map_err(|e| format!("Could not read search row: {e}"))?
        } else {
            // Tag-only: no MATCH, so pull the body prefix straight from the FTS row.
            let placeholders = vec!["?"; tags.len()].join(",");
            let sql = format!(
                "SELECT n.id, n.path, n.title, '', 0.0, \
                 substr(b.body, 1, 100), length(b.body) \
                 FROM notes n JOIN notes_fts b ON b.rowid = n.id \
                 WHERE n.id IN (SELECT note_id FROM tags WHERE tag IN ({placeholders}) \
                 GROUP BY note_id HAVING COUNT(DISTINCT tag) = ?) \
                 ORDER BY n.title COLLATE NOCASE ASC LIMIT ?"
            );
            let mut binds: Vec<Value> = tags.iter().map(|t| Value::Text(t.clone())).collect();
            binds.push(Value::Integer(tags.len() as i64));
            binds.push(Value::Integer(limit));

            let mut stmt = self
                .conn
                .prepare(&sql)
                .map_err(|e| format!("Could not prepare tag search: {e}"))?;
            let rows = stmt
                .query_map(params_from_iter(binds), |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                        r.get(6)?,
                    ))
                })
                .map_err(|e| format!("Could not run tag search: {e}"))?;
            rows.collect::<Result<_, _>>()
                .map_err(|e| format!("Could not read tag search row: {e}"))?
        };

        let mut hits = Vec::with_capacity(raw.len());
        for (id, path, title, snip, score, body_prefix, body_len) in raw {
            // A snippet with no <mark> means the match was title-only (or a
            // tag-only query): fall back to the leading body text, no marks.
            let snippet = if snip.contains("<mark>") {
                snip
            } else {
                let mut s = body_prefix;
                if body_len > 100 {
                    s.push('…');
                }
                s
            };
            let tags = self.tags_for(id)?;
            hits.push(SearchHit {
                path,
                title,
                snippet,
                score,
                tags,
            });
        }
        Ok(hits)
    }

    /// Resolves a wikilink target `name` to a vault-relative note path.
    ///
    /// `name` is a `[[wikilink]]` inner (either `Note Name` or
    /// `folder/Note Name`, with any `.md` suffix ignored). Matching is by
    /// **title** — the filename without `.md` — case-insensitively, anywhere in
    /// the vault. Among equal-title candidates the winner is chosen by:
    ///   1. folder match: a path equal to (or ending with `/`) the full
    ///      `name`, so `folder/Note` prefers the note actually in `folder/`;
    ///   2. shallower path (fewer `/` segments);
    ///   3. lexicographic path, purely for determinism.
    /// Returns `None` when no title matches.
    pub fn resolve(&self, name: &str) -> Result<Option<String>, String> {
        let cleaned = strip_md_suffix(name.trim());
        if cleaned.is_empty() {
            return Ok(None);
        }
        // The bare title to match is the final path segment of the target.
        let title = cleaned.rsplit('/').next().unwrap_or(cleaned);

        let mut stmt = self
            .conn
            .prepare("SELECT path FROM notes WHERE title = ?1 COLLATE NOCASE")
            .map_err(|e| format!("Could not prepare resolve: {e}"))?;
        let mut paths: Vec<String> = stmt
            .query_map(params![title], |r| r.get::<_, String>(0))
            .map_err(|e| format!("Could not run resolve: {e}"))?
            .filter_map(|r| r.ok())
            .collect();

        // Suffix (with .md) that a folder-qualified target should end with.
        let want = format!("{}.md", cleaned.to_lowercase());
        paths.sort_by(|a, b| {
            let rank = |p: &str| -> (bool, usize, String) {
                let low = p.to_lowercase();
                let folder_hit = low == want || low.ends_with(&format!("/{want}"));
                // `false` sorts before `true`, so negate to put hits first.
                (!folder_hit, p.matches('/').count(), low)
            };
            rank(a).cmp(&rank(b))
        });
        Ok(paths.into_iter().next())
    }

    /// All notes as `{ path, title, mtime }`, newest first — powers the quick
    /// switcher (fuzzy matching happens on the frontend).
    pub fn list_notes(&self) -> Result<Vec<NoteRef>, String> {
        let mut stmt = self
            .conn
            .prepare("SELECT path, title, mtime FROM notes ORDER BY mtime DESC")
            .map_err(|e| format!("Could not prepare list: {e}"))?;
        let rows = stmt
            .query_map([], |r| {
                Ok(NoteRef {
                    path: r.get(0)?,
                    title: r.get(1)?,
                    mtime: r.get(2)?,
                })
            })
            .map_err(|e| format!("Could not list notes: {e}"))?;
        rows.collect::<Result<_, _>>()
            .map_err(|e| format!("Could not read note row: {e}"))
    }

    /// Every distinct tag with its note count, sorted alphabetically
    /// (case-insensitive). Powers the Tags panel. The `tags` PK `(note_id, tag)`
    /// already guarantees a note is counted at most once per tag.
    pub fn list_tags(&self) -> Result<Vec<TagCount>, String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT tag, COUNT(*) FROM tags \
                 GROUP BY tag ORDER BY tag COLLATE NOCASE ASC, tag ASC",
            )
            .map_err(|e| format!("Could not prepare tag list: {e}"))?;
        let rows = stmt
            .query_map([], |r| {
                Ok(TagCount {
                    tag: r.get(0)?,
                    count: r.get::<_, i64>(1)? as u32,
                })
            })
            .map_err(|e| format!("Could not list tags: {e}"))?;
        rows.collect::<Result<_, _>>()
            .map_err(|e| format!("Could not read tag row: {e}"))
    }

    /// Notes carrying exactly `tag` (an exact, case-sensitive match against the
    /// stored tag), returned in the `SearchHit` shape sorted by title. The
    /// snippet is the leading ~100 chars of body with no `<mark>` marks. This
    /// bypasses the search query parser so tags containing whitespace or other
    /// query punctuation round-trip faithfully.
    pub fn notes_by_tag(&self, tag: &str, limit: u32) -> Result<Vec<SearchHit>, String> {
        let limit = limit as i64;
        let mut stmt = self
            .conn
            .prepare(
                "SELECT n.id, n.path, n.title, substr(b.body, 1, 100), length(b.body) \
                 FROM notes n \
                 JOIN notes_fts b ON b.rowid = n.id \
                 JOIN tags t ON t.note_id = n.id \
                 WHERE t.tag = ?1 \
                 ORDER BY n.title COLLATE NOCASE ASC LIMIT ?2",
            )
            .map_err(|e| format!("Could not prepare notes-by-tag: {e}"))?;
        let raw: Vec<(i64, String, String, String, i64)> = stmt
            .query_map(params![tag, limit], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })
            .map_err(|e| format!("Could not run notes-by-tag: {e}"))?
            .collect::<Result<_, _>>()
            .map_err(|e| format!("Could not read notes-by-tag row: {e}"))?;

        let mut hits = Vec::with_capacity(raw.len());
        for (id, path, title, mut body_prefix, body_len) in raw {
            if body_len > 100 {
                body_prefix.push('…');
            }
            let tags = self.tags_for(id)?;
            hits.push(SearchHit {
                path,
                title,
                snippet: body_prefix,
                score: 0.0,
                tags,
            });
        }
        Ok(hits)
    }

    /// Returns `(outgoing, backlinks)` for a note at `rel`.
    ///
    /// `outgoing` is this note's `[[wikilinks]]` resolved to existing note
    /// paths (unresolved targets are dropped). `backlinks` are the paths of
    /// other notes whose wikilinks resolve to `rel`. Both are deduped and
    /// sorted. Powers the AI `note_links` tool.
    pub fn links_for(&self, rel: &str) -> Result<(Vec<String>, Vec<String>), String> {
        // Every (source_path, target_path) pair in the vault.
        let pairs: Vec<(String, String)> = {
            let mut stmt = self
                .conn
                .prepare(
                    "SELECT n.path, l.target_path FROM links l JOIN notes n ON n.id = l.source_id",
                )
                .map_err(|e| format!("Could not prepare links query: {e}"))?;
            let rows = stmt
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
                .map_err(|e| format!("Could not query links: {e}"))?;
            rows.filter_map(|r| r.ok()).collect()
        };

        let mut outgoing: Vec<String> = Vec::new();
        let mut back: Vec<String> = Vec::new();
        let mut seen_out: HashSet<String> = HashSet::new();
        let mut seen_back: HashSet<String> = HashSet::new();
        for (source, target) in &pairs {
            if source == rel {
                if let Some(resolved) = self.resolve(target)? {
                    if resolved != rel && seen_out.insert(resolved.clone()) {
                        outgoing.push(resolved);
                    }
                }
            }
            if source != rel {
                if let Some(resolved) = self.resolve(target)? {
                    if resolved == rel && seen_back.insert(source.clone()) {
                        back.push(source.clone());
                    }
                }
            }
        }
        outgoing.sort();
        back.sort();
        Ok((outgoing, back))
    }

    /// Reads `(mtime_secs, size)` for a vault-relative note, if it exists.
    fn stat(&self, rel: &str) -> Option<(i64, i64)> {
        let abs = self.vault_root.join(rel);
        let meta = std::fs::metadata(&abs).ok()?;
        let mtime = meta
            .modified()
            .ok()
            .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        Some((mtime, meta.len() as i64))
    }
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Vault-scoped setup
// ---------------------------------------------------------------------------

/// Lowercase hex of a 32-byte key, for the SQLCipher `PRAGMA key = "x'…'"`
/// raw-key form. (A local copy so the plain-build index module doesn't depend on
/// the encryption umbrella's `crypto::to_hex`.)
fn to_hex_key(key: &[u8; 32]) -> String {
    let mut s = String::with_capacity(64);
    for b in key {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// FNV-1a 64-bit hash of a string, hex-encoded. Stable across runs so a vault
/// always maps to the same db filename.
pub(crate) fn hash_path(p: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in p.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{h:016x}")
}

/// Resolves the database path for a vault under `app_data_dir()/indexes/`.
fn db_path_for(app: &tauri::AppHandle, vault_root: &Path) -> Result<PathBuf, String> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("Could not resolve app data dir: {e}"))?;
    let name = format!("{}.db", hash_path(&vault_root.to_string_lossy()));
    Ok(dir.join("indexes").join(name))
}

/// Opens (or creates) a SQLCipher-**keyed** index for an encrypted-files vault
/// at the standard `app_data/indexes/<hash>.db` location. Unlike
/// [`init_for_vault`], this only opens the DB — the encrypted-files handle
/// populates and watches it, since only the handle can decrypt names/content.
#[cfg(feature = "provider-encrypted-files")]
pub(crate) fn open_keyed_index(
    app: &tauri::AppHandle,
    vault_root: &Path,
    key: &[u8; 32],
) -> Result<Index, String> {
    let db_path = db_path_for(app, vault_root)?;
    Index::open_keyed(&db_path, vault_root, Some(key))
}

/// Opens the index for `vault_root`, starts a fresh file watcher, and kicks off
/// a background full scan that emits `index-ready` when done. Any previous
/// watcher is dropped (stopping it). Called on startup and on `set_vault`.
pub fn init_for_vault(
    app: &tauri::AppHandle,
    state: &AppState,
    vault_root: &Path,
) -> Result<(), String> {
    // Let the asset protocol serve files from inside this vault so the editor's
    // `convertFileSrc()` URLs for vault-relative images resolve at runtime. When
    // the user switches vaults mid-session the scope only grows (old vault stays
    // allowed until restart); that's acceptable for a local single-user app.
    if let Err(e) = app.asset_protocol_scope().allow_directory(vault_root, true) {
        eprintln!(
            "Could not grant asset-protocol access to {}: {e}",
            vault_root.display()
        );
    }

    let db_path = db_path_for(app, vault_root)?;
    let index = Index::open(&db_path, vault_root)?;
    *state.index.lock().unwrap() = Some(index);

    // Install the plain storage handle every command dispatches through.
    *state.active.lock().unwrap() =
        Some(Box::new(crate::providers::plain::PlainHandle::new(vault_root)));

    // Replace the watcher (dropping the old one stops it).
    let handle = crate::watcher::start_watcher(
        app.clone(),
        state.index.clone(),
        state.recent_writes.clone(),
        vault_root.to_path_buf(),
    )?;
    *state.watcher.lock().unwrap() = Some(handle);

    // Full scan off the UI path; hold the index lock only for the scan.
    let index_arc = state.index.clone();
    let app = app.clone();
    std::thread::spawn(move || {
        if let Ok(guard) = index_arc.lock() {
            if let Some(idx) = guard.as_ref() {
                let _ = idx.full_scan();
            }
        }
        let _ = app.emit("index-ready", ());
    });
    Ok(())
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

/// True when the active vault handle owns its own index (encrypted-db), so the
/// search commands below must dispatch through the handle rather than
/// `state.index` (which is `None` for such vaults).
fn active_owns_index(state: &AppState) -> bool {
    state
        .active
        .lock()
        .unwrap()
        .as_deref()
        .map(|h| h.owns_index())
        .unwrap_or(false)
}

/// Error message for a search/index operation attempted with no usable index —
/// distinguishes a locked encrypted vault (nothing open at all) from a plain
/// vault whose index just isn't ready.
fn no_index_error(state: &AppState) -> String {
    if state.active.lock().unwrap().is_none() {
        "The vault is locked — unlock it to continue.".to_string()
    } else {
        "No vault is indexed".to_string()
    }
}

// ---------------------------------------------------------------------------
// Shared index dispatch (one branching point for both the commands below and
// the AI tools in `crate::ai::tools`, so search behaves identically on a plain
// vault, an encrypted-files vault (separate keyed index), and a self-indexing
// encrypted-db vault).
// ---------------------------------------------------------------------------

/// Full-text + tag search through whichever index the active vault uses.
pub(crate) fn dispatch_search(
    state: &AppState,
    query: &str,
    limit: u32,
) -> Result<Vec<SearchHit>, String> {
    if active_owns_index(state) {
        let g = state.active.lock().unwrap();
        return g.as_deref().unwrap().search(query, limit);
    }
    let guard = state.index.lock().unwrap();
    let index = guard.as_ref().ok_or_else(|| no_index_error(state))?;
    index.search(query, limit)
}

/// All notes (newest first) through the active vault's index.
pub(crate) fn dispatch_list_notes(state: &AppState) -> Result<Vec<NoteRef>, String> {
    if active_owns_index(state) {
        let g = state.active.lock().unwrap();
        return g.as_deref().unwrap().list_notes();
    }
    let guard = state.index.lock().unwrap();
    let index = guard.as_ref().ok_or_else(|| no_index_error(state))?;
    index.list_notes()
}

/// Every distinct tag with its note count.
pub(crate) fn dispatch_list_tags(state: &AppState) -> Result<Vec<TagCount>, String> {
    if active_owns_index(state) {
        let g = state.active.lock().unwrap();
        return g.as_deref().unwrap().list_tags();
    }
    let guard = state.index.lock().unwrap();
    let index = guard.as_ref().ok_or_else(|| no_index_error(state))?;
    index.list_tags()
}

/// Notes carrying exactly `tag`.
pub(crate) fn dispatch_notes_by_tag(
    state: &AppState,
    tag: &str,
    limit: u32,
) -> Result<Vec<SearchHit>, String> {
    if active_owns_index(state) {
        let g = state.active.lock().unwrap();
        return g.as_deref().unwrap().notes_by_tag(tag, limit);
    }
    let guard = state.index.lock().unwrap();
    let index = guard.as_ref().ok_or_else(|| no_index_error(state))?;
    index.notes_by_tag(tag, limit)
}

/// `(outgoing, backlinks)` for `rel`. A self-indexing container has no
/// wikilink graph exposed, so it reports empty; plain/encrypted-files use the
/// FTS `links` table.
pub(crate) fn dispatch_links_for(
    state: &AppState,
    rel: &str,
) -> Result<(Vec<String>, Vec<String>), String> {
    if active_owns_index(state) {
        return Ok((Vec::new(), Vec::new()));
    }
    let guard = state.index.lock().unwrap();
    let index = guard.as_ref().ok_or_else(|| no_index_error(state))?;
    index.links_for(rel)
}

/// Forces a full rescan of the current vault. Returns the number of files
/// (re)indexed.
#[tauri::command]
pub async fn reindex_vault(state: tauri::State<'_, AppState>) -> Result<usize, String> {
    // A handle that owns reindexing (encrypted-db owns the index; encrypted-files
    // owns the decrypt-scan that populates the separate index) drives it itself.
    let owns_reindex = state
        .active
        .lock()
        .unwrap()
        .as_deref()
        .map(|h| h.owns_reindex())
        .unwrap_or(false);
    if owns_reindex {
        let g = state.active.lock().unwrap();
        return g.as_deref().unwrap().reindex();
    }
    let guard = state.index.lock().unwrap();
    let index = guard.as_ref().ok_or("No vault is indexed")?;
    index.full_scan()
}

/// Returns the current index status (note count + last full-scan duration ms).
#[tauri::command]
pub async fn index_status(state: tauri::State<'_, AppState>) -> Result<IndexStatus, String> {
    if active_owns_index(&state) {
        let g = state.active.lock().unwrap();
        return g.as_deref().unwrap().status();
    }
    let guard = state.index.lock().unwrap();
    let index = guard.as_ref().ok_or("No vault is indexed")?;
    index.status()
}

/// Full-text + tag search over the current vault. `limit` defaults to 50.
#[tauri::command]
pub async fn search_notes(
    state: tauri::State<'_, AppState>,
    query: String,
    limit: Option<u32>,
) -> Result<Vec<SearchHit>, String> {
    dispatch_search(&state, &query, limit.unwrap_or(50))
}

/// Lists all indexed notes, newest first, for the quick switcher.
#[tauri::command]
pub async fn list_notes(state: tauri::State<'_, AppState>) -> Result<Vec<NoteRef>, String> {
    dispatch_list_notes(&state)
}

/// Lists every distinct tag with its note count, sorted alphabetically.
#[tauri::command]
pub async fn list_tags(state: tauri::State<'_, AppState>) -> Result<Vec<TagCount>, String> {
    dispatch_list_tags(&state)
}

/// Lists notes carrying exactly `tag`, sorted by title. `limit` defaults to 500.
#[tauri::command]
pub async fn notes_by_tag(
    state: tauri::State<'_, AppState>,
    tag: String,
    limit: Option<u32>,
) -> Result<Vec<SearchHit>, String> {
    dispatch_notes_by_tag(&state, &tag, limit.unwrap_or(500))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn make_temp_dir(tag: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "jaynotes-index-test-{tag}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Opens an in-memory index rooted at `vault`.
    fn mem_index(vault: &Path) -> Index {
        Index::from_conn(Connection::open_in_memory().unwrap(), vault).unwrap()
    }

    // ---- extraction ----

    #[test]
    fn split_frontmatter_matches_ts_behavior() {
        let (fm, body) = split_frontmatter("---\ntags: [a]\n---\n# Hi\n");
        assert_eq!(fm.as_deref(), Some("---\ntags: [a]\n---\n"));
        assert_eq!(body, "# Hi\n");

        // Empty block.
        let (fm, body) = split_frontmatter("---\n---\nbody");
        assert_eq!(fm.as_deref(), Some("---\n---\n"));
        assert_eq!(body, "body");

        // No frontmatter.
        let (fm, body) = split_frontmatter("# Just a heading\n");
        assert!(fm.is_none());
        assert_eq!(body, "# Just a heading\n");

        // Unterminated opening fence stays in the body.
        let (fm, _) = split_frontmatter("---\nno close here\n");
        assert!(fm.is_none());
    }

    #[test]
    fn frontmatter_tag_parsing_variants() {
        // Flow list.
        assert_eq!(
            frontmatter_tags("---\ntags: [alpha, beta]\n---\n"),
            vec!["alpha", "beta"]
        );
        // Block list with leading '#'.
        assert_eq!(
            frontmatter_tags("---\ntags:\n  - '#one'\n  - two\n---\n"),
            vec!["one", "two"]
        );
        // Single whitespace/comma separated string.
        assert_eq!(
            frontmatter_tags("---\ntags: red, green blue\n---\n"),
            vec!["red", "green", "blue"]
        );
        // No tags key.
        assert!(frontmatter_tags("---\ntitle: Hello\n---\n").is_empty());
    }

    #[test]
    fn inline_tag_extraction_with_code_exclusion() {
        let body = "\
Intro #alpha and #beta/gamma here.
Not a tag: foo#bar or #123 or email a#b.

```
#fenced should be ignored
```

Inline `#code` ignored but #delta counts.
Duplicate #alpha stays once.";
        let tags = extract_inline_tags(body);
        assert_eq!(tags, vec!["alpha", "beta/gamma", "delta"]);
    }

    #[test]
    fn wikilink_extraction() {
        let body = "See [[Note One]] and [[folder/Note Two|alias]] and [[Note One]].\n\
                    Broken [[unterminated and [[  Spaced  ]] end.";
        let links = extract_links(body);
        assert_eq!(links, vec!["Note One", "folder/Note Two", "Spaced"]);
    }

    #[test]
    fn extract_merges_frontmatter_and_inline_tags() {
        let ex = extract("---\ntags: [fm]\n---\nBody with #inline and #fm again.\n");
        assert_eq!(ex.tags, vec!["fm", "inline"]);
        assert_eq!(ex.body, "Body with #inline and #fm again.\n");
    }

    // ---- index ----

    fn write(root: &Path, rel: &str, content: &str) {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, content).unwrap();
    }

    #[test]
    fn full_scan_indexes_notes_tags_links_and_fts() {
        let root = make_temp_dir("scan");
        write(&root, "one.md", "---\ntags: [project]\n---\n# One\nLinks to [[two]] #inline");
        write(&root, "sub/two.md", "# Two\nThe quick brown fox.");
        write(&root, "notes.txt", "not markdown");
        write(&root, ".hidden/secret.md", "# hidden");

        let index = mem_index(&root);
        let n = index.full_scan().unwrap();
        assert_eq!(n, 2, "two .md files indexed (txt + hidden skipped)");
        assert_eq!(index.note_count().unwrap(), 2);

        // Tags: frontmatter + inline.
        let tags: Vec<String> = {
            let mut stmt = index
                .conn
                .prepare("SELECT tag FROM tags ORDER BY tag")
                .unwrap();
            stmt.query_map([], |r| r.get(0))
                .unwrap()
                .filter_map(|r| r.ok())
                .collect()
        };
        assert_eq!(tags, vec!["inline", "project"]);

        // Links.
        let link: String = index
            .conn
            .query_row("SELECT target_path FROM links", [], |r| r.get(0))
            .unwrap();
        assert_eq!(link, "two");

        // FTS smoke query.
        let hit: i64 = index
            .conn
            .query_row(
                "SELECT COUNT(*) FROM notes_fts WHERE notes_fts MATCH 'brown'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(hit, 1);

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn incremental_scan_skips_unchanged() {
        let root = make_temp_dir("incremental");
        write(&root, "a.md", "# A");
        write(&root, "b.md", "# B");
        let index = mem_index(&root);

        assert_eq!(index.full_scan().unwrap(), 2, "first scan indexes all");
        assert_eq!(index.full_scan().unwrap(), 0, "second scan skips unchanged");

        // Modify one file (also bump mtime so size-equal edits are caught).
        std::thread::sleep(std::time::Duration::from_millis(1100));
        write(&root, "a.md", "# A changed");
        assert_eq!(index.full_scan().unwrap(), 1, "only the changed file reindexes");

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn removal_cleans_fts_tags_and_links() {
        let root = make_temp_dir("removal");
        let index = mem_index(&root);
        index
            .index_file("gone.md", "---\ntags: [x]\n---\nBody [[target]] #y")
            .unwrap();
        assert_eq!(index.note_count().unwrap(), 1);

        index.remove_file("gone.md").unwrap();
        assert_eq!(index.note_count().unwrap(), 0);
        let tags: i64 = index
            .conn
            .query_row("SELECT COUNT(*) FROM tags", [], |r| r.get(0))
            .unwrap();
        let links: i64 = index
            .conn
            .query_row("SELECT COUNT(*) FROM links", [], |r| r.get(0))
            .unwrap();
        let fts: i64 = index
            .conn
            .query_row("SELECT COUNT(*) FROM notes_fts", [], |r| r.get(0))
            .unwrap();
        assert_eq!((tags, links, fts), (0, 0, 0));

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn folder_rename_updates_path_prefixes() {
        let root = make_temp_dir("rename");
        let index = mem_index(&root);
        index.index_file("old/a.md", "# A").unwrap();
        index.index_file("old/sub/b.md", "# B").unwrap();
        index.index_file("keep.md", "# Keep").unwrap();

        index.rename("old", "new").unwrap();

        let mut paths: Vec<String> = {
            let mut stmt = index.conn.prepare("SELECT path FROM notes").unwrap();
            stmt.query_map([], |r| r.get(0))
                .unwrap()
                .filter_map(|r| r.ok())
                .collect()
        };
        paths.sort();
        assert_eq!(paths, vec!["keep.md", "new/a.md", "new/sub/b.md"]);

        // FTS path column moved too.
        let fts_path: String = index
            .conn
            .query_row(
                "SELECT path FROM notes_fts WHERE title = 'a'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(fts_path, "new/a.md");

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn single_file_rename_updates_path() {
        let root = make_temp_dir("rename-file");
        let index = mem_index(&root);
        index.index_file("draft.md", "# Draft").unwrap();
        index.rename("draft.md", "final.md").unwrap();

        let (path, title): (String, String) = index
            .conn
            .query_row("SELECT path, title FROM notes", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!(path, "final.md");
        assert_eq!(title, "final");

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn remove_prefix_deletes_folder_subtree() {
        let root = make_temp_dir("remove-prefix");
        let index = mem_index(&root);
        index.index_file("dir/a.md", "# A").unwrap();
        index.index_file("dir/sub/b.md", "# B").unwrap();
        index.index_file("other.md", "# Other").unwrap();

        index.remove_prefix("dir").unwrap();
        assert_eq!(index.note_count().unwrap(), 1);
        let remaining: String = index
            .conn
            .query_row("SELECT path FROM notes", [], |r| r.get(0))
            .unwrap();
        assert_eq!(remaining, "other.md");

        std::fs::remove_dir_all(&root).ok();
    }

    // ---- query parsing ----

    #[test]
    fn parse_query_splits_tags_and_terms() {
        let (terms, tags) = parse_query("hello tag:project world tag:#idea");
        assert_eq!(terms, vec!["hello", "world"]);
        assert_eq!(tags, vec!["project", "idea"]);

        let (terms, tags) = parse_query("   tag:only   ");
        assert!(terms.is_empty());
        assert_eq!(tags, vec!["only"]);
    }

    #[test]
    fn build_match_escapes_and_prefixes() {
        // Last term gets the prefix star; quotes are doubled.
        assert_eq!(
            build_match(&["foo".into(), "bar".into()]).as_deref(),
            Some("\"foo\" \"bar\"*")
        );
        assert_eq!(
            build_match(&["qu\"ote".into()]).as_deref(),
            Some("\"qu\"\"ote\"*")
        );
        // A lone star carries no alphanumerics and is dropped entirely.
        assert!(build_match(&["*".into()]).is_none());
        assert!(build_match(&[]).is_none());
    }

    // ---- search ----

    #[test]
    fn search_tag_filter_uses_and_semantics() {
        let root = make_temp_dir("search-tags");
        let index = mem_index(&root);
        index
            .index_file("a.md", "---\ntags: [alpha, beta]\n---\nApple body")
            .unwrap();
        index
            .index_file("b.md", "---\ntags: [alpha]\n---\nBanana body")
            .unwrap();

        // Both tags required: only a.md qualifies.
        let hits = index.search("tag:alpha tag:beta", 50).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].path, "a.md");
        assert!(hits[0].tags.contains(&"beta".to_string()));

        // Single tag matches both, sorted by title.
        let hits = index.search("tag:alpha", 50).unwrap();
        assert_eq!(
            hits.iter().map(|h| h.path.as_str()).collect::<Vec<_>>(),
            vec!["a.md", "b.md"]
        );

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn search_prefix_matches_partial_word() {
        let root = make_temp_dir("search-prefix");
        let index = mem_index(&root);
        index.index_file("n.md", "# Note\nThe programmer wrote code.").unwrap();

        // "progr" should prefix-match "programmer".
        let hits = index.search("progr", 50).unwrap();
        assert_eq!(hits.len(), 1);
        assert!(hits[0].snippet.contains("<mark>"));

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn search_weights_title_over_body() {
        let root = make_temp_dir("search-weight");
        let index = mem_index(&root);
        // One note has "meeting" in the title, another only in the body.
        index.index_file("meeting.md", "# meeting\nUnrelated content here.").unwrap();
        index
            .index_file("other.md", "# other\nWe had a meeting yesterday.")
            .unwrap();

        let hits = index.search("meeting", 50).unwrap();
        assert_eq!(hits.len(), 2);
        // bm25 is lower (better) for the title hit; it must rank first.
        assert_eq!(hits[0].path, "meeting.md");
        assert!(hits[0].score <= hits[1].score);

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn search_snippet_marks_body_match() {
        let root = make_temp_dir("search-snip");
        let index = mem_index(&root);
        index
            .index_file("n.md", "# Title\nA quick brown fox jumps over.")
            .unwrap();

        let hits = index.search("brown", 50).unwrap();
        assert_eq!(hits.len(), 1);
        assert!(hits[0].snippet.contains("<mark>brown</mark>"));

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn search_title_only_match_falls_back_to_body_prefix() {
        let root = make_temp_dir("search-fallback");
        let index = mem_index(&root);
        index
            .index_file("Zebra.md", "The body never mentions the search term.")
            .unwrap();

        // Matches the title only; snippet should be plain body text, no marks.
        let hits = index.search("zebra", 50).unwrap();
        assert_eq!(hits.len(), 1);
        assert!(!hits[0].snippet.contains("<mark>"));
        assert!(hits[0].snippet.starts_with("The body"));

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn search_hostile_input_never_errors() {
        let root = make_temp_dir("search-hostile");
        let index = mem_index(&root);
        index.index_file("a.md", "# A\nHarmless body text.").unwrap();

        for q in [
            "\"quo\"tes",
            "foo(",
            "-bar",
            "*",
            "**",
            "( ) \"",
            "AND OR NOT",
            "tag:",
            "col:on tag:x",
            "\"",
            ")))",
        ] {
            let r = index.search(q, 50);
            assert!(r.is_ok(), "query {q:?} errored: {:?}", r.err());
        }

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn search_empty_query_returns_nothing() {
        let root = make_temp_dir("search-empty");
        let index = mem_index(&root);
        index.index_file("a.md", "# A\nBody.").unwrap();
        assert!(index.search("   ", 50).unwrap().is_empty());
        assert!(index.search("", 50).unwrap().is_empty());

        std::fs::remove_dir_all(&root).ok();
    }

    // ---- wikilink resolution ----

    #[test]
    fn resolve_exact_and_case_insensitive_match() {
        let root = make_temp_dir("resolve-exact");
        let index = mem_index(&root);
        index.upsert("Note One.md", "# One", 100, 5).unwrap();
        index.upsert("sub/Other.md", "# Other", 100, 5).unwrap();

        assert_eq!(index.resolve("Note One").unwrap().as_deref(), Some("Note One.md"));
        // Case-insensitive on the title.
        assert_eq!(index.resolve("note one").unwrap().as_deref(), Some("Note One.md"));
        // A trailing `.md` in the target is ignored.
        assert_eq!(index.resolve("Note One.md").unwrap().as_deref(), Some("Note One.md"));
        // Nested note matched by its bare title.
        assert_eq!(index.resolve("Other").unwrap().as_deref(), Some("sub/Other.md"));

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn resolve_prefers_shallower_path_on_ties() {
        let root = make_temp_dir("resolve-shallow");
        let index = mem_index(&root);
        // Three notes share the title "Topic"; the root one is shallowest.
        index.upsert("a/b/Topic.md", "# deep", 100, 5).unwrap();
        index.upsert("a/Topic.md", "# mid", 100, 5).unwrap();
        index.upsert("Topic.md", "# root", 100, 5).unwrap();

        assert_eq!(index.resolve("Topic").unwrap().as_deref(), Some("Topic.md"));

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn resolve_folder_qualified_prefers_matching_folder() {
        let root = make_temp_dir("resolve-folder");
        let index = mem_index(&root);
        index.upsert("Topic.md", "# root", 100, 5).unwrap();
        index.upsert("work/Topic.md", "# work", 100, 5).unwrap();

        // A folder-qualified target picks the note actually in that folder,
        // even though the root note is shallower.
        assert_eq!(
            index.resolve("work/Topic").unwrap().as_deref(),
            Some("work/Topic.md")
        );

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn resolve_miss_returns_none() {
        let root = make_temp_dir("resolve-miss");
        let index = mem_index(&root);
        index.upsert("Exists.md", "# e", 100, 5).unwrap();

        assert!(index.resolve("Nope").unwrap().is_none());
        assert!(index.resolve("").unwrap().is_none());
        assert!(index.resolve("   ").unwrap().is_none());

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn list_notes_sorted_by_mtime_desc() {
        let root = make_temp_dir("list-notes");
        let index = mem_index(&root);
        // upsert directly with explicit mtimes to control ordering.
        index.upsert("old.md", "# Old", 100, 5).unwrap();
        index.upsert("new.md", "# New", 300, 5).unwrap();
        index.upsert("mid.md", "# Mid", 200, 5).unwrap();

        let notes = index.list_notes().unwrap();
        assert_eq!(
            notes.iter().map(|n| n.path.as_str()).collect::<Vec<_>>(),
            vec!["new.md", "mid.md", "old.md"]
        );
        assert_eq!(notes[0].title, "new");

        std::fs::remove_dir_all(&root).ok();
    }

    // ---- tags panel ----

    #[test]
    fn list_tags_counts_and_alphabetical_order() {
        let root = make_temp_dir("list-tags");
        let index = mem_index(&root);
        index
            .index_file("a.md", "---\ntags: [project, Alpha]\n---\nBody #beta")
            .unwrap();
        index
            .index_file("b.md", "---\ntags: [project]\n---\nBody #beta")
            .unwrap();
        index.index_file("c.md", "# C\nNo tags here.").unwrap();

        let tags = index.list_tags().unwrap();
        // Case-insensitive alphabetical: Alpha, beta, project.
        assert_eq!(
            tags,
            vec![
                TagCount { tag: "Alpha".into(), count: 1 },
                TagCount { tag: "beta".into(), count: 2 },
                TagCount { tag: "project".into(), count: 2 },
            ]
        );

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn list_tags_empty_vault_is_empty() {
        let root = make_temp_dir("list-tags-empty");
        let index = mem_index(&root);
        assert!(index.list_tags().unwrap().is_empty());
        // A note with no tags still yields no tags.
        index.index_file("a.md", "# A\nPlain body.").unwrap();
        assert!(index.list_tags().unwrap().is_empty());

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn list_tags_dedups_within_a_note() {
        let root = make_temp_dir("list-tags-dedup");
        let index = mem_index(&root);
        // Same tag in frontmatter and inline must count the note once.
        index
            .index_file("a.md", "---\ntags: [dup]\n---\nBody #dup and #dup again.")
            .unwrap();
        let tags = index.list_tags().unwrap();
        assert_eq!(tags, vec![TagCount { tag: "dup".into(), count: 1 }]);

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn list_tags_updates_after_tag_removal_on_reindex() {
        let root = make_temp_dir("list-tags-reindex");
        let index = mem_index(&root);
        index
            .index_file("a.md", "---\ntags: [keep, drop]\n---\nBody")
            .unwrap();
        index.index_file("b.md", "---\ntags: [keep]\n---\nBody").unwrap();
        assert_eq!(
            index.list_tags().unwrap(),
            vec![
                TagCount { tag: "drop".into(), count: 1 },
                TagCount { tag: "keep".into(), count: 2 },
            ]
        );

        // Re-index a.md without the `drop` tag: it should vanish from the list.
        index
            .index_file("a.md", "---\ntags: [keep]\n---\nBody")
            .unwrap();
        assert_eq!(
            index.list_tags().unwrap(),
            vec![TagCount { tag: "keep".into(), count: 2 }]
        );

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn notes_by_tag_returns_hits_sorted_by_title() {
        let root = make_temp_dir("notes-by-tag");
        let index = mem_index(&root);
        index
            .index_file("zebra.md", "---\ntags: [topic]\n---\nZebra body content.")
            .unwrap();
        index
            .index_file("apple.md", "---\ntags: [topic]\n---\nApple body content.")
            .unwrap();
        index
            .index_file("other.md", "---\ntags: [misc]\n---\nOther body.")
            .unwrap();

        let hits = index.notes_by_tag("topic", 500).unwrap();
        assert_eq!(
            hits.iter().map(|h| h.path.as_str()).collect::<Vec<_>>(),
            vec!["apple.md", "zebra.md"]
        );
        // Snippet is plain body text, no marks; score 0.
        assert!(!hits[0].snippet.contains("<mark>"));
        assert!(hits[0].snippet.starts_with("Apple body"));
        assert_eq!(hits[0].score, 0.0);
        assert!(hits[0].tags.contains(&"topic".to_string()));

        // A tag no note carries yields nothing.
        assert!(index.notes_by_tag("nope", 500).unwrap().is_empty());

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn notes_by_tag_matches_tags_with_spaces_exactly() {
        let root = make_temp_dir("notes-by-tag-spaces");
        let index = mem_index(&root);
        // A frontmatter tag can contain spaces (inline `#tags` cannot).
        index
            .index_file("a.md", "---\ntags: ['two words']\n---\nBody")
            .unwrap();
        index
            .index_file("b.md", "---\ntags: [two]\n---\nBody")
            .unwrap();

        // Exact match on the spaced tag returns only a.md — the query parser
        // (whitespace-splitting) could never express this.
        let hits = index.notes_by_tag("two words", 500).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].path, "a.md");

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn notes_by_tag_long_body_snippet_is_truncated() {
        let root = make_temp_dir("notes-by-tag-snip");
        let index = mem_index(&root);
        let long = "x".repeat(200);
        index
            .index_file("a.md", &format!("---\ntags: [t]\n---\n{long}"))
            .unwrap();
        let hits = index.notes_by_tag("t", 500).unwrap();
        assert_eq!(hits.len(), 1);
        assert!(hits[0].snippet.ends_with('…'));
        // 100 body chars + the ellipsis.
        assert_eq!(hits[0].snippet.chars().count(), 101);

        std::fs::remove_dir_all(&root).ok();
    }
}
