import assert from "node:assert/strict"
const { chromium } = await import(process.env.PLAYWRIGHT_MODULE || "playwright")
const browser = await chromium.launch({ headless: true, ...(process.env.PLAYWRIGHT_EXECUTABLE ? { executablePath: process.env.PLAYWRIGHT_EXECUTABLE } : {}) })
const base = process.env.STEVE_TEST_WEB || "http://127.0.0.1:5193"
const page = await browser.newPage({ viewport: { width: 760, height: 420 } })
const errors = []
page.on("pageerror", e => errors.push(e.message))
try {
  await page.goto(base + "/test/fixtures/suggest-card.html")
  const card = page.locator('[data-suggest="append"]')
  await card.waitFor()
  /* the card about an existing item: append (recommended) or an item of its own — not “在这里回答” */
  assert.deepEqual(await card.locator("[data-suggest-choice]").allInnerTexts(), ["追加到 ST-40", "单独建项"])
  assert.equal(await page.getByText("在这里回答", { exact: true }).count(), 0)
  assert.equal(await page.getByText(/这是给/).count(), 0)
  const primary = async (l) => (await l.getAttribute("class")).split(" ").includes("bg-primary")
  assert.equal(await primary(card.locator('[data-suggest-choice="item"]')), true, "the recommended choice stands out")
  assert.equal(await primary(card.locator('[data-suggest-choice="new"]')), false)
  /* an older card keeps “在这里出方案” as its primary choice */
  assert.equal(await primary(page.locator('[data-suggest="old"] [data-suggest-choice="here"]')), true)
  assert.equal(await primary(page.locator('[data-suggest="old"] [data-suggest-choice="item"]')), false)
  if (process.env.STEVE_TEST_ARTIFACTS) await page.screenshot({ path: process.env.STEVE_TEST_ARTIFACTS + "/suggest-card.png", fullPage: true })
  assert.deepEqual(errors, [])
  console.log(JSON.stringify({ passed: true, scenarios: ["append or new", "no answer-here choice", "recommended primary", "old card"] }))
} finally { await browser.close() }
