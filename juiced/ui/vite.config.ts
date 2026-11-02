import { defineConfig } from "vite";
import { dependencyNotices } from "./licenses";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
import { fileURLToPath, URL } from "node:url";
export default defineConfig({
  plugins: [react(), tailwindcss(), dependencyNotices()],
  resolve: { alias: { "~": fileURLToPath(new URL("./src", import.meta.url)) } },
  build: { outDir: "../../web", emptyOutDir: true },
  server: {
    proxy: {
      "/logs/stream": {
        target: process.env.JUICED_DEV_API || "http://127.0.0.1:13305",
        ws: true,
        changeOrigin: true,
      },
      "/v1": process.env.JUICED_DEV_API || "http://127.0.0.1:13305",
      "/.well-known": process.env.JUICED_DEV_API || "http://127.0.0.1:13305",
    },
  },
});
