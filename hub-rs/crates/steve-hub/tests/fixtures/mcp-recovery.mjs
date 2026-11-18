#!/usr/bin/env node
// An offline ACP adapter. Only the first work session omits Steve's MCP; its successor really
// initializes/lists/calls the Node endpoint. No provider, model, account or private host is used.
import { createInterface } from "node:readline"
import { existsSync, writeFileSync } from "node:fs"
import { join } from "node:path"

if (process.argv[2] === "--version") { console.log("1.0.0 fixture"); process.exit(0) }
const send = m => process.stdout.write(JSON.stringify({ jsonrpc: "2.0", ...m }) + "\n")
const sid = "mcp-fixture-session"
let cwd, servers = [], cancel
const say = text => send({ method: "session/update", params: { sessionId: sid, update: {
  sessionUpdate: "agent_message_chunk", content: { type: "text", text }
} } })
async function rpc(server, method, params = {}) {
  const response = await fetch(server.url, { method: "POST", headers: {
    ...Object.fromEntries((server.headers || []).map(h => [h.name, h.value])),
    "content-type": "application/json", accept: "application/json, text/event-stream",
    "mcp-protocol-version": "2025-06-18"
  }, body: JSON.stringify({ jsonrpc: "2.0", id: method, method, params }), signal: AbortSignal.timeout(15000) })
  const text = await response.text()
  if (!response.ok) throw Error(`${method}: ${response.status} ${text}`)
  const result = JSON.parse(text.startsWith("data:") ? text.split("\n").find(l => l.startsWith("data:")).slice(5) : text)
  if (result.error || result.result?.isError) throw Error(`${method}: ${JSON.stringify(result)}`)
  return result.result
}
async function prompt(p) {
  const text = (p.prompt || []).filter(b => b.type === "text").map(b => b.text).join("\n")
  if (!text.includes("MCP recovery acceptance")) { say("{}"); return { stopReason: "end_turn" } }
  const marker = join(cwd, "mcp-first-session.txt")
  if (!existsSync(marker)) {
    writeFileSync(marker, "The existing work must survive session recovery.")
    say("The adapter opened successfully, but Steve MCP tools are undefined.")
    if (text.includes("startup-watchdog")) {
      await new Promise(resolve => { cancel = resolve })
      return { stopReason: "cancelled" }
    }
    return { stopReason: "end_turn" }
  }
  const server = servers.find(s => s.name === "steve")
  if (!server) throw Error("replacement has no Steve MCP configuration")
  await rpc(server, "initialize", { protocolVersion: "2025-06-18", capabilities: {}, clientInfo: { name: "attachment-acceptance", version: "1" } })
  const list = await rpc(server, "tools/list")
  for (const name of ["steve_update", "steve_ask", "steve_deliver"]) {
    if (!list.tools.some(t => t.name === name)) throw Error(`missing ${name}`)
  }
  await rpc(server, "tools/call", { name: "steve_update", arguments: {
    now: "新会话已实际调用 Steve 工具", plan: [{ step: "恢复 MCP 工具挂载", done: true }]
  } })
  await rpc(server, "tools/call", { name: "steve_deliver", arguments: {
    summary: "MCP recovery acceptance: replacement attached and called Steve tools.",
    outputs: [], checks: [{ name: "MCP initialize, tools/list and steve_update", passed: true }], unfinished: []
  } })
  say("MCP recovery acceptance completed.")
  return { stopReason: "end_turn" }
}
createInterface({ input: process.stdin }).on("line", line => {
  const m = JSON.parse(line), p = m.params || {}
  if (m.method === "session/cancel") { cancel?.(); cancel = null }
  if (m.id == null) return
  void (async () => {
    let result = {}
    if (m.method === "initialize") result = { protocolVersion: 1, agentCapabilities: { mcpCapabilities: { http: true } } }
    if (m.method === "session/new" || m.method === "session/load") {
      cwd = p.cwd; servers = p.mcpServers || []
      result = { sessionId: sid, modes: { currentModeId: "default", availableModes: ["default", "plan", "acceptEdits", "bypassPermissions", "read-only", "auto", "agent-full-access"].map(id => ({ id, name: id })) } }
    }
    if (m.method === "session/prompt") result = await prompt(p)
    send({ id: m.id, result })
  })().catch(error => send({ id: m.id, error: { code: -32603, message: String(error) } }))
})
