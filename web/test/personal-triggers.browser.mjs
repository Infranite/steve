// Run against personal_trigger_fixture + Vite with STEVE_HUB pointed at that fixture.
import assert from "node:assert/strict"
import { chromium } from "playwright"
const api = process.env.STEVE_TRIGGER_FIXTURE
assert.ok(api && new URL(api).hostname === "127.0.0.1", "requires an isolated loopback fixture")
const web = process.env.STEVE_TEST_WEB ?? "http://127.0.0.1:5193"
const request = async (path, body) => {
  const r = await fetch(api + path, body === undefined ? {} : { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body) })
  assert.equal(r.status, 200, await r.clone().text())
  return r.json()
}
const state = await request("/fixture/state")
const browser = await chromium.launch({ headless: true })
try {
  for (const width of [390, 1280]) {
    const page = await browser.newPage({ locale: "zh-CN", viewport: { width, height: 900 } })
    const errors = []
    page.on("pageerror", (e) => errors.push(e.message))
    await page.goto(`${web}/test/fixtures/personal-triggers.html?account=${state.connections.me}`)
    await page.locator("[data-trigger-prefix]").fill("BROWSER_AUTO:")
    await page.locator("[data-trigger-prefix-add]").click()
    const remove = page.getByRole("button", { name: "移除前缀 BROWSER_AUTO:" })
    await remove.waitFor()
    await page.reload()
    await page.getByRole("button", { name: "移除前缀 BROWSER_AUTO:" }).waitFor()
    const filtered = await request("/fixture/inbound", { id: `browser-filter-${width}`, text: "BROWSER_AUTO: /bot ignored" })
    assert.equal(filtered.trigger, false)
    await request("/fixture/mode", { skip: true })
    const skipped = await request("/fixture/inbound", { id: `browser-skip-${width}`, text: "/bot synthetic skip" })
    await page.goto(`${web}/test/fixtures/personal-triggers.html?space=${skipped.space_id}`)
    const skippedRow = page.locator(`[data-msg="${skipped.message_id}"] [data-handling="skipped"]`)
    await skippedRow.waitFor()
    assert.match(await skippedRow.innerText(), /已跳过.*No explicit request/s)
    // Real API states are rendered by the actual record component, including a held draft.
    await request("/fixture/mode", { approval: true })
    const draft = await request("/fixture/inbound", { id: `browser-draft-${width}`, text: "/bot draft synthetic answer" })
    await page.goto(`${web}/test/fixtures/personal-triggers.html?space=${draft.space_id}`)
    await page.locator(`[data-msg="${draft.message_id}"] [data-handling="drafted"]`).waitFor()
    assert.ok(await page.locator(`[data-msg="${draft.message_id}"]`).innerText().then((s) => s.includes("仅起草，未发送")))
    assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth))
    if (process.env.STEVE_TEST_OUTPUT) await page.screenshot({ path: `${process.env.STEVE_TEST_OUTPUT}/draft-${width}.png`, fullPage: true })
    await page.goto(`${web}/test/fixtures/personal-triggers.html?account=${state.connections.me}`)
    await page.getByRole("button", { name: "移除前缀 BROWSER_AUTO:" }).click()
    await page.getByRole("button", { name: "移除前缀 BROWSER_AUTO:" }).waitFor({ state: "detached" })
    assert.deepEqual(errors, [])
    await page.close()
  }
  console.log("passed: real fixture API prefix add/reload/filter/remove, skip reason and draft-only display at 390/1280px")
} finally { await browser.close() }
