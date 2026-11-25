// A protocol peer, without an LLM. All names are arbitrary to exercise the ACP baseline.
import readline from "node:readline";
import fs from "node:fs";
import path from "node:path";

const send = value => process.stdout.write(`${JSON.stringify(value)}\n`);
const update = value => send({
  jsonrpc: "2.0", method: "session/update",
  params: { sessionId: "research-session", update: value },
});
const text = value => update({ sessionUpdate: "agent_message_chunk", content: { type: "text", text: value } });
const reply = (id, result) => send({ jsonrpc: "2.0", id, result });
const barriers = new Map();
const control = process.argv[2];
const barrier = step => {
  const id = `fixture-barrier-${step}`;
  barriers.set(id, step);
  // FIFO after the preceding update, but this unavailable client RPC causes neither activity
  // nor a tool/event. Its error response proves Node has consumed the update.
  send({ jsonrpc: "2.0", id, method: "fixture/barrier", params: {} });
};
const controlled = (id, thought) => {
  fs.mkdirSync(control, { recursive: true });
  let step = 0;
  // Filesystem notifications can be coalesced or missed. The command remains in this mailbox
  // until its ACP round trip is acknowledged, so observe its state with a sleeping timer.
  const poller = setInterval(() => {
    const file = path.join(control, "release");
    if (!fs.existsSync(file)) return;
    const command = JSON.parse(fs.readFileSync(file, "utf8"));
    if (command.step <= step) return;
    step = command.step;
    if (command.action === "finish") {
      clearInterval(poller);
      text("完成");
      reply(id, { stopReason: "end_turn" });
      return;
    }
    if (command.action === "thought") {
      update({ sessionUpdate: "agent_thought_chunk", content: { type: "text", text: "private-thought" } });
    } else if (command.action !== "check") {
      throw new Error(`unknown control action: ${command.action}`);
    }
    barrier(step);
  }, 5);
  fs.writeFileSync(path.join(control, "pid"), String(process.pid));
  update(thought
    ? { sessionUpdate: "agent_thought_chunk", content: { type: "text", text: "private-thought" } }
    : { sessionUpdate: "available_commands_update", availableCommands: [] });
  barrier(0);
};
for await (const line of readline.createInterface({ input: process.stdin })) {
  const req = JSON.parse(line);
  if (!req.method && barriers.has(req.id)) {
    if (req.error?.code !== -32601) throw new Error("barrier unexpectedly supported");
    fs.writeFileSync(path.join(control, "ack"), String(barriers.get(req.id)));
    barriers.delete(req.id);
    continue;
  }
  if (req.method === "initialize") {
    reply(req.id, { protocolVersion: 1, agentCapabilities: {} });
  } else if (req.method === "session/new" || req.method === "session/load") {
    reply(req.id, { sessionId: "research-session", modes: {
      currentModeId: "read-only", availableModes: [{ id: "read-only", name: "Read only" }, { id: "plan", name: "Plan" }],
    } });
  } else if (req.method === "session/prompt") {
    const prompt = req.params.prompt.filter(p => p.type === "text").map(p => p.text).join("\n");
    if (prompt === "idle" || prompt === "thought") {
      controlled(req.id, prompt === "thought");
      continue;
    }
    text("我先");
    if (prompt === "cancel") continue;
    if (prompt.startsWith("command_")) {
      const [kind, title, command, paths] = {
        command_list: ["search", "List src", ["ls", "src"], ["src"]],
        command_read: ["read", "Read lib.rs", ["cat", "src/lib.rs"], ["src/lib.rs"]],
        command_search: ["search", "Search add in src", ["/bin/sh", "-lc", "rg add src"], []],
        command_outside: ["search", "List /etc", ["ls", "/etc"], []],
        command_secret: ["read", "Read .env.local", ["cat", ".env.local"], []],
        command_edit: ["edit", "Edit lib.rs", ["true"], ["src/lib.rs"]],
        command_external: ["read", "Read lib.rs", ["cat", "src/lib.rs"], ["src/lib.rs"]],
      }[prompt];
      // codex-acp 0.16's exec_command_begin preserves the full command event while
      // parse_command_tool_call classifies ls/rg as search and cat as read.
      update({ sessionUpdate: "tool_call", toolCallId: "command-1", kind, title,
        ...(prompt === "command_external" ? { name: "mcp__other__reader" } : {}),
        status: "in_progress", locations: paths.map(path => ({ path })),
        rawInput: { call_id: "command-1", turn_id: "turn-1", command,
          cwd: process.cwd(), source: "unified_exec", interaction_input: null,
          process_id: null, started_at_ms: null } });
      update({ sessionUpdate: "tool_call_update", toolCallId: "command-1", status: "completed" });
      text("已读取项目证据");
      reply(req.id, { stopReason: "end_turn" });
      continue;
    }
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
process.exit(0);
