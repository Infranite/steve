/* ACP runner: one adapter process per live session, JSON-RPC over stdio.
 * Owns: start / load, the prompt queue, cancel, permission requests, idle release. Knows nothing about items. */
import { spawn, type ChildProcess } from "node:child_process"
import { config } from "./config.js"
import { sessions, events, queued, decisions, bots, type Session, type FileRef } from "./repo.js"
import { forAgent } from "./files.js"
import { codexArgs, CODEX_MODES } from "./codex.js"
import { reportedConfig, requireEffort } from "./effort.js"

type Pending = { res: (v: any) => void; rej: (e: Error) => void }
type Live = { proc: ChildProcess; request: (m: string, p: unknown, ms?: number) => Promise<any>; notify: (m: string, p: unknown) => void; respond: (id: number | string, result: unknown) => void; acpId: string; busy: boolean; idleTimer?: NodeJS.Timeout; image?: boolean }
export type TurnEnd = { session: Session; stopReason: string; error?: string }

const live = new Map<string, Live>()
const permissionWaits = new Map<string, { sessionId: string; rpcId: number | string }>()
let onTurnEnd: (t: TurnEnd) => void = () => {}
export const setTurnEndHandler = (f: (t: TurnEnd) => void) => { onTurnEnd = f }

/** the MCP endpoint and token a session uses to reach Steve's tools */
let mcpFor: (s: Session) => { url: string; token: string } = () => ({ url: "", token: "" })
export const setMcpResolver = (f: typeof mcpFor) => { mcpFor = f }

function adapterCommand(harness: string): [string, string[]] {
  const cmd = config.adapters[harness]; if (!cmd) throw new Error(`no adapter for harness ${harness}`)
  const extra = harness === "codex" ? codexArgs() : []
  return cmd.endsWith(".js") ? [process.execPath, [cmd, ...extra]] : [cmd, extra]
}

async function startProcess(s: Session): Promise<Live> {
  const [cmd, args] = adapterCommand(s.harness)
  const proc = spawn(cmd, args, { cwd: s.cwd, stdio: ["pipe", "pipe", "pipe"], env: { ...process.env, CLAUDE_CODE_EXECUTABLE: config.claudeExecutable } })
  let buf = "", nextId = 0
  const pending = new Map<number, Pending>()
  const send = (m: object) => proc.stdin!.write(JSON.stringify({ jsonrpc: "2.0", ...m }) + "\n")
  const l: Live = {
    proc, acpId: s.acp_session_id ?? "", busy: false,
    request: (method, params, ms = 0) => new Promise((res, rej) => {
      const id = ++nextId; pending.set(id, { res, rej }); send({ id, method, params })
      if (ms) setTimeout(() => { if (pending.delete(id)) rej(new Error(`timeout ${method}`)) }, ms)
    }),
    notify: (method, params) => send({ method, params }),
    respond: (id, result) => send({ id, result }),
  }
  proc.stderr!.on("data", (d) => { const t = String(d).trim(); if (t) console.error(`[acp ${s.id}] ${t.slice(0, 500)}`) })
  /* a failed spawn (missing binary or cwd) or a closed pipe must fail the turn, not crash the Hub */
  proc.on("error", (e) => { for (const p of pending.values()) p.rej(e); pending.clear(); if (live.get(s.id) === l) live.delete(s.id) })
  proc.stdin!.on("error", () => {})
  proc.on("exit", (code) => {
    for (const p of pending.values()) p.rej(new Error(`adapter exited (${code})`))
    pending.clear(); if (live.get(s.id) === l) live.delete(s.id)
  })
  proc.stdout!.on("data", (d) => {
    buf += d
    let n: number
    while ((n = buf.indexOf("\n")) >= 0) {
      const line = buf.slice(0, n); buf = buf.slice(n + 1)
      if (!line.trim()) continue
      let m: any; try { m = JSON.parse(line) } catch { continue }
      if (m.id != null && !m.method && pending.has(m.id)) { const p = pending.get(m.id)!; pending.delete(m.id); m.error ? p.rej(Object.assign(new Error(m.error.message), { data: m.error })) : p.res(m.result); continue }
      if (m.method === "session/update") { onUpdate(s.id, m.params.update); continue }
      if (m.method === "session/request_permission") { onPermission(s.id, m.id, m.params); continue }
      if (m.id != null && m.method) send({ id: m.id, error: { code: -32601, message: `${m.method} not supported by Steve` } })
    }
  })
  const init = await l.request("initialize", { protocolVersion: 1, clientCapabilities: { fs: { readTextFile: false, writeTextFile: false }, terminal: false }, clientInfo: { name: "steve", version: config.version } }, 60_000)
  /* image content blocks only go to adapters that say they take them; attachments always also go by path */
  l.image = !!init?.agentCapabilities?.promptCapabilities?.image
  const mcp = mcpFor(s)
  const mcpServers = [{ type: "http", name: "steve", url: mcp.url, headers: [{ name: "Authorization", value: `Bearer ${mcp.token}` }] }]
  let r: any
  if (s.acp_session_id) {
    r = await l.request("session/load", { sessionId: s.acp_session_id, cwd: s.cwd, mcpServers }, 120_000)
  } else {
    r = await l.request("session/new", { cwd: s.cwd, mcpServers }, 120_000)
    l.acpId = r.sessionId; sessions.patch(s.id, { acp_session_id: r.sessionId })
  }
  /* Claude Code reports models in `models`; Codex only as the "model" config option */
  const modelOpt = r?.configOptions?.find((o: any) => o.id === "model")
  let options = r?.configOptions ?? []
  const reported = r?.models?.currentModelId ?? modelOpt?.currentValue
  if (reported) sessions.patch(s.id, { model: reported })
  sessions.patch(s.id, reportedConfig(options))
  /* the mode does not survive session/load, so it is set every time the process starts */
  const bot = s.bot_id ? bots.get(s.bot_id) : null
  const mode = s.harness === "codex" ? CODEX_MODES[bot?.perm_mode ?? "default"] ?? "read-only" : bot?.perm_mode && bot.perm_mode !== "default" ? bot.perm_mode : null
  if (mode) await l.request("session/set_mode", { sessionId: l.acpId, modeId: mode }, 30_000).catch((e) => console.error("set_mode", e.message))
  if (s.model) {
    const byOption = () => l.request("session/set_config_option", { sessionId: l.acpId, configId: "model", value: s.model }, 30_000)
    /* claude-agent-acp 0.99 and codex-acp only take the "model" config option (set_model is -32601 there) */
    const set = !r?.models && modelOpt ? byOption() : l.request("session/set_model", { sessionId: l.acpId, modelId: s.model }, 30_000).catch((e: any) => (e.data?.code === -32601 ? byOption() : Promise.reject(e)))
    /* an id the adapter doesn't know is silently replaced by another model: say so in the log */
    await set.then((res: any) => {
      options = res?.configOptions ?? options
      sessions.patch(s.id, reportedConfig(options))
      const cur = res?.models?.currentModelId
      if (cur) sessions.patch(s.id, { model: cur })
    }, (e) => console.error("set model", s.model, e.message))
  }
  if (s.reasoning_effort) {
    try {
      const configId = requireEffort(options, s.reasoning_effort)
      const res = await l.request("session/set_config_option", { sessionId: l.acpId, configId, value: s.reasoning_effort }, 30_000)
      const actual = reportedConfig(res?.configOptions ?? [])
      sessions.patch(s.id, actual)
      if (actual.reasoning_effort !== s.reasoning_effort) throw new Error(`思考强度未生效：请求 ${s.reasoning_effort}，实际 ${actual.reasoning_effort ?? "默认或未知"}`)
    } catch (e) { proc.kill(); throw e }
  }
  return l
}

/* replayed history arrives as updates during session/load; we only store updates that happen while a turn is live */
function onUpdate(sessionId: string, u: any) {
  const l = live.get(sessionId)
  if (u.sessionUpdate === "usage_update") { sessions.patch(sessionId, { used: u.used ?? null, size: u.size ?? null, cost: u.cost?.amount ?? undefined }); return }
  if (u.sessionUpdate === "config_option_update") { sessions.patch(sessionId, reportedConfig(u.configOptions ?? [])); return }
  if (u.sessionUpdate === "available_commands_update" || u.sessionUpdate === "current_mode_update") return
  if (!l?.busy) return
  events.add(sessionId, u.sessionUpdate, u)
}

function onPermission(sessionId: string, rpcId: number | string, params: any) {
  const l = live.get(sessionId); if (!l) return
  const title: string = params.toolCall?.title ?? params.toolCall?.kind ?? "工具调用"
  /* Steve's own tools are always allowed, once per call so nothing is persisted into the harness's settings */
  const own = steveTool(params), once = params.options.find((o: any) => o.kind === "allow_once")
  if (own && OWN_TOOLS.has(own) && once) { l.respond(rpcId, { outcome: { outcome: "selected", optionId: once.optionId } }); return }
  const s = sessions.get(sessionId)!
  const d = decisions.create({ workspace_id: s.workspace_id, kind: "approve", subject_type: "tool_call", subject_id: params.toolCall?.toolCallId ?? String(rpcId), item_id: s.item_id, session_id: sessionId,
    question: `允许执行：${title}`, draft: JSON.stringify(params.toolCall?.rawInput ?? {}, null, 1).slice(0, 2000), options: params.options.map((o: any) => `${o.optionId}|${o.name}|${o.kind}`), requested_by: s.bot_id ?? "agent" })
  permissionWaits.set(d.id, { sessionId, rpcId })
  events.add(sessionId, "permission", { decision: d.id, title })
}

/* every tool in mcp.ts only reports to Steve or asks the user; none has outside effects by itself */
const OWN_TOOLS = new Set(["steve_update", "steve_ask", "steve_deliver", "steve_request_approval", "steve_publish"])
/** the Steve MCP tool a permission request is for, read from the structured request rather than a free-text title
 * (a shell command's title is the command itself): Claude Code names the tool mcp__steve__<tool>;
 * Codex sends an MCP approval form that names the server and the tool */
function steveTool(params: any): string | null {
  const tc = params.toolCall ?? {}
  const cc = /^mcp__steve__(\w+)$/.exec(String(tc._meta?.claudeCode?.toolName ?? tc.name ?? "")); if (cc) return cc[1]
  const ri = tc.rawInput, req = ri?.request
  if (ri?.server_name === "steve" && req?._meta?.codex_approval_kind === "mcp_tool_call") {
    const m = /^Allow the steve MCP server to run tool "(\w+)"\?$/.exec(String(req.message ?? "")); if (m) return m[1]
  }
  return null
}

/** answer a pending tool permission; false when the adapter that asked is gone */
export function answerPermission(decisionId: string, optionId: string | null) {
  const w = permissionWaits.get(decisionId); if (!w) return false
  permissionWaits.delete(decisionId)
  const l = live.get(w.sessionId); if (!l) return false
  l.respond(w.rpcId, optionId ? { outcome: { outcome: "selected", optionId } } : { outcome: { outcome: "cancelled" } })
  return true
}

async function ensure(s: Session): Promise<Live> {
  const cur = live.get(s.id); if (cur) return cur
  const l = await startProcess(s); live.set(s.id, l); return l
}

/** send a prompt now, or queue it behind the running turn (or behind the hold before a restart) */
export async function prompt(sessionId: string, text: string, via: string, files: FileRef[] = []) {
  const s0 = sessions.get(sessionId); if (!s0) throw new Error("no session")
  const f = files.length ? { files } : {}
  if (turning.has(sessionId) || holding) {
    /* a session that never ran is not "interrupted" by the restart: its first prompt just waits */
    if (holding && !turning.has(sessionId) && s0.status === "starting") sessions.patch(sessionId, { status: "idle" })
    queued.add(sessionId, text, via, files); events.add(sessionId, "queued", { text, via, ...f }); return "queued"
  }
  void runTurn(sessionId, [{ text, via, ...f }]); return "sent"
}

/* a release is about to restart the Hub: no new turn starts, prompts wait in the queue (in the database, so they survive
 * the restart) and run when the hold is lifted or after the restart; running turns finish normally */
let holding = false
export const hold = (on: boolean) => { holding = on }
export const isHolding = () => holding
/** turns in flight (including ones waiting for a permission answer or a retry) */
export const runningTurns = () => turning.size
/** run what is queued for a session as one turn, optionally led by Steve's own message; identical texts are sent once */
export function flush(sessionId: string, lead?: Msg) {
  if (turning.has(sessionId) || holding) return false
  const seen = new Set<string>()
  const msgs = [...(lead ? [lead] : []), ...queued.take(sessionId).map(fromQueue)].filter((m) => { const k = m.text + "\u0000" + (m.files ?? []).map((f) => f.id).join(","); return !seen.has(k) && !!seen.add(k) })
  if (!msgs.length) return false
  void runTurn(sessionId, msgs); return true
}

/** sessions with a turn in flight, also while the adapter is still starting (busy is only set once the prompt runs) */
const turning = new Set<string>()
/* transient gateway errors (503, overloaded, rate limits) retry the same prompt on the same session a bounded number of times.
 * The session stays "in a turn" while waiting, so new messages queue behind the retry; cancel drops it; a Hub restart forgets it (recover reports the turn). */
const TRANSIENT = /\b(429|502|503|504|529)\b|temporarily|overloaded|try again|rate.?limit/i, RETRY_S = [20, 60]
const retrying = new Map<string, NodeJS.Timeout>()
type Msg = { text: string; via: string; files?: FileRef[] }
/** a queued prompt as a turn message; attachments wait in the queue with it (also across a restart) */
const fromQueue = (q: { text: string; via: string; files?: FileRef[] }): Msg => ({ text: q.text, via: q.via, ...(q.files?.length ? { files: q.files } : {}) })
async function runTurn(sessionId: string, msgs: Msg[], attempt = 0) {
  let s = sessions.get(sessionId)!
  turning.add(sessionId)
  if (!attempt) for (const m of msgs) events.add(sessionId, "user", m)
  sessions.patch(sessionId, { status: s.acp_session_id ? "running" : "starting" })
  let l: Live
  try { l = await ensure(s) } catch (e: any) {
    turning.delete(sessionId); sessions.patch(sessionId, { status: "error" }); events.add(sessionId, "system", { text: `无法启动 ${s.harness}：${e.message}` })
    onTurnEnd({ session: sessions.get(sessionId)!, stopReason: "error", error: e.message }); return
  }
  clearTimeout(l.idleTimer); l.busy = true; sessions.patch(sessionId, { status: "running" })
  let stopReason = "end_turn", error: string | undefined
  try {
    const r = await l.request("session/prompt", { sessionId: l.acpId, prompt: promptBlocks(s, msgs, !!l.image) })
    stopReason = r.stopReason ?? "end_turn"
  } catch (e: any) { stopReason = "error"; error = e.message; events.add(sessionId, "system", { text: `这一回合出错：${e.message}` }) }
  l.busy = false
  if (stopReason === "error" && error && TRANSIENT.test(error) && attempt < RETRY_S.length) {
    const wait = RETRY_S[attempt]
    events.add(sessionId, "system", { text: `推理网关暂时不可用，${wait} 秒后自动重试（第 ${attempt + 1} 次）` })
    retrying.set(sessionId, setTimeout(() => { retrying.delete(sessionId); void runTurn(sessionId, msgs, attempt + 1) }, wait * 1000)); return
  }
  turning.delete(sessionId)
  s = sessions.get(sessionId)!
  sessions.patch(sessionId, { status: stopReason === "error" ? "error" : "idle" })
  events.add(sessionId, "turn_end", { stopReason })
  const next = holding ? [] : queued.take(sessionId)
  if (next.length) { void runTurn(sessionId, next.map(fromQueue)); return }
  l.idleTimer = setTimeout(() => release(sessionId), config.idleReleaseMs)
  /* held messages will run after the hold: the item's turn is not over yet */
  if (holding && queued.list(sessionId).length) return
  onTurnEnd({ session: sessions.get(sessionId)!, stopReason, error })
}

/** the prompt as ACP content: one text block (attachment paths appended to each message), then the images */
function promptBlocks(s: Session, msgs: Msg[], image: boolean) {
  const imgs: { type: "image"; data: string; mimeType: string }[] = []
  const text = msgs.map((m) => {
    if (!m.files?.length) return m.text
    try { const a = forAgent(s.cwd, m.files, image); imgs.push(...a.images); return m.text + a.note }
    catch (e: any) { return m.text + `\n\n（用户附带了 ${m.files.length} 个附件，但没能放进工作目录：${e.message}）` }
  }).join("\n\n")
  return [{ type: "text", text }, ...imgs]
}

export function cancel(sessionId: string) {
  const r = retrying.get(sessionId)
  if (r) {
    clearTimeout(r); retrying.delete(sessionId); turning.delete(sessionId); sessions.patch(sessionId, { status: "idle" })
    events.add(sessionId, "system", { text: "你中断了当前回合，不再自动重试" }); events.add(sessionId, "turn_end", { stopReason: "cancelled" })
    /* like a cancelled live turn: whatever was queued behind it goes next */
    const next = holding ? [] : queued.take(sessionId)
    if (next.length) void runTurn(sessionId, next.map(fromQueue)); else onTurnEnd({ session: sessions.get(sessionId)!, stopReason: "cancelled" })
    return true
  }
  const l = live.get(sessionId); if (!l?.busy) return false
  l.notify("session/cancel", { sessionId: l.acpId }); events.add(sessionId, "system", { text: "你中断了当前回合" }); return true
}
/** free the adapter process; the next prompt resumes with session/load */
export function release(sessionId: string) { const l = live.get(sessionId); if (l && !l.busy) { l.proc.kill(); live.delete(sessionId) } }
export const isBusy = (sessionId: string) => turning.has(sessionId)
export function shutdown() { for (const l of live.values()) l.proc.kill() }
