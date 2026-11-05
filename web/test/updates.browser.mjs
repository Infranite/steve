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
const check = status => ({ status, checked_at: Date.now(), version: "v1.2.0", release_url: "https://github.com/Infranite/steve/releases/tag/v1.2.0", notes: "# Steve v1.2.0\n\n测试更新说明\n\n- **加粗的要点** 和 `GET /api/x`", error: null })
let state = { repository: "Infranite/steve", current: { hub: "v1.1.0", web: "v1.1.0" }, capability: { managed: false, can_execute: false, reason: "source checkout" }, check: null, operation: null }
let denyExecute = false, unrecordedPost = false, failCheck = false, hangStatus = false
let disconnected = false, dropPost = false, checkStatus = "available", approvals = 0, rejections = 0, gets = 0
// what the restarted Hub answers on /healthz; the fixture page itself loads as "dev" unless ?web= names a version
let health = { ok: true, version: "v1.2.0", web_version: "v1.2.0", drain: false, recovery: { state: "ok" } }, healthDown = false
const operation = (phase = "staging") => ({ id: "up_fixture", idempotency_key: "agent-request", version: "v1.2.0", phase, requested_by: "test-agent", created_at: Date.now(), updated_at: Date.now(), error: null, previous: "v1.1.0", target: "v1.2.0" })
await page.route("**/*", async route => {
  const req = route.request(), u = new URL(req.url())
  if (u.origin !== base) return route.abort("blockedbyclient")
  if (u.pathname === "/healthz") return healthDown ? route.abort("connectionfailed") : route.fulfill({ contentType: "application/json", body: JSON.stringify(health) })
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
  // no upgrade button here: said plainly, with the Hub's reason
  await page.locator("[data-upgrade-unavailable]").getByText("不能在这里一键升级").waitFor()
  await page.locator("[data-upgrade-reason]").getByText("source checkout", { exact: true }).waitFor()
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
  await page.reload(); await page.getByText("不能在这里一键升级 · 托管安装").waitFor()
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
  // the release notes are Markdown: rendered, not shown as their source
  await page.getByText("更新内容").click()
  await page.locator("[data-release-notes] li .font-semibold").waitFor()
  assert.equal(await page.locator("[data-release-notes] code").first().innerText(), "GET /api/x")
  assert.doesNotMatch(await page.locator("[data-release-notes]").innerText(), /\*\*|^# /m)
  if (process.env.STEVE_TEST_SHOTS) await page.locator("[data-release-notes]").screenshot({ path: `${process.env.STEVE_TEST_SHOTS}/updates-release-notes.png` })

  // Installations Steve doesn't switch itself: this machine's commands instead of a button, then a read-back.
  const home = "/Users/example/.steve", repo = "/Users/example/Agents/steve"
  const localReason = "这台机器的 Hub 是本机签名构建（ops/local-release.py），靠本机签名身份和 steve-keyhelper 读取 Keychain 里的密钥。"
  state = { ...state, current: { hub: "v1.1.0", web: "v1.1.0" }, capability: { managed: false, can_execute: false, reason: localReason }, install: { kind: "local_release", home, release: "1.1.0-local-51826fc", repo }, check: check("available"), operation: null, backup: null }
  await page.reload(); await page.locator('[data-upgrade-guide="local_release"]').waitFor()
  assert.equal(await page.locator("[data-upgrade-execute]").count(), 0)
  const unavailable = await page.locator('[data-upgrade-unavailable][data-install-kind="local_release"]').innerText()
  assert.match(unavailable, /^不能在这里一键升级 · 本机签名构建/)
  assert.ok(unavailable.includes(localReason), "the Hub's reason is shown, not swallowed")
  assert.match(unavailable, /按下方步骤在这台机器上升级/)
  assert.match(await page.locator("[data-upgrade-enable]").innerText(), /STEVE_VAULT_KEYSTORE=dir.*run-managed\.sh/)
  const commands = await page.locator("[data-upgrade-command] pre").allInnerTexts()
  assert.deepEqual(commands.map(c => c.trim()).slice(1), [`python3 ${repo}/ops/local-release.py build v1.2.0`, `python3 ${repo}/ops/local-release.py deploy ${home}/releases/1.2.0-local-*`])
  assert.match(await page.locator("[data-upgrade-backup]").innerText(), /\/Users\/example\/\.steve\/backups/)
  assert.match(await page.locator("[data-upgrade-recovery]").innerText(), /deploy \/Users\/example\/\.steve\/releases\/1\.1\.0-local-51826fc/)
  assert.equal(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth), false, "commands wrap on a phone")
  if (process.env.STEVE_TEST_ARTIFACTS) await page.screenshot({ path: process.env.STEVE_TEST_ARTIFACTS + "/upgrade-guide-mobile.png", fullPage: true })
  // not restarted yet: the read-back keeps trying and never says upgraded
  health = { ...health, version: "v1.1.0", web_version: "v1.1.0" }
  await page.locator("[data-upgrade-verify-start]").click()
  await page.locator('[data-verify-item="hub"][data-ok="false"]').waitFor()
  assert.equal(await page.locator('[data-upgrade-verify="passed"]').count(), 0)
  healthDown = true
  await page.getByText("暂时连不上服务").waitFor({ timeout: 8000 })
  healthDown = false; health = { ...health, version: "v1.2.0", web_version: "v1.2.0" }
  await page.locator('[data-verify-item="hub"][data-ok="true"]').waitFor({ timeout: 8000 })
  await page.locator("[data-upgrade-refresh]").waitFor()
  assert.match(await page.locator("[data-upgrade-verify]").innerText(), /重新载入页面后完成核对/, "an old page is never called upgraded")
  // someone else only learns who upgrades; no paths, no commands
  state.install = { kind: "local_release" }; state.capability = { ...state.capability, reason: "仅实例管理员可批准和执行升级" }
  await page.reload(); await page.getByText("不能在这里一键升级 · 本机签名构建").waitFor()
  await page.getByText("仅实例管理员可批准和执行升级", { exact: true }).waitFor()
  assert.equal(await page.locator("[data-upgrade-enable]").count(), 0)
  assert.equal(await page.locator("[data-upgrade-guide]").count(), 0)
  for (const kind of ["release_script", "source", "external"]) {
    state.install = { kind, home, repo }; await page.reload()
    await page.locator(`[data-upgrade-guide="${kind}"]`).waitFor()
    assert.ok(!(await page.locator(`[data-upgrade-guide="${kind}"]`).innerText()).includes("local-release.py"), kind)
  }
  // not checked yet, or nothing newer: still says why and what to do, without a guide
  state.install = { kind: "local_release", home, repo }; state.capability.reason = localReason
  for (const [status, says] of [[null, /点「检查更新」。有新版本时/], ["up_to_date", /^有新版本时/m]]) {
    state.check = status && check(status); await page.reload()
    await page.locator('[data-upgrade-unavailable][data-install-kind="local_release"]').getByText(says).waitFor()
    assert.equal(await page.locator("[data-upgrade-guide]").count(), 0, String(status))
  }
  if (process.env.STEVE_TEST_ARTIFACTS) await page.locator("[data-upgrade-settings]").screenshot({ path: process.env.STEVE_TEST_ARTIFACTS + "/upgrade-unavailable-up-to-date.png" })

  // Managed: steps while it runs, the backup it took, how a failure was recovered.
  state = { ...state, capability: { managed: true, can_execute: true, reason: "managed supervisor" }, install: { kind: "managed", home }, check: check("available") }
  state.operation = { ...operation("backing_up"), id: "up_progress", idempotency_key: "progress" }
  await page.reload(); await page.locator("[data-upgrade-progress]").waitFor()
  assert.equal(await page.locator('[data-upgrade-progress] [data-stage="done"]').count(), 3)
  assert.equal((await page.locator('[data-upgrade-progress] [data-stage="current"]').innerText()).trim(), "备份数据库")
  assert.equal(await page.locator("[data-upgrade-guide]").count(), 0, "a managed install upgrades from the button")
  state.backup = { database: `${home}/updates/up_progress.sqlite3`, dir: `${home}/updates` }
  state.operation = { ...state.operation, phase: "rolled_back", error: "Hub health verification timed out", previous: `${home}/releases/1.1.0` }
  await phase("rolled_back")
  const rolled = await page.locator('[data-upgrade-recovery="rolled_back"]').innerText()
  assert.match(rolled, /up_progress\.sqlite3/); assert.match(rolled, /已自动恢复/); assert.match(rolled, /releases\/1\.1\.0/)
  state.operation = { ...state.operation, phase: "failed", error: "升级已提交，业务启动失败；保留新数据库，禁止自动回滚：boom" }
  await page.locator('[data-upgrade-recovery="failed"]').filter({ hasText: "按设计不自动回退" }).waitFor()
  state.operation = { ...state.operation, phase: "failed", error: "维护排空超时；当前版本保留，尚未停止或切换" }; state.backup = { database: null, dir: `${home}/updates` }
  await page.locator('[data-upgrade-recovery="failed"]').filter({ hasText: "旧版本和数据库没有改动" }).waitFor()
  // success is only claimed once the Hub, the served web, this page and health agree
  state.operation = { ...state.operation, phase: "succeeded", error: null }; state.current = { hub: "v1.2.0", web: "v1.2.0" }; state.check = check("up_to_date")
  health = { ok: true, version: "v1.2.0", web_version: "v1.2.0", drain: true, recovery: { state: "ok" } }
  await page.goto(base + "/test/fixtures/updates.html?web=1.2.0"); await phase("succeeded")
  await page.locator('[data-upgrade-progress] [data-stage="current"]').waitFor()
  await page.locator('[data-verify-item="health"][data-ok="false"]').waitFor()
  assert.equal(await page.locator('[data-upgrade-verify="passed"]').count(), 0, "maintenance still on is not upgraded")
  health.drain = false
  await page.locator('[data-upgrade-verify="passed"]').waitFor({ timeout: 8000 })
  assert.match(await page.locator("[data-upgrade-verify]").innerText(), /已升级到 1\.2\.0/)
  assert.equal(await page.locator("[data-upgrade-refresh]").count(), 0)
  assert.equal(await page.locator('[data-upgrade-progress] [data-stage="done"]').count(), 7, "the last step completes with the read-back")
  if (process.env.STEVE_TEST_ARTIFACTS) await page.screenshot({ path: process.env.STEVE_TEST_ARTIFACTS + "/upgrade-mobile.png", fullPage: true })
  assert.deepEqual(errors, [])
  console.log(JSON.stringify({ passed: true, scenarios: ["source check only", "owner capability", "agent approve and reject", "ambiguous POST recovery", "restart disconnect", "reload without POST", "stable version key", "manual refresh preserves draft", "rollback and explicit retry", "ahead and unknown versions", "390px layout", "keychain capability reason", "drain and rollback health phases", "unrelated receipt cannot clear errors", "definite 4xx", "8s status timeout recovery", "same-version paired repair after old success", "stable repair intent", "repair refresh never POSTs", "later explicit repair gets fresh key", "local-release commands with this machine's paths", "read-back retries through restart and outage", "old page needs reload", "non-owner gets no paths", "per-install guides", "managed progress steps", "rollback backup and recovery", "committed failure keeps new database", "pre-switch failure untouched", "upgraded only after health and versions agree"], gets, writes: writes.length, approvals, rejections }))
} catch (e) { console.error(await page.locator("body").innerText(), errors); throw e } finally { await browser.close() }
