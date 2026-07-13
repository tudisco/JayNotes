//! Cross-vault note transfer — moving a note (and the attachments it references)
//! from the active vault into *another* configured vault, e.g. lifting a
//! passwords note out of a plain folder into an encrypted vault.
//!
//! ## Copy-then-delete ordering (never lose data)
//!
//! The transfer always **copies into the destination first, and only trashes the
//! source note last**. Any failure before that final step leaves the source
//! untouched; the worst case is a note that briefly exists in both vaults, never
//! one that exists in neither. The collision guard (below) also runs before any
//! mutation, so a name clash changes nothing at all.
//!
//! ## Attachments
//!
//! The referenced attachments in the note body are copied into the destination's
//! `attachments/` folder. On a name collision the bytes are compared: identical
//! content reuses the existing file (no duplicate, link unchanged); different
//! content is written under a uniquified `name-1.ext` and the link in the
//! transferred copy is rewritten to match. Attachments are only ever **copied**,
//! never removed from the source — other notes there may still reference them.
//!
//! ## Destination handle lifecycle
//!
//! The destination is opened as a *temporary secondary handle*
//! ([`crate::providers::open_secondary_handle`]) that is dropped as soon as the
//! transfer returns. For an encrypted-db destination that `Drop` flushes a fresh
//! snapshot, so the written note lands in the on-disk `.jaynotes` file even
//! though the vault was never the active one. All destination writes are routed
//! through a throwaway [`AppState`] so they can never touch the *active* vault's
//! live search index or its watcher-suppression set — the destination's own
//! index refreshes on its next real open (an accepted, documented cost).

use crate::index::AppState;
use crate::providers::VaultHandle;
use crate::vault::{load_settings, TreeNode};

/// Moves the note at `note_path` (in the active vault) into `dest_vault_id` under
/// `dest_folder` (`""` = destination root). Returns the note's new
/// vault-relative path in the destination.
///
/// Errors:
/// - `"dest-locked"` — the destination is an encrypted vault that isn't unlocked
///   (the frontend prompts for unlock and retries); nothing is changed.
/// - a collision message — a note of that name already exists in the destination
///   (no overwrite); nothing is changed.
#[tauri::command]
pub async fn transfer_note(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    note_path: String,
    dest_vault_id: String,
    dest_folder: String,
) -> Result<String, String> {
    let settings = load_settings(&app)?;
    let dest_vault = settings
        .vaults
        .iter()
        .find(|v| v.id == dest_vault_id)
        .ok_or("No such destination vault")?
        .clone();

    // Open the destination as a temporary secondary handle (may be "dest-locked"
    // or a clear "unsupported destination" error) BEFORE touching the source.
    let dest = crate::providers::open_secondary_handle(&app, &dest_vault)?;

    let st = state.inner();
    let guard = st.active.lock().unwrap();
    let source = guard
        .as_deref()
        .ok_or("No vault is open — it may need to be unlocked")?;

    let result = transfer_core(source, st, dest.as_ref(), &note_path, &dest_folder);

    drop(guard);
    // `dest` drops here → an encrypted-db handle flushes its final snapshot.
    result
}

/// Lists the destination-vault folder paths (root excluded; the caller prepends
/// its own "(vault root)" entry), for the transfer picker's second step. Opens a
/// temporary secondary handle, so a locked encrypted vault errors with
/// `"dest-locked"`.
#[tauri::command]
pub async fn list_vault_folders(
    app: tauri::AppHandle,
    id: String,
) -> Result<Vec<String>, String> {
    let settings = load_settings(&app)?;
    let vault = settings
        .vaults
        .iter()
        .find(|v| v.id == id)
        .ok_or("No such vault")?
        .clone();
    let handle = crate::providers::open_secondary_handle(&app, &vault)?;
    let tree = handle.scan_tree()?;
    Ok(collect_folder_paths(&tree))
}

/// Unlocks a **transfer destination** vault in place: it caches the credential
/// in the unlock session so [`open_secondary_handle`] can open it, *without*
/// making it the active backend (the source vault must stay active). Dispatches
/// by kind like the main unlock command. Present whenever a provider that needs
/// unlocking (encrypted or hosted `tinylord`) is compiled in.
///
/// [`open_secondary_handle`]: crate::providers::open_secondary_handle
#[cfg(any(feature = "encryption", feature = "provider-tinylord"))]
#[tauri::command]
pub async fn unlock_transfer_dest(
    app: tauri::AppHandle,
    id: String,
    password: String,
    extra: Option<std::collections::HashMap<String, String>>,
    remember: bool,
) -> Result<(), String> {
    #[allow(unused_imports)]
    use crate::vault::VaultKind;
    #[allow(unused_imports)]
    use tauri::Manager;

    let settings = load_settings(&app)?;
    let vault = settings
        .vaults
        .iter()
        .find(|v| v.id == id)
        .ok_or("No such vault")?
        .clone();
    let _ = (&app, &password, &extra, remember);
    match vault.kind {
        #[cfg(feature = "provider-encrypted-db")]
        VaultKind::EncryptedDb => {
            let session = app.state::<crate::providers::crypto::SecretsSession>();
            crate::providers::encrypted_db::unlock_session_only(
                &app, &session, &vault, &password, remember,
            )
        }
        #[cfg(feature = "provider-encrypted-files")]
        VaultKind::EncryptedFiles => {
            let session = app.state::<crate::providers::crypto::SecretsSession>();
            let password2 = extra
                .as_ref()
                .and_then(|m| m.get("password2"))
                .cloned()
                .unwrap_or_default();
            crate::providers::encrypted_files::unlock_session_only(
                &app, &session, &vault, &password, &password2, remember,
            )
        }
        #[cfg(feature = "provider-tinylord")]
        VaultKind::Tinylord => {
            // Username: an explicit `extra` field wins; otherwise the one stored
            // in the vault's config at creation (the common case — the transfer
            // unlock panel only asks for the password).
            let username = extra
                .as_ref()
                .and_then(|m| m.get("username"))
                .cloned()
                .filter(|u| !u.trim().is_empty())
                .or_else(|| {
                    vault
                        .config
                        .get("username")
                        .and_then(|v| v.as_str())
                        .map(str::to_string)
                })
                .unwrap_or_default();
            let session = app.state::<crate::providers::tinylord::TinyLordSessions>();
            crate::providers::tinylord::unlock_session_only(
                &session, &vault, &username, &password, remember,
            )
        }
        _ => Err("This vault type can't be unlocked as a transfer destination".into()),
    }
}

/// The transfer's storage-only core: copy the note + referenced attachments into
/// `dest`, then trash the source note. Split out from the command so it can be
/// unit-tested with any pair of [`VaultHandle`]s over temp vaults.
///
/// `source_state` is the *active* vault's real state (its index is pruned when
/// the source note is trashed); destination writes use a throwaway state so they
/// never reach the active index.
pub(crate) fn transfer_core(
    source: &dyn VaultHandle,
    source_state: &AppState,
    dest: &dyn VaultHandle,
    note_path: &str,
    dest_folder: &str,
) -> Result<String, String> {
    // 1. Read the note from the source.
    let content = source.read_note(note_path)?;
    let file_name = note_path.rsplit('/').next().unwrap_or(note_path).to_string();
    if file_name.is_empty() {
        return Err(format!("Invalid note path: {note_path}"));
    }

    // 2. Destination path + collision guard (runs before ANY mutation, so a
    //    clash leaves both vaults exactly as they were).
    let dest_folder = dest_folder.trim_matches('/');
    let dest_rel = if dest_folder.is_empty() {
        file_name.clone()
    } else {
        format!("{dest_folder}/{file_name}")
    };
    if dest.read_note(&dest_rel).is_ok() {
        return Err(format!(
            "A note named '{file_name}' already exists in the destination vault"
        ));
    }

    // 3. Copy referenced attachments; rewrite links only when a differing-content
    //    collision forces a new name. Destination writes go through a throwaway
    //    state (inert index/watcher) so nothing leaks into the active vault.
    let scratch = AppState::default();
    let mut body = content.clone();
    for raw_ref in crate::index::extract_attachment_refs(&content) {
        let decoded = percent_decode(&raw_ref);
        let name = decoded.rsplit('/').next().unwrap_or(&decoded);
        // Only copy things the destination will accept as an attachment (images).
        let (base, ext) = match crate::vault::sanitize_attachment_name(name) {
            Ok(pair) => pair,
            Err(_) => continue,
        };
        let src_bytes = match source.read_attachment(&decoded) {
            Ok(b) => b,
            Err(_) => continue, // broken/foreign ref — leave the link as-is
        };
        // Deterministic path the destination would give this exact name.
        let candidate_rel = format!("attachments/{base}.{ext}");
        // Same-content reuse: keep the existing file and the original link.
        if let Ok(existing) = dest.read_attachment(&candidate_rel) {
            if existing == src_bytes {
                continue;
            }
        }
        // Write it (auto-uniquified on any collision). Rewrite the link when the
        // stored path differs from what the note referenced.
        let written = dest.save_attachment(&scratch, name, &src_bytes)?;
        if written != raw_ref {
            body = rewrite_link(&body, &raw_ref, &written);
        }
    }

    // 4. Write the note copy into the destination — must succeed before we
    //    remove the source (copy-then-delete).
    dest.write_note(&scratch, &dest_rel, &body)?;

    // 5. Trash the source note (updates the source index; attachments are kept).
    source.trash(source_state, note_path)?;

    Ok(dest_rel)
}

/// Rewrites a Markdown link/image target from `old_target` to `new_rel`,
/// covering both the bare `](target)` and angle-bracketed `](<target>)` forms.
/// A `new_rel` containing spaces is emitted angle-bracketed so the link stays
/// valid.
fn rewrite_link(body: &str, old_target: &str, new_rel: &str) -> String {
    let repr = if new_rel.contains(' ') {
        format!("<{new_rel}>")
    } else {
        new_rel.to_string()
    };
    // Angle-bracketed form first (the bare form is a substring of it).
    body.replace(&format!("](<{old_target}>)"), &format!("]({repr})"))
        .replace(&format!("]({old_target})"), &format!("]({repr})"))
}

/// Decodes `%XX` percent-escapes in a link target so the bytes can be located on
/// disk. Invalid escapes are passed through verbatim. Deliberately minimal — it
/// only needs to undo the encoding the editor applies to attachment links.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = (bytes[i + 1] as char).to_digit(16);
            let lo = (bytes[i + 2] as char).to_digit(16);
            if let (Some(h), Some(l)) = (hi, lo) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(out).unwrap_or_else(|_| s.to_string())
}

/// Collects every folder path in a scanned tree, depth-first, parent before
/// child, mirroring the frontend's `collectFolderPaths`. The tree's children are
/// already folders-first and alphabetically sorted, so iterating them in order
/// yields the same shape. The root itself (path "") is excluded.
fn collect_folder_paths(root: &TreeNode) -> Vec<String> {
    fn walk(node: &TreeNode, out: &mut Vec<String>) {
        for child in &node.children {
            if child.is_dir {
                out.push(child.path.clone());
                walk(child, out);
            }
        }
    }
    let mut out = Vec::new();
    walk(root, &mut out);
    out
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::plain::PlainHandle;
    use crate::providers::VaultHandle;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir(tag: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "jaynotes-transfer-{tag}-{}-{nanos}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A 1x1 PNG-ish byte blob (content doesn't need to be a real image — the
    /// attachment path only validates the extension, not the bytes).
    fn png(marker: u8) -> Vec<u8> {
        vec![0x89, b'P', b'N', b'G', marker, 0x0a]
    }

    fn plain_handle(root: &Path) -> PlainHandle {
        PlainHandle::new(&root.canonicalize().unwrap())
    }

    #[test]
    fn plain_to_plain_moves_note_and_keeps_source_attachment() {
        let src = temp_dir("p2p-src");
        let dst = temp_dir("p2p-dst");
        let src_state = AppState::default();

        fs::write(
            src.join("secret.md"),
            "# Passwords\n\n![shot](attachments/img.png)\n",
        )
        .unwrap();
        fs::create_dir_all(src.join("attachments")).unwrap();
        fs::write(src.join("attachments/img.png"), png(1)).unwrap();

        let source = plain_handle(&src);
        let dest = plain_handle(&dst);

        let dest_rel = transfer_core(&source, &src_state, &dest, "secret.md", "").unwrap();
        assert_eq!(dest_rel, "secret.md");

        // The note + attachment arrived in the destination.
        assert!(dst.join("secret.md").is_file());
        assert_eq!(fs::read(dst.join("attachments/img.png")).unwrap(), png(1));

        // The source note was trashed (moved into the hidden test `.trash`).
        assert!(!src.join("secret.md").is_file());
        // The source attachment is KEPT (other notes might reference it).
        assert!(src.join("attachments/img.png").is_file());

        fs::remove_dir_all(&src).ok();
        fs::remove_dir_all(&dst).ok();
    }

    #[test]
    fn collision_errors_and_changes_nothing() {
        let src = temp_dir("coll-src");
        let dst = temp_dir("coll-dst");
        let src_state = AppState::default();

        fs::write(src.join("note.md"), "source body\n").unwrap();
        fs::write(dst.join("note.md"), "existing dest body\n").unwrap();

        let source = plain_handle(&src);
        let dest = plain_handle(&dst);

        let err = transfer_core(&source, &src_state, &dest, "note.md", "").unwrap_err();
        assert!(err.contains("already exists"), "got: {err}");

        // Nothing changed: source note still present, dest note untouched.
        assert!(src.join("note.md").is_file());
        assert_eq!(
            fs::read_to_string(dst.join("note.md")).unwrap(),
            "existing dest body\n"
        );

        fs::remove_dir_all(&src).ok();
        fs::remove_dir_all(&dst).ok();
    }

    #[test]
    fn attachment_same_content_is_reused_without_duplicate() {
        let src = temp_dir("same-src");
        let dst = temp_dir("same-dst");
        let src_state = AppState::default();

        fs::create_dir_all(src.join("attachments")).unwrap();
        fs::write(src.join("attachments/img.png"), png(7)).unwrap();
        fs::write(src.join("note.md"), "![x](attachments/img.png)\n").unwrap();

        // Destination already holds the identical attachment.
        fs::create_dir_all(dst.join("attachments")).unwrap();
        fs::write(dst.join("attachments/img.png"), png(7)).unwrap();

        let source = plain_handle(&src);
        let dest = plain_handle(&dst);

        transfer_core(&source, &src_state, &dest, "note.md", "").unwrap();

        // No duplicate created; the link is unchanged.
        assert!(!dst.join("attachments/img-1.png").exists());
        assert_eq!(
            fs::read_to_string(dst.join("note.md")).unwrap(),
            "![x](attachments/img.png)\n"
        );

        fs::remove_dir_all(&src).ok();
        fs::remove_dir_all(&dst).ok();
    }

    #[test]
    fn attachment_diff_content_is_uniquified_and_link_rewritten() {
        let src = temp_dir("diff-src");
        let dst = temp_dir("diff-dst");
        let src_state = AppState::default();

        fs::create_dir_all(src.join("attachments")).unwrap();
        fs::write(src.join("attachments/img.png"), png(1)).unwrap();
        fs::write(src.join("note.md"), "![x](attachments/img.png)\n").unwrap();

        // Destination has a DIFFERENT file at the same name.
        fs::create_dir_all(dst.join("attachments")).unwrap();
        fs::write(dst.join("attachments/img.png"), png(2)).unwrap();

        let source = plain_handle(&src);
        let dest = plain_handle(&dst);

        transfer_core(&source, &src_state, &dest, "note.md", "").unwrap();

        // The incoming attachment was uniquified and both files survive.
        assert_eq!(fs::read(dst.join("attachments/img.png")).unwrap(), png(2));
        assert_eq!(fs::read(dst.join("attachments/img-1.png")).unwrap(), png(1));
        // The transferred note's link points at the new name.
        assert_eq!(
            fs::read_to_string(dst.join("note.md")).unwrap(),
            "![x](attachments/img-1.png)\n"
        );

        fs::remove_dir_all(&src).ok();
        fs::remove_dir_all(&dst).ok();
    }

    #[test]
    fn moves_into_a_destination_subfolder() {
        let src = temp_dir("sub-src");
        let dst = temp_dir("sub-dst");
        let src_state = AppState::default();
        fs::write(src.join("n.md"), "body\n").unwrap();
        fs::create_dir_all(dst.join("Archive")).unwrap();

        let source = plain_handle(&src);
        let dest = plain_handle(&dst);
        let rel = transfer_core(&source, &src_state, &dest, "n.md", "Archive").unwrap();
        assert_eq!(rel, "Archive/n.md");
        assert!(dst.join("Archive/n.md").is_file());

        fs::remove_dir_all(&src).ok();
        fs::remove_dir_all(&dst).ok();
    }

    #[test]
    fn collect_folder_paths_is_depth_first_parent_first() {
        let src = temp_dir("folders");
        fs::create_dir_all(src.join("Projects/Duke")).unwrap();
        fs::create_dir_all(src.join("Archive")).unwrap();
        fs::write(src.join("top.md"), "").unwrap();
        let handle = plain_handle(&src);
        let tree = handle.scan_tree().unwrap();
        let folders = collect_folder_paths(&tree);
        assert_eq!(folders, vec!["Archive", "Projects", "Projects/Duke"]);
        fs::remove_dir_all(&src).ok();
    }

    #[test]
    fn percent_decode_handles_escapes() {
        assert_eq!(percent_decode("attachments/my%20photo.png"), "attachments/my photo.png");
        assert_eq!(percent_decode("plain.png"), "plain.png");
        // A malformed escape is passed through untouched.
        assert_eq!(percent_decode("a%2.png"), "a%2.png");
    }

    /// plain → encrypted-db: the note lands inside the container and is readable
    /// through the destination handle; the source note is trashed.
    #[cfg(feature = "provider-encrypted-db")]
    #[test]
    fn plain_to_encrypted_db_writes_into_container() {
        use crate::providers::encrypted_db::{container::Container, EncryptedDbHandle};

        let src = temp_dir("p2e-src");
        let enc = temp_dir("p2e-enc");
        let src_state = AppState::default();

        fs::write(src.join("login.md"), "# Login\n\nuser/pass\n").unwrap();
        let source = plain_handle(&src);

        // Build an encrypted-db destination handle over a fresh container.
        let key = [42u8; 32];
        let live = enc.join("live.db");
        let snapshot = enc.join("vault.jaynotes");
        let container = Container::create(&live, &key, "Secret").unwrap();
        let dest = EncryptedDbHandle::new(container, snapshot.clone());

        let rel = transfer_core(&source, &src_state, &dest, "login.md", "").unwrap();
        assert_eq!(rel, "login.md");

        // Readable back through the destination container.
        let got = dest.read_note("login.md").unwrap();
        assert_eq!(got, "# Login\n\nuser/pass\n");

        // Source note trashed.
        assert!(!src.join("login.md").is_file());

        drop(dest); // flushes a snapshot
        fs::remove_dir_all(&src).ok();
        fs::remove_dir_all(&enc).ok();
    }
}
