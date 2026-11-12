import { createHash, randomUUID } from "node:crypto";
import { copyFile, cp, mkdir, mkdtemp, readFile, readdir, readlink, realpath, rm, stat, writeFile } from "node:fs/promises";
import { cpus, loadavg, release } from "node:os";
import { dirname, resolve } from "node:path";

if (process.platform !== "linux") throw new Error("This inference profiler requires Linux /proc process accounting.");
const root = resolve(import.meta.dir, "..");
const output = resolve(root, process.env.JUICED_INFERENCE_PROFILE_OUT ?? "local/performance");
if (!process.env.JUICED_TEST_LLAMA_SERVER || !process.env.JUICED_TEST_GGUF) throw new Error("Set JUICED_TEST_LLAMA_SERVER and JUICED_TEST_GGUF to existing engine/model paths.");
const engine = await realpath(process.env.JUICED_TEST_LLAMA_SERVER);
const model = await realpath(process.env.JUICED_TEST_GGUF);
const nativeSource = process.env.JUICED_BASELINE_LEMOND ? await realpath(process.env.JUICED_BASELINE_LEMOND) : null;
const referenceRust = process.env.JUICED_REFERENCE_RUST_LEMOND ? await realpath(process.env.JUICED_REFERENCE_RUST_LEMOND) : null;
const nativeInhibitSetting = process.env.JUICED_BENCH_NATIVE_INHIBIT ?? "true";
if (!["true", "false"].includes(nativeInhibitSetting)) throw new Error("JUICED_BENCH_NATIVE_INHIBIT must be true or false.");
const nativeInhibitSuspend = nativeInhibitSetting === "true";
const rustInhibitSetting = process.env.JUICED_BENCH_RUST_INHIBIT ?? nativeInhibitSetting;
if (!["true", "false"].includes(rustInhibitSetting)) throw new Error("JUICED_BENCH_RUST_INHIBIT must be true or false.");
const rustInhibitSuspend = rustInhibitSetting === "true";
if (nativeSource && rustInhibitSuspend !== nativeInhibitSuspend) throw new Error("Native and Rust suspend-inhibition controls must match for the paired manager comparison.");
const crossoverSetting = process.env.JUICED_BENCH_INHIBIT_CROSSOVER ?? "false";
if (!["true", "false"].includes(crossoverSetting)) throw new Error("JUICED_BENCH_INHIBIT_CROSSOVER must be true or false.");
const inhibitCrossover = crossoverSetting === "true";
const runId = randomUUID();
const startedAt = new Date().toISOString();
const releaseManager = resolve(root, "target/release/lemond");
const profileManager = resolve(root, "target/profiling/lemond");
const benchmark = resolve(root, "target/release/bench-api");
const svg = resolve(output, "inference-manager-cpu-flamegraph.svg");
const comparisonPath = resolve(output, "inference-direct-versus-rust.json");
const profilePath = resolve(output, "inference-manager-cpu-profile.json");
const modelId = "rewrite-benchmark-stories15m";
const requestsPerRound = 300;
const pairedRounds = 5;
const profileRequests = 6000;
const concurrency = 1;
const ioThreads = 4;
const engineTemplate = ["-m", "{model}", "--ctx-size", "512", "--port", "{port}", "--jinja", "--metrics", "--host", "127.0.0.1", "--threads", "2", "--gpu-layers", "0", "--parallel", "1", "--slots", "--no-webui", "--no-warmup", "--alias", modelId];
const nativeArguments = "--host 127.0.0.1 --threads 2 --gpu-layers 0 --parallel 1 --slots --no-webui --no-warmup --alias " + modelId;
const body = JSON.stringify({ model: modelId, prompt: "Once upon a time, a small fox", max_tokens: 1, temperature: 0, stream: false, cache_prompt: false });
const hashBytes = (bytes: string | Uint8Array) => createHash("sha256").update(bytes).digest("hex");
async function hashFile(path: string): Promise<string> {
  const hash = createHash("sha256");
  for await (const chunk of Bun.file(path).stream()) hash.update(chunk);
  return hash.digest("hex");
}
async function sourceIdentity() {
  const files = new Set(["Cargo.toml", "Cargo.lock", "service/Cargo.toml", "rust-tests/Cargo.toml", "scripts/profile-inference.ts"]);
  for (const directory of ["service/src", "service/tests", "rust-tests/src"]) {
    for await (const file of new Bun.Glob("**/*.rs").scan({cwd: resolve(root,directory),onlyFiles:true})) files.add(`${directory}/${file}`);
  }
  const hash = createHash("sha256");
  for (const path of [...files].sort()) { hash.update(path); hash.update("\0"); hash.update(await readFile(resolve(root,path))); hash.update("\0"); }
  return { sha256: hash.digest("hex"), files: files.size };
}
const reservedRuntimeVariables = new Set(["LLAMA_API_KEY", "HF_TOKEN", "GGML_BACKEND_PATH", "WHISPER_ARG_DEVICE", "MODEL_ENDPOINT", "HF_ENDPOINT", "LLAMA_CACHE", "HF_HUB_CACHE", "HUGGINGFACE_HUB_CACHE", "HF_HOME", "HTTP_PROXY", "HTTPS_PROXY", "NO_PROXY", "LD_PRELOAD", "LD_AUDIT", "DYLD_INSERT_LIBRARIES", "ALL_PROXY"]);
const managerVariables = new Set(["INVOCATION_ID", "JOURNAL_STREAM", "NOTIFY_SOCKET", "WATCHDOG_PID", "WATCHDOG_USEC", "LISTEN_PID", "LISTEN_FDS", "LISTEN_FDNAMES", "SYSTEMD_EXEC_PID", "CREDENTIALS_DIRECTORY", "RUNTIME_DIRECTORY", "STATE_DIRECTORY", "CACHE_DIRECTORY", "LOGS_DIRECTORY", "CONFIGURATION_DIRECTORY"]);
function scrubbedVariable(name: string): boolean {
  const upper = name.toUpperCase();
  return upper.startsWith("LLAMA_") || upper.startsWith("LEMONADE_") || upper.startsWith("JUICED_") || reservedRuntimeVariables.has(upper) || managerVariables.has(upper);
}
const inherited = Object.fromEntries(Object.entries(process.env).filter(([name]) => !scrubbedVariable(name)));
const baseEnv: Record<string,string|undefined> = { ...inherited, LEMONADE_API_KEY: "", LEMONADE_ADMIN_API_KEY: "", JUICED_IO_THREADS: String(ioThreads), HF_HUB_OFFLINE: "1", OMP_NUM_THREADS: "2", OPENBLAS_NUM_THREADS: "2" };
const children = new Set<ReturnType<typeof launch>>();
const runtimeFiles = new Map<string, string>();
const commands = new Set<Bun.Subprocess>();
const uncertainManagerCleanup = new Set<string>();
let interrupted = false;

async function drain(stream: ReadableStream<Uint8Array>, log: { tail: string }) {
  const decoder = new TextDecoder();
  for await (const chunk of stream) log.tail = (log.tail + decoder.decode(chunk, { stream: true })).slice(-16_384);
}
function launch(name: string, argv: string[], cwd: string, env: Record<string, string | undefined>) {
  const child = Bun.spawn(argv, { cwd, env, stdin: "ignore", stdout: "pipe", stderr: "pipe" });
  const log = { tail: "" };
  const logs = Promise.all([drain(child.stdout, log), drain(child.stderr, log)]);
  const result = { name, argv, child, log, logs };
  children.add(result);
  return result;
}
type Managed = ReturnType<typeof launch>;
async function waitForExit(child: Bun.Subprocess, milliseconds: number): Promise<boolean> {
  let timer: ReturnType<typeof setTimeout> | undefined;
  try {
    return await Promise.race([child.exited.then(() => true), new Promise<boolean>(resolve => { timer = setTimeout(() => resolve(false), milliseconds); })]);
  } finally { if (timer) clearTimeout(timer); }
}
async function stop(process: Managed) {
  let escalated = false;
  if (process.child.exitCode === null) process.child.kill("SIGTERM");
  if (!await waitForExit(process.child, 10_000)) { escalated = true; process.child.kill("SIGKILL"); }
  if (!await waitForExit(process.child, 2_000)) throw new Error(`${process.name} did not terminate.`);
  await process.logs;
  children.delete(process);
  if (process.name !== "direct-engine" && (escalated || process.child.exitCode !== 0)) uncertainManagerCleanup.add(process.name);
  return { name: process.name, pid: process.child.pid, exit_code: process.child.exitCode, forced_kill: escalated };
}
function cancel() {
  interrupted = true;
  for (const process of children) if (process.child.exitCode === null) process.child.kill("SIGTERM");
  for (const command of commands) if (command.exitCode === null) command.kill("SIGTERM");
}
process.once("SIGINT", cancel);
process.once("SIGTERM", cancel);
async function command(argv: string[], timeout = 300_000): Promise<string> {
  const child = Bun.spawn(argv, { cwd: root, env: baseEnv, stdin: "ignore", stdout: "pipe", stderr: "inherit" });
  commands.add(child);
  const timer = setTimeout(() => { child.kill("SIGTERM"); setTimeout(() => { if (child.exitCode === null) child.kill("SIGKILL"); }, 1000); }, timeout);
  try {
    const stdout = await new Response(child.stdout).text();
    if (await child.exited !== 0 || interrupted) throw new Error(`Command failed: ${argv.join(" ")}`);
    return stdout.trim();
  } finally { clearTimeout(timer); commands.delete(child); if (child.exitCode === null) { child.kill("SIGKILL"); await child.exited; } }
}
function port(): number {
  const reservation = Bun.serve({ hostname: "127.0.0.1", port: 0, fetch: () => new Response() });
  const result = reservation.port!;
  reservation.stop(true);
  return result;
}
async function ready(server: Managed, url: string) {
  const deadline = Date.now() + 40_000;
  while (!interrupted && Date.now() < deadline && server.child.exitCode === null) {
    try {
      const response = await fetch(url, { signal: AbortSignal.timeout(500), redirect: "error" });
      await response.arrayBuffer();
      if (response.ok) return;
    } catch {}
    await Bun.sleep(50);
  }
  throw new Error(`${server.name} did not become ready. ${server.log.tail.slice(-2000)}`);
}
async function json(url: string): Promise<any> {
  const response = await fetch(url, { signal: AbortSignal.timeout(5000), redirect: "error" });
  if (!response.ok) throw new Error(`Discovery failed with HTTP ${response.status}.`);
  return response.json();
}
interface PowerSnapshot {
  schema_version: number; enabled: boolean; platform: string; active: number; held: boolean; acquiring: boolean;
  releasing: boolean; idle_grace: boolean; shutting_down: boolean; settled: boolean;
  last_error: string | null; attempts: number; acquisitions: number; releases: number;
}
async function powerSnapshot(origin: string): Promise<PowerSnapshot> {
  const state = await json(`${origin}/v1/juiced/state`);
  const power = state.resources?.power;
  if (state.schema_version !== 1 || state.implementation !== "rust-experiment" || !power || power.schema_version !== 1
    || !["active", "attempts", "acquisitions", "releases"].every(key => Number.isSafeInteger(power[key]) && power[key] >= 0)
    || !["enabled", "held", "acquiring", "releasing", "idle_grace", "shutting_down", "settled"].every(key => typeof power[key] === "boolean")
    || typeof power.platform !== "string" || !(power.last_error === null || typeof power.last_error === "string")
    || power.releases > power.acquisitions || power.acquisitions > power.attempts) throw new Error("The Rust power owner did not provide a valid bounded state observation.");
  if (power.enabled !== rustInhibitSuspend) throw new Error("The Rust suspend-inhibition setting does not match the selected control.");
  if (rustInhibitSuspend && (power.platform !== "linux-logind" || power.last_error !== null)) throw new Error("Rust did not confirm an available Linux suspend inhibitor; best-effort unavailability cannot qualify this comparison.");
  if (!rustInhibitSuspend && (power.held || power.acquiring || power.releasing || power.attempts !== 0 || power.acquisitions !== 0 || power.releases !== 0)) throw new Error("Disabled Rust suspend inhibition performed unexpected native work.");
  return power as PowerSnapshot;
}
async function observeWorkloadPower(origin: string, run: Bun.Subprocess) {
  const started = performance.now();
  const deadline = started + 5000;
  let polls = 0;
  while (!interrupted && performance.now() < deadline && run.exitCode === null && polls < 128) {
    polls++;
    const snapshot = await powerSnapshot(origin);
    if (run.exitCode === null && snapshot.active > 0 && snapshot.held && snapshot.acquisitions > 0 && !snapshot.shutting_down) {
      return { snapshot, polls, elapsed_ms: performance.now() - started,
        scope: "A snapshot showed an acknowledged inhibitor held during active Rust work while the benchmark process was running, including warmups. Bounded metadata queries may overlap early requests and count toward sampled manager CPU. This does not establish continuous coverage of every request." };
    }
    await Bun.sleep(2);
  }
  throw new Error("The Rust workload did not provide an observed active, OS-acknowledged suspend inhibitor.");
}
async function settledPower(origin: string) {
  const deadline = Date.now() + 5000;
  while (!interrupted && Date.now() < deadline) {
    const snapshot = await powerSnapshot(origin);
    if (snapshot.active === 0 && !snapshot.held && !snapshot.acquiring && !snapshot.releasing && snapshot.settled
      && snapshot.releases === snapshot.acquisitions && (!rustInhibitSuspend || snapshot.acquisitions > 0)) return snapshot;
    await Bun.sleep(20);
  }
  throw new Error("The Rust power owner did not confirm released, settled native resources before shutdown.");
}
async function stopRustManager(manager: {server: Managed; origin: string}) {
  const preShutdown = await settledPower(manager.origin);
  const stopped = await stop(manager.server);
  if (stopped.forced_kill || stopped.exit_code !== 0) throw new Error("The Rust manager did not confirm clean worker and power settlement at shutdown.");
  return { ...stopped, power: { configured_inhibit_suspend: rustInhibitSuspend, pre_shutdown_settled_snapshot: preShutdown,
    shutdown_settlement_confirmed_by_exit_code: true,
    scope: "The final HTTP snapshot was captured after workload idle release and before SIGTERM. Exit 0 then confirms the manager's shutdown settlement contract; no post-exit HTTP snapshot is claimed." } };
}
async function verifyPipeline(origin: string) {
  const response = await fetch(`${origin}/v1/completions`, { method: "POST", headers: { "content-type": "application/json" }, body, signal: AbortSignal.timeout(40_000), redirect: "error" });
  const bytes = await response.arrayBuffer();
  if (!response.ok) throw new Error(`Inference returned HTTP ${response.status}: ${new TextDecoder().decode(bytes).slice(0, 1000)}`);
  const value = JSON.parse(new TextDecoder().decode(bytes));
  if (value.usage?.completion_tokens !== 1 || value.usage?.prompt_tokens !== 10 || !Array.isArray(value.choices) || value.choices.length !== 1) throw new Error("The completed inference response did not confirm one generated token, ten prompt tokens and one choice.");
  return { status: response.status, response_bytes: bytes.byteLength, completion_tokens: value.usage.completion_tokens, prompt_tokens: value.usage.prompt_tokens };
}

interface ProcessSample { pid: number; start_ticks: string; cpu_ticks: number; rss_bytes: number; threads: number }
async function sample(pid: number): Promise<ProcessSample> {
  const [stat, status] = await Promise.all([readFile(`/proc/${pid}/stat`, "utf8"), readFile(`/proc/${pid}/status`, "utf8")]);
  const fields = stat.slice(stat.lastIndexOf(")") + 1).trim().split(/\s+/);
  const rss = status.match(/^VmRSS:\s+(\d+)\s+kB$/m)?.[1];
  const threads = status.match(/^Threads:\s+(\d+)$/m)?.[1];
  if (!rss || !threads || fields.length < 22) throw new Error("Process accounting was incomplete.");
  return { pid, start_ticks: fields[19]!, cpu_ticks: Number(fields[11]) + Number(fields[12]), rss_bytes: Number(rss) * 1024, threads: Number(threads) };
}
async function normalizedEngineArgv(pid: number) {
  const executable = await realpath(`/proc/${pid}/exe`);
  if (executable !== engine) throw new Error("The running engine does not match the selected executable.");
  const argv = (await readFile(`/proc/${pid}/cmdline`, "utf8")).split("\0").filter(Boolean);
  const actual = [...argv];
  argv[0] = "{engine}";
  let selectedPort: number | undefined;
  let modelArguments = 0;
  let portArguments = 0;
  for (let index = 1; index < argv.length; index++) {
    if (actual[index] === "--port") {
      selectedPort = Number(actual[index + 1]); portArguments++;
      argv[++index] = "{port}";
    } else if (["--model", "-m"].includes(actual[index]!)) {
      if (!actual[index + 1] || await realpath(actual[index + 1]!) !== model) throw new Error("The engine did not receive the selected model path.");
      modelArguments++; argv[index] = "--model"; argv[++index] = "{model}";
    }
  }
  if (modelArguments !== 1 || portArguments !== 1 || selectedPort === undefined || !Number.isInteger(selectedPort) || selectedPort < 1 || selectedPort > 65535) throw new Error("The selected worker model or port binding is unavailable or ambiguous.");
  const inodes = new Set<string>();
  for (const fd of await readdir(`/proc/${pid}/fd`)) {
    try { const link = await readlink(`/proc/${pid}/fd/${fd}`); const inode = /^socket:\[(\d+)\]$/.exec(link)?.[1]; if (inode) inodes.add(inode); } catch {}
  }
  const tcp = (await readFile(`/proc/${pid}/net/tcp`, "utf8")).split("\n").slice(1).map(line => line.trim().split(/\s+/));
  const address = `0100007F:${selectedPort.toString(16).toUpperCase().padStart(4,"0")}`;
  if (!tcp.some(row => row[1] === address && row[3] === "0A" && inodes.has(row[9]!))) throw new Error("The selected engine does not own its expected loopback listener.");
  return argv;
}
async function workerPid(origin: string): Promise<number> {
  const catalog = await json(`${origin}/v1/juiced/catalog`);
  const worker = catalog.data?.find((entry: any) => entry.id === modelId);
  if (!worker?.loaded || !Number.isSafeInteger(worker.pid) || worker.pid <= 0) throw new Error("The manager did not publish a loaded model-worker PID.");
  return worker.pid;
}
async function originalProcessGone(pid: number, startTicks: string) {
  try {
    const stat = await readFile(`/proc/${pid}/stat`, "utf8");
    return stat.slice(stat.lastIndexOf(")") + 1).trim().split(/\s+/)[19] !== startTicks;
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code === "ENOENT") return true;
    throw error;
  }
}
async function engineEnvironment(pid: number) {
  const raw = await readFile(`/proc/${pid}/environ`);
  if (raw.length > 4 * 1024 * 1024) throw new Error("The selected engine environment exceeds the benchmark bound.");
  const entries = new TextDecoder("utf-8", {fatal:true}).decode(raw).split("\0").filter(Boolean).map(value => {
    const separator = value.indexOf("=");
    return [value.slice(0,separator),value.slice(separator+1)] as const;
  }).filter(([key]) => /^(?:LLAMA_|LLAMACPP_|GGML_|GGUF_|MTMD_|OMP_|GOMP_|KMP_|MKL_|OPENBLAS_|GOTO_|LD_|DYLD_)/.test(key.toUpperCase()) || ["HF_HUB_OFFLINE","HF_DATASETS_OFFLINE","TRANSFORMERS_OFFLINE","HOME","XDG_CONFIG_HOME"].includes(key.toUpperCase()) || reservedRuntimeVariables.has(key.toUpperCase())).sort(([a],[b]) => a < b ? -1 : a > b ? 1 : 0);
  if (entries.some(([key]) => reservedRuntimeVariables.has(key.toUpperCase()) || key.toUpperCase().startsWith("LLAMA_ARG_"))) throw new Error("A compared engine retained a reserved runtime environment variable.");
  return { keys: entries.map(([key]) => key), sha256: hashBytes(JSON.stringify(entries)), scope: "Exact selected runtime, threading, loader, offline/cache, proxy and HOME/XDG configuration environment; reserved controls must be absent. Values are not retained. Manager variables and credentials are excluded from comparison." };
}
async function executableMappings(pid: number) {
  const mappings = await readFile(`/proc/${pid}/maps`, "utf8");
  const paths = new Set<string>();
  for (const line of mappings.split("\n")) {
    if (!/^\S+\s+\S*x\S*\s/.test(line)) continue;
    const path = line.match(/\s(\/[^\n]+)$/)?.[1];
    if (!path) continue;
    if (path.endsWith(" (deleted)")) throw new Error("A running executable mapping was deleted during identity inspection.");
    paths.add(await realpath(path));
  }
  if (paths.size === 0 || paths.size > 256) throw new Error("The running process executable mapping inventory was unavailable or oversized.");
  const result = [];
  for (const path of [...paths].sort()) {
    let sha256 = runtimeFiles.get(path);
    if (!sha256) { sha256 = await hashFile(path); runtimeFiles.set(path, sha256); }
    result.push({ path, sha256 });
  }
  return result;
}

await mkdir(output, { recursive: true });
await command(["cargo", "build", "--release", "--locked", "-p", "juiced-service", "-p", "juiced-rust-tests", "--bin", "lemond", "--bin", "bench-api"]);
await command(["cargo", "build", "--profile", "profiling", "--features", "cpu-profile", "--locked", "-p", "juiced-service", "--bin", "lemond"]);
const ticksPerSecond = Number(await command(["getconf", "CLK_TCK"]));
if (!Number.isSafeInteger(ticksPerSecond) || ticksPerSecond <= 0) throw new Error("CPU accounting frequency is unavailable.");
const identities = {
  rust_and_harness_sources: await sourceIdentity(),
  engine: { path: engine, sha256: await hashFile(engine) }, model: { path: model, sha256: await hashFile(model) },
  release_manager: { path: releaseManager, sha256: await hashFile(releaseManager) }, profiled_manager: { path: profileManager, sha256: await hashFile(profileManager) },
  benchmark: { path: benchmark, sha256: await hashFile(benchmark) }, script: { path: import.meta.path, sha256: await hashFile(import.meta.path) }, request_body_sha256: hashBytes(body), engine_argv_template: [engine, ...engineTemplate],
  controlled_environment: { JUICED_IO_THREADS: String(ioThreads), HF_HUB_OFFLINE: "1", OMP_NUM_THREADS: "2", OPENBLAS_NUM_THREADS: "2", inherited_LLAMA_variables: false, scrubbed_reserved_runtime_variables: [...reservedRuntimeVariables].sort(), isolated_shared_HOME_and_XDG: true, API_authentication: false },
  host: { platform: process.platform, architecture: process.arch, kernel: release(), logical_cpus: cpus().length, cpu_model: cpus()[0]?.model, bun: Bun.version, load_average_at_start: loadavg() },
  native_baseline: null as null | { source: string; binary_sha256: string; resource_tree_sha256: string; resource_files: { path: string; sha256: string }[] },
  reference_rust: referenceRust ? { path: referenceRust, sha256: await hashFile(referenceRust) } : null,
};
const temporary = await mkdtemp(resolve(output, ".inference-state-"));
const bodyFile = resolve(temporary, "body.json");
baseEnv.HOME = resolve(temporary, "engine-home");
baseEnv.XDG_CONFIG_HOME = resolve(temporary, "engine-config");
await mkdir(baseEnv.HOME, {recursive:true});
await mkdir(resolve(baseEnv.XDG_CONFIG_HOME, "llama.cpp"), {recursive:true});
const cleanup: unknown[] = [];
const observedWorkers = new Map<number, string>();
const configValue = { schema_version: 2, cache_enabled: false, inhibit_suspend: rustInhibitSuspend, resident_budget_bytes: 1024 * 1024 * 1024, startup_timeout_ms: 30_000, request_timeout_ms: 30_000, models: [{ id: modelId, model_file: model, execution_profile: { schema_version: 1, adapter: "llama-cpp", implementation_id: "llama-cpp", command: engine, args: engineTemplate, options: {}, placement: "unresolved" }, resident_bytes: 512 * 1024 * 1024, max_inflight: 1, operations: ["completions"], policy: "on-demand" }] };
async function manager(name: string, executable: string, profiled = false) {
  const directory = resolve(temporary, name);
  await mkdir(directory, { recursive: true });
  const config = resolve(directory, "config.json");
  await writeFile(config, JSON.stringify(configValue));
  const origin = `http://127.0.0.1:${port()}`;
  const server = launch(name, [executable, "--config", config, "--listen", origin.slice("http://".length)], directory, { ...baseEnv, JUICED_SNAPSHOT_DIR: resolve(directory, "snapshots"), ...(profiled ? { JUICED_CPU_PROFILE: svg } : {}) });
  await ready(server, `${origin}/v1/health`);
  return { server, origin };
}
async function resourceIdentity(directory: string) {
  const files: { path: string; sha256: string }[] = [];
  let bytes = 0;
  async function visit(relative: string) {
    for (const entry of await readdir(resolve(directory, relative), { withFileTypes: true })) {
      const path = relative ? `${relative}/${entry.name}` : entry.name;
      if (entry.isDirectory()) await visit(path);
      else if (entry.isFile()) {
        bytes += (await stat(resolve(directory, path))).size;
        if (files.length >= 10_000 || bytes > 64 * 1024 * 1024) throw new Error("Native resources exceed the bounded benchmark snapshot.");
        files.push({ path, sha256: await hashFile(resolve(directory, path)) });
      } else throw new Error("Native resource links or special files cannot enter this benchmark snapshot.");
    }
  }
  await visit("");
  files.sort((a,b) => a.path < b.path ? -1 : a.path > b.path ? 1 : 0);
  return { sha256: hashBytes(JSON.stringify(files)), files };
}
async function nativeManager() {
  if (!nativeSource) return null;
  const directory = resolve(temporary, "native-manager");
  const sourceResources = resolve(dirname(nativeSource), "resources");
  const original = { binary: await hashFile(nativeSource), resources: await resourceIdentity(sourceResources) };
  await mkdir(directory, { recursive: true });
  const binary = resolve(directory, "lemond");
  await copyFile(nativeSource, binary);
  await cp(sourceResources, resolve(directory, "resources"), { recursive: true });
  const copied = await resourceIdentity(resolve(directory, "resources"));
  if (await hashFile(binary) !== original.binary || await hashFile(nativeSource) !== original.binary || copied.sha256 !== original.resources.sha256 || (await resourceIdentity(sourceResources)).sha256 !== original.resources.sha256) throw new Error("Native baseline changed during snapshot; no measurement was started.");
  identities.native_baseline = { source: nativeSource, binary_sha256: original.binary, resource_tree_sha256: copied.sha256, resource_files: copied.files };
  const config = resolve(directory, "config");
  await mkdir(config);
  const configuration = { config_version: 2, offline: true, no_fetch_executables: true, auto_check_model_updates: false, auto_update_models: false, broadcast: false, websocket_port: 0, inhibit_suspend: nativeInhibitSuspend, llamacpp: { backend: "cpu", cpu_bin: dirname(engine), prefer_system: false } };
  const registration = { [modelId]: { recipe: "llamacpp", source: "local_path", checkpoints: { main: model }, labels: ["chat"], recipe_options: { ctx_size: 512, llamacpp_backend: "cpu", llamacpp_args: nativeArguments } } };
  await writeFile(resolve(config, "config.json"), JSON.stringify(configuration));
  await writeFile(resolve(config, "user_models.json"), JSON.stringify(registration));
  const origin = `http://127.0.0.1:${port()}`;
  const server = launch("native-manager", [binary, resolve(directory,"cache"), config, "--host", "127.0.0.1", "--port", origin.split(":").at(-1)!, "--no-broadcast", "--log-file", "disabled"], directory, { ...baseEnv, JUICED_SNAPSHOT_DIR: resolve(directory, "snapshots") });
  await ready(server, `${origin}/v1/health`);
  const effective = await json(`${origin}/internal/config`);
  if (effective.inhibit_suspend !== nativeInhibitSuspend) throw new Error("The native suspend-inhibition setting does not match the selected control.");
  async function post(path: string, body: unknown) {
    const response = await fetch(`${origin}${path.startsWith("internal/") ? "/" : "/v1/"}${path}`, { method: "POST", headers: {"content-type":"application/json"}, body: JSON.stringify(body), signal: AbortSignal.timeout(40_000), redirect: "error" });
    await response.arrayBuffer();
    if (!response.ok) throw new Error(`Native ${path} failed with HTTP ${response.status}. ${server.log.tail.slice(-1000)}`);
  }
  await post("juiced/policy", { model: `user.${modelId}`, policy: "on-demand" });
  const models = await json(`${origin}/v1/models`);
  if (!models.data?.some((entry: any) => entry.id === modelId && entry.downloaded === true)) throw new Error("The enabled native local model does not match the selected public ID.");
  await post("load", { model_name: modelId });
  const all = new Set<number>();
  const queue = [server.child.pid];
  for (let index = 0; index < queue.length; index++) {
    if (queue.length > 64) throw new Error("Native subprocess inventory exceeds the benchmark bound.");
    const parent = queue[index]!;
    let tasks: string[];
    try { tasks = await readdir(`/proc/${parent}/task`); } catch { continue; }
    for (const tid of tasks) {
      let list: string;
      try { list = await readFile(`/proc/${parent}/task/${tid}/children`, "utf8"); } catch { continue; }
      for (const pid of list.trim().split(/\s+/).filter(Boolean).map(Number)) if (!all.has(pid)) { all.add(pid); queue.push(pid); }
    }
  }
  const matches: number[] = [];
  for (const pid of all) {
    try { if (await realpath(`/proc/${pid}/exe`) === engine) matches.push(pid); } catch {}
  }
  if (matches.length !== 1) throw new Error("Native did not retain exactly one owned selected engine process.");
  const enginePid = matches[0]!;
  const start = (await sample(enginePid)).start_ticks;
  observedWorkers.set(enginePid, start);
  return { server, origin, enginePid, engineStart: start, binary, resources: resolve(directory,"resources"), configuration, registration, observedInhibitSuspend:effective.inhibit_suspend,
    setInhibit: async (enabled: boolean) => {
      await post("internal/set", {inhibit_suspend:enabled});
      const current = await json(`${origin}/internal/config`);
      if (current.inhibit_suspend !== enabled) throw new Error("The native crossover setting was not applied.");
    },
    unload: () => post("unload", { model_name: modelId }) };
}

async function measure(origin: string, requests: number, processes: Record<string, number>, rustPower = false) {
  if (interrupted) throw new Error("Profiling was interrupted.");
  const powerBefore = rustPower ? await powerSnapshot(origin) : null;
  if (powerBefore && (powerBefore.active !== 0 || powerBefore.shutting_down)) throw new Error("The Rust manager was not idle before its next benchmark workload.");
  const before = Object.fromEntries(await Promise.all(Object.entries(processes).map(async ([name, pid]) => [name, await sample(pid)] as const)));
  const peaks = Object.fromEntries(Object.entries(before).map(([name, value]) => [name, value.rss_bytes]));
  let sampling = true;
  let samplingError: unknown;
  const sampler = (async () => {
    while (sampling) {
      for (const [name, pid] of Object.entries(processes)) peaks[name] = Math.max(peaks[name]!, (await sample(pid)).rss_bytes);
      await Bun.sleep(5);
    }
  })().catch(error => { samplingError = error; sampling = false; });
  const run = Bun.spawn([benchmark, "--url", origin, "--path", "/v1/completions", "--requests", String(requests), "--concurrency", String(concurrency), "--body-file", bodyFile, "--expect-completion-tokens", "1", "--expect-prompt-tokens", "10"], { cwd: root, env: baseEnv, stdin: "ignore", stdout: "pipe", stderr: "pipe" });
  let powerError: unknown;
  const workloadPower = rustPower && rustInhibitSuspend ? observeWorkloadPower(origin, run).catch(error => { powerError = error; return null; }) : Promise.resolve(null);
  commands.add(run);
  const timeout = setTimeout(() => { run.kill("SIGTERM"); setTimeout(() => { if (run.exitCode === null) run.kill("SIGKILL"); }, 1000); }, 130_000);
  try {
    const [stdout, stderr] = await Promise.all([new Response(run.stdout).text(), new Response(run.stderr).text()]);
    if (await run.exited !== 0) throw new Error(`Inference benchmark failed: ${stderr.slice(-2000)}`);
    const powerDuring = await workloadPower;
    if (powerError) throw powerError;
    if (samplingError) throw samplingError;
    const measurement = JSON.parse(stdout);
    if (measurement.errors !== 0 || measurement.successful !== requests || measurement.workload.body_sha256 !== identities.request_body_sha256 || measurement.passing !== true || measurement.measurement_complete !== true || measurement.unrecorded_requests !== 0 || measurement.validation?.measured?.checked !== requests || measurement.validation?.measured?.passed !== requests || measurement.validation?.warmup?.passed !== concurrency * 8) throw new Error("The benchmark did not validate every response in the exact selected workload.");
    const after = Object.fromEntries(await Promise.all(Object.entries(processes).map(async ([name, pid]) => [name, await sample(pid)] as const)));
    const processSamples = Object.fromEntries(Object.entries(before).map(([name, initial]) => {
      const final = after[name]!;
      if (initial.start_ticks !== final.start_ticks) throw new Error("A measured process was replaced during its measurement interval.");
      return [name, { pid: initial.pid, start_ticks: initial.start_ticks, cpu_time_ms: (final.cpu_ticks - initial.cpu_ticks) * 1000 / ticksPerSecond, cpu_resolution_ms: 1000 / ticksPerSecond, rss_before_bytes: initial.rss_bytes, rss_after_bytes: final.rss_bytes, peak_sampled_rss_bytes: Math.max(peaks[name]!, final.rss_bytes), threads_before: initial.threads, threads_after: final.threads }];
    }));
    const powerAfter = rustPower ? await powerSnapshot(origin) : null;
    if (powerBefore && powerAfter && (powerAfter.acquisitions < powerBefore.acquisitions || powerAfter.releases < powerBefore.releases || powerAfter.attempts < powerBefore.attempts)) throw new Error("The Rust power owner changed generation during the workload.");
    return { measurement, process_samples: processSamples, process_sample_scope: "Each named Linux PID separately over the complete benchmark process lifetime, including that benchmark's warmups. Samples are not atomic across PIDs; CPU resolution is reported. RSS is not summed; shared pages and short peaks are not resolved.", rss_sampling_interval_ms: 5,
      ...(rustPower ? { rust_power: { configured_inhibit_suspend: rustInhibitSuspend, before: powerBefore, during: powerDuring, after: powerAfter } } : {}) };
  } finally {
    clearTimeout(timeout);
    commands.delete(run);
    if (run.exitCode === null) { run.kill("SIGTERM"); await waitForExit(run, 1000); if (run.exitCode === null) { run.kill("SIGKILL"); await run.exited; } }
    sampling = false;
    await sampler;
    await workloadPower;
    if (samplingError) throw samplingError;
  }
}

try {
  await writeFile(bodyFile, body);
  const rust = await manager("uninstrumented-rust", releaseManager);
  const rustPipeline = await verifyPipeline(rust.origin);
  const native = await nativeManager();
  const directDirectory = resolve(temporary, "direct-engine");
  await mkdir(directDirectory, { recursive: true });
  const directPort = port();
  const directOrigin = `http://127.0.0.1:${directPort}`;
  const direct = launch("direct-engine", [engine, ...engineTemplate.map(value => value.replaceAll("{port}", String(directPort)).replaceAll("{model}", model))], directDirectory, baseEnv);
  await ready(direct, `${directOrigin}/health`);
  const directPipeline = await verifyPipeline(directOrigin);
  const nativePipeline = native ? await verifyPipeline(native.origin) : null;
  const rustEnginePid = await workerPid(rust.origin);
  const rustEngineStart = (await sample(rustEnginePid)).start_ticks;
  observedWorkers.set(rustEnginePid, rustEngineStart);
  if (native) observedWorkers.set(native.enginePid, native.engineStart);
  const directArgv = await normalizedEngineArgv(direct.child.pid);
  const rustArgv = await normalizedEngineArgv(rustEnginePid);
  const nativeArgv = native ? await normalizedEngineArgv(native.enginePid) : null;
  if (nativeArgv && JSON.stringify(directArgv) !== JSON.stringify(nativeArgv)) throw new Error("The native and direct engines did not receive identical normalized arguments.");
  if (JSON.stringify(directArgv) !== JSON.stringify(rustArgv)) throw new Error("The direct and managed engines did not receive identical normalized arguments.");
  const environments = { direct: await engineEnvironment(direct.child.pid), rust: await engineEnvironment(rustEnginePid), native: native ? await engineEnvironment(native.enginePid) : null };
  if (environments.direct.sha256 !== environments.rust.sha256 || (environments.native && environments.native.sha256 !== environments.direct.sha256)) throw new Error("The selected engines received different relevant runtime environments.");
  const runtimeMappings = { direct_engine: await executableMappings(direct.child.pid), rust_engine: await executableMappings(rustEnginePid), rust_manager: await executableMappings(rust.server.child.pid), native_engine: native ? await executableMappings(native.enginePid) : null, native_manager: native ? await executableMappings(native.server.child.pid) : null };
  if (JSON.stringify(runtimeMappings.direct_engine) !== JSON.stringify(runtimeMappings.rust_engine)) throw new Error("The direct and managed engines loaded different executable libraries.");
  if (native && JSON.stringify(runtimeMappings.direct_engine) !== JSON.stringify(runtimeMappings.native_engine)) throw new Error("The native and direct engines loaded different executable libraries.");
  const processes: Record<string,number> = { direct_engine: direct.child.pid, rust_manager: rust.server.child.pid, rust_engine: rustEnginePid };
  if (native) { processes.native_manager = native.server.child.pid; processes.native_engine = native.enginePid; }
  const rounds: any[] = [];
  for (let pair = 0; pair < pairedRounds; pair++) {
    const order = pair % 2 === 0 ? ["direct", "rust"] : ["rust", "direct"];
    for (const implementation of order) rounds.push({ pair: pair + 1, implementation, ...await measure(implementation === "direct" ? directOrigin : rust.origin, requestsPerRound, processes, implementation === "rust") });
  }
  const nativeRounds: any[] = [];
  let nativeComparison: Record<string,unknown> | null = null;
  let referenceComparison: Record<string,unknown> | null = null;
  let crossoverComparison: Record<string,unknown> | null = null;
  if (native) {
    for (let pair = 0; pair < pairedRounds; pair++) {
      const order = pair % 2 === 0 ? ["native", "rust"] : ["rust", "native"];
      for (const implementation of order) nativeRounds.push({ pair: pair + 1, implementation, ...await measure(implementation === "native" ? native.origin : rust.origin, requestsPerRound, processes, implementation === "rust") });
    }
    if (inhibitCrossover) {
      const crossoverRounds: any[] = [];
      for (let pair = 0; pair < pairedRounds; pair++) {
        const order = pair % 2 === 0 ? [true,false] : [false,true];
        for (const enabled of order) {
          await native.setInhibit(enabled);
          crossoverRounds.push({ pair:pair+1, inhibit_suspend:enabled, ...await measure(native.origin,80,processes) });
          if ((await sample(native.enginePid)).start_ticks !== native.engineStart) throw new Error("The native worker changed during the power-setting crossover.");
        }
      }
      const finalArgv = await normalizedEngineArgv(native.enginePid);
      const finalEnvironment = await engineEnvironment(native.enginePid);
      if (JSON.stringify(finalArgv) !== JSON.stringify(nativeArgv) || finalEnvironment.sha256 !== environments.native!.sha256) throw new Error("Native worker inputs changed during the power-setting crossover.");
      await native.setInhibit(nativeInhibitSuspend);
      crossoverComparison = {
        schema_version:1,run_id:runId,measured_at:new Date().toISOString(),
        scope:"Same native manager/engine PID and generation throughout. Only inhibit_suspend is toggled through the native configuration API, with readback before each sample. No Rust language or release-parity claim.",
        identities,manager_pid:native.server.child.pid,engine_pid:native.enginePid,engine_start_ticks:native.engineStart,
        normalized_actual_engine_argv:nativeArgv,engine_environment_identity:environments.native,
        request_body_sha256:identities.request_body_sha256,concurrency,requests_per_round:80,paired_rounds:pairedRounds,
        order:"Alternating on/off and off/on pairs on one warmed native worker; configuration writes/readback finish before each benchmark interval.",rounds:crossoverRounds,
      };
    }
    await native.unload();
    cleanup.push(await stop(native.server));
    if (!await originalProcessGone(native.enginePid, native.engineStart)) throw new Error("The native engine remained alive after explicit unload and manager shutdown.");
    cleanup.push({ name: "native-managed-engine", pid: native.enginePid, original_process_terminated: true });
    if (await hashFile(native.binary) !== identities.native_baseline!.binary_sha256 || (await resourceIdentity(native.resources)).sha256 !== identities.native_baseline!.resource_tree_sha256) throw new Error("The pinned native baseline changed during its workload.");
    nativeComparison = {
      schema_version: 1, run_id: runId, started_at: startedAt, measured_at: new Date().toISOString(),
      scope: "Uninstrumented warmed CPU one-token forced-prefill completions through C++ and Rust managers with identical normalized engine argv, exact engine/model/library hashes and bounded response-shape validation. Both managers use the same explicit suspend-inhibition control. Enabled Rust inhibition additionally requires an observed acknowledged Linux guard during each Rust workload; bounded state probes may overlap early requests. Linux selected workload only; no cross-platform power parity, quality, significance, soak or release-parity claim.",
      identities, manager_launch_argv: {native: native.server.argv, rust: rust.server.argv},
      configurations: {native: native.configuration, native_registration: native.registration, rust: configValue}, observed_native_inhibit_suspend: native.observedInhibitSuspend, configured_rust_inhibit_suspend: rustInhibitSuspend,
      power_evidence_scope: "Rust enabled runs require a positive logind acknowledgment and observed held state. Native evidence is configuration readback; this baseline exposes no owned-lease counters. Cleanup records distinguish the pre-shutdown Rust snapshot from exit-code settlement.",
      request_body: JSON.parse(body), request_body_sha256: identities.request_body_sha256,
      concurrency, requests_per_round: requestsPerRound, paired_rounds: pairedRounds,
      order: "Alternating AB/BA pairs, native first on odd pairs. Direct and both managed engines remain resident; idle processes are sampled separately.",
      normalized_actual_engine_argv: {native:nativeArgv,rust:rustArgv}, executable_mapping_identities: runtimeMappings, engine_environment_identities: environments,
      manager_threading: {rust_io_threads:ioThreads,native:"Blocking HTTP pool clamps hardware_concurrency*4 to32..256 per active front; no runtime override. Actual /proc thread counts are sampled; numeric counts are not equivalent to Tokio threads."},
      initial_pipeline_checks: {native:nativePipeline,rust:rustPipeline}, host_load_average_after: loadavg(), rounds: nativeRounds,
    };
  }
  if (referenceRust) {
    const reference = await manager("reference-rust", referenceRust);
    const referencePipeline = await verifyPipeline(reference.origin);
    const referenceEnginePid = await workerPid(reference.origin);
    const referenceEngineStart = (await sample(referenceEnginePid)).start_ticks;
    observedWorkers.set(referenceEnginePid, referenceEngineStart);
    const referenceArgv = await normalizedEngineArgv(referenceEnginePid);
    const referenceEnvironment = await engineEnvironment(referenceEnginePid);
    const referenceMappings = { engine: await executableMappings(referenceEnginePid), manager: await executableMappings(reference.server.child.pid) };
    if (JSON.stringify(referenceArgv) !== JSON.stringify(rustArgv)
      || referenceEnvironment.sha256 !== environments.rust.sha256
      || JSON.stringify(referenceMappings.engine) !== JSON.stringify(runtimeMappings.rust_engine)) throw new Error("The reference and current Rust workers did not receive matching runtime inputs.");
    const referenceProcesses = { rust_manager: rust.server.child.pid, rust_engine: rustEnginePid, reference_rust_manager: reference.server.child.pid, reference_rust_engine: referenceEnginePid };
    const referenceRounds: any[] = [];
    for (let pair = 0; pair < pairedRounds; pair++) {
      const order = pair % 2 === 0 ? ["reference_rust", "rust"] : ["rust", "reference_rust"];
      for (const implementation of order) referenceRounds.push({ pair: pair + 1, implementation,
        ...await measure(implementation === "reference_rust" ? reference.origin : rust.origin, requestsPerRound, referenceProcesses, true) });
    }
    cleanup.push(await stopRustManager(reference));
    if (!await originalProcessGone(referenceEnginePid, referenceEngineStart)) throw new Error("The reference Rust engine remained alive after manager shutdown.");
    cleanup.push({ name: "reference-rust-managed-engine", pid: referenceEnginePid, original_process_terminated: true });
    if (await hashFile(referenceRust) !== identities.reference_rust!.sha256) throw new Error("The frozen reference Rust executable changed during measurement.");
    referenceComparison = {
      schema_version: 1, run_id: runId, started_at: startedAt, measured_at: new Date().toISOString(),
      scope: "Frozen reference and current uninstrumented Rust managers, identical selected engine/model/library hashes, normalized actual arguments, selected environments, request bytes and configuration values. Five alternating warmed CPU one-token pairs. Different temporary manager directory names and ordinary host variation remain; this does not isolate a single source change or establish significance, soak or release parity.",
      identities, manager_launch_argv: { reference_rust: reference.server.argv, rust: rust.server.argv },
      configuration: configValue, configuration_sha256: hashBytes(JSON.stringify(configValue)),
      request_body: JSON.parse(body), request_body_sha256: identities.request_body_sha256,
      concurrency, requests_per_round: requestsPerRound, paired_rounds: pairedRounds,
      order: "Reference first on odd pairs, current first on even pairs; both workers remain resident and idle while the other is measured.",
      normalized_actual_engine_argv: { reference_rust: referenceArgv, rust: rustArgv },
      engine_environment_identities: { reference_rust: referenceEnvironment, rust: environments.rust },
      executable_mapping_identities: { reference_rust: referenceMappings, rust_engine: runtimeMappings.rust_engine, rust_manager: runtimeMappings.rust_manager },
      initial_pipeline_checks: { reference_rust: referencePipeline, rust: rustPipeline },
      power_evidence_scope: "Both managers share the same explicit power configuration and require positive held Linux inhibition during every enabled workload. Bounded metadata probes may overlap early requests and count toward manager CPU. Cleanup confirms each owner's idle release and successful exit.",
      host_load_average_after: loadavg(), rounds: referenceRounds,
    };
  }
  cleanup.push(await stopRustManager(rust));
  if (!await originalProcessGone(rustEnginePid, rustEngineStart)) throw new Error("The managed engine remained alive after manager shutdown.");
  cleanup.push({ name: "uninstrumented-managed-engine", pid: rustEnginePid, original_process_terminated: true });
  cleanup.push(await stop(direct));
  const comparison = { schema_version: 1, run_id: runId, started_at: startedAt, measured_at: new Date().toISOString(), scope: "Uninstrumented direct llama.cpp versus Rust forwarding of warmed, forced-prefill, one-token completions, including the explicit Rust Linux power control. The direct engine has no manager-owned inhibitor. Enabled Rust power proof adds bounded metadata probes during benchmark process lifetime, including warmups. No isolated forwarding-cost, output-quality, significance, cross-platform power or release-parity claim.", identities, manager_launch_argv: rust.server.argv, manager_configuration: configValue, manager_configuration_sha256: hashBytes(JSON.stringify(configValue)), request_body: JSON.parse(body), concurrency, requests_per_round: requestsPerRound, paired_rounds: pairedRounds, order: "Alternating AB/BA pairs; direct first on odd pairs, Rust first on even pairs. Both engines remain resident and idle while the other is measured.", normalized_actual_engine_argv: directArgv, normalized_actual_engine_argv_sha256: hashBytes(JSON.stringify(directArgv)), executable_mapping_identities: runtimeMappings, engine_environment_identities: environments, initial_pipeline_checks: { direct: directPipeline, rust: rustPipeline }, rounds, host_load_average_after: loadavg() };

  await rm(svg, { force: true });
  const profiled = await manager("profiled-rust", profileManager, true);
  const pipeline = await verifyPipeline(profiled.origin);
  const profiledEnginePid = await workerPid(profiled.origin);
  const profiledEngineStart = (await sample(profiledEnginePid)).start_ticks;
  observedWorkers.set(profiledEnginePid, profiledEngineStart);
  const profiledArgv = await normalizedEngineArgv(profiledEnginePid);
  if (JSON.stringify(directArgv) !== JSON.stringify(profiledArgv)) throw new Error("The instrumented manager used a different engine launch profile.");
  const profiledEnvironment = await engineEnvironment(profiledEnginePid);
  if (profiledEnvironment.sha256 !== environments.direct.sha256) throw new Error("The profiled engine received different relevant runtime environment settings.");
  const profiledRuntimeMappings = { manager: await executableMappings(profiled.server.child.pid), engine: await executableMappings(profiledEnginePid) };
  if (JSON.stringify(runtimeMappings.direct_engine) !== JSON.stringify(profiledRuntimeMappings.engine)) throw new Error("The profiled managed engine loaded different executable libraries.");
  const measurement = await measure(profiled.origin, profileRequests, { profiled_manager: profiled.server.child.pid, profiled_engine: profiledEnginePid }, true);
  cleanup.push(await stopRustManager(profiled));
  if (!await originalProcessGone(profiledEnginePid, profiledEngineStart)) throw new Error("The profiled model worker remained alive after manager shutdown.");
  cleanup.push({ name: "profiled-managed-engine", pid: profiledEnginePid, original_process_terminated: true });
  const flamegraph = await Bun.file(svg).text();
  const frames = [...flamegraph.matchAll(/<title>([^<]+)<\/title>/g)].map(match => match[1]!.replaceAll("&lt;", "<").replaceAll("&gt;", ">").replaceAll("&amp;", "&"));
  const sampled = frames.map(frame => ({ frame, samples: Number(frame.match(/\(([\d,]+) samples?[,)]/)?.[1]?.replaceAll(",", "") ?? 0) })).filter(frame => frame.samples > 0).sort((a, b) => b.samples - a.samples);
  const forwardingFrames = sampled.filter(frame => /forward_request|forward_prioritized|reqwest|hyper|RequestLease|admission|request_model/.test(frame.frame));
  const projectFrames = sampled.filter(frame => /juiced_service::(?:http::(?:forward_|request_model)|supervisor::RequestLease|admission::)/.test(frame.frame));
  if (!flamegraph.includes("<svg") || projectFrames.length === 0) throw new Error("The flamegraph did not contain observed HTTP forwarding frames.");
  if (await hashFile(engine) !== identities.engine.sha256 || await hashFile(model) !== identities.model.sha256 || await hashFile(releaseManager) !== identities.release_manager.sha256 || await hashFile(profileManager) !== identities.profiled_manager.sha256) throw new Error("An executable or model changed during the workload.");
  if (referenceRust && await hashFile(referenceRust) !== identities.reference_rust!.sha256) throw new Error("The reference Rust executable changed before receipt acceptance.");
  if ((await sourceIdentity()).sha256 !== identities.rust_and_harness_sources.sha256) throw new Error("Rust or benchmark sources changed during measurement.");
  for (const [path, sha256] of runtimeFiles) if (await hashFile(path) !== sha256) throw new Error("An executable library changed during the workload.");
  const profile = { schema_version: 1, run_id: runId, started_at: startedAt, measured_at: new Date().toISOString(), scope: "Manager-only SIGPROF CPU samples across the profiled service lifetime, including startup identity/probes and shutdown. The separately measured engine is excluded from the SVG. Instrumented timings are diagnostic and excluded from the uninstrumented latency comparison.", identities, profile_frequency_hz: 199, retained_profile_samples: sampled.find(frame => frame.frame.startsWith("all ("))?.samples ?? null, profiler_blocklist: ["libc", "libgcc", "pthread", "vdso"], sampling_limits: "Retained stacks are diagnostic; inclusive frames overlap. The profiler does not count allocations, and a small sample count does not establish precise hotspot percentages.", instrumented_cpu_profile: true, manager_launch_argv: profiled.server.argv, manager_configuration: configValue, manager_configuration_sha256: hashBytes(JSON.stringify(configValue)), request_body: JSON.parse(body), normalized_actual_engine_argv: profiledArgv, normalized_actual_engine_argv_sha256: hashBytes(JSON.stringify(profiledArgv)), executable_mapping_identities: profiledRuntimeMappings, engine_environment_identity: profiledEnvironment, pipeline_check: pipeline, measurement, flamegraph: svg, flamegraph_sha256: hashBytes(flamegraph), largest_inclusive_frames: sampled.slice(0, 15), observed_forwarding_frames: forwardingFrames.slice(0, 20), project_forwarding_frames: projectFrames.slice(0, 15), cleanup };
  await writeFile(comparisonPath, JSON.stringify({...comparison, cleanup, accepted: true}, null, 2) + "\n");
  if (nativeComparison) await writeFile(resolve(output, "inference-native-versus-rust.json"), JSON.stringify({...nativeComparison, cleanup, accepted: true}, null, 2) + "\n");
  if (referenceComparison) await writeFile(resolve(output, "inference-reference-rust-versus-current.json"), JSON.stringify({...referenceComparison, cleanup, accepted: true}, null, 2) + "\n");
  if (crossoverComparison) await writeFile(resolve(output,"native-inhibit-crossover.json"),JSON.stringify({...crossoverComparison,cleanup,accepted:true},null,2)+"\n");
  await writeFile(profilePath, JSON.stringify({...profile, accepted: true}, null, 2) + "\n");
  const pairs = Array.from({ length: pairedRounds }, (_, index) => { const direct = rounds.find(round => round.pair === index + 1 && round.implementation === "direct"); const rust = rounds.find(round => round.pair === index + 1 && round.implementation === "rust"); return { pair: index + 1, direct_p50_us: direct.measurement.latency_us.p50, rust_p50_us: rust.measurement.latency_us.p50, rust_minus_direct_p50_us: rust.measurement.latency_us.p50 - direct.measurement.latency_us.p50 }; });
  console.log(JSON.stringify({ comparison: comparisonPath, native_comparison: nativeComparison ? resolve(output,"inference-native-versus-rust.json") : null, reference_rust_comparison: referenceComparison ? resolve(output,"inference-reference-rust-versus-current.json") : null, profile: profilePath, flamegraph: svg, pairs, profiled_project_frames: projectFrames.slice(0, 3) }, null, 2));
} finally {
  for (const process of children) await stop(process);
  const unsettled: number[] = [];
  for (const [pid, generation] of observedWorkers) if (!await originalProcessGone(pid, generation)) unsettled.push(pid);
  if (unsettled.length || uncertainManagerCleanup.size) throw new Error(`Owned engine or manager cleanup is unconfirmed; benchmark state retained at ${temporary}. No PID signals were sent to uncertain workers.`);
  await rm(temporary, { recursive: true, force: true });
  process.off("SIGINT", cancel);
  process.off("SIGTERM", cancel);
}
