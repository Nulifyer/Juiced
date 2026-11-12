import { afterEach, describe, expect, spyOn, test } from "bun:test";
import { Client } from "../src/api";
import type { Model } from "../src/domain";
import { registration } from "../src/management";
import { MAX_AUDIO_UPLOAD_BYTES, multipartPayloadBytes, speechModels, transcriptionForm, transcriptionText } from "../src/speech";

const wav = Uint8Array.from([82, 73, 70, 70, 8, 0, 0, 0, 87, 65, 86, 69, 1, 2, 3, 4]);
const model = { id: "speech", policy: "on-demand", downloaded: true, max_inflight: 1, operations: ["audio/transcriptions"] } as Model;
let fetchSpy: ReturnType<typeof spyOn> | undefined;
afterEach(() => { fetchSpy?.mockRestore(); fetchSpy = undefined; });

describe("temporary speech input boundary", () => {
  test("only enabled downloaded single-slot transcription models are eligible", () => {
    expect(speechModels([model])).toEqual([model]);
    expect(speechModels([{ ...model, policy: "disabled" }, { ...model, downloaded: false }, { ...model, max_inflight: 2 }, { ...model, operations: ["chat/completions"] }])).toEqual([]);
  });
  test("multipart keeps existing audio bytes and normalizes only the language field", async () => {
    const file = new File([wav], "existing.wav", { type: "audio/wav" });
    const form = await transcriptionForm("speech", file, " DE ");
    expect([...form.keys()]).toEqual(["model", "file", "language", "response_format"]);
    expect(form.get("language")).toBe("de"); expect(form.get("response_format")).toBe("json");
    expect(new Uint8Array(await (form.get("file") as File).arrayBuffer())).toEqual(wav);
    expect(multipartPayloadBytes(form)).toBe(wav.length + "speech".length + "de".length + "json".length);
  });
  test("empty, non-WAV and invalid language inputs fail before any upload", async () => {
    await expect(transcriptionForm("speech", new File([], "empty.wav"), "auto")).rejects.toThrow("nonempty");
    await expect(transcriptionForm("speech", new File(["private unsupported audio"], "fake.wav"), "de")).rejects.toThrow("WAV");
    for (const language of ["", "de-DE", "english"]) await expect(transcriptionForm("speech", new File([wav], "file.wav"), language)).rejects.toThrow("language");
  });
  test("a valid empty text is not replaced by fabricated output and malformed responses fail", () => {
    expect(transcriptionText({ text: "" })).toBe("");
    expect(transcriptionText({ text: "λ" })).toBe("λ");
    for (const value of [null, { text: 3 }, { segments: [] }]) expect(() => transcriptionText(value)).toThrow("invalid JSON text");
  });
  test("Whisper registration binds its explicit provider, loopback host, operation and one slot", () => {
    const args = '["--host","127.0.0.1","--port","{port}","--model","{model}","--no-gpu"]';
    const result = registration("a".repeat(64), "voice", "whisper-server", args, "1024", "1", ["audio/transcriptions"], "whisper-cpp");
    expect(result).toMatchObject({ execution_profile: { schema_version: 1, adapter: "whisper-cpp", implementation_id: "whisper-cpp", command: "whisper-server", args: JSON.parse(args), options: {}, placement: "unresolved" }, max_inflight: 1, operations: ["audio/transcriptions"], policy: "disabled" });
    for (const legacyField of ["provider", "command", "args", "options"]) expect(Object.hasOwn(result, legacyField)).toBe(false);
    expect(() => registration("a".repeat(64), "voice", "engine", args, "1024", "2", ["audio/transcriptions"], "whisper-cpp")).toThrow("one slot");
    expect(() => registration("a".repeat(64), "voice", "engine", args, "1024", "1", ["audio/transcriptions"], "external-http")).toThrow("explicit whisper.cpp");
    expect(() => registration("a".repeat(64), "voice", "engine", args.replace("127.0.0.1", "0.0.0.0"), "1024", "1", ["audio/transcriptions"], "whisper-cpp")).toThrow("loopback");
  });
});

describe("shared HTTP owner for multipart", () => {
  test("credentials remain headers and the browser owns the multipart boundary", async () => {
    let seenUrl = ""; let seen: RequestInit | undefined;
    fetchSpy = spyOn(globalThis, "fetch").mockImplementation(Object.assign(async (url: Parameters<typeof fetch>[0], options?: Parameters<typeof fetch>[1]) => { seenUrl = String(url); seen = options; return Response.json({ text: "fixture" }); }, { preconnect: fetch.preconnect }));
    const form = await transcriptionForm("speech", new File([wav], "file.wav"), "auto");
    const controller = new AbortController();
    expect(await new Client("", "private-key").multipart("audio/transcriptions", form, controller.signal)).toEqual({ text: "fixture" });
    expect(seenUrl).toBe("/v1/audio/transcriptions"); expect(seen?.body).toBe(form); expect(seen?.signal).toBe(controller.signal);
    expect(seen?.headers).toEqual({ Authorization: "Bearer private-key" });
    expect([...form.keys()]).not.toContain("Authorization"); expect([...form.keys()]).not.toContain("session");
  });
  test("oversized payloads are rejected before fetch, including text-field bytes", async () => {
    fetchSpy = spyOn(globalThis, "fetch");
    const form = new FormData(); form.set("file", new File([new Uint8Array(MAX_AUDIO_UPLOAD_BYTES)], "large.wav")); form.set("model", "speech");
    await expect(new Client("", "").multipart("audio/transcriptions", form)).rejects.toThrow("8 MiB");
    expect(fetchSpy).not.toHaveBeenCalled();
  });
  test("multipart uses the same structured service errors and bounded response decoder", async () => {
    fetchSpy = spyOn(globalThis, "fetch").mockResolvedValue(Response.json({ error: { code: "invalid_transcription", message: "Choose an existing WAV file." } }, { status: 400 }));
    await expect(new Client("", "").multipart("audio/transcriptions", new FormData())).rejects.toMatchObject({ status: 400, code: "invalid_transcription", message: "Choose an existing WAV file." });
    fetchSpy.mockResolvedValue(new Response(new Uint8Array(MAX_AUDIO_UPLOAD_BYTES + 1)));
    await expect(new Client("", "").request("juiced/state")).rejects.toMatchObject({ code: "response_limit" });
  });
});
