// Actual App + isolated HTTP fixture, injected native bridge, headless browser only. No installed Hub or user data.
import assert from "node:assert/strict"
import { fileURLToPath } from "node:url"
import { createServer } from "vite"
import { deliveryFixture } from "./fixtures/delivery-api.mjs"
const { chromium } = await import(process.env.PLAYWRIGHT_MODULE || "playwright")
const fixture = await deliveryFixture()
const root = fileURLToPath(new URL("..", import.meta.url))
const vite = await createServer({ root, configFile: root + "/vite.config.ts", server: { host: "127.0.0.1", port: 0, proxy: Object.fromEntries(["/api", "/auth", "/healthz"].map((p) => [p, { target: fixture.url }])) } })
await vite.listen()
const base = `http://127.0.0.1:${vite.httpServer.address().port}`
const browser = await chromium.launch({ headless: true, ...(process.env.PLAYWRIGHT_EXECUTABLE ? { executablePath: process.env.PLAYWRIGHT_EXECUTABLE } : {}) })
const errors = [], results = []
const check = (name) => results.push(name)
async function open(mode = "modern", signedIn = true) {
  const context = await browser.newContext({ locale: "en", viewport: { width: 1280, height: 900 } })
  if (signedIn) await context.addCookies([{ name: "fixture-auth", value: "1", url: base }])
  const page = await context.newPage()
  page.on("pageerror", (e) => errors.push(e.message))
  await page.addInitScript(({ mode }) => {
    localStorage.setItem("steve-onboarded", "1")
    if (!localStorage.getItem("steve-lang")) localStorage.setItem("steve-lang", "en")
    window.__contexts = []; window.__acks = []; window.__intents = []; window.__calls = []
    if (mode === "browser") return
    window.__STEVE_NATIVE__ = { v: 2, platform: mode === "ios" ? "ios" : "macos", can: ["hubs.list", "hubs.switch", "hubs.add", "hubs.forget", "hubs.rename", "pair.claim", ...(mode === "older" ? [] : ["shell.context", "shell.navigation.next", "shell.navigation.ack"]), ...(mode === "modern" ? ["shell.chrome.hub", "shell.attention.visible"] : [])], call: async (op, args) => {
      window.__calls.push({ op, args })
      if (op === "shell.context") { window.__contexts.push(args); return {} }
      if (op === "shell.navigation.next") return window.__intents.shift() ?? null
      if (op === "shell.navigation.ack") { window.__acks.push(args); return {} }
      if (op === "hubs.list") return { hubs: [{ id: "fixture-hub", name: "Fixture Hub", address: "http://fixture.example", current: true, signedIn: true, removable: false }] }
      return {}
    } }
  }, { mode })
  await page.goto(base + "/#/items")
  if (signedIn) await page.locator("[data-ws-switch]").waitFor()
  else await page.locator("[data-login]").waitFor()
  if (mode !== "browser" && mode !== "older") await page.waitForFunction(() => window.__contexts.some((c) => c.ready))
  return { page, close: () => context.close() }
}
async function navigate(page, id, route, extra = {}) {
  await page.evaluate(({ id, route, extra }) => window.__intents.push({ id, hubID: "fixture-hub", route, ...extra }), { id, route, extra })
  await page.waitForFunction((id) => window.__acks.some((a) => a.id === id), id)
  return page.evaluate((id) => window.__acks.find((a) => a.id === id), id)
}
try {
  for (const mode of ["browser", "older", "ios", "mac-web", "modern"]) {
    const f = await open(mode)
    try {
      assert.equal(await f.page.locator("[data-hub-switch]").count(), mode === "modern" ? 0 : 1, mode)
      assert.equal(await f.page.locator("[data-ws-switch]").count(), 1)
      assert.ok(await f.page.locator('[data-nav="items"]').count())
      if (mode === "ios" || mode === "mac-web") assert.equal(Object.hasOwn(await f.page.evaluate(() => window.__contexts.at(-1)), "appearance"), false, mode)
      if (mode === "browser" || mode === "older") {
        await f.page.evaluate(() => { location.hash = "#/hub/add" })
        await f.page.locator("[data-hubs-dialog]").waitFor()
        assert.equal(await f.page.evaluate(() => window.__contexts.length), 0)
      }
    } finally { await f.close() }
  }
  check("browser, older native and iOS preserve full Web Hub navigation; only opted-in Mac hides duplicate global Hub switcher")
  const f = await open()
  try {
    const page = f.page
    const initial = await page.evaluate(() => window.__contexts.at(-1))
    assert.equal(initial.workspaceID, "fixture")
    assert.equal(initial.workspaceName, "Test workspace")
    assert.equal(initial.route, "#/items")
    assert.equal(initial.authenticated, true)
    assert.equal(Object.hasOwn(initial, "hubID"), false)
    assert.deepEqual(Object.keys(initial).sort(), ["workspaceID", "workspaceName", "route", "title", "ready", "authenticated", "focused", "visible", "appearance", "language"].sort())
    assert.equal(initial.language, "en")
    assert.equal(typeof initial.appearance.dark, "boolean")
    assert.equal(initial.appearance.background.length, 3)
    assert.ok(initial.appearance.background.every(c => Number.isInteger(c) && c >= 0 && c <= 255))
    check("App-level context contains selected workspace and metadata only, never credentials or web-supplied Hub identity")
    assert.equal((await navigate(page, "language-detail", "#/items/ST-100")).ok, true)
    const fixedTitle = await page.evaluate(() => window.__contexts.at(-1).title)
    assert.equal(fixedTitle, "Delivered files")
    await page.evaluate(async () => { const { setLang } = await import("/src/i18n/index.ts"); setLang("zh-CN") })
    await page.waitForFunction(() => window.__contexts.at(-1).language === "zh-CN")
    assert.equal(await page.evaluate(() => window.__contexts.at(-1).title), fixedTitle)
    assert.equal(await page.evaluate(() => location.hash), "#/items/ST-100")
    await page.reload()
    await page.waitForFunction(() => window.__contexts.some(c => c.ready && c.language === "zh-CN"))
    assert.equal(await page.evaluate(() => window.__contexts.at(-1).title), fixedTitle)
    await page.evaluate(async () => { const { setLang } = await import("/src/i18n/index.ts"); setLang("en") })
    await page.waitForFunction(() => window.__contexts.at(-1).language === "en")
    assert.equal(await page.evaluate(() => window.__contexts.at(-1).title), fixedTitle)
    assert.equal((await navigate(page, "language-return", "#/items")).ok, true)
    check("language-only changes publish both ways without navigation or changing a user-owned detail title")
    // Theme preference, user theme stylesheet and inline body/root mutations need no route change.
    const systemDark = await page.evaluate(() => matchMedia("(prefers-color-scheme: dark)").matches)
    await page.evaluate(async (dark) => { const { setTheme } = await import("/src/ui.tsx"); setTheme(dark ? "dark" : "light") }, systemDark)
    await page.waitForFunction(() => window.__contexts.at(-1).appearance?.followSystem === false)
    const explicitAppearance = await page.evaluate(() => window.__contexts.at(-1).appearance)
    await page.evaluate(async () => { const { setTheme } = await import("/src/ui.tsx"); setTheme("system") })
    await page.waitForFunction(() => window.__contexts.at(-1).appearance?.followSystem === true)
    const systemAppearance = await page.evaluate(() => window.__contexts.at(-1).appearance)
    assert.equal(systemAppearance.dark, explicitAppearance.dark)
    assert.deepEqual(systemAppearance.background, explicitAppearance.background)
    await page.evaluate(async () => { const { setTheme } = await import("/src/ui.tsx"); setTheme("dark") })
    await page.waitForFunction(() => window.__contexts.at(-1).appearance?.dark === true)
    await page.evaluate(async () => { const { setAppearance } = await import("/src/themes/index.ts"); setAppearance({ neutral: "sand" }) })
    await page.waitForFunction(() => JSON.stringify(window.__contexts.at(-1).appearance?.background) === "[17,17,16]")
    await page.evaluate(async () => { const { setTheme } = await import("/src/ui.tsx"); setTheme("light") })
    await page.waitForFunction(() => window.__contexts.at(-1).appearance?.dark === false && JSON.stringify(window.__contexts.at(-1).appearance?.background) === "[252,252,251]")
    await page.evaluate(() => document.documentElement.style.setProperty("--background", "rgb(41 52 63)"))
    await page.waitForFunction(() => JSON.stringify(window.__contexts.at(-1).appearance?.background) === "[41,52,63]")
    await page.evaluate(() => { document.body.style.backgroundColor = "color(srgb 0.2 0.4 0.6)" })
    await page.waitForFunction(() => JSON.stringify(window.__contexts.at(-1).appearance?.background) === "[51,102,153]")
    assert.equal(await page.evaluate(() => location.hash), "#/items")
    const appearanceReports = await page.evaluate(() => window.__contexts.length)
    await page.evaluate(() => { for (let i = 0; i < 50; i++) document.documentElement.classList.toggle("unrelated-appearance-test") })
    await page.waitForTimeout(100)
    assert.equal(await page.evaluate(() => window.__contexts.length), appearanceReports)
    check("opted-in Mac tracks explicit dark/light, user theme stylesheet, root/body color changes without navigation; equivalent mutations coalesce")
    await page.evaluate(() => {
      window.__savedAnimationFrame = window.requestAnimationFrame
      window.requestAnimationFrame = () => 0 // Background WKWebView can stop painting entirely.
      Object.defineProperty(document, "visibilityState", { configurable: true, value: "hidden" })
      Object.defineProperty(document, "hasFocus", { configurable: true, value: () => false })
      document.body.style.backgroundColor = "rgb(71 82 93)"
    })
    await page.waitForFunction(() => {
      const context = window.__contexts.at(-1)
      return !context.visible && !context.focused && JSON.stringify(context.appearance?.background) === "[71,82,93]"
    }, null, { polling: 50 })
    await page.evaluate(() => {
      window.requestAnimationFrame = window.__savedAnimationFrame
      delete window.__savedAnimationFrame
      delete document.visibilityState
      delete document.hasFocus
      document.body.style.backgroundColor = "color(srgb 0.2 0.4 0.6)"
      document.dispatchEvent(new Event("visibilitychange"))
    })
    await page.waitForFunction(() => window.__contexts.at(-1).visible && JSON.stringify(window.__contexts.at(-1).appearance?.background) === "[51,102,153]")
    check("background theme metadata updates without animation frames or focus, then restores actual visibility")
    const before = await page.evaluate(() => window.__contexts.length)
    await page.evaluate(async () => { const { emit } = await import("/src/store.ts"); for (let i = 0; i < 50; i++) emit() })
    await page.waitForTimeout(150)
    assert.equal(await page.evaluate(() => window.__contexts.length), before)
    await page.evaluate(() => window.dispatchEvent(new Event("blur")))
    // hasFocus() is authoritative; a synthetic event must not invent focus loss.
    assert.equal(await page.evaluate(() => window.__contexts.at(-1).focused), await page.evaluate(() => document.hasFocus()))
    await page.evaluate(() => window.dispatchEvent(new Event("steve:shell-invalidated")))
    await page.waitForFunction((n) => window.__contexts.length > n, before)
    check("stream-like store renders do not publish metadata; native invalidation re-handshakes without reload")
    assert.equal((await navigate(page, "add", "#/hub/add")).ok, true)
    await page.locator("[data-hubs-dialog]").waitFor()
    assert.equal(await page.evaluate(() => location.hash), "#/")
    await page.keyboard.press("Escape")
    await page.locator("[data-hubs-dialog]").waitFor({ state: "detached" })
    assert.equal((await navigate(page, "pair", "#/hub/pair")).ok, true)
    await page.locator("[data-pair-device-dialog]").waitFor()
    await page.keyboard.press("Escape")
    await page.locator("[data-pair-device-dialog]").waitFor({ state: "detached" })
    check("Hub add and pair intents acknowledge actual rendered dialogs despite canonical #/ route")
    assert.equal((await navigate(page, "alias", "#/settings/access")).ok, true)
    assert.equal(await page.evaluate(() => location.hash), "#/devices/clients")
    check("settings aliases acknowledge canonical devices route, not a false failure")
    assert.equal((await navigate(page, "other-space", "#/items", { workspaceID: "other" })).ok, true)
    const last = await page.evaluate(() => window.__contexts.at(-1))
    assert.equal(last.workspaceID, "other")
    assert.equal(last.workspaceName, "Other workspace")
    assert.equal(last.route, "#/items")
    assert.equal(last.ready, true)
    assert.ok(fixture.requests.some((r) => r.method === "POST" && r.path === "/api/workspaces/other/switch"))
    assert.ok(fixture.requests.some((r) => r.path === "/api/state" && r.ws === "other"))
    check("workspace-scoped intent uses existing server selection and waits for target workspace data before routing/ack")
    let businessWrites = 0
    await page.route("**/api/me/feed**", async route => {
      const url = new URL(route.request().url())
      if (route.request().method() !== "GET") businessWrites++
      if (url.pathname === "/api/me/feed/entry-1") return route.fulfill({ json: { id: "entry-1", seq: 1, kind: "report", title: "Native target report", published_at: 1, read_at: null, excerpt: "", purged: false, source: { type: "flow", id: "flow-1" }, body: { markdown: "The specific report opened from a native notification.", fields: [], references: [], actions: [] } } })
      return route.fulfill({ json: { entries: [{ id: "entry-1", seq: 1, kind: "report", title: "Native target report", published_at: 1, read_at: null, excerpt: "", purged: false, source: { type: "flow", id: "flow-1" }, body: null }], unread_count: 1, next_cursor: null, snapshot: "feed-snapshot" } })
    })
    assert.equal((await navigate(page, "feed-list", "#/feed")).ok, true)
    await page.locator('[data-feed-entry="entry-1"] [data-feed-expand]').click()
    await page.waitForFunction(() => window.__calls.some(c => c.op === "shell.attention.visible" && c.args.targets.some(t => t.entityID === "entry-1")))
    const beforeTargetReports = await page.evaluate(() => window.__calls.filter(c => c.op === "shell.attention.visible").length)
    assert.equal((await navigate(page, "feed-target", "#/feed?entry=entry-1", { targetID: "entry-1" })).ok, true)
    await page.locator('[data-feed-target="entry-1"] [data-feed-body]').waitFor()
    const reports = await page.evaluate(() => window.__calls.filter(c => c.op === "shell.attention.visible").map(c => c.args))
    assert.ok(reports.length > beforeTargetReports, "route transitions republish an identical visible entity set after native clears it")
    assert.ok(reports.some(r => r.workspaceID === "other" && r.visible && r.targets.some(t => t.destination === "feed" && t.entityID === "entry-1")))
    assert.equal(businessWrites, 0, "opening a target is not a Feed read")
    check("native Feed target acknowledges actual expanded visible report, without an implicit read")
    await page.route("**/api/me/attention/replies/missing-reply", route => route.fulfill({ status: 404, json: { error: "Reply not found" } }))
    const failed = await navigate(page, "target", "#/", { targetID: "missing-reply" })
    assert.equal(failed.ok, false)
    assert.equal(await page.evaluate(() => location.hash), "#/")
    check("missing deep replies fail honestly after lookup rather than acknowledging an ignored target")
    // No added key handler intercepts editing or the existing composer shortcuts.
    await page.evaluate(() => { location.hash = "#/" })
    await page.waitForFunction(() => !!document.querySelector("textarea"))
    const textarea = page.locator("textarea").first()
    await textarea.fill("Native shell leaves editing with the web composer")
    assert.equal(await textarea.inputValue(), "Native shell leaves editing with the web composer")
    check("composer remains editable with web-owned controls and navigation")
  } finally { await f.close() }
  const signedOut = await open("modern", false)
  try {
    assert.equal((await signedOut.page.evaluate(() => window.__contexts.at(-1))).workspaceID, null)
    const ack = await navigate(signedOut.page, "signed-out-add", "#/hub/add")
    assert.equal(ack.ok, true)
    await signedOut.page.locator("[data-add-hub]").waitFor()
    check("signed-out Add Hub remains functional without pretending an authenticated workspace exists")
  } finally { await signedOut.close() }
  const visibilityFixture = await open("browser")
  try {
    const result = await visibilityFixture.page.evaluate(async () => {
      const { onScreen, visibleAttentionTargets } = await import("/src/native-attention-visible.ts")
      const host = document.createElement("div")
      host.style.cssText = "position:fixed;top:100px;left:100px;width:200px;height:80px;overflow:hidden;z-index:99999"
      host.innerHTML = '<div data-attention-reply="visible" style="height:60px">Visible</div><div data-attention-reply="clipped" style="margin-top:100px;height:60px">Clipped</div><div data-attention-reply="hidden" style="display:none">Hidden</div>'
      document.body.append(host)
      const clipped = onScreen(host.children[1]), hidden = onScreen(host.children[2])
      const targets = visibleAttentionTargets("#/").filter(t => ["visible", "clipped", "hidden"].includes(t.entityID))
      host.remove(); return { clipped, hidden, targets }
    })
    assert.deepEqual(result, { clipped: false, hidden: false, targets: [{ destination: "reply", entityID: "visible" }] })
    check("foreground visibility intersects clipping ancestors and excludes hidden/offscreen replies")
  } finally { await visibilityFixture.close() }
  assert.deepEqual(errors, [])
  console.log(JSON.stringify({ passed: true, scenarios: results }, null, 2))
} finally { await browser.close(); await vite.close(); await fixture.close() }
