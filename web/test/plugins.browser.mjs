// Vite-only fixture: 设置 · 插件 (web/src/plugins.tsx) in the real SettingsView, every /api call answered here.
// Checks that the pane opens on what is broken (a connection that is not running, a library that could not be read),
// then what is installed, what can be installed and the libraries; that the official library is listed unread and only
// read when the owner asks; that installing calls the Hub and a theme plugin's manifest becomes a choice in 外观 (and a
// bad one is shown as refused); that a member sees no owner's controls; and the layout at 1440 and 390, light and dark.
import assert from "node:assert/strict"
import { mkdir } from "node:fs/promises"
const { chromium } = await import(process.env.PLAYWRIGHT_MODULE || "playwright")
const browser = await chromium.launch({ headless: true, ...(process.env.PLAYWRIGHT_EXECUTABLE ? { executablePath: process.env.PLAYWRIGHT_EXECUTABLE } : {}) })
const base = process.env.STEVE_TEST_WEB || "http://127.0.0.1:5197"
const shots = process.env.STEVE_TEST_SHOTS
if (shots) await mkdir(shots, { recursive: true })

const text = (en, zh) => ({ en, "zh-CN": zh })
const plugin = (p) => ({ homepage: null, config: [], listed: true, unsupported: null, available: true, installed: null, update: false, connections: [], contract: 1, ...p })
const ocean = { id: "ocean", name: { en: "Ocean", "zh-CN": "海洋" }, kind: "accent", tokens: { light: { primary: "#0b6bc0" }, dark: { primary: "#4aa3f0" } } }
function view(owner) {
  return {
    target: "aarch64-apple-darwin", owner,
    stores: [
      { id: "infranite-steve-extensions", name: null, checked: null, official: true, plugins: [], ...(owner ? { address: "https://github.com/Infranite/steve-extensions", error: null } : { failed: false }) },
      { id: "local", name: text("Local plugins", "本地插件"), checked: Date.now() - 180_000, official: false, ...(owner ? { address: "/srv/steve-plugins", error: "no steve-store.json there" } : { failed: true }), plugins: [
        plugin({ id: "openlark", name: "OpenLark", summary: text("Lark as you", "以你的身份接入飞书"), kind: "channel", version: "0.2.0", homepage: "https://example.org/openlark", update: true,
          config: [{ key: "address", label: "Gateway", required: true, secret: false }, { key: "token", label: "Token", required: true, secret: true }],
          installed: { version: "0.1.0", installed: 1, config: [{ key: "address", label: "Gateway", required: true, secret: false }, { key: "token", label: "Token", required: true, secret: true }] },
          connections: [{ instance: "i1", connection: "c1", status: { state: "waiting", error: "dial tcp 127.0.0.1:443: connection refused" } }] }),
        plugin({ id: "calendar", name: "Team Calendar", summary: text("Calendar search and events", "搜索日历与事件"), kind: "connector", version: "1.0.0", installed: { version: "1.0.0", installed: 1, config: [{ key: "token", label: "Token", required: true, secret: true }] } }),
        plugin({ id: "ocean", name: text("Ocean", "海洋"), summary: text("A blue accent", "一种蓝色强调色"), kind: "theme", version: "1.0.0", installed: { version: "1.0.0", installed: 1, config: [] } }),
        plugin({ id: "broken", name: "Broken", summary: "A theme with a bad token", kind: "theme", version: "1.0.0", installed: { version: "1.0.0", installed: 1, config: [] } }),
        plugin({ id: "dusk", name: text("Dusk", "黄昏"), summary: text("A whole evening look", "一整套傍晚的外观"), kind: "theme", version: "0.3.0" }),
        plugin({ id: "later", name: "Later", summary: "From a newer Steve", kind: "tool", version: "1.0.0", unsupported: "kind", available: false }),
      ] },
    ],
  }
}
const themes = { themes: [
  { store: "local", plugin: "ocean", version: "1.0.0", manifest: ocean },
  { store: "local", plugin: "broken", version: "1.0.0", manifest: { id: "broken", name: "Broken", kind: "accent", tokens: { light: { primary: "url(x)" } } } },
] }

const errors = []
const open = async ({ width = 1440, theme = "light", owner = true } = {}) => {
  const page = await browser.newPage({ locale: "zh-CN", viewport: { width, height: 900 }, reducedMotion: "reduce", colorScheme: theme })
  page.on("pageerror", (e) => errors.push(e.message))
  const calls = []
  await page.addInitScript(() => localStorage.setItem("steve-lang", "zh-CN"))
  await page.route((u) => u.pathname.startsWith("/api/"), async (route) => {
    const r = route.request(), u = new URL(r.url())
    calls.push({ method: r.method(), path: u.pathname, ...(r.method() === "POST" ? { body: r.postDataJSON() } : {}) })
    const json = (body) => route.fulfill({ contentType: "application/json", body: JSON.stringify(body) })
    if (u.pathname === "/api/state") return
    if (u.pathname === "/api/plugins") return json(view(owner))
    if (u.pathname === "/api/plugins/themes") return json(themes)
    if (u.pathname === "/api/vault/status") return json({ initialized: true, keychain: "ok", kek_version: 1, passkeys: 0, rotating: false })
    if (u.pathname === "/api/plugins/installed/local/calendar/connect" && r.method() === "POST") return json({ instance: "calendar-instance", connection: "calendar-connection" })
    if (u.pathname === "/api/accounts") return json([{ id: "a1", system: { id: "lark", name: "Lark", kind: "im" }, party: { id: "me", name: "Lee", type: "person", is_me: true }, kind: "user", handle: "lee", display_name: "Lee · Lark", verified: true, confidence: 1, source: "", source_label: "", connection: { id: "c1" }, vault_items: [], revision: 1 }])
    if (u.pathname.endsWith("/refresh")) return json({ id: "infranite-steve-extensions", checked: Date.now(), error: null })
    if (r.method() === "POST" && u.pathname.startsWith("/api/plugins/installed/")) return json({ version: "0.3.0", installed: Date.now() })
    return route.fulfill({ status: 404, contentType: "application/json", body: JSON.stringify({ error: "fixture" }) })
  })
  await page.goto(base + `/test/fixtures/settings-panes.html${theme === "dark" ? "?theme=dark" : ""}#/settings/plugins`)
  await page.locator("[data-plugins]").waitFor()
  return { page, calls }
}

try {
  /* the owner: broken things first, then installed, available and the libraries */
  {
    const { page, calls } = await open()
    const order = await page.locator("[data-plugins] > *").evaluateAll((els) => els.map((el) => [...el.attributes].map((a) => a.name).find((n) => n.startsWith("data-plugins-"))))
    assert.deepEqual(order, ["data-plugins-attention", "data-plugins-installed", "data-plugins-offered", "data-plugins-libraries"])
    assert.equal(await page.locator('[data-plugin-down="i1"]').count(), 1, "the waiting connection is called out")
    assert.equal(await page.locator('[data-plugin-store-failed="local"]').count(), 1, "the failed library is called out")
    /* installed: the channel with its connection, the update offered; the themes */
    const lark = page.locator('[data-plugins-installed] [data-plugin="openlark"]')
    assert.equal(await lark.locator('[data-connection-state="waiting"]').count(), 1)
    assert.match(await lark.locator("[data-connection-error]").innerText(), /connection refused/)
    assert.match(await lark.locator("[data-plugin-update-row]").innerText(), /0\.2\.0/)
    assert.equal(await lark.locator("[data-plugin-update-row] [data-plugin-install]").count(), 1)
    /* connector plugins use the same real connection form, not an unsupported-kind fallback */
    const calendar = page.locator('[data-plugins-installed] [data-plugin="calendar"]')
    await calendar.getByText("连接器", { exact: true }).waitFor()
    await calendar.locator("[data-plugin-connect]").click()
    const connect = page.locator('[data-plugin-connect-dialog="calendar"]')
    await connect.waitFor()
    assert.equal(await connect.locator("[data-plugin-connect-go]").isDisabled(), true)
    const token = connect.locator('[data-plugin-field="token"]')
    assert.equal(await token.getAttribute("type"), "password")
    await token.fill("fixture-token")
    await connect.locator("[data-plugin-connect-go]").click()
    await page.locator("[data-plugin-connected]").waitFor()
    assert.deepEqual(calls.find(c => c.path === "/api/plugins/installed/local/calendar/connect"), { method: "POST", path: "/api/plugins/installed/local/calendar/connect", body: { config: { token: "fixture-token" } } })
    await page.locator("[data-plugin-connected]").getByRole("button", { name: "好了", exact: true }).click()
    await page.locator("[data-plugin-connected]").waitFor({ state: "detached" })
    /* the themes were read on opening: one registered, one refused with the reason */
    await page.locator('[data-plugin="ocean"] [data-plugin-theme="ocean"]').waitFor()
    assert.match(await page.locator('[data-plugin="broken"] [data-plugin-theme="refused"]').innerText(), /primary/)
    /* available: installable, and one needing a newer Steve has no install button */
    const offered = page.locator("[data-plugins-offered]")
    assert.equal(await offered.locator('[data-plugin="dusk"] [data-plugin-install]').count(), 1)
    assert.equal(await offered.locator('[data-plugin="later"] [data-plugin-install]').count(), 0)
    assert.equal(await offered.locator('[data-plugin="later"] [data-plugin-why="unsupported"]').count(), 1)
    /* the official library is listed and was never fetched; reading it is the owner's click */
    assert.equal(calls.filter((c) => c.path.endsWith("/refresh")).length, 0)
    await offered.locator('[data-store-unread-notice="infranite-steve-extensions"] [data-store-read]').click()
    await page.waitForTimeout(100)
    assert.deepEqual(calls.filter((c) => c.path.endsWith("/refresh")).map((c) => c.path), ["/api/plugins/stores/infranite-steve-extensions/refresh"])
    /* installing calls the Hub and reads the themes again */
    const before = calls.filter((c) => c.path === "/api/plugins/themes").length
    await offered.locator('[data-plugin="dusk"] [data-plugin-install]').click()
    await page.waitForTimeout(200)
    assert.ok(calls.some((c) => c.method === "POST" && c.path === "/api/plugins/installed/local/dusk"))
    assert.ok(calls.filter((c) => c.path === "/api/plugins/themes").length > before, "the themes are read again after an install")
    /* the registered theme is a choice in 外观 */
    await page.locator('[data-plugin="ocean"] [data-plugin-theme-open]').click()
    await page.locator('[data-appearance="accent"] [data-choice="ocean"]').waitFor()
    await page.locator('[data-appearance="accent"] [data-choice="ocean"]').click()
    assert.equal(await page.evaluate(() => getComputedStyle(document.documentElement).getPropertyValue("--primary").trim()), "#0b6bc0")
    assert.equal(await page.locator('[data-appearance="accent"] [data-choice="broken"]').count(), 0, "a refused theme is never a choice")
    assert.equal(await page.locator("[data-appearance-plugins]").getAttribute("href"), "#/settings/plugins")
    await page.close()
  }

  /* a member: no owner's controls, and why */
  {
    const { page } = await open({ owner: false })
    assert.equal(await page.locator("[data-plugin-install], [data-plugin-more], [data-plugin-add], [data-store-refresh], [data-store-remove], [data-store-read]").count(), 0)
    assert.equal(await page.locator('[data-plugin="openlark"] [data-plugin-connect]').count(), 1, "connecting is anyone's")
    assert.equal(await page.locator('[data-plugin="calendar"] [data-plugin-connect]').count(), 1, "a member can connect a connector too")
    assert.equal(await page.locator("[data-store-address]").count(), 0, "addresses are the owner's")
    await page.close()
  }

  /* the layout: nothing wider than the screen; screenshots for a look */
  for (const theme of ["light", "dark"]) for (const width of [1440, 390]) {
    const { page } = await open({ width, theme })
    await page.locator('[data-plugin="ocean"] [data-plugin-theme="ocean"]').waitFor()
    const over = await page.evaluate(() => document.documentElement.scrollWidth - window.innerWidth)
    assert.ok(over <= 0, `${theme} ${width}: ${over}px wider than the screen`)
    if (shots) await page.screenshot({ path: `${shots}/plugins-${theme}-${width}.png`, fullPage: true })
    await page.close()
  }
  assert.deepEqual(errors, [])
  console.log("plugins browser: ok")
} finally {
  await browser.close()
}
