// The person's appearance choice: light, dark or the system's. Light by default.
// The engine's parts show it (on `EngineProvider`'s root); the page around them is the application's.
import { useSyncExternalStore } from "react";

export type Theme = "light" | "dark" | "system";

const key = "chirality.theme";
let current: Theme | null = null;
const listeners = new Set<() => void>();

export function storedTheme(): Theme {
  if (current) return current;
  try {
    const t = localStorage.getItem(key);
    return t === "dark" || t === "system" ? t : "light";
  } catch {
    return "light";
  }
}

/** Shows `theme` in the engine's parts, and keeps it for next time. */
export function applyTheme(theme: Theme) {
  current = theme;
  try {
    localStorage.setItem(key, theme);
  } catch {
    // Kept for this session only.
  }
  for (const listener of listeners) listener();
}

/** The theme the parts show. */
export function useTheme(): Theme {
  return useSyncExternalStore(
    (listener) => {
      listeners.add(listener);
      return () => void listeners.delete(listener);
    },
    storedTheme,
  );
}
