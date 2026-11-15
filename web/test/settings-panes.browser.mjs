// Vite-only fixture: 设置 · 工作空间 / 数据 (the affectUI fields, switches and danger zone). All /api calls are
// answered here. Checks that a refresh after saving one workspace field never replaces text typed in another, that
// Escape puts a field back, that the irreversible "clear all memories" confirm opens on Cancel (Enter right after
// opening clears nothing) and hands the focus back on Escape, that the settings selects carry data-select on the combobox, and that a phone stacks a select under its row.
import assert from "node:assert/strict"
import { mkdir } from "node:fs/promises"
const { chromium } = await import(process.env.PLAYWRIGHT_MODULE || "playwright")
const browser = await chromium.launch({ headless: true, ...(process.env.PLAYWRIGHT_EXECUTABLE ? { executablePath: process.env.PLAYWRIGHT_EXECUTABLE } : {}) })
const base = process.env.STEVE_TEST_WEB || "http://127.0.0.1:5197"
const shots = process.env.STEVE_TEST_SHOTS
if (shots) await mkdir(shots, { recursive: true })

const errors = []
const open = async (pane, { width = 1440, theme = "light" } = {}) => {
  const page = await browser.newPage({ locale: "zh-CN", viewport: { width, height: 900 }, reducedMotion: "reduce", colorScheme: theme })
  page.on("pageerror", (e) => errors.push(e.message))
  const calls = []
  await page.addInitScript(() => localStorage.setItem("steve-lang", "zh-CN"))
  await page.route((u) => u.pathname.startsWith("/api/"), async (route) => {
    const r = route.request(), u = new URL(r.url())
    calls.push({ method: r.method(), path: u.pathname, body: r.postData() ? JSON.parse(r.postData()) : null })
    // a save's refresh never lands by itself here: the test lands it with __land when it wants to
    if (u.pathname === "/api/state") return
    if (["PATCH", "DELETE", "POST"].includes(r.method())) return route.fulfill({ contentType: "application/json", body: "{}" })
    return route.fulfill({ status: 404, contentType: "application/json", body: JSON.stringify({ error: "fixture" }) })
  })
  await page.goto(base + `/test/fixtures/settings-panes.html${theme === "dark" ? "?theme=dark" : ""}#/settings/${pane}`)
  await page.locator(`[data-settings-pane="${pane}"]`).waitFor()
  return { page, calls }
}

try {
  /* 工作空间: a refresh after saving the name keeps what is being typed in the description */
  {
    const { page, calls } = await open("workspace")
    const name = page.locator("[data-ws-name]"), desc = page.locator("[data-ws-desc]")
    await name.fill("Steve Hub")
    await desc.click() // the name loses focus and saves
    await page.keyboard.type("，正在写")
    assert.deepEqual(calls.filter((c) => c.method === "PATCH").map((c) => c.body), [{ name: "Steve Hub", description: "开源智能体枢纽", item_prefix: "ST" }])
    await page.evaluate(() => window.__land({ workspace: { name: "Steve Hub" } }))
    assert.equal(await desc.inputValue(), "开源智能体枢纽，正在写", "the refresh kept the description being typed")
    assert.equal(await name.inputValue(), "Steve Hub")
    /* the Hub changed the prefix elsewhere: an untouched field takes it */
    await page.evaluate(() => window.__land({ workspace: { item_prefix: "SV" } }))
    assert.equal(await page.locator("[data-prefix]").inputValue(), "SV")
    /* Escape puts the field back to what the Hub has, and leaving it then saves nothing */
    await page.keyboard.press("Escape")
    assert.equal(await desc.inputValue(), "开源智能体枢纽")
    const before = calls.filter((c) => c.method === "PATCH").length
    await name.click()
    assert.equal(calls.filter((c) => c.method === "PATCH").length, before, "nothing to save after Escape")
    await page.close()
  }

  /* 数据 · 危险操作: the confirm opens in place on Cancel */
  for (const width of [1440, 390]) {
    const { page, calls } = await open("data", { width })
    const opener = page.locator("[data-danger-open-btn]")
    await opener.focus()
    await page.keyboard.press("Enter")
    await page.locator("[data-danger-confirm]").waitFor()
    assert.equal(await page.evaluate(() => document.activeElement?.hasAttribute("data-danger-cancel")), true, "focus starts on Cancel")
    assert.equal(await opener.getAttribute("aria-expanded"), "true")
    assert.ok(await opener.isDisabled())
    const group = page.locator(`#${await opener.getAttribute("aria-controls")}`)
    assert.equal(await group.getAttribute("role"), "group")
    assert.match(await group.getAttribute("aria-labelledby"), /what$/)
    /* Enter right after opening is Cancel: nothing is cleared */
    await page.keyboard.press("Enter")
    await page.locator("[data-danger-confirm]").waitFor({ state: "detached" })
    assert.equal(calls.filter((c) => c.method === "DELETE").length, 0, "Enter after opening cleared nothing")
    await page.waitForFunction(() => document.activeElement?.hasAttribute("data-danger-open-btn"))
    /* Escape folds it back and returns the focus to the opener */
    await opener.click()
    await page.locator("[data-danger-confirm]").waitFor()
    await page.keyboard.press("Escape")
    await page.locator("[data-danger-confirm]").waitFor({ state: "detached" })
    await page.waitForFunction(() => document.activeElement?.hasAttribute("data-danger-open-btn"))
    assert.equal(await opener.getAttribute("aria-expanded"), "false")
    /* and the confirm button itself clears */
    await opener.click()
    await page.locator("[data-danger-confirm]").click()
    await page.waitForTimeout(100)
    assert.deepEqual(calls.filter((c) => c.method === "DELETE").map((c) => c.path), ["/api/memories"])
    await page.close()
  }

  /* 通用 on a phone: every select has its hook, and sits under its row's words at full width */
  {
    const { page } = await open("general", { width: 390 })
    for (const id of ["home", "away"]) {
      const s = page.locator(`[data-select="${id}"]`)
      assert.equal(await s.getAttribute("role"), "combobox")
      const [row, box] = await s.evaluate((el) => [el.closest('[data-slot="item"]').getBoundingClientRect().width, el.getBoundingClientRect().width])
      assert.ok(box > row * 0.8, `${id} select is ${box}px in a ${row}px row`)
    }
    if (shots) await page.screenshot({ path: `${shots}/settings-general-390.png`, fullPage: true })
    await page.close()
  }
  assert.deepEqual(errors, [])
  console.log("settings panes browser: ok")
} finally {
  await browser.close()
}
