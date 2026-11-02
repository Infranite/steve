// Item lifecycle: an item is done only when it is fully finished, and any new message puts it back in progress.
// Runs against a throwaway STEVE_HOME; agent processes never start (the adapter path does not exist).
import { test, after } from "node:test"
import assert from "node:assert/strict"
import { mkdtempSync, mkdirSync, writeFileSync, rmSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"

const home = mkdtempSync(join(tmpdir(), "steve-test-"))
Object.assign(process.env, { STEVE_HOME: home, STEVE_REPO: join(home, "no-repo"), STEVE_ADAPTER_CLAUDE: join(home, "no-adapter.js") })
const { ensureWorkspace, WS, items, sessions, decisions, projects, timeline } = await import("../src/repo.js")
const orch = await import("../src/orchestrator.js")
ensureWorkspace(); orch.seed()
/* failed adapter spawns leave timers behind; nothing else to wait for */
after(() => setImmediate(() => { rmSync(home, { recursive: true, force: true }); process.exit(0) }))

const project = projects.create(WS, { name: "P", description: "", device_id: "hub", path: home, kind: "dir", worktree: false, default_bot: "dev", release: { kind: "script", cmd: join(home, "release.sh") } })
function delivered(withRelease: boolean) {
  const i = items.create(WS, { title: "t", goal: "g", bot_id: "dev", project_id: withRelease ? project.id : null, status: "run", stage: "交付与验收" })
  items.patch(i.id, { cwd: home, branch: withRelease ? "steve/x" : null })
  const s = sessions.create({ workspace_id: WS, item_id: i.id, bot_id: "dev", harness: "claude-code", model: null, cwd: home, title: "t", token: "x" })
  sessions.patch(s.id, { status: "idle" })
  const d = decisions.create({ workspace_id: WS, kind: "accept", subject_type: "item", subject_id: i.id, item_id: i.id, session_id: s.id, question: "验收", draft: "", options: ["pass", "rework"], requested_by: "dev" })
  return { id: i.id, session: s.id, decision: d.id }
}

test("accepted without a release: done", () => {
  const x = delivered(false)
  orch.resolve(x.decision, "pass")
  assert.equal(items.get(x.id).status, "done")
})

test("accepted with a release pending: still in progress until the release lands", () => {
  const x = delivered(true)
  orch.resolve(x.decision, "pass")
  assert.notEqual(items.get(x.id).status, "done")
  assert.equal(items.get(x.id).stage, "收尾")
  mkdirSync(join(home, "releases"), { recursive: true })
  writeFileSync(join(home, "releases/last.json"), JSON.stringify({ ok: true, version: "v1", item: x.id }))
  orch.reportRelease()
  assert.equal(items.get(x.id).status, "done")
})

test("release declined without a reason: accepted, finished without releasing", () => {
  const x = delivered(true)
  orch.resolve(x.decision, "pass")
  const r = decisions.create({ workspace_id: WS, kind: "approve", subject_type: "release", subject_id: "steve/x", item_id: x.id, session_id: x.session, question: "发布", options: ["yes", "no"], requested_by: "dev" })
  orch.resolve(r.id, "no")
  assert.equal(items.get(x.id).status, "done")
})

test("a new message on a finished item puts it back in progress", () => {
  const x = delivered(false)
  orch.resolve(x.decision, "pass")
  assert.equal(items.get(x.id).status, "done")
  void orch.sessionMessage(x.session, "再改一下", "排队")
  assert.equal(items.get(x.id).status, "run")
  assert.equal(items.get(x.id).stage, "执行")
})

test("supplementing a finished item puts it back in progress", () => {
  const x = delivered(false)
  orch.resolve(x.decision, "pass")
  void Promise.resolve(orch.supplement(x.id, "还有一点", "auto")).catch(() => {})
  assert.equal(items.get(x.id).status, "run")
})

/* ── a project with a release: accepted is not done until it is live; a failed release reopens ── */
const { toolActions } = await import("../src/mcp.js")
const { events, settings } = await import("../src/repo.js")
const { config } = await import("../src/config.js")

test("accepted with a release pending waits for the release, a failed release reopens", () => {
  const x = delivered(true)
  orch.resolve(x.decision, "pass")
  assert.equal(items.get(x.id).now, "等待发布")
  writeFileSync(join(home, "releases/last.json"), JSON.stringify({ ok: false, version: "v2", prev: "v1", item: x.id, error: "构建失败" }))
  orch.reportRelease()
  const i = items.get(x.id)
  assert.notEqual(i.status, "done"); assert.equal(i.stage, "收尾")
  assert.ok(decisions.ofItem(x.id).some((d) => d.kind === "fail" && d.status === "open"))
})

test("auto-accepted with a release pending is not done either", () => {
  const x = delivered(true)
  decisions.resolve(x.decision, { choice: "skip" })
  items.patch(x.id, { accept_mode: "auto" })
  toolActions().deliver(sessions.get(x.session)!, "ok", [], [{ name: "tests", passed: true }])
  assert.notEqual(items.get(x.id).status, "done")
  assert.equal(items.get(x.id).now, "等待发布")
})

const planOf = (id: string) => items.get(id).plan.map((p) => [p.step, p.done, !!p.left])
test("a delivery with open steps is refused until they are done or declared left open", () => {
  const x = delivered(false), s = sessions.get(x.session)!
  items.patch(x.id, { plan: [{ step: "a", done: false }, { step: "b", done: false }, { step: "c", done: true }] })
  const refused = toolActions().deliver(s, "ok", [], [], ["b"])
  assert.match(refused ?? "", /Not delivered.*: a\./)
  assert.deepEqual(decisions.ofItem(x.id).filter((d) => d.status === "open").map((d) => d.kind), ["accept"], "the earlier card stays")
  toolActions().update(s, "做完 a", [{ step: "a", done: true }, { step: "b", done: false }, { step: "c", done: true }])
  assert.equal(toolActions().deliver(s, "ok", [], [], ["b"]), undefined)
  assert.deepEqual(planOf(x.id), [["a", true, false], ["b", false, true], ["c", true, false]])
  assert.match(decisions.ofItem(x.id).find((d) => d.kind === "accept" && d.status === "open")!.draft, /没做完的计划步骤：b/)
})

test("finishing never ticks steps off", () => {
  const x = delivered(false)
  items.patch(x.id, { plan: [{ step: "a", done: true }, { step: "b", done: false }] })
  orch.resolve(x.decision, "pass")
  assert.equal(items.get(x.id).status, "done")
  assert.deepEqual(planOf(x.id), [["a", true, false], ["b", false, true]])
  assert.ok(timeline.list(x.id).some((t) => t.text === "完成时计划里还有 1 步没做：b"))
})

test("a new plan keeps the old one as an abandoned or finished round; open steps reopen a finished item", () => {
  const x = delivered(false), s = sessions.get(x.session)!, up = toolActions().update
  up(s, "1", [{ step: "a", done: true }, { step: "b", done: false }])
  up(s, "2", [{ step: "a", done: true }, { step: "b", done: true }, { step: "c", done: false }])
  assert.equal(items.planRounds(x.id).length, 0)
  up(s, "3", [{ step: "x", done: false }])
  up(s, "4", [{ step: "x", done: true }])
  up(s, "5", [{ step: "y", done: false }])
  assert.deepEqual(items.planRounds(x.id).map((r) => [r.round, r.end, r.steps.length]), [[1, "replaced", 3], [2, "done", 1]])
  const y = delivered(false), t = sessions.get(y.session)!
  orch.resolve(y.decision, "pass")
  up(t, "补一步", [{ step: "b", done: false }])
  assert.equal(items.get(y.id).status, "run")
})

/* ── after a restart: an interrupted turn is resumed once; a failed resume, a second interruption raise the fail card ── */
function interrupted(opts: { resumedBefore?: boolean } = {}) {
  const i = items.create(WS, { title: "t", goal: "g", bot_id: "dev", project_id: null, status: "run", stage: "执行" })
  const s = sessions.create({ workspace_id: WS, item_id: i.id, bot_id: "dev", harness: "claude-code", model: null, cwd: home, title: "t", token: "y" + i.id })
  if (opts.resumedBefore) events.add(s.id, "system", { text: "Hub 重启，这一回合被中断，自动继续。", resumed: true })
  sessions.patch(s.id, { status: "running" })
  return { id: i.id, session: s.id }
}
const userTexts = (sid: string) => events.after(sid, 0).filter((e) => e.kind === "user").map((e) => e.data.text)
const openFails = (id: string) => decisions.ofItem(id).filter((d) => d.kind === "fail" && d.status === "open").map((d) => d.question)

test("after a restart: resumed once (a failed resume shows the card), a repeat shows the card, a waiting item is not poked", async () => {
  const fresh = interrupted(), again = interrupted({ resumedBefore: true }), waiting = interrupted()
  decisions.create({ workspace_id: WS, kind: "ask", subject_type: "question", subject_id: waiting.session, item_id: waiting.id, session_id: waiting.session, question: "选哪个？", options: ["a", "b"], requested_by: "dev" })
  orch.recover()
  assert.equal(openFails(fresh.id).length, 0, "no card while the resume is pending")
  assert.match(openFails(again.id)[0] ?? "", /已经自动继续过一次/)
  for (let k = 0; k < 60 && !openFails(fresh.id).length; k++) await new Promise((r) => setTimeout(r, 100))
  assert.deepEqual(userTexts(fresh.session), [orch.RESUME])
  assert.match(openFails(fresh.id)[0] ?? "", /自动继续没有成功/, "the adapter can't start here, so the resume fails")
  assert.deepEqual(userTexts(again.session), [])
  assert.deepEqual(userTexts(waiting.session), [])
  assert.equal(openFails(waiting.id).length, 0)
  assert.equal(sessions.get(waiting.session)!.status, "idle")
})

test("undoing an acceptance restores the plan and puts a dependent that hadn't started back to waiting", () => {
  const x = delivered(false)
  items.patch(x.id, { plan: [{ step: "a", done: true }, { step: "b", done: false }] })
  const dep = items.create(WS, { title: "d", goal: "g", bot_id: "dev", project_id: null, status: "block", stage: "定义" })
  orch.addDep(dep.id, x.id)
  settings.set(WS, { depsAfter: "等我开始" }, "me")
  orch.resolve(x.decision, "pass")
  assert.equal(items.get(dep.id).status, "wait")
  orch.undoDecision(x.decision)
  assert.deepEqual(items.get(x.id).plan.map((p) => p.done), [true, false])
  assert.equal(items.get(dep.id).status, "block")
  assert.ok(!decisions.ofItem(dep.id).some((d) => d.status === "open"))
  settings.set(WS, { depsAfter: "自动开始" }, "me")
})

/* ── git: rebase before a release, tidy up after done / dropped ── */
const { rebaseOnto, tidyWorktree } = await import("../src/lifecycle.js")
const { execFileSync } = await import("node:child_process")
const { existsSync } = await import("node:fs")
const repo = join(home, "repo")
const g = (dir: string, ...a: string[]) => execFileSync("git", ["-C", dir, ...a], { stdio: "pipe", encoding: "utf8" }).trim()
mkdirSync(repo)
g(repo, "init", "-q", "-b", "main"); g(repo, "config", "user.email", "t@t"); g(repo, "config", "user.name", "t")
writeFileSync(join(repo, "a.txt"), "a\n"); g(repo, "add", "."); g(repo, "commit", "-q", "-m", "a")
let wtNo = 0
function branchWith(file: string, text: string, dir = `T-${wtNo + 1}`) {
  ++wtNo
  const name = `steve/${dir.toLowerCase()}`, wt = join(config.worktrees, dir)
  g(repo, "worktree", "add", "-q", "-b", name, wt, "main")
  writeFileSync(join(wt, file), text); g(wt, "add", "."); g(wt, "commit", "-q", "-m", file)
  return { name, wt }
}
const onMain = (file: string, text: string) => { writeFileSync(join(repo, file), text); g(repo, "add", "."); g(repo, "commit", "-q", "-m", "main " + file) }

test("rebase: a branch behind main is rebased; a conflict is aborted and leaves the branch as it was", () => {
  const ok = branchWith("b.txt", "b\n"); onMain("c.txt", "c\n")
  assert.deepEqual(rebaseOnto(ok.wt), { ok: true, moved: true })
  g(ok.wt, "merge-base", "--is-ancestor", "main", "HEAD")
  assert.deepEqual(rebaseOnto(ok.wt), { ok: true, moved: false })

  const bad = branchWith("a.txt", "mine\n"); onMain("a.txt", "theirs\n")
  const head = g(bad.wt, "rev-parse", "HEAD")
  const r = rebaseOnto(bad.wt)
  assert.equal(r.ok, false); assert.equal(!r.ok && r.why, "conflict"); assert.match(!r.ok ? r.detail : "", /a\.txt/)
  assert.equal(g(bad.wt, "rev-parse", "HEAD"), head); assert.equal(g(bad.wt, "status", "--porcelain"), "")

  writeFileSync(join(ok.wt, "b.txt"), "changed\n")
  const d = rebaseOnto(ok.wt); assert.equal(!d.ok && d.why, "dirty")
})

test("tidy: merged → worktree and branch removed; unmerged or dirty work stays", () => {
  const merged = branchWith("m.txt", "m\n"); g(repo, "merge", "-q", "--ff-only", merged.name)
  assert.deepEqual(tidyWorktree(repo, merged.wt, merged.name, { onlyMerged: true }), { worktree: "removed", branch: "deleted" })
  assert.ok(!existsSync(merged.wt))

  const open = branchWith("o.txt", "o\n")
  assert.deepEqual(tidyWorktree(repo, open.wt, open.name, { onlyMerged: true }), { worktree: "unmerged", branch: "kept" })
  assert.ok(existsSync(open.wt))
  /* dropped: a clean worktree goes, the unmerged branch stays */
  assert.deepEqual(tidyWorktree(repo, open.wt, open.name, { onlyMerged: false }), { worktree: "removed", branch: "kept" })
  g(repo, "rev-parse", "--verify", open.name)

  const dirty = branchWith("d.txt", "d\n"); writeFileSync(join(dirty.wt, "untracked.txt"), "x")
  assert.deepEqual(tidyWorktree(repo, dirty.wt, dirty.name, { onlyMerged: false }), { worktree: "dirty", branch: "kept" })
  assert.ok(existsSync(join(dirty.wt, "untracked.txt")))
})

test("a dropped item gives back its clean worktree; a new message brings it back from the kept branch", () => {
  const gp = projects.create(WS, { name: "G", description: "", device_id: "hub", path: repo, kind: "git", worktree: true, default_bot: "dev", release: null })
  const i = items.create(WS, { title: "t", goal: "g", bot_id: "dev", project_id: gp.id, status: "run", stage: "执行" })
  const b = branchWith("x.txt", "x\n", i.id)
  items.patch(i.id, { cwd: b.wt, branch: b.name })
  orch.itemAction(i.id, "drop")
  assert.ok(!existsSync(b.wt)); g(repo, "rev-parse", "--verify", b.name)
  void Promise.resolve(orch.supplement(i.id, "还是要做", "auto")).catch(() => {})
  assert.ok(existsSync(join(b.wt, "x.txt")), "worktree re-created from the kept branch")
})
