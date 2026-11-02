// Exercises model-dependent reasoning controls without calling a model or any external service.
let input = "", model = "initial", effort = "low", configured = false, substitute = false
const send = (m) => process.stdout.write(JSON.stringify({ jsonrpc: "2.0", ...m }) + "\n")
const options = () => [
  { id: "model", currentValue: model },
  { id: "thinking", category: "thought_level", currentValue: effort, options: [{ value: model === "initial" ? "low" : "high" }] },
]
process.stdin.on("data", (data) => {
  input += data
  let end
  while ((end = input.indexOf("\n")) >= 0) {
    const line = input.slice(0, end); input = input.slice(end + 1)
    if (!line) continue
    const m = JSON.parse(line)
    let result = {}
    if (m.method === "session/new" || m.method === "session/load") result = { sessionId: "fake-effort", configOptions: options() }
    if (m.method === "session/set_config_option") {
      if (m.params.configId === "model") { substitute = m.params.value === "requested-substitute"; model = "actual-model-b" }
      if (m.params.configId === "thinking") { effort = substitute ? "low" : m.params.value; configured = true }
      result = { configOptions: options() }
    }
    if (m.method === "session/prompt") {
      if (!configured) { send({ id: m.id, error: { code: -32000, message: "reasoning not configured before prompt" } }); continue }
      send({ method: "session/update", params: { update: { sessionUpdate: "usage_update", used: 4200, size: 20000 } } })
      result = { stopReason: "end_turn" }
    }
    if (m.id != null) send({ id: m.id, result })
  }
})
