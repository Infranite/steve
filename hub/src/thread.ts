/* The main conversation with Steve, one per workspace: every message goes through the router. */
import { messages, items, tags, decisions, bots, type Message, type Item, type FileRef } from "./repo.js"
import { publish } from "./bus.js"
import { route, answer } from "./gateway.js"
import { createItem, supplement } from "./orchestrator.js"
import { draft } from "./changes.js"

const REF = /^#?([A-Z]{1,6}-\d+)\b\s*/
const AT = /^@(\S+)\s*/
const inflight = new Map<string, AbortController>()

/** meta.steps on a Steve reply: the routing decision, the reasoning behind a direct answer, the tools Steve used; times in ms */
export type StepTool = { name: "create_item" | "draft_config" | "supplement" | "digest"; args: Record<string, unknown>; result?: Record<string, unknown>; status: "ok" | "failed"; error?: string; ms: number }
export type Steps = {
  route?: { kind: "chat" | "item" | "config"; title: string; bot: string; project: string | null; tags: string[]; newTags: string[]; why: string; model: string | null; fallback: boolean; ms: number; picked?: string }
  answer?: { thought: string; model: string | null; ms: number; failed: boolean }
  tools: StepTool[]; ms?: number
}
const phase = (ws: string, p: "route" | "answer" | "config" | "item" | "supplement") => publish({ type: "steve.thinking", on: true, ws, phase: p })

/* one message at a time per workspace, in the order they were sent */
const lanes = new Map<string, Promise<unknown>>()
export function say(ws: string, text: string, files: FileRef[] = []) {
  const mid = messages.add(ws, "me", text, null, files.length ? { files } : null)
  const run = (lanes.get(ws) ?? Promise.resolve()).then(() => handle(ws, mid, text, files)).catch(() => {})
  lanes.set(ws, run); return mid
}
async function handle(ws: string, mid: string, text: string, files: FileRef[] = []) {
  const ctl = new AbortController(); inflight.set(ws, ctl)
  publish({ type: "steve.thinking", on: true, ws })
  /* what Steve did for this message, kept on the reply as meta.steps so the conversation can show it folded */
  const t0 = Date.now(), steps: Steps = { tools: [] }
  const tool = async <T>(name: StepTool["name"], args: Record<string, unknown>, f: () => T | Promise<T>, result: (x: T) => Record<string, unknown> = () => ({})) => {
    const s0 = Date.now(), t: StepTool = { name, args, status: "ok", ms: 0 }; steps.tools.push(t)
    const p = ({ create_item: "item", draft_config: "config", supplement: "supplement" } as const)[name as "create_item"]; if (p) phase(ws, p)
    try { const x = await f(); t.result = result(x); return x } catch (e) { t.status = "failed"; t.error = (e as Error).message.slice(0, 200); throw e } finally { t.ms = Date.now() - s0 }
  }
  const reply = (t: string, item: string | null = null, meta: Record<string, unknown> | null = null) => { if (!ctl.signal.aborted) messages.add(ws, "steve", t, item, { ...meta, steps: { ...steps, ms: Date.now() - t0 } }) }
  try {
    if (text.trim() === "/汇报") { reply(await tool("digest", {}, () => digestText(ws))); return mid }
    const ref = text.match(REF)
    if (ref && items.get(ref[1])?.workspace_id === ws) {
      await tool("supplement", { item: ref[1], files: files.length }, async () => { await supplement(ref[1], text.replace(REF, "") || text, "auto", files) })
      reply(`已作为补充送到 ${ref[1]}。`, ref[1]); return mid
    }
    const at0 = messages.get(mid)!.created, history = messages.list(ws).filter((m) => m.id !== mid && m.created <= at0)
    /* the router only sees that there are attachments; the bot that does the work gets the files themselves */
    phase(ws, "route"); const r0 = Date.now()
    const r = await route(ws, files.length ? `${text}\n（附带 ${files.length} 个附件：${files.map((f) => `${f.name} · ${f.mime.split(";")[0]}`).join("、")}）` : text, history)
    if (ctl.signal.aborted) return mid
    steps.route = { kind: r.kind, title: r.title, bot: r.bot, project: r.project, tags: r.tagIds, newTags: r.newTags.map((t) => t.name), why: r.why ?? "", model: r.model ?? null, fallback: !!r.fallback, ms: Date.now() - r0 }
    /* “@名字 …” picks the bot explicitly */
    const at = text.match(AT)
    if (r.kind === "chat" && !at) {
      phase(ws, "answer"); const a0 = Date.now(), a = await answer(text, history, files)
      steps.answer = { thought: a.thought, model: a.model ?? null, ms: Date.now() - a0, failed: !!a.failed }
      reply(a.text, null, { chat: text, ...(files.length ? { chatFiles: files } : {}) }); return mid
    }
    if (r.kind === "config" && !at) {
      const p = await tool("draft_config", { request: text.slice(0, 200) }, () => draft(ws, { kind: "global" }, text), (p) => ({ target: p.title, changes: p.changes.length }))
      reply(p.changes.length ? `这是配置改动，我起草好了，确认后应用到「${p.title}」：` : `我没看出要改「${p.title}」的哪一项，换个说法试试？`, null, p.changes.length ? { proposal: p } : null); return mid
    }
    /* the composer writes “@名字” without spaces, so a bot called “Steve 开发” is “@Steve开发” */
    const named = at ? bots.list(ws).find((b) => b.enabled && [b.name, b.name + "Bot"].map((n) => n.replace(/\s+/g, "")).includes(at[1])) : undefined
    if (named && steps.route) steps.route.picked = named.id
    /* a follow-up like “按建议” becomes the proposal it confirms; the user's own words stay with it */
    const goal = named ? text.replace(AT, "") : r.goal.trim() && r.goal.trim() !== text.trim() ? `${r.goal.trim()}\n\n（用户原话：${text}）` : text
    const made = r.newTags.map((t) => tags.create(ws, t, "Steve"))
    const tagIds = [...new Set([...r.tagIds, ...made.map((t) => t.id)])]
    const item = await tool("create_item", { title: r.title, bot: named?.id ?? r.bot, project: r.project, tags: tagIds, files: files.length },
      () => createItem({ ws, goal, title: r.title, bot: named?.id ?? r.bot, project: r.project, source: ["对话", "chat"], tagIds, files }), (i) => ({ item: i.id, bot: i.bot_id, project: i.project_id }))
    const tagText = tagIds.length ? ` 标签：${tagIds.map((id) => (tags.get(id)?.name ?? "") + (made.some((m) => m.id === id) ? "（新建）" : "")).join("、")}。` : ""
    reply((named ? `按你指定的，交给${named.name} Bot 了。` : r.reply || "好，交给它了，做完请你验收。") + tagText, item.id)
  } catch (e) { reply(`（出错了：${(e as Error).message.slice(0, 120)}）`) }
  finally { inflight.delete(ws); publish({ type: "steve.thinking", on: false, ws }) }
  return mid
}
/** stop the reply that is being prepared; the user's message stays */
export function stop(ws: string) {
  const c = inflight.get(ws); if (!c) return false
  c.abort(); messages.add(ws, "steve", "好，停下了。要我换个方式就直接说。"); return true
}
/** turn a chat answer into an item, with the conversation as context */
export function promote(ws: string, msgId: string): Promise<Item> {
  const m = messages.get(msgId) as Message | undefined
  if (!m || m.workspace_id !== ws) return Promise.reject(new Error("这条消息不能转成事项"))
  /* a message becomes at most one item: clicking again (or from another tab) returns the one already made */
  const done = m.meta?.promoted ? items.get(m.meta.promoted) : undefined
  if (done) return Promise.resolve(done)
  const busy = promoting.get(msgId); if (busy) return busy
  const q = m.meta?.chat; if (!q) return Promise.reject(new Error("这条消息不能转成事项"))
  /* routing takes seconds: a second click meanwhile waits for the same item */
  const run = (async () => {
    /* the conversation up to Steve's answer decides goal and title, not the bare question */
    const history = messages.list(ws).filter((x) => x.created <= m.created && x.id !== msgId).slice(-4)
    const t0 = Date.now(), r = await route(ws, q, [...history, { author: "steve", text: m.text }], { promote: true }), t1 = Date.now()
    const item = createItem({ ws, goal: r.goal.trim() || q, title: r.title, bot: r.bot, project: r.project, source: ["对话", "chat"], tagIds: r.tagIds, opening: `之前在对话里 Steve 的回答：${m.text.slice(0, 2000)}`, files: m.meta?.chatFiles ?? [] })
    messages.patchMeta(msgId, { ...m.meta, chat: undefined, promoted: item.id })
    const steps: Steps = { route: { kind: "item", title: r.title, bot: r.bot, project: r.project, tags: r.tagIds, newTags: [], why: r.why ?? "", model: r.model ?? null, fallback: !!r.fallback, ms: t1 - t0 },
      tools: [{ name: "create_item", args: { title: r.title, bot: r.bot, project: r.project, tags: r.tagIds }, result: { item: item.id, bot: item.bot_id, project: item.project_id }, status: "ok", ms: Date.now() - t1 }], ms: Date.now() - t0 }
    messages.add(ws, "steve", `好，转成事项交给它了，对话内容作为上下文带入。`, item.id, { steps })
    return item
  })().finally(() => promoting.delete(msgId))
  promoting.set(msgId, run); return run
}
const promoting = new Map<string, Promise<Item>>()
export function digestText(ws: string) {
  const all = items.list(ws), open = decisions.open(ws)
  const today = new Date(); today.setHours(0, 0, 0, 0)
  return `现在：${new Set(open.map((d) => d.item_id)).size} 件等你，${all.filter((i) => i.status === "run").length} 件在进行，今天完成 ${all.filter((i) => i.status === "done" && i.updated >= today.getTime()).length} 件。`
}
