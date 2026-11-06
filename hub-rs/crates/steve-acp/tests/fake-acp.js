// A scripted stand-in for an ACP adapter, for the steve-acp tests. Every incoming message is appended to
// $FAKE_ACP_LOG as JSON (plus a "spawn" line with argv/env). Directives in a prompt run in order:
//   @sleep <ms>            the turn takes that long (session/cancel ends it early → "cancelled")
//   @say <text>            agent_message_chunk
//   @flood [ms]            2000 chunks of 4 KiB at once; with ms, a pause of ms after every 8 (a fast
//                          harness a reader keeps up with, instead of one that overruns it)
//   @usage <used> <size> <cost> [model] [origin]   usage_update (model → _meta._claude/model)
//   @tally <in> <out> <cacheRead> <cacheWrite>   the response's `usage` (also on a cancel)
//   @quota <model>:<in>:<out>:<cr>:<cw>,…        the response's `_meta.quota.model_usage`
//   @tool <kind> <toolName> [failed]             tool_call + tool_call_update
//   @later <ms> <cost>                           after the turn, an autonomous usage_update
//   @fail <n> <message>    the first n prompts with this exact text fail with a JSON-RPC error <message>
//   @crash                 exit(3) mid-turn
//   @perm <toolName>       Claude-style request_permission; says "perm:<outcome>:<optionId>"
//   @permexec <command>    request_permission for an `execute` tool call running <command>
//   @codexperm <tool>      Codex-style MCP approval form for the steve server
//   @permtool <kind> <path> request_permission for a `kind` tool call on `path` (rawInput.file_path, locations);
//                          like a real harness, in mode bypassPermissions / full-access it asks nothing
//                          and says "perm:bypassed:"
// $FAKE_ACP_IMAGE=1 advertises promptCapabilities.image.
//   @fsread                asks the client for fs/read_text_file; says the error it got
// $FAKE_ACP_CODEX=1 reports the model as a config option instead of `models`.
// $FAKE_ACP_GRANDCHILD=<file> starts `sleep 300` (same process group) and writes its pid there.
// $FAKE_ACP_STUBBORN=<file> does the same with a `sleep 300` that ignores SIGTERM.
import { appendFileSync, writeFileSync } from "node:fs"
import { spawn } from "node:child_process"

const log = (o) => { if (process.env.FAKE_ACP_LOG) appendFileSync(process.env.FAKE_ACP_LOG, JSON.stringify(o) + "\n") }
log({ spawn: { argv: process.argv.slice(2), cwd: process.cwd(), claude: process.env.CLAUDE_CODE_EXECUTABLE ?? null, pid: process.pid } })
if (process.env.FAKE_ACP_GRANDCHILD) {
  const c = spawn("sleep", ["300"], { stdio: "ignore" })
  writeFileSync(process.env.FAKE_ACP_GRANDCHILD, String(c.pid))
}
if (process.env.FAKE_ACP_STUBBORN) {
  const c = spawn("sh", ["-c", "trap '' TERM; exec sleep 300"], { stdio: "ignore" })
  writeFileSync(process.env.FAKE_ACP_STUBBORN, String(c.pid))
}

let buf = "", sid = "", cancel = null, nextId = 1000, mode = "default"
const failures = new Map(), waiting = new Map()
const send = (m) => process.stdout.write(JSON.stringify({ jsonrpc: "2.0", ...m }) + "\n")
const update = (u) => send({ method: "session/update", params: { sessionId: sid, update: u } })
const say = (text) => update({ sessionUpdate: "agent_message_chunk", content: { type: "text", text } })
const ask = (method, params) => new Promise((res) => { const id = ++nextId; waiting.set(id, res); send({ id, method, params }) })
const sleep = (ms) => new Promise((r) => { const t = setTimeout(() => r(false), ms); cancel = () => { clearTimeout(t); r(true) } })
const options = [{ optionId: "allow", name: "Always Allow", kind: "allow_always" }, { optionId: "once", name: "Allow", kind: "allow_once" }, { optionId: "reject", name: "Reject", kind: "reject_once" }]

let toolN = 0
async function turn(text) {
  const n = (failures.get(text) ?? 0) + 1; failures.set(text, n)
  const out = { stopReason: "end_turn" }
  for (const [, cmd, arg] of text.matchAll(/@(flood|bigperm|bigreply|sleep|say|usage|tally|quota|tool|later|fail|crash|perm|permtool|permexec|codexperm|fsread)\b([^@\n]*)/g)) {
    const a = arg.trim()
    if (cmd === "sleep" && (await sleep(Number(a) || 1000))) return { ...out, stopReason: "cancelled" }
    if (cmd === "say") say(a)
    if (cmd === "flood") for (let i = 0; i < 2000; i++) {
      say("x".repeat(4096))
      if (a && i % 8 === 7 && (await sleep(Number(a)))) return { ...out, stopReason: "cancelled" }
    }
    if (cmd === "bigreply") out.padding = "x".repeat(2 << 20)
    if (cmd === "bigperm") await ask("session/request_permission", {sessionId: sid, options, toolCall: {toolCallId:"large", rawInput:{data:"x".repeat(2 << 20)}}})
    if (cmd === "usage") {
      const [used, size, cost, model, origin] = a.split(" ")
      const meta = { ...(model ? { "_claude/model": model } : {}), ...(origin ? { "_claude/origin": origin } : {}) }
      update({ sessionUpdate: "usage_update", used: Number(used), size: Number(size), cost: { amount: Number(cost), currency: "USD" }, ...(model || origin ? { _meta: meta } : {}) })
    }
    if (cmd === "tally") { const [i, o, r, w] = a.split(" ").map(Number); out.usage = { inputTokens: i, outputTokens: o, cachedReadTokens: r, cachedWriteTokens: w, totalTokens: i + o + r + w } }
    if (cmd === "quota") out._meta = { quota: { token_count: {}, model_usage: a.split(",").map((m) => { const [model, i, o, r, w] = m.split(":"); return { model, token_count: { inputTokens: +i, cachedInputTokens: +r, cachedWriteTokens: +w, outputTokens: +o, reasoningOutputTokens: 0, totalTokens: +i + +o + +r + +w } } }) } }
    if (cmd === "tool") {
      const [kind, name, failed] = a.split(" "); const id = "tool-" + ++toolN
      update({ sessionUpdate: "tool_call", toolCallId: id, kind, status: "pending", title: name, _meta: { claudeCode: { toolName: name } } })
      update({ sessionUpdate: "tool_call_update", toolCallId: id, status: failed ? "failed" : "completed" })
    }
    if (cmd === "later") { const [ms, cost] = a.split(" ").map(Number); setTimeout(() => update({ sessionUpdate: "usage_update", used: 1, size: 2, cost: { amount: cost, currency: "USD" }, _meta: { "_claude/origin": "task-notification", "_claude/model": "claude-haiku-5-5" } }), ms) }
    if (cmd === "fail") { const [k, ...msg] = a.split(" "); if (n <= Number(k)) throw new Error(msg.join(" ")) }
    if (cmd === "crash") process.exit(3)
    if (cmd === "perm") {
      const r = await ask("session/request_permission", { sessionId: sid, options, toolCall: { toolCallId: "tc-" + n, title: a, kind: "other", rawInput: { tool: a }, _meta: { claudeCode: { toolName: a } } } })
      say(`perm:${r.outcome.outcome}:${r.outcome.optionId ?? ""}`)
    }
    if (cmd === "permtool") {
      const [kind, path] = a.split(" ")
      if (mode === "bypassPermissions" || mode === "full-access") { say("perm:bypassed:"); continue }
      const r = await ask("session/request_permission", { sessionId: sid, options, toolCall: { toolCallId: `pt-${n}-${kind}`, title: `${kind} ${path}`, kind, rawInput: { file_path: path }, locations: [{ path }] } })
      say(`perm:${r.outcome.outcome}:${r.outcome.optionId ?? ""}`)
    }
    if (cmd === "permexec") {
      const r = await ask("session/request_permission", { sessionId: sid, options, toolCall: { toolCallId: `px-${n}`, title: a, kind: "execute", rawInput: { command: a } } })
      say(`perm:${r.outcome.outcome}:${r.outcome.optionId ?? ""}`)
    }
    if (cmd === "codexperm") {
      const r = await ask("session/request_permission", { sessionId: sid, options: [{ optionId: "approved", name: "Allow", kind: "allow_once" }, { optionId: "abort", name: "Deny", kind: "reject_once" }],
        toolCall: { toolCallId: "cx-" + n, title: "Approve MCP tool call", rawInput: { server_name: "steve", request: { message: `Allow the steve MCP server to run tool "${a}"?`, _meta: { codex_approval_kind: "mcp_tool_call" } } } } })
      say(`perm:${r.outcome.outcome}:${r.outcome.optionId ?? ""}`)
    }
    if (cmd === "fsread") { const r = await ask("fs/read_text_file", { sessionId: sid, path: "/etc/hosts" }); say(`fs:${r.error?.code}:${r.error?.message}`) }
  }
  return out
}

// FAKE_ACP_SHAPE=claude099: what claude-agent-acp 0.99.1 returns (modes + config options, no `models`)
let model = process.env.FAKE_ACP_CODEX ? "gpt-fake" : "kiro/claude-opus-5-5"
let effort = "medium"
const effortOpts = () => process.env.FAKE_ACP_EFFORT ? [{ id: "thought_level", category: "thought_level", type: "select", currentValue: effort, options: [{ group: "levels", options: [{ value: "low" }, { value: "medium" }, { value: "high" }] }] }] : []
const modelOpt = () => ({ id: "model", type: "select", currentValue: model, options: [{ value: "kiro/claude-opus-5-5" }, { value: "kiro/claude-sonnet-5-5" }, { value: "claude-haiku-5-5" }] })
const session = () => process.env.FAKE_ACP_CODEX
  ? { configOptions: [{ id: "model", currentValue: model, options: [] }, ...effortOpts()] }
  : process.env.FAKE_ACP_SHAPE === "claude099"
    ? { modes: { currentModeId: "default", availableModes: [] }, configOptions: [modelOpt(), ...effortOpts()] }
    : { models: { currentModelId: "fake-model", availableModels: [] } }
process.stdin.on("data", (d) => {
  buf += d
  let n
  while ((n = buf.indexOf("\n")) >= 0) {
    const line = buf.slice(0, n); buf = buf.slice(n + 1); if (!line.trim()) continue
    const m = JSON.parse(line)
    log(m)
    if (m.id != null && !m.method) { const w = waiting.get(m.id); waiting.delete(m.id); w?.(m.error ? { error: m.error } : m.result); continue }
    if (m.method === "initialize") send({ id: m.id, result: { protocolVersion: 1, agentCapabilities: { loadSession: true, ...(process.env.FAKE_ACP_IMAGE ? { promptCapabilities: { image: true } } : {}) } } })
    else if (m.method === "session/new") { sid = "fake-" + Math.random().toString(36).slice(2); const opened = () => setTimeout(() => send({ id: m.id, result: { sessionId: sid, ...session() } }), Number(process.env.FAKE_ACP_OPEN_DELAY || 0)); if (process.env.FAKE_ACP_OPEN_PERMISSION) ask("session/request_permission", {toolCall: {toolCallId:"opening", kind:"execute"}, options:[]}).then(opened); else opened() }
    else if (m.method === "session/load") { sid = m.params.sessionId; say("replayed history"); send({ id: m.id, result: session() }) }
    else if (m.method === "session/prompt") turn(m.params.prompt.map((p) => p.text).join("\n")).then((result) => send({ id: m.id, result }), (e) => send({ id: m.id, error: { code: -32000, message: e.message } }))
    else if (m.method === "session/cancel") cancel?.()
    else if (m.method === "session/set_mode") { mode = m.params.modeId; send({ id: m.id, result: {} }) }
    else if (m.method === "session/set_model" && process.env.FAKE_ACP_NO_SET_MODEL) send({ id: m.id, error: { code: -32601, message: "\"Method not found\": session/set_model", data: { method: "session/set_model" } } })
    else if (m.method === "session/set_config_option" && m.params.configId === "model") {
      if (!process.env.FAKE_ACP_CODEX && !modelOpt().options.some((o) => o.value === m.params.value)) send({ id: m.id, error: { code: -32602, message: `Invalid params: Invalid value for config option model: ${m.params.value}` } })
      else { model = m.params.value; send({ id: m.id, result: { configOptions: [modelOpt(), ...effortOpts()] } }) }
    }
    else if (m.method === "session/set_config_option" && m.params.configId === "thought_level") {
      effort = process.env.FAKE_ACP_EFFORT_SUBSTITUTE || m.params.value
      send({ id: m.id, result: { configOptions: [modelOpt(), ...effortOpts()] } })
    }
    else if (m.id != null) send({ id: m.id, result: {} })
  }
})
// noise that must be skipped by the client
process.stdout.write("not json\n\n")
