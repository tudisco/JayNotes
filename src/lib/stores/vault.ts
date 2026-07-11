import { invoke } from "@tauri-apps/api/core";
import { derived, get, writable } from "svelte/store";

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
// Multi-vault types (M13)
// ---------------------------------------------------------------------------

/** Vault kind, mirrored from the Rust `VaultKind` (`kind()` ids). */
export type VaultKind = "plain" | "encrypted-db";

/** On-disk status of a vault, mirrored from the Rust `VaultStatus`. */
export type VaultStatus = "ok" | "offline" | "missing" | "unsupported";

export interface VaultInfo {
  id: string;
  name: string;
  path: string;
  kind: VaultKind;
  status: VaultStatus;
}

/** Payload of the `list_vaults` command. */
export interface VaultList {
  vaults: VaultInfo[];
  activeId: string | null;
  removed: string[];
}

// ---------------------------------------------------------------------------
// Provider metadata (M14) — drives the vault-type picker + config forms
// ---------------------------------------------------------------------------

export interface ConfigField {
  key: string;
  label: string;
  fieldType: "folder" | "text" | "password" | "url";
  required: boolean;
  placeholder?: string;
}

export interface Capabilities {
  revealInFinder: boolean;
  needsUnlock: boolean;
  folderBacked: boolean;
}

export interface ProviderMeta {
  kind: VaultKind;
  displayName: string;
  description: string;
  configFields: ConfigField[];
  capabilities: Capabilities;
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
// Multi-vault state (M13)
// ---------------------------------------------------------------------------

/** All configured vaults with live status. */
export const vaults = writable<VaultInfo[]>([]);

/** Id of the active vault, or null when none is configured. */
export const activeVaultId = writable<string | null>(null);

/**
 * Names of vaults auto-removed (status "missing") on the last `list_vaults`.
 * Drives the one-time dismissable notice; cleared by `dismissRemovedNotice`.
 */
export const removedVaultNames = writable<string[]>([]);

/**
 * True when the active vault's folder is currently unreachable because its
 * drive is unplugged (status "offline"). The UI shows a retry state rather
 * than the tree, and never auto-removes the vault.
 */
export const activeVaultOffline = writable(false);

/** The active vault object, derived from `vaults` + `activeVaultId`. */
export const activeVault = derived(
  [vaults, activeVaultId],
  ([$vaults, $activeVaultId]) =>
    $vaults.find((v) => v.id === $activeVaultId) ?? null,
);

/** Compiled vault providers (from `list_providers`), for the type picker. */
export const providers = writable<ProviderMeta[]>([]);

/**
 * True when the active vault is an encrypted vault that is present but not yet
 * unlocked. The main pane shows the unlock prompt instead of the tree/editor.
 */
export const vaultLocked = writable(false);

/** Capabilities of the currently open vault handle (drives capability UI). */
export const activeCapabilities = writable<Capabilities | null>(null);

function currentActiveVault(): VaultInfo | null {
  const id = get(activeVaultId);
  return get(vaults).find((v) => v.id === id) ?? null;
}

// ---------------------------------------------------------------------------
// Vault lifecycle
// ---------------------------------------------------------------------------

/** Called once on app start: loads providers + the vault list, opens the active one. */
export async function initVault(): Promise<void> {
  try {
    await loadProviders();
    await loadVaults();
    await activateActive();
  } catch (e) {
    vaultError.set(String(e));
  } finally {
    vaultLoading.set(false);
  }
}

/** Loads the compiled provider metadata for the vault-type picker. */
export async function loadProviders(): Promise<void> {
  try {
    providers.set(await invoke<ProviderMeta[]>("list_providers"));
  } catch {
    providers.set([]);
  }
}

/**
 * Brings the active vault's UI into the right state after any list/switch:
 * scans a ready plain/unlocked vault, shows the offline retry state, or shows
 * the unlock prompt for a locked encrypted vault (trying a remembered key
 * silently first). Central so switch/init/remove all behave identically.
 */
async function activateActive(): Promise<void> {
  const active = currentActiveVault();
  selected.set(null);
  expandedDirs.set(new Set());
  activeCapabilities.set(null);

  if (!active || active.status === "missing") {
    vaultPath.set(null);
    fileTree.set(null);
    vaultLocked.set(false);
    activeVaultOffline.set(false);
    return;
  }
  if (active.status === "offline" || active.status === "unsupported") {
    vaultPath.set(null);
    fileTree.set(null);
    vaultLocked.set(false);
    activeVaultOffline.set(active.status === "offline");
    return;
  }

  activeVaultOffline.set(false);

  // Encrypted vaults may need unlocking; try a remembered key silently.
  if (active.kind !== "plain") {
    let locked = await invoke<boolean>("vault_needs_unlock", { id: active.id });
    if (locked) {
      const opened = await invoke<boolean>("unlock_remembered", {
        id: active.id,
      });
      locked = !opened;
    }
    if (locked) {
      vaultPath.set(active.path);
      fileTree.set(null);
      vaultLocked.set(true);
      return;
    }
  }

  vaultLocked.set(false);
  vaultPath.set(active.path);
  await loadCapabilities();
  await refreshTree();
}

/** Refreshes the active vault handle's capability flags. */
async function loadCapabilities(): Promise<void> {
  try {
    activeCapabilities.set(
      await invoke<Capabilities | null>("active_capabilities"),
    );
  } catch {
    activeCapabilities.set(null);
  }
}

/**
 * Fetches the vault list (which also prunes any "missing" vaults on the Rust
 * side and reports their names). Updates the vault/active stores and records
 * removals for the one-time notice.
 */
export async function loadVaults(): Promise<VaultList> {
  const list = await invoke<VaultList>("list_vaults");
  vaults.set(list.vaults);
  activeVaultId.set(list.activeId);
  if (list.removed.length > 0) {
    removedVaultNames.update((prev) => [...prev, ...list.removed]);
  }
  return list;
}

/** Dismisses the "removed missing vault" notice. */
export function dismissRemovedNotice(): void {
  removedVaultNames.set([]);
}

/**
 * Switches the active vault: clears selection/expanded/tree, re-inits the
 * backend index+watcher, then rescans. Throws (leaving state intact) if the
 * target vault is unreachable.
 */
export async function switchVault(id: string): Promise<void> {
  fileTree.set(null);
  await invoke<string>("switch_vault", { id });
  await loadVaults();
  await activateActive();
}

/**
 * Creates a new encrypted-db vault (a `<name>.jaynotes` container at `location`,
 * password-locked) and opens it. Returns the new vault id.
 */
export async function createEncryptedVault(
  location: string,
  name: string,
  password: string,
  remember: boolean,
): Promise<string> {
  const vault = await invoke<VaultInfo>("create_encrypted_vault", {
    location,
    name,
    password,
    remember,
  });
  await loadVaults();
  await activateActive();
  return vault.id;
}

/** Unlocks the given encrypted vault with a password, then opens it. */
export async function unlockVault(
  id: string,
  password: string,
  remember: boolean,
): Promise<void> {
  await invoke("unlock_vault", { id, password, remember });
  await activateActive();
}

/** Locks the active encrypted vault (clears the in-memory key). */
export async function lockVault(id: string): Promise<void> {
  await invoke("lock_vault", { id });
  vaultLocked.set(true);
  fileTree.set(null);
  selected.set(null);
  activeCapabilities.set(null);
}

/**
 * Opens the folder picker, adds the chosen folder as a new vault, and switches
 * to it. Returns the new vault id, or null if the picker was cancelled.
 */
export async function addVault(): Promise<string | null> {
  const picked = await invoke<string | null>("pick_vault");
  if (!picked) return null;
  const vault = await invoke<VaultInfo>("add_vault", { path: picked });
  await switchVault(vault.id);
  return vault.id;
}

/**
 * Creates a new vault folder named `name` under `parentPath` and switches to
 * it. Returns the new vault id.
 */
export async function createVault(
  parentPath: string,
  name: string,
): Promise<string> {
  const vault = await invoke<VaultInfo>("create_vault", { parentPath, name });
  await switchVault(vault.id);
  return vault.id;
}

/**
 * Forgets a vault (never touches the folder on disk). If it was the active
 * vault, the backend switches to the first remaining one; this reflects that in
 * the UI, resetting selection/tree as needed.
 */
export async function removeVault(id: string): Promise<void> {
  const wasActive = get(activeVaultId) === id;
  await invoke<string | null>("remove_vault", { id });
  await loadVaults();
  if (wasActive) {
    await activateActive();
  }
}

/** Renames a vault's display name only (the folder is untouched). */
export async function renameVault(id: string, name: string): Promise<void> {
  await invoke("rename_vault", { id, name });
  await loadVaults();
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
