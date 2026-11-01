export type Model = {
  id: string;
  recipe: string;
  labels: string[];
  service: string;
  downloaded: boolean;
  available?: boolean;
  unavailable_reason?: string;
  loaded: boolean;
  state: string;
  policy: "hot" | "on-demand" | "disabled";
  size?: number;
  context_length?: number;
  checkpoints?: Record<string, string>;
  operation?: { state: string; error?: string; pending: boolean };
  runtime?: { pid: number; is_busy: boolean };
};
export type Message = {
  role: "user" | "assistant" | "system";
  content:
    string | { type: string; text?: string; image_url?: { url: string } }[];
  reasoning_content?: string;
};
export type Thread = {
  id: string;
  title: string;
  shelf: "working" | "settled";
  model?: string;
  messages: Message[];
  revision: number;
  updated?: number;
  cache_state?: string;
  retain_cache?: boolean;
};
export async function api<T = any>(
  path: string,
  body?: unknown,
  signal?: AbortSignal,
): Promise<T> {
  const r = await fetch("/v1/" + path, {
    method: body === undefined ? "GET" : "POST",
    signal,
    headers: body === undefined ? {} : { "Content-Type": "application/json" },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  let j;
  try {
    j = await r.json();
  } catch {
    throw Error(
      "The server returned an unreadable response (" + r.status + ").",
    );
  }
  if (!r.ok)
    throw Error(
      typeof j.error === "string"
        ? j.error
        : j.error?.message || JSON.stringify(j),
    );
  return j;
}
export const bytes = (n: number) =>
  n >= 1024 ** 3
    ? (n / 1024 ** 3).toFixed(1) + " GiB"
    : n >= 1024 ** 2
      ? (n / 1024 ** 2).toFixed(1) + " MiB"
      : n.toLocaleString() + " B";
export async function wav16k(file: Blob): Promise<Blob> {
  const ctx = new AudioContext();
  try {
    const input = await ctx.decodeAudioData(await file.arrayBuffer());
    if (input.duration > 120) throw Error("Use audio up to 120 seconds.");
    const out = await (await makeOffline(input)).startRendering();
    return encodeWav(out.getChannelData(0));
  } finally {
    await ctx.close();
  }
}
async function makeOffline(input: AudioBuffer) {
  const out = new OfflineAudioContext(
      1,
      Math.ceil(input.duration * 16000),
      16000,
    ),
    s = out.createBufferSource();
  s.buffer = input;
  s.connect(out.destination);
  s.start();
  return out;
}
function encodeWav(samples: Float32Array) {
  const b = new ArrayBuffer(44 + samples.length * 2),
    v = new DataView(b);
  const str = (o: number, s: string) => {
    for (let i = 0; i < s.length; i++) v.setUint8(o + i, s.charCodeAt(i));
  };
  str(0, "RIFF");
  v.setUint32(4, b.byteLength - 8, true);
  str(8, "WAVE");
  str(12, "fmt ");
  v.setUint32(16, 16, true);
  v.setUint16(20, 1, true);
  v.setUint16(22, 1, true);
  v.setUint32(24, 16000, true);
  v.setUint32(28, 32000, true);
  v.setUint16(32, 2, true);
  v.setUint16(34, 16, true);
  str(36, "data");
  v.setUint32(40, samples.length * 2, true);
  for (let i = 0; i < samples.length; i++)
    v.setInt16(44 + 2 * i, Math.max(-1, Math.min(1, samples[i])) * 32767, true);
  return new Blob([b], { type: "audio/wav" });
}
