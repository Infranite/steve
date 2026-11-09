// A minimal ACP adapter for the Hub's tests: `serve(reply)` answers every `session/prompt` with
// `reply(prompt)`, where `prompt` is the session's system prompt (if any) and the prompt's text.
// It offers a read-only mode, a model and an effort option (whatever it is set to is in force), and
// takes images and a system prompt of the client's own. `--version` prints a version and exits.
import { createInterface } from "node:readline"

export function serve(reply) {
  if (process.argv[2] === "--version") { console.log("9.9.9 (stub)"); process.exit(0) }
  const send = (o) => process.stdout.write(JSON.stringify({ jsonrpc: "2.0", ...o }) + "\n")
  const state = { system: "", model: "default", effort: "medium" }
  const options = () => [
    { id: "model", category: "model", type: "select", currentValue: state.model, options: [] },
    { id: "effort", category: "thought_level", type: "select", currentValue: state.effort,
      options: ["low", "medium", "high", "xhigh", "max"].map((value) => ({ value })) },
  ]
  createInterface({ input: process.stdin }).on("line", (line) => {
    const msg = JSON.parse(line)
    const p = msg.params ?? {}
    let result = {}
    if (msg.method === "initialize") {
      result = { protocolVersion: 1, agentCapabilities: { promptCapabilities: { image: true },
        sessionCapabilities: { systemPrompt: { mode: { override: {}, append: {} } } } } }
    } else if (msg.method === "session/new") {
      state.system = (p.systemPrompt?.content ?? []).map((b) => b.text).join("\n\n")
      result = { sessionId: "s1", modes: { currentModeId: "default", availableModes: [{ id: "default" }, { id: "plan" }] }, configOptions: options() }
    } else if (msg.method === "session/set_config_option") {
      state[p.configId] = p.value
      result = { configOptions: options() }
    } else if (msg.method === "session/prompt") {
      const text = (p.prompt ?? []).filter((b) => b.type === "text").map((b) => b.text).join("\n\n")
      const out = reply(state.system, text)
      send({ method: "session/update", params: { sessionId: p.sessionId, update: { sessionUpdate: "agent_message_chunk", content: { type: "text", text: out } } } })
      result = { stopReason: "end_turn" }
    }
    if (msg.id !== undefined) send({ id: msg.id, result })
  })
}
