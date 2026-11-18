import { expect, test } from "bun:test";
import { downloadRows, listed, pullBytes, rowActions } from "../src/Downloading";
import type { ArtifactJob } from "../src/management";
import type { PullView } from "../src/localLibraryData";
import { thenFailure } from "../src/modelErrors";
import { keepNote, parseRemoval, removalSummary } from "../src/RemoveModel";

const pull = (id: string, state = "complete", then: PullView["then"] = null): PullView => ({ id, state, error: null, repo: "unsloth/Qwen3-0.6B-GGUF", label: "Q4_K_M", files: [{ file: "Qwen3-0.6B-Q4_K_M.gguf", bytes: 10, state: state === "complete" ? "present" : "downloading", downloaded: 10, job: state === "complete" ? null : `job-${id}`, source: null }], then });
const add = (state: "pending" | "done" | "failed", message: string | null = null) => ({ kind: "add" as const, model: "Qwen3-0.6B-Q4_K_M", policy: "on-demand", state, error: null, message });
const job = (id: string, state: string, active = false, extra: Partial<ArtifactJob> = {}) => ({ id, state, revision: 1, error: null, source_url: `https://example.com/${id}.gguf`, downloaded_bytes: 4, writer_active: active, ...extra }) as ArtifactJob;

test("Downloads lists only what is in progress or failed", () => {
  const pulls = [pull("done", "complete", add("done")), pull("bare"), pull("installing", "complete", add("pending")), pull("uninstalled", "complete", add("failed", "No engine.")), pull("running", "downloading", add("pending"))];
  expect(pulls.map(listed)).toEqual([false, false, true, true, true]);
  const jobs = [job("job-running", "downloading", true), job("url", "failed"), job("gone", "cancelled"), job("stored", "complete")];
  const rows = downloadRows(pulls, jobs);
  expect(rows.map((row) => [row.key, row.state])).toEqual([["pull:installing", "installing"], ["pull:uninstalled", "not-installed"], ["pull:running", "downloading"], ["job:url", "failed"]]);
  expect(rows[1]!.note, "the row says why the model wasn't made").toBe("No engine.");
});

test("a row offers one way forward and one way out, never Dismiss", () => {
  const rows = downloadRows([pull("installing", "complete", add("pending")), pull("uninstalled", "complete", add("failed")), pull("running", "downloading", add("pending"))], [job("job-running", "downloading", true)]);
  expect(rows.map(rowActions)).toEqual([[], ["retry", "remove"], ["pause", "cancel"]]);
  const dropped = { ...pull("dropped", "failed", add("failed")), error: "artifact_network" };
  expect(rowActions({ pull: dropped, jobs: [] }), "a dropped connection keeps its bytes").toEqual(["resume", "remove"]);
  expect(rowActions({ pull: { ...dropped, error: "artifact_hash_mismatch" }, jobs: [] })).toEqual(["retry", "remove"]);
  expect(rowActions({ jobs: [job("paused", "paused")] })).toEqual(["resume", "remove"]);
  expect(rowActions({ jobs: [job("pausing", "pausing", true)] })).toEqual(["cancel"]);
  expect(rowActions({ jobs: [], engine: true })).toEqual([]);
});

test("a follow-up failure reads in plain words", () => {
  expect(thenFailure({ error: null, message: "No engine." }), "an unknown failure keeps the service's words").toBe("No engine.");
  expect(thenFailure({ error: "artifact_deadline", message: "Artifact verification exceeded its deadline." })).toBe("Checking the files took too long. Retry.");
});

test("a failed split pull counts the bytes its failed parts' jobs kept", () => {
  const source = (part: number) => `https://huggingface.co/u/R/resolve/main/R-0000${part}-of-00003.gguf`;
  const failed: PullView = { id: "p", state: "failed", error: "artifact_job_failed", repo: "u/R", label: "Q6_K", then: null, files: [
    { file: "R-00001-of-00003.gguf", bytes: 6, state: "present", downloaded: 6, job: null, source: source(1) },
    { file: "R-00002-of-00003.gguf", bytes: 500, state: "failed", downloaded: 0, job: null, source: source(2) },
    { file: "R-00003-of-00003.gguf", bytes: 200, state: "failed", downloaded: 0, job: null, source: source(3) },
  ] };
  const part = (id: string, index: number, bytes: number) => ({ id, state: "failed", error: "artifact_network", source_url: source(index), downloaded_bytes: bytes, writer_active: false }) as ArtifactJob;
  expect(pullBytes(failed, [part("a", 2, 190), part("b", 3, 130)])).toBe(326);
  expect(pullBytes(failed, [])).toBe(6);
});

test("the removal confirmation says what is deleted and what stays", () => {
  const plan = parseRemoval({ model: "demo", revision: 7, removable: true, refusal: null, freed_bytes: 3 * 1024 ** 3, deletes: [{ name: "demo-Q8_0.gguf", bytes: 3 * 1024 ** 3 }],
    keeps: [{ name: "mmproj.gguf", bytes: 1, reason: "shared", used_by: ["demo-vision"], location: "/data/hub/mmproj.gguf" }, { name: "linked.gguf", bytes: 1, reason: "linked", used_by: [], location: "/models/linked.gguf" }] });
  expect(plan.revision).toBe(7);
  expect(removalSummary(plan)).toBe("Deletes 1 file and frees 3.0 GiB.");
  expect(plan.keeps.map(keepNote)).toEqual(["mmproj.gguf stays. demo-vision uses it.", "linked.gguf stays in /models. Juiced links it and didn't put it there."]);
  expect(removalSummary({ deletes: 0, freedBytes: 0 })).toBe("Deletes no files.");
  const refused = parseRemoval({ removable: false, refusal: { code: "model_files_read_only", message: "a.gguf is in a read-only folder." }, deletes: [], keeps: [] });
  expect([refused.removable, refused.refusal]).toEqual([false, "a.gguf is in a read-only folder."]);
  expect(() => parseRemoval({})).toThrow();
});
