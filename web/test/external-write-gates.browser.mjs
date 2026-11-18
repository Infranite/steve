// Run against external_write_gates_fixture and a local Vite server. All API responses are from
// the real isolated fixture; forwarding avoids requiring CORS on the loopback-only operator API.
import assert from "node:assert/strict"
import { mkdir } from "node:fs/promises"
import { chromium, request } from "playwright"

const hub = process.env.STEVE_TEST_HUB
if (!hub) throw new Error("STEVE_TEST_HUB must name the isolated external_write_gates_fixture")
const web = process.env.STEVE_TEST_WEB || "http://127.0.0.1:5174"
const api = await request.newContext({ baseURL: hub })
const state = async () => (await api.get("/fixture/state")).json()
const first = await state(), id = first.connection.id, account = first.connection.account_id
const browser = await chromium.launch({ headless: true, executablePath: process.env.PLAYWRIGHT_EXECUTABLE })
const page = await browser.newPage({ locale: "zh-CN" })
const errors = []
page.on("pageerror", e => errors.push(e.message))
await page.addInitScript(() => localStorage.setItem("steve-lang", new URL(location.href).searchParams.get("lang") || "zh-CN"))
await page.route("**/api/**", async route => {
  const u = new URL(route.request().url())
  if (!u.pathname.startsWith("/api/")) return route.continue()
  await route.fulfill({ response: await route.fetch({ url: hub + u.pathname + u.search }) })
})
const fixture = web + "/test/fixtures/external-write-gates.html"
const fits = async () => assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), true, "no horizontal overflow")
const policy = async value => {
  await api.post("/fixture/policy", { data: { policy: value } })
  for (let i = 0; i < 200; i++) {
    if ((await state()).connection.detail.external_write_policy.policy === value) return
    await new Promise(resolve => setTimeout(resolve, 25))
  }
  throw new Error("service policy not recorded: " + value)
}
try {
  if (process.env.STEVE_TEST_SCREENSHOTS) await mkdir(process.env.STEVE_TEST_SCREENSHOTS, { recursive: true })
  for (const lang of ["zh-CN", "en"]) {
    const localized = fixture + "?lang=" + lang
    for (const value of ["allowed", "restricted", "forbidden"]) {
      await policy(value)
      await api.patch(`/api/connections/${id}`, { data: { caps: ["msg.receive"] } })
      await page.goto(localized + "#/accounts/" + account)
      await page.locator(`[data-service-write-policy="${value}"]`).waitFor()
      assert.match(await page.locator(`[data-service-write-policy="${value}"]`).innerText(), lang === "en" ? /Service write policy/ : /服务端写策略/)
      const toggle = page.locator('[data-cap-toggle="msg.send_as_user"]')
      assert.equal(await toggle.isChecked(), false)
      await toggle.click()
      await page.waitForFunction(() => document.querySelector('[data-cap-toggle="msg.send_as_user"]')?.checked === true)
      const current = await state()
      assert.ok(current.connection.caps.includes("msg.send_as_user"))
      assert.equal(current.connection.detail.external_write_policy.policy, value, "the switch cannot rewrite service policy")
      assert.equal(current.writes.length, first.writes.length, "enabling cannot replay anything")
      for (const width of [1440, 390]) {
        await page.setViewportSize({ width, height: 1000 })
        await toggle.scrollIntoViewIfNeeded()
        await fits()
        if (process.env.STEVE_TEST_SCREENSHOTS) await page.screenshot({ path: `${process.env.STEVE_TEST_SCREENSHOTS}/account-${lang}-${value}-${width}.png`, animations: "disabled" })
      }
      await page.goto(localized + "#/integrations/ext_ws")
      await page.locator('[data-hub-write-policy="on"]').waitFor()
      await page.locator(`[data-service-write-policy="${value}"]`).waitFor()
      await fits()
      await page.locator('[data-hub-write-policy="on"] a').click()
      await toggle.waitFor()
      await toggle.click()
      await page.waitForFunction(() => document.querySelector('[data-cap-toggle="msg.send_as_user"]')?.checked === false)
      await page.goto(localized + "#/integrations/ext_ws")
      await page.locator('[data-hub-write-policy="off"]').waitFor()
      await page.locator(`[data-service-write-policy="${value}"]`).waitFor()
      if (process.env.STEVE_TEST_SCREENSHOTS) await page.screenshot({ path: `${process.env.STEVE_TEST_SCREENSHOTS}/integration-${lang}-${value}-390.png`, animations: "disabled" })
    }
  }
  assert.deepEqual(errors, [])
  console.log("PASS: real fixture APIs, existing Hub switch toggled on/off for all service policies, connection/account agreement, no replay, zh-CN/en, 1440px/390px, no page errors")
} finally {
  await browser.close()
  await api.dispose()
}
