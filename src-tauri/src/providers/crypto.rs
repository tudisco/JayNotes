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
/// link SQLCipher.
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

/// Per-vault unlocked key material, held in memory only. Cleared per-vault by
/// [`SecretsSession::lock`] and never persisted (the OS keyring, opt-in, is the
/// only place a key is stored across runs — see [`keyring_store`]).
#[derive(Default)]
pub struct SecretsSession {
    keys: Mutex<HashMap<String, Zeroizing<[u8; 32]>>>,
}

impl SecretsSession {
    pub fn store(&self, vault_id: &str, key: [u8; 32]) {
        self.keys
            .lock()
            .unwrap()
            .insert(vault_id.to_string(), Zeroizing::new(key));
    }

    /// A copy of the unlocked key for `vault_id`, if unlocked.
    pub fn get(&self, vault_id: &str) -> Option<[u8; 32]> {
        self.keys.lock().unwrap().get(vault_id).map(|k| **k)
    }

    pub fn is_unlocked(&self, vault_id: &str) -> bool {
        self.keys.lock().unwrap().contains_key(vault_id)
    }

    /// Forgets the in-memory key for `vault_id` (the `Zeroizing` wrapper wipes it).
    pub fn lock(&self, vault_id: &str) {
        self.keys.lock().unwrap().remove(vault_id);
    }
}

// ---------------------------------------------------------------------------
// OS keyring ("remember password") — always best-effort
// ---------------------------------------------------------------------------

/// Stores the derived key (hex) in the OS keyring for silent future unlocks.
/// Errors are swallowed by the caller: a Linux box without a Secret Service
/// simply won't remember, and must never crash the app.
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
pub fn keyring_get(vault_id: &str) -> Option<[u8; 32]> {
    let entry = keyring::Entry::new(KEYRING_SERVICE, vault_id).ok()?;
    let hex = entry.get_password().ok()?;
    from_hex_32(&hex)
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
