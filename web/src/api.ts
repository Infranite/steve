/* Hub API: raw shapes (mirroring hub/src/repo.ts) and calls. store.ts maps them onto the view model. */
import { lang, t } from "@/i18n"
import { resumableStream } from "./stream-cursor"
import { hubUrl } from "./hub"
import type { Workflow } from "./workflow"
export type ItemStatus = "prop" | "run" | "wait" | "pause" | "done" | "drop" | "block"
export type PlanStep = { step: string; done: boolean; left?: boolean }
export type RItem = {
  id: string; workspace_id: string; no: number; title: string; status: ItemStatus; stage: string; bot_id: string | null; project_id: string | null; cwd: string | null; branch: string | null
  plan: PlanStep[]; now: string; accept_mode: string | null; priority: string; archived: boolean; star: boolean; due: string | null; snooze: { label: string; until: number } | null
  source: [string, string]; parent: { item?: string | null; session?: string | null; at?: number | null } | null; created: number; updated: number
  goal: string; goals: number; tags: string[]; deps: string[]; sessionIds: string[]; open: string[]; queued: number; live: boolean
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
export type RItemDetail = RItem & { goalsList: { version: number; text: string; created: number }[]; planRounds?: { round: number; steps: PlanStep[]; end: "done" | "replaced"; created: number }[]; timeline: { id: number; ts: number; actor: string; kind: string; text: string; session_id: string | null; files?: RFile[]; via?: string }[]; artifacts: { id: string; kind: string; title: string; location: string | null; created: number }[]; decisions: RDecision[]; dependents: string[]
  workflow?: Workflow | null
  dependency_wait?: DependencyWait | null
  /** outputs delivered as records of the work (self-check screenshots, logs): `{artifact id: "record"}` */
  artifactRoles?: Record<string, string>
  /** the user's notes on the outputs, drafts and sent (ST-38) */
  annotations?: RAnnotation[]
  /** the item came from the main conversation (thread-routing §5): whether it reports back there */
  thread?: { follow: boolean; origin_msg: string } | null }
export type RSession = { id: string; workspace_id: string; item_id: string | null; no: number; bot_id: string | null; harness: string; model: string | null; reasoning_effort?: string | null; cwd: string; harness_session_id?: string | null; status: "starting" | "running" | "idle" | "ended" | "error"; used: number | null; size: number | null; cost: number | null; title: string; device_id: string; source: string; readonly: boolean; busy: boolean; queued: RQueued[]; created: number; updated: number }
/** a message waiting for the session's next turn */
export type RQueued = { id: string; session_id?: string; text: string; via: string; created: number; files?: RFile[] }
/** a suggestion card's choice: `here` answers in place, `item` makes the item (or, carrying `item`, appends to that one), `new` makes an item of its own */
export type SuggestChoice = "here" | "item" | "new"
export type SendNowResult = { result: "interrupted" | "sent" | "waiting"; reason?: string }
export type RDecision = { id: string; kind: "accept" | "ask" | "approve" | "fail" | "propose" | "confirm"; subject_type: string; subject_id: string; item_id: string | null; session_id: string | null; question: string; draft: string; options: string[]; status: "open" | "resolved" | "expired"; result: any; requested_by: string; created: number; resolved: number | null; undo: any
  /** opened from inside a turn that is still going: the answer reaches the agent once it stops */
  agent_busy?: boolean }
export type RBot = { id: string; name: string; role: string; main: boolean; harness: string; model: string | null; reasoning_effort?: string | null; rules: string; persona: string; proactive: number; accept_mode: string; perm_mode: string; avatar: { color?: string; skin?: string | null; round?: number }; default_project: string | null; prompt: string | null; device: string | null; enabled: boolean; revision: number; updated: number; workspaces: string[]; memories: number }
/** GET /sessions/:id/resume: the harness's own session, how to resume it there, and whether its transcript is on the Hub's machine */
export type RResume = { harness: string; id: string | null; cwd: string; cwd_exists: boolean; command: string | null; transcript: { path: string; updated: number | null; id: string | null; cwd: string | null; matches: boolean } | null }
/** `GET /projects/:id/lane`: the target's profile, each fact with where it came from */
export type RLaneFact<T> = { value: T; how: { state: "found" } | { state: "default" } | { state: "unknown"; why: string }; evidence: string[] }
export type RLane = {
  target: { upstream: string; branch: string }; reads: string; fetch_error: string | null
  merge: RLaneFact<"host_queue" | "host_pr" | "push">
  checks: RLaneFact<{ name: string; from: "required" | "workflow" | "suggested"; on?: string[] }[]>
  ship: RLaneFact<"tag" | "script" | "merge">
  numbered: { kind: "version" | "notes" | "migrations" | "lockfile"; path: string; evidence: string; handling: string }[]
  pace: { days: number; landed: number; releases: number; authors: number }
  /** the lane itself; `null` until a request first went into it or you set its limits */
  lane: RLaneState | null
}
export type RLaneMember = { id: string; item_id: string | null; branch: string; label: string }
/** a batch: `approved` false while its card waits for you (what arrives then joins it) */
export type RLaneUnit = { no: number; members: RLaneMember[]; deferred: RLaneMember[]; opened_at: number; closed: boolean; merged: boolean; approved: boolean }
export type RLaneLimits = { window_ms: number; max_members: number }
export type RLaneState = {
  id: string; policy: RLaneLimits; set: { window_ms: number | null; max_members: number | null }; defaults: RLaneLimits
  running: RLaneUnit | null; collecting: (RLaneUnit & { closes_at: number }) | null; queued: RLaneMember[]
}
export type RProject = { id: string; workspace_id: string; name: string; description: string; device_id: string; path: string; kind: "git" | "dir"; worktree: boolean; default_bot: string | null; release: { kind: string; cmd: string; done_when?: "merge" | "tag" } | null; archived: boolean; created: number; updated: number }
export type RWorkspace = { id: string; name: string; description: string; item_prefix: string; sort: number; created: number; role?: string }
export type RTag = { id: string; name: string; cat: string; description: string; created_by: string; created: number }
/** an item whose category Steve filled in unsure (ST-34) */
export type RKindReview = { id: string; kind: string; why: string }
export type RKindBackfill = { checked: number; kept: number; dry: boolean; filled: { id: string; title: string; kind: string; by: "model" | "rule"; confidence: number; uncertain: boolean; why: string; had?: string[] }[] }
export type RMessage = { id: string; author: "me" | "steve"; text: string; item_id: string | null; meta: any; created: number }
export type RDevice = { id: string; name: string; roles: string[]; os: string; version: string; status: string; max_sessions: number; seen: number; created: number }
export type RNode = { node_id: string; name: string; fingerprint: string; state: "pending" | "active" | "paused" | "revoked"; linked: boolean; local: boolean; last_seen?: number | null }
/** a Node machine's day (B3): the latest sample, and load per CPU, memory in use (0..1) and round trip over time */
export type RVitals = { latest: { at: number; cpus?: number; load?: [number, number, number]; mem_total?: number; mem_available?: number; roots?: { root: string; display?: string; free: number; total: number; inodes_free?: number }[]; sessions?: number; rtt_ms?: number | null }; series: { at: number; load: number | null; mem: number | null; rtt: number | null }[] }
export type RPairing = { code: string; expires: number; pin: string | null; hub_id: string }
/** remote Nodes over tailcat (A6): `hub_url` is `tailcat:<address>:<port>` once it runs */
/** a remote install over tailcat's SSH (A6b): the command for the target, then its steps */
export type RInstall = { id: string; state: "waiting" | "running" | "done" | "failed"; command: string; steps: { at: number; text: string }[]; node_id: string | null; error: string | null }
export type RTailcat = { available: boolean; enabled: boolean; running?: boolean; address?: string | null; hub_url?: string | null; port?: number; error?: string | null }
export type RRuntime = { id: string; name: string; ok: boolean; version: string; path: string; models: string[]; note: string }
export type REvent = { seq: number; ts: number; kind: string; data: any }
export type Proposal = { id: string; title: string; target: { kind: string; id?: string }; href: string; changes: { label: string; from: string; to: string }[] }
export type LocalSession = { key: string; harness: string; title: string; cwd: string; when: number; size: number; imported: boolean }
/** a newer Steve release the Hub's last version check found (`owner`: this user may install it) */
export type StateUpdate = { version: string; owner: boolean }
export type State = {
  version: string; web_version: string; update?: StateUpdate | null; workspace: RWorkspace; workspaces: RWorkspace[]; items: RItem[]; decisions: RDecision[]; recent: RDecision[]; bots: RBot[]; allBots: { id: string; name: string; workspaces: string[] }[]
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
export type HubEvent = { type: "item" | "bot" | "project" | "tag" | "workspace" | "settings" | "device" | "message"; id: string } | { type: "decision" | "session"; id: string; item_id?: string | null } | { type: "event"; session_id: string; seq: number; kind: string; data: unknown } | { type: "steve.thinking"; on: boolean; phase?: "route" | "answer" | "config" | "item" | "supplement" | "research" } | { type: "steve.tool"; ws: string; mid: string; run: string; name: string; brief: string; ok: boolean } | { type: "thread.run"; id: string; ws: string } | { type: "steve.draft"; ws: string; mid: string; thought: string; text: string } | { type: "release.hold"; hold: RHold | null } | { type: "stream.gap" | "stream.reset" }

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
export const signIn: { link: string | null; token: boolean } = { link: null, token: false }
export class Unauthenticated extends Error { login: string | null; constructor(login: string | null, token?: boolean) { super("unauthenticated"); this.login = login; signIn.link = login; if (token !== undefined) signIn.token = token } }
const CUT = {}
async function req<T>(method: string, path: string, body?: unknown, seen?: (r: Response) => void): Promise<T> {
  /* a read that stalls (proxy, dropped connection) must fail, not leave a view loading forever */
  const r = await fetch(hubUrl("/api" + path), { method, headers: wsHeaders(body !== undefined ? { "content-type": "application/json" } : {}), body: body !== undefined ? JSON.stringify(body) : undefined, credentials: "include", signal: method === "GET" ? AbortSignal.timeout(30_000) : undefined })
  if (r.status === 401) { const j = await r.json().catch(() => ({})); throw new Unauthenticated(j.login ?? null, j.token === true) }
  const j = await r.json().catch(() => (r.ok && method === "GET" ? CUT : {}))
  if (j === CUT) throw new Error(t("shell:http.incomplete")) // a cut-off body used to come back as {} and break the caller silently
  if (!r.ok) throw new Error(j.error ?? t("shell:http.failed", { status: r.status }))
  seen?.(r)
  return j as T
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
export class HubError extends Error { status: number; constructor(status: number, msg: string) { super(msg); this.status = status } }
export const isNotWired = (e: unknown): e is NotWired => e instanceof NotWired

export async function modReq<T>(method: string, path: string, body?: unknown, signal?: AbortSignal): Promise<T> {
  let r: Response
  try {
    r = await fetch(hubUrl("/api" + path), { method, headers: wsHeaders(body !== undefined ? { "content-type": "application/json" } : {}), body: body !== undefined ? JSON.stringify(body) : undefined, credentials: "include", cache: "no-store", signal: signal ?? (method === "GET" ? AbortSignal.timeout(30_000) : undefined) })
  } catch { throw new NotWired() }
  const json = (r.headers.get("content-type") ?? "").includes("application/json")
  const j: any = json ? await r.json().catch(() => undefined) : undefined
  /* the session's own 401 carries `login`; a 401 from a module (e.g. 请重新登录后再登记) is an ordinary error */
  if (r.status === 401 && (!j || j.error === "unauthenticated" || "login" in j)) throw new Unauthenticated(j?.login ?? null, j?.token === true)
  if (!json) {
    if (r.ok && method !== "GET") return {} as T // 204, or an empty body
    if (r.ok || r.status === 404 || r.status === 405) throw new NotWired()
    throw new HubError(r.status, t("shell:http.failed", { status: r.status }))
  }
  if ((r.status === 404 || r.status === 405) && !j?.error) throw new NotWired()
  if (!r.ok) throw new HubError(r.status, j?.error ?? t("shell:http.failed", { status: r.status }))
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
  workflowChange: (id: string, p: { action: "pause" | "resume" | "retry" | "cancel"; expected_revision: number; reason: string; unit_id?: string }) => post<Workflow>(`/items/${encodeURIComponent(id)}/workflow`, p),
  outPreview: (id: string, art: string, path?: string) => get<ROutPreview>(`/items/${encodeURIComponent(id)}/artifacts/${encodeURIComponent(art)}/preview${qs({ path })}`),
  annAdd: (id: string, p: { art: string; path?: string; anchor: RAnchor; note: string }) => post<RAnnotation>(`/items/${encodeURIComponent(id)}/annotations`, p),
  annEdit: (id: string, ann: string, note: string) => patch<RAnnotation>(`/items/${encodeURIComponent(id)}/annotations/${encodeURIComponent(ann)}`, { note }),
  annRemove: (id: string, ann: string) => del<{ ok: boolean }>(`/items/${encodeURIComponent(id)}/annotations/${encodeURIComponent(ann)}`),
  /* one send per key: the same key again (a second click, a retry) is answered `again` and sends nothing */
  annSend: (id: string, ids: string[], key: string) => post<{ ok: boolean; again: boolean; pending?: boolean; sent: number; timeline_id?: number | null; warning?: string }>(`/items/${encodeURIComponent(id)}/annotations/send`, { ids, key }),
  newItem: (p: { goal: string; bot?: string; project?: string | null; mode?: "start" | "propose"; client_id?: string }) => post<RItem>("/items", p),
  itemAction: (id: string, action: string) => post(`/items/${id}/actions`, { action }), bulk: (ids: string[], op: string) => post<{ n: number }>("/items/bulk", { ids, op }),
  goal: (id: string, text: string) => post(`/items/${id}/goal`, { text }),
  patchItem: (id: string, p: Partial<{ title: string; priority: string; due: string | null; star: boolean; accept_mode: string; project_id: string | null; tags: string[] }>) => patch(`/items/${id}`, p),
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
  newProject: (p: { name: string; path: string; description?: string; worktree?: boolean; default_bot?: string | null }) => post<RProject>("/projects", p), patchProject: (id: string, p: Partial<RProject>) => patch<RProject>("/projects/" + id, p),
  lane: (id: string) => get<RLane>(`/projects/${encodeURIComponent(id)}/lane`),
  lanePolicy: (id: string, p: { window_min: number | null; max_members: number | null }) => patch<{ lane: RLaneState | null }>(`/projects/${encodeURIComponent(id)}/lane`, p),
  laneNow: (id: string) => post<{ closed: boolean; lane: RLaneState | null }>(`/projects/${encodeURIComponent(id)}/lane/now`),
  describeProject: (path: string) => post<{ name: string; description: string; git: boolean }>("/projects/describe", { path }), dirs: (path: string) => get<string[]>(`/fs/dirs?path=${encodeURIComponent(path)}`),
  newTag: (p: { name: string; cat: string; description: string }) => post<RTag>("/tags", p), patchTag: (id: string, p: Partial<RTag>) => patch("/tags/" + id, p), deleteTag: (id: string) => del("/tags/" + id), mergeTag: (id: string, into: string) => post(`/tags/${id}/merge`, { into }),
  kindsReview: () => get<RKindReview[]>("/tags/kinds/review"), kindsReviewed: (ids: string[]) => post<RKindReview[]>("/tags/kinds/review", { ids }),
  kindsBackfill: (dry = false) => post<RKindBackfill>("/tags/kinds/backfill", { dry }),
  settings: (p: Record<string, string>) => patch("/settings", p),
  draft: (target: { kind: string; id?: string }, text: string) => post<Proposal>("/config/draft", { target, text }), apply: (id: string) => post(`/config/${id}/apply`), undoConfig: (id: string) => post(`/config/${id}/undo`), dropConfig: (id: string) => post(`/config/${id}/drop`),
  devices: (fresh = false) => get<{ devices: RDevice[]; runtimes: RRuntime[] }>("/devices" + (fresh ? "?fresh=1" : "")), patchDevice: (id: string, p: { name?: string; status?: string; max_sessions?: number }) => patch("/devices/" + id, p),
  nodes: () => get<{ nodes: RNode[] }>("/nodes"),
  createPairing: () => post<RPairing>("/nodes/pairings"),
  vitals: () => get<{ nodes: Record<string, RVitals> }>("/vitals"),
  nodesTailcat: () => get<RTailcat>("/nodes/tailcat"),
  setNodesTailcat: (enabled: boolean) => req<RTailcat>("PUT", "/nodes/tailcat", { enabled }),
  createInstall: () => post<RInstall>("/nodes/installs"),
  install: (id: string) => get<RInstall>(`/nodes/installs/${encodeURIComponent(id)}`),
  runInstall: (id: string, address: string) => post<RInstall>(`/nodes/installs/${encodeURIComponent(id)}/run`, { address }),
  confirmNode: (id: string) => post<{ ok: boolean }>(`/nodes/${encodeURIComponent(id)}/confirm`),
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
    const ua = navigator.userAgent
    const browser = /Edg\//.test(ua) ? "Edge" : /Chrome\//.test(ua) ? "Chrome" : /Firefox\//.test(ua) ? "Firefox" : /Safari\//.test(ua) ? "Safari" : "Browser"
    const r = await fetch(hubUrl("/auth/token"), { method: "POST", headers: { "content-type": "application/json", "accept-language": lang() }, credentials: "include",
      body: JSON.stringify({ user, token, cookie: true, client: { kind: "web", name: browser, platform: (navigator as any).userAgentData?.platform ?? navigator.platform ?? "", version } }) })
    if (!r.ok) { const j = await r.json().catch(() => ({})); throw new Error(j.error ?? t("shell:http.failed", { status: r.status })) }
  },
}
/** the live stream; `cursor` is the watermark of the state on screen, `onOpen(resumed)` is false when the Hub could not
 * be asked to resume (no cursor yet): then everything on screen has to be refetched */
export function stream(on: (e: HubEvent) => void, onOpen: (resumed: boolean) => void, cursor: () => string | null = () => null) {
  const s = resumableStream({
    url: (after) => withWs(`/api/stream?lang=${encodeURIComponent(lang())}${after ? "&after=" + encodeURIComponent(after) : ""}`), // the workspace and language at each (re)connect
    source: (url) => new EventSource(url, { withCredentials: true }),
    on: (data) => { try { on(JSON.parse(data)) } catch {} },
    onOpen, cursor,
    setTimeout: (f, ms) => setTimeout(f, ms), clearTimeout: (x) => clearTimeout(x as ReturnType<typeof setTimeout>), now: Date.now,
    win: window, doc: document,
  })
  return s.close
}
