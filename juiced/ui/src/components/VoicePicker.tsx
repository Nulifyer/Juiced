import { useEffect, useState } from "react";
import { api } from "../api";

export function VoicePicker({
  model,
  value,
  onChange,
  disabled,
}: {
  model: string;
  value: string;
  onChange: (voice: string) => void;
  disabled?: boolean;
}) {
  const [voices, setVoices] = useState<string[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState("");
  useEffect(() => {
    const controller = new AbortController();
    setVoices([]);
    setError("");
    setLoading(Boolean(model));
    if (!model) return;
    void api(
      "audio/voices?model=" + encodeURIComponent(model),
      undefined,
      controller.signal,
    )
      .then((result) => {
        const next: string[] = result.voices || [];
        setVoices(next);
        if (!next.length)
          setError(
            "This model did not report any voices. Enter a supported voice ID.",
          );
        if (next.length && !next.includes(value))
          onChange(next.includes("af_heart") ? "af_heart" : next[0]!);
      })
      .catch((e) => {
        if (!controller.signal.aborted) setError(e.message);
      })
      .finally(() => {
        if (!controller.signal.aborted) setLoading(false);
      });
    return () => controller.abort();
  }, [model]);
  const english = voices.filter((v) => /^(a|b)[fm]_/.test(v));
  const aliases = voices.filter((v) => !v.includes("_"));
  const other = voices.filter(
    (v) => !english.includes(v) && !aliases.includes(v),
  );
  return (
    <label className="voice-picker">
      Voice
      {voices.length ? (
        <select
          aria-label="Speech voice"
          value={value}
          disabled={disabled || loading}
          onChange={(e) => onChange(e.target.value)}
        >
          {[
            ["English voices", english],
            ["Other languages", other],
            ["OpenAI aliases", aliases],
          ].map(
            ([title, entries]) =>
              (entries as string[]).length > 0 && (
                <optgroup label={title as string} key={title as string}>
                  {(entries as string[]).map((voice) => (
                    <option key={voice} value={voice}>
                      {voice}
                    </option>
                  ))}
                </optgroup>
              ),
          )}
        </select>
      ) : (
        <input
          aria-label="Speech voice"
          value={value}
          disabled={disabled || loading || !model}
          onChange={(e) => onChange(e.target.value)}
          placeholder={loading ? "Loading voices…" : "Voice ID"}
        />
      )}
      <span className="voice-picker-note" role="status">
        {loading
          ? "Reading voices from the loaded model…"
          : voices.length
            ? `${english.length} English voices · ${voices.length} total options from ${model}`
            : error || "Load a speech model to see its voices."}
      </span>
    </label>
  );
}
