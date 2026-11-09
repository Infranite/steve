#!/usr/bin/env node
// A stand-in harness adapter (ACP) for the channel smoke tests: the disclosure check passes, every
// other prompt (the reply composition) answers one short reply that needs no approval.
import { serve } from "./contract/acp-stub.mjs"

serve((head, rest) => {
  const prompt = head && !head.startsWith("You are a component of Steve") ? head + "\n" + rest : rest
  return prompt.startsWith("【披露检查】")
    ? "```json\n" + JSON.stringify({ verdict: "pass", spans: [] }) + "\n```"
    : "```json\n" + JSON.stringify({ text: "按计划进行，我稍后同步进展。", needs_approval: false, why: "回答发布安排" }) + "\n```"
})
