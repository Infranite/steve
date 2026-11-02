#!/usr/bin/env node
// A deterministic stand-in for the `claude` CLI, used by contract.mjs as CLAUDE_CODE_EXECUTABLE:
// `--version` and `-p <prompt> --output-format json --model <m>` with canned replies chosen by prompt shape.
const args = process.argv.slice(2)
if (args[0] === "--version") { console.log("9.9.9 (Claude Code)"); process.exit(0) }
const flag = (f) => { const i = args.indexOf(f); return i >= 0 ? args[i + 1] : undefined }
// the gateway sends a prompt's stable head as the system prompt and the rest with -p
const head = flag("--system-prompt") ?? flag("--append-system-prompt")
const rest = flag("-p") ?? ""
const prompt = head && !head.startsWith("You are a component of Steve") ? head + "\n" + rest : rest
const message = (prompt.match(/User message: (".*")/) ?? [])[1]
const said = message ? JSON.parse(message) : ""
let result
if (prompt.startsWith("You are the router")) {
  if (said.includes("聊聊")) result = { kind: "chat", title: "闲聊", goal: said, bot: "steve", project: null, tags: [], newTags: [], reply: "" }
  else if (said.includes("以后")) result = { kind: "config", title: "配置", goal: said, bot: "steve", project: null, tags: [], newTags: [], reply: "" }
  else result = { kind: "item", confidence: 0.9, signals: ["change"], title: "路由标题", goal: said || "目标", bot: "steve", project: null, tags: [], newTags: [{ name: "路由标签", cat: "主题", description: "路由建议的标签" }], reply: "好，交给 Steve。" }
} else if (prompt.startsWith("Draft a bot")) result = { name: "周报", role: "写周报", persona: "简洁", rules: "每周五提醒", model: "haiku" }
else if (prompt.startsWith("Describe this project")) result = { name: "描述名", description: "一个用于契约测试的目录" }
else if (prompt.startsWith("Which Steve configuration")) result = { target: "settings:items" }
else if (prompt.startsWith("You configure")) result = { changes: [{ key: "handoff", value: "90" }, { key: "nope", value: 1 }] }
else if (prompt.startsWith("Manage item tags")) result = { create: [{ name: "财务", cat: "领域", description: "钱的事" }], edit: [] }
else result = "这是 Steve 的回答。"
console.log(JSON.stringify({ type: "result", is_error: false, result: typeof result === "string" ? result : "```json\n" + JSON.stringify(result) + "\n```" }))
