import { chmod, copyFile, lstat, mkdir, mkdtemp, readdir, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { resolve } from "node:path";

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
let source: ReturnType<typeof Bun.serve> | undefined;
try {
source = Bun.serve({
  hostname: "127.0.0.1", port: Number(process.env.JUICED_SOURCE_PORT ?? 13308),
  fetch(request) {
    const url = new URL(request.url);
    if (url.pathname === "/fixture.json") return Response.json({ model_file: resolve(directory, "fixture.gguf"), source_url: `${url.origin}/weights.gguf`, sha256, bytes: weights.length, command: executable("juiced-test-worker"), args: ["--port", "{port}", "--model", "{model}", "--events", resolve(directory, "managed.events"), "--slot-save-path", "{cache}"] });
    if (url.pathname === "/speech-fixture.wav") return new Response(Bun.file(resolve(root, "../../test/test_speech_de.wav")));
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
  const model = (id: string, policy: string, resident: number, missing = false) => ({
    id, model_file: missing ? resolve(directory, "unavailable.gguf") : file,
    command: executable("juiced-test-worker"), args: ["--port", "{port}", "--model", "{model}", "--events", resolve(directory, `${id}.events`), "--slot-save-path", "{cache}"],
    resident_bytes: resident * 1024 ** 3, max_inflight: 1, operations: ["chat/completions", "completions", "responses"], policy,
  });
  const config = resolve(directory, "config.json");
  if (process.env.JUICED_TEST_ORPHAN_FIXTURE === "1") {
    await mkdir(resolve(directory, "artifacts"));
    await writeFile(resolve(directory, "artifacts", `job-${"e".repeat(32)}.part`), "Unclaimed fixture staging bytes.\n");
  }
  const protectedRuntime = process.env.JUICED_TEST_PROTECTED_RUNTIME_FIXTURE === "1";
  const initialConfig = {
    schema_version: 1, artifact_source_policy: "allow-loopback-http", resident_budget_bytes: 6 * 1024 ** 3, startup_timeout_ms: 15000, request_timeout_ms: 15000,
    admission: protectedRuntime ? { memory: { mode: "protected", capacity_bytes: 512 * 1024 ** 2, essential_headroom_bytes: 0, maximum_sample_age_ms: 5000 } } : undefined,
    models: [model("fixture.chat-small-q4", "hot", 1), model("fixture.chat-long-context-q8", "on-demand", 2), model("fixture.missing-weights", "disabled", 1, true), { id: "fixture.speech-tiny", provider: "whisper-cpp", model_file: file, command: executable("juiced-test-worker"), args: ["--host", "127.0.0.1", "--port", "{port}", "--model", "{model}", "--events", resolve(directory, "speech.events"), "--inference-delay-ms", "750", "--no-gpu"], resident_bytes: 64 * 1024 ** 2, max_inflight: 1, operations: ["audio/transcriptions"], policy: "on-demand" }],
  };
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
      return { ...fields, execution_profile: { schema_version: 1, adapter: provider, implementation_id: provider === "external-http" ? null : provider, command, args, options: {}, placement: "unresolved" } };
    });
    const managed = { ...models[1]!, id: "fixture.managed-runtime", policy: "disabled", resident_bytes: 64 * 1024 ** 2, execution_profile: { schema_version: 1, adapter: "llama-cpp", implementation_id: "llama-cpp", runtime_package: installed[0], args: ["--host", "127.0.0.1", "--port", "{port}", "--model", "{model}", "--parallel", "1", "--ctx-size", "256", "--n-gpu-layers", "0", "--slot-save-path", "{cache}"], options: {}, placement: "unresolved" } };
    await writeFile(config, JSON.stringify({ ...initialConfig, schema_version: 2, models: [...models, managed] }, null, 2));
  }
  const child = Bun.spawn([executable("lemond"), "--config", config, "--listen", `127.0.0.1:${process.env.JUICED_PREVIEW_PORT ?? 13306}`, "--ui", resolve(root, "ui/dist")], {
    cwd: root, stdout: "inherit", stderr: "inherit", env: { ...process.env, LEMONADE_API_KEY: "", LEMONADE_ADMIN_API_KEY: "" },
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
  const packages = resolve(directory, "runtime-packages");
  if (await Bun.file(resolve(packages, "packages.json")).exists()) await writable(packages);
  await rm(directory, { recursive: true, force: true });
}
