/* Knowledge base API (knowledge.md §6). Only the Rust Hub serves it: on the TS Hub every /api/kb route falls through to
 * the SPA's index.html (or a plain 404), which `NotWired` marks so the pages fall back to the "还没有接入" placeholder. */
import { modReq as req, NotWired, qs, wsHeaders } from "@/api"
import { hubUrl } from "@/hub"
import { lazy } from "@/api/accounts"
import { t } from "@/i18n"

export type Level = "public" | "conclusion" | "reasoning" | "never"
export type ObjRef = { type: string; id: string; name?: string }
export type KbSource = {
  id: string; kind: "notes" | "files" | "folder" | "items" | "chat" | "mems" | "bot_mems"; name: string; path?: string; include?: string[]; exclude?: string[]
  visibility: string; level: Level; paused: boolean; status: "ok" | "syncing" | "error"; error: string | null
  stats: { docs?: number; chunks?: number; skipped?: number; bytes?: number; kept?: number; incomplete?: string[] }; last_sync: number | null; builtin: boolean; revision: number
}
export type DocBrief = {
  id: string; source: { id: string; kind: KbSource["kind"]; name: string }; title: string; ref: string; visibility: string; level: Level
  inherited: { visibility: boolean; level: boolean }; status: string; chunks: number; updated: number
}
export type Doc = DocBrief & { body?: string; about: ObjRef[]; tags: string[]; preview: { seq: number; heading: string; text: string }[]; uses: number; error: string | null }
export type Hit = { n: number; type: string; id: string; seq: number; title: string; heading?: string; text: string; label: string; source_kind: string; level: Level; visibility: string; score: number; href: string }
/** where a memory came from (`kind`: you | conversation | item | agent | mail | space | event | plugin | other), with a
 *  link there when there is one, who wrote it and the words it was based on */
export type Origin = { kind: string; type: string; id: string | null; label: string; href: string; writer: string; writer_name: string | null; evidence: string }
export type Mem = {
  id: string; kind: string; text: string; source_label: string; level: Level; status: "candidate" | "confirmed" | "stale"; visibility: string; about: ObjRef[]; created: number
  updated?: number; origin?: Origin; used?: { count: number; last: number | null }
}
/** a person, project, bot … Steve knows things about (`GET /kb/subjects`) */
export type Subject = { type: string; id: string; name: string; href: string; mems: number; candidates: number; docs: number; last: number }
/** where knowledge was handed out: a session, an answer, your search */
export type SubjectUse = { consumer_type: string; consumer_id: string; href: string; name: string; query: string; created: number; obj: { type: string; id: string; label: string } }
export type SubjectDetail = { type: string; id: string; name: string; href: string; mems: Mem[]; docs: (DocBrief & { used?: Mem["used"] })[]; uses: SubjectUse[] }
/** who writes the memories, as the 来源 tab lists them */
export type MemProvider = { kind: string; writer: string; writer_name: string | null; count: number; confirmed: number; candidates: number; stale: number; last: number }
/** what an agent changed in the memories (`GET /kb/changes`); a merge's state follows its candidate */
export type GoneMem = { id: string; gone: true }
export type Change = { id: string; op: "merge" | "resolve" | "flag" | "stale" | "about" | "source"; state: "applied" | "undone" | "pending" | "confirmed" | "dismissed" | "stale" | "open" | "kept"; reason: string; mem: Mem | GoneMem; targets: (Mem | GoneMem)[]; after: Record<string, unknown>; created: number }
export type ChangeRun = { run_id: string; href: string; title: string | null; writer: string; writer_name: string | null; at: number; changes: Change[] }
export const isGone = (m: Mem | GoneMem): m is GoneMem => "gone" in m
/** `record`, `why`, `scope`: what it would become and why it is worth keeping (ST-41; absent on older Hubs) */
export type Candidate = { mem: Mem; origin: string; origin_label: string; evidence: string; supersedes: Mem | null; decision_id: string | null; record?: string; why?: string; scope?: string }
export type ExpiredCandidate = { mem: Mem; origin_label: string; evidence: string; expired: number }
/** one entry of `messages.meta.cites` */
export type Cite = { n: number; label: string; href: string; level: Level }
export type Use = { n: number; type: string; id: string; seq: number; level: Level; title: string; label: string; href: string; deleted: boolean; use_id: string; query: string; created: number }

const get = <T>(p: string) => req<T>("GET", p)
const enc = encodeURIComponent

export const kb = {
  search: (q: string, limit = 20) => get<{ hits: Hit[]; took_ms: number; semantic_down?: boolean }>("/kb/search" + qs({ q, limit })),
  sources: () => get<KbSource[]>("/kb/sources"),
  addFolder: (p: { name: string; path: string; include?: string[]; exclude?: string[]; visibility?: string; level?: Level }) => req<KbSource>("POST", "/kb/sources", { kind: "folder", ...p }),
  patchSource: (id: string, p: Partial<{ name: string; paused: boolean; visibility: string; level: Level; include: string[]; exclude: string[] }>) => req<KbSource>("PATCH", "/kb/sources/" + enc(id), p),
  deleteSource: (id: string) => req<{ ok: true }>("DELETE", "/kb/sources/" + enc(id)),
  sync: (id: string) => req<{ job_id: string }>("POST", `/kb/sources/${enc(id)}/sync`, {}),
  addFile: (file_id: string, title?: string) => req<Doc>("POST", "/kb/files", { file_id, title }),
  docs: (p: { source?: string; q?: string; limit?: number } = {}) => get<DocBrief[]>("/kb/docs" + qs({ limit: 200, ...p })),
  doc: (id: string) => get<Doc>("/kb/docs/" + enc(id)),
  patchDoc: (id: string, p: { visibility?: string | null; level?: Level | null; about?: ObjRef[] }) => req<Doc>("PATCH", "/kb/docs/" + enc(id), p),
  deleteDoc: (id: string) => req<{ ok: true }>("DELETE", "/kb/docs/" + enc(id)),
  newNote: (p: { title: string; body: string; visibility?: string; level?: Level }) => req<Doc>("POST", "/kb/notes", p),
  saveNote: (id: string, p: { title?: string; body?: string }) => req<Doc>("PUT", "/kb/notes/" + enc(id), p),
  mems: (p: { status?: string; kind?: string; q?: string } = {}) => get<Mem[]>("/kb/mems" + qs(p)),
  addMem: (p: { text: string; kind?: string; level?: Level; visibility?: string }) => req<Mem>("POST", "/kb/mems", p),
  /* single memories are edited through the contacts module's routes */
  patchMem: (id: string, p: Partial<{ text: string; level: Level; status: "confirmed" | "stale"; visibility: string }>) => req<Mem>("PATCH", "/mems/" + enc(id), p),
  deleteMem: (id: string) => req<{ ok: true }>("DELETE", "/mems/" + enc(id)),
  /** the same delete, sent so it survives the page going away (reload, tab closed): fire and forget */
  deleteMemOnLeave: (id: string) => { void fetch(hubUrl("/api/mems/" + enc(id)), { method: "DELETE", keepalive: true, credentials: "include", headers: wsHeaders() }).catch(() => {}) },
  candidates: () => get<Candidate[]>("/kb/candidates"),
  resolve: (ids: string[], choice: "yes" | "no") => req<{ ok: true; done: number }>("POST", "/kb/candidates/resolve", { ids, choice }),
  /** candidates that expired unanswered; `restore` puts one back for another 30 days */
  expired: () => get<ExpiredCandidate[]>("/kb/candidates/expired"),
  restore: (id: string) => req<{ ok: true }>("POST", `/kb/candidates/${enc(id)}/restore`),
  subjects: () => get<Subject[]>("/kb/subjects"),
  subject: (type: string, id: string) => get<SubjectDetail>(`/kb/subjects/${enc(type)}/${enc(id)}`),
  providers: () => get<{ memory: MemProvider[]; store: string }>("/kb/providers"),
  changes: (run?: string) => get<ChangeRun[]>("/kb/changes" + qs({ run })),
  /** which schedule is the knowledge steward (the page set it up) */
  steward: () => get<{ schedule_id: string | null }>("/kb/steward"),
  setSteward: (schedule_id: string | null) => req<{ schedule_id: string | null }>("PUT", "/kb/steward", { schedule_id }),
  undoChange: (id: string) => req<{ ok: true }>("POST", `/kb/changes/${enc(id)}/undo`),
  /** answers a flagged conflict: `keep` holds and the others go stale, `null` keeps them all */
  keepChange: (id: string, keep: string | null) => req<{ ok: true }>("POST", `/kb/changes/${enc(id)}/keep`, { keep }),
  uses: (consumer_type: "session" | "chat", consumer_id: string) => get<{ hits: Use[] }>("/kb/uses" + qs({ consumer_type, consumer_id })),
}

/* ── is the module there? asked once per page load; any other failure is asked again next time ── */
let probe: Promise<boolean> | null = null
export function kbAvailable(): Promise<boolean> {
  probe ??= kb.sources().then(() => true, (e) => { if (e instanceof NotWired) return false; probe = null; throw e })
  return probe
}

export const LEVELS: [Level, string][] = lazy([() => ["public", t("api:level.public")], () => ["conclusion", t("api:level.conclusion")], () => ["reasoning", t("api:level.reasoning")], () => ["never", t("api:level.never")]])
export const levelLabel = (l: string) => LEVELS.find((x) => x[0] === l)?.[1] ?? l
/** StatusDot colour per level: quotable green, conclusion blue, reasoning grey, never orange */
export const LEVEL_DOT: Record<string, string> = { public: "done", conclusion: "prop", reasoning: "idle", never: "wait" }
export const VIS: [string, string][] = lazy([() => ["private", t("api:vis.private")], () => ["workspace", t("api:vis.workspace")]])
export const visLabel = (v: string) => VIS.find((x) => x[0] === v)?.[1] ?? (v.startsWith("team:") ? t("api:vis.team") : v.startsWith("space:") ? t("api:vis.space") : v)
export const MEM_KINDS: [string, string][] = lazy([
  () => ["fact", t("api:memKind.fact")], () => ["preference", t("api:memKind.preference")], () => ["rule", t("api:memKind.rule")], () => ["event", t("api:memKind.event")], () => ["summary", t("api:memKind.summary")],
])
export const memKind = (k: string) => MEM_KINDS.find((x) => x[0] === k)?.[1] ?? k
/** the four kinds of record (ST-41): an item's record and the conversation record are kept where they are and
 *  searchable; only 长期记忆 (preferences, rules you said hold from now on) and 知识条目 (verified facts reusable
 *  elsewhere) are distilled from them, and both wait for your confirmation */
export const LONG_TERM = ["preference", "rule"]
export const recordOf = (kind: string) => (LONG_TERM.includes(kind) ? t("api:record.longTerm") : t("api:record.knowledge"))
export const SOURCE_KIND: Record<string, string> = lazy({
  notes: () => t("api:sourceKind.notes"), files: () => t("api:sourceKind.files"), folder: () => t("api:sourceKind.folder"), items: () => t("api:sourceKind.items"),
  chat: () => t("api:sourceKind.chat"), mems: () => t("api:sourceKind.mems"), bot_mems: () => t("api:sourceKind.botMems"),
})
