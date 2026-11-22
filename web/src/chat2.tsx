import { notices, useShellStore, useStoreSelector, useNotices } from "@/store"
import { useChatLive } from "@/data/live"
import { getItems, getItem } from "@/store"
import { useWorkSummary } from "@/running-pill"
/* Chat page, second pass (ST-6): what Steve did for each message, folded like an agent transcript; notifications and a
 * compact summary in a right rail (a sheet on narrow screens); failures also leave one short line in the conversation.
 * Also: the open cards a session waits on, shown inside the session page. */
import { useEffect, useLayoutEffect, useReducer, useRef, useState, type ReactNode } from "react"
import { itemDisplayStatus } from "@/item-workflow-relation"
import { cn } from "cn"
import { ArchiveIcon, ChevronDownIcon, CircleAlertIcon, CircleCheckIcon, InfoIcon, ListPlusIcon, ListTodoIcon, MegaphoneIcon, SendIcon, SlidersHorizontalIcon, XIcon } from "lucide-react"
import { Collapsible, CollapsibleContent } from "@/components/ui/collapsible"
import { type TrailItem } from "@/components/work-trail"
import { recoveredCalls } from "@/thread/chat-progress"
import { toolClass } from "@/thread/tools"
import { steveToolIcon } from "@/tool-icons"
import { Sheet, SheetContent, SheetTitle } from "@/components/ui/sheet"
import { Button } from "@/components/ui/button"
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip"
import { st, tm, botName, projectName, tagName, decideCards, pendingCards, unreadNotices, readNotices, dismissNotice, WT, type Msg, type Steps } from "@/store"
import { ChatThinking } from "@/thread/thinking"
import { go, StatusDot, ItemNo, SectionLabel, SidePanel, PanelHead } from "@/ui"
import type { RNotice } from "@/api"
import { researchLiveItems, researchSummary, researchStep } from "@/thread/parts"
import { t, useT } from "@/i18n"
import { reducedMotion } from "@/lib/motion"

/* ───────── what Steve did ───────── */
/* labels are functions so they follow a language change */
const KIND: Record<string, () => string> = { chat: () => t("shell:chat.kind.answer"), answer: () => t("shell:chat.kind.answer"), research: () => t("shell:chat.kind.research"), item: () => t("shell:chat.kind.item"), suggest: () => t("shell:chat.kind.suggest"), ask: () => t("shell:chat.kind.ask"), supplement: () => t("shell:chat.kind.supplement"), status: () => t("shell:chat.kind.status"), config: () => t("shell:chat.kind.config"), reroute: () => t("shell:chat.kind.reroute") }
const kindLabel = (k: string) => KIND[k]?.() ?? k
const TOOL: Record<string, [() => string, any]> = { create_item: [() => t("shell:chat.tool.createItem"), ListPlusIcon], draft_config: [() => t("shell:chat.tool.draftConfig"), SlidersHorizontalIcon], supplement: [() => t("shell:chat.tool.supplement"), SendIcon], digest: [() => t("shell:chat.tool.digest"), MegaphoneIcon] }
/* a tenth of a second only matters under ten */
const secs = (ms?: number) => (ms == null ? "" : ms < 1000 ? t("shell:chat.ms", { n: ms }) : t("common:time.secondsN", { n: ms < 10_000 ? (ms / 1000).toFixed(1) : Math.round(ms / 1000) }))
/* the folded line's gist, unless the reply itself starts by saying it */
const firstSentence = (t: string) => t.split(/(?<=[。！？.!?])\s*/)[0]?.slice(0, 80) ?? ""
const RESEARCH_ERROR: Record<string, () => string> = {
  permission_denied: () => t("shell:chat.researchError.permissionDenied"),
  invalid_arguments: () => t("shell:chat.researchError.invalidArguments"),
  transport: () => t("shell:chat.researchError.transport"),
  guard_violation: () => t("shell:chat.researchError.guardViolation"),
  unknown: () => t("shell:chat.researchError.unknown"),
}
const researchError = (code?: string) => (code && Object.hasOwn(RESEARCH_ERROR, code) ? RESEARCH_ERROR[code] : RESEARCH_ERROR.unknown)()
/** one line for the folded block and the rail */
export function stepsSummary(s: Steps): string {
  const r = s.route
  if (s.research) return researchSummary(s)
  if (s.answer?.thought) return firstSentence(s.answer.thought)
  if (r?.why) return r.why
  if (r) return `${kindLabel(r.kind)}${r.kind === "item" ? " · " + botName(r.picked ?? r.bot) : ""}`
  const tool = s.tools[0]; return tool ? TOOL[tool.name]?.[0]() ?? tool.name : ""
}
const OPEN_EV = "steve-open-steps"
/** scroll the conversation to a message's steps and unfold them */
export function openSteps(id: string) { window.dispatchEvent(new CustomEvent(OPEN_EV, { detail: id })) }

/** Kept by the conversation page, not shared between workspaces. The research run ID is stable
 * across streaming and persistence; a direct draft is paired only with the reply that replaced it. */
export type ProcessMemory = Map<string, { open: boolean; top: number }>
const draftKeyOf = (mid: string, start: number) => `draft:${mid}:${start}`
function useProcess(memory: ProcessMemory, key: string, from?: string) {
  const [, draw] = useReducer((n: number) => n + 1, 0)
  const saved = memory.get(key) ?? (from ? memory.get(from) : undefined)
  useLayoutEffect(() => {
    if (saved && !memory.has(key)) { memory.set(key, saved); if (from) memory.delete(from) }
  }, [key, from, memory, saved])
  return {
    open: saved?.open ?? false, scrollPosition: saved,
    onOpenChange: (open: boolean) => {
      const next = saved ?? { open, top: 0 }; next.open = open
      memory.set(key, next); draw()
    },
  }
}
type End = "failed" | "user" | "budget" | "violation" | "restart"
const outcome = (m: Msg): End | undefined => m.steps?.research?.stopped ?? (m.stopped ? "user" : m.steps?.answer?.failed ? "failed" : m.rerun ? "restart" : undefined)
export function TurnInterruption({ m }: { m: Msg }) {
  const t = useT(), end = outcome(m)
  if (!end) return null
  const detail = end === "budget" || end === "violation" || end === "restart" ? t(`thread:progress.${end}`) : undefined
  return <div className={cn("flex items-start gap-2 rounded-lg border bg-muted/35 px-3 py-2 type-callout", end === "user" ? "text-muted-foreground" : "text-destructive")} data-chat-interruption={end}>
    <CircleAlertIcon className="mt-0.5 size-4 shrink-0" aria-hidden />
    <div><p className="font-medium">{t(end === "user" ? "thread:progress.stopped" : "thread:progress.unfinished")}</p>{detail && <p className="text-foreground">{detail}</p>}</div>
  </div>
}

export function StepsBlock({ m, memory }: { m: Msg; memory: ProcessMemory }) {
  const s = m.steps ?? (m.stopped ? { tools: [] } : undefined), t = useT()
  const end = outcome(m)
  const from = useChatLive((live) => m.id === live.drafted && m.origin && live.draftedStart != null ? draftKeyOf(m.origin, live.draftedStart) : undefined)
  const process = useProcess(memory, s?.research?.run ? "run:" + s.research.run : "msg:" + m.id, from)
  const openFromRail = useRef(process.onOpenChange); openFromRail.current = process.onOpenChange
  useEffect(() => {
    const h = (e: Event) => {
      if ((e as CustomEvent).detail !== m.id) return
      openFromRail.current(true)
      const el = document.getElementById("steps-" + m.id), scroller = el?.closest<HTMLElement>("[data-chat-scroll]")
      if (el && scroller) scroller.scrollTo({ top: scroller.scrollTop + el.getBoundingClientRect().top - scroller.getBoundingClientRect().top - 24, behavior: reducedMotion() ? "auto" : "smooth" })
    }
    window.addEventListener(OPEN_EV, h); return () => window.removeEventListener(OPEN_EV, h)
  }, [m.id, openFromRail])
  if (!s || (!s.route && !s.tools.length && !s.answer && !s.research && !m.stopped)) return null
  const r = s.route
  const routeDesc = r && [kindLabel(r.kind), r.kind === "item" && botName(r.picked ?? r.bot), r.project && projectName(r.project), ...r.tags.map(tagName), ...r.newTags.map((tag) => t("shell:chat.newTag", { tag }))].filter(Boolean).join(" · ")
  const items: TrailItem[] = []
  if (r) items.push({ t: "line", key: "route", state: "done", label: <span className="shrink-0">{t("shell:chat.triage")}</span>, detail: <span className="min-w-0 truncate text-foreground/85">{routeDesc}</span>, attrs: { "step-route": "" },
    body: <div className="pb-1 type-callout text-muted-foreground">{r.picked ? t("shell:chat.picked", { bot: botName(r.picked) }) : ""}{r.why}{r.guard && r.modelKind && r.modelKind !== r.kind ? t("shell:chat.guarded", { model: kindLabel(r.modelKind), kind: kindLabel(r.kind) }) : ""}<span className="ml-1.5 type-footnote tabular-nums text-tertiary">{[r.model, secs(r.ms)].filter(Boolean).join(" · ")}</span></div> })
  if (s.answer?.thought) items.push({ t: "think", key: "thought", text: s.answer.thought, live: false, ms: s.answer.ms, attrs: { "step-thought": "" } })
  const recovered = recoveredCalls(s.tools, step => step.status === "ok")
  s.tools.forEach((tool, n) => {
    const brief = typeof tool.args?.brief === "string" ? tool.args.brief : undefined
    const known = TOOL[tool.name], item = tool.result?.item as string | undefined, state = tool.status === "failed" ? "fail" : "done"
    const err = (tool.error || (s.research && tool.status === "failed")) ? <div className="pb-1 type-callout text-destructive">{s.research ? researchError(tool.error) : tool.error}</div> : undefined
    const extra = tool.ms > 0 ? <span className="shrink-0 tabular-nums text-tertiary">{secs(tool.ms)}</span> : undefined
    if (!known && (brief !== undefined || steveToolIcon(tool.name))) {
      const row = researchStep(String(n), tool.name, tool.kind, brief ?? "", state)
      items.push({ ...row, card: { name: row.verb[1], args: brief || undefined, result: tool.result, error: state === "fail" ? (s.research ? researchError(tool.error) : tool.error) : undefined, ms: tool.ms },
        attrs: { kind: toolClass(tool.name, tool.kind), "step-tool": tool.name, recovered: recovered.has(n) ? "" : undefined } })
      return
    }
    const desc = tool.name === "create_item" ? [item, botName((tool.result?.bot ?? tool.args.bot) as string), tool.result?.project ? projectName(tool.result.project as string) : t("shell:chat.noProject")].filter(Boolean).join(" · ")
      : tool.name === "draft_config" ? `${tool.result?.target ?? ""}${tool.result ? " · " + t("shell:chat.changes", { count: Number(tool.result.changes) }) : ""}` : tool.name === "supplement" ? String(tool.args.item ?? "") : ""
    const label = known ? known[0]() : tool.name
    items.push({ t: "line", key: String(n), state, attrs: { "step-tool": tool.name }, body: err,
      label: <span className={cn("shrink-0", state === "fail" && "text-destructive")}>{label}</span>,
      detail: <>{desc && (item ? <button type="button" className="min-w-0 truncate text-foreground/85 hover:text-foreground hover:underline" onClick={(e) => { e.stopPropagation(); go("#/items/" + item) }}>{desc}</button> : <span className="min-w-0 truncate text-foreground/85">{desc}</span>)}{extra}</> })
  })
  const took = s.research?.ms ?? s.ms
  const status = end === "failed" || end === "violation" ? "error" : end ? "stopped" : "done"
  return <div id={"steps-" + m.id} className="w-full min-w-0 self-start" data-steps={m.id} data-stopped={m.stopped || undefined}>
    <ChatThinking items={items} status={status} durationMs={took} {...process}
      endLabel={end ? t(`thread:research.stopped.${end}`) : undefined} headerAttrs={{ "data-steps-toggle": "" }} />
  </div>
}

/** The summary uses reported categories, never the command, path, tool title or prompt text. */
const ACTIVE_KIND = { read: "read", search: "search", exec: "exec", edit: "edit", fetch: "fetch" } as const
export function LiveSteps({ memory }: { memory: ProcessMemory }) {
  const { draft: d, live: l, phase: livePhase } = useChatLive((s) => s), t = useT()
  const items: TrailItem[] = l ? researchLiveItems(l) : d?.thought ? [{ t: "think", key: "thought", text: d.thought, live: !d.text, t0: d.t0 }] : []
  const phase = d?.text ? "reply" : l ? "research" : livePhase || "start"
  const kind = st.down ? undefined : l?.now?.kind
  const action = kind && Object.hasOwn(ACTIVE_KIND, kind) ? ACTIVE_KIND[kind as keyof typeof ACTIVE_KIND] : undefined
  const label = st.down ? t("thread:progress.unavailable") : d?.text ? t("pages:chat.doing.reply") : action ? t(`thread:progress.action.${action}`) : t("thread:thinking.working")
  const process = useProcess(memory, l ? "run:" + l.run : d ? draftKeyOf(d.mid, d.t0) : "starting")
  return <div className="w-full min-w-0 self-start" data-live-steps={phase} data-research-live={l?.run}>
    <ChatThinking items={items} label={`${(st.bots.steve ? botName("steve") : "Steve")} · ${label}`} startedAt={!st.down && (action || d?.text || !l?.at) ? l?.startedAt : undefined}
      lastActivity={!st.down ? l?.at : undefined} {...process} headerAttrs={{ "data-steps-toggle": "" }} />
  </div>
}

/** a failure notification's line in the conversation; it leaves once closed by hand or by a later release of its item */
export function NoticeLine({ m, onOpen }: { m: Msg; onOpen: () => void }) {
  const t = useT()
  return (
    <div className="flex max-w-[88%] items-start gap-2.5 self-start px-1 type-callout anim-in" data-notice-line={m.notice?.ref ?? m.id}>
      <CircleAlertIcon className="mt-0.5 size-4 shrink-0 text-destructive" strokeWidth={1.75} />
      <div className="min-w-0 flex-1 wrap-anywhere"><span className="block text-foreground">{m.t}</span>
        <span className="mt-1 flex flex-wrap gap-x-3 type-footnote text-muted-foreground">{m.item && <button type="button" className="hover:text-foreground hover:underline" onClick={() => go("#/items/" + m.item)}>{t("shell:chat.goItem", { item: m.item })}</button>}<button type="button" className="hover:text-foreground hover:underline" onClick={onOpen}>{t("shell:chat.allNotices")}</button></span></div>
      <Tooltip>
        <TooltipTrigger render={<Button variant="ghost" size="icon-xs" aria-label={t("shell:chat.dismissNotice")} className="-my-0.5 shrink-0 text-muted-foreground" onClick={() => dismissNotice(m.notice?.ref ?? m.id!)} data-notice-dismiss />}><XIcon /></TooltipTrigger>
        <TooltipContent>{t("shell:chat.dismissNoticeHint")}</TooltipContent>
      </Tooltip>
    </div>
  )
}

/* ───────── right rail ───────── */
const RAIL = "steve-chat-rail"
const railPref = () => { const v = localStorage.getItem(RAIL); return v ? v === "1" : window.innerWidth >= 1280 }
/* one panel beside the conversation at a time on wide screens: the rail (事项 and notices) or the history (ST-50); opening
 * the history hides the rail without forgetting that you keep it open */
let railOpen = railPref(), sheetOpen = false, historyOpen = false
const listeners = new Set<() => void>()
const bump = () => listeners.forEach((f) => f())
export function useRail() { const [, f] = useState(0); useEffect(() => { const h = () => f((x) => x + 1); listeners.add(h); return () => { listeners.delete(h) } }, []); return { railOpen: railOpen && !(historyOpen && wide()), sheetOpen, historyOpen } }
export const wide = () => window.matchMedia("(min-width: 1024px)").matches
export function useWide() {
  const [isWide, setWide] = useState(wide)
  useEffect(() => { const q = window.matchMedia("(min-width: 1024px)"), h = () => setWide(q.matches); q.addEventListener("change", h); return () => q.removeEventListener("change", h) }, [])
  return isWide
}
/** desktop: show or hide the rail (remembered); narrow screens: open the sheet */
export function toggleRail() {
  if (wide()) { if (historyOpen) { historyOpen = false; railOpen = true } else railOpen = !railOpen; localStorage.setItem(RAIL, railOpen ? "1" : "0") } else sheetOpen = !sheetOpen
  bump()
}
export function setHistory(open: boolean) { historyOpen = open; if (open && !wide()) sheetOpen = false; bump() }
export function showNotices() { if (wide()) { historyOpen = false; if (!railOpen) toggleRail(); else bump() } else { sheetOpen = true; bump() } setTimeout(() => document.querySelector("[data-rail-notices]")?.scrollIntoView({ behavior: reducedMotion() ? "auto" : "smooth", block: "start" }), 50) }

/* the same notice again (a disk that keeps filling, the same release failing): one line, the latest, with the others under it */
const family = (n: RNotice) => n.kind + ":" + n.text.split(/[\d(:,\uFF08\uFF1A\uFF0C]/)[0].trim()
function similar(xs: RNotice[]): [RNotice, RNotice[]][] {
  const out: [RNotice, RNotice[]][] = [], at = new Map<string, number>()
  for (const n of xs) { const k = family(n), i = at.get(k); if (i === undefined || !k.split(":")[1]) { at.set(k, out.length); out.push([n, []]) } else out[i][1].push(n) }
  return out
}
function Similar({ n, more, notice }: { n: RNotice; more: RNotice[]; notice: (n: RNotice) => ReactNode }) {
  const t = useT(), [open, setOpen] = useState(false)
  return <>{notice(n)}{more.length > 0 && <Collapsible open={open} onOpenChange={setOpen}>
    <CollapsibleContent>{more.map(notice)}</CollapsibleContent>
    <button type="button" className="mb-1 ml-6 flex items-center gap-1 self-start type-footnote text-muted-foreground hover:text-foreground" onClick={() => setOpen(!open)} aria-expanded={open} data-notice-similar={more.length}>{open ? t("shell:chat.similarHide") : t("shell:chat.similarMore", { count: more.length })}<ChevronDownIcon className={cn("size-3.5 transition-transform duration-260 ease-(--ease-out)", open && "rotate-180")} /></button>
  </Collapsible>}</>
}

/** 事项: what Steve is following for you (running, waiting on you) and the notices, in the rail */
export function RailToggle({ className }: { className?: string }) {
  const { railOpen: open, sheetOpen } = useRail(), t = useT()
  useNotices(open || sheetOpen)
  useStoreSelector(() => st.noticeSeen)
  const n = unreadNotices().length, waiting = decideCards().length, run = getItems().filter((i) => itemDisplayStatus(i) === "run" && !i.arch).length
  const label = [t("shell:chat.railLabel.running", { count: run }), waiting && t("shell:chat.railLabel.waiting", { count: waiting }), n && t("shell:chat.railLabel.unread", { count: n })].filter(Boolean).join(t("shell:chat.railLabel.sep"))
  return (
    <Tooltip>
      <TooltipTrigger render={<Button variant="ghost" size="sm" aria-label={label} aria-pressed={open && wide()} onClick={toggleRail} className={cn("gap-1.5 px-2 text-muted-foreground aria-pressed:bg-muted aria-pressed:text-foreground", className)} data-rail-toggle />}>
        <ListTodoIcon /><span className="max-sm:sr-only">{t("shell:chat.items")}</span>{run + waiting > 0 && <span className="tabular-nums text-foreground" data-rail-count>{run + waiting}</span>}
        {waiting > 0 ? <span className="size-1.5 rounded-full bg-attention" aria-hidden data-rail-waiting /> : n > 0 && <span className="size-1.5 rounded-full bg-blue" aria-hidden data-unread />}
      </TooltipTrigger>
      <TooltipContent>{open && wide() ? t("shell:chat.hideRail") : t("shell:chat.showRail")}</TooltipContent>
    </Tooltip>
  )
}

/** the rail beside the conversation: inline on wide screens, a sheet otherwise */
export function ChatRail() {
  const { railOpen } = useRail(), t = useT()
  const isWide = useWide()
  /* the same head as 历史 beside it: one height with the conversation's bar, the name the toggle has, a close */
  const head = <PanelHead title={t("shell:chat.items")} closeLabel={t("shell:chat.hideRail")} onClose={toggleRail} closeTest="data-rail-close" data-rail-head />
  if (isWide) return <SidePanel open={railOpen} test={{ "data-rail": "aside" }}>{head}<div className="min-h-0 flex-1 overflow-y-auto overflow-x-hidden overscroll-contain px-4 pb-6 pt-4" data-rail-scroll><RailBody /></div></SidePanel>
  return (
    <Sheet open={sheetOpen} onOpenChange={(o) => { sheetOpen = o; bump() }}>
      <SheetContent side="right" showCloseButton={false} className="w-[88vw] max-w-[360px]! gap-0 overflow-hidden p-0" data-rail="sheet"><SheetTitle className="sr-only">{t("shell:chat.railTitle")}</SheetTitle>{head}<div className="min-h-0 flex-1 overflow-y-auto overflow-x-hidden overscroll-contain px-4 pb-8 pt-4"><RailBody /></div></SheetContent>
    </Sheet>
  )
}

function Line({ children, onClick, test }: { children: ReactNode; onClick: () => void; test?: string }) {
  return <button type="button" onClick={onClick} className="-mx-2 flex min-h-9 w-[calc(100%+16px)] items-center gap-2.5 rounded-lg px-2 py-1.5 text-left transition-colors hover:bg-sidebar-accent anim-in" data-rail-row={test}>{children}</button>
}
const NI: Record<string, any> = { release: MegaphoneIcon, archive: ArchiveIcon, system: InfoIcon }
const DAY = 86400_000

function deliveredItems() {
  const cutoff = Date.now() - 7 * DAY
  return getItems().filter((i) => i.st === "done" && !i.arch && i.updated > cutoff).sort((a, b) => b.updated - a.updated).slice(0, 5)
}

function RecentDeliveries() {
  const t = useT()
  const items = deliveredItems()
  useStoreSelector(() => {
    const now = Date.now()
    return JSON.stringify(deliveredItems().map((i) => [i.id, i.t, now - i.updated < 60_000 ? 0 : Math.floor(i.updated / 60_000)]))
  })
  if (!items.length) return null
  return <section data-rail-done><SectionLabel>{t("shell:chat.recentDone")}</SectionLabel>
    {items.map((i) => <Line key={i.id} onClick={() => go("#/items/" + i.id)} test={"done-" + i.id}><CircleCheckIcon className="size-4 shrink-0 text-success" strokeWidth={1.75} /><ItemNo id={i.id} /><span className="min-w-0 flex-1 truncate type-callout">{i.t}</span><span className="shrink-0 type-footnote tabular-nums text-tertiary">{i.up}</span></Line>)}
  </section>
}

function RailBody() {
  useNotices()
  useShellStore()
  useWorkSummary()
  const [older, setOlder] = useState(false), t = useT()
  const unread = unreadNotices()
  /* what is on screen counts as read, after a moment so the new ones are seen highlighted */
  useEffect(() => { if (!unread.length) return; const h = setTimeout(readNotices, 2500); return () => clearTimeout(h) }, [unread.length])
  const run = getItems().filter((i) => itemDisplayStatus(i) === "run" && !i.arch), cards = pendingCards()
  const waiting = (n: RNotice) => !!n.item_id && !!getItem(n.item_id)?.wait
  /* failures stay on top until they are read and their item no longer waits on you, or until they are closed */
  const rows = notices()
  const pinned = rows.filter((n) => n.level === "fail" && !n.closed && (n.created > st.noticeSeen || waiting(n)))
  const rest = rows.filter((n) => !pinned.includes(n)), fresh = rest.filter((n) => n.created > st.noticeSeen), seen = rest.filter((n) => n.created <= st.noticeSeen)
  /* read ones fold away once there is something new; with nothing new the latest few stay visible */
  const base = pinned.length || fresh.length ? 0 : 5
  const notice = (n: RNotice) => {
    const I = n.level === "fail" ? CircleAlertIcon : n.level === "success" ? CircleCheckIcon : NI[n.kind] ?? InfoIcon, isNew = n.created > st.noticeSeen
    return (
      <Line key={n.id} onClick={() => n.item_id ? go("#/items/" + n.item_id) : n.text.startsWith("YOLO") ? go("#/yolo") : undefined} test={"notice-" + n.id}>
        <I className={cn("mt-0.5 size-4 shrink-0 self-start", n.level === "fail" && !n.closed ? "text-destructive" : n.level === "fail" ? "text-muted-foreground" : n.level === "success" ? "text-success" : "text-muted-foreground")} strokeWidth={1.75} />
        <span className="min-w-0 flex-1"><span title={n.text} className={cn("line-clamp-2 wrap-anywhere type-callout", isNew ? "font-medium text-foreground" : "text-foreground/80")}>{n.text}</span>
          <span className="mt-0.5 block type-footnote tabular-nums text-tertiary">{n.item_id && <span className="mr-1.5 font-mono">{n.item_id}</span>}{tm(n.created)}</span></span>
        {/* a new failure already stands out by its icon: the unread dot would only compete with it */}
        {isNew && <span className={cn("mt-1.5 size-2 shrink-0 self-start rounded-full bg-blue", n.level === "fail" && !n.closed && "sr-only")} aria-label={t("shell:chat.unread")} data-unread-dot />}
      </Line>
    )
  }
  return (
    <div className="flex flex-col gap-7" data-rail-body>
      {/* what waits on you, then what runs for you, then what happened; Steve's own thinking stays in the thread */}
      {cards.length > 0 && <section data-rail-wait><SectionLabel>{t("shell:chat.waitingHead", { count: cards.length })}</SectionLabel>
        {cards.slice(0, 5).map(({ i, w }) => <Line key={w.decision} onClick={() => go(i.id ? "#/inbox" : "#/sessions/" + w.session)} test={"wait-" + w.decision}><StatusDot s="wait" />{i.id && <ItemNo id={i.id} />}<span className="min-w-0 flex-1 truncate type-callout">{i.id ? i.t : t("shell:chat.sessionTitle", { title: i.t })}</span><span className="shrink-0 type-footnote text-tertiary">{w.subject === "tool_call" ? t("shell:chat.permission") : WT[w.type]}</span></Line>)}
        {cards.length > 5 && <Line onClick={() => go("#/inbox")}><span className="type-footnote text-muted-foreground">{t("shell:chat.moreWaiting", { count: cards.length - 5 })}</span></Line>}</section>}
      {run.length > 0 && <section data-rail-run><SectionLabel>{t("shell:chat.runningHead", { count: run.length })}</SectionLabel>
        {run.slice(0, 5).map((i) => <Line key={i.id} onClick={() => go("#/items/" + i.id)} test={"run-" + i.id}><StatusDot s="run" /><ItemNo id={i.id} /><span className="min-w-0 flex-1 truncate type-callout">{i.t}</span>{i.rel?.tone === "fail" && <span className="shrink-0 type-footnote text-destructive" data-rel="fail">{i.rel.outcome === "not_released" ? t("shell:chat.notReleased") : t("shell:chat.releaseFailed")}</span>}</Line>)}</section>}
      <section data-rail-notices><SectionLabel>{unread.length ? t("shell:chat.noticesUnread", { count: unread.length }) : t("shell:chat.notices")}</SectionLabel>
        {!rows.length && <div className="type-callout text-tertiary">{t("shell:chat.noticesEmpty")}</div>}
        {pinned.map(notice)}{similar(fresh).map(([n, more]) => <Similar key={n.id} n={n} more={more} notice={notice} />)}{similar(seen.slice(0, base)).map(([n, more]) => <Similar key={n.id} n={n} more={more} notice={notice} />)}
        {/* the read ones fold like the similar ones: the same chevron under the text, the height easing open */}
        {seen.length > base && <Collapsible open={older} onOpenChange={setOlder}>
          <CollapsibleContent>{similar(seen.slice(base, 50)).map(([n, more]) => <Similar key={n.id} n={n} more={more} notice={notice} />)}</CollapsibleContent>
          <button type="button" className="mt-1 ml-6 flex items-center gap-1 type-footnote text-muted-foreground hover:text-foreground" onClick={() => setOlder(!older)} aria-expanded={older} data-older>{older ? t("shell:chat.hideRead") : t("shell:chat.olderRead", { count: seen.length - base })}<ChevronDownIcon className={cn("size-3.5 transition-transform duration-260 ease-(--ease-out)", older && "rotate-180")} /></button>
        </Collapsible>}
      </section>
      <RecentDeliveries />
    </div>
  )
}
