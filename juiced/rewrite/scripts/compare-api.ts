import { mkdtemp, mkdir, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { resolve } from "node:path";
import { createHash } from "node:crypto";

const root = resolve(import.meta.dir, "..");
const native = process.env.JUICED_BASELINE_LEMOND;
if (!native) throw new Error("Set JUICED_BASELINE_LEMOND to the existing native lemond executable.");
const output = resolve(root, process.env.JUICED_BENCH_OUT ?? "local/performance");
await mkdir(output, { recursive: true });
const build = Bun.spawn(["cargo", "build", "--release", "--locked", "-p", "juiced-service", "-p", "juiced-rust-tests", "--bins"], { cwd: root, stdout: "inherit", stderr: "inherit" });
if (await build.exited !== 0) process.exit(1);
const rewrite = resolve(root, "target/release/lemond");
const bench = resolve(root, "target/release/bench-api");
const temporary = await mkdtemp(resolve(tmpdir(), "juiced-http-comparison-"));
const env = { ...process.env, LEMONADE_API_KEY: "", LEMONADE_ADMIN_API_KEY: "", JUICED_IO_THREADS: "4", HF_HUB_CACHE: resolve(temporary, "hf"), JUICED_SNAPSHOT_DIR: resolve(temporary, "snapshots") };
const results: unknown[] = [];
try {
  for (const [name, binary] of [["native", native], ["rust", rewrite]]) {
    const directory = resolve(temporary, name);
    await mkdir(directory, { recursive: true });
    const reservation = Bun.serve({ port: 0, hostname: "127.0.0.1", fetch: () => new Response() });
    const port = reservation.port!;
    reservation.stop(true);
    const config = resolve(directory, "config.json");
    if (name === "rust") await writeFile(config, JSON.stringify({ schema_version: 1, resident_budget_bytes: 1024, startup_timeout_ms: 1000, request_timeout_ms: 1000, models: [] }));
    const args = name === "rust" ? ["--config", config, "--listen", `127.0.0.1:${port}`] : [resolve(directory, "cache"), resolve(directory, "config"), "--host", "127.0.0.1", "--port", String(port), "--no-broadcast", "--log-file", "disabled"];
    const server = Bun.spawn([binary!, ...args], { env, cwd: directory, stdout: "ignore", stderr: "ignore" });
    try {
      const url = `http://127.0.0.1:${port}`;
      let ready = false;
      for (let attempt = 0; attempt < 100; attempt++) {
        try { const response = await fetch(`${url}/v1/health`, { signal: AbortSignal.timeout(100) }); if (response.ok) { await response.text(); ready = true; break; } } catch {}
        if (server.exitCode !== null) break;
        await Bun.sleep(50);
      }
      if (!ready) throw new Error(`${name} did not become ready (exit ${server.exitCode}).`);
      const health = await (await fetch(`${url}/v1/health`)).text();
      const rounds: unknown[] = [];
      for (let round = 0; round < 5; round++) {
        const run = Bun.spawn([bench, "--url", url, "--path", "/v1/health", "--requests", "100000", "--concurrency", "16", "--pid", String(server.pid)], { env, stdout: "pipe", stderr: "inherit" });
        const measurement = await new Response(run.stdout).json();
        if (await run.exited !== 0) throw new Error(`${name} benchmark failed.`);
        rounds.push(measurement);
      }
      const sha256 = createHash("sha256").update(new Uint8Array(await Bun.file(binary!).arrayBuffer())).digest("hex");
      results.push({ implementation: name, binary_sha256: sha256, health_response_bytes: Buffer.byteLength(health), rounds });
    } finally {
      server.kill("SIGTERM");
      await Promise.race([server.exited, Bun.sleep(5000)]);
      if (server.exitCode === null) { server.kill("SIGKILL"); await server.exited; }
    }
  }
  const receipt = { schema_version: 1, scope: "Uninstrumented manager HTTP health only; separate isolated empty stores; no inference or cache workload; implementations have different health payloads and feature scope", order: "five native rounds then five Rust rounds; not randomized", requests_per_round: 100000, concurrency: 16, rust_io_threads: 4, results };
  await writeFile(resolve(output, "http-health-comparison.json"), JSON.stringify(receipt, null, 2) + "\n");
  console.log(JSON.stringify(receipt));
} finally { await rm(temporary, { recursive: true, force: true }); }
