// Real DevicesView with an isolated owner API. Run against Vite, with external Playwright.
import assert from "node:assert/strict"
import { mkdir } from "node:fs/promises"
const { chromium } = await import(process.env.PLAYWRIGHT_MODULE || "playwright")
const browser = await chromium.launch({ headless: true, ...(process.env.PLAYWRIGHT_EXECUTABLE ? { executablePath: process.env.PLAYWRIGHT_EXECUTABLE } : {}) })
const base = process.env.STEVE_TEST_WEB || "http://127.0.0.1:5198"
const page = await browser.newPage({ locale: "zh-CN", viewport: { width: 1360, height: 980 } })
const errors = [], writes = []
page.on("pageerror", e => errors.push(e.message))
const remote = { node_id: "fixture-remote", name: "Remote laptop", fingerprint: "fixture-fingerprint", state: "active", linked: true, local: false, max_sessions: 32, running: 32, waiting: 2, last_seen: Date.now() - 60_000 }
let nodes = [{ ...remote }, { ...remote }, { ...remote, node_id: "fixture-local", local: true }]
let status = 200, release, reads = 0
const held = new Promise(resolve => { release = resolve })
let hold = true
await page.route("**/*", async route => {
  const request = route.request(), url = new URL(request.url())
  if (url.origin !== base) return route.abort("blockedbyclient")
  if (!url.pathname.startsWith("/api/")) return route.continue()
  if (request.method() !== "GET") writes.push(request.method() + " " + url.pathname)
  if (url.pathname === "/api/nodes/fixture-remote/limit" && request.method() === "POST") {
    const { max_sessions } = request.postDataJSON()
    assert.ok(Number.isInteger(max_sessions) && max_sessions >= 1 && max_sessions <= 256)
    nodes = nodes.map(n => n.node_id === remote.node_id ? { ...n, max_sessions } : n)
    return route.fulfill({ status: 200, contentType: "application/json", body: JSON.stringify({ ok: true }) })
  }
  if (url.pathname === "/api/nodes") {
    reads++
    assert.equal(request.headers()["x-steve-ws"], "fixture-workspace")
    if (hold) await held
    return route.fulfill({ status, contentType: "application/json", body: JSON.stringify(status === 200 ? { nodes } : { error: "fixture: node list unavailable" }) })
  }
  // OwnerChip's optional contacts probe is deliberately not wired in this fixture.
  return route.fulfill({ status: 404, contentType: "application/json", body: "{}" })
})
const row = () => page.locator('[data-node-row="fixture-remote"]')
const refresh = () => page.locator("[data-refresh-nodes]").click()
try {
  await page.goto(base + "/test/fixtures/devices.html?legacy")
  await page.locator("[data-nodes-loading]").waitFor()
  assert.equal(await page.getByText("还没有其他设备", { exact: true }).count(), 0, "loading is not an empty inventory")
  hold = false; release()
  await row().waitFor()
  assert.equal(await page.locator("[data-node-row]").count(), 1, "local Node and duplicate identities are not extra devices")
  assert.equal(await page.getByText("Legacy duplicate", { exact: true }).count(), 0, "exact legacy identity is deduplicated")
  await page.getByText("其他设备 · 1", { exact: true }).waitFor()
  await row().getByText("已启用", { exact: true }).waitFor()
  await row().getByText("已连接", { exact: true }).waitFor()
  assert.equal(await page.getByText("全部在 Hub", { exact: true }).count(), 0)
  await row().click()
  const detail = page.locator("[data-node-detail]")
  await detail.getByText(remote.fingerprint, { exact: true }).waitFor()
  await detail.getByText("最近连接", { exact: true }).waitFor()
  assert.equal(await detail.getByText("Hub-only harness", { exact: true }).count(), 0)
  assert.equal(await detail.locator("[data-owner-chip]").count(), 0)
  await page.keyboard.press("Escape")

  nodes = [{ ...remote, linked: false }]
  await refresh()
  await row().getByText("未连接", { exact: true }).waitFor()
  await row().getByText("已启用", { exact: true }).waitFor()
  for (const state of ["pending", "paused", "revoked"]) {
    nodes = [{ ...remote, state }]
    await refresh()
    await row().getByText({ pending: "等待确认", paused: "已暂停", revoked: "已撤销" }[state], { exact: true }).waitFor()
  }
  status = 503
  await refresh()
  await page.locator("[data-node-error]").waitFor()
  await row().getByText("连接状态待刷新", { exact: true }).waitFor()
  await row().getByText("已撤销 · 上次读取", { exact: true }).waitFor()
  assert.equal(await page.getByText("还没有其他设备", { exact: true }).count(), 0)
  await page.reload()
  await page.locator("[data-node-error]").waitFor()
  assert.equal(await page.getByText("还没有其他设备", { exact: true }).count(), 0, "initial failure is not empty")
  status = 200; nodes = []
  await page.goto(base + "/test/fixtures/devices.html")
  await page.getByText("还没有其他设备", { exact: true }).waitFor()
  nodes = [{ ...remote }]
  const beforePoll = reads
  await row().waitFor({ timeout: 13_000 })
  assert.ok(reads > beforePoll, "visible inventory refreshes without a page reload")
  await page.getByText("其他设备 · 1", { exact: true }).waitFor()

  if (process.env.STEVE_TEST_SHOTS) {
    await mkdir(process.env.STEVE_TEST_SHOTS, { recursive: true })
    await page.screenshot({ path: process.env.STEVE_TEST_SHOTS + "/devices-desktop.png", fullPage: true, animations: "disabled" })
  }
  await page.setViewportSize({ width: 390, height: 844 })
  assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), "mobile has no horizontal overflow")
  await row().click(); await detail.getByText(remote.fingerprint, { exact: true }).waitFor()
  if (process.env.STEVE_TEST_SHOTS) await page.screenshot({ path: process.env.STEVE_TEST_SHOTS + "/devices-mobile.png", fullPage: true, animations: "disabled" })
  assert.deepEqual(writes, [], "inventory and identity inspection never mutate pairing or permissions")
  // Capacity changes are explicit saves, not new pairing or execution approvals.
  const capacity = detail.locator("[data-session-limit-input]")
  await capacity.waitFor()
  assert.equal(await capacity.getAttribute("aria-valuenow"), "32")
  assert.match(await detail.locator("[data-limit-semantics]").textContent(), /无需重启.*不打断/)
  await capacity.fill("64"); await capacity.blur()
  assert.deepEqual(writes, [], "editing a capacity does not save until requested")
  await detail.locator("[data-session-limit-save]").click()
  await detail.locator('[data-session-limit="64"]').waitFor()
  assert.deepEqual(writes, ["POST /api/nodes/fixture-remote/limit"])
  assert.equal(await detail.locator("[data-running]").getAttribute("data-running"), "32")
  assert.equal(await detail.locator("[data-running]").getAttribute("data-waiting"), "2")
  for (const width of [1440, 1066, 390]) {
    for (const dark of [false, true]) {
      await page.setViewportSize({ width, height: 980 })
      await page.evaluate(dark => document.documentElement.classList.toggle("dark", dark), dark)
      assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), "capacity controls do not overflow")
      assert.ok(await detail.evaluate(el => el.scrollWidth <= el.clientWidth), "capacity panel fits the dialog")
      if (process.env.STEVE_TEST_SHOTS) await page.screenshot({ path: `${process.env.STEVE_TEST_SHOTS}/capacity-${width}-${dark ? "dark" : "light"}.png`, fullPage: true, animations: "disabled" })
    }
  }
  assert.deepEqual(errors, [])
  process.stdout.write("Device inventory: visible owner Nodes, states, loading/failure/empty, refresh, deduplication, details and mobile passed\n")
} finally { await browser.close() }
