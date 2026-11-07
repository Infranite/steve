// A protocol peer, without an LLM. All names are arbitrary to exercise the ACP baseline.
import readline from "node:readline";

const send = value => process.stdout.write(`${JSON.stringify(value)}\n`);
const update = value => send({
  jsonrpc: "2.0", method: "session/update",
  params: { sessionId: "research-session", update: value },
});
const text = value => update({ sessionUpdate: "agent_message_chunk", content: { type: "text", text: value } });
const reply = (id, result) => send({ jsonrpc: "2.0", id, result });
for await (const line of readline.createInterface({ input: process.stdin })) {
  const req = JSON.parse(line);
  if (req.method === "initialize") {
    reply(req.id, { protocolVersion: 1, agentCapabilities: {} });
  } else if (req.method === "session/new" || req.method === "session/load") {
    reply(req.id, { sessionId: "research-session", modes: {
      currentModeId: "read-only", availableModes: [{ id: "read-only", name: "Read only" }],
    } });
  } else if (req.method === "session/prompt") {
    const prompt = req.params.prompt.filter(p => p.type === "text").map(p => p.text).join("\n");
    if (prompt === "idle") continue;
    if (prompt === "thought") {
      let count = 0;
      const timer = setInterval(() => {
        update({ sessionUpdate: "agent_thought_chunk", content: { type: "text", text: "private-thought" } });
        if (++count === 6) {
          clearInterval(timer);
          text("完成");
          reply(req.id, { stopReason: "end_turn" });
        }
      }, 200);
      continue;
    }
    text("我先");
    if (prompt === "cancel") continue;
    if (prompt.startsWith("mcp_")) {
      const input = prompt === "mcp_title_only" ? { id: "ST-1" } : {
        server: prompt === "mcp_external" ? "other" : "steve",
        tool: prompt === "mcp_write" ? "steve_deliver" : prompt === "mcp_nested_name" ? "mcp__steve__steve_item_read" : "steve_item_read",
        arguments: { id: "ST-1" },
      };
      // Real ACP MCP notification shape: identity lives in rawInput, never in the title.
      update({ sessionUpdate: "tool_call", toolCallId: "mcp-1",
        title: "Tool: steve/steve_item_read", status: "in_progress", rawInput: input });
      update({ sessionUpdate: "tool_call_update", toolCallId: "mcp-1", status: "completed",
        rawOutput: { content: [{ type: "text", text: "item progress" }] } });
      text("已查询事项进度");
      reply(req.id, { stopReason: "end_turn" });
      continue;
    }
    const bad = prompt === "violation";
    for (let i = 0; i < (prompt === "tools" ? 3 : 1); i++) {
      update({
        sessionUpdate: "tool_call", toolCallId: `call-${i}`,
        name: "arbitrary_reader", kind: bad ? "edit" : "read", status: "completed",
        locations: [{ path: "src/lib.rs" }], rawInput: { path: "src/lib.rs" },
      });
    }
    setTimeout(() => text("答案第一段"), 100);
    setTimeout(() => text("，答案第二段。"), 300);
    setTimeout(() => {
      update({ sessionUpdate: "usage_update", used: 42, size: 1000, cost: { amount: 0.01, currency: "USD" } });
      reply(req.id, { stopReason: "end_turn" });
    }, 600);
  } else {
    reply(req.id, {});
  }
}
