# JayNotes — agent orientation map

Obsidian-style markdown notes app. Tauri 2.11 (Rust) + SvelteKit SPA (Svelte 5 runes, TS strict).
**Trust this map — read only the files named for your task; don't re-scan the codebase.**

## Verify (all must pass before committing)
- `cd src-tauri && cargo test` · `cargo check --all-targets` (0 warnings) — also with `--no-default-features` when touching providers
- `npm run check` (0 errors) · `npm test` (vitest) · `npm run build`
- Commits: conventional style + trailer `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>`. Commit only your own files.

## Rust backend (src-tauri/src/)
- `lib.rs` — plugin init, AppState manage, ALL command registration (per-entry `#[cfg(feature)]` supported), startup setup.
- `vault.rs` — Settings (vaults[], activeVaultId, serde-flattened unknown keys preserved), `atomic_write` (temp+rename, always use for vault files), file-op commands dispatch through `with_active(state, |handle| ...)`.
- `vaults.rs` — vault list/add/create/remove/rename/switch; status ok/offline/missing (offline = /Volumes root unmounted → never pruned).
- `providers/mod.rs` — `VaultProvider` trait (metadata: ConfigField list, Capabilities{reveal_in_finder, needs_unlock, folder_backed}) + `VaultHandle` trait (all storage ops; search overrides w/ `owns_index()`/`owns_reindex()`); feature-gated registry. Providers: `plain.rs`, `encrypted_db.rs` (SQLCipher container; container IS its index; live DB in app-data + VACUUM INTO snapshot `.jaynotes` file), `encrypted_files/` (rclone-crypt file-per-note; cipher.rs maps names incl. `.sync-conflict-` surfacing), `tinylord/` (hosted doc server; SSE live sync, client.rs). Cargo features: `provider-encrypted-db`, `provider-encrypted-files`, `provider-tinylord` (all default-on; public builds exclude).
- `providers/unlock.rs` — shared unlock commands by kind; `SecretsSession` (per-vault-id opaque key material, Zeroizing); keyring remember (`keyring` crate, service "jaynotes").
- `crypto.rs` — scrypt KDF, index-key derivation.
- `index.rs` — per-vault SQLite FTS5 index at app_data/indexes/<fnv1a-hash>.db (`Index::open` / `open_keyed` for SQLCipher); `search_notes`/`list_notes`/`list_tags`/`notes_by_tag`/`resolve_note`; `dispatch_*` helpers branch active-handle vs state.index.
- `watcher.rs` — fs watcher (plain + ciphertext-aware for encrypted-files); self-write suppression via `register_write` recent-writes map (2s window).
- `ai.rs` + `ai/` — agent loop (`ai_chat_send` streams AiEvent over ipc Channel: token/reasoning/toolCall/toolResult/permissionRequest/done/error), `ai/client.rs` (OpenAI-compatible SSE, think-tag splitter), `ai/tools.rs` (18 tools, ALL dispatch through the active handle; `Gate` helper = permission-gated deletes/moves), `ai/settings.rs` (masked key), `ai/revisions.rs` (undo snapshots; handle-backed `.revisions/` for encrypted vaults).
- `pdf.rs` — markdown → comrak → Typst emitter → typst-pdf; fonts via typst-assets; `export_note_pdf`.

## Frontend (src/lib/)
- `stores/vault.ts` — vaults/activeVaultId/fileTree/selected/expanded; openContextMenu/renamingPath/commitRename/moveNote/deleteToTrash (bumps noteSaved); refreshTree.
- `stores/indexEvents.ts` — `vaultChanged` (external changes via Rust event) + `noteSaved` (in-app saves; self-writes never emit vault-changed — subscribe to BOTH for live lists).
- `stores/ui.ts` (sidebarMode files/search/tags, filesView tree/recent, quickSwitcherOpen, vaultSwitcherOpen), `stores/search.ts` (invoke wrappers), `stores/chat.ts` + `chatReducer.ts` (pure AiEvent→ChatEntry reducer, tested), `stores/editorBridge.ts` (flush registry + reload nonce).
- `components/`: Sidebar (tab strip + panels), VaultSwitcher (popover, status dots, provider-aware create), FileTree / RecentList / TagsPanel / SearchPanel (all reuse shared ContextMenu via synthesized TreeNode `nodeOf` pattern), EditorPane (title + Export/Move/Trash header buttons w/ anchored-popover pattern; owns frontmatter $state), Editor.svelte (Crepe; wikilink + image paste/drop ProseMirror plugins; `flush()` exported; proxyImageURL: convertFileSrc for plain, data-URI command for non-plain), PropertiesBar, QuickSwitcher, ChatSidebar + chat/ (MarkdownView hardened marked renderer, ToolChip, PermissionCard, ThinkingBlock, AiSettingsPanel).
- `utils/`: frontmatter.ts (verbatim split/join), metadata.ts (YAML parse/serialize, fence-aware extractInlineTags), fuzzy.ts, markdown.ts (chat renderer + note-link preprocess), path.ts (collectFolderPaths), time.ts, url.ts.

## Cross-cutting rules
- Every vault write goes through the active VaultHandle + registers self-write; UI refresh via noteSaved/vaultChanged.
- Capabilities gate UI (e.g. reveal_in_finder hidden for db/hosted vaults).
- Markdown on disk stays clean/portable (relative attachment paths, never asset URLs).
- Chat/assistant markdown rendering: never `{@html}` unsanitized; follow MarkdownView's escape-first approach.
- Deletes: OS trash (plain/encrypted-files), tombstone (encrypted-db), server delete (tinylord) — never hard fs delete.
