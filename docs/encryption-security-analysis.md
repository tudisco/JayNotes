# Encryption security analysis — `encrypted-files` (rclone-crypt) vaults

*Engineer-to-engineer assessment. Scope: the `encrypted-files` provider, which encrypts each
note as an independent rclone-crypt file so the backing directory can be synced by Syncthing (or
any dumb file mover) without exposing plaintext.*

Three layers are kept distinct throughout, because they have different owners and different fixability:

- **FORMAT** — rclone's on-disk crypt design (nonce scheme, EME filenames, scrypt params). Fixed by the
  goal of rclone interoperability. Changing it breaks the format's main selling point.
- **IMPL** — the Rust library at `RcloneCryptRustLib/crates/rclone-crypt` that re-implements that format.
- **USAGE** — how JayNotes derives/holds keys and wires the library in (`providers/encrypted_files/`,
  `providers/crypto.rs`).

---

## 0. Correctness finding (read this first)

**No implementation bugs found.** The Rust re-implementation matches the rclone spec on every fact that
matters for security:

- **Nonce use is correct.** One random 24-byte nonce per file (`encrypt.rs:27–29`), and each 64 KiB
  block is sealed under `base_nonce + block_index` via a little-endian add-with-carry (`format.rs:32–48`,
  applied at `encrypt.rs:71`). Encryption starts at chunk 0 and increments, so no `(key, nonce)` pair is
  ever reused within a file, and the random base makes cross-file reuse a ~2⁻¹⁹² event. This is the
  single most important thing to get right for XSalsa20-Poly1305, and it is right.
- **Every block's Poly1305 tag is verified before any plaintext is released** (`object.rs:91–94`;
  decrypt returns `Error::Authentication` on any mismatch). Corruption/wrong-key tests confirm no
  plaintext ever leaks on failure (`object.rs:282–316`).
- **Primitives and encodings match rclone test vectors**: the filename zero-key vector
  (`names.rs:209–221`), the `obscure` vector (`credentials.rs:165–172`), and the ciphertext-size
  vectors (`format.rs:150–154`) all pass. scrypt params, EME, PKCS#7 name padding, and base32hex all
  agree with upstream.
- **Empty-password degenerate case is handled.** rclone yields an all-zero key when the password is
  empty (`credentials.rs:85–89` skips scrypt); JayNotes refuses to *create* a vault with an empty
  password (`mod.rs:758–760`), so the world-known all-zero key is never used.

So the rest of this document is about *design* limits of the format, not defects in the code.

---

## 1. Primitive quality — solid, with one dated parameter

| Primitive | What it is | Verdict |
|---|---|---|
| **Content AEAD** | XSalsa20-Poly1305 (NaCl `secretbox`) per 64 KiB block | Excellent pedigree. Misuse-resistant nonce size (24 B), well-analyzed, constant-time. No concern. |
| **Filename cipher** | AES-256 in EME wide-block mode (Halevi–Rogaway), base32hex | Sound. EME is a length-preserving SPRP; rclone has shipped it for years. It is deterministic *by design* (see §3). |
| **KDF (content/name keys)** | scrypt, **N=2¹⁴ (16384), r=8, p=1**, 80 B out (`credentials.rs:86`) | Memory-hard and fine in kind, but the cost is **low by 2026 standards** — see below. |
| **KDF (search-index key)** | scrypt, **N=2¹⁵**, r=8, p=1 (`crypto.rs:41`, `derive_index_key crypto.rs:220–224`) | Reasonable; input is already high-entropy key material, so this is belt-and-suspenders. |

**On the scrypt cost.** N=2¹⁴ is ~16 MB and ~tens of ms per guess. OWASP's current password-storage
guidance for scrypt is N=2¹⁷ (r=8, p=1) as a floor; libsodium and most 2026 designs have moved to
**Argon2id** (memory-hard *and* side-channel-hardened), which is what you'd choose greenfield today.
rclone's N=2¹⁴ is **not broken** — it is a real memory-hard KDF — but it buys perhaps 8× less
brute-force headroom than a current recommendation, and it is a FORMAT constant that can't be raised
without breaking rclone compatibility. The practical consequence: the *password's own entropy* is
doing most of the work. A strong passphrase makes the KDF cost almost irrelevant; a weak one is only
lightly protected. This is the main reason the password2/salt recommendation in §7 matters.

---

## 2. Confidentiality of content at rest — strong

For a stolen laptop, a synced-away copy sitting in someone's cloud, or forensic recovery of the backing
directory, the *contents* of notes are well protected: XSalsa20-Poly1305 under a key that only exists
after scrypt over the user's password. There is no known way to read a note body without the password
(or the derived key material). This is the property the design is built for, and it holds.

What leaks anyway is **metadata**, and it's worth being precise about how much:

| Leak | Mechanism | Precision |
|---|---|---|
| **Exact note byte-length** | Ciphertext size is a fixed function of plaintext size — 32 B header + 16 B per block, no content padding (`format.rs:50–65`, invertible at `plaintext_size`). | **Exact** (to the byte, minus fixed overhead). A 4,213-byte note is identifiable as such. |
| **Directory tree shape** | Encryption is file-per-note; folders stay folders. | Full tree structure, fan-out, depth, note count all visible. |
| **Approximate name length** | EME preserves length; PKCS#7 pads name to a 16-B multiple, base32hex expands 8/5 (`names.rs:56–66`). | Plaintext name length recoverable to within 16 bytes. |
| **Name *equality* across the whole vault** | The EME tweak is a single global constant (`keys.name_tweak`) applied to every segment; it is **not** per-path or per-segment (`names.rs:27–32`, `52–66`). | **Two files/folders with the same plaintext name encrypt to the identical ciphertext name, anywhere in the tree.** An observer can count distinct names, spot duplicates, and follow renames/moves over sync history. |
| **Modification times** | Filesystem mtimes are not touched by the format. | Full timing/edit-frequency signal at the fs layer. |
| **"This is a JayNotes rclone vault"** | Every file starts with magic `RCLONE\0\0` (`format.rs:3`); the root holds a literal `.jaynotes-check` probe (`mod.rs:54–55`). | Fingerprints the format and the app unambiguously. |

None of these are defects — they are inherent to a deterministic, format-compatible, file-per-note
design. But they mean the scheme protects *what your notes say*, not *that you have notes, how many,
how big, how they're foldered, or when you touch them*. For a note-taking app that's usually an
acceptable trade; a user hiding the existence or structure of their notes is not served by this format.

---

## 3. Integrity / active attacker — the real soft spots

Per-block Poly1305 gives strong *tamper-evidence within a block*: flip any byte and that block fails to
decrypt. But authentication is **per block and nothing wider**, and there is **no binding between a
file's content and its name**. That opens three gaps against an attacker who can *write* to the backing
store (not just read it):

1. **Whole-block truncation is undetectable.** There is no length field, block count, or end-of-file
   marker in the format. Every block authenticates independently, and the last block is simply allowed
   to be short. Delete trailing complete 64 KiB blocks and the file just decrypts to a shorter, fully
   *valid* plaintext. (Truncation *inside* a block's authenticator is caught — `format.rs:73–75`,
   `object.rs:88–90` — but a clean block-boundary chop is not.) For notes under 64 KiB this is moot;
   for large notes it means silent tail loss can't be distinguished from legitimate content.

2. **No filename↔content binding → content swap / rollback.** Nothing in the sealed data commits to the
   filename (no AAD is passed — `encrypt.rs:72`, `object.rs:92`). An attacker who controls storage can
   swap the entire ciphertext blob of note A onto note B's name, or replace a note with an older synced
   copy of *the same* note. Both decrypt cleanly and authentically — they're valid ciphertext under the
   right key, just not the content you expect. This is a genuine active-attacker and anti-rollback gap.

3. **Reordering/splicing is blocked, though.** Because each block's nonce is `base + index`, moving a
   block within a file or grafting one from another file makes the decryptor use the wrong nonce and the
   tag fails. So the position of each block *within its file* is bound; only whole-file identity and
   file length are not.

**Framing this honestly in the Syncthing threat model.** The intended adversary is *passive*: a stolen
disk, a cloud/relay that holds synced ciphertext, a backup. Sync *peers* are the user's own trusted
devices. Against that adversary, gaps (1) and (2) require write-back into a store you don't control for
integrity — i.e. a *malicious or compromised* cloud/relay/backup, not the honest-but-curious one the
design targets. So these are real but **lower-severity in context**: they matter if your Syncthing
relay or cloud replica turns actively hostile, not for the theft/snooping cases that motivate the
feature. They are worth knowing before anyone markets this as protection against a tampering storage
provider — it is not.

---

## 4. Key handling in JayNotes (USAGE layer) — carefully done

- **Password → keys.** `CryptCipher::derive` runs the rclone KDF: password + optional password2 →
  80-byte `data(32) ‖ name(32) ‖ name_tweak(16)` bundle (`cipher.rs:43–54`). An empty password2 means
  `None`, which selects rclone's fixed default salt (`credentials.rs:9–11, 78–82`) — see §7.
- **Derived material, never the password, is what's stored.** In memory it lives in a
  `Zeroizing<Vec<u8>>` keyed per vault-id (`crypto.rs:112–162`); the `Keys`/`NameCipher` structs are
  `ZeroizeOnDrop` (`credentials.rs:58`, `names.rs:14`). Opt-in "remember" writes the 80-byte material
  (hex) to the OS keyring, service `"jaynotes"` (`crypto.rs:193–200`) — again derived material, never
  the passphrase. Good discipline.
- **Wrong-password probe.** `.jaynotes-check` holds the authenticated ciphertext of a fixed marker; a
  wrong key fails its Poly1305 and unlock is rejected (`mod.rs:526–566`). The marker is known-plaintext,
  which is harmless against XSalsa20-Poly1305 (no known-plaintext weakness) and against scrypt (the
  attacker would still have to run the KDF per guess). Fine.
- **Index-key domain separation — done right.** The FTS index is itself a SQLCipher container holding
  *decrypted* note bodies, so it must be encrypted too. Its key is
  `scrypt(hex(80-byte material), salt="jaynotes-idx-v1\0", N=2¹⁵)` (`crypto.rs:220–224`): a
  deterministic function of the real key material, computable only after unlock, and cryptographically
  distinct from any content/name key via a purpose-specific constant salt. That's textbook domain
  separation. (Re-running full scrypt on already-high-entropy input is overkill — an HKDF/HMAC would do
  — but it's conservative, not wrong.)
- **Trash stays ciphertext.** Deletes move the *encrypted* file (encrypted name and all) to the OS Trash
  (`mod.rs:491–509`); unlike a plaintext vault, nothing plaintext lands in Trash. A real strength.

**Plaintext gaps that remain — by design, worth stating:**

- **AI features egress.** If the assistant is pointed at an encrypted vault, note bodies are decrypted
  and sent to the configured OpenAI-compatible endpoint, and chat history is retained. That is plaintext
  leaving the machine over the network — the encryption boundary ends at the app, and this is the widest
  practical hole if AI is used on sensitive notes.
- **Editor/OS scratch.** Plaintext exists in app memory while a note is open (unavoidable), and the
  encrypted-index DB, though itself SQLCipher-encrypted, is the one place decrypted bodies persist on
  disk locally (keyed, in app-data).

---

## 5. Threat model summary

| Adversary / scenario | Protected? | Notes |
|---|---|---|
| Lost/stolen laptop, vault locked | **Yes** | Content unreadable without password; keyring material gated by OS login/keychain. |
| Cloud/relay/backup holds synced ciphertext (passive) | **Yes for content** | Metadata (§2) leaks: sizes, tree, name equality, mtimes. |
| Forensic/casual disk recovery of backing dir | **Yes for content** | Same metadata caveats. |
| Malicious storage provider tampers/rolls back | **Partial** | Per-block tamper is caught; whole-file swap, rollback, and block-boundary truncation are not (§3). |
| Traffic analysis of sync | **No** | File count/size/timing observable on the wire and at rest. |
| Metadata inference (who/how-many/how-big/when) | **No** | Inherent to the format. |
| Weak user password + attacker has ciphertext | **Weak** | scrypt N=2¹⁴ + (often) a shared default salt; password entropy is the real defense. |
| Notes sent through AI on the vault | **No** | Plaintext egress by design. |

---

## 6. How it stacks up (one paragraph each)

- **gocryptfs** — Closest cousin. Content AEAD is comparable, but gocryptfs gives each file a *random*
  filename IV, so equal names do **not** collide, and it can bind filenames to parent-directory IVs.
  rclone-crypt is **weaker on filename privacy** precisely because its name encryption is deterministic
  and globally tweaked (§3's name-equality leak). That determinism is a feature for rclone (stable,
  dedup-friendly paths) and a cost here.
- **Cryptomator** — Similar file-per-object model with a per-vault masterkey file and per-file content
  keys; also leaks approximate sizes and tree shape, but wraps content keys per file. Broadly the same
  privacy class as rclone-crypt, arguably a bit more key hygiene. Neither hides structure.
- **restic** — Different beast: content-addressed, chunked, with repository-wide integrity (a pack/index
  Merkle-ish structure) so rollback/truncation/swap are detectable. rclone-crypt has **no whole-file or
  whole-repo integrity**, so restic is stronger against active tampering — at the cost of not being a
  transparent file mirror.
- **age** — Modern, clean AEAD file encryption with good defaults, but it's per-file blobs with no
  filename encryption and no incremental/sync story. rclone-crypt does *more* (names, streaming, sync
  compatibility); age does its narrower job with more modern primitives (ChaCha20-Poly1305, X25519) and
  no legacy KDF constant.

Net: rclone-crypt sits mid-pack — better than "roll your own," weaker than restic on integrity and
weaker than gocryptfs on filename privacy, roughly peer to Cryptomator on confidentiality.

---

## 7. Verdict

**rclone-crypt as used by JayNotes is a sound, conservatively-implemented confidentiality layer that
does exactly what its threat model asks and no more.** Content-at-rest protection is strong: NaCl-grade
XSalsa20-Poly1305 with correct per-block nonces, verified-before-release authentication, careful
zeroizing key handling, a properly domain-separated encrypted search index, and a re-implementation that
matches rclone's test vectors with no defects found. It protects well against the realistic adversaries
— laptop theft, cloud-storage snooping, casual and forensic disk access. It does **not** protect against
an *active* tampering storage provider (whole-file content-swap, rollback, and clean block-boundary
truncation are undetectable because integrity is per-block with no filename↔content binding and no
whole-file length commitment), it does **not** hide metadata (exact note sizes, directory structure,
name *equality* across folders, and modification times all leak), and it does **not** cover plaintext
that leaves the encryption boundary (notably AI-feature egress). The one dated primitive is scrypt at
N=2¹⁴, a FORMAT constant below current OWASP/Argon2id guidance — meaning the user's password strength,
not the KDF, is carrying the load. For a sync-friendly personal notes vault this is a reasonable and
honestly-engineered design; it should be described as "keeps your notes private on disk and in the
cloud," never as tamper-proof or metadata-hiding.

### Recommendations (prioritized)

1. **Always set a unique `password2` (salt).** With an empty password2 the design falls back to rclone's
   single hard-coded default salt (`credentials.rs:9–11`), shared by every rclone user on earth — which
   enables cross-user precomputation and neuters scrypt's salt. A per-vault random password2 restores
   full per-vault KDF cost and is the highest-leverage, zero-code-change win. (USAGE: consider generating
   and storing a random password2 automatically on vault creation rather than leaving it optional.)
2. **Encourage/enforce a strong vault passphrase.** Because the FORMAT pins scrypt at N=2¹⁴, password
   entropy is the actual security margin. A minimum-strength check (or a generated passphrase) at
   `create_encrypted_files_vault` matters more here than in a vault with a modern KDF.
3. **Make the active-tamper and metadata limits explicit to users, and treat AI-on-vault as an egress
   decision.** Document that the format hides note *contents*, not their existence/size/structure, and
   that it is not proof against a hostile storage provider. Where AI features touch an encrypted vault,
   surface that note text leaves the device in plaintext.

*Library-actionable, but weigh against rclone compatibility:* a format-breaking v2 could add a whole-file
length/close marker (kills silent truncation) and pass the filename as AEAD associated data (kills
content-swap). Both are cheap cryptographically. Neither is possible without abandoning rclone
interoperability — which is this format's entire reason for existing — so they belong in a deliberately
*non*-rclone "JayNotes-native" mode, if ever, not in the compatible path.
