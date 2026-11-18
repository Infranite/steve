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
const nodeMode = process.env.STEVE_TEST_NODE_MODE || "local"
assert.ok(["local", "off"].includes(nodeMode))
const records = resolve(root, nodeMode === "off" ? ".steve/chat-streaming-off" : ".steve/chat-streaming")
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
const hub = spawn(process.env.STEVE_TEST_HUB_BIN ?? resolve(root, "hub-rs/target/debug/steve-hub"), [], {
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
    STEVE_NODE_MODE: nodeMode,
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
const position = (loc) => loc.evaluate(el => ({ top: el.scrollTop, height: el.clientHeight, total: el.scrollHeight }))
let browser
const measurements = [], errors = []
try {
  await until(() => fetch(base + "/healthz").then((r) => r.ok).catch(() => false), "Hub ready")
  browser = await chromium.launch({ headless: true, ...(process.env.PLAYWRIGHT_EXECUTABLE ? { executablePath: process.env.PLAYWRIGHT_EXECUTABLE } : {}) })
  for (const [width, theme] of [[1440, "dark"], [390, "light"]]) {
    const context = await browser.newContext({ locale: "zh-CN", viewport: { width, height: 900 }, colorScheme: theme, hasTouch: width === 390 })
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
    // the reply being written, not the live steps above it (their notes are Markdown too)
    const draft = page.locator("[data-chat-draft] .md:not([data-live-steps] .md)")
    await until(async () => (await draft.textContent().catch(() => ""))?.includes("首段已经到达"), "first streamed text")
    const first = Date.now() - start
    const mid = await page.locator("[data-chat-draft]").getAttribute("data-chat-draft")
    const state = await page.evaluate(() => fetch("/api/state").then((r) => r.json()))
    assert.equal(state.thread.some((m) => m.author === "steve" && m.meta?.origin === mid), false, "first text is visible before the answer is stored")
    const firstLength = (await draft.textContent()).length
    const outer = page.locator("[data-chat-scroll]")
    const atBottom = () => position(outer).then(({ top, height, total }) => total - top - height < 4)
    await until(atBottom, "near the bottom while following")
    await page.screenshot({ path: resolve(records, `first-${width}-${theme}.png`) })
    await until(async () => (await draft.textContent().catch(() => ""))?.length > firstLength + 150, "draft keeps growing")
    await page.screenshot({ path: resolve(records, `middle-${width}-${theme}.png`) })
    // Deliberate scroll away: the answer must keep growing without moving the outer scroll position.
    await until(() => position(outer).then(({ top }) => top > 240), "enough content to scroll away")
    await outer.hover()
    await page.mouse.wheel(0, -210)
    const jumpLatest = page.locator("[data-new-messages], [data-jump-latest]").first()
    await jumpLatest.waitFor()
    const readAt = (await position(outer)).top, lengthBefore = (await draft.textContent()).length
    await until(async () => (await draft.textContent().catch(() => ""))?.length > lengthBefore + 160, "draft grows while reading above")
    assert.ok(Math.abs((await position(outer)).top - readAt) < 3, "streaming must not pull the reader down")
    await jumpLatest.click()
    await until(atBottom, "jump to latest restores following")
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
    await input.fill("请再给我一段长回答，我会中途停止。")
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
    const interruptions = page.locator("[data-chat-interruption]:not([data-chat-interruption=user])")
    const interruptionsBefore = await interruptions.count()
    await input.fill("测试研究：进度夹具，请查阅后给我完整解释。")
    const researchStart = Date.now()
    await input.press("Enter")
    const live = page.locator("[data-research-live]")
    await live.getByText("Steve · 正在搜索", { exact: true }).waitFor()
    const toggle = live.locator("[data-steps-toggle]")
    assert.equal(await toggle.getAttribute("aria-expanded"), "false")
    assert.equal(await live.locator("[data-card-row]").count(), 0, "no calls mounted in the folded state")
    await page.screenshot({ path: resolve(records, `progress-${width}-${theme}.png`) })
    await toggle.click()
    const process = live.locator("[data-chat-process-scroll]")
    const cards = process.locator("[data-card-row]")
    await until(() => cards.count().then(n => n >= 26), "25 calls plus the retained failure")
    const { height, total } = await position(process)
    assert.ok(height <= 288 && height >= 180 && total > height * 2, `bounded call list: ${height}/${total}`)
    const visibleRows = await process.evaluate(el => {
      const region = el.getBoundingClientRect()
      return [...el.querySelectorAll("[data-card-row]")].filter(row => {
        const box = row.getBoundingClientRect()
        return box.top >= region.top - 1 && box.bottom <= region.bottom + 1
      }).length
    })
    assert.ok(visibleRows >= 5 && visibleRows <= 8, `visible whole calls: ${visibleRows}`)
    await until(() => process.locator("[data-recovered]").count().then(n => n === 1), "only the same call recovered")
    assert.equal(await process.locator("[data-step-status=error]").count(), 1, "a success by another call must not clear the remaining failure")
    assert.equal(await interruptions.count(), interruptionsBefore, "a failed call is not a failed turn")
    await process.locator("[data-recovered] .ui-tclc__head").click()
    await process.locator("[data-recovered] .ui-tclc__body").getByText("同一搜索参数", { exact: true }).waitFor()
    await process.locator("[data-recovered] .ui-tclc__head").click()
    await page.screenshot({ path: resolve(records, `calls-${width}-${theme}.png`) })
    await process.hover()
    const parentTop = (await position(outer)).top
    const cdp = width === 390 ? await context.newCDPSession(page) : null
    const box = await process.boundingBox()
    const scrollCalls = async (delta) => {
      if (cdp) await cdp.send("Input.synthesizeScrollGesture", {
        x: Math.round(box.x + box.width / 2), y: Math.round(box.y + box.height / 2),
        yDistance: -delta, speed: 3400, gestureSourceType: "touch", preventFling: true,
      })
      else await page.mouse.wheel(0, delta)
    }
    await scrollCalls(1500)
    await until(() => position(process).then(({ top, height, total }) => total - top - height < 5), "scroll inside to the last call")
    await scrollCalls(450)
    await process.getByText("末条搜索", { exact: true }).waitFor()
    assert.ok(Math.abs((await position(outer)).top - parentTop) < 3, "overscroll in the call list does not move the conversation")
    await cdp?.detach()
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
    const finalMsg = page.locator(`[data-msg="${researchReplies[0].id}"]`)
    const finalList = finalMsg.locator("[data-chat-process-scroll]")
    assert.equal(await finalMsg.locator("[data-steps-toggle]").getAttribute("aria-expanded"), "true", "the run keeps manual expansion across persistence")
    assert.ok((await position(finalList)).top > 0, "preserves the inner position")
    assert.equal(await finalList.locator("[data-recovered]").count(), 1)
    assert.equal(await finalList.locator("[data-step-status=error]").count(), 1)
    await finalMsg.locator("[data-steps-toggle]").click()
    await jumpLatest.click()
    await until(atBottom, "latest reply takes the main position")
    await page.screenshot({ path: resolve(records, `result-${width}-${theme}.png`) })
    measurements.push({ width, theme, harness, kind: "research", calls: 25, visible_rows: visibleRows, detail_height: height, first_ms: researchFirst, complete_ms: Date.now() - researchStart })
    await input.fill("测试研究：这次我会中途停止。")
    await input.press("Enter")
    await until(async () => (await draft.textContent().catch(() => ""))?.includes("首段已经到达"), "research stop has partial text")
    const researchStopMid = await page.locator("[data-chat-draft]").getAttribute("data-chat-draft")
    await page.locator('[data-composer="chat"] [data-stop]').click()
    await until(async () => {
      const s = await page.evaluate(() => fetch("/api/state").then((r) => r.json()))
      return s.thread.some((m) => m.meta?.origin === researchStopMid && m.meta?.steps?.research?.stopped === "user" && m.text.includes("首段已经到达"))
    }, "research stop preserves generated text")
    await input.fill("测试研究：阻塞夹具，请查阅后回复。")
    await input.press("Enter")
    await until(() => interruptions.count().then(n => n === interruptionsBefore + 1), "this turn is blocked")
    const interruption = page.locator("[data-chat-interruption=failed]").last()
    assert.match(await interruption.innerText(), /本轮未完成/)
    await page.screenshot({ path: resolve(records, `blocked-${width}-${theme}.png`) })
    await input.fill("确认夹具：请确认我要看哪份记录。")
    await input.press("Enter")
    const awaiting = page.locator("[data-chat-awaiting]")
    await awaiting.waitFor()
    assert.match(await awaiting.innerText(), /待确认/)
    await page.screenshot({ path: resolve(records, `awaiting-${width}-${theme}.png`) })
    await awaiting.getByRole("button", { name: "本周记录" }).click()
    await awaiting.waitFor({ state: "hidden" })
    await until(async () => {
      const s = await page.evaluate(() => fetch("/api/state").then(r => r.json()))
      return s.thread.some(m => m.author !== "steve" && m.text === "本周记录")
    }, "the choice is sent to the conversation")
    await context.close()
  }
  assert.deepEqual(errors, [])
  const result = { passed: true, node_mode: nodeMode, measurements, scenarios: ["real Hub→Node→ACP→SSE→browser", "first text before persistence", "reading position while streaming and following again", "single final replacement", "stop preserves partial answer", "25 calls in a bounded independently scrolling list", "same call recovered; another remains failed", "blocked turn", "confirmation and answer", "research model fallback and streaming", "desktop dark / mobile light", "no horizontal overflow"], records }
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
