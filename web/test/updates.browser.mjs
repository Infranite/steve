// Vite-only fixture; external Playwright prerequisite, all API/provider calls mocked.
import assert from "node:assert/strict"
const { chromium } = await import(process.env.PLAYWRIGHT_MODULE || "playwright")
const browser = await chromium.launch({ headless: true, ...(process.env.PLAYWRIGHT_EXECUTABLE ? { executablePath: process.env.PLAYWRIGHT_EXECUTABLE } : {}) })
const base = process.env.STEVE_TEST_WEB || "http://127.0.0.1:5189"
const page = await browser.newPage({ viewport: { width: 390, height: 844 } })
const errors = [], writes = []
let navigations = 0
page.on("framenavigated", frame => { if (frame === page.mainFrame()) navigations++ })
page.on("pageerror", e => errors.push(e.message))
const check = status => ({ status, checked_at: Date.now(), version: "v1.2.0", release_url: "https://github.com/Infranite/steve/releases/tag/v1.2.0", notes: "测试更新说明", error: null })
let state = { repository: "Infranite/steve", current: { hub: "v1.1.0", web: "v1.1.0" }, capability: { managed: false, can_execute: false, reason: "source checkout" }, check: null, operation: null }
let denyExecute = false, unrecordedPost = false, failCheck = false, hangStatus = false
let disconnected = false, dropPost = false, checkStatus = "available", approvals = 0, rejections = 0, gets = 0
const operation = (phase = "staging") => ({ id: "up_fixture", idempotency_key: "agent-request", version: "v1.2.0", phase, requested_by: "test-agent", created_at: Date.now(), updated_at: Date.now(), error: null, previous: "v1.1.0", target: "v1.2.0" })
await page.route("**/*", async route => {
  const req = route.request(), u = new URL(req.url())
  if (u.origin !== base) return route.abort("blockedbyclient")
  if (!u.pathname.startsWith("/api/")) return route.continue()
  let data
  if (u.pathname === "/api/updates" && req.method() === "GET") { gets++; if (hangStatus) return; if (disconnected) return route.abort("connectionfailed"); data = state }
  else if (u.pathname === "/api/updates/check") { if (failCheck) return route.fulfill({ status: 403, contentType: "application/json", body: JSON.stringify({ error: "检查权限不足" }) }); state.check = check(checkStatus); data = state.check }
  else if (u.pathname === "/api/updates" && req.method() === "POST") { if (denyExecute) return route.fulfill({ status: 403, contentType: "application/json", body: JSON.stringify({ error: "此实例不允许自动升级" }) }); writes.push(req.postDataJSON()); if (unrecordedPost) return route.abort("connectionfailed"); state.operation = { ...operation(), ...req.postDataJSON() }; if (dropPost) return route.abort("connectionfailed"); data = state.operation }
  else if (u.pathname === "/api/updates/up_fixture/reject") { rejections++; state.operation = { ...operation("failed"), error: "实例管理员拒绝了升级请求" }; data = state.operation }
  else if (u.pathname === "/api/updates/up_fixture/approve") { approvals++; state.operation = operation(); data = state.operation }
  else throw new Error(`unexpected API ${req.method()} ${u.pathname}`)
  await route.fulfill({ contentType: "application/json", body: JSON.stringify(data) })
})
const go = () => page.goto(base + "/test/fixtures/updates.html")
const phase = value => page.locator(`[data-upgrade-phase="${value}"]`).waitFor()
try {
  await go()
  await page.getByText("前端和后端不是同一次构建。建议先保存工作，再重新载入与后端匹配的前端版本。").waitFor()
  await page.getByText(/当前为源码或非托管运行/).waitFor()
  await page.locator("[data-upgrade-check]").click()
  await page.locator('[data-upgrade-check-status="available"]').waitFor()
  assert.equal(await page.locator("[data-upgrade-execute]").count(), 0)
  state.capability = { managed: true, can_execute: false, reason: "owner only" }
  state.operation = operation("awaiting_approval")
  await page.reload(); await phase("awaiting_approval")
  assert.equal(await page.locator("[data-upgrade-approve]").count(), 0)
  state.capability.can_execute = true
  await page.reload(); await phase("awaiting_approval")
  await page.locator("[data-upgrade-reject]").click(); await phase("failed")
  assert.equal(rejections, 1)
  state.operation = operation("awaiting_approval")
  await page.reload(); await phase("awaiting_approval")
  await page.locator("[data-upgrade-approve]").click(); await phase("staging")
  assert.equal(approvals, 1)
  assert.equal(writes.length, 0)
  state.operation = null; state.check = check("available")
  await page.reload(); await page.locator("[data-upgrade-execute]").waitFor()
  await page.getByRole("textbox", { name: "对话草稿" }).fill("升级前未发送的草稿")
  dropPost = true
  await page.locator("[data-upgrade-execute]").click()
  await phase("staging")
  assert.equal(writes.length, 1)
  assert.equal(writes[0].version, "v1.2.0")
  disconnected = true; state.operation.phase = "restarting"
  await page.locator("[data-upgrade-connection]").waitFor()
  await page.reload(); await page.locator("[data-upgrade-connection]").waitFor()
  assert.equal(writes.length, 1, "reconnect/reload never POSTs")
  disconnected = false
  await phase("restarting")
  const beforeSuccess = navigations
  state.operation.phase = "succeeded"
  await phase("succeeded")
  assert.equal(navigations, beforeSuccess, "success never automatically refreshes the current page")
  assert.equal(await page.getByRole("textbox", { name: "对话草稿" }).inputValue(), "升级前未发送的草稿")
  await page.locator("[data-upgrade-refresh]").click(); await phase("succeeded")
  assert.equal(await page.getByRole("textbox", { name: "对话草稿" }).inputValue(), "升级前未发送的草稿")
  assert.equal(writes.length, 1)
  // Simulate an unrecorded first request: explicit retry retains the same key across reload.
  state.operation = null; dropPost = false
  await page.reload(); await page.locator("[data-upgrade-execute]").click(); await phase("staging")
  assert.equal(writes.length, 2)
  assert.equal(writes[0].idempotency_key, writes[1].idempotency_key)
  state.operation = operation("rolled_back"); state.operation.error = "新版本健康检查失败"
  await phase("rolled_back")
  await page.getByText("新版本健康检查失败", { exact: true }).waitFor()
  await page.locator("[data-upgrade-retry]").click(); await phase("staging")
  assert.equal(writes.length, 3)
  assert.notEqual(writes[2].idempotency_key, writes[1].idempotency_key, "explicit retry starts a new attempt")
  assert.ok(writes[2].idempotency_key.endsWith(":retry:up_fixture"))
  state.operation = null
  for (const status of ["ahead", "unknown_current", "unsupported_platform", "no_release", "up_to_date"]) {
    state.check = check(status); await page.reload()
    await page.locator(`[data-upgrade-check-status="${status}"]`).waitFor()
    assert.equal(await page.locator("[data-upgrade-execute]").count(), 0, status)
  }
  state.capability = { managed: true, can_execute: false, reason: "macOS Keychain 签名身份不能使用预编译升级包" }
  await page.reload(); await page.getByText("当前不能自动升级。请查看下方原因。").waitFor()
  await page.getByText(state.capability.reason, { exact: true }).waitFor()
  assert.equal(await page.getByText("只有实例管理员可以执行升级或批准 Agent 的升级请求。").count(), 0)
  state.capability.can_execute = true
  for (const [value, label] of [["draining", "正在等待运行中的任务安全停止"], ["rolled_back_pending_health", "旧版本已恢复，正在确认服务健康"]]) {
    state.operation = operation(value); await page.reload(); await phase(value)
    assert.ok((await page.locator(`[data-upgrade-phase="${value}"]`).innerText()).includes(label))
    assert.equal(await page.locator("[data-upgrade-check]").isDisabled(), true)
  }
  state.operation = { ...operation("succeeded"), id: "old-operation", version: "v1.1.0", idempotency_key: "old-key" }
  state.check = check("available"); failCheck = true
  await page.reload(); await page.locator("[data-upgrade-check]").click()
  await page.getByRole("alert").filter({ hasText: "检查权限不足" }).waitFor()
  await page.waitForTimeout(2300)
  assert.match(await page.getByRole("alert").innerText(), /检查权限不足/, "an unrelated old receipt cannot clear a check error")
  failCheck = false; denyExecute = true
  await page.locator("[data-upgrade-execute]").click()
  await page.getByRole("alert").filter({ hasText: "此实例不允许自动升级" }).waitFor()
  await page.waitForTimeout(2300)
  assert.equal(await page.getByRole("alert").innerText(), "此实例不允许自动升级", "4xx stays definite, not unknown")
  denyExecute = false; unrecordedPost = true
  await page.locator("[data-upgrade-execute]").click()
  await page.getByRole("alert").filter({ hasText: "提交结果尚未确认" }).waitFor()
  await page.waitForTimeout(2300)
  assert.match(await page.getByRole("alert").innerText(), /提交结果尚未确认/, "old receipt cannot confirm another target")
  state.operation = { ...operation(), ...writes.at(-1) }
  await phase("staging")
  await page.getByRole("alert").waitFor({ state: "detached" })
  hangStatus = true
  await page.reload()
  await page.locator("[data-upgrade-connection]").waitFor({ timeout: 12000 })
  hangStatus = false
  await phase("staging")
  await page.locator("[data-upgrade-connection]").waitFor({ state: "detached" })
  state.current = { hub: "1.2.0", web: "v1.1.0" }; state.check = check("available")
  state.operation = { ...operation("succeeded"), id: "old-success", idempotency_key: "web-upgrade:Infranite/steve:v1.2.0" }
  await page.reload(); await page.locator("[data-upgrade-repair]").waitFor()
  assert.equal(await page.locator("[data-upgrade-execute]").count(), 0)
  assert.match(await page.locator('[data-upgrade-check-status="available"]').innerText(), /前后端版本未对齐/)
  unrecordedPost = true
  await page.locator("[data-upgrade-repair]").click()
  await page.getByRole("alert").filter({ hasText: "提交结果尚未确认" }).waitFor()
  const repairAttempt = writes.at(-1), repairCount = writes.length
  assert.notEqual(repairAttempt.idempotency_key, state.operation.idempotency_key, "same-version repair must not return old success")
  await page.reload(); await page.locator("[data-upgrade-repair]").waitFor()
  assert.equal(writes.length, repairCount, "reloading mismatch does not repair automatically")
  unrecordedPost = false
  await page.locator("[data-upgrade-repair]").click(); await phase("staging")
  assert.equal(writes.at(-1).idempotency_key, repairAttempt.idempotency_key, "same repair intent is stable across reload/retry")
  state.current = { hub: "1.2.0", web: "1.2.0" }; state.check = check("up_to_date"); state.operation.phase = "succeeded"
  await phase("succeeded")
  assert.equal(await page.locator("[data-upgrade-repair]").count(), 0)
  const completedCount = writes.length
  await page.locator("[data-upgrade-refresh]").click(); await phase("succeeded")
  assert.equal(writes.length, completedCount, "successful refresh never starts another install")
  state.current.web = "1.1.0"; state.check = check("available")
  await page.locator("[data-upgrade-repair]").waitFor()
  await page.locator("[data-upgrade-repair]").click(); await phase("staging")
  assert.notEqual(writes.at(-1).idempotency_key, repairAttempt.idempotency_key, "a later explicit repair has a distinct prior receipt")
  state.operation = null; state.check = check("ahead")
  await page.reload(); await page.locator('[data-upgrade-check-status="ahead"]').waitFor()
  assert.equal(await page.locator("[data-upgrade-repair]").count(), 0, "pair mismatch cannot bypass check semantics")
  assert.equal(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth), false)
  if (process.env.STEVE_TEST_ARTIFACTS) await page.screenshot({ path: process.env.STEVE_TEST_ARTIFACTS + "/upgrade-mobile.png", fullPage: true })
  assert.deepEqual(errors, [])
  console.log(JSON.stringify({ passed: true, scenarios: ["source check only", "owner capability", "agent approve and reject", "ambiguous POST recovery", "restart disconnect", "reload without POST", "stable version key", "manual refresh preserves draft", "rollback and explicit retry", "ahead and unknown versions", "390px layout", "keychain capability reason", "drain and rollback health phases", "unrelated receipt cannot clear errors", "definite 4xx", "8s status timeout recovery", "same-version paired repair after old success", "stable repair intent", "repair refresh never POSTs", "later explicit repair gets fresh key"], gets, writes: writes.length, approvals, rejections }))
} catch (e) { console.error(await page.locator("body").innerText(), errors); throw e } finally { await browser.close() }
