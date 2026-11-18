import { expect, test } from "bun:test";
import { ApiError } from "../src/api";
import { pullJobs } from "../src/Downloading";
import { diskQuant, hubQuantState, quantUsers } from "../src/FindModels";
import { parseSearch } from "../src/libraryData";
import { parseLocal, parsePulls } from "../src/localLibraryData";
import { installFailures } from "../src/LocalModels";
import type { ArtifactJob } from "../src/management";
import { plainError } from "../src/modelErrors";
import { quantLabel } from "../src/Models";
import { policyWord } from "../src/QuantList";

const local = parseLocal({ groups: [{ id: "hf:unsloth/Demo-GGUF", repo: "unsloth/Demo-GGUF", name: "Demo-GGUF",
  quants: [
    { label: "Q4_K_M", file: "f4", bytes: 40, files: ["f4"], used_by: ["demo", "demo-hybrid"] },
    { label: "Q8_0", file: "f8", bytes: 80, files: ["f8"], used_by: [], add: { path: "/f8" }, install: { state: "pending", error: null, message: null } },
    { label: "Q6_K", file: "f6", bytes: 60, files: ["f6"], used_by: [], add: { path: "/f6" }, install: { state: "failed", error: "engine_not_installed", message: "Needs the llama.cpp cpu engine. Install it under Engines." } },
    { label: "Q2_K", file: "f2", bytes: 20, files: ["f2"], used_by: [], add: { path: "/f2" }, install: { state: "unusable", error: "companion_file", message: "This file is a text encoder." } },
    { label: "UD-Q6_K_XL", file: "p1", parts: 3, complete: false, bytes: 6, files: ["p1"], used_by: [] },
  ],
  files: [{ id: "f4", name: "Demo-Q4_K_M.gguf", used_by: ["demo", "demo-hybrid"] }, { id: "f8", name: "Demo-Q8_0.gguf" }, { id: "f6", name: "Demo-Q6_K.gguf" }, { id: "f2", name: "Demo-Q2_K.gguf" }, { id: "p1", name: "Demo-UD-Q6_K_XL-00001-of-00003.gguf", role: "shard" }] }] });
const models = [{ id: "demo", policy: "hot" as const }, { id: "demo-hybrid", policy: "disabled" as const }];
const hub = parseSearch({ sources: { huggingface: { status: "ok" } }, data: [{ repo: "Unsloth/Demo-GGUF", name: "Demo-GGUF", quants: [
  { label: "Q4_K_M", file: "Demo-Q4_K_M.gguf", bytes: 40, local: { present: true } },
  { label: "Q8_0", file: "Demo-Q8_0.gguf", bytes: 80, local: { present: true } },
  { label: "Q6_K", file: "Demo-Q6_K.gguf", bytes: 60, local: { present: true } },
  { label: "UD-Q6_K_XL", file: "UD-Q6_K_XL/Demo-UD-Q6_K_XL-00001-of-00003.gguf", parts: 3, bytes: 600, pullable: true, local: { present: false, present_files: 1 } },
  { label: "Q5_K_M", file: "Demo-Q5_K_M.gguf", bytes: 50, pullable: true, jobs: [{ id: "j", state: "downloading", downloaded_bytes: 25 }] },
  { label: "BF16", file: "Demo-BF16.gguf", bytes: 160, pullable: true },
] }] }).models[0]!;

test("a quant reads the same on disk and in Discover, and is never Not added", () => {
  expect(hub.quants.map((quant) => hubQuantState(quant, quantUsers(local, hub.repo, quant), models, diskQuant(local, hub.repo, quant)))).toEqual(["Hot +1", "Installing", "Not installed", "1 of 3 parts", "Downloading 50%", ""]);
  expect(hub.quants.map((quant) => hubQuantState(quant, quantUsers(null, hub.repo, quant), []))).toEqual(["Installing", "Installing", "Installing", "1 of 3 parts", "Downloading 50%", ""]);
  expect(policyWord(null)).toBe("Installed");
});

test("Installed warns about files that should be models and leaves the rest to Storage", () => {
  expect(installFailures(local)).toEqual([{ name: "Demo-GGUF Q6_K", message: "Needs the llama.cpp cpu engine. Install it under Engines." }]);
  expect(installFailures(null)).toEqual([]);
});

test("a model row names its quant only when the model name doesn't", () => {
  expect(quantLabel({ id: "demo" }, local)).toBe("Q4_K_M");
  const named = parseLocal({ groups: [{ id: "g", repo: "a/b", name: "b", quants: [{ label: "Q8_0", file: "f", files: ["f"], used_by: ["b-q8_0"] }, { label: "ONNX", file: "o", files: ["o"], used_by: ["kokoro"] }], files: [{ id: "f", name: "b-Q8_0.gguf", used_by: ["b-q8_0"] }, { id: "o", name: "k.onnx", used_by: ["kokoro"] }] }] });
  expect(quantLabel({ id: "b-q8_0" }, named)).toBeNull();
  expect(quantLabel({ id: "kokoro" }, named)).toBeNull();
  expect(quantLabel({ id: "demo" }, null)).toBeNull();
});

test("a failed part's job joins its pull by source, so one download is one row", () => {
  const source = (part: number) => `https://huggingface.co/unsloth/Demo-GGUF/resolve/abc/Demo-0000${part}-of-00003.gguf`;
  const pull = parsePulls({ data: [{ pull: "p", state: "failed", error: "artifact_job_failed", repo: "unsloth/Demo-GGUF", label: "UD-Q6_K_XL", files: [
    { file: "a", bytes: 5, state: "present", job: null, source_url: source(1) },
    { file: "b", bytes: 50, state: "downloading", downloaded_bytes: 20, job: "j2", source_url: source(2) },
    { file: "c", bytes: 23, state: "failed", job: null, source_url: source(3) },
  ] }] })[0]!;
  const job = (id: string, part: number, state: string) => ({ id, state, source_url: source(part), error: state === "failed" ? "artifact_network" : null }) as ArtifactJob;
  const jobs = [job("done1", 1, "complete"), job("j2", 2, "failed"), job("j3", 3, "failed"), { ...job("other", 3, "failed"), source_url: "https://example.com/else.gguf" }];
  expect(pullJobs(pull, jobs).map((entry) => entry.id)).toEqual(["j2", "j3"]);
});

test("library failures read in plain words", () => {
  expect(plainError(new ApiError("The artifact state changed. Refresh the current revision before retrying.", 409, "artifact_revision_conflict"), "x")).toBe("Something else changed the library at the same time. Try again.");
  expect(plainError(new ApiError("Not enough free space for this download. Free space, then retry.", 507, "insufficient_storage"), "x")).toBe("Not enough free space for this download. Free space, then retry.");
  expect(plainError("?", "The download didn't start.")).toBe("The download didn't start.");
});
