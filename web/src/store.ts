import { debounceMax, bounded } from "./live-utils"
import { readEventPages } from "./event-pages"
/* View model over the Hub API. Keeps the prototype's shapes (st.items, st.S, st.bots …) so the views stay as designed;
 * everything here is filled from /api/state, item details and session events are loaded on demand, and every action
 * calls the Hub — the SSE stream then refreshes what changed. */
import { useEffect, useRef, useSyncExternalStore } from "react"
import { toast as sonner } from "sonner"
import { promptNeedle, withDecisionNotes } from "./decision-notes"
import { blockSlots, type FeedRow } from "./feed-order"
import { api, cursorOf, setPageWorkspace, stream, Unauthenticated, type State, type RItem, type RItemDetail, type RSession, type RDecision, type RBot, type REvent, type RProject, type Proposal, type LocalSession, type HubEvent, type RNotice } from "./api"
import { cue } from "./bus"
import { newClientId } from "./composer-draft"
import { elapsed, pastTime, span } from "./i18n/format"
import i18n, { t } from "./i18n"
import type { StateUpdate, RFile, PlanStep, RHold, RQueued, RKindReview, SuggestChoice, A2uiRecord, A2uiSubmit, RAnnotation } from "./api"
import type { Cite } from "./api/knowledge"
import { yoloApi, type RYolo, type RYoloAction } from "./api/yolo"

/* ───────── view types (from the prototype) ───────── */
export type Ev = any[] // ['u',text,label?] ['q',string[]] ['th',text,secs] ['tool',kind,title,meta,detail,live?,add?,del?,failed?,tool?,desc?,input?,plan?] ['a',text] ['ho',text,toSid] ['hi',text] ['ask',text] ['sys',text] ['end',stopReason]
export type Sess = { id: string; n: number; item: string | null; b: string | null; h?: string; harness?: string; nativeId?: string | null; cwd?: string; m?: string; reasoningEffort?: string | null; contextKnown?: boolean; st: "run" | "wait" | "idle" | "ended"; title: string; dur: string; ctx: number; src: string; up: string; ev: Ev[]; seqs: number[]; ts: number[]; from?: string | null; fork?: { s: string; at: number }; queue: RQueued[]; ro?: boolean; device: string; loaded: boolean; busy: boolean }
export type Wait = { type: "approve" | "ask" | "accept" | "propose" | "fail" | "confirm"; subjectId?: string; q: string; draft?: string; opts?: string[]; since: string; decision: string; subject: string; session: string | null; options: string[]; by: string; busy?: boolean }
export type Done = { id: string; type: Wait["type"]; q: string; choice: string; tm: string; decision: string; canUndo: boolean; undone?: boolean; how: "chose" | "dismissed" | "expired"; rel?: Rel
  /** answered by YOLO: its reason (modules/yolo.md §3.3 `decided_by: "yolo"`) */
  yolo?: string }
/** what became of an approved release (hub decision.result.outcome): the card says this, never a bare "approved" */
export type Rel = { tone: "run" | "fail" | "ok"; text: string; /** the Hub's outcome, for deciding (text is a translated label) */ outcome?: "releasing" | "not_released" | "failed" | "released"; /** the release decision it comes from */ key?: string }
/** an earlier plan: replaced (its open steps abandoned) or done (a new round followed) */
/** an output the bot delivered: kind, title, location, artifact id, when it was delivered, and whether the bot said it is
 * a record of the work (`record`) or the result (`result`); unsaid, its title decides */
export type Out = [string, string, string | null, string, number?, ("record" | "result")?]
export type PlanRound = { n: number; steps: [string, number][]; end: "done" | "replaced"; at: string }
export type Item = {
  id: string; t: string; b: string; st: "prop" | "run" | "wait" | "pause" | "done" | "drop" | "block"; deps?: string[]; src: [string, string]; ws: string; project: string | null; up: string; star?: boolean; acc: string
  goals: string[]; now: string; plan?: [string, number][]; rounds: PlanRound[]; wait?: Wait; waits: Wait[]; ss: string[]; tl: any[]; outs: Out[]; anns: RAnnotation[]; q: RQueued[]; qs: string | null; parent?: { item: string; s?: string; at?: number } | null
  due?: string; pri?: "高" | "中" | "低"; arch?: boolean; snooze?: string; snoozeUntil?: number; tags?: string[]; branch: string | null; cwd: string | null; projNext?: boolean; detail: boolean; dependents: string[]; created: number; updated: number
  /** the last approved release of this item and what became of it */
  rel?: Rel
  workflow?: RItemDetail["workflow"]
}
export type Tag = { id: string; name: string; cat: string; desc: string; by: "Steve" | "你"; created: string }
export type Bot = { id: string; main?: boolean; n: string; a: string; role: string; pers: string; pro: number; h: string; harness: string; m: string; model: string | null; on: boolean; acc: string; ws: string; project: string | null; perm: string; permMode: string; rule: string; prompt: string | null
  reasoningEffort?: string | null; mem: [string, string, string][]; memCount: number; memLoaded: boolean; routines: { t: string; d: string; m: string; on: boolean; last: string }[]; hue: string; color: string; round: number; skin: string | null; dev?: string; workspaces: string[] }
export type Draft = { mid: string; thought: string; text: string }
export type Msg = { id?: string; clientId?: string; k: "me" | "steve" | "thinking" | "digest"; t?: string; item?: string; fresh?: boolean; chat?: string; files?: RFile[]; cfgs?: any[]; created?: number; steps?: Steps; notice?: { kind: string; level: string; ref?: string }; cites?: Cite[]
  /* thread routing (§3.4, §5.2): the offer under an answer, a suggestion card, ask options, a user message waiting in the
   * queue, the message this one answers or quotes, flow-back from an item, a research cut off by a restart */
  offer?: Offer; suggest?: Suggest; ask?: { options: string[] }; queued?: { run?: string | null }; replyTo?: string; origin?: string; answer?: string
  /* said while Steve was on a turn: waits in the queue above the composer (editable, withdrawable) until the turn ends;
   * `stopped`: a reply the user stopped */
  held?: boolean; stopped?: boolean
  update?: Update; updates?: Update[]; rerun?: string
  /** an item's one living card (§5.2): on its announcement, on the acknowledgement of your latest message to it, or on its own
   * message at the bottom when it needs you and the card was far up (`cardOnly`) */
  card?: ItemCardState; cardOnly?: boolean; wasCard?: boolean
  /** A2UI (ST-46): on a reply, what its interfaces submitted, by `block:surface`; on the user message an action made, that action */
  a2ui?: Record<string, A2uiRecord>; a2uiAction?: A2uiRecord & { ref: string } }
/** what an item made from the conversation has come to, kept on its card (hub `meta.card`) */
export type ItemCardState = { item: string; state: string; at?: number; text?: string; decision?: string
  delivery?: { text: string; full?: boolean; decision?: string; at?: number }; reply?: { text: string; ref?: string; at?: number }; released?: { version?: string; at?: number }
  /** a Release build that failed after the item was done */
  build?: { text: string; at?: number } }
/** a card as the hub kept it; older hubs filed a failed build's sentence as the released version: it is the build line */
function itemCard(c: ItemCardState): ItemCardState {
  const v = c.released?.version
  if (!v || !/\s/.test(v)) return c
  const { released: _, ...rest } = c
  return { ...rest, build: c.build ?? { text: v, at: c.released?.at } }
}
export type Offer = { title: string; goal: string; project?: string | null; bot?: string; state: "open" | "taken" | "dismissed"; item?: string; folded?: boolean }
export type { SuggestChoice }
export type Suggest = { question: string; choices: { id: SuggestChoice; label: string; item?: string; title?: string; recommended?: boolean }[]; state: "open" | "expired" | "chosen"; chosen?: string; item?: string }
/** a key moment of an item made from the conversation, posted back to it (§5.2) */
export type Update = { kind: "ask" | "reply" | "deliver" | "done" | "drop" | "fail"; item: string; text?: string; decision?: string; full?: boolean; ref?: string }
/** what Steve did for one message (hub meta.steps): routing, the reasoning behind a direct answer, a research run, tools; times in ms */
export type Steps = { route?: { kind: string; modelKind?: string; guard?: string | null; confidence?: number; title: string; bot: string; project: string | null; tags: string[]; newTags: string[]; why: string; model: string | null; fallback: boolean; ms: number; picked?: string }; answer?: { thought: string; model: string | null; ms: number; failed: boolean }
  research?: { run: string; model: string | null; ms: number; turns: number; tools: number; stopped?: "user" | "budget" | "violation" | "restart" | "failed" }
  tools: { name: string; args: Record<string, any>; result?: Record<string, any>; status: "ok" | "failed"; error?: string; ms: number }[]; ms?: number }
/** a research run being read live (steve.tool), for the user message `mid` */
export type LiveRun = { mid: string; run: string; t0: number; tools: { name: string; brief: string; ok: boolean }[] }
/** an open decision with no item (a forked or imported session, or its item was deleted): shown with the same card */
export type Orphan = { i: Item; w: Wait }
export type Project = RProject

/* label tables read when they are used (getters), so a language switch shows at once; the keys are the Hub's values */
function live<K extends string, V>(keys: readonly K[], f: (k: K) => V): Record<string, V> {
  const x: Record<string, V> = {}
  for (const k of keys) Object.defineProperty(x, k, { get: () => f(k), enumerable: true })
  return x
}
/** a list whose entries are read when they are used; `fixed` are the plain entries before and after the label at `at` */
function liveList<T extends unknown[]>(fixed: unknown[], at: number, f: () => string): T {
  const x: unknown[] = []
  fixed.forEach((v, n) => { if (n !== at) x[n] = v })
  Object.defineProperty(x, at, { get: f, enumerable: true })
  return x as T
}
/** a bot's proactive level 0, 1, 2 as words; the index is what is stored */
export const PRO: string[] = []
;(["rare", "moderate", "active"] as const).forEach((k, n) => Object.defineProperty(PRO, n, { get: () => t(`core:proactive.${k}`), enumerable: true }))
type Tone = "accent" | "orange" | "green" | "neutral" | "red"
const ST_TONE = { prop: "accent", block: "neutral", run: "accent", wait: "orange", pause: "neutral", done: "green", drop: "neutral" } as const
export const ST: Record<string, [string, Tone]> = live(Object.keys(ST_TONE) as (keyof typeof ST_TONE)[], (k) => [t(`core:itemStatus.${k}`), ST_TONE[k]])
const SS_TONE = { run: "accent", wait: "orange", idle: "neutral", ended: "neutral" } as const
export const SS: Record<string, [string, "accent" | "orange" | "green" | "neutral"]> = live(Object.keys(SS_TONE) as (keyof typeof SS_TONE)[], (k) => [t(`core:sessStatus.${k}`), SS_TONE[k]])
const WAIT_TYPES = ["approve", "ask", "accept", "propose", "fail", "confirm"] as const
export const WT: Record<string, string> = live(WAIT_TYPES, (k) => t(`core:waitType.${k}`))
/** what you do about a card of this type: the button that takes you to it */
export const WACT: Record<string, string> = live(WAIT_TYPES, (k) => t(`core:waitAction.${k}`))
/** what a card asks of you, in a line: `审批：推送到 main` */
export const waitLine = (w: Wait) => t("core:wait.line", { kind: w.subject === "tool_call" ? t("core:wait.tool") : w.subject === "release" ? t("core:wait.release") : WT[w.type] ?? t("core:wait.other"), q: w.q })
export const HARNESS: Record<string, string> = { "claude-code": "Claude Code", codex: "Codex" }
/** acceptance modes by their Hub value; the view model carries the label (Item.acc, Bot.acc), accKey turns it back */
export const ACC: Record<string, string> = live(["inherit", "manual", "auto"] as const, (k) => t(`core:acc.${k}`))
export const PERM: Record<string, string> = live(["plan", "default", "acceptEdits", "auto"] as const, (k) => t(`core:perm.${k}`))
/* a label in the current language, or the Hub value itself */
export const accKey = (label: string) => Object.keys(ACC).find((k) => ACC[k] === label) ?? (label in ACC ? label : "inherit")

/* ───────── time labels ───────── */
export const tm = (ts: number | null | undefined) => pastTime(ts)
const since = (ts: number) => span(Date.now() - ts)
const dur = (a: number, b: number) => elapsed(b - a)

/** the web build this page was loaded from: ops/release-web.sh writes it into index.html; "dev" under vite */
export const LOADED_WEB_VERSION = document.querySelector('meta[name="steve-web-version"]')?.getAttribute("content") || "dev"

/* ───────── the store ───────── */
export const st = {
  ready: false, error: "" as string, login: null as string | null, version: "", webVersion: "", update: null as StateUpdate | null,
  workspace: null as State["workspace"] | null, workspaces: [] as State["workspaces"],
  bots: {} as Record<string, Bot>, allBots: [] as State["allBots"], S: {} as Record<string, Sess>, items: [] as Item[], projects: [] as Project[],
  thread: [] as Msg[], queue: [] as Msg[], done: [] as Done[], tags: [] as Tag[], tagUses: {} as Record<string, number>,
  cfg: {} as Record<string, any>, settings: {} as Record<string, string>, devices: [] as State["devices"], runtimes: [] as State["runtimes"],
  /* YOLO (Rust Hub): the switch, and the current window's ledger by decision id; null where the Hub has none */
  yolo: null as RYolo | null, yoloActs: {} as Record<string, RYoloAction>,
  localSessions: [] as LocalSession[], hold: null as RHold | null, thinking: false, onboard: -1, prefix: "ST", noSeq: 0,
  notices: [] as RNotice[], noticeSeen: 0, phase: "" as string, orphans: [] as Orphan[],
  /* the chat answer being written (steve.draft), until the stored reply replaces it; `drafted` is that reply's id */
  draft: null as Draft | null, drafted: "",
  /* the research being read right now (steve.tool), the message being quoted, the item the conversation is talking to */
  live: null as LiveRun | null, quote: null as string | null, focus: null as { item: string; until: number } | null,
  /* modules that are not wired to a backend yet: always empty, never sample data */
  SIG: [] as any[], rules: [] as any[], schedules: [] as any[], sources: [] as any[], plugins: [] as any[],
  ch: { personas: [] as any[], spaces: [] as any[], accounts: [] as any[], providers: [] as any[], notify: { acc: "" }, disclosureDefaults: [] as any[] },
  pp: { contacts: [] as any[], orgs: [] as any[], teams: [] as any[], vault: [] as any[], itemPeople: [] as any[], accounts: [] as any[] },
  pendingActions: new Set<string>(),
  ver: 0,
}
const raw = { state: null as State | null, details: new Map<string, RItemDetail>(), events: new Map<string, REvent[]>(), loadingEv: new Set<string>(), loadingItem: new Set<string>() }
;(window as any).__steve = st

const ls = new Set<() => void>()
export function emit() { st.ver++; ls.forEach((f) => f()) }
export function useStore() { return useSyncExternalStore((cb) => { ls.add(cb); return () => ls.delete(cb) }, () => st.ver) }
export function toast(t: string, opts?: Parameters<typeof sonner>[1]) { sonner(t, opts) }
/** Tell the instance manager once per version (per browser) that a newer Steve can be installed from Settings · 关于. */
export function announceUpdate(update: StateUpdate | null) {
  if (!update?.owner) return
  const key = "steve-update-announced"
  try { if (localStorage.getItem(key) === update.version) return; localStorage.setItem(key, update.version) } catch { return }
  toast(t("core:toast.updateAvailable", { version: update.version }), { action: { label: t("core:toast.updateView"), onClick: () => { location.hash = "#/settings/about" } } })
}
const fail = (e: unknown) => { if (e instanceof Unauthenticated) { st.login = e.login; emit(); return } toast((e as Error).message || t("core:error.generic")) }
/** run an API call; refresh afterwards; report errors as a toast */
async function call<T>(p: Promise<T>, ok?: string): Promise<T | undefined> {
  try { const r = await p; if (ok) toast(ok); await refresh(); return r } catch (e) { fail(e); return undefined }
}
/** Once the Hub accepts a message, a subsequent refresh failure must not invite a duplicate send. */
async function sendMessage(p: Promise<unknown>, ok?: string): Promise<boolean> {
  try { await p } catch (e) { fail(e); return false }
  if (ok) toast(ok)
  await refresh().catch(fail)
  return true
}

/* ───────── mapping ───────── */
function botOf(b: RBot): Bot {
  const prev = st.bots[b.id], proj = raw.state?.projects.find((p) => p.id === b.default_project)
  return { id: b.id, main: b.main, n: b.name, a: b.name[0] ?? "B", role: b.role, pers: b.persona, pro: b.proactive, h: HARNESS[b.harness] ?? b.harness, harness: b.harness, m: b.model ?? t("core:defaultModel"), model: b.model, reasoningEffort: b.reasoning_effort,
    on: b.enabled, acc: ACC[b.accept_mode] ?? ACC.inherit, ws: proj?.name ?? t("core:projectByItem"), project: b.default_project, perm: PERM[b.perm_mode] ?? b.perm_mode, permMode: b.perm_mode, rule: b.rules, prompt: b.prompt,
    mem: prev?.mem ?? [], memLoaded: prev?.memLoaded ?? false, memCount: b.memories, routines: [], hue: b.avatar.color ?? "#8e8e93", color: b.avatar.color ?? defaultColor(b.name), round: b.avatar.round ?? 60, skin: b.avatar.skin ?? null, dev: b.device ?? undefined, workspaces: b.workspaces }
}
const AVATAR_HUES = [["#7aa7c7", "mist"], ["#8c7ae6", "lavender"], ["#5fb39a", "teal"], ["#e0a458", "amber"], ["#d9777f", "coral"], ["#6c7a89", "slate"], ["#3d4a5c", "navy"], ["#b08968", "walnut"]] as const
/** [color, its name] */
export const AVATAR_COLORS: [string, string][] = AVATAR_HUES.map(([c, k]) => liveList<[string, string]>([c, ""], 1, () => t(`core:avatarColor.${k}`)))
const hash = (s: string) => { let h = 2166136261; for (const ch of s) { h ^= ch.codePointAt(0)!; h = Math.imul(h, 16777619) } return h >>> 0 }
export const defaultColor = (name: string) => AVATAR_COLORS[hash(name) % AVATAR_COLORS.length][0]

/** one line for a prompt Steve hands an agent (the task brief runs to pages: role, rules, retrieved knowledge) */
export function briefHead(text: string): string {
  const title = /^You are working for Steve\b[^\n]*?“(.+?)”/.exec(text)?.[1]
  if (title) return t("core:brief.titled", { title })
  const line = text.split("\n").find((l) => l.trim())?.trim() ?? ""
  return !line ? t("core:brief.plain") : line.length > 120 ? line.slice(0, 120) + "…" : line
}
const STEVE_TOOL: Record<string, string> = live(["steve_update", "steve_ask", "steve_request_approval", "steve_deliver", "steve_publish", "steve_lane_status"] as const, (k) => t(`core:steveTool.${k}`))
const KIND: Record<string, string> = { read: "read", edit: "edit", search: "search", exec: "run", fetch: "search", think: "read", delete: "edit", move: "edit" }
const unfence = (t: string) => t.replace(/^```[a-z]*\n?/, "").replace(/\n?```\s*$/, "")
const diffText = (d: any) => [...String(d.old ?? "").split("\n").filter((l, _i, a) => a.length > 1 || l).map((l) => "- " + l), ...String(d.new ?? "").split("\n").map((l) => "+ " + l)].join("\n")
/** a tool's whole current view (transcript `tool`, the Node protocol's ToolView) → its transcript tuple, in place */
function toolEv(t: Ev, v: any) {
  const name = String(v.name ?? ""), short = name.replace(/^mcp__steve__/, ""), title = String(v.title ?? "")
  const steve = v.origin === "steve" || v.category === "steve", live = v.phase === "succeeded" || v.phase === "failed" ? 0 : 1
  const args = v.action?.type === "tool" && v.action.args && typeof v.action.args === "object" ? v.action.args : undefined
  const result: any[] = Array.isArray(v.result) ? v.result : [], diffs = result.filter((p) => p?.type === "diff")
  const text = result.filter((p) => p?.type === "text").map((p) => String(p.text ?? "")).join("\n")
  t.length = 0
  if (steve) t.push("tool", "steve", STEVE_TOOL[short] ?? short, "Steve", "", live)
  else t.push("tool", KIND[v.category] ?? "run", name || title.split(" ")[0] || i18n.t("core:tool.fallback"), v.target ?? title.split(" ").slice(1).join(" ").slice(0, 80), "", live)
  if (diffs.length) { t[4] = diffs.map(diffText).join("\n⋯\n"); t[6] = diffs.reduce((n, d) => n + (d.added ?? 0), 0); t[7] = diffs.reduce((n, d) => n + (d.removed ?? 0), 0); if (!steve && diffs[0].path) t[3] = diffs[0].path }
  else if (text) t[4] = t[1] === "run" ? unfence(text) : text
  if (!t[4] && v.error) t[4] = String(v.error)
  t[8] = v.phase === "failed" ? 1 : 0; t[9] = name
  const a = v.action
  if (a?.type === "exec") t[11] = a.command; else if (a?.type === "search") t[11] = a.query; else if (a?.type === "fetch") t[11] = a.url
  else if (t[1] === "run" && v.target) t[11] = v.target
  if (!steve && v.description) t[10] = v.description
  if (args) {
    if (typeof args.now === "string") t[10] = args.now
    if (Array.isArray(args.plan)) t[12] = args.plan.map((x: any) => [String(x.step ?? ""), !!x.done])
    if (typeof args.summary === "string") t[10] = args.summary.split("\n")[0].slice(0, 160)
    if (typeof args.question === "string") t[10] = args.question
  }
  return t
}
/** transcript entries (Steve's own kinds; the Hub reads old ACP rows into them) → transcript tuples; seqs[i] is the event sequence where tuple i starts (used for fork), ts[i] its time */
export function toEv(evs: REvent[], running: boolean): { ev: Ev[]; seqs: number[]; ts: number[] } {
  const ev: Ev[] = [], seqs: number[] = [], ts: number[] = [], tools = new Map<string, Ev>()
  let cur: REvent | undefined
  const push = (e: Ev, seq: number) => { ev.push(e); seqs.push(seq); ts.push(cur?.ts ?? 0) }
  for (const e of evs) {
    const d = e.data ?? {}, last = ev[ev.length - 1]; cur = e
    if (e.kind === "user") { if (d.via === "steve") push(["hi", briefHead(String(d.text ?? "")), d.files, String(d.text ?? "")], e.seq); else push(["u", d.text, d.via === "me" ? "" : d.via, d.files], e.seq); continue }
    if (e.kind === "text") {
      const t = String(d.text ?? ""), k = d.role === "thought" ? "th" : "a"
      if (last?.[0] === k) last[1] += t; else push(k === "th" ? ["th", t, ""] : ["a", t], e.seq)
      continue
    }
    if (e.kind === "tool") {
      const v = d.tool ?? {}, id = String(v.id ?? ""), had = tools.get(id)
      if (had) { toolEv(had, v); continue }
      const t = toolEv([], v); tools.set(id, t); push(t, e.seq); continue
    }
    if (e.kind === "system") { push(["sys", d.text], e.seq); continue }
    if (e.kind === "permission") { push(["ask", String(d.title ?? t("core:tool.permission")), d.decision], e.seq); continue }
    if (e.kind === "turn_end") { push(["end", d.stopReason ?? ""], e.seq); continue }
    if (e.kind === "queued") { push(["sys", t("core:queueLine.queued", { text: briefHead(String(d.text ?? "")) })], e.seq); continue }
    if (e.kind === "queued_edited") { push(["sys", t("core:queueLine.edited", { text: briefHead(String(d.text ?? "")) })], e.seq); continue }
    if (e.kind === "queued_removed") { push(["sys", t("core:queueLine.removed", { text: briefHead(String(d.text ?? "")) })], e.seq); continue }
    if (e.kind === "queued_sent_now") { push(["sys", t(d.interrupted ? "core:queueLine.sentNowInterrupted" : "core:queueLine.sentNow", { text: briefHead(String(d.text ?? "")) })], e.seq); continue }
  }
  if (!running) ev.forEach((x) => { if (x[0] === "tool") x[5] = 0 })
  return { ev, seqs, ts }
}
/* Full snapshots and targeted event paints share this cache: only a transcript whose events changed is mapped again */
const convs = new Map<string, { xs: REvent[]; n: number; running: boolean; conv: ReturnType<typeof toEv> }>()
function convOf(sid: string, xs: REvent[], running: boolean) {
  const c = convs.get(sid); if (c && c.xs === xs && c.n === xs.length && c.running === running) return c.conv
  const conv = toEv(xs, running); convs.set(sid, { xs, n: xs.length, running, conv }); return conv
}
function sessOf(s: RSession, openSessions: Set<string>): Sess {
  const evs = raw.events.get(s.id), running = s.status === "running" || s.status === "starting"
  const conv = evs ? convOf(s.id, evs, running) : { ev: [], seqs: [], ts: [] }
  return { id: s.id, n: s.no, item: s.item_id, b: s.bot_id, h: HARNESS[s.harness] ?? s.harness, harness: s.harness, nativeId: s.harness_session_id ?? null, cwd: s.cwd, m: s.model ?? t("core:defaultModel"), st: running ? "run" : s.status === "ended" ? "ended" : openSessions.has(s.id) ? "wait" : "idle",
    title: s.title, dur: dur(s.created, s.updated), reasoningEffort: s.reasoning_effort, contextKnown: s.used != null && s.size != null && s.size > 0, ctx: s.used && s.size ? Math.round((s.used / s.size) * 100) : 0, src: s.source, up: tm(s.updated), ev: conv.ev, seqs: conv.seqs, ts: conv.ts, queue: s.queued,
    ro: s.readonly, device: s.device_id, loaded: !!evs, busy: s.busy }
}
function waitOf(d: RDecision): Wait {
  const opts = d.kind === "ask" ? d.options : undefined
  return { type: d.kind, q: d.question, draft: d.draft, opts, since: since(d.created), decision: d.id, subject: d.subject_type, subjectId: d.subject_id, session: d.session_id, options: d.options, by: d.requested_by, busy: !!d.agent_busy }
}
/** Both pending and recent resolved decisions are returned by state, including orphan sessions. */
export function permissionDecision(id?: string) {
  if (!id) return undefined
  return raw.state?.decisions.find((d) => d.id === id) ?? raw.state?.recent.find((d) => d.id === id)
}
/** a stand-in item for a decision without one: id "" marks it; the title and bot come from its session */
function orphanOf(d: RDecision): Orphan {
  const s = raw.state?.sessions.find((x) => x.id === d.session_id), w = waitOf(d)
  const i = { id: "", t: s?.title || t("core:orphan.title"), b: s?.bot_id ?? d.requested_by, st: "wait", waits: [w], wait: w, ss: s ? [s.id] : [], tags: [], deps: [], tl: [], outs: [], anns: [], q: [], goals: [], rounds: [], now: "", src: [t("core:orphan.source"), "session"], ws: "—", project: null, up: "", acc: ACC.inherit, branch: null, cwd: null, detail: false, dependents: [], created: d.created, updated: d.created } as unknown as Item
  return { i, w }
}
/* what was chosen on a module card (signals, flows, vault, connections); a flow_step choice is its own label */
const MODULE_CHOICE_SUBJECTS = new Set(["signal", "flow_step", "flow_run", "secret_use", "connection", "mail_draft"])
const MODULE_CHOICE: Record<string, string> = live(["done", "to_item", "retry", "drop", "continue_once", "stop", "once", "grant_1h", "grant_8h", "grant_24h", "deny", "pause", "reauth", "send", "no"] as const, (k) => t(`core:moduleChoice.${k}`))
/** an approved release's outcome; undefined for anything else (and for approvals recorded before outcomes existed) */
export function relOf(d: RDecision): Rel | undefined {
  const r = d.result ?? {}
  if (d.kind !== "approve" || d.subject_type !== "release" || d.status !== "resolved" || !r.approved) return undefined
  switch (r.outcome) {
    case "releasing": return { tone: "run", outcome: "releasing", text: t("core:rel.releasing") }
    case "not_released": return { tone: "fail", outcome: "not_released", text: r.reason ? t("core:rel.notReleasedWhy", { why: r.reason }) : t("core:rel.notReleased") }
    case "failed": {
      const why = [r.reason, r.note].filter(Boolean).join(t("core:rel.sep")), hubOnly = r.hub_ok && !r.web_ok
      return { tone: "fail", outcome: "failed", text: hubOnly ? (why ? t("core:rel.hubOnlyWhy", { why }) : t("core:rel.hubOnly")) : why ? t("core:rel.failedWhy", { why }) : t("core:rel.failed") }
    }
    case "released": {
      const what = t(r.kind === "web" ? "core:rel.releasedWeb" : r.kind === "hub+web" ? "core:rel.releasedBoth" : "core:rel.released")
      return { tone: "ok", outcome: "released", text: r.version ? t("core:rel.withVersion", { what, version: r.version }) : what }
    }
  }
  return undefined
}
/** the item's newest release decision, if it is settled and approved */
function itemRel(id: string, ds: RDecision[]): Rel | undefined {
  const last = ds.filter((d) => d.item_id === id && d.kind === "approve" && d.subject_type === "release").sort((a, b) => b.created - a.created)[0]
  const r = last ? relOf(last) : undefined
  return r && { ...r, key: last.id }
}
/* ── alerts that are over ──
 * a failure asks for attention only while it is current: a later release took its place (only the newest counts), the item
 * went on or finished, or you closed it. Closed ones are remembered by the Hub with the closed notifications (setting
 * noticeDismissed: notification ids and decision ids), so a refresh or another device does not bring them back.
 * The failure itself stays in the item's timeline and in the notifications. */
const finished = (s?: string) => s === "done" || s === "drop"
export function alertClosed(key?: string) {
  const xs = raw.state?.settings.noticeDismissed as unknown
  return !!key && Array.isArray(xs) && xs.includes(key)
}
/** the failed release still worth showing on the item, or undefined */
function liveRel(i: RItem, r?: Rel): Rel | undefined {
  if (r?.tone !== "fail") return r
  if (finished(i.status) || alertClosed(r.key)) return undefined
  /* closed from the notifications: the item's newest release notice (they come newest first) */
  const n = raw.state?.notifications?.find((x) => x.item_id === i.id && x.kind === "release")
  return n?.level === "fail" && n.closed ? undefined : r
}
/** close alerts by hand: decision ids, notification ids or their lines in the conversation */
export function closeAlerts(keys: string[]) {
  const s = raw.state, xs = keys.filter(Boolean); if (!s || !xs.length) return
  const was = Array.isArray(s.settings.noticeDismissed as unknown) ? (s.settings.noticeDismissed as unknown as string[]) : []
  s.settings = { ...s.settings, noticeDismissed: [...was, ...xs] as unknown as string }
  s.thread = s.thread.filter((m) => !xs.includes(m.meta?.notice?.ref) && !xs.includes(m.id))
  s.notifications = s.notifications?.map((n) => (xs.includes(n.id) ? { ...n, closed: true } : n))
  build(); emit()
  void call(Promise.all(xs.map((x) => api.dismissNotice(x))))
}
/** close an item's failed release: the decision, and its notifications still open */
export function closeRel(i: Item) {
  closeAlerts([i.rel?.key ?? "", ...st.notices.filter((n) => n.item_id === i.id && n.kind === "release" && n.level === "fail" && !n.closed).map((n) => n.id)])
}
function choiceLabel(d: RDecision) {
  const r = d.result ?? {}
  if (r.dismissed) return t("core:choice.dismissed")
  if (d.status === "expired") return String(r.superseded ?? (r.dropped ? t("core:choice.itemDropped") : r.reassigned ? t("core:choice.reassigned") : t("core:choice.expired")))
  if (d.kind === "accept") return r.choice === "pass" ? t("core:choice.pass") : t("core:choice.rework", { text: r.text ?? "" })
  if (d.subject_type === "space") return (r.choice ?? r.answer) === "mute" ? t("core:choice.mute") : t("core:choice.setup")
  if (d.kind === "ask") return String(r.answer ?? "")
  if (d.subject_type === "party_merge") return r.choice === "merge" ? t("core:choice.merge") : t("core:choice.notSame")
  if (d.kind === "confirm") return r.choice === "yes" ? t("core:choice.confirmYes") : t("core:choice.confirmNo")
  if (d.kind === "propose") return r.choice === "yes" ? t("core:choice.proposeYes") : t("core:choice.proposeNo")
  if (MODULE_CHOICE_SUBJECTS.has(d.subject_type)) return MODULE_CHOICE[r.choice] ?? String(r.choice ?? t("core:choice.handled"))
  if (d.kind === "fail") return FAIL_CHOICE[r.choice] ?? r.choice
  if (d.subject_type === "tool_call") return r.optionId ? t("core:choice.allow") : t("core:choice.deny")
  if (d.subject_type === "release") return r.approved ? t("core:choice.release") : t("core:choice.noRelease")
  return r.approved ? t("core:choice.approve") : t("core:choice.deny")
}
/** the choices on a card about something that went wrong, by the Hub's choice id */
export const FAIL_CHOICE: Record<string, string> = live(["continue", "delivered", "look", "tosteve", "drop", "release_waive", "release_retry", "release_live"] as const, (k) => t(`core:fail.${k}`))
/* ── which agent turn answered a comment in the work log ──
 * a comment (timeline kind "me") becomes a user prompt in one of the item's sessions: "补充信息：…", "用户的回答：…", a merged queue…
 * the first unclaimed prompt sent at or after the comment that contains its text is the one; the reply is that turn */
export type Reply = { s: string; from: number; to: number; text: string; tools: number; live: boolean; /** when the turn ended; Infinity while it runs */ end: number }
function repliesOf(timeline: RItemDetail["timeline"], sids: string[]): Map<number, Reply> {
  const out = new Map<number, Reply>()
  const prompts = sids.flatMap((sid) => (raw.events.get(sid) ?? []).filter((e) => e.kind === "user").map((e) => ({ sid, e })))
    .sort((a, b) => a.e.ts - b.e.ts || a.e.seq - b.e.seq)
  if (!prompts.length) return out
  const used = new Set<number>()
  for (const t of timeline) {
    if (t.kind !== "me") continue
    const needle = promptNeedle(t.text); if (!needle) continue
    const p = prompts.find(({ e }) => !used.has(e.seq) && e.ts >= t.ts - 1000 && String(e.data?.text ?? "").includes(needle))
    if (!p) continue
    used.add(p.e.seq)
    const evs = raw.events.get(p.sid)!, S = st.S[p.sid]
    /* prompts merged into one turn are consecutive user events: the reply starts after the last of them */
    let k = evs.findIndex((e) => e.seq === p.e.seq)
    while (evs[k + 1]?.kind === "user") k++
    let end = k + 1
    while (end < evs.length && evs[end].kind !== "user") end++
    const turn = evs.slice(k + 1, end)
    const text = turn.filter((e) => e.kind === "text" && e.data?.role === "assistant").map((e) => e.data?.text ?? "").join("").trim()
    const tools = new Set(turn.filter((e) => e.kind === "tool").map((e) => e.data?.tool?.id)).size
    const live = !!S && S.st === "run" && end === evs.length && !turn.some((e) => e.kind === "turn_end")
    /* the transcript tuples of this turn, for the full view */
    const firstSeq = evs[k + 1]?.seq ?? Infinity, endSeq = evs[end]?.seq ?? Infinity
    const at = (seq: number) => { const n = S ? S.seqs.findIndex((q) => q >= seq) : -1; return n < 0 ? (S?.seqs.length ?? 0) : n }
    out.set(t.id, { s: p.sid, from: at(firstSeq), to: at(endSeq), text, tools, live, end: live ? Infinity : (turn.at(-1)?.ts ?? p.e.ts) })
  }
  return out
}
/* a plan step as [text, state]: 0 open, 1 done, 2 left open when the item finished */
const steps = (p: PlanStep[]) => p.map((x) => [x.step, x.done ? 1 : x.left ? 2 : 0] as [string, number])
let sessionIndex = new Map<string, RSession>(), itemIndex = new Map<string, RItem>(), itemPosition = new Map<string, number>()
let projectIndex = new Map<string, RProject>(), decisionIndex = new Map<string, RDecision[]>(), activeIndex = new Map<string, RSession>(), openSessionIds = new Set<string>()
function itemOf(i: RItem): Item {
  const det = raw.details.get(i.id), proj = projectIndex.get(i.project_id ?? "")
  const open = decisionIndex.get(i.id) ?? []
  const snoozed = i.snooze && i.snooze.until > Date.now() ? i.snooze.label : undefined
  const act = activeIndex.get(i.id)
  const lines = det ? withDecisionNotes(det.timeline, det.decisions) : []
  const replies = det ? repliesOf(lines, i.sessionIds) : new Map<number, Reply>()
  /* a session and a comment's reply show below the lines written while they ran (ST-42) */
  const started = (t: (typeof lines)[number]) => t.kind === "session" && t.session_id && t.text.startsWith("开始会话") ? t.session_id : null
  const rows = lines.map((t): FeedRow => {
    const sid = started(t), r = t.kind === "me" ? replies.get(t.id) : undefined
    if (sid) { const S = st.S[sid]; return { ts: t.ts, until: S?.st === "run" || S?.st === "wait" ? Infinity : sessionIndex.get(sid)?.updated ?? t.ts } }
    return { ts: t.ts, until: r?.end, fence: t.kind === "me" }
  })
  const after = new Map(blockSlots(rows).flatMap((at, k) => at < 0 ? [] : [[at, k] as const])), tl: any[] = []
  lines.forEach((t, n) => {
    const sid = started(t)
    tl.push(sid ? ["sys", i18n.t("core:sessionStarted"), tm(t.ts)] : t.kind === "me" ? ["me", t.text, tm(t.ts), t.session_id ?? undefined, replies.get(t.id), t.files, t.via, t.id] : ["sys", t.text, tm(t.ts)])
    const k = after.get(n); if (k == null) return
    const sk = started(lines[k]); tl.push(sk ? ["sess", sk] : ["reply", replies.get(lines[k].id)])
  })
  return {
    id: i.id, t: i.title, b: i.bot_id ?? "steve", st: i.status, deps: i.deps, src: i.source, ws: proj?.name ?? "—", project: i.project_id, up: tm(i.updated), star: i.star, acc: ACC[i.accept_mode ?? "inherit"] ?? ACC.inherit,
    goals: det ? det.goalsList.map((g) => g.text) : [i.goal], now: i.now || "", plan: i.plan.length ? steps(i.plan) : undefined,
    rounds: (det?.planRounds ?? []).map((r) => ({ n: r.round, steps: steps(r.steps), end: r.end, at: tm(r.created) })),
    workflow: det?.workflow,
    wait: open.length ? waitOf(open[0]) : undefined, waits: open.map(waitOf), ss: i.sessionIds, tl, outs: det ? det.artifacts.map((a): Out => [a.kind, a.title, a.location, a.id, a.created, det.artifactRoles?.[a.id] as Out[5]]) : [], anns: det?.annotations ?? [],
    q: act?.queued ?? [], qs: act?.id ?? null,
    parent: i.parent?.item ? { item: i.parent.item, s: i.parent.session ?? undefined, at: undefined } : null, due: i.due ?? undefined, pri: (i.priority as Item["pri"]) ?? "中", arch: i.archived, snooze: snoozed, snoozeUntil: snoozed ? i.snooze!.until : undefined, tags: i.tags,
    /* the hub clears cwd when the project changes; a session still running then is in the old directory */
    branch: i.branch, cwd: i.cwd, projNext: !i.cwd && !!act, detail: !!det, dependents: det?.dependents ?? [], created: i.created, updated: i.updated,
    /* an open release card wins (it is the newest); a settled one says what happened */
    rel: open.some((d) => d.subject_type === "release") ? undefined : liveRel(i, itemRel(i.id, [...(det?.decisions ?? []), ...(raw.state?.recent ?? [])])),
  }
}
/** every message of the conversation this page has seen, older ones included (thread/history.ts loads those) */
export const knownMsgs = new Map<string, Msg>()
export function msgOf(m: State["thread"][number]): Msg {
  const x = m.meta ?? {}
  /* one drafted change (`proposal`) or several (`proposals`); `cfg_state` is what became of each */
  const ps: Proposal[] = Array.isArray(x.proposals) ? x.proposals : x.proposal ? [x.proposal] : []
  const cfgs = ps.map((p) => proposalOf(p, x.cfg_state?.[p.id] ?? (x.applied ? "applied" : x.dropped ? "dropped" : "draft")))
  return { id: m.id, k: m.author, t: m.text, item: m.item_id ?? undefined, chat: x.chat, files: x.files, created: m.created, steps: x.steps, notice: x.notice, cites: x.cites, cfgs: cfgs.length ? cfgs : undefined,
    held: !!x.held || undefined, stopped: !!x.stopped || undefined,
    offer: x.offer ?? undefined, suggest: x.suggest ?? undefined, ask: x.ask ?? undefined, queued: x.queued ?? undefined, replyTo: x.reply_to ?? undefined, origin: x.origin ?? undefined, answer: x.answer ?? undefined,
    update: x.update ?? undefined, updates: Array.isArray(x.updates) ? x.updates : undefined, rerun: x.rerun ?? undefined,
    card: x.card?.item ? itemCard(x.card) : undefined, cardOnly: !!x.card_only, wasCard: !!x.was_card,
    a2ui: x.a2ui && typeof x.a2ui === "object" ? x.a2ui : undefined, a2uiAction: x.a2ui_action?.name ? x.a2ui_action : undefined }
}
const proposals = new Map<string, any>()
/** a drafted change as the prototype's Proposal: apply/undo/drop call the Hub. The Hub keeps what became of a
 * conversation card (`state`); until it says so, what this page just did stands. */
export function proposalOf(p: Proposal, state: "draft" | "applied" | "dropped" = "draft") {
  const cur = proposals.get(p.id)
  if (cur) { if (!cur.touched || state === cur.state) { cur.state = state; cur.touched = false } return cur }
  const x: any = { title: p.title, target: p.title, href: p.href, changes: p.changes, state, id: p.id, touched: false }
  const set = (s: typeof state) => { x.state = s; x.touched = true; emit() }
  /* a field changed again since this change keeps its value: the Hub names it (`kept`) */
  x.undo = async () => {
    const r = await call(api.undoConfig(p.id) as Promise<{ kept?: string[] }>); if (!r) return
    toast(r.kept?.length ? t("core:toast.undoneKept", { fields: r.kept.join(t("core:toast.fieldSep")) }) : t("core:toast.undone")); set("draft")
  }
  x.apply = async () => { const r = await call(api.apply(p.id)); if (r) { set("applied"); toast(t("core:toast.applied"), { action: { label: t("core:toast.undo"), onClick: () => void x.undo() } }) } }
  x.drop = async () => { if (await call(api.dropConfig(p.id))) set("dropped") }
  proposals.set(p.id, x); return x
}

function build() {
  const s = raw.state; if (!s) return
  st.hold = s.hold ?? null
  st.version = s.version; st.webVersion = s.web_version ?? ""; st.update = s.update ?? null; announceUpdate(st.update); st.workspace = s.workspace; st.workspaces = s.workspaces; st.prefix = s.workspace.item_prefix; st.projects = s.projects
  st.noSeq = Math.max(0, ...s.items.map((i) => (i.id.startsWith(st.prefix + "-") ? i.no : 0)))
  const openDec = s.decisions, openSess = new Set(openDec.map((d) => d.session_id).filter(Boolean) as string[])
  sessionIndex = new Map(s.sessions.map((x) => [x.id, x])); itemIndex = new Map(s.items.map((x) => [x.id, x])); itemPosition = new Map(s.items.map((x, n) => [x.id, n]))
  projectIndex = new Map(s.projects.map((x) => [x.id, x])); decisionIndex = new Map(); activeIndex = new Map(); openSessionIds = openSess
  for (const d of openDec) { if (d.item_id) { const xs = decisionIndex.get(d.item_id) ?? []; xs.push(d); decisionIndex.set(d.item_id, xs) } }
  for (const x of s.sessions) if (x.item_id && x.status !== "ended") activeIndex.set(x.item_id, x)
  st.bots = Object.fromEntries(s.bots.map((b) => [b.id, botOf(b)])); st.allBots = s.allBots
  st.S = Object.fromEntries(s.sessions.map((x) => [x.id, sessOf(x, openSess)]))
  st.items = s.items.map(itemOf)
  st.tags = s.tags.map((t) => ({ id: t.id, name: t.name, cat: t.cat, desc: t.description, by: t.created_by === "你" ? "你" : "Steve", created: tm(t.created) }))
  st.tagUses = s.tagUses
  /* a failure line of a finished item is history: the notifications keep it */
  const msgs = s.thread.filter((m) => !(m.meta?.notice && m.item_id && finished(itemIndex.get(m.item_id)?.status))).map(msgOf)
  st.thread = msgs.filter((m) => !m.held); st.queue = [...msgs.filter((m) => m.held), ...sendingHeld]
  /* after a reload, whether Steve is still on a turn (afterwards the steve.thinking events say so) */
  if (!st.ready && s.thinking) st.thinking = true
  if (st.draft) {
    const at = s.thread.findIndex((m) => m.id === st.draft!.mid), reply = at < 0 ? undefined : s.thread.slice(at + 1).find((m) => m.author === "steve")
    if (reply) { st.drafted = reply.id; st.draft = null }
  }
  /* a research run's answer is stored: its live line goes, and the thinking line with it */
  if (st.live && s.thread.some((m) => m.author === "steve" && m.meta?.origin === st.live!.mid && m.meta?.steps?.research)) {
    st.live = null; if (st.phase === "research" || st.phase === "answer") { st.thinking = false; st.phase = "" }
  }
  if (st.thinking || st.draft || st.live) st.thread.push({ k: "thinking" })
  st.notices = (s.notifications ?? []).map((n) => (n.level === "fail" && !n.closed && n.item_id && finished(itemIndex.get(n.item_id)?.status) ? { ...n, closed: true } : n)); st.noticeSeen = Math.max(st.noticeSeen, s.notifySeen ?? 0)
  st.orphans = openDec.filter((d) => !d.item_id || !s.items.some((i) => i.id === d.item_id)).map(orphanOf)
  st.done = s.recent.filter((d) => d.item_id).map((d) => ({ id: d.item_id!, type: d.kind, q: d.question, choice: choiceLabel(d), tm: tm(d.resolved), decision: d.id, canUndo: !!d.undo && !!d.resolved && Date.now() - d.resolved < 86400_000, rel: relOf(d),
    how: d.result?.dismissed ? "dismissed" : d.status === "expired" ? "expired" : "chose", yolo: d.result?.decided_by === "yolo" ? String(d.result?.yolo_reason ?? "") : undefined }))
  st.settings = s.settings; st.devices = s.devices; st.runtimes = s.runtimes
  st.cfg = { ...s.settings, accDefault: ACC[s.settings.accDefault] ?? ACC.manual, harness: s.runtimes.map((r) => ({ id: r.id, n: r.name, ok: r.ok, ver: r.version, path: r.path, models: r.models, def: t("core:defaultModel"), login: r.note })) }
  if (st.onboard < 0 && !s.thread.length && !s.items.length && localStorage.getItem("steve-onboarded") !== "1") st.onboard = 0
  st.ready = true
}
/* the view model holds labels (status words, "默认模型", transcript lines): map it again in the new language */
i18n.on("languageChanged", () => { convs.clear(); build(); emit() })

/* ───────── loading and live updates ───────── */
let scopeEpoch = 0
/** the change-log watermark of the state on screen: a stream opened after it replays only what changed since */
let watermark: string | null = null
let inflight: Promise<void> | null = null, again = false
export function refresh(): Promise<void> {
  if (inflight) { again = true; return inflight.then(() => inflight ?? undefined) } // resolve after the follow-up, so callers see state newer than their action
  inflight = (async () => {
    /* asked after Steve stopped, the state has the reply (stored before steve.thinking went off): the draft has served */
    const settled = !st.thinking, epoch = scopeEpoch
    try { const state = await api.state(); if (epoch !== scopeEpoch) return; raw.state = state; watermark = cursorOf(state) ?? watermark; setPageWorkspace(state.workspace?.id ?? ""); st.login = null; st.error = ""; build(); if (settled && st.draft) { st.draft = null; build() } emit() }
    catch (e) { if (epoch !== scopeEpoch) return; if (e instanceof Unauthenticated) st.login = e.login ?? "/auth/login"; else st.error = (e as Error).message; emit() }
    finally { inflight = null; if (again) { again = false; void refresh() } }
  })()
  return inflight
}
/* debounced, but a steady event stream still refreshes at least once a second */
let timer: any = null, due = 0
const soon = () => { clearTimeout(timer); due ||= Date.now() + 1000; timer = setTimeout(() => { due = 0; void refresh().then(reloadOpen) }, Math.max(0, Math.min(120, due - Date.now()))) }
const replayFrom = new Map<string, number>()
const openItems = new Set<string>(), dirty = new Set<string>(), reloadAgain = new Set<string>(), missedEv = new Set<string>()
/** refetch only the loaded item details an event touched */
async function reloadOpen() { const epoch = scopeEpoch, ids = [...dirty]; ids.forEach((id) => dirty.delete(id)); await bounded(ids, async (id) => { if (epoch === scopeEpoch && (raw.details.has(id) || raw.loadingItem.has(id))) await loadItem(id, true) }) }
export async function loadItem(id: string, force = false) {
  openItems.add(id)
  if (raw.loadingItem.has(id)) { if (force) reloadAgain.add(id); return }
  if (!force && raw.details.has(id)) return
  const epoch = scopeEpoch
  raw.loadingItem.add(id)
  try { const detail = await api.item(id); if (epoch !== scopeEpoch) return; raw.details.set(id, detail); tries.delete("i:" + id); build(); emit() } catch (e) { if (epoch !== scopeEpoch) return; if (!(e instanceof Unauthenticated)) { raw.details.delete(id); retry("i:" + id, e, () => { if (epoch === scopeEpoch && st.items.some((x) => x.id === id)) void loadItem(id, true) }) } } finally { if (epoch === scopeEpoch) { raw.loadingItem.delete(id); if (reloadAgain.delete(id)) void loadItem(id, true) } }
}
/* a view asks once, when it mounts: a load that failed (Hub restarting for a release, dropped connection) used to spin
 * forever. Retry here, backing off to every 30 s, until it loads or the thing is gone */
const tries = new Map<string, number>()
function retry(key: string, e: unknown, f: () => unknown) {
  const n = (tries.get(key) ?? 0) + 1; tries.set(key, n)
  if (n === 3) toast(t("core:error.loadRetry", { msg: (e as Error).message || t("core:error.network") }))
  setTimeout(f, 1000 * Math.min(30, 2 ** n))
}
/** Pages are limited by bytes as well as rows; only an empty page means EOF. */
async function readEvents(sid: string, after: number, epoch: number) {
  await readEventPages(after, async (cursor) => {
    if (epoch !== scopeEpoch) return []
    return api.events(sid, cursor)
  }, (page) => {
    if (epoch !== scopeEpoch) return
    const xs = raw.events.get(sid) ?? []
    const seen = new Set(xs.map((e) => e.seq))
    for (const e of page) if (!seen.has(e.seq)) xs.push(e)
    xs.sort((a, b) => a.seq - b.seq); raw.events.set(sid, xs); updateSession(sid); emit()
  })
  if (epoch === scopeEpoch && !raw.events.has(sid)) { raw.events.set(sid, []); updateSession(sid); emit() }
}
export async function loadEvents(sid: string) {
  if (raw.events.has(sid) || raw.loadingEv.has(sid)) return
  await fetchEvents(sid)
}
async function fetchEvents(sid: string) {
  if (raw.loadingEv.has(sid)) { missedEv.add(sid); return }
  const epoch = scopeEpoch
  raw.loadingEv.add(sid)
  const after = replayFrom.get(sid) ?? raw.events.get(sid)?.at(-1)?.seq ?? 0
  replayFrom.delete(sid)
  try {
    await readEvents(sid, after, epoch)
    if (epoch === scopeEpoch) tries.delete("e:" + sid)
  } catch (e) {
    if (epoch !== scopeEpoch) return
    replayFrom.set(sid, Math.min(after, replayFrom.get(sid) ?? after)); missedEv.delete(sid)
    if (e instanceof Unauthenticated) fail(e)
    else retry("e:" + sid, e, () => { if (epoch === scopeEpoch && st.S[sid]) void fetchEvents(sid) })
  } finally {
    if (epoch === scopeEpoch) { raw.loadingEv.delete(sid); if (missedEv.delete(sid)) void catchUp(sid) }
  }
}
/** Loading pages own the cursor; SSE during a fetch is caught up after EOF. */
async function catchUp(sid: string) {
  if (raw.loadingEv.has(sid)) { missedEv.add(sid); return }
  if (raw.events.has(sid)) await fetchEvents(sid)
}
/* streamed chunks arrive dozens a second per running agent: re-render the app at most every 80 ms, not per chunk */
/* a chat answer streams in at up to ~20 updates a second: draw once per frame */
let drawing = 0
const drawDraft = () => { drawing ||= requestAnimationFrame(() => { drawing = 0; emit() }) }
function updateSession(sid: string) {
  const s = sessionIndex.get(sid); if (!s) return
  st.S[sid] = sessOf(s, openSessionIds)
  // The item's reply previews derive from this transcript too.
  const item = s.item_id && itemIndex.get(s.item_id), pos = s.item_id && itemPosition.get(s.item_id)
  if (item && typeof pos === "number" && raw.details.has(item.id)) st.items[pos] = itemOf(item)
}
let painting: ReturnType<typeof setTimeout> | null = null
const painted = new Set<string>()
const paint = (sid: string) => { painted.add(sid); painting ??= setTimeout(() => { painting = null; for (const id of painted) updateSession(id); painted.clear(); emit() }, 80) }
/* The module SSE types (steve-modules `sse::MODULE_TYPES`, plus `kb`): ids only, scoped to the workspace; the module
 * pages refetch what they show. They don't touch /api/state, so they never trigger a state refresh here. */
export const MODULE_EVENTS = ["party", "mm", "changeset", "account", "connection", "vault", "signal", "flow", "run", "space", "chan_message", "persona", "channel", "gh", "mail", "kb", "yolo", "yolo_action", "stats"] as const
export type ModuleEvent = { type: (typeof MODULE_EVENTS)[number] | "resync"; id: string; ws?: string; flow_id?: string; space_id?: string }
const moduleTypes = new Set<string>(MODULE_EVENTS)
/** every stream event, for pages that keep their own data (contacts, knowledge, accounts, vault, signals, flows, GitHub …) */
export const hubSubs = new Set<(e: HubEvent | ModuleEvent) => void>()
/** call `f` (debounced by `ms`) whenever the stream reports a change of one of `types` */
export function useHubEvents(types: readonly string[], f: () => void, ms = 300, options: { maxWait?: number; filter?: (e: HubEvent | ModuleEvent) => boolean } = {}) {
  const ref = useRef({ f, filter: options.filter }); ref.current = { f, filter: options.filter }
  const key = types.join(), maxWait = options.maxWait ?? 1000
  useEffect(() => {
    const d = debounceMax(() => ref.current.f(), ms, maxWait)
    const h = (e: HubEvent | ModuleEvent) => { if (e.type === "resync" || (types.includes(e.type) && (!ref.current.filter || ref.current.filter(e)))) d.schedule() }
    hubSubs.add(h); return () => { hubSubs.delete(h); d.cancel() }
  }, [key, ms, maxWait]) // eslint-disable-line react-hooks/exhaustive-deps
}
/* ───────── YOLO: the switch and the current window's ledger, refetched on `yolo` / `yolo_action` ───────── */
let yoloTimer: any = null
export async function loadYolo() {
  try {
    const [y, d] = await Promise.all([yoloApi.status(), yoloApi.actions({ window: "current" })])
    const acts: Record<string, RYoloAction> = {}
    for (const w of d.windows) for (const g of w.items) for (const a of g.actions) if (a.decision_id) acts[a.decision_id] = a
    st.yolo = y; st.yoloActs = acts
  } catch { st.yolo = null; st.yoloActs = {} }
  emit()
}
const yoloSoon = () => { clearTimeout(yoloTimer); yoloTimer = setTimeout(() => void loadYolo(), 250) }
async function yoloCall<T>(p: Promise<T>, ok?: string): Promise<T | undefined> {
  try { const r = await p; if (ok) toast(ok); await loadYolo(); return r } catch (e) { fail(e); return undefined }
}
export const yoloSet = (mode: "off" | "on" | "full", includeOpen = false) => yoloCall(yoloApi.set(mode, includeOpen), t(`core:yolo.${mode}`))
export const yoloScope = (type: "project" | "item", id: string, mode: "on" | "off" | "inherit" | "full") => yoloCall(yoloApi.scope(type, id, mode), t("common:state.saved"))
export const yoloResume = () => yoloCall(yoloApi.resume(), t("core:yolo.resumed"))
export const yoloUndo = (id: string, text?: string) => yoloCall(yoloApi.undo(id, text), t("core:toast.undone")).then((r) => { void refresh(); return r })
export const yoloNow = (id: string) => yoloCall(yoloApi.now(id)).then((r) => { void refresh(); return r })
export const yoloCancel = (id: string) => yoloCall(yoloApi.cancel(id), t("core:yolo.leftToYou"))

function onHub(e: HubEvent | ModuleEvent) {
  /* the Hub could not continue from our cursor (it restarted, or we were away past its window): refetch everything, once */
  if (e.type === "stream.reset") { void resync(); return }
  /* the replay after a reconnect is done; transcript chunks from the gap are not replayed, the events API has them */
  if (e.type === "stream.gap") { freezeCursors(); void bounded([...new Set([...raw.events.keys(), ...raw.loadingEv])], catchUp); yoloSoon(); return }
  hubSubs.forEach((f) => f(e))
  if (e.type === "yolo" || e.type === "yolo_action") { yoloSoon(); return }
  if (moduleTypes.has(e.type)) return
  e = e as HubEvent
  if (e.type === "workspace") { void resync(); return }
  if (e.type === "release.hold") { st.hold = e.hold; emit(); return }
  if (e.type === "steve.draft") { st.draft = { mid: e.mid, thought: e.thought, text: e.text }; if (!st.thread.some((m) => m.k === "thinking")) st.thread.push({ k: "thinking" }); drawDraft(); return }
  if (e.type === "steve.tool") {
    if (st.live?.run !== e.run) st.live = { mid: e.mid, run: e.run, t0: Date.now(), tools: [] }
    st.live.tools.push({ name: e.name, brief: e.brief, ok: e.ok })
    if (!st.thread.some((m) => m.k === "thinking")) st.thread.push({ k: "thinking" })
    drawDraft(); return
  }
  if (e.type === "thread.run") { soon(); return }
  if (e.type === "steve.thinking") { st.thinking = e.on; st.phase = e.on ? e.phase ?? "" : ""; cue(e.on ? "think" : "reply"); build(); emit(); if (!e.on) soon(); return }
  if (e.type === "event") {
    const xs = raw.events.get(e.session_id)
    if (["turn_end", "user", "permission", "system"].includes(e.kind)) soon()
    if (raw.loadingEv.has(e.session_id)) { missedEv.add(e.session_id); return }
    if (xs && !(xs.length && xs[xs.length - 1].seq >= e.seq && xs.some((x) => x.seq === e.seq))) { xs.push({ seq: e.seq, ts: Date.now(), kind: e.kind, data: e.data }); if (xs.length > 1 && xs.at(-2)!.seq > e.seq) xs.sort((a, b) => a.seq - b.seq); paint(e.session_id) }
    return
  }
  if (e.type === "item") dirty.add(e.id); else if ((e.type === "decision" || e.type === "session") && e.item_id) dirty.add(e.item_id)
  soon()
}
/* the stream is bound to the workspace it was opened in: reopen it after a switch */
let closeStream = () => {}
let resyncing: Promise<void> | null = null, resyncAgain = false
/** Freeze the pre-gap transcript cursors before newer SSE frames can advance the caches. */
function freezeCursors() { for (const [sid, xs] of raw.events) if (!replayFrom.has(sid)) replayFrom.set(sid, xs.at(-1)?.seq ?? 0) }
function resync(): Promise<void> {
  freezeCursors()
  if (resyncing) { resyncAgain = true; return resyncing }
  resyncing = (async () => {
    do {
      resyncAgain = false
      const epoch = scopeEpoch
      for (const id of new Set([...raw.details.keys(), ...raw.loadingItem])) dirty.add(id)
      hubSubs.forEach((f) => f({ type: "resync", id: "" }))
      await refresh()
      if (epoch !== scopeEpoch) return
      await reloadOpen()
      if (epoch !== scopeEpoch) return
      await bounded([...new Set([...raw.events.keys(), ...raw.loadingEv])], catchUp)
      yoloSoon()
    } while (resyncAgain)
  })().finally(() => { resyncing = null; if (resyncAgain) void resync() })
  return resyncing
}
/* Opened after the state it continues from. A resumed (re)open replays only the frames after the cursor, which take the
 * live paths below; without a cursor (the state failed to load, or an old Hub) every open resyncs everything. A stream
 * opened without a cursor carries no frame ids, so the watermark a later resync loads is never its cursor: the frames
 * handled live since then would all be replayed on the next reconnect (steve.tool twice). Once that resync has a
 * watermark the stream is reopened from it instead, so its frames carry ids; only the short handoff can repeat. */
let streamGen = 0
const connect = () => {
  closeStream()
  const gen = ++streamGen, from = watermark
  closeStream = stream(onHub, (resumed) => {
    if (resumed) return
    void resync().then(() => { if (gen === streamGen && watermark) connect() })
  }, () => from)
  yoloSoon()
}
export function start() {
  let live = true
  void refresh().then(() => { if (live) connect() })
  return () => { live = false; streamGen++; closeStream(); closeStream = () => {} }
}

/* ───────── selectors ───────── */
export const it = (id: string) => st.items.find((x) => x.id === id)!
export const pending = () => st.items.filter((i) => (i.st === "wait" || i.st === "prop" || !!i.wait) && !!i.wait && !i.snooze && !i.arch)
const allCards = (): Orphan[] => [...pending().flatMap((i) => i.waits.map((w) => ({ i, w }))), ...st.orphans]
/** a memory to confirm: it blocks nothing, so it is counted on one line of 待处理, not shown as a card */
export const isMemCard = (w: Wait) => w.type === "confirm" && w.subject === "mem"
/** every open card that holds work, several per item when it has more than one */
export const pendingCards = (): Orphan[] => allCards().filter((c) => !isMemCard(c.w))
/** the memories waiting to be confirmed (their batch page is 知识 › 待确认) */
export const memCards = (): Orphan[] => allCards().filter((c) => isMemCard(c.w))
/** the open cards a session is waiting on, with or without an item */
export const sessionCards = (sid: string): Orphan[] => [...st.items.flatMap((i) => i.waits.filter((w) => w.session === sid).map((w) => ({ i, w }))), ...st.orphans.filter((o) => o.w.session === sid)]
/** "" is the stand-in id of a card without an item */
const cardOf = (id: string, decision?: string): { i?: Item; w?: Wait } => { if (!id) return st.orphans.find((o) => o.w.decision === decision) ?? {}; const i = it(id); return { i, w: decision ? i?.waits.find((x) => x.decision === decision) : i?.wait } }
export const snoozed = () => st.items.filter((i) => !!i.wait && !!i.snooze)
export const active = (i: Item) => i.ss.map((x) => st.S[x]).filter((s) => s && s.st !== "ended").pop()
export const sidOf = (s: Sess) => s.id
export const accOf = (i: Item) => (i.acc !== ACC.inherit ? i.acc : st.bots[i.b] && st.bots[i.b].acc !== ACC.inherit ? st.bots[i.b].acc : st.cfg.accDefault)
export const botName = (k: string | null) => (!k ? "" : st.bots[k] ? (st.bots[k].main ? st.bots[k].n : st.bots[k].n + " Bot") : k === "agent" ? "Agent" : k)
export const hm = (s: Sess) => t("core:harnessModel", { harness: s.h ?? t("core:unknownHarness"), model: s.m ?? t("core:defaultModel") })
export const isThinking = () => st.thinking
export const projectName = (id: string | null | undefined) => st.projects.find((p) => p.id === id)?.name ?? t("common:state.none")
export const depsOpen = (i: Item) => (i.deps || []).filter((d) => st.items.find((x) => x.id === d)?.st !== "done")
export function stageOf(i: Item): string {
  if (i.st === "prop") return "分流"; if (i.st === "block") return "规划"; if (i.st === "done" || i.st === "drop") return "收尾"
  if (i.wait?.type === "accept") return "交付与验收"; if (i.wait?.type === "propose") return "规划"
  if (/发布/.test(i.now)) return "收尾"
  if (/^正在开始|刚开始/.test(i.now)) return "定义"; return "执行"
}

/* ───────── actions: conversation ───────── */
/** how long the conversation keeps talking to an item after the last message sent to it (§5.3) */
const FOCUS_MS = 10 * 60_000
const ITEM_REF = /^#([A-Z]{1,6}-\d+)\b/
/** `clientId`: the send idempotency key (the composer's draft keeps one across retries; a button sends a new one) */
export async function chatSend(v: string, files: string[] = [], refs: RFile[] = [], mentions: { kind: "party" | "bot"; id: string }[] = [], clientId = newClientId()) {
  /* a quote of an item's message, or the focus bar, sends straight to that item; `#ST-n` does too and starts the focus */
  const quoted = st.quote ? st.thread.find((m) => m.id === st.quote) ?? knownMsgs.get(st.quote) : undefined
  const focus = st.focus && st.focus.until > Date.now() ? st.focus.item : undefined
  const item = quoted?.item ?? (quoted ? undefined : focus)
  const toItem = item ?? ITEM_REF.exec(v)?.[1]
  const to = { item, reply_to: quoted?.id, client_id: clientId }
  st.quote = null; st.focus = toItem ? { item: toItem, until: Date.now() + FOCUS_MS } : st.focus
  /* Steve is on a turn: the message waits in the queue above the composer and goes when the turn ends (the Hub decides) */
  if (chatBusy()) {
    const m: Msg = { clientId: crypto.randomUUID(), k: "me", t: v, files: refs, replyTo: to.reply_to, held: true }
    sendingHeld.push(m); st.queue = [...st.queue, m]; emit()
    const done = () => { const n = sendingHeld.indexOf(m); if (n >= 0) sendingHeld.splice(n, 1) }
    try { await api.say(v, files, mentions, to); done(); await refresh(); return true } catch (e) { done(); fail(e); build(); emit(); cue("failure"); return false }
  }
  st.draft = null; st.thread.push({ clientId: crypto.randomUUID(), k: "me", t: v, files: refs, replyTo: to.reply_to }, { k: "thinking" }); st.thinking = true; emit(); cue("think")
  try { await api.say(v, files, mentions, to); return true } catch (e) { fail(e); st.thinking = false; build(); emit(); cue("failure"); return false }
}
/** queued messages still being sent: shown in the queue until the Hub has them */
const sendingHeld: Msg[] = []
/** Steve is on a turn, or messages wait for one: what is sent now queues */
export const chatBusy = () => st.thinking || !!st.live || st.queue.length > 0
/** stop the turn Steve is on; what waits in the queue goes next, the composer keeps what is being typed */
export async function stopReply() {
  const r = await Promise.all([st.live ? call(api.stopRun(st.live.run)) : undefined, call(api.stop())])
  cue("idle")
  /* nothing was running any more (the turn ended meanwhile, or a reload missed its end): the page stops waiting */
  if (r[1] && !(r[1] as { ok?: boolean }).ok && !r[0]?.ok) { st.thinking = false; st.live = null; build(); emit(); soon() }
}
/* ── the main conversation's queue: a message waiting for the turn can be changed or withdrawn until it goes ── */
export async function threadQueuedEdit(id: string, text: string) { const r = await qCall(api.threadQueuedEdit(id, text)); if (r) toast(t("core:toast.queuedEdited")); return !!r }
/** back into the composer when it is empty; otherwise removed, with an undo that queues it again */
export async function threadQueuedWithdraw(id: string, composer?: { empty: () => boolean; fill: (t: string, f: RFile[]) => void } | null) {
  const r = await qCall(api.threadQueuedWithdraw(id)); if (!r) return
  const files = r.files ?? []
  if (composer?.empty()) { composer.fill(r.text, files); toast(t("core:toast.withdrawnToComposer")); return }
  toast(t("core:toast.withdrawn"), { action: { label: t("core:toast.undo"), onClick: () => void chatSend(r.text, files.map((f) => f.id), files) } })
}
export function quoteMsg(id: string | null) { st.quote = id; emit(); requestAnimationFrame(() => document.querySelector<HTMLTextAreaElement>("[data-composer='chat'] textarea, textarea")?.focus()) }
export function clearFocus() { st.focus = null; emit() }
export const focusItem = () => (st.focus && st.focus.until > Date.now() ? st.focus.item : null)
/* thread routing actions: a 409 (already handled) comes back as its message */
export async function takeOffer(m: Msg, take: boolean) { if (!m.id) return; const r = await call(api.offer(m.id, take)); if (r?.item) toast(t("core:toast.madeItem", { id: r.item.id })) }
/** an A2UI button pressed in a reply: `already` when this surface had been submitted (another tab, a double click that got through) */
export async function a2uiSubmit(msg: string, a: A2uiSubmit): Promise<{ already: boolean }> {
  const r = await api.a2ui(msg, a)
  await refresh().catch(fail)
  return { already: r.already }
}
export async function chooseSuggest(m: Msg, choice: SuggestChoice) {
  if (!m.id) return
  const append = !!m.suggest?.choices.find((c) => c.id === choice)?.item
  const r = await call(api.suggest(m.id, choice))
  if (r?.item) toast(t(append ? "core:toast.appendedTo" : "core:toast.createdItem", { id: r.item.id }))
}
export async function reroute(m: Msg, to: string) {
  if (!m.id) return
  const r = await call(api.reroute(m.id, to))
  if (r) toast(to === "inline" ? t("core:toast.answerHere") : r.item ? t(to.startsWith("item:") ? "core:toast.sentTo" : "core:toast.madeItem", { id: r.item.id }) : t("core:toast.changed"))
}
/** 做成事项 on an answer: the old promote when it is a chat answer, else a correction to an item */
export function makeItem(m: Msg) { if (m.chat) void promote(m); else void reroute(m, "item") }
export function cancelQueued(run: string) { void call(api.cancelRun(run), t("core:toast.withdrawn")) }
export function stopRun(run: string) { void call(api.stopRun(run)); cue("idle") }
export async function threadFollow(item: string, follow: boolean) { await call(api.threadFollow(item, follow), t(follow ? "core:toast.follow" : "core:toast.unfollow")); dirty.add(item); soon() }
/** the raw thread info of an item's detail (§5), once loaded */
export const itemThread = (id: string) => raw.details.get(id)?.thread ?? null
/** a decision's full text (a delivery summary), from the open cards, item details or recent decisions */
export function decisionDraft(id: string): string | undefined {
  for (const d of raw.state?.decisions ?? []) if (d.id === id) return d.draft
  for (const det of raw.details.values()) for (const d of det.decisions) if (d.id === id) return d.draft
  return raw.state?.recent.find((d) => d.id === id)?.draft
}
/** notifications newer than the read cursor */
export const unreadNotices = () => st.notices.filter((n) => n.created > st.noticeSeen)
export function readNotices() {
  const at = st.notices[0]?.created ?? 0; if (at <= st.noticeSeen) return
  st.noticeSeen = at; emit(); void api.readNotifications(at).catch(fail)
}
/** close a failure notice: its line leaves the conversation and it is no longer pinned; the Hub remembers it */
export function dismissNotice(id: string) { closeAlerts([id]) }
/* a chat answer becomes one item: clicks while it is being made are ignored, and the hub returns the same item if asked again */
const promoting = new Set<string>()
export const isPromoting = (m: Msg) => !!m.id && promoting.has(m.id)
export async function promote(m: Msg) {
  if (!m.id || promoting.has(m.id)) return
  promoting.add(m.id); emit(); cue("success")
  try { const r = await call(api.promote(m.id)); if (r) toast(t("core:toast.madeItem", { id: r.id })) } finally { promoting.delete(m.id); emit() }
}
export function finishOnboarding() { localStorage.setItem("steve-onboarded", "1"); st.onboard = -1; emit() }

/* ───────── actions: decisions ───────── */
/** when a snoozed card comes back; the Hub keeps the label it is given and shows it */
export const SNOOZE_KEYS = ["hour", "tonight", "morning"] as const
export type SnoozeKey = (typeof SNOOZE_KEYS)[number]
const SNOOZE: Record<SnoozeKey, () => number> = {
  hour: () => Date.now() + 3600_000,
  tonight: () => { const d = new Date(); d.setHours(20, 0, 0, 0); if (d.getTime() < Date.now()) d.setDate(d.getDate() + 1); return d.getTime() },
  morning: () => { const d = new Date(); d.setDate(d.getDate() + 1); d.setHours(8, 30, 0, 0); return d.getTime() },
}
export const snoozeLabel = (k: SnoozeKey) => t(`core:snooze.${k}`)
/* the Hub stores the label and writes it into the timeline, so it gets the zh-CN text whatever the UI language */
export function snooze(id: string, key: SnoozeKey) { const when = snoozeLabel(key); void call(api.snooze(id, t(`core:snooze.${key}`, { lng: "zh-CN" }), (SNOOZE[key] ?? SNOOZE.hour)()), t("core:snooze.toast", { when })) }
export function unsnooze(id: string) { void call(api.snooze(id, null, null)) }
/** resolve the item's open decision; `c` is the decision's own choice id */
/* the toast after answering a card: per module subject first, then by choice; the plain fallbacks last */
const DECIDED_BY_SUBJECT: Record<string, readonly string[] | ((c: string) => string)> = {
  signal: ["done", "to_item"],
  flow_step: (c) => (c.startsWith("不") ? t("core:decided.flowSkipped") : t("core:decided.flowChose", { choice: c })),
  flow_run: ["retry", "drop", "continue_once", "stop"],
  secret_use: ["once", "grant_1h", "grant_8h", "grant_24h", "deny"],
  connection: ["pause"],
  space: ["open", "mute"],
  mem: ["yes", "no"],
  party_merge: ["merge", "not_same"],
  release: ["no"],
  chan_message: ["yes", "no", "rewrite"],
}
const DECIDED_ANY = ["pass", "rework", "yes", "no", "continue", "delivered", "look", "tosteve", "drop", "deny"]
function decidedText(w: Wait, c: string, bot: string | null) {
  const m = DECIDED_BY_SUBJECT[w.subject]
  if (typeof m === "function") return m(c)
  if (m?.includes(c)) return t(`core:decided.${w.subject}.${c}` as "core:decided.signal.done")
  if (w.type === "confirm") return c === "yes" ? t("core:decided.confirmYes") : t("core:decided.confirmNo")
  if (DECIDED_ANY.includes(c)) return t(`core:decided.any.${c}` as "core:decided.any.pass")
  return w.type === "ask" ? t("core:decided.toldBot", { bot: botName(bot) }) : w.subject === "tool_call" ? t("core:decided.allowed") : t("core:decided.handled")
}
/** accepted, but the project still has a release to go: the item is not finished yet */
const releaseAhead = (i?: Item) => !!i?.branch && !!st.projects.find((p) => p.id === i.project)?.release
/** the acceptance chooses whether to release (验收并发布 / 验收，先不发布): a release script runs on accept. A project
 * done by its rule (release.done_when merge/tag) is finished by that rule, so its acceptance has nothing to choose */
export const acceptChoosesRelease = (i?: Item) => {
  const r = releaseAhead(i) ? st.projects.find((p) => p.id === i!.project)?.release : null
  return !!r?.cmd && !r.done_when
}
export async function decide(id: string, c: string, text?: string, decision?: string) {
  const { i, w } = cardOf(id, decision); if (!w) return
  const key = "decision:" + w.decision
  if (st.pendingActions.has(key)) return
  st.pendingActions.add(key); emit()
  try {
    if (w.subject === "release" && c === "yes") {
      /* the answer already says whether the release started; "已发布" only comes with the result */
      const d = await call(api.decide(w.decision, c, text))
      if (d) { const rel = relOf(d); toast(rel?.text ?? t("core:decided.releaseApproved")); if (rel?.tone === "fail") cue("failure") }
      return
    }
    if (c === "pass" && w.type === "accept" && releaseAhead(i) && i) {
      /* the Hub finishes an item with nothing on its branch right away: say so instead of waiting for a release;
       * 验收并发布 starts it on the Hub unless the branch moved or a release runs (then the agent asks for it) */
      const d = await call(api.decide(w.decision, c, text)); if (!d) return
      cue("success")
      const after = await api.item(i.id).catch(() => null)
      toast(after?.status === "done" ? (text === "no-release" ? t("core:decided.acceptedNoRelease") : t("core:decided.acceptedNothingToRelease"))
        : d.result?.release === "started" ? t("core:decided.acceptedReleasing") : t("core:decided.acceptedAwaitRelease"))
      return
    }
    const result = await call(api.decide(w.decision, c, text), decidedText(w, c, i?.b ?? null))
    if (result && c === "pass") cue("success")
  } finally { st.pendingActions.delete(key); emit() }
}
/** a reply drafted in an IM conversation, closed in that conversation: not sent, for good (undo from 已处理);
 *  one answer at a time, a second click while it is on its way does nothing */
export async function discardOut(decision: string) {
  const key = "decision:" + decision
  if (st.pendingActions.has(key)) return false
  st.pendingActions.add(key); emit()
  try { return !!(await call(api.decide(decision, "no"), t("core:toast.discarded"))) }
  finally { st.pendingActions.delete(key); emit() }
}
/** take one card out of 待处理 without answering it (the item's first one unless `decision` says which);
 *  undoable from the toast or from 已处理, except a tool permission, which this denies */
export async function dismiss(id: string, decision?: string) {
  const { w } = cardOf(id, decision); if (!w) return
  const d = await call(api.dismiss(w.decision))
  if (d) toast(t(w.subject === "tool_call" ? "core:toast.dismissedTool" : w.subject === "chan_message" && w.type === "approve" ? "core:toast.dismissedReply" : "core:toast.dismissed"), w.subject === "tool_call" ? undefined : { action: { label: t("core:toast.undo"), onClick: () => void call(api.undo(w.decision), id ? t("core:toast.backToPending", { id }) : t("core:toast.backToPendingIt")) } })
}
export function undoDecision(n: number) { const d = st.done[n]; if (!d?.canUndo) return; void call(api.undo(d.decision), t("core:toast.undoneToPending", { id: d.id })) }

/* ───────── actions: items ───────── */
/** `clientId`: the same for every retry of one unchanged form, so a retry does not make a second item */
export async function newItem(goal: string, bot?: string, project?: string | null, mode: "start" | "propose" = "start", clientId = newClientId()) {
  const r = await call(api.newItem({ goal, bot, project, mode, client_id: clientId })); return r?.id
}
export async function supp(id: string, v: string, target: "auto" | "steer" | "new", files: string[] = []) {
  const msg = t(target === "steer" ? "core:toast.steered" : target === "new" ? "core:toast.newSession" : active(it(id))?.st === "run" ? "core:toast.queuedForTurn" : "core:toast.delivered")
  return sendMessage(api.supplement(id, v, target, files), msg)
}
export function deliver(id: string) { void call(api.deliver(id), t("core:toast.delivered")) }
export function withdraw(qid: string) { void call(api.dropQueued(qid), t("core:toast.withdrawn")) }
/* ── queued messages of a session: each one can be edited, withdrawn or sent now ──
 * A row a turn already took answers 409 (“已经发出”): the error is shown and the strip refreshes. */
async function qCall<T>(p: Promise<T>): Promise<T | undefined> {
  try { const r = await p; await refresh(); return r } catch (e) { fail(e); await refresh(); return undefined }
}
export async function qEdit(sid: string, qid: string, text: string) { const r = await qCall(api.queuedEdit(sid, qid, text)); if (r) toast(t("core:toast.queuedEdited")); return !!r }
/** back into the composer when it is empty; otherwise just removed, with an undo that queues it again */
export async function qWithdraw(sid: string, qid: string, composer?: { empty: () => boolean; fill: (t: string, f: RFile[]) => void } | null, strip = (t: string) => t) {
  const r = await qCall(api.queuedRemove(sid, qid)); if (!r) return
  const files = r.files ?? []
  if (composer?.empty()) { composer.fill(strip(r.text), files); toast(t("core:toast.withdrawnToComposer")); return }
  toast(t("core:toast.withdrawn"), { action: { label: t("core:toast.undo"), onClick: () => void call(api.sessionSay(sid, r.text, "排队", files.map((f) => f.id))).then((x: any) => x && toast(t(x.result === "queued" ? "core:toast.requeued" : "core:toast.sent"))) } })
}
export async function qSendNow(sid: string, qid: string) {
  const r = await qCall(api.queuedSendNow(sid, qid)); if (!r) return
  toast(r.result === "interrupted" ? t("core:toast.interruptedThenSend") : r.result === "sent" ? t("core:toast.sent") : (r.reason ?? t("core:toast.cannotSendYet")))
}
export function saveGoal(id: string, v: string) { const i = it(id); if (!v || v === i.goals[i.goals.length - 1]) return; void call(api.goal(id, v), t("core:toast.goalUpdated")) }
export function reassign(id: string, k: string) { if (k === it(id).b) return; void call(api.reassign(id, k), t("core:toast.handedTo", { bot: botName(k) })) }
export function setSt(id: string, s: Item["st"]) {
  const i = it(id), action = s === "pause" ? "pause" : s === "drop" ? "drop" : ["done", "drop"].includes(i.st) ? "reopen" : "resume"
  void call(api.itemAction(id, action), t(({ pause: "core:toast.paused", drop: "core:toast.dropped", reopen: "core:toast.reopened", resume: "core:toast.resumed" } as const)[action]))
}
/** mark done right away; the toast can take it back (the Hub restores the item as it was) */
export async function markDone(id: string) {
  const r = await call(api.itemAction(id, "finish")); if (!r) return
  toast(t("core:toast.markedDone", { id }), { action: { label: t("core:toast.undo"), onClick: () => void call(api.itemAction(id, "unfinish"), t("core:toast.backToBefore", { id })) } })
}
export function star(id: string) { void call(api.patchItem(id, { star: !it(id).star })) }
export function setAcc(id: string, v: string) { void call(api.patchItem(id, { accept_mode: accKey(v) })) }
export function patchItem(id: string, p: Partial<Item>) {
  const q: Parameters<typeof api.patchItem>[1] = {}
  if (p.t !== undefined) q.title = p.t
  if (p.pri !== undefined) q.priority = p.pri
  if ("due" in p) q.due = p.due ?? null
  if ("project" in p) q.project_id = p.project ?? null
  void call(api.patchItem(id, q))
}
export function archive(ids: string[], v = true) { void call(Promise.all(ids.map((id) => api.itemAction(id, v ? "archive" : "unarchive"))), t(v ? "core:toast.archived" : "core:toast.restored", { count: ids.length })) }
export function deleteItem(id: string) { void call(api.deleteItem(id), t("core:toast.deletedItem", { id })) }
export function bulk(ids: string[], op: "pause" | "run" | "drop" | "arch") {
  if (op === "arch") return archive(ids)
  void call(api.bulk(ids, op === "run" ? "resume" : op).then((r) => { toast(t(({ pause: "core:toast.bulkPaused", run: "core:toast.bulkResumed", drop: "core:toast.bulkDropped" } as const)[op], { count: r.n })); return r }))
}
export function addDep(id: string, dep: string) { void call(api.addDep(id, dep), t("core:toast.depAdded", { id: dep })) }
export function rmDep(id: string, dep: string) { void call(api.rmDep(id, dep)) }

/* ───────── actions: sessions ───────── */
/** at = index into the session's transcript; the Hub forks at that event */
export async function doFork(k: string, at: number, goal: string, asSess: boolean): Promise<string> {
  const s = st.S[k], n = Math.min(at, s.seqs.length - 1)
  /* fork after the whole tuple (an agent reply spans many chunk events), not at its first event */
  const seq = s.seqs[n + 1] ? s.seqs[n + 1] - 1 : n >= 0 ? (raw.events.get(k)?.at(-1)?.seq ?? s.seqs[n]) : 0
  const r = await call(api.fork(k, seq, goal, asSess), t(asSess ? "core:toast.forkedSession" : "core:toast.forkedItem"))
  return r?.item ? "#/items/" + r.item : r?.session ? "#/sessions/" + r.session : "#/sessions/" + k
}
export async function sessSend(k: string, v: string, mode: "插话" | "排队", files: string[] = []) {
  const key = "session:" + k
  if (st.pendingActions.has(key)) return false
  st.pendingActions.add(key); emit()
  try { return await sendMessage(api.sessionSay(k, v, mode, files), st.S[k]?.st === "run" && mode === "排队" ? t("core:toast.queuedAfterTurn") : undefined) }
  finally { st.pendingActions.delete(key); emit() }
}
export function interrupt(k: string) { void call(api.cancel(k), t("core:toast.interrupted")) }
export function link(k: string, id: string) { void call(api.link(k, id), t("core:toast.linked", { title: it(id).t })) }
export function sessPatch(k: string, p: Partial<Sess>, msg?: string) {
  if (p.title) void call(api.renameSession(k, p.title), msg)
  if (p.st === "ended") void call(api.endSession(k), msg)
}
export function sessRemove(k: string) { const title = st.S[k]?.title; void call(api.removeSession(k), t("core:toast.sessionRemoved", { title })) }
export async function loadLocalSessions() { try { st.localSessions = await api.localSessions(); emit() } catch (e) { fail(e) } }
export async function importSessions(keys: string[]) {
  const r = await call(api.importSessions(keys)); if (!r) return
  const bad = r.filter((x) => typeof x !== "string") as { error: string }[]
  toast(bad.length ? t("core:toast.importedSome", { ok: r.length - bad.length, bad: bad.length, error: bad[0].error }) : t("core:toast.imported", { count: r.length }))
}

/* ───────── actions: bots ───────── */
export async function loadMemories(k: string) {
  try { const xs = await api.memories(k); const b = st.bots[k]; if (b) { b.mem = xs.map((m) => [m.text, m.source, m.id]); b.memLoaded = true; emit() } } catch (e) { fail(e) }
}
export async function saveBot(k: string, p: { n?: string; role?: string; pers?: string; pro?: number; rule?: string; acc?: string; harness?: string; model?: string | null; reasoningEffort?: string | null; permMode?: string; project?: string | null; color?: string; round?: number; skin?: string | null; prompt?: string | null; on?: boolean; dev?: string | null }, msg = t("core:toast.botSaved")) {
  const b = st.bots[k]
  const q: Partial<RBot> = {}
  if (p.reasoningEffort !== undefined) q.reasoning_effort = p.reasoningEffort
  if (p.n !== undefined) q.name = p.n; if (p.role !== undefined) q.role = p.role; if (p.pers !== undefined) q.persona = p.pers; if (p.pro !== undefined) q.proactive = p.pro; if (p.rule !== undefined) q.rules = p.rule
  if (p.acc !== undefined) q.accept_mode = accKey(p.acc); if (p.harness !== undefined) q.harness = p.harness; if (p.model !== undefined) q.model = p.model; if (p.permMode !== undefined) q.perm_mode = p.permMode
  if (p.project !== undefined) q.default_project = p.project; if (p.prompt !== undefined) q.prompt = p.prompt; if (p.on !== undefined) q.enabled = p.on; if (p.dev !== undefined) q.device = p.dev
  if (p.color !== undefined || p.round !== undefined || p.skin !== undefined) q.avatar = { color: p.color ?? b.color, round: p.round ?? b.round, skin: p.skin !== undefined ? p.skin : b.skin }
  await call(api.patchBot(k, q), msg)
}
export async function makeBot(n: string, role: string, harness = "claude-code", model: string | null = null) { const r = await call(api.newBot({ name: n, role, harness, model }), t("core:toast.botCreated")); return r?.id }
export async function draftBot(desc: string) { const r = await call(api.draftBot(desc), t("core:toast.botDrafted")); return r?.id }
export async function deleteBot(k: string, to: string, mem: "删除" | "导出") {
  const b = st.bots[k]; const r = await call(api.deleteBot(k, to, mem === "删除" ? "delete" : "export")); if (!r) return
  if (mem === "导出" && r.memories.length) { const a = document.createElement("a"); a.href = URL.createObjectURL(new Blob([JSON.stringify(r.memories, null, 2)], { type: "application/json" })); a.download = t("core:toast.memoryFile", { name: b?.n ?? k }); a.click() }
  toast(t(mem === "导出" ? "core:toast.botDeletedExported" : "core:toast.botDeleted", { name: b?.n, count: r.moved.length, to: botName(to) }), { action: { label: t("core:toast.undo"), onClick: () => void call(api.restoreBot(k), t("core:toast.botRestored", { name: b?.n })) } })
}
export async function dupBot(k: string) { const r = await call(api.dupBot(k), t("core:toast.botCopied")); return r?.id }
export function addMem(k: string, text: string) { void call(api.addMemory(k, text), t("core:toast.memoryAdded")).then(() => loadMemories(k)) }
export function editMem(k: string, id: string, text: string) { void call(api.editMemory(id, text)).then(() => loadMemories(k)) }
export function delMem(k: string, id: string) { void call(api.deleteMemory(id), t("core:toast.deleted")).then(() => loadMemories(k)) }
export function setBotWorkspaces(k: string, ids: string[]) { void call(api.botWorkspaces(k, ids), t("core:toast.botWorkspaces")) }

/* ───────── actions: workspaces and projects ───────── */
export async function switchWorkspace(id: string) {
  try { await api.switchWs(id); setPageWorkspace(id); scopeEpoch++; replayFrom.clear(); painted.clear(); raw.loadingItem.clear(); raw.loadingEv.clear(); reloadAgain.clear(); missedEv.clear(); raw.details.clear(); raw.events.clear(); convs.clear(); openItems.clear(); dirty.clear(); watermark = null; st.thinking = false; st.draft = null; closeStream(); await refresh(); connect(); location.hash = "#/"; toast(t("core:toast.switchedWorkspace", { name: st.workspace?.name ?? "" })) } catch (e) { fail(e) }
}
export async function newWorkspace(name: string, description: string, item_prefix?: string) { const w = await call(api.newWs({ name, description, item_prefix })); if (w) await switchWorkspace(w.id) }
export async function newProject(p: { name: string; path: string; description?: string; worktree?: boolean; default_bot?: string | null }) { const r = await call(api.newProject(p), t("core:toast.projectAdded", { name: p.name })); return r?.id }
export function patchProject(id: string, p: Partial<RProject>, msg = t("common:state.saved")) { void call(api.patchProject(id, p), msg) }

/* ───────── tags ───────── */
export const tag = (id: string) => st.tags.find((t) => t.id === id)
export const tagName = (id: string) => tag(id)?.name ?? id
export const tagUses = (id: string) => st.tagUses[id] ?? 0
/** tag categories as the Hub stores them (Chinese values); show one with tagCatLabel */
export const TAG_CATS = ["项目", "类型", "领域", "客户", "主题"]
const TAG_CAT_KEY: Record<string, "project" | "kind" | "domain" | "client" | "topic"> = { 项目: "project", 类型: "kind", 领域: "domain", 客户: "client", 主题: "topic" }
/** a tag category in the UI language; an unknown one as it is */
export const tagCatLabel = (cat: string) => (TAG_CAT_KEY[cat] ? t(`core:tagCat.${TAG_CAT_KEY[cat]}`) : cat)
/* item categories (ST-34): every item has exactly one of these six tags of category 类型, the Hub refuses
   anything else; the other categories are domain tags, as many as fit */
export const KIND_CAT = "类型"
const KIND_HUES = [["bug", "#d9777f"], ["feat", "#7aa7c7"], ["doc", "#8c7ae6"], ["refactor", "#e0a458"], ["ops", "#5fb39a"], ["chore", "#6c7a89"]] as const
/** [the tag name the Hub keeps, its label, its color] */
export const KINDS: [string, string, string][] = KIND_HUES.map(([n, c]) => liveList<[string, string, string]>([n, "", c], 1, () => t(`core:kind.${n}`)))
/** categories a new or edited domain tag may have */
export const DOMAIN_CATS = TAG_CATS.filter((c) => c !== KIND_CAT)
export const isKind = (t?: Tag) => t?.cat === KIND_CAT
export const kindTags = () => KINDS.map(([n]) => st.tags.find((t) => isKind(t) && t.name === n)).filter(Boolean) as Tag[]
export const kindColor = (name: string) => KINDS.find(([n]) => n === name)?.[2] ?? "currentColor"
/** the item's category tag id; the Hub keeps exactly one */
export const kindOf = (i: Pick<Item, "tags">) => (i.tags ?? []).find((x) => isKind(tag(x)))
/** the category first, then the domain tags as they were added */
export const sortTags = (ids: string[] = []) => [...ids].sort((a, b) => +isKind(tag(b)) - +isKind(tag(a)))
export function setTags(id: string, ids: string[]) { void call(api.patchItem(id, { tags: ids })) }
/** switch the category in one write: the old one goes as the new one comes, never none in between */
export const loadKindsReview = () => api.kindsReview().catch(() => [] as RKindReview[])
/** the user looked at these filled-in categories (kept or changed); returns what is left */
export const kindsReviewed = (ids: string[]) => call(api.kindsReviewed(ids))
/** fill in the items without exactly one category; already filled ones are left alone */
export async function kindsBackfill() { const r = await call(api.kindsBackfill()); if (r) toast(r.filled.length ? t("core:toast.kindsFilled", { count: r.filled.length, unsure: r.filled.filter((f) => f.uncertain).length }) : t("core:toast.kindsAllSet", { count: r.checked })); return r }
export function setKind(id: string, kind: string) { const i = it(id); if (kindOf(i) === kind) return; setTags(id, [kind, ...(i.tags ?? []).filter((x) => !isKind(tag(x)))]) }
export async function makeTag(t: { name: string; cat: string; desc: string }) { const r = await call(api.newTag({ name: t.name, cat: t.cat, description: t.desc })); return r?.id }
export function saveTag(id: string, p: Partial<Tag>) { void call(api.patchTag(id, { name: p.name, cat: p.cat, description: p.desc }), t("core:toast.tagSaved")) }
export function deleteTag(id: string) { const n = tagUses(id), name = tagName(id); void call(api.deleteTag(id), t("core:toast.tagDeleted", { name, count: n })) }
export function mergeTag(from: string, into: string) { const a = tagName(from); void call(api.mergeTag(from, into), t("core:toast.tagMerged", { from: a, into: tagName(into) })) }

/* ───────── settings ───────── */
export function setSetting(p: Record<string, string>, msg = t("common:state.saved")) { void call(api.settings(p), msg) }

/* ───────── relation graph model ───────── */
export type GNode = { id: string; k: "item" | "sess" | "proj"; title: string; st: string; n?: number; b?: string | null; project?: string | null }
export type GEdge = [string, string, string]
export function gBuild() {
  const N: Record<string, GNode> = {}, E: GEdge[] = []
  st.items.filter((i) => !i.arch).forEach((i) => {
    N[i.id] = { id: i.id, k: "item", title: i.t, st: i.st, b: i.b, project: i.project }
    let prev: string | null = null
    i.ss.forEach((k) => {
      const s = st.S[k]; if (!s) return
      N[k] = { id: k, k: "sess", title: s.title, st: s.st, n: s.n, b: s.b }
      E.push(prev ? [prev, k, "交接"] : [i.id, k, ""]); prev = k
    })
    ;(i.deps || []).forEach((d) => E.push([d, i.id, "依赖"]))
    if (i.parent) E.push(i.parent.s && st.S[i.parent.s] ? [i.parent.s, i.id, "派生"] : [i.parent.item, i.id, "派生"])
  })
  Object.keys(st.S).forEach((k) => { const s = st.S[k]; if (!s.item) N[k] = { id: k, k: "sess", title: s.title, st: s.st, n: s.n, b: s.b } })
  return { N, E: E.filter((e) => N[e[0]] && N[e[1]]) }
}
/** the projects of the shown items, as extra roots; kept out of gBuild so a project doesn't merge all its items into one component */
export function gProjects(ids: string[], N: Record<string, GNode>) {
  const P: Record<string, GNode> = {}, E: GEdge[] = []
  for (const k of ids) {
    const n = N[k], p = n?.k === "item" && n.project ? st.projects.find((x) => x.id === n.project) : undefined
    if (!p) continue
    P[p.id] ??= { id: p.id, k: "proj", title: p.name, st: "", n: st.items.filter((i) => i.project === p.id && !i.arch).length }
    E.push([p.id, k, "项目"])
  }
  return { P, E }
}
export function gComps(N: Record<string, GNode>, E: GEdge[]) {
  const p: Record<string, string> = {}; const f = (x: string): string => (p[x] === x ? x : (p[x] = f(p[x])))
  Object.keys(N).forEach((k) => (p[k] = k)); E.forEach(([a, b]) => { p[f(a)] = f(b) })
  const g: Record<string, string[]> = {}; Object.keys(N).forEach((k) => (g[f(k)] = g[f(k)] || []).push(k)); return Object.values(g)
}
export function gNear(f: string, E: GEdge[]) {
  const up = new Set([f]); let ch = true
  while (ch) { ch = false; E.forEach(([a, b]) => { if (up.has(b) && !up.has(a)) { up.add(a); ch = true } }) }
  E.forEach(([a, b]) => { if (a === f) up.add(b) }); return [...up]
}
export function gRank(ids: string[], E: GEdge[]) {
  const set = new Set(ids), es = E.filter((e) => set.has(e[0]) && set.has(e[1])), r: Record<string, number> = {}
  ids.forEach((k) => (r[k] = 0))
  for (let n = 0; n < ids.length; n++) es.forEach(([a, b]) => { if (r[b] < r[a] + 1) r[b] = r[a] + 1 })
  const rows: string[][] = []; ids.forEach((k) => (rows[r[k]] = rows[r[k]] || []).push(k))
  const ord: Record<string, number> = {}
  rows.forEach((c, ri) => {
    if (ri > 0) { const bc = (k: string) => { const ps = es.filter((e) => e[1] === k).map((e) => ord[e[0]]); return ps.length ? ps.reduce((a, b) => a + b) / ps.length : 99 }; c.sort((a, b) => bc(a) - bc(b)) }
    c.forEach((k, o) => (ord[k] = o))
  })
  return { rows, es, ord }
}
