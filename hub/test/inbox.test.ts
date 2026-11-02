// A chat answer becomes at most one item, and every reminder in 待处理 can be cleared.
// Runs against a throwaway STEVE_HOME; no model or agent process ever starts (both executables are missing).
import { test, after } from "node:test"
import assert from "node:assert/strict"
import { mkdtempSync, rmSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"

const home = mkdtempSync(join(tmpdir(), "steve-test-"))
Object.assign(process.env, { STEVE_HOME: home, STEVE_REPO: join(home, "no-repo"), STEVE_ADAPTER_CLAUDE: join(home, "no-adapter.js"), CLAUDE_CODE_EXECUTABLE: join(home, "no-claude") })
const { ensureWorkspace, WS, items, sessions, decisions, messages } = await import("../src/repo.js")
const orch = await import("../src/orchestrator.js")
const { promote } = await import("../src/thread.js")
ensureWorkspace(); orch.seed()
after(() => setImmediate(() => { rmSync(home, { recursive: true, force: true }); process.exit(0) }))

const chatAnswer = () => messages.add(WS, "steve", "建议这样做……", null, { chat: "按建议" })
const count = () => items.list(WS).length

test("promoting the same answer twice at once makes one item", async () => {
  const m = chatAnswer(), before = count()
  const [a, b] = await Promise.all([promote(WS, m), promote(WS, m)])
  assert.equal(a.id, b.id)
  assert.equal(count(), before + 1)
})

test("promoting an answer that was already promoted returns the same item", async () => {
  const m = chatAnswer(), first = await promote(WS, m), before = count()
  const again = await promote(WS, m)
  assert.equal(again.id, first.id)
  assert.equal(count(), before)
  assert.equal(messages.get(m)!.meta.chat, undefined)
})

test("promoting a message from another workspace is refused", async () => {
  await assert.rejects(promote("elsewhere", chatAnswer()))
})

function waiting(kind: "ask" | "fail") {
  const i = items.create(WS, { title: "t", goal: "g", bot_id: "dev", project_id: null, status: "wait", stage: "执行" })
  const s = sessions.create({ workspace_id: WS, item_id: i.id, bot_id: "dev", harness: "claude-code", model: null, cwd: home, title: "t", token: "x" })
  sessions.patch(s.id, { status: "idle" })
  const d = decisions.create({ workspace_id: WS, kind, subject_type: "item", subject_id: i.id, item_id: i.id, session_id: s.id, question: "还要吗？", options: [], requested_by: "dev" })
  return { id: i.id, decision: d.id }
}
const openOf = (id: string) => decisions.ofItem(id).filter((d) => d.status === "open")

test("dismissing a reminder clears it, and undo brings it back", () => {
  const x = waiting("ask")
  orch.dismissDecision(x.decision)
  assert.equal(openOf(x.id).length, 0)
  /* nothing else open and the session is idle: parked, so 继续 picks it up */
  assert.equal(items.get(x.id).status, "pause")
  orch.undoDecision(x.decision)
  assert.equal(openOf(x.id).length, 1)
  assert.equal(items.get(x.id).status, "wait")
})

test("dismissing twice is refused", () => {
  const x = waiting("fail")
  orch.dismissDecision(x.decision)
  assert.throws(() => orch.dismissDecision(x.decision))
})

test("dropping an item clears all its reminders", () => {
  const x = waiting("fail")
  decisions.create({ workspace_id: WS, kind: "ask", subject_type: "question", subject_id: "s", item_id: x.id, question: "q", options: [], requested_by: "dev" })
  orch.itemAction(x.id, "drop")
  assert.equal(openOf(x.id).length, 0)
})

test("finishing an item clears reminders still open on it", () => {
  const x = waiting("ask")
  const acc = decisions.create({ workspace_id: WS, kind: "accept", subject_type: "item", subject_id: x.id, item_id: x.id, question: "验收", options: ["pass", "rework"], requested_by: "dev" })
  orch.resolve(acc.id, "pass")
  assert.equal(items.get(x.id).status, "done")
  assert.equal(openOf(x.id).length, 0)
})

test("on boot, reminders left on finished or dropped items are cleared", () => {
  const x = waiting("fail")
  items.patch(x.id, { status: "drop" })
  orch.recover()
  assert.equal(openOf(x.id).length, 0)
})

test("on boot, a question older than a delivery expires: the item shows the delivery", async () => {
  const x = waiting("ask")
  await new Promise((r) => setTimeout(r, 5))
  const acc = decisions.create({ workspace_id: WS, kind: "accept", subject_type: "item", subject_id: x.id, item_id: x.id, question: "验收", options: ["pass", "rework"], requested_by: "dev" })
  orch.recover()
  assert.deepEqual(openOf(x.id).map((d) => d.id), [acc.id])
})

test("a turn cut off by a restart continues once; a second cut within the window waits for the user", () => {
  const i = items.create(WS, { title: "t", goal: "g", bot_id: "dev", project_id: null, status: "run", stage: "执行" })
  const s = sessions.create({ workspace_id: WS, item_id: i.id, bot_id: "dev", harness: "claude-code", model: null, cwd: home, title: "t", token: "x" })
  sessions.patch(s.id, { status: "running" })
  orch.recover()
  assert.equal(sessions.get(s.id)!.status, "idle")
  assert.equal(openOf(i.id).length, 0)
  sessions.patch(s.id, { status: "running" })
  orch.recover()
  assert.deepEqual(openOf(i.id).map((d) => d.kind), ["fail"])
})
