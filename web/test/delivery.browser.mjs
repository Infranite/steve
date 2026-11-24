/* Runs production routes against an isolated HTTP fixture. This is front-end contract evidence, not Hub integration.
 * PLAYWRIGHT_MODULE / PLAYWRIGHT_EXECUTABLE / PLAYWRIGHT_CHANNEL choose an installed browser; STEVE_TEST_SHOTS saves evidence. */
import assert from "node:assert/strict"
import { mkdir, readFile, writeFile } from "node:fs/promises"
import { fileURLToPath } from "node:url"
import { createServer } from "vite"
import { deliveryFixture, longName, staticHtmlCsp } from "./fixtures/delivery-api.ts"
import { settle } from "./settle.mjs"

const { chromium } = await import(process.env.PLAYWRIGHT_MODULE || "playwright")
const fixture = await deliveryFixture()
const previousHub = process.env.STEVE_HUB
process.env.STEVE_HUB = fixture.url
const root = fileURLToPath(new URL("..", import.meta.url))
const web = await createServer({ root, configFile: root + "/vite.config.ts", server: { host: "127.0.0.1", port: 0, strictPort: true }, logLevel: "error" })
await web.listen()
const base = `http://127.0.0.1:${web.httpServer.address().port}`
const browser = await chromium.launch({ headless: true,
  ...(process.env.PLAYWRIGHT_CHANNEL ? { channel: process.env.PLAYWRIGHT_CHANNEL } : {}),
  ...(process.env.PLAYWRIGHT_EXECUTABLE ? { executablePath: process.env.PLAYWRIGHT_EXECUTABLE } : {}),
})
const errors = [], shots = process.env.STEVE_TEST_SHOTS
if (shots) await mkdir(shots, { recursive: true })
async function screenshot(page, name) { if (shots) { await settle(page); await page.screenshot({ path: `${shots}/${name}.png`, fullPage: true }) } }
async function noOverflow(page) {
  assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), "no horizontal document overflow")
  const over = await page.locator("[data-delivery]").evaluate((el) => {
    const bound = el.getBoundingClientRect()
    return [...el.querySelectorAll("h1,p,button,a")].filter((e) => e.getBoundingClientRect().right > bound.right + 1).map((e) => e.outerHTML.slice(0, 180))
  })
  assert.deepEqual(over, [], "file names, links and actions fit the page")
}
async function context(locale = "en-US", width = 390) {
  const c = await browser.newContext({ locale, viewport: { width, height: 844 }, reducedMotion: "reduce", permissions: ["clipboard-read", "clipboard-write"] })
  c.on("page", (p) => p.on("pageerror", (e) => errors.push(e.message)))
  return c
}
try {
  const owner = await context()
  // A saved Hub must never redirect a stable file link to a different installation.
  await owner.addInitScript(() => { try { localStorage.setItem("steve.hub", "https://unused.invalid") } catch { /* opaque sandbox document */ } })
  const page = await owner.newPage()
  await page.goto(base + "/deliveries/md?ws=fixture")
  await page.locator("[data-login]").click()
  await page.locator('[data-out-md] [data-streamdown="strong"]').waitFor()
  assert.match(page.url(), /\/deliveries\/md\?ws=fixture$/)
  assert.equal(await page.locator("[data-delivery-name]").innerText(), longName)
  assert.equal(fixture.shares.size, 0, "opening a delivery never creates a share")
  assert.ok(fixture.requests.some((r) => r.path === "/api/file-deliveries/md" && r.ws === "fixture"))
  await noOverflow(page)
  await screenshot(page, "delivery-mobile-markdown")
  await page.locator("[data-delivery-copy]").click()
  assert.equal(await page.evaluate(() => navigator.clipboard.readText()), base + "/deliveries/md?ws=fixture")
  const downloadWait = page.waitForEvent("download")
  await page.locator("[data-delivery-download]").click()
  const original = await downloadWait
  assert.match(await readFile(await original.path(), "utf8"), /Persistent \*\*Markdown\*\*/)
  await page.locator("[data-share-duration]").click()
  await page.getByRole("option", { name: "1 hour", exact: true }).click()
  await page.locator("[data-share-create]").click()
  await page.locator("[data-share-url]").waitFor()
  const shareUrl = await page.locator("[data-share-url]").getAttribute("href")
  const token = new URL(shareUrl).pathname.split("/").pop()
  assert.ok(fixture.shares.get(token).expires_at - Date.now() <= 3_600_000)
  assert.ok(fixture.shares.get(token).expires_at - Date.now() > 3_500_000)
  await page.locator("[data-share-copy]").click()
  assert.equal(await page.evaluate(() => navigator.clipboard.readText()), shareUrl)
  await page.locator("[data-loopback-warning]").first().waitFor()
  await screenshot(page, "delivery-share-management")

  const anonymous = await context("zh-CN", 320), publicPage = await anonymous.newPage()
  const start = fixture.requests.length
  await publicPage.goto(shareUrl + "?ws=foreign")
  await publicPage.locator('[data-out-md] [data-streamdown="strong"]').waitFor()
  assert.equal(await publicPage.locator("[data-delivery-shares], [data-delivery-item]").count(), 0)
  assert.deepEqual(fixture.requests.slice(start).filter((r) => r.path.startsWith("/api/") && !r.path.startsWith("/api/delivery-shares/")), [], "anonymous page only requests the token-scoped API")
  const metaRequest = fixture.requests.slice(start).find((r) => r.path === `/api/delivery-shares/${token}`)
  assert.equal(metaRequest.ws, undefined)
  assert.equal(metaRequest.cookie, undefined)
  assert.equal(metaRequest.query, "")
  await noOverflow(publicPage)
  await screenshot(publicPage, "share-mobile-chinese")
  const publicDownloadWait = publicPage.waitForEvent("download")
  await publicPage.locator("[data-delivery-download]").click()
  assert.match(await readFile(await (await publicDownloadWait).path(), "utf8"), /Persistent/)
  assert.equal((await anonymous.request.get(base + "/api/file-deliveries/md")).status(), 401)
  await page.locator("[data-share-revoke]").click()
  await page.locator('[data-share-state="revoked"]').waitFor()
  await publicPage.evaluate(() => dispatchEvent(new Event("focus")))
  await publicPage.locator("[data-delivery-error]").waitFor()
  assert.equal(await publicPage.locator("[data-out-md]").count(), 0, "revoked share removes already-loaded preview on recheck")
  assert.equal((await anonymous.request.get(base + `/api/delivery-shares/${token}/raw`)).status(), 410)
  await screenshot(publicPage, "share-revoked")

  await page.locator("[data-share-create]").click()
  await page.locator("[data-share-url]").waitFor()
  const expiringUrl = await page.locator("[data-share-url]").getAttribute("href")
  const expiringToken = new URL(expiringUrl).pathname.split("/").pop()
  fixture.shares.get(expiringToken).expires_at = Date.now() + 3500
  await publicPage.goto(expiringUrl)
  await publicPage.locator("[data-out-md]").waitFor()
  await publicPage.locator("[data-delivery-error]").waitFor({ timeout: 10_000 })
  assert.equal((await anonymous.request.get(base + `/api/delivery-shares/${expiringToken}/content/file`)).status(), 410)

  await page.goto(base + "/deliveries/html")
  const frame = page.frameLocator("[data-delivery-html]")
  await frame.getByRole("heading", { name: "Design entry" }).waitFor()
  assert.equal(await frame.locator("h1").evaluate((el) => getComputedStyle(el).color), "rgb(20, 100, 180)")
  assert.equal(await frame.locator("img").evaluate((el) => el.naturalWidth), 1)
  assert.equal(await page.locator("body").getAttribute("data-escaped"), null)
  assert.equal(await page.locator("[data-delivery-html]").getAttribute("sandbox"), "allow-same-origin")
  assert.equal(await frame.locator("body").getAttribute("data-executed"), null, "authenticated static preview runs no scripts")
  await frame.getByRole("button", { name: "Run script" }).click()
  assert.equal(await frame.getByRole("button").innerText(), "Run script", "inline handlers stay disabled")
  assert.equal((await owner.request.get(base + "/api/file-deliveries/html/content/pages/index.html")).headers()["content-security-policy"], staticHtmlCsp)
  await frame.getByRole("link", { name: "Open details" }).click()
  await frame.getByRole("heading", { name: "Design details" }).waitFor()
  await page.locator("[data-html-reset]").click()
  await frame.getByRole("heading", { name: "Design entry" }).waitFor()
  await screenshot(page, "delivery-html-relative-assets")

  // Repeat HTML navigation anonymously: relative resources retain the share token in their path.
  await page.locator("[data-share-create]").click()
  const htmlShare = await page.locator("[data-share-url]").getAttribute("href")
  await publicPage.goto(htmlShare)
  const sharedFrame = publicPage.frameLocator("[data-delivery-html]")
  await sharedFrame.getByRole("heading", { name: "Design entry" }).waitFor()
  assert.equal(await publicPage.locator("[data-delivery-html]").getAttribute("sandbox"), "allow-same-origin")
  assert.equal(await sharedFrame.locator("body").getAttribute("data-executed"), null)
  await sharedFrame.getByRole("button", { name: "Run script" }).click()
  assert.equal(await sharedFrame.getByRole("button").innerText(), "Run script", "sharing grants no script capability")
  assert.match(await publicPage.locator("[data-delivery]").innerText(), /静态预览/)
  assert.equal(await sharedFrame.locator("h1").evaluate((el) => getComputedStyle(el).color), "rgb(20, 100, 180)")
  assert.equal(await sharedFrame.locator("img").evaluate((el) => el.naturalWidth), 1)
  await sharedFrame.getByRole("link", { name: "Open details" }).click()
  await sharedFrame.getByRole("heading", { name: "Design details" }).waitFor()
  assert.equal(await sharedFrame.locator("h1").evaluate((el) => getComputedStyle(el).color), "rgb(20, 100, 180)")
  assert.equal(await sharedFrame.locator("img").evaluate((el) => el.naturalWidth), 1)
  await screenshot(publicPage, "share-static-html-navigation")

  // Exercise a real workspace switch endpoint in this fixture rather than faking a cookie name.
  assert.equal((await owner.request.post(base + "/api/workspaces/other/switch", { headers: { "x-steve-ws": "fixture" }, data: {} })).status(), 200)
  assert.equal((await (await owner.request.get(base + "/api/shell")).json()).workspace.id, "other")
  const switchedAt = fixture.requests.length
  await page.goto(base + "/deliveries/html")
  await frame.getByRole("heading", { name: "Design entry" }).waitFor()
  const afterSwitch = fixture.requests.slice(switchedAt)
  const firstMeta = afterSwitch.find((r) => r.path === "/api/file-deliveries/html")
  assert.equal(firstMeta.ws, undefined, "ownership discovery must precede state binding")
  assert.equal(firstMeta.query, "")
  assert.ok(afterSwitch.some((r) => r.path === "/api/shell" && r.ws === "fixture"))
  await frame.getByRole("link", { name: "Open details" }).click()
  await frame.getByRole("heading", { name: "Design details" }).waitFor()
  assert.equal(await frame.locator("img").evaluate((el) => el.naturalWidth), 1)
  await screenshot(page, "delivery-after-workspace-switch")
  const itemUrl = await page.locator("[data-delivery-item]").getAttribute("href")
  assert.match(itemUrl, /\?ws=fixture#\/items\/ST-100$/)
  await page.locator("[data-delivery-item]").click()
  await page.locator("[data-now]").waitFor()
  await page.getByRole("tab", { name: "Outputs", exact: true }).click()
  await page.locator("[data-outs]").waitFor()
  await page.goto(base + "/deliveries/html?ws=other")
  await page.locator("[data-delivery-error]").waitFor()
  assert.equal(await page.locator("[data-delivery-html]").count(), 0)
  fixture.memberships.delete("fixture")
  await page.goto(base + "/deliveries/html")
  await page.locator("[data-delivery-error]").waitFor()
  assert.equal(await page.locator("[data-delivery-html]").count(), 0)
  fixture.memberships.add("fixture")
  await owner.request.post(base + "/api/workspaces/fixture/switch", { headers: { "x-steve-ws": "other" }, data: {} })

  await page.goto(base + "/deliveries/image")
  await page.locator("[data-delivery-image]").waitFor()
  assert.equal(await page.locator("[data-delivery-image]").evaluate((el) => el.complete && el.naturalWidth), 1)
  await screenshot(page, "delivery-image")
  await page.goto(base + "/deliveries/office")
  await page.locator('[data-office="docx"]').waitFor()
  assert.match(await page.locator('[data-office="docx"]').innerText(), /Delivered Office document/)
  await screenshot(page, "delivery-office")
  const pdfResponse = page.waitForResponse((r) => r.url().includes("/api/file-deliveries/pdf/content/file") && r.status() === 200)
  await page.goto(base + "/deliveries/pdf")
  await page.locator("[data-delivery-pdf]").waitFor()
  assert.equal((await pdfResponse).headers()["content-type"], "application/pdf")
  await screenshot(page, "delivery-pdf")
  await page.goto(base + "/deliveries/binary")
  await page.locator("[data-delivery-no-preview]").waitFor()
  assert.ok(await page.locator("[data-delivery-download]").isVisible())
  await page.goto(base + "/deliveries/missing")
  await page.locator("[data-delivery-error]").waitFor()
  await page.locator("[data-delivery-retry]").click()
  await page.locator("[data-delivery-error]").waitFor()
  await publicPage.goto(base + "/shares/nonexistent")
  await publicPage.locator("[data-delivery-error]").waitFor()

  // The production item card reads its artifact associations from the live fixture API.
  await page.goto(base + "/test/fixtures/delivery-cards.html")
  await page.getByRole("button", { name: "Delivery details · 4 records", exact: true }).click()
  await page.locator('[data-delivery-card="md"]').waitFor()
  const card = page.locator('[data-delivery-card="md"]')
  assert.match(await card.locator("[data-delivery-open]").getAttribute("href"), /\/deliveries\/md\?ws=fixture$/)
  const popupWait = page.waitForEvent("popup")
  await card.locator("[data-delivery-open]").click()
  const popup = await popupWait
  await popup.locator("[data-out-md]").waitFor()
  assert.match(popup.url(), /\/deliveries\/md\?ws=fixture$/)
  await popup.close()
  const cardDownloadWait = page.waitForEvent("download")
  await card.locator("[data-delivery-download]").click()
  assert.match(await readFile(await (await cardDownloadWait).path(), "utf8"), /Persistent/)
  await card.locator("[data-delivery-preview]").click()
  await page.locator("[data-preview] [data-out-md]").waitFor()
  await page.keyboard.press("Escape")
  const legacy = page.locator('[data-delivery-card="none"][data-artifact="artifact-old"]')
  assert.equal(await legacy.locator("a").count(), 0, "local paths never become remote links")
  const foreign = page.locator('[data-delivery-card="none"][data-artifact="artifact-foreign"]')
  assert.equal(await foreign.locator("a").count(), 0, "foreign delivery-shaped URLs never become this Hub's links")
  await legacy.locator("[data-delivery-preview]").click()
  await page.locator("[data-preview] [data-out-md]").waitFor()
  await page.keyboard.press("Escape")
  await screenshot(page, "chat-delivery-cards")

  for (const [width, locale] of [[320, "zh-CN"], [1440, "en-US"]]) {
    const c = await context(locale, width)
    await c.addCookies(await owner.cookies())
    const p = await c.newPage()
    await p.goto(base + "/deliveries/md")
    await p.locator("[data-out-md]").waitFor()
    await noOverflow(p)
    await screenshot(p, `delivery-layout-${width}`)
    await c.close()
  }
  assert.deepEqual(errors, [], "no uncaught page errors")
  console.log("PASS: login return, metadata ownership before App state, workspace switch and item link, wrong selector/non-member denial, Markdown, DOCX, image, PDF response, binary fallback, logged-in/shared static HTML CSP/CSS/image/navigation, expiring/revocable anonymous shares, downloads, chat/legacy/external artifacts, 320/390/1440px and both languages. Isolated fixture HTTP APIs; no real Hub or second device tested by this script.")
} catch (e) {
  console.error("Page errors:", errors)
  console.error("Recent fixture requests:", fixture.requests.slice(-24))
  for (const [i, c] of browser.contexts().entries()) for (const [j, p] of c.pages().entries()) {
    console.error("Open page:", p.url(), (await p.locator("body").innerText()).slice(0, 4000))
    if (shots) { await screenshot(p, `failure-${i}-${j}`); await writeFile(`${shots}/failure-${i}-${j}.html`, await p.content()) }
  }
  throw e
} finally {
  await browser.close(); await web.close(); await fixture.close()
  if (previousHub === undefined) delete process.env.STEVE_HUB; else process.env.STEVE_HUB = previousHub
}
