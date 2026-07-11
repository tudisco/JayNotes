//! The SQLCipher container: one encrypted `.jaynotes`-shaped database that holds
//! notes, attachments, folders, the FTS index, and AI revisions for an
//! encrypted-db vault. It is simultaneously the storage backend *and* the search
//! index (so plaintext note text never lands in app-data), reusing the plain
//! index's extraction + FTS query helpers verbatim.
//!
//! Keying uses SQLCipher raw-key mode (`PRAGMA key = "x'<64 hex>'"`), the 32-byte
//! key coming from [`crate::providers::crypto::derive_vault_key`]. A wrong key
//! makes the first query fail — that is how "wrong password" is detected.

use std::path::Path;

use rusqlite::{params, params_from_iter, types::Value, Connection, OptionalExtension};

use crate::index::{
    build_match, extract, parse_query, rel_is_hidden, strip_md_suffix, title_of, IndexStatus,
    NoteRef, SearchHit, TagCount,
};
use crate::providers::crypto;
use crate::vault::TreeNode;

use super::merge::{self, Item, MergePlan};

const SCHEMA_VERSION: i64 = 1;

/// An open, keyed encrypted-db container.
pub struct Container {
    conn: Connection,
    /// This device's stable id (from `meta.device_id`), stamped on every write
    /// so a future multi-device merge can attribute changes.
    device: String,
}

impl Container {
    /// Applies the raw key to a freshly opened connection and switches on WAL.
    fn apply_key(conn: &Connection, key: &[u8; 32]) -> Result<(), String> {
        let hex = crypto::to_hex(key);
        conn.execute_batch(&format!("PRAGMA key = \"x'{hex}'\";"))
            .map_err(|e| format!("Could not key container: {e}"))?;
        conn.pragma_update(None, "journal_mode", "WAL").ok();
        Ok(())
    }

    /// Verifies the key works by touching the schema catalog. A wrong key yields
    /// SQLCipher's "file is not a database" / HMAC error here.
    fn verify_key(conn: &Connection) -> Result<(), String> {
        conn.query_row("SELECT count(*) FROM sqlite_master", [], |r| r.get::<_, i64>(0))
            .map(|_| ())
            .map_err(|_| "Wrong password".to_string())
    }

    /// Creates a brand-new container at `path` keyed with `key`, seeding meta
    /// (`schema_version`, `vault_name`, a random `device_id`). Errors if a file
    /// already exists there.
    pub fn create(path: &Path, key: &[u8; 32], vault_name: &str) -> Result<Container, String> {
        if path.exists() {
            return Err(format!("A file already exists at {}", path.display()));
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("Could not create vault folder: {e}"))?;
        }
        let conn = Connection::open(path).map_err(|e| format!("Could not create container: {e}"))?;
        Self::apply_key(&conn, key)?;
        let device = crypto::to_hex(&crypto::random_bytes(8)?);
        let c = Container {
            conn,
            device: device.clone(),
        };
        c.create_schema()?;
        c.set_meta("schema_version", &SCHEMA_VERSION.to_string())?;
        c.set_meta("vault_name", vault_name)?;
        c.set_meta("device_id", &device)?;
        Ok(c)
    }

    /// Opens an existing keyed container at `path`. Errors (as "Wrong password")
    /// if the key is wrong.
    pub fn open(path: &Path, key: &[u8; 32]) -> Result<Container, String> {
        let conn = Connection::open(path).map_err(|e| format!("Could not open container: {e}"))?;
        Self::apply_key(&conn, key)?;
        Self::verify_key(&conn)?;
        let device = conn
            .query_row("SELECT value FROM meta WHERE key='device_id'", [], |r| {
                r.get::<_, String>(0)
            })
            .optional()
            .map_err(|e| format!("Could not read container meta: {e}"))?
            .unwrap_or_default();
        let c = Container { conn, device };
        c.create_schema()?; // idempotent (IF NOT EXISTS)
        Ok(c)
    }

    fn create_schema(&self) -> Result<(), String> {
        self.conn
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
                 CREATE TABLE IF NOT EXISTS notes (
                    id INTEGER PRIMARY KEY,
                    path TEXT UNIQUE NOT NULL,
                    title TEXT NOT NULL,
                    content TEXT NOT NULL,
                    mtime INTEGER NOT NULL,
                    frontmatter TEXT,
                    deleted INTEGER NOT NULL DEFAULT 0,
                    device TEXT
                 );
                 CREATE VIRTUAL TABLE IF NOT EXISTS notes_fts USING fts5(
                    title, body, path UNINDEXED,
                    tokenize = \"unicode61 remove_diacritics 2\"
                 );
                 CREATE TABLE IF NOT EXISTS tags (
                    note_id INTEGER NOT NULL, tag TEXT NOT NULL,
                    PRIMARY KEY (note_id, tag)
                 );
                 CREATE TABLE IF NOT EXISTS links (
                    source_id INTEGER NOT NULL, target_path TEXT NOT NULL
                 );
                 CREATE TABLE IF NOT EXISTS folders (
                    path TEXT PRIMARY KEY, deleted INTEGER NOT NULL DEFAULT 0
                 );
                 CREATE TABLE IF NOT EXISTS attachments (
                    name TEXT PRIMARY KEY, bytes BLOB NOT NULL,
                    mtime INTEGER NOT NULL, deleted INTEGER NOT NULL DEFAULT 0
                 );
                 CREATE TABLE IF NOT EXISTS revisions (
                    id TEXT PRIMARY KEY, path TEXT NOT NULL,
                    content TEXT NOT NULL, ts INTEGER NOT NULL
                 );
                 CREATE INDEX IF NOT EXISTS idx_tags_tag ON tags(tag);
                 CREATE INDEX IF NOT EXISTS idx_links_target ON links(target_path);",
            )
            .map_err(|e| format!("Could not create container schema: {e}"))
    }

    // ---- meta -----------------------------------------------------------

    fn set_meta(&self, key: &str, value: &str) -> Result<(), String> {
        self.conn
            .execute(
                "INSERT OR REPLACE INTO meta(key,value) VALUES(?1,?2)",
                params![key, value],
            )
            .map(|_| ())
            .map_err(|e| format!("Could not write meta '{key}': {e}"))
    }

    fn get_meta(&self, key: &str) -> Option<String> {
        self.conn
            .query_row("SELECT value FROM meta WHERE key=?1", params![key], |r| {
                r.get::<_, String>(0)
            })
            .optional()
            .ok()
            .flatten()
    }

    /// Epoch (ms) of the last snapshot export — the merge baseline. 0 if never.
    pub fn last_export_time(&self) -> i64 {
        self.get_meta("last_export_time")
            .and_then(|s| s.parse().ok())
            .unwrap_or(0)
    }

    // ---- notes ----------------------------------------------------------

    fn note_id(&self, rel: &str) -> Result<Option<i64>, String> {
        self.conn
            .query_row("SELECT id FROM notes WHERE path=?1", params![rel], |r| {
                r.get(0)
            })
            .optional()
            .map_err(|e| format!("Could not look up note: {e}"))
    }

    /// True if a live (non-deleted) note exists at `rel`.
    pub fn note_exists(&self, rel: &str) -> Result<bool, String> {
        self.conn
            .query_row(
                "SELECT 1 FROM notes WHERE path=?1 AND deleted=0",
                params![rel],
                |_| Ok(()),
            )
            .optional()
            .map(|o| o.is_some())
            .map_err(|e| format!("Could not check note: {e}"))
    }

    /// Reads a live note's text.
    pub fn read_note(&self, rel: &str) -> Result<String, String> {
        self.conn
            .query_row(
                "SELECT content FROM notes WHERE path=?1 AND deleted=0",
                params![rel],
                |r| r.get::<_, String>(0),
            )
            .optional()
            .map_err(|e| format!("Could not read note: {e}"))?
            .ok_or_else(|| format!("Note does not exist: {rel}"))
    }

    /// Upserts a note's full text at `rel` (full-replace), refreshing the FTS,
    /// tags, links, and ancestor folder rows. Stamps `mtime`/`device`.
    pub fn write_note(&self, rel: &str, content: &str) -> Result<(), String> {
        self.upsert_note(rel, content, now_millis())
    }

    fn upsert_note(&self, rel: &str, content: &str, mtime: i64) -> Result<(), String> {
        let ex = extract(content);
        let title = title_of(rel);
        let id = match self.note_id(rel)? {
            Some(id) => {
                self.conn
                    .execute(
                        "UPDATE notes SET title=?2, content=?3, mtime=?4, frontmatter=?5, \
                         deleted=0, device=?6 WHERE id=?1",
                        params![id, title, content, mtime, ex.frontmatter, self.device],
                    )
                    .map_err(|e| format!("Could not update note: {e}"))?;
                id
            }
            None => {
                self.conn
                    .execute(
                        "INSERT INTO notes(path,title,content,mtime,frontmatter,deleted,device) \
                         VALUES(?1,?2,?3,?4,?5,0,?6)",
                        params![rel, title, content, mtime, ex.frontmatter, self.device],
                    )
                    .map_err(|e| format!("Could not insert note: {e}"))?;
                self.conn.last_insert_rowid()
            }
        };
        self.refresh_derived(id, rel, &title, &ex.body, &ex.tags, &ex.links)?;
        self.ensure_ancestor_folders(rel)?;
        Ok(())
    }

    fn refresh_derived(
        &self,
        id: i64,
        rel: &str,
        title: &str,
        body: &str,
        tags: &[String],
        links: &[String],
    ) -> Result<(), String> {
        self.conn
            .execute("DELETE FROM notes_fts WHERE rowid=?1", params![id])
            .map_err(|e| format!("fts clear: {e}"))?;
        self.conn
            .execute(
                "INSERT INTO notes_fts(rowid,title,body,path) VALUES(?1,?2,?3,?4)",
                params![id, title, body, rel],
            )
            .map_err(|e| format!("fts insert: {e}"))?;
        self.conn
            .execute("DELETE FROM tags WHERE note_id=?1", params![id])
            .map_err(|e| format!("tags clear: {e}"))?;
        for t in tags {
            self.conn
                .execute(
                    "INSERT OR IGNORE INTO tags(note_id,tag) VALUES(?1,?2)",
                    params![id, t],
                )
                .map_err(|e| format!("tag insert: {e}"))?;
        }
        self.conn
            .execute("DELETE FROM links WHERE source_id=?1", params![id])
            .map_err(|e| format!("links clear: {e}"))?;
        for l in links {
            self.conn
                .execute(
                    "INSERT INTO links(source_id,target_path) VALUES(?1,?2)",
                    params![id, l],
                )
                .map_err(|e| format!("link insert: {e}"))?;
        }
        Ok(())
    }

    /// Drops a note's derived (FTS/tags/links) rows — used when tombstoning so
    /// search hides it while the `notes` tombstone row survives for merge.
    fn clear_derived(&self, id: i64) -> Result<(), String> {
        self.conn
            .execute("DELETE FROM notes_fts WHERE rowid=?1", params![id])
            .map_err(|e| format!("fts clear: {e}"))?;
        self.conn
            .execute("DELETE FROM tags WHERE note_id=?1", params![id])
            .map_err(|e| format!("tags clear: {e}"))?;
        self.conn
            .execute("DELETE FROM links WHERE source_id=?1", params![id])
            .map_err(|e| format!("links clear: {e}"))?;
        Ok(())
    }

    // ---- folders --------------------------------------------------------

    fn ensure_ancestor_folders(&self, rel: &str) -> Result<(), String> {
        let parts: Vec<&str> = rel.split('/').collect();
        for i in 1..parts.len() {
            let dir = parts[..i].join("/");
            self.conn
                .execute(
                    "INSERT INTO folders(path,deleted) VALUES(?1,0) \
                     ON CONFLICT(path) DO UPDATE SET deleted=0",
                    params![dir],
                )
                .map_err(|e| format!("folder ensure: {e}"))?;
        }
        Ok(())
    }

    /// Creates an (empty) folder row. Errors if a live folder already exists.
    pub fn create_folder(&self, rel: &str) -> Result<(), String> {
        if rel.trim().is_empty() {
            return Err("Folder name cannot be empty".into());
        }
        let live: Option<i64> = self
            .conn
            .query_row(
                "SELECT 1 FROM folders WHERE path=?1 AND deleted=0",
                params![rel],
                |_| Ok(1),
            )
            .optional()
            .map_err(|e| format!("folder check: {e}"))?;
        if live.is_some() {
            return Err(format!("'{rel}' already exists"));
        }
        self.conn
            .execute(
                "INSERT INTO folders(path,deleted) VALUES(?1,0) \
                 ON CONFLICT(path) DO UPDATE SET deleted=0",
                params![rel],
            )
            .map_err(|e| format!("Could not create folder: {e}"))?;
        // Materialize ancestors too.
        if rel.contains('/') {
            self.ensure_ancestor_folders(&format!("{rel}/x"))?;
        }
        Ok(())
    }

    fn is_dir(&self, rel: &str) -> Result<bool, String> {
        if rel.is_empty() {
            return Ok(true);
        }
        let as_folder: Option<i64> = self
            .conn
            .query_row(
                "SELECT 1 FROM folders WHERE path=?1 AND deleted=0",
                params![rel],
                |_| Ok(1),
            )
            .optional()
            .map_err(|e| format!("dir check: {e}"))?;
        Ok(as_folder.is_some())
    }

    /// Untitled-note creation mirroring the plain provider's semantics.
    pub fn create_note(&self, rel: &str) -> Result<String, String> {
        let dir_prefix = |dir: &str, name: &str| {
            if dir.is_empty() {
                name.to_string()
            } else {
                format!("{dir}/{name}")
            }
        };
        let created = if rel.is_empty() || self.is_dir(rel)? {
            let mut chosen = None;
            let first = dir_prefix(rel, "Untitled.md");
            if !self.note_exists(&first)? {
                chosen = Some(first);
            } else {
                for n in 1..10_000 {
                    let c = dir_prefix(rel, &format!("Untitled {n}.md"));
                    if !self.note_exists(&c)? {
                        chosen = Some(c);
                        break;
                    }
                }
            }
            chosen.ok_or("Could not find a free Untitled name")?
        } else {
            let mut p = rel.to_string();
            if !p.to_lowercase().ends_with(".md") {
                p.push_str(".md");
            }
            if self.note_exists(&p)? {
                return Err(format!("A file named '{p}' already exists"));
            }
            p
        };
        self.write_note(&created, "")?;
        Ok(created)
    }

    // ---- rename / trash -------------------------------------------------

    /// Renames/moves a note or folder (and its descendants). Errors if the
    /// destination is occupied.
    pub fn rename(&self, old_rel: &str, new_rel: &str) -> Result<(), String> {
        let old = old_rel.trim_end_matches('/');
        let new = new_rel.trim_end_matches('/');
        if self.note_exists(new)? || self.is_dir(new)? {
            return Err(format!("'{new}' already exists"));
        }
        let like = format!("{old}/%");
        // Move every note that is `old` or under `old/`.
        let rows: Vec<(i64, String)> = {
            let mut stmt = self
                .conn
                .prepare("SELECT id,path FROM notes WHERE path=?1 OR path LIKE ?2")
                .map_err(|e| format!("rename query: {e}"))?;
            let mapped = stmt
                .query_map(params![old, like], |r| Ok((r.get(0)?, r.get(1)?)))
                .map_err(|e| format!("rename query: {e}"))?;
            mapped.filter_map(|r| r.ok()).collect()
        };
        for (id, path) in rows {
            let np = if path == old {
                new.to_string()
            } else {
                format!("{new}{}", &path[old.len()..])
            };
            let title = title_of(&np);
            self.conn
                .execute(
                    "UPDATE notes SET path=?2,title=?3 WHERE id=?1",
                    params![id, np, title],
                )
                .map_err(|e| format!("rename update: {e}"))?;
            self.conn
                .execute(
                    "UPDATE notes_fts SET path=?2,title=?3 WHERE rowid=?1",
                    params![id, np, title],
                )
                .map_err(|e| format!("rename fts: {e}"))?;
            self.ensure_ancestor_folders(&np)?;
        }
        // Move folder rows.
        self.conn
            .execute(
                "UPDATE folders SET path = ?2 || substr(path, ?3) \
                 WHERE path=?1 OR path LIKE ?4",
                params![old, new, (old.len() + 1) as i64, like],
            )
            .ok();
        Ok(())
    }

    /// Tombstones a note or a whole folder subtree.
    pub fn trash(&self, rel: &str) -> Result<(), String> {
        let mtime = now_millis();
        if let Some(id) = self.note_id(rel)? {
            self.conn
                .execute(
                    "UPDATE notes SET deleted=1, mtime=?2, device=?3 WHERE id=?1",
                    params![id, mtime, self.device],
                )
                .map_err(|e| format!("trash note: {e}"))?;
            self.clear_derived(id)?;
            return Ok(());
        }
        // Folder subtree.
        let like = format!("{}/%", rel.trim_end_matches('/'));
        let ids: Vec<i64> = {
            let mut stmt = self
                .conn
                .prepare("SELECT id FROM notes WHERE path LIKE ?1 AND deleted=0")
                .map_err(|e| format!("trash query: {e}"))?;
            let out: Vec<i64> = stmt
                .query_map(params![like], |r| r.get(0))
                .map_err(|e| format!("trash query: {e}"))?
                .filter_map(|r| r.ok())
                .collect();
            out
        };
        if ids.is_empty() && !self.is_dir(rel)? {
            return Err(format!("'{rel}' does not exist"));
        }
        for id in ids {
            self.conn
                .execute(
                    "UPDATE notes SET deleted=1, mtime=?2, device=?3 WHERE id=?1",
                    params![id, mtime, self.device],
                )
                .map_err(|e| format!("trash note: {e}"))?;
            self.clear_derived(id)?;
        }
        self.conn
            .execute(
                "UPDATE folders SET deleted=1 WHERE path=?1 OR path LIKE ?2",
                params![rel.trim_end_matches('/'), like],
            )
            .ok();
        Ok(())
    }

    // ---- attachments ----------------------------------------------------

    /// Saves image bytes under `attachments/<unique>.<ext>` and returns the rel.
    pub fn save_attachment(&self, file_name: &str, data: &[u8]) -> Result<String, String> {
        let (base, ext) = crate::vault::sanitize_attachment_name(file_name)?;
        let mut rel = format!("attachments/{base}.{ext}");
        if self.attachment_exists(&rel)? {
            let mut n = 1;
            loop {
                rel = format!("attachments/{base}-{n}.{ext}");
                if !self.attachment_exists(&rel)? {
                    break;
                }
                n += 1;
            }
        }
        self.conn
            .execute(
                "INSERT OR REPLACE INTO attachments(name,bytes,mtime,deleted) VALUES(?1,?2,?3,0)",
                params![rel, data, now_millis()],
            )
            .map_err(|e| format!("Could not save attachment: {e}"))?;
        Ok(rel)
    }

    fn attachment_exists(&self, rel: &str) -> Result<bool, String> {
        self.conn
            .query_row(
                "SELECT 1 FROM attachments WHERE name=?1 AND deleted=0",
                params![rel],
                |_| Ok(()),
            )
            .optional()
            .map(|o| o.is_some())
            .map_err(|e| format!("attachment check: {e}"))
    }

    /// Raw bytes of a live attachment.
    pub fn read_attachment(&self, rel: &str) -> Result<Vec<u8>, String> {
        self.conn
            .query_row(
                "SELECT bytes FROM attachments WHERE name=?1 AND deleted=0",
                params![rel],
                |r| r.get::<_, Vec<u8>>(0),
            )
            .optional()
            .map_err(|e| format!("Could not read attachment: {e}"))?
            .ok_or_else(|| format!("Attachment does not exist: {rel}"))
    }

    // ---- tree -----------------------------------------------------------

    /// Builds the folder/file tree from live note + folder paths.
    pub fn scan_tree(&self) -> Result<TreeNode, String> {
        let vault_name = self.get_meta("vault_name").unwrap_or_else(|| "Vault".into());
        let mut root = TreeNode {
            name: vault_name,
            path: String::new(),
            is_dir: true,
            children: Vec::new(),
        };
        let mut folders: Vec<String> = self
            .collect_paths("SELECT path FROM folders WHERE deleted=0")?
            .into_iter()
            .filter(|p| !rel_is_hidden(p))
            .collect();
        folders.sort();
        for dir in &folders {
            insert_node(&mut root, dir, true);
        }
        let notes: Vec<String> = self
            .collect_paths("SELECT path FROM notes WHERE deleted=0")?
            .into_iter()
            .filter(|p| !rel_is_hidden(p))
            .collect();
        for rel in &notes {
            insert_node(&mut root, rel, false);
        }
        sort_tree(&mut root);
        Ok(root)
    }

    fn collect_paths(&self, sql: &str) -> Result<Vec<String>, String> {
        let mut stmt = self
            .conn
            .prepare(sql)
            .map_err(|e| format!("path query: {e}"))?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(|e| format!("path query: {e}"))?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    // ---- search (mirrors index.rs, over live notes) --------------------

    fn tags_for(&self, id: i64) -> Result<Vec<String>, String> {
        let mut stmt = self
            .conn
            .prepare("SELECT tag FROM tags WHERE note_id=?1 ORDER BY tag")
            .map_err(|e| format!("tags query: {e}"))?;
        let rows = stmt
            .query_map([id], |r| r.get::<_, String>(0))
            .map_err(|e| format!("tags query: {e}"))?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    pub fn search(&self, query: &str, limit: u32) -> Result<Vec<SearchHit>, String> {
        let (terms, tags) = parse_query(query);
        let match_expr = build_match(&terms);
        if match_expr.is_none() && tags.is_empty() {
            return Ok(Vec::new());
        }
        let limit = limit as i64;
        let raw: Vec<(i64, String, String, String, f64, String, i64)> = if let Some(m) = match_expr {
            let mut sql = String::from(
                "SELECT n.id,n.path,n.title, \
                 snippet(notes_fts,1,'<mark>','</mark>','…',12), \
                 bm25(notes_fts,5.0,1.0), substr(notes_fts.body,1,100), length(notes_fts.body) \
                 FROM notes_fts JOIN notes n ON n.id=notes_fts.rowid \
                 WHERE n.deleted=0 AND notes_fts MATCH ?",
            );
            let mut binds: Vec<Value> = vec![Value::Text(m)];
            if !tags.is_empty() {
                let ph = vec!["?"; tags.len()].join(",");
                sql.push_str(&format!(
                    " AND n.id IN (SELECT note_id FROM tags WHERE tag IN ({ph}) \
                     GROUP BY note_id HAVING COUNT(DISTINCT tag)=?)"
                ));
                for t in &tags {
                    binds.push(Value::Text(t.clone()));
                }
                binds.push(Value::Integer(tags.len() as i64));
            }
            sql.push_str(" ORDER BY bm25(notes_fts,5.0,1.0) LIMIT ?");
            binds.push(Value::Integer(limit));
            self.query_hits(&sql, binds)?
        } else {
            let ph = vec!["?"; tags.len()].join(",");
            let sql = format!(
                "SELECT n.id,n.path,n.title,'',0.0,substr(b.body,1,100),length(b.body) \
                 FROM notes n JOIN notes_fts b ON b.rowid=n.id \
                 WHERE n.deleted=0 AND n.id IN (SELECT note_id FROM tags WHERE tag IN ({ph}) \
                 GROUP BY note_id HAVING COUNT(DISTINCT tag)=?) \
                 ORDER BY n.title COLLATE NOCASE ASC LIMIT ?"
            );
            let mut binds: Vec<Value> = tags.iter().map(|t| Value::Text(t.clone())).collect();
            binds.push(Value::Integer(tags.len() as i64));
            binds.push(Value::Integer(limit));
            self.query_hits(&sql, binds)?
        };
        self.finish_hits(raw)
    }

    fn query_hits(
        &self,
        sql: &str,
        binds: Vec<Value>,
    ) -> Result<Vec<(i64, String, String, String, f64, String, i64)>, String> {
        let mut stmt = self
            .conn
            .prepare(sql)
            .map_err(|e| format!("search prepare: {e}"))?;
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
            .map_err(|e| format!("search run: {e}"))?;
        rows.collect::<Result<_, _>>()
            .map_err(|e| format!("search read: {e}"))
    }

    fn finish_hits(
        &self,
        raw: Vec<(i64, String, String, String, f64, String, i64)>,
    ) -> Result<Vec<SearchHit>, String> {
        let mut hits = Vec::with_capacity(raw.len());
        for (id, path, title, snip, score, body_prefix, body_len) in raw {
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

    pub fn list_notes(&self) -> Result<Vec<NoteRef>, String> {
        let mut stmt = self
            .conn
            .prepare("SELECT path,title,mtime FROM notes WHERE deleted=0 ORDER BY mtime DESC")
            .map_err(|e| format!("list prepare: {e}"))?;
        let rows = stmt
            .query_map([], |r| {
                Ok(NoteRef {
                    path: r.get(0)?,
                    title: r.get(1)?,
                    // container mtime is ms; NoteRef mtime is used only for sort.
                    mtime: r.get::<_, i64>(2)? / 1000,
                })
            })
            .map_err(|e| format!("list notes: {e}"))?;
        rows.collect::<Result<_, _>>()
            .map_err(|e| format!("list read: {e}"))
    }

    pub fn list_tags(&self) -> Result<Vec<TagCount>, String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT t.tag, COUNT(*) FROM tags t JOIN notes n ON n.id=t.note_id \
                 WHERE n.deleted=0 GROUP BY t.tag ORDER BY t.tag COLLATE NOCASE ASC, t.tag ASC",
            )
            .map_err(|e| format!("tag list prepare: {e}"))?;
        let rows = stmt
            .query_map([], |r| {
                Ok(TagCount {
                    tag: r.get(0)?,
                    count: r.get::<_, i64>(1)? as u32,
                })
            })
            .map_err(|e| format!("tag list: {e}"))?;
        rows.collect::<Result<_, _>>()
            .map_err(|e| format!("tag read: {e}"))
    }

    pub fn notes_by_tag(&self, tag: &str, limit: u32) -> Result<Vec<SearchHit>, String> {
        let limit = limit as i64;
        let mut stmt = self
            .conn
            .prepare(
                "SELECT n.id,n.path,n.title,substr(b.body,1,100),length(b.body) \
                 FROM notes n JOIN notes_fts b ON b.rowid=n.id JOIN tags t ON t.note_id=n.id \
                 WHERE n.deleted=0 AND t.tag=?1 ORDER BY n.title COLLATE NOCASE ASC LIMIT ?2",
            )
            .map_err(|e| format!("by-tag prepare: {e}"))?;
        let raw: Vec<(i64, String, String, String, i64)> = stmt
            .query_map(params![tag, limit], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })
            .map_err(|e| format!("by-tag run: {e}"))?
            .collect::<Result<_, _>>()
            .map_err(|e| format!("by-tag read: {e}"))?;
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

    pub fn resolve(&self, name: &str) -> Result<Option<String>, String> {
        let cleaned = strip_md_suffix(name.trim());
        if cleaned.is_empty() {
            return Ok(None);
        }
        let title = cleaned.rsplit('/').next().unwrap_or(cleaned);
        let mut stmt = self
            .conn
            .prepare("SELECT path FROM notes WHERE deleted=0 AND title=?1 COLLATE NOCASE")
            .map_err(|e| format!("resolve prepare: {e}"))?;
        let mut paths: Vec<String> = stmt
            .query_map(params![title], |r| r.get::<_, String>(0))
            .map_err(|e| format!("resolve run: {e}"))?
            .filter_map(|r| r.ok())
            .collect();
        let want = format!("{}.md", cleaned.to_lowercase());
        paths.sort_by(|a, b| {
            let rank = |p: &str| -> (bool, usize, String) {
                let low = p.to_lowercase();
                let hit = low == want || low.ends_with(&format!("/{want}"));
                (!hit, p.matches('/').count(), low)
            };
            rank(a).cmp(&rank(b))
        });
        Ok(paths.into_iter().next())
    }

    pub fn status(&self) -> Result<IndexStatus, String> {
        let note_count: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM notes WHERE deleted=0", [], |r| r.get(0))
            .map_err(|e| format!("count: {e}"))?;
        Ok(IndexStatus {
            note_count,
            last_scan_ms: 0,
        })
    }

    // ---- revisions (AI undo trail, stored in-container) ----------------

    // Wired in a follow-up: the AI tool layer still targets plain fs vaults, so
    // this in-container revision store isn't called yet. Kept (and tested-by-
    // construction via the schema) so M14's container is revision-ready.
    #[allow(dead_code)]
    pub fn snapshot_revision(&self, rel: &str, content: &str) -> Result<String, String> {
        let id = format!("{}-{}", now_millis(), rand_suffix());
        self.conn
            .execute(
                "INSERT INTO revisions(id,path,content,ts) VALUES(?1,?2,?3,?4)",
                params![id, rel, content, now_millis()],
            )
            .map_err(|e| format!("revision insert: {e}"))?;
        Ok(id)
    }

    // ---- snapshot export / import --------------------------------------

    /// Exports a consistent, same-key encrypted snapshot to `dest` via
    /// `VACUUM INTO` + atomic rename, and records the export hash + time so the
    /// next open can detect an external change.
    pub fn export_snapshot(&self, dest: &Path) -> Result<(), String> {
        let parent = dest
            .parent()
            .ok_or_else(|| "Snapshot path has no parent".to_string())?;
        std::fs::create_dir_all(parent).map_err(|e| format!("snapshot dir: {e}"))?;
        let tmp = parent.join(format!(
            ".{}.tmp-{}",
            dest.file_name().unwrap_or_default().to_string_lossy(),
            std::process::id()
        ));
        let _ = std::fs::remove_file(&tmp);
        // VACUUM INTO inherits the source connection's key → encrypted snapshot.
        self.conn
            .execute("VACUUM INTO ?1", params![tmp.to_string_lossy()])
            .map_err(|e| format!("Could not export snapshot: {e}"))?;
        std::fs::rename(&tmp, dest).map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            format!("Could not finalize snapshot: {e}")
        })?;
        let hash = file_hash(dest)?;
        self.set_meta("last_export_hash", &hash)?;
        self.set_meta("last_export_time", &now_millis().to_string())?;
        Ok(())
    }

    /// Records the current snapshot file as our last export (used after
    /// hydrating a fresh live DB from an existing snapshot, so we don't treat it
    /// as an external change and re-import it).
    pub fn mark_synced(&self, snapshot: &Path) -> Result<(), String> {
        let hash = file_hash(snapshot)?;
        self.set_meta("last_export_hash", &hash)?;
        self.set_meta("last_export_time", &now_millis().to_string())?;
        Ok(())
    }

    /// True if `snapshot` on disk differs from the last one we exported (an
    /// external write — another device or a Syncthing sync).
    pub fn snapshot_changed_externally(&self, snapshot: &Path) -> bool {
        if !snapshot.exists() {
            return false;
        }
        match (self.get_meta("last_export_hash"), file_hash(snapshot).ok()) {
            (Some(prev), Some(now)) => prev != now,
            (None, Some(_)) => true,
            _ => false,
        }
    }

    /// Imports notes + attachments from an external snapshot (or sync-conflict
    /// sibling) at `path`, keyed with `key`, merging per the pure policy in
    /// [`super::merge`]. Returns the number of records written. Conflict losers
    /// are stored as `… (conflict <date>)` copies.
    pub fn import_from(&self, path: &Path, key: &[u8; 32], date: &str) -> Result<usize, String> {
        let other = Container::open(path, key)?;
        let base = self.last_export_time();
        let mut written = 0usize;

        // Notes.
        let remote_notes = other.all_note_items()?;
        for r in &remote_notes {
            let local = self.note_item(&r.path)?;
            let plan = merge::merge_item(local.as_ref(), Some(r), base);
            written += self.apply_note_plan(&r.path, plan, date)?;
        }
        // Attachments (same rules, digest as the compared payload).
        let remote_atts = other.all_attachment_items()?;
        for (r, bytes) in &remote_atts {
            let local = self.attachment_item(&r.path)?;
            let plan = merge::merge_item(local.as_ref(), Some(r), base);
            written += self.apply_attachment_plan(&r.path, plan, bytes, date)?;
        }
        Ok(written)
    }

    fn note_item(&self, rel: &str) -> Result<Option<Item>, String> {
        self.conn
            .query_row(
                "SELECT path,content,mtime,deleted FROM notes WHERE path=?1",
                params![rel],
                |r| {
                    Ok(Item {
                        path: r.get(0)?,
                        content: r.get(1)?,
                        mtime: r.get(2)?,
                        deleted: r.get::<_, i64>(3)? != 0,
                    })
                },
            )
            .optional()
            .map_err(|e| format!("note item: {e}"))
    }

    fn all_note_items(&self) -> Result<Vec<Item>, String> {
        let mut stmt = self
            .conn
            .prepare("SELECT path,content,mtime,deleted FROM notes")
            .map_err(|e| format!("items query: {e}"))?;
        let rows = stmt
            .query_map([], |r| {
                Ok(Item {
                    path: r.get(0)?,
                    content: r.get(1)?,
                    mtime: r.get(2)?,
                    deleted: r.get::<_, i64>(3)? != 0,
                })
            })
            .map_err(|e| format!("items query: {e}"))?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    fn apply_note_plan(&self, path: &str, plan: MergePlan, date: &str) -> Result<usize, String> {
        let mut n = 0;
        if let Some(w) = plan.write {
            if w.deleted {
                if let Some(id) = self.note_id(path)? {
                    self.conn
                        .execute(
                            "UPDATE notes SET deleted=1,mtime=?2 WHERE id=?1",
                            params![id, w.mtime],
                        )
                        .map_err(|e| format!("apply tombstone: {e}"))?;
                    self.clear_derived(id)?;
                }
            } else {
                self.upsert_note(path, &w.content, w.mtime)?;
            }
            n += 1;
        }
        if let Some(loser) = plan.conflict {
            let taken = |p: &str| self.note_exists(p).unwrap_or(false);
            let cp = merge::conflict_path(path, date, &taken);
            self.upsert_note(&cp, &loser.content, loser.mtime)?;
            n += 1;
        }
        Ok(n)
    }

    fn attachment_item(&self, rel: &str) -> Result<Option<Item>, String> {
        self.conn
            .query_row(
                "SELECT name,mtime,deleted, quote(bytes) FROM attachments WHERE name=?1",
                params![rel],
                |r| {
                    Ok(Item {
                        path: r.get(0)?,
                        mtime: r.get(1)?,
                        deleted: r.get::<_, i64>(2)? != 0,
                        content: r.get::<_, String>(3)?, // hex digest-ish for comparison
                    })
                },
            )
            .optional()
            .map_err(|e| format!("att item: {e}"))
    }

    fn all_attachment_items(&self) -> Result<Vec<(Item, Vec<u8>)>, String> {
        let mut stmt = self
            .conn
            .prepare("SELECT name,mtime,deleted,quote(bytes),bytes FROM attachments")
            .map_err(|e| format!("att query: {e}"))?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    Item {
                        path: r.get(0)?,
                        mtime: r.get(1)?,
                        deleted: r.get::<_, i64>(2)? != 0,
                        content: r.get::<_, String>(3)?,
                    },
                    r.get::<_, Vec<u8>>(4)?,
                ))
            })
            .map_err(|e| format!("att query: {e}"))?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    fn apply_attachment_plan(
        &self,
        name: &str,
        plan: MergePlan,
        bytes: &[u8],
        date: &str,
    ) -> Result<usize, String> {
        let mut n = 0;
        if let Some(w) = plan.write {
            self.conn
                .execute(
                    "INSERT OR REPLACE INTO attachments(name,bytes,mtime,deleted) VALUES(?1,?2,?3,?4)",
                    params![name, bytes, w.mtime, w.deleted as i64],
                )
                .map_err(|e| format!("apply att: {e}"))?;
            n += 1;
        }
        if let Some(loser) = plan.conflict {
            let taken = |p: &str| self.attachment_exists(p).unwrap_or(false);
            let cp = merge::conflict_path(name, date, &taken);
            self.conn
                .execute(
                    "INSERT OR REPLACE INTO attachments(name,bytes,mtime,deleted) VALUES(?1,?2,?3,0)",
                    params![cp, bytes, loser.mtime],
                )
                .map_err(|e| format!("apply att conflict: {e}"))?;
            n += 1;
        }
        Ok(n)
    }
}

// ---------------------------------------------------------------------------
// Free helpers
// ---------------------------------------------------------------------------

fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[allow(dead_code)]
fn rand_suffix() -> String {
    crypto::to_hex(&crypto::random_bytes(4).unwrap_or_default())
}

/// FNV-1a 64-bit hash of a file's bytes, hex — a cheap change detector.
fn file_hash(path: &Path) -> Result<String, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("Could not hash snapshot: {e}"))?;
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    Ok(format!("{h:016x}"))
}

fn insert_node(root: &mut TreeNode, rel: &str, is_dir: bool) {
    let parts: Vec<&str> = rel.split('/').filter(|s| !s.is_empty()).collect();
    let mut cur = root;
    for (i, name) in parts.iter().enumerate() {
        let is_last = i == parts.len() - 1;
        let existing = cur.children.iter().position(|c| c.name == *name);
        let idx = match existing {
            Some(idx) => idx,
            None => {
                cur.children.push(TreeNode {
                    name: (*name).to_string(),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_path(tag: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("jaynotes-enc-{tag}-{}-{nanos}.jaynotes", std::process::id()))
    }

    fn key() -> [u8; 32] {
        crypto::derive_vault_key("correct horse", b"unit-test-salt16").unwrap()
    }

    #[test]
    fn create_write_reopen_roundtrip() {
        let path = tmp_path("roundtrip");
        let k = key();
        {
            let c = Container::create(&path, &k, "My Vault").unwrap();
            c.write_note("notes/hello.md", "---\ntags: [x]\n---\n# Hi\nBody #inline")
                .unwrap();
            c.write_note("notes/other.md", "quick brown fox").unwrap();
            c.save_attachment("pic.png", &[1, 2, 3, 4]).unwrap();
        }
        // Reopen with the correct key: content survives.
        let c = Container::open(&path, &k).unwrap();
        assert_eq!(
            c.read_note("notes/hello.md").unwrap(),
            "---\ntags: [x]\n---\n# Hi\nBody #inline"
        );
        assert_eq!(c.read_attachment("attachments/pic.png").unwrap(), vec![1, 2, 3, 4]);
        // Search + tags work over the container.
        assert_eq!(c.search("fox", 10).unwrap().len(), 1);
        let tags = c.list_tags().unwrap();
        assert!(tags.iter().any(|t| t.tag == "x"));
        assert!(tags.iter().any(|t| t.tag == "inline"));
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn wrong_password_fails_to_open() {
        let path = tmp_path("wrongpw");
        let k = key();
        {
            Container::create(&path, &k, "V").unwrap().write_note("a.md", "hi").unwrap();
        }
        let wrong = crypto::derive_vault_key("nope", b"unit-test-salt16").unwrap();
        assert!(Container::open(&path, &wrong).is_err(), "wrong key must fail");
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn trash_hides_from_search_and_tree() {
        let path = tmp_path("trash");
        let k = key();
        let c = Container::create(&path, &k, "V").unwrap();
        c.write_note("a.md", "findme apple").unwrap();
        assert_eq!(c.search("apple", 10).unwrap().len(), 1);
        c.trash("a.md").unwrap();
        assert_eq!(c.search("apple", 10).unwrap().len(), 0);
        assert!(c.read_note("a.md").is_err());
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn export_then_import_merges_newer_wins() {
        let base = tmp_path("exp-base");
        let snap = tmp_path("exp-snap");
        let k = key();
        let c = Container::create(&base, &k, "V").unwrap();
        c.write_note("a.md", "v1").unwrap();
        c.export_snapshot(&snap).unwrap();
        // Simulate an external edit landing in the snapshot: open snap, bump note.
        {
            let other = Container::open(&snap, &k).unwrap();
            // Force a newer mtime than base export.
            std::thread::sleep(std::time::Duration::from_millis(5));
            other.write_note("a.md", "v2-remote").unwrap();
            other.write_note("b.md", "brand new remote").unwrap();
        }
        assert!(c.snapshot_changed_externally(&snap));
        let n = c.import_from(&snap, &k, "2026-07-11").unwrap();
        assert!(n >= 2);
        assert_eq!(c.read_note("a.md").unwrap(), "v2-remote");
        assert_eq!(c.read_note("b.md").unwrap(), "brand new remote");
        std::fs::remove_file(&base).ok();
        std::fs::remove_file(&snap).ok();
    }

    #[test]
    fn both_sides_changed_keeps_loser_as_conflict_copy() {
        let base = tmp_path("conf-base");
        let snap = tmp_path("conf-snap");
        let k = key();
        let c = Container::create(&base, &k, "V").unwrap();
        c.write_note("a.md", "v1").unwrap();
        c.export_snapshot(&snap).unwrap();

        // Edit locally first, then a newer remote edit lands in the snapshot.
        std::thread::sleep(std::time::Duration::from_millis(3));
        c.write_note("a.md", "local-edit").unwrap();
        {
            let other = Container::open(&snap, &k).unwrap();
            std::thread::sleep(std::time::Duration::from_millis(3));
            other.write_note("a.md", "remote-edit").unwrap();
        }
        c.import_from(&snap, &k, "2026-07-11").unwrap();
        // Newer remote wins the primary path; the local loser is preserved.
        assert_eq!(c.read_note("a.md").unwrap(), "remote-edit");
        assert_eq!(
            c.read_note("a (conflict 2026-07-11).md").unwrap(),
            "local-edit"
        );
        std::fs::remove_file(&base).ok();
        std::fs::remove_file(&snap).ok();
    }
}
