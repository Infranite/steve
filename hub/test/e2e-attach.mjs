// Acceptance for attachments: a PNG screenshot is uploaded, added to an item as a supplement, and the real agent
// describes what is in it; the same picture in the main conversation gets an answer about it.
// Usage: node test/e2e-attach.mjs <base-url>   (a throwaway Hub started with STEVE_DEV_LOGIN=1; see test/e2e.sh)
import { existsSync, readdirSync, rmSync } from "node:fs"
import { join } from "node:path"
import { deflateSync } from "node:zlib"
import { base, login, cookieHeader, call, statusOf, log, waitFor, openOf, check, setup, answerPermission } from "./lib.mjs"

/** a 240×160 "screenshot": white background, a red disc on the left, a blue square on the right */
function picture() {
  const W = 240, H = 160, raw = Buffer.alloc((W * 3 + 1) * H, 255)
  for (let y = 0; y < H; y++) {
    raw[y * (W * 3 + 1)] = 0
    for (let x = 0; x < W; x++) {
      const o = y * (W * 3 + 1) + 1 + x * 3
      if ((x - 60) ** 2 + (y - 80) ** 2 < 45 ** 2) raw.set([220, 30, 30], o)
      else if (x >= 140 && x < 220 && y >= 40 && y < 120) raw.set([30, 60, 220], o)
    }
  }
  const crc = (b) => { let c = ~0; for (const v of b) { c ^= v; for (let k = 0; k < 8; k++) c = c & 1 ? (c >>> 1) ^ 0xedb88320 : c >>> 1 } return ~c >>> 0 }
  const chunk = (t, d) => { const l = Buffer.alloc(4); l.writeUInt32BE(d.length); const td = Buffer.concat([Buffer.from(t), d]); const c = Buffer.alloc(4); c.writeUInt32BE(crc(td)); return Buffer.concat([l, td, c]) }
  const ihdr = Buffer.alloc(13); ihdr.writeUInt32BE(W, 0); ihdr.writeUInt32BE(H, 4); ihdr[8] = 8; ihdr[9] = 2
  return Buffer.concat([Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]), chunk("IHDR", ihdr), chunk("IDAT", deflateSync(raw)), chunk("IEND", Buffer.alloc(0))])
}
const describes = (t) => /红|red/i.test(t) && /蓝|blue/i.test(t)

await login()
const cookie = cookieHeader()
const upload = async (buf, name) => { const r = await fetch(`${base}/api/files?name=${encodeURIComponent(name)}`, { method: "POST", body: buf, headers: { cookie, "content-type": "application/octet-stream" } }); const j = await r.json(); if (!r.ok) throw new Error("upload " + r.status + " " + JSON.stringify(j)); return j }

const png = picture()
const f = await upload(png, "截图 2026-10-31.png")
check(f.mime === "image/png" && f.size === png.length && f.name === "截图 2026-10-31.png", "upload sniffed a PNG")
const dl = await fetch(`${base}/api/files/${f.id}`, { headers: { cookie } })
check(dl.status === 200 && dl.headers.get("content-type") === "image/png" && dl.headers.get("x-content-type-options") === "nosniff" && /^inline;/.test(dl.headers.get("content-disposition") ?? ""), "download: inline image, nosniff")
check((await fetch(`${base}/api/files/${f.id}`)).status === 401, "download needs a login")
const other = await call("POST", "/api/workspaces", { name: "e2e-att-" + Date.now().toString(36) })
check(await statusOf("GET", "/api/files/" + f.id, null, other.id) === 404, "file is invisible from another workspace")
check(await statusOf("POST", "/api/thread", { text: "x", files: [f.id] }, other.id) === 404, "another workspace can't attach it")
const bad = await fetch(`${base}/api/files?name=a.exe`, { method: "POST", body: Buffer.from([0xcf, 0xfa, 0xed, 0xfe, 7, 0, 0, 1]), headers: { cookie } })
check(bad.status === 415, "a binary that is not an allowed type is refused")

// ── an item gets the screenshot as a supplement; the agent has to say what is in it
const { dir, project, bot } = await setup("attach")
const item = await call("POST", "/api/items", { goal: "看用户补充的截图，用中文说出图里有哪些图形、各是什么颜色。不要修改任何文件。说完用 steve_deliver 交付，summary 里写出你的描述。", title: "描述截图", bot: bot.id, project: project.id, mode: "propose" })
/* propose: no session yet, so the supplement opens the first one with the picture in it */
for (const d of await openOf(item.id)) await call("POST", `/api/decisions/${d.id}/dismiss`)
await call("POST", `/api/items/${item.id}/supplement`, { text: "截图在这里", mode: "new", files: [f.id] })
let acc
while (!acc) {
  const ds = await waitFor(async () => { const x = await openOf(item.id); return x.length ? x : null }, "a decision", 400000)
  for (const d of ds) {
    if (d.kind === "accept") acc = d
    else if (d.kind === "approve" && d.subject_type === "tool_call") await answerPermission(d, dir)
    else throw new Error(`unexpected decision ${d.kind}: ${d.question}`)
  }
}
log("deliver:", acc.draft.slice(0, 300))
check(describes(acc.draft), "the agent described the red and the blue shape")
const det = await call("GET", "/api/items/" + item.id)
const tl = det.timeline.find((t) => t.kind === "me" && t.files?.length)
check(tl && tl.files[0].id === f.id && tl.files[0].mime === "image/png", "the item comment carries the attachment")
const evs = await call("GET", `/api/sessions/${det.sessionIds.at(-1)}/events`)
const u = evs.find((e) => e.kind === "user" && e.data?.files?.length)
check(u && u.data.files[0].id === f.id, "the session prompt carries the attachment")
const saved = existsSync(join(dir, ".steve/attachments")) ? readdirSync(join(dir, ".steve/attachments")) : []
check(saved.some((n) => n.endsWith("-截图 2026-10-31.png")) && saved.includes(".gitignore"), "the file was saved into .steve/attachments of the working directory")
const reads = evs.filter((e) => e.kind === "tool_call").map((e) => e.data?.title ?? "")
log("tool calls:", JSON.stringify(reads))

// ── the main conversation: the same picture with a question
const t0 = Date.now()
await call("POST", "/api/thread", { text: "这张截图里有什么颜色的图形？直接回答，不用建事项。", files: [f.id] })
const reply = await waitFor(async () => { const s = await call("GET", "/api/state"); return s.thread.find((m) => m.author === "steve" && m.created > t0) }, "Steve's reply", 300000)
const mine = (await call("GET", "/api/state")).thread.find((m) => m.author === "me" && m.created >= t0 - 1000)
check(mine?.meta?.files?.[0]?.id === f.id, "the chat message carries the attachment")
log("chat reply:", reply.text.slice(0, 300), "| item:", reply.item_id ?? "-")
if (reply.item_id) {
  /* the router may still make it an item: then the bot sees the picture */
  const d = await waitFor(async () => (await openOf(reply.item_id)).find((x) => x.kind === "accept"), "delivery of the chat item", 400000)
  check(describes(d.draft), "the chat item's agent described the picture")
} else check(describes(reply.text), "Steve's chat answer describes the picture")
console.log(JSON.stringify({ ok: true, file: f.id, item: item.id, deliver: acc.draft.slice(0, 200), chat: reply.text.slice(0, 200) }))
rmSync(dir, { recursive: true, force: true })
