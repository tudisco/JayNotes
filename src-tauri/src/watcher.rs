//! Recursive vault file watcher.
//!
//! On each vault switch a fresh [`WatcherHandle`] replaces the previous one;
//! dropping the old handle stops its background thread. Filesystem events are
//! debounced (~500ms) by `notify-debouncer-full`, then each batch is reduced to
//! the set of affected `.md` paths, the index is updated, and a single
//! `vault-changed` event is emitted to the frontend.
//!
//! ## Self-write suppression
//!
//! The app's own writes would otherwise echo back as watcher events and trigger
//! a pointless reindex (and, worse, a reload that could stomp the editor). Every
//! vault command that writes registers `(rel_path, Instant::now())` in the
//! shared `recent_writes` map (see [`crate::index::register_write`]); this
//! watcher ignores any event for a path written within the last 2 seconds.

use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use notify_debouncer_full::notify::RecursiveMode;
use notify_debouncer_full::{new_debouncer, DebounceEventResult, Debouncer, RecommendedCache};
use serde::Serialize;
use tauri::Emitter;

use crate::index::{rel_is_hidden, Index};

/// How long a self-write suppresses matching watcher events.
const SUPPRESS_WINDOW: Duration = Duration::from_secs(2);
/// Entries older than this are pruned from the recent-writes map.
const PRUNE_AFTER: Duration = Duration::from_secs(5);

type SharedIndex = Arc<Mutex<Option<Index>>>;
type RecentWrites = Arc<Mutex<HashMap<String, Instant>>>;

/// Owns the running debouncer; dropping it stops watching.
pub struct WatcherHandle {
    _debouncer: Debouncer<notify_debouncer_full::notify::RecommendedWatcher, RecommendedCache>,
}

#[derive(Debug, Serialize, Clone)]
struct ChangePayload {
    paths: Vec<String>,
}

/// Starts watching `vault_root` recursively. The returned handle must be kept
/// alive for watching to continue.
pub fn start_watcher(
    app: tauri::AppHandle,
    index: SharedIndex,
    recent: RecentWrites,
    vault_root: PathBuf,
) -> Result<WatcherHandle, String> {
    let watch_root = vault_root.clone();
    let mut debouncer = new_debouncer(
        Duration::from_millis(500),
        None,
        move |result: DebounceEventResult| {
            if let Ok(events) = result {
                let paths: Vec<PathBuf> =
                    events.into_iter().flat_map(|e| e.event.paths.clone()).collect();
                process_batch(&app, &index, &recent, &vault_root, paths);
            }
        },
    )
    .map_err(|e| format!("Could not create file watcher: {e}"))?;

    debouncer
        .watch(&watch_root, RecursiveMode::Recursive)
        .map_err(|e| format!("Could not watch vault: {e}"))?;

    Ok(WatcherHandle {
        _debouncer: debouncer,
    })
}

/// Reduces a debounced batch of absolute paths to affected `.md` notes, applies
/// index updates, and emits one `vault-changed` event.
fn process_batch(
    app: &tauri::AppHandle,
    index: &SharedIndex,
    recent: &RecentWrites,
    vault_root: &Path,
    paths: Vec<PathBuf>,
) {
    let mut changed: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    for abs in paths {
        if !is_markdown(&abs) {
            continue;
        }
        let rel = match to_rel_string(vault_root, &abs) {
            Some(r) if !r.is_empty() => r,
            _ => continue,
        };
        if rel_is_hidden(&rel) {
            continue;
        }
        if !seen.insert(rel.clone()) {
            continue; // already handled this path in this batch
        }
        if recently_written(recent, &rel) {
            continue; // our own write; ignore the echo
        }

        // Existence decides create/modify vs. remove. A rename shows up as a
        // remove of the old path and a create of the new one — handled as such.
        if let Ok(guard) = index.lock() {
            if let Some(idx) = guard.as_ref() {
                let _ = if abs.exists() {
                    match std::fs::read_to_string(&abs) {
                        Ok(content) => idx.index_file(&rel, &content),
                        Err(_) => Ok(()),
                    }
                } else {
                    idx.remove_file(&rel)
                };
            }
        }
        changed.push(rel);
    }

    if !changed.is_empty() {
        let _ = app.emit("vault-changed", ChangePayload { paths: changed });
    }
}

/// True if `rel` was written by the app within [`SUPPRESS_WINDOW`]. Also prunes
/// entries older than [`PRUNE_AFTER`] so the map can't grow unbounded.
fn recently_written(recent: &RecentWrites, rel: &str) -> bool {
    let now = Instant::now();
    let mut map = match recent.lock() {
        Ok(m) => m,
        Err(_) => return false,
    };
    map.retain(|_, t| now.duration_since(*t) < PRUNE_AFTER);
    map.get(rel)
        .map(|t| now.duration_since(*t) < SUPPRESS_WINDOW)
        .unwrap_or(false)
}

fn is_markdown(path: &Path) -> bool {
    path.extension()
        .map(|e| e.eq_ignore_ascii_case("md"))
        .unwrap_or(false)
}

// ---------------------------------------------------------------------------
// Encrypted-files watcher
//
// The backing dir holds ciphertext files, so events carry CIPHERTEXT paths.
// Self-write suppression therefore keys on the ciphertext relative path (the
// handle registers ciphertext paths). Surviving events are mapped to plaintext
// via the session cipher before reindexing / emitting `vault-changed`. A locked
// vault has `index == None`, so events are simply dropped.
// ---------------------------------------------------------------------------

/// Starts watching the ciphertext `backing` dir for an encrypted-files vault,
/// decrypting names to keep the keyed index and the UI in sync.
#[cfg(feature = "provider-encrypted-files")]
pub fn start_crypt_watcher(
    app: tauri::AppHandle,
    index: SharedIndex,
    recent: RecentWrites,
    backing: PathBuf,
    cipher: std::sync::Arc<crate::providers::encrypted_files::cipher::CryptCipher>,
) -> Result<WatcherHandle, String> {
    let watch_root = backing.clone();
    let mut debouncer = new_debouncer(
        Duration::from_millis(500),
        None,
        move |result: DebounceEventResult| {
            if let Ok(events) = result {
                let paths: Vec<PathBuf> =
                    events.into_iter().flat_map(|e| e.event.paths.clone()).collect();
                process_crypt_batch(&app, &index, &recent, &backing, &cipher, paths);
            }
        },
    )
    .map_err(|e| format!("Could not create file watcher: {e}"))?;

    debouncer
        .watch(&watch_root, RecursiveMode::Recursive)
        .map_err(|e| format!("Could not watch vault: {e}"))?;

    Ok(WatcherHandle {
        _debouncer: debouncer,
    })
}

/// Ciphertext-aware batch processor: maps backing paths to plaintext note rels,
/// reindexes decrypted content, and emits one `vault-changed` with plaintext
/// paths.
#[cfg(feature = "provider-encrypted-files")]
fn process_crypt_batch(
    app: &tauri::AppHandle,
    index: &SharedIndex,
    recent: &RecentWrites,
    backing: &Path,
    cipher: &crate::providers::encrypted_files::cipher::CryptCipher,
    paths: Vec<PathBuf>,
) {
    let mut changed: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    for abs in paths {
        // Suppression keys on the CIPHERTEXT relative path (what the handle
        // registered). Skip our own writes and dot-prefixed backing entries.
        let ct_rel = match to_rel_string(backing, &abs) {
            Some(r) if !r.is_empty() => r,
            _ => continue,
        };
        if ct_rel.split('/').any(|p| p.starts_with('.')) {
            continue; // tmp files, probe, trash sink
        }
        if recently_written(recent, &ct_rel) {
            continue;
        }

        // Map ciphertext → plaintext rel (segment-by-segment). Undecryptable →
        // stray, skip.
        let plain_rel = match decrypt_ct_rel(cipher, &ct_rel) {
            Some(r) => r,
            None => continue,
        };
        if !is_markdown_name(&plain_rel) || crate::index::rel_is_hidden(&plain_rel) {
            continue;
        }
        if !seen.insert(plain_rel.clone()) {
            continue;
        }

        if let Ok(guard) = index.lock() {
            if let Some(idx) = guard.as_ref() {
                let _ = if abs.exists() {
                    match std::fs::read(&abs).ok().and_then(|ct| cipher.decrypt_content(&ct).ok()) {
                        Some(bytes) => match String::from_utf8(bytes) {
                            Ok(content) => idx.index_file(&plain_rel, &content),
                            Err(_) => Ok(()),
                        },
                        None => Ok(()),
                    }
                } else {
                    idx.remove_file(&plain_rel)
                };
            }
        }
        changed.push(plain_rel);
    }

    if !changed.is_empty() {
        let _ = app.emit("vault-changed", ChangePayload { paths: changed });
    }
}

/// Decrypts a ciphertext relative path to its plaintext note rel, or `None` if
/// any segment is a stray.
#[cfg(feature = "provider-encrypted-files")]
fn decrypt_ct_rel(
    cipher: &crate::providers::encrypted_files::cipher::CryptCipher,
    ct_rel: &str,
) -> Option<String> {
    let mut parts = Vec::new();
    for seg in ct_rel.split('/') {
        parts.push(cipher.decrypt_backing_name(seg).ok()?);
    }
    Some(parts.join("/"))
}

#[cfg(feature = "provider-encrypted-files")]
fn is_markdown_name(rel: &str) -> bool {
    rel.rsplit('/')
        .next()
        .map(|n| n.to_ascii_lowercase().ends_with(".md"))
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
