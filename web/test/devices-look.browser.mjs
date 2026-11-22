// Screenshot check of the devices pages: every page and its main states (the overview with a few, many and no Nodes,
// the Hub, the Node list, a Node in each of its version states and with its limit's tip tapped open, the clients, a
// client's sign-out, the Add device steps) at 1440 and 390 px (390 px as a touch screen), light and dark, on the real
// pages with an isolated owner API and example names only.
//
// Each screen is checked as it is drawn: nothing runs past the window or is cut off at its right edge, no text key is
// shown instead of its words, the dark theme is on when asked, and the page logs no error and asks the Hub for nothing
// this fixture does not answer. With STEVE_TEST_SHOTS=<dir> every screen is written there as
// <screen>-<width>-<theme>.png; with STEVE_TEST_BASELINE=<dir> as well, each is compared with the same file there (an
// earlier run on the same machine, for example on the base branch) and a screen whose pixels moved fails, with a
// <screen>.diff.png beside it that marks what moved. The screens are drawn against a fixed clock, so two runs on one
// machine match pixel for pixel; STEVE_TEST_SHOT_TOLERANCE (a share of a screen's pixels, default 0) lets some move.
// STEVE_TEST_ONLY=<word> runs only the screens whose name contains it.
// Run against Vite, with external Playwright (PLAYWRIGHT_MODULE, PLAYWRIGHT_EXECUTABLE, STEVE_TEST_WEB).
import assert from "node:assert/strict"
import { mkdir, readFile, writeFile } from "node:fs/promises"
import { existsSync } from "node:fs"
const { chromium } = await import(process.env.PLAYWRIGHT_MODULE || "playwright")
const browser = await chromium.launch({ headless: true, ...(process.env.PLAYWRIGHT_EXECUTABLE ? { executablePath: process.env.PLAYWRIGHT_EXECUTABLE } : {}) })
const base = process.env.STEVE_TEST_WEB || "http://127.0.0.1:5198"
const SHOTS = process.env.STEVE_TEST_SHOTS, BASELINE = process.env.STEVE_TEST_BASELINE, ONLY = process.env.STEVE_TEST_ONLY
const TOLERANCE = Number(process.env.STEVE_TEST_SHOT_TOLERANCE ?? 0)
if (SHOTS) await mkdir(SHOTS, { recursive: true })

/* the clock every screen is drawn against, fixed in the page too (in UTC): relative times ("3 小时前"), a chart's hours
 * and a code's countdown come out the same on every run */
const NOW = Date.UTC(2026, 10, 17, 12, 0), MIN = 60_000, HOUR = 60 * MIN, DAY = 24 * HOUR, GIB = 2 ** 30, HUB = "0.2.112"
const FP = "9e3f5a07c21b84d60f9a77e3b10c4d52"
const run = (phase, extra = {}) => ({ operation_id: "nup_1", version: HUB, from_version: "0.2.109", phase, error: null, requested_by: "Owner", force: false, created: NOW - 6 * MIN, updated: NOW - 2 * MIN, ...extra })
const up = (v, extra = {}) => ({ build: { version: v, commit: "abc", target: "x" }, policy: "notify", own_policy: null, can: true, reason: v === HUB ? "current" : "outdated", latest: null, ...extra })
const link = (route, via = "direct", tls = false) => ({ route, via, tls, since: NOW - DAY, latency_ms: 38, latency_at: NOW })
const host = (os, os_version, arch, cpus, gb) => ({ os, os_version, arch, cpus, mem_total: gb * GIB })
const node = (id, name, extra = {}) => ({ node_id: id, name, fingerprint: FP, state: "active", linked: true, local: false, max_sessions: 6, running: 0, waiting: 0, last_seen: NOW - 30_000, upgrade: up(HUB), link: link("lan"), ...extra })
const local = node("local", "Studio Mac", { local: true, max_sessions: 32, running: 3, upgrade: undefined, link: null, host: host("macos", "26.1", "aarch64", 14, 64) })
const build = node("n-build", "build-linux-01", { running: 1, max_sessions: 4, upgrade: up("0.2.109"), link: link("public", "direct", true), host: host("linux", "24.04", "x86_64", 16, 32) })
const mbp = node("n-mbp", "Studio MacBook Pro", { running: 2, link: link("loopback", "tailcat"), host: host("macos", "26.1", "aarch64", 12, 36) })
const imac = node("n-imac", "studio-imac", { linked: false, last_seen: NOW - 3 * HOUR, link_error: { at: NOW - 3 * HOUR, error: "connection refused: tcp connect" }, upgrade: up("0.2.110", { reason: "unknown", can: false }), host: host("macos", "15.6", "x86_64", 8, 16) })
const scenes = {
  normal: () => [local, build, mbp, imac],
  empty: () => [local],
  many: () => [local, ...Array.from({ length: 10 }, (_, i) => node("r" + i, "runner-" + String(i + 1).padStart(2, "0"), { running: i % 3, link: link(i % 2 ? "tailnet" : "lan"), host: host("linux", "24.04", "aarch64", 8, 16), ...(i === 3 ? { linked: false, last_seen: NOW - 2 * HOUR } : {}) }))],
  upgrading: () => [local, { ...build, upgrade: up("0.2.109", { latest: run("draining") }) }, mbp, imac],
  failed: () => [local, { ...build, upgrade: up("0.2.109", { latest: run("rolled_back", { error: "the new release did not link within 180 s; went back to 0.2.109" }) }) }, mbp, imac],
  pairing: () => [local, build, mbp, imac, node("n-new", "mac-studio-lab", { state: "pending", link: { ...link("lan"), peer: "192.168.1.42" }, host: host("macos", "26.1", "aarch64", 24, 96) })],
}
/* a day of samples, one every ten minutes, with a gap: the charts and sparklines draw from these */
const series = (seed) => Array.from({ length: 144 }, (_, i) => ({ at: NOW - (143 - i) * 10 * MIN, load: i > 60 && i < 66 ? null : 0.2 + 0.15 * Math.sin((i + seed) / 9), mem: 0.4 + 0.05 * Math.cos((i + seed) / 13), rtt: 30 + 10 * Math.sin((i + seed) / 7) }))
const latest = (cpus, gb, load, freeGb) => ({ at: NOW - 20_000, cpus, load: [load, load * 0.9, load * 0.8], mem_total: gb * GIB, mem_available: gb * 0.55 * GIB, roots: [{ root: "/work", display: "~/Code", free: freeGb * GIB, total: 994 * GIB }], rtt_ms: 38 })
const vitals = (nodes) => Object.fromEntries(nodes.filter((n) => n.linked).map((n, i) => [n.node_id, { sampling: { interval_ms: 60_000, stale_after_ms: 180_000, now: NOW }, latest: latest(n.host?.cpus ?? 8, (n.host?.mem_total ?? 16 * GIB) / GIB, (n.host?.cpus ?? 8) * 0.4, 212), series: series(i * 11) }]))
const client = (id, name, kind, platform, extra = {}) => ({ id, user_id: "u1", token_id: null, kind, name, platform, version: HUB, origin: null, created: NOW - 90 * DAY, last_seen: NOW - MIN, last_route: "lan", last_ip: null, revoked: null, ...extra })
const clients = [client("c1", "This browser", "web", "macOS"), client("c2", "Phone", "ios", "iOS", { online: true, last_route: "tailnet+tls" }), client("c3", "Steve for Mac", "mac", "macOS", { last_seen: NOW - 22 * MIN, last_route: "tailnet" }),
  client("c4", "ci-bot", "cli", "linux", { last_seen: NOW - 46 * DAY, last_route: "public+tls", token_id: "t1" }), client("c5", "Old laptop", "web", "macOS", { last_seen: NOW - 12 * DAY }), client("c6", "Signed out", "web", "macOS", { revoked: NOW - 5 * DAY })]
const tokens = [{ id: "t1", name: "ci-bot", created: NOW - 60 * DAY, expires: null, last_used: NOW - 46 * DAY, revoked: null }]
const runtimes = [{ id: "claude", name: "Claude Code", ok: true, version: "2.1.325", path: "/usr/local/bin/claude", models: [], note: "" }, { id: "codex", name: "Codex", ok: true, version: "0.130.0", path: "/usr/local/bin/codex", models: [], note: "" }]
const metadata = { contract: 1, capabilities: { shared_metadata: 1, addresses: 1 }, metadata: { hub_id: "hub_example", display_name: "Studio", purpose_note: "Data, scheduling and model calls for the team.", revision: 3, updated_at: NOW - 3 * DAY, updated_by: "Owner" }, can_edit: true, edit_reason: null, device_id: "hub",
  addresses: [{ url: "https://hub.example.net", page: "https://hub.example.net", kind: "public" }, { url: "http://192.168.1.10:8080", page: "http://192.168.1.10:8080", kind: "lan" }] }

/** the screens: a name, a set of Nodes, where to go, what shows it is drawn, what to do before the picture (`then`)
 *  and what it must show (`check`) */
const screens = [
  { name: "overview", scene: "normal", hash: "#/devices", ready: '[data-map-card="hub"]' },
  { name: "overview-10-nodes", scene: "many", hash: "#/devices", ready: '[data-map-node="r9"]' },
  { name: "overview-empty", scene: "empty", hash: "#/devices", ready: '[data-map-card="add"]' },
  { name: "hub", scene: "normal", hash: "#/devices/hub", ready: "[data-hub-edit]:not([disabled])", check: async (page) => {
    /* the Hub's own addresses, each with its kind, not only the one this client uses */
    assert.deepEqual(await page.locator("[data-hub-addresses]").evaluateAll((xs) => xs.map((x) => x.dataset.hubAddresses)), ["public", "lan"])
  } },
  { name: "nodes-list", scene: "normal", hash: "#/devices/nodes", ready: '[data-node-row="n-imac"]' },
  { name: "node-local", scene: "normal", hash: "#/devices/nodes/local", ready: "[data-node-trends]" },
  { name: "node-remote-mbp", scene: "normal", hash: "#/devices/nodes/n-mbp", ready: '[data-node-upgrade="current"]' },
  { name: "node-limit-tip", scene: "normal", hash: "#/devices/nodes/n-mbp", ready: "[data-limit-how]", then: async (page) => {
    /* what changing the limit does opens on a tap (390 px is a touch screen here) or a click, not only on hover or focus */
    const how = page.locator("[data-limit-how]")
    if (page.viewportSize().width < 600) await how.tap(); else await how.click()
    await page.locator("[data-limit-semantics]").waitFor()
  }, check: async (page) => {
    assert.match(await page.locator("[data-limit-semantics]").innerText(), /无需重启[\s\S]*不打断/)
    assert.equal(await page.locator("[data-limit-how]").getAttribute("aria-expanded"), "true")
  } },
  { name: "node-outdated", scene: "normal", hash: "#/devices/nodes/n-build", ready: '[data-node-upgrade="outdated"]' },
  { name: "node-offline", scene: "normal", hash: "#/devices/nodes/n-imac", ready: "[data-node-offline]" },
  { name: "node-upgrading", scene: "upgrading", hash: "#/devices/nodes/n-build", ready: '[data-node-upgrade="running"]' },
  { name: "node-upgrade-failed", scene: "failed", hash: "#/devices/nodes/n-build", ready: '[data-node-upgrade="failed"]' },
  { name: "clients", scene: "normal", hash: "#/devices/clients", ready: '[data-client="c4"]' },
  { name: "clients-revoke", scene: "normal", hash: "#/devices/clients", ready: '[data-client="c4"]', then: async (page) => { await page.locator('[data-client="c3"] [data-revoke-client]').click(); await page.locator("[data-revoke-dialog]").waitFor() } },
  { name: "pairing", scene: "normal", hash: "#/devices", ready: '[data-map-card="hub"]', then: async (page) => {
    await page.locator("[data-add-device-open]").click(); await page.locator("[data-add-kind=node]").click(); await page.locator(".ui-stpc__btn-primary").click()
    await page.locator('[data-add-via="direct"]').waitFor(); await page.locator(".ui-stpc__btn-primary").click(); await page.locator("[data-pair-code]").getByText("stv1-fixture-code").waitFor()
  } },
  { name: "pairing-connect", scene: "pairing", hash: "#/devices", ready: '[data-attention-confirm="n-new"]', then: async (page) => {
    await page.locator('[data-attention-confirm="n-new"]').click(); await page.locator('[data-add-confirm="n-new"]').waitFor()
    await page.locator('[data-fp-group="0"] input').evaluate((el) => { const dt = new DataTransfer(); dt.setData("text", "9e3f 5a07 c21b 84d6"); el.dispatchEvent(new ClipboardEvent("paste", { clipboardData: dt, bubbles: true, cancelable: true })) })
    await page.locator('[data-fp-group="3"][data-fp-state="match"]').waitFor()
  } },
  { name: "pairing-client", scene: "normal", hash: "#/devices", ready: '[data-map-card="hub"]', then: async (page) => {
    await page.locator("[data-add-device-open]").click(); await page.locator("[data-add-kind=client]").click(); await page.locator(".ui-stpc__btn-primary").click()
    await page.locator('[data-add-device][data-add-step="scan"] svg').first().waitFor()
  } },
].filter((s) => !ONLY || s.name.includes(ONLY))

const json = (route, body, code = 200) => route.fulfill({ status: code, contentType: "application/json", body: JSON.stringify(body) })
/** the owner's Hub for one screen: what it answers, and every path it was asked that it does not know */
async function hub(page, scene, unknown) {
  const nodes = scenes[scene]()
  await page.route("**/*", async (route) => {
    const request = route.request(), url = new URL(request.url()), path = url.pathname, get = request.method() === "GET"
    if (url.origin !== base) return route.abort("blockedbyclient")
    if (path === "/auth/hello") return json(route, { name: "steve", api: 2, version: HUB, capabilities: { hub_metadata: 1 } })
    if (!path.startsWith("/api/")) return route.continue()
    if (path === "/api/nodes") return json(route, { nodes })
    if (path === "/api/vitals") return json(route, { nodes: vitals(nodes) })
    if (path === "/api/nodes/upgrade-default") return json(route, { policy: "notify", hub: { version: HUB, commit: "abc", target: "x" }, drain_secs: 1800, health_secs: 180 })
    if (path === "/api/nodes/pairings" && !get) return json(route, { code: "stv1-fixture-code", expires: NOW + 10 * MIN, pin: null, hub_id: "hub_example" })
    if (path === "/api/nodes/tailcat") return json(route, { available: true, enabled: false })
    if (path === "/api/node-tags") return json(route, { tags: { local: ["gpu"], "n-mbp": ["mac", "xcode"] } })
    if (path === "/api/node-context") return json(route, { owner_context: "on" })
    if (path === "/api/access") return json(route, { tokens, clients, current: "c1", route: "lan", hub: { version: HUB }, me: "u1", all: true, users: { u1: "Owner" } })
    if (path === "/api/access/oidc") return json(route, { source: "none", issuer: null, client_id: null, has_secret: false, callback: "https://hub.example.net/auth/callback", enabled: false })
    if (path === "/api/access/pairings" && !get) return json(route, { id: "cp1", code: "fixture-client-code", name: "New client", expires: NOW + 10 * MIN, public_url: "https://hub.example.net", lan_urls: ["http://192.168.1.10:8080"], loopback_only: false, links: [{ url: "https://hub.example.net", kind: "public", link: "https://hub.example.net/#/pair/fixture-client-code" }] })
    if (path.startsWith("/api/access/pairings/")) return json(route, { id: "cp1", status: "pending", expires: NOW + 10 * MIN, client: null })
    if (path === "/api/hub-metadata") return json(route, metadata)
    if (path === "/api/devices") return json(route, { devices: [{ id: "hub", name: "Studio Mac", roles: ["Hub"], node_id: "local", os: "macOS 26.1", version: HUB, status: "正常", max_sessions: 32, seen: NOW, created: 0 }], runtimes })
    /* the owner chip asks for the people module, which this Hub (like any without it) does not have */
    if (path === "/api/me/contact") return json(route, {}, 404)
    /* the page header's account button asks who is signed in */
    if (path === "/api/me") return json(route, { user: { id: "u1", name: "Owner", email: null, role: "owner" }, version: HUB, web_version: HUB })
    unknown.add(request.method() + " " + path)
    return json(route, { error: "fixture" }, 404)
  })
}

/** what a drawn screen must hold: nothing past the window or cut at the side of the page's scroller, no scroller that
 *  scrolls sideways, no text key in place of its words. A box that clips on purpose (the map, a code line, a cut-off
 *  name) keeps what is inside it; a scroller (the page's own) must not hide anything sideways. */
const inspect = (page) => page.evaluate(() => {
  const w = innerWidth, out = { page: document.documentElement.scrollWidth - w, cut: [], wide: [], keys: [] }
  const scrolls = (el) => { const s = getComputedStyle(el); return s.overflowY === "auto" || s.overflowY === "scroll" }
  const name = (el) => `${el.tagName.toLowerCase()}${typeof el.className === "string" && el.className.trim() ? "." + el.className.trim().split(/\s+/).slice(0, 2).join(".") : ""}`
  /* the nearest box that clips sideways: null (the window), a scroller, or a box that clips on purpose */
  const clipOf = (el) => { for (let p = el.parentElement; p && p !== document.body; p = p.parentElement) if (getComputedStyle(p).overflowX !== "visible") return p; return null }
  for (const el of document.querySelectorAll("#root *, [role=dialog] *")) {
    const r = el.getBoundingClientRect()
    if (!r.width || !r.height || getComputedStyle(el).visibility === "hidden") continue
    if (scrolls(el) && el.scrollWidth > el.clientWidth + 1 && getComputedStyle(el).overflowX !== "hidden") out.wide.push(`${name(el)} ${el.scrollWidth - el.clientWidth} px`)
    const clip = clipOf(el)
    if (clip && !scrolls(clip)) continue
    const box = clip ? clip.getBoundingClientRect() : { left: 0, right: w }
    if (r.right > Math.min(box.right, w) + 1 || r.left < Math.max(box.left, 0) - 1) out.cut.push(`${name(el)} ${Math.round(r.left)}..${Math.round(r.right)}`)
  }
  const text = document.body.innerText
  out.keys = [...new Set(text.match(/\b(?:devices|device|shell|hub|common|core|pages|manage):[a-zA-Z][\w.]*\b/g) ?? [])]
  out.dark = document.documentElement.classList.contains("dark")
  return out
})

/** the share of pixels that differ between two PNGs (by more than a faint anti-aliasing step), and an image marking them */
async function compare(page, a, b) {
  return page.evaluate(async ([a, b]) => {
    const load = (src) => new Promise((ok, bad) => { const i = new Image(); i.onload = () => ok(i); i.onerror = bad; i.src = src })
    const [x, y] = await Promise.all([load(a), load(b)])
    if (x.width !== y.width || x.height !== y.height) return { size: `${y.width}×${y.height} → ${x.width}×${x.height}`, share: 1, diff: null }
    const c = document.createElement("canvas"); c.width = x.width; c.height = x.height
    const g = c.getContext("2d", { willReadFrequently: true })
    g.drawImage(x, 0, 0); const p = g.getImageData(0, 0, c.width, c.height)
    g.clearRect(0, 0, c.width, c.height); g.drawImage(y, 0, 0); const q = g.getImageData(0, 0, c.width, c.height)
    let n = 0
    for (let i = 0; i < p.data.length; i += 4) {
      const d = Math.max(Math.abs(p.data[i] - q.data[i]), Math.abs(p.data[i + 1] - q.data[i + 1]), Math.abs(p.data[i + 2] - q.data[i + 2]))
      if (d > 40) { n++; p.data[i] = 255; p.data[i + 1] = 0; p.data[i + 2] = 64; p.data[i + 3] = 255 } else p.data[i + 3] = 60
    }
    g.putImageData(p, 0, 0)
    return { n, share: n / (c.width * c.height), diff: n ? c.toDataURL("image/png") : null }
  }, [a, b])
}
const dataUrl = async (file) => "data:image/png;base64," + (await readFile(file)).toString("base64")

const problems = [], moved = []
let drawn = 0
try {
  for (const [width, height] of [[1440, 1000], [390, 844]]) for (const theme of ["light", "dark"]) {
    const ctx = await browser.newContext({ locale: "zh-CN", timezoneId: "UTC", colorScheme: theme, reducedMotion: "reduce", viewport: { width, height }, deviceScaleFactor: 1, hasTouch: width < 600 })
    await ctx.clock.setFixedTime(NOW)
    await ctx.addInitScript(() => { localStorage.setItem("steve-lang", "zh-CN"); localStorage.setItem("steve-onboarded", "1") })
    for (const s of screens) {
      const tag = `${s.name}-${width}-${theme}`, page = await ctx.newPage(), errors = [], unknown = new Set()
      page.on("pageerror", (e) => errors.push(e.message))
      await hub(page, s.scene, unknown)
      await page.goto(base + "/test/fixtures/devices.html" + s.hash)
      try { await page.locator(s.ready).first().waitFor({ timeout: 15_000 }) } catch (e) { if (SHOTS) await page.screenshot({ path: `${SHOTS}/${tag}.failed.png`, fullPage: true }); throw new Error(`${tag}: ${s.ready} never showed; unanswered: ${[...unknown].join(", ") || "none"}; errors: ${errors.join("; ") || "none"}`) }
      if (s.then) await s.then(page)
      if (s.check) await s.check(page)
      /* drawn and still: fonts in, finite animations over, the map's edges laid out on the cards' final boxes */
      await page.evaluate(() => document.fonts.ready)
      await page.waitForFunction(() => document.getAnimations().every((a) => a.playState !== "running" || a.effect?.getTiming().iterations === Infinity))
      await page.evaluate(() => new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r))))
      await page.waitForTimeout(150)
      const seen = await inspect(page)
      if (seen.page > 0) problems.push(`${tag}: the page is ${seen.page} px wider than the window`)
      if (seen.cut.length) problems.push(`${tag}: cut off at the window's edge: ${seen.cut.slice(0, 4).join("; ")}${seen.cut.length > 4 ? ` and ${seen.cut.length - 4} more` : ""}`)
      if (seen.wide.length) problems.push(`${tag}: scrolls sideways: ${seen.wide.slice(0, 4).join("; ")}`)
      if (seen.keys.length) problems.push(`${tag}: text keys shown instead of words: ${seen.keys.join(", ")}`)
      if (seen.dark !== (theme === "dark")) problems.push(`${tag}: the ${theme} theme is not on`)
      if (errors.length) problems.push(`${tag}: page errors: ${errors.join("; ")}`)
      if (unknown.size) problems.push(`${tag}: asked the Hub for what this fixture does not answer: ${[...unknown].join(", ")}`)
      if (SHOTS) {
        const file = `${SHOTS}/${tag}.png`
        /* a page is drawn whole; a dialog's screen as the window shows it */
        await page.screenshot({ path: file, fullPage: !s.then, animations: "disabled", caret: "hide" })
        if (BASELINE) {
          const was = `${BASELINE}/${tag}.png`
          if (!existsSync(was)) moved.push(`${tag}: no earlier screenshot to compare with`)
          else {
            const d = await compare(page, await dataUrl(file), await dataUrl(was))
            if (d.share > TOLERANCE) {
              moved.push(`${tag}: ${d.size ?? `${d.n} pixels moved (${(d.share * 100).toFixed(3)}%)`}`)
              if (d.diff) await writeFile(`${SHOTS}/${tag}.diff.png`, Buffer.from(d.diff.split(",")[1], "base64"))
            }
          }
        }
      }
      drawn++
      await page.close()
    }
    await ctx.close()
  }
  assert.deepEqual(problems, [], "every devices screen drawn whole at 1440 and 390 px in both themes")
  assert.deepEqual(moved, [], `screens that differ from ${BASELINE}`)
  assert.ok(drawn > 0, "no screen matched STEVE_TEST_ONLY")
  process.stdout.write(`Devices look: ${drawn} screens (${screens.length} × 1440/390 × light/dark) drawn whole${SHOTS ? `, written to ${SHOTS}` : ""}${BASELINE ? `, matching ${BASELINE}` : ""}\n`)
} finally { await browser.close() }
