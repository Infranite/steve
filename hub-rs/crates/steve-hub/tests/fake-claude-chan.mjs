#!/usr/bin/env node
// A stand-in `claude` CLI for the channel smoke tests: the disclosure check passes, every other prompt
// (the reply composition) answers one short reply that needs no approval. Same output shape as contract/fake-claude.mjs.
const args = process.argv.slice(2)
if (args[0] === "--version") { console.log("9.9.9 (Claude Code)"); process.exit(0) }
const prompt = args[args.indexOf("-p") + 1] ?? ""
const result = prompt.startsWith("【披露检查】")
  ? "```json\n" + JSON.stringify({ verdict: "pass", spans: [] }) + "\n```"
  : "```json\n" + JSON.stringify({ text: "按计划进行，我稍后同步进展。", needs_approval: false, why: "回答发布安排" }) + "\n```"
console.log(JSON.stringify({ type: "result", is_error: false, result }))
