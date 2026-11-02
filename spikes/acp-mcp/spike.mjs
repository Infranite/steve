// Step 1 spike: can a harness ACP adapter (a) take Steve's MCP tools via session/new, (b) call them,
// (c) ask for permission, (d) report usage, (e) be resumed with session/load after the process dies?
// Usage: node spike.mjs <claude|codex> <workdir>
//   CODEX_ACP_ARGS='["-c","model_catalog_json=\"/path/models.json\""]'  extra adapter args (codex)
//   SPIKE_MODE=read-only SPIKE_MODEL=gpt-6-sol                       set mode / model before the prompt
import { spawn } from "node:child_process"
import http from "node:http"
import { randomUUID } from "node:crypto"
import { execSync } from "node:child_process"
import { McpServer } from "@modelcontextprotocol/sdk/server/mcp.js"
import { StreamableHTTPServerTransport } from "@modelcontextprotocol/sdk/server/streamableHttp.js"
import { z } from "zod"

const which = process.argv[2] ?? "claude", cwd = process.argv[3] ?? "/tmp/steve-spike-ws"
const G = execSync("npm root -g").toString().trim()
const ADAPTER = { claude: [process.execPath, [`${G}/@agentclientprotocol/claude-agent-acp/dist/index.js`]], codex: [process.execPath, [`${G}/@zed-industries/codex-acp/bin/codex-acp.js`, ...JSON.parse(process.env.CODEX_ACP_ARGS ?? "[]")]] }[which]
const report = { harness: which, checks: {}, notes: [] }
const ok = (k, v = true) => { report.checks[k] = v; console.error(`✓ ${k}`, typeof v === "object" ? JSON.stringify(v).slice(0, 200) : v) }

/* ── Steve MCP server (HTTP, one bearer token per session) ── */
const TOKEN = randomUUID(), calls = []
function mcp() {
  const s = new McpServer({ name: "steve", version: "0.0.1" })
  s.tool("steve_update", "Report what you are doing now and your plan to Steve. Call this at the start and whenever the plan changes.",
    { now: z.string(), plan: z.array(z.object({ step: z.string(), done: z.boolean() })) }, async (a) => { calls.push(["steve_update", a]); return { content: [{ type: "text", text: "noted" }] } })
  s.tool("steve_ask", "Ask the user a question and wait for the answer.", { question: z.string(), options: z.array(z.string()) },
    async (a) => { calls.push(["steve_ask", a]); return { content: [{ type: "text", text: `user answered: ${a.options[0]}` }] } })
  s.tool("steve_deliver", "Deliver the result of this item for the user's acceptance.", { summary: z.string(), outputs: z.array(z.string()) },
    async (a) => { calls.push(["steve_deliver", a]); return { content: [{ type: "text", text: "delivered; waiting for acceptance" }] } })
  return s
}
const srv = http.createServer(async (req, res) => {
  if (req.headers.authorization !== `Bearer ${TOKEN}`) { res.writeHead(401).end(); report.notes.push("mcp: request without token"); return }
  const t = new StreamableHTTPServerTransport({ sessionIdGenerator: undefined }); const s = mcp()
  res.on("close", () => { t.close(); s.close() })
  await s.connect(t); await t.handleRequest(req, res)
})
await new Promise((r) => srv.listen(0, "127.0.0.1", r)); const MCP_URL = `http://127.0.0.1:${srv.address().port}/mcp`

/* ── minimal ACP client over stdio JSON-RPC ── */
function start() {
  const p = spawn(ADAPTER[0], ADAPTER[1], { cwd, stdio: ["pipe", "pipe", "pipe"], env: { ...process.env } })
  let buf = "", id = 0; const pending = new Map(), events = []
  p.stderr.on("data", (d) => { report.notes.push("stderr: " + String(d).slice(0, 300)); console.error("STDERR", String(d).slice(0, 500)) }); p.on("exit", (c) => { console.error("ADAPTER EXIT", c); for (const q of pending.values()) q.rej(new Error("adapter exited " + c)); pending.clear() })
  const send = (m) => p.stdin.write(JSON.stringify({ jsonrpc: "2.0", ...m }) + "\n")
  const request = (method, params, ms = 240000) => new Promise((res, rej) => { const i = ++id; pending.set(i, { res, rej }); send({ id: i, method, params }); setTimeout(() => pending.has(i) && (pending.delete(i), rej(new Error("timeout " + method))), ms) })
  p.stdout.on("data", (d) => {
    buf += d; let n
    while ((n = buf.indexOf("\n")) >= 0) {
      const line = buf.slice(0, n); buf = buf.slice(n + 1); if (!line.trim()) continue
      const m = JSON.parse(line)
      if (m.id != null && (m.result !== undefined || m.error) && pending.has(m.id)) { const q = pending.get(m.id); pending.delete(m.id); m.error ? q.rej(Object.assign(new Error(m.error.message), { data: m.error })) : q.res(m.result); continue }
      if (m.method === "session/update") { const u = m.params.update; if (!/chunk$/.test(u.sessionUpdate)) console.error("UPDATE", JSON.stringify(u).slice(0, 600)); events.push(u); continue }
      if (m.method === "session/request_permission") { console.error("PERMISSION", JSON.stringify(m.params).slice(0, 1500)); events.push({ sessionUpdate: "permission", req: m.params }); const opt = m.params.options.find((o) => o.kind?.startsWith("allow")) ?? m.params.options[0]; send({ id: m.id, result: { outcome: { outcome: "selected", optionId: opt.optionId } } }); continue }
      if (m.method === "fs/read_text_file") { import("node:fs").then((fs) => send({ id: m.id, result: { content: fs.readFileSync(m.params.path, "utf8") } })); continue }
      if (m.method === "fs/write_text_file") { import("node:fs").then((fs) => { fs.writeFileSync(m.params.path, m.params.content); send({ id: m.id, result: null }) }); continue }
      if (m.id != null && m.method) { report.notes.push("unhandled request " + m.method); send({ id: m.id, error: { code: -32601, message: "not supported in spike" } }) }
    }
  })
  return { p, request, events }
}

const mcpServers = [{ type: "http", name: "steve", url: MCP_URL, headers: [{ name: "Authorization", value: `Bearer ${TOKEN}` }] }]
let a = start()
const init = await a.request("initialize", { protocolVersion: 1, clientCapabilities: { fs: { readTextFile: true, writeTextFile: true }, terminal: false } })
ok("initialize", { protocol: init.protocolVersion, agent: init.agentInfo?.name, loadSession: init.agentCapabilities?.loadSession, mcpHttp: init.agentCapabilities?.mcpCapabilities?.http, auth: init.authMethods?.map((m) => m.id) })
const ses = await a.request("session/new", { cwd, mcpServers })
ok("session/new", { sessionId: ses.sessionId, models: ses.models?.availableModels?.map((m) => m.modelId), current: ses.models?.currentModelId, modes: ses.modes?.availableModes?.map((m) => m.id), currentMode: ses.modes?.currentModeId, configOptions: ses.configOptions?.map((o) => o.id) }); console.error("SESSION_NEW", JSON.stringify(ses))
if (process.env.SPIKE_MODE) ok("set_mode " + process.env.SPIKE_MODE, await a.request("session/set_mode", { sessionId: ses.sessionId, modeId: process.env.SPIKE_MODE }).then(() => true, (e) => "FAILED: " + e.message))
if (process.env.SPIKE_MODEL) {
  ok("set_model", await a.request("session/set_model", { sessionId: ses.sessionId, modelId: process.env.SPIKE_MODEL }).then(() => true, (e) => "FAILED: " + e.message))
  ok("set_config_option model", await a.request("session/set_config_option", { sessionId: ses.sessionId, configId: "model", value: process.env.SPIKE_MODEL }).then((r) => r?.configOptions?.find((o) => o.id === "model")?.currentValue ?? true, (e) => "FAILED: " + e.message))
}
const t0 = Date.now()
const r1 = await a.request("session/prompt", { sessionId: ses.sessionId, prompt: [{ type: "text", text:
  "This is a protocol test. Do exactly this, nothing else: 1) call the MCP tool steve_update with now='testing tools' and a two-step plan. 2) create a file hello.txt containing 'hi' in the current directory. 3) call steve_ask with question 'pick one' and options ['alpha','beta']. 4) call steve_deliver with summary 'done' and outputs ['hello.txt']. Then reply with the single word FINISHED." }] })
ok("prompt round 1", { stopReason: r1.stopReason, seconds: Math.round((Date.now() - t0) / 1000), usage: r1.usage ?? null })
ok("steve tool calls", calls.map((c) => c[0]))
const kinds = {}; a.events.forEach((e) => { kinds[e.sessionUpdate] = (kinds[e.sessionUpdate] ?? 0) + 1 }); ok("update kinds", kinds)
const usage = a.events.filter((e) => e.sessionUpdate === "usage_update" || e.used != null); ok("usage reported", usage.length ? usage[usage.length - 1] : false)
ok("permission requests", a.events.filter((e) => e.sessionUpdate === "permission").map((e) => e.req.toolCall?.title ?? e.req.toolCall?.kind))
import("node:fs").then((fs) => ok("file written", fs.existsSync(cwd + "/hello.txt")))
/* kill the adapter, start a fresh one, load the same session */
a.p.kill("SIGKILL"); await new Promise((r) => setTimeout(r, 800)); calls.length = 0
const b = start(); await b.request("initialize", { protocolVersion: 1, clientCapabilities: { fs: { readTextFile: true, writeTextFile: true }, terminal: false } })
try {
  await b.request("session/load", { sessionId: ses.sessionId, cwd, mcpServers }); ok("session/load after kill", { replayed: b.events.length })
  const r2 = await b.request("session/prompt", { sessionId: ses.sessionId, prompt: [{ type: "text", text: "What option did the user pick earlier? Answer with one word, then call steve_update with now='resumed' and an empty plan." }] })
  const said = b.events.filter((e) => e.sessionUpdate === "agent_message_chunk").map((e) => e.content?.text ?? "").join("")
  ok("resume remembers context", { stopReason: r2.stopReason, said: said.slice(-80), toolAfterResume: calls.map((c) => c[0]) })
} catch (e) { ok("session/load after kill", "FAILED: " + e.message) }
b.p.kill(); srv.close()
console.log(JSON.stringify(report, null, 1)); process.exit(0)
