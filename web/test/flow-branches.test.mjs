import assert from "node:assert/strict"
import { test } from "node:test"
import { branchWarnings, renameBranchEdges, branchLabels, unreachableNodes, ruleConflicts } from "../src/auto/branches.ts"

const filter = (labels, other) => ({ id: "f", kind: "filter", title: "是不是", config: { rules: labels.map((label) => ({ label, when: "true" })), else_label: other } })
const graph = (f, edges) => ({ nodes: [{ id: "t", kind: "trigger", title: "", config: {} }, f, { id: "a", kind: "action", title: "", config: {} }, { id: "b", kind: "action", title: "", config: {} }], edges: [{ from: "t", to: "f", label: "" }, ...edges] })

test("an edge no branch names is strong, a branch without an edge is weak (as the hub says)", () => {
  const g = graph(filter(["是"], "否"), [{ from: "f", to: "a", label: "yes" }, { from: "f", to: "b", label: "否" }])
  assert.deepEqual(branchWarnings(g), [
    { node: "f", label: "yes", kind: "edge", strong: true },
    { node: "f", label: "是", kind: "branch", strong: false },
  ])
  const ok = graph(filter(["是"], "否"), [{ from: "f", to: "a", label: "是" }, { from: "f", to: "b", label: "否" }])
  assert.deepEqual(branchWarnings(ok), [])
})

test("renaming a branch on the canvas renames its edges and its on_fail branch", () => {
  const f = { ...filter(["是"], "否"), on_fail: "branch:否" }
  const g = graph(f, [{ from: "f", to: "a", label: "是" }, { from: "f", to: "b", label: "否" }, { from: "t", to: "b", label: "是" }])
  const renamed = { ...f, config: { ...f.config, else_label: "其他" } }
  const out = renameBranchEdges({ ...g, nodes: g.nodes.map((n) => (n.id === "f" ? renamed : n)) }, "f", branchLabels(f), branchLabels(renamed))
  assert.deepEqual(out.edges.map((e) => e.label), ["", "是", "其他", "是"], "only this node's edges follow")
  assert.equal(out.nodes.find((n) => n.id === "f").on_fail, "branch:其他")
  assert.deepEqual(branchWarnings(out), [])
  // an emptied label, or one more branch, renames nothing
  assert.equal(renameBranchEdges(g, "f", ["是", "否"], ["", "否"]), g)
  assert.equal(renameBranchEdges(g, "f", ["是", "否"], ["是", "否", "也许"]), g)
})

test("a node the trigger can't reach is listed, as the hub refuses it on save", () => {
  const nodes = [{ id: "t", kind: "trigger", config: {} }, { id: "f", kind: "filter", config: {} }, { id: "a", kind: "action", config: {} }, { id: "b", kind: "action", config: {} }, { id: "c", kind: "action", config: {} }]
  assert.deepEqual(unreachableNodes({ nodes, edges: [{ from: "t", to: "f", label: "" }, { from: "f", to: "a", label: "" }, { from: "b", to: "c", label: "" }] }), ["b", "c"])
  assert.deepEqual(unreachableNodes({ nodes, edges: [{ from: "t", to: "f", label: "" }, { from: "f", to: "a", label: "" }, { from: "a", to: "b", label: "" }, { from: "f", to: "c", label: "x" }] }), [])
})

test("ruleConflicts flags duplicate branches, shadowed filter rules, duplicate edges, and overlapping triggers", () => {
  const g = {
    nodes: [
      { id: "t", kind: "trigger", title: "邮件", config: { source: "mail", types: ["mail.received"] } },
      { id: "f", kind: "filter", title: "分类", config: { rules: [{ label: "是", when: "true" }, { label: "是", when: "event.urgent" }], else_label: "否" } },
      { id: "a", kind: "action", title: "A", config: {} },
      { id: "b", kind: "action", title: "B", config: {} },
    ],
    edges: [
      { from: "t", to: "f", label: "" },
      { from: "f", to: "a", label: "是" },
      { from: "f", to: "b", label: "是" },
    ],
  }
  const others = [
    { id: "flw_2", kind: "flow", name: "重复邮件流程", enabled: true, trigger: { source: "mail", types: [] } },
    { id: "flw_3", kind: "flow", name: "已停用邮件流程", enabled: false, trigger: { source: "mail", types: ["mail.received"] } },
    { id: "flw_4", kind: "flow", name: "不同类型", enabled: true, trigger: { source: "mail", types: ["mail.sent"] } },
  ]
  assert.deepEqual(ruleConflicts(g, { id: "flw_1", enabled: true }, others), [
    { kind: "duplicate_branch", node: "f", label: "是" },
    { kind: "shadowed_rule", node: "f", earlier: "是", shadowed: "是" },
    { kind: "duplicate_edge", node: "f", label: "是" },
    { kind: "trigger_overlap", flowId: "flw_2", flowName: "重复邮件流程", source: "mail" },
  ])
  const clean = graph(filter(["是"], "否"), [{ from: "f", to: "a", label: "是" }, { from: "f", to: "b", label: "否" }])
  assert.deepEqual(ruleConflicts(clean, { id: "flw_1", enabled: false }, others), [])
})
