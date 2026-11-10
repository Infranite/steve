#!/usr/bin/env node
// A scripted ACP adapter for research tests, without an LLM. Every process appends a line to
// `$FAKE_RESEARCH_LOG.spawns`; `session/new` goes to `.new`, `session/load` to `.load`, the config
// options it was set to to `.config` (appended), each prompt to `.prompt`.
//
// The prompt's lines that start with `@` run in order; no script: it reads `src/lib.rs` and answers.
//   @msg <id>            the next chunks are the message <id>
//   @say <text>          agent_message_chunk (`\\n` is a line break)
//   @read <path>         an ACP read of <path> (locations and rawInput.file_path), then done
//   @asking <kind> <path…> a pending tool call of <kind> on <path…> (an `execute`: the command)
//                        that never completes
//   @sleep <ms>          waits
//   @touch <path>        creates the file
//   @cat <dir> <out>     copies the first `*.txt` under <dir>/*/ to <out>
//   @mcp                 copies session/new's mcpServers to `.mcp`
//   @child               a child `sleep 30` (same process group), its pid to `<log>.pid` (written whole)
//   @wait                waits for that child
//   @hold                while `<log>.hold` exists: @child, then @wait
//   @usage <in> <out> <cacheRead> <cost>  the response's usage, and a usage_update with the cost
//   @fail <message>      the prompt fails with a JSON-RPC error
//   @failon <model> <message>  the same, only on that model
//   @stop <reason>       the turn ends with that stopReason
// A session/load answers as the session it loads.
import { appendFileSync, copyFileSync, existsSync, readdirSync, renameSync, writeFileSync } from "node:fs"
import { spawn } from "node:child_process"
import { join } from "node:path"
import readline from "node:readline"

const log = process.env.FAKE_RESEARCH_LOG
// inside a sandbox that keeps the log out of reach, nothing is logged
const put = (what, text, append = false) => {
  try { (append ? appendFileSync : writeFileSync)(`${log}.${what}`, text) } catch {}
}
const probing = process.cwd().split("/").pop() === "probe"
if (!probing) put("spawns", `${process.pid}\n`, true)
const send = (v) => process.stdout.write(`${JSON.stringify(v)}\n`)
let sid = "sess-1"
let mid = "m1"
let mcp = []
const update = (u) => send({ jsonrpc: "2.0", method: "session/update", params: { sessionId: sid, update: u } })
const say = (text) => update({ sessionUpdate: "agent_message_chunk", messageId: mid, content: { type: "text", text } })
const reply = (id, result) => send({ jsonrpc: "2.0", id, result })
const sleep = (ms) => new Promise((r) => setTimeout(r, ms))
const state = { model: "default", effort: "medium" }
const options = () => [
  { id: "model", category: "model", type: "select", currentValue: state.model, options: [] },
  { id: "effort", category: "thought_level", type: "select", currentValue: state.effort,
    options: ["low", "medium", "high", "max"].map((value) => ({ value })) },
]
let calls = 0
let child
const tool = (kind, path, done) => {
  const id = `t${++calls}`
  update({ sessionUpdate: "tool_call", toolCallId: id, kind, title: `${kind} ${path}`, status: "pending",
    locations: kind === "execute" ? [] : [{ path }],
    rawInput: kind === "execute" ? { command: path } : { file_path: path } })
  if (done) update({ sessionUpdate: "tool_call_update", toolCallId: id, status: "completed",
    content: [{ type: "content", content: { type: "text", text: "1\tfn add() {}" } }] })
}

async function run(id, prompt) {
  const lines = prompt.split("\n").filter((l) => l.startsWith("@"))
  if (!lines.length) lines.push("@msg m1", "@say 我先看看", "@read src/lib.rs", "@msg m2", "@say 答案。")
  let usage, stop = "end_turn"
  for (const l of lines) {
    const [cmd, ...a] = l.slice(1).split(" ")
    const rest = a.join(" ")
    if (cmd === "msg") mid = a[0]
    else if (cmd === "say") say(rest.replaceAll("\\n", "\n"))
    else if (cmd === "read") tool("read", a[0], true)
    else if (cmd === "asking") tool(a[0], a.slice(1).join(" "), false)
    else if (cmd === "sleep") await sleep(Number(a[0]))
    else if (cmd === "touch") writeFileSync(a[0], "")
    else if (cmd === "cat") {
      for (const d of readdirSync(a[0])) {
        const f = readdirSync(join(a[0], d)).find((n) => n.endsWith(".txt"))
        if (f) { copyFileSync(join(a[0], d, f), a[1]); break }
      }
    } else if (cmd === "mcp") put("mcp", JSON.stringify(mcp))
    else if (cmd === "child" || (cmd === "hold" && existsSync(`${log}.hold`))) {
      child = spawn("sleep", ["30"], { stdio: "ignore" })
      writeFileSync(`${log}.pid.tmp`, String(child.pid))
      renameSync(`${log}.pid.tmp`, `${log}.pid`)
      if (cmd === "hold") await new Promise((r) => child.on("exit", r))
    } else if (cmd === "wait" && child) await new Promise((r) => child.on("exit", r))
    else if (cmd === "usage") {
      usage = { inputTokens: +a[0], outputTokens: +a[1], cachedReadTokens: +a[2] }
      update({ sessionUpdate: "usage_update", used: 1, size: 100, cost: { amount: +a[3], currency: "USD" } })
    } else if (cmd === "fail") return send({ jsonrpc: "2.0", id, error: { code: -32603, message: rest } })
    else if (cmd === "failon" && state.model === a[0]) {
      return send({ jsonrpc: "2.0", id, error: { code: -32603, message: a.slice(1).join(" ") } })
    }
    else if (cmd === "stop") stop = a[0]
  }
  reply(id, { stopReason: stop, ...(usage ? { usage } : {}) })
}

for await (const line of readline.createInterface({ input: process.stdin })) {
  const m = JSON.parse(line)
  const p = m.params ?? {}
  if (m.method === "initialize") {
    reply(m.id, { protocolVersion: 1, agentCapabilities: { loadSession: true,
      promptCapabilities: { image: true }, mcpCapabilities: { http: true } } })
  } else if (m.method === "session/new" || m.method === "session/load") {
    if (m.method === "session/load") { sid = p.sessionId; if (!probing) put("load", sid) }
    else if (!probing) put("new", JSON.stringify(p))
    mcp = p.mcpServers ?? []
    reply(m.id, { sessionId: sid, modes: { currentModeId: "default",
      availableModes: ["default", "plan", "acceptEdits", "bypassPermissions"].map((id) => ({ id, name: id })) }, configOptions: options() })
  } else if (m.method === "session/set_config_option") {
    state[p.configId] = p.value
    if (!probing) put("config", `${p.configId}=${p.value}\n`, true)
    reply(m.id, { configOptions: options() })
  } else if (m.method === "session/prompt") {
    put("prompt", JSON.stringify(p.prompt))
    const text = p.prompt.filter((b) => b.type === "text").map((b) => b.text).join("\n")
    run(m.id, text)
  } else if (m.id !== undefined) {
    reply(m.id, {})
  }
}
