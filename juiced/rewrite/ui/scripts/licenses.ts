import { copyFile, mkdir } from "node:fs/promises";
import { resolve } from "node:path";

const output = resolve(import.meta.dir, "../dist/licenses");
const source = resolve(import.meta.dir, "../../../ui");
await mkdir(output, { recursive: true });
for (const [from, to] of [
  ["LICENSE.t3code", "LICENSE.t3code"], ["T3-SOURCES.json", "T3-SOURCES.json"],
  ["public/fonts/LICENSE.Inter.txt", "LICENSE.Inter.txt"], ["public/fonts/SOURCE.json", "Inter-SOURCE.json"],
]) await copyFile(resolve(source, from), resolve(output, to));
