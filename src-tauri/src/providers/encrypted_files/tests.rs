//! Tests for the encrypted-files provider: the handle round-trips, the unlock
//! probe, stray handling, sync-conflict surfacing, the keyed index, and an
//! end-to-end AI-tools smoke test — all against temp directories.

use super::*;
use crate::index::Index;
use rusqlite::Connection;
use std::time::{SystemTime, UNIX_EPOCH};

fn temp_backing(tag: &str) -> PathBuf {
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "jaynotes-encfiles-{tag}-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A handle over `backing` with an in-memory index, ready to exercise directly.
fn make_handle(backing: &Path, password: &str) -> (EncryptedFilesHandle, Arc<Mutex<Option<Index>>>) {
    let cipher = Arc::new(CryptCipher::derive(password, "").unwrap());
    let idx = Index::from_conn(Connection::open_in_memory().unwrap(), backing).unwrap();
    let index = Arc::new(Mutex::new(Some(idx)));
    let recent = Arc::new(Mutex::new(HashMap::new()));
    let handle = EncryptedFilesHandle::open_at(backing, cipher, index.clone(), recent).unwrap();
    (handle, index)
}

fn index_paths(index: &Arc<Mutex<Option<Index>>>) -> Vec<String> {
    index
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .list_notes()
        .unwrap()
        .into_iter()
        .map(|n| n.path)
        .collect()
}

// ---------------------------------------------------------------------------
// Round-trip through the handle
// ---------------------------------------------------------------------------

#[test]
fn create_scan_read_rename_move_trash_roundtrip() {
    let backing = temp_backing("roundtrip");
    let st = AppState::default();
    let (h, index) = make_handle(&backing, "hunter2");

    // create (via write) → the ciphertext hits disk, plaintext never does
    h.write_note(&st, "notes/Alpha.md", "# Alpha\nhello world").unwrap();
    assert!(!backing.join("notes/Alpha.md").exists(), "no plaintext path");
    assert!(!backing.join("notes").exists(), "folder name is encrypted too");

    // scan → the plaintext tree shows the note
    let tree = h.scan_tree().unwrap();
    let notes = tree.children.iter().find(|c| c.name == "notes").expect("notes/ folder");
    assert!(notes.children.iter().any(|c| c.name == "Alpha.md"));

    // read → decrypts back to the original
    assert_eq!(h.read_note("notes/Alpha.md").unwrap(), "# Alpha\nhello world");

    // the index (populated by write) carries the plaintext path
    assert_eq!(index_paths(&index), vec!["notes/Alpha.md"]);

    // rename in place
    h.rename(&st, "notes/Alpha.md", "notes/Beta.md").unwrap();
    assert!(h.read_note("notes/Alpha.md").is_err());
    assert_eq!(h.read_note("notes/Beta.md").unwrap(), "# Alpha\nhello world");
    assert_eq!(index_paths(&index), vec!["notes/Beta.md"]);

    // move to another folder
    h.rename(&st, "notes/Beta.md", "archive/Beta.md").unwrap();
    assert_eq!(h.read_note("archive/Beta.md").unwrap(), "# Alpha\nhello world");
    assert_eq!(index_paths(&index), vec!["archive/Beta.md"]);

    // trash → gone from the vault and the index
    h.trash(&st, "archive/Beta.md").unwrap();
    assert!(h.read_note("archive/Beta.md").is_err());
    assert!(index_paths(&index).is_empty());

    std::fs::remove_dir_all(&backing).ok();
}

#[test]
fn attachments_encrypt_and_decrypt() {
    let backing = temp_backing("attach");
    let st = AppState::default();
    let (h, _index) = make_handle(&backing, "pw");

    let bytes = vec![1u8, 2, 3, 4, 5, 200, 255];
    let rel = h.save_attachment(&st, "pic.png", &bytes).unwrap();
    assert_eq!(rel, "attachments/pic.png");
    // Round-trips through the data-URI read path used by the editor.
    assert_eq!(h.read_attachment(&rel).unwrap(), bytes);

    std::fs::remove_dir_all(&backing).ok();
}

// ---------------------------------------------------------------------------
// Strays are skipped, not fatal
// ---------------------------------------------------------------------------

#[test]
fn undecryptable_strays_are_skipped_not_fatal() {
    let backing = temp_backing("strays");
    let st = AppState::default();
    let (h, _index) = make_handle(&backing, "pw");

    h.write_note(&st, "Real.md", "real note").unwrap();
    // A file whose name is not valid ciphertext (a hand-dropped stray).
    std::fs::write(backing.join("not-a-real-crypt-name.md"), b"garbage").unwrap();

    // Scan still succeeds and shows the real note; the stray is absent.
    let tree = h.scan_tree().unwrap();
    let names: Vec<&str> = tree.children.iter().map(|c| c.name.as_str()).collect();
    assert!(names.contains(&"Real.md"));
    assert!(!names.iter().any(|n| n.contains("not-a-real")));

    // Reindex likewise tolerates the stray.
    assert!(h.reindex().is_ok());

    std::fs::remove_dir_all(&backing).ok();
}

// ---------------------------------------------------------------------------
// Sync-conflict surfacing (handle level; the pure mapping is unit-tested in
// cipher.rs)
// ---------------------------------------------------------------------------

#[test]
fn sync_conflict_file_is_surfaced_and_readable() {
    let backing = temp_backing("conflict");
    let st = AppState::default();
    let (h, _index) = make_handle(&backing, "pw");

    h.write_note(&st, "Meeting.md", "original").unwrap();
    // Locate the note's ciphertext file and fabricate a Syncthing conflict copy
    // with different content.
    let ct_rel = h.cipher.encrypt_rel("Meeting.md").unwrap();
    let conflict_ct = h.cipher.encrypt_content(b"conflicting body").unwrap();
    let meta = "20260711-140000-DEVICE7";
    let conflict_path = backing.join(format!("{ct_rel}.sync-conflict-{meta}"));
    std::fs::write(&conflict_path, &conflict_ct).unwrap();

    // The tree shows BOTH the real note and the surfaced conflict, distinctly.
    let tree = h.scan_tree().unwrap();
    let names: Vec<&str> = tree.children.iter().map(|c| c.name.as_str()).collect();
    assert!(names.contains(&"Meeting.md"), "{names:?}");
    let conflict_name = format!("Meeting (sync-conflict {meta}).md");
    assert!(names.contains(&conflict_name.as_str()), "{names:?}");

    // Reading each yields its own content.
    assert_eq!(h.read_note("Meeting.md").unwrap(), "original");
    assert_eq!(h.read_note(&conflict_name).unwrap(), "conflicting body");

    std::fs::remove_dir_all(&backing).ok();
}

// ---------------------------------------------------------------------------
// Unlock probe (wrong/right password, empty-vault first unlock)
// ---------------------------------------------------------------------------

#[test]
fn probe_accepts_right_rejects_wrong_and_seeds_on_empty() {
    let backing = temp_backing("probe");
    let cipher = CryptCipher::derive("correct", "").unwrap();

    // Empty vault, first unlock: accepted, and the probe is seeded.
    assert!(!backing.join(PROBE_FILE).exists());
    verify_key(&backing, &cipher).unwrap();
    assert!(backing.join(PROBE_FILE).exists(), "probe seeded on first unlock");

    // Right password verifies against the seeded probe.
    verify_key(&backing, &CryptCipher::derive("correct", "").unwrap()).unwrap();

    // Wrong password fails the authenticated probe.
    let wrong = CryptCipher::derive("nope", "").unwrap();
    assert!(verify_key(&backing, &wrong).is_err());

    std::fs::remove_dir_all(&backing).ok();
}

#[test]
fn probe_detects_wrong_password_via_existing_notes_without_probe() {
    let backing = temp_backing("probe-noprobe");
    let st = AppState::default();
    let (h, _index) = make_handle(&backing, "right");
    h.write_note(&st, "A.md", "body").unwrap();
    // Remove the probe to simulate an externally-created (pure rclone) vault.
    std::fs::remove_file(backing.join(PROBE_FILE)).ok();

    // Right key: an existing note decrypts → accepted (and reseeds the probe).
    verify_key(&backing, &CryptCipher::derive("right", "").unwrap()).unwrap();
    // Wrong key against notes that don't decrypt → rejected.
    std::fs::remove_file(backing.join(PROBE_FILE)).ok();
    assert!(verify_key(&backing, &CryptCipher::derive("wrong", "").unwrap()).is_err());

    std::fs::remove_dir_all(&backing).ok();
}

// ---------------------------------------------------------------------------
// Keyed index: opens with the key, fails without / with the wrong key
// ---------------------------------------------------------------------------

#[test]
fn keyed_index_requires_its_key() {
    let dir = temp_backing("keyed-index");
    let db = dir.join("idx.db");
    let key = [7u8; 32];

    {
        let idx = Index::open_keyed(&db, &dir, Some(&key)).unwrap();
        idx.index_file("a.md", "hello").unwrap();
    }
    // Reopen with the right key → data survives.
    {
        let idx = Index::open_keyed(&db, &dir, Some(&key)).unwrap();
        assert_eq!(
            idx.list_notes().unwrap().into_iter().map(|n| n.path).collect::<Vec<_>>(),
            vec!["a.md"]
        );
    }
    // Opening WITHOUT a key (plain SQLite over an encrypted file) fails.
    assert!(Index::open(&db, &dir).is_err(), "unkeyed open must fail");
    // A wrong key fails too.
    assert!(Index::open_keyed(&db, &dir, Some(&[9u8; 32])).is_err());

    std::fs::remove_dir_all(&dir).ok();
}

// ---------------------------------------------------------------------------
// Key material / index-key derivation
// ---------------------------------------------------------------------------

#[test]
fn index_key_is_deterministic_and_distinct_from_material() {
    let cipher = CryptCipher::derive("pw", "salt").unwrap();
    let material = cipher.key_material();
    let k1 = crypto::derive_index_key(&material).unwrap();
    let k2 = crypto::derive_index_key(&material).unwrap();
    assert_eq!(k1, k2, "deterministic from the same material");
    assert_ne!(&k1[..], &material[..32], "index key is not the content key");
    // Different material → different index key.
    let other = CryptCipher::derive("pw2", "salt").unwrap().key_material();
    assert_ne!(k1, crypto::derive_index_key(&other).unwrap());
}

// ---------------------------------------------------------------------------
// AI tools run end-to-end against an encrypted-files vault
// ---------------------------------------------------------------------------

#[tokio::test]
async fn ai_tools_operate_on_encrypted_files_vault() {
    use crate::ai::tools::{dispatch, RevisionSink, ToolContext};
    use serde_json::json;

    let backing = temp_backing("ai");
    let st = AppState::default();

    // Wire the vault as the active backend, sharing the index Arc so search sees
    // what writes produce.
    let cipher = Arc::new(CryptCipher::derive("pw", "").unwrap());
    let idx = Index::from_conn(Connection::open_in_memory().unwrap(), &backing).unwrap();
    *st.index.lock().unwrap() = Some(idx);
    let handle = EncryptedFilesHandle::open_at(
        &backing,
        cipher,
        st.index.clone(),
        st.recent_writes.clone(),
    )
    .unwrap();
    *st.active.lock().unwrap() = Some(Box::new(handle));

    let ai = crate::ai::AppAiState::default();
    let ch: tauri::ipc::Channel<crate::ai::AiEvent> = tauri::ipc::Channel::new(|_| Ok(()));
    let ctx = ToolContext {
        state: &st,
        ai: &ai,
        channel: &ch,
        // Encrypted vault → snapshots stored encrypted inside the backing.
        revisions: RevisionSink::Handle,
        app: None,
    };

    // create_note
    let out = dispatch(&ctx, "create_note", &json!({"path":"Ideas.md","content":"# Ideas\n#project first"})).await;
    assert!(out.result.contains("\"created\":true"), "{}", out.result);
    // The plaintext never touches disk.
    assert!(!backing.join("Ideas.md").exists());

    // read_note round-trips the decrypted content
    let read = dispatch(&ctx, "read_note", &json!({"path":"Ideas.md"})).await;
    assert!(read.result.contains("# Ideas"), "{}", read.result);

    // search finds it through the keyed index
    let search = dispatch(&ctx, "search_notes", &json!({"query":"Ideas"})).await;
    assert!(search.result.contains("Ideas.md"), "{}", search.result);

    // list_tags reflects the inline #project tag (indexed at create time)
    let tags = dispatch(&ctx, "list_tags", &json!({})).await;
    assert!(tags.result.contains("project"), "{}", tags.result);

    // update_note takes a revision snapshot (stored encrypted under .revisions/)
    let upd = dispatch(&ctx, "update_note", &json!({"path":"Ideas.md","content":"# Ideas v2"})).await;
    assert!(upd.revision_id.is_some(), "update produced a revision: {}", upd.result);
    assert!(dispatch(&ctx, "read_note", &json!({"path":"Ideas.md"})).await.result.contains("v2"));
    // The revision snapshot was written into the encrypted backing, not app-data.
    let rev_list = crate::ai::revisions::handle_list(&st, "Ideas.md");
    assert_eq!(rev_list.len(), 1, "one snapshot recorded");

    // rename_note through the handle
    let ren = dispatch(&ctx, "rename_note", &json!({"old_path":"Ideas.md","new_path":"Plans.md"})).await;
    assert!(ren.result.contains("Plans.md"), "{}", ren.result);
    assert!(dispatch(&ctx, "read_note", &json!({"path":"Plans.md"})).await.result.contains("v2"));

    std::fs::remove_dir_all(&backing).ok();
}

// ---------------------------------------------------------------------------
// Locked vault → clean error from the AI tools
// ---------------------------------------------------------------------------

#[tokio::test]
async fn locked_vault_yields_clean_error() {
    use crate::ai::tools::{dispatch, RevisionSink, ToolContext};
    use serde_json::json;

    // No active handle, no index → the vault is "locked".
    let st = AppState::default();
    let ai = crate::ai::AppAiState::default();
    let ch: tauri::ipc::Channel<crate::ai::AiEvent> = tauri::ipc::Channel::new(|_| Ok(()));
    let ctx = ToolContext {
        state: &st,
        ai: &ai,
        channel: &ch,
        revisions: RevisionSink::Handle,
        app: None,
    };

    let search = dispatch(&ctx, "search_notes", &json!({"query":"x"})).await;
    assert!(search.result.to_lowercase().contains("locked"), "{}", search.result);
    let read = dispatch(&ctx, "read_note", &json!({"path":"a.md"})).await;
    assert!(read.result.starts_with("Error:"), "{}", read.result);
}
