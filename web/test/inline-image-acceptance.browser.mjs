/* End-to-end with an actual isolated Hub, its local Node, the offline research ACP adapter and
 * the built App/ChatView. No seeded messages, production installation, provider or OAuth.
 * Run after build:web with STEVE_TEST_HUB_BIN and Playwright (PLAYWRIGHT_MODULE if external).
 * The checked-in QR encodes only "steve-image-fixture:offline-no-oauth" (320 x 320 pixels).
 * STEVE_TEST_SHOTS optionally selects the evidence directory. Only our own Hub PID is stopped. */
import assert from "node:assert/strict"
import { createHash } from "node:crypto"
import { spawn, execFileSync } from "node:child_process"
import { once } from "node:events"
import { createWriteStream } from "node:fs"
import { access, mkdir, mkdtemp, readFile, writeFile, unlink, rmdir } from "node:fs/promises"
import { createServer } from "node:net"
import { resolve, dirname, basename, sep } from "node:path"
import { fileURLToPath } from "node:url"
import { deflateSync } from "node:zlib"

const repo = resolve(fileURLToPath(new URL("../..", import.meta.url)))
const binary = resolve(process.env.STEVE_TEST_HUB_BIN || "")
assert.ok(process.env.STEVE_TEST_HUB_BIN, "Provide a newly built Hub; never attach to an existing installation")
await access(binary); await access(repo + "/web/dist/index.html")
await mkdir(repo + "/.steve", { recursive: true })
const root = await mkdtemp(repo + "/.steve/inline-image-acceptance-")
const home = root + "/home", output = process.env.STEVE_TEST_SHOTS || root + "/evidence"
await mkdir(home); await mkdir(root + "/tmp"); await mkdir(output, { recursive: true })
const qr = await readFile(repo + "/web/test/fixtures/inline-image-qr.png")
assert.equal(qr.readUInt32BE(16), 320); assert.equal(qr.readUInt32BE(20), 320)
const sha = (b) => createHash("sha256").update(b).digest("hex")
const pause = (ms) => new Promise((r) => setTimeout(r, ms))
const exists = (p) => access(p).then(() => true, () => false)
async function freePort() {
  const server = createServer()
  await new Promise((r) => server.listen(0, "127.0.0.1", r))
  const port = server.address().port
  await new Promise((r) => server.close(r))
  return port
}
const port = await freePort(), nodePort = await freePort(), base = `http://127.0.0.1:${port}`
// Explicit allowlist: do not inherit the invoking agent's tokens, adapter settings or real HOME.
const env = Object.fromEntries(["PATH", "SHELL", "LANG", "LC_ALL"].filter((k) => process.env[k]).map((k) => [k, process.env[k]]))
Object.assign(env, { HOME: root, TMPDIR: root + "/tmp", STEVE_HOME: home, STEVE_HOST: "127.0.0.1",
  STEVE_PORT: String(port), STEVE_LOCAL_NODE_PORT: String(nodePort), STEVE_PUBLIC_URL: base,
  STEVE_DEV_LOGIN: "1", STEVE_REPO: root + "/no-source-checkout", STEVE_VAULT_KEYSTORE: "file",
  STEVE_FORBID_REAL_HOSTS: "1", STEVE_BACKUP: "off", STEVE_NODE_MODE: "local",
  STEVE_WEB_DIR: repo + "/web/dist", FAKE_RESEARCH_LOG: root + "/fixture", CLAUDE_CODE_EXECUTABLE: "/usr/bin/false" })
for (const m of (await readFile(repo + "/hub-rs/crates/steve-acp/src/adapters.rs", "utf8")).matchAll(/command_var: "(STEVE_ADAPTER_[A-Z_]+)"/g)) env[m[1]] = "/usr/bin/false"
env.STEVE_ADAPTER_CLAUDE = repo + "/hub-rs/crates/steve-node/tests/research-script.mjs"
const evidence = { operated: true, revision: execFileSync("git", ["rev-parse", "HEAD"], { cwd: repo, encoding: "utf8" }).trim(),
  diff_sha256: sha(execFileSync("git", ["diff", "HEAD"], { cwd: repo })), hub_binary_sha256: sha(await readFile(binary)),
  web_index_sha256: sha(await readFile(repo + "/web/dist/index.html")), qr_sha256: sha(qr), base_url: base,
  installation: root, fixture: "offline ACP; synthetic QR; no provider, OAuth or production data",
  checks: [], processes: [], screenshots: [], imageResponses: [], pageErrors: [] }
const log = createWriteStream(root + "/hub.log", { mode: 0o600 })
let hub, hubExit, browser, owner, page
async function save() { await writeFile(output + "/inline-image-evidence.json", JSON.stringify(evidence, null, 2)) }
async function check(name, operation) {
  try { const detail = await operation(); evidence.checks.push({ name, passed: true, operated: true, scope: "unit", detail }); console.log("PASS", name) }
  catch (error) { evidence.checks.push({ name, passed: false, operated: true, scope: "unit", error: String(error) }); throw error }
  finally { await save() }
}
async function start() {
  hub = spawn(binary, [], { env, stdio: ["ignore", "pipe", "pipe"] })
  hub.stdout.pipe(log, { end: false }); hub.stderr.pipe(log, { end: false })
  hubExit = once(hub, "exit")
  evidence.processes.push({ pid: hub.pid, node_mode: env.STEVE_NODE_MODE, started: new Date().toISOString() })
  for (let i = 0; ; i++) {
    assert.equal(hub.exitCode, null, "our isolated Hub exited")
    const health = await fetch(base + "/healthz").then((r) => r.ok ? r.json() : null, () => null)
    if (health && !health.maintenance) break
    assert.ok(i < 300, "isolated Hub startup timed out"); await pause(200)
  }
}
async function stop() {
  if (!hub) return
  const current = hub
  if (current.exitCode === null && current.signalCode === null) current.kill("SIGTERM")
  const timeout = setTimeout(() => { if (current.exitCode === null && current.signalCode === null) current.kill("SIGKILL") }, 15000)
  try { await hubExit } finally { clearTimeout(timeout) }
  evidence.processes.at(-1).stopped = new Date().toISOString()
  hub = undefined
}
function rows(sql) {
  return JSON.parse(execFileSync("python3", ["-c", "import json,sqlite3,sys; c=sqlite3.connect('file:'+sys.argv[1]+'?mode=ro',uri=True); print(json.dumps(c.execute(sys.argv[2]).fetchall()))", home + "/data/steve.sqlite3", sql], { encoding: "utf8" }))
}
const fileCount = () => rows("SELECT count(*) FROM files")[0][0]
async function turn(commands, marker) {
  const result = await owner.request.post(base + "/api/thread", { data: { text: "离线图片集成验收（不进行授权）\n" + commands + "\n@say " + marker } })
  assert.equal(result.status(), 200, await result.text())
  for (let i = 0; i < 300; i++) {
    const response = await owner.request.get(base + "/api/thread/history")
    assert.equal(response.status(), 200)
    const history = await response.json(), message = history.messages.filter((m) => m.author === "steve").at(-1)
    const attempt = rows("SELECT state FROM runtime_job_attempts ORDER BY rowid DESC LIMIT 1")
    if (message?.text.includes(marker) && attempt[0]?.[0] === "finished") {
      await writeFile(output + "/" + marker.toLowerCase() + "-history.json", JSON.stringify(history, null, 2))
      return { text: message.text, source: (await readFile(root + "/fixture.image", "utf8")).trim() }
    }
    await pause(200)
  }
  throw Error("thread completion timed out: " + marker)
}
async function retained(text, expected) {
  const urls = [...text.matchAll(/\]\((\/api\/files\/[^)\s]+)/g)].map((m) => m[1])
  assert.equal(urls.length, 2); assert.equal(urls[0], urls[1]); assert.equal(new URL(urls[0], base).searchParams.get("ws"), "default")
  const response = await owner.request.get(base + urls[0])
  assert.equal(response.status(), 200); assert.match(response.headers()["content-type"], /^image\/png/)
  assert.deepEqual(await response.body(), expected)
  return { url: urls[0], sha256: sha(expected), bytes: expected.length }
}
async function decoded(marker, width = 320) {
  const block = page.locator(".md").filter({ hasText: marker }).last()
  await block.waitFor()
  const images = block.locator('img[alt="授权二维码"], img[alt="重复二维码"]')
  await images.first().waitFor()
  for (const image of await images.all()) await image.evaluate((i) => i.decode())
  const result = await images.evaluateAll((xs) => xs.map((i) => ({ alt: i.alt, src: i.src, naturalWidth: i.naturalWidth,
    naturalHeight: i.naturalHeight, displayedWidth: i.getBoundingClientRect().width, crossOrigin: i.crossOrigin })))
  assert.equal(result.length, 2)
  for (const image of result) {
    assert.equal(image.naturalWidth, width); assert.equal(image.naturalHeight, width)
    assert.equal(new URL(image.src).origin, base); assert.ok(new URL(image.src).pathname.startsWith("/api/files/"))
    assert.equal(image.crossOrigin, "use-credentials")
  }
  return result
}
async function shot(marker, name) {
  const block = page.locator(".md").filter({ hasText: marker }).last()
  await block.scrollIntoViewIfNeeded()
  const path = output + "/" + name + ".png"
  await page.screenshot({ path }); evidence.screenshots.push(path)
}
// Different valid bytes for R2 avoid deduplication of the already successful QR hiding a failure.
function png(rgb) {
  function chunk(type, data) {
    const content = Buffer.concat([Buffer.from(type), data]); let crc = 0xffffffff
    for (const b of content) { crc ^= b; for (let i = 0; i < 8; i++) crc = (crc >>> 1) ^ (crc & 1 ? 0xedb88320 : 0) }
    const head = Buffer.alloc(4), tail = Buffer.alloc(4); head.writeUInt32BE(data.length); tail.writeUInt32BE((crc ^ 0xffffffff) >>> 0)
    return Buffer.concat([head, content, tail])
  }
  const header = Buffer.alloc(13); header.writeUInt32BE(2); header.writeUInt32BE(2, 4); header[8] = 8; header[9] = 2
  const row = Buffer.from([0, ...rgb, ...rgb])
  return Buffer.concat([Buffer.from("89504e470d0a1a0a", "hex"), chunk("IHDR", header), chunk("IDAT", deflateSync(Buffer.concat([row, row]))), chunk("IEND", Buffer.alloc(0))])
}
try {
  await start()
  const { chromium } = await import(process.env.PLAYWRIGHT_MODULE || "playwright")
  browser = await chromium.launch({ headless: true, ...(process.env.PLAYWRIGHT_CHANNEL ? { channel: process.env.PLAYWRIGHT_CHANNEL } : {}),
    ...(process.env.PLAYWRIGHT_EXECUTABLE ? { executablePath: process.env.PLAYWRIGHT_EXECUTABLE } : {}) })
  owner = await browser.newContext({ locale: "zh-CN", viewport: { width: 1280, height: 900 }, reducedMotion: "reduce" })
  assert.equal((await owner.request.get(base + "/auth/dev")).status(), 200)
  page = await owner.newPage()
  page.on("pageerror", (e) => evidence.pageErrors.push(e.message))
  page.on("response", (r) => { if (new URL(r.url()).pathname.startsWith("/api/files/")) evidence.imageResponses.push({ url: r.url(), status: r.status() }) })
  const automatic = await turn("@msg qr-temp\n@image " + qr.toString("base64"), "INLINE_QR_TEMP_DONE")
  await check("FullAccess TMPDIR QR is retained before automatic source cleanup", async () => {
    assert.equal(await exists(automatic.source), false)
    const saved = await retained(automatic.text, qr)
    const execution = rows("SELECT node_id,json_extract(spec,'$.run.policy.permissions'),json_extract(spec,'$.work.kind') FROM runtime_job_attempts ORDER BY rowid DESC LIMIT 1")
    assert.deepEqual(execution[0], ["local", "full_access", "research"])
    await writeFile(output + "/retained-qr.png", qr)
    return { ...saved, source: automatic.source, source_exists: false, execution: execution[0] }
  })
  const manual = await turn("@msg qr-manual\n@imagecwd " + qr.toString("base64"), "INLINE_QR_MANUAL_DONE")
  await check("Desktop production ChatView displays the generated QR twice", async () => {
    await page.goto(base + "/#/")
    const images = await decoded("INLINE_QR_MANUAL_DONE")
    await shot("INLINE_QR_MANUAL_DONE", "qr-desktop-before-cleanup")
    return { images, source_exists: await exists(manual.source), persisted_files: fileCount() }
  })
  await check("Existing original Node path fails in the browser while its retained URL displays", async () => {
    assert.equal(await exists(manual.source), true)
    const result = await page.evaluate((url) => new Promise((done) => {
      const image = new Image(); image.onload = () => done({ event: "load", width: image.naturalWidth })
      image.onerror = () => done({ event: "error", width: image.naturalWidth }); image.src = url
    }), base + manual.source)
    assert.deepEqual(result, { event: "error", width: 0 })
    return { ...result, source: manual.source, source_exists: true, retained: await retained(manual.text, qr) }
  })
  await check("Delete only our generated cwd source; authenticated bytes survive", async () => {
    assert.ok(manual.source.startsWith(root + sep), "refuse to delete outside our test installation")
    assert.equal(basename(manual.source), "qr.png"); assert.ok(basename(dirname(manual.source)).startsWith("inline-image-"))
    assert.equal(await exists(manual.source), true)
    await unlink(manual.source); await rmdir(dirname(manual.source))
    assert.equal(await exists(manual.source), false)
    return { ...await retained(manual.text, qr), source: manual.source, source_exists: false, removed_by_this_test: true }
  })
  await check("Desktop reload decodes both QR messages after both sources disappear", async () => {
    await page.reload(); const images = await decoded("INLINE_QR_TEMP_DONE")
    await decoded("INLINE_QR_MANUAL_DONE"); await shot("INLINE_QR_MANUAL_DONE", "qr-desktop-after-cleanup-refresh")
    return images
  })
  await check("390px reload shows the 320px QR without horizontal overflow", async () => {
    await page.setViewportSize({ width: 390, height: 844 }); await page.reload()
    const images = await decoded("INLINE_QR_MANUAL_DONE")
    assert.equal(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth), false)
    assert.ok(images.every((i) => i.displayedWidth > 0 && i.displayedWidth <= 390))
    await shot("INLINE_QR_MANUAL_DONE", "qr-mobile-390-after-cleanup-refresh")
    return images
  })
  await check("Anonymous access is 401 and another real workspace is 404", async () => {
    const href = (await retained(automatic.text, qr)).url, anonymous = await browser.newContext()
    const status = (await anonymous.request.get(base + href)).status(); await anonymous.close(); assert.equal(status, 401)
    const created = await owner.request.post(base + "/api/workspaces", { data: { name: "Inline image isolation" } })
    assert.equal(created.status(), 200); const other = await created.json(), wrong = new URL(href, base)
    wrong.searchParams.set("ws", other.id)
    const cross = (await owner.request.get(wrong.href)).status(); assert.equal(cross, 404)
    return { anonymous_status: status, cross_workspace_status: cross }
  })
  await check("R1 literal https/www URLs neither upload nor become browser images", async () => {
    const before = fileCount(), result = await turn("@msg url\n@urlimage " + qr.toString("base64"), "INLINE_URL_DONE")
    assert.equal(result.text, `https://example.test/![授权二维码](${result.source})\nwww.example.test/![授权二维码](${result.source})INLINE_URL_DONE`)
    assert.equal(fileCount(), before); assert.equal(await exists(result.source), false)
    await page.reload(); const block = page.locator(".md").filter({ hasText: "INLINE_URL_DONE" }).last(); await block.waitFor()
    assert.equal(await block.locator("img").count(), 0)
    return { files_before: before, files_after: fileCount(), browser_images: 0, text: result.text }
  })
  const good = png([0, 160, 0]), next = png([0, 0, 160])
  let sameTurn, nextTurn
  await check("R2 four malformed PNGs release quota for the same and next executions", async () => {
    const before = fileCount(), started = Date.now()
    const bad = Array.from({ length: 4 }, (_, i) => Buffer.concat([Buffer.from("89504e470d0a1a0a", "hex"), Buffer.from("bad-" + i)]))
    sameTurn = await turn("@msg bad-batch\n" + [...bad, good].map((b) => "@image " + b.toString("base64")).join("\n"), "INLINE_RECOVERED_DONE")
    const same = await retained(sameTurn.text, good)
    nextTurn = await turn("@msg next\n@image " + next.toString("base64"), "INLINE_NEXT_DONE")
    const later = await retained(nextTurn.text, next)
    assert.equal(await exists(sameTurn.source), false); assert.equal(await exists(nextTurn.source), false)
    assert.equal(fileCount(), before + 2)
    await page.reload(); await decoded("INLINE_RECOVERED_DONE", 2); await decoded("INLINE_NEXT_DONE", 2)
    return { same, next: later, files_before: before, files_after: fileCount(), elapsed_ms: Date.now() - started,
      manual_abort: false, hub_restart: false, waited_for_ttl: false }
  })
  await check("Hub restart with Node off preserves history, bytes and visible QR", async () => {
    await stop(); env.STEVE_NODE_MODE = "off"; env.STEVE_NO_RESUME = "1"; await start()
    await retained(automatic.text, qr); await retained(manual.text, qr); await retained(sameTurn.text, good); await retained(nextTurn.text, next)
    await page.reload(); const images = await decoded("INLINE_QR_MANUAL_DONE")
    await shot("INLINE_QR_MANUAL_DONE", "qr-mobile-390-node-off-restart")
    assert.deepEqual(evidence.pageErrors, [])
    return { images, node_mode: "off", page_errors: 0, sources_exist: [await exists(automatic.source), await exists(manual.source)] }
  })
  evidence.passed = true
} catch (error) {
  evidence.passed = false; evidence.error = String(error)
  if (page) await page.screenshot({ path: output + "/failure.png" }).catch(() => {})
  throw error
} finally {
  await browser?.close(); await stop(); log.end()
  evidence.owned_processes_stopped = evidence.processes.every((p) => p.stopped)
  await save()
}
