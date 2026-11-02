// Release hand-off (see test/e2e-release.sh): rebase before release (clean and conflicting), drain while another turn
// runs, prompts queued during the drain arrive after the restart, the item is done only once live, its worktree is given back.
import { execFileSync } from "node:child_process"
import { existsSync, readFileSync } from "node:fs"
import { join } from "node:path"
import { base, login, call, sleep, log, waitFor, openOf, check } from "./lib.mjs"

const home = process.env.STEVE_HOME, repo = process.env.STEVE_REPO, promptLog = process.env.FAKE_ACP_LOG
const git = (dir, ...a) => execFileSync("git", ["-C", dir, ...a], { encoding: "utf8" }).trim()
const prompts = async (itemId) => {
  const ids = await Promise.all((await call("GET", "/api/items/" + itemId)).sessionIds.map(async (s) => (await call("GET", "/api/sessions/" + s)).acp_session_id))
  return (existsSync(promptLog) ? readFileSync(promptLog, "utf8").trim().split("\n").filter(Boolean).map((l) => JSON.parse(l)) : []).filter((p) => ids.includes(p.session)).map((p) => p.text)
}
const item = (goal, project) => call("POST", "/api/items", { goal, title: goal.slice(0, 20), bot: "dev", project })
const decision = (id, kind, sub) => waitFor(async () => (await openOf(id)).find((d) => d.kind === kind && (!sub || d.subject_type === sub)), `${kind} on ${id}`, 60000)
const sessionOf = async (id) => call("GET", "/api/sessions/" + (await call("GET", "/api/items/" + id)).sessionIds.at(-1))

await login()
const steve = (await call("GET", "/api/state")).projects.find((p) => p.path === repo)
check(steve?.release, "Steve's project (the temp repo) has a release action")

// A and D branch off the same main; then main moves: A can't fast-forward, D conflicts on shared.txt
const A = await item("@commit a.txt from-a @deliver", steve.id), D = await item("@commit shared.txt from-d @deliver", steve.id)
const accA = await decision(A.id, "accept"), accD = await decision(D.id, "accept")
execFileSync("bash", ["-c", `cd "${repo}" && echo from-main > shared.txt && git commit -qam "main moves"`])
const mainBefore = git(repo, "rev-parse", "main")

// D: accepted → waits for release; the release approval triggers the rebase, which conflicts → back to the agent
await call("POST", "/api/decisions/" + accD.id, { choice: "pass" })
check((await call("GET", "/api/items/" + D.id)).now === "等待发布", "accepted with a release action: 等待发布, not done")
const relD = await decision(D.id, "approve", "release")
const dHead = git(join(home, "worktrees", D.id), "rev-parse", "HEAD")
await call("POST", "/api/decisions/" + relD.id, { choice: "yes" })
await waitFor(async () => (await prompts(D.id)).some((t) => t.includes("rebase") && t.includes("冲突")), "conflict message to D's agent", 20000)
check(git(join(home, "worktrees", D.id), "rev-parse", "HEAD") === dHead && git(join(home, "worktrees", D.id), "status", "--porcelain") === "", "conflicting rebase aborted, D's worktree untouched")
check(git(repo, "rev-parse", "main") === mainBefore && (await call("GET", "/api/items/" + D.id)).status !== "done", "nothing released, D not done")
log("D:", (await call("GET", "/api/items/" + D.id)).now)

// B: a long turn that is running when the release wants to restart
const B = await item("@sleep 15000", null)
await waitFor(async () => (await sessionOf(B.id)).busy, "B running", 20000)

// A: accepted, publishes, approved → rebased onto main, built, drained (waits for B), restarted, done
await call("POST", "/api/decisions/" + accA.id, { choice: "pass" })
const relA = await decision(A.id, "approve", "release")
const t0 = Date.now()
await call("POST", "/api/decisions/" + relA.id, { choice: "yes" })
const held = await waitFor(async () => (await call("GET", "/api/items/" + A.id)).timeline.find((t) => /等 1 个正在进行的回合结束后重启/.test(t.text)), "drain started", 60000)
log("drain:", held.text)
const C = await item("@sleep 10", null)
await sleep(1500)
const cs = await sessionOf(C.id)
check(!cs.busy && cs.queued.length === 1, "a new item during the drain: its first prompt is queued, no turn starts")
check((await sessionOf(B.id)).busy, "B still running during the drain")

await waitFor(async () => { try { return (await call("GET", "/api/items/" + A.id)).status === "done" } catch { await login().catch(() => {}); return false } }, "A released and done", 120000)
log(`released after ${Math.round((Date.now() - t0) / 1000)}s`)
const health = await (await fetch(base + "/healthz")).json()
check(health.version !== "v0", `the Hub was restarted on the new version (${health.version})`)
const bItem = await call("GET", "/api/items/" + B.id), bEvents = await call("GET", `/api/sessions/${bItem.sessionIds[0]}/events`)
check(bEvents.some((e) => e.kind === "turn_end" && e.data.stopReason === "end_turn"), "B's turn ended normally before the restart")
check(!bItem.decisions.some((d) => d.kind === "fail" && /中断/.test(d.question)) && (await prompts(B.id)).length === 1, "B was not interrupted, nothing re-sent to it")
await waitFor(async () => (await prompts(C.id)).length === 1, "C's queued prompt delivered after the restart", 30000)
check((await prompts(C.id))[0].includes("@sleep 10"), "C got its opening once")
check(git(repo, "log", "--format=%s", "main").split("\n").includes("fake: a.txt") && git(repo, "merge-base", "--is-ancestor", "main", "main") === "", "A's commit is on main")
check(!existsSync(join(home, "worktrees", A.id)), "A's worktree removed")
check(git(repo, "branch", "--list", "steve/" + A.id.toLowerCase()) === "", "A's merged branch deleted")
check(existsSync(join(home, "worktrees", D.id)) && git(repo, "branch", "--list", "steve/" + D.id.toLowerCase()) !== "", "D's unmerged work kept")
console.log(JSON.stringify({ ok: true, version: health.version }))
