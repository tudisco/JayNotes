import { invoke } from "@tauri-apps/api/core";
import { get, writable } from "svelte/store";

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

export interface TreeNode {
  name: string;
  /** Path relative to the vault root, forward-slash separated. */
  path: string;
  isDir: boolean;
  children: TreeNode[];
}

export interface Selection {
  path: string;
  isDir: boolean;
}

export interface ContextMenuState {
  x: number;
  y: number;
  node: TreeNode;
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

/** Absolute path of the open vault, or null if none is configured. */
export const vaultPath = writable<string | null>(null);

/** Root node of the scanned vault tree (its own path is ""). */
export const fileTree = writable<TreeNode | null>(null);

/** Currently selected tree item (file or folder). */
export const selected = writable<Selection | null>(null);

/** Relative paths of expanded folders. In-memory only, by design (M1). */
export const expandedDirs = writable<Set<string>>(new Set());

/** Relative path of the item currently being renamed inline, or null. */
export const renamingPath = writable<string | null>(null);

/** Context menu state, or null when closed. */
export const contextMenu = writable<ContextMenuState | null>(null);

/** True while the initial get_vault/scan is in flight. */
export const vaultLoading = writable(true);

/** Last vault-level error message (scan/init failures), or null. */
export const vaultError = writable<string | null>(null);

// ---------------------------------------------------------------------------
// Vault lifecycle
// ---------------------------------------------------------------------------

/** Called once on app start: restores the saved vault and scans it. */
export async function initVault(): Promise<void> {
  try {
    const path = await invoke<string | null>("get_vault");
    if (path) {
      vaultPath.set(path);
      await refreshTree();
    }
  } catch (e) {
    vaultError.set(String(e));
  } finally {
    vaultLoading.set(false);
  }
}

/** Opens the native folder picker, persists the choice, and scans. */
export async function openVault(): Promise<void> {
  try {
    const picked = await invoke<string | null>("pick_vault");
    if (!picked) return;
    await invoke("set_vault", { path: picked });
    vaultPath.set(picked);
    selected.set(null);
    expandedDirs.set(new Set());
    await refreshTree();
  } catch (e) {
    vaultError.set(String(e));
  }
}

/** Re-scans the vault and updates the tree. */
export async function refreshTree(): Promise<void> {
  fileTree.set(await invoke<TreeNode>("scan_vault"));
  vaultError.set(null);
}

// ---------------------------------------------------------------------------
// Note IO (EditorPane stub uses these; real editor comes in M2)
// ---------------------------------------------------------------------------

export function readNote(relPath: string): Promise<string> {
  return invoke<string>("read_note", { relPath });
}

export function writeNote(relPath: string, content: string): Promise<void> {
  return invoke("write_note", { relPath, content });
}

// ---------------------------------------------------------------------------
// Tree interaction helpers
// ---------------------------------------------------------------------------

export function toggleDir(path: string): void {
  expandedDirs.update((set) => {
    const next = new Set(set);
    if (next.has(path)) {
      next.delete(path);
    } else {
      next.add(path);
    }
    return next;
  });
}

function expandDir(path: string): void {
  if (!path) return;
  expandedDirs.update((set) => new Set(set).add(path));
}

function parentOf(relPath: string): string {
  const idx = relPath.lastIndexOf("/");
  return idx === -1 ? "" : relPath.slice(0, idx);
}

/** Ensure every ancestor folder of `relPath` is expanded. */
function expandAncestors(relPath: string): void {
  let parent = parentOf(relPath);
  while (parent) {
    expandDir(parent);
    parent = parentOf(parent);
  }
}

function findNode(root: TreeNode | null, path: string): TreeNode | null {
  if (!root) return null;
  if (path === root.path) return root;
  for (const child of root.children) {
    if (path === child.path || path.startsWith(child.path + "/")) {
      return findNode(child, path);
    }
  }
  return null;
}

/** The folder new items should go into: the selected folder, or the root. */
export function newItemTargetDir(): string {
  const sel = get(selected);
  return sel && sel.isDir ? sel.path : "";
}

// ---------------------------------------------------------------------------
// Mutations
// ---------------------------------------------------------------------------

/**
 * Creates an auto-named note inside `targetDir` (relative path, "" = root),
 * selects it, and puts it into inline-rename mode.
 */
export async function newNote(targetDir: string): Promise<void> {
  const created = await invoke<string>("create_note", { relPath: targetDir });
  expandDir(targetDir);
  await refreshTree();
  selected.set({ path: created, isDir: false });
  renamingPath.set(created);
}

/**
 * Creates a note named `name` in the vault root (".md" appended by the backend
 * if missing) and opens it. Used by the quick switcher's "create" affordance.
 * Returns the created relative path.
 */
export async function createNamedNote(name: string): Promise<string> {
  const created = await invoke<string>("create_note", { relPath: name });
  await refreshTree();
  selected.set({ path: created, isDir: false });
  return created;
}

/**
 * Creates an auto-named folder inside `targetDir`, selects it, and puts it
 * into inline-rename mode. Unique naming uses the already-loaded tree.
 */
export async function newFolder(targetDir: string): Promise<void> {
  const parent = findNode(get(fileTree), targetDir);
  const taken = new Set(
    (parent?.children ?? []).map((c) => c.name.toLowerCase()),
  );
  let name = "New Folder";
  for (let n = 1; taken.has(name.toLowerCase()); n++) {
    name = `New Folder ${n}`;
  }
  const relPath = targetDir ? `${targetDir}/${name}` : name;
  await invoke("create_folder", { relPath });
  expandDir(targetDir);
  await refreshTree();
  selected.set({ path: relPath, isDir: true });
  renamingPath.set(relPath);
}

/**
 * Renames `node` to `newName` (within the same parent folder). For files a
 * missing ".md" extension is appended automatically. No-op if unchanged.
 */
export async function commitRename(
  node: TreeNode,
  newName: string,
): Promise<void> {
  renamingPath.set(null);
  let name = newName.trim();
  if (!name) return;
  if (!node.isDir && !name.toLowerCase().endsWith(".md")) {
    name += ".md";
  }
  if (name === node.name) return;
  if (name.includes("/")) {
    throw new Error("Name cannot contain '/'");
  }

  const parent = parentOf(node.path);
  const newRel = parent ? `${parent}/${name}` : name;
  await invoke("rename_path", { oldRel: node.path, newRel });

  remapPaths(node.path, newRel);
  await refreshTree();
}

/**
 * Renames the note at `oldRelPath` by changing only its filename (the parent
 * folder is preserved), appending ".md" if the caller omitted it. Used by the
 * editable title in the editor header. No-op when the name is unchanged.
 */
export async function renameNote(
  oldRelPath: string,
  newTitle: string,
): Promise<void> {
  let name = newTitle.trim();
  if (!name) return;
  if (!name.toLowerCase().endsWith(".md")) name += ".md";
  if (name.includes("/")) {
    throw new Error("Name cannot contain '/'");
  }
  const idx = oldRelPath.lastIndexOf("/");
  const currentName = idx === -1 ? oldRelPath : oldRelPath.slice(idx + 1);
  if (name === currentName) return;

  const parent = idx === -1 ? "" : oldRelPath.slice(0, idx);
  const newRel = parent ? `${parent}/${name}` : name;
  await invoke("rename_path", { oldRel: oldRelPath, newRel });

  remapPaths(oldRelPath, newRel);
  await refreshTree();
}

/** Moves `node` to the OS Trash (backend never hard-deletes). */
export async function deleteToTrash(node: TreeNode): Promise<void> {
  await invoke("trash_path", { relPath: node.path });
  const sel = get(selected);
  if (sel && (sel.path === node.path || sel.path.startsWith(node.path + "/"))) {
    selected.set(null);
  }
  expandedDirs.update(
    (set) =>
      new Set(
        [...set].filter(
          (p) => p !== node.path && !p.startsWith(node.path + "/"),
        ),
      ),
  );
  await refreshTree();
}

export function revealInFinder(node: TreeNode): Promise<void> {
  return invoke("reveal_in_finder", { relPath: node.path });
}

/** After a rename, keep selection and expanded folders pointing at the new paths. */
function remapPaths(oldPath: string, newPath: string): void {
  const remap = (p: string) =>
    p === oldPath || p.startsWith(oldPath + "/")
      ? newPath + p.slice(oldPath.length)
      : p;
  selected.update((sel) => (sel ? { ...sel, path: remap(sel.path) } : sel));
  expandedDirs.update((set) => new Set([...set].map(remap)));
}

// ---------------------------------------------------------------------------
// Context menu
// ---------------------------------------------------------------------------

export function openContextMenu(x: number, y: number, node: TreeNode): void {
  contextMenu.set({ x, y, node });
}

export function closeContextMenu(): void {
  contextMenu.set(null);
}

/** Starts inline rename for a node (also used by the context menu). */
export function startRename(node: TreeNode): void {
  renamingPath.set(node.path);
}

/** Expands ancestors so a newly created/selected item is visible. */
export function ensureVisible(relPath: string): void {
  expandAncestors(relPath);
}
