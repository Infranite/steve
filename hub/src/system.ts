/* What this machine is and what it can run: the Hub registers itself as a device, reports the harnesses it finds,
 * and can import local harness history (Claude Code, Codex) as read-only sessions. */
import { execFileSync } from "node:child_process"
import { existsSync, readdirSync, readFileSync, statSync } from "node:fs"
import { cpus, homedir, hostname, totalmem, release, platform } from "node:os"
import { join } from "node:path"
import { config } from "./config.js"
import { db, now } from "./db.js"
import { devices, sessions, events, uid, sha } from "./repo.js"
import { publish } from "./bus.js"

const run = (cmd: string, args: string[]) => { try { return execFileSync(cmd, args, { stdio: ["ignore", "pipe", "ignore"], timeout: 8000 }).toString().trim() } catch { return "" } }
function osLabel() {
  if (platform() === "darwin") { const v = run("sw_vers", ["-productVersion"]); return `macOS ${v} · ${cpus()[0]?.model ?? ""} · ${Math.round(totalmem() / 2 ** 30)} GB` }
  return `${platform()} ${release()} · ${Math.round(totalmem() / 2 ** 30)} GB`
}
export function registerHub() {
  const name = run("scutil", ["--get", "ComputerName"]) || hostname()
  devices.upsert({ id: "hub", name, roles: ["Hub", "执行节点"], os: osLabel(), version: `Steve ${config.version}` })
}

export type Runtime = { id: string; name: string; ok: boolean; version: string; path: string; models: string[]; note: string }
let cache: { at: number; list: Runtime[] } | null = null
export function runtimes(fresh = false): Runtime[] {
  if (cache && !fresh && Date.now() - cache.at < 5 * 60_000) return cache.list
  const claudeV = existsSync(config.claudeExecutable) ? run(config.claudeExecutable, ["--version"]) : ""
  const codexBin = ["/opt/homebrew/bin/codex", "/usr/local/bin/codex"].find(existsSync) ?? ""
  const codexV = codexBin ? run(codexBin, ["--version"]) : ""
  const list: Runtime[] = [
    { id: "claude-code", name: "Claude Code", path: config.claudeExecutable, version: claudeV.split(" ")[0] ?? "", models: ["claude-opus-5-5", "claude-sonnet-5-5", "claude-haiku-5-5"],
      ok: !!claudeV && existsSync(config.adapters["claude-code"]), note: !claudeV ? "没有找到 claude" : !existsSync(config.adapters["claude-code"]) ? "缺少 ACP 适配器 @agentclientprotocol/claude-agent-acp" : "通过 ACP 适配器运行" },
    { id: "codex", name: "Codex", path: codexBin || "未找到", version: codexV.replace(/^codex-cli\s*/, ""), /* ids codex-acp offers in its "model" config option (from the user's model catalog); the first is Codex's default */
      models: ["gpt-6.1-sol", "gpt-6-astra", "gpt-6-sol", "gpt-6-luna", "gpt-5.6-sol"],
      ok: !!codexV && existsSync(config.adapters.codex), note: !codexV ? "没有找到 codex" : !existsSync(config.adapters.codex) ? "缺少 ACP 适配器 @zed-industries/codex-acp" : "通过 ACP 适配器运行" },
  ]
  cache = { at: Date.now(), list }; return list
}

/* ── local harness history ── */
export type LocalSession = { key: string; harness: string; title: string; cwd: string; when: number; size: number; imported: boolean }
const steveDirs = [config.home, "/tmp/steve", "/private/tmp/steve"]
const isSteve = (cwd: string) => steveDirs.some((d) => cwd.startsWith(d))
function firstLines(file: string, maxBytes = 400_000) { const buf = readFileSync(file); return buf.subarray(0, maxBytes).toString("utf8").split("\n") }
const realUserText = (t: string) => !!t && !t.startsWith("# AGENTS.md") && !t.startsWith("<environment_context>") && !t.startsWith("<user_instructions>") && !t.startsWith("<") && !t.startsWith("Caveat:")

function claudeFiles() {
  const root = join(homedir(), ".claude/projects"); if (!existsSync(root)) return []
  return readdirSync(root).flatMap((d) => { try { return readdirSync(join(root, d)).filter((f) => f.endsWith(".jsonl")).map((f) => join(root, d, f)) } catch { return [] } })
}
function codexFiles() {
  const root = join(homedir(), ".codex/sessions"), out: string[] = []
  const walk = (d: string, depth: number) => { if (depth > 4 || !existsSync(d)) return; for (const f of readdirSync(d)) { const p = join(d, f); if (f.endsWith(".jsonl")) out.push(p); else if (depth < 3) { try { if (statSync(p).isDirectory()) walk(p, depth + 1) } catch {} } } }
  walk(root, 0); return out
}
function peekClaude(file: string): Omit<LocalSession, "imported"> | null {
  let title = "", cwd = ""
  for (const l of firstLines(file, 200_000)) {
    if (!l) continue; let m: any; try { m = JSON.parse(l) } catch { continue }
    if (m.cwd && !cwd) cwd = m.cwd
    if (m.type === "user" && typeof m.message?.content === "string" && realUserText(m.message.content)) { title = m.message.content; break }
  }
  if (!title || isSteve(cwd)) return null
  const st = statSync(file); return { key: "cc:" + file, harness: "Claude Code", title: title.slice(0, 80), cwd, when: st.mtimeMs, size: st.size }
}
function peekCodex(file: string): Omit<LocalSession, "imported"> | null {
  let title = "", cwd = ""
  for (const l of firstLines(file, 400_000)) {
    if (!l) continue; let m: any; try { m = JSON.parse(l) } catch { continue }
    if (m.type === "session_meta") cwd = m.payload?.cwd ?? ""
    if (m.type === "response_item" && m.payload?.type === "message" && m.payload.role === "user") { const t = m.payload.content?.map((c: any) => c.text ?? "").join("") ?? ""; if (realUserText(t)) { title = t; break } }
  }
  if (!title || isSteve(cwd)) return null
  const st = statSync(file); return { key: "cx:" + file, harness: "Codex", title: title.slice(0, 80), cwd, when: st.mtimeMs, size: st.size }
}
export function localSessions(limit = 60): LocalSession[] {
  const files = [...claudeFiles().map((f) => ["cc", f] as const), ...codexFiles().map((f) => ["cx", f] as const)]
    .map(([k, f]) => { try { return { k, f, t: statSync(f).mtimeMs } } catch { return null } }).filter(Boolean).sort((a, b) => b!.t - a!.t).slice(0, limit * 2) as { k: string; f: string; t: number }[]
  const done = new Set((db.prepare("SELECT import_key FROM sessions WHERE import_key IS NOT NULL").all() as { import_key: string }[]).map((r) => r.import_key))
  return files.map(({ k, f }) => (k === "cc" ? peekClaude(f) : peekCodex(f))).filter(Boolean).slice(0, limit).map((s) => ({ ...s!, imported: done.has(s!.key) }))
}

/** copy a local session into Steve as a read-only, unlinked session */
export function importLocal(ws: string, key: string) {
  const [kind, ...rest] = key.split(":"), file = rest.join(":")
  if (!existsSync(file)) throw new Error("本机记录不存在了")
  if (db.prepare("SELECT 1 FROM sessions WHERE import_key = ?").get(key)) throw new Error("已经导入过了")
  const peek = kind === "cc" ? peekClaude(file) : peekCodex(file); if (!peek) throw new Error("读不出这个会话")
  const id = uid("ses"), t = now()
  db.prepare("INSERT INTO sessions (id, workspace_id, item_id, no, bot_id, harness, model, cwd, status, token_hash, title, source, readonly, import_key, created, updated) VALUES (?, ?, NULL, 0, NULL, ?, NULL, ?, 'ended', ?, ?, ?, 1, ?, ?, ?)")
    .run(id, ws, kind === "cc" ? "claude-code" : "codex", peek.cwd, sha(uid("ro")), peek.title, "本机 " + peek.harness, key, peek.when, t)
  const add = (k: string, d: unknown) => events.add(id, k, d)
  for (const l of readFileSync(file, "utf8").split("\n")) {
    if (!l) continue; let m: any; try { m = JSON.parse(l) } catch { continue }
    if (kind === "cc") {
      if (m.type === "user" && typeof m.message?.content === "string" && realUserText(m.message.content)) add("user", { text: m.message.content, via: "me" })
      if (m.type === "assistant") for (const c of m.message?.content ?? []) {
        if (c.type === "text" && c.text) add("agent_message_chunk", { sessionUpdate: "agent_message_chunk", content: { type: "text", text: c.text } })
        if (c.type === "tool_use") add("tool_call", { sessionUpdate: "tool_call", toolCallId: c.id, title: c.name + (c.input?.command ? " " + String(c.input.command).slice(0, 120) : c.input?.file_path ? " " + c.input.file_path : ""), kind: /Read|Grep|Glob/.test(c.name) ? "read" : /Edit|Write/.test(c.name) ? "edit" : "execute", status: "completed" })
      }
    } else if (m.type === "response_item") {
      const p = m.payload
      if (p?.type === "message") { const t = p.content?.map((c: any) => c.text ?? "").join("") ?? ""; if (p.role === "user" && realUserText(t)) add("user", { text: t, via: "me" }); if (p.role === "assistant" && t) add("agent_message_chunk", { sessionUpdate: "agent_message_chunk", content: { type: "text", text: t } }) }
      if (p?.type === "custom_tool_call" || p?.type === "function_call") add("tool_call", { sessionUpdate: "tool_call", toolCallId: p.call_id ?? uid("t"), title: `${p.name} ${String(p.input ?? p.arguments ?? "").slice(0, 120)}`, kind: "execute", status: "completed" })
    }
  }
  publish({ type: "session", id }); return sessions.get(id)!
}
export function removeImported(id: string) {
  const s = sessions.get(id) as any; if (!s?.readonly) throw new Error("只能移除导入的会话")
  db.transaction(() => { db.prepare("DELETE FROM events WHERE session_id = ?").run(id); db.prepare("DELETE FROM queued WHERE session_id = ?").run(id); db.prepare("DELETE FROM sessions WHERE id = ?").run(id) })()
  publish({ type: "session", id })
}
