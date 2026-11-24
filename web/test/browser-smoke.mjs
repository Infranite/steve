// Runs headless Chromium browser tests against a web dev server of this checkout (CI runs both tiers on Linux).
// The server's Hub address goes nowhere: every test answers the API itself (or starts its own loopback mock), so
// none can reach an installed or development Hub. Every web/test/*.browser.mjs file belongs to SMOKE (<= 30 s) or
// EXTENDED (> 30 s); web/test/browser-inventory.test.mjs enforces that no unowned browser test exists.
// Usage:
//   node web/test/browser-smoke.mjs               # run SMOKE
//   node web/test/browser-smoke.mjs --extended    # run EXTENDED
//   node web/test/browser-smoke.mjs --all         # run SMOKE + EXTENDED
//   node web/test/browser-smoke.mjs [name…]       # run named *.browser.mjs tests
// Needs Playwright with Chromium (`npx playwright install --only-shell chromium`, or PLAYWRIGHT_MODULE/_EXECUTABLE).
// Concurrency defaults to min(4, availableParallelism()); set STEVE_BROWSER_JOBS=1 to run serially.
import { spawn } from "node:child_process"
import { availableParallelism } from "node:os"
import { resolve } from "node:path"
import { fileURLToPath } from "node:url"

/* Fast browser tests (<= 30 s each): run in CI's `web` job. */
export const SMOKE = [
  "a2ui",
  "appearance",
  "attention-read",
  "audit-channels",
  "auto",
  "bot-mcp",
  "bot-skills",
  "bots",
  "calendar",
  "capabilities",
  "channel-record",
  "channel-reply",
  "channel-reply-refresh",
  "channel-states",
  "chat-layout",
  "cleanup",
  "comm-pages",
  "command-audit",
  "composer-run",
  "config-cards",
  "conversations",
  "debug",
  "decision-source",
  "delivery",
  "delivery-isolation",
  "devices",
  "devices-install",
  "devices-map",
  "disk",
  "external-write-gates",
  "feed",
  "feishu",
  "feishu-proxy",
  "handoff-card",
  "harness-updates",
  "hub-management",
  "item-card",
  "item-feed",
  "item-rows",
  "knowledge",
  "look",
  "markdown-highlight",
  "markdown-images",
  "me-page",
  "message-cards",
  "message-media",
  "message-surfaces",
  "model-recovery",
  "models-settings",
  "native-shell",
  "node-harnesses",
  "notifications",
  "onboarding",
  "outputs",
  "page-followup",
  "people-management",
  "personal-triggers",
  "plugins",
  "queued-comments",
  "reply-preview",
  "run-bar",
  "run-progress",
  "running-pill",
  "runtime-evidence",
  "send-authorization",
  "session-panel",
  "settings-panes",
  "sidebar-versions",
  "sign-in",
  "stats",
  "stats-prices",
  "stream-resume",
  "suggest-card",
  "thinking-state",
  "todos",
  "tool-approval",
  "tool-pins",
  "usage-ledger",
  "work-trail",
]

/* Slow browser tests (> 30 s each): run in CI's `browser-extended` job when web files change. */
export const EXTENDED = [
  "chat-history",
  "chat-panel",
  "devices-look",
  "runtime-scope",
  "runtime-setup",
  "updates",
]

const LIMIT_MS = 180_000

async function main() {
  const args = process.argv.slice(2)
  let label = "browser smoke"
  let names = SMOKE
  if (args.length === 1 && args[0] === "--extended") {
    label = "browser extended"
    names = EXTENDED
  } else if (args.length === 1 && args[0] === "--all") {
    label = "browser all"
    names = [...SMOKE, ...EXTENDED]
  } else if (args.length) {
    names = args
  }

  const jobs = Math.max(1, Math.min(names.length, Number(process.env.STEVE_BROWSER_JOBS) || Math.min(4, availableParallelism())))
  const web = fileURLToPath(new URL("..", import.meta.url))
  process.env.STEVE_HUB = "http://127.0.0.1:9"
  const { createServer } = await import("vite")
  const server = await createServer({ root: web, configFile: web + "vite.config.ts", logLevel: "silent", server: { host: "127.0.0.1", port: 0, strictPort: true, hmr: false, forwardConsole: false } })
  await server.listen()
  const base = `http://127.0.0.1:${server.httpServer.address().port}`

  const run = (name) => new Promise((resolveRun) => {
    const start = Date.now()
    const child = spawn(process.execPath, [fileURLToPath(new URL(`${name}.browser.mjs`, import.meta.url))], {
      env: { ...process.env, STEVE_TEST_WEB: base, STEVE_TEST_SHOTS: "" }, stdio: ["ignore", "pipe", "pipe"],
    })
    let out = ""
    child.stdout.on("data", (d) => { out += d }); child.stderr.on("data", (d) => { out += d })
    const timer = setTimeout(() => { out += `\nstopped after ${LIMIT_MS / 1000} s`; child.kill("SIGKILL") }, LIMIT_MS)
    child.on("close", (code) => { clearTimeout(timer); resolveRun({ name, ok: code === 0, s: ((Date.now() - start) / 1000).toFixed(1), out }) })
  })

  const started = Date.now()
  const failed = []
  let next = 0
  try {
    await Promise.all(Array.from({ length: jobs }, async () => {
      while (next < names.length) {
        const name = names[next++]
        const r = await run(name)
        console.log(`${r.ok ? "ok  " : "FAIL"} ${r.name} (${r.s} s)`)
        if (!r.ok) { failed.push(name); console.log(r.out.trimEnd().replace(/^/gm, "    ")) }
      }
    }))
  } finally { await server.close() }

  const total = ((Date.now() - started) / 1000).toFixed(1)
  if (failed.length) {
    console.log(`${label}: ${failed.length}/${names.length} failed in ${total} s: ${failed.join(", ")}`)
    process.exit(1)
  }
  console.log(`${label}: ok (${names.length} tests in ${total} s, jobs=${jobs})`)
}

if (process.argv[1] && fileURLToPath(import.meta.url) === resolve(process.argv[1])) {
  await main()
}
