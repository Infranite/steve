import { isResearchSnapshot, liveRunFromSnapshot, researchEvent, researchProgressEvent } from "@/thread/chat-progress"
export type { LiveRun } from "@/thread/chat-progress"
import type { ThreadLiveThought, ThreadPlanEntry, ThreadSubtask } from "@/gen"
import { debounceMax } from "./live-utils"
import { appendTranscript, getTranscript, recoverTranscripts, resetTranscripts, useTranscripts } from "./data/transcript"
export { briefHead } from "./data/transcript-model"
/* UI selectors over the read projection. The shell is independent of the read views.
 * Item detail, queue contents and session events load when opened. */
import { isCancelledError, useQuery } from "@tanstack/react-query"
import { useCallback, useEffect, useRef, useSyncExternalStore } from "react"
import { toast as notify, type ToastOptions } from "./components/ui/toaster"
import { promptNeedle, withDecisionNotes } from "./decision-notes"
import { OPTION_IDS, optionKey } from "./decision-options"
import { blockSlots, type FeedRow } from "./feed-order"
import { api, setPageWorkspace, stream, Unauthenticated, BotPlacementError, type Shell, type ConversationMessage, type RItemDetail, type RBot, type RExecutionSurface, type RProject, type Proposal, type LocalSession, type HubEvent, type RNotice, type RRuntime, type RDevice, type DecisionAnswer } from "./api"
import { cue } from "./bus"
import { newClientId } from "./composer-draft"
import { elapsed, pastTime, span } from "./i18n/format"
import i18n, { t } from "./i18n"
import { decisionText } from "./i18n/decision"
import type { StateUpdate, RFile, PlanStep, RHold, RLocalNodeUpdating, RQueued, A2uiRecord, A2uiSubmit, RAnnotation } from "./api"
import type { Cite } from "./api/knowledge"
import { applyLook } from "./lib/motion"
import { PERMISSION_MODES, canConfigurePermission } from "./runtime-status"
import type { SendRecoveryInfo } from "./api/channels"
import type { BotAccessory, DecisionTier, DecisionView, EffortMapping, FoldReason, ItemView, MemberIdentity, MessageView, PlacementExplanation, PlacementReason, PlacementRejection, ReasoningEffort, SessionLiveVitals, SessionView, SessionWaiting, ViewKind } from "./gen"
import { applyPatch, decisionsForItem, decisionsForSession, decisionView, loadViews, onCatalog, onEntityView, onViewKinds, messageView, messageViews, openDecisions, resetViews, settle, settled, subscribeItemScope, viewsCursor, itemView, itemViews, sessionView, sessionViews, sessionsForItem, qc, scopeKey, readViews, readItemSessions, table, itemWork, useItemView, useSessionView, useViewGroup } from "./data/sync"
import { chatLive, updateChat } from "./data/live"
import { itemDisplayStatusOf, itemState } from "./item-state"

/* ───────── view types (from the prototype) ───────── */
export type Sess = { id: string; n: number; item: string | null; b: string | null; h?: string; harness?: string; nativeId?: string | null; cwd?: string; m?: string; reasoningEffort?: ReasoningEffort | null; effortMapping?: EffortMapping | null; contextKnown?: boolean; /** the context in tokens, as the harness reported it: used and the window's size */ ctxUsed?: number; ctxSize?: number; st: "run" | "wait" | "idle" | "ended"; title: string; dur: string; ctx: number; src: string; up: string; /** status updated (ms), not execution activity */ at?: number; from?: string | null; fork?: { s: string; at: number }; queue: RQueued[]; ro?: boolean; device: string; node?: string; queued: number; busy: boolean; /** its next turn waits for room on its device */ room?: SessionWaiting | null
  /** not opened on its Node yet: no turn runs, the queue waits for the start; `since` is when it was created */
  starting?: boolean; since?: number; placement?: PlacementExplanation
  /** cumulative tokens & estimated USD cost (read-time enriched) */
  tokens?: number; costUsd?: number
  /** latest live CPU, memory, and disk-write snapshot for an active session */
  vitals?: SessionLiveVitals }
export type Wait = { type: "approve" | "ask" | "accept" | "propose" | "fail" | "confirm"; subjectId?: string; q: string; draft?: string; opts?: string[]; since: string; decision: string; subject: string; session: string | null; options: string[]; by: string; busy?: boolean
  recovery?: SendRecoveryInfo
  /** True when an unhandled @-mention or DM draft escalated to Inbox after 4 working hours (#213). */
  escalated?: boolean
  /** where it goes in 待处理 (the Hub's rules): only `decide` counts and reminds; drafts are one row per conversation */
  tier: DecisionTier; folded?: FoldReason; space?: string }
export type Done = { id: string; type: Wait["type"]; q: string; choice: string; tm: string; at: number; decision: string; revision: number; canUndo: boolean; undone?: boolean; how: "chose" | "dismissed" | "expired"; rel?: Rel
  /** Actor who settled the decision (a user id, "steve", "me", or undefined for system/expired). */
  resolvedBy?: string
  /** the client it was answered from (A4c `result.via`), when it was one signed in with a token */
  via?: string }
/** what became of an approved release (hub decision.result.outcome): the card says this, never a bare "approved" */
export type Rel = { tone: "run" | "fail" | "ok"; text: string; /** the Hub's outcome, for deciding (text is a translated label) */ outcome?: "releasing" | "not_released" | "failed" | "released"; /** the release decision it comes from */ key?: string }
/** an earlier plan: replaced (its open steps abandoned) or done (a new round followed) */
/** an output the bot delivered: kind, title, location, artifact id, when it was delivered, and whether the bot said it is
 * a record of the work (`record`) or the result (`result`); unsaid, its title decides. Last: an optional Hub delivery id. */
export type Out = [string, string, string | null, string, number?, ("record" | "result")?, (string | null)?]
export type PlanRound = { n: number; steps: [string, number][]; end: "done" | "replaced"; at: string }
export type Item = {
  id: string; t: string; b: string; st: "prop" | "run" | "wait" | "pause" | "done" | "drop" | "block" | "fail"; phase?: ItemView["state"]["kind"]; cycle?: number; deps?: string[]; src: [string, string]; ws: string; project: string | null; up: string; star?: boolean; acc: string
  /** the design gate (Q1): a delivery waits for the owner to confirm the design; off unless the owner turned it on */
  designGate?: boolean
  queued?: number
  goals: string[]; progress: string; planDone?: number; planTotal?: number; plan?: [string, number][]; rounds: PlanRound[]; wait?: Wait; waits: Wait[]; ss: string[]; tl: any[]; outs: Out[]; anns: RAnnotation[]; q: RQueued[]; qs: string | null; parent?: { item?: string; s?: string; at?: number } | null
  due?: string; pri?: "高" | "中" | "低"; arch?: boolean; snooze?: string; snoozeUntil?: number; tags?: string[]; branch: string | null; cwd: string | null; projNext?: boolean; detail: boolean; dependents: string[]; created: number; updated: number
  /** the last approved release of this item and what became of it */
  rel?: Rel
  dependencyWait?: RItemDetail["dependency_wait"]
  dispatch?: RItemDetail["dispatch"]
  dispatching?: boolean
  placement?: PlacementExplanation
}
export type Tag = { id: string; name: string; cat: string; desc: string; by: "Steve" | "你"; created: string }
export type ExecutionSurface = RExecutionSurface
export type Bot = { id: string; owner?: string | null; main?: boolean; n: string; a: string; role: string; specialties?: string[]; pers: string; pro: number; h: string; harness: string; m: string; model: string | null; fallbackSurfaces?: ExecutionSurface[]; on: boolean; acc: string; ws: string; project: string | null; perm: string; permMode: string; rule: string; prompt: string | null
  reasoningEffort?: ReasoningEffort | null; routines: { t: string; d: string; m: string; on: boolean; last: string }[]; hue: string; color: string; round: number; accessory?: BotAccessory | null; dev?: string; workspaces: string[] }
export type Draft = { mid: string; thought: string; text: string; /** when the reasoning began, and when the answer's text did (the reasoning is over) */ t0: number; t1?: number }
export type Msg = { id?: string; clientId?: string; k: "me" | "steve" | "thinking" | "digest"; t?: string; item?: string; fresh?: boolean; chat?: string; files?: RFile[]; cfgs?: any[]; created?: number; steps?: Steps; notice?: { kind: string; level: string; ref?: string; closed?: boolean }; cites?: Cite[]
  /** Steve's welcome that opened the first meeting */
  meet?: boolean
  /* thread routing (§3.4, §5.2): the offer under an answer, ask options, a user message waiting in the
   * queue, the message this one answers or quotes, flow-back from an item, a research cut off by a restart */
  offer?: Offer; ask?: { options: string[] }; queued?: { run?: string | null }; replyTo?: string; origin?: string; answer?: string
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
export type Offer = {
  title: string; goal: string; project?: string | null; bot?: string | null
  why?: string | null; reason?: PlacementReason | null; rejected?: PlacementRejection | null; tags?: string[]
  state: "open" | "taken" | "dismissed"; item?: string; folded?: boolean
}
/** a key moment of an item made from the conversation, posted back to it (§5.2) */
export type Update = { kind: "ask" | "reply" | "deliver" | "done" | "drop" | "fail"; item: string; text?: string; decision?: string; full?: boolean; ref?: string }
/** what Steve did for one message (hub meta.steps): routing, the reasoning behind a direct answer, a research run, tools; times in ms */
export type Steps = { route?: { kind: string; modelKind?: string; guard?: string | null; confidence?: number; title: string; bot: string; project: string | null; tags: string[]; newTags: string[]; why: string; model: string | null; fallback: boolean; ms: number; picked?: string }; answer?: { thought: string; model: string | null; ms: number; failed: boolean }
  research?: { run: string; model: string | null; ms: number; turns: number; tools: number; stopped?: "user" | "budget" | "violation" | "restart" | "failed" }
  plan?: ThreadPlanEntry[]
  thoughts?: ThreadLiveThought[]
  subtasks?: ThreadSubtask[]
  tools: { name: string; call?: number; /** the category the adapter gave the call (`read`, `exec`, …); absent on Steve's own steps */ kind?: string; description?: string; args: Record<string, any>; result?: Record<string, any>; excerpt?: string; status: "ok" | "failed"; error?: string; ms: number }[]; ms?: number }
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
/* fail: an item the Hub closed as failed (its cycle ended in failure) */
const ST_TONE = { prop: "accent", block: "neutral", run: "accent", wait: "orange", pause: "neutral", done: "green", drop: "neutral", fail: "red" } as const
export const ST: Record<string, [string, Tone]> = live(Object.keys(ST_TONE) as (keyof typeof ST_TONE)[], (k) => [t(`core:itemStatus.${k}`), ST_TONE[k]])
/** a closed item's status: done, dropped, or closed as failed (the Hub's outcome `failed`) */
export const closedSt = (s?: string) => s === "done" || s === "drop" || s === "fail"
const SS_TONE = { run: "accent", wait: "orange", idle: "neutral", ended: "neutral" } as const
export const SS: Record<string, [string, "accent" | "orange" | "green" | "neutral"]> = live(Object.keys(SS_TONE) as (keyof typeof SS_TONE)[], (k) => [t(`core:sessStatus.${k}`), SS_TONE[k]])
const WAIT_TYPES = ["approve", "ask", "accept", "propose", "fail", "confirm"] as const
export const WT: Record<string, string> = live(WAIT_TYPES, (k) => t(`core:waitType.${k}`))
/** what you do about a card of this type: the button that takes you to it */
export const WACT: Record<string, string> = live(WAIT_TYPES, (k) => t(`core:waitAction.${k}`))
/** what a card asks of you, in a line: `审批：推送到 main` */
/** the card asking you whether to merge a change no independent review passed (only you answer it) */
export const GITHUB_PR_MERGE = "gh_pr_merge"
export const waitLine = (w: Wait) => t("core:wait.line", { kind: w.subject === "tool_call" ? t("core:wait.tool") : w.subject === "release" ? t("core:wait.release") : w.subject === GITHUB_PR_MERGE ? t("core:wait.prMerge") : WT[w.type] ?? t("core:wait.other"), q: w.q })
/* the name a harness's runtime reports (an adapter of one's own included), when the Hub listed it */
export const runtimeName = (h: string): string | undefined => runtimes().find((r) => r.id === h)?.name ?? shell()?.harnessNames?.[h]
const harnessLabel = (h: string) => runtimeName(h) ?? h
/** the harness adapter a new bot runs on unless one is chosen: the workspace's main bot's if ready, else the first that is ready */
export const defaultHarness = (): string => {
  const rs = runtimes()
  const main = shell()?.bots?.find((b) => b.main)?.harness
  if (main && rs.some((r) => r.id === main && r.ok)) return main
  return (rs.find((r) => r.ok) ?? rs[0])?.id ?? ""
}
/** the bot modes `harness` can be set to (all while the Hub does not know) */
export const permModes = (harness: string): string[] => {
  const r = runtimes().find(r => r.id === harness)
  // Session-native auto is a workspace decision, not a Bot preference.
  const supported = PERMISSION_MODES.filter(k => canConfigurePermission(r, k))
  if (supported.length === 1 && supported[0] === "full_access") return supported
  const offered = supported.filter(k => k === "plan" || k === "ask")
  return offered.length ? offered : ["ask"]
}
/** a compatible permission mode for `harness`, in priority order */
export const defaultPermMode = (harness: string): string => {
  const ms = permModes(harness)
  return (["ask", "plan", "full_access"] as const).find((k) => ms.includes(k)) ?? ms[0] ?? "ask"
}
/** the reasoning efforts `harness` takes, as its adapter reported them */
export const harnessEfforts = (harness: string): string[] => runtimes().find((r) => r.id === harness)?.efforts ?? []
/** the 5-level canonical effort mapping `harness` reported for its default model */
export const harnessEffortMap = (harness: string): EffortMapping[] | undefined => runtimes().find((r) => r.id === harness)?.effort_map
/** whether `harness` reads a Bot's skills, as its adapter declares (undefined: the Hub did not say) */
export const harnessSkills = (harness: string): boolean | undefined => runtimes().find((r) => r.id === harness)?.skills
/** acceptance modes by their Hub value; the view model carries the label (Item.acc, Bot.acc), accKey turns it back */
export const ACC: Record<string, string> = live(["inherit", "manual", "auto"] as const, (k) => t(`core:acc.${k}`))
export const PERM: Record<string, string> = live(PERMISSION_MODES, (k) => t(`core:perm.${k}`))
/* a label in the current language, or the Hub value itself */
export const accKey = (label: string) => Object.keys(ACC).find((k) => ACC[k] === label) ?? (label in ACC ? label : "inherit")

/* ───────── time labels ───────── */
export const tm = (ts: number | null | undefined) => pastTime(ts)
const since = (ts: number) => span(Date.now() - ts)
const dur = (a: number, b: number) => elapsed(b - a)

/** the web build this page was loaded from: the release packager writes it into index.html; "dev" under vite */
export const LOADED_WEB_VERSION = document.querySelector('meta[name="steve-web-version"]')?.getAttribute("content") || "dev"

/* ───────── the store ───────── */
export const st = {
  ready: false, error: "" as string, login: null as string | null,
  /* the live stream has been broken for a while: what is on screen may be old, and nothing sent will arrive */
  down: false, version: "", webVersion: "", update: null as StateUpdate | null,
  modules: null as Shell["modules"] | null,
  me: null as MemberIdentity | null, members: [] as MemberIdentity[],
  workspace: null as Shell["workspace"] | null, workspaces: [] as Shell["workspaces"],
  bots: {} as Record<string, Bot>, allBots: [] as Shell["allBots"], projects: [] as Project[],
  tags: [] as Tag[],
  cfg: {} as Record<string, any>, settings: {} as Record<string, string>,
  localSessions: [] as LocalSession[], hold: null as RHold | null, localNodeUpdating: null as RLocalNodeUpdating | null, onboard: -1, /** the user is the instance's owner */ owner: false, prefix: "ST",
  noticeSeen: 0,
  dismissedAlerts: [] as string[],
  quote: null as string | null, focus: null as { item: string; until: number } | null,
  pendingActions: new Set<string>(),
  /** An accepted channel recovery must not be submitted again while state readback is stale or unavailable. */
  submittedChannelDecisions: new Set<string>(),
  /** The channel record says this old card can no longer recover a send (for example it is pending elsewhere). */
  inactiveChannelDecisions: new Set<string>(),
  ver: 0,
}
const shellKey = () => ["shell", scopeKey()] as const
const shell = () => qc.getQueryData<Shell>(shellKey())
const runtimeKey = ["runtimes"] as const
const emptyRuntimes: RRuntime[] = []
export const runtimes = () => qc.getQueryData<RRuntime[]>(runtimeKey) ?? emptyRuntimes
export function useRuntimesQuery(enabled = true) {
  return useQuery({ queryKey: runtimeKey, queryFn: () => api.runtimes(), staleTime: 5 * 60_000, enabled }, qc)
}
export function useRuntimes(enabled = true) {
  return useRuntimesQuery(enabled).data ?? emptyRuntimes
}
export function setRuntimes(next: RRuntime[]) { qc.setQueryData(runtimeKey, next) }
const deviceKey = ["device-inventory"] as const
const emptyDevices: RDevice[] = []
export function useDeviceInventory(enabled = true) {
  return useQuery({ queryKey: deviceKey, queryFn: async () => (await api.devices()).devices, enabled }, qc).data ?? emptyDevices
}
const noticeKey = () => ["notifications", scopeKey()] as const
const noticeQuery = () => ({ queryKey: noticeKey(), queryFn: () => api.notifications() })
export const noticeList = () => qc.getQueryData<RNotice[]>(noticeKey()) ?? []
export function useNotices(enabled = true) {
  return useQuery({ ...noticeQuery(), enabled }, qc).data
}
const tagUsesKey = () => ["tag-uses", scopeKey()] as const
export function useTagUses() {
  return useQuery({ queryKey: tagUsesKey(), queryFn: () => api.tags() }, qc).data
}

/* A channel submission receipt is not a provider receipt. Keep all views locked until the decision's view says it is
 * no longer open (a patch at least as new as the answer). */
const channelAnswers = new Map<string, { result: DecisionAnswer }>()
export const channelAnswer = (w: Wait) => w.subject === "chan_message" ? channelAnswers.get(w.decision)?.result : undefined
/** whether the views say decision `id` is no longer open */
const closedNow = (id: string) => decisionView(id)?.status !== "open"
function unlockClosed() {
  for (const id of channelAnswers.keys()) if (closedNow(id)) channelAnswers.delete(id)
  for (const locked of [st.submittedChannelDecisions, st.inactiveChannelDecisions]) for (const id of locked) if (closedNow(id)) locked.delete(id)
}
;(window as any).__steve = st

const ls = new Set<() => void>()
const shellLs = new Set<() => void>()
let shellVersion = 0
const publishStore = () => { st.ver++; ls.forEach((f) => f()) }
export function emit() {
  st.ver++
  shellVersion = st.ver
  shellLs.forEach((f) => f())
  ls.forEach((f) => f())
}
const onStore = (cb: () => void) => { ls.add(cb); return () => ls.delete(cb) }
const onShell = (cb: () => void) => { shellLs.add(cb); return () => shellLs.delete(cb) }
export function useStoreSelector<T>(read: () => T) { return useSyncExternalStore(onStore, read) }
export function useStore() { return useStoreSelector(() => st.ver) }
export function useShellSelector<T>(read: () => T) { return useSyncExternalStore(onShell, read) }
export function useShellStore() { return useShellSelector(() => shellVersion) }
export function toast(t: string, opts?: ToastOptions) { notify(t, opts) }
/** Tell the instance manager once per version (per browser) that a newer Steve can be installed from Settings · 关于. */
export function announceUpdate(update: StateUpdate | null) {
  if (!update?.owner) return
  const key = "steve-update-announced"
  try { if (localStorage.getItem(key) === update.version) return; localStorage.setItem(key, update.version) } catch { return }
  toast(t("core:toast.updateAvailable", { version: update.version }), { action: { label: t("core:toast.updateView"), onClick: () => { location.hash = "#/settings/about" } } })
}
const fail = (e: unknown) => { if (e instanceof Unauthenticated) { st.login = e.login ?? "/auth/login"; emit(); return } toast((e as Error).message || t("core:error.generic")) }
async function call<T>(p: Promise<T>, ok?: string, after?: () => Promise<void>): Promise<T | undefined> {
  try { const r = await p; if (ok) toast(ok); if (after) await after(); return r } catch (e) { fail(e); return undefined }
}
/** Configuration commands that immediately navigate to or render their new catalogue entry. */
const shellCall = <T,>(p: Promise<T>, ok?: string) => call(p, ok, refresh)
async function viewCall<T>(kind: ViewKind, id: string | ((answer: T) => string), p: Promise<T>, ok?: string) {
  try {
    const r = await p
    await settle(r, kind, [typeof id === "string" ? id : id(r)])
    if (ok) toast(ok)
    return r
  } catch (e) { fail(e); return undefined }
}
const itemCall = <T,>(id: string, p: Promise<T>, ok?: string) => viewCall("item", id, p, ok)
const sessionCall = <T,>(id: string, p: Promise<T>, ok?: string) => viewCall("session", id, p, ok)
/* ───────── mapping ───────── */
function botOf(b: RBot): Bot {
  const proj = shell()?.projects.find((p) => p.id === b.default_project)
  return { id: b.id, owner: b.owner ?? null, main: b.main, n: b.name, a: b.name[0] ?? "B", role: b.role, specialties: b.specialties ?? [], pers: b.persona, pro: b.proactive, h: harnessLabel(b.harness), harness: b.harness, m: b.model ?? t("core:defaultModel"), model: b.model, reasoningEffort: b.reasoning_effort, fallbackSurfaces: b.fallback_surfaces ?? [],
    on: b.enabled, acc: ACC[b.accept_mode] ?? ACC.inherit, ws: proj?.name ?? t("core:projectByItem"), project: b.default_project, perm: PERM[b.perm_mode] ?? b.perm_mode, permMode: b.perm_mode, rule: b.rules, prompt: b.prompt,
    routines: [], hue: b.avatar.color ?? defaultColor(b.id), color: b.avatar.color ?? defaultColor(b.id), round: b.avatar.round ?? defaultRound(b.id), accessory: b.avatar.accessory ?? null, dev: b.device ?? undefined, workspaces: b.workspaces }
}
const AVATAR_HUES = [["#7aa7c7", "mist"], ["#8c7ae6", "lavender"], ["#5fb39a", "teal"], ["#e0a458", "amber"], ["#d9777f", "coral"], ["#6c7a89", "slate"], ["#3d4a5c", "navy"], ["#b08968", "walnut"]] as const
const DEFAULT_AVATAR_ROUNDS = [15, 45, 70, 95] as const
/** [color, its name] */
export const AVATAR_COLORS: [string, string][] = AVATAR_HUES.map(([c, k]) => liveList<[string, string]>([c, ""], 1, () => t(`core:avatarColor.${k}`)))
const hash = (s: string) => { let h = 2166136261; for (const ch of s) { h ^= ch.codePointAt(0)!; h = Math.imul(h, 16777619) } return h >>> 0 }
export const defaultColor = (id: string) => AVATAR_COLORS[hash(id) % AVATAR_COLORS.length][0]
export const defaultRound = (id: string) => DEFAULT_AVATAR_ROUNDS[(hash(id) >>> 3) % DEFAULT_AVATAR_ROUNDS.length]

/** what Steve's own tools are called; one Steve has no word for goes by its name */
export const STEVE_TOOL: Record<string, string> = live(["steve_contacts","steve_github_read","steve_mail_read","steve_matters","steve_tools_search","steve_tool_call","steve_person_holdings","steve_digest_batch","steve_digest_read","steve_digest_chunk","steve_digest_finish","steve_digest_control","steve_digest_report","steve_matters_search","steve_matter_read","steve_todo_list","steve_todo_add","steve_todo_update","steve_todo_complete", "steve_update", "steve_ask", "steve_request_approval", "steve_deliver", "steve_deliver_file", "steve_publish", "steve_lane_status", "steve_item_read", "steve_items_search", "steve_item_dependencies", "steve_knowledge_search", "steve_knowledge_read", "steve_knowledge_get", "steve_debug", "steve_devices", "steve_reply_source", "steve_event_get", "steve_sessions_list", "steve_remember", "steve_memory_list", "steve_memory_tidy", "steve_note", "steve_whois", "steve_contacts_find", "steve_contacts_search", "steve_contact_get", "steve_contacts_remember", "steve_im_read", "steve_mail_thread", "steve_mail_attachment", "steve_mail_draft", "steve_mail_send", "steve_github_status", "steve_github_ci_log", "steve_github_comment", "steve_github_open_pr", "steve_secret_list", "steve_secret_use", "steve_steward_propose", "steve_upgrade"] as const, (k) => t(`core:steveTool.${k}`))
/** Unfolded device evidence; the general conversation view intentionally has coarser states. */
export const deviceWorkEvidence = () => ({ loaded: !!viewsCursor(), sessions: sessionViews(), decisions: openDecisions() })
export function deviceDecision(id: string) {
  const d = openDecisions().find(d => d.id === id)
  if (!d) return undefined
  const i = d.item_id ? getItem(d.item_id) : undefined
  return { i: i ?? orphanOf(d).i, w: waitOf(d) }
}
const emptyQueue: RQueued[] = []
const sessionModels = new WeakMap<SessionView, Sess>()
export function sessOf(s: SessionView): Sess {
  const existing = sessionModels.get(s)
  if (existing) return existing
  const running = s.status === "running" || s.status === "starting"
  const model: Sess = {
    id: s.id, n: s.no, item: s.item_id ?? null, b: s.bot_id ?? null,
    get h() { return s.harness ? harnessLabel(s.harness) : undefined },
    placement: s.placement, harness: s.harness, nativeId: s.harness_session_id ?? null, cwd: s.cwd, m: s.model,
    get st() { return running ? "run" : s.status === "ended" ? "ended" : decisionsForSession(s.id).length ? "wait" : "idle" },
    title: s.title, get dur() { return dur(s.created, s.updated) }, reasoningEffort: s.reasoning_effort ?? null, effortMapping: s.effort_mapping ?? null,
    contextKnown: s.used != null && s.size != null && s.size > 0, ctxUsed: s.used, ctxSize: s.size,
    ctx: s.used && s.size ? Math.round((s.used / s.size) * 100) : 0, src: s.source ?? "", get up() { return tm(s.updated) }, at: s.updated,
    get queue() { return qc.getQueryData<RQueued[]>(queueKey(s.id)) ?? emptyQueue },
    ro: s.readonly, device: s.device_id ?? "", node: s.node_id, queued: s.queued ?? 0,
    busy: s.busy ?? false, room: running ? s.waiting ?? null : null, starting: s.status === "starting", since: s.created,
    tokens: s.tokens, costUsd: s.cost_usd, vitals: s.vitals,
  }
  sessionModels.set(s, model)
  return model
}

export const getSession = (id: string) => { const s = sessionView(id); return s ? sessOf(s) : undefined }
export const getSessions = () => sessionViews().map(sessOf)
export function useSession(id: string | null | undefined) {
  const s = useSessionView(id)
  useViewGroup(id ? `decision:session:${id}` : null)
  const scope = scopeKey()
  useEffect(() => { if (id && !sessionView(id) && !table().tomb.has(`session:${id}`)) void readViews("session", [id]).catch(fail) }, [id, scope])
  return s ? sessOf(s) : undefined
}

const queueKey = (id: string) => ["detail", scopeKey(), "queue", id] as const
export function useSessionQueue(id?: string | null) {
  useQuery({ queryKey: queueKey(id ?? ""), queryFn: () => api.queued(id!), enabled: !!id }, qc)
}

/** A decision's recorded result: what it was settled with (opaque on the wire). */
const resultOf = (d: { result?: unknown }): Record<string, any> => (d.result && typeof d.result === "object" ? d.result as Record<string, any> : {})
/** Opened from inside a turn that is still going: the answer reaches the agent once it stops (tool permissions are not:
 * the agent waits on those). */
const agentBusy = (d: DecisionView) => !!d.session_id && d.subject_type !== "tool_call" && ["ask", "approve", "accept", "confirm"].includes(d.kind)
  && !!sessionView(d.session_id)?.busy
let waitsByView = new WeakMap<DecisionView, Wait>()
function waitOf(d: DecisionView): Wait {
  const cached = waitsByView.get(d)
  if (cached) return cached
  const w: Wait = { type: d.kind as Wait["type"], get q() { return decisionText(d.question) }, draft: d.draft, opts: d.kind === "ask" ? d.options : undefined, since: since(d.created), decision: d.id, subject: d.subject_type, subjectId: d.subject_id, session: d.session_id, options: d.options, by: d.requested_by, get busy() { return agentBusy(d) }, tier: d.tier, folded: d.folded ?? undefined, space: d.space_id ?? undefined,
    recovery: d.subject_type === "chan_message" && d.kind === "fail" ? resultOf(d).recovery : undefined,
    escalated: d.subject_type === "chan_message" && d.kind === "approve" ? resultOf(d).escalated === true : undefined }
  waitsByView.set(d, w)
  return w
}
/** A decision, open or settled in the last days, including those of sessions without an item. */
export function permissionDecision(id?: string) {
  if (!id) return undefined
  const view = decisionView(id)
  return view?.status === "open" ? view : settled().find((d) => d.id === id) ?? view
}
/** a stand-in item for a decision without one: id "" marks it; the title and bot come from its session */
let orphansByView = new WeakMap<DecisionView, Orphan>()
function orphanOf(d: DecisionView): Orphan {
  const cached = orphansByView.get(d)
  if (cached) return cached
  const s = d.session_id ? sessionView(d.session_id) : undefined, w = waitOf(d)
  const i = { id: "", t: s?.title || t("core:orphan.title"), b: s?.bot_id ?? d.requested_by, st: "wait", waits: [w], wait: w, ss: s ? [s.id] : [], tags: [], deps: [], tl: [], outs: [], anns: [], q: [], goals: [], rounds: [], now: "", src: [t("core:orphan.source"), "session"], ws: "—", project: null, up: "", acc: ACC.inherit, branch: null, cwd: null, detail: false, dependents: [], created: d.created, updated: d.created } as unknown as Item
  const card = { i, w }; orphansByView.set(d, card); return card
}
/* what was chosen on a module card (signals, flows, vault, connections); a flow_step choice is its own label */
const MODULE_CHOICE_SUBJECTS = new Set(["signal", "flow_step", "flow_run", "secret_use", "connection", "mail_draft"])
const MODULE_CHOICE: Record<string, string> = live(["done", "to_item", "retry", "drop", "continue_once", "stop", "once", "grant_1h", "grant_8h", "grant_24h", "deny", "pause", "reauth", "send", "no"] as const, (k) => t(`core:moduleChoice.${k}`))
/** an option's label: the label of a module's option id (decision-options.ts), else the option itself, already words */
export const optionLabel = (subject: string, option: string) => { const k = optionKey(subject, option); return k ? t(k as "core:option.chan_message.done") : option }
/** an approved release's outcome; undefined for anything else (and for approvals recorded before outcomes existed) */
export function relOf(d: { kind: string; subject_type: string; status: string; result?: unknown }): Rel | undefined {
  const r = resultOf(d)
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
/** The item's latest release decision, from the current projection and any older item history. */
function itemRel(id: string, details: DecisionView[] = []): Rel | undefined {
  const history = settled()
  if (history !== releaseHistory) {
    releaseHistory = history
    releases = new Map()
    for (const d of history) {
      if (!d.item_id || !isRelease(d)) continue
      const previous = releases.get(d.item_id)
      if (!previous || d.created > previous.created) releases.set(d.item_id, d)
    }
  }
  let last = releases.get(id)
  for (const d of details) {
    if (isRelease(d) && (!last || d.created > last.created)) last = d
  }
  const r = last && relOf(last)
  return r && { ...r, key: last!.id }
}
const isRelease = (d: DecisionView) => d.kind === "approve" && d.subject_type === "release"
let releaseHistory: DecisionView[] | undefined, releases = new Map<string, DecisionView>()
/* ── alerts that are over ──
 * a failure asks for attention only while it is current: a later release took its place (only the newest counts), the item
 * went on or finished, or you closed it. Closed ones are remembered by the Hub in `thread_notices` (returned in
 * `shell.dismissedAlerts`), so a refresh or another device does not bring them back.
 * The failure itself stays in the item's timeline and in the notifications. */
export function alertClosed(key?: string) {
  return !!key && st.dismissedAlerts.includes(key)
}
/** the failed release still worth showing on the item, or undefined */
function liveRel(i: ItemView, r?: Rel): Rel | undefined {
  if (r?.tone !== "fail") return r
  if (i.state.kind === "closed" || alertClosed(r.key)) return undefined
  /* closed from the notifications: the item's newest release notice (they come newest first) */
  const n = noticeList().find((x) => x.item_id === i.id && x.kind === "release")
  return n?.level === "fail" && n.closed ? undefined : r
}
/** close alerts by hand: decision ids, notification ids or their lines in the conversation */
export function closeAlerts(keys: string[]) {
  const s = shell(), xs = keys.filter(Boolean); if (!s || !xs.length) return
  const was = s.dismissedAlerts ?? []
  const dismissedAlerts = [...new Set([...was, ...xs])]
  qc.setQueryData(shellKey(), { ...s, dismissedAlerts })
  applyShell({ ...s, dismissedAlerts })
  qc.setQueryData(noticeKey(), noticeList().map((n) => xs.includes(n.id) ? { ...n, closed: true } : n))
  emit()
  void call(Promise.all(xs.map((x) => api.dismissNotice(x))))
}
/** close an item's failed release: the decision, and its notifications still open */
export async function closeRel(i: Item) {
  const scope = scopeKey(), key = i.rel?.key ?? ""
  try {
    const rows = await qc.fetchQuery(noticeQuery())
    if (scope !== scopeKey()) return
    closeAlerts([key, ...rows.filter((n) => n.item_id === i.id && n.kind === "release" && n.level === "fail" && !n.closed).map((n) => n.id)])
  } catch (error) { fail(error) }
}
function choiceLabel(d: DecisionView) {
  const r = resultOf(d)
  if (r.dismissed) return t("core:choice.dismissed")
  if (d.status === "expired") return String(r.superseded ?? (r.dropped ? t("core:choice.itemDropped") : r.reassigned ? t("core:choice.reassigned") : t("core:choice.expired")))
  if (d.kind === "accept") return r.choice === "pass" ? t("core:choice.pass") : t("core:choice.rework", { text: r.text ?? "" })
  if (d.subject_type === "space") return (r.choice ?? r.answer) === "mute" ? t("core:choice.mute") : t("core:choice.setup")
  if (optionKey(d.subject_type, r.choice)) return optionLabel(d.subject_type, r.choice)
  if (d.subject_type === "no_executor") {
    if (r.choice === "later") return t("core:choice.noExecutorLater")
    if (r.choice === "confirm_proposal") return t("core:choice.noExecutorProposal", { bot: botName(r.bot ?? null) })
    if (r.choice === "assign_bot") return t("core:choice.noExecutorAssign", { bot: botName(r.bot ?? null) })
  }
  if (d.kind === "ask") return String(r.answer ?? "")
  if (d.subject_type === "party_merge") return r.choice === "merge" ? t("core:choice.merge") : t("core:choice.notSame")
  if (d.kind === "confirm") return r.choice === "yes" ? t("core:choice.confirmYes") : t("core:choice.confirmNo")
  if (d.kind === "propose") return r.choice === "yes" ? t("core:choice.proposeYes") : t("core:choice.proposeNo")
  if (MODULE_CHOICE_SUBJECTS.has(d.subject_type)) return MODULE_CHOICE[r.choice] ?? String(r.choice ?? t("core:choice.handled"))
  if (d.subject_type === "session_start" && r.choice === "retry") return t("core:card.retrySession")
  if (d.kind === "fail") {
    if (typeof r.choice === "string" && r.choice.startsWith("bot:")) return t("core:fail.handoffBot", { bot: botName(r.choice.slice(4)) })
    return FAIL_CHOICE[r.choice] ?? r.choice
  }
  if (d.subject_type === "tool_call") return r.optionId ? t("core:choice.allow") : t("core:choice.deny")
  if (d.subject_type === "release") return r.approved ? t("core:choice.release") : t("core:choice.noRelease")
  return r.approved ? t("core:choice.approve") : t("core:choice.deny")
}
/** the choices on a card about something that went wrong, by the Hub's choice id */
export const FAIL_CHOICE: Record<string, string> = live(["wait", "continue", "delivered", "look", "tosteve", "drop", "release_waive", "release_retry", "retry", "release_live"] as const, (k) => t(`core:fail.${k}`))
/* ── which agent turn answered a comment in the work log ──
 * a comment (timeline kind "me") becomes a user prompt in one of the item's sessions: "补充信息：…", "用户的回答：…", a merged queue…
 * the first unclaimed prompt sent at or after the comment that contains its text is the one; the reply is that turn */
export type Reply = { s: string; from: number; turn: number; text: string; tools: number; live: boolean; end: number }
function repliesOf(timeline: RItemDetail["timeline"], sids: string[]): Map<number, Reply> {
  const out = new Map<number, Reply>()
  if (!timeline.some((line) => line.kind === "me")) return out
  const prompts = sids.flatMap((sid) => {
    const turns = getTranscript(sid)?.turns ?? []
    return turns.flatMap((turn) => {
      const e = turn.events[0]
      if (e.kind !== "user") return []
      const at = turn.events.findIndex((event) => event.kind !== "user")
      const prompts = turn.events.slice(0, at === -1 ? undefined : at)
      return [{ sid, turn, e, prompts, last: turn === turns.at(-1) }]
    })
  }).sort((a, b) => a.e.ts - b.e.ts || a.e.seq - b.e.seq)
  const used = new Set<number>()
  for (const line of timeline) {
    if (line.kind !== "me") continue
    const needle = promptNeedle(line.text); if (!needle) continue
    const match = prompts.find(({ e, prompts }) => !used.has(e.seq) && prompts.some((event) =>
      event.ts >= line.ts - 1000 && String(event.data?.text ?? "").includes(needle)))
    if (!match) continue
    const { sid, turn, e, last } = match
    used.add(e.seq)
    const at = turn.ev.findIndex((tuple) => tuple[0] !== "u" && tuple[0] !== "hi")
    const from = at < 0 ? Number.POSITIVE_INFINITY : turn.seqs[at]
    const text = turn.ev.filter((tuple) => tuple[0] === "a").map((tuple) => tuple[1]).join("").trim()
    const tools = turn.ev.filter((tuple) => tuple[0] === "tool").length
    const live = last && getSession(sid)?.st === "run" && !turn.ev.some((tuple) => tuple[0] === "end")
    out.set(line.id, { s: sid, from, turn: turn.start, text, tools, live, end: live ? Infinity : turn.events.at(-1)!.ts })
  }
  return out
}
/* a plan step as [text, state]: 0 open, 1 done, 2 left open when the item finished */
const steps = (p: PlanStep[]) => p.map((x) => [x.step, x.done ? 1 : x.left ? 2 : 0] as [string, number])
const sessionOrder = (a: SessionView, b: SessionView) => a.no - b.no || a.created - b.created || a.id.localeCompare(b.id)
const itemSessions = (id: string) => sessionsForItem(id).sort(sessionOrder)
export const itemRunning = (id: string) => itemWork(id).running

export function itemOf(i: ItemView, det?: RItemDetail): Item {
  if (!det) {
    const cached = itemModels.get(i)
    if (cached) return cached
  }
  const model = mapItem(i, det)
  if (!det) itemModels.set(i, model)
  return model
}
const itemModels = new WeakMap<ItemView, Item>()

function itemStatus(item: ItemView) {
  switch (item.state.kind) {
    case "closed":
      switch (item.state.outcome) {
        case "finished": return "done"
        case "cancelled": return "drop"
        case "failed": return "fail"
      }
    case "paused": return "pause"
    case "ready":
    case "active":
    case "closing":
      if (item.waits?.includes("prerequisite")) return "block"
      return decisionsForItem(item.id).length ? "wait" : "run"
  }
}

function mapItem(i: ItemView, det?: RItemDetail): Item {
  const snooze = () => i.snooze && i.snooze.until > Date.now() ? i.snooze : undefined
  const ss = () => itemSessions(i.id)
  const act = () => itemWork(i.id).active
  const visibleTimeline = det ? det.timeline.filter((t) => !t.kind.startsWith("deferred_")) : []
  const lines = det ? withDecisionNotes(visibleTimeline, det.decisions) : []
  const replies = det ? repliesOf(lines, ss().map((s) => s.id)) : new Map<number, Reply>()
  /* a session and a comment's reply show below the lines written while they ran (ST-42) */
  const started = (t: (typeof lines)[number]) => t.kind === "session" && t.session_id && t.text.startsWith("开始会话") ? t.session_id : null
  const rows = lines.map((t): FeedRow => {
    const sid = started(t), r = t.kind === "me" ? replies.get(t.id) : undefined
    if (sid) { const S = getSession(sid); return { ts: t.ts, until: S?.st === "run" || S?.st === "wait" ? Infinity : sessionView(sid)?.updated ?? t.ts } }
    return { ts: t.ts, until: r?.end, fence: t.kind === "me" }
  })
  const after = new Map(blockSlots(rows).flatMap((at, k) => at < 0 ? [] : [[at, k] as const])), tl: any[] = []
  lines.forEach((t, n) => {
    const sid = started(t)
    /* [8] is the moment itself, for the page's day headings; [9] is the Hub actor id */
    const row: any[] = sid ? ["sys", i18n.t("core:sessionStarted"), tm(t.ts)] : t.kind === "me" ? ["me", t.text, tm(t.ts), t.session_id ?? undefined, replies.get(t.id), t.files, t.via, t.id] : t.kind === "dispatch" ? ["dispatch", t.text, tm(t.ts)] : ["sys", t.text, tm(t.ts)]
    row[8] = t.ts
    row[9] = t.actor
    tl.push(row)
    const k = after.get(n); if (k == null) return
    const sk = started(lines[k]); tl.push(sk ? ["sess", sk] : ["reply", replies.get(lines[k].id)])
  })
  return {
    id: i.id, t: i.title, b: i.bot_id ?? "", get st() { return itemStatus(i) }, phase: i.state.kind, cycle: i.cycle, deps: i.deps ?? [], src: det?.source ?? [i.source, i.source], get ws() { return st.projects.find((p) => p.id === i.project_id)?.name ?? "—" }, project: i.project_id ?? null, get up() { return tm(i.updated) }, star: !!i.star, get acc() { return ACC[i.accept_mode ?? "inherit"] ?? ACC.inherit }, designGate: !!i.design_gate,
    goals: det?.goalsList.map((g) => g.text) ?? [], progress: det?.timeline.findLast((entry) => entry.kind === "progress")?.text ?? "", plan: det?.plan.length ? steps(det.plan) : undefined, planDone: i.plan_done, planTotal: i.plan_total,
    rounds: (det?.planRounds ?? []).map((r) => ({ n: r.round, steps: steps(r.steps), end: r.end, at: tm(r.created) })),
    dependencyWait: det?.dependency_wait,
    dispatch: det?.dispatch,
    dispatching: !!i.waits?.includes("dispatching"),
    placement: i.placement,
    get wait() { const d = decisionsForItem(i.id)[0]; return d ? waitOf(d) : undefined },
    get waits() { return itemWork(i.id).decisions.map(waitOf) }, get ss() { return ss().map((s) => s.id) }, tl, outs: det ? det.artifacts.map((a): Out => [a.kind, a.title, a.location, a.id, a.created, det.artifactRoles?.[a.id] as Out[5], a.delivery_id]) : [], anns: det?.annotations ?? [],
    get queued() { return itemWork(i.id).queued }, get q() { const a = act(); return a ? sessOf(a).queue : emptyQueue }, get qs() { return act()?.id ?? null },
    parent: i.parent?.item || i.parent?.session ? { item: i.parent.item ?? undefined, s: i.parent.session ?? undefined, at: i.parent.at ?? undefined } : null, due: i.due ?? undefined, pri: (i.priority as Item["pri"]) ?? "中", arch: i.archived, get snooze() { return snooze()?.label }, get snoozeUntil() { return snooze()?.until }, tags: i.tags,
    /* the hub clears cwd when the project changes; a session still running then is in the old directory */
    branch: det?.branch ?? null, cwd: det?.cwd ?? null, get projNext() { return !det?.cwd && !!act() }, detail: !!det, dependents: det?.dependents ?? [], created: i.created, updated: i.updated,
    /* an open release card wins (it is the newest); a settled one says what happened */
    get rel() {
      if (decisionsForItem(i.id).some((d) => d.subject_type === "release")) return undefined
      return liveRel(i, itemRel(i.id, det?.decisions))
    },
  }
}
/* A rerun keeps the question's origin: replies already on screen cannot finish its new draft. */
let priorDraftReplies = new Set<string>(), nextDraft = true
/* a reply stopped while it was being written was stored for a while (#301) with `steps: {}`: no tool list, which every
 * reader of steps assumes */
const stepsOf = (s: unknown): Steps | undefined => s && typeof s === "object" ? { ...(s as Steps), tools: Array.isArray((s as Steps).tools) ? (s as Steps).tools : [] } : undefined
export function msgOf(m: ConversationMessage): Msg {
  const x = m.meta ?? {}
  /* one drafted change (`proposal`) or several (`proposals`); `cfg_state` is what became of each */
  const ps: Proposal[] = Array.isArray(x.proposals) ? x.proposals : x.proposal ? [x.proposal] : []
  const cfgs = ps.map((p) => proposalOf(p, x.cfg_state?.[p.id] ?? (x.applied ? "applied" : x.dropped ? "dropped" : "draft")))
  return { id: m.id, k: m.author, t: m.text, item: m.item_id ?? undefined, chat: x.chat, files: x.files, created: m.created, steps: stepsOf(x.steps), notice: x.notice, cites: x.cites, cfgs: cfgs.length ? cfgs : undefined,
    held: !!x.held || undefined, stopped: !!x.stopped || undefined, meet: x.onboard === true || undefined,
    offer: x.offer ?? undefined, ask: x.ask ?? undefined, queued: x.queued ?? undefined, replyTo: x.reply_to ?? undefined, origin: x.origin ?? undefined, answer: x.answer ?? undefined,
    update: x.update ?? undefined, updates: Array.isArray(x.updates) ? x.updates : undefined, rerun: x.rerun ?? undefined,
    card: x.card?.item ? x.card : undefined, cardOnly: !!x.card_only, wasCard: !!x.was_card,
    a2ui: x.a2ui && typeof x.a2ui === "object" ? x.a2ui : undefined, a2uiAction: x.a2ui_action?.name ? x.a2ui_action : undefined }
}
const messages = new WeakMap<MessageView, Msg>()
export function msgOfView(view: MessageView): Msg {
  let m = messages.get(view)
  if (!m) {
    m = msgOf(view as ConversationMessage)
    messages.set(view, m)
  }
  return m
}
export const chatMessage = (id: string) => {
  const v = messageView(id)
  return v && msgOfView(v)
}
let queued: Msg[] = [], queuedVer = -1
export function chatQueue() {
  const ver = table().kindVersion("message")
  if (ver === queuedVer) return queued
  queuedVer = ver
  const rows = messageViews().map(msgOfView).filter((m) => m.held).sort((a, b) => (a.created ?? 0) - (b.created ?? 0) || a.id!.localeCompare(b.id!))
  if (rows.length !== queued.length || rows.some((m, i) => m !== queued[i])) queued = rows
  return queued
}
let completedResearchRuns = new Set<string>(), researchRunsVer = -1
function hasCompletedResearchRun(run: string): boolean {
  const ver = table().kindVersion("message")
  if (ver !== researchRunsVer) {
    researchRunsVer = ver
    completedResearchRuns = new Set()
    for (const v of messageViews()) {
      const r = msgOfView(v).steps?.research?.run
      if (r) completedResearchRuns.add(r)
    }
  }
  return completedResearchRuns.has(run)
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

function applyShell(s: Shell) {
  st.version = s.version; st.webVersion = s.web_version ?? ""; st.update = s.update ?? null; announceUpdate(st.update); st.workspace = s.workspace; st.workspaces = s.workspaces; st.prefix = s.workspace.item_prefix; st.projects = s.projects
  st.me = s.me ?? null
  st.members = s.members ?? []
  waitsByView = new WeakMap(); orphansByView = new WeakMap()
  st.bots = Object.fromEntries(s.bots.map((b) => [b.id, botOf(b)])); st.allBots = s.allBots
  st.tags = s.tags.map((t) => ({ id: t.id, name: t.name, cat: t.cat, desc: t.description, by: t.created_by === "你" ? "你" : "Steve", created: tm(t.created) }))
  st.noticeSeen = Math.max(st.noticeSeen, s.notifySeen ?? 0)
  st.dismissedAlerts = s.dismissedAlerts ?? []
  st.settings = s.settings
  st.modules = s.modules
  applyLook(s.settings)
  st.cfg = { ...s.settings, accDefault: ACC[s.settings.accDefault] ?? ACC.auto }
  st.owner = !!s.owner
  /* the tour shows on an empty instance until this browser finished it, or the owner met Steve or said no (the Hub remembers that) */
  st.ready = true
  maybeOnboard()
}
/* the views changed (a patch, a snapshot, a page): what is derived from them, and the locks they release */
function maybeOnboard() {
  if (st.ready && viewsCursor() && st.onboard < 0 && !messageViews().length && !itemViews().length && !st.settings.onboard && localStorage.getItem("steve-onboarded") !== "1") st.onboard = 0
}
onCatalog(() => { maybeOnboard(); unlockClosed(); publishStore() })
/* the view model holds labels (status words, "默认模型", transcript lines): map it again in the new language */
i18n.on("languageChanged", () => {
  const s = shell()
  if (s) { applyShell(s); void refresh() }
  emit()
  void qc.invalidateQueries({ queryKey: runtimeKey })
})

/* ───────── loading and live updates ───────── */
const fetchFailed = (e: unknown) => {
  if (e instanceof Unauthenticated) st.login = e.login ?? "/auth/login"
  else st.error = (e as Error).message
  emit()
}
export async function refresh(): Promise<void> {
  const scope = scopeKey()
  try {
    const key = shellKey()
    await qc.cancelQueries({ queryKey: key }, { silent: true })
    if (scope !== scopeKey()) return
    const s = await qc.fetchQuery({ queryKey: key, queryFn: ({ signal }) => api.shell(signal), staleTime: 0 })
    if (scope !== scopeKey()) return
    setPageWorkspace(s.workspace?.id ?? "")
    st.login = null; st.error = ""
    applyShell(s); emit()
  } catch (e) { if (scope === scopeKey() && !isCancelledError(e)) fetchFailed(e) }
}
async function loadSummary() {
  const scope = scopeKey(), live = chatLive(), hold = st.hold, updating = st.localNodeUpdating
  try {
    const summary = await api.summary()
    if (scope !== scopeKey()) return
    let changed = false
    if (st.hold === hold) { st.hold = summary.hold; changed = true }
    if (st.localNodeUpdating === updating) { st.localNodeUpdating = summary.local_node_updating ?? null; changed = true }
    if (changed) emit()
    if (chatLive() === live) {
      if (summary.live) {
        const snap = summary.live
        const restoredLive = isResearchSnapshot(snap) ? liveRunFromSnapshot(snap) : null
        const restoredDraft = (snap.draft_text || snap.draft_thought)
          ? {
              mid: snap.mid ?? snap.run,
              thought: snap.draft_thought ?? "",
              text: snap.draft_text ?? "",
              t0: snap.started_at,
              t1: snap.draft_text ? snap.last_activity_at : undefined,
            }
          : null
        updateChat({
          thinking: true,
          phase: snap.phase ?? "",
          thinkingStartedAt: snap.started_at,
          thinkingAt: snap.last_activity_at,
          thinkingStage: snap.stage,
          live: restoredLive,
          draft: restoredDraft,
        })
      } else if (live.thinking !== summary.thinking) {
        updateChat(
          summary.thinking
            ? { thinking: true, thinkingStartedAt: live.thinkingStartedAt ?? Date.now() }
            : { thinking: false, thinkingStartedAt: undefined, thinkingAt: undefined, thinkingStage: undefined, draft: null, live: null, phase: "" },
        )
      }
    }
  } catch (e) { if (scope === scopeKey()) fetchFailed(e) }
}
async function loadReadSide() {
  const scope = scopeKey()
  // The snapshot cursor must predate the shell and summary reads, so replay covers concurrent changes.
  await loadViews().catch((e) => { if (scope === scopeKey()) fetchFailed(e) })
  if (scope !== scopeKey() || st.login) return
  await Promise.all([refresh(), loadSummary()])
}
const detailKey = (id: string) => ["detail", scopeKey(), "item", id] as const
const itemDetail = (id: string) => qc.getQueryData<RItemDetail>(detailKey(id))
async function itemDetails(id: string, signal: AbortSignal) {
  const scope = scopeKey()
  const [detail] = await Promise.all([api.item(id, signal), readItemSessions([id]), itemView(id) ? Promise.resolve() : readViews("item", [id])])
  if (scope !== scopeKey() || signal.aborted) return detail
  const missing = detail.dependents.filter((x) => !itemView(x))
  const deps = itemView(id)?.deps?.filter((x) => !itemView(x)) ?? []
  if (missing.length || deps.length) void readViews("item", [...new Set([...missing, ...deps])]).catch(fail)
  return detail
}
export function useItemDetail(id: string, enabled = true) {
  const { data } = useQuery({ queryKey: detailKey(id), queryFn: ({ signal }) => itemDetails(id, signal), enabled }, qc)
  const v = useItemView(id)
  useViewGroup(`session:item:${id}`)
  useViewGroup(`decision:item:${id}`)
  const sessions = itemSessions(id)
  const latest = itemWork(id).active?.id ?? sessions.at(-1)?.id
  useTranscripts(sessions.map((s) => s.id), latest ? [latest] : [])
  return v ? itemOf(v, data) : undefined
}
export async function loadItem(id: string, force = false) {
  try {
    if (force) await qc.invalidateQueries({ queryKey: detailKey(id), refetchType: "none" })
    await qc.fetchQuery({ queryKey: detailKey(id), queryFn: ({ signal }) => itemDetails(id, signal) })
    emit()
  } catch (e) { fail(e) }
}
/* The module SSE types (steve-modules `sse::MODULE_TYPES`, plus `kb`): ids only, scoped to the workspace; the module
 * pages refetch what they show. */
export const MODULE_EVENTS = ["party", "mm", "changeset", "account", "connection", "vault", "signal", "flow", "run", "space", "chan_message", "persona", "digest", "channel", "gh", "mail", "calendar", "kb", "approval_policy", "stats"] as const
export type ModuleEvent = Extract<HubEvent, { type: (typeof MODULE_EVENTS)[number] }> | { type: "resync"; id: string }
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
function forgetResearchActivity() {
  const { live } = chatLive()
  if (live) updateChat({ live: { ...live, now: undefined, active: [], at: undefined } })
}

function onHub(e: HubEvent) {
  /* the Hub could not continue from our cursor (it restarted, or we were away past its window): refetch everything, once */
  if (e.type === "stream.reset" || e.type === "stream.gap") {
    // The research deltas are not replayed. Keep recorded results, but forget which call was active.
    forgetResearchActivity(); emit()
    if (e.type === "stream.reset") { resetTranscripts(); resetViews(); updateChat({}); void resync(); return }
  }
  /* Live transcript frames are caught up through the event read API. */
  if (e.type === "stream.gap") {
    void recoverTranscripts()
    if (chatLive().thinking) void loadSummary()
    return
  }
  hubSubs.forEach((f) => f(e))
  if (e.type === "patch") {
    const items = new Set<string>()
    for (const op of e.ops) {
      if (op.type === "item") items.add(op.id)
      else if (op.type !== "message") {
        const prior = op.type === "session" ? sessionView(op.id) : decisionView(op.id)
        if (prior?.item_id) items.add(prior.item_id)
        const next = op.view as SessionView | DecisionView | undefined
        if (next?.item_id) items.add(next.item_id)
        if (op.type === "session" && (prior as SessionView | undefined)?.queued !== (next as SessionView | undefined)?.queued)
          void qc.invalidateQueries({ queryKey: queueKey(op.id) })
      }
    }
    applyPatch(e)
    const { draft, live, phase } = chatLive()
    for (const op of e.ops) {
      if (op.type !== "message" || op.op !== "upsert") continue
      const m = chatMessage(op.id)
      if (m?.k === "me") {
        const { sending } = chatLive()
        const match = sending.find((s) => s.t === m.t && s.replyTo === m.replyTo)
        if (match) updateChat({ sending: sending.filter((s) => s !== match) })
      }
      if (m?.k !== "steve") continue
      if (draft && m.origin === draft.mid && !priorDraftReplies.has(op.id)) {
        updateChat({ drafted: op.id, draftedStart: draft.t0, draft: null })
      }
      if (live && m.origin === live.mid && m.steps?.research?.run === live.run) {
        updateChat({
          live: null,
          ...(phase === "research" || phase === "answer"
            ? { thinking: false, phase: "", thinkingStartedAt: undefined, thinkingAt: undefined, thinkingStage: undefined }
            : {}),
        })
      }
    }
    if (e.ops.some((op) => op.type === "item")) void qc.invalidateQueries({ queryKey: tagUsesKey() })
    for (const id of items) {
      const queryKey = detailKey(id)
      void qc.cancelQueries({ queryKey }, { silent: true }).then(() => qc.invalidateQueries({ queryKey }))
    }
    return
  }
  /* Only pages displaying clients subscribe to their presence. */
  if (e.type === "client") return
  if (e.type === "party" && st.me) {
    const pid = "id" in e ? e.id : undefined
    if (!st.me.party_id || !pid || st.me.party_id === pid || st.members.some((m) => m.party_id === pid)) {
      void refresh()
    }
  }
  if (moduleTypes.has(e.type)) return
  e = e as HubEvent
  if (e.type === "workspace" || e.type === "bot" || e.type === "project" || e.type === "tag" || e.type === "settings") {
    if (e.type === "bot") void qc.invalidateQueries({ queryKey: e.id === "*" ? botMemoriesKey() : botMemoryKey(e.id) })
    if (e.type === "tag") void qc.invalidateQueries({ queryKey: tagUsesKey() })
    if (e.type === "settings") void qc.invalidateQueries({ queryKey: noticeKey() })
    void refresh()
    return
  }
  if (e.type === "device" || e.type === "node") {
    void qc.invalidateQueries({ queryKey: deviceKey })
    void qc.invalidateQueries({ queryKey: runtimeKey })
    return
  }
  if (e.type === "release.hold") { st.hold = e.hold as RHold | null; emit(); return }
  if (e.type === "local_node.updating") { st.localNodeUpdating = e.updating as RLocalNodeUpdating | null; emit(); return }
  if (e.type === "steve.draft") {
    const cur = chatLive(), { draft } = cur
    if (nextDraft || draft?.mid !== e.mid) priorDraftReplies = new Set(messageViews().map(msgOfView).filter((m) => m.k === "steve" && m.origin === e.mid).map((m) => m.id!))
    nextDraft = false
    const was = draft?.mid === e.mid ? draft : undefined, now = Date.now()
    updateChat({
      draft: { mid: e.mid, thought: e.thought, text: e.text, t0: was?.t0 ?? now, t1: was?.t1 ?? (e.text ? now : undefined) },
      thinkingAt: now,
      thinkingStage: e.text ? "writing" : e.thought ? "thinking" : cur.thinkingStage,
    }, was && !!was.text === !!e.text ? "reply" : "frame")
    return
  }
  if (e.type === "steve.tool") {
    if (!hasCompletedResearchRun(e.run)) {
      const cur = chatLive()
      const nextLive = researchEvent(cur.live, e)
      updateChat({
        live: nextLive,
        thinkingStartedAt: nextLive.startedAt ?? cur.thinkingStartedAt,
        thinkingAt: e.at ?? nextLive.at ?? cur.thinkingAt,
      }, "frame")
    }
    return
  }
  if (e.type === "steve.progress") {
    if (!hasCompletedResearchRun(e.run)) {
      const cur = chatLive()
      const nextLive = researchProgressEvent(cur.live, e)
      updateChat({
        live: nextLive,
        thinkingStartedAt: nextLive.startedAt ?? cur.thinkingStartedAt,
        thinkingAt: e.at ?? nextLive.at ?? cur.thinkingAt,
      }, "frame")
    }
    return
  }
  if (e.type === "steve.thinking") {
    const cur = chatLive(), now = Date.now()
    if (e.on && !cur.thinking) nextDraft = true
    const staleLive = e.on && !cur.thinking && cur.live != null && (
      hasCompletedResearchRun(cur.live.run)
      || (e.started_at != null && cur.live.startedAt != null && e.started_at > cur.live.startedAt)
    )
    updateChat({
      thinking: e.on,
      phase: e.on ? e.phase ?? "" : "",
      thinkingStartedAt: e.on ? (e.started_at ?? cur.thinkingStartedAt ?? now) : undefined,
      thinkingAt: e.on ? (e.at ?? now) : undefined,
      thinkingStage: undefined,
      ...(staleLive ? { live: null } : {}),
    })
    cue(e.on ? "think" : "reply")
    if (!e.on) {
      const { draft, live } = chatLive(), scope = scopeKey()
      setTimeout(() => {
        if (scope !== scopeKey()) return
        const current = chatLive()
        if (!current.thinking && current.draft === draft && current.live === live) updateChat({ draft: null, live: null })
      }, 1500)
    }
    return
  }
  if (e.type === "event") {
    appendTranscript(e.session_id, { seq: e.seq, ts: Date.now(), kind: e.kind, data: e.data })
    return
  }
  if (e.type === "message" && e.space === "notify") {
    void qc.invalidateQueries({ queryKey: noticeKey() })
    void refresh()
  }
}
/* the stream is bound to the workspace it was opened in: reopen it after a switch */
let closeStream = () => {}
let resyncing: Promise<void> | null = null, resyncAgain = false
function resync(): Promise<void> {
  void recoverTranscripts()
  if (resyncing) { resyncAgain = true; return resyncing }
  resyncing = (async () => {
    do {
      resyncAgain = false
      const scope = scopeKey()
      hubSubs.forEach((f) => f({ type: "resync", id: "" }))
      await loadReadSide()
      if (scope !== scopeKey()) return
      await Promise.all([qc.invalidateQueries({ queryKey: ["detail", scopeKey()] }), qc.invalidateQueries({ queryKey: noticeKey() })])
      if (scope !== scopeKey()) return
    } while (resyncAgain)
  })().finally(() => { resyncing = null; if (resyncAgain) void resync() })
  return resyncing
}
/* Each stream captures the snapshot cursor it opens from. After a failed initial read, a
 * successful resync can reopen a stream with resumable frame IDs. */
let streamGen = 0
/** a stream that comes back within this is not worth a word */
const DOWN_AFTER_MS = 4000
let downTimer: ReturnType<typeof setTimeout> | null = null
const streamUp = () => {
  if (downTimer) { clearTimeout(downTimer); downTimer = null }
  if (st.down) { st.down = false; emit() }
}
const connect = () => {
  closeStream()
  const gen = ++streamGen, from = viewsCursor()
  closeStream = stream(onHub, (resumed) => {
    if (gen === streamGen) streamUp()
    if (resumed) {
      if (!viewsCursor()) void loadViews().catch(fail)
      return
    }
    void resync().then(() => { if (gen === streamGen && viewsCursor()) connect() })
  }, () => from, () => {
    if (gen !== streamGen || st.down || downTimer) return
    downTimer = setTimeout(() => { downTimer = null; if (gen === streamGen) { st.down = true; forgetResearchActivity(); emit() } }, DOWN_AFTER_MS)
  })
}
export function start() {
  let live = true
  void loadReadSide().then(() => { if (live && !st.login) connect() })
  return () => { live = false; streamGen++; closeStream(); closeStream = () => {}; streamUp() }
}

/* ───────── selectors ───────── */
export const getItem = (id: string) => { const v = itemView(id); return v ? itemOf(v, itemDetail(id)) : undefined }
export const it = (id: string) => getItem(id)!
const itemLists = new WeakMap<ItemView[], Item[]>()
export function getItems(): Item[] {
  const views = itemViews()
  let list = itemLists.get(views)
  if (!list) {
    list = views.map((v) => itemOf(v))
    itemLists.set(views, list)
  }
  return list
}
export function useItem(id: string | null | undefined) {
  const v = useItemView(id)
  useViewGroup(id ? `decision:item:${id}` : null)
  useViewGroup(id ? `session:item:${id}` : null)
  return v ? itemOf(v) : undefined
}
let runtimeScope = -1
const runtimeRequested = new Set<string>()
const runtimePending = new Set<string>()
function loadItemRuntime(id: string) {
  if (runtimeScope !== scopeKey()) { runtimeScope = scopeKey(); runtimeRequested.clear(); runtimePending.clear() }
  if (runtimeRequested.has(id)) return
  runtimeRequested.add(id)
  runtimePending.add(id)
  if (runtimePending.size !== 1) return
  const current = runtimeScope
  queueMicrotask(() => {
    if (scopeKey() !== current) return
    const ids = [...runtimePending]
    runtimePending.clear()
    void readItemSessions(ids).catch(() => ids.forEach((id) => scopeKey() === current && runtimeRequested.delete(id)))
  })
}

export function useItemRuntime(id: string) {
  const stable = useRef<SessionView[]>([])
  const depsKey = (itemView(id)?.deps ?? []).join(",")
  const subscribe = useCallback((cb: () => void) => {
    const offs = [subscribeItemScope(id, cb)]
    for (const dep of depsKey ? depsKey.split(",") : []) offs.push(onEntityView("item", dep, cb))
    return () => { for (const off of offs) off() }
  }, [id, depsKey])
  useSyncExternalStore(subscribe, () => {
    const t = table()
    const sessions = sessionsForItem(id)
    if (sessions.length !== stable.current.length || sessions.some((s, n) => s !== stable.current[n])) stable.current = sessions
    let ver = Math.max(t.groupVersion(`session:item:${id}`), t.groupVersion(`decision:item:${id}`), t.entityVersion("item", id))
    for (const dep of itemView(id)?.deps ?? []) {
      const dv = t.entityVersion("item", dep)
      if (dv > ver) ver = dv
    }
    return ver
  })
  const scope = scopeKey()
  const v = itemView(id)
  useEffect(() => {
    if (!id) return
    loadItemRuntime(id)
    if (!v) return
    const missing = (v.deps ?? []).filter((d) => !itemView(d) && !table().tomb.has(`item:${d}`))
    if (missing.length) void readViews("item", missing).catch(() => {})
  }, [id, scope, v])
  return stable.current.slice().sort((a, b) => b.created - a.created || b.no - a.no || a.id.localeCompare(b.id)).map(sessOf)
}

export const nextItemNumber = () => 1 + Math.max(0, ...itemViews().filter((v) => v.id.startsWith(st.prefix + "-")).map((v) => v.no))
export const pending = () => getItems().filter((i) => (i.st === "wait" || i.st === "prop" || !!i.wait) && !!i.wait && !i.snooze && !i.arch)
const itemOfDecision = (d: DecisionView) => d.item_id ? getItem(d.item_id) : undefined
const cardFrom = (d: DecisionView, i = itemOfDecision(d)): Orphan => i ? { i, w: waitOf(d) } : orphanOf(d)
export const orphans = (): Orphan[] => openDecisions().filter((d) => !itemOfDecision(d)).map(orphanOf)
const allCards = (decisions = openDecisions()): Orphan[] => decisions.flatMap((d) => {
  const i = itemOfDecision(d)
  return i?.arch || i?.snooze ? [] : [cardFrom(d, i)]
})
/** a memory to confirm: it blocks nothing, so it is counted on one line of 待处理, not shown as a card */
export const isMemCard = (w: Wait) => w.type === "confirm" && w.subject === "mem"
/** every open card that holds work, several per item when it has more than one */
export const pendingCards = (decisions?: DecisionView[]): Orphan[] => allCards(decisions).filter((c) => !isMemCard(c.w))
/** the open cards that ask you (tier `decide`): what the badge counts and what reminds you */
export const decideCards = (views = openDecisions()): Orphan[] => pendingCards(views).filter((c) => c.w.tier === "decide")
export const decisionBadgeCount = () => {
  const now = Date.now()
  return openDecisions().filter((d) => {
    const item = d.item_id ? itemView(d.item_id) : undefined
    return d.tier === "decide" && !item?.archived && (item?.snooze?.until ?? 0) <= now
  }).length
}
const onDecisionBadge = (cb: () => void) => onViewKinds(["decision", "item"], cb)
export const useDecisionBadgeCount = () => useSyncExternalStore(onDecisionBadge, decisionBadgeCount)
/** the memories waiting to be confirmed (their batch page is 知识 › 待确认) */
export const memCards = (decisions?: DecisionView[]): Orphan[] => allCards(decisions).filter((c) => isMemCard(c.w))
/** the open cards a session is waiting on, with or without an item */
export const sessionCards = (sid: string): Orphan[] => decisionsForSession(sid).map((d) => cardFrom(d))
/** "" is the stand-in id of a card without an item */
function cardOf(id: string, decision?: string): { i?: Item; w?: Wait } {
  if (decision) {
    const d = decisionView(decision)
    return d?.status === "open" ? cardFrom(d) : {}
  }
  const i = it(id)
  return { i, w: i?.wait }
}
export const snoozed = () => getItems().filter((i) => !!i.wait && !!i.snooze)
export const active = (i: Item) => i.qs ? getSession(i.qs) : undefined
export const sidOf = (s: Sess) => s.id
export const accOf = (i: Item) => (i.acc !== ACC.inherit ? i.acc : st.bots[i.b] && st.bots[i.b].acc !== ACC.inherit ? st.bots[i.b].acc : st.cfg.accDefault)
export function notePersonIdentity(c: { id?: string; name?: string; avatar?: unknown; is_me?: boolean }) {
  let changed = false
  const av = c.avatar && typeof c.avatar === "object" ? (c.avatar as MemberIdentity["avatar"]) : undefined
  if (st.me && (c.is_me || (c.id && st.me.party_id === c.id))) {
    const next: MemberIdentity = {
      ...st.me,
      party_id: c.id ?? st.me.party_id,
      name: c.name?.trim() ? c.name : st.me.name,
      avatar: av ?? st.me.avatar,
    }
    if (next.name !== st.me.name || next.party_id !== st.me.party_id || JSON.stringify(next.avatar) !== JSON.stringify(st.me.avatar)) {
      st.me = next
      changed = true
    }
  }
  if (c.id && st.members.length) {
    let hit = false
    const nextMembers = st.members.map((m) => {
      if (m.party_id !== c.id && !(c.is_me && st.me && m.user_id === st.me.user_id)) return m
      const next: MemberIdentity = {
        ...m,
        party_id: c.id ?? m.party_id,
        name: c.name?.trim() ? c.name : m.name,
        avatar: av ?? m.avatar,
      }
      if (next.name !== m.name || next.party_id !== m.party_id || JSON.stringify(next.avatar) !== JSON.stringify(m.avatar)) {
        hit = true
        return next
      }
      return m
    })
    if (hit) { st.members = nextMembers; changed = true }
  }
  if (changed) emit()
}

export function clearStoreMe() {
  st.me = null
  st.members = []
}

const BOT_SUFFIX_RE = /\bbot$/i
/** Format a Bot's display name without doubling a trailing "Bot" suffix (e.g. "后端 Bot" stays "后端 Bot"). */
export const formatBotName = (name: string, main?: boolean): string => {
  const trimmed = name.trim()
  if (!trimmed || main || BOT_SUFFIX_RE.test(trimmed)) return trimmed
  return t("manage:bot.name", { name: trimmed })
}
export const botName = (k: string | null) => (!k ? "" : st.bots[k] ? formatBotName(st.bots[k].n, st.bots[k].main) : k === "agent" ? "Agent" : k)
export const hm = (s: Sess) => t("core:harnessModel", { harness: s.h ?? t("core:unknownHarness"), model: s.m ?? t("core:unknownModel") })
export const isThinking = () => chatLive().thinking
export const projectName = (id: string | null | undefined) => st.projects.find((p) => p.id === id)?.name ?? t("common:state.none")
export const depsOpen = (i: Item) => (i.deps || []).filter((d) => getItem(d)?.st !== "done")

/* ───────── actions: conversation ───────── */
/** how long the conversation keeps talking to an item after the last message sent to it (§5.3) */
const FOCUS_MS = 10 * 60_000
/** `clientId`: the send idempotency key (the composer's draft keeps one across retries; a button sends a new one) */
export async function chatSend(v: string, files: string[] = [], refs: RFile[] = [], mentions: { kind: "party" | "bot"; id: string }[] = [], clientId = newClientId()) {
  /* Only an explicit quote or focus selects an item. A textual reference never starts a focus. */
  const quoted = st.quote ? chatMessage(st.quote) : undefined
  const focus = focusItem() ?? undefined
  const item = quoted?.item ?? (quoted ? undefined : focus)
  const to = { item, reply_to: quoted?.id, client_id: clientId }
  st.quote = null
  st.focus = item && !closedSt(it(item)?.st) ? { item, until: Date.now() + FOCUS_MS } : null
  const sending: Msg = { clientId, k: "me", t: v, files: refs, replyTo: to.reply_to, held: chatBusy() }
  const scope = scopeKey()
  updateChat({ sending: [...chatLive().sending, sending] })
  emit()
  try {
    await api.say(v, files, mentions, to)
    return true
  } catch (e) {
    fail(e); cue("failure"); return false
  } finally {
    if (scope === scopeKey()) updateChat({ sending: chatLive().sending.filter((m) => m !== sending) })
  }
}
/** Steve is on a turn, or messages wait for one: what is sent now queues. */
export const chatBusy = () => isThinking() || !!chatLive().live || chatQueue().length > 0 || chatLive().sending.length > 0
/** stop the turn Steve is on; what waits in the queue goes next, the composer keeps what is being typed */
export async function stopReply() {
  const { live } = chatLive()
  const r = await Promise.all([live ? call(api.stopRun(live.run)) : undefined, call(api.stop())])
  cue("idle")
  /* nothing was running any more (the turn ended meanwhile, or a reload missed its end): the page stops waiting */
  if (r[1] && !(r[1] as { ok?: boolean }).ok && !r[0]?.ok) updateChat({ thinking: false, live: null })
}
/* ── the main conversation's queue: a message waiting for the turn can be changed or withdrawn until it goes ── */
export async function threadQueuedEdit(id: string, text: string) { const r = await call(api.threadQueuedEdit(id, text)); if (r) toast(t("core:toast.queuedEdited")); return !!r }
/** back into the composer when it is empty; otherwise removed, with an undo that queues it again */
export async function threadQueuedWithdraw(id: string, composer?: { empty: () => boolean; fill: (t: string, f: RFile[]) => void } | null) {
  const r = await call(api.threadQueuedWithdraw(id)); if (!r) return
  const files = r.files ?? []
  if (composer?.empty()) { composer.fill(r.text, files); toast(t("core:toast.withdrawnToComposer")); return }
  toast(t("core:toast.withdrawn"), { action: { label: t("core:toast.undo"), onClick: () => void chatSend(r.text, files.map((f) => f.id), files) } })
}
export function quoteMsg(id: string | null) { st.quote = id; emit(); requestAnimationFrame(() => document.querySelector<HTMLTextAreaElement>("[data-composer='chat'] textarea, textarea")?.focus()) }
export function clearFocus() { st.focus = null; emit() }
export const focusItem = () => (st.focus && st.focus.until > Date.now() && !closedSt(it(st.focus.item)?.st) ? st.focus.item : null)
/* thread routing actions: a 409 (already handled) comes back as its message */
export async function takeOffer(m: Msg, take: boolean, bot?: string) { if (!m.id) return; const r = await call(api.offer(m.id, take, take ? bot : undefined)); if (r?.item) toast(t("core:toast.madeItem", { id: r.item.id })) }
/** an A2UI button pressed in a reply: `already` when this surface had been submitted (another tab, a double click that got through) */
export async function a2uiSubmit(msg: string, a: A2uiSubmit): Promise<{ already: boolean }> {
  const r = await api.a2ui(msg, a)
  await refresh().catch(fail)
  return { already: r.already }
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
export async function threadFollow(item: string, follow: boolean) { await call(api.threadFollow(item, follow), t(follow ? "core:toast.follow" : "core:toast.unfollow")); void qc.invalidateQueries({ queryKey: detailKey(item) }) }
/** the raw thread info of an item's detail (§5), once loaded */
export const itemThread = (id: string) => itemDetail(id)?.thread ?? null
/** a decision's full text (a delivery summary), from the open cards, item details or recent decisions */
export function decisionDraft(id: string): string | undefined {
  const open = decisionView(id); if (open) return open.draft
  for (const [, det] of qc.getQueriesData<RItemDetail>({ queryKey: ["detail", scopeKey(), "item"] })) for (const d of det?.decisions ?? []) if (d.id === id) return d.draft
  return settled().find((d) => d.id === id)?.draft
}
/** notifications newer than the read cursor */
export const notices = () => noticeList().map((n) => n.level === "fail" && !n.closed && n.item_id && itemView(n.item_id)?.state.kind === "closed" ? { ...n, closed: true } : n)
export const unreadNotices = () => notices().filter((n) => n.created > st.noticeSeen)
export function readNotices() {
  const at = noticeList()[0]?.created ?? 0; if (at <= st.noticeSeen) return
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
/** The first meeting is on: the owner and Steve are getting to know each other in the main conversation. */
export const meeting = () => st.settings?.onboard === "pending"
/** The tour's last step for the instance's owner: Steve opens the first meeting in the conversation. */
export async function meet() { finishOnboarding(); await shellCall(api.onboardStart()) }
/** No tour; for the owner no first meeting either, which the Hub remembers for every browser. */
export function skipOnboarding() { finishOnboarding(); if (st.owner) void shellCall(api.onboardSkip()) }
/** Ends the first meeting that is on. */
export const endMeeting = () => shellCall(api.onboardSkip(), t("core:toast.meetingSkipped"))

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
  no_executor: ["assign_bot", "confirm_proposal", "later"],
  chan_message: OPTION_IDS.chan_message,
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
/* commands on a decision answer once the views hold what they did (data/sync.ts settle) */
async function decisionCall<T>(p: Promise<T>, ok?: string): Promise<T | undefined> {
  try { const r = await p; if (ok) toast(ok); return r } catch (e) { fail(e); return undefined }
}

export async function dismissDrafts(ids: string[]) {
  for (let at = 0; at < ids.length; at += 500) {
    const r = await decisionCall(api.dismissDrafts(ids.slice(at, at + 500))
      .then((r) => settle(r, "decision", r.ids)))
    if (!r) return
  }
}

export async function editReplyDraft(id: string, previous: string, text: string) {
  return !!await decisionCall(api.editReplyDraft(id, previous, text).then((r) => settle(r, "decision", [id])))
}
const answer = (id: string, c: string, text?: string) => api.decide(id, c, text, decisionView(id)?.revision).then((r) => settle(r, "decision", [id]) as Promise<DecisionAnswer>)
const dismissed = (id: string, revision: number) => api.dismiss(id, revision).then((r) => settle(r, "decision", [id]))
const undone = (id: string, revision: number) => api.undo(id, revision).then((r) => settle(r, "decision", [id]))
export async function decide(id: string, c: string, text?: string, decision?: string) {
  const { i, w } = cardOf(id, decision); if (!w) return
  const accepted = channelAnswer(w); if (accepted) return accepted
  const key = "decision:" + w.decision
  if (st.pendingActions.has(key) || st.submittedChannelDecisions.has(w.decision) || st.inactiveChannelDecisions.has(w.decision)) return
  st.pendingActions.add(key); emit()
  try {
    if (w.subject === "chan_message" && w.type !== "fail") {
      let result: DecisionAnswer
      try { result = await answer(w.decision, c, text) } catch (e) { fail(e); return }
      channelAnswers.set(w.decision, { result }); unlockClosed()
      toast(decidedText(w, c, i?.b ?? null)); emit()
      return result
    }
    if (w.subject === "chan_message" && w.type === "fail") {
      // Keep an accepted answer locked until the Hub has confirmed this decision is no longer open.
      // A failed/stale state refresh must not turn an acknowledged retry into a second send request.
      try {
        const result = await answer(w.decision, c, text)
        st.submittedChannelDecisions.add(w.decision); unlockClosed(); emit()
        toast(c === "retry" ? t("core:sendRecovery.retryRequested") : c === "rewrite" ? t("core:sendRecovery.rewriteRequested") : decidedText(w, c, i?.b ?? null))
          return result
      } catch (e) { fail(e); return }
    }
    if (w.subject === "release" && c === "yes") {
      /* the answer already says whether the release started; "已发布" only comes with the result */
      const d = await decisionCall(answer(w.decision, c, text))
      if (d) { const rel = relOf(d); toast(rel?.text ?? t("core:decided.releaseApproved")); if (rel?.tone === "fail") cue("failure") }
      return
    }
    const result = await decisionCall(answer(w.decision, c, text), decidedText(w, c, i?.b ?? null))
    if (result && c === "pass") cue("success")
    if (result && w.subject === "no_executor" && (c === "later" || c === "confirm_proposal")) void refresh()
  } finally { st.pendingActions.delete(key); emit() }
}
/** a reply drafted in an IM conversation, closed in that conversation: not sent, for good (undo from 已处理);
 *  one answer at a time, a second click while it is on its way does nothing */
export async function discardOut(decision: string) {
  const key = "decision:" + decision
  if (st.pendingActions.has(key)) return false
  st.pendingActions.add(key); emit()
  try { return !!(await decisionCall(answer(decision, "no"), t("core:toast.discarded"))) }
  finally { st.pendingActions.delete(key); emit() }
}
/** take one card out of 待处理 without answering it (the item's first one unless `decision` says which);
 *  undoable from the toast or from 已处理, except a tool permission, which this denies */
export async function dismiss(id: string, decision?: string) {
  const { w } = cardOf(id, decision); if (!w) return
  const view = decisionView(w.decision); if (!view) return
  const d = await decisionCall(dismissed(w.decision, view.revision))
  if (d) toast(t(w.subject === "tool_call" ? "core:toast.dismissedTool" : w.subject === "chan_message" && w.type === "approve" ? "core:toast.dismissedReply" : "core:toast.dismissed"), w.subject === "tool_call" ? undefined : { action: { label: t("core:toast.undo"), onClick: () => void decisionCall(undone(w.decision, d.revision), id ? t("core:toast.backToPending", { id }) : t("core:toast.backToPendingIt")) } })
}
export function undoDecision(decision: string) {
  const d = done().find((x) => x.decision === decision); if (!d?.canUndo) return
  void decisionCall(undone(d.decision, d.revision), d.id ? t("core:toast.undoneToPending", { id: d.id }) : t("core:toast.backToPendingIt"))
}
/** 已处理: the decisions about items settled in the last days, newest first */
const doneLists = new WeakMap<DecisionView[], Done[]>()
export function done(): Done[] {
  const xs = settled()
  let out = doneLists.get(xs)
  if (!out) {
    out = xs.map((d) => { const r = resultOf(d); const resolvedBy = d.resolved_by ?? (typeof r.decided_by === "string" ? r.decided_by : undefined); return { id: d.item_id ?? "", type: d.kind as Wait["type"], get q() { return decisionText(d.question) }, choice: choiceLabel(d), tm: tm(d.resolved), at: d.resolved ?? 0, decision: d.id, revision: d.revision, canUndo: (d.undo_until ?? 0) > Date.now(), rel: relOf(d),
      resolvedBy,
      how: r.dismissed ? "dismissed" : ["expired", "superseded"].includes(d.status) ? "expired" : "chose",
      via: typeof r.via?.name === "string" ? r.via.name : undefined } satisfies Done })
    doneLists.set(xs, out)
  }
  return out
}

/* ───────── actions: items ───────── */
/** `clientId`: the same for every retry of one unchanged form, so a retry does not make a second item */
export async function newItem(goal: string, bot?: string, project?: string | null, mode: "start" | "propose" = "start", clientId = newClientId(), team?: string, options?: {
  requiredSpecialties?: string[]
  blocked?: (error: BotPlacementError) => void
}) {
  try {
    const r = await api.newItem({ goal, bot, project, mode, client_id: clientId, team, required_specialties: options?.requiredSpecialties })
    await settle(r, "item", [r.id])
    return r.id
  } catch (error) {
    if (error instanceof BotPlacementError && options?.blocked) options.blocked(error)
    else fail(error)
  }
}
export async function supp(id: string, v: string, target: "auto" | "steer" | "new", files: string[] = []) {
  const msg = t(target === "steer" ? "core:toast.steered" : target === "new" ? "core:toast.newSession" : active(it(id))?.st === "run" ? "core:toast.queuedForTurn" : "core:toast.delivered")
  return !!await itemCall(id, api.supplement(id, v, target, files), msg)
}
export function deliver(id: string) { void itemCall(id, api.deliver(id), t("core:toast.delivered")) }
export function withdraw(qid: string) { void call(api.dropQueued(qid), t("core:toast.withdrawn")) }
/* ── queued messages of a session: each one can be edited, withdrawn or sent now ──
 * A row a turn already took answers 409 (“已经发出”): the error is shown and the strip refreshes. */
async function qCall<T>(sid: string, p: Promise<T>): Promise<T | undefined> {
  try { return await p } catch (e) { fail(e); return undefined } finally { await qc.invalidateQueries({ queryKey: queueKey(sid) }) }
}
export async function qEdit(sid: string, qid: string, text: string) { const r = await qCall(sid, api.queuedEdit(sid, qid, text)); if (r) toast(t("core:toast.queuedEdited")); return !!r }
/** back into the composer when it is empty; otherwise just removed, with an undo that queues it again */
export async function qWithdraw(sid: string, qid: string, composer?: { empty: () => boolean; fill: (t: string, f: RFile[]) => void } | null, strip = (t: string) => t) {
  const r = await qCall(sid, api.queuedRemove(sid, qid)); if (!r) return
  const files = r.files ?? []
  if (composer?.empty()) { composer.fill(strip(r.text), files); toast(t("core:toast.withdrawnToComposer")); return }
  toast(t("core:toast.withdrawn"), { action: { label: t("core:toast.undo"), onClick: () => void call(api.sessionSay(sid, r.text, "排队", files.map((f) => f.id))).then((x: any) => x && toast(t(x.result === "queued" ? "core:toast.requeued" : "core:toast.sent"))) } })
}
export async function qSendNow(sid: string, qid: string) {
  const r = await qCall(sid, api.queuedSendNow(sid, qid)); if (!r) return
  toast(r.result === "interrupted" ? t("core:toast.interruptedThenSend") : r.result === "sent" ? t("core:toast.sent") : (r.reason ?? t("core:toast.cannotSendYet")))
}
export function saveGoal(id: string, v: string) { const i = it(id); if (!v || v === i.goals[i.goals.length - 1]) return; void itemCall(id, api.goal(id, v), t("core:toast.goalUpdated")) }
export function reassign(id: string, k: string) { if (k === it(id).b) return; void itemCall(id, api.reassign(id, k), t("core:toast.handedTo", { bot: botName(k) })) }
export async function handoff(
  where: { item: string; session?: string },
  choice: { bot?: string; harness?: string; model?: string; effort?: ReasoningEffort; make_bot_default?: boolean },
  blocked?: (error: BotPlacementError) => void,
): Promise<string | undefined> {
  try {
    const r = where.session
      ? await api.handoffSession(where.session, choice)
      : await api.handoffItem(where.item, choice)
    if (r.bot_default_updated) {
      const { bot, previous } = r.bot_default_updated
      toast(t("core:toast.handedOffAndSavedDefault", { bot: botName(bot) }), {
        action: {
          label: t("core:toast.undo"),
          onClick: () => void call(api.patchBot(bot, { harness: previous.harness, model: previous.model, reasoning_effort: previous.reasoning_effort }), t("core:toast.undone")),
        },
      })
    } else {
      toast(t("core:toast.handedOff"))
    }
    await settle(r, "session", [r.session])
    await qc.invalidateQueries({ queryKey: detailKey(where.item) })
    return r.session
  } catch (error) {
    if (error instanceof BotPlacementError && blocked) blocked(error)
    else fail(error)
    return undefined
  }
}
export function actOnItem(id: string, action: "pause" | "resume" | "drop") {
  void itemCall(id, api.itemAction(id, action), t(({ pause: "core:toast.paused", drop: "core:toast.dropped", resume: "core:toast.resumed" } as const)[action]))
}
export function startItemCycle(id: string) {
  const item = itemView(id)
  if (!item) return
  void itemCall(id, api.itemLifecycle(id, "new_cycle", item.cycle, item.revision), t("core:toast.reopened"))
}
export async function markDone(id: string) {
  const item = itemView(id)
  if (!item) return
  await itemCall(id, api.itemLifecycle(id, "finish", item.cycle, item.revision), t("core:toast.markedDone", { id }))
}
export function star(id: string) { void itemCall(id, api.patchItem(id, { star: !it(id).star })) }
export function setAcc(id: string, v: string) { void itemCall(id, api.patchItem(id, { accept_mode: accKey(v) })) }
/** the owner switches the item's design gate (the Hub refuses anyone else) */
export function setDesignGate(id: string, on: boolean) { void itemCall(id, api.designGate(id, on), t(on ? "core:toast.designGateOn" : "core:toast.designGateOff")) }
export function patchItem(id: string, p: Partial<Item>) {
  const q: Parameters<typeof api.patchItem>[1] = {}
  if (p.t !== undefined) q.title = p.t
  if (p.pri !== undefined) q.priority = p.pri
  if ("due" in p) q.due = p.due ?? null
  if ("project" in p) q.project_id = p.project ?? null
  void itemCall(id, api.patchItem(id, q))
}
export function archive(ids: string[], v = true) { void Promise.all(ids.map((id) => itemCall(id, api.itemAction(id, v ? "archive" : "unarchive")))).then((r) => { const count = r.filter(Boolean).length; if (count) toast(t(v ? "core:toast.archived" : "core:toast.restored", { count })) }) }
export function deleteItem(id: string) { void itemCall(id, api.deleteItem(id), t("core:toast.deletedItem", { id })) }
export function bulk(ids: string[], op: "pause" | "run" | "drop" | "arch") {
  if (op === "arch") return archive(ids)
  void call(api.bulk(ids, op === "run" ? "resume" : op).then((r) => { toast(t(({ pause: "core:toast.bulkPaused", run: "core:toast.bulkResumed", drop: "core:toast.bulkDropped" } as const)[op], { count: r.n })); return r }))
}
export function addDep(id: string, dep: string) { void itemCall(id, api.addDep(id, dep), t("core:toast.depAdded", { id: dep })) }
export function rmDep(id: string, dep: string) { void itemCall(id, api.rmDep(id, dep)) }

/* ───────── actions: sessions ───────── */
export async function doFork(k: string, seq: number, goal: string, asSess: boolean): Promise<string> {
  const r = await call(api.fork(k, seq, goal, asSess), t(asSess ? "core:toast.forkedSession" : "core:toast.forkedItem"))
  return r?.item ? "#/items/" + r.item : r?.session ? "#/sessions/" + r.session : "#/sessions/" + k
}
export async function sessSend(k: string, v: string, how: "steer" | "queue" | undefined, files: string[] = []) {
  const key = "session:" + k
  if (st.pendingActions.has(key)) return false
  st.pendingActions.add(key); emit()
  try { return !!await sessionCall(k, api.sessionSay(k, v, how === "queue" ? "排队" : "插话", files), getSession(k)?.st === "run" && how === "queue" ? t("core:toast.queuedAfterTurn") : undefined) }
  finally { st.pendingActions.delete(key); emit() }
}
export function interrupt(k: string) { void sessionCall(k, api.cancel(k), t("core:toast.interrupted")) }
export function link(k: string, id: string) { void sessionCall(k, api.link(k, id), t("core:toast.linked", { title: it(id).t })) }
export function sessPatch(k: string, p: Partial<Sess>, msg?: string) {
  if (p.title) void sessionCall(k, api.renameSession(k, p.title), msg)
  if (p.st === "ended") void sessionCall(k, api.endSession(k), msg)
}
export function sessRemove(k: string) { const title = getSession(k)?.title; void sessionCall(k, api.removeSession(k), t("core:toast.sessionRemoved", { title })) }
export async function loadLocalSessions() { try { st.localSessions = await api.localSessions(); emit() } catch (e) { fail(e) } }
export async function importSessions(keys: string[]) {
  const r = await call(api.importSessions(keys)); if (!r) return
  const bad = r.filter((x) => typeof x !== "string") as { error: string }[]
  toast(bad.length ? t("core:toast.importedSome", { ok: r.length - bad.length, bad: bad.length, error: bad[0].error }) : t("core:toast.imported", { count: r.length }))
}

/* ───────── actions: bots ───────── */
const botMemoriesKey = () => ["bot-memories", scopeKey()] as const
const botMemoryKey = (id: string) => [...botMemoriesKey(), id] as const
export function useBotMemories(id: string) {
  return useQuery({ queryKey: botMemoryKey(id), queryFn: () => api.memories(id) }, qc).data
}
async function changeBotMemory(k: string, action: Promise<unknown>, message?: string) {
  const queryKey = botMemoryKey(k)
  try {
    await action
    if (message) toast(message)
    await qc.invalidateQueries({ queryKey })
  } catch (e) { fail(e) }
}
export async function saveBot(k: string, p: { n?: string; role?: string; specialties?: string[]; pers?: string; pro?: number; rule?: string; acc?: string; harness?: string; model?: string | null; reasoningEffort?: ReasoningEffort | null; fallbackSurfaces?: ExecutionSurface[]; permMode?: string; project?: string | null; color?: string; round?: number; accessory?: BotAccessory | null; prompt?: string | null; on?: boolean; dev?: string | null }, msg = t("core:toast.botSaved")) {
  const b = st.bots[k]
  const q: Partial<RBot> = {}
  if (p.reasoningEffort !== undefined) q.reasoning_effort = p.reasoningEffort
  if (p.fallbackSurfaces !== undefined) q.fallback_surfaces = p.fallbackSurfaces
  if (p.specialties !== undefined) q.specialties = p.specialties
  if (p.n !== undefined) q.name = p.n; if (p.role !== undefined) q.role = p.role; if (p.pers !== undefined) q.persona = p.pers; if (p.pro !== undefined) q.proactive = p.pro; if (p.rule !== undefined) q.rules = p.rule
  if (p.acc !== undefined) q.accept_mode = accKey(p.acc); if (p.harness !== undefined) q.harness = p.harness; if (p.model !== undefined) q.model = p.model; if (p.permMode !== undefined) q.perm_mode = p.permMode
  if (p.project !== undefined) q.default_project = p.project; if (p.prompt !== undefined) q.prompt = p.prompt; if (p.on !== undefined) q.enabled = p.on; if (p.dev !== undefined) q.device = p.dev
  if (p.color !== undefined || p.round !== undefined || p.accessory !== undefined) {
    q.avatar = {
      color: p.color ?? b.color,
      round: p.round ?? b.round,
      accessory: p.accessory !== undefined ? p.accessory : (b.accessory ?? null),
    }
  }
  const saved = await shellCall(api.patchBot(k, q), msg)
  if (saved?.selection_validation === "unknown" && (p.harness !== undefined || p.model !== undefined || p.reasoningEffort !== undefined)) toast(t("core:toast.botSelectionUnknown"))
}
export async function makeBot(n: string, role: string, harness = defaultHarness(), model: string | null = null, permMode?: string) {
  const q: Partial<RBot> & { name: string } = { name: n, role, model }
  if (harness) { q.harness = harness; q.perm_mode = permMode ?? defaultPermMode(harness) }
  const r = await shellCall(api.newBot(q), t("core:toast.botCreated"))
  if (r?.selection_validation === "unknown") toast(t("core:toast.botSelectionUnknown"))
  return r?.id
}
export async function draftBot(desc: string) { const r = await call(api.draftBot(desc), t("core:toast.botDrafted")); return r?.id }
export function ownsBot(owner: string | null | undefined): boolean {
  return owner ? owner === st.me?.user_id : st.owner
}
export async function deleteBot(k: string, to: string, mem: "delete" | "export") {
  const b = st.bots[k]; const label = formatBotName(b?.n ?? k, b?.main); const r = await shellCall(api.deleteBot(k, to, mem)); if (!r) return
  if (mem === "export" && r.memories.length) { const a = document.createElement("a"); a.href = URL.createObjectURL(new Blob([JSON.stringify(r.memories, null, 2)], { type: "application/json" })); a.download = t("core:toast.memoryFile", { name: b?.n ?? k }); a.click() }
  toast(t(mem === "export" ? "core:toast.botDeletedExported" : "core:toast.botDeleted", { name: label, count: r.moved.length, to: botName(to) }), { action: { label: t("core:toast.undo"), onClick: () => void shellCall(api.restoreBot(k), t("core:toast.botRestored", { name: label })) } })
}
export async function removeBotFromWorkspace(k: string, to: string, mem: "delete" | "export") {
  const b = st.bots[k]; const label = formatBotName(b?.n ?? k, b?.main); const r = await shellCall(api.removeBotFromWorkspace(k, to, mem)); if (!r) return
  if (mem === "export" && r.memories.length) { const a = document.createElement("a"); a.href = URL.createObjectURL(new Blob([JSON.stringify(r.memories, null, 2)], { type: "application/json" })); a.download = t("core:toast.memoryFile", { name: b?.n ?? k }); a.click() }
  toast(t(mem === "export" ? "core:toast.botRemovedFromWorkspaceExported" : "core:toast.botRemovedFromWorkspace", { name: label, count: r.moved.length, to: botName(to) }))
}
export async function dupBot(k: string) { const r = await shellCall(api.dupBot(k), t("core:toast.botCopied")); return r?.id }
export function addMem(k: string, text: string) { void changeBotMemory(k, api.addMemory(k, text), t("core:toast.memoryAdded")) }
export function editMem(k: string, id: string, text: string) { void changeBotMemory(k, api.editMemory(id, text)) }
export function delMem(k: string, id: string) { void changeBotMemory(k, api.deleteMemory(id), t("core:toast.deleted")) }
export function setBotWorkspaces(k: string, ids: string[]) { void shellCall(api.botWorkspaces(k, ids), t("core:toast.botWorkspaces")) }

/* ───────── actions: workspaces and projects ───────── */
export async function switchWorkspace(id: string) {
  try { await api.switchWs(id); setPageWorkspace(id); st.ready = false; emit(); resetTranscripts(); st.noticeSeen = 0; st.hold = null; st.localNodeUpdating = null; resetViews(); updateChat({}); closeStream(); await loadReadSide(); connect(); location.hash = "#/"; toast(t("core:toast.switchedWorkspace", { name: st.workspace?.name ?? "" })); return st.ready && st.workspace?.id === id } catch (e) { fail(e); return false }
}
export async function newWorkspace(name: string, description: string, item_prefix?: string) { const w = await call(api.newWs({ name, description, item_prefix })); if (w) await switchWorkspace(w.id) }
export async function newProject(p: { name: string; path: string; description?: string; worktree?: boolean; default_bot?: string | null; node?: string }) { const r = await shellCall(api.newProject(p), t("core:toast.projectAdded", { name: p.name })); return r?.id }
export function patchProject(id: string, p: Partial<RProject>, msg = t("common:state.saved")) { void shellCall(api.patchProject(id, p), msg) }

/* ───────── tags ───────── */
export const tag = (id: string) => st.tags.find((t) => t.id === id)
/* the built-in tags the Hub seeds in Chinese (ST-34): the six delivery tags by name (core:delivery in zh-CN), and the
 * default descriptions of those and of the six categories (core:tagDesc, in either language the Hub seeded them in).
 * A description someone wrote stays as it is. */
let seeded: { names: Record<string, string>; descs: Record<string, string> } | null = null
const seededTags = () => seeded ??= {
  names: Object.fromEntries(Object.entries(i18n.getResource("zh-CN", "core", "delivery") ?? {}).map(([k, v]) => [v as string, k])),
  descs: Object.fromEntries(["zh-CN", "en"].flatMap((l) => Object.entries(i18n.getResource(l, "core", "tagDesc") ?? {}).map(([k, v]) => [v as string, k]))),
}
/** a tag's name in the UI language: a built-in delivery tag translated, any other as it was named */
export const tagLabel = (tg: Pick<Tag, "name" | "cat">): string => { const k = tg.cat === "交付" && seededTags().names[tg.name]; return k ? t(`core:delivery.${k}` as "core:delivery.plan") : tg.name }
/** a tag's description in the UI language when it is still the one the Hub seeded */
export const tagDesc = (tg: Pick<Tag, "desc">): string => { const k = seededTags().descs[tg.desc]; return k ? t(`core:tagDesc.${k}` as "core:tagDesc.bug") : tg.desc }
export const tagName = (id: string) => { const tg = tag(id); return tg ? tagLabel(tg) : id }
export const tagUses = (id: string) => qc.getQueryData<Awaited<ReturnType<typeof api.tags>>>(tagUsesKey())?.uses?.[id] ?? 0
/** tag categories as the Hub stores them (Chinese values); show one with tagCatLabel */
export const TAG_CATS = ["项目", "类型", "交付", "领域", "客户", "主题"]
const TAG_CAT_KEY: Record<string, "project" | "kind" | "delivery" | "domain" | "client" | "topic"> = { 项目: "project", 类型: "kind", 交付: "delivery", 领域: "domain", 客户: "client", 主题: "topic" }
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
export const isKind = (t?: Tag) => t?.cat === KIND_CAT && KINDS.some(([name]) => name === t.name)
export const kindTags = () => KINDS.map(([n]) => st.tags.find((t) => isKind(t) && t.name === n)).filter(Boolean) as Tag[]
export const kindColor = (name: string) => KINDS.find(([n]) => n === name)?.[2] ?? "currentColor"
/** the item's category tag id; the Hub keeps exactly one */
export const kindOf = (i: Pick<Item, "tags">) => (i.tags ?? []).find((x) => isKind(tag(x)))
/** the category first, then the domain tags as they were added */
export const sortTags = (ids: string[] = []) => [...ids].sort((a, b) => {
  const rank = (id: string) => isKind(tag(id)) ? 0 : tag(id)?.cat === "交付" ? 1 : 2
  return rank(a) - rank(b)
})
export function setTags(id: string, ids: string[]) { void itemCall(id, api.patchItem(id, { tags: ids })) }
/** switch the category in one write: the old one goes as the new one comes, never none in between */
export function setKind(id: string, kind: string) { const i = it(id); if (kindOf(i) === kind) return; setTags(id, [kind, ...(i.tags ?? []).filter((x) => !isKind(tag(x)))]) }
export async function makeTag(t: { name: string; cat: string; desc: string }) { const r = await shellCall(api.newTag({ name: t.name, cat: t.cat, description: t.desc })); return r?.id }
export function saveTag(id: string, p: Partial<Tag>) { void shellCall(api.patchTag(id, { name: p.name, cat: p.cat, description: p.desc }), t("core:toast.tagSaved")) }
export function deleteTag(id: string) { const n = tagUses(id), name = tagName(id); void shellCall(api.deleteTag(id), t("core:toast.tagDeleted", { name, count: n })) }
export function mergeTag(from: string, into: string) { const a = tagName(from); void shellCall(api.mergeTag(from, into), t("core:toast.tagMerged", { from: a, into: tagName(into) })) }

/* ───────── settings ───────── */
export function setSetting(p: Record<string, string>, msg = t("common:state.saved")) { void shellCall(api.settings(p), msg) }

export const itemDisplayState = (item: Item) => itemState(item, itemRunning(item.id))
export const itemDisplayStatus = (item: Item): Item["st"] => itemDisplayStatusOf(itemDisplayState(item))

/* ───────── relation graph model ───────── */
export type GNode = { id: string; k: "item" | "sess" | "proj" | "missing"; title: string; st: string; n?: number; b?: string | null; project?: string | null; itemId?: string | null; group?: string; arch?: boolean; sourceAt?: number }
export type GEdge = [string, string, string]
/** Dependency and provenance are different questions. Do not infer handoffs from session order. */
export function gRelation(mode: "blocking" | "lineage", focus?: string | null) {
  const N: Record<string, GNode> = {}, E: GEdge[] = []
  const missing = (id: string) => {
    const key = `missing:${id}`
    N[key] ??= { id: key, k: "missing", title: id, st: "" }
    return key
  }
  for (const i of getItems()) N[i.id] = { id: i.id, k: "item", title: i.t, st: i.st, b: i.b, project: i.project, itemId: i.id, arch: !!i.arch, sourceAt: i.parent?.at }
  if (mode === "blocking") {
    for (const i of getItems()) {
      for (const dep of i.deps ?? []) E.push([N[dep] ? dep : missing(dep), i.id, "dependency"])
    }
  } else {
    if (focus && getSession(focus)) {
      const s = getSession(focus)!
      N[focus] = { id: focus, k: "sess", title: s.title, st: s.st, b: s.b, n: s.n }
    }
    for (const i of getItems()) {
      if (!i.parent) continue
      if (i.parent.s) {
        const s = getSession(i.parent.s)
        if (s) N[i.parent.s] = { id: i.parent.s, k: "sess", title: s.title, st: s.st, b: s.b, n: s.n }
        E.push([s ? i.parent.s : missing(i.parent.s), i.id, "source"])
      }
      if (i.parent.item) E.push([N[i.parent.item] ? i.parent.item : missing(i.parent.item), i.id, "source"])
    }
  }
  return { N, E }
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
