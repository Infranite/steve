// Run against the Vite dev server; Playwright is a test-only external prerequisite.
// PLAYWRIGHT_MODULE may point at an existing installation's index.mjs.
// No requests are allowed to leave localhost; every API is fake.
import assert from "node:assert/strict"
const { chromium } = await import(process.env.PLAYWRIGHT_MODULE || "playwright")
const browser = await chromium.launch({ headless: true, ...(process.env.PLAYWRIGHT_EXECUTABLE ? { executablePath: process.env.PLAYWRIGHT_EXECUTABLE } : {}) })
const base = process.env.STEVE_TEST_WEB || "http://127.0.0.1:5178"
const errors = []
const page = await browser.newPage({ locale: "zh-CN", viewport: { width: 390, height: 844 } })
page.on("pageerror", e => errors.push(e.message))
page.on("console", m => { if (m.type() === "error" && m.text().includes("Base UI:")) errors.push(m.text()) })
let registration = null, starts = 0, reads = 0, retry = 0, deviceCancelled = false, deviceDrop = false, deviceBody
let holdStart = false, releaseStart, sawStart
const pending = () => ({ id: "reg_fixture", brand: "feishu", state: "pending", verification_uri_complete: "https://open.feishu.cn/page/cli?user_code=FAKE", user_code: "FAKE", expires_at: Date.now() + 600_000, interval: 1 })
await page.route("**/*", async route => {
  const req = route.request(), u = new URL(req.url())
  if (u.origin !== base) return route.abort("blockedbyclient")
  if (!u.pathname.startsWith("/api/")) return route.continue()
  let data = {}
  const path = u.pathname
  if (path.endsWith("/registrations/current")) data = registration
  else if (path.endsWith("/registrations") && req.method() === "POST") { starts++; registration = pending(); data = registration }
  else if (path.endsWith("/reg_fixture/cancel")) { registration = { ...registration, state: "cancelled", app_may_exist: true }; data = registration }
  else if (path.endsWith("/reg_fixture/retry")) { retry++; registration = { ...registration, state: "succeeded", error: undefined, connection_id: "con_fixture" }; data = registration }
  else if (path.endsWith("/reg_fixture")) { reads++; data = registration }
  else if (path.endsWith("/device/start")) { deviceBody = req.postDataJSON(); if (holdStart) { sawStart(); await new Promise(resolve => { releaseStart = resolve }) }; data = { id: "dev_fixture", user_code: "FAKE", verification_uri: "https://accounts.feishu.cn", expires_in: 600, interval: 1 } }
  else if (path.endsWith("/dev_fixture/cancel")) { deviceCancelled = true; data = { status: "cancelled" } }
  else if (path.endsWith("/dev_fixture")) { if (deviceDrop) return route.abort("connectionfailed"); data = { status: deviceCancelled ? "cancelled" : "pending" } }
  else if (path.endsWith("/vault/status")) data = { initialized: true }
  else if (path.endsWith("/adapters") || path.endsWith("/personas")) data = []
  else throw new Error(`unexpected API: ${req.method()} ${path}`)
  return route.fulfill({ contentType: "application/json", body: JSON.stringify(data) })
})
try {
  await page.goto(base + "/test/fixtures/feishu.html")
  await page.locator("[data-registration-start]").waitFor()
  await page.locator("[data-registration-start]").click({ timeout: 5000 })
  await page.locator('[data-registration-state="pending"]').waitFor()
  assert.equal(starts, 1)
  if (process.env.STEVE_TEST_ARTIFACTS) {
    await page.screenshot({ path: process.env.STEVE_TEST_ARTIFACTS + "/lark-mobile.png", fullPage: true })
    await page.setViewportSize({ width: 1280, height: 900 })
    await page.screenshot({ path: process.env.STEVE_TEST_ARTIFACTS + "/lark-desktop.png", fullPage: true })
    await page.setViewportSize({ width: 390, height: 844 })
  }
  assert.match(await page.locator("[data-registration-link]").getAttribute("href"), /^https:\/\/open.feishu.cn\//)
  await page.reload()
  await page.locator('[data-registration-state="pending"]').waitFor()
  assert.equal(starts, 1, "reload must restore, not start another application")
  registration = { ...registration, state: "verifying" }
  await page.locator('[data-registration-state="verifying"]').waitFor()
  assert.ok(reads > 0, "the view follows server progress")
  await page.getByRole("button", { name: "取消本次接入" }).click()
  await page.locator('[data-registration-state="cancelled"]').waitFor()
  assert.equal(await page.locator("[data-registration-link]").count(), 0)
  await page.getByRole("button", { name: "高级：OAuth 回调（可选）" }).click()
  assert.equal(await page.locator("[data-redirect-url]").count(), 0, "no location.origin fallback")
  assert.match(await page.locator("body").innerText(), /尚未配置回调地址/)
  assert.equal(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth), false, "mobile must not overflow")
  registration = { ...pending(), state: "credentials_saved", error: "检查失败，凭据已保存" }
  await page.reload()
  await page.getByRole("button", { name: "重试连接检查" }).click()
  await page.locator('[data-registration-state="succeeded"]').waitFor()
  assert.equal(retry, 1)
  assert.equal(starts, 1, "verify retry must not begin another application")
  registration = null
  await page.goto(base + "/test/fixtures/feishu.html?existing")
  assert.equal(await page.getByText(/7\s*\/\s*8/).count(), 0)
  await page.locator("[data-self-device]").click()
  await page.getByRole("button", { name: "继续到官方授权" }).click()
  assert.equal(deviceBody.scopes.includes("im:message.send_as_user"), false)
  await page.locator("[data-device-flow]").waitFor()
  deviceDrop = true
  await page.getByText("暂时无法读取进度，正在重试。").waitFor()
  deviceDrop = false
  await page.getByText("等待你确认；链接过期后可重新开始").waitFor()
  await page.getByRole("button", { name: "稍后继续", exact: true }).click()
  assert.equal(deviceCancelled, false, "closing must not claim to cancel")
  await page.getByRole("button", { name: "继续本人授权", exact: true }).click()
  await page.getByRole("button", { name: "取消授权接入", exact: true }).click()
  await page.locator("[data-device-flow]").waitFor({ state: "detached" })
  assert.equal(deviceCancelled, true)
  await page.goto(base + "/test/fixtures/feishu.html?existing&callback")
  await page.getByRole("button", { name: "高级：OAuth 回调（可选）" }).click()
  assert.equal(await page.locator("[data-redirect-url]").innerText(), "https://steve.example.test/auth/connect/callback")
  await page.locator("[data-self-oauth]").click()
  await page.getByRole("button", { name: "申请读取与代发权限", exact: true }).waitFor()
  await page.getByRole("button", { name: "返回", exact: true }).click()
  await page.goto(base + "/test/fixtures/feishu.html?existing&callback&lark")
  await page.getByRole("button", { name: "高级：OAuth 回调（可选）" }).click()
  await page.getByText("当前应用请使用设备码授权；Lark 的回调授权暂未开放。").waitFor()
  assert.equal(await page.locator("[data-self-oauth]").count(), 0)
  await page.goto(base + "/test/fixtures/feishu.html?race")
  holdStart = true
  const started = new Promise(resolve => { sawStart = resolve })
  await page.locator("[data-self-device]").click()
  await page.getByRole("button", { name: "继续到官方授权" }).click()
  await started
  assert.equal(deviceBody.app_connection, "app_a")
  await page.evaluate(() => window.dispatchEvent(new Event("fixture-switch-app")))
  await page.locator('[data-fixture-app="app_b"]').waitFor()
  releaseStart()
  await page.getByRole("button", { name: "授权你本人", exact: true }).waitFor()
  await page.waitForFunction(() => !document.querySelector("[data-self-device]")?.disabled)
  assert.equal(await page.locator("[data-device-flow]").count(), 0, "late app A response cannot open app B authorization")
  assert.equal(await page.getByRole("button", { name: "继续本人授权", exact: true }).count(), 0)
  assert.deepEqual(errors, [])
  console.log(JSON.stringify({ passed: true, scenarios: ["official link", "reload without duplicate", "cancel", "optional callback", "390px layout", "credential retry", "optional self identity", "device network recovery", "read-only scope choice", "device close versus cancel", "late response after app switch", "configured callback and explicit full-scope consent", "Lark uses device authorization"], starts, reads, retry }))
} catch (e) { console.error(await page.locator("body").innerText(), errors); throw e } finally { await browser.close() }
