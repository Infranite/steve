// Offline research peer: one model fails, and saved sessions have expired when loaded again.
import { createInterface } from "node:readline"
const send = (m) => process.stdout.write(JSON.stringify({ jsonrpc: "2.0", ...m }) + "\n")
let model = ""
for await (const line of createInterface({ input: process.stdin })) {
  const m = JSON.parse(line)
  if (m.id == null) continue
  if (m.method === "session/load") {
    send({ id: m.id, error: { code: -32602, message: "session expired" } })
    continue
  }
  let result = {}
  if (m.method === "initialize") result = { protocolVersion: 1, agentCapabilities: {} }
  if (m.method === "session/new") result = {
    sessionId: "expired-session",
    modes: { currentModeId: "plan", availableModes: [{ id: "plan", name: "Plan" }] },
  }
  if (m.method === "session/set_model") {
    model = m.params.modelId
    result = { models: { currentModelId: model } }
  }
  if (m.method === "session/prompt") {
    if (model === "unavailable") {
      send({ id: m.id, error: { code: -32603, message: "503 unavailable" } })
      continue
    }
    const text = m.params.prompt.filter(p => p.type === "text").map(p => p.text).join("\n")
    if (model === "rich") {
      send({ method: "session/update", params: { sessionId: "expired-session", update: {
        sessionUpdate: "plan",
        entries: [{ content: "Inspect repository", status: "in_progress" }, { content: "Summarize", status: "pending" }],
      } } })
      send({ method: "session/update", params: { sessionId: "expired-session", update: {
        sessionUpdate: "agent_thought_chunk",
        content: { type: "text", text: "Checking main implementation" },
      } } })
      send({ method: "session/update", params: { sessionId: "expired-session", update: {
        sessionUpdate: "tool_call",
        toolCallId: "t1",
        kind: "read",
        title: "Read file",
        status: "in_progress",
        rawInput: { file_path: "src/lib.rs", description: "Read crate entrypoint" },
      } } })
      send({ method: "session/update", params: { sessionId: "expired-session", update: {
        sessionUpdate: "tool_call_update",
        toolCallId: "t1",
        status: "completed",
        content: [{ type: "content", content: { type: "text", text: "pub fn run() {}" } }],
      } } })
    }
    send({ method: "session/update", params: { sessionId: "expired-session", update: {
      sessionUpdate: "agent_message_chunk", content: {
        type: "text", text: model === "rich" ? "rich answer" : (text.includes("Steve：answer") ? "fresh answer with prior context" : "answer"),
      },
    } } })
    if (model === "slow") continue
    result = { stopReason: "end_turn", usage: { inputTokens: 4, outputTokens: 2 } }
  }
  send({ id: m.id, result })
}
