/* Orchestrator: items ↔ sessions ↔ decisions. The only place that knows the item lifecycle. */
import { execFileSync, spawn } from "node:child_process"
import { existsSync, mkdirSync, openSync, readFileSync, writeFileSync, watchFile } from "node:fs"
import { join } from "node:path"
import { homedir } from "node:os"
import { randomBytes } from "node:crypto"
import { config } from "./config.js"
import { WS, items, sessions, decisions, timeline, artifacts, bots, events, messages, audit, links, queued, projects, settings, deps, memories, tags, type Item, type Session, type Decision, type Bot, type Parent, type PlanStep, type PlanRound } from "./repo.js"
import * as acp from "./acp.js"
import { db } from "./db.js"
import { sha } from "./repo.js"
import { setToolActions } from "./mcp.js"
import type { FileRef } from "./repo.js"
import { settle, supersede, depsOpen, mayResume, tidy, rebaseOnto, tidyWorktree } from "./lifecycle.js"
import { notify } from "./notify.js"

/* ── seed: the personal workspace, Steve's own project, the main bot and the developer bot ── */
export const STEVE_REPO = process.env.STEVE_REPO ?? join(homedir(), "Agents/steve")
export function seed() {
  bots.seed({ id: "steve", name: "Steve", main: true, role: "你的助手：理解需求、分派、汇报", harness: "claude-code", accept_mode: "inherit", perm_mode: "default", persona: "沉稳、简洁，先说结论；不客套", avatar: { color: "#7aa7c7", round: 70 },
    rules: "能直接回答的就回答；是一件事就建事项交给合适的 Bot。只在有新结果或需要决定时打扰。" }, WS)
  let steveProject = projects.byPath(WS, STEVE_REPO)
  if (!steveProject && existsSync(join(STEVE_REPO, ".git")))
    steveProject = projects.create(WS, { name: "Steve", description: "Steve 自己的代码：Hub（TypeScript）、Web 界面、发布与运维脚本", device_id: "hub", path: STEVE_REPO, kind: "git", worktree: true, default_bot: "dev",
      release: { kind: "script", cmd: join(config.home, "current/ops/release.sh") } })
  bots.seed({ id: "dev", name: "Steve 开发", role: "开发 Steve 自己：Hub、前端、部署脚本", harness: "claude-code", accept_mode: "inherit", perm_mode: "auto", persona: "直接、话少", default_project: steveProject?.id ?? null, avatar: { color: "#8c7ae6", round: 45 },
    rules: "在 git worktree 的分支上工作，不直接改 main。改完必须跑类型检查和测试；界面改动要截图自查。提交信息用中文说明改了什么。不推送、不发布，除非用 steve_request_approval 得到批准。" }, WS)
  /* bots seeded before projects existed: give them their project and a face once */
  if (steveProject) db.prepare("UPDATE bots SET default_project = ? WHERE id = 'dev' AND default_project IS NULL").run(steveProject.id)
  db.prepare(`UPDATE bots SET avatar = '{"color":"#7aa7c7","round":70}' WHERE id = 'steve' AND avatar = '{}'`).run()
  db.prepare(`UPDATE bots SET avatar = '{"color":"#8c7ae6","round":45}' WHERE id = 'dev' AND avatar = '{}'`).run()
}

const PRO = ["只在必要时", "适中", "积极"]
const acceptOf = (i: Item) => {
  if (i.accept_mode && i.accept_mode !== "inherit") return i.accept_mode
  const b = i.bot_id ? bots.get(i.bot_id) : undefined
  if (b?.accept_mode && b.accept_mode !== "inherit") return b.accept_mode
  return settings.get(i.workspace_id, "accDefault", "manual")
}
/** the bot's own part of the prompt: written by the user, or generated from its settings */
export function botPrompt(b: Bot, ws: string) {
  const mem = memories.list(b.id, ws)
  const memText = mem.length ? "\n你记得的事：\n" + mem.map((m) => "- " + m.text).join("\n") : ""
  if (b.prompt) return b.prompt + memText
  return [`你是 ${b.main ? b.name + "，用户唯一直接打交道的助手" : b.name + " Bot，由 Steve 调用"}。`, `职责：${b.role || "（未填写）"}`, b.persona ? `说话方式：${b.persona}` : "", `主动程度：${PRO[b.proactive] ?? "适中"}`,
    b.rules ? `工作规范：\n${b.rules}` : ""].filter(Boolean).join("\n") + memText
}

/* ── the working protocol every session starts with ── */
function protocol(item: Item, bot: Bot, s: Session) {
  const p = projects.get(item.project_id)
  const known = p ? [] : projects.list(item.workspace_id)
  return [
    `You are working for Steve, the user's assistant, on item ${item.id}: “${item.title}”.`,
    `Goal (from the user): ${items.goal(item.id)}`,
    p ? `Project: ${p.name} — ${p.description || "no description"}.` : "This item has no project; you are in a scratch directory made for it.",
    ...(known.length ? ["The user's projects (name — path — description):", ...known.map((x) => `- ${x.name} — ${x.path} — ${x.description || "no description"}`),
      "If the work belongs to one of them, do not search the disk for code: use steve_ask to ask the user to attach it, with the project names as options (and “不需要”). When the user picks one, Steve moves the item into that project and a new session continues there."] : []),
    `Working directory: ${s.cwd}${item.branch ? ` (git worktree on branch ${item.branch}; commit your work there, do not touch main)` : ""}.`,
    "", botPrompt(bot, item.workspace_id), "",
    "How to work with Steve — use the steve MCP tools:",
    "- steve_update: call it first with a short plan, and again whenever your step or plan changes. Write `now` in Chinese.",
    "- steve_ask: when you need a decision from the user. Never ask questions in plain text. After calling it, end your turn.",
    "- steve_request_approval: before anything with outside effects (deploy, release, push, send, pay, delete). After calling it, end your turn.",
    "- steve_deliver: when the goal is met, with a Chinese summary, the outputs (branch, commits, files) and the checks you actually ran. Tick off finished plan steps with steve_update first; list in `unfinished` the steps you deliberately left open. A delivery with other open steps is refused. Then end your turn.",
    p?.release && item.branch ? "- steve_publish: only after the user has accepted the item and asked for it to go live. Commit everything on your branch first. Then end your turn." : "",
    "Do the work yourself; do not stop to ask for confirmation you don't need. Reply to the user in Chinese.",
  ].filter((x) => x !== "").join("\n")
}

/* ── items and sessions ── */
const isGit = (dir: string) => { try { execFileSync("git", ["-C", dir, "rev-parse", "--is-inside-work-tree"], { stdio: "pipe" }); return true } catch { return false } }
function prepareCwd(item: Item): { cwd: string; branch: string | null } {
  const p = projects.get(item.project_id)
  if (!p) { const dir = join(config.scratch, item.id); mkdirSync(dir, { recursive: true }); return { cwd: dir, branch: null } }
  if (!existsSync(p.path)) throw new Error(`项目目录不存在：${p.path}`)
  if (p.kind !== "git" || !p.worktree || !isGit(p.path)) return { cwd: p.path, branch: null }
  const branch = `steve/${item.id.toLowerCase()}`, path = join(config.worktrees, item.id)
  if (!existsSync(path)) {
    const has = (() => { try { execFileSync("git", ["-C", p.path, "rev-parse", "--verify", branch], { stdio: "pipe" }); return true } catch { return false } })()
    execFileSync("git", ["-C", p.path, "worktree", "add", ...(has ? [path, branch] : ["-b", branch, path, "HEAD"])], { stdio: "pipe" })
  }
  return { cwd: path, branch }
}

export type NewItem = { ws: string; goal: string; title: string; bot?: string | null; project?: string | null; source?: [string, string]; parent?: Parent | null; tagIds?: string[]; mode?: "start" | "propose"; opening?: string; actor?: string; files?: FileRef[] }
export function createItem(n: NewItem) {
  const ws = n.ws
  const bot = (n.bot && bots.get(n.bot) && bots.inWorkspace(n.bot, ws) ? bots.get(n.bot) : null) ?? bots.main()
  const project = n.project !== undefined ? (n.project ? projects.get(n.project) : null) : (bot.default_project ? projects.get(bot.default_project) : null)
  const propose = n.mode === "propose"
  const item = items.create(ws, { title: n.title, goal: n.goal, bot_id: bot.id, project_id: project?.workspace_id === ws ? project.id : null, source: n.source, parent: n.parent ?? null, status: propose ? "wait" : "run", stage: propose ? "规划" : "定义" })
  if (n.tagIds?.length) tags.setItem(item.id, n.tagIds)
  timeline.add(item.id, "steve", "sys", `Steve 建立事项，交给${bot.main ? bot.name : bot.name + " Bot"}${item.project_id ? `，项目 ${project!.name}` : ""}`)
  audit(n.actor ?? "steve", "item.create", item.id, { bot: bot.id, project: item.project_id, source: n.source }, ws)
  if (propose) {
    decisions.create({ workspace_id: ws, kind: "propose", subject_type: "item", subject_id: item.id, item_id: item.id, question: "先给你方案：按这个方向做？", draft: n.goal, options: ["yes", "no"], requested_by: "steve" })
    items.patch(item.id, { now: "等你确认后开始" })
  } else startSession(item.id, n.opening, undefined, n.files).catch((e) => fail(item.id, `开不了会话：${e.message}`))
  return items.get(item.id)
}

export async function startSession(itemId: string, opening?: string, botOverride?: string, files: FileRef[] = []) {
  let item = items.get(itemId)
  if (botOverride) items.patch(itemId, { bot_id: botOverride })
  item = items.get(itemId)
  const bot = bots.get(item.bot_id ?? "steve") ?? bots.main()
  const { cwd, branch } = item.cwd && existsSync(item.cwd) ? { cwd: item.cwd, branch: item.branch } : prepareCwd(item)
  items.patch(itemId, { cwd, branch, stage: "执行", status: "run", now: "正在开始" })
  const token = randomBytes(24).toString("base64url")
  const s = sessions.create({ workspace_id: item.workspace_id, item_id: itemId, bot_id: bot.id, harness: bot.harness, model: bot.model ?? config.defaultModels[bot.harness] ?? null, reasoning_effort: bot.reasoning_effort, cwd, title: item.title, token })
  tokens.set(s.id, token)
  timeline.add(itemId, "steve", "session", `开始会话 #${s.no}`, s.id)
  const head = protocol(items.get(itemId), bot, s)
  await acp.prompt(s.id, opening ? `${head}\n\n${opening}` : head, "steve", files)
  return s
}
/* session tokens live only in memory: after a Hub restart each live session gets a fresh token on its next start */
const tokens = new Map<string, string>()
export function mcpEndpoint(s: Session) {
  let t = tokens.get(s.id)
  if (!t) { t = randomBytes(24).toString("base64url"); tokens.set(s.id, t); rotateToken(s.id, t) }
  return { url: `http://127.0.0.1:${config.listen.port}/mcp`, token: t }
}
const rotateToken = (id: string, t: string) => db.prepare("UPDATE sessions SET token_hash = ? WHERE id = ?").run(sha(t), id)

const activeSession = (itemId: string) => sessions.ofItem(itemId).filter((s) => s.status !== "ended").at(-1)

function fail(itemId: string, why: string) {
  const i = items.get(itemId)
  const opts = ["continue", "delivered", "look", ...(i.bot_id !== "steve" ? ["tosteve"] : [])]
  decisions.create({ workspace_id: i.workspace_id, kind: "fail", subject_type: "item", subject_id: itemId, item_id: itemId, question: why, options: opts, requested_by: "steve" })
  timeline.add(itemId, "steve", "sys", why); settle(itemId)
}

/* ── Steve tools ── */
const pending = new Map<string, "asked" | "delivered" | "approval">()
setToolActions({
  update(s, now, plan) {
    if (!s.item_id) return
    supersede(s.item_id, ["fail"], "Bot 继续工作了")
    const i = items.get(s.item_id), before = i.plan, by = s.bot_id ?? "agent"
    plan = plan.map((p) => ({ step: p.step, done: p.done }))
    /* a plan that drops steps of the one before starts a new round: the old one is kept, not overwritten */
    const turn = planTurn(before, plan)
    if (turn) items.addPlanRound(i.workspace_id, i.id, before, turn, by)
    items.patch(i.id, { now, plan })
    const note = turn === "done" ? "上一轮计划已完成，开始新一轮计划" : turn ? `换了一版计划，上一版没做完的 ${before.filter((p) => !p.done).length} 步已废弃`
      : JSON.stringify(before.map((p) => p.step)) !== JSON.stringify(plan.map((p) => p.step)) ? "更新了计划" : ""
    if (note) timeline.add(i.id, by, "sys", note, s.id)
    /* a finished item whose plan has open steps again is not finished */
    if (i.status === "done" && plan.some((p) => !p.done)) { items.patch(i.id, { status: "run", stage: "执行" }); timeline.add(i.id, "steve", "sys", "计划里又有没做完的步骤，事项回到进行中") }
  },
  ask(s, question, options) {
    /* a session without an item (forked, imported and continued, or its item was deleted) still gets an answerable card */
    if (!s.item_id) { decisions.create({ workspace_id: s.workspace_id, kind: "ask", subject_type: "question", subject_id: s.id, item_id: null, session_id: s.id, question, options, requested_by: s.bot_id ?? "agent" }); pending.set(s.id, "asked"); return }
    supersede(s.item_id, ["ask", "fail"], "Bot 提了新的问题")
    decisions.create({ workspace_id: s.workspace_id, kind: "ask", subject_type: "question", subject_id: s.id, item_id: s.item_id, session_id: s.id, question, options, requested_by: s.bot_id ?? "agent" })
    items.patch(s.item_id, { now: "在等你回答" }); pending.set(s.id, "asked"); settle(s.item_id)
  },
  requestApproval(s, action, detail) {
    if (!s.item_id) { decisions.create({ workspace_id: s.workspace_id, kind: "approve", subject_type: "action", subject_id: s.id, item_id: null, session_id: s.id, question: action, draft: detail, options: ["yes", "no"], requested_by: s.bot_id ?? "agent" }); pending.set(s.id, "approval"); return }
    supersede(s.item_id, ["ask", "fail"], "Bot 继续往下做了")
    decisions.create({ workspace_id: s.workspace_id, kind: "approve", subject_type: "action", subject_id: s.id, item_id: s.item_id, session_id: s.id, question: action, draft: detail, options: ["yes", "no"], requested_by: s.bot_id ?? "agent" })
    items.patch(s.item_id, { now: "在等你批准" }); pending.set(s.id, "approval"); settle(s.item_id)
  },
  deliver(s, summary, outputs, checks, unfinished = []) {
    if (!s.item_id) return
    /* every plan step is ticked off or declared left open before anything is delivered */
    const item = items.get(s.item_id), open = item.plan.filter((p) => !p.done && !unfinished.includes(p.step)).map((p) => p.step)
    if (open.length) return `Not delivered: these plan steps are not done yet: ${open.join("; ")}. Finish them and tick them off with steve_update, or list the steps you deliberately leave open in \`unfinished\`.`
    /* a new delivery also replaces an older one still waiting for acceptance */
    supersede(s.item_id, ["ask", "fail", "accept"], "Bot 已经交付")
    const left = planNote(item.plan)
    if (left) items.patch(item.id, { plan: item.plan.map((p) => ({ ...p, left: !p.done })) })
    for (const o of outputs) artifacts.add(item.id, o.kind, o.title, o.location ?? null)
    const checkText = checks.length ? "\n\n检查：" + checks.map((c) => `${c.passed ? "✓" : "✗"} ${c.name}`).join("；") : ""
    timeline.add(item.id, s.bot_id ?? "agent", "sys", "交付，等你验收", s.id)
    /* steps left open are the user's call, never auto-accepted */
    const auto = acceptOf(item) === "auto" && checks.every((c) => c.passed) && !left
    pending.set(s.id, "delivered")
    if (auto) {
      timeline.add(item.id, "steve", "sys", "自检全部通过，自动验收")
      if (releasePending(item)) awaitRelease(item.id, (m) => void acp.prompt(s.id, m, "steve")); else finish(item.id, "已自动验收")
      return
    }
    decisions.create({ workspace_id: item.workspace_id, kind: "accept", subject_type: "item", subject_id: item.id, item_id: item.id, session_id: s.id, question: "做完了，请验收", draft: summary + checkText + left, options: ["pass", "rework"], requested_by: s.bot_id ?? "agent" })
    items.patch(item.id, { stage: "交付与验收", now: "交付了，等你验收" }); settle(item.id)
  },
  publish(s, branch, summary) {
    if (!s.item_id) return
    const item = items.get(s.item_id), p = projects.get(item.project_id)
    if (!p?.release) { events.add(s.id, "system", { text: "这个项目没有配置发布动作，不能发布。" }); return }
    decisions.create({ workspace_id: s.workspace_id, kind: "approve", subject_type: "release", subject_id: branch, item_id: s.item_id, session_id: s.id, question: `发布新版本（${p.name} · 分支 ${branch}）`, draft: summary + "\n\n只改了前端时不重启，切到新前端后刷新页面即可；改到 Hub 时 Steve 会重启一次，大约一分钟。检查不通过会自动回到当前版本。", options: ["yes", "no"], requested_by: s.bot_id ?? "agent" })
    items.patch(s.item_id, { now: "等你批准发布" }); pending.set(s.id, "approval"); settle(s.item_id)
  },
})

/* a turn ended: if the agent neither asked, delivered nor is waiting, it stalled — tell the user instead of leaving it silent */
acp.setTurnEndHandler(({ session, stopReason, error }) => {
  if (!session.item_id) { pending.delete(session.id); return }
  const mark = pending.get(session.id); pending.delete(session.id)
  const wasResume = resumed.delete(session.id)
  const item = items.get(session.item_id)
  if (!item || ["done", "drop", "pause"].includes(item.status)) return
  if (wasResume && stopReason === "error") { fail(item.id, `${INTERRUPTED}；自动继续没有成功：${error ?? "会话出错"}`); return }
  if (mark) { settle(item.id); return }
  if (stopReason === "cancelled") { items.patch(item.id, { now: "你中断了这一回合" }); return }
  const last = lastAgentText(session.id)
  fail(item.id, error ? `会话出错：${error}` : `Agent 停下来了，没有交付也没有提问${last ? "：" + last.slice(0, 160) : ""}`)
})
export function lastAgentText(sessionId: string) {
  const evs = events.after(sessionId, 0).filter((e) => e.kind === "agent_message_chunk")
  const tail: string[] = []
  for (let i = evs.length - 1; i >= 0 && tail.join("").length < 400; i--) tail.unshift(evs[i].data?.content?.text ?? "")
  return tail.join("").trim()
}

/* ── finishing and prerequisites ── */
function finish(itemId: string, now: string) {
  const i = items.get(itemId)
  /* finishing never ticks steps off: what was not done stays visible as left open */
  const fresh = i.plan.filter((p) => !p.done && !p.left).map((p) => p.step)
  if (fresh.length) timeline.add(itemId, "steve", "sys", `完成时计划里还有 ${fresh.length} 步没做：${fresh.join("；")}`)
  items.patch(itemId, { status: "done", stage: "收尾", now, plan: i.plan.map((p) => ({ ...p, left: !p.done })) })
  expireOpen(itemId, "finished")
  for (const d of deps.dependents(itemId)) checkBlocked(d)
  tidyBranch(itemId)
}
/* a project with a release action: accepted is not finished; the item stays open until the release is live */
const releasePending = (i: Item) => !!(i.branch && projects.get(i.project_id)?.release)
function awaitRelease(itemId: string, send: (m: string) => void) {
  const i = items.get(itemId)
  items.patch(itemId, { stage: "收尾", now: "等待发布" }); settle(itemId)
  send(`验收通过。确认改动都已提交到 ${i.branch} 后，用 steve_publish 请求发布。`)
}
/** a finished or dropped item gives back its worktree (only when clean; for a finished item only once its branch is merged)
 * and its branch once it is merged into main; unmerged work stays */
export function tidyBranch(itemId: string) {
  const i = items.get(itemId), p = projects.get(i?.project_id)
  if (!i || !["done", "drop"].includes(i.status) || !i.branch || !i.cwd || !p || !i.cwd.startsWith(config.worktrees + "/") || !isGit(p.path)) return
  const ss = sessions.ofItem(itemId)
  if (ss.some((s) => acp.isBusy(s.id))) return
  for (const s of ss) acp.release(s.id)
  const r = tidyWorktree(p.path, i.cwd, i.branch, { onlyMerged: i.status === "done" })
  const said = [r.worktree === "removed" ? `删除了工作区 ${i.cwd}` : "", r.branch === "deleted" ? `删除了已合并的分支 ${i.branch}` : ""].filter(Boolean)
  if (said.length) timeline.add(itemId, "steve", "sys", said.join("；"))
}
/* a removed worktree comes back (from the kept branch, or fresh from main) when the item continues */
function ensureCwd(itemId: string) {
  const i = items.get(itemId)
  if (i?.branch && i.cwd && !existsSync(i.cwd)) try { prepareCwd(i) } catch (e) { console.error("worktree", (e as Error).message) }
}
export { depsOpen }
function checkBlocked(itemId: string) {
  const i = items.get(itemId); if (!i || i.status !== "block" || depsOpen(itemId).length) return
  const auto = settings.get(i.workspace_id, "depsAfter", "自动开始") === "自动开始"
  timeline.add(itemId, "steve", "sys", auto ? "前置事项都已完成，Steve 自动开始" : "前置事项都已完成，等你开始")
  if (!auto) { decisions.create({ workspace_id: i.workspace_id, kind: "propose", subject_type: "item", subject_id: itemId, item_id: itemId, question: "前置事项都已完成，现在开始？", draft: items.goal(itemId), options: ["yes", "no"], requested_by: "steve" }); items.patch(itemId, { status: "wait", now: "前置完成，等你开始" }); return }
  const summary = deps.of(itemId).map((d) => `${d}「${items.get(d).title}」：${items.get(d).now}`).join("\n")
  items.patch(itemId, { status: "run", now: "前置事项都已完成，开始处理" })
  const s = activeSession(itemId)
  const msg = `前置事项都已完成：\n${summary}\n现在开始这个事项。`
  if (s) void acp.prompt(s.id, msg, "steve"); else startSession(itemId, msg).catch((e) => fail(itemId, `开不了会话：${e.message}`))
}
function reaches(from: string, to: string, seen = new Set<string>()): boolean { if (seen.has(from)) return false; seen.add(from); return deps.of(from).some((d) => d === to || reaches(d, to, seen)) }
export function addDep(itemId: string, dep: string) {
  if (itemId === dep || deps.of(itemId).includes(dep)) return
  if (reaches(dep, itemId)) throw new Error("会形成循环依赖，没有添加")
  deps.add(itemId, dep); timeline.add(itemId, "me", "sys", `添加前置事项 ${dep}`)
  const i = items.get(itemId), s = activeSession(itemId)
  if (items.get(dep).status !== "done" && ["run", "prop", "pause"].includes(i.status) && !(s && acp.isBusy(s.id))) items.patch(itemId, { status: "block", now: "等前置事项完成后自动开始" })
}
export function rmDep(itemId: string, dep: string) { deps.remove(itemId, dep); timeline.add(itemId, "me", "sys", `移除前置事项 ${dep}`); checkBlocked(itemId) }

/* ── decisions ── */
const DAY = 24 * 3600_000
export function resolve(id: string, choice: string, text = "", actor = "me"): Decision {
  const d = decisions.get(id); if (!d || d.status !== "open") throw new Error("这个决定已经处理过了")
  audit(actor, "decision.resolve", id, { choice, text }, d.workspace_id)
  const itemId = d.item_id, s = d.session_id ? sessions.get(d.session_id) : itemId ? activeSession(itemId) : undefined
  if (!itemId || !items.get(itemId)) return resolveDetached(d, choice, text, actor, s)
  const send = (msg: string) => { if (itemId) supersede(itemId, ["ask", "fail"], "你已经回复，会话继续"); if (s && s.status !== "ended") void acp.prompt(s.id, msg, "steve"); else if (itemId) void startSession(itemId, msg).catch((e) => fail(itemId, `开不了会话：${e.message}`)) }
  if (d.kind === "accept") {
    if (choice === "pass") {
      const before = items.get(itemId!)
      decisions.resolve(id, { choice }, "resolved", { status: before.status, now: before.now, stage: before.stage, plan: before.plan })
      timeline.add(itemId!, actor, "me", "验收通过")
      /* a release still has to happen: the item stays in progress until it is live */
      if (releasePending(before) && text !== "no-release") awaitRelease(itemId!, send)
      else finish(itemId!, "已验收")
      return decisions.get(id)
    }
    decisions.resolve(id, { choice: "rework", text }); timeline.add(itemId!, actor, "me", "返工：" + text, s?.id ?? null)
    items.patch(itemId!, { stage: "执行", now: "按你的意见返工" }); send(`验收没有通过，返工意见：${text || "请再检查一遍"}\n改完再用 steve_deliver 交付。`)
  } else if (d.kind === "ask") {
    const answer = text || choice
    decisions.resolve(id, { answer }); timeline.add(itemId!, actor, "me", "回答：" + answer, s?.id ?? null)
    /* an item without a project that asked which project it belongs to: move it there, a new session continues in it */
    const i0 = items.get(itemId!), attach = !i0.project_id ? projects.list(i0.workspace_id).find((p) => p.name === answer.trim()) : undefined
    if (attach) {
      items.patch(itemId!, { project_id: attach.id, cwd: null, branch: null, now: `转到项目 ${attach.name}` }); timeline.add(itemId!, "steve", "sys", `关联到项目 ${attach.name}，开新会话`)
      if (s) { sessions.patch(s.id, { status: "ended" }); acp.release(s.id) }
      void startSession(itemId!, `用户把这个事项关联到了项目 ${attach.name}。上一个会话的最后进展：${s ? lastAgentText(s.id).slice(0, 600) : ""}\n在这个项目里继续。`).catch((e) => fail(itemId!, `开不了会话：${e.message}`))
    } else { items.patch(itemId!, { now: "收到回答，继续" }); send(`用户的回答：${answer}`) }
  } else if (d.kind === "approve" && d.subject_type === "tool_call") {
    const opt = choice === "deny" ? null : choice
    /* answer first: resolve() only touches open decisions, so marking it resolved first would hide the expiry */
    if (acp.answerPermission(id, opt)) decisions.resolve(id, { optionId: opt })
    else { decisions.resolve(id, { expired: true }, "expired"); send("你之前请求权限的那次工具调用已经失效（会话被重启过）。如果还需要，请重新执行。") }
  } else if (d.kind === "approve" && d.subject_type === "release") {
    const yes = choice === "yes"
    decisions.resolve(id, { approved: yes, text }); timeline.add(itemId!, actor, "me", (yes ? "批准发布 " : "不发布 ") + d.subject_id, s?.id ?? null)
    if (yes) { if (rebaseForRelease(itemId!, d.subject_id, send)) { items.patch(itemId!, { now: "正在发布" }); startRelease(d.subject_id, itemId!) } }
    else if (text) send(`用户没有批准发布：${text}。`)
    else { finish(itemId!, "已验收，未发布"); return decisions.get(id) }
  } else if (d.kind === "approve") {
    const yes = choice === "yes"
    decisions.resolve(id, { approved: yes, text }); timeline.add(itemId!, actor, "me", (yes ? "批准：" : "拒绝：") + d.question, s?.id ?? null)
    send(yes ? `用户批准了：${d.question}${text ? "。补充：" + text : ""}` : `用户拒绝了：${d.question}${text ? "。原因：" + text : ""}。不要执行这个操作。`)
  } else if (d.kind === "propose") {
    const before = items.get(itemId!)
    if (choice === "yes") { decisions.resolve(id, { choice }); timeline.add(itemId!, actor, "me", "同意开始"); items.patch(itemId!, { status: "run" }); const a = activeSession(itemId!); if (a) void acp.prompt(a.id, "用户同意了，开始做。", "steve"); else void startSession(itemId!).catch((e) => fail(itemId!, `开不了会话：${e.message}`)) }
    else { decisions.resolve(id, { choice }, "resolved", { status: before.status, now: before.now }); timeline.add(itemId!, actor, "me", "不需要"); items.patch(itemId!, { status: "drop", now: "你认为不需要" }); tidyBranch(itemId!) }
    return decisions.get(id)
  } else if (d.kind === "fail") {
    const before = items.get(itemId!)
    if (choice === "continue") { decisions.resolve(id, { choice, text }); items.patch(itemId!, { now: "让它继续" }); send(text || "请继续。完成后用 steve_deliver 交付；需要决定时用 steve_ask。") }
    else if (choice === "delivered") { decisions.resolve(id, { choice }); decisions.create({ workspace_id: d.workspace_id, kind: "accept", subject_type: "item", subject_id: itemId!, item_id: itemId, session_id: s?.id ?? null, question: "请验收", draft: lastAgentText(s?.id ?? "") + planNote(items.get(itemId!).plan), options: ["pass", "rework"], requested_by: "steve" }) }
    else if (choice === "look") { decisions.resolve(id, { choice }, "resolved", { status: before.status, now: before.now }); if (s) acp.cancel(s.id); items.patch(itemId!, { status: "pause", now: "你在查看，已暂停" }) }
    else if (choice === "tosteve") { decisions.resolve(id, { choice }); reassign(itemId!, "steve", actor) }
    else decisions.resolve(id, { choice, text })
  } else decisions.resolve(id, { choice, text })
  if (itemId) settle(itemId)
  return decisions.get(id)
}
/** a decision with no item: the session never had one (forked, imported then continued) or the item was deleted since.
 *  Every kind is answered straight to its session, which is woken up if it had ended; nothing item-shaped is touched. */
function resolveDetached(d: Decision, choice: string, text: string, actor: string, s: Session | undefined): Decision {
  const id = d.id
  const send = (msg: string) => { if (!s) return; if (s.status === "ended") sessions.patch(s.id, { status: "idle" }); void acp.prompt(s.id, msg, "steve") }
  const note = (t: string) => { if (s) events.add(s.id, "system", { text: t }) }
  if (d.kind === "approve" && d.subject_type === "tool_call") {
    const opt = choice === "deny" ? null : choice
    if (acp.answerPermission(id, opt)) decisions.resolve(id, { optionId: opt })
    else { decisions.resolve(id, { expired: true }, "expired"); send("你之前请求权限的那次工具调用已经失效（会话被重启过）。如果还需要，请重新执行。") }
  } else if (d.kind === "ask") {
    const answer = text || choice
    decisions.resolve(id, { answer }); note(`你回答了：${answer.slice(0, 200)}`); send(`用户的回答：${answer}`)
  } else if (d.kind === "approve" && d.subject_type === "release") {
    /* a release belongs to an item's project; without the item there is nothing Steve can publish */
    const yes = choice === "yes"
    decisions.resolve(id, { approved: yes, text, detached: true })
    send(yes ? "这个会话没有关联的事项（或事项已删除），Steve 不能发布。需要发布的话，先把会话关联到事项。" : `用户没有批准发布${text ? "：" + text : ""}。`)
  } else if (d.kind === "approve") {
    const yes = choice === "yes"
    decisions.resolve(id, { approved: yes, text }); note((yes ? "你批准了：" : "你拒绝了：") + d.question)
    send(yes ? `用户批准了：${d.question}${text ? "。补充：" + text : ""}` : `用户拒绝了：${d.question}${text ? "。原因：" + text : ""}。不要执行这个操作。`)
  } else if (d.kind === "accept") {
    if (choice === "pass") { decisions.resolve(id, { choice: "pass" }); note("你验收通过了"); send("验收通过。") }
    else { decisions.resolve(id, { choice: "rework", text }); send(`验收没有通过，返工意见：${text || "请再检查一遍"}`) }
  } else if (d.kind === "fail") {
    decisions.resolve(id, { choice, text })
    if (choice === "continue") send(text || "请继续。")
    else if (choice === "look" && s) acp.cancel(s.id)
  } else decisions.resolve(id, { choice, text })
  return decisions.get(id)
}
/** undo a decision within 24 hours, when nothing irreversible happened because of it */
export function undoDecision(id: string, actor = "me") {
  const d = decisions.get(id)
  if (!d || d.status !== "resolved" || !d.undo || !d.resolved || Date.now() - d.resolved > DAY) throw new Error("这个决定不能撤销")
  if (d.item_id) items.patch(d.item_id, d.undo)
  decisions.reopen(id); audit(actor, "decision.undo", id, {}, d.workspace_id)
  if (d.item_id) { timeline.add(d.item_id, actor, "me", "撤销了决定"); settle(d.item_id) }
  if (d.kind === "accept" && d.item_id) reblock(d.item_id, d.resolved)
  return decisions.get(id)
}
/** an acceptance was taken back: dependents it unblocked go back to waiting, unless they already started a turn */
function reblock(itemId: string, since: number) {
  for (const depId of deps.dependents(itemId)) {
    const x = items.get(depId)
    if (!x || ["done", "drop", "block"].includes(x.status) || !depsOpen(depId).length) continue
    if (!timeline.list(depId).some((t) => t.ts >= since && String(t.text).startsWith("前置事项都已完成"))) continue
    const ran = sessions.ofItem(depId).some((s) => acp.isBusy(s.id) || db.prepare("SELECT 1 FROM events WHERE session_id = ? AND ts >= ? LIMIT 1").get(s.id, since))
    if (ran) { timeline.add(depId, "steve", "sys", `前置事项 ${itemId} 的验收被撤销了；这个事项已经开始，没有停下`); continue }
    decisions.ofItem(depId).filter((o) => o.status === "open" && o.kind === "propose").forEach((o) => decisions.resolve(o.id, { superseded: "前置事项的验收被撤销" }, "expired"))
    items.patch(depId, { status: "block", now: "等前置事项完成后自动开始" }); timeline.add(depId, "steve", "sys", `前置事项 ${itemId} 的验收被撤销了，回到等前置`)
  }
}
/** take a card out of 待处理 without answering it (it no longer applies): nothing is sent to the agent.
 *  Undoable like other decisions, except a tool permission — dismissing that denies the call, and an answered permission can't be taken back. */
export function dismissDecision(id: string, actor = "me"): Decision {
  const d = decisions.get(id); if (!d || d.status !== "open") throw new Error("这个决定已经处理过了")
  audit(actor, "decision.dismiss", id, {}, d.workspace_id)
  const i = d.item_id ? items.get(d.item_id) : undefined
  if (d.kind === "approve" && d.subject_type === "tool_call") { acp.answerPermission(id, null); decisions.resolve(id, { dismissed: true }) }
  else decisions.resolve(id, { dismissed: true }, "resolved", i ? { status: i.status, now: i.now } : null)
  if (i) {
    timeline.add(i.id, actor, "me", "撤回了待处理：" + d.question.slice(0, 80))
    const s = activeSession(i.id), open = decisions.ofItem(i.id).some((x) => x.status === "open")
    /* nothing left to wait for and nothing running: park it, so 继续 picks it up again */
    if (!open && !(s && acp.isBusy(s.id)) && ["wait", "run"].includes(i.status)) items.patch(i.id, { status: "pause", now: "你撤回了待处理，需要时点继续" })
    else settle(i.id)
  }
  return decisions.get(id)
}
/* a finished or dropped item has nothing left to decide */
const expireOpen = (itemId: string, why: string) => decisions.ofItem(itemId).filter((d) => d.status === "open").forEach((d) => decisions.resolve(d.id, { [why]: true }, "expired"))

/* ── user actions on items and sessions ── */
/** any new message means the item is in progress again, even if it was finished */
function reactivate(itemId: string | null) {
  const i = itemId ? items.get(itemId) : undefined; if (!i || !["done", "drop", "pause"].includes(i.status)) return
  ensureCwd(i.id)
  items.patch(i.id, { status: "run", stage: "执行", now: "收到新消息，继续处理", archived: false }); settle(i.id)
}
export function supplement(itemId: string, text: string, mode: "auto" | "steer" | "new", files: FileRef[] = []) {
  const s = activeSession(itemId)
  timeline.add(itemId, "me", "me", text, s?.id ?? null, files)
  supersede(itemId, ["ask", "fail"], "你直接补充了信息"); reactivate(itemId)
  if (mode === "new" || !s) return startSession(itemId, `补充信息：${text}\n\n（这是事项 ${itemId} 的新会话。）`, undefined, files)
  /* past the handoff threshold the next message starts a fresh session with a handoff summary */
  const limit = Number(settings.get(items.get(itemId).workspace_id, "handoff", "80"))
  if (s.used && s.size && (s.used / s.size) * 100 >= limit && !acp.isBusy(s.id)) {
    const pct = Math.round((s.used / s.size) * 100), last = lastAgentText(s.id).slice(0, 800)
    events.add(s.id, "system", { text: `上下文已用 ${pct}%，带交接摘要转到新会话` }); sessions.patch(s.id, { status: "ended" }); acp.release(s.id)
    timeline.add(itemId, "steve", "sys", `上下文已用 ${pct}%：开新会话`)
    return startSession(itemId, `交接：上一个会话的上下文用到了 ${pct}%。它的最后进展：${last}\n\n补充信息：${text}`, undefined, files)
  }
  if (mode === "steer" && acp.isBusy(s.id)) acp.cancel(s.id)
  return acp.prompt(s.id, `补充信息：${text}`, "me", files)
}
/** deliver everything queued for the item's active session now */
export function deliverQueued(itemId: string) {
  const s = activeSession(itemId); if (!s) return
  const q = queued.take(s.id); if (!q.length) return
  reactivate(itemId)
  if (acp.isBusy(s.id)) { for (const x of q) queued.add(s.id, x.text, x.via, x.files); return }
  void acp.prompt(s.id, q.map((x) => x.text).join("\n\n"), "me", q.flatMap((x) => x.files))
}
export function itemAction(id: string, action: "pause" | "resume" | "drop" | "reopen" | "archive" | "unarchive", actor = "me") {
  const i = items.get(id); audit(actor, "item." + action, id, {}, i.workspace_id)
  const s = activeSession(id)
  if (action === "pause") { if (s) acp.cancel(s.id); items.patch(id, { status: "pause", now: "你暂停了" }) }
  if (action === "drop") { if (s) { acp.cancel(s.id); sessions.patch(s.id, { status: "ended" }); acp.release(s.id) } expireOpen(id, "dropped"); items.patch(id, { status: "drop", now: "你放弃了" }); tidyBranch(id) }
  if (action === "resume" || action === "reopen") {
    supersede(id, ["fail"], "你让它继续了")
    ensureCwd(id)
    if (depsOpen(id).length) items.patch(id, { status: "block", now: "等前置事项完成后自动开始" })
    /* paused while a question or approval was open: back to waiting on it, don't push the agent past it */
    else if (decisions.ofItem(id).some((d) => d.status === "open")) { items.patch(id, { status: "run", now: "继续处理" }); settle(id) }
    else { items.patch(id, { status: "run", now: "继续处理" }); if (s && s.status !== "ended") void acp.prompt(s.id, "继续这个事项。", "me"); else void startSession(id).catch((e) => fail(id, `开不了会话：${e.message}`)) }
  }
  if (action === "archive") items.patch(id, { archived: true })
  if (action === "unarchive") items.patch(id, { archived: false })
  timeline.add(id, actor, "me", ({ pause: "暂停", resume: "继续", drop: "放弃", reopen: "重新打开", archive: "归档", unarchive: "恢复" } as const)[action])
  return items.get(id) ?? i
}
export function reviseGoal(id: string, text: string) {
  const prev = items.goal(id), v = items.addGoal(id, text); timeline.add(id, "me", "sys", `目标更新为 v${v}`)
  const s = activeSession(id)
  /* a much bigger goal starts over with a handoff, a small change is told to the running session */
  if (s && text.length > prev.length * 1.6) { sessions.patch(s.id, { status: "ended" }); acp.release(s.id); void startSession(id, `目标改动较大（v${v}）：${text}\n上一个会话的最后进展：${lastAgentText(s.id).slice(0, 600)}`).catch((e) => fail(id, `开不了会话：${e.message}`)); timeline.add(id, "steve", "sys", "目标改动较大：开新会话") }
  else if (s) void acp.prompt(s.id, `目标更新了（v${v}）：${text}\n按新目标调整计划，并用 steve_update 更新。`, "me")
}
/** give the item to another bot: the old session ends, the new bot starts with a handoff */
export function reassign(id: string, botId: string, actor = "me") {
  const i = items.get(id); if (i.bot_id === botId) return items.get(id)
  const b = bots.get(botId); if (!b || !bots.inWorkspace(botId, i.workspace_id)) throw new Error("这个 Bot 不在当前工作空间")
  const s = activeSession(id), handoff = s ? lastAgentText(s.id).slice(0, 800) : ""
  if (s) { acp.cancel(s.id); sessions.patch(s.id, { status: "ended" }); acp.release(s.id) }
  decisions.ofItem(id).filter((d) => d.status === "open" && d.session_id).forEach((d) => decisions.resolve(d.id, { reassigned: true }, "expired"))
  timeline.add(id, actor, "sys", `换成${b.main ? b.name : b.name + " Bot"}`)
  void startSession(id, `这个事项从别的 Bot 交接给你。${handoff ? "上一位的最后进展：" + handoff : ""}`, botId).catch((e) => fail(id, `开不了会话：${e.message}`))
  return items.get(id)
}
export function sessionMessage(sessionId: string, text: string, mode: "插话" | "排队", files: FileRef[] = []) {
  const s = sessions.get(sessionId)!
  if (s.item_id) { timeline.add(s.item_id, "me", "me", text, s.id, files); supersede(s.item_id, ["ask", "fail"], "你直接在会话里发了消息"); reactivate(s.item_id) }
  if (s.status === "ended") sessions.patch(sessionId, { status: "idle" })
  if (mode === "插话" && acp.isBusy(sessionId)) acp.cancel(sessionId)
  return acp.prompt(sessionId, text, "me", files)
}
export function endSession(sessionId: string) { acp.cancel(sessionId); sessions.patch(sessionId, { status: "ended" }); acp.release(sessionId); events.add(sessionId, "system", { text: "会话已结束，发送消息可以恢复" }) }

/* ── fork: carry the conversation up to a message into a new item or a new session of the same item ── */
function transcriptUpTo(sessionId: string, seq: number) {
  const out: string[] = []; let agent = ""
  for (const e of events.upTo(sessionId, seq)) {
    if (e.kind === "agent_message_chunk") { agent += e.data?.content?.text ?? ""; continue }
    if (agent) { out.push("Agent：" + agent.trim()); agent = "" }
    if (e.kind === "user") out.push((e.data?.via === "steve" ? "Steve：" : "用户：") + String(e.data?.text ?? "").slice(0, 1500))
  }
  if (agent) out.push("Agent：" + agent.trim())
  const text = out.join("\n\n"); return text.length > 8000 ? "…" + text.slice(-8000) : text
}
export function fork(ws: string, sessionId: string, seq: number, goal: string, asSession: boolean, actor = "me") {
  const s = sessions.get(sessionId); if (!s) throw new Error("没有这个会话")
  const ctx = transcriptUpTo(sessionId, seq), g = goal || "从这里继续另一件事"
  const opening = `这是从另一个会话派生出来的。派生点之前的对话：\n\n${ctx}\n\n现在要做的：${g}`
  if (asSession && s.item_id) {
    timeline.add(s.item_id, actor, "sys", `从会话 #${s.no} 派生出新会话`)
    return startSession(s.item_id, opening).then((ns) => ({ session: ns.id }))
  }
  const parentItem = s.item_id ? items.get(s.item_id) : null
  const item = createItem({ ws, goal: g, title: g.slice(0, 24), bot: s.bot_id ?? parentItem?.bot_id ?? "steve", project: parentItem?.project_id ?? null, source: ["派生", "fork"], parent: { item: s.item_id, session: s.id, at: seq }, tagIds: parentItem ? tags.ofItem(parentItem.id) : [], opening, actor })
  if (s.item_id) timeline.add(s.item_id, actor, "sys", `派生出事项 ${item.id}「${g.slice(0, 20)}」`)
  return Promise.resolve({ item: item.id })
}

/* ── bots ── */
export function deleteBot(id: string, to: string, actor = "me") {
  const b = bots.get(id); if (!b || b.main) throw new Error("主 Bot 不能删除")
  const target0 = bots.get(to); if (!target0 || to === id) throw new Error("没有接手的 Bot")
  const moved: string[] = []
  for (const w of bots.workspaces(id)) {
    /* the heir may not be in every workspace the bot served: there the main bot takes over */
    const target = bots.inWorkspace(to, w.workspace_id) ? target0 : bots.main()
    for (const i of items.list(w.workspace_id).filter((x) => x.bot_id === id && !["done", "drop"].includes(x.status))) {
      timeline.add(i.id, "steve", "sys", `${b.name} Bot 已删除，交给${target.main ? target.name : target.name + " Bot"}`); moved.push(i.id)
      /* running items continue with the heir (a new session with a handoff); the rest just change hands */
      if (i.status === "run") { reassign(i.id, target.id, actor); continue }
      items.patch(i.id, { bot_id: target.id })
      const s = activeSession(i.id); if (s) { acp.cancel(s.id); sessions.patch(s.id, { status: "ended" }); acp.release(s.id) }
    }
  }
  bots.remove(id); audit(actor, "bot.delete", id, { to, moved })
  return { moved }
}

/* ── boot: a turn cut off by the restart continues once by itself, with whatever was queued for it; an item that waits
 *  on you (paused, or an open decision) is not poked; a repeat within the window, or a resume that fails, shows a fail card ── */
const INTERRUPTED = "Hub 重启时这个会话正在运行，这一回合被中断了"
export const RESUME = "Hub 刚重启，上一回合被中断了，请从中断处继续；中断前做到一半的操作，先确认结果再往下。"
const resumed = new Set<string>()
const waitsOnUser = (itemId: string | null) => { const i = itemId ? items.get(itemId) : undefined; return !!i && (i.status === "pause" || decisions.ofItem(i.id).some((d) => d.status === "open")) }
export function recover() {
  reportRelease()
  /* first the cards that no longer apply, so "waits on the user" below only counts live ones */
  for (const w of db.prepare("SELECT id FROM workspaces").all() as { id: string }[]) {
    for (const d of decisions.open(w.id)) {
      /* a tool permission dies with the adapter that asked; the resumed turn asks again if it still needs it */
      if (d.kind === "approve" && d.subject_type === "tool_call") { decisions.resolve(d.id, { expired: true }, "expired"); if (d.item_id) settle(d.item_id) }
      /* reminders left behind on items that were finished or dropped before this was enforced */
      else if (d.item_id && ["done", "drop"].includes(items.get(d.item_id)?.status)) decisions.resolve(d.id, { [items.get(d.item_id).status === "done" ? "finished" : "dropped"]: true }, "expired")
    }
    /* cards from before the expiry rules: a question or failure older than a delivery or approval request */
    for (const id of new Set(decisions.open(w.id).map((d) => d.item_id).filter((x): x is string => !!x))) tidy(id)
  }
  const resume: string[] = []
  for (const s of sessions.list()) {
    if (s.status !== "running" && s.status !== "starting") continue
    sessions.patch(s.id, { status: "idle" })
    const i = s.item_id ? items.get(s.item_id) : undefined
    if (!i || ["done", "drop"].includes(i.status)) { events.add(s.id, "system", { text: "Hub 重启，这一回合被中断。" }); continue }
    if (i.status === "pause") { events.add(s.id, "system", { text: "Hub 重启，这一回合被中断。事项已暂停，继续时接着做。" }); continue }
    if (waitsOnUser(i.id)) { events.add(s.id, "system", { text: "Hub 重启，这一回合被中断。事项在等你处理，之后再继续。" }); continue }
    /* never wake an agent in a directory that isn't there, nor at all when resuming is switched off (tests on a copy of the
     * real database: its sessions point at real worktrees and real harness histories) */
    if (config.noResume || !existsSync(s.cwd)) { events.add(s.id, "system", { text: "Hub 重启，这一回合被中断。没有自动继续，需要时让它继续。" }); fail(i.id, INTERRUPTED); continue }
    if (!mayResume(s.id)) {
      events.add(s.id, "system", { text: "Hub 重启，这一回合被中断。半小时内已经自动继续过一次，这次等你决定。" })
      fail(i.id, `${INTERRUPTED}（已经自动继续过一次）`); continue
    }
    events.add(s.id, "system", { text: "Hub 重启，这一回合被中断，自动继续。", resumed: true })
    timeline.add(i.id, "steve", "sys", "Hub 重启打断了会话，已自动继续", s.id)
    resume.push(s.id)
  }
  /* after the listener is up, so the sessions can reach the Steve tools: the resumes (each with what was queued for it,
   * identical texts once), then prompts held for the restart */
  setTimeout(() => {
    for (const id of resume) { resumed.add(id); if (!acp.flush(id, { text: RESUME, via: "steve" })) resumed.delete(id) }
    if (!config.noResume) flushQueued()
  }, RESUME_DELAY).unref()
}
const RESUME_DELAY = 3000
function flushQueued() {
  for (const { id } of db.prepare("SELECT DISTINCT q.session_id id FROM queued q JOIN sessions s ON s.id = q.session_id WHERE s.status != 'ended'").all() as { id: string }[])
    if (!waitsOnUser(sessions.get(id)?.item_id ?? null)) acp.flush(id)
}
acp.setMcpResolver(mcpEndpoint)
export { messages }

/* ── releases: run the project's release script detached; it restarts the Hub, so the outcome is reported on the next boot.
 * Hand-off: the script builds first, then asks this Hub over loopback (with a one-off token) to drain — no new turns,
 * new prompts queue — and polls until the running turns have ended (bounded by the script), then restarts. ── */
let releasing: { token: string; item: string; timer?: NodeJS.Timeout } | null = null
/** rebase the item's branch onto main in its worktree; on any problem the agent is sent back and nothing is released */
function rebaseForRelease(itemId: string, branch: string, send: (m: string) => void) {
  const i = items.get(itemId)
  if (!i.cwd || i.branch !== branch || !existsSync(i.cwd)) return true
  const r = rebaseOnto(i.cwd, "main")
  if (r.ok) { if (r.moved) timeline.add(itemId, "steve", "sys", `已把 ${branch} rebase 到最新的 main`); return true }
  const msg = r.why === "dirty" ? `工作区有未提交的改动（${r.detail}），没有发布。先把改动提交到 ${branch}，再用 steve_publish 请求发布。`
    : r.why === "conflict" ? `发布前要把 ${branch} rebase 到最新的 main，自动 rebase 有冲突（${r.detail}），已经撤销，没有发布。请在工作区里执行 git rebase main，解决冲突，重新跑类型检查和测试并提交，然后再用 steve_publish 请求发布。不要强推，也不要改 main。`
    : `发布前 rebase 到 main 失败（${r.detail}），没有发布。请检查分支 ${branch} 的状态，处理后再用 steve_publish 请求发布。`
  timeline.add(itemId, "steve", "sys", r.why === "conflict" ? `rebase 到 main 有冲突，已撤销，交回给它处理：${r.detail}` : msg)
  items.patch(itemId, { stage: "收尾", now: r.why === "conflict" ? "要先 rebase 解决冲突" : "发布前要先处理分支" })
  send(msg); return false
}
function startRelease(branch: string, itemId: string) {
  const p = projects.get(items.get(itemId).project_id), script = p?.release?.cmd
  if (!script || !existsSync(script)) { fail(itemId, `这个项目的发布脚本不存在（${script ?? "没有配置"}），无法发布`); return }
  if (releasing) { fail(itemId, `另一个发布（${releasing.item}）正在进行；它结束后选“继续”，让它重新请求发布`); return }
  mkdirSync(join(config.home, "logs"), { recursive: true })
  const log = join(config.home, "logs", `release-${Date.now()}.log`), out = openSync(log, "a"), token = randomBytes(24).toString("base64url")
  const child = spawn("/bin/zsh", [script, branch, itemId], { detached: true, stdio: ["ignore", out, out], env: { ...process.env, STEVE_REPO: p!.path, STEVE_HOME: config.home, STEVE_PORT: String(config.listen.port), STEVE_DRAIN_TOKEN: token } })
  releasing = { token, item: itemId }
  /* the script ended while we are still running: it failed before the restart (its result is in last.json) — lift the hold */
  const done = () => { clearTimeout(releasing?.timer); releasing = null; endHold() }
  child.on("exit", done); child.on("error", done); child.unref()
  timeline.add(itemId, "steve", "sys", `开始发布 ${branch}，日志 ${log}`)
}
/** the release script's drain call: start holding (once) and report whether running turns are left; null for a wrong token */
export function drain(token: string, start: boolean) {
  if (!releasing || !token || token !== releasing.token) return null
  if (start && !acp.isHolding()) {
    acp.hold(true)
    const n = acp.runningTurns()
    timeline.add(releasing.item, "steve", "sys", n ? `新版本已构建好，等 ${n} 个正在进行的回合结束后重启；新消息先排队，重启后送达` : "新版本已构建好，没有正在进行的回合，马上重启")
    /* the script restarts us or exits; if neither happens, don't hold forever */
    releasing.timer = setTimeout(endHold, 20 * 60_000); releasing.timer.unref()
  }
  return { idle: acp.runningTurns() === 0, running: acp.runningTurns() }
}
function endHold() { if (!acp.isHolding()) return; acp.hold(false); flushQueued() }
/* the script outlives the Hub it restarts, so the final result can land after boot: keep watching the file */
export function watchReleases() { watchFile(join(config.home, "releases/last.json"), { interval: 3000 }, () => reportRelease()) }
export function reportRelease() {
  const f = join(config.home, "releases/last.json"); if (!existsSync(f)) return
  let r: any; try { r = JSON.parse(readFileSync(f, "utf8")) } catch { return }
  if (r.reported || r.pending) return
  r.reported = true; writeFileSync(f, JSON.stringify(r, null, 1))
  /* kind (ops/release.sh): "hub" restarted the Hub, "web" only switched the web build, "hub+web" did both; absent before the split */
  const what = r.kind === "web" ? "前端" : r.kind === "hub+web" ? "Hub 和前端" : ""
  const text = r.ok ? `已发布${what}新版本 ${r.version}${r.kind === "web" ? "（Hub 没有重启，刷新页面生效）" : ""}`
    : `发布${what} ${r.version ?? ""} 失败，${r.kind === "hub+web" && r.hub_ok ? `Hub 已是新版本，前端仍是 ${r.web_prev || "上一个版本"}` : `已回到 ${r.prev || "上一个版本"}`}：${r.error ?? "健康检查没有通过"}`
  const item = r.item ? items.get(r.item) : undefined
  if (item) {
    timeline.add(item.id, "steve", "sys", text)
    /* a failed release reopens the item, whatever state it was in */
    if (r.ok) finish(item.id, "已发布"); else { items.patch(item.id, { status: "run", stage: "收尾", now: "发布失败" }); fail(item.id, text) }
  }
  notify(item?.workspace_id ?? WS, "release", r.ok ? "success" : "fail", text, item?.id ?? null)
}

/** how a new plan relates to the one before: undefined when it keeps every earlier step (the same round, grown or ticked off),
 *  else the earlier round ends — done if all of it was done, replaced if not */
export function planTurn(before: PlanStep[], plan: PlanStep[]): PlanRound["end"] | undefined {
  if (!before.length || before.every((b) => plan.some((p) => p.step === b.step))) return undefined
  return before.every((b) => b.done) ? "done" : "replaced"
}
/* the steps left open, for an acceptance card */
const planNote = (plan: PlanStep[]) => { const open = plan.filter((p) => !p.done).map((p) => p.step); return open.length ? `\n\n没做完的计划步骤：${open.join("；")}` : "" }
