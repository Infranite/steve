// The devices overview, its connection map and the Add device dialog, on the real pages with an isolated owner API.
// Run against Vite, with external Playwright (PLAYWRIGHT_MODULE, PLAYWRIGHT_EXECUTABLE, STEVE_TEST_WEB).
import assert from "node:assert/strict"
import { mkdir } from "node:fs/promises"
const { chromium } = await import(process.env.PLAYWRIGHT_MODULE || "playwright")
const browser = await chromium.launch({ headless: true, ...(process.env.PLAYWRIGHT_EXECUTABLE ? { executablePath: process.env.PLAYWRIGHT_EXECUTABLE } : {}) })
const base = process.env.STEVE_TEST_WEB || "http://127.0.0.1:5198"
const NOW = Date.now(), GIB = 2 ** 30, FP = "9e3f5a07c21b84d60f9a77e3b10c4d52"
const up = (v, extra = {}) => ({ build: { version: v, commit: "abc", target: "x" }, policy: "notify", own_policy: null, can: true, reason: v === "0.2.112" ? "current" : "outdated", latest: null, ...extra })
const node = (id, name, extra = {}) => ({ node_id: id, name, fingerprint: FP, state: "active", linked: true, local: false, max_sessions: 6, running: 0, waiting: 0, last_seen: NOW - 30_000, upgrade: up("0.2.112"), ...extra })
const link = (route, via = "direct", tls = false) => ({ route, via, tls, since: NOW, latency_ms: 40, latency_at: NOW })
const local = { ...node("fixture-local", "Studio Mac", { local: true, max_sessions: 32, running: 3 }), upgrade: undefined }
const three = () => [local,
  node("n-build", "build-linux-01", { running: 1, max_sessions: 4, upgrade: up("0.2.109"), link: link("public", "direct", true) }),
  node("n-mbp", "Studio MacBook Pro", { running: 2, link: link("loopback", "tailcat") }),
  node("n-imac", "studio-imac", { linked: false, last_seen: NOW - 3 * 3600_000, link_error: { at: NOW - 3 * 3600_000, error: "connection refused: tcp connect" } }),
]
const client = (id, name, kind, extra = {}) => ({ id, kind, name, platform: "macOS", version: "0.2.112", origin: null, created: NOW - 90 * 86400_000, last_seen: NOW - 60_000, last_route: "lan", last_ip: null, revoked: null, online: false, streams: 0, connected_since: null, ...extra })
const clients = [client("c1", "This browser", "web", { online: true, streams: 1 }), client("c2", "Phone", "ios", { online: true, streams: 1 }), client("c3", "Old laptop", "mac", { last_seen: NOW - 20 * 86400_000 }),
  client("c4", "ci-bot", "cli", { last_seen: NOW - 40 * 86400_000 }), client("c5", "Tablet", "ios", { last_seen: NOW - 2 * 86400_000 }), client("c6", "Signed out", "web", { revoked: NOW })]

let nodes = three(), late = [], paired = 0
const writes = [], errors = []
const page = await browser.newPage({ locale: "zh-CN", viewport: { width: 1440, height: 1000 } })
page.on("pageerror", (e) => errors.push(e.message))
const json = (route, body, code = 200) => route.fulfill({ status: code, contentType: "application/json", body: JSON.stringify(body) })
await page.route("**/*", async (route) => {
  const request = route.request(), url = new URL(request.url())
  if (url.origin !== base) return route.abort("blockedbyclient")
  if (!url.pathname.startsWith("/api/")) return route.continue()
  if (request.method() !== "GET") writes.push(request.method() + " " + url.pathname)
  if (url.pathname === "/api/nodes/pairings") { paired++; nodes = [...nodes, ...late]; late = []; return json(route, { code: "stv1-fixture-code", expires: Date.now() + 600_000, pin: null, hub_id: "hub" }) }
  if (/^\/api\/nodes\/[^/]+\/(confirm|revoke|upgrade)$/.test(url.pathname)) {
    const [, , , id, act] = url.pathname.split("/")
    nodes = nodes.map((n) => (n.node_id === id ? { ...n, state: act === "confirm" ? "active" : act === "revoke" ? "revoked" : n.state } : n))
    return json(route, act === "upgrade" ? { operation: "nup" } : { ok: true }, act === "upgrade" ? 202 : 200)
  }
  if (url.pathname === "/api/nodes") return json(route, { nodes })
  if (url.pathname === "/api/vitals") return json(route, { nodes: { local: { latest: { at: NOW, cpus: 14, load: [5.9, 5, 4], mem_total: 64 * GIB, mem_available: 38 * GIB }, series: [] } } })
  if (url.pathname === "/api/nodes/upgrade-default") return json(route, { policy: "notify", hub: { version: "0.2.112", commit: "abc", target: "x" }, drain_secs: 1800, health_secs: 180 })
  if (url.pathname === "/api/node-context") return json(route, { owner_context: "on" })
  if (url.pathname === "/api/access") return json(route, { tokens: [], clients, current: "c1", route: "lan", hub: { version: "0.2.112" } })
  if (url.pathname === "/api/nodes/tailcat") return json(route, { available: true, enabled: false })
  return json(route, {}, 404)
})
const map = page.locator("[data-device-map]")
const reread = () => page.evaluate(() => document.dispatchEvent(new Event("visibilitychange")))
/* every card lies inside the map and the columns fit in it: the map clips what runs past it, so the page itself never
 * overflows and noOverflow alone would not see a cut-off column */
const cardsInside = async (what) => {
  const out = await page.evaluate(() => {
    const map = document.querySelector(".dmap"), cols = map.querySelector(".dmap-cols"), m = map.getBoundingClientRect()
    const cut = [...map.querySelectorAll("[data-map-card]")].filter((c) => { const r = c.getBoundingClientRect(); return r.left < m.left - 0.5 || r.right > m.right + 0.5 }).map((c) => c.dataset.mapCard)
    return { cut, cols: cols.scrollWidth - cols.clientWidth, width: Math.round(m.width), narrow: map.hasAttribute("data-narrow") }
  })
  assert.deepEqual(out.cut, [], `${what}: every card inside the map (map ${out.width} px, narrow ${out.narrow})`)
  assert.ok(out.cols <= 0, `${what}: the columns fit in the map (map ${out.width} px, ${out.cols} px over)`)
  return out
}
const noOverflow = async (what) => assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), what + ": no horizontal overflow")
const shot = async (name) => { if (process.env.STEVE_TEST_SHOTS) { await mkdir(process.env.STEVE_TEST_SHOTS, { recursive: true }); await page.screenshot({ path: `${process.env.STEVE_TEST_SHOTS}/${name}.png`, fullPage: true, animations: "disabled" }) } }
try {
  await page.goto(base + "/test/fixtures/devices.html#/devices")
  await map.locator('[data-map-card="hub"]').waitFor()
  await map.locator('[data-map-node="n-build"]').waitFor()

  // what wants you: the failed link first words, an outdated Node with its upgrade; the four numbers
  await page.locator('[data-attention-row="n-imac"]').getByText(/studio-imac · 离线 · 3 小时前/).waitFor()
  await page.locator('[data-attention-row="n-imac"]').getByText("（connection refused）", { exact: true }).waitFor()
  await page.locator('[data-attention-upgrade="n-build"]').click()
  for (let i = 0; i < 60 && !writes.includes("POST /api/nodes/n-build/upgrade"); i++) await page.waitForTimeout(50)
  assert.ok(writes.includes("POST /api/nodes/n-build/upgrade"), "Upgrade on the line starts that Node's upgrade")
  assert.match(await page.locator('[data-stat="clients"]').innerText(), /在线客户端\s*2\s*5 个已登录/, "online: the Hub's word and this client; one per client, signed-out ones left out")
  assert.match(await page.locator('[data-stat="sessions"]').innerText(), /6\s*\/ 42/, "an offline Node's capacity is not counted")

  // the map: the Hub and its own Node in one frame, remote Nodes by what needs you, edges in their status, routes at their ends
  assert.equal(await map.locator("[data-map-machine] [data-map-card]").count(), 2)
  assert.match(await map.locator("[data-map-machine]").innerText(), /Studio Mac · 14 核 · 64 GB/)
  assert.deepEqual(await map.locator("[data-map-node]").evaluateAll((xs) => xs.map((x) => x.dataset.mapNode)), ["n-build", "n-mbp", "n-imac"])
  await page.waitForFunction(() => document.querySelectorAll("[data-device-map] path[data-edge]").length === 8)
  const edge = (id) => map.locator(`path[data-edge="${id}"]`)
  assert.equal(await edge("n-build").getAttribute("data-edge-kind"), "warn")
  assert.equal(await edge("n-imac").getAttribute("data-edge-kind"), "off")
  assert.match(await edge("n-mbp").getAttribute("class"), /is-flow/, "sessions running: the edge moves")
  assert.equal(await edge("local").getAttribute("data-edge-kind"), "ok")
  assert.equal(await edge("c:c1").getAttribute("data-edge-kind"), "access")
  assert.match(await edge("c:c1").getAttribute("class"), /is-me/)
  assert.match(await edge("c:c3").getAttribute("class"), /is-stale/, "a client not used for a week is greyed")
  assert.equal(await map.locator('[data-edge-label="n-build"]').innerText(), "HTTPS")
  assert.equal(await map.locator('[data-edge-label="n-mbp"]').innerText(), "tailcat")
  assert.equal(await map.locator('[data-edge-label="n-imac"]').innerText(), "—", "no link reported: a placeholder")
  assert.equal(await map.locator("[data-map-client]").count(), 4)
  await map.locator("[data-map-more-clients]").getByText("还有 1 个").waitFor()
  assert.deepEqual(await map.locator("[data-map-client]").evaluateAll((xs) => xs.map((x) => x.dataset.mapClient)), ["c1", "c2", "c5", "c3"], "this one, online, then the latest")

  // keyboard: Tab reaches the cards, arrows move within a column, focus lights its own edge only and shows its facts
  await map.locator('[data-map-card="n-build"]').focus()
  await page.keyboard.press("ArrowDown")
  assert.equal(await page.evaluate(() => document.activeElement?.getAttribute("data-map-card")), "n-mbp")
  await page.waitForFunction(() => document.querySelector('[data-device-map] path[data-edge="n-build"]')?.classList.contains("is-dim"))
  assert.doesNotMatch(await edge("n-mbp").getAttribute("class"), /is-dim/)
  await page.keyboard.press("Tab"); await page.keyboard.press("Shift+Tab")
  await page.locator("[data-map-hover]").getByText("Studio MacBook Pro").waitFor()
  assert.match(await page.locator("[data-map-hover]").innerText(), /tailcat · 40 ms/)
  await page.keyboard.press("Enter")
  await page.waitForFunction(() => location.hash === "#/devices/nodes/n-mbp")
  await page.goBack()
  await map.locator('[data-map-card="c:c2"]').waitFor()
  await map.locator('[data-map-card="c:c2"]').click()
  await page.waitForFunction(() => location.hash === "#/devices/clients/c2")
  await page.locator('[data-client="c2"][data-client-focus]').waitFor()
  // the clients page says who is online now, as the map does, and the head counts them
  await page.locator('[data-client="c2"] [data-client-online]').waitFor()
  assert.equal(await page.locator("[data-client-online]").count(), 1, "only the client the Hub says is online")
  assert.match(await page.locator('[data-client="c2"]').innerText(), /现在/)
  await page.locator("[data-clients-note]").getByText(/「在线」= 此刻和 Hub 保持着实时连接/).waitFor()
  assert.match(await page.locator("[data-devices-sub]").innerText(), /2 个客户端在线/)
  await page.goto(base + "/test/fixtures/devices.html#/devices")

  // "Show clients" off: two columns, remembered on this client
  await map.locator("[data-map-show-clients]").click()
  await page.waitForFunction(() => !document.querySelector("[data-map-clients]"))
  assert.equal(await page.evaluate(() => localStorage.getItem("steve.devices.mapClients")), "0")
  await page.reload(); await map.locator('[data-map-card="hub"]').waitFor()
  assert.equal(await map.locator("[data-map-clients]").count(), 0)
  await map.locator("[data-map-show-clients]").click()
  await map.locator("[data-map-clients]").waitFor()
  await shot("map-1440"); await noOverflow("1440"); await cardsInside("1440")

  // between a phone and a wide window: the wide map where its columns fit, the tree where they do not, nothing cut off
  const layouts = new Set()
  for (const width of [1280, 1160, 1024, 980, 920, 900, 860, 820, 780, 720, 640, 560]) {
    await page.setViewportSize({ width, height: 1000 })
    await page.evaluate(() => new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r))))
    layouts.add((await cardsInside("window " + width)).narrow)
  }
  assert.deepEqual([...layouts].sort(), [false, true], "both layouts seen between 560 and 1280 px")
  await page.setViewportSize({ width: 780, height: 1000 })
  await page.waitForFunction(() => document.querySelector(".dmap")?.hasAttribute("data-narrow"))
  await map.locator("[data-map-show-clients]").click()
  await page.waitForFunction(() => !document.querySelector(".dmap")?.hasAttribute("data-narrow"))
  await cardsInside("780 without clients")
  await map.locator("[data-map-show-clients]").click()
  await map.locator("[data-map-clients]").waitFor()
  await page.setViewportSize({ width: 1440, height: 1000 })

  // a phone: a tree, no hover card
  await page.setViewportSize({ width: 390, height: 844 })
  await page.waitForFunction(() => document.querySelector(".dmap")?.hasAttribute("data-narrow"))
  await map.locator("path.is-trunk").waitFor({ state: "attached" })
  await noOverflow("390"); await shot("map-390")
  await page.setViewportSize({ width: 1440, height: 1000 })

  // every count: seven and more one line each, more than twelve folded; none at all a placeholder to add one
  nodes = [local, ...Array.from({ length: 13 }, (_, i) => node("m" + i, "runner-" + String(i).padStart(2, "0"), { link: link("lan") }))]
  await page.reload(); await map.locator('[data-map-node="m0"]').waitFor()
  assert.equal(await page.locator(".dmap[data-map-compact]").count(), 1)
  assert.equal(await map.locator("[data-map-node]").count(), 12)
  await map.locator("[data-map-more-nodes]").getByText("还有 1 台").waitFor()
  await noOverflow("13 Nodes")
  nodes = [local]
  await page.reload(); await map.locator('[data-map-card="add"]').waitFor()
  assert.equal(await map.locator("[data-map-node]").count(), 1)
  await page.waitForFunction(() => document.querySelector('[data-device-map] path[data-edge="add"]'))
  await map.locator('[data-map-card="add"]').click()
  await page.locator('[data-add-device][data-add-step="via"]').waitFor()
  await page.keyboard.press("Escape")
  await page.locator("[data-add-device]").waitFor({ state: "detached" })

  // Add device: a Node over the same network, the command and a code, it arrives, its whole fingerprint is checked
  late = [node("n-new", "mac-studio-lab", { state: "pending", link: { ...link("lan"), peer: "192.168.1.42" }, host: { os: "macos", os_version: "26.1", arch: "aarch64", cpus: 24, mem_total: 96 * GIB } })]
  await page.locator("[data-add-device-open]").click()
  await page.locator("[data-add-kind=node]").click()
  await page.locator(".ui-stpc__btn-primary").click()
  await page.locator('[data-add-via="direct"][aria-checked="true"]').waitFor()
  await page.locator(".ui-stpc__btn-primary").click()
  await page.locator("[data-pair-code]").getByText("stv1-fixture-code").waitFor()
  assert.match(await page.locator("[data-setup-command]").innerText(), /steve setup node --hub http:\/\/127\.0\.0\.1:\d+/)
  await page.locator('[data-add-confirm="n-new"]').waitFor()
  assert.match(await page.locator("[data-add-arrived]").innerText(), /mac-studio-lab 已连上（局域网 · 192\.168\.1\.42 · macOS 26\.1 · arm64 · 24 核 · 96 GB）/)
  assert.equal(await page.locator(".ui-stpc__btn-primary").getAttribute("aria-disabled"), "true")
  const paste = (text, group = 0) => page.locator(`[data-fp-group="${group}"] input`).evaluate((el, t) => { const dt = new DataTransfer(); dt.setData("text", t); el.dispatchEvent(new ClipboardEvent("paste", { clipboardData: dt, bubbles: true, cancelable: true })) }, text)
  await paste("9e3f 5a07")
  await page.locator('[data-fp-group="1"][data-fp-state="match"]').waitFor()
  assert.equal(await page.locator('[data-fp-group="2"]').getAttribute("data-fp-state"), "open", "two groups do not confirm a Node")
  await page.locator('[data-fp-group="2"] input').fill("C21C")
  await page.locator('[data-fp-summary="wrong"]').getByText(/有 1 组对不上/).waitFor()
  await paste("fingerprint 9E3F 5A07 C21B 84D6 0F9A 77E3 B10C 4D52", 4)
  await page.locator('[data-fp-summary="match"]').waitFor()
  assert.equal(await page.locator('[data-fp-state="match"]').count(), 8, "a whole pasted fingerprint fills every group")
  await page.locator(".ui-stpc__btn-primary").click()
  await page.locator("[data-add-open-node]").waitFor()
  assert.ok(writes.includes("POST /api/nodes/n-new/confirm"))
  await page.locator("[data-add-done]").click()
  await page.locator("[data-add-device]").waitFor({ state: "detached" })

  // a Node to confirm from the overview's line; rejecting it unpairs it
  nodes = [...three(), node("n-odd", "someone-else", { state: "pending" })]
  await reread()
  await page.locator('[data-attention-confirm="n-odd"]').click()
  await page.locator('[data-add-confirm="n-odd"]').waitFor()
  await page.locator("[data-add-reject]").click()
  for (let i = 0; i < 60 && !writes.includes("POST /api/nodes/n-odd/revoke"); i++) await page.waitForTimeout(50)
  assert.ok(writes.includes("POST /api/nodes/n-odd/revoke"))
  await page.locator("[data-add-device]").waitFor({ state: "detached" })

  // a client: the QR code
  await page.locator("[data-add-device-open]").click()
  await page.locator("[data-add-kind=client]").click()
  await page.locator(".ui-stpc__btn-primary").click()
  await page.locator('[data-add-device][data-add-step="scan"]').waitFor()
  assert.deepEqual(errors, [])
  process.stdout.write("Devices map: attention, numbers, map edges and labels, keyboard, hover facts, show clients, phone tree, every count, Add device with the whole fingerprint, reject, client passed\n")
} finally { await browser.close() }
