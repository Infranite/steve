// Chat redesign (ST-6): notifications leave the conversation; decisions without an item are answerable.
// Runs against a throwaway STEVE_HOME; no model or agent process ever starts (both executables are missing).
import { test, after } from "node:test"
import assert from "node:assert/strict"
import { mkdtempSync, rmSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"

const home = mkdtempSync(join(tmpdir(), "steve-test-"))
Object.assign(process.env, { STEVE_HOME: home, STEVE_REPO: join(home, "no-repo"), STEVE_ADAPTER_CLAUDE: join(home, "no-adapter.js"), CLAUDE_CODE_EXECUTABLE: join(home, "no-claude") })
const { ensureWorkspace, WS, items, sessions, decisions, messages, events } = await import("../src/repo.js")
const orch = await import("../src/orchestrator.js")
const notices = await import("../src/notify.js")
const { toolActions } = await import("../src/mcp.js")
ensureWorkspace(); orch.seed()
after(() => setImmediate(() => { rmSync(home, { recursive: true, force: true }); process.exit(0) }))

/* ── notifications ── */
test("a successful release is a notification only; a failure also leaves one line in the conversation", () => {
  const before = notices.thread(WS).length
  const ok = notices.notify(WS, "release", "success", "已发布新版本 v1")
  assert.equal(notices.thread(WS).length, before)
  const bad = notices.notify(WS, "release", "fail", "发布 v2 失败，已回到 v1：健康检查没有通过")
  const line = notices.thread(WS).at(-1)!
  assert.equal(line.meta.notice.ref, bad)
  assert.deepEqual(notices.list(WS).slice(0, 2).map((n) => [n.id, n.level]), [[bad, "fail"], [ok, "success"]])
})

test("old release messages written into the conversation are listed as notifications; the successful ones leave the conversation", () => {
  const ok = messages.add(WS, "steve", "已发布新版本 v0.9")
  const bad = messages.add(WS, "steve", "发布 v0.8 失败，已回到 v0.7：健康检查没有通过")
  const chat = messages.add(WS, "steve", "已发布新版本 是个好消息", null, { chat: "x" })
  const ids = notices.thread(WS).map((m) => m.id)
  assert.ok(!ids.includes(ok)); assert.ok(ids.includes(bad)); assert.ok(ids.includes(chat))
  const ls = notices.list(WS)
  assert.equal(ls.find((n) => n.id === ok)?.level, "success")
  assert.equal(ls.find((n) => n.id === bad)?.level, "fail")
  assert.ok(!ls.some((n) => n.id === chat))
})

test("the read cursor moves forward and is kept without a revision", () => {
  /* unset: the first look counts everything so far as seen */
  assert.ok(notices.seen(WS) >= Date.now() - 5000)
  const at = notices.markSeen(WS, 12345)
  assert.equal(notices.seen(WS), at)
})

/* ── decisions without an item ── */
function loose(status: "idle" | "ended" = "idle") {
  const s = sessions.create({ workspace_id: WS, item_id: null, bot_id: "dev", harness: "claude-code", model: null, cwd: home, title: "派生", token: "x" })
  sessions.patch(s.id, { status }); return sessions.get(s.id)!
}
const userText = (sid: string) => events.after(sid, 0).filter((e) => e.kind === "user").map((e) => String(e.data?.text ?? ""))

test("steve_ask from a session without an item makes an answerable card, and the answer reaches the session", () => {
  const s = loose()
  toolActions().ask(s, "用哪个方案？", ["A", "B"])
  const d = decisions.open(WS).find((x) => x.session_id === s.id)!
  assert.equal(d.item_id, null); assert.equal(d.kind, "ask")
  orch.resolve(d.id, "answer", "B")
  assert.equal(decisions.get(d.id).status, "resolved")
  assert.ok(userText(s.id).some((t) => t.includes("B")))
})

test("steve_request_approval from a session without an item is answerable; an ended session wakes up", () => {
  const s = loose("ended")
  toolActions().requestApproval(s, "推送分支", "git push")
  const d = decisions.open(WS).find((x) => x.session_id === s.id)!
  orch.resolve(d.id, "no", "先别推")
  assert.equal(decisions.get(d.id).result.approved, false)
  assert.notEqual(sessions.get(s.id)!.status, "ended")
  assert.ok(userText(s.id).some((t) => t.includes("先别推")))
})

test("every kind resolves when the item is missing", () => {
  const cases: [string, string, string, string][] = [
    ["accept", "item", "pass", ""], ["accept", "item", "rework", "再看看"], ["propose", "item", "yes", ""], ["fail", "item", "continue", ""], ["fail", "item", "look", ""], ["fail", "item", "tosteve", ""],
    ["approve", "action", "yes", ""], ["approve", "release", "yes", ""], ["approve", "tool_call", "deny", ""], ["ask", "question", "answer", "好"],
  ]
  for (const [kind, subject_type, choice, text] of cases) {
    const s = loose()
    const d = decisions.create({ workspace_id: WS, kind: kind as any, subject_type, subject_id: s.id, item_id: null, session_id: s.id, question: "q", options: [], requested_by: "dev" })
    orch.resolve(d.id, choice, text)
    assert.notEqual(decisions.get(d.id).status, "open", `${kind}/${subject_type}/${choice}`)
  }
})

test("a card whose item was deleted becomes item-less and still resolves", () => {
  const i = items.create(WS, { title: "t", goal: "g", bot_id: "dev", project_id: null, status: "wait", stage: "执行" })
  const s = sessions.create({ workspace_id: WS, item_id: i.id, bot_id: "dev", harness: "claude-code", model: null, cwd: home, title: "t", token: "x" })
  sessions.patch(s.id, { status: "idle" })
  const d = decisions.create({ workspace_id: WS, kind: "accept", subject_type: "item", subject_id: i.id, item_id: i.id, session_id: s.id, question: "验收", options: ["pass", "rework"], requested_by: "dev" })
  items.patch(i.id, { archived: true }); items.remove(i.id)
  assert.equal(decisions.get(d.id).item_id, null)
  orch.resolve(d.id, "pass")
  assert.equal(decisions.get(d.id).status, "resolved")
})

/* ── what Steve did ── */
test("a reply records the routing decision, the tools used and the times", async () => {
  const { say } = await import("../src/thread.js")
  const mid = say(WS, "给事项列表加个筛选"), at = messages.get(mid)!.created
  const reply = () => messages.list(WS).find((m) => m.author === "steve" && m.created >= at && m.meta?.steps)
  for (let n = 0; n < 400 && !reply(); n++) await new Promise((r) => setTimeout(r, 25))
  const r = reply()!, st = r.meta.steps
  /* no model here: the router falls back to an item for Steve, and says so */
  assert.equal(st.route.kind, "item"); assert.equal(st.route.fallback, true); assert.ok(st.route.why)
  assert.equal(st.tools[0].name, "create_item"); assert.equal(st.tools[0].status, "ok"); assert.equal(st.tools[0].result.item, r.item_id)
  assert.equal(typeof st.ms, "number"); assert.equal(typeof st.route.ms, "number")
})
