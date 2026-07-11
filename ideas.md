# JayNotes — Feature Ideas

Backlog of possible features. Nothing here is committed work — it's a menu.

## CSV data in notes
Support CSV files as first-class note content. Came up during the image
migration (2026-07-11): a couple of notes embed CSVs Obsidian-style
(`![[pfg_import_report.csv]]`, `![[invoicedetail (2).csv]]` in
`Projects/PosReporting/Fintech Info/Fintech Notes.md`) and JayNotes currently
ignores them — the files weren't migrated.
- Minimal: allow CSVs in `attachments/`, render `![](attachments/x.csv)` as a
  read-only sortable table in the editor (parse client-side, cap rows shown).
- Bigger: a CSV/table view mode with column sorting + filtering; AI tools to
  read CSV data so the assistant can answer questions about it.
- Migration follow-up: copy those two CSVs over once supported.

## Shipped from this list
- PDF export — shipped as M10 (pure-Rust Typst pipeline, cross-platform).
