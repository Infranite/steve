import type { RFile } from "@/api"
import { Files } from "@/attach"
import { cue, onCue, type HeroCue } from "@/bus"
import { ChannelMessageBody } from "@/channels/message-body"
import { ChatRail, LiveSteps, NoticeLine, RailToggle, showNotices, StepsBlock } from "@/chat2"
import { A2uiHost } from "@/a2ui/host"
import { A2uiActionNote } from "@/a2ui/note"
import { Markdown } from "@/components/markdown"
import { Button } from "@/components/ui/button"
import { Card } from "@/components/ui/card"
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuTrigger } from "@/components/ui/dropdown-menu"
import { Input } from "@/components/ui/input"
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover"
import { SidebarTrigger } from "@/components/ui/sidebar"
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip"
import { draftKey } from "@/composer-draft"
import { ProposalCard } from "@/config-ui"
import { Cites, linkCites } from "@/knowledge/parts"
import { Robot } from "@/robot"
import { chatBusy, chatSend, emit, finishOnboarding, focusItem, isPromoting, isThinking, makeItem, pending, pendingCards, quoteMsg, reroute, st, stopReply, tm, toast, WT, type Item, type Msg } from "@/store"
import { ChatQueue } from "@/queue"
import { Spinner } from "@/components/ui/spinner"
import { fillTo, historyState, loadOlder, mainThread, onJump, useHistory } from "@/thread/history"
import { dayLabel, HistoryPanel, HistoryToggle } from "@/thread/history-panel"
import { readPos, savePos, type ReadPos } from "@/thread/topics"
import { ChanOutNote, ConvBadgeChip, ConvSwitcher, ImReadOnly, SenderLine, useConversation } from "@/thread/conversations"
import { AskChips, CardLink, ComposerBars, ItemCard, OfferLine, QueuedNote, QuoteHead, Rerun, SuggestCard, Updates } from "@/thread/parts"
import { Composer, go, ItemNo, ItemRef, openSheet, StatusDot, type ComposerHandle } from "@/ui"
import { PlanBar } from "@/view-shared"
import { cn } from "cn"
import { ArrowDownIcon, ChevronRightIcon, CircleCheckIcon, CircleStopIcon, CopyIcon, ListChecksIcon, MessageCircleIcon, MessageSquareReplyIcon, RouteIcon, Undo2Icon } from "lucide-react"
import { Fragment, useEffect, useLayoutEffect, useRef, useState, type ReactNode } from "react"


/* ═════════ 对话 ═════════ */
/* One compact bar (ST-50): who, what Steve himself is doing right now, and the way to history and to 事项. How many items
 * he follows is on 事项, not under his name, so it does not read as “working on 8 things”. */
const CUE_LABEL: Partial<Record<HeroCue, string>> = { input: "在听", think: "在想…", success: "交出去了", affirm: "收到", inspect: "在找你说的事项", wary: "", love: "♥", failure: "没发出去" }
const DOING: Record<string, string> = { route: "正在理解", answer: "正在想", research: "正在查", item: "正在建事项", config: "正在起草改动", supplement: "正在转给事项" }

function Hero() {
  const S = st.bots.steve
  const [label, setLabel] = useState<string | null>(null)
  useEffect(() => {
    let t: any
    const off = onCue((c) => { clearTimeout(t); const l = CUE_LABEL[c]; setLabel(c === "wary" ? `有 ${pendingCards().length} 件等你决定` : l ?? null); if (c !== "think" && c !== "input") t = setTimeout(() => setLabel(null), 2600) })
    const w = pendingCards().length ? setTimeout(() => cue("wary"), 700) : undefined
    return () => { off(); clearTimeout(t); clearTimeout(w) }
  }, [])
  const doing = st.live ? "正在查" : st.draft?.text ? "正在回复" : isThinking() ? DOING[st.phase] ?? "正在想" : ""
  /* while he works, the work is the state; a cue only fills the quiet moments */
  const text = doing || label || "空闲"
  return (
    <div className="flex min-w-0 shrink items-center gap-2.5" data-hero="compact">
      <div className="shrink-0" onDoubleClick={() => cue("love")}><Robot k="steve" size={28} live /></div>
      <span className="shrink-0 type-headline">{S?.n ?? "Steve"}</span>
      <span className={cn("flex min-w-0 items-center gap-1.5 truncate type-footnote transition-colors", doing || label ? "text-foreground/85" : "text-muted-foreground")} data-hero-label>
        {doing && <span className="size-1.5 shrink-0 rounded-full bg-blue dot-running" aria-hidden />}<span className="truncate">{text}</span></span>
    </div>
  )
}

/** `still`: no entrance, for the reply that takes the place of its draft */
function SteveSays({ children, sub, still }: { children: ReactNode; sub?: string; still?: boolean }) {
  return (
    <div className={cn("min-w-0 self-stretch px-1", !still && "anim-in")} data-steve-msg>
      {sub && <div className="mb-1 type-footnote text-muted-foreground">{sub}</div>}
      <div className="type-chat text-foreground">{children}</div>
    </div>
  )
}

const Me = ({ t, files }: { t: string; files?: RFile[] }) => <div className="flex max-w-[80%] flex-col items-end gap-1.5 self-end anim-in" data-me-msg>
  <Files files={files} align="end" />
  {(t !== "（附件）" || !files?.length) && <div className="min-w-0 max-w-full rounded-[20px] bg-muted px-4 py-2.5 type-chat"><Markdown text={t} /></div>}</div>

/* hover actions under Steve's replies: copy, quote, and the corrections of thread routing (§3.5) */
function MsgActions({ m }: { m: Msg }) {
  const fromChat = !!m.item && (m.steps?.route?.kind === "item" || !!m.updates?.length) && !m.update
  const canItem = !m.item && (!!m.chat || !!m.suggest || m.steps?.route?.kind === "research" || m.steps?.route?.kind === "answer")
  return (
    <div className="absolute -bottom-7 left-0 flex gap-0.5 pl-1 opacity-0 transition-opacity focus-within:opacity-100 group-hover/sm:opacity-100 group-focus-within/sm:opacity-100 [@media(hover:none)]:opacity-100" data-msg-actions>
      <Tooltip><TooltipTrigger render={<Button variant="ghost" size="icon-xs" aria-label="复制" className="text-muted-foreground" onClick={() => { navigator.clipboard?.writeText(m.t ?? "").catch(() => {}); toast("已复制") }} />}><CopyIcon /></TooltipTrigger><TooltipContent>复制</TooltipContent></Tooltip>
      {m.id && <Tooltip><TooltipTrigger render={<Button variant="ghost" size="icon-xs" aria-label="引用回复" className="text-muted-foreground" onClick={() => quoteMsg(m.id!)} data-quote-reply />}><MessageSquareReplyIcon /></TooltipTrigger><TooltipContent>{m.item ? `引用回复，送到 ${m.item}` : "引用回复"}</TooltipContent></Tooltip>}
      {canItem && <Tooltip><TooltipTrigger render={<Button variant="ghost" size="icon-xs" aria-label="做成事项" className="text-muted-foreground" disabled={isPromoting(m)} onClick={() => makeItem(m)} data-make-item />}><ListChecksIcon /></TooltipTrigger><TooltipContent>做成事项</TooltipContent></Tooltip>}
      {fromChat && <Tooltip><TooltipTrigger render={<Button variant="ghost" size="icon-xs" aria-label="改成在这里回答" className="text-muted-foreground" onClick={() => void reroute(m, "inline")} data-to-inline />}><MessageCircleIcon /></TooltipTrigger><TooltipContent>改成在这里回答</TooltipContent></Tooltip>}
    </div>
  )
}

/* the menu on your own messages: quote it, or send it somewhere else than the router did */
function MeActions({ m }: { m: Msg }) {
  const [toItem, setToItem] = useState(false), [id, setId] = useState("")
  if (!m.id) return null
  const send = () => { const v = id.trim().toUpperCase().replace(/^#/, ""); if (!/^[A-Z]{1,6}-\d+$/.test(v)) { toast("写一个事项编号，比如 ST-12"); return } setToItem(false); setId(""); void reroute(m, "item:" + v) }
  return (
    <div className="absolute -bottom-7 right-0 flex items-center gap-0.5 opacity-0 transition-opacity focus-within:opacity-100 group-hover/me:opacity-100 group-focus-within/me:opacity-100 [@media(hover:none)]:opacity-100" data-me-actions>
      <Tooltip><TooltipTrigger render={<Button variant="ghost" size="icon-xs" aria-label="引用回复" className="text-muted-foreground" onClick={() => quoteMsg(m.id!)} />}><MessageSquareReplyIcon /></TooltipTrigger><TooltipContent>引用回复</TooltipContent></Tooltip>
      <DropdownMenu>
        <DropdownMenuTrigger render={<Button variant="ghost" size="icon-xs" aria-label="重新分流" className="text-muted-foreground" data-reroute-menu />}><RouteIcon /></DropdownMenuTrigger>
        <DropdownMenuContent align="end" className="min-w-44">
          <div className="px-2 py-1 type-footnote text-muted-foreground">重新分流…</div>
          <DropdownMenuItem onClick={() => void reroute(m, "inline")} data-reroute="inline">在这里回答</DropdownMenuItem>
          <DropdownMenuItem onClick={() => void reroute(m, "item")} data-reroute="item">建成事项</DropdownMenuItem>
          <DropdownMenuItem onClick={() => setToItem(true)} data-reroute="supplement">送到 ST-n…</DropdownMenuItem>
        </DropdownMenuContent>
      </DropdownMenu>
      <Popover open={toItem} onOpenChange={setToItem}>
        <PopoverTrigger render={<span className="size-0" aria-hidden />} />
        <PopoverContent align="end" className="w-60 gap-2 rounded-2xl p-3" data-reroute-popover>
          <div className="type-callout font-medium">送到哪个事项？</div>
          <Input autoFocus placeholder="ST-12" value={id} onChange={(e) => setId(e.target.value)} onKeyDown={(e) => e.key === "Enter" && send()} className="h-8" data-reroute-item />
          <div className="flex justify-end gap-2"><Button size="sm" variant="ghost" onClick={() => setToItem(false)}>取消</Button><Button size="sm" onClick={send}>送过去</Button></div>
        </PopoverContent>
      </Popover>
    </div>
  )
}

/* first-run onboarding, inline in the conversation */
function Onboarding() {
  const n = st.onboard
  if (n < 0) return null
  const rt = st.runtimes.filter((r) => r.ok).map((r) => r.name)
  const steps: [string, string, string[]][] = [
    ["你好，我是 Steve，帮你把事情交出去、盯进度、该你决定时叫你。", "开始", ["好的"]],
    [rt.length ? `我在这台机器上找到了 ${rt.join("、")}，默认用 ${rt[0]} 干活。每个 Bot 可以单独设置。` : "这台机器上还没有找到可用的 harness（Claude Code 或 Codex），先装好再交代事情。", "运行环境", ["好的", "去设备页看看"]],
    [`事情按工作空间和项目来管：现在在「${st.workspace?.name}」，有 ${st.projects.length} 个项目。项目就是一个目录，事项选了项目就在那里干活。`, "项目", ["好的", "去看看项目"]],
    ["邮件、日历和 IM 还没有接入，接入后我会在后台分诊。现在试着交给我一件事，比如“给事项列表加按状态筛选”。", "第一件事", ["完成引导"]],
  ]
  return <>
    {steps.slice(0, n + 1).map(([q, step, opts], j) => (
      <div key={j} className="flex flex-col gap-2" data-onboard={j}>
        <SteveSays sub={`首次引导 · ${j + 1}/${steps.length} · ${step}`}>{q}</SteveSays>
        {j === n && <div className="flex flex-wrap gap-2 self-start">{opts.map((o) => <Button key={o} size="sm" variant="outline" className="rounded-full" data-onboard-opt={o}
          onClick={() => { if (o === "去设备页看看") { go("#/devices"); return } if (o === "去看看项目") { go("#/projects"); return } if (j === steps.length - 1) { finishOnboarding(); toast("引导完成"); return } st.onboard = j + 1; emit() }}>{o}</Button>)}
          <Button size="sm" variant="ghost" className="text-muted-foreground" onClick={finishOnboarding}>跳过引导</Button></div>}
      </div>))}
  </>
}

/* away digest — modelled on iOS notification summaries: one headline, then short sections with the top three rows each */
const ACT: Record<string, string> = { accept: "验收", approve: "审批", ask: "回答", propose: "决定", fail: "查看" }

function DigestSection({ title, items, row, id }: { title: string; items: Item[]; row: (i: Item) => ReactNode; id: string }) {
  const [all, setAll] = useState(false)
  if (!items.length) return null
  const xs = all ? items : items.slice(0, 3)
  return (
    <div className="border-t px-1.5 py-2" data-digest-section={id}>
      <div className="flex items-center justify-between px-2.5 py-1 type-label text-muted-foreground"><span>{title} · {items.length}</span>
        {items.length > 3 && <button type="button" className="font-normal hover:text-foreground" onClick={() => setAll(!all)}>{all ? "收起" : `还有 ${items.length - 3} 件`}</button>}</div>
      {xs.map((i) => <div key={i.id} role="button" tabIndex={0} onClick={() => go("#/items/" + i.id)} className="flex min-h-10 cursor-pointer items-center gap-2.5 rounded-lg px-2.5 py-1.5 transition-colors hover:bg-accent">{row(i)}</div>)}
    </div>
  )
}

const SEEN = "steve-seen"

function Digest({ from }: { from: number }) {
  const p = pending(), done = st.items.filter((i) => i.st === "done" && i.updated >= from && !i.arch), run = st.items.filter((i) => (i.st === "run" || i.st === "block") && !i.arch)
  const h = Math.round((Date.now() - from) / 3600_000)
  return (
    <Card size="sm" className="-mt-2 w-full max-w-[560px] self-start gap-0 py-0 anim-in" data-digest>
      <div className="px-4 pb-3 pt-3.5">
        <div className="type-footnote text-muted-foreground">{tm(from)} – 现在 · 离开 {h} 小时</div>
        <div className="mt-1 type-headline">{p.length} 件等你决定，{done.length} 件已完成，{run.length} 件在进行</div>
      </div>
      <DigestSection id="need" title="需要你" items={p} row={(i) => <>
        <StatusDot s="wait" /><ItemNo id={i.id} /><span className="min-w-0 flex-1 truncate type-callout">{i.t}</span>
        <Button size="xs" variant="outline" onClick={(e) => { e.stopPropagation(); go("#/inbox") }}>{ACT[i.wait!.type]}</Button></>} />
      <DigestSection id="done" title="已完成" items={done} row={(i) => <>
        <CircleCheckIcon className="size-4 shrink-0 text-success" strokeWidth={1.75} /><ItemNo id={i.id} /><span className="min-w-0 flex-1 truncate type-callout">{i.t}</span><span className="type-footnote text-muted-foreground">{i.now}</span></>} />
      <DigestSection id="run" title="进行中" items={run} row={(i) => <>
        <StatusDot s={i.st} /><ItemNo id={i.id} /><span className="min-w-0 flex-1 truncate type-callout">{i.t}</span><span className="hidden type-footnote text-muted-foreground sm:inline">{i.plan ? <PlanBar i={i} /> : i.st === "block" ? "等前置" : ""}</span></>} />
    </Card>
  )
}

/** last time this browser looked at Steve; the digest shows when you were away longer than the setting */
function useAway() {
  const [from] = useState(() => Number(localStorage.getItem(SEEN) ?? 0))
  useEffect(() => { const mark = () => localStorage.setItem(SEEN, String(Date.now())); mark(); const t = setInterval(mark, 60_000); window.addEventListener("beforeunload", mark); return () => { clearInterval(t); window.removeEventListener("beforeunload", mark) } }, [])
  const hours = Number(st.settings.awayH ?? 4)
  return hours > 0 && from > 0 && Date.now() - from > hours * 3600_000 ? from : 0
}

type Anchor = { id: string; created: number; offset: number }
const FLASH_MS = 1600

/** `conv`: an IM conversation (`chan:<space>`, channels §2.12), read-only; without it, yours */
export function ChatView({ conv }: { conv?: string } = {}) {
  const S = st.bots.steve, p = pendingCards(), away = useAway()
  useHistory()
  const im = useConversation(conv)
  const list: Msg[] = conv ? im?.msgs ?? [] : mainThread(st.thread)
  const rawOf = (m: Msg) => (m.id ? im?.raw.get(m.id) : undefined)
  const scroller = useRef<HTMLDivElement>(null), composer = useRef<ComposerHandle | null>(null), top = useRef<HTMLDivElement>(null)
  const working = isThinking() || !!st.live
  const posKey = conv ? "" : st.workspace?.id ?? ""
  /* where you were reading last time (ST-50): reopening there, the page does not start at the bottom first */
  const saved = useRef<ReadPos | null | undefined>(undefined)
  if (saved.current === undefined) saved.current = conv ? null : readPos(posKey)
  const stuck = useRef(!saved.current || saved.current.bottom), [unread, setUnread] = useState(false)
  const restoring = useRef(!stuck.current)
  /* new messages since you left the bottom: everything said after the latest one you had */
  const leftAt = useRef(0)
  /* where a jump came from (ST-50): back to that message, or to the latest when you were there */
  const [back, setBack] = useState<Anchor | "latest" | null>(null)
  const [flash, setFlash] = useState<string | null>(null)
  const keep = useRef<Anchor | null>(null)
  /* handlers set up once read the list as it is now */
  const shown = useRef(list); shown.current = list
  const latest = () => shown.current.findLast((m) => m.created)?.created ?? 0
  const bottom = () => { stuck.current = true; setUnread(false); setBack(null); scroller.current?.scrollTo({ top: 1e9 }) }
  /** the first message showing at the top of the list, and how far below the top it starts */
  const anchorNow = (): Anchor | null => {
    const el = scroller.current; if (!el) return null
    const top0 = el.getBoundingClientRect().top
    for (const x of el.querySelectorAll<HTMLElement>("[data-msg]")) {
      const r = x.getBoundingClientRect(), id = x.dataset.msg, m = id ? shown.current.find((y) => y.id === id) : undefined
      if (r.bottom > top0 + 1 && m?.created) return { id: m.id!, created: m.created, offset: r.top - top0 }
    }
    return null
  }
  /** put a message back where it was; measures the gap, so applying it twice changes nothing */
  const toAnchor = (a: Anchor) => {
    const el = scroller.current, x = el?.querySelector<HTMLElement>(`[data-msg="${CSS.escape(a.id)}"]`); if (!el || !x) return false
    el.scrollTop += x.getBoundingClientRect().top - el.getBoundingClientRect().top - a.offset
    return true
  }
  const leave = () => { if (stuck.current) leftAt.current = latest(); stuck.current = false }
  useEffect(() => {
    if (stuck.current) bottom()
    else if (restoring.current) return
    else if (list.some((m) => m.created && m.created > leftAt.current && m.k !== "thinking") || (st.draft?.text && !back)) setUnread(true)
  }, [list.length, list.at(-1)?.id, list.at(-1)?.t, st.draft?.text])
  /* older messages came in above: the one you were reading stays where it was */
  useLayoutEffect(() => { if (keep.current && toAnchor(keep.current)) keep.current = null }, [list.length, list[0]?.id])
  /* rendered Markdown and folded steps grow after the first scroll: stay at the bottom until the user scrolls away */
  useEffect(() => {
    const el = scroller.current, inner = el?.firstElementChild; if (!el || !inner) return
    const near = () => { if (el.scrollHeight - el.scrollTop - el.clientHeight >= 40) leave(); else if (!stuck.current) { stuck.current = true; setUnread(false); setBack(null) } }
    /* only scrolling up leaves the bottom: a wheel or swipe that cannot move further down fires no scroll event to undo it */
    let y = 0
    const wheel = (e: WheelEvent) => { if (e.deltaY < 0) leave() }, touch = (e: TouchEvent) => { y = e.touches[0]?.clientY ?? 0 }
    const swipe = (e: TouchEvent) => { const t = e.touches[0]?.clientY ?? y; if (t > y) leave(); y = t }
    const key = (e: KeyboardEvent) => { if (["PageUp", "ArrowUp", "Home"].includes(e.key)) leave() }
    el.addEventListener("wheel", wheel, { passive: true }); el.addEventListener("touchstart", touch, { passive: true }); el.addEventListener("touchmove", swipe, { passive: true }); el.addEventListener("scroll", near, { passive: true }); el.addEventListener("keydown", key)
    window.addEventListener("steve-open-steps", leave)
    /* unfolding something you are reading must not jump you to the bottom */
    const unfold = (e: Event) => { if ((e.target as HTMLElement).closest?.("[data-steps-toggle]")) leave() }; el.addEventListener("click", unfold)
    /* the list grows, or the composer and queue above it grow and shrink the viewport */
    const ro = new ResizeObserver(() => { if (stuck.current) el.scrollTop = el.scrollHeight }); ro.observe(inner); ro.observe(el)
    return () => { ro.disconnect(); el.removeEventListener("wheel", wheel); el.removeEventListener("touchstart", touch); el.removeEventListener("touchmove", swipe); el.removeEventListener("scroll", near); el.removeEventListener("keydown", key); window.removeEventListener("steve-open-steps", leave); el.removeEventListener("click", unfold) }
  }, [])
  /* the top of the list: the conversation before what was loaded comes in (yours only) */
  useEffect(() => {
    const el = scroller.current, t = top.current; if (conv || !el || !t) return
    const io = new IntersectionObserver((es) => {
      if (!es[0]?.isIntersecting || stuck.current || restoring.current || !historyState().more || historyState().loading) return
      keep.current = anchorNow(); void loadOlder()
    }, { root: el, rootMargin: "300px 0px 0px 0px" })
    io.observe(t); return () => io.disconnect()
  }, [conv])
  /* a jump (a search hit, a topic, a quote): load what lies between, show the message, offer the way back */
  const go2 = useRef<(id: string, created?: number) => void>(() => {})
  go2.current = async (id, created) => {
    const from: Anchor | "latest" | null = stuck.current ? "latest" : anchorNow()
    if (!list.some((m) => m.id === id) && created) await fillTo(created)
    requestAnimationFrame(() => requestAnimationFrame(() => {
      const el = scroller.current, x = el?.querySelector<HTMLElement>(`[data-msg="${CSS.escape(id)}"]`)
      if (!el || !x) { toast("这条消息不在对话里了"); return }
      leave()
      /* centred; what was just loaded above still lays out (Markdown, code), so it is put back once more after that */
      const at: Anchor = { id, created: created ?? 0, offset: Math.max(16, (el.clientHeight - x.offsetHeight) / 2) }
      toAnchor(at); setTimeout(() => { if (!stuck.current) toAnchor(at) }, 180)
      setBack((b) => b ?? from); setFlash(id)
    }))
  }
  useEffect(() => { if (!conv) return onJump(({ id, created }) => void go2.current(id, created)) }, [conv])
  useEffect(() => { if (!flash) return; const t = setTimeout(() => setFlash(null), FLASH_MS); return () => clearTimeout(t) }, [flash])
  /* where you were reading, kept across reloads (with the draft, which the composer keeps) */
  const restored = useRef(false)
  useEffect(() => {
    if (conv || restored.current || !st.ready || !list.length) return
    restored.current = true
    const pos = saved.current
    if (!pos || pos.bottom) return
    leftAt.current = latest()
    const show = () => { restoring.current = false; if (toAnchor(pos)) setBack("latest"); else bottom() }
    if (list.some((m) => m.id === pos.id)) requestAnimationFrame(show)
    else void fillTo(pos.created).then(() => requestAnimationFrame(() => requestAnimationFrame(show)))
  }, [conv, st.ready, list.length])
  useEffect(() => {
    if (conv) return
    const el = scroller.current; if (!el) return
    let t: any
    const save = () => { if (!restored.current || restoring.current) return; const a = stuck.current ? null : anchorNow(); savePos(posKey, a ? { bottom: false, ...a } : { bottom: true }) }
    const later = () => { clearTimeout(t); t = setTimeout(save, 300) }
    el.addEventListener("scroll", later, { passive: true }); window.addEventListener("pagehide", save)
    return () => { clearTimeout(t); save(); el.removeEventListener("scroll", later); window.removeEventListener("pagehide", save) }
  }, [conv, posKey])
  const fresh = unread ? list.filter((m) => m.created && m.created > leftAt.current && m.k !== "thinking").length : 0
  const { more, ended, loading } = historyState()
  return (
    <div className="flex h-full min-h-0 overflow-hidden text-foreground" data-chat-page><div className="relative flex min-w-0 flex-1 flex-col">
      <header className="flex h-[52px] shrink-0 items-center gap-2 border-b border-border/60 px-3 sm:px-5" data-chat-header>
        <SidebarTrigger className="hidden shrink-0 text-muted-foreground md:inline-flex" />
        <Hero />
        <div className="flex-1" />
        {!conv && <HistoryToggle />}
        <RailToggle />
      </header>
      <ConvSwitcher current={conv} badge={im?.badge} />
      {conv && im && <div className="mx-auto flex w-full max-w-[800px] shrink-0 px-4 pb-1 sm:px-8"><ConvBadgeChip b={im.badge} /></div>}
      {/* the conversation never scrolls sideways: wide code and tables scroll in their own box, and a pixel too wide here
       * put a horizontal scrollbar line under the messages on every swipe */}
      <div className="relative flex min-h-0 flex-1 flex-col"><div ref={scroller} tabIndex={-1} className="min-h-0 flex-1 overflow-y-auto overflow-x-hidden overscroll-none outline-none" data-chat-scroll>
        <div className="mx-auto flex w-full max-w-[800px] flex-col gap-8 px-4 pb-10 pt-5 sm:px-8" data-chat-messages>
          {!conv && list.length > 0 && <div ref={top} className="-mb-4 flex min-h-5 justify-center type-footnote text-muted-foreground" data-chat-older={loading ? "loading" : more ? "more" : ended ? "start" : "none"}>
            {loading ? <span className="flex items-center gap-2"><Spinner className="size-3.5" />载入更早的消息</span> : more ? <button type="button" className="hover:text-foreground" onClick={() => { keep.current = anchorNow(); void loadOlder() }}>更早的消息</button> : ended && <span>对话从这里开始</span>}
          </div>}
          {conv && im === null && <div className="py-10 text-center type-callout text-muted-foreground" data-conv-missing>没有这个对话，或者你看不到它所在的空间。</div>}
          {list.map((m, n) => {
            const key = m.clientId ?? m.id ?? m.k
            const prev = list[n - 1], day = m.created && (!prev?.created || new Date(prev.created).toDateString() !== new Date(m.created).toDateString())
            const dayLabel = day ? <div className="flex items-center gap-4 type-footnote text-muted-foreground" key={"d" + key} data-chat-day><span className="h-px flex-1 bg-border/60" /><span>{chatDay(m.created!)}</span><span className="h-px flex-1 bg-border/60" /></div> : null
            const lit = m.id && m.id === flash ? "rounded-2xl bg-blue/10 ring-8 ring-blue/10" : ""
            if (m.k === "me" && conv) return <Fragment key={key}>{dayLabel}<div className="flex flex-col gap-1.5" data-msg={m.id} data-im-turn><SenderLine m={rawOf(m)} />{rawOf(m)?.channel_message ? <div className="w-full max-w-[600px] self-end"><ChannelMessageBody message={rawOf(m)!.channel_message!} /></div> : <Me t={m.t!} files={m.files} />}<QueuedNote m={m} /></div></Fragment>
            if (m.k === "me") return <Fragment key={key}>{dayLabel}<div className={cn("group/me relative flex flex-col gap-1.5 transition-colors duration-500", lit)} data-msg={m.id} data-flash={lit ? "" : undefined}><QuoteHead id={m.replyTo} className="self-end" />{m.a2uiAction ? <A2uiActionNote a={m.a2uiAction} /> : <Me t={m.t!} files={m.files} />}<QueuedNote m={m} /><MeActions m={m} /></div></Fragment>
            if (m.k === "thinking") return <div key={key} className="flex flex-col gap-2"><LiveSteps />{st.draft?.text && <SteveSays><Markdown text={st.draft.text} streaming /></SteveSays>}</div>
            if (m.notice) return <Fragment key={key}>{dayLabel}<NoticeLine m={m} onOpen={showNotices} /></Fragment>
            return <Fragment key={key}>{dayLabel}<div className={cn("group/sm relative flex min-w-0 flex-col gap-2.5 transition-colors duration-500", lit)} data-msg={m.id} data-flash={lit ? "" : undefined}>
              <div className="flex min-w-0 items-center gap-2 px-1 type-footnote text-muted-foreground" data-chat-author><span className="truncate font-medium text-foreground">{S?.n ?? "Steve"}</span>{m.created && <time className="shrink-0" dateTime={new Date(m.created).toISOString()}>{tm(m.created)}</time>}</div>
              <QuoteHead id={m.replyTo} /><StepsBlock m={m} /><SteveSays still={m.id === st.drafted}>{!m.update && !m.cardOnly && <A2uiHost msg={m.id} records={m.a2ui} readonly={conv ? "群聊里的界面只能看，操作请在你和 Steve 的主线对话里进行" : undefined}><Markdown text={linkCites(m.t ?? "", m.cites)} /></A2uiHost>}{m.item && !m.update && !m.card && (m.wasCard || list.some((x) => x.card?.item === m.item) ? <CardLink id={m.item} /> : <ItemRef id={m.item} />)}{m.card ? <ItemCard c={m.card} inText={!m.cardOnly && m.card.delivery?.text === m.t} /> : <Updates m={m} />}{m.stopped && <div className="mt-2 flex items-center gap-1.5 type-footnote text-muted-foreground" data-stopped><CircleStopIcon className="size-3.5" aria-hidden />你停下了这次回答</div>}<OfferLine m={m} /><SuggestCard m={m} /><AskChips m={m} conv={!!conv} /><Rerun m={m} conv={!!conv} /></SteveSays>{m.cfgs && <div className="-mt-1 flex flex-col gap-2 self-start">{m.cfgs.map((p) => <ProposalCard key={p.id} p={p} inChat />)}</div>}<Cites cites={m.cites} />{conv ? <ChanOutNote m={rawOf(m)} /> : <MsgActions m={m} />}</div></Fragment>
          })}
          {!conv && away > 0 && <><SteveSays sub="离开期间汇报">你不在的时候：</SteveSays><Digest from={away} /></>}
          {!conv && <Onboarding />}
        </div>
      </div>
      {/* pinned to the list, not the page: the composer's height differs between phone and desktop and grows with the queue */}
      {back ? <div className="absolute bottom-3 left-1/2 flex -translate-x-1/2 items-center gap-0.5 rounded-full border bg-popover p-0.5 shadow-md anim-in" data-jump-bar>
          {back !== "latest" && <Button size="sm" variant="ghost" className="rounded-full" onClick={() => { const a = back; setBack(null); if (!toAnchor(a)) bottom() }} data-jump-back><Undo2Icon />返回刚才位置</Button>}
          <Button size="sm" variant="ghost" className="rounded-full text-muted-foreground" onClick={bottom} data-jump-latest>{fresh ? `${fresh} 条新消息` : "回到最新"}<ArrowDownIcon /></Button>
        </div>
        : unread && <Button className="absolute bottom-3 left-1/2 -translate-x-1/2 rounded-full" size="sm" onClick={bottom} data-new-messages>{fresh ? `${fresh} 条新消息` : "新消息"}<ArrowDownIcon /></Button>}</div>
      <div className="mx-auto w-full max-w-[800px] shrink-0 px-4 pb-[76px] pt-3 sm:px-8 md:pb-5" data-chat-input>
        {conv ? im && <ImReadOnly b={im.badge} /> : <>
        {p.length > 0 && (
          <button type="button" onClick={() => go("#/inbox")} data-pending-strip className="mb-2 flex w-full items-center gap-2.5 rounded-xl px-3 py-2 text-left type-callout transition-colors hover:bg-accent">
            <StatusDot s="wait" /><span className="font-medium">{p.length} 件等你</span>
            <span className="min-w-0 flex-1 truncate text-muted-foreground">{p.map((c) => (c.w.subject === "tool_call" ? "权限" : WT[c.w.type]) + "：" + (c.i.id ? c.i.t : "会话 " + c.i.t)).join(" · ")}</span>
            <ChevronRightIcon className="size-4 text-muted-foreground" />
          </button>
        )}
        <ChatQueue composer={composer} />
        <ComposerBars />
        <Composer draftKey={draftKey(st.workspace?.id, "chat")} handle={composer} placeholder={focusItem() ? `补充给 ${focusItem()}…` : chatBusy() ? `${S?.n ?? "Steve"} 还在忙，可以接着说，本回合结束后发出…` : `和 ${S?.n ?? "Steve"} 说…`} testId="chat" onSend={(t, files, refs, mentions) => { bottom(); return chatSend(t, files, refs, mentions) }} busy={working} onStop={stopReply}
          onCommand={(c) => { if (c === "digest") { void chatSend("/汇报"); return } openSheet(c) }} />
        </>}
      </div>
    </div>{!conv && <HistoryPanel />}<ChatRail /></div>
  )
}

/** the day line in the conversation: 今天, 昨天, then the date */
const chatDay = (ts: number) => dayLabel(ts)
