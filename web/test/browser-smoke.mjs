// A few fast browser tests, one after another, against a web dev server of this checkout (CI's web job runs it on Linux).
// The server's Hub address goes nowhere: the tests answer the API themselves, so none can reach an installed or
// development Hub. `node web/test/browser-smoke.mjs [name…]` runs the named *.browser.mjs tests instead of the list.
// Needs Playwright with Chromium (`npx playwright install --only-shell chromium`, or PLAYWRIGHT_MODULE/_EXECUTABLE).
import { spawn } from "node:child_process"
import { fileURLToPath } from "node:url"

/* full app and its shell first (sign-in, settings with a workspace switch, the native shell's bridge), then pages */
const SMOKE = ["sign-in", "nav-ia", "settings-panes", "notifications", "native-shell", "bots", "node-harnesses", "conversations", "channel-states", "item-card", "message-cards", "reply-preview", "channel-reply", "channel-reply-refresh", "runtime-setup"]
const LIMIT_MS = 120_000

const web = fileURLToPath(new URL("..", import.meta.url))
process.env.STEVE_HUB = "http://127.0.0.1:9"
const { createServer } = await import("vite")
const server = await createServer({ root: web, configFile: web + "vite.config.ts", logLevel: "error", server: { host: "127.0.0.1", port: 0, strictPort: true, forwardConsole: false } })
await server.listen()
const base = `http://127.0.0.1:${server.httpServer.address().port}`

const run = (name) => new Promise((resolve) => {
  const start = Date.now()
  const child = spawn(process.execPath, [fileURLToPath(new URL(`${name}.browser.mjs`, import.meta.url))], {
    env: { ...process.env, STEVE_TEST_WEB: base, STEVE_TEST_SHOTS: "" }, stdio: ["ignore", "pipe", "pipe"],
  })
  let out = ""
  child.stdout.on("data", (d) => { out += d }); child.stderr.on("data", (d) => { out += d })
  const timer = setTimeout(() => { out += `\nstopped after ${LIMIT_MS / 1000} s`; child.kill("SIGKILL") }, LIMIT_MS)
  child.on("close", (code) => { clearTimeout(timer); resolve({ name, ok: code === 0, s: ((Date.now() - start) / 1000).toFixed(1), out }) })
})

SMOKE.push("harness-updates", "node-tabs", "markdown-highlight", "person-avatar")

const failed = []
try {
  for (const name of process.argv.slice(2).length ? process.argv.slice(2) : SMOKE) {
    const r = await run(name)
    console.log(`${r.ok ? "ok  " : "FAIL"} ${r.name} (${r.s} s)`)
    if (!r.ok) { failed.push(name); console.log(r.out.trimEnd().replace(/^/gm, "    ")) }
  }
} finally { await server.close() }
if (failed.length) { console.log(`browser smoke: ${failed.length} failed: ${failed.join(", ")}`); process.exit(1) }
console.log("browser smoke: ok")
