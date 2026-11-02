/* Model gateway: Steve's own short tasks. Default path is the harness CLI in print mode, so no API key is needed.
 * Every call has a timeout and a deterministic fallback; nothing here may block the conversation. */
import { execFile } from "node:child_process"
import { config } from "./config.js"
import { bots, projects, tags, type FileRef } from "./repo.js"
import { forAgent } from "./files.js"

function printOnce(prompt: string, model: string, timeoutMs: number, extra: string[] = []): Promise<string> {
  return new Promise((res, rej) => {
    execFile(config.claudeExecutable, ["-p", prompt, "--output-format", "json", "--model", model, ...extra], { timeout: timeoutMs, maxBuffer: 4 << 20, cwd: config.scratch }, (err, stdout) => {
      let d: any; try { d = JSON.parse(stdout) } catch { return rej(err ?? new Error("harness 没有返回结果")) }
      if (d.is_error) return rej(new Error(String(d.result).slice(0, 300)))
      res(String(d.result ?? ""))
    })
  })
}
/** "fast" is for routing and other short calls, "smart" for answers and drafts; each tier falls back down its model list */
export async function claudePrint(prompt: string, tier: "fast" | "smart", timeoutMs: number, extra: string[] = []): Promise<string> { return (await claudePrintWith(prompt, tier, timeoutMs, extra)).text }
/** like claudePrint, and says which model of the tier answered */
export async function claudePrintWith(prompt: string, tier: "fast" | "smart", timeoutMs: number, extra: string[] = []): Promise<{ text: string; model: string }> {
  let last: Error | null = null
  for (const m of config.models[tier]) {
    try { return { text: await printOnce(prompt, m, timeoutMs, extra), model: m } } catch (e) { last = e as Error; console.error(`model ${m} failed: ${last.message.slice(0, 200)}`) }
  }
  throw last ?? new Error("没有可用的模型")
}
export const firstJson = (t: string) => { const m = t.match(/\{[\s\S]*\}/); return m ? JSON.parse(m[0]) : null }

export type NewTag = { name: string; cat: string; description: string }
/** why: the router's one-sentence reason; model: who routed; fallback: the router failed and this is the default */
export type Route = { kind: "chat" | "item" | "config"; title: string; goal: string; bot: string; project: string | null; tagIds: string[]; newTags: NewTag[]; reply: string; why?: string; model?: string; fallback?: boolean }
export type Said = { author: string; text: string }
export const TAG_CATS = ["项目", "类型", "领域", "客户", "主题"]
/** history: the last messages of the thread before this one (with Steve's last reply), so follow-ups like “按建议” have their context;
 * promote: the user turned Steve's last answer into an item */
export async function route(ws: string, text: string, history: Said[] = [], opts: { promote?: boolean } = {}): Promise<Route> {
  const bs = bots.list(ws).filter((b) => b.enabled), ps = projects.list(ws), ts = tags.list(ws)
  const steve = ps.find((p) => p.path === config.steveRepo)
  const recent = history.slice(-6).map((m) => `  ${m.author === "me" ? "User" : "Steve"}: ${JSON.stringify(m.text.length > 800 ? m.text.slice(0, 800) + "…" : m.text)}`)
  const prompt = [
    "You are the router of Steve, a personal assistant. Decide what to do with the user's message.",
    '- "chat": a question or small talk Steve can answer directly, without working in files, code or accounts.',
    '- "item": a task that needs work (code, files, research, writing, operations). It becomes a tracked item.',
    '- "config": the user wants to change a standing setting of Steve, a bot, a project or tags — a preference that applies from now on, not a piece of work. Examples: "以后默认自动验收", "完成 7 天后自动归档", "把开发 Bot 改用 opus", "免打扰改成 22 点到 9 点", "加一个叫财务的标签", "Steve 说话再简洁一点".',
    "Bots (id: role):", ...bs.map((b) => `  ${b.id}: ${b.role}`),
    'Pick "steve" unless the task clearly needs a specialised bot.',
    "Projects (id: name — description). Pick the project whose directory the work happens in, or null if none fits:", ...ps.map((p) => `  ${p.id}: ${p.name} — ${p.description}`),
    steve ? `Anything about Steve itself — its chat page, web UI, cards, Hub, bots, releases, design — is work in project ${steve.id}.` : "",
    "Tags (id: name · category · description). Choose existing tags that fit; only if none of the non-project tags fits, propose ONE new tag with category in " + TAG_CATS.join("/") + " and a concrete description:",
    ...ts.map((t) => `  ${t.id}: ${t.name} · ${t.cat} · ${t.description}`),
    ...(recent.length ? ["Recent conversation, oldest first:", ...recent] : []),
    `User message: ${JSON.stringify(text)}`,
    opts.promote ? 'The user pressed “转成事项” on Steve\'s last answer: kind is "item"; goal is the work this conversation is about, drawn from the question and Steve\'s answer.'
      : 'If the message only confirms or picks up something Steve proposed in its last reply (e.g. “按建议”, “就这么办”, “好，做吧”, “可以”, “第二个”), it is about that proposal: when the proposal is work, kind is "item" and goal restates the proposed work completely from Steve\'s reply.',
    'goal: the task for the bot, self-contained, in the user language (for an ordinary message, the message itself). title: names the work concretely, never just “按建议” or “好的”.',
    'Reply with ONLY one JSON object: {"kind":"chat"|"item"|"config","title":"<= 24 chars, user language","goal":"","bot":"<bot id>","project":"<project id or null>","tags":["<tag id>"],"newTags":[{"name":"","cat":"","description":""}],"reply":"one short sentence in the user language","why":"one short sentence in the user language: why this kind, bot and project"}',
  ].filter(Boolean).join("\n")
  try {
    const { text: raw, model } = await claudePrintWith(prompt, "fast", 60_000)
    if (process.env.STEVE_DEBUG_ROUTE) console.error("route raw:", raw)
    const r = firstJson(raw)
    if (r && ["chat", "item", "config"].includes(r.kind)) {
      const project = ps.some((p) => p.id === r.project) ? r.project : null
      /* work in a project goes to the project's own bot unless a specialised one was picked */
      const own = ps.find((p) => p.id === project)?.default_bot, ownOk = !!own && bs.some((b) => b.id === own)
      const bot = bs.some((b) => b.id === r.bot) && !(r.bot === "steve" && ownOk) ? r.bot : ownOk ? own! : "steve"
      return { kind: opts.promote ? "item" : r.kind, title: String(r.title || text).slice(0, 40), goal: String(r.goal || text), bot, project, tagIds: (r.tags ?? []).filter((x: string) => ts.some((t) => t.id === x)),
        newTags: (r.newTags ?? []).filter((t: any) => t?.name && TAG_CATS.includes(t.cat) && t.description).slice(0, 1), reply: String(r.reply ?? ""), why: String(r.why ?? "").slice(0, 200), model }
    }
  } catch (e) { console.error("route failed", (e as Error).message) }
  return { kind: "item", title: text.slice(0, 24), goal: text, bot: "steve", project: null, tagIds: [], newTags: [], reply: "", why: "没能判断，默认建成事项交给 Steve", fallback: true }
}

/** a direct answer, with a short account of how Steve got there (shown folded above the answer) */
export type Answer = { text: string; thought: string; model?: string; failed?: boolean }
export async function answer(text: string, history: { author: string; text: string }[], files: FileRef[] = []): Promise<Answer> {
  const ctx = history.slice(-10).map((m) => `${m.author === "me" ? "User" : "Steve"}: ${m.text}`).join("\n")
  const main = bots.main()
  /* attachments are copied next to the print-mode run (its cwd is the scratch dir) and read with the Read tool only */
  let att = "", extra: string[] = []
  if (files.length) { try { att = forAgent(config.scratch, files, false).note; extra = ["--allowedTools", "Read"] } catch (e) { att = `\n（附件没能读取：${(e as Error).message}）` } }
  const prompt = `You are ${main.name}, the user's assistant. ${main.persona} ${main.rules}\nAnswer in the user's language, briefly. Your reply is shown with a “转成事项” button that turns this conversation into a tracked item handed to a bot, and the user can also just reply “按建议” to a proposal of yours. So never say you can't create items or start work: when the request needs real work, say briefly what you would do and that they can press 转成事项 or confirm. You don't change settings or start work from this reply yourself, so never claim you already did.${files.length ? " Read the attached files with the Read tool before answering." : ""}\n` +
    `Reply with ONLY one JSON object: {"thought":"2-4 short sentences in the user's language: what the user is asking and how you approach the answer","answer":"the answer itself (markdown allowed)"}\n\nRecent conversation:\n${ctx}\n\nUser: ${text}${att}`
  try {
    const { text: raw, model } = await claudePrintWith(prompt, "smart", files.length ? 150_000 : 90_000, extra)
    let r: any = null; try { r = firstJson(raw) } catch {}
    if (r && typeof r.answer === "string" && r.answer.trim()) return { text: r.answer.trim(), thought: String(r.thought ?? "").trim().slice(0, 1200), model }
    return { text: raw.trim(), thought: "", model }
  } catch (e) { return { text: `（回答失败：${(e as Error).message.slice(0, 80)}）`, thought: "", failed: true } }
}

/** conversational configuration: the model maps a request onto a target's fields; values are validated by the caller */
export type FieldSpec = { key: string; label: string; type: "text" | "enum" | "number"; values?: string[]; current: unknown }
export async function draftFields(target: string, fields: FieldSpec[], request: string): Promise<{ key: string; value: unknown }[]> {
  const prompt = [
    `You configure "${target}" in Steve, a personal assistant app. Map the user's request onto these fields. Change only what the request asks for.`,
    "Fields (key · label · type · allowed values · current value):",
    ...fields.map((f) => `  ${f.key} · ${f.label} · ${f.type}${f.values ? " · " + f.values.join("|") : ""} · ${JSON.stringify(f.current)}`),
    "For free-text fields, write the full new value in the user's language (append to the current text if the request adds a rule).",
    `Request: ${JSON.stringify(request)}`,
    'Reply with ONLY JSON: {"changes":[{"key":"","value":...}]}',
  ].join("\n")
  const r = firstJson(await claudePrint(prompt, "smart", 90_000))
  return Array.isArray(r?.changes) ? r.changes.filter((c: any) => fields.some((f) => f.key === c.key)) : []
}
