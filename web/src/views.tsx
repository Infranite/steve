import { ChannelMessageBody } from "@/channels/message-body"
import { Fragment, useEffect, useRef, useState, type ReactNode } from "react"
import { cn } from "cn"
import { BookOpenIcon, ChartNoAxesColumnIcon, ContactRoundIcon, KeyRoundIcon, UserRoundIcon,
  CalendarIcon, ChevronDownIcon, ChevronRightIcon, GitBranchIcon, GitPullRequestIcon, ListIcon, MailIcon, MessageSquareIcon, MoreHorizontalIcon,
  PauseIcon, PlayIcon, PlusIcon, RadioIcon, SearchIcon, SettingsIcon, SquareTerminalIcon, StarIcon, ZapIcon, CheckIcon, XIcon,
  WorkflowIcon, LockIcon, CircleCheckIcon, UploadIcon, CopyIcon, BotIcon, RotateCcwIcon, ArchiveIcon, TrashIcon, LinkIcon, PencilIcon, FolderIcon,
  ListChecksIcon, DownloadIcon, SquareIcon, MinusIcon, BanIcon, MessageCircleIcon, MessageSquareReplyIcon, RouteIcon,
  PlugIcon, MessagesSquareIcon,
} from "lucide-react"
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover"
import { Checkbox } from "@/components/ui/checkbox"
import { Button } from "@/components/ui/button"
import { Select as UISelect, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select"
import { Field, FieldContent, FieldDescription, FieldGroup, FieldLabel, FieldTitle } from "@/components/ui/field"
import { RadioGroup, RadioGroupItem } from "@/components/ui/radio-group"
import { Card } from "@/components/ui/card"
import { Badge } from "@/components/ui/badge"
import { Robot } from "@/robot"
import { cue, onCue, type HeroCue } from "@/bus"
import { SidebarTrigger } from "@/components/ui/sidebar"
import { AlertDialog, AlertDialogAction, AlertDialogCancel, AlertDialogContent, AlertDialogDescription, AlertDialogFooter, AlertDialogHeader, AlertDialogTitle } from "@/components/ui/alert-dialog"
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip"
import { Input } from "@/components/ui/input"
import { Textarea } from "@/components/ui/textarea"
import { Switch } from "@/components/ui/switch"
import { Progress } from "@/components/ui/progress"
import { Collapsible, CollapsibleContent, CollapsibleTrigger } from "@/components/ui/collapsible"
import { HoverCard, HoverCardContent, HoverCardTrigger } from "@/components/ui/hover-card"
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuSeparator, DropdownMenuTrigger } from "@/components/ui/dropdown-menu"
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog"
import { Spinner } from "@/components/ui/spinner"
import { draftKey } from "@/composer-draft"
import {
  st, it, pending, active, accOf, hm, WT, chatSend, supp, saveGoal, reassign, setSt, star, setAcc, sessSend, interrupt,
  link, newItem, emit, toast, botName, markDone, gBuild, gComps, doFork, addDep, rmDep, depsOpen, isPromoting, pendingCards, stageOf, type Item, type PlanRound,
  stopReply, isThinking, undoDecision, setTags, tag, TAG_CATS, snoozed, unsnooze, archive, deleteItem, bulk, patchItem, sessPatch, sessRemove,
  loadItem, loadEvents, loadLocalSessions, importSessions, finishOnboarding, tm,
  quoteMsg, makeItem, reroute, focusItem, itemThread, threadFollow, type Msg,
} from "@/store"
import {
  go, Page, BotAvatar, BotName, Status, StatusDot, Meta, SectionLabel, Group, Row, Segmented, Empty, Composer, Transcript, RunBar, SessionBlock, ReplyBlock,
  DecisionCard, ItemRef, openSheet, closeSheet, ThemeSwitch, ItemNo, Chips, Tags, RelNote, type ComposerHandle,
} from "@/ui"
import { RelationGraph, GraphLegend } from "@/graph"
import { ProposalCard } from "@/config-ui"
import { api } from "@/api"
import { StepsBlock, LiveSteps, NoticeLine, ChatRail, RailToggle, SessionCards, showNotices } from "@/chat2"
import { OfferLine, SuggestCard, AskChips, Rerun, QueuedNote, QuoteHead, Updates, ComposerBars, ItemCard, CardLink } from "@/thread/parts"
import { ConvSwitcher, ConvBadgeChip, ChanOutNote, ImReadOnly, SenderLine, useConversation } from "@/thread/conversations"
import { Markdown } from "@/components/markdown"
import { Files } from "@/attach"
import { QueueStrip, queueNote } from "@/queue"
import { YoloPill, yoloState } from "@/yolo/switch"
import { YoloItemSwitch, YoloMark, choiceText } from "@/yolo/badges"
import { ItemPeopleProp } from "@/people/item-people"
import { ItemStatsRows, SessionStats } from "@/stats/drill"
import type { RFile } from "@/api"
import { GitHubMoreRow } from "@/github/index"
import { Cites, linkCites, SessionKnowledge } from "@/knowledge/parts"
import { usePeopleOn } from "@/api/people"

export const STAGES = ["接收", "分流", "定义", "规划", "准备", "执行", "交付与验收", "收尾"] as const

const openNode = (n: { k: string; id: string }) => go((n.k === "item" ? "#/items/" : n.k === "proj" ? "#/projects/" : "#/sessions/") + n.id)
/** the item's project as a link; items without one say so, since the work then happens in a scratch directory */
function ProjectChip({ id, className, plain }: { id: string | null; className?: string; plain?: boolean }) {
  const p = st.projects.find((x) => x.id === id)
  if (!p) return <span className={cn("inline-flex items-center gap-1", className)} data-project-chip="none"><FolderIcon className="size-3.5" strokeWidth={1.75} />没有项目</span>
  /* plain: inside a clickable row or card, where a nested link would be a control inside a control */
  if (plain) return <span className={cn("inline-flex min-w-0 items-center gap-1", className)} data-project-chip={p.id}><FolderIcon className="size-3.5 shrink-0" strokeWidth={1.75} /><span className="truncate">{p.name}</span></span>
  return <a href={"#/projects/" + p.id} onClick={(e) => e.stopPropagation()} className={cn("inline-flex items-center gap-1 text-foreground/80 hover:text-foreground hover:underline", className)} data-project-chip={p.id}><FolderIcon className="size-3.5" strokeWidth={1.75} />{p.name}</a>
}
export function Select({ value, options, onChange, align = "start", testId, quiet, className }: { value: string; options: { v: string; l: ReactNode }[]; onChange: (v: string) => void; align?: "start" | "end"; testId?: string; quiet?: boolean; className?: string }) {
  return (
    <UISelect items={Object.fromEntries(options.map((o) => [o.v, o.l]))} value={value} onValueChange={(v) => v != null && onChange(String(v))}>
      <SelectTrigger size="sm" className={cn("rounded-lg bg-transparent font-normal", quiet ? "-ml-px max-w-full border-transparent px-2 hover:bg-accent [&>svg]:opacity-0 hover:[&>svg]:opacity-100" : "border-border", className)} data-select={testId}><SelectValue /></SelectTrigger>
      <SelectContent align={align === "end" ? "end" : "start"} className="min-w-44">{options.map((o) => <SelectItem key={o.v} value={o.v}>{o.l}</SelectItem>)}</SelectContent>
    </UISelect>
  )
}

/* ═════════ 对话 ═════════ */
/* A steady, compact identity bar keeps the reading area still while scrolling. */
const CUE_LABEL: Partial<Record<HeroCue, string>> = { input: "在听", think: "在想…", success: "交出去了", affirm: "收到", inspect: "在找你说的事项", wary: "", love: "♥", failure: "没发出去" }
function Hero() {
  const S = st.bots.steve, p = pendingCards(), run = st.items.filter((i) => i.st === "run" && !i.arch)
  const [label, setLabel] = useState<string | null>(null)
  useEffect(() => {
    let t: any
    const off = onCue((c) => { clearTimeout(t); const l = CUE_LABEL[c]; setLabel(c === "wary" ? `有 ${pendingCards().length} 件等你决定` : l ?? null); if (c !== "think" && c !== "input") t = setTimeout(() => setLabel(null), 2600) })
    const w = pendingCards().length ? setTimeout(() => cue("wary"), 700) : undefined
    return () => { off(); clearTimeout(t); clearTimeout(w) }
  }, [])
  const base = run.length + p.length ? `在跟 ${run.length + p.length} 件事${p.length ? ` · ${p.length} 件等你` : ""}` : "空闲"
  return (
    <div className="flex h-16 min-w-0 shrink-0 items-center gap-3" data-hero="compact">
      <div className="shrink-0" onDoubleClick={() => cue("love")}><Robot k="steve" size={32} live /></div>
      <div className="flex min-w-0 flex-col">
        <span className="truncate type-headline">{S?.n ?? "Steve"}</span>
        <span className={cn("truncate type-footnote transition-colors", label ? "text-foreground" : "text-muted-foreground")} data-hero-label>{label || base}</span>
      </div>
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
/** `conv`: an IM conversation (`chan:<space>`, channels §2.12), read-only; without it, yours */
export function ChatView({ conv }: { conv?: string } = {}) {
  const S = st.bots.steve, p = pendingCards(), away = useAway()
  const im = useConversation(conv)
  const list: Msg[] = conv ? im?.msgs ?? [] : st.thread
  const rawOf = (m: Msg) => (m.id ? im?.raw.get(m.id) : undefined)
  const scroller = useRef<HTMLDivElement>(null)
  useEffect(() => { const f = requestAnimationFrame(() => scroller.current?.scrollTo({ top: 1e9, behavior: "smooth" })); return () => cancelAnimationFrame(f) }, [list.length, list[list.length - 1]?.k])
  /* rendered Markdown and folded steps grow after the first scroll: stay at the bottom until the user scrolls away */
  useEffect(() => {
    const el = scroller.current, inner = el?.firstElementChild; if (!el || !inner) return
    let stick = true
    const away = () => { stick = false }, near = () => { if (el.scrollHeight - el.scrollTop - el.clientHeight < 40) stick = true }
    el.addEventListener("wheel", away, { passive: true }); el.addEventListener("touchmove", away, { passive: true }); el.addEventListener("scroll", near, { passive: true })
    window.addEventListener("steve-open-steps", away)
    /* unfolding something you are reading must not jump you to the bottom */
    const unfold = (e: Event) => { if ((e.target as HTMLElement).closest?.("[data-steps-toggle]")) away() }; el.addEventListener("click", unfold)
    const ro = new ResizeObserver(() => { if (stick) el.scrollTop = el.scrollHeight }); ro.observe(inner)
    return () => { ro.disconnect(); el.removeEventListener("wheel", away); el.removeEventListener("touchmove", away); el.removeEventListener("scroll", near); window.removeEventListener("steve-open-steps", away); el.removeEventListener("click", unfold) }
  }, [])
  return (
    <div className="flex h-full min-h-0 text-foreground" data-chat-page><div className="relative flex min-w-0 flex-1 flex-col">
      <header className="flex shrink-0 items-center gap-3 border-b border-border/60 px-4 sm:px-6" data-chat-header>
        <SidebarTrigger className="hidden shrink-0 text-muted-foreground md:inline-flex" />
        <Hero />
        <div className="flex-1" />
        <RailToggle />
      </header>
      <ConvSwitcher current={conv} badge={im?.badge} />
      {conv && im && <div className="mx-auto flex w-full max-w-[800px] shrink-0 px-4 pb-1 sm:px-8"><ConvBadgeChip b={im.badge} /></div>}
      <div ref={scroller} className="min-h-0 flex-1 overflow-y-auto overscroll-contain" data-chat-scroll>
        <div className="mx-auto flex w-full max-w-[800px] flex-col gap-8 px-4 pb-10 pt-5 sm:px-8" data-chat-messages>
          {conv && im === null && <div className="py-10 text-center type-callout text-muted-foreground" data-conv-missing>没有这个对话，或者你看不到它所在的空间。</div>}
          {list.map((m, n) => {
            const prev = list[n - 1], day = m.created && (!prev?.created || new Date(prev.created).toDateString() !== new Date(m.created).toDateString())
            const dayLabel = day ? <div className="flex items-center gap-4 type-footnote text-muted-foreground" key={"d" + n} data-chat-day><span className="h-px flex-1 bg-border/60" /><span>{new Date(m.created!).toDateString() === new Date().toDateString() ? "今天" : tm(m.created).split(" ")[0]}</span><span className="h-px flex-1 bg-border/60" /></div> : null
            if (m.k === "me" && conv) return <Fragment key={n}>{dayLabel}<div className="flex flex-col gap-1.5" data-msg={m.id} data-im-turn><SenderLine m={rawOf(m)} />{rawOf(m)?.channel_message ? <div className="w-full max-w-[600px] self-end"><ChannelMessageBody message={rawOf(m)!.channel_message!} /></div> : <Me t={m.t!} files={m.files} />}<QueuedNote m={m} /></div></Fragment>
            if (m.k === "me") return <Fragment key={n}>{dayLabel}<div className="group/me relative flex flex-col gap-1.5 transition-colors" data-msg={m.id}><QuoteHead id={m.replyTo} className="self-end" /><Me t={m.t!} files={m.files} /><QueuedNote m={m} /><MeActions m={m} /></div></Fragment>
            if (m.k === "thinking") return <div key={n} className="flex flex-col gap-2"><LiveSteps />{st.draft?.text && <SteveSays><Markdown text={st.draft.text} streaming /></SteveSays>}</div>
            if (m.notice) return <Fragment key={n}>{dayLabel}<NoticeLine m={m} onOpen={showNotices} /></Fragment>
            return <Fragment key={n}>{dayLabel}<div className="group/sm relative flex min-w-0 flex-col gap-2.5 transition-colors" data-msg={m.id}>
              <div className="flex min-w-0 items-center gap-2 px-1 type-footnote text-muted-foreground" data-chat-author><span className="truncate font-medium text-foreground">{S?.n ?? "Steve"}</span>{m.created && <time className="shrink-0" dateTime={new Date(m.created).toISOString()}>{tm(m.created)}</time>}</div>
              <QuoteHead id={m.replyTo} /><StepsBlock m={m} /><SteveSays still={m.id === st.drafted}>{!m.update && !m.cardOnly && <Markdown text={linkCites(m.t ?? "", m.cites)} />}{m.item && !m.update && !m.card && (m.wasCard || list.some((x) => x.card?.item === m.item) ? <CardLink id={m.item} /> : <ItemRef id={m.item} />)}{m.card ? <ItemCard c={m.card} inText={!m.cardOnly && m.card.delivery?.text === m.t} /> : <Updates m={m} />}<OfferLine m={m} /><SuggestCard m={m} /><AskChips m={m} /><Rerun m={m} /></SteveSays>{m.cfg && <div className="-mt-1 self-start"><ProposalCard p={m.cfg} inChat /></div>}<Cites cites={m.cites} />{conv ? <ChanOutNote m={rawOf(m)} /> : <MsgActions m={m} />}</div></Fragment>
          })}
          {!conv && away > 0 && <><SteveSays sub="离开期间汇报">你不在的时候：</SteveSays><Digest from={away} /></>}
          {!conv && <Onboarding />}
        </div>
      </div>
      <div className="mx-auto w-full max-w-[800px] shrink-0 px-4 pb-[76px] pt-3 sm:px-8 md:pb-5" data-chat-input>
        {conv ? im && <ImReadOnly b={im.badge} /> : <>
        {p.length > 0 && (
          <button type="button" onClick={() => go("#/inbox")} data-pending-strip className="mb-2 flex w-full items-center gap-2.5 rounded-xl px-3 py-2 text-left type-callout transition-colors hover:bg-accent">
            <StatusDot s="wait" /><span className="font-medium">{p.length} 件等你</span>
            <span className="min-w-0 flex-1 truncate text-muted-foreground">{p.map((c) => (c.w.subject === "tool_call" ? "权限" : WT[c.w.type]) + "：" + (c.i.id ? c.i.t : "会话 " + c.i.t)).join(" · ")}</span>
            <ChevronRightIcon className="size-4 text-muted-foreground" />
          </button>
        )}
        <ComposerBars />
        <Composer draftKey={draftKey(st.workspace?.id, "chat")} placeholder={focusItem() ? `补充给 ${focusItem()}…` : `和 ${S?.n ?? "Steve"} 说…`} testId="chat" onSend={(t, files, refs, mentions) => chatSend(t, files, refs, mentions)} busy={isThinking() || !!st.live} onStop={stopReply}
          onCommand={(c) => { if (c === "digest") { void chatSend("/汇报"); return } openSheet(c) }} />
        </>}
      </div>
    </div><ChatRail /></div>
  )
}

/* ═════════ 待处理 ═════════ */
let ibTab: "待处理" | "已处理" = "待处理"; let ibType = "全部"
export function InboxView() {
  const [, f] = useState(0); const r = () => f((x) => x + 1)
  const p = pendingCards(), sz = snoozed()
  const types = ["accept", "approve", "ask", "propose", "fail", "confirm"] as const
  const typeOpts = ["全部", ...types.filter((t) => p.some((c) => c.w.type === t)).map((t) => WT[t])]
  const xs = p.filter((c) => ibType === "全部" || WT[c.w.type] === ibType)
  return (
    <Page title="待处理" sub={p.length ? `${p.length} 件需要你决定` : "没有事卡在你这里"} max={640}
      toolbar={<><YoloPill className="max-sm:hidden" /><Segmented value={ibTab} onChange={(v) => { ibTab = v; r() }} options={["待处理", "已处理"] as const} /></>}>
      {ibTab === "待处理" && <>
        {p.length > 0 && <Chips className="mb-6" value={typeOpts.includes(ibType) ? ibType : "全部"} onChange={(v) => { ibType = v; r() }} options={typeOpts} />}
        {p.length === 0 && <Empty title="都处理完了" sub="新的决定会出现在这里，也会推送到手机。" />}
        <div className="flex flex-col gap-8">
          {types.map((t) => {
            const g = xs.filter((c) => c.w.type === t)
            return g.length ? <section key={t}><SectionLabel>{WT[t]} · {g.length}</SectionLabel><div className="flex flex-col gap-3">{g.map((c) => <DecisionCard key={c.w.decision} i={c.i} w={c.w} />)}</div></section> : null
          })}
          {sz.length > 0 && <section data-snoozed><SectionLabel>稍后提醒 · {sz.length}</SectionLabel>
            <Group>{sz.map((i) => <Row key={i.id} title={<><ItemNo id={i.id} className="mr-2" />{i.t}</>} sub={`${WT[i.wait!.type]} · ${i.snooze}提醒`}><Button size="xs" variant="ghost" className="text-muted-foreground" onClick={() => unsnooze(i.id)} data-unsnooze>现在处理</Button></Row>)}</Group></section>}
        </div>
      </>}
      {ibTab === "已处理" && (st.done.length ? <>
        <Group>{st.done.map((d, n) => (
          <Row key={n} title={<><ItemNo id={d.id} className="mr-2" />{d.q}</>} sub={d.rel ? <span className="flex min-w-0 flex-wrap items-center gap-x-1">{WT[d.type]} · <RelNote rel={d.rel} className="inline-flex" /> · {d.tm}</span>
            : d.yolo !== undefined ? <span className="flex min-w-0 flex-wrap items-center gap-x-1">{WT[d.type]} · <YoloMark /> 选了「{choiceText(d.choice)}」{d.yolo ? `：${d.yolo}` : ""} · {d.tm}{d.undone ? " · 已撤销" : ""}</span>
            : `${WT[d.type]} · ${d.how === "dismissed" ? "你撤回了" : d.how === "expired" ? "已失效：" + d.choice : `你选了「${d.choice}」`} · ${d.tm}${d.undone ? " · 已撤销" : ""}`} data-done-row={d.id}>
            {d.canUndo && <Button size="xs" variant="ghost" className="text-muted-foreground" onClick={() => undoDecision(n)} data-undo-decision><RotateCcwIcon />撤销</Button>}
          </Row>))}</Group>
        <div className="mt-2 px-1 type-footnote text-muted-foreground">显示最近 7 天做过的决定。验收通过、“不需要”、“我来看看”和撤回在 24 小时内可以撤销；已经告诉 Bot 的回答和批准收不回来。</div>
      </> : <Empty title="最近 7 天没有做过决定" />)}
    </Page>
  )
}

/* ═════════ 事项 ═════════ */
let itemsView: "列表" | "看板" | "关系图" = "列表"; let filt = "全部", q = "", showAll = false, botF = "all", sortBy = "更新时间", tagF = "all", projF = "all"
/** finished and dropped items shown before “显示全部”; the rest are a click away */
const CLOSED_SHOWN = 8
/** a filter select in the toolbar: compact, and muted until it narrows the list */
const FILTER = (set: boolean) => cn("h-7 px-2 text-[12.5px] [&>svg]:opacity-60", set ? "text-foreground" : "text-muted-foreground"); const openGroups = new Set<string>()
const selected = new Set<string>(); let selecting = false
export const showTag = (id: string) => { tagF = id; filt = "全部"; itemsView = "列表"; go("#/items") }
/** plan progress as a count with a short bar; on a finished item only when steps were left open, which is worth a look */
function PlanBar({ i, warn }: { i: Item; warn?: boolean }) {
  if (!i.plan?.length) return null
  const dn = i.plan.filter((x) => x[1] === 1).length
  return <span className={cn("inline-flex items-center gap-1.5 tabular-nums", warn && "text-attention")} title={`计划 ${dn}/${i.plan.length}`} data-plan>{dn}/{i.plan.length}<Progress value={(dn / i.plan.length) * 100} className={cn("hidden w-8 sm:flex [&_[data-slot=progress-track]]:h-1", warn ? "[&_[data-slot=progress-indicator]]:bg-attention" : "[&_[data-slot=progress-indicator]]:bg-foreground/60")} /></span>
}
const PRI_ORDER: Record<string, number> = { 高: 0, 中: 1, 低: 2 }
/** one item in a list. Open items take two lines: the title with what tells it apart on the right (tags, due, plan,
 *  bot, time), then what it is doing now. Finished and dropped items fold into one line, their outcome after the title.
 *  `proj` / `bot` off: the list is already about one project or bot, so the row doesn't repeat it */
function ItemRow({ i, pick, proj = true, bot = true }: { i: Item; pick?: () => void; proj?: boolean; bot?: boolean }) {
  const on = selected.has(i.id), closed = i.st === "done" || i.st === "drop"
  const planOpen = !!i.plan?.length && i.plan.some((x) => x[1] !== 1)
  const extra = [
    i.ss.length > 1 && <span key="ss">{i.ss.length} 个会话</span>,
    i.q.length > 0 && <span key="q">{i.q.length} 条补充排队</span>,
    i.parent && <span key="p" className="inline-flex items-center gap-1"><GitBranchIcon className="size-3" />派生</span>,
    !!i.deps?.length && <span key="d">前置 {i.deps.length - depsOpen(i).length}/{i.deps.length}</span>,
  ].filter(Boolean)
  const side = (
    <span className="ml-auto flex shrink-0 items-center gap-2.5 pl-2 type-footnote text-muted-foreground">
      <Tags ids={i.tags} max={2} className="hidden flex-nowrap sm:inline-flex" />
      {i.pri === "高" && !closed && <span className="rounded bg-attention/12 px-1.5 text-attention">高</span>}
      {i.due && !closed && <span className={cn("whitespace-nowrap", /今天/.test(i.due) && "text-foreground/80")}>截止 {i.due}</span>}
      {proj && i.project && <span className="hidden max-w-28 md:inline-flex"><ProjectChip id={i.project} plain /></span>}
      {(!closed || planOpen) && <PlanBar i={i} warn={closed} />}
      {bot && i.b && <span title={botName(i.b)} className="hidden sm:inline-flex"><BotAvatar k={i.b} size={18} /></span>}
      <span className="min-w-11 whitespace-nowrap text-right tabular-nums">{i.up}</span>
    </span>
  )
  return (
    <div role="button" tabIndex={0} onClick={() => (pick ? pick() : go("#/items/" + i.id))} onKeyDown={(e) => e.key === "Enter" && go("#/items/" + i.id)} data-item-row={i.id} data-dense={closed || undefined}
      className={cn("group flex cursor-pointer gap-2.5 rounded-lg px-2 transition-colors duration-150 hover:bg-accent", closed ? "items-center py-1.5" : "items-start py-2.5", pick && on && "bg-blue/5")}>
      {pick ? <Checkbox checked={on} className={cn(!closed && "mt-[3px]")} aria-label={"选择 " + i.id} data-pick={i.id} /> : <StatusDot s={i.st} className={cn(!closed && "mt-[7px]")} />}
      <div className="min-w-0 flex-1">
        <div className="flex min-w-0 items-center gap-2">
          <ItemNo id={i.id} className="w-12" />
          <span className={cn("truncate type-body", closed ? "text-foreground/85" : "font-medium")}>{i.t}</span>
          {i.star && <StarIcon className="size-3.5 shrink-0 fill-current text-tertiary" />}
          {closed && i.now && <span className="hidden min-w-0 shrink-[2] truncate type-footnote text-muted-foreground sm:block" data-item-now>{i.now}</span>}
          {side}
        </div>
        {!closed && (i.now || extra.length > 0) && (
          <Meta className="mt-0.5 flex-nowrap pl-14 [&>*:first-child]:min-w-0 [&>*:first-child]:truncate"><span className="type-callout" data-item-now>{i.now || "—"}</span>{extra}</Meta>
        )}
        {i.rel && i.rel.tone !== "ok" && <RelNote rel={i.rel} className="mt-1 pl-14" />}
      </div>
    </div>
  )
}
export const ItemRowExport = ItemRow
export function ItemsView() {
  const [, f] = useState(0); const r = () => f((x) => x + 1)
  const [confirmDel, setConfirmDel] = useState<string | null>(null)
  const arch = filt === "已归档"
  let xs = st.items.filter((i) => (arch ? i.arch : !i.arch) && (arch || filt === "全部" || (filt === "等你" && ["wait", "prop"].includes(i.st)) || (filt === "进行中" && i.st === "run") || (filt === "关注" && i.star))
    && (botF === "all" || i.b === botF) && (tagF === "all" || !!i.tags?.includes(tagF)) && (projF === "all" || (projF === "none" ? !i.project : i.project === projF)) && (!q || i.t.includes(q) || i.id.toLowerCase().includes(q.toLowerCase())))
  if (sortBy === "截止时间") xs = [...xs].sort((a, b) => (a.due ? 0 : 1) - (b.due ? 0 : 1))
  if (sortBy === "优先级") xs = [...xs].sort((a, b) => PRI_ORDER[a.pri ?? "中"] - PRI_ORDER[b.pri ?? "中"])
  if (sortBy === "编号") xs = [...xs].sort((a, b) => +b.id.split("-")[1] - +a.id.split("-")[1])
  const groups: [Item["st"], string][] = [["wait", "等你"], ["prop", "提议"], ["run", "进行中"], ["block", "等前置"], ["pause", "已暂停"], ["done", "最近完成"], ["drop", "已放弃"]]
  const simple = (() => { const { N, E } = gBuild(); return gComps(N, E).filter((c) => c.some((k) => N[k].k === "item") && c.length <= 2).length })()
  const seg = <Segmented value={itemsView} onChange={(v) => { itemsView = v; selecting = false; selected.clear(); r() }} options={["列表", "看板", "关系图"] as const} />
  const pick = selecting ? (id: string) => () => { selected.has(id) ? selected.delete(id) : selected.add(id); r() } : () => undefined
  const ids = [...selected]
  const done = () => { selected.clear(); selecting = false; r() }
  /* the project and bot columns only when the visible items differ in them */
  const multiProj = new Set(xs.map((i) => i.project).filter(Boolean)).size > 1, multiBot = new Set(xs.map((i) => i.b).filter(Boolean)).size > 1
  const rowOf = (i: Item) => arch
    ? <div key={i.id} className="flex items-center"><div className="min-w-0 flex-1"><ItemRow i={i} proj={multiProj} bot={multiBot} /></div>
        <Button size="xs" variant="ghost" className="text-muted-foreground" onClick={() => archive([i.id], false)} data-unarchive={i.id}>恢复</Button>
        <Button size="xs" variant="ghost" className="text-destructive" onClick={() => setConfirmDel(i.id)} data-delete-item={i.id}>删除</Button></div>
    : <ItemRow key={i.id} i={i} pick={pick(i.id)} proj={multiProj} bot={multiBot} />
  return (
    <Page title="事项" sub={arch ? "已归档的事项不在列表、看板和关系图里出现" : `${st.items.filter((i) => i.st === "run" && !i.arch).length} 件进行中 · ${pending().length} 件等你`} max={itemsView === "列表" ? 880 : 1120}
      actions={<>{itemsView === "列表" && !arch && <Button size="sm" variant="ghost" className="text-muted-foreground" onClick={() => { selecting = !selecting; selected.clear(); r() }} data-select-mode>{selecting ? "完成" : "选择"}</Button>}<Button size="sm" onClick={() => openSheet("newItem")} data-new-item><PlusIcon />新事项</Button></>}
      toolbar={<span className="hidden sm:block">{seg}</span>}>
      <AlertDialog open={!!confirmDel} onOpenChange={(o) => !o && setConfirmDel(null)}>
        <AlertDialogContent>
          <AlertDialogHeader><AlertDialogTitle>永久删除 {confirmDel}？</AlertDialogTitle><AlertDialogDescription>事项的目标、动态和产出会被删除，编号不再使用。它的会话保留为“未关联”，可以在会话页找到。</AlertDialogDescription></AlertDialogHeader>
          <AlertDialogFooter><AlertDialogCancel>取消</AlertDialogCancel><AlertDialogAction variant="destructive" onClick={() => { deleteItem(confirmDel!); setConfirmDel(null) }} data-delete-confirm>删除</AlertDialogAction></AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
      <div className="mb-4 sm:hidden">{seg}</div>
      <div className="mb-4 flex flex-wrap items-center gap-1.5 border-b pb-3" data-items-toolbar>
        <Chips value={filt} onChange={(v) => { filt = v; done() }} options={["全部", "等你", "进行中", "关注", "已归档"]} />
        <span className="flex-1" />
        <Select quiet className={FILTER(tagF !== "all")} testId="tag-filter" value={tag(tagF) ? tagF : "all"} onChange={(v) => { tagF = v; r() }} options={[{ v: "all", l: "全部标签" }, ...TAG_CATS.flatMap((c) => st.tags.filter((t) => t.cat === c).map((t) => ({ v: t.id, l: <span>{t.name} <span className="text-muted-foreground">· {c}</span></span> })))]} />
        <Select quiet className={FILTER(projF !== "all")} testId="project-filter" value={st.projects.some((p) => p.id === projF) || projF === "none" ? projF : "all"} onChange={(v) => { projF = v; r() }} options={[{ v: "all", l: "全部项目" }, ...st.projects.map((p) => ({ v: p.id, l: p.name })), { v: "none", l: "没有项目" }]} />
        <Select quiet className={FILTER(botF !== "all")} testId="bot-filter" value={botF} onChange={(v) => { botF = v; r() }} options={[{ v: "all", l: "全部 Bot" }, ...Object.keys(st.bots).map((k) => ({ v: k, l: <BotName k={k} /> }))]} />
        {itemsView === "列表" && <Select quiet className={FILTER(sortBy !== "更新时间")} testId="sort" value={sortBy} onChange={(v) => { sortBy = v; r() }} options={["更新时间", "截止时间", "优先级", "编号"].map((v) => ({ v, l: "按" + v }))} />}
        <label className="relative"><SearchIcon className="pointer-events-none absolute left-2.5 top-1/2 size-3.5 -translate-y-1/2 text-tertiary" /><Input className={cn("h-7 rounded-lg pl-8 text-[13px] transition-[width] duration-200", q ? "w-40" : "w-8 cursor-pointer border-transparent bg-transparent placeholder:text-transparent focus:w-40 focus:cursor-text focus:border-border focus:placeholder:text-muted-foreground")} placeholder="搜索标题或编号" aria-label="搜索标题或编号" data-items-search defaultValue={q} onChange={(e) => { q = e.target.value; r() }} /></label>
      </div>
      {selecting && (
        <div className="sticky top-0 z-10 mb-4 flex flex-wrap items-center gap-2 rounded-xl border bg-background/95 px-3 py-2 backdrop-blur" data-bulk-bar>
          <span className="type-callout">已选 {ids.length} 件</span>
          <Button size="xs" variant="ghost" className="text-muted-foreground" onClick={() => { xs.forEach((i) => selected.add(i.id)); r() }}>全选</Button>
          <span className="flex-1" />
          {([["pause", "暂停"], ["run", "继续"], ["arch", "归档"], ["drop", "放弃"]] as const).map(([op, l]) => <Button key={op} size="sm" variant={op === "drop" ? "ghost" : "outline"} className={cn(op === "drop" && "text-destructive")} disabled={!ids.length} onClick={() => { bulk(ids, op); done() }} data-bulk={op}>{l}</Button>)}
        </div>
      )}
      {itemsView === "列表" && (xs.length ? (arch || sortBy !== "更新时间"
        ? <Group>{xs.map(rowOf)}</Group>
        : <div className="flex flex-col gap-5">{groups.map(([s, l]) => {
            const g = xs.filter((i) => i.st === s), fold = (s === "done" || s === "drop") && !openGroups.has(s) && g.length > CLOSED_SHOWN + 2
            return g.length ? (
              <section key={s} data-items-group={s}>
                <SectionLabel className="mb-1 px-0.5" right={fold || openGroups.has(s) ? <Button size="xs" variant="ghost" className="text-muted-foreground" onClick={() => { openGroups.has(s) ? openGroups.delete(s) : openGroups.add(s); r() }} data-group-more={s}>{fold ? `显示全部 ${g.length} 件` : "收起"}</Button> : undefined}>
                  <span className="inline-flex items-center gap-2"><StatusDot s={s} />{l}<span className="tabular-nums text-tertiary">{g.length}</span></span>
                </SectionLabel>
                <Group>{(fold ? g.slice(0, CLOSED_SHOWN) : g).map(rowOf)}</Group>
              </section>) : null })}</div>)
        : <Empty title={arch ? "没有归档的事项" : st.items.length ? "没有符合条件的事项" : "还没有事项"} sub={arch ? (st.cfg.archDays === "0" ? "可以手动归档完成的事项。" : `完成满 ${st.cfg.archDays} 天的事项会自动归档，也可以手动归档。`) : st.items.length ? undefined : "在对话里交代一件事，或者点“新事项”。"} />)}
      {itemsView === "看板" && (
        <div className="-mx-1 grid auto-cols-[minmax(232px,1fr)] grid-flow-col gap-3 overflow-x-auto px-1 pb-2">
          {groups.filter((g) => g[0] !== "drop").map(([s, l]) => { const g = xs.filter((i) => i.st === s); return (
            <div key={s} className="flex min-h-72 flex-col gap-2 rounded-xl bg-muted/60 p-2">
              <div className="flex items-center gap-2 px-2 py-1.5 type-label"><StatusDot s={s} />{l}<span className="tabular-nums text-muted-foreground">{g.length}</span></div>
              {g.map((i) => (
                <button key={i.id} type="button" onClick={() => go("#/items/" + i.id)} className="rounded-lg border bg-card p-3 text-left transition-shadow duration-150 hover:shadow-md" data-kanban-card={i.id}>
                  <ItemNo id={i.id} /><div className="mt-0.5 type-callout font-medium">{i.t}</div><div className="mt-1 line-clamp-2 type-footnote text-muted-foreground">{i.now}</div>
                  <Tags ids={i.tags} max={2} className="mt-2" /><Meta className="mt-2"><BotName k={i.b} />{i.project && <ProjectChip id={i.project} plain />}{i.due && <span>截止 {i.due}</span>}{i.plan && <PlanBar i={i} />}</Meta>
                </button>
              ))}
            </div>) })}
        </div>
      )}
      {itemsView === "关系图" && (
        <div className="flex flex-col gap-3">
          <GraphLegend />
          <RelationGraph scope="all" hideSimple={!showAll} height={560} onOpen={openNode} />
          <div className="flex flex-wrap items-center gap-2 type-footnote text-muted-foreground">{showAll ? "已显示全部事项。" : `另有 ${simple} 个事项只有一个会话、没有交接和派生，默认不画。`}<Button size="xs" variant="outline" onClick={() => { showAll = !showAll; r() }}>{showAll ? "只看有关联的" : "全部显示"}</Button></div>
        </div>
      )}
    </Page>
  )
}

/* ═════════ dialogs ═════════ */
/* shadcn “choice card”: RadioGroup + FieldLabel wrapping a horizontal Field */
export function ChoiceCards<T extends string>({ value, onChange, options, cols = 2 }: { value: T; onChange: (v: T) => void; options: { v: T; t: string; d?: string; test?: string }[]; cols?: number }) {
  return (
    <RadioGroup value={value} onValueChange={(v) => onChange(v as T)} className={cn("gap-2", cols === 2 ? "grid-cols-2" : "grid-cols-1")}>
      {options.map((o) => (
        <FieldLabel key={o.v} htmlFor={"cc-" + o.v} className="rounded-xl! has-data-checked:border-blue! has-data-checked:bg-blue/5!" data-mode={o.test ?? o.v}>
          <Field orientation="horizontal" className="p-3.5!">
            <FieldContent><FieldTitle className="type-callout font-medium">{o.t}</FieldTitle>{o.d && <FieldDescription className="type-footnote">{o.d}</FieldDescription>}</FieldContent>
            <RadioGroupItem value={o.v} id={"cc-" + o.v} />
          </Field>
        </FieldLabel>
      ))}
    </RadioGroup>
  )
}
function Modal({ open, title, desc, children, footer, wide }: { open: boolean; title: string; desc?: string; children: ReactNode; footer?: ReactNode; wide?: boolean }) {
  return (
    <Dialog open={open} onOpenChange={(o) => !o && closeSheet()}>
      <DialogContent className={cn("gap-4 rounded-2xl", wide ? "sm:max-w-[960px]" : "sm:max-w-[480px]")}>
        <DialogHeader><DialogTitle className="type-headline">{title}</DialogTitle>{desc && <DialogDescription className="type-callout">{desc}</DialogDescription>}</DialogHeader>
        {children}
        {footer && <DialogFooter className="gap-2">{footer}</DialogFooter>}
      </DialogContent>
    </Dialog>
  )
}
export function ForkDialog({ k, at }: { k: string; at: number }) {
  const s = st.S[k], e = s?.ev[at]
  const [mode, setMode] = useState<"item" | "sess">("item"), [g, setG] = useState(""), [busy, setBusy] = useState(false)
  useEffect(() => { if (s && !s.loaded) void loadEvents(k) }, [k])
  if (!s) return null
  return (
    <Modal open title="从这条派生" desc="两种方式都带上这条之前的对话，作为新会话的开场。"
      footer={<><Button variant="outline" onClick={closeSheet}>取消</Button><Button data-testid="fork-go" disabled={busy} onClick={async () => { setBusy(true); const to = await doFork(k, at, g.trim(), mode === "sess"); closeSheet(); go(to) }}>{busy && <Spinner className="size-3.5" />}派生</Button></>}>
      {e && <div className="line-clamp-3 rounded-lg bg-muted px-3.5 py-2.5 type-callout text-muted-foreground">{String(e[1])}</div>}
      <FieldGroup className="gap-4">
        <Field><FieldLabel>派生为</FieldLabel>
          <ChoiceCards value={mode} onChange={setMode} cols={s.item ? 2 : 1} options={[{ v: "item", t: "新事项", d: "另做一件事，不影响原事项" }, ...(s.item ? [{ v: "sess" as const, t: "同一事项的新会话", d: "同一件事换条思路" }] : [])]} /></Field>
        <Field><FieldLabel htmlFor="fork-goal">要做什么</FieldLabel>
          <Textarea id="fork-goal" className="min-h-20 rounded-lg" placeholder="例如：把搜索结果的高亮单独做" value={g} onChange={(ev) => setG(ev.target.value)} /></Field>
      </FieldGroup>
    </Modal>
  )
}
export function NewItemDialog({ project: pre }: { project?: string } = {}) {
  const [g, setG] = useState(""), [mode, setMode] = useState<"直接开始" | "先给我方案">("直接开始"), [bot, setBot] = useState("auto"), [proj, setProj] = useState(pre ?? "auto"), [busy, setBusy] = useState(false)
  return (
    <Modal open title="新事项" desc="也可以直接在对话里说。留给 Steve 的部分（Bot、项目、标签）它会按内容判断，建立后可以在事项里更换。"
      footer={<><Button variant="outline" onClick={closeSheet}>取消</Button><Button data-testid="new-go" disabled={!g.trim() || busy} onClick={async () => {
        setBusy(true); const id = await newItem(g.trim(), bot === "auto" ? undefined : bot, proj === "auto" ? undefined : proj === "none" ? null : proj, mode === "先给我方案" ? "propose" : "start")
        setBusy(false); if (id) { closeSheet(); go("#/items/" + id) } }}>{busy && <Spinner className="size-3.5" />}建立</Button></>}>
      <FieldGroup className="gap-4">
        <Field><FieldLabel htmlFor="new-goal">要做什么</FieldLabel><Textarea id="new-goal" autoFocus className="min-h-24 rounded-lg" placeholder="例如：设置页的模型列表改成可折叠" value={g} onChange={(e) => setG(e.target.value)} /></Field>
        <div className="grid grid-cols-2 gap-3">
          <Field><FieldLabel>交给</FieldLabel><Select testId="new-bot" value={bot} onChange={setBot} options={[{ v: "auto", l: "由 Steve 判断" }, ...Object.keys(st.bots).filter((k) => st.bots[k].on).map((k) => ({ v: k, l: <BotName k={k} /> }))]} /></Field>
          <Field><FieldLabel>项目</FieldLabel><Select testId="new-project" value={proj} onChange={setProj} options={[{ v: "auto", l: "由 Steve 判断" }, ...st.projects.map((p) => ({ v: p.id, l: p.name })), { v: "none", l: "不放进项目" }]} /></Field>
        </div>
        <Field><FieldLabel>开始方式</FieldLabel><ChoiceCards value={mode} onChange={setMode} options={[{ v: "直接开始", t: "直接开始", d: "Bot 马上开工" }, { v: "先给我方案", t: "先等我确认", d: "放进待处理，你同意后再开始" }]} /></Field>
      </FieldGroup>
    </Modal>
  )
}
export function GraphDialog({ k }: { k: string }) {
  return <Modal open wide title="关系图"><GraphLegend /><RelationGraph focus={k} scope="comp" height={520} onOpen={(n) => { closeSheet(); openNode(n) }} /></Modal>
}
export function LinkDialog({ k }: { k: string }) {
  return (
    <Modal open title="关联到事项" desc="会话会成为这个事项的下一个会话。">
      <Group>{st.items.filter((i) => !["done", "drop"].includes(i.st)).map((i) => (
        <Row key={i.id} title={i.t} onClick={() => { link(k, i.id); closeSheet() }}><Status s={i.st} /></Row>
      ))}</Group>
    </Modal>
  )
}
/* ═════════ 事项详情 ═════════ */
let dtab: "动态" | "产出" | "关系图" = "动态"; let lastItem = ""; let feedMode: "重要" | "全部" = "全部"
/* system lines worth keeping in the “重要” view; the rest (建立、来自信号…) is shown only in “全部” */
const KEY_SYS = /派生|交付|目标更新|前置|开新会话|交接|自动开始|暂停|放弃|继续|关联|发布|rebase|冲突|失败|回到 /
function Prop({ label, children, top }: { label: string; children: ReactNode; top?: boolean }) {
  return <><dt className={cn("pt-1.5 type-footnote text-muted-foreground", top && "pt-1")}>{label}</dt><dd className="min-w-0 type-callout">{children}</dd></>
}
/* add a tag: search existing ones by name, category or description; create a new one when nothing fits */
function TagPicker({ i }: { i: Item }) {
  const [q, setQ] = useState("")
  const has = new Set(i.tags ?? [])
  const xs = st.tags.filter((t) => !has.has(t.id) && (!q || [t.name, t.cat, t.desc].some((v) => v.toLowerCase().includes(q.toLowerCase()))))
  return (
    <DropdownMenu onOpenChange={(o) => !o && setQ("")}>
      <DropdownMenuTrigger render={<button type="button" className="self-start type-footnote text-muted-foreground hover:text-foreground" data-add-tag />}>{(i.tags ?? []).length ? "+ 添加" : "无 · 添加"}</DropdownMenuTrigger>
      <DropdownMenuContent align="start" className="max-h-80 w-72">
        <div className="p-1"><Input autoFocus className="h-8 rounded-md text-[13px]" placeholder="搜索名称、分类或描述" value={q} onChange={(e) => setQ(e.target.value)} onKeyDown={(e) => e.stopPropagation()} data-tag-search /></div>
        {TAG_CATS.map((c) => { const g = xs.filter((t) => t.cat === c); return g.length ? <div key={c}><div className="px-2 pb-0.5 pt-2 type-footnote text-muted-foreground">{c}</div>
          {g.map((t) => <DropdownMenuItem key={t.id} onClick={() => setTags(i.id, [...(i.tags ?? []), t.id])} data-tag-opt={t.name} className="flex-col items-start gap-0"><span className="type-callout">{t.name}</span><span className="line-clamp-1 type-footnote text-muted-foreground">{t.desc}</span></DropdownMenuItem>)}</div> : null })}
        {!xs.length && <div className="px-2 py-2 type-footnote text-muted-foreground">没有匹配的标签</div>}
        <DropdownMenuSeparator />
        <DropdownMenuItem onClick={() => openSheet("tag", { item: i.id, name: q })} data-new-tag><PlusIcon />新建标签{q && `「${q}」`}</DropdownMenuItem>
      </DropdownMenuContent>
    </DropdownMenu>
  )
}
function OutPreview({ o }: { o: [string, string, string | null] }) {
  const [k, v, loc] = o
  const url = loc && /^https?:\/\//.test(loc)
  return (
    <div className="flex flex-col gap-3" data-out-preview>
      <dl className="grid grid-cols-[56px_1fr] gap-x-3 gap-y-1.5 type-callout"><dt className="text-muted-foreground">种类</dt><dd>{k}</dd><dt className="text-muted-foreground">名称</dt><dd className="break-words">{v}</dd>
        {loc && <><dt className="text-muted-foreground">位置</dt><dd className="break-all font-mono text-[12.5px]">{loc}</dd></>}</dl>
      {loc && <div className="flex gap-2">{url && <Button variant="outline" size="sm" onClick={() => window.open(loc, "_blank", "noopener")}>打开</Button>}
        <Button variant="ghost" size="sm" onClick={() => { navigator.clipboard?.writeText(loc).catch(() => {}); toast("已复制位置") }}><CopyIcon />复制位置</Button></div>}
    </div>
  )
}
/* the plan: earlier rounds folded above the current one; a replaced round is struck through, its open steps abandoned */
function PlanSteps({ steps, run, old }: { steps: [string, number][]; run?: boolean; old?: PlanRound["end"] }) {
  const next = old ? -1 : steps.findIndex((x) => !x[1])
  return (
    <ol className="flex flex-col gap-1.5 pl-0.5">
      {steps.map(([l, d], n) => {
        const gone = old === "replaced" && !d
        return (
          <li key={n} className="flex items-start gap-2.5 type-callout" data-step={d === 1 ? "done" : d === 2 ? "left" : gone ? "abandoned" : "open"}>
            <span className="flex h-[1lh] shrink-0 items-center">
              {d === 1 ? <CheckIcon className={cn("size-4", old ? "text-muted-foreground" : "text-success")} strokeWidth={2} />
                : d === 2 || gone ? <MinusIcon className="size-4 text-tertiary" strokeWidth={2} />
                : run && n === next ? <Spinner className="size-4 text-blue" /> : <span className="mx-[3px] size-2.5 rounded-full border border-tertiary" />}
            </span>
            <span className={cn("min-w-0 break-words", gone ? "text-tertiary line-through decoration-tertiary" : n === next && run ? "text-foreground" : "text-muted-foreground")}>{l}</span>
            {d === 2 && <span className="shrink-0 pt-px type-footnote text-tertiary">没做</span>}
          </li>
        )
      })}
    </ol>
  )
}
function Plan({ i }: { i: Item }) {
  const cur = i.plan ?? [], dn = cur.filter((x) => x[1] === 1).length
  return (
    <div className="mt-3 flex flex-col gap-2" data-plan>
      {i.rounds.map((r) => { const k = r.steps.filter((x) => x[1] === 1).length; return (
        <Collapsible key={r.n} data-plan-round={r.end}>
          <CollapsibleTrigger className="group/round flex items-center gap-1.5 type-footnote text-muted-foreground hover:text-foreground">
            <ChevronRightIcon className="size-3.5 transition-transform group-data-[panel-open]/round:rotate-90" />
            <span className={cn(r.end === "replaced" && "line-through decoration-tertiary")}>第 {r.n} 轮计划</span>
            <span>· {r.end === "done" ? `已完成 ${k}/${r.steps.length}` : `已废弃，完成 ${k}/${r.steps.length}`}</span>
            <span className="text-tertiary">· {r.at}</span>
          </CollapsibleTrigger>
          <CollapsibleContent className="mt-1.5 pl-5"><PlanSteps steps={r.steps} old={r.end} /></CollapsibleContent>
        </Collapsible>) })}
      {i.rounds.length > 0 && cur.length > 0 && <div className="mt-1 type-footnote text-muted-foreground">第 {i.rounds.length + 1} 轮计划 · 当前 · 完成 {dn}/{cur.length}</div>}
      {cur.length > 0 && <PlanSteps steps={cur} run={i.st === "run"} />}
    </div>
  )
}
export function ItemView({ id }: { id: string }) {
  const [, f] = useState(0); const r = () => f((x) => x + 1)
  const [target, setTarget] = useState<"自动排队" | "插话" | "新会话">("自动排队")
  const [editing, setEditing] = useState(false), [gtxt, setG] = useState("")
  const [confirmDrop, setConfirmDrop] = useState(false), [confirmDel, setConfirmDel] = useState(false)
  const [doneBusy, setDoneBusy] = useState(false)
  const itemComposer = useRef<ComposerHandle | null>(null)
  const [older, setOlder] = useState(false), [titleEdit, setTitleEdit] = useState(false), [preview, setPreview] = useState<[string, string, string | null] | null>(null)
  if (lastItem !== id) { lastItem = id; dtab = "动态" }
  useEffect(() => { void loadItem(id, true) }, [id])
  const i = st.items.find((x) => x.id === id)
  /* replies to comments are read from the sessions' events */
  const sids = i?.ss.join(",") ?? ""
  useEffect(() => { if (sids) for (const k of sids.split(",")) void loadEvents(k) }, [sids])
  if (!i) return <Page title="事项" back={["#/items", "事项"]}><Empty title="事项不存在" sub="可能在别的工作空间里，或者已经删除。" /></Page>
  const dependents = st.items.filter((x) => i.dependents.includes(x.id) || (x.deps || []).includes(i.id))
  const a = active(i), B = st.bots[i.b]
  const targets = (a && a.st === "run" ? ["自动排队", "插话", "新会话"] : ["自动排队", "新会话"]) as ("自动排队" | "插话" | "新会话")[]
  /* activity feed: filter, then fold everything but the latest four entries */
  const feed = i.tl.map((e, n) => ({ e, n })).filter(({ e }) => feedMode === "全部" || e[0] !== "sys" || KEY_SYS.test(e[1]))
  const KEEP = 4, hidden = feedMode === "重要" && !older ? Math.max(0, feed.length - KEEP) : 0
  const actions = <>
    <Tooltip><TooltipTrigger render={<Button variant="ghost" size="icon-sm" aria-label="关注" data-star onClick={() => star(i.id)} className="text-muted-foreground" />}><StarIcon className={cn(i.star && "fill-current text-foreground")} /></TooltipTrigger><TooltipContent>{i.star ? "取消关注" : "关注"}</TooltipContent></Tooltip>
    <DropdownMenu>
      <DropdownMenuTrigger render={<Button variant="ghost" size="icon-sm" aria-label="更多" className="text-muted-foreground" />}><MoreHorizontalIcon /></DropdownMenuTrigger>
      <DropdownMenuContent align="end" className="min-w-40">
        <DropdownMenuItem onClick={() => { setG(i.goals[i.goals.length - 1]); setEditing(true) }}>修改目标</DropdownMenuItem>
        <DropdownMenuItem onClick={() => setTitleEdit(true)}>改标题</DropdownMenuItem>
        <DropdownMenuItem onClick={() => openSheet("graph", { k: id })}>查看关系图</DropdownMenuItem>
        <DropdownMenuItem onClick={() => { const k = i.ss[i.ss.length - 1]; openSheet("fork", { k, at: Math.max(0, (st.S[k]?.seqs.length ?? 1) - 1) }) }} disabled={!i.ss.length}>派生新事项</DropdownMenuItem>
        <DropdownMenuItem onClick={() => { navigator.clipboard?.writeText(location.href).catch(() => {}); toast("已复制链接") }}><LinkIcon />复制链接</DropdownMenuItem>
        <DropdownMenuSeparator />
        {["done", "drop"].includes(i.st) && <DropdownMenuItem onClick={() => { archive([i.id], !i.arch); if (!i.arch) return; go("#/items") }} data-archive><ArchiveIcon />{i.arch ? "恢复" : "归档"}</DropdownMenuItem>}
        {i.arch && <DropdownMenuItem variant="destructive" onClick={() => setConfirmDel(true)} data-delete><TrashIcon />永久删除</DropdownMenuItem>}
      </DropdownMenuContent>
    </DropdownMenu>
  </>
  /* always-visible quick actions beside the title; labels fold into icons on narrow screens */
  const over = ["done", "drop"].includes(i.st)
  const [pl, PI, pa] = over ? ["重新打开", RotateCcwIcon, () => setSt(i.id, "run")] as const
    : i.st === "pause" ? ["继续", PlayIcon, () => setSt(i.id, "run")] as const
    : ["暂停", PauseIcon, () => setSt(i.id, "pause")] as const
  const quick = (
    <div className="flex items-center gap-1.5" data-quick-actions>
      <Button variant="outline" size="sm" className="max-sm:w-8 max-sm:px-0" aria-label={pl} title={pl} disabled={i.st === "prop"} onClick={pa} data-quick="pause"><PI /><span className="max-sm:sr-only">{pl}</span></Button>
      <Button variant="outline" size="sm" className="max-sm:w-8 max-sm:px-0" aria-label="标记完成" title="标记完成" disabled={over || doneBusy} onClick={() => { setDoneBusy(true); void markDone(i.id).finally(() => setDoneBusy(false)) }} data-quick="done"><CircleCheckIcon /><span className="max-sm:sr-only">标记完成</span></Button>
      <Popover open={confirmDrop} onOpenChange={setConfirmDrop}>
        <PopoverTrigger render={<Button variant="destructive" size="sm" className="max-sm:w-8 max-sm:px-0" aria-label="放弃" title="放弃" disabled={over} data-quick="drop" />}><BanIcon /><span className="max-sm:sr-only">放弃</span></PopoverTrigger>
        <PopoverContent align="end" className="w-64 gap-3 rounded-2xl p-3" data-drop-popover>
          <div className="type-callout"><div className="font-medium">放弃 {i.id}？</div><div className="mt-0.5 text-muted-foreground">正在运行的会话会停止，排队的补充不再送达。之后可以重新打开。</div></div>
          <div className="flex justify-end gap-2"><Button size="sm" variant="ghost" onClick={() => setConfirmDrop(false)}>取消</Button><Button size="sm" variant="destructive" autoFocus onClick={() => { setSt(i.id, "drop"); setConfirmDrop(false) }} data-drop-confirm>放弃</Button></div>
        </PopoverContent>
      </Popover>
    </div>
  )
  const entry = ({ e, n }: { e: any[]; n: number }) => {
    if (e[0] === "sess") return <SessionBlock key={e[1]} k={e[1]} defaultOpen={false} />
    if (e[0] === "me") return (
      <Fragment key={n}><div className="flex items-start gap-2 py-1.5" data-comment><span className="flex size-5 shrink-0 items-center justify-center rounded-full bg-muted text-[10px] font-medium text-muted-foreground">我</span>
        <div className="min-w-0">{(e[1] !== "（附件）" || !e[5]?.length) && <div className="type-body"><Markdown text={e[1]} /></div>}<Files files={e[5]} className="my-1" /><div className="type-footnote text-muted-foreground">{e[2]}{e[3] && st.S[e[3]] ? ` · 已送达会话 #${st.S[e[3]].n}` : ""}{e[6] === "thread" && <a href="#/" className="ml-1.5 rounded bg-muted px-1.5 hover:text-foreground" data-from-thread>来自对话</a>}</div></div></div>
        {e[4] && <ReplyBlock r={e[4]} />}</Fragment>
    )
    const yolo = typeof e[1] === "string" && e[1].startsWith("YOLO")
    return <div key={n} className="flex items-center gap-2 py-1 type-footnote text-muted-foreground" data-yolo-line={yolo ? "" : undefined}><span className="flex size-5 shrink-0 items-center justify-center"><span className="size-1 rounded-full bg-tertiary" /></span>{yolo && <YoloMark />}<span className="min-w-0 truncate">{yolo ? e[1].replace(/^YOLO[：:]\s*/, "") : e[1]}</span><span className="shrink-0 text-tertiary">{e[2]}</span></div>
  }
  return (
    <Page small title={titleEdit ? <Input autoFocus defaultValue={i.t} className="h-9 rounded-lg text-[20px] font-semibold" data-title-input
        onBlur={(e) => { const v = e.target.value.trim(); if (v && v !== i.t) patchItem(i.id, { t: v }, "标题改为「" + v + "」"); setTitleEdit(false) }} onKeyDown={(e) => { if (e.key === "Enter") (e.target as HTMLInputElement).blur(); if (e.key === "Escape") setTitleEdit(false) }} />
      : <span className="cursor-text" onDoubleClick={() => setTitleEdit(true)} data-title>{i.t}</span>} back={["#/items", "事项"]} actions={actions} toolbar={quick} toolbarTop max={1080}
      footer={<div className="w-full shrink-0 border-t bg-background/85 backdrop-blur-xl"><div className="mx-auto w-full px-4 pb-[76px] pt-3 sm:px-8 md:pb-4" style={{ maxWidth: 1080 + 64 }}><div className="lg:pr-[296px]">
        {i.qs && <QueueStrip sid={i.qs} rows={i.q} prefix="补充信息：" composer={itemComposer} note={a ? queueNote(st.S[i.qs] ?? null) : "将开新会话送达"} />}
        <Composer draftKey={draftKey(st.workspace?.id, "item", i.id)} handle={itemComposer} placeholder={`补充给 ${i.id}…  输入 # 引用其他事项`} testId="item" onSend={(t, files) => supp(i.id, t, target === "插话" ? "steer" : target === "新会话" ? "new" : "auto", files)}
          left={<Segmented size="sm" value={targets.includes(target) ? target : "自动排队"} onChange={setTarget} options={targets} />} />
      </div></div></div>}
      sub={<Meta><button type="button" className="inline-flex items-center gap-1 font-mono tabular-nums hover:text-foreground" data-copy-id onClick={() => { navigator.clipboard?.writeText(i.id).catch(() => {}); toast(`已复制 ${i.id}`) }}>{i.id}<CopyIcon className="size-3" /></button><Status s={i.st} /><ProjectChip id={i.project} /><span>更新于 {i.up}</span></Meta>}>
      {st.yolo && !over && <div className="-mt-3 mb-5"><YoloItemSwitch i={i} /></div>}
      <AlertDialog open={confirmDel} onOpenChange={setConfirmDel}>
        <AlertDialogContent>
          <AlertDialogHeader><AlertDialogTitle>永久删除 {i.id}？</AlertDialogTitle><AlertDialogDescription>目标、动态和产出会被删除，编号不再使用。它的 {i.ss.length} 个会话保留为“未关联”。</AlertDialogDescription></AlertDialogHeader>
          <AlertDialogFooter><AlertDialogCancel>取消</AlertDialogCancel><AlertDialogAction variant="destructive" onClick={() => { go("#/items"); deleteItem(i.id) }}>删除</AlertDialogAction></AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
      <Dialog open={!!preview} onOpenChange={(o) => !o && setPreview(null)}>
        <DialogContent className="gap-3 rounded-2xl sm:max-w-[640px]" data-preview>
          <DialogHeader><DialogTitle className="type-headline">{preview?.[1]}</DialogTitle><DialogDescription className="type-callout">{preview?.[0]} · 来自 {i.id}</DialogDescription></DialogHeader>
          {preview && <OutPreview o={preview} />}
        </DialogContent>
      </Dialog>
      {i.arch && <div className="mb-6 flex items-center gap-3 rounded-xl border px-4 py-2.5 type-callout" data-arch-banner><ArchiveIcon className="size-4 text-muted-foreground" /><span className="flex-1">这个事项已归档</span><Button size="sm" variant="ghost" onClick={() => archive([i.id], false)}>恢复</Button></div>}
      <div className="grid gap-12 lg:grid-cols-[minmax(0,1fr)_248px]">
        <div className="flex min-w-0 flex-col gap-10">
          {/* 1 — goal reads as the description of the title */}
          <section data-goal className="-mt-3">
            {editing ? (
              <Field><Textarea className="min-h-24 rounded-lg" value={gtxt} onChange={(e) => setG(e.target.value)} data-goal-input autoFocus />
                <div className="flex flex-wrap items-center gap-2"><Button size="sm" onClick={() => { saveGoal(i.id, gtxt.trim()); setEditing(false) }}>保存为 v{i.goals.length + 1}</Button><Button size="sm" variant="ghost" onClick={() => setEditing(false)}>取消</Button><FieldDescription className="type-footnote">改动较大时会带交接摘要开新会话</FieldDescription></div></Field>
            ) : (
              <div className="group/goal">
                <Markdown text={i.goals[i.goals.length - 1]} className="type-body text-foreground/80" />
                <div className="mt-1 flex items-center gap-2 type-footnote text-muted-foreground">
                  <span>目标 v{i.goals.length}</span>
                  {i.goals.length > 1 && <Collapsible><CollapsibleTrigger className="hover:text-foreground">· 历史版本</CollapsibleTrigger><CollapsibleContent className="mt-1">{i.goals.slice(0, -1).map((g, n) => <div key={n}>v{n + 1}：{g}</div>)}</CollapsibleContent></Collapsible>}
                  <button type="button" className="opacity-0 transition-opacity hover:text-foreground focus-visible:opacity-100 group-hover/goal:opacity-100 [@media(hover:none)]:opacity-100" data-edit-goal onClick={() => { setG(i.goals[i.goals.length - 1]); setEditing(true) }}>· 修改</button>
                </div>
              </div>
            )}
          </section>
          {/* 2 — what needs you is the only boxed element on the page */}
          {i.waits.length > 0 && <section className="flex flex-col gap-3">{i.waits.map((w) => <DecisionCard key={w.decision} i={i} w={w} showItem={false} />)}</section>}
          <section data-now>
            <div className="mb-1.5 flex flex-wrap items-center gap-x-1 gap-y-1 type-footnote" data-stage={stageOf(i)}>
              {STAGES.slice(2).map((g, n, arr) => { const cur = STAGES.indexOf(stageOf(i) as any), me = STAGES.indexOf(g); return (
                <span key={g} className="inline-flex items-center gap-1"><span className={cn(me === cur ? "font-medium text-foreground" : me < cur ? "text-muted-foreground" : "text-tertiary")}>{g}</span>{n < arr.length - 1 && <ChevronRightIcon className="size-3 text-tertiary" />}</span>) })}
            </div>
            <div className="flex items-center gap-2"><StatusDot s={i.st} /><span className={cn("type-headline", i.st === "run" && "shimmer shimmer-once")}>{i.now}</span></div>
            {i.rel && <RelNote rel={i.rel} box={i.rel.tone !== "ok"} className="mt-2" />}
            {(i.plan || i.rounds.length > 0) && <Plan i={i} />}
          </section>
          {/* 3 — activity, newest at the bottom, older entries folded */}
          <section className="flex flex-col gap-3">
            <div className="flex flex-wrap items-center justify-between gap-2 border-b pb-2">
              <Segmented size="sm" value={dtab} onChange={(v) => { dtab = v; r() }} options={["动态", "产出", "关系图"] as const} />
              {dtab === "动态" && <Chips value={feedMode} onChange={(v) => { feedMode = v; setOlder(false); r() }} options={["重要", "全部"] as const} />}
            </div>
            {dtab === "动态" && (
              <div className="flex flex-col" data-worklog>
                {hidden > 0 && <button type="button" onClick={() => setOlder(true)} className="mb-1 flex items-center gap-2 self-start py-1 type-footnote text-muted-foreground hover:text-foreground" data-older><ChevronDownIcon className="size-3.5" />更早的 {hidden} 条动态</button>}
                {feed.slice(hidden).map(entry)}
              </div>
            )}
            {dtab === "产出" && (i.outs.length ? <Group>{i.outs.map((o, n) => <Row key={n} title={<span className="break-words font-mono text-[13px]">{o[1]}</span>} sub={o[2] ?? undefined} onClick={() => setPreview(o)} data-out={n}><span className="type-footnote text-muted-foreground">{o[0]}</span><ChevronRightIcon className="size-4 text-tertiary" /></Row>)}</Group> : <Empty title="还没有产出" sub="Bot 交付时附上的分支、提交、文件和文档会出现在这里。" />)}
            {dtab === "关系图" && <div className="flex flex-col gap-3"><GraphLegend /><RelationGraph key={id} focus={id} scope="comp" height={460} onOpen={openNode} /></div>}
          </section>
        </div>
        {/* properties: one aligned label column, controls stay quiet until hovered */}
        <aside className="flex flex-col gap-8">
          <dl className="grid grid-cols-[64px_minmax(0,1fr)] items-start gap-x-3 gap-y-2.5">
            <Prop label="执行"><Select quiet testId="bot" value={i.b} onChange={(k) => reassign(i.id, k)} options={Object.keys(st.bots).filter((k) => st.bots[k].on || k === i.b).map((k) => ({ v: k, l: <BotName k={k} /> }))} />{B && <div className="pl-2 type-footnote text-muted-foreground">{B.h} · {B.m}</div>}</Prop>
            <ItemPeopleProp id={i.id} Prop={Prop} />
            <Prop label="标签"><div className="flex flex-col gap-1.5 pt-1" data-item-tags>
              <Tags ids={i.tags} onRemove={(t) => setTags(i.id, (i.tags ?? []).filter((x) => x !== t))} />
              <TagPicker i={i} /></div></Prop>
            <Prop label="优先级"><Select quiet testId="pri" value={i.pri ?? "中"} onChange={(v) => patchItem(i.id, { pri: v as Item["pri"] })} options={["高", "中", "低"].map((v) => ({ v, l: v }))} /></Prop>
            <Prop label="截止"><Select quiet testId="due" value={i.due ?? "无"} onChange={(v) => patchItem(i.id, { due: v === "无" ? undefined : v }, v === "无" ? "移除截止时间" : "截止时间设为 " + v)} options={[...new Set(["无", i.due ?? "无", "今天 18:00", "明天", "周五", "下周一"])].map((v) => ({ v, l: v }))} /></Prop>
            <Prop label="验收"><Select quiet testId="acc" value={i.acc} onChange={(v) => setAcc(i.id, v)} options={["跟随全局", "需要你验收", "自动验收"].map((v) => ({ v, l: v }))} />{i.acc === "跟随全局" && <div className="pl-2 type-footnote text-muted-foreground">生效：{accOf(i)}</div>}</Prop>
            <Prop label="前置">
              <div className="flex flex-col gap-1 pt-1" data-deps>
                {(i.deps || []).filter((d) => st.items.some((x) => x.id === d)).map((d) => { const x = it(d); return (
                  <div key={d} className="group/dep flex items-center gap-2"><StatusDot s={x.st} /><a href={"#/items/" + d} className="min-w-0 flex-1 truncate hover:underline"><span className="font-mono text-[12px] tabular-nums text-muted-foreground">{d}</span> {x.t}</a>
                    <button type="button" aria-label={"移除前置 " + d} onClick={() => rmDep(i.id, d)} className="text-tertiary opacity-0 transition-opacity hover:text-foreground group-hover/dep:opacity-100 [@media(hover:none)]:opacity-100"><XIcon className="size-3.5" /></button></div>) })}
                <DropdownMenu>
                  <DropdownMenuTrigger render={<button type="button" className="self-start type-footnote text-muted-foreground hover:text-foreground" data-add-dep />}>{(i.deps || []).length ? "+ 添加" : "无 · 添加"}</DropdownMenuTrigger>
                  <DropdownMenuContent align="start" className="max-h-72 min-w-64">
                    {st.items.filter((x) => x.id !== i.id && !(i.deps || []).includes(x.id) && x.st !== "drop").map((x) => <DropdownMenuItem key={x.id} onClick={() => addDep(i.id, x.id)} data-dep-opt={x.id}><span className="font-mono text-[12px] text-muted-foreground">{x.id}</span><span className="truncate">{x.t}</span></DropdownMenuItem>)}
                  </DropdownMenuContent>
                </DropdownMenu>
                {i.st === "block" && <span className="type-footnote text-muted-foreground">还差 {depsOpen(i).length} 件，完成后自动开始</span>}
              </div>
            </Prop>
            {dependents.length > 0 && <Prop label="后续"><div className="flex flex-col gap-1 pt-1">{dependents.map((x) => <a key={x.id} href={"#/items/" + x.id} className="flex items-center gap-2 hover:underline"><StatusDot s={x.st} /><span className="min-w-0 truncate"><span className="font-mono text-[12px] tabular-nums text-muted-foreground">{x.id}</span> {x.t}</span></a>)}</div></Prop>}
            <Prop label="会话"><div className="flex flex-col gap-1 pt-1">{i.ss.filter((k) => st.S[k]).length ? i.ss.filter((k) => st.S[k]).map((k) => <a key={k} href={"#/sessions/" + k} className="flex items-center gap-2 hover:underline"><span>#{st.S[k].n}</span><Status s={st.S[k].st} sess /><span className="ml-auto type-footnote tabular-nums text-muted-foreground">{st.S[k].ctx ? st.S[k].ctx + "%" : ""}</span></a>) : <span className="text-muted-foreground">暂无</span>}</div></Prop>
            <ItemStatsRows id={i.id} render={(u, t, href) => <><Prop label="用量"><a href={href} className="block pt-1 hover:underline" data-item-usage>{u}</a></Prop><Prop label="用时"><a href={href} className="block pt-1 hover:underline" data-item-time>{t}</a></Prop></>} />
            <Prop label="项目"><Select quiet testId="project" value={i.project ?? "none"} onChange={(v) => patchItem(i.id, { project: v === "none" ? null : v })} options={[{ v: "none", l: "无" }, ...st.projects.map((p) => ({ v: p.id, l: p.name }))]} />
              {i.projNext && <div className="pl-2 type-footnote text-muted-foreground">下一个会话起生效</div>}</Prop>
            {i.cwd && <Prop label="目录"><span className="block break-all pt-1 font-mono text-[12px] text-muted-foreground" data-cwd>{i.cwd.replace(/^\/Users\/[^/]+/, "~")}</span></Prop>}
            {i.branch && <Prop label="分支"><span className="block break-all pt-1 font-mono text-[12px]" data-branch>{i.branch}</span></Prop>}
            <Prop label="来源"><span className="block pt-1">{i.parent?.item ? <a href={"#/items/" + i.parent.item} className="hover:underline">派生自 {i.parent.item}</a> : i.src[1] === "chat" ? <a href="#/" className="hover:underline">对话</a> : i.src[0]}</span>
              {itemThread(i.id) && <label className="mt-1.5 flex items-center gap-2 pl-0.5 type-footnote text-muted-foreground" data-thread-follow><Switch size="sm" checked={!itemThread(i.id)!.follow} onCheckedChange={(v) => void threadFollow(i.id, !v)} />不在对话里跟进</label>}</Prop>
          </dl>
        </aside>
      </div>
    </Page>
  )
}

/* ═════════ 会话 ═════════ */
let sf = "全部", sq = "", hf = "all"
export function SessionsView() {
  const [, f] = useState(0); const r = () => f((x) => x + 1)
  const [hits, setHits] = useState<Set<string> | null>(null)
  useEffect(() => { if (!sq) { setHits(null); return } const t = setTimeout(() => api.searchSessions(sq).then((xs) => setHits(new Set(xs))).catch(() => setHits(new Set())), 250); return () => clearTimeout(t) }, [sq])
  const all = Object.keys(st.S).map((k) => [k, st.S[k]] as const)
  const xs = all.filter(([k, s]) => (hf === "all" || s.h === hf) && (sf === "全部" || (sf === "运行中" && s.st === "run") || (sf === "未关联" && !s.item) || (sf === "导入" && !!s.ro))
    && (!sq || s.title.includes(sq) || (s.item ?? "").toLowerCase().includes(sq.toLowerCase()) || !!hits?.has(k)))
  return (
    <Page title="会话" sub={`${all.length} 个 · ${all.filter((x) => x[1].st === "run").length} 个运行中`} actions={<Button size="sm" variant="outline" onClick={() => openSheet("import")} data-import><UploadIcon />导入本机会话</Button>}>
      <label className="relative mb-3 block"><SearchIcon className="pointer-events-none absolute left-3 top-1/2 size-4 -translate-y-1/2 text-tertiary" />
        <Input className="h-10 rounded-xl bg-muted/70 pl-9 text-[14px]" placeholder="搜索消息、思考和工具输出" defaultValue={sq} onChange={(e) => { sq = e.target.value.trim(); r() }} data-session-search /></label>
      <div className="mb-5 flex flex-wrap items-center gap-2"><Chips value={sf} onChange={(v) => { sf = v; r() }} options={["全部", "运行中", "未关联", "导入"]} /><span className="flex-1" />
        <Select testId="harness-filter" value={hf} onChange={(v) => { hf = v; r() }} options={[{ v: "all", l: "全部 harness" }, ...st.runtimes.map((x) => ({ v: x.name, l: x.name }))]} /></div>
      {xs.length ? (
        <Group>{xs.map(([k, s]) => (
          <div key={k} role="button" tabIndex={0} onClick={() => go("#/sessions/" + k)} data-session-row={k} className="flex cursor-pointer items-start gap-3 rounded-lg px-2 py-3 transition-colors hover:bg-accent">
            <StatusDot s={s.st} className="mt-[7px]" />
            <div className="min-w-0 flex-1">
              <div className="truncate type-body font-medium">{s.title}</div>
              <Meta className="mt-0.5">{s.item ? <span className="max-w-[300px] truncate"><span className="font-mono">{s.item}</span> · #{s.n}</span> : <span>未关联事项</span>}{s.ro && <Badge variant="outline" className="h-4.5 font-normal text-muted-foreground"><LockIcon />只读</Badge>}<span>{hm(s)}</span><span>{st.devices.find((d) => d.id === s.device)?.name ?? "Hub"}</span><span>{s.src}</span>{s.ctx > 0 && <span>上下文 {s.ctx}%</span>}</Meta>
              {sq && hits?.has(k) && <div className="mt-1 type-footnote text-muted-foreground">内容里有“{sq}”</div>}
            </div>
            <span className="shrink-0 type-footnote text-muted-foreground">{s.up}</span>
          </div>
        ))}</Group>
      ) : <Empty title={all.length ? "没有匹配的会话" : "还没有会话"} sub={all.length ? undefined : "事项开始后，每个会话都会出现在这里；也可以导入本机的 Claude Code、Codex 历史会话。"} />}
    </Page>
  )
}

export function SessionView({ k }: { k: string }) {
  const [mode, setMode] = useState<"插话" | "排队">("插话"), [renaming, setRenaming] = useState(false)
  const [infoOpen, setInfoOpen] = useState(false)
  const composer = useRef<ComposerHandle | null>(null)
  const s = st.S[k]
  const scroller = useRef<HTMLDivElement>(null)
  useEffect(() => { void loadEvents(k) }, [k])
  useEffect(() => { scroller.current?.scrollTo({ top: 1e9 }) }, [s?.ev.length, k])
  if (!s) return <Page title="会话" back={["#/sessions", "会话"]}><Empty title="会话不存在" /></Page>
  const i = s.item ? it(s.item) : null, run = s.st === "run"
  const exportMd = () => {
    const md = `# ${s.title}\n\n` + s.ev.map((e) => e[0] === "u" ? "**你**：" + e[1] : e[0] === "hi" ? "> " + e[1] : e[0] === "a" ? e[1] : e[0] === "tool" ? "- " + e[2] + " " + e[3] : e[0] === "sys" ? "_" + e[1] + "_" : "").filter(Boolean).join("\n\n")
    const a = document.createElement("a"); a.href = URL.createObjectURL(new Blob([md], { type: "text/markdown" })); a.download = `${s.title.slice(0, 40)}.md`; a.click(); toast("已导出 Markdown")
  }
  const menu = (
    <DropdownMenu>
      <DropdownMenuTrigger render={<Button variant="ghost" size="icon-sm" aria-label="更多" className="text-muted-foreground" data-session-menu />}><MoreHorizontalIcon /></DropdownMenuTrigger>
      <DropdownMenuContent align="end" className="min-w-40">
        {!s.ro && <DropdownMenuItem onClick={() => setRenaming(true)} data-rename><PencilIcon />重命名</DropdownMenuItem>}
        <DropdownMenuItem onClick={exportMd} data-export><DownloadIcon />导出 Markdown</DropdownMenuItem>
        {!s.ro && s.st === "idle" && <DropdownMenuItem onClick={() => sessPatch(k, { st: "ended" }, "会话已结束，发送消息可以恢复")} data-end><SquareIcon />结束会话</DropdownMenuItem>}
        {s.ro && <><DropdownMenuSeparator /><DropdownMenuItem variant="destructive" onClick={() => { go("#/sessions"); sessRemove(k) }} data-remove-import><TrashIcon />从 Steve 移除</DropdownMenuItem></>}
      </DropdownMenuContent>
    </DropdownMenu>
  )
  const ctx = (
    <HoverCard>
      <HoverCardTrigger render={<button type="button" className="rounded-full px-2 py-1 type-footnote tabular-nums text-muted-foreground hover:bg-accent" />}>上下文 {s.contextKnown ? `${s.ctx}%` : "未上报"}</HoverCardTrigger>
      <HoverCardContent className="w-60"><div className="type-label">上下文</div>{s.contextKnown && <Progress value={Math.min(100, s.ctx)} className="my-2" />}<div className="type-footnote text-muted-foreground">{s.contextKnown ? `已用 ${s.ctx}%。超过 80% 时，新补充会带交接摘要开新会话。` : "当前 harness 尚未上报上下文用量。"}</div></HoverCardContent>
    </HoverCard>
  )
  const panel = (
    <div className="flex flex-col gap-6">
      {i ? (
        <div><SectionLabel>所属事项</SectionLabel>
          <button type="button" onClick={() => go("#/items/" + i.id)} className="-mx-2 w-[calc(100%+16px)] rounded-lg px-2 py-1.5 text-left transition-colors hover:bg-accent"><div className="type-callout font-medium"><span className="mr-1.5 font-mono text-[12px] font-normal text-muted-foreground">{i.id}</span>{i.t}</div><Status s={i.st} className="mt-1" /></button>
          <div className="mt-1 px-1 type-callout text-muted-foreground"><ProjectChip id={i.project} /></div></div>
      ) : (
        <div><SectionLabel>未关联事项</SectionLabel><div className="flex flex-wrap gap-2">{!s.ro && <Button size="sm" variant="outline" onClick={() => openSheet("link", { k })} data-link>关联到事项</Button>}<Button size="sm" variant="outline" onClick={() => openSheet("fork", { k, at: Math.max(0, s.ev.length - 1) })}>据此建事项</Button></div></div>
      )}
      <div><SectionLabel right={<Button variant="ghost" size="xs" className="text-muted-foreground" onClick={() => openSheet("graph", { k })}>放大</Button>}>上游与直接下游</SectionLabel>
        <RelationGraph key={k} focus={k} scope="near" height={260} minimal onOpen={openNode} /></div>
      <SessionKnowledge id={k} />
      <div><SectionLabel>会话信息</SectionLabel><div className="flex flex-col gap-1 px-1 type-callout text-muted-foreground"><BotName k={s.b} className="text-foreground" /><span>{hm(s)}</span><span>思考强度：{s.reasoningEffort ?? "跟随 harness"}</span><a href={"#/devices/" + s.device} className="hover:text-foreground">运行在 {st.devices.find((d) => d.id === s.device)?.name ?? "Hub"}</a><span>来源：{s.src}</span><span>用时 {s.dur} · 上下文 {s.contextKnown ? `${s.ctx}%` : "未上报"}</span></div></div>
    </div>
  )
  return (
    <div className="flex h-full min-h-0">
      <div className="min-w-0 flex-1">
        <Page noTitle title={`会话 #${s.n}`} back={i ? ["#/items/" + i.id, "事项"] : ["#/sessions", "会话"]} max={720} scrollRef={scroller}
          bar={<div className="flex min-w-0 items-center gap-2"><span className="truncate type-headline">{s.n ? `会话 #${s.n} · ` : ""}{renaming ? <Input autoFocus defaultValue={s.title} className="inline-flex h-7 w-56 rounded-md" data-rename-input onBlur={(e) => { const v = e.target.value.trim(); if (v && v !== s.title) sessPatch(k, { title: v }, "已重命名"); setRenaming(false) }} onKeyDown={(e) => e.key === "Enter" && (e.target as HTMLInputElement).blur()} /> : s.title}</span><Status s={s.st} sess pill className="hidden sm:inline-flex" />{i && <ProjectChip id={i.project} className="hidden shrink-0 type-footnote text-muted-foreground sm:inline-flex" />}</div>}
          actions={<>{!s.ro && <SessionStats id={k} />}<span className="hidden sm:inline-flex">{ctx}</span><Button size="sm" variant="ghost" className="text-muted-foreground xl:hidden" onClick={() => setInfoOpen(true)}>会话信息</Button>{s.ro && <span className="inline-flex items-center gap-1 type-footnote text-muted-foreground"><LockIcon className="size-3" />只读</span>}{menu}</>}
          footer={
            <div className="mx-auto w-full max-w-[784px] shrink-0 px-4 pb-[76px] sm:px-8 lg:pb-5">
              <div className="flex flex-wrap items-center gap-x-4 gap-y-1 py-2 type-footnote text-muted-foreground" data-session-runtime>
                <span>Harness <span className="text-foreground">{s.h ?? "未知"}</span></span>
                <span className="break-all">模型 <span className="text-foreground">{s.m ?? "默认模型"}</span></span>
                <span>思考强度 <span className="text-foreground">{s.reasoningEffort ?? "跟随 harness"}</span></span>
                <span className={s.ctx >= 80 ? "text-attention" : ""}>上下文 {s.contextKnown ? `${s.ctx}%` : "未上报"}</span>
              </div>
              <RunBar k={k} onStop={s.ro ? undefined : () => interrupt(k)} />
              {!s.ro && <QueueStrip sid={k} rows={s.queue} note={queueNote(s)} composer={composer} />}
              {s.ro ? (
                <div className="flex flex-wrap items-center gap-3 rounded-2xl border bg-card px-4 py-3" data-readonly>
                  <LockIcon className="size-4 text-muted-foreground" /><span className="min-w-0 flex-1 type-callout text-muted-foreground">从{s.src.replace("本机 ", "本机的 ")}导入，只读。需要继续，可以从任一回复派生，或据此建事项。</span>
                  <Button size="sm" variant="outline" onClick={() => openSheet("fork", { k, at: Math.max(0, s.ev.length - 1) })}>据此建事项</Button>
                </div>
              ) : <Composer key={k} draftKey={draftKey(st.workspace?.id, "session", k)} handle={composer} testId="session" model={hm(s)} placeholder={s.st === "ended" ? "会话已结束，发送会恢复这个会话继续" : "直接和 agent 对话…"} onSend={(t, files) => sessSend(k, t, mode, files)}
                left={run ? <Segmented size="sm" value={mode} onChange={setMode} options={["插话", "排队"] as const} /> : undefined} />}
            </div>
          }>
          <div className="pt-6"><Transcript k={k} /><SessionCards k={k} /></div>
        </Page>
      </div>
      <aside className="hidden w-[300px] shrink-0 overflow-y-auto border-l border-sidebar-border bg-sidebar px-5 py-5 xl:block" data-session-panel>{panel}</aside>
      <Dialog open={infoOpen} onOpenChange={setInfoOpen}><DialogContent className="max-h-[85dvh] overflow-y-auto"><DialogHeader><DialogTitle>会话信息</DialogTitle><DialogDescription>当前会话的关联、运行环境和知识引用。</DialogDescription></DialogHeader>{panel}</DialogContent></Dialog>
    </div>
  )
}

/* ═════════ 设置 / Bot ═════════ */
export const PLUGIN_ICON: Record<string, any> = { mail: MailIcon, calendar: CalendarIcon, message: MessageSquareIcon, git: GitPullRequestIcon, shield: LockIcon, list: ListIcon, zap: ZapIcon }
export const Ico = ({ I }: { I: any }) => <span className="flex size-7 shrink-0 items-center justify-center rounded-md bg-muted text-muted-foreground"><I className="size-4" strokeWidth={1.5} /></span>
/** 我: the user's own contact (称呼、时区、语言); only where the Hub has the contacts module */
function MeRow() {
  const on = usePeopleOn()
  return on ? <Group><Row icon={<Ico I={UserRoundIcon} />} title="我" sub="你自己的联系人资料：称呼、时区、语言" onClick={() => go("#/contacts/me")} data-more-me><ChevronRightIcon className="size-4 text-tertiary" /></Row></Group> : null
}
export function MoreView() {
  const groups = [
    { name: "工作", rows: [["projects", "项目", `「${st.workspace?.name}」的项目和目录`, FolderIcon], ["sessions", "会话", "搜索全部会话，进入会话模式", SquareTerminalIcon], ["stats", "统计", "tokens、估算花费、完成效率", ChartNoAxesColumnIcon]] },
    { name: "连接与资料", rows: [["integrations", "接入", "飞书、GitHub、邮箱等服务，每个连接能做什么", PlugIcon], ["channels", "渠道", "各个接入里的群、私聊和对外身份", MessagesSquareIcon], ["mail", "邮件", "往来、分诊和回复草稿", MailIcon], ["contacts", "联系人", "你打交道的人、组织和团队", ContactRoundIcon], ["knowledge", "知识", "记忆、笔记、文档和来源", BookOpenIcon]] },
    { name: "自动化", rows: [["bots", "Bot", "Steve 调用的能力：新建、编辑、删除", BotIcon], ["auto", "自动化", "流程、定时任务和事件源", WorkflowIcon], ["signals", "信号", "外部信息，以及 Bot 怎么处理的", RadioIcon]] },
    { name: "系统", rows: [["devices", "设备", "Hub、运行环境、正在跑的会话", SquareTerminalIcon], ["accounts", "账号", "我的账号、接入 Steve 的连接", LockIcon], ["vault", "保险箱", "密码、令牌、银行卡这类机密", KeyRoundIcon], ["settings", "设置", "工作空间、事项、外观和版本", SettingsIcon]] },
  ] as const
  return (
    <Page title="我的" max={640}>
      <div className="flex flex-col gap-8">
        <MeRow />
        <Group><Row icon={<BotAvatar k="steve" size={40} />} title={<span className="type-headline">{st.bots.steve?.n ?? "Steve"}</span>} sub="你的助手 · 名字、性格、头像" onClick={() => go("#/bots/steve")}><ChevronRightIcon className="size-4 text-tertiary" /></Row></Group>
        {groups.map((group) => <section key={group.name}><SectionLabel>{group.name}</SectionLabel><Group>{group.rows.map(([h, t, s, I]) => (
          <Row key={h} icon={<Ico I={I} />} title={t} sub={s} onClick={() => go("#/" + h)}><ChevronRightIcon className="size-4 text-tertiary" /></Row>
        ))}</Group></section>)}
        <Group><GitHubMoreRow /></Group>
        {st.yolo && <Group><Row icon={<Ico I={ZapIcon} />} title="YOLO 模式" sub={{ on: "YOLO 替你审批：能撤回的决定 Steve 来做", full: "完全放开：Agent 不再请求权限", off: "关：每个决定都等你", paused: "已暂停：" + (st.yolo.paused?.reason ?? ""), forced: "这台 Hub 上不可用" }[yoloState(st.yolo)!]} onClick={() => openSheet("yolo")} data-more-yolo><ChevronRightIcon className="size-4 text-tertiary" /></Row>
          <Row title="YOLO 记录" sub="Steve 替你做的每个决定，能撤销的可以撤" onClick={() => go("#/yolo")}><ChevronRightIcon className="size-4 text-tertiary" /></Row></Group>}
        <Group><Row title="外观"><ThemeSwitch labels /></Row></Group>
      </div>
    </Page>
  )
}

/* ═════════ 视觉规范 ═════════ */
function lum(hex: string) {
  const m = hex.replace("#", ""); const c = [0, 2, 4].map((n) => parseInt(m.slice(n, n + 2), 16) / 255).map((v) => (v <= 0.03928 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4))
  return 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2]
}
const ratio = (a: string, b: string) => { const [x, y] = [lum(a), lum(b)].sort((p, q) => q - p); return (x + 0.05) / (y + 0.05) }
function cssHex(v: string) {
  const el = document.createElement("span"); el.style.color = `var(${v})`; document.body.appendChild(el)
  const c = getComputedStyle(el).color; el.remove()
  const m = c.match(/[\d.]+/g)?.map(Number) ?? [0, 0, 0]
  return "#" + m.slice(0, 3).map((n) => Math.round(n).toString(16).padStart(2, "0")).join("")
}
const ROLES: [string, string, string][] = [
  ["--foreground", "正文", "标题、正文"], ["--muted-foreground", "次要文字", "说明、元信息"], ["--tertiary", "三级灰", "占位符、装饰（不用于阅读）"],
  ["--blue", "系统蓝", "选中、运行中、链接、焦点"], ["--attention", "系统橙", "只用于“需要你”"], ["--destructive", "系统红", "失败、危险操作"], ["--success", "系统绿", "只用于完成的勾"],
]
const SURF: [string, string][] = [["--background", "内容区"], ["--sidebar", "侧栏"], ["--muted", "输入、气泡、悬停"], ["--card", "卡片"], ["--border", "分隔线"]]
export function DesignView() {
  const [, f] = useState(0)
  useEffect(() => { const t = setTimeout(() => f((x) => x + 1), 30); return () => clearTimeout(t) }, [st.ver])
  const bg = cssHex("--background")
  const Do = ({ ok: good, children, note }: { ok: boolean; children: ReactNode; note: string }) => (
    <div className="flex flex-col gap-2"><div className="flex min-h-24 items-center justify-center rounded-xl border bg-card p-4">{children}</div>
      <div className="flex items-start gap-1.5 type-footnote">{good ? <CheckIcon className="mt-px size-3.5 text-success" /> : <XIcon className="mt-px size-3.5 text-destructive" />}<span className="text-muted-foreground">{note}</span></div></div>
  )
  return (
    <Page title="视觉规范" sub="安静、清晰、可信：内容是主角，界面退后。颜色只表达状态。" max={880} toolbar={<ThemeSwitch labels />}>
      <div className="flex flex-col gap-12">
        <section>
          <SectionLabel>原则</SectionLabel>
          <div className="grid gap-3 sm:grid-cols-3">
            {[["内容优先", "标题、正文和回复是主体；控件默认安静，悬停或需要时才出现。"], ["颜色只表达状态", "界面是黑白灰。蓝表示进行中和选中，橙只表示需要你。"], ["只有运行中的在动", "循环动画只给正在运行的那一项；其他过渡 120–280ms。"]].map(([t, d]) => (
              <div key={t} className="rounded-xl border bg-card px-4 py-3.5"><div className="type-headline">{t}</div><div className="mt-1 type-callout text-muted-foreground">{d}</div></div>
            ))}
          </div>
        </section>
        <section>
          <SectionLabel>颜色 · 文字与状态（对比度相对内容区实时计算）</SectionLabel>
          <Group>{ROLES.map(([v, n, use]) => { const hx = cssHex(v), cr = ratio(hx, bg); return (
            <div key={v} className="flex items-center gap-3 px-4 py-2.5">
              <span className="size-7 shrink-0 rounded-full ring-1 ring-inset ring-black/5" style={{ background: `var(${v})` }} />
              <div className="min-w-0 flex-1"><div className="type-body">{n} <span className="font-mono text-[12px] text-muted-foreground">{v.slice(2)}</span></div><div className="type-footnote text-muted-foreground">{use}</div></div>
              <span className="font-mono text-[12px] text-muted-foreground">{hx.toUpperCase()}</span>
              <span className={cn("w-16 text-right font-mono text-[12px] tabular-nums", cr >= 4.5 ? "text-foreground" : "text-muted-foreground")} data-contrast={v}>{cr.toFixed(1)}:1</span>
            </div>) })}</Group>
          <div className="mt-6 grid grid-cols-2 gap-3 sm:grid-cols-5">
            {SURF.map(([v, n]) => <div key={v}><div className="h-16 rounded-xl border" style={{ background: `var(${v})` }} /><div className="mt-1.5 type-footnote"><span className="text-foreground">{n}</span> <span className="font-mono text-muted-foreground">{cssHex(v).toUpperCase()}</span></div></div>)}
          </div>
        </section>
        <section>
          <SectionLabel>字体 · 系统字体（SF Pro / 苹方），只用 400、500、600</SectionLabel>
          <Group>{([["type-large-title", "大标题 26/32 · 600", "待处理"], ["type-title", "标题 20/28 · 600", "修复登录后跳转回首页的问题"], ["type-headline", "小标题 15/22 · 600", "修好了，请验收"], ["type-body", "正文 14/22 · 400", "登录后回到原页面，深链保留。PR #12；构建通过。"], ["type-callout", "说明 13/20 · 400", "搜索结果列表已完成，正在构建"], ["type-footnote", "脚注 12/16 · 400", "前端 Bot · 2 个会话 · 08:52"], ["type-code", "代码 12.5/20 · Mono", "let target = state.return_to.unwrap_or(\"/\");"]] as const).map(([c, l, s]) => (
            <div key={c} className="flex flex-col gap-1 px-4 py-3 sm:flex-row sm:items-baseline sm:gap-6"><span className="w-40 shrink-0 type-footnote text-muted-foreground">{l}</span><span className={cn(c, "min-w-0 truncate")}>{s}</span></div>
          ))}</Group>
        </section>
        <section>
          <SectionLabel>状态 · 圆点加文字</SectionLabel>
          <div className="flex flex-wrap gap-x-6 gap-y-3 rounded-xl border bg-card px-4 py-4">{["run", "wait", "prop", "pause", "done", "drop"].map((s) => <Status key={s} s={s} className="type-callout" />)}<span className="w-full" />{["run", "wait", "idle", "ended"].map((s) => <Status key={s} s={s} sess className="type-callout" />)}</div>
        </section>
        <section>
          <SectionLabel>组件</SectionLabel>
          <div className="grid gap-6 lg:grid-cols-2">
            <div className="flex flex-col gap-5 rounded-xl border bg-card p-5">
              <div className="type-footnote text-muted-foreground">按钮：一个区域一个深色主按钮</div>
              <div className="flex flex-wrap items-center gap-2"><Button>验收通过</Button><Button variant="outline">返工…</Button><Button variant="ghost">取消</Button><Button variant="destructive">放弃事项</Button></div>
              <div className="type-footnote text-muted-foreground">分段控件、开关、输入</div>
              <div className="flex flex-wrap items-center gap-3"><Segmented value="列表" onChange={() => {}} options={["列表", "看板", "关系图"]} /><Switch defaultChecked /><Input className="h-8 w-40 rounded-lg" placeholder="搜索事项" /></div>
              <div className="type-footnote text-muted-foreground">输入框：圆角 20，深色圆形发送</div>
              <Composer placeholder="和 Steve 说…（示例，不会发送）" onSend={() => toast("这是示例输入框，不会发送")} />
            </div>
            <div className="flex flex-col gap-5 rounded-xl border bg-card p-5">
              <div className="type-footnote text-muted-foreground">Agent 过程：单色图标、次要文字，运行中的一行带流光</div>
              <div className="rounded-lg border p-4">{(() => { const k = Object.keys(st.S).find((x) => !st.S[x].ro); return k ? <Transcript k={k} compact tail={3} /> : <span className="type-callout text-muted-foreground">有会话之后，这里用最近的一个做示例</span> })()}</div>
            </div>
          </div>
        </section>
        <section>
          <SectionLabel>该做与不该做</SectionLabel>
          <div className="grid gap-x-4 gap-y-6 sm:grid-cols-2">
            <Do ok note="状态用圆点加文字，文字保持灰色"><div className="flex gap-4"><Status s="run" className="type-callout" /><Status s="wait" className="type-callout" /><Status s="done" className="type-callout" /></div></Do>
            <Do ok={false} note="彩色实心标签：颜色太多，失去含义"><div className="flex gap-2">{[["进行中", "#0071e3"], ["等你", "#ff9500"], ["已完成", "#28a745"], ["提议", "#9a5cff"]].map(([l, c]) => <span key={l} className="rounded-md px-2 py-0.5 text-[12px] font-medium text-white" style={{ background: c }}>{l}</span>)}</div></Do>
            <Do ok note="头像固定用 Agent Robot Avatar，每个 Bot 自己定颜色和头型，静止不动"><div className="flex gap-2">{Object.keys(st.bots).slice(0, 5).map((k) => <BotAvatar key={k} k={k} size={32} />)}</div></Do>
            <Do ok={false} note="每个 Bot 一种颜色：和状态色混在一起"><div className="flex gap-2">{["#5b8def", "#e0703a", "#2fa36b", "#9a5cff", "#d6a23a"].map((c, n) => <span key={c} className="flex size-8 items-center justify-center rounded-lg text-[13px] font-semibold text-white" style={{ background: c }}>{"前邮日运评"[n]}</span>)}</div></Do>
            <Do ok note="一个深色主按钮，其余描边或纯文字"><div className="flex gap-2"><Button variant="ghost">不回复</Button><Button variant="outline">改得更正式</Button><Button>发送</Button></div></Do>
            <Do ok={false} note="多个彩色按钮并列：分不清主次"><div className="flex gap-2">{[["不回复", "#e5352b"], ["改写", "#ff9500"], ["发送", "#28a745"]].map(([l, c]) => <span key={l} className="rounded-lg px-3 py-1.5 text-[13px] font-medium text-white" style={{ background: c }}>{l}</span>)}</div></Do>
            <Do ok note="提示用一行次要文字"><span className="type-footnote text-muted-foreground">改动较大时会带交接摘要开新会话</span></Do>
            <Do ok={false} note="彩色底块做提示：抢走正文的注意力"><span className="rounded-lg bg-[#e8f0fe] px-3 py-2 text-[12.5px] font-medium text-[#1a56db]">设计说明 · 改动较大时会带交接摘要开新会话</span></Do>
          </div>
        </section>
      </div>
    </Page>
  )
}

export function ImportDialog() {
  const [pick, setPick] = useState<string[]>([]), [busy, setBusy] = useState(false)
  useEffect(() => { void loadLocalSessions() }, [])
  const left = st.localSessions.filter((x) => !x.imported)
  const groups = [...new Set(left.map((x) => x.harness))]
  return (
    <Modal open wide title="导入本机会话" desc="选择要导入的会话。导入后只读，可以搜索、查看、派生或据此建事项；本机的原始记录不改动。"
      footer={<><Button variant="outline" onClick={closeSheet}>取消</Button><Button disabled={!pick.length || busy} data-testid="import-go" onClick={async () => { setBusy(true); await importSessions(pick); setBusy(false); closeSheet(); go("#/sessions") }}>{busy && <Spinner className="size-3.5" />}导入 {pick.length || ""} 个</Button></>}>
      <div className="max-h-[60vh] overflow-y-auto">
      {left.length ? groups.map((h) => (
        <section key={h} className="mb-4"><SectionLabel>{h} · 这台 Hub 上</SectionLabel>
          <Group>{left.filter((x) => x.harness === h).map((x) => (
            <label key={x.key} className="flex cursor-pointer items-center gap-3 px-4 py-2.5 hover:bg-accent" data-import-opt={x.key}>
              <Checkbox checked={pick.includes(x.key)} onCheckedChange={(v) => setPick(v ? [...pick, x.key] : pick.filter((k) => k !== x.key))} />
              <span className="min-w-0 flex-1"><span className="block truncate type-body">{x.title}</span><span className="block truncate type-footnote text-muted-foreground"><span className="font-mono">{x.cwd.replace(/^\/Users\/[^/]+/, "~")}</span> · {Math.max(1, Math.round(x.size / 1024))} KB</span></span>
              <span className="type-footnote text-muted-foreground">{tm(x.when)}</span>
            </label>))}</Group>
        </section>)) : <Empty title={st.localSessions.length ? "都导入过了" : "正在读取本机会话…"} />}
      </div>
    </Modal>
  )
}
