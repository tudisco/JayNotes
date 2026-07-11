# JayNotes — Simple Obsidian-style Notes App (Tauri + Svelte)

## Context

Build a personal, simple Obsidian clone in `/Volumes/WorkDrive/Hot/Jason3/JayNotes` (currently empty). The user wants:

- **Folder-based** markdown vault — plain `.md` files on disk, portable
- **Clean, minimal UI** modeled on the Notebook++ screenshot (white/off-white, teal accent, generous whitespace) **plus a dark mode**
- **Live-preview editor** (Obsidian-style: type markdown, it renders in place) with first-class **code blocks** for pasting code/shell commands
- **Tags + metadata** via YAML frontmatter (Obsidian-compatible), plus inline `#tags`
- **Wikilinks** (`[[note]]`) supported but low priority — user rarely uses them
- **Fast search** backed by SQLite FTS5, reindexed on every change
- Working base first; more features later

Decisions confirmed with user: live-preview editor, YAML frontmatter, pick-a-folder vault (remembered), **Svelte + TypeScript** frontend.

This plan is also written to `PLAN.md` in the repo root as step 0 so it lives with the project.

## Tech Stack

| Layer | Choice | Why |
|---|---|---|
| Shell | **Tauri 2** (Rust) | Native app, small binary, Rust backend for file I/O + indexing |
| Frontend | **Svelte 5 + TypeScript + Vite** | User's choice; light and fast |
| Editor | **Milkdown Crepe** (`@milkdown/crepe`) | ProseMirror-based markdown live-preview out of the box: GFM, fenced code blocks with syntax highlighting + language picker, tables, task lists. Framework-agnostic (mounts into a Svelte component cleanly). |
| Index/Search | **rusqlite** (bundled SQLite) + **FTS5** | Fast full-text search with bm25 ranking, prefix matching, snippets |
| File watching | **notify** crate | Catch external edits (e.g. from another editor) and reindex |
| Frontmatter | **gray_matter** crate (Rust side) | Parse YAML frontmatter during indexing; frontend parses/edits it too (small TS util) |
| Deletes | **trash** crate | Delete moves to OS Trash, never hard-deletes |
| Persistence of settings | JSON config file in Tauri app-config dir | Remembers vault path, theme; no plugin needed |

## Architecture

```
┌─ Svelte frontend ────────────────────────────────┐
│ Sidebar (file tree, search, tags)                │
│ Editor pane (Crepe live-preview + properties bar)│
│ Quick switcher (Cmd+P) / Search panel (Cmd+Shift+F)
└──────────────┬───────────────────────────────────┘
               │ invoke() commands + events
┌──────────────┴───────────────────────────────────┐
│ Rust backend (Tauri commands)                    │
│  vault: open/scan/read/write/create/rename/trash │
│  index: SQLite FTS5, incremental reindex         │
│  watcher: notify → reindex → emit "vault-changed"│
└──────────────────────────────────────────────────┘
```

- **All file I/O goes through Rust commands** (not tauri-plugin-fs from JS) so every write triggers a reindex in the same call.
- **Index DB lives in the app-data dir**, keyed by vault path hash — vault folder stays clean, pure markdown.
- **Autosave**: editor debounces ~600ms after typing stops → `save_note` command → write file + reindex that one file.
- **External changes**: `notify` watcher debounces events → reindex changed files → emit event → frontend refreshes tree/editor if the open file changed on disk.

## SQLite Schema

```sql
notes(id INTEGER PK, path TEXT UNIQUE, title TEXT, mtime INTEGER,
      size INTEGER, frontmatter TEXT /* JSON */);
notes_fts(title, body, path UNINDEXED) USING fts5;  -- content synced with notes
tags(note_id, tag);            -- from frontmatter `tags:` + inline #tags
links(source_id, target_path); -- [[wikilinks]], indexed even if unused for now
meta(key, value);              -- schema version, last full scan
```

Incremental indexing: on vault open, walk the tree (walkdir), compare stored `mtime`+`size`, only re-parse changed files. Full scan is a fallback command.

Search: FTS5 `bm25()` ranking, `snippet()` for result context, `prefix` tokenizer options for as-you-type. `tag:foo` filter joins the tags table.

## UI (matching the screenshot + dark twin)

- **Left sidebar**: vault file tree (folders collapsible), new note / new folder buttons, star/favorites later. Context menu: rename, delete (→ Trash), reveal in Finder.
- **Main pane**: note title as H1-style editable field, Crepe editor below.
- **Properties bar** above the editor body: tag chips (add/remove) + key:value metadata rows, collapsed by default — edits write back to YAML frontmatter. Frontmatter is stripped from the editor content so raw `---` blocks never show in the live preview.
- **Cmd+P**: quick switcher (fuzzy on title/path). **Cmd+Shift+F**: full-text search panel with highlighted snippets. **Cmd+N**: new note.
- **Theme**: light (screenshot palette: near-white bg, `#0f766e`-ish teal accent, dark slate text) + dark variant; CSS custom properties, follows system with manual toggle, persisted in settings.
- Code blocks: monospace, subtle bg, language label + copy button (Crepe provides this), syntax highlighting.

## Execution Strategy — Opus 4.8 agents do the coding

Fable 5 (this session) acts as **planner/orchestrator/reviewer**, not primary coder — to save cost:

- Each milestone below is delegated to an **Opus 4.8 agent** (`Agent` tool with `model: "opus"`) with a self-contained prompt: the relevant plan section, decisions already made, file paths, and acceptance criteria.
- Milestones run **sequentially** (each builds on the last). Within a milestone, independent pieces (e.g. M4's Rust indexer vs. M5's Svelte search UI scaffolding) can be parallel Opus agents when they don't touch the same files.
- After each agent finishes, Fable reviews the diff, runs the build (`npm run tauri dev` / `cargo check`), and verifies the milestone's acceptance criteria before starting the next.
- **Fable jumps in directly only when** an Opus agent gets stuck (repeated build failures, architectural confusion, gnarly Rust/ProseMirror integration issues) — fixes the blocker, then hands back to Opus agents.

## Milestones

**M0 — Scaffold** *(repo root gets PLAN.md — this document)*
`npm create tauri-app` → Svelte + TS + Vite template, Tauri 2. App shell: sidebar + main pane layout, CSS variable theme system (light+dark), window config. Verify `npm run tauri dev` opens the window.

**M1 — Vault & file tree**
Folder picker (tauri dialog plugin) on first run; persist vault path in app-config JSON; auto-open on later launches. Rust commands: `scan_vault`, `read_note`, `write_note`, `create_note`, `create_folder`, `rename_path`, `trash_path`. Svelte file tree with expand/collapse, selection, context menu.

**M2 — Editor**
Crepe integration in a Svelte component. Open note → strip frontmatter → load body. Debounced autosave reassembles frontmatter + body and writes via Rust. Verify: paste a shell script and a code fence, confirm highlighting and copy button; kill app, confirm content persisted.

**M3 — Frontmatter, tags & metadata**
TS frontmatter util (parse/serialize YAML). Properties bar with tag chips and metadata key/values. Inline `#tag` recognition (regex on body at index time).

**M4 — SQLite index**
rusqlite + FTS5 schema above. Full scan on vault open, incremental by mtime. Reindex-on-save wired into `write_note`. `notify` watcher → debounce → reindex → `vault-changed` event → frontend refresh (and "file changed on disk" reload for the open note).

**M5 — Search**
`search_notes(query)` command: FTS5 match with bm25 + snippets + `tag:` filter. Cmd+P quick switcher (title/path fuzzy). Cmd+Shift+F search panel with results list, click → open note (later: scroll to match).

**M6 — Polish & basic wikilinks**
Dark mode pass over every surface. Keyboard shortcuts (Cmd+N, Cmd+P, Cmd+Shift+F, Cmd+, theme toggle). Minimal wikilinks: `[[Note Name]]` rendered as a link in the editor, click navigates (resolve via index by title/path); autocomplete popup can come later. App icon + `tauri build` to produce the .app.

**M7 — Local image support** *(added 2026-07-10 after v1 shipped)*
Paste or drag an image into the editor → saved as a real file under `attachments/` in the vault (Rust `save_attachment` command: sanitized, uniquified name), inserting a standard relative `![](attachments/…)` link so files stay Obsidian-portable. Rendering via Tauri asset protocol (vault dir allowed at runtime on vault open) + Crepe `proxyDomURL` resolving relative paths to asset URLs for display only. `dragDropEnabled: false` on the window so HTML5 drops reach ProseMirror. Remote https images keep working.

**M8 — Tags panel** *(added 2026-07-10 after v1 shipped)*
Third sidebar tab "Tags" next to Files/Search: `list_tags()` Rust command (tag + note count from the tags table), tag list with counts, click a tag → notes carrying it (reuses `tag:` search), click note opens. Refreshes on vault-changed events.

**M13 — Multi-vault** *(feature_vault_upgrade branch, added 2026-07-11)*
Settings gain `vaults: [{id, name, path, kind}]` + `activeVaultId` (migrating the old single `vaultPath`). Vault switcher UI (add existing / create new / remove / switch; remembers all vaults). Startup pruning: a vault whose folder is gone from a MOUNTED volume is auto-removed (with a notice); a vault on an unmounted volume (external drive) is kept and shown "offline". Atomic writes (temp+rename) everywhere for Syncthing safety. Index/watcher already per-vault; re-init on switch.

**M14 — Encrypted vault (SQLCipher container, sync-safe)** *(feature_vault_upgrade branch; user decision 2026-07-11 — replaces the earlier rclone-crypt plan)*
Vault kind `encrypted`: notes + attachments (blobs) + FTS index live in ONE SQLCipher database. Syncthing-safe via snapshot-and-merge: the live DB stays in app-data (never synced); the vault folder holds `vault.jaynotes`, a consistent snapshot exported by `VACUUM INTO` + atomic rename after each debounced edit burst. External snapshot changes and `*.sync-conflict-*` siblings are opened with the same key and merged ROW-WISE: per-note last-writer-wins, concurrent edits keep the loser as a visible "(conflict)" note, deletion tombstones prevent resurrection. Password prompt per session; optional cross-platform keyring remember (`keyring` crate); KDF layer designed so FIDO2 hmac-secret/PRF passkey unlock can slot in later. VaultBackend abstraction (fs vs db) so editor/AI/PDF/search all route transparently; images render from blobs (data-URI or protocol handler).

## Verification

- Each milestone: run `npm run tauri dev` and exercise the feature by hand (create/edit/search notes).
- M2: paste code + shell commands, restart app, confirm persistence and rendering.
- M4: edit a note in an external editor while the app runs → tree/search reflect it within ~1s.
- M5: create 3 notes with distinct words + tags; verify search hits, snippets, and `tag:` filtering; quick-switcher finds by partial title.
- Final: `npm run tauri build` produces a working macOS .app; open vault, full flow smoke test.

## Out of scope for v1 (future ideas)

Graph view, backlinks panel, wikilink autocomplete, multiple vaults UI, canvas/kanban/table blocks (from the screenshot's app), sync, plugins, export.
