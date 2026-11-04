/* REST + SSE for the web app. Thin: validation here, behaviour in orchestrator/thread/changes.
 * Every request runs in one workspace (cookie steve_ws, checked against membership). */
import { Hono, type Context } from "hono"
import { getCookie, setCookie } from "hono/cookie"
import { streamSSE } from "hono/streaming"
import { z } from "zod"
import { existsSync, readdirSync, readFileSync, statSync } from "node:fs"
import { join, basename } from "node:path"
import { homedir } from "node:os"
import { items, decisions, sessions, events, timeline, artifacts, bots, messages, queued, projects, tags, deps, memories, settings, workspaces, devices, audit, type Item } from "./repo.js"
import { subscribe } from "./bus.js"
import * as orch from "./orchestrator.js"
import { say, stop, promote } from "./thread.js"
import * as notices from "./notify.js"
import * as acp from "./acp.js"
import * as changes from "./changes.js"
import { runtimes, localSessions, importLocal, removeImported } from "./system.js"
import { nativeId, resumeInfo } from "./resume.js"
import { claudePrint, firstJson, route } from "./gateway.js"
import { config, webVersion } from "./config.js"
import { db } from "./db.js"
import { bodyLimit } from "hono/body-limit"
import { files as fileRows } from "./repo.js"
import * as att from "./files.js"

type Env = { Variables: { user: any; ws: string } }
export const api = new Hono<Env>()
const body = async <T extends z.ZodTypeAny>(c: Context, s: T): Promise<z.infer<T>> => s.parse(await c.req.json())
api.onError((e, c) => c.json({ error: e instanceof z.ZodError ? "参数不对：" + e.issues.map((i) => i.path.join(".")).join("、") : e.message }, e instanceof z.ZodError ? 400 : ((e as any).status ?? 500)))

/* workspace context */
api.use("*", async (c, next) => {
  const u = c.get("user"), mine = workspaces.ofUser(u.id)
  const want = c.req.header("x-steve-ws") ?? getCookie(c, "steve_ws")
  c.set("ws", mine.find((w) => w.id === want)?.id ?? mine[0]?.id ?? "default")
  await next()
})
const ws = (c: Context<Env>) => c.get("ws")
const me = (c: Context<Env>) => c.get("user").id as string
/** objects fetched by id must belong to the current workspace */
const own = <T extends { workspace_id: string } | undefined>(c: Context<Env>, x: T, what: string) => { if (!x || x.workspace_id !== ws(c)) throw Object.assign(new Error(`没有这个${what}`), { status: 404 }); return x as NonNullable<T> }
/** bots are shared across workspaces: the bot must be a member of the current one (deleted ones too, for restore) */
const ownBot = (c: Context<Env>, id: string) => { if (!bots.inWorkspace(id, ws(c))) throw Object.assign(new Error("没有这个 Bot"), { status: 404 }); return id }

const summary = (i: Item) => {
  const ss = sessions.ofItem(i.id), open = decisions.ofItem(i.id).filter((d) => d.status === "open")
  const act = ss.filter((s) => s.status !== "ended").at(-1)
  return { ...i, goal: items.goal(i.id), goals: items.goals(i.id).length, tags: tags.ofItem(i.id), deps: deps.of(i.id), sessionIds: ss.map((s) => s.id), open: open.map((d) => d.id),
    queued: act ? queued.list(act.id).length : 0, live: ss.some((s) => s.status === "running" || s.status === "starting") }
}
const sessOut = (s: any) => ({ ...s, token_hash: undefined, import_key: undefined, readonly: !!s.readonly, busy: acp.isBusy(s.id), queued: queued.list(s.id), harness_session_id: nativeId(s) })
/* what the web shows of an event: screenshots (base64) and huge tool output stay in the database (fork and handoff read
 * them there), so one screenshot no longer ships 300 KB to every client and into every transcript rebuild */
const slim = (v: any, k = ""): any => typeof v === "string" ? (v.length > 4000 && k !== "newText" && k !== "oldText" ? v.slice(0, 4000) + "…" : v)
  : Array.isArray(v) ? v.map((x) => slim(x)) : v && typeof v === "object" ? (v.type === "image" ? { type: "image" } : Object.fromEntries(Object.entries(v).map(([a, b]) => [a, slim(b, a)]))) : v
/* of the harness's _meta (a second copy of the tool output) only the tool's name ships, as `tool`; of rawInput only what
 * the transcript shows (the command and its description, the file, the pattern, an edit's old and new text, Steve's plan) */
const INPUT = new Set(["command", "description", "file_path", "pattern", "query", "url", "now", "plan", "summary", "question", "old_string", "new_string", "content", "edits"])
export const evOut = (d: any) => {
  if (!d || typeof d !== "object") return d
  const { _meta, rawInput, ...x } = d
  const inp = rawInput && typeof rawInput === "object" && !Array.isArray(rawInput) ? Object.fromEntries(Object.entries(rawInput).filter(([k]) => INPUT.has(k))) : {}
  if (Object.keys(inp).length) x.rawInput = inp
  if (typeof _meta?.claudeCode?.toolName === "string") x.tool = _meta.claudeCode.toolName
  return slim(x)
}
const botOut = (b: any, w: string) => ({ ...b, workspaces: bots.workspaces(b.id).map((x) => x.workspace_id), memories: memories.count(b.id, w) })

api.get("/me", (c) => c.json({ user: c.get("user"), version: config.version, web_version: webVersion(), workspaces: workspaces.ofUser(me(c)), workspace: ws(c) }))
api.get("/state", (c) => {
  const w = ws(c)
  return c.json({
    version: config.version, web_version: webVersion(), workspace: workspaces.get(w), workspaces: workspaces.ofUser(me(c)),
    items: items.all(w).map(summary), decisions: decisions.open(w), recent: decisions.recent(w, Date.now() - 7 * 24 * 3600_000),
    bots: bots.list(w).map((b) => botOut(b, w)), allBots: bots.list().map((b) => ({ id: b.id, name: b.name, workspaces: bots.workspaces(b.id).map((x) => x.workspace_id) })),
    projects: projects.list(w), tags: tags.list(w), tagUses: tags.uses(w), thread: notices.thread(w), notifications: notices.list(w), notifySeen: notices.seen(w),
    settings: { ...changes.settingDefaults(), ...settings.all(w) }, sessions: sessions.list(w).map(sessOut), devices: devices.list(), runtimes: runtimes(),
  })
})

/* ── workspaces ── */
api.post("/workspaces", async (c) => { const p = await body(c, z.object({ name: z.string().min(1).max(40), description: z.string().max(400).optional(), item_prefix: z.string().regex(/^[A-Z]{1,6}$/).optional() })); const w = workspaces.create(p, me(c)); bots.setWorkspaces("steve", [...bots.workspaces("steve").map((x) => x.workspace_id), w.id]); audit(me(c), "workspace.create", w.id, p, w.id); return c.json(w) })
api.patch("/workspaces/:id", async (c) => {
  const id = c.req.param("id"); if (!workspaces.isMember(id, me(c))) return c.json({ error: "没有这个工作空间" }, 404)
  const p = await body(c, z.object({ name: z.string().min(1).max(40).optional(), description: z.string().max(400).optional(), item_prefix: z.string().regex(/^[A-Z]{1,6}$/).optional() }))
  return c.json(workspaces.patch(id, p))
})
api.post("/workspaces/:id/switch", (c) => {
  const id = c.req.param("id"); if (!workspaces.isMember(id, me(c))) return c.json({ error: "没有这个工作空间" }, 404)
  setCookie(c, "steve_ws", id, { httpOnly: true, sameSite: "Lax", path: "/", secure: config.publicUrl.startsWith("https://"), maxAge: 365 * 24 * 3600 }); return c.json({ ok: true })
})

/* ── attachments ── upload: the raw bytes as the body (name in ?name= or X-File-Name, URL-encoded) or multipart field "file";
 * the type comes from the bytes. Download: images inline, everything else as a download, never sniffed or rendered as HTML. */
const Files = z.array(z.string().max(64)).max(att.MAX_FILES).optional()
api.post("/files", bodyLimit({ maxSize: att.MAX_FILE + 64 * 1024, onError: (c) => c.json({ error: `文件太大，最多 ${att.MAX_FILE / 1024 / 1024} MB` }, 413) }), async (c) => {
  let buf: Buffer, name: string | null | undefined
  if ((c.req.header("content-type") ?? "").startsWith("multipart/form-data")) {
    const f = (await c.req.parseBody())["file"]
    if (!(f instanceof File)) return c.json({ error: "缺少文件（字段 file）" }, 400)
    buf = Buffer.from(await f.arrayBuffer()); name = f.name
  } else {
    buf = Buffer.from(await c.req.arrayBuffer())
    const raw = c.req.query("name") ?? c.req.header("x-file-name"); try { name = raw ? decodeURIComponent(raw) : null } catch { name = raw }
  }
  const f = att.store(ws(c), buf, name, me(c))
  return c.json({ id: f.id, name: f.name, mime: f.mime, size: f.size })
})
api.get("/files/:id", (c) => {
  const f = own(c, fileRows.get(c.req.param("id")), "附件"), p = att.blobPath(f)
  if (!existsSync(p)) return c.json({ error: "附件的文件不见了" }, 404)
  const inline = att.isInlineImage(f.mime) && c.req.query("download") !== "1"
  const ascii = f.name.replace(/[^\x20-\x7e]/g, "_").replace(/["\\]/g, "_")
  return c.body(readFileSync(p), 200, {
    "content-type": f.mime, "content-length": String(f.size), "x-content-type-options": "nosniff",
    "content-disposition": `${inline ? "inline" : "attachment"}; filename="${ascii}"; filename*=UTF-8''${encodeURIComponent(f.name)}`,
    "content-security-policy": "default-src 'none'; sandbox", "cross-origin-resource-policy": "same-origin", "cache-control": "private, max-age=31536000, immutable",
  })
})

/* ── conversation ── */
api.post("/thread", async (c) => { const p = await body(c, z.object({ text: z.string().min(1).max(20000), files: Files })); void say(ws(c), p.text, att.refs(ws(c), p.files)); return c.json({ ok: true }) })
api.post("/thread/stop", (c) => c.json({ ok: stop(ws(c)) }))
/* notifications: everything up to `at` (default now) counts as read */
api.post("/notifications/read", async (c) => { const p = await body(c, z.object({ at: z.number().int().positive().optional() })); return c.json({ seen: notices.markSeen(ws(c), Math.min(p.at ?? Date.now(), Date.now())) }) })
api.post("/notifications/:id/dismiss", (c) => { notices.dismiss(ws(c), c.req.param("id")); return c.json({ ok: true }) })
api.post("/thread/:id/promote", async (c) => c.json(await promote(ws(c), c.req.param("id"))))

/* ── items ── */
api.post("/items", async (c) => {
  const p = await body(c, z.object({ goal: z.string().min(1), title: z.string().optional(), bot: z.string().optional(), project: z.string().nullable().optional(), mode: z.enum(["start", "propose"]).default("start") }))
  /* whatever the user left open (bot, project, title, tags) Steve decides, the same way as in the conversation */
  const r = !p.bot || p.project === undefined || !p.title ? await route(ws(c), p.goal) : null
  const made = r ? r.newTags.map((t) => tags.create(ws(c), t, "Steve")) : []
  return c.json(orch.createItem({ ws: ws(c), goal: p.goal, title: p.title ?? r?.title ?? p.goal.slice(0, 24), bot: p.bot ?? r?.bot, project: p.project !== undefined ? p.project : r?.project ?? null,
    tagIds: r ? [...r.tagIds, ...made.map((t) => t.id)] : [], source: ["手动新建", "manual"], mode: p.mode, actor: me(c) }))
})
api.get("/items/:id", (c) => {
  const i = own(c, items.get(c.req.param("id")), "事项")
  return c.json({ ...summary(i), goalsList: items.goals(i.id), planRounds: items.planRounds(i.id), timeline: timeline.list(i.id), artifacts: artifacts.list(i.id), decisions: decisions.ofItem(i.id), dependents: deps.dependents(i.id) })
})
api.post("/items/:id/actions", async (c) => { own(c, items.get(c.req.param("id")), "事项"); const { action } = await body(c, z.object({ action: z.enum(["pause", "resume", "drop", "reopen", "archive", "unarchive"]) })); return c.json(orch.itemAction(c.req.param("id"), action, me(c))) })
api.post("/items/bulk", async (c) => {
  const p = await body(c, z.object({ ids: z.array(z.string()).min(1).max(200), op: z.enum(["pause", "resume", "drop", "archive"]) }))
  let n = 0
  for (const id of p.ids) { const i = items.get(id); if (!i || i.workspace_id !== ws(c)) continue
    if (p.op === "pause" && i.status !== "run") continue; if (p.op === "resume" && i.status !== "pause") continue; if (p.op === "drop" && ["done", "drop"].includes(i.status)) continue
    orch.itemAction(id, p.op, me(c)); n++ }
  return c.json({ n })
})
api.post("/items/:id/goal", async (c) => { own(c, items.get(c.req.param("id")), "事项"); const { text } = await body(c, z.object({ text: z.string().min(1) })); orch.reviseGoal(c.req.param("id"), text); return c.json({ ok: true }) })
api.patch("/items/:id", async (c) => {
  const i = own(c, items.get(c.req.param("id")), "事项")
  const p = await body(c, z.object({ title: z.string().min(1).optional(), priority: z.enum(["高", "中", "低"]).optional(), due: z.string().nullable().optional(), star: z.boolean().optional(), accept_mode: z.enum(["inherit", "manual", "auto"]).optional(), project_id: z.string().nullable().optional(), tags: z.array(z.string()).optional() }))
  const { tags: t, ...rest } = p
  if (rest.project_id) own(c, projects.get(rest.project_id), "项目")
  if (rest.title && rest.title !== i.title) timeline.add(i.id, me(c), "sys", `标题改为「${rest.title}」`)
  if (rest.due !== undefined && rest.due !== i.due) timeline.add(i.id, me(c), "sys", rest.due ? "截止时间设为 " + rest.due : "移除截止时间")
  if (rest.project_id !== undefined && rest.project_id !== i.project_id) timeline.add(i.id, me(c), "sys", "项目改为 " + (projects.get(rest.project_id)?.name ?? "无") + "，下一个会话起生效")
  if (t) {
    const before = tags.ofItem(i.id), add = t.filter((x) => !before.includes(x)), rm = before.filter((x) => !t.includes(x))
    tags.setItem(i.id, t.filter((x) => tags.get(x)?.workspace_id === ws(c)))
    if (add.length) timeline.add(i.id, me(c), "me", "添加标签：" + add.map((x) => tags.get(x)?.name).join("、"))
    if (rm.length) timeline.add(i.id, me(c), "me", "移除标签：" + rm.map((x) => tags.get(x)?.name ?? x).join("、"))
  }
  /* a different project only applies from the next session: forget the old directory */
  return c.json(items.patch(i.id, { ...rest, ...(rest.project_id !== undefined && rest.project_id !== i.project_id ? { cwd: null, branch: null } : {}) }))
})
api.delete("/items/:id", (c) => { const i = own(c, items.get(c.req.param("id")), "事项"); if (!i.archived) return c.json({ error: "先归档，才能永久删除" }, 400); items.remove(i.id); audit(me(c), "item.delete", i.id, {}, i.workspace_id); return c.json({ ok: true }) })
api.post("/items/:id/supplement", async (c) => { own(c, items.get(c.req.param("id")), "事项"); const p = await body(c, z.object({ text: z.string().min(1), mode: z.enum(["auto", "steer", "new"]).default("auto"), files: Files })); await orch.supplement(c.req.param("id"), p.text, p.mode, att.refs(ws(c), p.files)); return c.json({ ok: true }) })
api.post("/items/:id/deliver", (c) => { own(c, items.get(c.req.param("id")), "事项"); orch.deliverQueued(c.req.param("id")); return c.json({ ok: true }) })
api.post("/items/:id/reassign", async (c) => { own(c, items.get(c.req.param("id")), "事项"); const { bot } = await body(c, z.object({ bot: z.string() })); return c.json(orch.reassign(c.req.param("id"), bot, me(c))) })
api.post("/items/:id/deps", async (c) => { own(c, items.get(c.req.param("id")), "事项"); const { dep } = await body(c, z.object({ dep: z.string() })); own(c, items.get(dep), "事项"); orch.addDep(c.req.param("id"), dep); return c.json({ ok: true }) })
api.delete("/items/:id/deps/:dep", (c) => { own(c, items.get(c.req.param("id")), "事项"); orch.rmDep(c.req.param("id"), c.req.param("dep")); return c.json({ ok: true }) })
api.post("/items/:id/snooze", async (c) => {
  const i = own(c, items.get(c.req.param("id")), "事项")
  const p = await body(c, z.object({ label: z.string().nullable(), until: z.number().nullable() }))
  items.patch(i.id, { snooze: p.label && p.until ? { label: p.label, until: p.until } : null })
  if (p.label) timeline.add(i.id, me(c), "me", "稍后提醒：" + p.label)
  return c.json({ ok: true })
})

/* ── sessions ── */
api.get("/sessions", (c) => c.json(sessions.list(ws(c)).map(sessOut)))
api.get("/sessions/search", (c) => c.json(sessions.search(ws(c), c.req.query("q") ?? "")))
api.get("/sessions/:id", (c) => c.json(sessOut(own(c, sessions.get(c.req.param("id")), "会话"))))
api.get("/sessions/:id/events", (c) => { own(c, sessions.get(c.req.param("id")), "会话"); return c.json(events.after(c.req.param("id"), Number(c.req.query("after") ?? 0)).map((e) => ({ ...e, data: evOut(e.data) }))) })
api.post("/sessions/:id/messages", async (c) => {
  const s = own(c, sessions.get(c.req.param("id")), "会话"); if ((s as any).readonly) return c.json({ error: "导入的会话是只读的" }, 400)
  const p = await body(c, z.object({ text: z.string().min(1), mode: z.enum(["插话", "排队"]).default("排队"), files: Files })); return c.json({ result: await orch.sessionMessage(s.id, p.text, p.mode, att.refs(ws(c), p.files)) })
})
api.get("/sessions/:id/resume", (c) => c.json(resumeInfo(own(c, sessions.get(c.req.param("id")), "会话") as any)))
api.post("/sessions/:id/cancel", (c) => { own(c, sessions.get(c.req.param("id")), "会话"); return c.json({ ok: acp.cancel(c.req.param("id")) }) })
api.post("/sessions/:id/end", (c) => { own(c, sessions.get(c.req.param("id")), "会话"); orch.endSession(c.req.param("id")); return c.json({ ok: true }) })
api.patch("/sessions/:id", async (c) => { own(c, sessions.get(c.req.param("id")), "会话"); const p = await body(c, z.object({ title: z.string().min(1).max(80) })); sessions.patch(c.req.param("id"), p); return c.json({ ok: true }) })
api.delete("/sessions/:id", (c) => { own(c, sessions.get(c.req.param("id")), "会话"); removeImported(c.req.param("id")); return c.json({ ok: true }) })
api.post("/sessions/:id/fork", async (c) => {
  const s = own(c, sessions.get(c.req.param("id")), "会话")
  const p = await body(c, z.object({ seq: z.number(), goal: z.string().default(""), asSession: z.boolean().default(false) }))
  return c.json(await orch.fork(ws(c), s.id, p.seq, p.goal, p.asSession, me(c)))
})
api.post("/sessions/:id/link", async (c) => {
  const s = own(c, sessions.get(c.req.param("id")), "会话"); const { item } = await body(c, z.object({ item: z.string() })); const i = own(c, items.get(item), "事项")
  sessions.patch(s.id, { item_id: i.id, no: sessions.ofItem(i.id).length + 1 }); timeline.add(i.id, me(c), "session", "关联了一个已有会话", s.id); return c.json({ ok: true })
})
api.delete("/queued/:id", (c) => { own(c, sessions.get(queued.get(c.req.param("id"))?.session_id ?? ""), "排队消息"); queued.drop(c.req.param("id")); return c.json({ ok: true }) })
api.get("/local-sessions", (c) => c.json(localSessions()))
api.post("/local-sessions/import", async (c) => { const { keys } = await body(c, z.object({ keys: z.array(z.string()).min(1).max(50) })); const out = keys.map((k) => { try { return importLocal(ws(c), k).id } catch (e) { return { error: (e as Error).message } } }); return c.json(out) })

/* ── decisions ── */
api.post("/decisions/:id", async (c) => { own(c, decisions.get(c.req.param("id")), "决定"); const p = await body(c, z.object({ choice: z.string(), text: z.string().optional() })); return c.json(orch.resolve(c.req.param("id"), p.choice, p.text ?? "", me(c))) })
api.post("/decisions/:id/dismiss", (c) => { own(c, decisions.get(c.req.param("id")), "决定"); return c.json(orch.dismissDecision(c.req.param("id"), me(c))) })
api.post("/decisions/:id/undo", (c) => { own(c, decisions.get(c.req.param("id")), "决定"); return c.json(orch.undoDecision(c.req.param("id"), me(c))) })

/* ── bots ── */
const BotPatch = z.object({ name: z.string().min(1).max(40).optional(), role: z.string().max(200).optional(), harness: z.enum(["claude-code", "codex"]).optional(), model: z.string().nullable().optional(), rules: z.string().max(8000).optional(), persona: z.string().max(400).optional(),
  reasoning_effort: z.enum(["none", "minimal", "low", "medium", "high", "xhigh", "max"]).nullable().optional(),
  proactive: z.number().int().min(0).max(2).optional(), accept_mode: z.enum(["inherit", "manual", "auto"]).optional(), perm_mode: z.enum(["default", "acceptEdits", "auto", "plan"]).optional(),
  avatar: z.object({ color: z.string().regex(/^#[0-9a-fA-F]{6}$/).optional(), skin: z.string().regex(/^\d{3,4}$/).nullable().optional(), round: z.number().min(0).max(100).optional() }).optional(),
  default_project: z.string().nullable().optional(), prompt: z.string().max(20000).nullable().optional(), device: z.string().nullable().optional(), enabled: z.boolean().optional() })
api.get("/bots/:id/prompt", (c) => { const b = bots.get(ownBot(c, c.req.param("id"))); if (!b) return c.json({ error: "没有这个 Bot" }, 404); return c.json({ generated: orch.botPrompt({ ...b, prompt: null }, ws(c)), custom: b.prompt }) })
api.post("/bots", async (c) => { const p = await body(c, BotPatch.extend({ name: z.string().min(1).max(40), harness: z.enum(["claude-code", "codex"]).default("claude-code") })); return c.json(botOut(bots.create(ws(c), p as any, me(c)), ws(c))) })
/* “描述给 Steve，让它起草”: the model fills the bot's fields from one sentence */
api.post("/bots/draft", async (c) => {
  const { desc } = await body(c, z.object({ desc: z.string().min(2).max(2000) }))
  const r = firstJson(await claudePrint(`Draft a bot for Steve (a personal assistant app) from this description. Reply in Chinese, ONLY JSON: {"name":"<= 6 chars","role":"one line","persona":"how it talks","rules":"working rules, a few lines","model":"opus|sonnet|haiku"}\nDescription: ${JSON.stringify(desc)}`, "smart", 90_000)) ?? {}
  const b = bots.create(ws(c), { name: String(r.name || "新 Bot").slice(0, 20), role: String(r.role ?? desc.slice(0, 40)), persona: String(r.persona ?? ""), rules: String(r.rules ?? desc), model: ({ opus: "claude-opus-5-5", sonnet: "claude-sonnet-5-5", haiku: "claude-haiku-5-5" } as Record<string, string>)[r.model] ?? null, harness: "claude-code", perm_mode: "default" }, me(c))
  return c.json(botOut(b, ws(c)))
})
api.patch("/bots/:id", async (c) => { ownBot(c, c.req.param("id")); const p = await body(c, BotPatch); if (p.default_project) own(c, projects.get(p.default_project), "项目"); return c.json(botOut(bots.patch(c.req.param("id"), p as any, me(c)), ws(c))) })
api.put("/bots/:id/workspaces", async (c) => {
  ownBot(c, c.req.param("id"))
  const { ids } = await body(c, z.object({ ids: z.array(z.string()).min(1) }))
  const mine = new Set(workspaces.ofUser(me(c)).map((w) => w.id)); if (ids.some((x) => !mine.has(x))) return c.json({ error: "没有这个工作空间" }, 404)
  bots.setWorkspaces(c.req.param("id"), ids); return c.json({ ok: true })
})
api.post("/bots/:id/duplicate", (c) => {
  const b = bots.get(ownBot(c, c.req.param("id"))); if (!b) return c.json({ error: "没有这个 Bot" }, 404)
  const n = bots.create(ws(c), { ...b, name: b.name + " 副本", enabled: false, main: false } as any, me(c)); bots.patch(n.id, { enabled: false }, me(c)); return c.json(botOut(bots.get(n.id), ws(c)))
})
api.delete("/bots/:id", async (c) => {
  const p = await body(c, z.object({ to: z.string(), memory: z.enum(["delete", "export"]).default("export") }))
  const id = ownBot(c, c.req.param("id")); ownBot(c, p.to); const mem = bots.workspaces(id).flatMap((w) => memories.list(id, w.workspace_id))
  const r = orch.deleteBot(id, p.to, me(c))
  if (p.memory === "delete") db.prepare("DELETE FROM bot_memories WHERE bot_id = ?").run(id)
  return c.json({ ...r, memories: p.memory === "export" ? mem.map((m) => ({ text: m.text, source: m.source, created: m.created })) : [] })
})
api.post("/bots/:id/restore", (c) => { bots.restore(ownBot(c, c.req.param("id"))); return c.json({ ok: true }) })
api.get("/bots/:id/memories", (c) => c.json(memories.list(ownBot(c, c.req.param("id")), ws(c))))
api.post("/bots/:id/memories", async (c) => { const { text } = await body(c, z.object({ text: z.string().min(1).max(2000) })); return c.json({ id: memories.add(ownBot(c, c.req.param("id")), ws(c), text, "你手动添加") }) })
api.patch("/memories/:id", async (c) => { const { text } = await body(c, z.object({ text: z.string().min(1).max(2000) })); own(c, memories.get(c.req.param("id")), "记忆"); memories.edit(c.req.param("id"), text); return c.json({ ok: true }) })
api.delete("/memories/:id", (c) => { own(c, memories.get(c.req.param("id")), "记忆"); memories.remove(c.req.param("id")); return c.json({ ok: true }) })
api.delete("/memories", (c) => { memories.clear(ws(c)); return c.json({ ok: true }) })

/* ── projects ── */
const expand = (p: string) => (p.startsWith("~/") ? join(homedir(), p.slice(2)) : p)
api.post("/projects", async (c) => {
  const p = await body(c, z.object({ name: z.string().min(1).max(40), path: z.string().min(1), description: z.string().max(1000).default(""), worktree: z.boolean().optional(), default_bot: z.string().nullable().default(null) }))
  if (p.default_bot) ownBot(c, p.default_bot)
  const path = expand(p.path); if (!existsSync(path) || !statSync(path).isDirectory()) return c.json({ error: `目录不存在：${p.path}` }, 400)
  if (projects.byPath(ws(c), path)) return c.json({ error: "这个目录已经是一个项目" }, 400)
  const git = existsSync(join(path, ".git"))
  const x = projects.create(ws(c), { name: p.name, description: p.description, device_id: "hub", path, kind: git ? "git" : "dir", worktree: git && (p.worktree ?? true), default_bot: p.default_bot, release: null })
  audit(me(c), "project.create", x.id, { path }, ws(c)); return c.json(x)
})
api.patch("/projects/:id", async (c) => {
  own(c, projects.get(c.req.param("id")), "项目")
  const p = await body(c, z.object({ name: z.string().min(1).max(40).optional(), description: z.string().max(1000).optional(), worktree: z.boolean().optional(), default_bot: z.string().nullable().optional(), archived: z.boolean().optional() }))
  if (p.default_bot) ownBot(c, p.default_bot)
  return c.json(projects.patch(c.req.param("id"), p, me(c)))
})
/* let Steve draft a description from what is in the directory */
api.post("/projects/describe", async (c) => {
  const { path: raw } = await body(c, z.object({ path: z.string().min(1) })), path = expand(raw)
  if (!existsSync(path)) return c.json({ error: `目录不存在：${raw}` }, 400)
  const list = readdirSync(path).filter((f) => !f.startsWith(".")).slice(0, 60).join(", ")
  const readme = ["README.md", "readme.md", "AGENTS.md", "package.json"].map((f) => join(path, f)).filter(existsSync).map((f) => basename(f) + ":\n" + readFileSync(f, "utf8").slice(0, 2500)).join("\n\n")
  const r = firstJson(await claudePrint(`Describe this project directory for an assistant that routes tasks to projects. Chinese, one or two sentences: what it is and what kind of work happens there. ONLY JSON {"name":"<= 12 chars","description":""}\nPath: ${path}\nFiles: ${list}\n\n${readme}`, "fast", 60_000)) ?? {}
  return c.json({ name: String(r.name ?? basename(path)), description: String(r.description ?? ""), git: existsSync(join(path, ".git")) })
})
api.get("/fs/dirs", (c) => {
  const base = expand(c.req.query("path") || "~/"); if (!existsSync(base)) return c.json([])
  return c.json(readdirSync(base, { withFileTypes: true }).filter((d) => d.isDirectory() && !d.name.startsWith(".")).slice(0, 200).map((d) => join(base, d.name).replace(homedir(), "~")))
})

/* ── tags ── */
const TagIn = z.object({ name: z.string().min(1).max(20), cat: z.enum(["项目", "类型", "领域", "客户", "主题"]), description: z.string().min(1).max(400) })
api.post("/tags", async (c) => { const p = await body(c, TagIn); if (tags.byName(ws(c), p.name)) return c.json({ error: "已有同名标签" }, 400); return c.json(tags.create(ws(c), p, "你")) })
api.patch("/tags/:id", async (c) => { own(c, tags.get(c.req.param("id")), "标签"); return c.json(tags.patch(c.req.param("id"), await body(c, TagIn.partial()))) })
api.delete("/tags/:id", (c) => { own(c, tags.get(c.req.param("id")), "标签"); tags.remove(c.req.param("id")); return c.json({ ok: true }) })
api.post("/tags/:id/merge", async (c) => { own(c, tags.get(c.req.param("id")), "标签"); const { into } = await body(c, z.object({ into: z.string() })); own(c, tags.get(into), "标签"); tags.merge(c.req.param("id"), into); return c.json({ ok: true }) })

/* ── settings and conversational configuration ── */
api.get("/settings/schema", (c) => c.json(changes.SETTINGS))
api.patch("/settings", async (c) => {
  const p = await body(c, z.record(z.string(), z.unknown()))
  const specs = Object.values(changes.SETTINGS).flat()
  for (const [k, v] of Object.entries(p)) { const s = specs.find((x) => x.key === k); if (!s || !s.values.includes(String(v))) return c.json({ error: `设置项不对：${k}` }, 400) }
  settings.set(ws(c), Object.fromEntries(Object.entries(p).map(([k, v]) => [k, String(v)])), me(c)); return c.json({ ok: true })
})
const TargetIn = z.object({ kind: z.enum(["bot", "project", "settings", "tags", "global"]), id: z.string().optional() })
api.post("/config/draft", async (c) => { const p = await body(c, z.object({ target: TargetIn, text: z.string().min(1).max(4000) })); return c.json(await changes.draft(ws(c), p.target, p.text)) })
api.post("/config/:id/apply", (c) => c.json(changes.apply(c.req.param("id"), me(c), ws(c))))
api.post("/config/:id/undo", (c) => c.json(changes.undo(c.req.param("id"), me(c), ws(c))))

/* ── devices ── */
api.get("/devices", (c) => c.json({ devices: devices.list(), runtimes: runtimes(c.req.query("fresh") === "1") }))
api.patch("/devices/:id", async (c) => { const p = await body(c, z.object({ name: z.string().min(1).max(60).optional(), status: z.enum(["正常", "暂停接活"]).optional(), max_sessions: z.number().int().min(1).max(16).optional() })); devices.patch(c.req.param("id"), p); return c.json({ ok: true }) })

/* one stream for everything the UI shows; clients refetch what changed. Session events only go to the session's workspace.
 * Each event is serialized (and slimmed) once, not once per client */
const wire = new WeakMap<object, string>()
const wired = (e: any) => { let x = wire.get(e); if (x === undefined) { x = JSON.stringify(e.type === "event" ? { ...e, data: evOut(e.data) } : e); wire.set(e, x) } return x }
api.get("/stream", (c) => {
  const w = ws(c)
  return streamSSE(c, async (s) => {
    const off = subscribe((e) => {
      if (e.type === "event" && sessions.get(e.session_id)?.workspace_id !== w) return
      if (e.type === "steve.thinking" && e.ws && e.ws !== w) return
      void s.writeSSE({ data: wired(e) })
    })
    const ping = setInterval(() => { void s.writeSSE({ event: "ping", data: "" }) }, 25_000)
    await new Promise<void>((r) => s.onAbort(() => r()))
    off(); clearInterval(ping)
  })
})
