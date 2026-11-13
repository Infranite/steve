// Vite-only fixture; external Playwright prerequisite, all API calls mocked.
import assert from "node:assert/strict"
import { mkdir } from "node:fs/promises"
const { chromium } = await import(process.env.PLAYWRIGHT_MODULE || "playwright")
const browser = await chromium.launch({ headless: true, ...(process.env.PLAYWRIGHT_EXECUTABLE ? { executablePath: process.env.PLAYWRIGHT_EXECUTABLE } : {}) })
const base = process.env.STEVE_TEST_WEB || "http://127.0.0.1:5245"
const shots = process.env.STEVE_TEST_SHOTS
if (shots) await mkdir(shots, { recursive: true })
const GB = 2 ** 30
const entry = (item, title, o = {}) => ({ item, workspace: "w", title, status: "run", phase: "active", archived: false, branch: `steve/${item.toLowerCase()}`, path: `/home/example/.steve/worktrees/${item}`, exists: true, bytes: GB, cache_bytes: 0, caches: [], busy: false, in_use: false, updated: 0, ...o })
const report = (free, pressure, closedCache) => ({
  root: "/home/example/.steve/worktrees", free, total: 1000 * GB, pressure, policy: { floor: 20 * GB, low: 60 * GB, idle_ms: 24 * 3_600_000 }, git: "2.54", trash: 0, measured_at: 0,
  entries: [
    entry("ST-41", "统计页按小时展示今天的趋势和一个特别长的标题用来检查窄屏截断", { bytes: 17.9 * GB, cache_bytes: 17.8 * GB, caches: ["hub-rs/target", "node_modules", "web/node_modules"], busy: true }),
    entry("ST-36", "已放弃的整改", { phase: "closed", status: "drop", bytes: closedCache + 0.02 * GB, cache_bytes: closedCache, caches: closedCache ? ["hub-rs/target", "node_modules"] : [] }),
    entry("ST-38", "开发服务器还开着", { phase: "closed", status: "done", bytes: 0.6 * GB, cache_bytes: 0.58 * GB, caches: ["node_modules"], in_use: true }),
    entry("ST-30", "已回收的工作区", { phase: "closed", status: "done", exists: false, bytes: 0 }),
  ],
  orphans: [{ path: "/home/example/.steve/worktrees/OLD-1", bytes: 0.3 * GB }],
  scratch: [
    entry("ST-54", "没有项目的调研，装了一堆依赖", { phase: "closed", status: "done", branch: null, path: "/home/example/.steve/scratch/ST-54", bytes: 3.2 * GB, cache_bytes: 2.1 * GB, caches: ["node_modules", "venv/target"] }),
    entry("ST-60", "进行中的临时事项", { branch: null, path: "/home/example/.steve/scratch/ST-60", bytes: 0.2 * GB, cache_bytes: 0, caches: [] }),
  ],
})
const outbox = () => ({ pending: 3, dead: revived ? 0 : 3, oldest: 1, groups: revived ? [] : [
  { consumer: "github.after", topic: "github.after", count: 2, last_error: "GitHub 503 Service Unavailable: a long body ".repeat(8), updated: 2 },
  { consumer: "automation.item_trigger", topic: "work.item.done", count: 1, last_error: null, updated: 1 },
] })
let revived = false, revivedBy = []
let state = report(12 * GB, "floor", 11 * GB), reclaims = 0, forbid = false
const errors = []
try {
  for (const width of [1440, 390]) for (const theme of ["light", "dark"]) {
    const page = await browser.newPage({ locale: "zh-CN", viewport: { width, height: width === 390 ? 844 : 1000 }, reducedMotion: "reduce", colorScheme: theme })
    page.on("pageerror", e => errors.push(e.message))
    state = report(12 * GB, "floor", 11 * GB)
    revived = false
    await page.route(u => new URL(u).pathname.startsWith("/api/"), async route => {
      const req = route.request(), u = new URL(req.url())
      if (forbid) return route.fulfill({ status: 403, contentType: "application/json", body: JSON.stringify({ error: "仅实例管理员可查看和回收工作区" }) })
      if (u.pathname === "/api/system/workspaces" && req.method() === "GET") return route.fulfill({ contentType: "application/json", body: JSON.stringify(state) })
      if (u.pathname === "/api/system/outbox" && req.method() === "GET") return route.fulfill({ contentType: "application/json", body: JSON.stringify(outbox()) })
      if (u.pathname === "/api/system/outbox/revive" && req.method() === "POST") {
        revivedBy.push(JSON.parse(req.postData() || "{}").consumer)
        revived = true
        return route.fulfill({ contentType: "application/json", body: JSON.stringify({ revived: 2 }) })
      }
      if (u.pathname === "/api/system/workspaces/reclaim" && req.method() === "POST") {
        reclaims++
        state = report(23 * GB, "low", 0)
        return route.fulfill({ contentType: "application/json", body: JSON.stringify({ skipped: false, free_before: 12 * GB, free_after: 23 * GB, freed: [{ item: "ST-36", caches: ["hub-rs/target", "node_modules"], bytes: 11 * GB }], removed: [], kept: [{ item: "ST-36", reason: "有未提交的改动，保留工作区" }, { item: "ST-38", reason: "有进程还在工作区里运行" }] }) })
      }
      throw new Error(`unexpected API ${req.method()} ${u.pathname}`)
    })
    await page.goto(base + "/test/fixtures/disk.html")
    await page.locator('[data-disk-pressure="floor"]').waitFor()
    assert.match(await page.locator("[data-disk-pressure]").innerText(), /只剩 12 GB，低于下限 20 GB/)
    assert.deepEqual(await page.locator("[data-disk-entry]:not([data-disk-scratch])").evaluateAll(xs => xs.map(x => x.getAttribute("data-disk-entry"))), ["ST-41", "ST-36", "ST-38"], "largest first; the reclaimed one only counts")
    assert.equal(await page.locator('[data-disk-entry="ST-38"]').getAttribute("data-disk-state"), "in_use")
    /* scratch directories of items without a project: their own list and summary */
    assert.deepEqual(await page.locator("[data-disk-scratch-list] [data-disk-entry]").evaluateAll(xs => xs.map(x => x.getAttribute("data-disk-entry"))), ["ST-54", "ST-60"])
    assert.match(await page.locator("[data-disk-scratch-total]").innerText(), /临时目录[\s\S]*2 个没有项目的事项，缓存 2\.1 GB，其中已结束事项的 2\.1 GB 在磁盘紧张时回收/)
    /* dead background deliveries, one row per consumer */
    await page.locator("[data-outbox-dead]").waitFor()
    assert.equal(await page.locator("[data-outbox-group]").count(), 2)
    const overflow = await page.evaluate(() => document.documentElement.scrollWidth > document.documentElement.clientWidth)
    assert.equal(overflow, false, `no horizontal overflow at ${width}px`)
    if (shots) await page.screenshot({ path: `${shots}/disk-${width}-${theme}-floor.png`, fullPage: true })
    await page.locator('[data-outbox-revive="github.after"]').click()
    await page.locator("[data-toast-id]").getByText("已重新投递 2 条").waitFor() // the toast; its screen-reader copy says the same
    await page.locator("[data-outbox-dead]").waitFor({ state: "detached" })
    await page.locator("[data-disk-reclaim]").click()
    await page.locator("[data-toast-id]").getByText("释放了 11 GB，2 个按规则保留").waitFor()
    await page.locator('[data-disk-pressure="low"]').waitFor()
    if (shots) await page.screenshot({ path: `${shots}/disk-${width}-${theme}-after.png`, fullPage: true })
    await page.close()
  }
  /* someone who is not the instance owner sees nothing of this machine */
  forbid = true
  const page = await browser.newPage({ locale: "zh-CN" })
  await page.route(u => new URL(u).pathname.startsWith("/api/"), route => route.fulfill({ status: 403, contentType: "application/json", body: JSON.stringify({ error: "仅实例管理员可查看和回收工作区" }) }))
  await page.goto(base + "/test/fixtures/disk.html")
  await page.waitForTimeout(500)
  assert.equal(await page.locator("[data-workspace-disk]").count(), 0)
  assert.equal(reclaims, 4)
  assert.deepEqual(revivedBy, ["github.after", "github.after", "github.after", "github.after"])
  assert.deepEqual(errors, [])
  console.log("disk.browser: ok")
} finally {
  await browser.close()
}
