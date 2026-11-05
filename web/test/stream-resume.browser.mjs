// Vite fixture + a Hub played here; external Playwright prerequisite.
// C1-S4: after the connection drops (offline, a phone in the background) the page resumes the stream from its cursor and
// refetches only what changed meanwhile; after a Hub restart it resyncs everything exactly once.
import assert from "node:assert/strict"
import { createServer } from "node:http"
const { chromium } = await import(process.env.PLAYWRIGHT_MODULE || "playwright")
const browser = await chromium.launch({ headless: true, ...(process.env.PLAYWRIGHT_EXECUTABLE ? { executablePath: process.env.PLAYWRIGHT_EXECUTABLE } : {}) })
const base = process.env.STEVE_TEST_WEB || "http://127.0.0.1:5189"

/* ───── the Hub: a change log with an epoch, the resumable stream (real SSE, so it stays open), the REST reads ───── */
const hub = { epoch: "E1", head: 5, log: [], events: [1], clients: new Set(), streams: [] }
const frame = (o) => `data: ${JSON.stringify(o)}\n\n`
const publish = (o) => { const n = ++hub.head; hub.log.push({ n, o }); for (const c of hub.clients) c.write(`id: ${hub.epoch}:${n}\n` + frame(o)) }
const drop = () => { for (const c of hub.clients) c.destroy(); hub.clients.clear() }
const sse = createServer((req, res) => {
  const after = new URL(req.url, "http://x").searchParams.get("after")
  hub.streams.push(after)
  res.writeHead(200, { "content-type": "text/event-stream", "cache-control": "no-cache" })
  const [ep, n] = (after ?? "").split(":")
  if (after && (ep !== hub.epoch || Number(n) > hub.head)) res.write(`id: ${hub.epoch}:${hub.head}\n` + frame({ type: "stream.reset" }))
  else if (after) { for (const f of hub.log) if (f.n > Number(n)) res.write(`id: ${hub.epoch}:${f.n}\n` + frame(f.o)); res.write(frame({ type: "stream.gap" })) }
  hub.clients.add(res); req.on("close", () => hub.clients.delete(res))
})
await new Promise((r) => sse.listen(0, "127.0.0.1", r))
const ssePort = sse.address().port
const session = { id: "s1", workspace_id: "ws", item_id: null, no: 1, bot_id: null, harness: "codex", model: null, cwd: "/tmp", status: "running", used: 0, size: 0, cost: null, title: "s1", device_id: "hub", source: "item", readonly: false, busy: false, queued: [], created: 1, updated: 2 }
const state = { workspace: { id: "ws", name: "ws", item_prefix: "ST" }, workspaces: [], items: [], bots: [], allBots: [], projects: [], sessions: [session], decisions: [], recent: [], thread: [], tags: [], tagUses: {}, settings: {}, devices: [], runtimes: [] }
const seen = []
const until = async (f, what, ms = 8000) => { const t0 = Date.now(); while (!(await f())) { if (Date.now() - t0 > ms) throw new Error("timed out: " + what + " " + JSON.stringify({ seen, streams: hub.streams })); await new Promise((r) => setTimeout(r, 50)) } }

const errors = []
try {
  const context = await browser.newContext({ locale: "zh-CN" })
  const page = await context.newPage()
  page.on("pageerror", (e) => errors.push(e.message))
  await page.route("**/*", (route) => {
    const u = new URL(route.request().url())
    if (u.origin !== base) return route.abort("blockedbyclient")
    if (!u.pathname.startsWith("/api/")) return route.continue()
    if (u.pathname === "/api/stream") return route.continue({ url: `http://127.0.0.1:${ssePort}${u.pathname}${u.search}` })
    seen.push(u.pathname + (u.pathname.endsWith("/events") ? "?after=" + u.searchParams.get("after") : ""))
    const json = (body, headers = {}) => route.fulfill({ status: 200, contentType: "application/json", headers, body: JSON.stringify(body) })
    if (u.pathname === "/api/state") return json(state, { "x-steve-cursor": `${hub.epoch}:${hub.head}` })
    if (u.pathname.startsWith("/api/items/")) return json({})
    if (u.pathname === "/api/sessions/s1/events") { const after = Number(u.searchParams.get("after")); return json(hub.events.filter((s) => s > after).map((seq) => ({ seq, ts: seq, kind: "text", data: { role: "assistant", text: "ABCD"[seq - 1] } }))) }
    return route.fulfill({ status: 404, contentType: "text/plain", body: "" }) // modules this fixture does not serve
  })
  await page.goto(base + "/test/fixtures/stream-resume.html")
  await page.waitForFunction(() => typeof window.__load === "function") // Vite may reload the page once while it bundles deps
  await until(() => hub.clients.size === 1, "first stream")
  assert.ok(hub.streams.length && hub.streams.every((x) => x === "E1:5"), "the first stream continues from the state's watermark")
  hub.streams = ["E1:5"]
  await page.evaluate(() => window.__load())
  await until(async () => (await page.evaluate(() => window.__transcript())).join("") === "A", "transcript loaded")
  assert.equal(seen.filter((x) => x === "/api/state").length, 1, "the first load fetches the state once")
  assert.equal(await page.evaluate(() => window.__resyncs()), 0)

  /* offline: the stream drops; meanwhile ST-1 changes and the session writes two chunks */
  seen.length = 0
  await context.setOffline(true); drop()
  await new Promise((r) => setTimeout(r, 300))
  publish({ type: "item", id: "ST-1" }); hub.events.push(2, 3)
  await context.setOffline(false)
  await until(() => hub.streams.length === 2, "reconnect on online", 1500) // before the 2 s backoff
  assert.equal(hub.streams[1], "E1:5")
  await until(async () => seen.includes("/api/items/ST-1") && (await page.evaluate(() => window.__transcript())).join("") === "ABC", "resumed refetches")
  await new Promise((r) => setTimeout(r, 1500)) // let anything else that would refetch show up
  assert.deepEqual(seen.filter((x) => x.startsWith("/api/items/")), ["/api/items/ST-1"], "only the touched item is refetched")
  assert.equal(seen.filter((x) => x === "/api/state").length, 1)
  assert.deepEqual(seen.filter((x) => x.includes("/events")).slice(0, 1), ["/api/sessions/s1/events?after=1"], "the transcript continues from before the gap")
  assert.deepEqual(await page.evaluate(() => window.__transcript()), ["ABC"], "missed chunks land once")
  assert.equal(await page.evaluate(() => window.__resyncs()), 0, "module pages are not told to refetch")

  /* the Hub restarts: its log is gone, the cursor is from another epoch → one reset, one full resync */
  seen.length = 0
  hub.epoch = "E2"; hub.log = []; hub.head = 2; hub.events.push(4)
  drop()
  await until(() => hub.streams.length === 3, "reconnect after restart")
  assert.equal(hub.streams[2], "E1:6")
  await until(async () => (await page.evaluate(() => window.__resyncs())) === 1, "one resync")
  await until(() => seen.filter((x) => x.startsWith("/api/items/")).length === 2, "every loaded item refetched")
  await new Promise((r) => setTimeout(r, 1000))
  assert.equal(seen.filter((x) => x === "/api/state").length, 1)
  assert.deepEqual(seen.filter((x) => x.startsWith("/api/items/")).sort(), ["/api/items/ST-1", "/api/items/ST-2"])
  assert.deepEqual(await page.evaluate(() => window.__transcript()), ["ABCD"])

  /* the next drop continues from the new head: no second reset */
  seen.length = 0; drop()
  await until(() => hub.streams.length === 4, "reconnect after the reset")
  assert.equal(hub.streams[3], "E2:2")
  await new Promise((r) => setTimeout(r, 800))
  assert.equal(await page.evaluate(() => window.__resyncs()), 1)
  assert.deepEqual(seen.filter((x) => x.startsWith("/api/items/") || x === "/api/state"), [])
  assert.deepEqual(errors, [])
  console.log(JSON.stringify({ passed: true, scenarios: ["first stream from the state watermark", "offline → online resumes at once, only the touched item and the gap's transcript refetch", "Hub restart → one reset, one resync", "no second reset"] }))
} finally { await browser.close(); sse.close() }
