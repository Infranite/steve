/* Runtime configuration. Everything comes from the environment; secrets are read from files, never from argv. */
import { readFileSync, mkdirSync, existsSync } from "node:fs"
import { homedir } from "node:os"
import { join, resolve } from "node:path"

import { execSync } from "node:child_process"
const env = (k: string, d?: string) => process.env[k] ?? d
let root = ""
function npmRoot() { if (!root) { try { root = execSync("npm root -g", { stdio: ["ignore", "pipe", "ignore"] }).toString().trim() } catch { root = "/opt/homebrew/lib/node_modules" } } return root }
/* codex-acp's bin is a Node wrapper that spawnSync's a native binary; killing the wrapper would orphan the binary,
 * so Steve runs the platform binary directly when it is installed */
function codexAdapter() {
  const pkg = join(npmRoot(), "@zed-industries/codex-acp")
  const native = join(pkg, "node_modules/@zed-industries", `codex-acp-${process.platform}-${process.arch}`, "bin/codex-acp")
  return existsSync(native) ? native : join(pkg, "bin/codex-acp.js")
}
const fromFile = (k: string) => { const p = env(k + "_FILE"); return p ? readFileSync(p, "utf8").trim() : env(k) }

const home = resolve(env("STEVE_HOME", join(homedir(), ".steve"))!)
export const config = {
  home,
  /** Steve's own repository: items about Steve itself are worked on there */
  steveRepo: env("STEVE_REPO", join(homedir(), "Agents/steve"))!,
  dataDir: join(home, "data"),
  worktrees: join(home, "worktrees"),
  scratch: join(home, "scratch"),
  listen: { host: env("STEVE_HOST", "127.0.0.1")!, port: Number(env("STEVE_PORT", "8440")) },
  publicUrl: env("STEVE_PUBLIC_URL", "http://127.0.0.1:8440")!.replace(/\/$/, ""),
  /** every origin Steve is served on (e.g. the .dev and the .home name); login returns to the one the browser used */
  publicUrls: (env("STEVE_PUBLIC_URLS") ?? env("STEVE_PUBLIC_URL", "http://127.0.0.1:8440")!).split(",").map((u) => u.trim().replace(/\/$/, "")).filter(Boolean),
  webDir: env("STEVE_WEB_DIR", resolve(import.meta.dirname, "../../web/dist"))!,
  oidc: { issuer: env("STEVE_OIDC_ISSUER"), clientId: env("STEVE_OIDC_CLIENT_ID"), clientSecret: fromFile("STEVE_OIDC_CLIENT_SECRET") },
  /** local development only: allow /auth/dev from loopback */
  devLogin: env("STEVE_DEV_LOGIN") === "1",
  /** don't resume interrupted turns or send held prompts at boot (always set when running on a copy of a real database) */
  noResume: env("STEVE_NO_RESUME") === "1",
  /** harness adapters */
  claudeExecutable: env("CLAUDE_CODE_EXECUTABLE", "/opt/homebrew/bin/claude")!,
  /** adapter entry points; a .js path runs with this Node */
  adapters: {
    "claude-code": env("STEVE_ADAPTER_CLAUDE", join(npmRoot(), "@agentclientprotocol/claude-agent-acp/dist/index.js"))!,
    codex: env("STEVE_ADAPTER_CODEX", codexAdapter())!,
  } as Record<string, string>,
  /** Steve's own model calls go through the harness CLI; each tier is tried in order. Use full model ids: behind the lab gateway the aliases fail or are served by another model */
  models: { fast: env("STEVE_MODELS_FAST", "claude-haiku-5-5,claude-sonnet-5-5")!.split(","), smart: env("STEVE_MODELS_SMART", "claude-sonnet-5-5,claude-opus-5-5")!.split(",") },
  /** model for bots that don't pick one, per harness: the official Claude models (the kiro route is gone) */
  defaultModels: { "claude-code": env("STEVE_DEFAULT_MODEL_CLAUDE", "claude-opus-5-5")! } as Record<string, string>,
  idleReleaseMs: Number(env("STEVE_IDLE_RELEASE_MS", String(10 * 60_000))),
  itemPrefix: env("STEVE_ITEM_PREFIX", "ST")!,
  version: env("STEVE_VERSION", "dev")!,
}
for (const d of [config.dataDir, config.worktrees, config.scratch]) mkdirSync(d, { recursive: true })
/** the web build being served: release.json in the web dir (web/current, a symlink), read on every call because a web
 * release switches the link without restarting the Hub; "" for a build without one (development, before the split) */
export function webVersion(): string {
  try { return String(JSON.parse(readFileSync(join(config.webDir, "release.json"), "utf8")).version ?? "") } catch { return "" }
}
