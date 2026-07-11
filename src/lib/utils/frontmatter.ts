// Frontmatter splitting helpers.
//
// A note may begin with a YAML frontmatter block delimited by `---` fences,
// e.g.
//
//     ---
//     title: My note
//     tags: [a, b]
//     ---
//     # Body starts here
//
// In Milestone 2 we do NOT parse the YAML (that arrives in M3); we only need to
// separate the raw block from the body so the editor never touches it, and to
// reassemble the file byte-for-byte on save.
//
// Invariant: `joinFrontmatter(splitFrontmatter(raw)) === raw` for every input.

export interface SplitFrontmatter {
  /** The raw frontmatter block, verbatim, including both `---` fences and the
   *  trailing newline after the closing fence. `null` when the note has none. */
  frontmatter: string | null;
  /** Everything after the frontmatter block (or the whole input when none). */
  body: string;
}

// Matches a leading frontmatter block at position 0:
//   - opening fence line:  `---` (optional trailing spaces/tabs) + newline
//   - optional content lines, each terminated by a newline
//   - closing fence line:  `---` (optional trailing spaces/tabs) + newline OR EOF
//
// The closing fence must sit on its own line — the mandatory newline before it
// (via the content group, or absent for an empty block) prevents an inline
// `foo---` from being mistaken for a fence. If no closing fence is found the
// regex fails and the input is treated as having no frontmatter (an unterminated
// opening `---` is therefore left in the body untouched).
const FRONTMATTER_RE = /^---[ \t]*\r?\n(?:[\s\S]*?\r?\n)?---[ \t]*(?:\r?\n|$)/;

export function splitFrontmatter(raw: string): SplitFrontmatter {
  const match = FRONTMATTER_RE.exec(raw);
  if (!match) {
    return { frontmatter: null, body: raw };
  }
  const block = match[0];
  return { frontmatter: block, body: raw.slice(block.length) };
}

/** Reassembles a note from its (verbatim) frontmatter block and body. */
export function joinFrontmatter(
  frontmatter: string | null,
  body: string,
): string {
  return frontmatter === null ? body : frontmatter + body;
}
