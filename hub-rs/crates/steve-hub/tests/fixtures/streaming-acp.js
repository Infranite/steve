// An offline ACP adapter for the conversation's integration and browser checks.
// No model, credentials or external network. Chunks arrive before the prompt response.
import { createInterface } from "node:readline"
const send = (m) => process.stdout.write(JSON.stringify({ jsonrpc: "2.0", ...m }) + "\n")
let model = "test-model", cancelled = false
const sessionId = "streaming-session"
const say = (text) => send({ method: "session/update", params: { sessionId, update: { sessionUpdate: "agent_message_chunk", content: { type: "text", text } } } })
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms))
const answer = "首段已经到达，回复还在继续生成。\n\n" +
  "### 流式输出验证\n\n- 中文与 **Markdown** 会逐段显示。\n- 生成过程中可以继续阅读和停止回复。\n\n" +
  "| 阶段 | 状态 |\n| --- | --- |\n| 接收正文 | 持续进行 |\n| 保存回复 | 完成后替换草稿 |\n\n" +
  "```ts\nconst streaming = true\nconsole.log(\"边生成边显示\")\n```\n\n" +
  Array.from({ length: 48 }, (_, n) => `第 ${n + 1} 段：这是一段长回复，正在验证内容持续增长时的阅读位置与自动滚动。\n\n`).join("") +
  "回复生成完毕。"
async function prompt(params) {
  if (model === "unavailable") throw new Error("503 temporary model failure")
  const text = params.prompt.filter((p) => p.type === "text").map((p) => p.text).join("\n")
  cancelled = false
  if (text.includes("@wire-check")) { say("答案"); return { stopReason: "end_turn" } }
  // The routing call uses this schema; return research only for an explicit test question.
  if (text.includes('"kind"') && text.includes('"confidence"') && !text.includes('"thought"')) {
    const current = JSON.parse(text.match(/User message: ("(?:\\.|[^"\\])*")/)?.[1] || '""')
    say(JSON.stringify({ kind: current.includes("测试研究") ? "research" : "answer", confidence: 1, title: "流式测试", goal: "", why: "回答问题", signals: {} }))
    return { stopReason: "end_turn" }
  }
  const body = text.includes('"thought"') ? JSON.stringify({ thought: "准备分段展示回答。", answer, offer: null }) : answer
  for (let n = 0; n < body.length && !cancelled; n += 42) {
    say(body.slice(n, n + 42))
    await sleep(120)
  }
  return { stopReason: cancelled ? "cancelled" : "end_turn", usage: { inputTokens: 10, outputTokens: 20, totalTokens: 30 } }
}
for await (const line of createInterface({ input: process.stdin })) {
  if (!line.trim()) continue
  const m = JSON.parse(line)
  if (m.method === "session/cancel") { cancelled = true; continue }
  if (m.id == null) continue
  void (async () => {
    let result = {}
    if (m.method === "initialize") result = { protocolVersion: 1, agentCapabilities: { promptCapabilities: { image: true }, mcpCapabilities: { http: true } }, agentInfo: { name: "offline-streaming-fixture", version: "1" }, authMethods: [] }
    if (m.method === "session/new" || m.method === "session/load") result = { sessionId, models: { currentModelId: model, availableModels: [] }, modes: { currentModeId: "default", availableModes: [ /* the modes Claude Code and Codex offer: it stands in for both */ { id: "default", name: "Default" }, { id: "plan", name: "Plan" }, { id: "bypassPermissions", name: "Bypass Permissions" }, { id: "read-only", name: "Read only" }, { id: "auto", name: "Auto" }, { id: "agent-full-access", name: "Full access" }] } }
    if (m.method === "session/set_model") { model = m.params.modelId; result = { models: { currentModelId: model } } }
    if (m.method === "session/prompt") result = await prompt(m.params)
    send({ id: m.id, result })
  })().catch((error) => send({ id: m.id, error: { code: -32603, message: String(error) } }))
}
