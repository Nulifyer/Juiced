import { mkdir, readFile, writeFile } from "node:fs/promises";
import { createHash } from "node:crypto";
import { createReadStream } from "node:fs";
import { resolve } from "node:path";

const root = resolve(import.meta.dir, "..");
const flags = new Set(process.argv.slice(2));
for (const flag of flags) if (!["--full", "--real", "--speech", "--power", "--resources"].includes(flag)) throw new Error(`Unknown check option ${flag}`);
async function sourceIdentity() {
  const files = new Set(["Cargo.toml", "Cargo.lock", "config.example.json", "service/Cargo.toml", "rust-tests/Cargo.toml", "ui/package.json", "ui/bun.lock", "ui/index.html", "ui/tsconfig.json", "ui/playwright.config.ts", "PLAN.md", "FEATURE-COVERAGE.json", "MAIN-SNAPSHOT.json", "api-parity/inventory.json", "api-parity/main-comparison.json", "catalog-design/inventory.json", "catalog-design/inventory.ts", "../../src/cpp/resources/backend_versions.json", "../../src/cpp/resources/server_models.json", "../../src/cpp/resources/architecture_defaults.json", "../../juiced/Dockerfile", "../../juiced/runtime/futo_server.cpp", "../../juiced/BACKENDS.md", "../../test/test_speech_de.wav"]);
  const nativeApi = JSON.parse(await readFile(resolve(root, "api-parity/inventory.json"), "utf8")) as { sources: { path: string; hash_check: string }[] };
  for (const source of nativeApi.sources) if (source.hash_check !== "snapshot_only") files.add(`../../${source.path}`);
  const mainPlan = JSON.parse(await readFile(resolve(root, "MAIN-SNAPSHOT.json"), "utf8")) as { files: { path: string }[] };
  for (const source of mainPlan.files) files.add(`../../${source.path}`);
  for (const directory of ["service/src", "service/tests", "service/examples", "rust-tests/src", "rust-tests/tests", "ui/src", "ui/test", "ui/e2e", "ui/scripts", "scripts", "api-parity"]) {
    for await (const file of new Bun.Glob("**/*.{rs,ts,tsx,css}").scan({ cwd: resolve(root, directory), onlyFiles: true })) files.add(`${directory}/${file}`);
  }
  const hash = createHash("sha256");
  for (const file of [...files].sort()) { hash.update(file); hash.update("\0"); hash.update(await readFile(resolve(root, file))); hash.update("\0"); }
  return { sha256: hash.digest("hex"), files: files.size };
}
async function fileHash(path: string) {
  const hash = createHash("sha256");
  for await (const chunk of createReadStream(path)) hash.update(chunk);
  return hash.digest("hex");
}
const realInputs = flags.has("--real") && process.env.JUICED_TEST_LLAMA_SERVER && process.env.JUICED_TEST_GGUF ? {
  engine_sha256: await fileHash(process.env.JUICED_TEST_LLAMA_SERVER),
  model_sha256: await fileHash(process.env.JUICED_TEST_GGUF),
} : null;
const speechInputs = flags.has("--speech") && process.env.JUICED_TEST_WHISPER_SERVER && process.env.JUICED_TEST_WHISPER_MODEL && process.env.JUICED_TEST_WHISPER_BUNDLE ? {
  engine_sha256: await fileHash(process.env.JUICED_TEST_WHISPER_SERVER),
  model_sha256: await fileHash(process.env.JUICED_TEST_WHISPER_MODEL),
  bundle_manifest_sha256: await fileHash(process.env.JUICED_TEST_WHISPER_BUNDLE),
} : null;
const started = new Date().toISOString();
const before = await sourceIdentity();
const steps: { command: string[]; directory: string; exit_code: number; elapsed_ms: number }[] = [];
let failure: string | null = null;
async function run(command: string[], cwd = root) {
  console.log(`Running ${command.join(" ")}`);
  const start = performance.now();
  const child = Bun.spawn(command, { cwd, stdout: "inherit", stderr: "inherit", stdin: "inherit" });
  const code = await child.exited;
  steps.push({ command, directory: cwd, exit_code: code, elapsed_ms: Math.round(performance.now() - start) });
  if (code !== 0) throw new Error(`Check failed: ${command.join(" ")}`);
}
try {
  await run(["cargo", "build", "-p", "juiced-service", "--bins", "--features", "test-fixture", "--locked"]);
  await run(["cargo", "test", "-p", "juiced-service", "--features", "test-fixture", "--locked"]);
  await run(["cargo", "test", "-p", "juiced-rust-tests", "--locked"]);
  await run([process.execPath, "test", "test/"], resolve(root, "ui"));
  await run([process.execPath, "test", "scripts/plan.test.ts"]);
  await run([process.execPath, "catalog-design/inventory.ts", "--check", "--native-root", resolve(root,"../..")]);
  await run([process.execPath, "api-parity/inventory.ts", "--check"]);
  await run([process.execPath, "test", "api-parity/inventory.test.ts"]);
  if (flags.has("--real")) {
    if (!process.env.JUICED_TEST_LLAMA_SERVER || !process.env.JUICED_TEST_GGUF) throw new Error("Set JUICED_TEST_LLAMA_SERVER and JUICED_TEST_GGUF for the real-engine lane.");
    await run(["cargo", "test", "-p", "juiced-rust-tests", "--test", "real_engine", "--locked", "tiny_gguf_", "--", "--ignored"]);
    if (process.platform === "linux") await run(["cargo", "test", "-p", "juiced-rust-tests", "--test", "runtime_packages", "--locked", "--", "--ignored", "--exact", "tiny_gguf_installed_runtime_copy_load_cache_restart_and_continue_without_source"]);
    if (process.platform === "linux") await run(["cargo", "test", "-p", "juiced-rust-tests", "--test", "runtime_packages", "--locked", "--", "--ignored", "--exact", "tiny_gguf_shared_runtime_models_promote_unregister_and_restore_the_survivor"]);
    if (process.platform === "linux") await run(["cargo", "test", "-p", "juiced-rust-tests", "--test", "native_import", "--locked", "--", "--ignored", "--exact", "tiny_real_native_import_load_cache_restart_edit_and_clear_pipeline"]);
    await run(["cargo", "test", "-p", "juiced-service", "--lib", "profiles::tests::real_gguf_metadata_observation", "--locked", "--", "--ignored"]);
  }
  if (flags.has("--speech")) {
    if (!speechInputs) throw new Error("Set JUICED_TEST_WHISPER_SERVER, JUICED_TEST_WHISPER_MODEL and JUICED_TEST_WHISPER_BUNDLE for the real speech lane.");
    await run(["cargo", "test", "-p", "juiced-rust-tests", "--test", "real_whisper", "--locked", "--", "--ignored"]);
    if (process.platform === "linux") await run(["cargo", "test", "-p", "juiced-rust-tests", "--test", "runtime_packages", "--locked", "--", "--ignored", "--exact", "tiny_whisper_installed_runtime_transcribes_after_source_removal_promotion_restart_and_rollback"]);
  }
  if (flags.has("--power")) {
    if (process.platform !== "linux" || process.env.JUICED_TEST_POWER_INHIBIT !== "1") throw new Error("The power lane requires Linux, JUICED_TEST_POWER_INHIBIT=1 and accessible logind inhibition.");
    await run(["cargo", "test", "-p", "juiced-service", "--lib", "--locked", "--", "--ignored", "--exact", "power::tests::real_logind_inhibitor_round_trip"]);
    await run(["cargo", "test", "-p", "juiced-rust-tests", "--test", "power_lifecycle", "--locked", "--", "--ignored", "--exact", "real_logind_daemon_requests_share_a_lease_cancel_release_and_settle_on_shutdown"]);
  }
  if (flags.has("--resources")) {
    if (process.platform !== "linux" || process.env.JUICED_TEST_HOST_MEMORY !== "1") throw new Error("The resource lane requires Linux, JUICED_TEST_HOST_MEMORY=1 and complete observed host headroom.");
    await run(["cargo", "test", "-p", "juiced-service", "--lib", "--locked", "--", "--ignored", "--exact", "host_memory::tests::actual_host_observation"]);
    await run(["cargo", "test", "-p", "juiced-service", "--lib", "--locked", "--", "--ignored", "--exact", "writer_identity::tests::actual_owned_identity_live_exit_and_no_reap"]);
    await run(["cargo", "test", "-p", "juiced-rust-tests", "--test", "runtime_packages", "--locked", "--", "--ignored", "--exact", "protected_runtime_import_retains_metadata_and_binding_claims_across_retries_and_restart"]);
    await run(["cargo", "test", "-p", "juiced-rust-tests", "--test", "native_import", "--locked", "--", "--ignored", "--exact", "protected_host_claims_normalize_text_refund_original_eof_and_retain_disconnect_until_explicit_unload"]);
    await run(["cargo", "test", "-p", "juiced-rust-tests", "--test", "native_import", "--locked", "--", "--ignored", "--exact", "protected_cache_metadata_and_socket_responses_share_host_claims_without_loading_a_model"]);
    for (const scenario of [
      "protected_state::interrupted_inspection_retains_state_without_creating_a_save_intent",
      "protected_state::original_eof_controls_publication_and_shared_state_slot_queues_cancel_without_dispatch",
      "protected_state::cancelled_file_work_and_snapshot_reader_remain_owned_until_exact_generation_exit",
    ]) await run(["cargo", "test", "-p", "juiced-rust-tests", "--test", "native_import", "--locked", "--", "--ignored", "--exact", scenario]);
    if (flags.has("--real")) await run(["cargo", "test", "-p", "juiced-rust-tests", "--test", "native_import", "--locked", "--", "--ignored", "--exact", "tiny_real_protected_state_save_restart_restore_and_continue_pipeline"]);
    if (flags.has("--real")) await run(["cargo", "test", "-p", "juiced-rust-tests", "--test", "native_import", "--locked", "--", "--ignored", "--exact", "protected_five_real_slots_share_host_claims_without_changing_context"]);
  }
  if (flags.has("--full")) {
    await run(["cargo", "fmt", "--all", "--check"]);
    await run(["cargo", "clippy", "--workspace", "--all-targets", "--features", "test-fixture", "--locked", "--", "-D", "warnings"]);
    await run([process.execPath, "run", "build"], resolve(root, "ui"));
    await run([process.execPath, "run", "test:e2e"], resolve(root, "ui"));
  }
} catch (error) { failure = error instanceof Error ? error.message : "Verification failed."; }
const after = await sourceIdentity();
let inputsChanged = false;
try {
  if (realInputs) inputsChanged ||= realInputs.engine_sha256 !== await fileHash(process.env.JUICED_TEST_LLAMA_SERVER!) || realInputs.model_sha256 !== await fileHash(process.env.JUICED_TEST_GGUF!);
  if (speechInputs) inputsChanged ||= speechInputs.engine_sha256 !== await fileHash(process.env.JUICED_TEST_WHISPER_SERVER!) || speechInputs.model_sha256 !== await fileHash(process.env.JUICED_TEST_WHISPER_MODEL!) || speechInputs.bundle_manifest_sha256 !== await fileHash(process.env.JUICED_TEST_WHISPER_BUNDLE!);
} catch { failure ??= "An execution input became unavailable during verification."; }
const sourceChanged = before.sha256 !== after.sha256;
const receipt = { schema_version: 1, started_at: started, finished_at: new Date().toISOString(), flags: [...flags].sort(), real_inputs: realInputs, speech_inputs: speechInputs, execution_inputs_changed_during_run: inputsChanged, source_before: before, source_after: after, source_changed_during_run: sourceChanged, steps, checks_passed: failure === null, passed: failure === null && !sourceChanged && !inputsChanged, failure, scope: "Executed checks only. Real GGUF, speech and browser lanes run only with their explicit flags. Platform, feature parity and release acceptance are separate." };
await mkdir(resolve(root, "local/verification"), { recursive: true });
await writeFile(resolve(root, "local/verification/latest.json"), JSON.stringify(receipt, null, 2) + "\n");
if (failure) { console.error(failure); process.exit(1); }
if (receipt.source_changed_during_run) { console.error("Sources changed during verification. The receipt identifies both versions; a stable run is still required."); process.exit(2); }
if (inputsChanged) { console.error("Execution inputs changed during verification. A stable run is still required."); process.exit(2); }
console.log("Checks passed. Receipt: local/verification/latest.json");
