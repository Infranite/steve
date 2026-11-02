// Shared by the e2e tests: dev login, JSON calls, polling, and a throwaway project + bot to work in.
import { mkdtempSync, realpathSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"

export const base = process.argv[2] ?? "http://127.0.0.1:8441"
let cookie = ""
export async function login() {
  const r = await fetch(base + "/auth/dev", { redirect: "manual" }); cookie = (r.headers.get("set-cookie") ?? "").split(";")[0]
  if (!cookie) throw new Error("dev login failed " + r.status)
}
/** the session cookie, for raw fetches (uploads, downloads) */
export const cookieHeader = () => cookie
/** ws: run the call in that workspace (x-steve-ws), otherwise the default one */
export async function call(method, path, body, ws) {
  const x = await fetch(base + path, { method, headers: { cookie, "content-type": "application/json", ...(ws ? { "x-steve-ws": ws } : {}) }, body: body ? JSON.stringify(body) : undefined })
  const t = await x.text(); let v; try { v = JSON.parse(t) } catch { v = t }
  if (!x.ok) throw Object.assign(new Error(`${method} ${path} → ${x.status} ${t.slice(0, 200)}`), { status: x.status })
  return v
}
/** the HTTP status of a call that is expected to fail */
export const statusOf = (method, path, body, ws) => call(method, path, body, ws).then(() => 200, (e) => e.status ?? 0)
export const sleep = (ms) => new Promise((r) => setTimeout(r, ms))
export const log = (...a) => console.log(new Date().toISOString().slice(11, 19), ...a)
export async function waitFor(pred, what, ms = 300000) { const t0 = Date.now(); while (Date.now() - t0 < ms) { try { const v = await pred(); if (v) return v } catch {} await sleep(1500) } throw new Error("timeout waiting for " + what) }
export const openOf = async (id) => (await call("GET", "/api/items/" + id)).decisions.filter((d) => d.status === "open")
export const check = (ok, what) => { if (!ok) throw new Error("check failed: " + what); log("✓", what) }

/** a scratch directory as a project, and a bot that may edit files there (perm_mode acceptEdits) but still has to ask for anything else */
export async function setup(tag) {
  const dir = realpathSync(mkdtempSync(join(tmpdir(), `steve-e2e-${tag}-`)))
  const project = await call("POST", "/api/projects", { name: "e2e-" + tag + "-" + Date.now().toString(36), path: dir, description: "e2e 测试用的临时目录" })
  const bot = await call("POST", "/api/bots", { name: "e2e-" + tag, role: "e2e 测试", harness: process.env.E2E_HARNESS ?? "claude-code", model: process.env.E2E_HARNESS === "codex" ? (process.env.E2E_MODEL ?? null) : process.env.E2E_MODEL ?? "claude-opus-5-5", perm_mode: "acceptEdits", accept_mode: "manual", default_project: project.id })
  log("project", project.id, dir, "| bot", bot.id, bot.model)
  return { dir, project, bot }
}
/** a tool permission request is only allowed when it is a file edit inside the test directory; anything else (a shell command, a path outside) is denied */
export async function answerPermission(d, dir) {
  let input = {}; try { input = JSON.parse(d.draft) } catch {}
  const path = String(input.file_path ?? input.path ?? "")
  const ok = d.subject_type === "tool_call" && path.startsWith(dir + "/")
  const opt = d.options.map((o) => o.split("|")).find((o) => (ok ? o[2] === "allow_once" || o[2]?.startsWith("allow") : o[2]?.startsWith("reject")))
  await call("POST", "/api/decisions/" + d.id, { choice: opt?.[0] ?? "deny" })
  log(ok ? "allowed" : "denied", d.question, path)
  return ok
}
/** an open decision on the item: accept is returned; permissions are inspected; a stall after a denial gets one nudge, anything else fails */
let nudged = 0
export async function handle(d, dir) {
  if (d.kind === "accept") return d
  if (d.kind === "approve" && d.subject_type === "tool_call") { await answerPermission(d, dir); return null }
  if (d.kind === "fail" && nudged++ < 2) { await call("POST", "/api/decisions/" + d.id, { choice: "continue", text: "请用 Write 工具写文件（shell 命令不会被批准），然后用 steve_deliver 交付。" }); log("nudged:", d.question.slice(0, 120)); return null }
  throw new Error(`unexpected decision ${d.kind}: ${d.question}`)
}
