/** Split GGUF parts keep the exact `-00001-of-00003.gguf` spelling llama.cpp looks for. */
export function splitPart(name: string): { prefix: string; part: number; parts: number } | null {
  const match = /^(.+)-(\d{5})-of-(\d{5})\.gguf$/.exec(name);
  if (!match) return null;
  const part = Number(match[2]), parts = Number(match[3]);
  return parts > 1 && part >= 1 && part <= parts ? { prefix: match[1]!, part, parts } : null;
}

export const isProjectorName = (name: string) => /mmproj/i.test(name) && /\.gguf$/i.test(name);

type Named = { name: string };
export type UploadPlan<T extends Named> = { files: T[]; label: string; parts: number } | { error: string };

/** One model (a single file or every part of one split model), optionally with one projector. */
export function planUpload<T extends Named>(picked: T[]): UploadPlan<T> {
  const projectors = picked.filter((file) => isProjectorName(file.name));
  const models = picked.filter((file) => !isProjectorName(file.name));
  if (projectors.length > 1) return { error: "Pick at most one vision projector (mmproj) file." };
  if (models.length === 0) return { error: projectors.length ? "This is a vision projector. Pick its model file too." : "Pick a model file." };
  const split = models.map((file) => splitPart(file.name));
  const first = split[0];
  if (first && split.every((part) => part && part.prefix === first.prefix && part.parts === first.parts)) {
    const found = new Set(split.map((part) => part!.part));
    const missing = Array.from({ length: first.parts }, (_, index) => index + 1).filter((part) => !found.has(part));
    if (missing.length || found.size !== models.length) return { error: `Pick all ${first.parts} parts of ${first.prefix}. Missing part ${missing.join(", ") || "numbers are repeated"}.` };
    const ordered = [...models].sort((left, right) => splitPart(left.name)!.part - splitPart(right.name)!.part);
    return { files: [...ordered, ...projectors], label: ordered[0]!.name, parts: first.parts };
  }
  if (models.length > 1) return { error: "Pick one model file, or every part of one split model." };
  return { files: [...models, ...projectors], label: models[0]!.name, parts: 1 };
}

type Entry = { name: string; bytes: number | null; split: boolean };

/** Shows a split model once, as its first part with the group's total size and part count. */
export function groupEntries<T extends Entry>(entries: T[]): (T & { parts: number })[] {
  const totals = new Map<string, number>();
  const firsts = new Set<string>();
  const key = (part: { prefix: string; parts: number }) => `${part.prefix}/${part.parts}`;
  for (const entry of entries) {
    const part = entry.split ? splitPart(entry.name) : null;
    if (!part) continue;
    totals.set(key(part), (totals.get(key(part)) ?? 0) + (entry.bytes ?? 0));
    if (part.part === 1) firsts.add(key(part));
  }
  return entries.flatMap((entry) => {
    const part = entry.split ? splitPart(entry.name) : null;
    if (!part || !firsts.has(key(part))) return [{ ...entry, parts: 1 }];
    if (part.part !== 1) return [];
    return [{ ...entry, parts: part.parts, bytes: totals.get(key(part)) ?? entry.bytes }];
  });
}
