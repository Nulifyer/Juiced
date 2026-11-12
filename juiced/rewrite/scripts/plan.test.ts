import { expect, test } from "bun:test";
import { resolve } from "node:path";

const root = resolve(import.meta.dir, "..");
const source = await Bun.file(resolve(root, "../PLATFORM-FEATURES.json")).json();
const coverage = await Bun.file(resolve(root, "FEATURE-COVERAGE.json")).json();
type Entry = { id: string; scope: string; disposition: string; rewrite_phase: string | null; rewrite_status: string; constraint?: string };
const entries = coverage.entries as Entry[];

test("every source feature and explicit user requirement has exactly one disposition", () => {
  const all = [...source.entries, ...source.user_requirements];
  expect(entries).toHaveLength(217);
  expect(new Set(entries.map((entry) => entry.id)).size).toBe(217);
  for (const feature of all) {
    const entry = entries.find((entry) => entry.id === feature.id)!;
    expect(entry.scope).toBe(feature.scope);
    expect(entry.disposition).toBe(feature.disposition);
  }
});

test("excluded and conditional scope cannot turn into completed rewrite claims", () => {
  expect(entries.filter((entry) => entry.scope === "excluded")).toHaveLength(20);
  expect(entries.filter((entry) => entry.scope === "conditional")).toHaveLength(38);
  for (const entry of entries) {
    expect(["planned", "partial", "conditional", "excluded"]).toContain(entry.rewrite_status);
    if (entry.scope === "excluded") {
      expect(entry.rewrite_phase).toBeNull();
      expect(entry.rewrite_status).toBe("excluded");
    } else expect(entry.rewrite_phase).toMatch(/^R[1-8]$/);
    if (entry.scope === "conditional") expect(entry.rewrite_status).toBe("conditional");
  }
});

test("assistant, sessions and Library retain their early milestones and restrictions", () => {
  const byId = (id: string) => entries.find((entry) => entry.id === id)!;
  expect(byId("J001").rewrite_phase).toBe("R3");
  expect(byId("J001").constraint).toContain("B0 in R1/R2");
  expect(byId("J002").rewrite_phase).toBe("R2");
  expect(byId("J005").rewrite_phase).toBe("R2");
  expect(byId("F194").constraint).toContain("No general shell");
  expect(byId("F028").constraint).toContain("Local conversion remains excluded");
});

test("source hashes, product approval binding and generated output have no drift", async () => {
  const child = Bun.spawn([process.execPath, resolve(import.meta.dir, "sync-plan.ts"), "--check"], { cwd: root, stdout: "pipe", stderr: "pipe" });
  const error = await new Response(child.stderr).text();
  expect(await child.exited, error).toBe(0);
});
