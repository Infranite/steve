/** GitHub installations, repositories, and their activity. */
import { modReq as req, qs as q } from "@/api"

export type GhSettings = { after_accept: "ask_pr" | "open_pr" | "none"; finish_on: "accepted" | "merged"; merge_method: "squash" | "merge" | "rebase"; delete_branch: boolean; ci_fail: "supplement" | "signal"; draft_pr: boolean; pr_review?: boolean; issue_triage?: boolean }
export type GhConnection = { id: string; kind: "app" | "pat"; account: string; status: string }
export type GhRepo = {
  id: string; full_name: string; private: boolean; default_branch: string; connection: { id: string; kind: "app" | "pat"; account: string }
  project: { id: string; name: string } | null; ingest: "app" | "repo_hook" | "poll"; healthy: boolean; problem: string | null; last_delivery: number | null
  settings: GhSettings; open_prs: number; open_issues: number; enabled: boolean; revision: number
}
export type GhItemRef = { id: string; title: string; status: string } | null
export type GhCheck = { name: string; state: "ok" | "fail" | "pending" | "skipped"; url: string; job_id: number | null }
export type GhPr = {
  id: string; repo: string; number: number; title: string; url: string; state: "open" | "closed" | "merged"; draft: boolean; head_ref: string; head_sha: string
  checks_state: "none" | "pending" | "success" | "failure"; checks: GhCheck[]; review_state: string; mergeable: string | null; item: GhItemRef; link_source: string | null; opened_by: string; updated: number
}
export type GhIssue = { id: string; repo_id: string; number: number; title: string; state: string; labels: string[]; url: string; item: GhItemRef }
export type GhRun = {
  id: string; flow_id: string; flow_name: string; flow_deleted: boolean; status: string
  outcome: string | null; label: string; error: string | null; item_id: string | null
  publication: { state: string; error?: string | null; url?: string | null } | null
}
export type GhTrace = {
  kind: "pr_review" | "issue_triage"; state: "matched" | "skipped" | "failed"; reason: string
  number: number | null; base: string | null; head: string | null; event_id: string | null; runs: GhRun[]
}
export type GhDelivery = { id: string; received: number; event: string; action: string | null; status: "received" | "ok" | "ignored" | "failed"; error: string | null; repo_id: string | null; automation?: GhTrace | null }
export type GhOverview = { apps: { slug: string; owner: string; html_url: string }[]; connections: GhConnection[]; repos: GhRepo[]; suggestions: { project_id: string; full_name: string; gh_id: number; connection_id: string }[] }
export type GhAvailable = { gh_id: number; full_name: string; private: boolean; linked: boolean }
export type GhManifest = { action: string; manifest: unknown; state: string }

export const gh = {
  overview: () => req<GhOverview>("GET", "/github"),
  available: (connection: string) => req<GhAvailable[]>("GET", "/github/available" + q({ connection })),
  link: (p: { connection_id: string; gh_id: number; project_id?: string | null }) => req<GhRepo>("POST", "/github/repos", p),
  patch: (id: string, p: { project_id?: string | null; settings?: Partial<GhSettings>; enabled?: boolean }) => req<GhRepo>("PATCH", "/github/repos/" + encodeURIComponent(id), p),
  unlink: (id: string) => req<{ ok: true }>("DELETE", "/github/repos/" + encodeURIComponent(id)),
  sync: (id: string) => req<unknown>("POST", `/github/repos/${encodeURIComponent(id)}/sync`, {}),
  prs: (repo: string, state: "open" | "all") => req<GhPr[]>("GET", "/github/prs" + q({ repo, state })),
  issues: (repo: string, state: "open" | "all") => req<GhIssue[]>("GET", "/github/issues" + q({ repo, state })),
  deliveries: (repo: string, limit = 50, automation?: "only") => req<GhDelivery[]>("GET", "/github/deliveries" + q({ repo, limit, automation })),
  commentDraft: (item: string) => req<{ id: string }>("POST", `/github/analysis/${encodeURIComponent(item)}/comment-draft`, {}),
  manifest: (owner?: string) => req<GhManifest>("POST", "/github/app/manifest", owner ? { owner } : {}),
  /* a PAT is a generic connection (accounts A2) with the github_pat adapter */
  connectPat: (token: string) => req<{ id: string }>("POST", "/connections", { adapter: "github_pat", fields: { token } }),
}

/** step 1 of the manifest flow: GitHub wants a top-level form POST with the manifest as a JSON string */
export function submitManifest(m: GhManifest) {
  const f = document.createElement("form")
  f.method = "post"; f.action = m.action; f.style.display = "none"
  const i = document.createElement("input")
  i.type = "hidden"; i.name = "manifest"; i.value = JSON.stringify(m.manifest)
  f.appendChild(i); document.body.appendChild(f); f.submit()
}
