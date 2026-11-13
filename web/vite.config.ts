import { resolve } from "node:path"
import tailwindcss from "@tailwindcss/vite"
import react from "@vitejs/plugin-react"
import { defineConfig } from "vite"

const hub = process.env.STEVE_HUB ?? "http://127.0.0.1:8441"
export default defineConfig({
  plugins: [react(), tailwindcss()],
  resolve: { alias: [{ find: "@", replacement: resolve(import.meta.dirname, "./src") }, { find: /^cn$/, replacement: resolve(import.meta.dirname, "./src/lib/cn.ts") }] },
  /* scan every source file at start, lazy routes included: a dependency first met on a lazy page would otherwise be
   * bundled in a second pass with its own copy of React, and that page would fail with "Invalid hook call" */
  optimizeDeps: { entries: ["index.html", "src/**/*.{ts,tsx}"] },
  server: { port: 5174, proxy: { "/api": { target: hub, changeOrigin: false }, "/auth": { target: hub }, "/healthz": { target: hub } } },
  build: { chunkSizeWarningLimit: 4000 },
})
