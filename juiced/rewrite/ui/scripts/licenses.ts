import { copyFile, mkdir, readdir, readFile, writeFile } from "node:fs/promises";
import { join, resolve } from "node:path";

const root = resolve(import.meta.dir, "..");
const output = resolve(root, "dist/licenses");
const source = resolve(root, "../../ui");
await mkdir(output, { recursive: true });
for (const [from, to] of [
  [resolve(source, "LICENSE.t3code"), "LICENSE.t3code"], [resolve(source, "T3-SOURCES.json"), "T3-SOURCES.json"],
  [resolve(root, "T3-SOURCES.json"), "T3-SOURCES.console.json"],
  [resolve(source, "public/fonts/LICENSE.Inter.txt"), "LICENSE.Inter.txt"], [resolve(source, "public/fonts/SOURCE.json"), "Inter-SOURCE.json"],
]) await copyFile(from!, resolve(output, to!));

type Manifest = { name: string; version: string; license?: string; dependencies?: Record<string, string> };
async function locate(name: string, from: string): Promise<string> {
  for (let directory = from; ; directory = resolve(directory, "..")) {
    const candidate = join(directory, "node_modules", name);
    try { await readFile(join(candidate, "package.json")); return candidate; } catch { /* Keep walking up. */ }
    if (directory === root) throw new Error(`Runtime dependency ${name} is not installed. Run bun install.`);
  }
}
const seen = new Map<string, { manifest: Manifest; text: string }>();
async function visit(name: string, from: string) {
  const directory = await locate(name, from);
  const manifest = JSON.parse(await readFile(join(directory, "package.json"), "utf8")) as Manifest;
  const key = `${manifest.name}@${manifest.version}`;
  if (seen.has(key)) return;
  const file = (await readdir(directory)).find((entry) => /^(licen[cs]e|copying)(\.(md|txt))?$/i.test(entry));
  if (!file) throw new Error(`${key} ships no license file, so its notice cannot be reproduced.`);
  seen.set(key, { manifest, text: (await readFile(join(directory, file), "utf8")).trim() });
  for (const dependency of Object.keys(manifest.dependencies ?? {})) await visit(dependency, directory);
}
const own = JSON.parse(await readFile(join(root, "package.json"), "utf8")) as Manifest;
for (const name of Object.keys(own.dependencies ?? {})) await visit(name, root);
const notices = [...seen.entries()].sort(([left], [right]) => left.localeCompare(right))
  .map(([key, { manifest, text }]) => `${key} (${manifest.license ?? "see notice"})\n\n${text}`);
await writeFile(resolve(output, "THIRD-PARTY-NOTICES.txt"), `Third-party packages bundled into the Juiced console.\n\n${notices.join(`\n\n${"-".repeat(72)}\n\n`)}\n`);
console.log(`licenses: ${seen.size} bundled packages`);
