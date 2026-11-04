/* Hub API: raw shapes (mirroring hub/src/repo.ts) and calls. store.ts maps them onto the view model. */
export type ItemStatus = "prop" | "run" | "wait" | "pause" | "done" | "drop" | "block"
export type PlanStep = { step: string; done: boolean; left?: boolean }
export type RItem = {
  id: string; workspace_id: string; no: number; title: string; status: ItemStatus; stage: string; bot_id: string | null; project_id: string | null; cwd: string | null; branch: string | null
  plan: PlanStep[]; now: string; accept_mode: string | null; priority: string; archived: boolean; star: boolean; due: string | null; snooze: { label: string; until: number } | null
  source: [string, string]; parent: { item?: string | null; session?: string | null; at?: number | null } | null; created: number; updated: number
  goal: string; goals: number; tags: string[]; deps: string[]; sessionIds: string[]; open: string[]; queued: number; live: boolean
}
export type RItemDetail = RItem & { goalsList: { version: number; text: string; created: number }[]; planRounds?: { round: number; steps: PlanStep[]; end: "done" | "replaced"; created: number }[]; timeline: { id: number; ts: number; actor: string; kind: string; text: string; session_id: string | null; files?: RFile[]; via?: string }[]; artifacts: { id: string; kind: string; title: string; location: string | null; created: number }[]; decisions: RDecision[]; dependents: string[]
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
export type RProject = { id: string; workspace_id: string; name: string; description: string; device_id: string; path: string; kind: "git" | "dir"; worktree: boolean; default_bot: string | null; release: { kind: string; cmd: string; done_when?: "merge" | "tag" } | null; archived: boolean; created: number; updated: number }
export type RWorkspace = { id: string; name: string; description: string; item_prefix: string; sort: number; created: number; role?: string }
export type RTag = { id: string; name: string; cat: string; description: string; created_by: string; created: number }
/** an item whose category Steve filled in unsure (ST-34) */
export type RKindReview = { id: string; kind: string; why: string }
export type RKindBackfill = { checked: number; kept: number; dry: boolean; filled: { id: string; title: string; kind: string; by: "model" | "rule"; confidence: number; uncertain: boolean; why: string; had?: string[] }[] }
export type RMessage = { id: string; author: "me" | "steve"; text: string; item_id: string | null; meta: any; created: number }
export type RDevice = { id: string; name: string; roles: string[]; os: string; version: string; status: string; max_sessions: number; seen: number; created: number }
export type RRuntime = { id: string; name: string; ok: boolean; version: string; path: string; models: string[]; note: string }
export type REvent = { seq: number; ts: number; kind: string; data: any }
export type Proposal = { id: string; title: string; target: { kind: string; id?: string }; href: string; changes: { label: string; from: string; to: string }[] }
export type LocalSession = { key: string; harness: string; title: string; cwd: string; when: number; size: number; imported: boolean }
export type State = {
  version: string; web_version: string; workspace: RWorkspace; workspaces: RWorkspace[]; items: RItem[]; decisions: RDecision[]; recent: RDecision[]; bots: RBot[]; allBots: { id: string; name: string; workspaces: string[] }[]
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
export type HubEvent = { type: "item" | "bot" | "project" | "tag" | "workspace" | "settings" | "device" | "message"; id: string } | { type: "decision" | "session"; id: string; item_id?: string | null } | { type: "event"; session_id: string; seq: number; kind: string; data: unknown } | { type: "steve.thinking"; on: boolean; phase?: "route" | "answer" | "config" | "item" | "supplement" | "research" } | { type: "steve.tool"; ws: string; mid: string; run: string; name: string; brief: string; ok: boolean } | { type: "thread.run"; id: string; ws: string } | { type: "steve.draft"; ws: string; mid: string; thought: string; text: string } | { type: "release.hold"; hold: RHold | null }

/** an uploaded attachment, as messages, comments and session events carry it */
export type RFile = { id: string; name: string; mime: string; size: number }
/** what an item's output holds (`/items/:id/artifacts/:art/preview`); `path` is an entry inside a folder output */
export type ROutFile = { name: string; path: string; size?: number; modified?: number | null }
export type ROutCommit = { hash: string; subject: string; author: string; at: number | null }
export type ROutPreview =
  | ({ type: "text"; lang: string; text: string; truncated: boolean; from?: string } & ROutFile)
  | ({ type: "image" | "pdf" | "video" | "audio"; mime: string; large: boolean } & ROutFile)
  | ({ type: "binary" } & ROutFile)
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
export const wsHeaders = (h: Record<string, string> = {}): Record<string, string> => (pageWs ? { ...h, "x-steve-ws": pageWs } : h)
/** the same for what can't carry a header (an EventSource, an <img>, a download link): `?ws=` */
export const withWs = (url: string) => (pageWs ? `${url}${url.includes("?") ? "&" : "?"}ws=${encodeURIComponent(pageWs)}` : url)

export class Unauthenticated extends Error { login: string | null; constructor(login: string | null) { super("unauthenticated"); this.login = login } }
const CUT = {}
async function req<T>(method: string, path: string, body?: unknown): Promise<T> {
  /* a read that stalls (proxy, dropped connection) must fail, not leave a view loading forever */
  const r = await fetch("/api" + path, { method, headers: wsHeaders(body !== undefined ? { "content-type": "application/json" } : {}), body: body !== undefined ? JSON.stringify(body) : undefined, credentials: "same-origin", signal: method === "GET" ? AbortSignal.timeout(30_000) : undefined })
  if (r.status === 401) { const j = await r.json().catch(() => ({})); throw new Unauthenticated(j.login ?? null) }
  const j = await r.json().catch(() => (r.ok && method === "GET" ? CUT : {}))
  if (j === CUT) throw new Error("响应不完整，稍后重试") // a cut-off body used to come back as {} and break the caller silently
  if (!r.ok) throw new Error(j.error ?? `请求失败（${r.status}）`)
  return j as T
}

/* ───────── module routes (contacts, knowledge, accounts, vault, signals, flows, GitHub …) ─────────
 * Only the Rust Hub serves them. The TS Hub answers an unknown GET /api/* with the SPA's index.html (200) and other
 * methods with a plain-text 404, so "this Hub has no such module" is: a network error, a non-JSON answer that is 200 /
 * 404 / 405, or a JSON 404 / 405 without an `error`. Every module client throws `NotWired` for these, and its page
 * shows the module's Unwired placeholder instead — never sample data. */
export class NotWired extends Error { constructor() { super("连不上 Steve，稍后重试") } }
/** an error answer of a module route that exists; `status` tells 400 / 404 / 409 / 503 apart */
export class HubError extends Error { status: number; constructor(status: number, msg: string) { super(msg); this.status = status } }
export const isNotWired = (e: unknown): e is NotWired => e instanceof NotWired

export async function modReq<T>(method: string, path: string, body?: unknown, signal?: AbortSignal): Promise<T> {
  let r: Response
  try {
    r = await fetch("/api" + path, { method, headers: wsHeaders(body !== undefined ? { "content-type": "application/json" } : {}), body: body !== undefined ? JSON.stringify(body) : undefined, credentials: "same-origin", cache: "no-store", signal: signal ?? (method === "GET" ? AbortSignal.timeout(30_000) : undefined) })
  } catch { throw new NotWired() }
  const json = (r.headers.get("content-type") ?? "").includes("application/json")
  const j: any = json ? await r.json().catch(() => undefined) : undefined
  /* the session's own 401 carries `login`; a 401 from a module (e.g. 请重新登录后再登记) is an ordinary error */
  if (r.status === 401 && (!j || j.error === "unauthenticated" || "login" in j)) throw new Unauthenticated(j?.login ?? null)
  if (!json) {
    if (r.ok && method !== "GET") return {} as T // 204, or an empty body
    if (r.ok || r.status === 404 || r.status === 405) throw new NotWired()
    throw new HubError(r.status, `请求失败（${r.status}）`)
  }
  if ((r.status === 404 || r.status === 405) && !j?.error) throw new NotWired()
  if (!r.ok) throw new HubError(r.status, j?.error ?? `请求失败（${r.status}）`)
  if (j === undefined) throw new Error("响应不完整，稍后重试")
  return j as T
}
/** `?a=1&b=2` from the defined, non-empty values */
export const qs = (o: Record<string, string | number | boolean | undefined | null>) => { const s = new URLSearchParams(); for (const [k, v] of Object.entries(o)) if (v !== undefined && v !== null && v !== "") s.set(k, String(v)); const x = s.toString(); return x ? "?" + x : "" }
const get = <T>(p: string) => req<T>("GET", p), post = <T = any>(p: string, b: unknown = {}) => req<T>("POST", p, b), patch = <T = any>(p: string, b: unknown) => req<T>("PATCH", p, b), del = <T = any>(p: string, b?: unknown) => req<T>("DELETE", p, b)
export const api = {
  state: () => get<State>("/state"),
  me: () => get<{ user: { id: string; name: string; email: string | null; role: string }; version: string; web_version: string }>("/me"),
  switchWs: (id: string) => post(`/workspaces/${id}/switch`), newWs: (p: { name: string; description?: string; item_prefix?: string }) => post<RWorkspace>("/workspaces", p), patchWs: (id: string, p: Partial<RWorkspace>) => patch(`/workspaces/${id}`, p),
  say: (text: string, files: string[] = [], mentions: { kind: "party" | "bot"; id: string }[] = [], to: { item?: string; reply_to?: string } = {}) => post("/thread", { text, files, ...(mentions.length ? { mentions } : {}), ...(to.item ? { item: to.item } : {}), ...(to.reply_to ? { reply_to: to.reply_to } : {}) }),
  stop: () => post("/thread/stop"),
  /* a message said while Steve was on a turn waits for it; until then it can be changed or withdrawn (409 once it went) */
  threadQueuedEdit: (msg: string, text: string) => patch<{ ok: boolean }>(`/thread/queued/${msg}`, { text }),
  threadQueuedWithdraw: (msg: string) => del<{ text: string; files: RFile[] }>(`/thread/queued/${msg}`),
  promote: (msg: string) => post<RItem>(`/thread/${msg}/promote`),
  /* thread routing (§3.4–3.5, §7.2): offers, suggestion cards, corrections, research runs */
  offer: (msg: string, take: boolean) => post<{ message_id: string; item?: RItem }>(`/thread/${msg}/offer`, { take }),
  suggest: (msg: string, choice: SuggestChoice) => post<{ message_id: string; item?: RItem }>(`/thread/${msg}/suggest`, { choice }),
  reroute: (msg: string, to: string) => post<{ message_id: string; item?: RItem }>(`/thread/${msg}/reroute`, { to }),
  stopRun: (run: string) => post<{ ok: boolean }>(`/thread/runs/${run}/stop`), cancelRun: (run: string) => del<{ ok: boolean }>(`/thread/runs/${run}`),
  threadFollow: (item: string, follow: boolean) => patch(`/items/${item}/thread`, { follow }),
  readNotifications: (at?: number) => post<{ seen: number }>("/notifications/read", { at }),
  dismissNotice: (id: string) => post<{ ok: true }>(`/notifications/${encodeURIComponent(id)}/dismiss`, {}),
  item: (id: string) => get<RItemDetail>("/items/" + id),
  outPreview: (id: string, art: string, path?: string) => get<ROutPreview>(`/items/${encodeURIComponent(id)}/artifacts/${encodeURIComponent(art)}/preview${qs({ path })}`),
  newItem: (p: { goal: string; bot?: string; project?: string | null; mode?: "start" | "propose" }) => post<RItem>("/items", p),
  itemAction: (id: string, action: string) => post(`/items/${id}/actions`, { action }), bulk: (ids: string[], op: string) => post<{ n: number }>("/items/bulk", { ids, op }),
  goal: (id: string, text: string) => post(`/items/${id}/goal`, { text }),
  patchItem: (id: string, p: Partial<{ title: string; priority: string; due: string | null; star: boolean; accept_mode: string; project_id: string | null; tags: string[] }>) => patch(`/items/${id}`, p),
  deleteItem: (id: string) => del(`/items/${id}`),
  supplement: (id: string, text: string, mode: "auto" | "steer" | "new", files: string[] = []) => post(`/items/${id}/supplement`, { text, mode, files }), deliver: (id: string) => post(`/items/${id}/deliver`),
  reassign: (id: string, bot: string) => post(`/items/${id}/reassign`, { bot }),
  addDep: (id: string, dep: string) => post(`/items/${id}/deps`, { dep }), rmDep: (id: string, dep: string) => del(`/items/${id}/deps/${dep}`),
  snooze: (id: string, label: string | null, until: number | null) => post(`/items/${id}/snooze`, { label, until }),
  events: (id: string, after = 0) => get<REvent[]>(`/sessions/${id}/events?after=${after}&page=bounded`), searchSessions: (q: string) => get<string[]>(`/sessions/search?q=${encodeURIComponent(q)}`),
  sessionSay: (id: string, text: string, mode: "插话" | "排队", files: string[] = []) => post(`/sessions/${id}/messages`, { text, mode, files }), cancel: (id: string) => post(`/sessions/${id}/cancel`), endSession: (id: string) => post(`/sessions/${id}/end`),
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
  describeProject: (path: string) => post<{ name: string; description: string; git: boolean }>("/projects/describe", { path }), dirs: (path: string) => get<string[]>(`/fs/dirs?path=${encodeURIComponent(path)}`),
  newTag: (p: { name: string; cat: string; description: string }) => post<RTag>("/tags", p), patchTag: (id: string, p: Partial<RTag>) => patch("/tags/" + id, p), deleteTag: (id: string) => del("/tags/" + id), mergeTag: (id: string, into: string) => post(`/tags/${id}/merge`, { into }),
  kindsReview: () => get<RKindReview[]>("/tags/kinds/review"), kindsReviewed: (ids: string[]) => post<RKindReview[]>("/tags/kinds/review", { ids }),
  kindsBackfill: (dry = false) => post<RKindBackfill>("/tags/kinds/backfill", { dry }),
  settings: (p: Record<string, string>) => patch("/settings", p),
  draft: (target: { kind: string; id?: string }, text: string) => post<Proposal>("/config/draft", { target, text }), apply: (id: string) => post(`/config/${id}/apply`), undoConfig: (id: string) => post(`/config/${id}/undo`), dropConfig: (id: string) => post(`/config/${id}/drop`),
  devices: (fresh = false) => get<{ devices: RDevice[]; runtimes: RRuntime[] }>("/devices" + (fresh ? "?fresh=1" : "")), patchDevice: (id: string, p: { name?: string; status?: string; max_sessions?: number }) => patch("/devices/" + id, p),
  /* the raw bytes as the body; the Hub decides the type from the content */
  upload: async (f: Blob, name: string): Promise<RFile> => {
    const r = await fetch("/api/files?name=" + encodeURIComponent(name), { method: "POST", body: f, headers: wsHeaders({ "content-type": "application/octet-stream" }), credentials: "same-origin" })
    if (r.status === 401) { const j = await r.json().catch(() => ({})); throw new Unauthenticated(j.login ?? null) }
    const j = await r.json().catch(() => ({})); if (!r.ok) throw new Error(j.error ?? `上传失败（${r.status}）`); return j as RFile
  },
  logout: () => fetch("/auth/logout", { method: "POST", credentials: "same-origin" }),
}
export function stream(on: (e: HubEvent) => void, onOpen: () => void) {
  let es: EventSource | null = null, retry = 1000, closed = false
  const open = () => {
    if (closed) return // a retry scheduled before close must not reopen
    es = new EventSource(withWs("/api/stream")) // the workspace at each (re)connect
    es.onopen = () => { retry = 1000; onOpen() }
    es.onmessage = (m) => { try { on(JSON.parse(m.data)) } catch {} }
    es.onerror = () => { es?.close(); if (!closed) setTimeout(open, retry = Math.min(retry * 2, 15000)) }
  }
  open(); return () => { closed = true; es?.close() }
}
