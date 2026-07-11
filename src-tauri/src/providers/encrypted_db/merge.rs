//! Pure merge logic for the live-DB ↔ snapshot reconciliation.
//!
//! The live working container is "local"; an externally-changed snapshot (or a
//! Syncthing `*.sync-conflict-*` sibling) is "remote". This module decides, per
//! note/attachment, what to write — with **no** database or filesystem access —
//! so the whole policy is exhaustively unit-testable. `base_time` is the epoch
//! (ms) at which we last exported: an item whose `mtime` exceeds it changed on
//! that side since the export.
//!
//! Policy:
//! * present only on one side → keep/import that side (never lose data);
//! * changed on **both** sides since the last export, and differing → the newer
//!   mtime wins; the loser (if it still has content) is preserved as a
//!   `… (conflict <date>)` copy;
//! * otherwise → newer mtime wins outright (a tombstone is just a versioned
//!   event, so a newer delete beats a stale edit and vice-versa).

/// One note or attachment as seen on one side. For an attachment, `content`
/// carries the bytes as an opaque string key is not used — see [`Item`] usage in
/// the container; here we stay generic over a comparable payload.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    pub path: String,
    /// Note text, or (for attachments) a stable digest/marker of the bytes.
    pub content: String,
    /// Last-write time, epoch milliseconds.
    pub mtime: i64,
    /// Tombstone flag.
    pub deleted: bool,
}

/// What to apply for one path after merging local vs. remote.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MergePlan {
    /// Record to upsert at `path`, or `None` when local already holds the right
    /// state (no write needed).
    pub write: Option<Item>,
    /// A loser record to preserve under a conflict path (never a tombstone).
    pub conflict: Option<Item>,
}

impl MergePlan {
    fn noop() -> Self {
        MergePlan {
            write: None,
            conflict: None,
        }
    }
    fn take(item: Item) -> Self {
        MergePlan {
            write: Some(item),
            conflict: None,
        }
    }
}

/// Decides the merge for one path given the local and remote views and the
/// last-export baseline time.
pub fn merge_item(local: Option<&Item>, remote: Option<&Item>, base_time: i64) -> MergePlan {
    match (local, remote) {
        (None, None) => MergePlan::noop(),
        // New from elsewhere → import it.
        (None, Some(r)) => MergePlan::take(r.clone()),
        // Absent from the snapshot → keep local (never destroy unsynced local data).
        (Some(_), None) => MergePlan::noop(),
        (Some(l), Some(r)) => {
            // Identical state → nothing to do.
            if l.content == r.content && l.deleted == r.deleted {
                return MergePlan::noop();
            }
            let local_changed = l.mtime > base_time;
            let remote_changed = r.mtime > base_time;

            if local_changed && remote_changed {
                // Genuine divergence: newer wins, loser kept as a conflict copy.
                let (winner, loser) = if r.mtime >= l.mtime { (r, l) } else { (l, r) };
                let write = if winner == l {
                    None // local already is the winner
                } else {
                    Some(winner.clone())
                };
                // Only preserve a loser that still has content (not a tombstone).
                let conflict = if loser.deleted {
                    None
                } else {
                    Some(loser.clone())
                };
                MergePlan { write, conflict }
            } else {
                // Only one side (or neither, per base) changed → newer wins, no copy.
                if r.mtime > l.mtime {
                    MergePlan::take(r.clone())
                } else {
                    MergePlan::noop()
                }
            }
        }
    }
}

/// Builds the conflict-copy path for a losing note: `stem (conflict <date>).md`
/// (or `.<ext>` for a non-md attachment). `date` is a `YYYY-MM-DD` string.
/// `taken` reports whether a candidate path is already occupied, so repeated
/// conflicts on the same day get ` 2`, ` 3`, … suffixes.
pub fn conflict_path(path: &str, date: &str, taken: &dyn Fn(&str) -> bool) -> String {
    let (dir, name) = match path.rsplit_once('/') {
        Some((d, n)) => (format!("{d}/"), n.to_string()),
        None => (String::new(), path.to_string()),
    };
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) => (s.to_string(), format!(".{e}")),
        None => (name.clone(), String::new()),
    };
    let base = format!("{dir}{stem} (conflict {date}){ext}");
    if !taken(&base) {
        return base;
    }
    for n in 2..10_000 {
        let candidate = format!("{dir}{stem} (conflict {date} {n}){ext}");
        if !taken(&candidate) {
            return candidate;
        }
    }
    base
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(path: &str, content: &str, mtime: i64, deleted: bool) -> Item {
        Item {
            path: path.into(),
            content: content.into(),
            mtime,
            deleted,
        }
    }

    #[test]
    fn remote_only_is_imported() {
        let r = item("a.md", "hi", 100, false);
        let plan = merge_item(None, Some(&r), 50);
        assert_eq!(plan.write, Some(r));
        assert_eq!(plan.conflict, None);
    }

    #[test]
    fn local_only_is_kept() {
        let l = item("a.md", "hi", 100, false);
        let plan = merge_item(Some(&l), None, 50);
        assert_eq!(plan, MergePlan::noop());
    }

    #[test]
    fn identical_is_noop() {
        let l = item("a.md", "same", 100, false);
        let r = item("a.md", "same", 120, false);
        assert_eq!(merge_item(Some(&l), Some(&r), 50), MergePlan::noop());
    }

    #[test]
    fn newer_remote_wins_when_only_remote_changed() {
        // base=100: local unchanged (mtime 90 ≤ 100), remote changed (mtime 150).
        let l = item("a.md", "old", 90, false);
        let r = item("a.md", "new", 150, false);
        let plan = merge_item(Some(&l), Some(&r), 100);
        assert_eq!(plan.write, Some(r));
        assert_eq!(plan.conflict, None);
    }

    #[test]
    fn both_changed_newer_wins_loser_becomes_conflict() {
        // base=100: both edited after export; remote newer → remote wins,
        // local preserved as the conflict copy.
        let l = item("a.md", "local edit", 120, false);
        let r = item("a.md", "remote edit", 150, false);
        let plan = merge_item(Some(&l), Some(&r), 100);
        assert_eq!(plan.write, Some(r));
        assert_eq!(plan.conflict, Some(l));
    }

    #[test]
    fn both_changed_local_newer_keeps_local_and_copies_remote() {
        let l = item("a.md", "local edit", 200, false);
        let r = item("a.md", "remote edit", 150, false);
        let plan = merge_item(Some(&l), Some(&r), 100);
        assert_eq!(plan.write, None, "local already the winner");
        assert_eq!(plan.conflict, Some(r));
    }

    #[test]
    fn newer_tombstone_beats_stale_edit() {
        // base=100: remote deleted at 160 (newer) beats local edit at 130.
        let l = item("a.md", "edited", 130, false);
        let r = item("a.md", "", 160, true);
        let plan = merge_item(Some(&l), Some(&r), 100);
        // Only one relevant delta here treated as newer-wins → take tombstone,
        // but both changed since base → conflict branch: winner is the tombstone,
        // loser local has content → kept as conflict copy so an edit isn't lost.
        assert_eq!(plan.write, Some(r));
        assert_eq!(plan.conflict, Some(l));
    }

    #[test]
    fn stale_tombstone_loses_to_newer_edit() {
        // Remote tombstone is older than local edit → local edit wins, no data lost.
        let l = item("a.md", "edited", 200, false);
        let r = item("a.md", "", 150, true);
        let plan = merge_item(Some(&l), Some(&r), 100);
        assert_eq!(plan.write, None); // local (the edit) stays
        assert_eq!(plan.conflict, None); // loser is a tombstone → nothing to preserve
    }

    #[test]
    fn conflict_path_appends_date_and_dedups() {
        let none = |_: &str| false;
        assert_eq!(
            conflict_path("notes/foo.md", "2026-07-11", &none),
            "notes/foo (conflict 2026-07-11).md"
        );
        assert_eq!(
            conflict_path("foo.md", "2026-07-11", &none),
            "foo (conflict 2026-07-11).md"
        );
        // Attachment (png) keeps its extension.
        assert_eq!(
            conflict_path("attachments/x.png", "2026-07-11", &none),
            "attachments/x (conflict 2026-07-11).png"
        );
        // Collision on the same day bumps the counter.
        let taken = |p: &str| p == "foo (conflict 2026-07-11).md";
        assert_eq!(
            conflict_path("foo.md", "2026-07-11", &taken),
            "foo (conflict 2026-07-11 2).md"
        );
    }
}
