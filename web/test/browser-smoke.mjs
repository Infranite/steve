// Runs headless Chromium browser tests against a web dev server of this checkout (CI runs both tiers on Linux).
// The server's Hub address goes nowhere: every test answers the API itself (or starts its own loopback mock), so
// none can reach an installed or development Hub. Every web/test/*.browser.mjs file belongs to SMOKE or EXTENDED,
// partitioned by CI_SMOKE_THRESHOLD_S; web/test/browser-inventory.test.mjs enforces that no unowned test exists.
// Usage:
//   node web/test/browser-smoke.mjs                          # run SMOKE
//   node web/test/browser-smoke.mjs --extended               # run all EXTENDED
//   node web/test/browser-smoke.mjs --extended --shard=1/2   # run shard 1 of 2 of EXTENDED
//   node web/test/browser-smoke.mjs --all                    # run SMOKE + EXTENDED
//   node web/test/browser-smoke.mjs [name…]                  # run named *.browser.mjs tests
// Needs Playwright with Chromium (`npx playwright install --only-shell chromium`, or PLAYWRIGHT_MODULE/_EXECUTABLE).
// Concurrency defaults to min(4, availableParallelism()); set STEVE_BROWSER_JOBS=1 to run serially.
import { spawn } from "node:child_process"
import { availableParallelism } from "node:os"
import { resolve } from "node:path"
import { fileURLToPath } from "node:url"

/**
 * CI-measured per-test duration ceiling (seconds) for SMOKE.
 * Basis: GitHub Actions `ubuntu-22.04` (4 vCPU, jobs=4) run 45466409731
 * (web job 138702386318, browser-extended job 138702522180).
 * Tests <= 30 s on CI stay in SMOKE (59 tests, ~228 s browser step / ~6.1 min total `web` job);
 * tests > 30 s on CI move to EXTENDED (26 tests, split across EXTENDED_SHARDS parallel runners so
 * each shard completes in ~4.3-4.8 min, keeping both jobs well within the 8 min CI target).
 */
export const CI_SMOKE_THRESHOLD_S = 30
export const EXTENDED_SHARDS = 2

/* Fast browser tests (<= CI_SMOKE_THRESHOLD_S on CI): run in CI's `web` job. */
export const SMOKE = [
  "appearance",
  "attention-read",
  "audit-channels",
  "bot-mcp",
  "bot-skills",
  "bots",
  "capabilities",
  "channel-record",
  "channel-reply-refresh",
  "channel-states",
  "cleanup",
  "comm-pages",
  "command-audit",
  "composer-run",
  "config-cards",
  "conversations",
  "debug",
  "decision-source",
  "delivery-isolation",
  "devices",
  "devices-install",
  "devices-map",
  "disk",
  "effort-mapping",
  "external-write-gates",
  "feishu",
  "feishu-proxy",
  "harness-tab",
  "harness-updates",
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
  "models-settings",
  "no-executor",
  "node-harnesses",
  "node-tabs",
  "onboarding",
  "people-management",
  "person-avatar",
  "personal-triggers",
  "plugins",
  "queued-comments",
  "run-bar",
  "running-pill",
  "runtime-evidence",
  "session-panel",
  "settings-panes",
  "sidebar-versions",
  "sign-in",
  "stats-prices",
  "stream-resume",
  "tool-approval",
  "tool-pins",
  "usage-ledger",
  "work-trail",
]

/* Slow browser tests (> CI_SMOKE_THRESHOLD_S on CI): run in CI's sharded `browser-extended` job when web files change. */
export const EXTENDED = [
  "a2ui",
  "auto",
  "calendar",
  "channel-reply",
  "chat-history",
  "chat-layout",
  "chat-panel",
  "delivery",
  "devices-look",
  "feed",
  "handoff-card",
  "hub-management",
  "model-recovery",
  "nav-ia",
  "native-shell",
  "notifications",
  "outputs",
  "page-followup",
  "reply-preview",
  "run-progress",
  "runtime-scope",
  "runtime-setup",
  "send-authorization",
  "stats",
  "thinking-state",
  "todos",
  "updates",
]

const LIMIT_MS = 180_000

export function parseShard(spec) {
  const m = /^(\d+)\/(\d+)$/.exec(spec)
  if (!m) throw new Error(`invalid --shard value "${spec}" (expected k/N, e.g. 1/2)`)
  const index = Number(m[1]), total = Number(m[2])
  if (!Number.isInteger(index) || !Number.isInteger(total) || total < 1 || index < 1 || index > total) {
    throw new Error(`invalid --shard range "${spec}" (expected 1 <= k <= N)`)
  }
  return { index, total }
}

/** Deterministic test-count sharding: interleaves sorted test names so every shard differs in count by at most 1. */
export function selectShard(names, index, total) {
  return names.filter((_, i) => i % total === index - 1)
}

function parseArgs(argv) {
  let mode = null
  let shard = null
  const picked = []
  for (let i = 0; i < argv.length; i++) {
    const arg = argv[i]
    if (arg === "--extended" || arg === "--all") {
      if (mode) throw new Error(`conflicting flags: ${mode} and ${arg}`)
      mode = arg
    } else if (arg === "--shard") {
      if (i + 1 >= argv.length) throw new Error("--shard requires a k/N argument")
      shard = parseShard(argv[++i])
    } else if (arg.startsWith("--shard=")) {
      shard = parseShard(arg.slice("--shard=".length))
    } else if (arg.startsWith("-")) {
      throw new Error(`unknown option: ${arg}`)
    } else {
      picked.push(arg)
    }
  }
  if (mode && picked.length) throw new Error(`cannot combine ${mode} with explicit test names`)
  let label = "browser smoke"
  let names = SMOKE
  if (mode === "--extended") {
    label = "browser extended"
    names = EXTENDED
  } else if (mode === "--all") {
    label = "browser all"
    names = [...SMOKE, ...EXTENDED]
  } else if (picked.length) {
    names = picked
  }
  if (shard) {
    names = selectShard(names, shard.index, shard.total)
    label += ` (shard ${shard.index}/${shard.total})`
  }
  return { label, names }
}

async function main() {
  const { label, names } = parseArgs(process.argv.slice(2))
  if (!names.length) {
    console.log(`${label}: ok (0 tests in 0.0 s, jobs=0)`)
    return
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
