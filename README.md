# JayNotes

A simple, clean Obsidian-style markdown notes desktop app. Point it at a folder
("vault") of `.md` files and get a fast file tree, a live-preview editor,
full-text search, tags, and `[[wikilinks]]` — with a light and a dark theme.

## Stack

- **Tauri 2** (Rust backend, native macOS window)
- **SvelteKit** (Svelte 5 runes) as an SPA via `adapter-static`
- **TypeScript** (strict)
- **Milkdown Crepe** — the live-preview markdown editor
- **SQLite FTS5** (via `rusqlite`) — the on-disk search/tags/links index
- File watching via `notify-debouncer-full`

## Features

- Vault picker; a live file tree with create / rename / move-to-Trash / reveal.
- Live-preview editor with autosave, an editable title, and a frontmatter
  properties bar (tags + fields).
- Full-text search (`tag:name` filters) and a `Cmd+P` quick switcher.
- `[[Wikilinks]]` render as accent-colored links; **Cmd/Ctrl+Click** opens the
  target, creating the note in the vault root if it doesn't exist yet.
- Light / dark / system themes; a settings popover to change vault or rebuild
  the index.

## Development

```sh
npm install
npm run tauri dev      # run the desktop app with hot reload
```

Frontend-only tools:

```sh
npm run check          # svelte-check (TypeScript) — expect 0 errors
npm test               # vitest unit tests
npm run build          # build the SvelteKit SPA into build/
```

Rust backend (from `src-tauri/`):

```sh
cargo test             # unit + integration tests
cargo check --all-targets   # expect 0 warnings
```

## Build

```sh
npm run tauri build
```

Produces a macOS bundle at
`src-tauri/target/release/bundle/macos/JayNotes.app` (and a `.dmg` under
`bundle/dmg/`). Unsigned/ad-hoc signing is fine for personal use.

To regenerate the app icons from a 1024×1024 source PNG:

```sh
npm run tauri icon path/to/icon.png
```

## Keyboard shortcuts

| Shortcut          | Action                          |
| ----------------- | ------------------------------- |
| `Cmd/Ctrl+P`      | Quick switcher (open a note)    |
| `Cmd/Ctrl+O`      | Quick switcher (same as above)  |
| `Cmd/Ctrl+Shift+F`| Focus search                    |
| `Cmd/Ctrl+E`      | Show the Files tab              |
| `Cmd/Ctrl+N`      | New note                        |
| `Cmd/Ctrl+Click`  | Follow a `[[wikilink]]`         |
| `Enter` / `Esc`   | Commit / cancel an inline edit  |

## Where things live

- **Settings** (`settings.json`, including the vault path) — the OS app-config
  directory. On macOS: `~/Library/Application Support/biz.tudisco.jaynotes/`.
- **Search index** — one SQLite database per vault under `indexes/` in the OS
  app-data directory (same base folder on macOS), keyed by a hash of the vault
  path, so the vault folder itself stays clean.
- **Notes** — plain `.md` files in your chosen vault folder. Nothing about a
  note lives anywhere but the file; the index is a rebuildable cache
  ("Rebuild index" in the settings popover).
