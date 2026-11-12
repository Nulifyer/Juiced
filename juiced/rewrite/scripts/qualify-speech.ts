import { createHash, randomUUID } from "node:crypto";
import { createReadStream } from "node:fs";
import { lstat, mkdir, mkdtemp, open, readFile, readdir, realpath, rename, rm, writeFile } from "node:fs/promises";
import { cpus, homedir, release, tmpdir } from "node:os";
import { basename, dirname, relative, resolve, sep } from "node:path";
import { createGunzip } from "node:zlib";

const root = resolve(import.meta.dir, "..");
const repository = resolve(root, "../..");
const cache = resolve(process.env.JUICED_WHISPER_CACHE ?? resolve(homedir(), ".cache/juiced-rewrite-whisper"));
const output = resolve(root, "local/verification/speech");
const bundleName = "whisper-v1.8.4-linux-cpu-x86_64";
const archive = { name: `${bundleName}.tar.gz`, url: "https://github.com/lemonade-sdk/whisper.cpp-rocm/releases/download/v1.8.4/whisper-v1.8.4-linux-cpu-x86_64.tar.gz", bytes: 4365860, sha256: "88df3f571d58871cd3f43e4a028df5dfe1463b24f8fd7df5dc4b7a2fb068ea0d" };
const modelAsset = { name: "ggml-tiny.bin", url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/5359861c739e955e79d9a303bcbc70fb988958b1/ggml-tiny.bin", bytes: 77691713, sha256: "be07e048e1e599ad46341c8d2a135645097a538221678b7acdd1b1919c6e1b21" };
const fixturePath = resolve(repository, "test/test_speech_de.wav");
const fixtureSha = "1f010435b3ed4a2776cc1739ae5b0e524c951c9312c2a4922893b00a15bde633";
const engine = resolve(cache, bundleName, "whisper-server");
const model = resolve(cache, modelAsset.name);
const daemon = resolve(root, "target/debug/lemond");
const runId = randomUUID();
const receiptPath = resolve(output, `speech-${runId}.json`);
const args = new Set(process.argv.slice(2));
for (const arg of args) if (!["--prepare-only", "--direct-only", "--no-build", "--help"].includes(arg)) throw new Error("Unknown qualification option. Use --help.");
if (args.has("--help")) { console.log("bun scripts/qualify-speech.ts [--prepare-only | --direct-only] [--no-build]\nPinned Linux x64 CPU bundle, multilingual tiny model and checked-in WAV.\nDefault runs three direct and three daemon multipart requests, unload and cold restart.\nNo transcript words are printed or retained. Receipts: local/verification/speech/.\nJUICED_WHISPER_CACHE optionally selects the task-owned asset cache."); process.exit(0); }
if (args.has("--prepare-only") && args.has("--direct-only")) throw new Error("Choose one qualification mode.");
const mode = args.has("--prepare-only") ? "asset-preparation" : args.has("--direct-only") ? "direct-engine" : "direct-and-daemon";
if (process.platform !== "linux" || process.arch !== "x64") throw new Error("This selected speech bundle requires Linux x64; other platforms are not qualified here.");

const hashBytes = (bytes: Uint8Array | string) => createHash("sha256").update(bytes).digest("hex");
async function hashFile(path: string) {
  const metadata = await lstat(path);
  if (!metadata.isFile() || metadata.isSymbolicLink()) throw new Error("A selected asset is not a regular file.");
  const digest = createHash("sha256"); let bytes = 0;
  for await (const chunk of Bun.file(path).stream()) { digest.update(chunk); bytes += chunk.byteLength; }
  if (bytes !== metadata.size) throw new Error("A selected asset changed while being hashed.");
  return { bytes, sha256: digest.digest("hex") };
}
async function ownedDirectory(path: string) {
  await mkdir(path, { recursive: true, mode: 0o700 });
  const metadata = await lstat(path);
  if (!metadata.isDirectory() || metadata.isSymbolicLink()) throw new Error("A qualification directory is not an owned regular directory.");
}
async function prepareAsset(asset: typeof archive) {
  const path = resolve(cache, asset.name);
  if (await Bun.file(path).exists()) {
    const identity = await hashFile(path);
    if (identity.bytes !== asset.bytes || identity.sha256 !== asset.sha256) throw new Error("An existing qualification asset has a different identity. No repair was authorized.");
    return path;
  }
  const temporary = resolve(cache, `.${asset.name}.${randomUUID()}.part`);
  const file = await open(temporary, "wx", 0o600);
  try {
    const response = await fetch(asset.url, { redirect: "follow", signal: AbortSignal.timeout(120_000) });
    if (!response.ok || !response.body) throw new Error("A pinned asset download was not successful.");
    const digest = createHash("sha256"); let bytes = 0;
    for await (const chunk of response.body) {
      bytes += chunk.byteLength;
      if (bytes > asset.bytes) throw new Error("A pinned asset exceeded its expected size.");
      digest.update(chunk); let offset = 0;
      while (offset < chunk.byteLength) { const written = await file.write(chunk, offset, chunk.byteLength - offset); if (!written.bytesWritten) throw new Error("An asset write did not progress."); offset += written.bytesWritten; }
    }
    if (bytes !== asset.bytes || digest.digest("hex") !== asset.sha256) throw new Error("Pinned asset size or SHA256 validation failed.");
    await file.sync(); await file.close(); await rename(temporary, path);
    return path;
  } finally { await file.close().catch(() => {}); await rm(temporary, { force: true }); }
}

type ArchiveEntry = { path: string; bytes: number; mode: number; directory: boolean; data: Buffer; sha256: string };
async function inspectArchive(path: string): Promise<ArchiveEntry[]> {
  const chunks: Buffer[] = []; let length = 0;
  for await (const chunk of createReadStream(path).pipe(createGunzip())) {
    length += chunk.length;
    if (length > 64 * 1024 * 1024) throw new Error("Archive expansion exceeds the qualification limit.");
    chunks.push(chunk);
  }
  const tar = Buffer.concat(chunks, length); const entries: ArchiveEntry[] = []; const names = new Set<string>();
  const string = (bytes: Buffer) => bytes.subarray(0, bytes.indexOf(0) < 0 ? bytes.length : bytes.indexOf(0)).toString("utf8");
  const octal = (bytes: Buffer) => { const value = string(bytes).trim(); if (!/^[0-7]+$/.test(value)) throw new Error("Unsupported archive numeric encoding."); return Number.parseInt(value, 8); };
  for (let position = 0; position + 512 <= tar.length;) {
    const header = tar.subarray(position, position + 512); position += 512;
    if (header.every((byte) => byte === 0)) break;
    const checksum = header.reduce((sum, byte, index) => sum + (index >= 148 && index < 156 ? 32 : byte), 0);
    if (checksum !== octal(header.subarray(148, 156))) throw new Error("Archive header checksum failed.");
    const prefix = string(header.subarray(345, 500)); const path = (prefix ? `${prefix}/` : "") + string(header.subarray(0, 100));
    const components = path.replace(/\/$/, "").split("/"); const bytes = octal(header.subarray(124, 136));
    const type = header[156]; const directory = type === 53;
    if (entries.length >= 128 || !/^[a-zA-Z0-9._\/-]+$/.test(path) || path.startsWith("/") || components.some((part) => !part || part === "." || part === "..") || components[0] !== bundleName || names.has(path) || ![0, 48, 53].includes(type!) || bytes > 16 * 1024 * 1024 || (directory && bytes !== 0) || position + bytes > tar.length) throw new Error("Archive entry exceeds bounds or contains an unsupported path, link or file type.");
    names.add(path); const data = tar.subarray(position, position + bytes);
    entries.push({ path, bytes, mode: octal(header.subarray(100, 108)), directory, data, sha256: hashBytes(data) });
    position += Math.ceil(bytes / 512) * 512;
  }
  if (!entries.some((entry) => entry.path === `${bundleName}/whisper-server`) || !entries.some((entry) => entry.path === `${bundleName}/libwhisper.so.1`)) throw new Error("The pinned archive lacks the selected engine or its companion library.");
  return entries;
}
async function inventoryDirectory(path: string, prefix = ""): Promise<string[]> {
  const files: string[] = [];
  for (const item of await readdir(resolve(path, prefix), { withFileTypes: true })) {
    const child = prefix ? `${prefix}/${item.name}` : item.name;
    if (item.isSymbolicLink() || (!item.isDirectory() && !item.isFile())) throw new Error("The extracted bundle contains a link or unsupported entry.");
    if (item.isDirectory()) files.push(...await inventoryDirectory(path, child)); else files.push(child);
    if (files.length > 128) throw new Error("The extracted bundle inventory exceeds the qualification limit.");
  }
  return files;
}
async function prepareBundle(entries: ArchiveEntry[]) {
  const target = resolve(cache, bundleName);
  if (!await Bun.file(engine).exists()) {
    if (await lstat(target).catch(() => null)) throw new Error("An incomplete bundle already exists. Inspection does not authorize repair.");
    const temporary = await mkdtemp(resolve(cache, ".speech-extract-"));
    try {
      for (const entry of entries) {
        const path = resolve(temporary, entry.path);
        if (!path.startsWith(temporary + sep)) throw new Error("Archive containment failed.");
        if (entry.directory) await mkdir(path, { recursive: true, mode: 0o700 });
        else { await mkdir(dirname(path), { recursive: true, mode: 0o700 }); const file = await open(path, "wx", entry.mode & 0o111 ? 0o700 : 0o600); try { await file.writeFile(entry.data); await file.sync(); } finally { await file.close(); } }
      }
      await rename(resolve(temporary, bundleName), target);
    } finally { await rm(temporary, { recursive: true, force: true }); }
  }
  if ((await lstat(target)).isSymbolicLink()) throw new Error("The bundle root must not be a symlink.");
  const expected = entries.filter((entry) => !entry.directory).map((entry) => ({ path: entry.path.slice(bundleName.length + 1), sha256: entry.sha256, bytes: entry.bytes, role: /\.so(?:\.|$)/.test(entry.path) ? "library" : entry.mode & 0o111 ? "executable" : "resource" }));
  const actual = await inventoryDirectory(target);
  if (actual.length !== expected.length || actual.some((path) => !expected.some((entry) => entry.path === path))) throw new Error("The bundle has missing or unlisted files.");
  for (const entry of expected) { const identity = await hashFile(resolve(target, entry.path)); if (identity.bytes !== entry.bytes || identity.sha256 !== entry.sha256) throw new Error("An extracted package file differs from the verified archive."); }
  return { schema_version: 1, implementation_id: "whisper-cpp", root: await realpath(target), entrypoint: "whisper-server", library_directories: ["."], files: expected.map(({ path, sha256, role }) => ({ path, sha256, role })) };
}

const inherited = Object.fromEntries(Object.entries(process.env).filter(([name]) => !name.startsWith("LD_") && !name.startsWith("WHISPER_") && !name.startsWith("LEMONADE_") && !name.endsWith("_API_KEY") && !name.endsWith("_TOKEN") && name !== "JUICED_CPU_PROFILE"));
const baseEnv = { ...inherited, LEMONADE_API_KEY: "", LEMONADE_ADMIN_API_KEY: "", JUICED_IO_THREADS: "2", HF_HUB_OFFLINE: "1", OMP_NUM_THREADS: "2", OPENBLAS_NUM_THREADS: "2" };
const children = new Set<Managed>(); let interrupted = false;
type Managed = ReturnType<typeof launch>;
function launch(name: string, argv: string[], cwd: string, env: Record<string, string | undefined>) {
  const child = Bun.spawn(argv, { cwd, env, stdin: "ignore", stdout: "pipe", stderr: "pipe" });
  const stdout = { bytes: 0, hash: createHash("sha256"), tail: "" }; const stderr = { bytes: 0, hash: createHash("sha256"), tail: "" };
  async function drain(stream: ReadableStream<Uint8Array>, log: typeof stdout) { for await (const chunk of stream) { log.bytes += chunk.byteLength; log.hash.update(chunk); if (name === "daemon") log.tail = (log.tail + new TextDecoder().decode(chunk)).slice(-4096); } }
  const logs = Promise.all([drain(child.stdout, stdout), drain(child.stderr, stderr)]);
  const result = { name, child, stdout, stderr, logs }; children.add(result); return result;
}
async function exited(child: Bun.Subprocess, timeout: number) {
  let timer: ReturnType<typeof setTimeout> | undefined;
  try { return await Promise.race([child.exited.then(() => true), new Promise<boolean>((resolve) => { timer = setTimeout(() => resolve(false), timeout); })]); }
  finally { if (timer) clearTimeout(timer); }
}
async function stop(child: Managed) {
  let forced = false;
  if (child.child.exitCode === null) child.child.kill("SIGTERM");
  if (!await exited(child.child, 10_000)) { forced = true; child.child.kill("SIGKILL"); }
  if (!await exited(child.child, 2000)) throw new Error("An owned qualification process did not exit.");
  await child.logs; children.delete(child);
  return { name: child.name, pid: child.child.pid, exit_code: child.child.exitCode, forced_kill: forced, stdout_bytes: child.stdout.bytes, stderr_bytes: child.stderr.bytes, stdout_sha256: child.stdout.hash.digest("hex"), stderr_sha256: child.stderr.hash.digest("hex"), content_retained: false };
}
function cancel() { interrupted = true; for (const child of children) if (child.child.exitCode === null) child.child.kill("SIGTERM"); }
process.once("SIGINT", cancel); process.once("SIGTERM", cancel);
function port() { const server = Bun.serve({ hostname: "127.0.0.1", port: 0, fetch: () => new Response() }); const port = server.port!; server.stop(true); return port; }
async function ready(child: Managed, url: string) {
  const deadline = Date.now() + 30_000;
  while (!interrupted && child.child.exitCode === null && Date.now() < deadline) { try { const response = await fetch(url, { signal: AbortSignal.timeout(500), redirect: "error" }); await response.arrayBuffer(); if (response.ok) return; } catch {} await Bun.sleep(50); }
  throw new Error("The selected owned process did not become ready within its deadline.");
}
async function boundedResponse(response: Response, maximum = 2 * 1024 * 1024) {
  if (!response.body) throw new Error("The HTTP response had no readable body.");
  const parts: Uint8Array[] = []; let length = 0;
  for await (const chunk of response.body) { length += chunk.byteLength; if (length > maximum) throw new Error("The HTTP response exceeded its bound."); parts.push(chunk); }
  const bytes = Buffer.concat(parts, length);
  let value: any; try { value = JSON.parse(bytes.toString("utf8")); } catch { throw new Error(`The speech pipeline returned unreadable JSON (HTTP ${response.status}).`); }
  if (!response.ok) { const code = typeof value?.error?.code === "string" && /^[a-z0-9_]{1,80}$/.test(value.error.code) ? value.error.code : "request_rejected"; throw new Error(`Speech pipeline request failed: HTTP ${response.status}, ${code}.`); }
  return { bytes, value };
}
async function json(origin: string, path: string, body?: unknown) {
  const response = await fetch(origin + path, { method: body === undefined ? "GET" : "POST", headers: body === undefined ? {} : { "Content-Type": "application/json" }, body: body === undefined ? undefined : JSON.stringify(body), signal: AbortSignal.timeout(40_000), redirect: "error" });
  return (await boundedResponse(response)).value;
}
async function transcribe(origin: string, path: string, managed: boolean) {
  if (interrupted) throw new Error("Speech qualification was interrupted.");
  const form = new FormData(); form.set("file", Bun.file(fixturePath), "test_speech_de.wav"); form.set("response_format", "json"); form.set("language", "de"); if (managed) form.set("model", "speech-qualification");
  const start = performance.now(); const response = await fetch(origin + path, { method: "POST", body: form, signal: AbortSignal.timeout(40_000), redirect: "error" });
  const { value, bytes } = await boundedResponse(response);
  if (!value || typeof value !== "object" || typeof value.text !== "string" || !response.headers.get("content-type")?.includes("application/json")) throw new Error("The speech response did not contain JSON text:string.");
  return { http_status: response.status, content_type: response.headers.get("content-type"), response_bytes: bytes.byteLength, response_sha256: hashBytes(bytes), text_type: "string", text_bytes: Buffer.byteLength(value.text, "utf8"), transcript_retained: false, elapsed_ms: performance.now() - start };
}
async function processIdentity(pid: number) {
  if (await realpath(`/proc/${pid}/exe`) !== await realpath(engine)) throw new Error("The actual speech worker executable differs from the selected engine.");
  const stat = await readFile(`/proc/${pid}/stat`, "utf8");
  const start = stat.slice(stat.lastIndexOf(")") + 1).trim().split(/\s+/)[19]!;
  const maps = await readFile(`/proc/${pid}/maps`, "utf8"); if (maps.length > 2 * 1024 * 1024) throw new Error("Worker mapping observation exceeded its bound.");
  const paths = new Set<string>();
  for (const line of maps.split("\n")) { const path = line.match(/\s(\/[^\n]+)$/)?.[1]; if (path?.startsWith(resolve(cache, bundleName) + sep)) paths.add(path); }
  const mappings = [];
  for (const path of [...paths].sort()) { if (path.endsWith(" (deleted)")) throw new Error("A selected worker mapping was deleted."); mappings.push({ bundle_path: relative(resolve(cache, bundleName), path), ...await hashFile(path) }); }
  return { pid, start_ticks: start, executable: "{bundle}/whisper-server", mappings };
}
async function gone(identity: { pid: number; start_ticks: string }) {
  try { const stat = await readFile(`/proc/${identity.pid}/stat`, "utf8"); return stat.slice(stat.lastIndexOf(")") + 1).trim().split(/\s+/)[19] !== identity.start_ticks; }
  catch (error) { if ((error as NodeJS.ErrnoException).code === "ENOENT") return true; throw error; }
}

await ownedDirectory(cache); await ownedDirectory(output);
const receipt: any = { schema_version: 1, run_id: runId, started_at: new Date().toISOString(), mode, scope: mode === "asset-preparation" ? "Pinned asset identities, bounded extraction and native help only; no speech request was run" : mode === "direct-engine" ? "Pinned direct Linux x64 CPU Whisper transport only, not transcription quality or general engine qualification" : "Pinned Linux x64 CPU Whisper direct and daemon transport/lifecycle pipeline, not transcription quality or general engine qualification", accepted: false, transcript_retained: false, qualification_receipt_promoted: false, cleanup: [] };
let temporary: string | undefined; let stage = "asset_preparation"; let failure: unknown;
try {
  const [archivePath] = await Promise.all([prepareAsset(archive), prepareAsset(modelAsset)]);
  const fixture = await hashFile(fixturePath); if (fixture.sha256 !== fixtureSha) throw new Error("The checked-in speech fixture digest differs.");
  const entries = await inspectArchive(archivePath!); const bundle = await prepareBundle(entries);
  const released = Buffer.alloc(0); for (const entry of entries) entry.data = released;
  const flags = (await readFile("/proc/cpuinfo", "utf8")).match(/^flags\s*:\s*(.*)$/m)?.[1]?.split(/\s+/) ?? [];
  if (!flags.includes("avx2")) throw new Error("This selected native CPU archive requires an AVX2 host for the qualification run.");
  receipt.host = { platform: process.platform, architecture: process.arch, kernel: release(), logical_cpus: cpus().length, avx2_observed: true, bun: Bun.version };
  receipt.identities = { archive, model: modelAsset, fixture: { path: "test/test_speech_de.wav", ...fixture }, engine: { path: "{bundle}/whisper-server", ...await hashFile(engine) }, bundle_files: bundle.files, script_sha256: (await hashFile(import.meta.path)).sha256 };
  receipt.bundle = { schema_version: bundle.schema_version, implementation_id: bundle.implementation_id, entrypoint: bundle.entrypoint, library_directories: bundle.library_directories, file_count: bundle.files.length, source: "verified external archive extraction", immutable_promotion_qualified: false };
  receipt.upstream_license_observations = { engine: { declared: "MIT", source: "https://raw.githubusercontent.com/lemonade-sdk/whisper.cpp-rocm/v1.8.4/LICENSE" }, model: { declared: "MIT model-card metadata", source: "https://huggingface.co/ggerganov/whisper.cpp/raw/5359861c739e955e79d9a303bcbc70fb988958b1/README.md" }, archive_license_file_present: entries.some((entry) => /license/i.test(basename(entry.path))), complete_dependency_license_qualification: false };
  await writeFile(resolve(cache, "speech-runtime-bundle.json"), JSON.stringify(bundle, null, 2) + "\n", { mode: 0o600 });
  stage = "help_observation";
  const help = launch("help", [engine, "--help"], cache, { ...baseEnv, LD_LIBRARY_PATH: bundle.root });
  const timer = setTimeout(() => help.child.kill("SIGKILL"), 5000);
  await help.child.exited; clearTimeout(timer);
  const helpResult = await stop(help); if (helpResult.exit_code !== 0) throw new Error("The pinned engine --help did not exit successfully with bundle-local libraries.");
  receipt.help = { ...helpResult, observed_flags: ["--host", "--port", "--model", "--threads", "--no-gpu", "--public"], version_flag_advertised: false, source: "verified v1.8.4 archive; actual --help" };
  if (!args.has("--prepare-only")) {
    temporary = await mkdtemp(resolve(tmpdir(), "juiced-speech-qualification-")); await mkdir(resolve(temporary, "public"));
    stage = "direct_engine";
    const origin = `http://127.0.0.1:${port()}`;
    const direct = launch("direct-whisper", [engine, "--host", "127.0.0.1", "--port", new URL(origin).port, "--model", model, "--no-gpu", "--threads", "2", "--public", resolve(temporary, "public")], temporary, { ...baseEnv, LD_LIBRARY_PATH: bundle.root });
    await ready(direct, origin + "/health");
    receipt.direct_worker = await processIdentity(direct.child.pid);
    receipt.direct = [];
    for (let run = 0; run < 3; run++) receipt.direct.push(await transcribe(origin, "/inference", false));
    receipt.cleanup.push(await stop(direct));
    if (!await gone(receipt.direct_worker)) throw new Error("The owned direct speech process remained after cleanup.");
    if (!args.has("--direct-only")) {
      stage = "daemon_build";
      if (!args.has("--no-build")) {
        const build = launch("build", ["cargo", "build", "--manifest-path", resolve(root, "Cargo.toml"), "--target-dir", resolve(root, "target"), "--locked", "--offline", "-p", "juiced-service", "--bin", "lemond"], root, baseEnv);
        if (!await exited(build.child, 300_000)) { build.child.kill("SIGKILL"); throw new Error("Daemon compilation exceeded its deadline."); }
        const result = await stop(build); receipt.cleanup.push(result); if (result.exit_code !== 0) throw new Error("The current Rust daemon did not compile.");
      }
      stage = "daemon_pipeline";
      const state = resolve(temporary, "daemon"); await mkdir(state);
      const config = { schema_version: 1, cache_enabled: false, resident_budget_bytes: 1024 ** 3, startup_timeout_ms: 30_000, request_timeout_ms: 40_000, models: [{ id: "speech-qualification", provider: "whisper-cpp", model_file: model, command: engine, runtime_bundle: bundle, args: ["--host", "127.0.0.1", "--port", "{port}", "--model", "{model}", "--threads", "2", "--no-gpu"], resident_bytes: 512 * 1024 ** 2, max_inflight: 1, operations: ["audio/transcriptions"], policy: "on-demand", revision: 0 }] };
      const path = resolve(state, "config.json"); await writeFile(path, JSON.stringify(config));
      receipt.daemon_identity = { ...await hashFile(daemon), config_sha256: hashBytes(JSON.stringify(config)), runtime_library_scope: "verified child bundle only; daemon LD_LIBRARY_PATH unset" };
      const manager = launch("daemon", [daemon, "--config", path, "--listen", "127.0.0.1:0", "--no-broadcast"], state, baseEnv);
      const deadline = Date.now() + 30_000; let managedOrigin: string | undefined;
      while (!interrupted && manager.child.exitCode === null && Date.now() < deadline) { const port = manager.stdout.tail.match(/listening on http:\/\/127\.0\.0\.1:(\d+)/)?.[1]; if (port) { managedOrigin = `http://127.0.0.1:${port}`; break; } await Bun.sleep(25); }
      if (!managedOrigin) throw new Error("The isolated daemon did not report its dynamically bound loopback origin.");
      await ready(manager, managedOrigin + "/v1/health");
      receipt.managed = [];
      for (let run = 0; run < 3; run++) receipt.managed.push(await transcribe(managedOrigin, "/v1/audio/transcriptions", true));
      const worker = (await json(managedOrigin, "/v1/juiced/catalog")).data.find((entry: any) => entry.id === "speech-qualification");
      if (!worker?.loaded || !Number.isSafeInteger(worker.pid)) throw new Error("The daemon did not publish a real loaded speech-worker PID.");
      receipt.managed_worker = await processIdentity(worker.pid);
      const records = (await json(managedOrigin, "/v1/juiced/requests")).data.filter((entry: any) => entry.model === "speech-qualification");
      if (records.length !== 3 || !records.every((entry: any) => entry.operation === "audio/transcriptions" && entry.outcome === "completed" && entry.status === 200)) throw new Error("Managed request metadata did not confirm the three successful speech transfers.");
      receipt.request_metadata = records;
      await json(managedOrigin, "/v1/unload", { model: "speech-qualification" });
      if (!await gone(receipt.managed_worker)) throw new Error("The unloaded speech-worker process remained alive.");
      const unloaded = (await json(managedOrigin, "/v1/juiced/catalog")).data.find((entry: any) => entry.id === "speech-qualification");
      if (unloaded.loaded || unloaded.pid !== null) throw new Error("The daemon did not confirm its unloaded state.");
      receipt.after_restart = await transcribe(managedOrigin, "/v1/audio/transcriptions", true);
      const restarted = (await json(managedOrigin, "/v1/juiced/catalog")).data.find((entry: any) => entry.id === "speech-qualification");
      receipt.restarted_worker = await processIdentity(restarted.pid);
      if (receipt.restarted_worker.pid === receipt.managed_worker.pid && receipt.restarted_worker.start_ticks === receipt.managed_worker.start_ticks) throw new Error("Cold restart did not replace the unloaded worker generation.");
      receipt.cleanup.push(await stop(manager));
      if (!await gone(receipt.restarted_worker)) throw new Error("The speech worker remained after daemon shutdown.");
    }
  }
  stage = "identity_recheck";
  await prepareBundle(entries);
  if ((await hashFile(model)).sha256 !== modelAsset.sha256 || (await hashFile(fixturePath)).sha256 !== fixtureSha) throw new Error("A selected model or audio fixture changed during qualification.");
  receipt.accepted = true;
} catch (error) { failure = error; receipt.failure = { stage, message: error instanceof Error ? error.message : "Speech qualification failed." }; }
finally {
  for (const child of [...children]) { try { receipt.cleanup.push(await stop(child)); } catch { receipt.cleanup.push({ name: child.name, cleanup_confirmed: false }); receipt.accepted = false; } }
  if (temporary) { await rm(temporary, { recursive: true, force: true }); receipt.temporary_state_removed = true; }
  receipt.finished_at = new Date().toISOString();
  const receiptTemp = receiptPath + ".tmp"; await writeFile(receiptTemp, JSON.stringify(receipt, null, 2) + "\n", { flag: "wx", mode: 0o600 }); await rename(receiptTemp, receiptPath);
  process.removeListener("SIGINT", cancel); process.removeListener("SIGTERM", cancel);
}
console.log(JSON.stringify({ accepted: receipt.accepted, mode, receipt: relative(root, receiptPath), direct_requests: receipt.direct?.length ?? 0, managed_requests: receipt.managed?.length ?? 0, restart_request: !!receipt.after_restart, transcript_retained: false }));
if (failure || !receipt.accepted) process.exitCode = 1;
