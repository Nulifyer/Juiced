import { describe, expect, test } from "bun:test";
import { describeResult, parseSyncReply, parseVariants, type SyncResult } from "../src/QuantSync";

const options = { context_tokens: 131072, threads: null, gpu_layers: 66, parallel_slots: 1 };
const member = { model: "Qwen3.8-27B-GGUF-UD-Q6_K_XL", revision: 9, quant: "UD-Q6_K_XL", provider: "llama-cpp", policy: "disabled", state: "unloaded", loaded: false, saved: options, eligible: true, reason: null, differs: ["threads"] };
const view = { model: "Qwen3.8-27B-GGUF-UD-Q4_K_XL-unsloth", revision: 9, provider: "llama-cpp", syncable: true, saved: options, fields: ["context_tokens", "threads", "gpu_layers", "parallel_slots"], group: { id: "qwen3.8-27b", label: "Qwen3.8-27B" }, members: [member] };
const result = (fields: Partial<SyncResult>): SyncResult => ({ model: member.model, outcome: "updated", reason: null, copied: [], skipped_fields: [], restarted: false, ...fields });

describe("quant sync", () => {
  test("accepts the service view and rejects malformed members", () => {
    expect(parseVariants(view).members[0]!.quant).toBe("UD-Q6_K_XL");
    expect(parseVariants({ ...view, group: null, members: [] }).group).toBeNull();
    expect(() => parseVariants({ ...view, members: [{ ...member, differs: ["command"] }] })).toThrow();
    expect(parseVariants({ ...view, members: [{ ...member, differs: ["sampling", "attention", "batching", "extra_args"] }] }).members[0]!.differs).toHaveLength(4);
    expect(() => parseVariants({ ...view, group: { id: 1 } })).toThrow();
    expect(() => parseSyncReply({ generation: 3, results: [{ ...result({}), outcome: "maybe" }] })).toThrow();
    expect(parseSyncReply({ generation: null, results: [result({ outcome: "unchanged" })] }).generation).toBeNull();
  });

  test("summarizes copies, kept fields, skips and failed reloads", () => {
    expect(describeResult(result({ copied: ["threads", "gpu_layers"], skipped_fields: [{ field: "context_tokens", reason: "context_exceeds_training", value: 131072, limit: 32768 }] })))
      .toBe("Copied threads, GPU layers; context kept: over its 32,768 trained tokens.");
    expect(describeResult(result({ copied: ["context_tokens"], restarted: true }))).toBe("Copied context. Restarted.");
    expect(describeResult(result({ outcome: "skipped", reason: "different_architecture" }))).toBe("Different architecture.");
    expect(describeResult(result({ outcome: "skipped", skipped_fields: [{ field: "parallel_slots", reason: "slot_resize_requires_registration", limit: 2 }] }))).toBe("slots kept: it has 2 slots.");
    expect(describeResult(result({ outcome: "unchanged" }))).toBe("Already the same.");
    expect(describeResult(result({ outcome: "reload_failed", reason: "worker_exited", restored_revision: 12 }))).toBe("Didn't start with the new settings; previous settings restored.");
  });
});
