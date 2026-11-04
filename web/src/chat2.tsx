/* Chat page, second pass (ST-6): what Steve did for each message, folded like an agent transcript; notifications and a
 * compact summary in a right rail (a sheet on narrow screens); failures also leave one short line in the conversation.
 * Also: the open cards a session waits on, shown inside the session page. */
import { useEffect, useState, type ReactNode } from "react"
import { cn } from "cn"
import { ArchiveIcon, BrainIcon, CheckIcon, CircleAlertIcon, CircleCheckIcon, InfoIcon, ListPlusIcon, ListTodoIcon, MegaphoneIcon, RouteIcon, SendIcon, SlidersHorizontalIcon, XIcon } from "lucide-react"
import { Trace, TraceLine, TraceIcon, TraceTitle, TraceDescription, TraceCollapsible, TraceCollapsibleTrigger, TraceCollapsibleContent } from "@/components/ui/trace"
import { Sheet, SheetContent, SheetTitle } from "@/components/ui/sheet"
import { Button } from "@/components/ui/button"
import { Spinner } from "@/components/ui/spinner"
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip"
import { st, tm, botName, projectName, tagName, pendingCards, sessionCards, unreadNotices, readNotices, dismissNotice, closeRel, WT, type Msg, type Steps } from "@/store"
import { go, StatusDot, ItemNo, DecisionCard, SectionLabel, RelNote } from "@/ui"
import type { RNotice } from "@/api"
import { ResearchLive, researchSummary, researchIcon, researchToolLabel } from "@/thread/parts"

/* ───────── what Steve did ───────── */
const KIND: Record<string, string> = { chat: "直接回答", answer: "直接回答", research: "就地研究", item: "建事项", suggest: "建议", ask: "追问", supplement: "送到事项", status: "问进展", config: "配置", reroute: "改道" }
const TOOL: Record<string, [string, any]> = { create_item: ["建事项", ListPlusIcon], draft_config: ["起草配置改动", SlidersHorizontalIcon], supplement: ["送到事项", SendIcon], digest: ["汇报进展", MegaphoneIcon] }
const secs = (ms?: number) => (ms == null ? "" : ms < 1000 ? `${ms} 毫秒` : `${(ms / 1000).toFixed(1)} 秒`)
const firstSentence = (t: string) => t.split(/(?<=[。！？.!?])\s*/)[0]?.slice(0, 80) ?? ""
const RESEARCH_ERROR: Record<string, string> = {
  permission_denied: "工具调用被权限或审批策略拒绝，查询未完成。",
  invalid_arguments: "查询参数不正确，未能完成这次调用。",
  transport: "查询服务连接失败，请稍后重试。",
  guard_violation: "调用超出本次研究允许的范围，已停止。",
  unknown: "这次查询未完成，未能确认具体原因。",
}
const researchError = (code?: string) => code && Object.hasOwn(RESEARCH_ERROR, code) ? RESEARCH_ERROR[code] : RESEARCH_ERROR.unknown
/** one line for the folded block and the rail */
export function stepsSummary(s: Steps): string {
  const r = s.route
  if (s.research) return researchSummary(s)
  if (s.answer?.thought) return firstSentence(s.answer.thought)
  if (r?.why) return r.why
  if (r) return `${KIND[r.kind]}${r.kind === "item" ? " · " + botName(r.picked ?? r.bot) : ""}`
  const t = s.tools[0]; return t ? TOOL[t.name]?.[0] ?? t.name : ""
}
const OPEN_EV = "steve-open-steps"
/** scroll the conversation to a message's steps and unfold them */
export function openSteps(id: string) { window.dispatchEvent(new CustomEvent(OPEN_EV, { detail: id })) }

export function StepsBlock({ m }: { m: Msg }) {
  const s = m.steps, [open, setOpen] = useState(false)
  useEffect(() => {
    const h = (e: Event) => { if ((e as CustomEvent).detail !== m.id) return; setOpen(true); document.getElementById("steps-" + m.id)?.scrollIntoView({ behavior: "smooth", block: "center" }) }
    window.addEventListener(OPEN_EV, h); return () => window.removeEventListener(OPEN_EV, h)
  }, [m.id])
  if (!s || (!s.route && !s.tools.length)) return null
  const r = s.route, failed = s.tools.some((t) => t.status === "failed")
  const routeDesc = r && [KIND[r.kind] ?? r.kind, r.kind === "item" && botName(r.picked ?? r.bot), r.project && projectName(r.project), ...r.tags.map(tagName), ...r.newTags.map((t) => t + "（新建）")].filter(Boolean).join(" · ")
  return (
    <TraceCollapsible open={open} onOpenChange={setOpen} id={"steps-" + m.id} className="max-w-full self-start" data-steps={m.id}>
      <TraceCollapsibleTrigger className="px-2 py-1 hover:bg-muted [&_[data-slot=trace-line]]:text-[12px] [&_[data-slot=trace-description]]:text-muted-foreground" data-steps-toggle>
        <TraceLine status={failed ? "error" : undefined}><TraceIcon><BrainIcon strokeWidth={1.5} /></TraceIcon><TraceTitle>{s.research ? "查了" : "思考"}</TraceTitle>
          <TraceDescription className="min-w-0 shrink truncate">{stepsSummary(s)}{s.ms != null && !s.research && <span className="ml-1.5">· {secs(s.ms)}</span>}</TraceDescription></TraceLine>
      </TraceCollapsibleTrigger>
      <TraceCollapsibleContent>
        <Trace className="gap-1.5 pt-1">
          {r && <div className="flex flex-col gap-0.5" data-step-route>
            <TraceLine><TraceIcon><RouteIcon strokeWidth={1.5} /></TraceIcon><TraceTitle className="text-foreground/80">分诊</TraceTitle><TraceDescription className="min-w-0 shrink truncate">{routeDesc}</TraceDescription></TraceLine>
            <div className="pl-6 type-footnote text-muted-foreground">{r.picked ? `你用 @ 指定了${botName(r.picked)}。` : ""}{r.why}{r.guard && r.modelKind && r.modelKind !== r.kind ? `（模型判为${KIND[r.modelKind] ?? r.modelKind}，按规则改成${KIND[r.kind] ?? r.kind}）` : ""}<span className="ml-1.5 text-tertiary">{[r.model, secs(r.ms)].filter(Boolean).join(" · ")}</span></div>
          </div>}
          {s.answer?.thought && <div className="flex flex-col gap-0.5" data-step-thought>
            <TraceLine><TraceIcon><BrainIcon strokeWidth={1.5} /></TraceIcon><TraceTitle className="text-foreground/80">怎么回答</TraceTitle><TraceDescription>{secs(s.answer.ms)}</TraceDescription></TraceLine>
            <div className="whitespace-pre-wrap pl-6 type-callout text-muted-foreground">{s.answer.thought}</div>
          </div>}
          {s.tools.map((t, n) => {
            const brief = typeof t.args?.brief === "string" ? t.args.brief : undefined
            const [label, I] = TOOL[t.name] ?? (brief !== undefined ? [researchToolLabel(t.name), researchIcon(t.name)] : [t.name, InfoIcon]), item = t.result?.item as string | undefined
            const desc = t.name === "create_item" ? [item, botName((t.result?.bot ?? t.args.bot) as string), t.result?.project ? projectName(t.result.project as string) : "无项目"].filter(Boolean).join(" · ")
              : t.name === "draft_config" ? `${t.result?.target ?? ""}${t.result ? ` · ${t.result.changes} 项改动` : ""}` : t.name === "supplement" ? String(t.args.item ?? "") : brief ?? ""
            return (
              <div key={n} className="flex flex-col gap-0.5" data-step-tool={t.name}>
                <TraceLine status={t.status === "failed" ? "error" : undefined}>
                  <TraceIcon>{t.status === "failed" ? <XIcon strokeWidth={1.75} /> : <I strokeWidth={1.5} />}</TraceIcon><TraceTitle className={t.status === "failed" ? "" : "text-foreground/80"}>{label}</TraceTitle>
                  <TraceDescription className="min-w-0 shrink truncate font-mono text-[12px]">{item ? <button type="button" className="hover:text-foreground hover:underline" onClick={() => go("#/items/" + item)}>{desc}</button> : desc}</TraceDescription>
                  <TraceDescription className="inline-flex items-center gap-1">{t.status === "ok" && <CheckIcon className="size-3.5" strokeWidth={1.75} />}{secs(t.ms)}</TraceDescription>
                </TraceLine>
                {(t.error || (s.research && t.status === "failed")) && <div className="pl-6 type-footnote text-destructive">{s.research ? researchError(t.error) : t.error}</div>}
              </div>
            )
          })}
        </Trace>
      </TraceCollapsibleContent>
    </TraceCollapsible>
  )
}

const PHASE: Record<string, string> = { route: "正在判断怎么处理", answer: "正在想怎么回答", research: "正在查", item: "正在建事项", config: "正在起草配置改动", supplement: "正在送到事项" }
/** while Steve works on a message: what it is doing right now; while an answer is written, its reasoning so far */
export function LiveSteps() {
  const d = st.draft
  if (st.live) return <ResearchLive />
  if (d?.thought) return (
    <div className="-mb-1 max-w-[88%] self-start px-1" data-live-steps="thought">
      <TraceLine><TraceIcon>{d.text ? <BrainIcon strokeWidth={1.5} /> : <Spinner className="size-3.5" />}</TraceIcon><TraceTitle>思考</TraceTitle>
        <TraceDescription className="min-w-0 shrink truncate">{firstSentence(d.thought)}</TraceDescription></TraceLine>
    </div>
  )
  if (d?.text) return null
  return <div className="flex items-center gap-2 self-start px-1 type-callout text-muted-foreground" data-live-steps={st.phase || "start"}><Spinner className="size-3.5" /><span className="shimmer">{PHASE[st.phase] ?? "正在理解"}</span></div>
}

/** a failure notification's line in the conversation; it leaves once closed by hand or by a later release of its item */
export function NoticeLine({ m, onOpen }: { m: Msg; onOpen: () => void }) {
  return (
    <div className="flex max-w-[88%] items-start gap-2.5 self-start px-1 type-callout anim-in" data-notice-line={m.notice?.ref ?? m.id}>
      <CircleAlertIcon className="mt-0.5 size-4 shrink-0 text-destructive" strokeWidth={1.75} />
      <div className="min-w-0 flex-1 wrap-anywhere"><span className="text-foreground">{m.t}</span>
        <span className="ml-2 inline-flex gap-2 type-footnote text-muted-foreground">{m.item && <button type="button" className="hover:text-foreground hover:underline" onClick={() => go("#/items/" + m.item)}>去 {m.item}</button>}<button type="button" className="hover:text-foreground hover:underline" onClick={onOpen}>通知</button></span></div>
      <Tooltip>
        <TooltipTrigger render={<Button variant="ghost" size="icon-xs" aria-label="关闭这条提示" className="-my-0.5 shrink-0 text-muted-foreground" onClick={() => dismissNotice(m.notice?.ref ?? m.id!)} data-notice-dismiss />}><XIcon /></TooltipTrigger>
        <TooltipContent>关闭，通知里仍可查到</TooltipContent>
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
export function showNotices() { if (wide()) { historyOpen = false; if (!railOpen) toggleRail(); else bump() } else { sheetOpen = true; bump() } setTimeout(() => document.querySelector("[data-rail-notices]")?.scrollIntoView({ behavior: "smooth", block: "start" }), 50) }

/** 事项: what Steve is following for you (running, waiting on you) and the notices, in the rail */
export function RailToggle({ className }: { className?: string }) {
  const { railOpen: open } = useRail()
  const n = unreadNotices().length, waiting = pendingCards().length, run = st.items.filter((i) => i.st === "run" && !i.arch).length
  const label = [`事项，${run} 件进行中`, waiting && `${waiting} 件等你`, n && `${n} 条未读通知`].filter(Boolean).join("，")
  return (
    <Tooltip>
      <TooltipTrigger render={<Button variant="ghost" size="sm" aria-label={label} aria-pressed={open && wide()} onClick={toggleRail} className={cn("gap-1.5 px-2 text-muted-foreground aria-pressed:bg-muted aria-pressed:text-foreground", className)} data-rail-toggle />}>
        <ListTodoIcon /><span className="max-sm:sr-only">事项</span>{run + waiting > 0 && <span className="tabular-nums text-foreground" data-rail-count>{run + waiting}</span>}
        {waiting > 0 ? <span className="size-1.5 rounded-full bg-attention" aria-hidden data-rail-waiting /> : n > 0 && <span className="size-1.5 rounded-full bg-blue" aria-hidden data-unread />}
      </TooltipTrigger>
      <TooltipContent>{open && wide() ? "收起事项栏" : "在办事项、等你决定的和通知"}</TooltipContent>
    </Tooltip>
  )
}

/** the rail beside the conversation: inline on wide screens, a sheet otherwise */
export function ChatRail() {
  const { railOpen } = useRail()
  const isWide = useWide()
  if (isWide) return railOpen ? <aside className="flex w-[300px] shrink-0 flex-col overflow-y-auto overflow-x-hidden border-l border-sidebar-border bg-sidebar px-4 pb-6 pt-3" data-rail="aside"><RailBody /></aside> : null
  return (
    <Sheet open={sheetOpen} onOpenChange={(o) => { sheetOpen = o; bump() }}>
      <SheetContent side="right" className="w-[88vw] max-w-[360px]! overflow-y-auto overflow-x-hidden px-4 pb-8 pt-3" data-rail="sheet"><SheetTitle className="sr-only">通知和摘要</SheetTitle><RailBody /></SheetContent>
    </Sheet>
  )
}

function Line({ children, onClick, test }: { children: ReactNode; onClick: () => void; test?: string }) {
  return <button type="button" onClick={onClick} className="-mx-2 flex min-h-9 w-[calc(100%+16px)] items-center gap-2 rounded-lg px-2 py-1.5 text-left transition-colors hover:bg-accent" data-rail-row={test}>{children}</button>
}
const NI: Record<string, any> = { release: MegaphoneIcon, archive: ArchiveIcon, system: InfoIcon }
const DAY = 86400_000

function RailBody() {
  const [older, setOlder] = useState(false)
  const unread = unreadNotices()
  /* what is on screen counts as read, after a moment so the new ones are seen highlighted */
  useEffect(() => { if (!unread.length) return; const t = setTimeout(readNotices, 2500); return () => clearTimeout(t) }, [unread.length])
  const run = st.items.filter((i) => i.st === "run" && !i.arch), cards = pendingCards()
  const delivered = st.items.filter((i) => i.st === "done" && !i.arch && Date.now() - i.updated < 7 * DAY).sort((a, b) => b.updated - a.updated).slice(0, 5)
  const thoughts = st.thread.filter((m) => m.k === "steve" && m.steps && (m.steps.route || m.steps.tools.length)).slice(-5).reverse()
  const waiting = (n: RNotice) => !!n.item_id && !!st.items.find((i) => i.id === n.item_id)?.wait
  /* failures stay on top until they are read and their item no longer waits on you, or until they are closed */
  const pinned = st.notices.filter((n) => n.level === "fail" && !n.closed && (n.created > st.noticeSeen || waiting(n)))
  const rest = st.notices.filter((n) => !pinned.includes(n)), fresh = rest.filter((n) => n.created > st.noticeSeen), seen = rest.filter((n) => n.created <= st.noticeSeen)
  /* read ones fold away once there is something new; with nothing new the latest few stay visible */
  const shownSeen = older ? 50 : pinned.length || fresh.length ? 0 : 5
  const notice = (n: RNotice) => {
    const I = n.level === "fail" ? CircleAlertIcon : n.level === "success" ? CircleCheckIcon : NI[n.kind] ?? InfoIcon, isNew = n.created > st.noticeSeen
    return (
      <Line key={n.id} onClick={() => n.item_id ? go("#/items/" + n.item_id) : n.text.startsWith("YOLO") ? go("#/yolo") : undefined} test={"notice-" + n.id}>
        <I className={cn("mt-0.5 size-4 shrink-0 self-start", n.level === "fail" && !n.closed ? "text-destructive" : n.level === "fail" ? "text-muted-foreground" : n.level === "success" ? "text-success" : "text-muted-foreground")} strokeWidth={1.75} />
        <span className="min-w-0 flex-1"><span title={n.text} className={cn("line-clamp-2 break-all type-callout", isNew ? "font-medium text-foreground" : "text-foreground/80")}>{n.text}</span>
          <span className="mt-0.5 block type-footnote text-muted-foreground">{n.item_id && <span className="mr-1.5 font-mono">{n.item_id}</span>}{tm(n.created)}</span></span>
        {isNew && <span className="mt-1.5 size-2 shrink-0 self-start rounded-full bg-blue" aria-label="未读" data-unread-dot />}
      </Line>
    )
  }
  return (
    <div className="flex flex-col gap-6" data-rail-body>
      {/* the bar above the conversation already says what Steve is doing: the rail starts with what waits on you */}
      {cards.length > 0 && <section data-rail-wait><SectionLabel>等你 · {cards.length}</SectionLabel>
        {cards.slice(0, 5).map(({ i, w }) => <Line key={w.decision} onClick={() => go(i.id ? "#/inbox" : "#/sessions/" + w.session)} test={"wait-" + w.decision}><StatusDot s="wait" />{i.id && <ItemNo id={i.id} />}<span className="min-w-0 flex-1 truncate type-callout">{i.id ? i.t : "会话 " + i.t}</span><span className="shrink-0 type-footnote text-muted-foreground">{w.subject === "tool_call" ? "权限" : WT[w.type]}</span></Line>)}
        {cards.length > 5 && <Line onClick={() => go("#/inbox")}><span className="type-footnote text-muted-foreground">还有 {cards.length - 5} 件，去待处理</span></Line>}</section>}
      {thoughts.length > 0 && <section data-rail-thoughts><SectionLabel>思考摘要</SectionLabel>
        {thoughts.map((m) => <Line key={m.id} onClick={() => openSteps(m.id!)} test={"thought-" + m.id}><BrainIcon className="size-4 shrink-0 text-muted-foreground" strokeWidth={1.5} /><span className="min-w-0 flex-1 truncate type-callout">{stepsSummary(m.steps!)}</span><span className="shrink-0 type-footnote text-muted-foreground">{tm(m.created)}</span></Line>)}</section>}
      <section data-rail-notices><SectionLabel>通知{unread.length ? ` · ${unread.length} 条未读` : ""}</SectionLabel>
        {!st.notices.length && <div className="type-footnote text-muted-foreground">发布结果、自动归档这类消息会出现在这里</div>}
        {pinned.map(notice)}{fresh.map(notice)}{seen.slice(0, shownSeen).map(notice)}
        {seen.length > shownSeen && <button type="button" className="mt-1 type-footnote text-muted-foreground hover:text-foreground" onClick={() => setOlder(true)} data-older>已读的 {seen.length - shownSeen} 条</button>}
        {older && <button type="button" className="mt-1 type-footnote text-muted-foreground hover:text-foreground" onClick={() => setOlder(false)}>收起已读</button>}
      </section>
      {run.length > 0 && <section data-rail-run><SectionLabel>进行中 · {run.length}</SectionLabel>
        {run.slice(0, 5).map((i) => <Line key={i.id} onClick={() => go("#/items/" + i.id)} test={"run-" + i.id}><StatusDot s="run" /><ItemNo id={i.id} /><span className="min-w-0 flex-1 truncate type-callout">{i.t}</span>{i.rel?.tone === "fail" && <span className="shrink-0 type-footnote text-destructive" data-rel="fail">{i.rel.text.includes("没有发布") ? "没有发布" : "发布失败"}</span>}</Line>)}</section>}
      {delivered.length > 0 && <section data-rail-done><SectionLabel>最近交付</SectionLabel>
        {delivered.map((i) => <Line key={i.id} onClick={() => go("#/items/" + i.id)} test={"done-" + i.id}><CircleCheckIcon className="size-4 shrink-0 text-success" strokeWidth={1.75} /><ItemNo id={i.id} /><span className="min-w-0 flex-1 truncate type-callout">{i.t}</span><span className="shrink-0 type-footnote text-muted-foreground">{i.up}</span></Line>)}</section>}
    </div>
  )
}

/* ───────── a session's open cards ───────── */
/** decisions the session waits on, answerable here like in 待处理 (also for sessions without an item) */
export function SessionCards({ k }: { k: string }) {
  const cards = sessionCards(k)
  /* after the release card is answered, what became of it stays under the conversation */
  const item = st.S[k]?.item, it = item ? st.items.find((x) => x.id === item) : undefined, rel = it?.rel
  if (!cards.length && !rel) return null
  return <div className="mt-6 flex flex-col gap-3" data-session-cards={k}>{rel && <RelNote rel={rel} box={rel.tone !== "ok"} onClose={rel.tone === "fail" && it ? () => closeRel(it) : undefined} />}{cards.map(({ i, w }) => <DecisionCard key={w.decision} i={i} w={w} showItem={!!i.id} />)}</div>
}
