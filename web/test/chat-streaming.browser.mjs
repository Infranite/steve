// Real Rust Hub + local Node + offline ACP adapter + production web bundle.
// Build both first. No production directory, model credentials or external host is used.
import assert from "node:assert/strict"
import { spawn } from "node:child_process"
import { createServer } from "node:net"
import { mkdir, mkdtemp, writeFile, open } from "node:fs/promises"
import { resolve } from "node:path"
import { fileURLToPath } from "node:url"

const { chromium } = await import(process.env.PLAYWRIGHT_MODULE || "playwright")
const root = resolve(fileURLToPath(new URL("../..", import.meta.url)))
const records = resolve(root, ".steve/chat-streaming")
await mkdir(records, { recursive: true })
const home = await mkdtemp(resolve(records, "hub-"))
const socket = createServer()
await new Promise((r) => socket.listen(0, "127.0.0.1", r))
const port = socket.address().port
await new Promise((r) => socket.close(r))
assert.notEqual(port, 8440)
const base = `http://127.0.0.1:${port}`
const log = await open(resolve(home, "hub.log"), "w")
const adapter = resolve(root, "hub-rs/crates/steve-hub/tests/fixtures/streaming-acp.js")
const hub = spawn(resolve(root, "hub-rs/target/debug/steve-hub"), [], {
  cwd: root, stdio: ["ignore", log.fd, log.fd],
  env: {
    PATH: process.env.PATH, HOME: home, STEVE_HOME: resolve(home, "steve"),
    STEVE_HOST: "127.0.0.1", STEVE_PORT: String(port), STEVE_PUBLIC_URL: base,
    STEVE_WEB_DIR: resolve(root, "web/dist"), STEVE_DEV_LOGIN: "1",
    STEVE_REPO: resolve(home, "no-project"), STEVE_VAULT_KEYSTORE: "file",
    STEVE_FORBID_REAL_HOSTS: "1", STEVE_PRICE_CATALOG_URL: `${base}/unavailable`,
    STEVE_ADAPTER_CLAUDE: adapter, STEVE_ADAPTER_CODEX: adapter,
    CLAUDE_CODE_EXECUTABLE: "/usr/bin/false", CODEX_EXECUTABLE: "/usr/bin/false",
    STEVE_MODELS_FAST: "test-model", STEVE_MODELS_SMART: "test-model",
    STEVE_MODELS_RESEARCH: "unavailable,test-model",
  },
})
const exited = new Promise((r) => hub.once("exit", r))
const until = async (f, what, ms = 20000) => {
  const start = Date.now()
  while (!(await f())) {
    if (Date.now() - start > ms) throw new Error(`timed out: ${what}; logs: ${home}`)
    await new Promise((r) => setTimeout(r, 50))
  }
}
let browser
const measurements = [], errors = []
try {
  await until(() => fetch(base + "/healthz").then((r) => r.ok).catch(() => false), "Hub ready")
  browser = await chromium.launch({ headless: true, ...(process.env.PLAYWRIGHT_EXECUTABLE ? { executablePath: process.env.PLAYWRIGHT_EXECUTABLE } : {}) })
  for (const [width, theme] of [[1440, "dark"], [390, "light"]]) {
    const context = await browser.newContext({ locale: "zh-CN", viewport: { width, height: 900 }, colorScheme: theme })
    await context.addInitScript(() => localStorage.setItem("steve-onboarded", "1"))
    const page = await context.newPage()
    page.on("pageerror", (e) => errors.push(e.message))
    await page.route("**/*", (route) => new URL(route.request().url()).origin === base ? route.continue() : route.abort())
    await page.goto(base + "/auth/dev")
    await page.goto(base + "/")
    const harness = width === 1440 ? "claude-code" : "codex"
    const settingsStatus = await page.evaluate(async (harness) => {
      const { workspace } = await fetch("/api/state").then((r) => r.json())
      const r = await fetch("/api/settings/models", {
        method: "PATCH", headers: { "content-type": "application/json", "x-steve-ws": workspace.id },
        body: JSON.stringify({ purposes: {
          route: { harness, models: ["test-model"] },
          chat: { harness, models: ["test-model"] },
          research: { harness, models: ["unavailable", "test-model"] },
        } }),
      })
      return { status: r.status, body: await r.json() }
    }, harness)
    assert.equal(settingsStatus.status, 200, JSON.stringify(settingsStatus.body))
    const input = page.locator('[data-composer="chat"] textarea')
    await input.waitFor()
    await input.fill("请解释一个概念，给我一段比较长的回答。")
    const start = Date.now()
    await input.press("Enter")
    const draft = page.locator("[data-chat-draft] .md")
    await until(async () => (await draft.textContent().catch(() => ""))?.includes("首段已经到达"), "first streamed text")
    const first = Date.now() - start
    const mid = await page.locator("[data-chat-draft]").getAttribute("data-chat-draft")
    const state = await page.evaluate(() => fetch("/api/state").then((r) => r.json()))
    assert.equal(state.thread.some((m) => m.author === "steve" && m.meta?.origin === mid), false, "first text is visible before the answer is stored")
    const firstLength = (await draft.textContent()).length
    await page.screenshot({ path: resolve(records, `first-${width}-${theme}.png`) })
    await until(async () => (await draft.textContent().catch(() => ""))?.length > firstLength + 150, "draft keeps growing")
    await page.screenshot({ path: resolve(records, `middle-${width}-${theme}.png`) })
    await until(() => page.locator("[data-chat-draft]").count().then((n) => n === 0), "stored reply replaces draft")
    const done = Date.now() - start
    const final = await page.evaluate(() => fetch("/api/state").then((r) => r.json()))
    const replies = final.thread.filter((m) => m.author === "steve" && m.meta?.origin === mid)
    assert.equal(replies.length, 1)
    assert.match(replies[0].text, /回复生成完毕/)
    assert.equal(replies[0].meta.steps.route.kind, "answer")
    assert.equal(await page.locator(`[data-msg="${replies[0].id}"]`).count(), 1)
    assert.equal(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth), false)
    await page.screenshot({ path: resolve(records, `complete-${width}-${theme}.png`) })
    measurements.push({ width, theme, harness, kind: "answer", first_ms: first, complete_ms: done })
    // Real stop action while text is streaming; the partial reply is saved once.
    await input.fill("再解释一次，我会中途停止。")
    await input.press("Enter")
    await until(async () => (await draft.textContent().catch(() => ""))?.includes("首段已经到达"), "stop has partial text")
    const stoppedMid = await page.locator("[data-chat-draft]").getAttribute("data-chat-draft")
    await page.locator('[data-composer="chat"] [data-stop]').click()
    await until(() => page.locator("[data-chat-draft]").count().then((n) => n === 0), "stop clears live draft")
    await until(async () => {
      const s = await page.evaluate(() => fetch("/api/state").then((r) => r.json()))
      return s.thread.some((m) => m.meta?.origin === stoppedMid && m.meta?.stopped && m.text.includes("首段已经到达"))
    }, "stop preserves generated text")
    // Research takes a different Node job path from a direct answer.
    await input.fill("测试研究：请查阅后给我完整解释。")
    const researchStart = Date.now()
    await input.press("Enter")
    await until(async () => (await draft.textContent().catch(() => ""))?.includes("首段已经到达"), "research streams")
    const researchMid = await page.locator("[data-chat-draft]").getAttribute("data-chat-draft")
    const researchFirst = Date.now() - researchStart
    const researchState = await page.evaluate(() => fetch("/api/state").then((r) => r.json()))
    assert.equal(researchState.thread.some((m) => m.author === "steve" && m.meta?.origin === researchMid), false)
    await page.screenshot({ path: resolve(records, `research-${width}-${theme}.png`) })
    await until(() => page.locator("[data-chat-draft]").count().then((n) => n === 0), "research completes")
    const researchFinal = await page.evaluate(() => fetch("/api/state").then((r) => r.json()))
    const researchReplies = researchFinal.thread.filter((m) => m.author === "steve" && m.meta?.origin === researchMid)
    assert.equal(researchReplies.length, 1)
    assert.match(researchReplies[0].text, /回复生成完毕/)
    assert.equal(researchReplies[0].meta.steps.route.kind, "research")
    assert.equal(researchReplies[0].meta.steps.research.model, "test-model")
    measurements.push({ width, theme, harness, kind: "research", first_ms: researchFirst, complete_ms: Date.now() - researchStart })
    await input.fill("测试研究：这次我会中途停止。")
    await input.press("Enter")
    await until(async () => (await draft.textContent().catch(() => ""))?.includes("首段已经到达"), "research stop has partial text")
    const researchStopMid = await page.locator("[data-chat-draft]").getAttribute("data-chat-draft")
    await page.locator('[data-composer="chat"] [data-stop]').click()
    await until(async () => {
      const s = await page.evaluate(() => fetch("/api/state").then((r) => r.json()))
      return s.thread.some((m) => m.meta?.origin === researchStopMid && m.meta?.steps?.research?.stopped === "user" && m.text.includes("首段已经到达"))
    }, "research stop preserves generated text")
    await context.close()
  }
  assert.deepEqual(errors, [])
  const result = { passed: true, measurements, scenarios: ["real Hub→Node→ACP→SSE→browser", "first text before persistence", "growing long Markdown", "single final replacement", "stop preserves partial answer", "research model fallback and streaming", "desktop dark / mobile light", "no horizontal overflow"], records }
  await writeFile(resolve(records, "result.json"), JSON.stringify(result, null, 2))
  console.log(JSON.stringify(result))
} finally {
  if (browser) await browser.close()
  hub.kill("SIGTERM")
  const force = setTimeout(() => hub.kill("SIGKILL"), 5000)
  await exited
  clearTimeout(force)
  await log.close()
}
