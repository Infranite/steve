/* stale 待处理: a question or failure card expires once the conversation goes on without it */
import { test, after } from "node:test"
import assert from "node:assert/strict"
import { mkdtempSync, rmSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"

const home = process.env.STEVE_HOME = mkdtempSync(join(tmpdir(), "steve-test-"))
after(() => rmSync(home, { recursive: true, force: true }))
const { items, decisions, bots, sessions, events, WS, ensureWorkspace } = await import("../src/repo.js")
const { settle, supersede, tidy, mayResume, RESUME_WINDOW } = await import("../src/lifecycle.js")

const ws = WS
ensureWorkspace()
bots.seed({ id: "steve", name: "Steve", main: true, harness: "claude-code" }, ws)
const newItem = () => items.create(ws, { title: "t", goal: "g", bot_id: "steve", project_id: null })
const ask = (item: string, kind: "ask" | "fail" | "accept" = "ask") =>
  decisions.create({ workspace_id: ws, kind, subject_type: "question", subject_id: "s", item_id: item, question: "q?", options: [], requested_by: "steve" })

test("open question keeps the item waiting", () => {
  const i = newItem(); ask(i.id); settle(i.id)
  assert.equal(items.get(i.id).status, "wait")
})

test("superseding expires questions and failure cards and puts the item back to running", () => {
  const i = newItem(); const a = ask(i.id), f = ask(i.id, "fail"); settle(i.id)
  assert.equal(supersede(i.id, ["ask", "fail"], "你直接补充了信息"), 2)
  assert.equal(decisions.get(a.id).status, "expired")
  assert.equal(decisions.get(f.id).status, "expired")
  assert.equal(items.get(i.id).status, "run")
  assert.equal(decisions.open(ws).filter((d) => d.item_id === i.id).length, 0)
})

test("other kinds stay open and the item keeps waiting", () => {
  const i = newItem(); ask(i.id); const acc = ask(i.id, "accept"); settle(i.id)
  supersede(i.id, ["ask", "fail"], "x")
  assert.equal(decisions.get(acc.id).status, "open")
  assert.equal(items.get(i.id).status, "wait")
})

test("nothing to supersede changes nothing", () => {
  const i = newItem(); items.patch(i.id, { status: "pause" })
  assert.equal(supersede(i.id, ["ask"], "x"), 0)
  assert.equal(items.get(i.id).status, "pause")
})

test("a restart-interrupted turn resumes once per window", () => {
  const i = newItem()
  const s = sessions.create({ workspace_id: ws, item_id: i.id, bot_id: "steve", harness: "claude-code", model: null, cwd: "/tmp", title: "t", token: "x" })
  assert.equal(mayResume(s.id), true)
  events.add(s.id, "system", { text: "Hub 重启，这一回合被中断，自动继续。", resumed: true })
  assert.equal(mayResume(s.id), false)
  assert.equal(mayResume(s.id, Date.now() + RESUME_WINDOW + 1000), true)
})

test("tidy expires questions older than a delivery and keeps the delivery", async () => {
  const i = newItem(); const a = ask(i.id)
  await new Promise((r) => setTimeout(r, 5))
  const acc = ask(i.id, "accept"); settle(i.id)
  assert.equal(tidy(i.id), 1)
  assert.equal(decisions.get(a.id).status, "expired")
  assert.equal(decisions.get(acc.id).status, "open")
  assert.equal(items.get(i.id).status, "wait")
})
