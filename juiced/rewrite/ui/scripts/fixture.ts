import { chmod, copyFile, lstat, mkdir, mkdtemp, readdir, rm, symlink, truncate, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, relative, resolve } from "node:path";

const root = resolve(import.meta.dir, "../..");
const build = Bun.spawn(["cargo", "build", "--all-features", "--locked"], { cwd: root, stdout: "inherit", stderr: "inherit" });
if (await build.exited !== 0) process.exit(1);
if (!await Bun.file(resolve(root, "ui/dist/index.html")).exists()) {
  throw new Error("Build the console first with bun run build.");
}
const directory = await mkdtemp(resolve(tmpdir(), "juiced-rust-preview-"));
const executable = (name: string) => resolve(root, "target/debug", name + (process.platform === "win32" ? ".exe" : ""));
const weights = Uint8Array.from({ length: 256 * 1024 }, (_, index) => index % 251);
const sha256 = new Bun.CryptoHasher("sha256").update(weights).digest("hex");
const failed = new Set<string>();
const hubRevision = "0123456789abcdef0123456789abcdef01234567";
type HubFile = { path: string; bytes: Uint8Array; declared?: number; small?: boolean };
const hubBytes = (seed: number, length: number) => Uint8Array.from({ length }, (_, index) => (index % 239) ^ seed);
function metadataGguf(context: number, length: number) {
  const encoder = new TextEncoder();
  const parts: Uint8Array[] = [encoder.encode("GGUF")];
  const integer = (bits: 32 | 64, value: number) => { const view = new DataView(new ArrayBuffer(bits / 8)); if (bits === 32) view.setUint32(0, value, true); else view.setBigUint64(0, BigInt(value), true); parts.push(new Uint8Array(view.buffer)); };
  const string = (text: string) => { const bytes = encoder.encode(text); integer(64, bytes.length); parts.push(bytes); };
  integer(32, 3); integer(64, 0); integer(64, 2);
  string("general.architecture"); integer(32, 8); string("llama");
  string("llama.context_length"); integer(32, 4); integer(32, context);
  const bytes = new Uint8Array(length);
  let offset = 0;
  for (const part of parts) { bytes.set(part, offset); offset += part.length; }
  return bytes;
}
const hubDigest = (file: HubFile) => file.declared ? "f".repeat(64) : new Bun.CryptoHasher("sha256").update(file.bytes).digest("hex");
const gitBlob = (file: HubFile) => new Bun.CryptoHasher("sha1").update(`blob ${file.bytes.length}\0`).update(file.bytes).digest("hex");
const smallFile = (path: string, text: string): HubFile => ({ path, bytes: new TextEncoder().encode(text), small: true });
const hubRepos = new Map<string, { pipeline: string; downloads: number; files: HubFile[]; tags?: string[]; gated?: boolean }>([
  ["Qwen/Qwen3-0.6B", { pipeline: "text-generation", downloads: 9800000, tags: ["transformers", "safetensors"], files: [smallFile("config.json", '{"architectures":["Qwen3ForCausalLM"],"hidden_size":1024,"num_attention_heads":16,"num_key_value_heads":8,"num_hidden_layers":28,"head_dim":128,"max_position_embeddings":40960,"torch_dtype":"bfloat16"}'), smallFile("generation_config.json", '{"temperature":0.6}'), smallFile("tokenizer_config.json", '{"model_max_length":131072}'), smallFile("merges.txt", "#version: 0.2\n"), smallFile("vocab.json", "{}"), { path: "tokenizer.json", bytes: hubBytes(41, 64 * 1024) }, { path: "model.safetensors", bytes: new Uint8Array(), declared: 1503300328 }, smallFile("README.md", "# Qwen3-0.6B")] }],
  ["fixture-labs/Tiny-Classifier-ONNX", { pipeline: "text-classification", downloads: 3100, tags: ["onnx", "text-classification"], files: [smallFile("config.json", '{"architectures":["DistilBertForSequenceClassification"],"id2label":{"0":"safe","1":"phishing"}}'), smallFile("tokenizer.json", '{"version":"1.0","model":{"type":"WordPiece"}}'), smallFile("special_tokens_map.json", '{"cls_token":"[CLS]"}'), smallFile("vocab.txt", "[PAD]\n[UNK]\nhello\n"), { path: "model.onnx", bytes: Uint8Array.from({ length: 192 * 1024 }, (_, index) => index < 2 ? [8, 7][index]! : (index % 241) ^ 17) }, smallFile("README.md", "# Tiny classifier")] }],
  ["unsloth/Qwen3-0.6B-GGUF", { pipeline: "text-generation", downloads: 125000, files: ["Q2_K", "Q3_K_M", "Q4_0", "Q4_K_M", "Q5_K_M", "Q6_K", "Q8_0", "F16", "IQ4_NL", "IQ2_XXS"].map((label, index): HubFile => ({ path: `Qwen3-0.6B-${label}.gguf`, bytes: hubBytes(index + 1, 4096 + index * 512) })).concat([{ path: "Qwen3-0.6B-BF16.gguf", bytes: new Uint8Array(), declared: 4 * 1024 ** 4 }]) }],
  ["fixture-labs/Tiny-Chat-GGUF", { pipeline: "text-generation", downloads: 4200, files: [{ path: "Tiny-Chat-Q4_K_M.gguf", bytes: hubBytes(31, 256 * 1024) }, { path: "Tiny-Chat-Q8_0.gguf", bytes: metadataGguf(2048, 96 * 1024) }] }],
]);
const hubFiles = new Map<string, HubFile>([...hubRepos].flatMap(([repo, entry]) => entry.files.map((file): [string, HubFile] => [`/${repo}/resolve/${hubRevision}/${file.path}`, file])));
function slowBody(bytes: Uint8Array, start: number, chunk: number, delay: number) {
  let position = start, cancelled = false;
  return new ReadableStream<Uint8Array>({
    async pull(controller) {
      await Bun.sleep(delay);
      if (cancelled) return;
      const end = Math.min(position + chunk, bytes.length);
      controller.enqueue(bytes.subarray(position, end)); position = end;
      if (position === bytes.length) controller.close();
    },
    cancel() { cancelled = true; },
  });
}
function hub(url: URL): Response | undefined {
  if (url.pathname === "/api/models") {
    const search = (url.searchParams.get("search") ?? "").toLowerCase();
    const pipeline = url.searchParams.get("pipeline_tag");
    const filters = url.searchParams.getAll("filter");
    const has = (entry: { files: HubFile[] }, format: string) => entry.files.some((file) => file.path.endsWith(`.${format}`));
    return Response.json([...hubRepos].filter(([repo, entry]) => repo.toLowerCase().includes(search) && (!pipeline || entry.pipeline === pipeline) && filters.every((format) => has(entry, format))).map(([id, entry]) => ({ id, downloads: entry.downloads, likes: 3, pipeline_tag: entry.pipeline, tags: entry.tags ?? ["gguf"] })));
  }
  const info = /^\/api\/models\/([^/]+\/[^/]+)(?:\/revision\/([0-9a-f]{40}))?$/.exec(url.pathname);
  if (info && url.searchParams.get("blobs") === "true") {
    const entry = hubRepos.get(info[1]!);
    if (!entry || (info[2] && info[2] !== hubRevision)) return new Response("{}", { status: 401 });
    const siblings = entry.files.map((file) => { const size = file.declared ?? file.bytes.length; return file.small ? { rfilename: file.path, size, blobId: gitBlob(file) } : { rfilename: file.path, size, lfs: { sha256: hubDigest(file), size, pointerSize: 134 } }; });
    return Response.json({ id: info[1], sha: hubRevision, downloads: entry.downloads, likes: 3, pipeline_tag: entry.pipeline, gated: entry.gated ?? false, tags: entry.tags ?? ["gguf"], cardData: { license: "apache-2.0" }, siblings: [...siblings, { rfilename: ".gitattributes", size: 1519 }] });
  }
  const file = hubFiles.get(url.pathname);
  if (file?.small) return new Response(new Blob([file.bytes as Uint8Array<ArrayBuffer>]), { headers: { "Content-Length": String(file.bytes.length), ETag: `"${gitBlob(file)}"` } });
  if (file && !file.declared) return new Response(slowBody(file.bytes, 0, 4096, 60), { headers: { "Content-Length": String(file.bytes.length), ETag: `"${hubDigest(file)}"` } });
  if (url.pathname.includes("/resolve/")) return new Response("Not found", { status: 404 });
}
type CacheFile = [path: string, bytes: number];
const gib = 1024 ** 3, mib = 1024 ** 2;
/** Sparse files in the `huggingface_hub` cache layout: content-named blobs behind snapshot links. */
async function cacheRepo(hub: string, repo: string, revision: string, files: CacheFile[]) {
  const directory = resolve(hub, `models--${repo.replace("/", "--")}`);
  const snapshot = resolve(directory, "snapshots", revision);
  await mkdir(resolve(directory, "blobs"), { recursive: true });
  await mkdir(resolve(directory, "refs"), { recursive: true });
  await writeFile(resolve(directory, "refs", "main"), revision);
  for (const [path, size] of files) {
    const blob = resolve(directory, "blobs", new Bun.CryptoHasher("sha256").update(`${repo}/${path}`).digest("hex"));
    await writeFile(blob, path.endsWith(".gguf") ? metadataGguf(4096, 1024) : new Uint8Array(1024));
    await truncate(blob, Math.round(size));
    const link = resolve(snapshot, path);
    await mkdir(dirname(link), { recursive: true });
    await symlink(relative(dirname(link), blob), link);
  }
  return snapshot;
}
let source: ReturnType<typeof Bun.serve> | undefined;
let engineArchive = new Uint8Array();
let ortArchive = new Uint8Array();
const llamaRelease = { repository: "ggml-org/llama.cpp", tag: "b12032", asset: "llama-b12032-bin-ubuntu-x64.tar.gz" };
const ortRelease = { repository: "lemonade-sdk/ort-server", tag: "0.3.7", asset: "ort-server-0.3.7-linux-x64.tar.gz" };
function engineRelease(url: URL): Response | undefined {
  if (url.pathname === `/api/repos/${ortRelease.repository}/releases/tags/${ortRelease.tag}`) {
    return Response.json({ tag_name: ortRelease.tag, assets: [{ name: ortRelease.asset, size: ortArchive.length, digest: `sha256:${new Bun.CryptoHasher("sha256").update(ortArchive).digest("hex")}` }] });
  }
  if (url.pathname === `/${ortRelease.repository}/releases/download/${ortRelease.tag}/${ortRelease.asset}`) {
    return new Response(slowBody(ortArchive, 0, 256 * 1024, 70), { headers: { "Content-Length": String(ortArchive.length), "Content-Type": "application/octet-stream" } });
  }
  if (url.pathname === `/api/repos/${llamaRelease.repository}/releases/tags/${llamaRelease.tag}`) {
    return Response.json({ tag_name: llamaRelease.tag, assets: [{ name: llamaRelease.asset, size: engineArchive.length, digest: `sha256:${new Bun.CryptoHasher("sha256").update(engineArchive).digest("hex")}` }] });
  }
  if (url.pathname === `/${llamaRelease.repository}/releases/download/${llamaRelease.tag}/${llamaRelease.asset}`) {
    return new Response(slowBody(engineArchive, 0, 256 * 1024, 70), { headers: { "Content-Length": String(engineArchive.length), "Content-Type": "application/octet-stream" } });
  }
  if (url.pathname.startsWith("/api/repos/") || url.pathname.includes("/releases/download/")) return new Response("Not found", { status: 404 });
}
try {
  const hardware = resolve(directory, "hardware");
  await mkdir(resolve(hardware, "proc"), { recursive: true });
  await writeFile(resolve(hardware, "proc/cpuinfo"), Array.from({ length: 8 }, (_, processor) => `processor\t: ${processor}\nvendor_id\t: AuthenticAMD\nmodel name\t: Fixture CPU\nphysical id\t: 0\ncore id\t\t: ${processor % 4}\ncpu cores\t: 4\nflags\t\t: avx avx2 fma f16c\n\n`).join(""));
  await writeFile(resolve(hardware, "proc/meminfo"), "MemTotal:       32763640 kB\nMemAvailable:   26446104 kB\n");
  if (process.platform === "linux" && process.arch === "x64") {
    const release = resolve(directory, "engine-release");
    await mkdir(resolve(release, "llama-b12032"), { recursive: true });
    await copyFile(executable("juiced-test-worker"), resolve(release, "llama-b12032/llama-server"));
    await writeFile(resolve(release, "llama-b12032/LICENSE"), "MIT fixture\n");
    const packer = Bun.spawn(["tar", "--format=gnu", "-C", release, "-czf", resolve(release, llamaRelease.asset), "llama-b12032"], { stdout: "inherit", stderr: "inherit" });
    if (await packer.exited !== 0) throw new Error("The engine release fixture could not be packed.");
    engineArchive = new Uint8Array(await Bun.file(resolve(release, llamaRelease.asset)).arrayBuffer());
    await mkdir(resolve(release, "ort-server-0.3.7"), { recursive: true });
    await copyFile(executable("juiced-test-worker"), resolve(release, "ort-server-0.3.7/ort-server"));
    await writeFile(resolve(release, "ort-server-0.3.7/LICENSE"), "MIT fixture\n");
    const ortPacker = Bun.spawn(["tar", "--format=gnu", "-C", release, "-czf", resolve(release, ortRelease.asset), "ort-server-0.3.7"], { stdout: "inherit", stderr: "inherit" });
    if (await ortPacker.exited !== 0) throw new Error("The ONNX Runtime release fixture could not be packed.");
    ortArchive = new Uint8Array(await Bun.file(resolve(release, ortRelease.asset)).arrayBuffer());
    await rm(release, { recursive: true });
  }
source = Bun.serve({
  hostname: "127.0.0.1", port: Number(process.env.JUICED_SOURCE_PORT ?? 13308),
  fetch(request) {
    const url = new URL(request.url);
    if (url.pathname === "/fixture.json") return Response.json({ model_file: resolve(directory, "fixture.gguf"), source_url: `${url.origin}/weights.gguf`, upload_file: resolve(directory, "Upload-Sample-Q8_0.gguf"), import_root: resolve(directory, "import"), cache_hub: resolve(directory, "huggingface", "hub"), library_root: resolve(directory, "library"), sha256, bytes: weights.length, command: executable("juiced-test-worker"), args: ["--port", "{port}", "--model", "{model}", "--events", resolve(directory, "managed.events"), "--slot-save-path", "{cache}"] });
    if (url.pathname === "/speech-fixture.wav") return new Response(Bun.file(resolve(root, "../../test/test_speech_de.wav")));
    const releaseResponse = engineRelease(url);
    if (releaseResponse) return releaseResponse;
    const hubResponse = hub(url);
    if (hubResponse) return hubResponse;
    if (!url.pathname.endsWith(".gguf")) return new Response("Not found", { status: 404 });
    if (url.pathname.startsWith("/unstable/") && !failed.has(url.pathname)) { failed.add(url.pathname); return new Response("Fixture transient failure", { status: 503 }); }
    const range = request.headers.get("Range");
    let position = range && request.headers.get("If-Range") === '"fixture-v1"' ? Number(/^bytes=(\d+)-$/.exec(range)?.[1] ?? 0) : 0;
    if (!Number.isSafeInteger(position) || position < 0 || position >= weights.length) return new Response(null, { status: 416 });
    const start = position;
    let cancelled = false;
    const body = new ReadableStream<Uint8Array>({
      async pull(controller) {
        await Bun.sleep(35);
        if (cancelled) return;
        const end = Math.min(position + 4096, weights.length);
        controller.enqueue(weights.subarray(position, end)); position = end;
        if (position === weights.length) controller.close();
      },
      cancel() { cancelled = true; },
    });
    return new Response(body, { status: start ? 206 : 200, headers: { "Content-Length": String(weights.length - start), ETag: '"fixture-v1"', ...(start ? { "Content-Range": `bytes ${start}-${weights.length - 1}/${weights.length}` } : {}) } });
  },
});
  const file = resolve(directory, "fixture.gguf");
  await writeFile(file, "Test fixture bytes. Not model weights.\n");
  const importRoot = resolve(directory, "import");
  await mkdir(resolve(importRoot, "nested"), { recursive: true });
  await writeFile(resolve(importRoot, "nested", "Local-Import-Q8_0.gguf"), metadataGguf(2048, 48 * 1024));
  await writeFile(resolve(directory, "Upload-Sample-Q8_0.gguf"), metadataGguf(2048, 40 * 1024));
  await mkdir(resolve(directory, "cache", "llama", "bin"), { recursive: true });
  await writeFile(resolve(directory, "cache", "llama", "bin", "llama-server"), new Uint8Array(96 * 1024));
  await mkdir(resolve(directory, "prompt-state"), { recursive: true });
  await writeFile(resolve(directory, "prompt-state", "slot-0.bin"), new Uint8Array(12 * 1024));
  const oldVolume = resolve(importRoot, "old-juiced");
  await mkdir(resolve(oldVolume, "config", "juiced"), { recursive: true });
  await writeFile(resolve(oldVolume, "config", "user_models.json"), JSON.stringify({ OldChat: { checkpoint: "acme/Old-Chat-GGUF:Q8_0", recipe: "llamacpp" } }));
  await writeFile(resolve(oldVolume, "config", "recipe_options.json"), JSON.stringify({ "user.OldChat": { ctx_size: 2048 } }));
  await writeFile(resolve(oldVolume, "config", "juiced", "state.json"), JSON.stringify({ version: 1, policies: { "user.OldChat": "on-demand" } }));
  await mkdir(resolve(oldVolume, "huggingface", "hub", "models--acme--Old-Chat-GGUF", "snapshots", "abc123"), { recursive: true });
  await writeFile(resolve(oldVolume, "huggingface", "hub", "models--acme--Old-Chat-GGUF", "snapshots", "abc123", "Old-Chat-Q8_0.gguf"), metadataGguf(2048, 36 * 1024));
  const ownedHub = resolve(directory, "huggingface", "hub");
  const libraryRoot = resolve(directory, "library");
  const libraryHub = resolve(libraryRoot, "huggingface", "hub");
  const qwen4b = await cacheRepo(ownedHub, "unsloth/Qwen3-4B-Instruct-2507-GGUF", "a6f3b0d5c1e2a4b7c9d8e0f1a2b3c4d5e6f70812", [["Qwen3-4B-Instruct-2507-Q4_K_M.gguf", 2.33 * gib], ["Qwen3-4B-Instruct-2507-Q8_0.gguf", 3.99 * gib]]);
  await cacheRepo(ownedHub, "ggml-org/gemma-3-4b-it-GGUF", "1b2c3d4e5f60718293a4b5c6d7e8f90a1b2c3d4e", [["gemma-3-4b-it-Q4_K_M.gguf", 2.32 * gib], ["mmproj-model-f16.gguf", 811 * mib]]);
  await cacheRepo(ownedHub, "unsloth/Qwen3-0.6B-GGUF", hubRevision, [["Qwen3-0.6B-Q8_0.gguf", 604 * mib]]);
  await cacheRepo(ownedHub, "hexgrad/Kokoro-82M", "c3d4e5f60718293a4b5c6d7e8f90a1b2c3d4e5f6", [["kokoro-v1.0.onnx", 310 * mib], ["voices-v1.0.bin", 27 * mib]]);
  const skyfall = await cacheRepo(libraryHub, "TheDrummer/Skyfall-31B-v4.2-GGUF", "d4e5f60718293a4b5c6d7e8f90a1b2c3d4e5f607", [["Skyfall-31B-v4.2-IQ4_NL.gguf", 16.6 * gib]]);
  await cacheRepo(libraryHub, "leejet/Z-Image-Turbo-GGUF", "e5f60718293a4b5c6d7e8f90a1b2c3d4e5f60718", [["z_image_turbo-Q8_0.gguf", 6.24 * gib], ["vae/ae.safetensors", 320 * mib], ["text_encoder/Qwen3-4B-Q8_0.gguf", 3.99 * gib]]);
  await cacheRepo(libraryHub, "unsloth/GLM-4.5-Air-GGUF", "f60718293a4b5c6d7e8f90a1b2c3d4e5f6071829", [["Q4_K_M/GLM-4.5-Air-Q4_K_M-00001-of-00002.gguf", 46.4 * gib], ["Q4_K_M/GLM-4.5-Air-Q4_K_M-00002-of-00002.gguf", 26.1 * gib]]);
  const asr = resolve(libraryRoot, "asr", "models");
  await mkdir(asr, { recursive: true });
  await writeFile(resolve(asr, "asr4all-l-gptq-Q8_0.gguf"), metadataGguf(4096, 1024));
  await truncate(resolve(asr, "asr4all-l-gptq-Q8_0.gguf"), Math.round(1.18 * gib));
  await chmod(libraryHub, 0o555);
  const model = (id: string, policy: string, resident: number, missing = false) => ({
    id, model_file: missing ? resolve(directory, "unavailable.gguf") : file,
    command: executable("juiced-test-worker"), args: ["--port", "{port}", "--model", "{model}", "--events", resolve(directory, `${id}.events`), "--slot-save-path", "{cache}", "--report-timings"],
    resident_bytes: resident * 1024 ** 3, max_inflight: 1, operations: ["chat/completions", "completions", "responses"], policy,
  });
  const config = resolve(directory, "config.json");
  if (process.env.JUICED_TEST_ORPHAN_FIXTURE === "1") {
    await mkdir(resolve(directory, "artifacts"));
    await writeFile(resolve(directory, "artifacts", `job-${"e".repeat(32)}.part`), "Unclaimed fixture staging bytes.\n");
  }
  const protectedRuntime = process.env.JUICED_TEST_PROTECTED_RUNTIME_FIXTURE === "1";
  const initialConfig = {
    schema_version: 1, artifact_source_policy: "allow-loopback-http", import_roots: [importRoot, libraryRoot], resident_budget_bytes: 6 * 1024 ** 3, startup_timeout_ms: 15000, request_timeout_ms: 15000,
    admission: protectedRuntime ? { memory: { mode: "protected", capacity_bytes: 512 * 1024 ** 2, essential_headroom_bytes: 0, maximum_sample_age_ms: 5000 } } : undefined,
    models: [model("fixture.chat-small-q4", "hot", 1), model("fixture.chat-long-context-q8", "on-demand", 2), model("fixture.missing-weights", "disabled", 1, true), { id: "fixture.speech-tiny", provider: "whisper-cpp", model_file: file, command: executable("juiced-test-worker"), args: ["--host", "127.0.0.1", "--port", "{port}", "--model", "{model}", "--events", resolve(directory, "speech.events"), "--inference-delay-ms", "750", "--no-gpu"], resident_bytes: 64 * 1024 ** 2, max_inflight: 1, operations: ["audio/transcriptions"], policy: "on-demand" }, { ...model("fixture.embed-tiny", "on-demand", 0), resident_bytes: 64 * 1024 ** 2, operations: ["embeddings"] }],
  };
  if (process.env.JUICED_FIXTURE_LIBRARY_MODELS === "1") initialConfig.models.push(
    { ...model("Qwen3-4B-Instruct-2507-Q8_0", "on-demand", 4), model_file: resolve(qwen4b, "Qwen3-4B-Instruct-2507-Q8_0.gguf") },
    { ...model("TheDrummer_Skyfall-31B-v4.2-GGUF-IQ4_NL", "disabled", 1), model_file: resolve(skyfall, "Skyfall-31B-v4.2-IQ4_NL.gguf") },
    { ...model("asr4all-l-gptq-Q8_0", "disabled", 1), model_file: resolve(asr, "asr4all-l-gptq-Q8_0.gguf"), operations: ["chat/completions"] },
  );
  if (process.env.JUICED_FIXTURE_GENERATION_MODELS === "1") initialConfig.models.push(
    { id: "fixture.music", provider: "acestep", model_file: file, command: executable("juiced-test-worker"), args: ["--models", "{model}", "--port", "{port}", "--events", resolve(directory, "music.events")], resident_bytes: 64 * 1024 ** 2, max_inflight: 1, operations: ["audio/generations"], policy: "on-demand" } as never,
    { id: "fixture.mesh", provider: "trellis", model_file: file, command: executable("juiced-test-worker"), args: ["--models", "{model}", "--port", "{port}", "--events", resolve(directory, "mesh.events")], resident_bytes: 64 * 1024 ** 2, max_inflight: 1, operations: ["3d/generations"], policy: "on-demand" } as never,
  );
  if (protectedRuntime) initialConfig.models = initialConfig.models.map((model) => ({ ...model, policy: "disabled" }));
  await writeFile(config, JSON.stringify(initialConfig, null, 2));
  if (process.platform !== "win32") {
    const runtimeSource = resolve(directory, "runtime-source");
    await mkdir(runtimeSource);
    const engine = resolve(runtimeSource, "engine");
    await copyFile(executable("juiced-test-worker"), engine);
    const engineFile = Bun.file(engine);
    const hasher = new Bun.CryptoHasher("sha256");
    const reader = engineFile.stream().getReader();
    try {
      while (true) {
        const chunk = await reader.read();
        if (chunk.done) break;
        hasher.update(chunk.value);
      }
    } finally { reader.releaseLock(); }
    const hash = hasher.digest("hex");
    const input = resolve(directory, "runtime-import.json");
    const bundle = { schema_version: 1, implementation_id: "llama-cpp", root: runtimeSource, entrypoint: "engine", library_directories: [], files: [{ path: "engine", sha256: hash, role: "executable" }] };
    const installed: string[] = [];
    for (const revision of ["fixture-runtime-first", "fixture-runtime-second"]) {
      await writeFile(input, JSON.stringify({ schema_version: 1, bundle, files: [{ path: "engine", bytes: engineFile.size }], provenance: { builder_id: "browser-fixture", source_revision: "synthetic-worker", build_revision: revision, declared_licenses: ["unknown"], declared_sbom: null }, declared_target: { operating_system: process.platform === "darwin" ? "macos" : "linux", architecture: process.arch, abi: "unknown", compute_paths: ["cpu"] } }));
      const importer = Bun.spawn([executable("lemond"), "--config", config, "--import-runtime-bundle", input], { cwd: root, stdout: "pipe", stderr: "pipe", env: { ...process.env, LEMONADE_API_KEY: "", LEMONADE_ADMIN_API_KEY: "" } });
      const [output, errors, code] = await Promise.all([new Response(importer.stdout).text(), new Response(importer.stderr).text(), importer.exited]);
      if (code !== 0) throw new Error(`Runtime fixture import failed: ${errors}`);
      installed.push(JSON.parse(output).package);
    }
    await rm(runtimeSource, { recursive: true });
    const models = initialConfig.models.map((model) => {
      const { command, args, provider = "external-http", ...fields } = model as typeof model & { provider?: string };
      const implementation = ({ acestep: "acestep-cpp", trellis: "trellis-cpp" } as Record<string, string>)[provider] ?? provider;
      return { ...fields, execution_profile: { schema_version: 1, adapter: provider, implementation_id: provider === "external-http" ? null : implementation, command, args, options: {}, placement: "unresolved" } };
    });
    const managed = { ...models[1]!, id: "fixture.managed-runtime", policy: "disabled", resident_bytes: 64 * 1024 ** 2, execution_profile: { schema_version: 1, adapter: "llama-cpp", implementation_id: "llama-cpp", runtime_package: installed[0], args: ["--host", "127.0.0.1", "--port", "{port}", "--model", "{model}", "--parallel", "1", "--ctx-size", "256", "--n-gpu-layers", "0", "--slot-save-path", "{cache}"], options: {}, placement: "unresolved" } };
    await writeFile(config, JSON.stringify({ ...initialConfig, schema_version: 2, models: [...models, managed] }, null, 2));
  }
  const { HF_TOKEN: _token, ...environment } = process.env;
  const child = Bun.spawn([executable("lemond"), "--config", config, "--listen", `127.0.0.1:${process.env.JUICED_PREVIEW_PORT ?? 13306}`, "--ui", resolve(root, "ui/dist")], {
    cwd: root, stdout: "inherit", stderr: "inherit", env: { ...environment, LEMONADE_API_KEY: "", LEMONADE_ADMIN_API_KEY: "", JUICED_HUGGINGFACE_URL: `http://127.0.0.1:${source!.port}`, JUICED_TEST_ENGINE_RELEASES: `http://127.0.0.1:${source!.port}`, JUICED_TEST_HARDWARE_ROOT: hardware },
  });
  const stop = () => child.kill("SIGTERM");
  process.once("SIGINT", stop); process.once("SIGTERM", stop);
  console.log("Synthetic subprocess fixture. Responses are not model inference. State is temporary.");
  console.log(`Isolated configuration: ${config}`);
  await child.exited;
  process.removeListener("SIGINT", stop); process.removeListener("SIGTERM", stop);
 } finally {
  source?.stop(true);
  async function writable(path: string) {
    if (!(await lstat(path)).isDirectory()) return;
    await chmod(path, 0o700);
    for (const entry of await readdir(path, { withFileTypes: true })) if (entry.isDirectory()) await writable(resolve(path, entry.name));
  }
  await chmod(resolve(directory, "library", "huggingface", "hub"), 0o755).catch(() => undefined);
  const packages = resolve(directory, "runtime-packages");
  if (await Bun.file(resolve(packages, "packages.json")).exists()) await writable(packages);
  await rm(directory, { recursive: true, force: true });
}
