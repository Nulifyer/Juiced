import type { Plugin } from "vite";
import { existsSync, readFileSync, readdirSync } from "node:fs";
import { dirname, join, basename } from "node:path";

// Retain the actual notices for packages whose modules enter the browser build.
export function dependencyNotices(): Plugin {
  return {
    name: "juiced-dependency-notices",
    generateBundle() {
      const roots = new Set<string>();
      for (const id of this.getModuleIds()) {
        if (!id.includes("node_modules")) continue;
        let directory = dirname(id.replace(/^\0/, "").split("?")[0]);
        while (directory !== dirname(directory)) {
          const file = join(directory, "package.json");
          if (existsSync(file)) {
            const p = JSON.parse(readFileSync(file, "utf8"));
            if (p.name && p.version) {
              roots.add(directory);
              break;
            }
          }
          directory = dirname(directory);
        }
      }
      // These tools emit runtime helpers and CSS rather than normal modules.
      for (const name of ["vite", "tailwindcss"])
        roots.add(join(process.cwd(), "node_modules", name));
      const notices: string[] = [];
      for (const root of [...roots].sort()) {
        const p = JSON.parse(readFileSync(join(root, "package.json"), "utf8"));
        const files = readdirSync(root).filter((n) =>
          /^(licen[cs]e|copying|notice)(\.|$)/i.test(n),
        );
        if (!files.length)
          this.error(`Missing distribution notice: ${p.name}@${p.version}`);
        for (const file of files) {
          const source = readFileSync(join(root, file), "utf8");
          const filename = `licenses/${p.name.replace(/[@/]/g, "_")}@${p.version}-${basename(file)}.txt`;
          this.emitFile({ type: "asset", fileName: filename, source });
          notices.push(
            `${p.name}@${p.version}\t${p.license || "See notice"}\t${filename}`,
          );
        }
      }
      this.emitFile({
        type: "asset",
        fileName: "licenses/T3-Code-MIT.txt",
        source: readFileSync("LICENSE.t3code", "utf8"),
      });
      this.emitFile({
        type: "asset",
        fileName: "THIRD_PARTY_NOTICES.txt",
        source:
          "Juiced browser dependencies and generated helpers/CSS\n" +
          notices.sort().join("\n") +
          "\nInter 4.1: fonts/LICENSE.Inter.txt (source and checksums: fonts/SOURCE.json)\n" +
          "\nT3 Code presentation: licenses/T3-Code-MIT.txt\n",
      });
    },
  };
}
