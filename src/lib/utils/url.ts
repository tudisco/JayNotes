/**
 * URL helpers for image/link handling in the editor.
 */

/**
 * True when `url` is a vault-relative reference that should be resolved against
 * the vault root before display — i.e. it has no URI scheme (`http:`, `https:`,
 * `data:`, `blob:`, `file:`, `asset:`, …), is not root-absolute (`/foo`), and is
 * not protocol-relative (`//host`).
 *
 * Remote and inline URLs return `false` so they pass through to the DOM
 * untouched; only relative paths like `attachments/pic.png` return `true`.
 */
export function isRelativeUrl(url: string): boolean {
  const trimmed = url.trim();
  if (trimmed === "") return false;
  // Root-absolute (`/foo`) or protocol-relative (`//host`): leave as-is.
  if (trimmed.startsWith("/")) return false;
  // Any explicit scheme (`http:`, `data:`, `blob:`, `file:`, `asset:`, …).
  if (/^[a-zA-Z][a-zA-Z0-9+.-]*:/.test(trimmed)) return false;
  return true;
}
