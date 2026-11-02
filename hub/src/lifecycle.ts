/* Item status from its open decisions; decisions the conversation has moved past expire instead of waiting forever.
 * Item branches in git: brought up to date with main before a release, given back after the item is finished. */
import { execFileSync } from "node:child_process"
import { existsSync } from "node:fs"
import { items, decisions, timeline, deps, events, type DecisionKind } from "./repo.js"

export const depsOpen = (itemId: string) => deps.of(itemId).filter((d) => items.get(d)?.status !== "done")

/** recompute the user-facing status from open decisions and prerequisites */
export function settle(itemId: string) {
  const i = items.get(itemId); if (!i || ["done", "drop", "pause"].includes(i.status)) return
  const open = decisions.ofItem(itemId).filter((d) => d.status === "open")
  if (open.length) { items.patch(itemId, { status: "wait" }); return }
  if (i.status === "block" && depsOpen(itemId).length) return
  items.patch(itemId, { status: "run" })
}

/** questions and failure cards are only meaningful until the conversation goes on without them:
 *  the user writes to the session anyway, or the bot keeps working, asks again or delivers */
export function supersede(itemId: string, kinds: DecisionKind[], why: string) {
  const xs = decisions.ofItem(itemId).filter((d) => d.status === "open" && kinds.includes(d.kind))
  for (const d of xs) decisions.resolve(d.id, { superseded: why }, "expired")
  if (xs.length) { timeline.add(itemId, "steve", "sys", `${xs.length} 件待处理已失效：${why}`); settle(itemId) }
  return xs.length
}

/** a turn cut off by a Hub restart continues by itself once; if the same session was already resumed
 *  in the last half hour (the restart keeps happening during its turn), it waits for the user instead */
export const RESUME_WINDOW = 30 * 60_000
export function mayResume(sessionId: string, at = Date.now()) {
  return !events.after(sessionId, 0).some((e) => e.kind === "system" && e.data?.resumed && at - e.ts < RESUME_WINDOW)
}

/** cards left from before these rules: a question or failure older than a delivery or approval request has been moved past */
export function tidy(itemId: string) {
  const open = decisions.ofItem(itemId).filter((d) => d.status === "open")
  const later = Math.max(0, ...open.filter((d) => d.kind === "accept" || d.kind === "approve").map((d) => d.created))
  const stale = open.filter((d) => (d.kind === "ask" || d.kind === "fail") && d.created < later)
  for (const d of stale) decisions.resolve(d.id, { superseded: "Bot 后来已经交付或请求批准" }, "expired")
  if (stale.length) { timeline.add(itemId, "steve", "sys", `${stale.length} 件待处理已失效：Bot 后来已经交付或请求批准`); settle(itemId) }
  return stale.length
}

/* ── git: nothing here forces anything — a conflicting rebase is aborted, a dirty worktree stays, an unmerged branch stays ── */
const git = (dir: string, ...args: string[]) => execFileSync("git", ["-C", dir, ...args], { stdio: "pipe", encoding: "utf8", env: { ...process.env, GIT_EDITOR: "true" } }).trim()
const succeeds = (dir: string, ...args: string[]) => { try { git(dir, ...args); return true } catch { return false } }
const errText = (e: any) => String(e?.stderr || e?.message || e).trim().slice(0, 300)

export type Rebase = { ok: true; moved: boolean } | { ok: false; why: "dirty" | "conflict" | "error"; detail: string }
/** rebase the branch checked out in `worktree` onto `base`; on a conflict the rebase is aborted and the worktree is as before */
export function rebaseOnto(worktree: string, base = "main"): Rebase {
  try {
    const dirty = git(worktree, "status", "--porcelain", "--untracked-files=no")
    if (dirty) return { ok: false, why: "dirty", detail: dirty.split("\n").slice(0, 10).join("\n") }
    if (succeeds(worktree, "merge-base", "--is-ancestor", base, "HEAD")) return { ok: true, moved: false }
    const before = git(worktree, "rev-parse", "HEAD")
    try { git(worktree, "rebase", base) } catch (e) {
      let files = ""; try { files = git(worktree, "diff", "--name-only", "--diff-filter=U") } catch {}
      try { git(worktree, "rebase", "--abort") } catch {}
      return files ? { ok: false, why: "conflict", detail: files.split("\n").join("、") } : { ok: false, why: "error", detail: errText(e) }
    }
    return { ok: true, moved: git(worktree, "rev-parse", "HEAD") !== before }
  } catch (e) { return { ok: false, why: "error", detail: errText(e) } }
}

export type Tidy = { worktree: "removed" | "dirty" | "unmerged" | "absent"; branch: "deleted" | "kept" | "absent" }
/** after an item is done or dropped: remove its worktree when it is clean (and, if `onlyMerged`, when the branch is merged),
 * and delete the branch when it is fully merged into `base`. Unmerged work is never deleted. */
export function tidyWorktree(repo: string, worktree: string, branch: string, opts: { onlyMerged: boolean; base?: string }): Tidy {
  const base = opts.base ?? "main"
  const exists = succeeds(repo, "rev-parse", "--verify", "--quiet", `refs/heads/${branch}`)
  const merged = exists && succeeds(repo, "merge-base", "--is-ancestor", `refs/heads/${branch}`, base)
  let wt: Tidy["worktree"] = "absent"
  if (existsSync(worktree)) {
    /* `git worktree remove` without --force refuses modified or untracked files */
    if (opts.onlyMerged && !merged) wt = "unmerged"
    else wt = succeeds(repo, "worktree", "remove", worktree) ? "removed" : "dirty"
  }
  let br: Tidy["branch"] = exists ? "kept" : "absent"
  /* -d checks again that the branch is merged */
  if (exists && merged && wt !== "dirty" && wt !== "unmerged" && succeeds(repo, "branch", "-d", branch)) br = "deleted"
  return { worktree: wt, branch: br }
}
