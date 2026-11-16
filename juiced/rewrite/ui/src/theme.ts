export type Theme = "auto" | "dark" | "light";
export const themes: readonly (readonly [Theme, string])[] = [["auto", "Auto"], ["dark", "Dark"], ["light", "Light"]];
const KEY = "juiced.rewrite.theme";

export function readTheme(): Theme {
  try {
    const saved = localStorage.getItem(KEY);
    return saved === "dark" || saved === "light" ? saved : "auto";
  } catch { return "auto"; }
}

export function applyTheme(theme: Theme) {
  if (theme === "auto") delete document.documentElement.dataset.theme;
  else document.documentElement.dataset.theme = theme;
}

export function saveTheme(theme: Theme) {
  applyTheme(theme);
  try {
    if (theme === "auto") localStorage.removeItem(KEY); else localStorage.setItem(KEY, theme);
  } catch { /* Storage can be unavailable in private browser sessions. */ }
}
