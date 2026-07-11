# JayNotes — Feature Ideas

Backlog of possible features. Nothing here is committed work — it's a menu.

## PDF export
Export the current note as a nicely formatted PDF (same typography as the editor,
code blocks with highlighting, images included).
- Likely approach: render the note HTML in a hidden webview → native print-to-PDF
  (`window.print()` / WKWebView PDF API via Tauri), or a Rust-side
  markdown→HTML→PDF pipeline.
- Nice extras: export folder to PDFs, page margins/header options in Settings.
