// Small pieces of cross-component UI state driven by keyboard shortcuts.

import { writable } from "svelte/store";

/** Which view the sidebar shows below its tab strip. */
export type SidebarMode = "files" | "search";

export const sidebarMode = writable<SidebarMode>("files");

/** Whether the Cmd+P quick switcher modal is open. */
export const quickSwitcherOpen = writable(false);

/**
 * Incremented whenever the search panel should grab focus (e.g. Cmd+Shift+F
 * pressed while it's already visible). The panel watches this and focuses its
 * input; the value itself is meaningless.
 */
export const searchFocusNonce = writable(0);
