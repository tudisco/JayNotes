// Pure path helpers for the vault switcher UI. Kept dependency-free and
// separate from the Tauri stores so they can be unit-tested directly.

/** The final path segment (folder or file name), ignoring a trailing slash. */
export function basename(path: string): string {
  const trimmed = path.replace(/[/\\]+$/, "");
  const idx = Math.max(trimmed.lastIndexOf("/"), trimmed.lastIndexOf("\\"));
  return idx === -1 ? trimmed : trimmed.slice(idx + 1);
}

/**
 * Shortens an absolute path for display in the narrow vault switcher.
 *
 * - The user's home prefix (`/Users/<name>` or `/home/<name>`) collapses to `~`.
 * - If still longer than `maxLen`, the middle is elided with `…`, always
 *   keeping the final segment (the vault's own folder name) intact so the row
 *   stays recognizable.
 */
export function shortenPath(path: string, maxLen = 40): string {
  let p = path.replace(/\/+$/, "");
  const home = p.match(/^(\/Users\/[^/]+|\/home\/[^/]+)(\/.*)?$/);
  if (home) {
    p = "~" + (home[2] ?? "");
  }
  if (p.length <= maxLen) return p;

  const segments = p.split("/");
  const last = segments[segments.length - 1] ?? p;
  // Reserve room for the leading anchor ("~" or "") + "/…/" + last segment.
  const head = segments[0] || "/";
  const candidate = `${head}/…/${last}`;
  if (candidate.length <= maxLen) return candidate;
  // Last segment alone is still too long: hard-truncate its front.
  if (last.length > maxLen) {
    return "…" + last.slice(last.length - (maxLen - 1));
  }
  return `…/${last}`;
}

/** Minimal structural shape of a vault tree node (kept store-independent). */
export interface FolderNode {
  name: string;
  path: string;
  isDir: boolean;
  children: FolderNode[];
}

/**
 * Collects every folder path in the tree, depth-first with each level sorted
 * alphabetically (case-insensitive), a parent always listed before its
 * children. Files are ignored; the root itself (path "") is not included — the
 * caller prepends its own "(vault root)" entry. Paths are the relative,
 * slash-separated form (e.g. "Projects/Duke"), suitable both as a label and as
 * a move destination.
 */
export function collectFolderPaths(root: FolderNode | null): string[] {
  const out: string[] = [];
  const walk = (node: FolderNode): void => {
    const dirs = node.children
      .filter((c) => c.isDir)
      .sort((a, b) =>
        a.name.localeCompare(b.name, undefined, { sensitivity: "base" }),
      );
    for (const dir of dirs) {
      out.push(dir.path);
      walk(dir);
    }
  };
  if (root) walk(root);
  return out;
}
