// Vite-only fixture over mocked Hub APIs: service write policy stays read-only while the Hub's
// personal-send capability switch toggles on/off across Accounts and Integrations in zh-CN/en.
import assert from "node:assert/strict"
import { mkdir } from "node:fs/promises"
const { chromium } = await import(process.env.PLAYWRIGHT_MODULE || "playwright")

const web = process.env.STEVE_TEST_WEB || "http://127.0.0.1:5174"
const browser = await chromium.launch({ headless: true, ...(process.env.PLAYWRIGHT_EXECUTABLE ? { executablePath: process.env.PLAYWRIGHT_EXECUTABLE } : {}) })
const id = "conn_ext", account = "acc_ext", now = Date.now()
let policyValue = "allowed", caps = ["msg.receive"], writes = 0

const connection = () => ({
  id, account_id: account, adapter: "ext_ws", auth: "credential", status: "ok", installed: true,
  caps: [...caps], available_caps: ["msg.receive", "msg.send_as_user"],
  health: { last_ok: now, last_error: "", error_at: 0 }, last_sync: now,
  detail: { external_write_policy: { policy: policyValue, observed_at: now } },
})
const accountView = () => ({
  id: account, system: { id: "ext_ws", name: "External Chat", kind: "comm" },
  party: { id: "party_me", name: "Me", type: "person", is_me: true },
  kind: "personal", handle: "me@ext.example", display_name: "My External Chat",
  verified: true, confidence: 1, source: "fixture", source_label: "Fixture",
  connection: { id, status: "ok", caps: [...caps], last_sync: now }, vault_items: [], revision: 1,
})
const integrationsView = () => {
  const iconn = {
    id, account_id: account, adapter: "ext_ws", adapter_name: "External Chat", identity: "self",
    name: "My External Chat", handle: "me@ext.example", status: "ok", installed: true,
    health: { last_ok: now, last_error: "", error_at: 0 }, last_sync: now,
    capabilities: [
      { area: "channel", state: "on", missing_scopes: [], suggested_scopes: [], caps: ["msg.receive"] },
      { area: "send_as_you", state: caps.includes("msg.send_as_user") ? "on" : "off", missing_scopes: [], suggested_scopes: [], caps: caps.includes("msg.send_as_user") ? ["msg.send_as_user"] : [] },
    ],
    detail: { external_write_policy: { policy: policyValue, observed_at: now } },
  }
  return {
    areas: [{ id: "channel", label: "Chat" }], callback_urls: [],
    providers: [{
      id: "ext_ws", name: "External Chat", status: "supported",
      areas: [{ area: "channel", state: "supported" }],
      adapters: [{ id: "ext_ws", name: "External Chat", identity: "self", auth: "credential", device: false, capabilities: [], pairing: null, channel: null }],
      connections: [iconn],
    }],
    installations: [{
      id: "inst_ext", provider: "ext_ws", provider_name: "External Chat", name: "My External Chat",
      status: "ok", missing: 0, identities: [{ kind: "you", connection: iconn }],
    }],
  }
}

const page = await browser.newPage({ locale: "zh-CN" })
const errors = []
page.on("pageerror", (e) => errors.push(e.message))
await page.addInitScript(() => localStorage.setItem("steve-lang", new URL(location.href).searchParams.get("lang") || "zh-CN"))
await page.route("**/*", async (route) => {
  const req = route.request(), u = new URL(req.url())
  if (u.origin !== new URL(web).origin) return route.abort("blockedbyclient")
  if (!u.pathname.startsWith("/api/")) return route.continue()
  const m = req.method(), p = u.pathname
  if (p === `/api/accounts/${account}` && m === "GET") return route.fulfill({ json: accountView() })
  if (p === "/api/connections" && m === "GET") return route.fulfill({ json: [connection()] })
  if (p === `/api/connections/${id}` && m === "GET") return route.fulfill({ json: connection() })
  if (p === `/api/connections/${id}` && m === "PATCH") {
    const body = req.postDataJSON()
    if (Array.isArray(body?.caps)) caps = [...body.caps]
    return route.fulfill({ json: connection() })
  }
  if (p === "/api/connections/adapters" && m === "GET") return route.fulfill({ json: [{ id: "ext_ws", system: "ext_ws", name: "External Chat", auth: "credential", caps: ["msg.receive", "msg.send_as_user"], device: false }] })
  if (p === "/api/connections/integrations" && m === "GET") return route.fulfill({ json: integrationsView() })
  if (p === "/api/channels" && m === "GET") return route.fulfill({ json: { accounts: [{ connection_id: id, mode: "self", permissions: null, persona: null, rules: [], trigger: null, receive: null, send_delay_s: null, send_delay_default_s: 3 }] } })
  if (p === "/api/spaces" && m === "GET") return route.fulfill({ json: [] })
  if (p === "/api/personas" && m === "GET") return route.fulfill({ json: [] })
  if (p === "/api/me/contact" && m === "GET") return route.fulfill({ status: 404, json: { error: "not_found" } })
  if (p.startsWith("/api/stewards/") && m === "GET") return route.fulfill({ status: 404, json: { error: "not_found" } })
  if (m !== "GET") writes++
  return route.fulfill({ status: 404, json: { error: "fixture" } })
})

const fixture = web + "/test/fixtures/external-write-gates.html"
const fits = async () => assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), true, "no horizontal overflow")
try {
  if (process.env.STEVE_TEST_SCREENSHOTS) await mkdir(process.env.STEVE_TEST_SCREENSHOTS, { recursive: true })
  for (const lang of ["zh-CN", "en"]) {
    const localized = fixture + "?lang=" + lang
    for (const value of ["allowed", "restricted", "forbidden"]) {
      policyValue = value
      caps = ["msg.receive"]
      await page.goto(localized + "#/accounts/" + account)
      await page.waitForFunction((l) => document.documentElement.lang === l, lang)
      await page.locator(`[data-service-write-policy="${value}"]`).waitFor()
      assert.match(await page.locator(`[data-service-write-policy="${value}"]`).innerText(), lang === "en" ? /Service write policy/ : /服务端写策略/)
      const toggle = page.locator('[data-cap-toggle="msg.send_as_user"]')
      assert.equal(await toggle.isChecked(), false)
      await toggle.click()
      await page.waitForFunction(() => {
        const el = document.querySelector('[data-cap-toggle="msg.send_as_user"]')
        return el?.checked === true || el?.getAttribute("aria-checked") === "true"
      })
      assert.ok(caps.includes("msg.send_as_user"))
      assert.equal(policyValue, value, "the switch cannot rewrite service policy")
      assert.equal(writes, 0, "enabling cannot replay anything")
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
      await page.waitForFunction(() => {
        const el = document.querySelector('[data-cap-toggle="msg.send_as_user"]')
        return el?.checked === false || el?.getAttribute("aria-checked") === "false"
      })
      await page.goto(localized + "#/integrations/ext_ws")
      await page.locator('[data-hub-write-policy="off"]').waitFor()
      await page.locator(`[data-service-write-policy="${value}"]`).waitFor()
      if (process.env.STEVE_TEST_SCREENSHOTS) await page.screenshot({ path: `${process.env.STEVE_TEST_SCREENSHOTS}/integration-${lang}-${value}-390.png`, animations: "disabled" })
    }
  }
  assert.deepEqual(errors, [])
  console.log("PASS: mocked Hub APIs, existing Hub switch toggled on/off for all service policies, connection/account agreement, no replay, zh-CN/en, 1440px/390px, no page errors")
} finally {
  await browser.close()
}
