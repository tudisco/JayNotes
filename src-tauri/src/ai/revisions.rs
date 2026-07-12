//! Revision snapshots — a lightweight undo trail for AI-authored writes.
//!
//! Before the assistant overwrites or appends to a note, the *current* content
//! is snapshotted here so the user can revert. Snapshots live outside the vault
//! (under `app_data_dir/ai-revisions/<vault-hash>/`) so they never clutter the
//! notes folder or get indexed.
//!
//! Each snapshot is a real `.md` file holding the captured content, named
//! `<id>-<sanitized-rel>.md`. A `manifest.json` maps each snapshot id to the
//! exact vault-relative path it came from (the sanitized filename is lossy, so
//! the manifest is the source of truth for reverts). The trail is capped at
//! [`MAX_PER_VAULT`]; the oldest snapshots (and their files) are pruned.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

/// Maximum snapshots retained per vault before the oldest are pruned.
pub const MAX_PER_VAULT: usize = 50;

/// Process-global monotonic counter, combined with a millisecond timestamp to
/// form collision-free revision ids even for rapid successive writes.
static SEQ: AtomicU64 = AtomicU64::new(0);

/// One entry in the revision manifest.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct RevEntry {
    id: String,
    /// Vault-relative path the snapshot was taken from.
    path: String,
    /// Snapshot file name within the revisions dir.
    file: String,
    /// Capture time, epoch milliseconds.
    ts: i64,
}

/// Metadata about a snapshot, surfaced to the UI.
#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RevisionMeta {
    pub id: String,
    pub path: String,
    pub ts: i64,
}

/// A per-vault revision store rooted at `dir`.
pub struct Revisions {
    dir: PathBuf,
}

impl Revisions {
    pub fn new(dir: PathBuf) -> Self {
        Revisions { dir }
    }

    fn manifest_path(&self) -> PathBuf {
        self.dir.join("manifest.json")
    }

    fn load_manifest(&self) -> Vec<RevEntry> {
        match std::fs::read_to_string(self.manifest_path()) {
            Ok(raw) => serde_json::from_str(&raw).unwrap_or_default(),
            Err(_) => Vec::new(),
        }
    }

    fn save_manifest(&self, entries: &[RevEntry]) -> Result<(), String> {
        std::fs::create_dir_all(&self.dir)
            .map_err(|e| format!("Could not create revisions dir: {e}"))?;
        let raw = serde_json::to_string_pretty(entries)
            .map_err(|e| format!("Could not serialize manifest: {e}"))?;
        std::fs::write(self.manifest_path(), raw)
            .map_err(|e| format!("Could not write revisions manifest: {e}"))
    }

    /// Snapshots `content` (the note's current text) as a revision of `rel`,
    /// pruning to [`MAX_PER_VAULT`]. Returns the new revision id.
    pub fn snapshot(&self, rel: &str, content: &str) -> Result<String, String> {
        std::fs::create_dir_all(&self.dir)
            .map_err(|e| format!("Could not create revisions dir: {e}"))?;

        let ts = now_millis();
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let id = format!("{ts}-{seq}");
        let file = format!("{id}-{}.md", sanitize(rel));
        std::fs::write(self.dir.join(&file), content)
            .map_err(|e| format!("Could not write snapshot: {e}"))?;

        let mut entries = self.load_manifest();
        entries.push(RevEntry {
            id: id.clone(),
            path: rel.to_string(),
            file,
            ts,
        });
        self.prune(&mut entries);
        self.save_manifest(&entries)?;
        Ok(id)
    }

    /// Removes oldest entries (and their files) beyond the cap.
    fn prune(&self, entries: &mut Vec<RevEntry>) {
        if entries.len() <= MAX_PER_VAULT {
            return;
        }
        // Keep insertion order (already chronological); drop from the front.
        let drop = entries.len() - MAX_PER_VAULT;
        for e in entries.drain(..drop) {
            let _ = std::fs::remove_file(self.dir.join(&e.file));
        }
    }

    /// Lists snapshots for `rel`, newest first.
    pub fn list(&self, rel: &str) -> Vec<RevisionMeta> {
        let mut out: Vec<RevisionMeta> = self
            .load_manifest()
            .into_iter()
            .filter(|e| e.path == rel)
            .map(|e| RevisionMeta {
                id: e.id,
                path: e.path,
                ts: e.ts,
            })
            .collect();
        out.sort_by(|a, b| b.ts.cmp(&a.ts).then_with(|| b.id.cmp(&a.id)));
        out
    }

    /// Resolves a revision id to `(original_path, snapshotted_content)`.
    pub fn get(&self, id: &str) -> Result<(String, String), String> {
        let entry = self
            .load_manifest()
            .into_iter()
            .find(|e| e.id == id)
            .ok_or_else(|| format!("No such revision: {id}"))?;
        let content = std::fs::read_to_string(self.dir.join(&entry.file))
            .map_err(|e| format!("Could not read snapshot: {e}"))?;
        Ok((entry.path, content))
    }
}

// ---------------------------------------------------------------------------
// Handle-backed revisions (encrypted vaults)
//
// For an encrypted vault, snapshots must NOT be written to the app-data
// plaintext store — that would leak note content. Instead they live under a
// dot-prefixed `.revisions/` prefix INSIDE the vault, written through the active
// handle so they are encrypted exactly like every other note (and hidden from
// the tree + search, since scan and indexing skip dot-prefixed paths). The
// manifest is a `.revisions/manifest.json` note. These free functions mirror the
// [`Revisions`] API but take `&AppState` and dispatch through `with_active`.
// ---------------------------------------------------------------------------

#[cfg(any(feature = "encryption", feature = "provider-tinylord"))]
mod handle_backed {
    use super::*;
    use crate::index::AppState;
    use crate::vault::with_active;

    const REV_DIR: &str = ".revisions";

    fn manifest_rel() -> String {
        format!("{REV_DIR}/manifest.json")
    }

    fn load_manifest(state: &AppState) -> Vec<RevEntry> {
        match with_active(state, |h| h.read_note(&manifest_rel())) {
            Ok(raw) => serde_json::from_str(&raw).unwrap_or_default(),
            Err(_) => Vec::new(),
        }
    }

    fn save_manifest(state: &AppState, entries: &[RevEntry]) -> Result<(), String> {
        let raw = serde_json::to_string_pretty(entries)
            .map_err(|e| format!("Could not serialize manifest: {e}"))?;
        with_active(state, |h| h.write_note(state, &manifest_rel(), &raw))
    }

    /// Snapshots `content` as a revision of `rel`, encrypted inside the vault.
    pub fn snapshot(state: &AppState, rel: &str, content: &str) -> Result<String, String> {
        let ts = now_millis();
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let id = format!("{ts}-{seq}");
        let file = format!("{id}-{}.md", sanitize(rel));
        let file_rel = format!("{REV_DIR}/{file}");
        with_active(state, |h| h.write_note(state, &file_rel, content))?;

        let mut entries = load_manifest(state);
        entries.push(RevEntry {
            id: id.clone(),
            path: rel.to_string(),
            file,
            ts,
        });
        prune(state, &mut entries);
        save_manifest(state, &entries)?;
        Ok(id)
    }

    /// Drops oldest snapshots (and trashes their encrypted files) beyond the cap.
    fn prune(state: &AppState, entries: &mut Vec<RevEntry>) {
        if entries.len() <= MAX_PER_VAULT {
            return;
        }
        let drop = entries.len() - MAX_PER_VAULT;
        for e in entries.drain(..drop) {
            let rel = format!("{REV_DIR}/{}", e.file);
            let _ = with_active(state, |h| h.trash(state, &rel));
        }
    }

    /// Lists snapshots for `rel`, newest first.
    pub fn list(state: &AppState, rel: &str) -> Vec<RevisionMeta> {
        let mut out: Vec<RevisionMeta> = load_manifest(state)
            .into_iter()
            .filter(|e| e.path == rel)
            .map(|e| RevisionMeta {
                id: e.id,
                path: e.path,
                ts: e.ts,
            })
            .collect();
        out.sort_by(|a, b| b.ts.cmp(&a.ts).then_with(|| b.id.cmp(&a.id)));
        out
    }

    /// Resolves a revision id to `(original_path, snapshotted_content)`.
    pub fn get(state: &AppState, id: &str) -> Result<(String, String), String> {
        let entry = load_manifest(state)
            .into_iter()
            .find(|e| e.id == id)
            .ok_or_else(|| format!("No such revision: {id}"))?;
        let content = with_active(state, |h| h.read_note(&format!("{REV_DIR}/{}", entry.file)))?;
        Ok((entry.path, content))
    }
}

#[cfg(any(feature = "encryption", feature = "provider-tinylord"))]
pub use handle_backed::{
    get as handle_get, list as handle_list, snapshot as handle_snapshot,
};

/// Turns a vault-relative path into a filesystem-safe, length-capped slug.
fn sanitize(rel: &str) -> String {
    let mut s: String = rel
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if s.chars().count() > 80 {
        s = s.chars().take(80).collect();
    }
    s
}

fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir(tag: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("jaynotes-rev-{tag}-{nanos}"));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn snapshot_list_and_get_roundtrip() {
        let dir = temp_dir("roundtrip");
        let rev = Revisions::new(dir.clone());
        let id = rev.snapshot("notes/a.md", "original body").unwrap();

        let list = rev.list("notes/a.md");
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, id);
        assert_eq!(list[0].path, "notes/a.md");

        let (path, content) = rev.get(&id).unwrap();
        assert_eq!(path, "notes/a.md");
        assert_eq!(content, "original body");

        // A different note has no revisions.
        assert!(rev.list("notes/b.md").is_empty());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn prunes_to_cap_removing_oldest_files() {
        let dir = temp_dir("prune");
        let rev = Revisions::new(dir.clone());
        let mut ids = Vec::new();
        for i in 0..(MAX_PER_VAULT + 5) {
            ids.push(rev.snapshot("n.md", &format!("v{i}")).unwrap());
        }
        let list = rev.list("n.md");
        assert_eq!(list.len(), MAX_PER_VAULT, "trail capped at the max");

        // The five oldest ids are gone (files + manifest entries).
        for old in &ids[..5] {
            assert!(rev.get(old).is_err(), "pruned revision {old} should be gone");
        }
        // The newest is still retrievable.
        assert!(rev.get(ids.last().unwrap()).is_ok());

        // Files on disk should number MAX (+ manifest.json).
        let md_files = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().map(|x| x == "md").unwrap_or(false))
            .count();
        assert_eq!(md_files, MAX_PER_VAULT);

        std::fs::remove_dir_all(&dir).ok();
    }
}
