//! The bridge between JayNotes' plaintext note world and the rclone-crypt
//! library — the encryption core of the `encrypted-files` provider.
//!
//! Everything here maps PLAINTEXT (a vault-relative note path / note bytes,
//! facing the app) to and from CIPHERTEXT (a name-encrypted path / XSalsa20
//! chunked content, facing the backing directory that Syncthing synchronises).
//! Object NAMES use rclone's AES-256-EME filename mode (deterministic, so a
//! plaintext name always maps to the same ciphertext name) and object CONTENT
//! uses rclone's authenticated chunked format (so a wrong key fails to decrypt
//! rather than yielding garbage — the basis of the unlock probe).
//!
//! ## Sync-conflict surfacing
//!
//! Syncthing renames a conflicting copy to `<name>.sync-conflict-<meta>`. Since
//! it operates on the *ciphertext* files, that suffix lands on the ciphertext
//! segment. [`decrypt_backing_name`] strips the suffix, decrypts the real name,
//! and produces a display name `<stem> (sync-conflict <meta>)<ext>` that cannot
//! collide with the real note. [`encrypt_rel`] reverses it, so reading/trashing
//! a surfaced conflict note targets the right ciphertext file.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll, Wake, Waker};

use rclone_crypt::{encrypt_to, Credential, Keys, MemorySource, NameCipher, RcloneCryptObject};

/// The marker Syncthing embeds in a conflicting copy's filename.
const SYNC_CONFLICT: &str = ".sync-conflict-";

/// A derived, ready-to-use cipher for one encrypted-files vault: the rclone
/// content keys plus the filename cipher. Derived once per unlock and shared
/// (behind an `Arc`) by the handle and its watcher.
pub struct CryptCipher {
    keys: Keys,
    names: NameCipher,
}

impl CryptCipher {
    /// Derives the cipher from a password and an optional second password/salt.
    /// An empty `password2` means rclone's fixed default salt (passing `None`),
    /// matching rclone's own semantics.
    pub fn derive(password: &str, password2: &str) -> Result<Self, String> {
        let pw = Credential::Clear(password.to_string());
        let salt = if password2.is_empty() {
            None
        } else {
            Some(Credential::Clear(password2.to_string()))
        };
        let keys = Keys::derive(&pw, salt.as_ref())
            .map_err(|e| format!("Key derivation failed: {e}"))?;
        let names = NameCipher::new(&keys);
        Ok(CryptCipher { keys, names })
    }

    /// Rebuilds the cipher from persisted 80-byte key material (session/keyring),
    /// avoiding a second scrypt run and never needing the password.
    pub fn from_material(material: &[u8]) -> Result<Self, String> {
        if material.len() != 80 {
            return Err("Corrupt key material".into());
        }
        let mut bytes = [0u8; 80];
        bytes.copy_from_slice(material);
        let keys = Keys::from_bytes(&bytes);
        let names = NameCipher::new(&keys);
        Ok(CryptCipher { keys, names })
    }

    /// The 80-byte derived key material (`data || name || name_tweak`), for the
    /// in-memory session, the opt-in keyring, and the index-key derivation.
    pub fn key_material(&self) -> [u8; 80] {
        self.keys.to_bytes()
    }

    // ----------------------------------------------------------------- names

    /// Encrypts a plaintext vault-relative path into its backing (ciphertext)
    /// path. A final segment carrying a `(sync-conflict <meta>)` marker is
    /// mapped to the corresponding `.sync-conflict-<meta>` ciphertext file so a
    /// surfaced conflict note round-trips to the real backing file.
    pub fn encrypt_rel(&self, rel: &str) -> Result<String, String> {
        let rel = rel.trim_matches('/');
        if rel.is_empty() {
            return Ok(String::new());
        }
        let (clean, conflict) = split_conflict_display(rel);
        let ct = self
            .names
            .encrypt_path(&clean)
            .map_err(|e| format!("Could not encrypt name '{clean}': {e}"))?;
        match conflict {
            Some(meta) => Ok(format!("{ct}{SYNC_CONFLICT}{meta}")),
            None => Ok(ct),
        }
    }

    /// Decrypts one backing path segment or the full backing filename of a leaf,
    /// returning the plaintext display name. A `.sync-conflict-<meta>` suffix is
    /// stripped, the base decrypted, and the marker folded back into the display
    /// name. Returns `Err` for an undecryptable (stray) segment.
    pub fn decrypt_backing_name(&self, backing_name: &str) -> Result<String, String> {
        if let Some((ct_base, meta)) = split_sync_conflict(backing_name) {
            let plain = self
                .names
                .decrypt_segment(ct_base)
                .map_err(|e| format!("stray: {e}"))?;
            Ok(insert_conflict_marker(&plain, meta))
        } else {
            self.names
                .decrypt_segment(backing_name)
                .map_err(|e| format!("stray: {e}"))
        }
    }

    // --------------------------------------------------------------- content

    /// Encrypts plaintext bytes into rclone's chunked content format.
    pub fn encrypt_content(&self, plaintext: &[u8]) -> Result<Vec<u8>, String> {
        let mut reader: &[u8] = plaintext;
        let mut out: Vec<u8> = Vec::with_capacity(plaintext.len() + 64);
        block_on(async {
            encrypt_to(&mut reader, &mut out, &self.keys)
                .await
                .map_err(|e| format!("Could not encrypt content: {e}"))
        })?;
        Ok(out)
    }

    /// Decrypts rclone-format ciphertext bytes back to plaintext. A wrong key or
    /// tampered ciphertext fails the Poly1305 authentication and errors (never
    /// returns garbage) — this is what the unlock probe relies on.
    pub fn decrypt_content(&self, ciphertext: &[u8]) -> Result<Vec<u8>, String> {
        block_on(async {
            let src = Arc::new(MemorySource::new(ciphertext.to_vec()));
            let obj = RcloneCryptObject::open(src, self.keys.clone())
                .await
                .map_err(|e| format!("Could not open content: {e}"))?;
            let len = obj.plaintext_len();
            let bytes = obj
                .read_range(0..len)
                .await
                .map_err(|e| format!("Could not decrypt content: {e}"))?;
            Ok(bytes.to_vec())
        })
    }
}

// ---------------------------------------------------------------------------
// Sync-conflict name mapping (pure string transforms, unit-tested both ways)
// ---------------------------------------------------------------------------

/// Splits a backing filename `"<ct>.sync-conflict-<meta>"` into `(ct, meta)`, or
/// `None` if it isn't a conflict copy.
fn split_sync_conflict(backing_name: &str) -> Option<(&str, &str)> {
    let idx = backing_name.find(SYNC_CONFLICT)?;
    let ct = &backing_name[..idx];
    let meta = &backing_name[idx + SYNC_CONFLICT.len()..];
    if ct.is_empty() || meta.is_empty() {
        return None;
    }
    Some((ct, meta))
}

/// Folds a conflict marker into a decrypted plaintext name, inserting it before
/// the extension: `Note.md` + `20260711-140000-DEV` →
/// `Note (sync-conflict 20260711-140000-DEV).md`.
fn insert_conflict_marker(plain_name: &str, meta: &str) -> String {
    let (stem, ext) = split_ext(plain_name);
    format!("{stem} (sync-conflict {meta}){ext}")
}

/// The inverse of [`insert_conflict_marker`] over the FINAL segment of a display
/// rel: returns `(clean_rel, Some(meta))` when the leaf carries a
/// `(sync-conflict <meta>)` marker, else `(rel, None)`.
fn split_conflict_display(rel: &str) -> (String, Option<String>) {
    let (parent, leaf) = match rel.rfind('/') {
        Some(i) => (&rel[..=i], &rel[i + 1..]),
        None => ("", rel),
    };
    const OPEN: &str = " (sync-conflict ";
    let open = match leaf.find(OPEN) {
        Some(i) => i,
        None => return (rel.to_string(), None),
    };
    let after = &leaf[open + OPEN.len()..];
    let close = match after.find(')') {
        Some(i) => i,
        None => return (rel.to_string(), None),
    };
    let meta = &after[..close];
    let stem = &leaf[..open];
    let ext = &after[close + 1..];
    (format!("{parent}{stem}{ext}"), Some(meta.to_string()))
}

/// Splits a filename into `(stem, ext)` on the LAST dot, keeping the dot on the
/// extension. A leading-dot name (`.hidden`) has no extension.
fn split_ext(name: &str) -> (&str, &str) {
    match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    }
}

// ---------------------------------------------------------------------------
// Minimal in-place executor
//
// Every future driven here is backed by an in-memory `MemorySource` / `&[u8]` /
// `Vec<u8>` — no real I/O, no timers — so each poll is immediately `Ready` and
// this trivial executor completes without ever parking. Keeping it dependency-
// free avoids spinning up a tokio runtime (which cannot be nested inside the
// Tauri command runtime) for what is pure in-memory CPU work.
// ---------------------------------------------------------------------------

struct NoopWaker;
impl Wake for NoopWaker {
    fn wake(self: Arc<Self>) {}
}

fn block_on<F: Future>(fut: F) -> F::Output {
    let waker = Waker::from(Arc::new(NoopWaker));
    let mut cx = Context::from_waker(&waker);
    let mut fut = Box::pin(fut);
    loop {
        match Pin::as_mut(&mut fut).poll(&mut cx) {
            Poll::Ready(v) => return v,
            // In-memory sources are always immediately ready; a Pending here
            // would only occur if a future depended on external wakeups, which
            // none of these do. Yield to be safe rather than busy-spin hot.
            Poll::Pending => std::thread::yield_now(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cipher() -> CryptCipher {
        CryptCipher::derive("correct horse battery staple", "salt-two").unwrap()
    }

    #[test]
    fn content_roundtrip_including_empty() {
        let c = cipher();
        for plain in [
            b"".to_vec(),
            b"# Title\nhello world".to_vec(),
            vec![0u8; 200_000],
        ] {
            let ct = c.encrypt_content(&plain).unwrap();
            assert_ne!(ct, plain, "content is actually encrypted");
            assert_eq!(c.decrypt_content(&ct).unwrap(), plain);
        }
    }

    #[test]
    fn wrong_key_fails_to_decrypt() {
        let a = cipher();
        let b = CryptCipher::derive("different password", "salt-two").unwrap();
        let ct = a.encrypt_content(b"secret").unwrap();
        assert!(b.decrypt_content(&ct).is_err(), "wrong key must not decrypt");
    }

    #[test]
    fn material_roundtrip_rebuilds_same_cipher() {
        let a = cipher();
        let material = a.key_material();
        let b = CryptCipher::from_material(&material).unwrap();
        let ct = a.encrypt_content(b"body").unwrap();
        assert_eq!(b.decrypt_content(&ct).unwrap(), b"body");
        // Names are deterministic and agree across the rebuilt cipher.
        assert_eq!(
            a.encrypt_rel("folder/Note.md").unwrap(),
            b.encrypt_rel("folder/Note.md").unwrap()
        );
    }

    #[test]
    fn name_roundtrip_is_deterministic() {
        let c = cipher();
        let ct = c.encrypt_rel("folder/Note Name.md").unwrap();
        assert_ne!(ct, "folder/Note Name.md");
        // Deterministic: same plaintext → same ciphertext.
        assert_eq!(ct, c.encrypt_rel("folder/Note Name.md").unwrap());
    }

    #[test]
    fn sync_conflict_display_mapping_both_ways() {
        let c = cipher();
        // The real note's final ciphertext segment.
        let ct_rel = c.encrypt_rel("notes/Meeting.md").unwrap();
        let ct_leaf = ct_rel.rsplit('/').next().unwrap();
        // Syncthing appends its suffix to the ciphertext leaf.
        let meta = "20260711-140000-ABCD234";
        let backing_leaf = format!("{ct_leaf}{SYNC_CONFLICT}{meta}");

        // backing → display
        let display = c.decrypt_backing_name(&backing_leaf).unwrap();
        assert_eq!(display, format!("Meeting (sync-conflict {meta}).md"));

        // display → backing (as a full rel, parent decrypts normally)
        let display_rel = format!("notes/{display}");
        let back = c.encrypt_rel(&display_rel).unwrap();
        assert_eq!(back, format!("{ct_rel}{SYNC_CONFLICT}{meta}"));
    }

    #[test]
    fn conflict_marker_does_not_collide_with_real_note() {
        let meta = "20260101-000000-X";
        // A conflict display name is distinct from the real note name.
        assert_eq!(
            insert_conflict_marker("Note.md", meta),
            "Note (sync-conflict 20260101-000000-X).md"
        );
        // And the inverse recovers the clean name.
        let (clean, m) = split_conflict_display("Note (sync-conflict 20260101-000000-X).md");
        assert_eq!(clean, "Note.md");
        assert_eq!(m.as_deref(), Some(meta));
        // A normal name is untouched.
        let (clean2, m2) = split_conflict_display("Ordinary Note.md");
        assert_eq!(clean2, "Ordinary Note.md");
        assert!(m2.is_none());
    }

    #[test]
    fn split_ext_handles_dotfiles_and_multidot() {
        assert_eq!(split_ext("Note.md"), ("Note", ".md"));
        assert_eq!(split_ext("v1.2.final.md"), ("v1.2.final", ".md"));
        assert_eq!(split_ext(".hidden"), (".hidden", ""));
        assert_eq!(split_ext("noext"), ("noext", ""));
    }
}
