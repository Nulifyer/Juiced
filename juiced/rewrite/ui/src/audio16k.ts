export const SPEECH_RATE = 16000;

/** 16-bit PCM mono WAV bytes for samples in [-1, 1]. */
export function encodeWav(samples: Float32Array, rate = SPEECH_RATE): Uint8Array<ArrayBuffer> {
  const bytes = new Uint8Array(44 + samples.length * 2);
  const view = new DataView(bytes.buffer);
  const ascii = (offset: number, text: string) => { for (let index = 0; index < text.length; index++) bytes[offset + index] = text.charCodeAt(index); };
  ascii(0, "RIFF"); view.setUint32(4, 36 + samples.length * 2, true); ascii(8, "WAVE");
  ascii(12, "fmt "); view.setUint32(16, 16, true); view.setUint16(20, 1, true); view.setUint16(22, 1, true);
  view.setUint32(24, rate, true); view.setUint32(28, rate * 2, true); view.setUint16(32, 2, true); view.setUint16(34, 16, true);
  ascii(36, "data"); view.setUint32(40, samples.length * 2, true);
  for (let index = 0; index < samples.length; index++) {
    const sample = Math.max(-1, Math.min(1, samples[index]!));
    view.setInt16(44 + index * 2, sample < 0 ? sample * 0x8000 : sample * 0x7fff, true);
  }
  return bytes;
}

/** Whether the file is already the WAV layout speech engines want: mono 16-bit PCM at 16 kHz. */
export async function isSpeechWav(file: Blob): Promise<boolean> {
  const header = new DataView(await file.slice(0, 44).arrayBuffer());
  if (header.byteLength < 44) return false;
  const tag = (offset: number) => String.fromCharCode(...new Uint8Array(header.buffer, offset, 4));
  return tag(0) === "RIFF" && tag(8) === "WAVE" && tag(12) === "fmt " && header.getUint16(20, true) === 1 && header.getUint16(22, true) === 1 && header.getUint32(24, true) === SPEECH_RATE && header.getUint16(34, true) === 16;
}

/** Decodes any browser-playable audio and resamples it to a 16 kHz mono WAV file. */
export async function toSpeechWav(file: Blob, name: string): Promise<File> {
  if (await isSpeechWav(file)) return file instanceof File ? file : new File([file], name, { type: "audio/wav" });
  const Context = globalThis.AudioContext ?? (globalThis as unknown as { webkitAudioContext?: typeof AudioContext }).webkitAudioContext;
  if (!Context || typeof OfflineAudioContext === "undefined") throw new Error("This browser can't convert audio. Choose a 16 kHz WAV file.");
  const context = new Context();
  let decoded: AudioBuffer;
  try { decoded = await context.decodeAudioData(await file.arrayBuffer()); }
  catch { throw new Error("The browser couldn't read this audio. Choose a WAV, MP3, OGG or other audio file."); }
  finally { void context.close(); }
  const frames = Math.max(1, Math.ceil(decoded.duration * SPEECH_RATE));
  const offline = new OfflineAudioContext(1, frames, SPEECH_RATE);
  const source = offline.createBufferSource();
  source.buffer = decoded; source.connect(offline.destination); source.start();
  const rendered = await offline.startRendering();
  const base = name.replace(/\.[^.]*$/, "") || "audio";
  return new File([encodeWav(rendered.getChannelData(0))], `${base}.wav`, { type: "audio/wav" });
}

export const recordingType = () => typeof MediaRecorder === "undefined" ? null : ["audio/webm;codecs=opus", "audio/webm", "audio/ogg;codecs=opus", "audio/mp4"].find((type) => MediaRecorder.isTypeSupported(type)) ?? "";
