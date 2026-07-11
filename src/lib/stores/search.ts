// Thin wrappers over the Rust search commands, plus their result types.

import { invoke } from "@tauri-apps/api/core";

/** A full-text / tag search result. Mirrors `SearchHit` in `index.rs`. */
export interface SearchHit {
  path: string;
  title: string;
  /** May contain `<mark>…</mark>` around matched terms; otherwise plain text. */
  snippet: string;
  /** bm25 rank (lower = better), or 0 for tag-only queries. */
  score: number;
  tags: string[];
}

/** A lightweight note reference for the quick switcher. Mirrors `NoteRef`. */
export interface NoteRef {
  path: string;
  title: string;
  mtime: number;
}

export function searchNotes(query: string, limit?: number): Promise<SearchHit[]> {
  return invoke<SearchHit[]>("search_notes", { query, limit });
}

export function listNotes(): Promise<NoteRef[]> {
  return invoke<NoteRef[]>("list_notes");
}
