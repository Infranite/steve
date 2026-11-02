/* Codex adapter compatibility. codex-acp embeds its own (older) Codex core but reads the user's ~/.codex/config.toml.
 * A model catalog written for a newer Codex CLI (`model_catalog_json`) does not parse there, and its `code_mode_only`
 * tool mode starts an embedded V8 that crashes on this macOS. We hand the adapter a sanitized copy instead;
 * the user's own files are never changed. */
import { existsSync, mkdirSync, readFileSync, statSync, writeFileSync } from "node:fs"
import { homedir } from "node:os"
import { join } from "node:path"
import { config } from "./config.js"

const EFFORTS = new Set(["none", "minimal", "low", "medium", "high", "xhigh"])
let cached: { src: string; mtime: number; out: string } | null = null

/** the catalog path set at the top level of config.toml (before the first [table]), if any */
function catalogPath(): string | null {
  const file = join(process.env.CODEX_HOME ?? join(homedir(), ".codex"), "config.toml")
  if (!existsSync(file)) return null
  const top = readFileSync(file, "utf8").split(/^\s*\[/m)[0]
  const m = top.match(/^\s*model_catalog_json\s*=\s*["']([^"']+)["']/m)
  return m ? m[1].replace(/^~(?=\/)/, homedir()) : null
}

function sanitize(src: string, out: string) {
  const d = JSON.parse(readFileSync(src, "utf8"))
  const models: any[] = Array.isArray(d) ? d : d.models ?? []
  const fallback = models.find((m) => typeof m.base_instructions === "string" && m.base_instructions)?.base_instructions ?? ""
  for (const m of models) {
    if (typeof m.base_instructions !== "string") m.base_instructions = fallback
    m.tool_mode = "direct"
    if (Array.isArray(m.supported_reasoning_levels)) m.supported_reasoning_levels = m.supported_reasoning_levels.filter((l: any) => EFFORTS.has(l?.effort))
    if (m.default_reasoning_level != null && !EFFORTS.has(m.default_reasoning_level)) m.default_reasoning_level = "medium"
  }
  mkdirSync(join(config.home, "codex"), { recursive: true })
  writeFileSync(out, JSON.stringify(d), { mode: 0o600 })
}

/** extra `-c key=value` arguments for codex-acp; empty when the user's config needs no help */
export function codexArgs(): string[] {
  const src = catalogPath(); if (!src || !existsSync(src)) return []
  const out = join(config.home, "codex", "models.json"), mtime = statSync(src).mtimeMs
  if (!cached || cached.src !== src || cached.mtime !== mtime || !existsSync(out)) {
    try { sanitize(src, out); cached = { src, mtime, out } } catch (e: any) { console.error("codex catalog", e.message); return [] }
  }
  return ["-c", `model_catalog_json=${JSON.stringify(out)}`]
}

/** Steve's permission modes in Codex terms. Codex has no plan mode and its own default may be full access,
 * so the mode is always set: read-only asks before every edit or command, auto works inside the workspace
 * and asks before network access or writes outside it. full-access is never used. */
export const CODEX_MODES: Record<string, string> = { plan: "read-only", default: "read-only", acceptEdits: "auto", auto: "auto" }
