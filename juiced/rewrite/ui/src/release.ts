export type ReleaseNotice = { check: boolean; current: string; latest: string | null; url: string | null; newer: boolean };

const RELEASES = "https://github.com/Nulifyer/Juiced/releases";

export function parseRelease(value: unknown): ReleaseNotice | null {
  if (!value || typeof value !== "object" || Array.isArray(value)) return null;
  const item = value as Record<string, unknown>;
  if (typeof item.check !== "boolean" || typeof item.current !== "string") return null;
  const latest = typeof item.latest === "string" && /^v?\d+\.\d+\.\d+[\w.+-]*$/.test(item.latest) ? item.latest : null;
  const url = typeof item.url === "string" && item.url.startsWith(`${RELEASES}/`) ? item.url : latest ? RELEASES : null;
  return { check: item.check, current: item.current, latest, url, newer: latest !== null && item.newer === true };
}

export const releaseLabel = (version: string) => `${version.startsWith("v") ? version : `v${version}`} available`;
