# JayNotes — Feature Ideas

Backlog of possible features. Nothing here is committed work — it's a menu.

## Queued up (things Jason actually wants)

### PDF export
Export the current note as a nicely formatted PDF (same typography as the editor,
code blocks with highlighting, images included).
- Likely approach: render the note HTML in a hidden webview → native print-to-PDF
  (`window.print()` / WKWebView PDF API via Tauri), or a Rust-side
  markdown→HTML→PDF pipeline (e.g. headless render + `wkhtmltopdf`-style step).
- Nice extras: export folder to PDFs, page margins/header options in Settings.

---

## Plugin-inspired ideas (from Obsidian's most-downloaded community plugins)

Obsidian's plugin download charts are a decent proxy for "what do heavy note-takers
actually want beyond the basics." (Source: obsidianstats.com, July 2026.)
Grouped by how they'd translate to JayNotes.

### Would fit JayNotes' "simple + fast" philosophy well
| Rank | Plugin (downloads) | What it does | JayNotes translation | Effort |
|---|---|---|---|---|
| 6 | Calendar (2.9M) | Calendar pane for daily notes | Calendar popover + daily-note-per-day (`2026-07-11.md`) | Small |
| 7 | Git (2.8M) | Versions/syncs the vault with git | Auto-commit vault on save (git2 crate), history browser later | Small–Med |
| 15 | Omnisearch (1.6M) | Smarter fuzzy full-text search | We already have FTS5 — add typo-tolerance (trigram tokenizer) & better ranking | Small |
| 21 | Recent Files (1.1M) | Recently opened list in sidebar | Recents section above the file tree, or in Cmd+P empty state (partly done) | Tiny |
| 23 | Tag Wrangler (1.0M) | Rename/merge tags vault-wide | Right-click a tag in our Tags panel → rename everywhere | Small |
| 5 | Advanced Tables (3.0M) | Spreadsheet-like markdown table editing | Crepe already edits GFM tables; add row/col shortcuts + alignment UI | Small |
| 24 | Linter (0.95M) | Auto-format notes consistently | On-save tidy: trailing whitespace, heading spacing, frontmatter order | Small |
| 20 | Homepage (1.2M) | Open a chosen note on launch | "Open last note / a pinned note on startup" setting | Tiny |

### Popular, plausible later — bigger lifts
| Rank | Plugin (downloads) | What it does | JayNotes translation | Effort |
|---|---|---|---|---|
| 2 | Templater (4.7M) | Dynamic note templates | Simple templates folder + variables ({{date}}, {{title}}) — skip the JS engine | Medium |
| 4 | Tasks (3.7M) | Vault-wide task tracking with due dates | Index `- [ ]` items in SQLite → Tasks panel (we already parse every note) | Medium |
| 9 | Kanban (2.4M) | Markdown-backed kanban boards | A `.kanban.md` view mode — columns from headings, cards from list items | Medium |
| 12 | QuickAdd (1.9M) | Quick-capture macros | Global quick-capture hotkey → append to inbox note | Medium |
| 11 | Remotely Save (2.0M) | Sync vault to S3/Drive/WebDAV | Vault is plain files — document iCloud/Syncthing; native sync only if pain | Medium–Large |
| 17 | Importer (1.4M) | Import from Evernote/Notion | One-shot import scripts, only if ever needed | Medium |

### AI tier (three of the top 22 are AI plugins — clearly in demand)
| Rank | Plugin (downloads) | What it does | JayNotes translation | Effort |
|---|---|---|---|---|
| 16 | Copilot (1.5M) | AI chat/writing help over your notes | "Ask my notes": local embeddings or API + our SQLite index as retrieval | Large |
| 18 | Claudian (1.3M) | Claude as an agent inside the vault | Shell out to `claude` CLI with vault as cwd — cheap integration, big power | Medium |
| 22 | Smart Connections (1.1M) | Auto-suggest related notes | "Related notes" footer via embeddings, or cheap version: shared-tag/link overlap | Medium (cheap ver: Small) |

### Probably never (conflicts with "simple", or Obsidian-specific)
- **Excalidraw** (#1, 6.6M) — full sketching app embedded; huge dependency, separate tool does it better.
- **Dataview** (#3, 4.5M) — query language over notes; powerful but the opposite of simple. Our Tags panel + search covers the common cases.
- **Style Settings / Minimal Theme / Iconize / Editing Toolbar** — theme-plumbing for Obsidian's ecosystem; JayNotes ships one good theme in two modes instead.
- **Outliner** — opinionated list-editing mode; Crepe's default list UX is fine.
- **Admonition** — callout boxes; could do a light version via blockquote styling if ever wanted.

## Previously discussed, parked
- Backlinks panel (links table already indexed — cheapest big feature if linking ever becomes a habit)
- Wikilink autocomplete on `[[`
- Rename rewrites `[[links]]` across vault (data integrity)
- Tabs / split panes
- Outline panel from headings
- Graph view (eye candy; skip)
