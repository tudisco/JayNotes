//! PDF export: render a note's markdown into a paginated PDF entirely in Rust.
//!
//! Pipeline: markdown → comrak AST → a Typst markup string (see [`render_note_typst`])
//! → compiled in-process by the bundled Typst engine → PDF bytes. Fonts ship
//! with the binary via `typst-assets`, so a note renders byte-for-byte the same
//! on any machine with no network access at export time.
//!
//! Why Typst rather than an HTML/print path: it is pure Rust, cross-platform,
//! and produces genuinely paginated output (page breaks, margins, page numbers)
//! with built-in syntect code highlighting and image embedding — none of which
//! HTML gives you without a browser.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use comrak::nodes::{AstNode, ListType, NodeValue, TableAlignment};
use comrak::{parse_document, Arena, Options};
use tauri_plugin_dialog::DialogExt;

use typst::diag::{FileError, FileResult};
use typst::foundations::{Bytes, Datetime, Duration};
use typst::syntax::{FileId, RootedPath, Source, VirtualPath, VirtualRoot};
use typst::text::{Font, FontBook};
use typst::utils::LazyHash;
use typst::{Library, LibraryExt, World};
use typst_layout::PagedDocument;
use typst_pdf::PdfOptions;

// ---------------------------------------------------------------------------
// Typst document preamble
// ---------------------------------------------------------------------------

/// Page geometry and light-theme styling shared by every export. The PDF is
/// paper, so it always renders light regardless of the app theme.
const PREAMBLE: &str = r##"#set document(title: "Note")
#set page(paper: "us-letter", margin: (x: 0.9in, y: 1in), numbering: "1")
#set text(size: 11pt)
#set par(justify: false, leading: 0.62em)
#set heading(numbering: none)
#show link: it => text(fill: rgb("#2563eb"), it)
#show raw.where(block: true): set block(fill: rgb("#f6f8fa"), inset: 8pt, radius: 4pt, width: 100%)
#let jnquote(body) = block(inset: (left: 0.9em), spacing: 1.1em, stroke: (left: 2pt + rgb("#d0d7de")))[#body]

"##;

// ---------------------------------------------------------------------------
// Public entry: markdown → Typst markup (pure, unit-testable)
// ---------------------------------------------------------------------------

/// Renders a note (its filename `title` plus raw file `contents`) into a
/// self-contained Typst markup document. Frontmatter is stripped (its tags
/// optionally surface as a subtle line under the title); the title becomes a
/// top-level heading. Image paths are left vault-relative — the [`PdfWorld`]
/// resolves and embeds them at compile time.
pub fn render_note_typst(title: &str, contents: &str) -> String {
    let (tags, body) = strip_frontmatter(contents);

    let arena = Arena::new();
    let mut options = Options::default();
    options.extension.strikethrough = true;
    options.extension.table = true;
    options.extension.autolink = true;
    options.extension.tasklist = true;
    let root = parse_document(&arena, body, &options);

    let mut out = String::with_capacity(contents.len() * 2);
    out.push_str(PREAMBLE);

    let mut title_esc = String::new();
    escape_markup(title, &mut title_esc);
    out.push_str(&format!("#heading(level: 1)[{title_esc}]\n\n"));

    if !tags.is_empty() {
        let mut line = String::new();
        for (i, tag) in tags.iter().enumerate() {
            if i > 0 {
                line.push_str("  ");
            }
            line.push_str("\\#");
            escape_markup(tag, &mut line);
        }
        out.push_str(&format!("#text(fill: luma(130), size: 9pt)[{line}]\n\n"));
    }

    emit_blocks(root, &mut out);
    out
}

// ---------------------------------------------------------------------------
// Public entry: Typst markup → PDF bytes
// ---------------------------------------------------------------------------

/// Compiles a Typst markup document into PDF bytes, resolving any embedded
/// image paths against `vault_root`. Returns the bytes and the page count.
pub fn markup_to_pdf(vault_root: &Path, markup: &str) -> Result<(Vec<u8>, usize), String> {
    let world = PdfWorld::new(vault_root.to_path_buf(), markup.to_string());
    let compiled = typst::compile::<PagedDocument>(&world);
    let document = compiled
        .output
        .map_err(|diags| format!("Typst compile failed: {}", diag_msg(&diags)))?;
    let pages = document.pages().len();
    let pdf = typst_pdf::pdf(&document, &PdfOptions::default())
        .map_err(|diags| format!("PDF export failed: {}", diag_msg(&diags)))?;
    Ok((pdf, pages))
}

fn diag_msg<I>(diags: I) -> String
where
    I: IntoIterator,
    I::Item: std::fmt::Debug,
{
    diags
        .into_iter()
        .map(|d| format!("{d:?}"))
        .collect::<Vec<_>>()
        .join("; ")
}

// ---------------------------------------------------------------------------
// Tauri command
// ---------------------------------------------------------------------------

/// Exports the note at `rel_path` to a PDF the user chooses via a native save
/// dialog (defaulting to `<title>.pdf`). Returns the written path, or an empty
/// string when the user cancels the dialog.
#[tauri::command]
pub async fn export_note_pdf(app: tauri::AppHandle, rel_path: String) -> Result<String, String> {
    let root = crate::vault::vault_root(&app)?;
    let note_path = crate::vault::safe_join(&root, &rel_path)?;
    if !note_path.is_file() {
        return Err(format!("Note does not exist: {rel_path}"));
    }
    let raw = fs::read_to_string(&note_path)
        .map_err(|e| format!("Could not read note '{rel_path}': {e}"))?;
    let title = note_title(&rel_path);
    let markup = render_note_typst(&title, &raw);

    // Ask where to save. `None` means the user cancelled.
    let chosen = app
        .dialog()
        .file()
        .set_file_name(format!("{title}.pdf"))
        .add_filter("PDF Document", &["pdf"])
        .blocking_save_file();
    let dest = match chosen {
        Some(f) => f
            .into_path()
            .map_err(|e| format!("Invalid destination: {e}"))?,
        None => return Ok(String::new()),
    };

    let (pdf, _pages) = markup_to_pdf(&root, &markup)?;
    fs::write(&dest, &pdf).map_err(|e| format!("Could not write PDF: {e}"))?;
    Ok(dest.to_string_lossy().into_owned())
}

/// The note's display title: its filename with any `.md` extension removed.
fn note_title(rel: &str) -> String {
    let base = rel.rsplit('/').next().unwrap_or(rel);
    let stem = if base.len() >= 3 && base[base.len() - 3..].eq_ignore_ascii_case(".md") {
        &base[..base.len() - 3]
    } else {
        base
    };
    if stem.is_empty() {
        "Untitled".to_string()
    } else {
        stem.to_string()
    }
}

// ---------------------------------------------------------------------------
// Frontmatter
// ---------------------------------------------------------------------------

/// Splits a leading `---` YAML frontmatter block off the note, returning its
/// parsed tag list and the remaining body. Mirrors `frontmatter.ts`: the block
/// is only recognized when the very first line is a `---` fence and a closing
/// `---` fence exists on its own line.
fn strip_frontmatter(raw: &str) -> (Vec<String>, &str) {
    let first_end = match raw.find('\n') {
        Some(e) => e,
        None => return (Vec::new(), raw),
    };
    if raw[..first_end].trim_end() != "---" {
        return (Vec::new(), raw);
    }
    let yaml_start = first_end + 1;
    let mut line_start = yaml_start;
    loop {
        let rel = &raw[line_start..];
        let nl = rel.find('\n');
        let line_end = nl.map(|e| line_start + e).unwrap_or(raw.len());
        if raw[line_start..line_end].trim_end() == "---" {
            let yaml = &raw[yaml_start..line_start];
            let body_start = nl.map(|e| line_start + e + 1).unwrap_or(raw.len());
            return (parse_tags_yaml(yaml), &raw[body_start..]);
        }
        match nl {
            Some(e) => line_start = line_start + e + 1,
            None => break,
        }
        if line_start >= raw.len() {
            break;
        }
    }
    (Vec::new(), raw)
}

/// Extracts and normalizes the `tags` field from a YAML frontmatter block.
/// Accepts a sequence or a single whitespace/comma-separated scalar; strips a
/// leading `#`, trims, and de-duplicates while preserving order.
fn parse_tags_yaml(yaml: &str) -> Vec<String> {
    let value: serde_yaml::Value = match serde_yaml::from_str(yaml) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    let raw = match value.get("tags") {
        Some(t) => t,
        None => return Vec::new(),
    };

    let mut parts: Vec<String> = Vec::new();
    match raw {
        serde_yaml::Value::Sequence(seq) => {
            for item in seq {
                if let Some(s) = yaml_scalar_string(item) {
                    parts.push(s);
                }
            }
        }
        other => {
            if let Some(s) = yaml_scalar_string(other) {
                parts.extend(s.split([',', ' ', '\t']).map(|p| p.to_string()));
            }
        }
    }

    let mut out: Vec<String> = Vec::new();
    for part in parts {
        let tag = part.trim().trim_start_matches('#').trim().to_string();
        if tag.is_empty() || out.contains(&tag) {
            continue;
        }
        out.push(tag);
    }
    out
}

fn yaml_scalar_string(v: &serde_yaml::Value) -> Option<String> {
    match v {
        serde_yaml::Value::String(s) => Some(s.clone()),
        serde_yaml::Value::Number(n) => Some(n.to_string()),
        serde_yaml::Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// AST → Typst emitter
// ---------------------------------------------------------------------------

fn emit_blocks<'a>(node: &'a AstNode<'a>, out: &mut String) {
    for child in node.children() {
        emit_block(child, out);
    }
}

fn emit_block<'a>(node: &'a AstNode<'a>, out: &mut String) {
    let value = node.data.borrow().value.clone();
    match value {
        NodeValue::Paragraph => {
            // `#h(0pt)` is a zero-width guard so a line that begins with `=`,
            // `-`, `+` or `1.` can never be misread as Typst block markup.
            out.push_str("#h(0pt)");
            emit_inlines(node, out);
            out.push_str("\n\n");
        }
        NodeValue::Heading(h) => {
            out.push_str(&format!("#heading(level: {})[", h.level.min(6)));
            emit_inlines(node, out);
            out.push_str("]\n\n");
        }
        NodeValue::CodeBlock(cb) => {
            let lang = cb.info.split_whitespace().next().unwrap_or("");
            let lang_arg = if lang.is_empty() {
                "none".to_string()
            } else {
                typst_string(lang)
            };
            let body = cb.literal.trim_end_matches('\n');
            out.push_str(&format!(
                "#raw(block: true, lang: {}, {})\n\n",
                lang_arg,
                typst_string(body)
            ));
        }
        NodeValue::BlockQuote => {
            out.push_str("#jnquote[");
            emit_blocks(node, out);
            out.push_str("]\n\n");
        }
        NodeValue::List(nl) => emit_list(node, &nl, out),
        NodeValue::Table(t) => emit_table(node, &t.alignments, t.num_columns, out),
        NodeValue::ThematicBreak => {
            out.push_str("#line(length: 100%, stroke: 0.5pt + luma(200))\n\n");
        }
        // Raw HTML can't render in a paginated PDF; drop it silently.
        NodeValue::HtmlBlock(_) => {}
        // Fall back to walking children for any container we don't special-case.
        _ => emit_blocks(node, out),
    }
}

fn emit_list<'a>(node: &'a AstNode<'a>, nl: &comrak::nodes::NodeList, out: &mut String) {
    let ordered = matches!(nl.list_type, ListType::Ordered);
    let mut items: Vec<String> = Vec::new();
    for item in node.children() {
        let mut content = String::new();
        if let NodeValue::TaskItem(task) = &item.data.borrow().value {
            content.push_str(if task.symbol.is_some() { "[x] " } else { "[ ] " });
        }
        emit_blocks(item, &mut content);
        items.push(format!("[{}]", content.trim_end()));
    }

    let func = if ordered { "enum" } else { "list" };
    let mut args = String::new();
    if nl.is_task_list && !ordered {
        args.push_str("marker: none, ");
    }
    if ordered && nl.start != 1 {
        args.push_str(&format!("start: {}, ", nl.start));
    }
    args.push_str("tight: true, ");
    out.push_str(&format!("#{}({}{})\n\n", func, args, items.join(", ")));
}

fn emit_table<'a>(
    node: &'a AstNode<'a>,
    alignments: &[TableAlignment],
    num_columns: usize,
    out: &mut String,
) {
    let aligns: Vec<&str> = alignments
        .iter()
        .map(|a| match a {
            TableAlignment::Left => "left",
            TableAlignment::Center => "center",
            TableAlignment::Right => "right",
            TableAlignment::None => "left",
        })
        .collect();

    let mut cells = String::new();
    for row in node.children() {
        let is_header = matches!(row.data.borrow().value, NodeValue::TableRow(true));
        for cell in row.children() {
            let mut inner = String::new();
            emit_inlines(cell, &mut inner);
            if is_header {
                cells.push_str(&format!("[#strong[{inner}]], "));
            } else {
                cells.push_str(&format!("[{inner}], "));
            }
        }
    }

    let align_arg = if aligns.is_empty() {
        String::new()
    } else {
        format!("align: ({},), ", aligns.join(", "))
    };
    out.push_str(&format!(
        "#table(columns: {num_columns}, {align_arg}{cells})\n\n"
    ));
}

fn emit_inlines<'a>(node: &'a AstNode<'a>, out: &mut String) {
    for child in node.children() {
        emit_inline(child, out);
    }
}

fn emit_inline<'a>(node: &'a AstNode<'a>, out: &mut String) {
    let value = node.data.borrow().value.clone();
    match value {
        NodeValue::Text(text) => emit_text(&text, out),
        NodeValue::SoftBreak => out.push(' '),
        NodeValue::LineBreak => out.push_str("#linebreak() "),
        NodeValue::Emph => {
            out.push_str("#emph[");
            emit_inlines(node, out);
            out.push(']');
        }
        NodeValue::Strong => {
            out.push_str("#strong[");
            emit_inlines(node, out);
            out.push(']');
        }
        NodeValue::Strikethrough => {
            out.push_str("#strike[");
            emit_inlines(node, out);
            out.push(']');
        }
        NodeValue::Code(code) => {
            out.push_str(&format!("#raw({})", typst_string(&code.literal)));
        }
        NodeValue::Link(link) => {
            out.push_str(&format!("#link({})[", typst_string(&link.url)));
            let before = out.len();
            emit_inlines(node, out);
            if out.len() == before {
                escape_markup(&link.url, out);
            }
            out.push(']');
        }
        NodeValue::Image(link) => emit_image(node, &link.url, out),
        // Inline raw HTML is dropped, like block HTML.
        NodeValue::HtmlInline(_) => {}
        _ => emit_inlines(node, out),
    }
}

/// Emits an image node. Vault-relative paths become `#image(...)` (the compiler
/// reads and embeds the bytes); remote / `data:` / absolute sources can't be
/// embedded offline, so their alt text is shown in italics instead.
fn emit_image<'a>(node: &'a AstNode<'a>, url: &str, out: &mut String) {
    if is_embeddable(url) {
        let rel = clean_rel(url);
        out.push_str(&format!("#image({}, width: 100%)", typst_string(&rel)));
    } else {
        out.push_str("#emph[");
        let before = out.len();
        emit_inlines(node, out);
        if out.len() == before {
            escape_markup(url, out);
        }
        out.push(']');
    }
}

/// Emits body text, rendering `[[wikilinks]]` as plain accent-colored text
/// (never a broken link) and escaping everything else for Typst markup.
fn emit_text(text: &str, out: &mut String) {
    let mut rest = text;
    while let Some(start) = rest.find("[[") {
        let (before, from_open) = rest.split_at(start);
        escape_markup(before, out);
        let after_open = &from_open[2..];
        if let Some(end) = after_open.find("]]") {
            let inner = &after_open[..end];
            if !inner.contains('\n') && !inner.contains('[') && !inner.contains(']') {
                // `[[target|display]]` shows the display half, like Obsidian.
                let display = match inner.find('|') {
                    Some(i) => &inner[i + 1..],
                    None => inner,
                };
                out.push_str("#text(fill: rgb(\"#2563eb\"))[");
                escape_markup(display.trim(), out);
                out.push(']');
                rest = &after_open[end + 2..];
                continue;
            }
        }
        // Not a well-formed wikilink: emit the `[[` literally and move on.
        escape_markup("[[", out);
        rest = after_open;
    }
    escape_markup(rest, out);
}

// ---------------------------------------------------------------------------
// Escaping helpers
// ---------------------------------------------------------------------------

/// Escapes the characters that are significant in Typst markup so arbitrary
/// note text renders verbatim.
fn escape_markup(s: &str, out: &mut String) {
    for ch in s.chars() {
        match ch {
            '\\' | '#' | '$' | '*' | '_' | '`' | '@' | '<' | '>' | '~' | '[' | ']' => {
                out.push('\\');
                out.push(ch);
            }
            _ => out.push(ch),
        }
    }
}

/// Renders `s` as a Typst string literal (including the surrounding quotes).
fn typst_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => {}
            '\t' => out.push_str("\\t"),
            _ => out.push(ch),
        }
    }
    out.push('"');
    out
}

/// True when a URL points at a local, vault-relative file we can embed.
fn is_embeddable(url: &str) -> bool {
    let u = url.trim();
    !(u.is_empty()
        || u.starts_with('/')
        || u.starts_with("http://")
        || u.starts_with("https://")
        || u.starts_with("data:")
        || u.contains("://"))
}

/// Cleans a relative image URL: strips a leading `./` and decodes `%NN`
/// percent-escapes (e.g. `%20` → space) so it names the real on-disk file.
fn clean_rel(url: &str) -> String {
    let trimmed = url.trim().trim_start_matches("./");
    percent_decode(trimmed)
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = (bytes[i + 1] as char).to_digit(16);
            let lo = (bytes[i + 2] as char).to_digit(16);
            if let (Some(h), Some(l)) = (hi, lo) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

// ---------------------------------------------------------------------------
// Typst World
// ---------------------------------------------------------------------------

/// The bundled fonts, parsed once. `typst-assets` ships an offline set (a
/// Libertinus serif for body text, DejaVu Sans Mono for code, and more) so
/// output is identical on any machine.
fn bundled_fonts() -> &'static [Font] {
    static FONTS: OnceLock<Vec<Font>> = OnceLock::new();
    FONTS.get_or_init(|| {
        let mut fonts = Vec::new();
        for data in typst_assets::fonts() {
            let bytes = Bytes::new(data);
            fonts.extend(Font::iter(bytes));
        }
        fonts
    })
}

/// A minimal [`World`] backing a single in-memory note document. `file` reads
/// image bytes from the vault root; there are no other on-disk sources.
struct PdfWorld {
    library: LazyHash<Library>,
    book: LazyHash<FontBook>,
    fonts: &'static [Font],
    root: PathBuf,
    main: Source,
}

impl PdfWorld {
    fn new(root: PathBuf, markup: String) -> Self {
        let fonts = bundled_fonts();
        let vpath = VirtualPath::new("/main.typ").expect("valid virtual path");
        let id = FileId::new(RootedPath::new(VirtualRoot::Project, vpath));
        let main = Source::new(id, markup);
        Self {
            library: LazyHash::new(Library::default()),
            book: LazyHash::new(FontBook::from_fonts(fonts.iter())),
            fonts,
            root,
            main,
        }
    }
}

impl World for PdfWorld {
    fn library(&self) -> &LazyHash<Library> {
        &self.library
    }

    fn book(&self) -> &LazyHash<FontBook> {
        &self.book
    }

    fn main(&self) -> FileId {
        self.main.id()
    }

    fn source(&self, id: FileId) -> FileResult<Source> {
        if id == self.main.id() {
            Ok(self.main.clone())
        } else {
            Err(FileError::NotFound(PathBuf::from(
                id.vpath().get_without_slash(),
            )))
        }
    }

    fn file(&self, id: FileId) -> FileResult<Bytes> {
        let path = id
            .vpath()
            .realize(&self.root)
            .map_err(|_| FileError::NotFound(PathBuf::from(id.vpath().get_without_slash())))?;
        fs::read(&path)
            .map(Bytes::new)
            .map_err(|_| FileError::NotFound(path))
    }

    fn font(&self, index: usize) -> Option<Font> {
        self.fonts.get(index).cloned()
    }

    fn today(&self, _offset: Option<Duration>) -> Option<Datetime> {
        None
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    use std::time::{SystemTime, UNIX_EPOCH};

    const FIXTURE: &str = r#"---
tags: [alpha, beta]
title: ignored
---
# Section One

Some **bold** and _italic_ text with a [[Linked Note]] and a link to [site](https://example.com).

```sh
echo "hello world"
ls -la
```

- one
- two

- [x] done
- [ ] todo

| Name | Value |
| ---- | ----: |
| a    | 1     |

![diagram](attachments/pic.png)
"#;

    fn temp_dir(tag: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("jaynotes-pdf-{tag}-{nanos}"));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A tiny but fully valid PNG so the image embed path exercises real bytes.
    fn write_fixture_png(dir: &Path) {
        const PNG_B64: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAIAAACQd1PeAAAADElEQVR4nGP4z8AAAAMBAQDJ/pLvAAAAAElFTkSuQmCC";
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(PNG_B64)
            .unwrap();
        fs::create_dir_all(dir.join("attachments")).unwrap();
        fs::write(dir.join("attachments/pic.png"), bytes).unwrap();
    }

    #[test]
    fn render_strips_frontmatter_and_keeps_content() {
        let markup = render_note_typst("My Note", FIXTURE);

        // Title present as a level-1 heading.
        assert!(markup.contains("#heading(level: 1)[My Note]"));
        // Frontmatter is gone: no raw fences, no `title: ignored` leak.
        assert!(!markup.contains("---"));
        assert!(!markup.contains("title: ignored"));
        // Tags surface as a subtle line.
        assert!(markup.contains("\\#alpha"));
        assert!(markup.contains("\\#beta"));
        // Code block highlighted via a Typst raw block carrying the language.
        assert!(markup.contains("#raw(block: true, lang: \"sh\""));
        assert!(markup.contains("echo"));
        // Wikilink rendered as styled text, not a broken link.
        assert!(markup.contains("#text(fill: rgb(\"#2563eb\"))[Linked Note]"));
        // Local image embedded by relative path.
        assert!(markup.contains("#image(\"attachments/pic.png\""));
        // Table and task list mapped to Typst functions.
        assert!(markup.contains("#table(columns: 2"));
        assert!(markup.contains("[x] "));
    }

    #[test]
    fn strip_frontmatter_handles_missing_block() {
        let (tags, body) = strip_frontmatter("# Just a heading\n\nbody");
        assert!(tags.is_empty());
        assert_eq!(body, "# Just a heading\n\nbody");
    }

    #[test]
    fn escaping_neutralizes_markup() {
        let mut out = String::new();
        escape_markup("a*b_c#d[e]", &mut out);
        assert_eq!(out, "a\\*b\\_c\\#d\\[e\\]");
        assert_eq!(typst_string("say \"hi\"\n"), "\"say \\\"hi\\\"\\n\"");
    }

    #[test]
    fn compiles_fixture_to_real_pdf() {
        let dir = temp_dir("compile");
        write_fixture_png(&dir);

        let markup = render_note_typst("Fixture Note", FIXTURE);
        let (pdf, pages) = markup_to_pdf(&dir, &markup).expect("compile should succeed");

        // Real PDF magic and at least one page.
        assert!(pdf.starts_with(b"%PDF"), "output is not a PDF");
        assert!(pages >= 1, "expected at least one page, got {pages}");

        // Extract text back and confirm the title and a code line survived.
        if let Ok(text) = pdf_extract::extract_text_from_mem(&pdf) {
            assert!(
                text.contains("Fixture Note"),
                "title missing from extracted text: {text:?}"
            );
            assert!(
                text.contains("echo"),
                "code line missing from extracted text"
            );
        }

        fs::remove_dir_all(&dir).ok();
    }

    /// Writes a rich sample PDF to a caller-provided path for manual visual
    /// inspection. Run with:
    /// `JAYNOTES_PDF_OUT=/tmp/sample.pdf cargo test -- --ignored sample_pdf`
    #[test]
    #[ignore]
    fn sample_pdf() {
        let out = std::env::var("JAYNOTES_PDF_OUT").expect("set JAYNOTES_PDF_OUT");
        let dir = temp_dir("sample");
        write_fixture_png(&dir);
        const RICH: &str = r#"---
tags: [demo, "pdf-export"]
---
## Quarterly notes

A paragraph with **bold**, _italic_, ~~struck~~ and `inline code`, plus a
[[Project Plan|the plan]] wikilink and a [real link](https://typst.app).

> A blockquote that should sit in an indented, ruled block and can wrap onto
> more than one line without any trouble at all.

### Code

```rust
fn main() {
    println!("Hello, JayNotes!");
}
```

### Lists

1. First ordered
2. Second ordered
   - nested bullet
- [x] shipped
- [ ] todo item

### Table

| Feature | Status |
| ------- | -----: |
| Export  |    yes |
| Print   |     no |

---

![sample](attachments/pic.png)
"#;
        let markup = render_note_typst("Sample Export", RICH);
        let (pdf, pages) = markup_to_pdf(&dir, &markup).expect("compile");
        assert!(pdf.starts_with(b"%PDF"));
        fs::write(&out, &pdf).unwrap();
        eprintln!("wrote {pages}-page PDF to {out}");
        fs::remove_dir_all(&dir).ok();
    }
}
