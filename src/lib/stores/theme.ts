import { writable } from "svelte/store";
import { browser } from "$app/environment";

export type ThemeMode = "light" | "dark" | "system";
export type ResolvedTheme = "light" | "dark";

const STORAGE_KEY = "jaynotes:theme";

function readStored(): ThemeMode {
  if (!browser) return "system";
  const value = localStorage.getItem(STORAGE_KEY);
  if (value === "light" || value === "dark" || value === "system") {
    return value;
  }
  return "system";
}

function systemPrefersDark(): boolean {
  if (!browser) return false;
  return window.matchMedia("(prefers-color-scheme: dark)").matches;
}

export function resolveTheme(mode: ThemeMode): ResolvedTheme {
  if (mode === "system") {
    return systemPrefersDark() ? "dark" : "light";
  }
  return mode;
}

function applyTheme(mode: ThemeMode): void {
  if (!browser) return;
  document.documentElement.setAttribute("data-theme", resolveTheme(mode));
}

/** The user's chosen mode: 'light' | 'dark' | 'system'. Defaults to system. */
export const themeMode = writable<ThemeMode>(readStored());

// Persist + apply on every change.
themeMode.subscribe((mode) => {
  if (!browser) return;
  localStorage.setItem(STORAGE_KEY, mode);
  applyTheme(mode);
});

// Keep 'system' mode in sync with OS-level changes.
if (browser) {
  const mql = window.matchMedia("(prefers-color-scheme: dark)");
  mql.addEventListener("change", () => {
    let current: ThemeMode = "system";
    const unsub = themeMode.subscribe((m) => (current = m));
    unsub();
    if (current === "system") applyTheme("system");
  });
}

/** Cycle light -> dark -> system -> light. */
export function cycleTheme(): void {
  themeMode.update((mode) =>
    mode === "light" ? "dark" : mode === "dark" ? "system" : "light",
  );
}
