// The real Add device dialog with a controlled owner API. Run against Vite with external Playwright.
import assert from "node:assert/strict"
import { mkdir } from "node:fs/promises"
const { chromium } = await import(process.env.PLAYWRIGHT_MODULE || "playwright")
const base = process.env.STEVE_TEST_WEB || "http://127.0.0.1:5198"
const shots = process.env.STEVE_TEST_SHOTS
const browser = await chromium.launch({ headless: true, ...(process.env.PLAYWRIGHT_EXECUTABLE ? { executablePath: process.env.PLAYWRIGHT_EXECUTABLE } : {}) })
const page = await browser.newPage({ locale: "zh-CN", viewport: { width: 734, height: 844 } })
const errors = [], runs = [], cancelled = []
page.on("pageerror", (e) => { errors.push(e.message); console.error(e.stack) })
const first = {
  id: "ni_failed", state: "failed", command: "tailcat serve --ssh-authorized-keys='ssh-ed25519 fixture-first' ssh",
  node_id: null, address: "tcFIRSTtarget0123456789_-x", error: "目标机器上的命令失败（exit status: 255）", failed_step: "send",
  steps: ["probe", "fetch", "send"].map((kind) => ({ kind, text: kind, at: 1, ended: 2, elapsed_ms: kind === "send" ? 268000 : 1000 })),
}
let installs = [{ ...first }], serial = 0, cancelError = false, tailcatEnabled = false
const json = (route, body, status = 200) => route.fulfill({ status, contentType: "application/json", body: JSON.stringify(body) })
await page.route("**/*", async (route) => {
  const req = route.request(), path = new URL(req.url()).pathname
  if (!path.startsWith("/api/")) return route.continue()
  if (path === "/api/nodes/installs") {
    if (req.method() === "GET") return json(route, { installs })
    const op = { id: `ni_new_${++serial}`, state: "waiting", command: `tailcat serve --ssh-authorized-keys='ssh-ed25519 fixture-new-${serial}' ssh`, steps: [], node_id: null, error: null, address: null }
    installs.unshift(op)
    return json(route, op)
  }
  const match = path.match(/^\/api\/nodes\/installs\/([^/]+)(\/run)?$/)
  if (match) {
    const op = installs.find((o) => o.id === match[1])
    if (req.method() === "DELETE") {
      if (cancelError) return json(route, { error: "无法取消：请重试" }, 500)
      cancelled.push(match[1]); installs = installs.filter((o) => o.id !== match[1])
      return json(route, { ok: true })
    }
    if (!op) return json(route, { error: "not found" }, 404)
    if (match[2]) {
      const { address } = req.postDataJSON()
      runs.push({ id: op.id, address })
      Object.assign(op, { state: "running", address, error: null, failed_step: null, steps: [{ kind: "probe", text: "正在连接目标机器", at: Date.now(), elapsed_ms: 0 }] })
    }
    return json(route, op)
  }
  if (path === "/api/nodes/tailcat") return json(route, { available: true, enabled: tailcatEnabled, running: tailcatEnabled, hub_url: tailcatEnabled ? "tailcat:tcHUBfixture0123456789:22" : null })
  if (path === "/api/nodes/pairings") return json(route, { code: "stv1-fixture-code", expires: Date.now() + 600000 })
  if (path === "/api/nodes") return json(route, { nodes: [] })
  if (path === "/api/vitals") return json(route, { nodes: {} })
  if (path === "/api/access") return json(route, { tokens: [], clients: [], current: null, hub: {} })
  return json(route, {}, 404)
})
const dialog = page.locator("[data-add-device]")
const open = async () => {
  await page.locator("[data-pair]").click()
  await page.locator('[data-add-via="install"]').click()
  await dialog.locator('button[type="submit"]').click()
  await page.locator("[data-remote-install]").waitFor()
}
const shot = async (name) => {
  if (!shots) return
  await mkdir(shots, { recursive: true })
  await page.screenshot({ path: `${shots}/${name}.png`, fullPage: true, animations: "disabled" })
}
try {
  await page.goto(base + "/test/fixtures/devices.html?strict#/devices/nodes")
  await open()
  await page.locator("[data-install-failed]").getByText(/exit status: 255/).waitFor()
  assert.equal(await page.locator("[data-install-address]").inputValue(), first.address)
  await page.keyboard.press("Escape")
  await dialog.waitFor({ state: "detached" })
  await open()
  await page.locator("[data-install-failed]").waitFor()
  await shot("failed-restored")
  assert.equal(await page.locator("[data-install-cancel]").count(), 1, "a restored failure needs an explicit way to cancel, even while Finish is disabled")

  // Retrying keeps the same key and target; a running install is still resumed on reopening.
  await page.locator("[data-install-run]").click()
  await page.locator("[data-install-failed]").waitFor({ state: "detached" })
  assert.deepEqual(runs, [{ id: first.id, address: first.address }])
  assert.equal(await page.locator("[data-install-cancel]").count(), 0)
  await page.keyboard.press("Escape"); await dialog.waitFor({ state: "detached" })
  await open()
  assert.equal(await page.locator("[data-install-run]").count(), 0)
  assert.deepEqual(cancelled, [], "closing a running install does not cancel it")
  installs[0] = { ...first }
  await page.locator("[data-install-failed]").waitFor()

  // A failed cancellation leaves the attempt retryable, and does not close the dialog.
  cancelError = true
  await page.locator("[data-install-cancel]").click()
  await dialog.getByRole("alert").getByText("无法取消：请重试", { exact: true }).waitFor()
  assert.equal(await page.locator("[data-install-address]").inputValue(), first.address)
  cancelError = false
  await page.locator("[data-install-cancel]").click()
  await dialog.waitFor({ state: "detached" })
  assert.deepEqual(cancelled, [first.id])

  // The next machine gets a fresh command, no previous error or address, and a different install.
  await open()
  assert.equal(installs.length, 1, "effect replay does not create a second install")
  assert.equal(await page.locator("[data-install-address]").inputValue(), "")
  assert.equal(await page.locator("[data-install-failed]").count(), 0)
  assert.match(await page.locator("[data-install-command]").textContent(), /fixture-new-1/)
  await shot("next-pairing")
  await page.locator("[data-install-address]").fill("tcNEXTtarget0123456789_-x")
  await page.locator("[data-install-run]").click()
  await page.locator("[data-install-address]").waitFor({ state: "detached" })
  assert.deepEqual(runs[1], { id: "ni_new_1", address: "tcNEXTtarget0123456789_-x" })
  await shot("next-started")

  // A real browser must show all the transfer facts at phone width without truncating them
  // in the Agent Trail's single-line status area.
  await page.setViewportSize({ width: 390, height: 844 })
  const current = installs[0]
  current.transfer_route = "derp_observed"
  current.steps = [
    { kind: "probe", text: "连接目标机器", ended: 1, elapsed_ms: 40 },
    { kind: "fetch", text: "获取并校验安装包", ended: 1, elapsed_ms: 94000 },
    { kind: "send", text: "正在把安装包发到目标机器", elapsed_ms: 150000,
      progress: { phase: "send", done: 51 * 2 ** 20, total: 60 * 2 ** 20, rate: 356515, eta_secs: 27 } },
  ]
  const transfer = page.locator("[data-install-transfer]")
  await transfer.getByText("tailcat SSH · 观测到 DERP 中继", { exact: true }).waitFor()
  for (const fact of ["51.0 MB / 60.0 MB", "348 KB/s", "预计还需 27 s 传完"]) {
    const item = transfer.getByText(fact, { exact: true })
    await item.waitFor()
    assert.ok(await item.evaluate(el => el.scrollWidth <= el.clientWidth + 1), fact)
  }
  await shot("relayed-transfer")
  current.transfer_route = "direct_observed"
  const send = current.steps.at(-1)
  Object.assign(send, { transfer_ms: 152000, unpack_ms: 400,
    progress: { phase: "unpack", done: 60 * 2 ** 20, total: 60 * 2 ** 20, rate: null } })
  await transfer.getByText("tailcat SSH · 观测到直连", { exact: true }).waitFor()
  await transfer.getByText("传输：2:32", { exact: true }).waitFor()
  await transfer.getByText("目标机解压：<1 秒", { exact: true }).waitFor()
  assert.equal(await transfer.getByText(/预计还需/).count(), 0)
  await shot("target-extraction")

  // Waiting attempts can also be cancelled, including after Back and a remount of the install step.
  installs = []
  await page.keyboard.press("Escape"); await dialog.waitFor({ state: "detached" })
  await open()
  await dialog.getByRole("button", { name: "返回", exact: true }).click()
  await dialog.locator('button[type="submit"]').click()
  await page.locator("[data-install-cancel]").click()
  await dialog.waitFor({ state: "detached" })
  assert.deepEqual(cancelled, [first.id, "ni_new_2"])

  // A new computer can install the CLI before following the setup command, over either route.
  for (const via of ["direct", "tailcat"]) {
    tailcatEnabled = via === "tailcat"
    await page.locator("[data-pair]").click()
    await page.locator(`[data-add-via="${via}"]`).click()
    await dialog.locator('button[type="submit"]').click()
    await page.locator("[data-setup-command]").waitFor()
    assert.match(await page.locator("[data-install-steve-command]").textContent(), /brew install infranite\/tap\/steve/)
    assert.ok(await page.locator("[data-install-steve]").evaluate((el) => !!(el.compareDocumentPosition(document.querySelector("[data-setup-command]")) & Node.DOCUMENT_POSITION_FOLLOWING)), "installation instructions precede setup")
    assert.equal(await page.locator('[data-install-steve] a').getAttribute("href"), "https://brew.sh/")
    if (via === "tailcat") assert.match(await page.locator("[data-setup-command]").textContent(), /steve setup node --hub tailcat:/)
    for (const width of [734, 390]) {
      await page.setViewportSize({ width, height: 844 })
      assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), "pairing guidance fits the viewport")
      await shot(`${via}-homebrew-${width}`)
    }
    await page.keyboard.press("Escape"); await dialog.waitFor({ state: "detached" })
  }
  assert.deepEqual(errors, [])
  console.log("Remote install: failure restored, same-target retry, running resume, cancellation error/retry, fresh next-machine pairing, waiting cancellation, and Homebrew guidance before setup on both connection routes passed")
} catch (e) {
  await shot("failure")
  e.message += `\nPage errors: ${errors.join("; ") || "none"}`
  throw e
} finally { await browser.close() }
