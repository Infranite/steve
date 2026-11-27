// Browser smoke for three-level workspace Permissions (Ask / Auto / Full access):
// verifies owner-only control, discovered harnesses Auto/Full access table,
// mandatory risk confirmation before Full access (Cancel/Escape never writes; Confirm sends confirmed: true),
// and spec-driven unconfined harness names in both zh-CN and en.
import assert from "node:assert/strict"
const { chromium } = await import(process.env.PLAYWRIGHT_MODULE || "playwright")
const browser = await chromium.launch({ headless: true, ...(process.env.PLAYWRIGHT_EXECUTABLE ? { executablePath: process.env.PLAYWRIGHT_EXECUTABLE } : {}) })
const base = process.env.STEVE_TEST_WEB || "http://127.0.0.1:5197"

const HARNESSES = [
  { id: "claude-code", name: "Claude Code", ready: true, auto: true, auto_mode: "auto", full_access: true, full_access_mode: "bypassPermissions", nested_sandbox: true },
  { id: "codex", name: "Codex", ready: true, auto: true, auto_mode: "agent", full_access: true, full_access_mode: "agent-full-access", nested_sandbox: false },
  { id: "opencode", name: "OpenCode", ready: true, auto: false, auto_mode: null, full_access: false, full_access_mode: null, nested_sandbox: true },
]
const UNCONFINED = ["Codex", "Custom Box"]

const errors = []

async function openWorkspace({ lang = "zh-CN", editable = true } = {}) {
  const page = await browser.newPage({ locale: lang, viewport: { width: 1280, height: 900 }, reducedMotion: "reduce" })
  page.on("pageerror", (e) => errors.push(e.message))
  const puts = []
  let state = {
    policy: { mode: "ask", revision: 0, actor: null },
    editable,
    harnesses: HARNESSES,
    unconfined_harnesses: UNCONFINED,
  }
  await page.addInitScript((l) => localStorage.setItem("steve-lang", l), lang)
  await page.route((u) => u.pathname.startsWith("/api/"), async (route) => {
    const req = route.request(), u = new URL(req.url())
    if (u.pathname === "/api/shell") return
    if (u.pathname === "/api/permission-policy" && req.method() === "GET") {
      return route.fulfill({ status: 200, contentType: "application/json", body: JSON.stringify(state) })
    }
    if (u.pathname === "/api/permission-policy" && req.method() === "PUT") {
      const body = JSON.parse(req.postData() || "{}")
      puts.push(body)
      if (body.mode === "full_access" && !body.confirmed) {
        return route.fulfill({ status: 400, contentType: "application/json", body: JSON.stringify({ error: "confirmed required" }) })
      }
      state = { ...state, policy: { mode: body.mode, revision: state.policy.revision + 1, actor: "owner" } }
      return route.fulfill({ status: 200, contentType: "application/json", body: JSON.stringify(state) })
    }
    return route.fulfill({ status: 200, contentType: "application/json", body: "{}" })
  })
  await page.goto(base + "/test/fixtures/settings-panes.html#/settings/workspace")
  await page.locator('[data-permission-policy]').waitFor()
  return { page, puts }
}

try {
  // 1. zh-CN owner flow: Ask -> Auto -> Full access (Cancel does not write, Confirm writes confirmed: true)
  {
    const { page, puts } = await openWorkspace({ lang: "zh-CN", editable: true })
    const group = page.locator('[data-choices="permission-policy"]')
    assert.equal(await group.locator('[data-choice="ask"]').getAttribute("aria-checked"), "true")

    // Discovered harnesses matrix shows Auto & Full access capabilities and outside-sandbox tag
    const table = page.locator("[data-permission-harnesses]")
    await table.waitFor()
    assert.equal(await table.locator('[data-permission-harness="claude-code"] [data-harness-auto]').getAttribute("data-harness-auto"), "yes")
    assert.equal(await table.locator('[data-permission-harness="opencode"] [data-harness-auto]').getAttribute("data-harness-auto"), "no")
    assert.equal(await table.locator('[data-permission-harness="opencode"] [data-harness-full-access]').getAttribute("data-harness-full-access"), "no")
    assert.equal(await table.locator('[data-permission-harness="codex"] [data-outside-sandbox]').count(), 1)

    // No YOLO text anywhere on screen
    assert.doesNotMatch(await page.locator('[data-settings-pane="workspace"]').innerText(), /\byolo\b/i)

    // Switch Ask -> Auto directly without confirmation dialog
    await group.locator('[data-choice="auto"]').click()
    await page.waitForFunction(() => document.querySelector('[data-permission-policy]')?.getAttribute('data-permission-policy') === 'auto')
    assert.deepEqual(puts, [{ mode: "auto", revision: 0 }])

    // Click Full access -> risk dialog MUST appear and NO PUT sent yet
    await group.locator('[data-choice="full_access"]').click()
    const dlg = page.locator("[data-full-access-confirm]")
    await dlg.waitFor()
    assert.equal(puts.length, 1, "clicking full_access must not PUT before confirmation")

    // Verify all 4 risk points and deterministic spec-driven unconfined harness names
    const risksText = await dlg.locator("[data-full-access-risks]").innerText()
    assert.match(risksText, /不再询问任何命令/)
    assert.match(risksText, /网络/)
    assert.match(risksText, /Steve 的沙箱/)
    const outsideBullet = await dlg.locator("[data-full-access-outside-sandbox]").innerText()
    assert.match(outsideBullet, /Codex/)
    assert.match(outsideBullet, /Custom Box/)

    // Cancel leaves mode at auto and sends no PUT
    await dlg.locator("button", { hasText: "取消" }).click()
    await dlg.waitFor({ state: "hidden" })
    assert.equal(puts.length, 1, "cancelling full_access confirmation must not PUT")
    assert.equal(await page.locator('[data-permission-policy]').getAttribute("data-permission-policy"), "auto")

    // Confirm sends PUT with confirmed: true and updates mode to full_access
    await group.locator('[data-choice="full_access"]').click()
    await dlg.waitFor()
    await dlg.locator("button", { hasText: "确认切换到完全放开" }).click()
    await dlg.waitFor({ state: "hidden" })
    await page.waitForFunction(() => document.querySelector('[data-permission-policy]')?.getAttribute('data-permission-policy') === 'full_access')
    assert.deepEqual(puts[1], { mode: "full_access", revision: 1, confirmed: true })
    await page.close()
  }

  // 2. en locale: verify English risk confirmation copy and spec-driven names
  {
    const { page, puts } = await openWorkspace({ lang: "en", editable: true })
    const group = page.locator('[data-choices="permission-policy"]')
    await group.locator('[data-choice="full_access"]').click()
    const dlg = page.locator("[data-full-access-confirm]")
    await dlg.waitFor()
    const risksText = await dlg.locator("[data-full-access-risks]").innerText()
    assert.match(risksText, /no longer ask/i)
    assert.match(risksText, /network/i)
    assert.match(risksText, /Steve's sandbox/i)
    assert.match(risksText, /Codex/)
    assert.match(risksText, /Custom Box/)
    assert.doesNotMatch(risksText, /\byolo\b/i)
    await page.keyboard.press("Escape")
    await dlg.waitFor({ state: "hidden" })
    assert.equal(puts.length, 0, "Escape dismisses without PUT")
    await page.close()
  }

  // 3. Non-owner cannot change permission level
  {
    const { page, puts } = await openWorkspace({ lang: "zh-CN", editable: false })
    const autoBtn = page.locator('[data-choices="permission-policy"] [data-choice="auto"]')
    assert.equal(await autoBtn.isDisabled(), true, "non-owner control is disabled")
    assert.equal(puts.length, 0)
    await page.close()
  }

  assert.deepEqual(errors, [])
  console.log("permissions browser: ok")
} finally {
  await browser.close()
}
