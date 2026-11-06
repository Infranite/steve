import assert from "node:assert/strict"
import { test } from "node:test"
import { branchWarnings, renameBranchEdges, branchLabels } from "../src/auto/branches.ts"

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
