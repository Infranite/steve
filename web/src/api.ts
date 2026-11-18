import type { Code } from "./gen"
import type { ResearchEvent } from "@/thread/chat-progress"
/* Hub API: raw shapes (as the Hub's store returns them) and calls. store.ts maps them onto the view model. */
import { lang, t } from "@/i18n"
import { resumableStream } from "./stream-cursor"
import { clientInfo, hello, hubBase, hubUrl, signedIn } from "./hub"
import type { ItemDispatch } from "./bot-dispatch"
import type { Workflow } from "./workflow"
import type { Delivery, DeliveryShare } from "./delivery"
export type ItemStatus = "prop" | "run" | "wait" | "pause" | "done" | "drop" | "fail" | "block"
export type PlanStep = { step: string; done: boolean; left?: boolean }
export type Closeout = {
  status: "syncing" | "sync_failed" | "no_pr" | "waiting_checks" | "checks_failed" | "conflict" | "behind" | "draft"
    | "head" | "uncommitted" | "accepted_unknown" | "closed" | "waiting_github" | "waiting_result" | "ready"
    | "merge_retry" | "merge_method" | "awaiting_approval" | "manual_merge" | "merged"
  next_owner: "steve" | "agent" | "user" | "none"
  pr: { number: number; url: string; head: string; state: string; checks: { passed: number; pending: number; failed: number; other: number; failing: string[] } | null } | null
  last_success_at: number | null; sync_error: string | null; merge_error: string | null
}
export type RItem = {
  id: string; workspace_id: string; no: number; title: string; status: ItemStatus; stage: string; bot_id: string | null; project_id: string | null; cwd: string | null; branch: string | null
  plan: PlanStep[]; now: string; accept_mode: string | null; priority: string; archived: boolean; star: boolean; due: string | null; snooze: { label: string; until: number } | null
  source: [string, string]; parent: { item?: string | null; session?: string | null; at?: number | null } | null; design_gate?: boolean; phase?: "ready" | "active" | "closing" | "closed"; cycle?: number; created: number; updated: number
  goal: string; goals: number; tags: string[]; deps: string[]; sessionIds: string[]; open: string[]; queued: number; live: boolean
  closeout?: Closeout
  workflow_paused?: boolean; workflow_cycle?: number; workflow_failed?: string[]; workflow_attempt?: number; workflow_superseded?: boolean
}
export type DependencyWait = {
  item_id: string; status: ItemStatus; phase: string; cycle: number
  dependencies: { id: string; status: string | null; phase: string | null }[]; open_dependencies: string[]
  resume_mode: "automatic" | "notify"; resume_source: "workspace" | "item" | "fallback"
  resume_override: "workspace" | "automatic" | "notify"; resume_enabled: boolean
  resume_blocker: string | null; resume_fallback_reason: string | null
  waiting: boolean; wait_state: string | null; reason: string | null; active_turn: boolean
  can_register: boolean; register_blocker: string | null
}
export type RItemDetail = RItem & { goalsList: { version: number; text: string; created: number }[]; planRounds?: { round: number; steps: PlanStep[]; end: "done" | "replaced"; created: number }[]; timeline: { id: number; ts: number; actor: string; kind: string; text: string; session_id: string | null; files?: RFile[]; via?: string }[]; artifacts: { id: string; kind: string; title: string; location: string | null; created: number; delivery_id?: string | null }[]; decisions: RDecision[]; dependents: string[]
  workflow?: Workflow | null
  dependency_wait?: DependencyWait | null
  dispatch?: ItemDispatch
  /** outputs delivered as records of the work (self-check screenshots, logs): `{artifact id: "record"}` */
  artifactRoles?: Record<string, string>
  /** the user's notes on the outputs, drafts and sent (ST-38) */
  annotations?: RAnnotation[]
  /** the item came from the main conversation (thread-routing §5): whether it reports back there */
  thread?: { follow: boolean; origin_msg: string } | null }
export type RSession = { id: string; workspace_id: string; item_id: string | null; no: number; bot_id: string | null; harness: string; model: string | null; reasoning_effort?: string | null; cwd: string; harness_session_id?: string | null; status: "starting" | "running" | "idle" | "ended" | "error"; used: number | null; size: number | null; cost: number | null; title: string; device_id: string; source: string; readonly: boolean; busy: boolean; queued: RQueued[]; /** its next turn waits: its device runs as many as its owner set */ room?: RRoom | null; created: number; updated: number }
/** a message waiting for the session's next turn */
export type RQueued = { id: string; session_id?: string; text: string; via: string; created: number; files?: RFile[] }
/** a suggestion card's choice: `here` answers in place, `item` makes the item (or, carrying `item`, appends to that one), `new` makes an item of its own */
export type SuggestChoice = "here" | "item" | "new"
export type SendNowResult = { result: "interrupted" | "sent" | "waiting"; reason?: string }
export type RDecision = { id: string; kind: "accept" | "ask" | "approve" | "fail" | "propose" | "confirm"; subject_type: string; subject_id: string; item_id: string | null; session_id: string | null; question: string; draft: string; options: string[]; status: "open" | "resolved" | "expired"; result: any; requested_by: string; created: number; resolved: number | null; undo: any
  /** opened from inside a turn that is still going: the answer reaches the agent once it stops */
  agent_busy?: boolean
  /** a decision about a chat message: opens it (or its chat) in the platform's own client (newer Hubs) */
  open_url?: string }
export type RBot = { selection_validation?: "supported" | "unsupported" | "unknown" | "default"; id: string; name: string; role: string; main: boolean; harness: string; model: string | null; reasoning_effort?: string | null; rules: string; persona: string; proactive: number; accept_mode: string; perm_mode: string; avatar: { color?: string; skin?: string | null; round?: number }; default_project: string | null; prompt: string | null; device: string | null; enabled: boolean; revision: number; updated: number; workspaces: string[]; memories: number }
/** GET /sessions/:id/resume: the harness's own session, how to resume it there, and whether its transcript is on the Hub's machine */
export type RResume = { harness: string; id: string | null; cwd: string; cwd_exists: boolean; command: string | null; fork_command?: string | null; transcript_checked?: boolean; transcript: { path: string; updated: number | null; id: string | null; cwd: string | null; matches: boolean } | null }
export type RProject = { id: string; workspace_id: string; name: string; description: string; device_id: string; path: string; kind: "git" | "dir"; worktree: boolean; default_bot: string | null; release: { kind: string; cmd: string; done_when?: "merge" | "tag" } | null; archived: boolean; created: number; updated: number }
export type NewProjectInput = { name: string; path: string; description?: string; worktree?: boolean; default_bot?: string | null; done_when?: "merge" | "tag" | null; node?: string }
export type ProjectDestination = { parent: string; directory: string; node: "hub" }
export type ProjectSetup = Omit<NewProjectInput, "path"> & ProjectDestination & { preview: string } & (
  { mode: "empty" } | { mode: "clone"; remote: string; branch?: string }
)
export type CloneProgress = { phase: "connecting" | "receiving" | "resolving" | "checkout" | "registering"; percent?: number }
export type RWorkspace = { id: string; name: string; description: string; item_prefix: string; sort: number; created: number; role?: string }
export type RTag = { id: string; name: string; cat: string; description: string; created_by: string; created: number }
/** an item whose category Steve filled in unsure (ST-34) */
export type RKindReview = { id: string; kind: string; why: string }
export type RKindBackfill = { checked: number; kept: number; dry: boolean; filled: { id: string; title: string; kind: string; by: "model" | "rule"; confidence: number; uncertain: boolean; why: string; had?: string[] }[] }
export type RMessage = { id: string; author: "me" | "steve"; text: string; item_id: string | null; meta: any; created: number }
/** a device's room for sessions as the Hub keeps to it: running now, at most `max` (its saved concurrent-turn limit, 1–256), the rest waiting */
export type RRoom = { running: number; max: number; waiting: number; reason?: "capacity" | "node"; /** actual Node refusal, not a guessed capacity cause */ detail?: string | null }
/** the most a device may be set to run at once */
export const SESSIONS_MAX = 256
export type RDevice = { id: string; name: string; roles: string[]; os: string; version: string; status: string; max_sessions: number; seen: number; created: number; revision?: number; node_id?: string | null; can_configure?: boolean; connected?: boolean | null; config_reason?: string; eligibility?: { state: "eligible" | "ineligible" | "unknown"; reason: string }; occupancy?: number | null; running?: number; waiting?: number }
/** one directory of a Node's machine, for choosing a project's directory: `path` and `parent` show the home directory as `~`;
 *  `denied`: it exists but the Hub is not allowed to read it */
export type RBrowse = { path: string; parent: string | null; exists: boolean; denied: boolean; git: boolean; dirs: { name: string; path: string; git: boolean }[]; more: boolean }
export type RBuild = { version: string; commit: string; target: string }
export type RUpgradePolicy = "auto" | "notify" | "off"
export type RUpgradeDefault = { policy: RUpgradePolicy; hub?: RBuild | null; drain_secs?: number | null; health_secs?: number | null }
/** one Hub-managed upgrade of a Node, as the Hub records it */
export type RNodeUpgradeRun = { operation_id: string; version: string; from_version?: string | null; phase: string; error?: string | null; requested_by: string; force: boolean; created: number; updated: number }
/** what the Hub knows of a Node's build and whether it can upgrade it (`reason`: outdated | current | manual | unknown | hub_dev_build) */
export type RNodeUpgrade = { build?: RBuild | null; policy: RUpgradePolicy; own_policy?: RUpgradePolicy | null; can: boolean; reason: string; latest?: RNodeUpgradeRun | null }
/** how a Node's latest link reached the Hub, as the Hub saw it arrive (kept while it is away; `linked` says whether it is up):
 * `route` where its machine is, `via` what it came through, `latency_ms` the latest round trip of a call while it is linked */
export type RNodeLink = { route: "loopback" | "lan" | "tailnet" | "public" | "unknown"; tls: boolean; via: "direct" | "proxy" | "tailcat"; peer?: string; since: number; latency_ms: number | null; latency_at: number | null }
/** what a Node's machine is, as the Node last said (`node.host`); `os`/`arch` as Rust names them (`macos`, `aarch64`) */
export type RNodeHost = { hostname?: string; os: string; os_name?: string; os_version?: string; arch: string; cpus?: number; mem_total?: number }
/** one removal of a Node from its machine, as the Hub records it: `requested` | `draining` | `cancelling` | `ready` |
 * `confirming`, then `removed` (unpaired), `unconfirmed` (it did not confirm), `failed` or `manual` (it cannot remove itself) */
export type RNodeRemovalRun = { operation_id: string; phase: string; error?: string | null; requested_by: string; grace_secs: number; created: number; updated: number }
/** whether the Hub can ask a Node to remove itself from its machine (`reason`: ok | offline | manual | unknown | local |
 * unpaired), its latest removal, and the command to run on its machine instead */
export type RNodeRemoval = { reason: string; latest?: RNodeRemovalRun | null; command: string; grace_secs?: number }
export type RNode = { node_id: string; name: string; fingerprint: string; state: "pending" | "active" | "paused" | "revoked"; linked: boolean; local: boolean; last_seen?: number | null; link_error?: { at: number; error: string }; max_sessions?: number; running?: number; waiting?: number; owner_context?: boolean | null; upgrade?: RNodeUpgrade; removal?: RNodeRemoval; link?: RNodeLink | null; host?: RNodeHost | null }
/** a Node machine's day (B3): the latest sample, and load per CPU, memory in use (0..1) and round trip over time */
export type RVitals = { sampling?: { interval_ms: number; stale_after_ms: number; now: number }; gaps?: { from: number; to: number }[]; latest: { at: number; sampled_at?: number; cpus?: number; load?: [number, number, number]; mem_total?: number; mem_available?: number; roots?: { root: string; display?: string; free: number; total: number; inodes_free?: number }[]; sessions?: number; rtt_ms?: number | null }; series: { at: number; load: number | null; mem: number | null; rtt: number | null }[] }
export type RPairing = { code: string; expires: number; pin: string | null; hub_id: string }
/** remote Nodes over tailcat (A6): `hub_url` is `tailcat:<address>:<port>` once it runs */
/** a remote install over tailcat's SSH (A6b): the command for the target, then its steps */
export type RInstall = { id: string; state: "waiting" | "running" | "done" | "failed"; command: string; steps: RInstallStep[]; node_id: string | null; error: string | null; failed_step?: RInstallKind | null; address?: string | null; created?: number; transfer_route?: "direct_observed" | "derp_observed" | null }
export type RInstallKind = "probe" | "fetch" | "send" | "setup" | "done"
/** fetch: download, cached (a copy fetched before, checked again) or verify; send: send; setup: install, pair, adapters, start */
export type RInstallProgress = { phase: string; done: number; total: number | null; rate: number | null; eta_secs?: number }
/** a step of a remote install: `elapsed_ms` is up to now while it runs */
export type RInstallStep = { kind: RInstallKind; at: number; text: string; ended?: number | null; elapsed_ms?: number; progress?: RInstallProgress | null; transfer_ms?: number; unpack_ms?: number }
export type RTailcat = { available: boolean; enabled: boolean; running?: boolean; address?: string | null; hub_url?: string | null; port?: number; error?: string | null }
/* `custom`: an adapter the Hub's harness manifest adds (harness parity S1), with `error` and `install_hint` when it has them */
/** a harness adapter as the Hub knows it: what its Node's probe said (modes are Steve's: plan, ask, accept_edits, auto, full_access) */
export type RRuntime = {
  id: string; name: string; ok: boolean; version: string; path: string; models: string[]; note: string; custom?: boolean; builtin?: boolean; error?: string | null; install_hint?: string | null
  adapter_version?: string | null; default_model?: string | null; efforts?: string[]; modes?: string[] | null
  /** its adapter declares where it reads skills, so a Bot's skills reach its sessions (absent: not said) */
  skills?: boolean
  caps?: { sessions: boolean; jobs: boolean; images: boolean; resume: boolean; history: boolean; mcp_http: boolean; system_prompt: boolean; fork?: boolean } | null
  auth?: { id: string; name: string; description?: string | null }[]
  resume_command?: string | null; fork_command?: string | null
  /** Registry-managed components and the local Node's setup result (absent on older Nodes). */
  setup?: {
    state: "not_installed" | "installing" | "ready" | "unavailable" | "unknown"
    components: {
      id: string; name: string; package: string; installed: boolean; version?: string | null; path?: string | null
      /** Evidence for the expected component target; takes precedence over the legacy boolean. */
      status?: "installed" | "missing" | "unknown"
      /** Another discovered executable; does not establish that the expected target is complete. */
      detected?: { path: string; version?: string | null } | null
    }[]
    can_install: boolean; reason?: string | null; help?: string | null; error?: string | null
  }
  /** an adapter Steve installed and keeps up to date: its last check (`policy`: STEVE_ADAPTER_UPDATES) */
  update?: { policy: "auto" | "check" | "off"; installed?: string | null; latest?: string | null; checked?: number | null; error?: string | null; pending?: boolean } | null
}
/** Read-only report from one particular Node, never the Hub's merged model catalogue. */
export type RNodeHarness = {
  id: string; name: string; available: boolean | null; version: string; adapter_version: string | null
  models: string[]; error: string | null; setup: RRuntime["setup"] | null
}
export type RNodeHarnessReport = {
  state: "reported" | "unreported" | "offline" | "unsupported" | "failed"
  checked_at: number | null; attempted_at: number | null; stale: boolean; refreshing: boolean
  error: string | null; harnesses: RNodeHarness[]
}
export type REvent = { seq: number; ts: number; kind: string; data: any }
export type Proposal = { id: string; title: string; target: { kind: string; id?: string }; href: string; changes: { label: string; from: string; to: string }[]; patch?: Record<string, unknown> }
export type LocalSession = { key: string; harness: string; title: string; cwd: string; when: number; size: number; imported: boolean }
/** a newer Steve release the Hub's last version check found (`owner`: this user may install it) */
export type StateUpdate = { version: string; owner: boolean }
export type State = {
  version: string; web_version: string; /** the user is the instance's owner */ owner?: boolean; update?: StateUpdate | null; workspace: RWorkspace; workspaces: RWorkspace[]; items: RItem[]; decisions: RDecision[]; recent: RDecision[]; bots: RBot[]; allBots: { id: string; name: string; workspaces: string[] }[]
  projects: RProject[]; tags: RTag[]; tagUses: Record<string, number>; thread: RMessage[]; settings: Record<string, string>; sessions: RSession[]; devices: RDevice[]; runtimes: RRuntime[]
  /* notifications (release results, auto-archive, system notices), newest first; notifySeen = read cursor (ms) */
  notifications?: RNotice[]; notifySeen?: number
  /* Steve is on a turn in the main conversation (routing, answering, or messages waiting their turn) */
  thinking?: boolean
  /** the Hub holds new turns for a release (Rust Hub): the releasing item, turns it still waits for, since (ms) */
  hold?: RHold | null
}
export type RHold = { item: string; running: number; since: number }
export type RNotice = { id: string; kind: "release" | "archive" | "system"; level: "info" | "success" | "fail"; text: string; item_id: string | null; created: number; closed?: boolean }
export type HubEvent = { type: "item" | "bot" | "project" | "tag" | "workspace" | "settings" | "device" | "message"; id: string } | { type: "decision" | "session"; id: string; item_id?: string | null } | { type: "event"; session_id: string; seq: number; kind: string; data: unknown } | { type: "steve.thinking"; on: boolean; phase?: "route" | "answer" | "config" | "item" | "supplement" | "research" } | ResearchEvent | { type: "thread.run"; id: string; ws: string } | { type: "steve.draft"; ws: string; mid: string; thought: string; text: string } | { type: "release.hold"; hold: RHold | null } | { type: "node" | "client"; id: string; user: string } | { type: "stream.gap" | "stream.reset" }

/** an A2UI action as the page sends it (A2UI v0.9.1 `action` plus where it came from); `data`: the surface's data model then */
export type A2uiSubmit = { block: string; surfaceId: string; sourceComponentId: string; name: string; label: string; context: Record<string, unknown>; data: unknown; timestamp: string }
/** what the Hub kept of it on the reply (`meta.a2ui[block:surface]`) */
export type A2uiRecord = { name: string; label?: string; sourceComponentId: string; context: Record<string, unknown>; data?: unknown; at: number; message?: string }
/** an uploaded attachment, as messages, comments and session events carry it */
export type RFile = { id: string; name: string; mime: string; size: number }
/** where on an output a note is: a region of an image (fractions of it), selected text, a PDF page, or the whole file;
 * `at` names the part it is in (a sheet, a slide) */
export type RAnchor = ({ type: "region"; x: number; y: number; w: number; h: number } | { type: "text"; quote: string; prefix?: string; suffix?: string; line?: number } | { type: "page"; page: number } | { type: "file" }) & { at?: string }
/** a note on one output as delivered (`artifact_id` is its version); `path`: a file inside a folder output */
export type RAnnotation = { id: string; item_id: string; artifact_id: string; path: string; anchor: RAnchor; note: string; status: "draft" | "sending" | "sent"; send_key: string | null; timeline_id: number | null; created: number; updated: number; sent: number | null }
/** what an item's output holds (`/items/:id/artifacts/:art/preview`); `path` is an entry inside a folder output */
export type ROutFile = { name: string; path: string; size?: number; modified?: number | null }
export type ROutCommit = { hash: string; subject: string; author: string; at: number | null }
export type ROutPreview =
  | ({ type: "text"; lang: string; text: string; truncated: boolean; from?: string } & ROutFile)
  | ({ type: "image" | "pdf" | "video" | "audio"; mime: string; large: boolean } & ROutFile)
  | ({ type: "binary" } & ROutFile)
  | ({ type: "office"; mime: string; large: boolean } & ROutFile)
  | { type: "dir"; path: string; truncated: boolean; entries: { name: string; dir: boolean; size: number | null; modified: number | null; form: "image" | "pdf" | "video" | "audio" | null }[] }
  | { type: "commit"; hash: string; author: string; at: number | null; message: string; stat: string; diff: string; truncated: boolean }
  | { type: "branch"; branch: string; base: string | null; worktree: string | null; commits: ROutCommit[]; stat: string; diff: string; truncated: boolean }
  | { type: "link"; url: string }
  | { type: "missing"; reason: string }
export const outRawUrl = (item: string, art: string, path?: string, download = false) =>
  withWs(`/api/items/${encodeURIComponent(item)}/artifacts/${encodeURIComponent(art)}/raw${qs({ path, download: download ? 1 : undefined })}`)
export const fileUrl = (id: string, download = false) => withWs(`/api/files/${encodeURIComponent(id)}${download ? "?download=1" : ""}`)

/* the workspace this page shows, sent with every request: the workspace cookie is shared by all tabs, so a tab
 * switching workspace must never redirect another page's requests (the Hub refuses a browser write without it) */
let pageWs = ""
export const setPageWorkspace = (id: string) => { pageWs = id }
/* and the UI language, so the Hub answers errors and labels in it (the Hub falls back to zh-CN) */
export const wsHeaders = (h: Record<string, string> = {}): Record<string, string> => ({ ...h, "accept-language": lang(), ...(pageWs ? { "x-steve-ws": pageWs } : {}) })
/** the same for what can't carry a header (an EventSource, an <img>, a download link): `?ws=` */
export const withWs = (path: string) => { const url = hubUrl(path); return pageWs ? `${url}${url.includes("?") ? "&" : "?"}ws=${encodeURIComponent(pageWs)}` : url }

/** what the Hub's last 401 offered: its sign-in page (`link`: OIDC or the local development login) and/or signing in
 * with a user's name and an access token (`token`, A1) */
export const signIn: { link: string | null; token: boolean; /** the Hub does not let this page's origin in yet (it says so on a 401, or refuses the call outright, below): a
   * browser sign-in could not come back here, signing in by token lets the origin in */ refused: boolean } = { link: null, token: false, refused: false }
export class Unauthenticated extends Error { login: string | null; constructor(login: string | null, token?: boolean) { super("unauthenticated"); this.login = login; signIn.link = login; if (token !== undefined) signIn.token = token } }
/** A Hub on another origin refuses a page it has not let in yet before the call is made (its preflight answers 403),
 * and the page sees only a failed fetch, never the 401. When the Hub still answers `hello` (open to any origin), the
 * page is not signed in there rather than cut off: it is shown how to sign in, by token, which lets its origin in. */
async function refused(e: unknown): Promise<never> {
  if (hubBase() && e instanceof TypeError) {
    const h = await hello(hubBase(), 4000).catch(() => null)
    if (h) { signIn.refused = true; throw new Unauthenticated(null, h.login.token) }
  }
  throw e
}
const CUT = {}
async function apiResponse(method: string, path: string, body?: unknown, signal?: AbortSignal): Promise<Response> {
  /* a read that stalls (proxy, dropped connection) must fail, not leave a view loading forever; a delete (no body) is
   * keepalive so one sent as the page goes away, like a forget whose Undo ran out, still reaches the Hub */
  const r = await fetch(hubUrl("/api" + path), { method, headers: wsHeaders(body !== undefined ? { "content-type": "application/json" } : {}), body: body !== undefined ? JSON.stringify(body) : undefined, credentials: "include", signal: signal ?? (method === "GET" ? AbortSignal.timeout(30_000) : undefined), keepalive: method === "DELETE" }).catch(refused)
  if (r.status === 401) { const j = await r.json().catch(() => ({})); if (r.headers.get("x-steve-origin") === "refused") signIn.refused = true; throw new Unauthenticated(j.login ?? null, j.token === true) }
  return r
}
async function req<T>(method: string, path: string, body?: unknown, seen?: (r: Response) => void, signal?: AbortSignal): Promise<T> {
  const r = await apiResponse(method, path, body, signal)
  const j = await r.json().catch(() => (r.ok && method === "GET" ? CUT : {}))
  if (j === CUT) throw new Error(t("shell:http.incomplete")) // a cut-off body used to come back as {} and break the caller silently
  if (!r.ok) throw new Error(j.error ?? t("shell:http.failed", { status: r.status }))
  seen?.(r)
  return j as T
}
async function setupProject(p: ProjectSetup, progress: (p: CloneProgress) => void): Promise<RProject> {
  const interrupted = () => new Error(t("pages:projects.setupInterrupted"))
  const r = await apiResponse("POST", "/projects/setup", p, AbortSignal.timeout(660_000))
    .catch((e) => { throw e instanceof Unauthenticated ? e : interrupted() })
  if (!r.ok) {
    const answer = await r.json().catch(() => ({}))
    throw new Error(answer.error ?? t("shell:http.failed", { status: r.status }))
  }
  // A POST so the clone URL is never in a query string. The Hub sends SSE JSON and
  // keepalives, and only the terminal project record confirms registration.
  if (!r.headers.get("content-type")?.startsWith("text/event-stream") || !r.body) throw interrupted()
  const reader = r.body.getReader(), decoder = new TextDecoder()
  let pending = ""
  try {
    for (;;) {
      const read = await reader.read().catch(() => { throw interrupted() })
      if (read.done) throw interrupted()
      const lines = (pending + decoder.decode(read.value, { stream: true })).split("\n")
      pending = lines.pop()!
      for (const line of lines) {
        if (!line.startsWith("data: ")) continue
        const event = JSON.parse(line.slice(6)) as CloneProgress | { project: RProject } | { error: string }
        if ("error" in event) throw new Error(event.error)
        if ("project" in event) return event.project
        progress(event)
      }
    }
  } finally {
    void reader.cancel().catch(() => {})
  }
}
/* the change-log watermark each /api/state body was read at (`x-steve-cursor`): the stream resumes after it */
const stateCursors = new WeakMap<object, string>()
export const cursorOf = (s: State): string | null => stateCursors.get(s) ?? null

/* ───────── module routes (contacts, knowledge, accounts, vault, signals, flows, GitHub …) ─────────
 * Only the Rust Hub serves them. The TS Hub answers an unknown GET /api/* with the SPA's index.html (200) and other
 * methods with a plain-text 404, so "this Hub has no such module" is: a network error, a non-JSON answer that is 200 /
 * 404 / 405, or a JSON 404 / 405 without an `error`. Every module client throws `NotWired` for these, and its page
 * shows the module's Unwired placeholder instead — never sample data. */
export class NotWired extends Error { constructor() { super(t("shell:http.unreachable")) } }
/** an error answer of a module route that exists; `status` tells 400 / 404 / 409 / 503 apart */
export class HubError extends Error { status: number; code?: Code; constructor(status: number, msg: string, code?: Code) { super(msg); this.status = status; this.code = code } }
export const isNotWired = (e: unknown): e is NotWired => e instanceof NotWired

/** A resource that outlives a workspace/Hub switch can bind requests to the scope it was loaded in. */
export type ModuleScope = { workspace: string; hub: string }
export async function modReq<T>(method: string, path: string, body?: unknown, signal?: AbortSignal, scope?: ModuleScope): Promise<T> {
  let r: Response
  try {
    const headers = { ...wsHeaders(body !== undefined ? { "content-type": "application/json" } : {}), ...(scope ? { "x-steve-ws": scope.workspace } : {}) }
    r = await fetch(scope ? scope.hub + "/api" + path : hubUrl("/api" + path), { method, headers, body: body !== undefined ? JSON.stringify(body) : undefined, credentials: "include", cache: "no-store", signal: signal ?? (method === "GET" ? AbortSignal.timeout(30_000) : undefined) })
  } catch { throw new NotWired() }
  const json = (r.headers.get("content-type") ?? "").includes("application/json")
  const j: any = json ? await r.json().catch(() => undefined) : undefined
  /* the session's own 401 carries `login`; a 401 from a module (e.g. 请重新登录后再登记) is an ordinary error */
  if (r.status === 401 && (!j || j.error === "unauthenticated" || "login" in j)) { if (r.headers.get("x-steve-origin") === "refused") signIn.refused = true; throw new Unauthenticated(j?.login ?? null, j?.token === true) }
  if (!json) {
    if (r.ok && method !== "GET") return {} as T // 204, or an empty body
    if (r.ok || r.status === 404 || r.status === 405) throw new NotWired()
    throw new HubError(r.status, t("shell:http.failed", { status: r.status }))
  }
  if ((r.status === 404 || r.status === 405) && !j?.error) throw new NotWired()
  if (!r.ok) throw new HubError(r.status, j?.error ?? t("shell:http.failed", { status: r.status }), j?.code)
  if (j === undefined) throw new Error(t("shell:http.incomplete"))
  return j as T
}
/** `?a=1&b=2` from the defined, non-empty values */
export const qs = (o: Record<string, string | number | boolean | undefined | null>) => { const s = new URLSearchParams(); for (const [k, v] of Object.entries(o)) if (v !== undefined && v !== null && v !== "") s.set(k, String(v)); const x = s.toString(); return x ? "?" + x : "" }
const get = <T>(p: string) => req<T>("GET", p), post = <T = any>(p: string, b: unknown = {}) => req<T>("POST", p, b), patch = <T = any>(p: string, b: unknown) => req<T>("PATCH", p, b), del = <T = any>(p: string, b?: unknown) => req<T>("DELETE", p, b)
export const api = {
  state: () => { let c: string | null = null; return req<State>("GET", "/state", undefined, (r) => { c = r.headers.get("x-steve-cursor") }).then((s) => { if (c) stateCursors.set(s, c); return s }) },
  me: () => get<{ user: { id: string; name: string; email: string | null; role: string }; version: string; web_version: string }>("/me"),
  switchWs: (id: string) => post(`/workspaces/${id}/switch`), newWs: (p: { name: string; description?: string; item_prefix?: string }) => post<RWorkspace>("/workspaces", p), patchWs: (id: string, p: Partial<RWorkspace>) => patch(`/workspaces/${id}`, p),
  say: (text: string, files: string[] = [], mentions: { kind: "party" | "bot"; id: string }[] = [], to: { item?: string; reply_to?: string; client_id?: string } = {}) => post("/thread", { text, files, ...(mentions.length ? { mentions } : {}), ...(to.item ? { item: to.item } : {}), ...(to.reply_to ? { reply_to: to.reply_to } : {}), ...(to.client_id ? { client_id: to.client_id } : {}) }),
  stop: () => post("/thread/stop"),
  /* a message said while Steve was on a turn waits for it; until then it can be changed or withdrawn (409 once it went) */
  threadQueuedEdit: (msg: string, text: string) => patch<{ ok: boolean }>(`/thread/queued/${msg}`, { text }),
  threadQueuedWithdraw: (msg: string) => del<{ text: string; files: RFile[] }>(`/thread/queued/${msg}`),
  promote: (msg: string) => post<RItem>(`/thread/${msg}/promote`),
  /* the first meeting (the instance owner's): Steve's welcome opens it in the main conversation; skip ends it or says there is none */
  onboardStart: () => post<{ message: string }>("/onboard/start"), onboardSkip: () => post("/onboard/skip"),
  /* thread routing (§3.4–3.5, §7.2): offers, suggestion cards, corrections, research runs */
  offer: (msg: string, take: boolean) => post<{ message_id: string; item?: RItem }>(`/thread/${msg}/offer`, { take }),
  suggest: (msg: string, choice: SuggestChoice) => post<{ message_id: string; item?: RItem }>(`/thread/${msg}/suggest`, { choice }),
  /* an A2UI button in a reply (ST-46): the Hub keeps one submission per surface and says it in the conversation */
  a2ui: (msg: string, a: A2uiSubmit) => post<{ already: boolean; message_id?: string; action: A2uiRecord }>(`/thread/${msg}/a2ui`, a),
  reroute: (msg: string, to: string) => post<{ message_id: string; item?: RItem }>(`/thread/${msg}/reroute`, { to }),
  stopRun: (run: string) => post<{ ok: boolean }>(`/thread/runs/${run}/stop`), cancelRun: (run: string) => del<{ ok: boolean }>(`/thread/runs/${run}`),
  threadFollow: (item: string, follow: boolean) => patch(`/items/${item}/thread`, { follow }),
  readNotifications: (at?: number) => post<{ seen: number }>("/notifications/read", { at }),
  dismissNotice: (id: string) => post<{ ok: true }>(`/notifications/${encodeURIComponent(id)}/dismiss`, {}),
  item: (id: string) => get<RItemDetail>("/items/" + id),
  workflow: (id: string) => get<Workflow | null>(`/items/${encodeURIComponent(id)}/workflow`),
  workflowChange: (id: string, p: { action: "pause" | "resume" | "retry" | "cancel" | "single_session"; expected_revision: number; reason: string; unit_id?: string }) => post<Workflow>(`/items/${encodeURIComponent(id)}/workflow`, p),
  outPreview: (id: string, art: string, path?: string) => get<ROutPreview>(`/items/${encodeURIComponent(id)}/artifacts/${encodeURIComponent(art)}/preview${qs({ path })}`),
  delivery: (id: string) => get<Delivery>(`/file-deliveries/${encodeURIComponent(id)}`),
  /** "我的发布": the deliveries this user manages in the current workspace, newest first */
  myDeliveries: () => get<Delivery[]>("/file-deliveries"),
  setDeliveryInteractive: (id: string, interactive: boolean) => patch<Delivery>(`/file-deliveries/${encodeURIComponent(id)}`, { interactive }),
  deliveryShares: (id: string) => get<DeliveryShare[]>(`/file-deliveries/${encodeURIComponent(id)}/shares`),
  createDeliveryShare: (id: string, expires_at: number) => post<DeliveryShare>(`/file-deliveries/${encodeURIComponent(id)}/shares`, { expires_at }),
  revokeDeliveryShare: (id: string, token: string) => del(`/file-deliveries/${encodeURIComponent(id)}/shares/${encodeURIComponent(token)}`),
  annAdd: (id: string, p: { art: string; path?: string; anchor: RAnchor; note: string }) => post<RAnnotation>(`/items/${encodeURIComponent(id)}/annotations`, p),
  annEdit: (id: string, ann: string, note: string) => patch<RAnnotation>(`/items/${encodeURIComponent(id)}/annotations/${encodeURIComponent(ann)}`, { note }),
  annRemove: (id: string, ann: string) => del<{ ok: boolean }>(`/items/${encodeURIComponent(id)}/annotations/${encodeURIComponent(ann)}`),
  /* one send per key: the same key again (a second click, a retry) is answered `again` and sends nothing */
  annSend: (id: string, ids: string[], key: string) => post<{ ok: boolean; again: boolean; pending?: boolean; sent: number; timeline_id?: number | null; warning?: string }>(`/items/${encodeURIComponent(id)}/annotations/send`, { ids, key }),
  newItem: (p: { goal: string; bot?: string; team?: string; project?: string | null; mode?: "start" | "propose"; client_id?: string }) => post<RItem>("/items", p),
  itemAction: (id: string, action: string) => post(`/items/${id}/actions`, { action }), bulk: (ids: string[], op: string) => post<{ n: number }>("/items/bulk", { ids, op }),
  goal: (id: string, text: string) => post(`/items/${id}/goal`, { text }),
  patchItem: (id: string, p: Partial<{ title: string; priority: string; due: string | null; star: boolean; accept_mode: string; project_id: string | null; tags: string[] }>) => patch(`/items/${id}`, p),
  /* only the instance's owner switches it; nothing else writes it */
  designGate: (id: string, on: boolean) => req<RItem>("PUT", `/items/${encodeURIComponent(id)}/design-gate`, { on }),
  deleteItem: (id: string) => del(`/items/${id}`),
  supplement: (id: string, text: string, mode: "auto" | "steer" | "new", files: string[] = []) => post(`/items/${id}/supplement`, { text, mode, files }), deliver: (id: string) => post(`/items/${id}/deliver`),
  reassign: (id: string, bot: string) => post(`/items/${id}/reassign`, { bot }),
  dependencyWait: (id: string) => get<DependencyWait>(`/items/${id}/dependency-wait`),
  saveDependencyWait: (id: string, body: { expected_cycle: number; resume: DependencyWait["resume_override"]; reason: string }) => post<DependencyWait>(`/items/${id}/dependency-wait`, body),
  addDep: (id: string, dep: string) => post(`/items/${id}/deps`, { dep }), rmDep: (id: string, dep: string) => del(`/items/${id}/deps/${dep}`),
  snooze: (id: string, label: string | null, until: number | null) => post(`/items/${id}/snooze`, { label, until }),
  events: (id: string, after = 0) => get<REvent[]>(`/sessions/${id}/events?after=${after}&page=bounded`), searchSessions: (q: string) => get<string[]>(`/sessions/search?q=${encodeURIComponent(q)}`),
  sessionSay: (id: string, text: string, mode: "插话" | "排队", files: string[] = []) => post(`/sessions/${id}/messages`, { text, mode, files }), cancel: (id: string) => post(`/sessions/${id}/cancel`), endSession: (id: string) => post(`/sessions/${id}/end`), continueSession: (id: string) => post<{ session: string }>(`/sessions/${id}/continue`),
  renameSession: (id: string, title: string) => patch(`/sessions/${id}`, { title }), removeSession: (id: string) => del(`/sessions/${id}`), dropQueued: (id: string) => del("/queued/" + id),
  queued: (sid: string) => get<RQueued[]>(`/sessions/${sid}/queued`), resumeInfo: (sid: string) => get<RResume>(`/sessions/${sid}/resume`),
  queuedEdit: (sid: string, qid: string, text: string) => patch<RQueued>(`/sessions/${sid}/queued/${qid}`, { text }),
  queuedRemove: (sid: string, qid: string) => del<RQueued>(`/sessions/${sid}/queued/${qid}`),
  queuedSendNow: (sid: string, qid: string) => post<SendNowResult>(`/sessions/${sid}/queued/${qid}/send-now`),
  fork: (id: string, seq: number, goal: string, asSession: boolean) => post<{ item?: string; session?: string }>(`/sessions/${id}/fork`, { seq, goal, asSession }), link: (id: string, item: string) => post(`/sessions/${id}/link`, { item }),
  localSessions: () => get<LocalSession[]>("/local-sessions"), importSessions: (keys: string[]) => post<(string | { error: string })[]>("/local-sessions/import", { keys }),
  decide: (id: string, choice: string, text?: string) => post<RDecision>("/decisions/" + id, { choice, text }), undo: (id: string) => post(`/decisions/${id}/undo`), dismiss: (id: string) => post<RDecision>(`/decisions/${id}/dismiss`),
  newBot: (p: Partial<RBot> & { name: string }) => post<RBot>("/bots", p), draftBot: (desc: string) => post<RBot>("/bots/draft", { desc }), patchBot: (id: string, p: Partial<RBot>) => patch<RBot>("/bots/" + id, p),
  botWorkspaces: (id: string, ids: string[]) => req("PUT", `/bots/${id}/workspaces`, { ids }), dupBot: (id: string) => post<RBot>(`/bots/${id}/duplicate`), deleteBot: (id: string, to: string, memory: "delete" | "export") => del<{ moved: string[]; memories: any[] }>("/bots/" + id, { to, memory }), restoreBot: (id: string) => post(`/bots/${id}/restore`),
  botPrompt: (id: string) => get<{ generated: string; custom: string | null }>(`/bots/${id}/prompt`), memories: (id: string) => get<{ id: string; text: string; source: string; created: number }[]>(`/bots/${id}/memories`),
  addMemory: (id: string, text: string) => post(`/bots/${id}/memories`, { text }), editMemory: (id: string, text: string) => patch(`/memories/${id}`, { text }), deleteMemory: (id: string) => del(`/memories/${id}`), clearMemories: () => del("/memories"),
  newProject: (p: NewProjectInput) => post<RProject>("/projects", p),
  projectDestination: (p: ProjectDestination, signal: AbortSignal) => req<{ path: string }>("POST", "/projects/setup/preview", p, undefined, signal),
  setupProject,
   patchProject: (id: string, p: Partial<RProject> & { done_when?: "merge" | "tag" | null }) => patch<RProject>("/projects/" + id, p),
  describeProject: (path: string) => post<{ name: string; description: string; git: boolean }>("/projects/describe", { path }),
  browse: (path: string, node = "hub") => get<RBrowse>(`/fs/browse?path=${encodeURIComponent(path)}&node=${encodeURIComponent(node)}`),
  newTag: (p: { name: string; cat: string; description: string }) => post<RTag>("/tags", p), patchTag: (id: string, p: Partial<RTag>) => patch("/tags/" + id, p), deleteTag: (id: string) => del("/tags/" + id), mergeTag: (id: string, into: string) => post(`/tags/${id}/merge`, { into }),
  kindsReview: () => get<RKindReview[]>("/tags/kinds/review"), kindsReviewed: (ids: string[]) => post<RKindReview[]>("/tags/kinds/review", { ids }),
  kindsBackfill: (dry = false) => post<RKindBackfill>("/tags/kinds/backfill", { dry }),
  settings: (p: Record<string, string>) => patch("/settings", p),
  /** `field` + `base` (bot targets): draft only the bot's working rules or own system prompt, from the text the editor shows */
  draft: (target: { kind: string; id?: string; field?: "rules" | "prompt"; base?: string }, text: string) => post<Proposal>("/config/draft", { target, text }), apply: (id: string) => post(`/config/${id}/apply`), undoConfig: (id: string) => post(`/config/${id}/undo`), dropConfig: (id: string) => post(`/config/${id}/drop`),
  devices: (fresh = false) => get<{ devices: RDevice[]; runtimes: RRuntime[] }>("/devices" + (fresh ? "?fresh=1" : "")),
  /** Installs only this registry entry's declared components, on its Node. */
  installHarness: (id: string) => post<{ accepted: boolean; restart?: boolean }>("/system/harnesses/" + encodeURIComponent(id) + "/install"),
  harnessUpdates: (node: string, fresh = false) => get<import("./harness-update-status").HarnessUpdates>("/node-harness-updates/" + encodeURIComponent(node) + (fresh ? "?fresh=1" : "")),
  upgradeHarness: (node: string, harness: string, operation: string, versions: Record<string, string>, retry_unknown = false) => post<{ receipt: import("./harness-update-status").HarnessReceipt }>("/node-harness-updates/" + encodeURIComponent(node) + "/" + encodeURIComponent(harness), { operation, versions, retry_unknown }),
  /** declares a harness adapter in the Hub's manifest (in force after the Hub restarts) */
  addHarness: (p: { id: string; name?: string; command: string; args?: string[]; env?: Record<string, string>; properties?: Record<string, unknown> }) => post<{ saved: boolean; restart: boolean }>("/system/harnesses", p),
  removeHarness: (id: string) => del<{ removed: boolean; restart: boolean }>("/system/harnesses/" + encodeURIComponent(id)), patchDevice: (id: string, p: { revision: number; name?: string; status?: string; max_sessions?: number }) => modReq<{ device: RDevice }>("PATCH", "/devices/" + encodeURIComponent(id), p, AbortSignal.timeout(30_000)),
  /** `off`: this Hub does not listen for remote Nodes (STEVE_NODE_LISTEN=0, or a read-only copy) */
  nodes: () => get<{ nodes: RNode[]; off?: boolean }>("/nodes"),
  nodeHarnesses: () => get<{ nodes: Record<string, RNodeHarnessReport> }>("/node-harnesses"),
  refreshNodeHarnesses: (id: string) => get<{ node_id: string; report: RNodeHarnessReport }>(`/node-harnesses/${encodeURIComponent(id)}?fresh=1`),
  createPairing: () => post<RPairing>("/nodes/pairings"),
  vitals: () => get<{ nodes: Record<string, RVitals> }>("/vitals"),
  nodeTags: () => get<{ tags: Record<string, string[]> }>("/node-tags"),
  setNodeTags: (node: string, tags: string[]) => req<{ tags: string[] }>("PUT", `/node-tags/${encodeURIComponent(node)}`, { tags }),
  setItemNodeTag: (item: string, tag: string | null) => req<{ ok: boolean }>("PUT", `/items/${encodeURIComponent(item)}/node-tag`, { tag }),
  nodesTailcat: () => get<RTailcat>("/nodes/tailcat"),
  setNodesTailcat: (enabled: boolean) => req<RTailcat>("PUT", "/nodes/tailcat", { enabled }),
  createInstall: () => post<RInstall>("/nodes/installs"),
  /** the caller's installs, newest first */
  installs: () => get<{ installs: RInstall[] }>("/nodes/installs"),
  install: (id: string) => get<RInstall>(`/nodes/installs/${encodeURIComponent(id)}`),
  runInstall: (id: string, address: string) => post<RInstall>(`/nodes/installs/${encodeURIComponent(id)}/run`, { address }),
  cancelInstall: (id: string) => del<{ ok: boolean }>(`/nodes/installs/${encodeURIComponent(id)}`),
  confirmNode: (id: string) => post<{ ok: boolean }>(`/nodes/${encodeURIComponent(id)}/confirm`),
  /* the owner's Node actions: stop or resume taking new work (running sessions carry on), rename, unpair */
  pauseNode: (id: string) => post<{ ok: boolean }>(`/nodes/${encodeURIComponent(id)}/pause`),
  resumeNode: (id: string) => post<{ ok: boolean }>(`/nodes/${encodeURIComponent(id)}/resume`),
  renameNode: (id: string, name: string) => post<{ ok: boolean }>(`/nodes/${encodeURIComponent(id)}/rename`, { name }),
  revokeNode: (id: string) => post<{ ok: boolean }>(`/nodes/${encodeURIComponent(id)}/revoke`),
  /** asks the Node to remove itself from its machine (stop, uninstall its service, delete its home), then unpairs it;
   *  refused with `offline`, `manual` (it cannot) or `busy` */
  removeNode: (id: string) => post<{ operation: string }>(`/nodes/${encodeURIComponent(id)}/remove`),
  nodeLimit: (id: string, max_sessions: number) => post<{ ok: boolean }>(`/nodes/${encodeURIComponent(id)}/limit`, { max_sessions }),
  nodeOwnerContext: (id: string, owner_context: "inherit" | "on" | "off") => post<{ ok: boolean }>(`/nodes/${encodeURIComponent(id)}/context`, { owner_context }),
  nodeUpgradePolicy: (id: string, policy: RUpgradePolicy | "") => post<{ ok: boolean }>(`/nodes/${encodeURIComponent(id)}/upgrade_policy`, { policy }),
  upgradeNode: (id: string, force = false) => post<{ operation: string }>(`/nodes/${encodeURIComponent(id)}/upgrade`, { force }),
  /** the Node's upgrade under way stops waiting for its running sessions and switches now; never starts one */
  switchNodeUpgrade: (id: string) => post<{ operation: string }>(`/nodes/${encodeURIComponent(id)}/upgrade/switch`, {}),
  /** `drain_secs`: how long an upgrade waits for running turns; `health_secs`: how long the new release has to link before the Node goes back */
  nodeUpgradeDefault: () => get<RUpgradeDefault>("/nodes/upgrade-default"),
  setNodeUpgradeDefault: (policy: RUpgradePolicy) => req<{ policy: RUpgradePolicy }>("PUT", "/nodes/upgrade-default", { policy }),
  /** the Hub's default for whether a Node owner's sessions load the owner's personal agent config there (a Node may choose for itself) */
  nodeContextDefault: () => get<{ owner_context: "on" | "off" }>("/node-context"),
  setNodeContextDefault: (owner_context: "on" | "off") => req<{ owner_context: "on" | "off" }>("PUT", "/node-context", { owner_context }),
  /* the raw bytes as the body; the Hub decides the type from the content */
  upload: async (f: Blob, name: string): Promise<RFile> => {
    const r = await fetch(hubUrl("/api/files?name=" + encodeURIComponent(name)), { method: "POST", body: f, headers: wsHeaders({ "content-type": "application/octet-stream" }), credentials: "include" })
    if (r.status === 401) { const j = await r.json().catch(() => ({})); throw new Unauthenticated(j.login ?? null, j.token === true) }
    const j = await r.json().catch(() => ({})); if (!r.ok) throw new Error(j.error ?? t("shell:http.uploadFailed", { status: r.status })); return j as RFile
  },
  /* a browser session and a token-signed client alike */
  logout: () => Promise.all([fetch(hubUrl("/auth/logout"), { method: "POST", credentials: "include" }), fetch(hubUrl("/auth/client/logout"), { method: "POST", credentials: "include" })]),
  /** signs this page in with a user's name and an access token; the Hub keeps the client's credential in an HttpOnly cookie */
  tokenSignIn: async (user: string, token: string, version: string): Promise<void> => {
    const r = await fetch(hubUrl("/auth/token"), { method: "POST", headers: { "content-type": "application/json", "accept-language": lang() }, credentials: "include",
      body: JSON.stringify({ user, token, cookie: true, client: clientInfo(version) }) })
    if (!r.ok) { const j = await r.json().catch(() => ({})); throw new Error(j.error ?? t("shell:http.failed", { status: r.status })) }
    await signedIn(hubBase())
  },
}
/** the live stream; `cursor` is the watermark of the state on screen, `onOpen(resumed)` is false when the Hub could not
 * be asked to resume (no cursor yet): then everything on screen has to be refetched */
export function stream(on: (e: HubEvent) => void, onOpen: (resumed: boolean) => void, cursor: () => string | null = () => null, onDown?: () => void) {
  const s = resumableStream({
    url: (after) => withWs(`/api/stream?lang=${encodeURIComponent(lang())}${after ? "&after=" + encodeURIComponent(after) : ""}`), // the workspace and language at each (re)connect
    source: (url) => new EventSource(url, { withCredentials: true }),
    on: (data) => { try { on(JSON.parse(data)) } catch {} },
    onOpen, cursor, onDown,
    setTimeout: (f, ms) => setTimeout(f, ms), clearTimeout: (x) => clearTimeout(x as ReturnType<typeof setTimeout>), now: Date.now,
    win: window, doc: document,
  })
  return s.close
}
