#!/usr/bin/env node
// A deterministic stand-in harness adapter (ACP), used by the Hub's tests for every
// adapter: canned replies chosen by prompt shape. The gateway sends a prompt's stable head as the
// session's system prompt (or Steve's fixed one) and the rest as the prompt.
import { serve } from "./acp-stub.mjs"

serve((head, rest) => {
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
  return typeof result === "string" ? result : "```json\n" + JSON.stringify(result) + "\n```"
})
