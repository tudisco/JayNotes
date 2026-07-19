# JayNotes

A simple, clean Obsidian-style markdown notes desktop app. Fast file tree,
live-preview editor, full-text search, tags, `[[wikilinks]]`, images, PDF
export, an AI assistant over your notes — and four kinds of vaults, from a
plain folder of `.md` files to fully encrypted and server-hosted stores.
Light and dark themes throughout.

## Vault types

| Type | Storage | Best for |
| --- | --- | --- |
| **Plain folder** | `.md` files on disk | Portability, Syncthing/iCloud, Obsidian compatibility |
| **Encrypted database** | One SQLCipher `.jaynotes` file (notes, images, search index all inside) | Locked-down single-device vaults |
| **Encrypted files** | rclone-crypt file-per-note (AES-EME names, XSalsa20-Poly1305 content) | Encrypted **and** Syncthing-synced — per-note conflicts surface in the tree; recoverable with stock [rclone](https://rclone.org/crypt/) |
| **TinyLord** | Self-hosted document server, live SSE sync | Multi-device in near-real-time, server as source of truth |

Non-plain vault types are compile-time **provider modules** behind Cargo
features (`provider-encrypted-db`, `provider-encrypted-files`,
`provider-tinylord`, all default-on). Build with `--no-default-features` plus
the features you want and the UI adapts automatically — omitted providers
leave no trace.

Related projects:

- **[RcloneCryptRustLib](https://github.com/tudisco/RcloneCryptRustLib)** — the
  rclone-compatible encryption library behind encrypted-files vaults. See
  [`docs/encryption-security-analysis.md`](docs/encryption-security-analysis.md)
  for an honest security review of the design (including a post-quantum note).
- **[TinyLord](https://github.com/tudisco/tinylord)** — the small self-hosted
  application server (auth, JSON documents, realtime SSE) behind TinyLord
  vaults.

## Features

- Live-preview editor (Milkdown Crepe) with autosave, editable title, dimmed
  folder path, and a frontmatter properties bar (tags + fields).
- Paste or drag images — saved to `attachments/`, clean relative markdown links.
- Export any note to PDF (embedded Typst — pure Rust, no printing involved).
- Full-text search (SQLite FTS5, `tag:name` filters), `Cmd+P` quick switcher,
  a Tags panel, and a recent-files view.
- `[[Wikilinks]]`: **Cmd/Ctrl+Click** opens (creating if missing).
- Move notes between folders — or between vaults (e.g. plain → encrypted),
  attachments travel along.
- **AI assistant** (collapsible right sidebar): any OpenAI-compatible provider
  (OpenAI, OpenRouter, Ollama, MiniMax…). Searches, reads, writes, organizes,
  and links notes through 18 tools; destructive actions ask permission; every
  AI edit has one-click revert; reasoning-model "thinking" renders collapsed.
- Multi-vault switcher with offline-drive detection; encrypted vaults unlock
  per session with optional OS-keyring remember.

## Stack

Tauri 2 (Rust) · SvelteKit/Svelte 5 (strict TS) · Milkdown Crepe ·
SQLite FTS5 (SQLCipher-capable via `rusqlite`) · comrak + Typst (PDF) ·
`notify` file watching.

## Development

```sh
npm install
npm run tauri dev      # run the desktop app with hot reload
```

```sh
npm run check          # svelte-check — expect 0 errors
npm test               # vitest
npm run build          # SvelteKit SPA build
```

Rust backend (from `src-tauri/`):

```sh
cargo test
cargo check --all-targets            # expect 0 warnings
cargo check --no-default-features    # plain-only "public" build must stay clean
```

> **Note for cloners:** the `provider-encrypted-files` feature currently
> resolves [RcloneCryptRustLib](https://github.com/tudisco/RcloneCryptRustLib)
> via a local path dependency. Either clone that repo and adjust the path in
> `src-tauri/Cargo.toml`, or build without that feature:
> `cargo build --no-default-features --features provider-encrypted-db,provider-tinylord`.

## Build

```sh
npm run tauri build
```

Produces `src-tauri/target/release/bundle/macos/JayNotes.app` (and a `.dmg`).
Unsigned/ad-hoc signing is fine for personal use.

## Keyboard shortcuts

| Shortcut           | Action                          |
| ------------------ | ------------------------------- |
| `Cmd/Ctrl+P` / `O` | Quick switcher                  |
| `Cmd/Ctrl+Shift+F` | Focus search                    |
| `Cmd/Ctrl+Shift+A` | Toggle AI assistant             |
| `Cmd/Ctrl+E`       | Show the Files tab              |
| `Cmd/Ctrl+N`       | New note                        |
| `Cmd/Ctrl+Click`   | Follow a `[[wikilink]]`         |
| `Enter` / `Esc`    | Commit / cancel an inline edit  |

## Where things live

- **Settings** (`settings.json`: vault list, AI config) — the OS app-config
  dir; macOS: `~/Library/Application Support/biz.tudisco.jaynotes/`.
- **Search indexes** — one SQLite DB per vault under `indexes/` in app-data
  (SQLCipher-keyed for encrypted vaults). Encrypted-db vaults carry their
  index *inside* the container instead.
- **Notes** — in your vault, in whatever form its provider defines; for plain
  vaults that's just `.md` files, and the index is always a rebuildable cache.
- **AI revisions** (undo snapshots for AI edits) — app-data for plain vaults,
  stored encrypted inside the vault for encrypted ones.
