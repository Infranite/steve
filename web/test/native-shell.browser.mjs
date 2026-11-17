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
    localStorage.setItem("steve-lang", "en")
    window.__contexts = []; window.__acks = []; window.__intents = []; window.__calls = []
    if (mode === "browser") return
    window.__STEVE_NATIVE__ = { v: 2, platform: mode === "ios" ? "ios" : "macos", can: ["hubs.list", "hubs.switch", "hubs.add", "hubs.forget", "hubs.rename", "pair.claim", ...(mode === "older" ? [] : ["shell.context", "shell.navigation.next", "shell.navigation.ack"]), ...(mode === "modern" ? ["shell.chrome.hub"] : [])], call: async (op, args) => {
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
  for (const mode of ["browser", "older", "ios", "modern"]) {
    const f = await open(mode)
    try {
      assert.equal(await f.page.locator("[data-hub-switch]").count(), mode === "modern" ? 0 : 1, mode)
      assert.equal(await f.page.locator("[data-ws-switch]").count(), 1)
      assert.ok(await f.page.locator('[data-nav="items"]').count())
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
    assert.deepEqual(Object.keys(initial).sort(), ["workspaceID", "workspaceName", "route", "title", "ready", "authenticated", "focused", "visible"].sort())
    check("App-level context contains selected workspace and metadata only, never credentials or web-supplied Hub identity")
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
    const failed = await navigate(page, "target", "#/", { targetID: "not-yet-supported-reply" })
    assert.equal(failed.ok, false)
    assert.match(failed.error, /not available/)
    assert.equal(await page.evaluate(() => location.hash), "#/items")
    check("unsupported deep reply targets fail honestly rather than acknowledging an ignored target")
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
  assert.deepEqual(errors, [])
  console.log(JSON.stringify({ passed: true, scenarios: results }, null, 2))
} finally { await browser.close(); await vite.close(); await fixture.close() }
