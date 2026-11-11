import assert from "node:assert/strict"
import { test } from "node:test"

const m = await import("../src/thread/tools.ts")

test("a research step is told by the category its adapter reported, whatever the harness names the tool", () => {
  assert.equal(m.toolClass("exec_command", "exec"), "exec")
  assert.equal(m.toolClass("Bash", "exec"), "exec")
  assert.equal(m.toolClass("read_file", "read"), "read")
  assert.equal(m.toolClass("apply_patch", "edit"), "edit")
  assert.equal(m.toolClass("web_fetch", "fetch"), "fetch")
  assert.equal(m.toolClass("something_else", "other"), "other")
  assert.equal(m.toolClass("something_else"), "other")
})

test("a step stored before the category was kept is told by its tool name", () => {
  assert.equal(m.toolClass("Read"), "read")
  assert.equal(m.toolClass("Grep"), "search")
  assert.equal(m.toolClass("Bash"), "exec")
})

test("Steve's own tools are told by name, through any MCP server prefix", () => {
  assert.equal(m.toolClass("mcp__steve__steve_onboard_profile", "steve"), "meet")
  assert.equal(m.meetStep("mcp__steve__steve_onboard_models"), "models")
  assert.equal(m.meetStep("steve_onboard_done"), "done")
  assert.equal(m.meetStep("mcp__steve__steve_item_get"), "")
  assert.equal(m.toolClass("mcp__steve__steve_knowledge_search", "steve"), "kb")
  assert.equal(m.toolClass("mcp__steve__steve_item_get", "steve"), "item")
  assert.equal(m.toolShort("mcp__codex__list_mcp_resources"), "list_mcp_resources")
})

test("a run's summary counts what it did: a run that only ran commands did not do nothing", () => {
  const run = [
    { name: "exec_command", kind: "exec", args: { brief: "ls" } },
    { name: "exec_command", kind: "exec", args: { brief: "git config --global --list" } },
    { name: "read_file", kind: "read", args: { brief: "a.md" } },
    { name: "read_file", kind: "read", args: { brief: "a.md" } },
    { name: "mcp__steve__steve_onboard_profile", kind: "steve", args: { brief: "" } },
    { name: "mcp__codex__list_mcp_resources", kind: "other", args: { brief: "" } },
  ]
  const n = m.toolCounts(run)
  assert.deepEqual([n.exec, n.read, n.files, n.meet, n.other, n.search], [2, 2, 1, 1, 1, 0])
})
