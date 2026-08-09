/**
 * Helpers for embedding a pasted/dropped text file inline in a note.
 *
 * Non-image files dropped into the editor are read as UTF-8 and inserted as
 * note content rather than saved to `attachments/` — so the data stays in the
 * markdown: searchable by the FTS index, greppable, portable to any other
 * markdown tool, and encrypted along with the note in encrypted vaults.
 *
 * Every file lands in a fenced code block, so its bytes round-trip verbatim —
 * a recognized type gets a language tag for highlighting, anything else gets a
 * bare fence. Never a paragraph: markdown syntax inside the file (`*`, `_`,
 * `#`) would be re-interpreted on the next load.
 */

/** Largest file accepted for inline embedding (~1MB of text). */
export const MAX_TEXT_FILE_BYTES = 1024 * 1024;

/** Extension → fence language. Anything absent falls back to an empty fence. */
const LANGUAGE_BY_EXT: Record<string, string> = {
  json: "json",
  jsonc: "json",
  json5: "json",
  yaml: "yaml",
  yml: "yaml",
  toml: "toml",
  // `.env`, `.secret`, `.token` and friends are all `KEY=value` shaped.
  env: "ini",
  ini: "ini",
  cfg: "ini",
  conf: "ini",
  properties: "ini",
  xml: "xml",
  plist: "xml",
  html: "html",
  htm: "html",
  css: "css",
  scss: "scss",
  less: "less",
  js: "javascript",
  mjs: "javascript",
  cjs: "javascript",
  jsx: "jsx",
  ts: "typescript",
  mts: "typescript",
  cts: "typescript",
  tsx: "tsx",
  py: "python",
  rs: "rust",
  go: "go",
  rb: "ruby",
  php: "php",
  java: "java",
  c: "c",
  h: "c",
  cpp: "cpp",
  cc: "cpp",
  cxx: "cpp",
  hpp: "cpp",
  cs: "csharp",
  swift: "swift",
  kt: "kotlin",
  kts: "kotlin",
  sh: "bash",
  bash: "bash",
  zsh: "bash",
  fish: "fish",
  ps1: "powershell",
  sql: "sql",
  csv: "csv",
  tsv: "csv",
  diff: "diff",
  patch: "diff",
  lua: "lua",
  graphql: "graphql",
  gql: "graphql",
  proto: "protobuf",
  tf: "hcl",
  hcl: "hcl",
  svelte: "svelte",
  vue: "vue",
  md: "markdown",
  markdown: "markdown",
};

/** Extensionless filenames that carry a known language. */
const LANGUAGE_BY_NAME: Record<string, string> = {
  dockerfile: "dockerfile",
  makefile: "makefile",
  gemfile: "ruby",
  rakefile: "ruby",
  procfile: "bash",
};

/**
 * The fence language for `fileName`, or `""` for an unrecognized type — which
 * still gets a code block, just an unlabeled one. Directory components are
 * ignored and matching is case-insensitive; a leading dot is stripped first so
 * `.env` reads as extension `env`, not as a bare dotfile.
 */
export function languageForFilename(fileName: string): string {
  const last = fileName.split(/[/\\]/).pop() ?? fileName;
  const name = last.trim().replace(/^\.+/, "").toLowerCase();
  if (name === "") return "";

  const dot = name.lastIndexOf(".");
  if (dot === -1) {
    // A stripped dotfile (`.env`) reads as a bare extension here, so fall
    // through to the extension table before giving up on a language.
    return LANGUAGE_BY_NAME[name] ?? LANGUAGE_BY_EXT[name] ?? "";
  }
  return LANGUAGE_BY_EXT[name.slice(dot + 1)] ?? "";
}

/**
 * Decodes `bytes` as strict UTF-8, or returns `null` when the file is binary.
 * A NUL byte is treated as binary up front (UTF-8 permits it, but no text file
 * a user means to embed contains one), and invalid sequences fail the decode.
 */
export function decodeTextFile(bytes: Uint8Array): string | null {
  const probe = bytes.subarray(0, 8000);
  for (const b of probe) {
    if (b === 0) return null;
  }
  try {
    return new TextDecoder("utf-8", { fatal: true }).decode(bytes);
  } catch {
    return null;
  }
}

/** Strips a BOM, normalizes CRLF/CR to LF, and drops trailing blank lines. */
export function normalizeText(text: string): string {
  return text
    .replace(/^\uFEFF/, "")
    .replace(/\r\n?/g, "\n")
    .replace(/\n+$/, "");
}
