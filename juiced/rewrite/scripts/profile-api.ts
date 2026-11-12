import { mkdtemp, mkdir, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { resolve } from "node:path";
import { createHash } from "node:crypto";

const root = resolve(import.meta.dir, "..");
const output = resolve(root, process.env.JUICED_PROFILE_OUT ?? "local/performance");
await mkdir(output, { recursive: true });
const executable = (profile: string, name: string) => resolve(root, "target", profile, name + (process.platform === "win32" ? ".exe" : ""));
for (const command of [
  ["cargo", "build", "-p", "juiced-service", "--profile", "profiling", "--features", "cpu-profile", "--locked"],
  ["cargo", "build", "-p", "juiced-rust-tests", "--bin", "bench-api", "--release", "--locked"],
]) {
  const child = Bun.spawn(command, { cwd: root, stdout: "inherit", stderr: "inherit" });
  if (await child.exited !== 0) process.exit(1);
}
const directory = await mkdtemp(resolve(tmpdir(), "juiced-api-profile-"));
const config = resolve(directory, "config.json");
await writeFile(config, JSON.stringify({ schema_version: 1, resident_budget_bytes: 1024, startup_timeout_ms: 1000, request_timeout_ms: 1000, models: [] }));
const svg = resolve(output, "api-cpu-flamegraph.svg");
const server = Bun.spawn([executable("profiling", "lemond"), "--config", config, "--listen", "127.0.0.1:0"], {
  cwd: root, stdout: "pipe", stderr: "inherit",
  env: { ...process.env, LEMONADE_API_KEY: "", LEMONADE_ADMIN_API_KEY: "", JUICED_CPU_PROFILE: svg, JUICED_IO_THREADS: "4" },
});
let ended = false;
try {
  const reader = server.stdout.getReader();
  let line = "";
  let origin: string | undefined;
  const deadline = Date.now() + 15000;
  while (!origin && Date.now() < deadline) {
    const value = await Promise.race([reader.read(), Bun.sleep(15000).then(() => ({ done: true, value: undefined }))]);
    if (value.done) break;
    line += new TextDecoder().decode(value.value);
    origin = line.match(/listening on (http:\/\/[^\s]+)/)?.[1];
  }
  reader.releaseLock();
  if (!origin) throw new Error("The profiled service did not announce readiness.");
  const benchmark = Bun.spawn([executable("release", "bench-api"), "--url", origin, "--path", "/v1/health", "--requests", "100000", "--concurrency", "16", "--pid", String(server.pid)], { stdout: "pipe", stderr: "inherit", env: { ...process.env, LEMONADE_API_KEY: "", LEMONADE_ADMIN_API_KEY: "" } });
  const measurement = await new Response(benchmark.stdout).json();
  if (await benchmark.exited !== 0) throw new Error("The profiling workload failed.");
  server.kill("SIGTERM");
  await server.exited;
  ended = true;
  const binary = await Bun.file(executable("profiling", "lemond")).arrayBuffer();
  const receipt = { schema_version: 1, scope: "Manager-only HTTP health hot path; no model inference or cache workload", profile_frequency_hz: 199, instrumented_cpu_profile: true, io_threads: 4, server_sha256: createHash("sha256").update(new Uint8Array(binary)).digest("hex"), measurement, flamegraph: svg };
  await writeFile(resolve(output, "api-cpu-profile.json"), JSON.stringify(receipt, null, 2) + "\n");
  console.log(JSON.stringify(receipt));
} finally {
  if (!ended) { server.kill("SIGTERM"); await server.exited; }
  await rm(directory, { recursive: true, force: true });
}
