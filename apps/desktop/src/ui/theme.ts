// The person's appearance choice: light, dark or the system's. Light by default.
export type Theme = "light" | "dark" | "system";

const key = "chirality.theme";

export function storedTheme(): Theme {
  try {
    const t = localStorage.getItem(key);
    return t === "dark" || t === "system" ? t : "light";
  } catch {
    return "light";
  }
}

export function applyTheme(theme: Theme) {
  document.documentElement.dataset.theme = theme;
  try {
    localStorage.setItem(key, theme);
  } catch {
    // Kept for this session only.
  }
}
