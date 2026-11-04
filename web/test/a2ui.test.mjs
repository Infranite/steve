import assert from "node:assert/strict"
import { test } from "node:test"

const m = await import("../src/a2ui/model.ts")
const V = "v0.9.1"
const lines = (...xs) => xs.map((x) => JSON.stringify(x)).join("\n")
const create = (id = "s", catalogId = m.STEVE) => ({ version: V, createSurface: { surfaceId: id, catalogId } })
const comps = (components, id = "s") => ({ version: V, updateComponents: { surfaceId: id, components } })
const dataModel = (value, path = "/", id = "s") => ({ version: V, updateDataModel: { surfaceId: id, path, value } })

test("JSON Lines, pretty-printed objects and one array all read the same", () => {
  const msgs = [create(), comps([{ id: "root", component: "Text", text: "你好" }])]
  for (const src of [lines(...msgs), msgs.map((x) => JSON.stringify(x, null, 2)).join("\n\n"), JSON.stringify(msgs)]) {
    const d = m.parseBlock(src)
    assert.equal(d.fatal, undefined, src)
    assert.equal(d.surfaces.length, 1)
    assert.equal(d.surfaces[0].comps.get("root").text, "你好")
    assert.deepEqual(d.problems, [])
  }
})

test("messages apply in order: later components replace earlier ones, data merges by pointer, delete removes", () => {
  const d = m.parseBlock(lines(
    create(), create("gone"),
    comps([{ id: "root", component: "Text", text: "旧" }]),
    comps([{ id: "root", component: "Text", text: "新" }]),
    comps([{ id: "root", component: "Text", text: "x" }], "gone"),
    dataModel({ form: { name: "A" }, rows: [1, 2] }),
    dataModel("B", "/form/name"),
    dataModel(undefined, "/rows"),
    { version: V, deleteSurface: { surfaceId: "gone" } },
  ))
  assert.deepEqual(d.surfaces.map((s) => s.id), ["s"])
  assert.equal(d.surfaces[0].comps.get("root").text, "新")
  assert.deepEqual(d.surfaces[0].data, { form: { name: "B" } })
})

test("what an agent gets wrong is a problem with a path, never a throw", () => {
  const d = m.parseBlock([
    JSON.stringify(create()),
    "这不是 JSON",
    JSON.stringify({ version: "v2", createSurface: { surfaceId: "x", catalogId: m.BASIC } }),
    JSON.stringify(comps([{ id: "root", component: "Column", children: ["a", "missing"] }, { component: "Text" }, { id: "a", component: "Fancy" }])),
    JSON.stringify(comps([{ id: "root", component: "Text", text: "无 create" }], "late")),
  ].join("\n"))
  assert.equal(d.fatal, undefined)
  assert.deepEqual(d.surfaces.map((s) => s.id), ["s", "late"])
  const paths = d.problems.map((p) => p.path)
  assert.ok(paths.some((p) => p.startsWith("@")), "the stray text")
  assert.ok(paths.includes("/1/version"), "the unknown version")
  assert.ok(paths.includes("/2/updateComponents/components/1"), "the component without an id")
  assert.ok(d.problems.some((p) => p.surfaceId === "late" && /createSurface/.test(p.message)))
})

test("nothing drawable is fatal, with the reason", () => {
  assert.match(m.parseBlock("").fatal, /空/)
  assert.match(m.parseBlock("{ 坏").fatal, /JSON/)
  assert.match(m.parseBlock(lines(create())).fatal, /root/)
  assert.match(m.parseBlock("x".repeat(m.LIMITS.bytes + 1)).fatal, /KB/)
  const many = Array.from({ length: m.LIMITS.components + 5 }, (_, i) => ({ id: i ? `c${i}` : "root", component: "Text", text: "" }))
  const d = m.parseBlock(lines(create(), comps(many)))
  assert.equal(d.surfaces[0].comps.size, m.LIMITS.components)
  assert.ok(d.problems.some((p) => /超过/.test(p.message)))
})

test("bindings, template children and relative paths", () => {
  const s = { data: { rows: [{ name: "甲" }, { name: "乙" }], form: { n: 3 } }, base: "/" }
  assert.equal(m.resolve({ path: "/form/n" }, s), 3)
  assert.equal(m.resolve("字面量", s), "字面量")
  const kids = m.childrenOf({ path: "/rows", componentId: "row" }, s)
  assert.deepEqual(kids.map((k) => k.base), ["/rows/0", "/rows/1"])
  assert.equal(m.resolve({ path: "name" }, { data: s.data, base: kids[1].base }), "乙")
  assert.deepEqual(m.childrenOf(["a", 3, "b"], s).map((k) => k.id), ["a", "b"])
  assert.deepEqual(m.setPointer({ a: [1, 2] }, "/a/1", 9), { a: [1, 9] })
  assert.deepEqual(m.setPointer(undefined, "/x/0", "v"), { x: ["v"] })
  assert.equal(m.getPointer({ "a/b": { "~": 1 } }, "/a~1b/~0"), 1)
})

test("basic catalog functions and checks", () => {
  const s = { data: { email: "a@b.co", name: "", n: 1234.5, d: "2026-11-05T08:09:00", on: true }, base: "/" }
  const c = (call, args) => m.call(call, args, s)
  assert.equal(c("required", { value: { path: "/name" } }), false)
  assert.equal(c("email", { value: { path: "/email" } }), true)
  assert.equal(c("length", { value: "abc", min: 2, max: 3 }), true)
  assert.equal(c("numeric", { value: "x" }), false)
  assert.equal(c("regex", { value: "ST-46", pattern: "^ST-\\d+$" }), true)
  assert.equal(c("regex", { value: "a", pattern: "(" }), false, "a broken pattern fails, it does not throw")
  assert.equal(c("formatNumber", { value: { path: "/n" }, decimals: 1 }), "1,234.5")
  assert.equal(c("formatDate", { value: { path: "/d" }, format: "MM-dd HH:mm" }), "11-05 08:09")
  assert.equal(c("and", { values: [true, { path: "/on" }] }), true)
  assert.equal(c("not", { value: { path: "/on" } }), false)
  assert.equal(m.formatString("你好 ${/email}，\\${不替换}", s), "你好 a@b.co，${不替换}")
  const btn = { id: "b", component: "Button", checks: [{ condition: { call: "required", args: { value: { path: "/name" } } }, message: "要填名字" }] }
  assert.deepEqual(m.failedChecks(btn, s), ["要填名字"])
  assert.deepEqual(m.eventContext({ who: { path: "/email" }, fixed: 1 }, s), { who: "a@b.co", fixed: 1 })
})

test("block keys are stable per text; a chart shows the last N rows", () => {
  assert.equal(m.blockKey("abc"), m.blockKey("abc"))
  assert.notEqual(m.blockKey("abc"), m.blockKey("abd"))
  assert.match(m.blockKey("任何内容"), /^[0-9a-z]{1,8}$/)
  assert.equal(m.actionKey("k", "s"), "k:s")
  const rows = [1, 2, 3, 4].map((v) => ({ v }))
  assert.deepEqual(m.chartRows(rows, ["2"]).map((r) => r.v), [3, 4])
  assert.deepEqual(m.chartRows(rows, undefined).length, 4)
  assert.deepEqual(m.chartRows("bad", 2), [])
})
