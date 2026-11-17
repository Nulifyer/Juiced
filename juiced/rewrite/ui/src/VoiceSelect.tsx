import { useCallback } from "react";
import { Picker, wordsMatch, type PickGroup, type PickQuery, type PickView } from "./Picker";

/** Kokoro names a voice by language, then gender, then name: `bf_emma` is a British English female voice. */
const languages: [string, string][] = [["a", "American English"], ["b", "British English"], ["e", "Spanish"], ["f", "French"], ["h", "Hindi"], ["i", "Italian"], ["j", "Japanese"], ["p", "Brazilian Portuguese"], ["z", "Mandarin Chinese"]];
const coded = /^([a-z])([fm])_[a-z0-9]+$/;

export type Voice = { id: string; language: string | null; gender: "Female" | "Male" | null };

export function describeVoice(id: string): Voice {
  const match = coded.exec(id);
  const language = match ? languages.find(([code]) => code === match[1])?.[1] ?? null : null;
  return { id, language, gender: language ? match![2] === "f" ? "Female" : "Male" : null };
}

/** Groups voices by language when the engine's names say it, and matches every word against the name, language, gender and the names clients may send instead. */
export function voiceView(voices: string[], aliases: Record<string, string[]>, query: string): PickView {
  const described = voices.map(describeVoice);
  const matching = described.filter((voice) => wordsMatch([voice.id, voice.language, voice.gender, ...(aliases[voice.id] ?? [])].join(" "), query));
  const groups: PickGroup[] = [];
  for (const [, language] of languages) {
    const items = matching.filter((voice) => voice.language === language);
    if (items.length) groups.push({ key: language, label: language, items: items.map((voice) => ({ value: voice.id, label: voice.id, kind: voice.gender ?? undefined })) });
  }
  const other = matching.filter((voice) => voice.language === null);
  if (other.length) groups.push({ key: "other", label: groups.length ? "Other" : "", items: other.map((voice) => ({ value: voice.id, label: voice.id })) });
  return { groups, shown: matching.length, total: voices.length, hidden: 0, columns: described.some((voice) => voice.gender !== null) };
}

/** Maps each voice to the other names that reach it, from `{ "alloy": "af_alloy" }`. */
export function voiceAliases(value: unknown): Record<string, string[]> {
  const aliases = value && typeof value === "object" ? (value as { aliases?: unknown }).aliases : undefined;
  const result: Record<string, string[]> = {};
  if (!aliases || typeof aliases !== "object") return result;
  for (const [alias, voice] of Object.entries(aliases).slice(0, 256)) if (typeof voice === "string") (result[voice] ??= []).push(alias);
  return result;
}

export function VoiceSelect({ voices, aliases = {}, value, onChange, placeholder, leading, id, disabled, ...naming }: { voices: string[]; aliases?: Record<string, string[]>; value: string; onChange: (voice: string) => void; placeholder: string; leading?: [string, string][]; id?: string; disabled?: boolean; "aria-label"?: string }) {
  const view = useCallback(({ query }: PickQuery) => voiceView(voices, aliases, query), [voices, aliases]);
  return <Picker value={value} onChange={onChange} view={view} placeholder={placeholder} noun={["voice", "voices"]} searchHint="Search by name or language" leading={leading} id={id} disabled={disabled} {...naming} />;
}
