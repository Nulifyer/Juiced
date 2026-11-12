import { operationEligible, type Model } from "./domain";

export const MAX_AUDIO_UPLOAD_BYTES = 8 * 1024 * 1024;
export function speechModels(models: Model[]): Model[] {
  return models.filter((model) => operationEligible(model, "audio/transcriptions") && model.max_inflight === 1);
}
export function multipartPayloadBytes(form: FormData): number {
  let bytes = 0;
  for (const [, value] of form) {
    bytes += typeof value === "string" ? new TextEncoder().encode(value).byteLength : value.size;
    if (bytes > MAX_AUDIO_UPLOAD_BYTES) throw new Error("Audio and form fields must total at most 8 MiB. Choose a smaller WAV file.");
  }
  return bytes;
}
export async function transcriptionForm(model: string, file: File, language: string): Promise<FormData> {
  if (!model || new TextEncoder().encode(model).length > 256 || /[\x00-\x1f\x7f]/.test(model)) throw new Error("Choose a registered speech model.");
  if (file.size === 0 || file.size > MAX_AUDIO_UPLOAD_BYTES) throw new Error("Choose a nonempty WAV file under 8 MiB, including form fields.");
  const header = new Uint8Array(await file.slice(0, 12).arrayBuffer());
  const tag = (start: number, end: number) => String.fromCharCode(...header.subarray(start, end));
  if (header.length < 12 || !["RIFF", "RIFX", "RF64", "BW64"].includes(tag(0, 4)) || tag(8, 12) !== "WAVE") throw new Error("Choose a WAV audio file. This console does not convert or repack audio.");
  const code = language.trim().toLowerCase();
  if (code !== "auto" && !/^[a-z]{2,3}$/.test(code)) throw new Error("Use auto or a two- or three-letter audio language code.");
  const form = new FormData(); form.set("model", model); form.set("file", file, file.name); form.set("language", code); form.set("response_format", "json");
  multipartPayloadBytes(form);
  return form;
}
export function transcriptionText(value: unknown): string {
  if (!value || typeof value !== "object" || typeof (value as { text?: unknown }).text !== "string") throw new Error("The speech service returned an invalid JSON text response. Check the selected engine and retry.");
  return (value as { text: string }).text;
}
