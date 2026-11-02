// Attachments: upload validation by content, name sanitizing, workspace isolation, download headers,
// and what an agent gets (files in .steve/attachments, images inline). Throwaway STEVE_HOME; no agent runs.
import { test, after } from "node:test"
import assert from "node:assert/strict"
import { mkdtempSync, mkdirSync, readFileSync, existsSync, rmSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { deflateSync } from "node:zlib"

const home = mkdtempSync(join(tmpdir(), "steve-files-"))
Object.assign(process.env, { STEVE_HOME: home, STEVE_REPO: join(home, "no-repo"), STEVE_ADAPTER_CLAUDE: join(home, "no-adapter.js") })
const { ensureWorkspace, WS, db } = { ...(await import("../src/repo.js")), ...(await import("../src/db.js")) }
const att = await import("../src/files.js")
const { Hono } = await import("hono")
const { api } = await import("../src/api.js")
ensureWorkspace()
after(() => setImmediate(() => { rmSync(home, { recursive: true, force: true }); process.exit(0) }))

/** a real 2×2 PNG */
function png() {
  const crc = (b: Buffer) => { let c = ~0; for (const x of b) { c ^= x; for (let k = 0; k < 8; k++) c = c & 1 ? (c >>> 1) ^ 0xedb88320 : c >>> 1 } return ~c >>> 0 }
  const chunk = (t: string, d: Buffer) => { const l = Buffer.alloc(4); l.writeUInt32BE(d.length); const td = Buffer.concat([Buffer.from(t), d]); const c = Buffer.alloc(4); c.writeUInt32BE(crc(td)); return Buffer.concat([l, td, c]) }
  const ihdr = Buffer.alloc(13); ihdr.writeUInt32BE(2, 0); ihdr.writeUInt32BE(2, 4); ihdr[8] = 8; ihdr[9] = 2
  const raw = Buffer.from([0, 255, 0, 0, 0, 255, 0, 0, 0, 0, 255, 255, 255, 255])
  return Buffer.concat([Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]), chunk("IHDR", ihdr), chunk("IDAT", deflateSync(raw)), chunk("IEND", Buffer.alloc(0))])
}

test("types come from the bytes, not the name", () => {
  assert.equal(att.validate(png(), "shot.png").mime, "image/png")
  assert.equal(att.validate(Buffer.from("%PDF-1.7\n..."), "a.pdf").mime, "application/pdf")
  assert.equal(att.validate(Buffer.from([0x50, 0x4b, 0x03, 0x04, 1, 2, 3]), "a.zip").mime, "application/zip")
  assert.equal(att.validate(Buffer.from("2026-10-31 ERROR 出错了\n"), "hub.log").mime, "text/plain; charset=utf-8")
  /* HTML and SVG are only ever text, never served as markup */
  assert.equal(att.validate(Buffer.from("<svg onload=alert(1)>"), "x.svg").mime, "text/plain; charset=utf-8")
  /* binaries that are none of the allowed kinds are refused */
  assert.throws(() => att.validate(Buffer.from([0xcf, 0xfa, 0xed, 0xfe, 0, 0, 0, 7]), "tool"), (e: any) => e.status === 415)
  assert.throws(() => att.validate(Buffer.from([0xff, 0xfe, 0x00, 0x41]), "a.txt"), (e: any) => e.status === 415)
  assert.throws(() => att.validate(Buffer.alloc(0), "a.txt"), (e: any) => e.status === 400)
  assert.throws(() => att.validate(Buffer.alloc(att.MAX_FILE + 1, 0x61), "big.txt"), (e: any) => e.status === 413)
  assert.equal(att.validate(Buffer.alloc(att.MAX_FILE, 0x61), "ok.txt").mime, "text/plain; charset=utf-8")
})

test("names are sanitized and the extension matches the content", () => {
  const n = (raw: string, b: Buffer = png()) => att.validate(b, raw).name
  assert.equal(n("../../etc/passwd.png"), "passwd.png")
  assert.equal(n("C:\\Users\\me\\截图 2026.png"), "截图 2026.png")
  assert.equal(n(".bashrc.png"), "bashrc.png")
  assert.equal(n("evil.exe"), "evil.exe.png")
  assert.equal(n("a\u202egnp.exe"), "agnp.exe.png")
  assert.equal(n('x"; filename=y.png'), "x_; filename=y.png")
  assert.equal(n(""), "file.png")
  assert.equal(n("notes", Buffer.from("hello")), "notes.txt")
  assert.equal(n("report.html", Buffer.from("<b>hi</b>")), "report.html")
  assert.ok(n("a".repeat(300) + ".png").length <= 120)
  assert.ok(n("a".repeat(300) + ".png").endsWith(".png"))
})

test("stored per workspace under the sha prefix; another workspace can't reference it", () => {
  const f = att.store(WS, png(), "s.png", "usr_1")
  assert.equal(att.blobPath(f), join(home, "data/files", WS, f.sha256.slice(0, 2), f.id))
  assert.deepEqual(readFileSync(att.blobPath(f)), png())
  assert.equal(att.store(WS, png(), "s.png", "usr_1").id, f.id, "same bytes and name are stored once")
  assert.deepEqual(att.refs(WS, [f.id]), [{ id: f.id, name: "s.png", mime: "image/png", size: png().length }])
  assert.throws(() => att.refs("ws_other", [f.id]), (e: any) => e.status === 404)
  assert.throws(() => att.refs(WS, ["fil_nope"]), (e: any) => e.status === 404)
})

test("agents get the files in .steve/attachments and images inline", () => {
  const cwd = join(home, "cwd"); mkdirSync(cwd)
  const img = att.store(WS, png(), "shot.png", "u"), log = att.store(WS, Buffer.from("boom\n"), "hub.log", "u")
  const a = att.forAgent(cwd, att.refs(WS, [img.id, log.id]), true)
  assert.equal(a.images.length, 1)
  assert.deepEqual(a.images[0], { type: "image", data: png().toString("base64"), mimeType: "image/png" })
  assert.match(a.note, /\.steve\/attachments\/[0-9a-f]+-shot\.png/)
  assert.match(a.note, /hub\.log/)
  assert.equal(readFileSync(join(cwd, ".steve/attachments/.gitignore"), "utf8"), "*\n")
  assert.ok(existsSync(join(cwd, `.steve/attachments/${log.id.slice(4)}-hub.log`)))
  /* an adapter without image support only gets paths */
  assert.equal(att.forAgent(cwd, att.refs(WS, [img.id]), false).images.length, 0)
})

test("attachments wait in the queue with their prompt (release hold, restart) and go out with it", async () => {
  const acp = await import("../src/acp.js"), { sessions, queued, events } = await import("../src/repo.js")
  const img = att.store(WS, png(), "held.png", "u")
  const s = sessions.create({ workspace_id: WS, item_id: null, bot_id: null, harness: "claude-code", model: null, cwd: join(home, "cwd"), title: "t", token: "tok" })
  acp.hold(true)
  assert.equal(await acp.prompt(s.id, "看这张", "me", att.refs(WS, [img.id])), "queued")
  /* what a restart reads back from the database */
  assert.deepEqual(queued.list(s.id).map((q) => q.files.map((f) => f.id)), [[img.id]])
  acp.hold(false)
  assert.ok(acp.flush(s.id, { text: "继续", via: "steve" }))
  const u = events.after(s.id).filter((e: any) => e.kind === "user")
  assert.deepEqual(u.map((e: any) => [e.data.text, (e.data.files ?? []).map((f: any) => f.id)]), [["继续", []], ["看这张", [img.id]]])
})

/* the HTTP layer, with a user already signed in */
const app = new Hono<{ Variables: { user: any } }>()
app.use("*", async (c, next) => { c.set("user", { id: db.prepare("SELECT id FROM users LIMIT 1").pluck().get() ?? "usr_test" }); await next() })
app.route("/api", api)
db.prepare("INSERT OR IGNORE INTO users (id, workspace_id, subject, name, role, created) VALUES ('usr_test', ?, 'test', 'T', 'owner', 0)").run(WS)
db.prepare("INSERT OR IGNORE INTO workspace_members (workspace_id, user_id, role, created) VALUES (?, 'usr_test', 'owner', 0)").run(WS)

test("upload and download over HTTP", async () => {
  const up = await app.request("/api/files?name=" + encodeURIComponent("屏幕截图.png"), { method: "POST", body: png(), headers: { "content-type": "application/octet-stream" } })
  assert.equal(up.status, 200)
  const f = await up.json() as any
  assert.equal(f.mime, "image/png"); assert.equal(f.name, "屏幕截图.png")
  const dl = await app.request("/api/files/" + f.id)
  assert.equal(dl.status, 200)
  assert.equal(dl.headers.get("content-type"), "image/png")
  assert.equal(dl.headers.get("x-content-type-options"), "nosniff")
  assert.match(dl.headers.get("content-disposition")!, /^inline; filename="_+\.png"; filename\*=UTF-8''%E5%B1%8F/)
  assert.deepEqual(Buffer.from(await dl.arrayBuffer()), png())

  const fd = new FormData(); fd.append("file", new Blob(["<html><script>alert(1)</script>"]), "page.html")
  const t = await (await app.request("/api/files", { method: "POST", body: fd })).json() as any
  assert.equal(t.mime, "text/plain; charset=utf-8")
  const td = await app.request("/api/files/" + t.id)
  assert.match(td.headers.get("content-disposition")!, /^attachment;/)
  assert.equal(td.headers.get("content-type"), "text/plain; charset=utf-8")

  const bad = await app.request("/api/files?name=a.bin", { method: "POST", body: Buffer.from([0, 1, 2, 3, 0xff]) })
  assert.equal(bad.status, 415)
  const big = await app.request("/api/files?name=a.txt", { method: "POST", body: Buffer.alloc(att.MAX_FILE + 100 * 1024, 0x61) })
  assert.equal(big.status, 413)
  /* files of another workspace are not found */
  db.prepare("UPDATE files SET workspace_id = 'ws_other' WHERE id = ?").run(t.id)
  assert.equal((await app.request("/api/files/" + t.id)).status, 404)
  const say = await app.request("/api/thread", { method: "POST", body: JSON.stringify({ text: "看图", files: [t.id] }), headers: { "content-type": "application/json" } })
  assert.equal(say.status, 404)
})
