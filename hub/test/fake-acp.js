// A scripted stand-in for an ACP adapter, for tests that need agent turns without a model.
// Every prompt is appended to $FAKE_ACP_LOG as {session, text}. Directives anywhere in a prompt run in order:
//   @sleep <ms>               the turn takes that long (session/cancel ends it early)
//   @commit <file> <text>     write the file in the session's cwd and commit it
//   @deliver                  call steve_deliver over the session's Steve MCP endpoint
// and a prompt starting with “验收通过” makes it call steve_publish for the branch it is on.
import { appendFileSync, writeFileSync } from "node:fs"
import { execFileSync } from "node:child_process"
import { join } from "node:path"

const cwd = process.cwd(), log = process.env.FAKE_ACP_LOG
let buf = "", mcp = null, sid = "", cancel = null
const send = (m) => process.stdout.write(JSON.stringify({ jsonrpc: "2.0", ...m }) + "\n")
const sleep = (ms) => new Promise((r) => { const t = setTimeout(() => r(false), ms); cancel = () => { clearTimeout(t); r(true) } })
const git = (...a) => execFileSync("git", ["-C", cwd, ...a], { stdio: "pipe", encoding: "utf8" }).trim()

async function tool(name, args) {
  const r = await fetch(mcp.url, { method: "POST", headers: { "content-type": "application/json", accept: "application/json, text/event-stream", ...mcp.headers },
    body: JSON.stringify({ jsonrpc: "2.0", id: 1, method: "tools/call", params: { name, arguments: args } }) })
  const t = await r.text(); if (!r.ok) throw new Error(`${name}: ${r.status} ${t.slice(0, 200)}`)
}
const say = (text) => send({ method: "session/update", params: { sessionId: sid, update: { sessionUpdate: "agent_message_chunk", content: { type: "text", text } } } })

async function turn(text) {
  if (log) appendFileSync(log, JSON.stringify({ session: sid, text }) + "\n")
  for (const [, cmd, arg] of text.matchAll(/@(sleep|commit|deliver)\b([^@\n]*)/g)) {
    const a = arg.trim()
    if (cmd === "sleep" && (await sleep(Number(a) || 1000))) return "cancelled"
    if (cmd === "commit") { const [file, ...rest] = a.split(" "); writeFileSync(join(cwd, file), rest.join(" ") + "\n"); git("add", file); git("commit", "-q", "-m", "fake: " + file) }
    if (cmd === "deliver") await tool("steve_deliver", { summary: "fake 交付", outputs: [], checks: [{ name: "fake", passed: true }] })
  }
  if (text.startsWith("验收通过")) await tool("steve_publish", { branch: git("rev-parse", "--abbrev-ref", "HEAD"), summary: "fake 发布" })
  say("ok")
  return "end_turn"
}

const headersOf = (servers) => { const s = servers?.find((x) => x.name === "steve"); return s ? { url: s.url, headers: Object.fromEntries(s.headers.map((h) => [h.name, h.value])) } : null }
process.stdin.on("data", (d) => {
  buf += d
  let n
  while ((n = buf.indexOf("\n")) >= 0) {
    const line = buf.slice(0, n); buf = buf.slice(n + 1); if (!line.trim()) continue
    const m = JSON.parse(line)
    if (m.method === "initialize") send({ id: m.id, result: { protocolVersion: 1, agentCapabilities: { loadSession: true } } })
    else if (m.method === "session/new") { sid = "fake-" + Math.random().toString(36).slice(2); mcp = headersOf(m.params.mcpServers); send({ id: m.id, result: { sessionId: sid } }) }
    else if (m.method === "session/load") { sid = m.params.sessionId; mcp = headersOf(m.params.mcpServers); send({ id: m.id, result: {} }) }
    else if (m.method === "session/prompt") turn(m.params.prompt.map((p) => p.text).join("\n")).then((stopReason) => send({ id: m.id, result: { stopReason } }), (e) => send({ id: m.id, error: { code: -32000, message: e.message } }))
    else if (m.method === "session/cancel") cancel?.()
    else if (m.id != null) send({ id: m.id, result: {} })
  }
})
