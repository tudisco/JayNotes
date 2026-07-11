//! Shared encryption plumbing for every encrypted provider (M14 encrypted-db,
//! and M15 encrypted-files later): the KDF, the in-memory unlock session, and
//! the optional OS-keyring "remember" path.
//!
//! ## KDF choice
//!
//! [`derive_vault_key`] uses **scrypt** (`log_n = 15` → N=32768, r=8, p=1),
//! producing a 32-byte key for SQLCipher's raw-key mode. scrypt's memory-hard
//! cost (~32 MB, ~100–250 ms on a desktop) is a deliberate brute-force speed
//! bump over a bare hash while staying interactive. The salt (16 random bytes)
//! is stored **unencrypted** in the vault's `config` (a salt is not secret and
//! must be readable before the key exists).
//!
//! ## Future secret sources (FIDO2 hmac-secret / PRF passkey)
//!
//! The whole layer is shaped around one type — a `[u8; 32]` key. A password is
//! only *one* way to obtain it: [`derive_vault_key`] turns a password+salt into
//! the key, but a passkey's PRF output is already 32 bytes and could be stored
//! straight into the [`SecretsSession`] without touching any provider. Providers
//! never see the password; they receive the derived key via the session.
//!
//! ## Opaque per-vault material (M15 encrypted-files)
//!
//! Not every provider's secret is a 32-byte SQLCipher key. The rclone-crypt
//! provider derives an 80-byte `Keys` bundle (a different KDF entirely). The
//! session therefore stores **opaque bytes** per vault; the `[u8; 32]` helpers
//! ([`SecretsSession::store`]/[`get`]) are a thin typed façade over that store
//! for encrypted-db, and [`store_bytes`]/[`get_bytes`] carry arbitrary material
//! for encrypted-files. The keyring path gets matching hex helpers. A provider
//! chooses which — see each provider's `unlock`. Nothing here ever holds a
//! password: only derived key material is stored, in memory or (opt-in) keyring.

use std::collections::HashMap;
use std::sync::Mutex;

use zeroize::Zeroizing;

/// scrypt cost parameter (N = 2^15). Documented tradeoff: interactive unlock
/// latency vs. brute-force resistance. Changing it would invalidate existing
/// containers, so it is fixed for the schema.
const SCRYPT_LOG_N: u8 = 15;
const SCRYPT_R: u32 = 8;
const SCRYPT_P: u32 = 1;

/// Keyring service name; the account is the vault id.
const KEYRING_SERVICE: &str = "jaynotes";

/// Derives a 32-byte vault key from a password and salt (scrypt). Deterministic
/// for a given `(password, salt)`; a different salt yields a different key.
pub fn derive_vault_key(password: &str, salt: &[u8]) -> Result<[u8; 32], String> {
    let params = scrypt::Params::new(SCRYPT_LOG_N, SCRYPT_R, SCRYPT_P, 32)
        .map_err(|e| format!("Invalid KDF params: {e}"))?;
    let mut out = [0u8; 32];
    scrypt::scrypt(password.as_bytes(), salt, &params, &mut out)
        .map_err(|e| format!("Key derivation failed: {e}"))?;
    Ok(out)
}

/// `n` cryptographically-random bytes, sourced from SQLite's `randomblob` (which
/// draws from the OS CSPRNG) so no extra RNG dependency is needed — we already
/// link SQLCipher. (Used by encrypted-db to generate its scrypt salt; the
/// rclone KDF derives its own material, so encrypted-files needs no salt.)
#[cfg(any(feature = "provider-encrypted-db", test))]
pub fn random_bytes(n: usize) -> Result<Vec<u8>, String> {
    let conn = rusqlite::Connection::open_in_memory()
        .map_err(|e| format!("Could not open RNG source: {e}"))?;
    conn.query_row("SELECT randomblob(?1)", [n as i64], |r| r.get::<_, Vec<u8>>(0))
        .map_err(|e| format!("Could not generate random bytes: {e}"))
}

/// Lowercase hex of a byte slice.
pub fn to_hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// Parses lowercase/uppercase hex into bytes.
pub fn from_hex(s: &str) -> Result<Vec<u8>, String> {
    if s.len() % 2 != 0 {
        return Err("Hex string has odd length".into());
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(|e| format!("Bad hex: {e}")))
        .collect()
}

#[cfg(feature = "provider-encrypted-db")]
fn from_hex_32(s: &str) -> Option<[u8; 32]> {
    let v = from_hex(s).ok()?;
    if v.len() != 32 {
        return None;
    }
    let mut k = [0u8; 32];
    k.copy_from_slice(&v);
    Some(k)
}

// ---------------------------------------------------------------------------
// Unlock session (Tauri-managed)
// ---------------------------------------------------------------------------

/// Per-vault unlocked key material, held in memory only as opaque bytes.
/// Cleared per-vault by [`SecretsSession::lock`] and never persisted (the OS
/// keyring, opt-in, is the only place material is stored across runs — see
/// [`keyring_store`]). The bytes are provider-defined: a 32-byte SQLCipher key
/// for encrypted-db, an 80-byte rclone `Keys` bundle for encrypted-files.
#[derive(Default)]
pub struct SecretsSession {
    material: Mutex<HashMap<String, Zeroizing<Vec<u8>>>>,
}

impl SecretsSession {
    /// Stores a 32-byte key (encrypted-db's SQLCipher raw key).
    #[cfg(any(feature = "provider-encrypted-db", test))]
    pub fn store(&self, vault_id: &str, key: [u8; 32]) {
        self.store_bytes(vault_id, key.to_vec());
    }

    /// A copy of the unlocked 32-byte key for `vault_id`, if it is unlocked and
    /// its material is exactly 32 bytes.
    #[cfg(any(feature = "provider-encrypted-db", test))]
    pub fn get(&self, vault_id: &str) -> Option<[u8; 32]> {
        let bytes = self.get_bytes(vault_id)?;
        if bytes.len() != 32 {
            return None;
        }
        let mut k = [0u8; 32];
        k.copy_from_slice(&bytes);
        Some(k)
    }

    /// Stores arbitrary opaque material (encrypted-files' 80-byte `Keys` bundle).
    pub fn store_bytes(&self, vault_id: &str, bytes: Vec<u8>) {
        self.material
            .lock()
            .unwrap()
            .insert(vault_id.to_string(), Zeroizing::new(bytes));
    }

    /// A copy of the unlocked opaque material for `vault_id`, if unlocked.
    pub fn get_bytes(&self, vault_id: &str) -> Option<Vec<u8>> {
        self.material
            .lock()
            .unwrap()
            .get(vault_id)
            .map(|k| k.to_vec())
    }

    pub fn is_unlocked(&self, vault_id: &str) -> bool {
        self.material.lock().unwrap().contains_key(vault_id)
    }

    /// Forgets the in-memory material for `vault_id` (the `Zeroizing` wrapper
    /// wipes it).
    pub fn lock(&self, vault_id: &str) {
        self.material.lock().unwrap().remove(vault_id);
    }
}

// ---------------------------------------------------------------------------
// OS keyring ("remember password") — always best-effort
// ---------------------------------------------------------------------------

/// Stores the derived key (hex) in the OS keyring for silent future unlocks.
/// Errors are swallowed by the caller: a Linux box without a Secret Service
/// simply won't remember, and must never crash the app.
#[cfg(feature = "provider-encrypted-db")]
pub fn keyring_store(vault_id: &str, key: &[u8; 32]) -> Result<(), String> {
    let entry =
        keyring::Entry::new(KEYRING_SERVICE, vault_id).map_err(|e| format!("Keyring: {e}"))?;
    entry
        .set_password(&to_hex(key))
        .map_err(|e| format!("Keyring: {e}"))
}

/// Tries to fetch a remembered key from the OS keyring. Returns `None` on any
/// failure (no keyring, no entry, corrupt value) so the caller falls back to a
/// password prompt.
#[cfg(feature = "provider-encrypted-db")]
pub fn keyring_get(vault_id: &str) -> Option<[u8; 32]> {
    let entry = keyring::Entry::new(KEYRING_SERVICE, vault_id).ok()?;
    let hex = entry.get_password().ok()?;
    from_hex_32(&hex)
}

/// Stores arbitrary derived key material (hex) in the OS keyring for silent
/// future unlocks. Used by encrypted-files (an 80-byte `Keys` bundle). Like
/// [`keyring_store`], this persists *derived key material*, never the password.
#[cfg(feature = "provider-encrypted-files")]
pub fn keyring_store_bytes(vault_id: &str, bytes: &[u8]) -> Result<(), String> {
    let entry =
        keyring::Entry::new(KEYRING_SERVICE, vault_id).map_err(|e| format!("Keyring: {e}"))?;
    entry
        .set_password(&to_hex(bytes))
        .map_err(|e| format!("Keyring: {e}"))
}

/// Fetches remembered opaque material (hex) from the OS keyring, or `None`.
#[cfg(feature = "provider-encrypted-files")]
pub fn keyring_get_bytes(vault_id: &str) -> Option<Vec<u8>> {
    let entry = keyring::Entry::new(KEYRING_SERVICE, vault_id).ok()?;
    let hex = entry.get_password().ok()?;
    from_hex(&hex).ok()
}

/// Derives a 32-byte SQLCipher key for the *separate search index* of an
/// encrypted-files vault, domain-separated from the vault's content key material.
///
/// The index DB must never leak plaintext (it holds decrypted note bodies for
/// FTS), so it is itself a SQLCipher container. Its key is derived from the
/// unlocked content-key `material` (the rclone `Keys` bytes) via scrypt with a
/// fixed domain salt — so it is a deterministic function of the vault's real key
/// material, cannot be computed without unlocking the vault, and is distinct
/// from any key used for the files themselves.
#[cfg(feature = "provider-encrypted-files")]
pub fn derive_index_key(material: &[u8]) -> Result<[u8; 32], String> {
    // Domain-separated: hex the material and run it through the same scrypt as
    // the vault KDF with a constant, purpose-specific salt.
    derive_vault_key(&to_hex(material), b"jaynotes-idx-v1\0")
}

/// Removes a remembered key (on explicit lock / vault removal). Best-effort.
pub fn keyring_delete(vault_id: &str) {
    if let Ok(entry) = keyring::Entry::new(KEYRING_SERVICE, vault_id) {
        let _ = entry.delete_credential();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derive_is_deterministic_per_salt() {
        let salt = b"0123456789abcdef";
        let a = derive_vault_key("hunter2", salt).unwrap();
        let b = derive_vault_key("hunter2", salt).unwrap();
        assert_eq!(a, b, "same password+salt → same key");
    }

    #[test]
    fn different_salt_yields_different_key() {
        let a = derive_vault_key("hunter2", b"salt-aaaaaaaaaaa").unwrap();
        let b = derive_vault_key("hunter2", b"salt-bbbbbbbbbbb").unwrap();
        assert_ne!(a, b, "different salt → different key");
    }

    #[test]
    fn different_password_yields_different_key() {
        let salt = b"same-salt-16byte";
        let a = derive_vault_key("password-one", salt).unwrap();
        let b = derive_vault_key("password-two", salt).unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn hex_roundtrips() {
        let bytes = random_bytes(16).unwrap();
        assert_eq!(bytes.len(), 16);
        let hex = to_hex(&bytes);
        assert_eq!(hex.len(), 32);
        assert_eq!(from_hex(&hex).unwrap(), bytes);
    }

    #[test]
    fn random_bytes_are_not_constant() {
        let a = random_bytes(16).unwrap();
        let b = random_bytes(16).unwrap();
        assert_ne!(a, b, "randomblob should not repeat");
    }

    #[test]
    fn session_store_get_lock() {
        let s = SecretsSession::default();
        assert!(!s.is_unlocked("v1"));
        s.store("v1", [7u8; 32]);
        assert!(s.is_unlocked("v1"));
        assert_eq!(s.get("v1"), Some([7u8; 32]));
        s.lock("v1");
        assert!(!s.is_unlocked("v1"));
        assert_eq!(s.get("v1"), None);
    }
}
