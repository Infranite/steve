// Restart behaviour with a scripted adapter (test/fake-acp.js): the hold before a release, the resume after it.
import { test, after } from "node:test"
import assert from "node:assert/strict"
import { mkdtempSync, readFileSync, existsSync, rmSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"

const home = mkdtempSync(join(tmpdir(), "steve-test-")), log = join(home, "prompts.jsonl")
Object.assign(process.env, { STEVE_HOME: home, STEVE_REPO: join(home, "no-repo"), STEVE_ADAPTER_CLAUDE: join(import.meta.dirname, "fake-acp.js"), FAKE_ACP_LOG: log })
const { ensureWorkspace, WS, items, sessions, decisions, queued, events } = await import("../src/repo.js")
const orch = await import("../src/orchestrator.js")
const acp = await import("../src/acp.js")
ensureWorkspace(); orch.seed()
after(() => { acp.shutdown(); setImmediate(() => { rmSync(home, { recursive: true, force: true }); process.exit(0) }) })

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms))
const prompts = (sid: string) => (existsSync(log) ? readFileSync(log, "utf8").trim().split("\n").filter(Boolean).map((l) => JSON.parse(l)) : []).filter((p) => p.session === sessions.get(sid)!.acp_session_id).map((p) => p.text as string)
function session(status: string) {
  const i = items.create(WS, { title: "t", goal: "g", bot_id: "dev", project_id: null, status: "run", stage: "执行" })
  const s = sessions.create({ workspace_id: WS, item_id: i.id, bot_id: "dev", harness: "claude-code", model: null, cwd: home, title: "t", token: "t" + i.id })
  sessions.patch(s.id, { status })
  return { id: i.id, session: s.id }
}
const idle = async (sid: string) => { for (let k = 0; k < 100 && (acp.isBusy(sid) || sessions.get(sid)!.status !== "idle"); k++) await sleep(50) }

test("after a restart: the interrupted turn resumes once, with what was queued, each text once; a waiting one is left alone", async () => {
  const cut = session("running")
  queued.add(cut.session, "请继续。", "steve"); queued.add(cut.session, "请继续。", "steve"); queued.add(cut.session, "补充：用中文", "me")
  const waiting = session("running")
  decisions.create({ workspace_id: WS, kind: "ask", subject_type: "question", subject_id: waiting.session, item_id: waiting.id, session_id: waiting.session, question: "选哪个？", options: ["a"], requested_by: "dev" })
  const heldNew = session("idle"); queued.add(heldNew.session, "@sleep 10", "me")

  orch.recover()
  assert.deepEqual(prompts(cut.session), [], "nothing is sent before the Hub listens")
  await sleep(3300)
  await idle(cut.session); await idle(heldNew.session)
  assert.deepEqual(prompts(cut.session), [`${orch.RESUME}\n\n请继续。\n\n补充：用中文`])
  assert.equal(queued.list(cut.session).length, 0)
  assert.deepEqual(prompts(heldNew.session), ["@sleep 10"], "a prompt held for the restart is sent after it")
  assert.deepEqual(prompts(waiting.session), [])
  assert.ok(!decisions.ofItem(cut.id).some((d) => d.kind === "fail" && /中断/.test(d.question)), "no interruption card for a successful resume")
})

test("hold: running turns finish, new prompts queue and go when the hold is lifted", async () => {
  const a = session("idle"), b = session("idle")
  await acp.prompt(a.session, "@sleep 400", "me")
  await sleep(50)
  acp.hold(true)
  assert.equal(acp.runningTurns(), 1)
  assert.equal(await acp.prompt(b.session, "新的一回合", "me"), "queued")
  assert.equal(await acp.prompt(a.session, "排在后面", "me"), "queued")
  await idle(a.session)
  assert.equal(acp.runningTurns(), 0, "the running turn ended normally and nothing new started")
  assert.deepEqual(prompts(b.session), [])
  assert.deepEqual(prompts(a.session), ["@sleep 400"])
  acp.hold(false); acp.flush(a.session); acp.flush(b.session)
  await idle(a.session); await idle(b.session)
  assert.deepEqual(prompts(a.session), ["@sleep 400", "排在后面"])
  assert.deepEqual(prompts(b.session), ["新的一回合"])
  assert.equal(events.after(b.session, 0).filter((e) => e.kind === "user").length, 1, "sent once")
})

test("drain refuses a call without the release's token", () => {
  assert.equal(orch.drain("nope", true), null)
  assert.equal(acp.isHolding(), false)
})
