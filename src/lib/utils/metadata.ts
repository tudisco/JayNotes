// Frontmatter metadata layer.
//
// `frontmatter.ts` splits a note into a verbatim frontmatter block and body,
// deliberately WITHOUT parsing the YAML. This module adds the parse/serialize
// layer used by the properties bar (M3): it reads the YAML inside the fences
// into structured `tags` + `fields`, and serializes them back into a full
// fenced block.
//
// Data-safety contract: parsing NEVER mutates a note. Only an explicit user
// edit re-serializes the block (via `serializeFrontmatter`), and malformed
// YAML is reported (`parseError`) so callers can refuse to rewrite it.

import { Document, parse } from "yaml";

export interface ParsedFrontmatter {
  /** Normalized tag list (trimmed, deduped, order-preserving). */
  tags: string[];
  /** All non-`tags` top-level keys, insertion order preserved. */
  fields: Record<string, unknown>;
  /** True when the YAML could not be parsed — treat the block as opaque. */
  parseError: boolean;
}

/** Strip the leading/trailing `---` fences from a verbatim frontmatter block. */
function stripFences(fm: string): string {
  // The block always looks like `---\n<inner>---\n` (or `---\n---` at EOF).
  // Remove the first line (opening fence) and the last fence line.
  const withoutOpen = fm.replace(/^---[ \t]*\r?\n/, "");
  return withoutOpen.replace(/---[ \t]*\r?\n?$/, "");
}

/** Normalize a raw `tags` value (array | string | anything) into a tag list. */
function normalizeTags(raw: unknown): string[] {
  let parts: string[];
  if (Array.isArray(raw)) {
    parts = raw.map((t) => String(t));
  } else if (typeof raw === "string") {
    // A single scalar may hold several whitespace/comma separated tags.
    parts = raw.split(/[,\s]+/);
  } else if (raw === null || raw === undefined) {
    return [];
  } else {
    parts = [String(raw)];
  }

  const out: string[] = [];
  const seen = new Set<string>();
  for (const part of parts) {
    const tag = part.trim().replace(/^#/, "");
    if (!tag || seen.has(tag)) continue;
    seen.add(tag);
    out.push(tag);
  }
  return out;
}

/**
 * Parse the YAML inside a frontmatter block into `tags` + `fields`.
 * `null` input (no frontmatter) yields empty results with no error.
 */
export function parseFrontmatter(fm: string | null): ParsedFrontmatter {
  if (fm === null) {
    return { tags: [], fields: {}, parseError: false };
  }

  let data: unknown;
  try {
    data = parse(stripFences(fm));
  } catch {
    return { tags: [], fields: {}, parseError: true };
  }

  // Empty block (`---\n---\n`) parses to null/undefined.
  if (data === null || data === undefined) {
    return { tags: [], fields: {}, parseError: false };
  }
  // A non-map document (e.g. a bare list or scalar) isn't valid properties.
  if (typeof data !== "object" || Array.isArray(data)) {
    return { tags: [], fields: {}, parseError: true };
  }

  const obj = data as Record<string, unknown>;
  const fields: Record<string, unknown> = {};
  let tags: string[] = [];
  for (const [key, value] of Object.entries(obj)) {
    if (key === "tags") {
      tags = normalizeTags(value);
    } else {
      fields[key] = value;
    }
  }
  return { tags, fields, parseError: false };
}

/**
 * Serialize `tags` + `fields` back into a full fenced frontmatter block
 * (`---\n…\n---\n`). Tags come first as a flow sequence, then the fields in
 * insertion order. Returns `null` when there is nothing to write, so an
 * emptied properties block disappears from the file entirely.
 */
export function serializeFrontmatter(
  tags: string[],
  fields: Record<string, unknown>,
): string | null {
  const hasTags = tags.length > 0;
  const fieldKeys = Object.keys(fields);
  if (!hasTags && fieldKeys.length === 0) {
    return null;
  }

  const doc = new Document();
  doc.contents = doc.createNode({});
  if (hasTags) {
    const seq = doc.createNode(tags);
    // Render tags inline: `tags: [a, b]`.
    (seq as { flow: boolean }).flow = true;
    doc.set("tags", seq);
  }
  for (const key of fieldKeys) {
    doc.set(key, doc.createNode(fields[key]));
  }

  const inner = doc.toString({ flowCollectionPadding: false });
  return `---\n${inner}---\n`;
}

/**
 * Extract inline `#tags` from a note body. A tag starts with `#`, then a
 * letter, then letters/digits/`-`/`_`/`/`. Matches inside fenced code blocks
 * (``` or ~~~) and inline code spans (`` `…` ``) are ignored, as is `foo#bar`
 * (the `#` must not follow a word character) and `#123` (must start a letter).
 * Order-preserving and deduped.
 */
export function extractInlineTags(body: string): string[] {
  const lines = body.split(/\r?\n/);
  const scanned: string[] = [];
  let fence: string | null = null; // active code-fence marker, or null

  for (const line of lines) {
    const fenceMatch = /^\s{0,3}(`{3,}|~{3,})/.exec(line);
    if (fence) {
      // Inside a fence: a matching-or-longer fence of the same char closes it.
      if (fenceMatch && fenceMatch[1][0] === fence[0] &&
          fenceMatch[1].length >= fence.length) {
        fence = null;
      }
      continue; // fenced content never contributes tags
    }
    if (fenceMatch) {
      fence = fenceMatch[1];
      continue;
    }
    // Strip inline code spans before tag scanning.
    scanned.push(line.replace(/`[^`\n]*`/g, ""));
  }

  const text = scanned.join("\n");
  const re = /(?<![\w#/])#([A-Za-z][\w/-]*)/g;
  const out: string[] = [];
  const seen = new Set<string>();
  let m: RegExpExecArray | null;
  while ((m = re.exec(text)) !== null) {
    const tag = m[1];
    if (seen.has(tag)) continue;
    seen.add(tag);
    out.push(tag);
  }
  return out;
}
