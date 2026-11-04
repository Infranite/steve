import type { RFile } from "@/api"
import { Files } from "@/attach"
import { cue, onCue, type HeroCue } from "@/bus"
import { ChannelMessageBody } from "@/channels/message-body"
import { ChatRail, LiveSteps, NoticeLine, RailToggle, showNotices, StepsBlock } from "@/chat2"
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
import { chatSend, emit, finishOnboarding, focusItem, isPromoting, isThinking, makeItem, pending, pendingCards, quoteMsg, reroute, st, stopReply, tm, toast, WT, type Item, type Msg } from "@/store"
import { ChanOutNote, ConvBadgeChip, ConvSwitcher, ImReadOnly, SenderLine, useConversation } from "@/thread/conversations"
import { AskChips, CardLink, ComposerBars, ItemCard, OfferLine, QueuedNote, QuoteHead, Rerun, SuggestCard, Updates } from "@/thread/parts"
import { Composer, go, ItemNo, ItemRef, openSheet, StatusDot } from "@/ui"
import { PlanBar } from "@/view-shared"
import { cn } from "cn"
import { ChevronRightIcon, CircleCheckIcon, CopyIcon, ListChecksIcon, MessageCircleIcon, MessageSquareReplyIcon, RouteIcon } from "lucide-react"
import { Fragment, useEffect, useRef, useState, type ReactNode } from "react"


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
  const stuck = useRef(true), [unread, setUnread] = useState(false)
  const bottom = () => { stuck.current = true; setUnread(false); scroller.current?.scrollTo({ top: 1e9 }) }
  useEffect(() => { if (stuck.current) bottom(); else setUnread(true) }, [list.length, list.at(-1)?.id, list.at(-1)?.t, st.draft?.text])
  /* rendered Markdown and folded steps grow after the first scroll: stay at the bottom until the user scrolls away */
  useEffect(() => {
    const el = scroller.current, inner = el?.firstElementChild; if (!el || !inner) return
    const away = () => { stuck.current = false }, near = () => { stuck.current = el.scrollHeight - el.scrollTop - el.clientHeight < 40; if (stuck.current) setUnread(false) }
    el.addEventListener("wheel", away, { passive: true }); el.addEventListener("touchmove", away, { passive: true }); el.addEventListener("scroll", near, { passive: true })
    window.addEventListener("steve-open-steps", away)
    /* unfolding something you are reading must not jump you to the bottom */
    const unfold = (e: Event) => { if ((e.target as HTMLElement).closest?.("[data-steps-toggle]")) away() }; el.addEventListener("click", unfold)
    const ro = new ResizeObserver(() => { if (stuck.current) el.scrollTop = el.scrollHeight }); ro.observe(inner)
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
            const key = m.clientId ?? m.id ?? m.k
            const prev = list[n - 1], day = m.created && (!prev?.created || new Date(prev.created).toDateString() !== new Date(m.created).toDateString())
            const dayLabel = day ? <div className="flex items-center gap-4 type-footnote text-muted-foreground" key={"d" + key} data-chat-day><span className="h-px flex-1 bg-border/60" /><span>{new Date(m.created!).toDateString() === new Date().toDateString() ? "今天" : tm(m.created).split(" ")[0]}</span><span className="h-px flex-1 bg-border/60" /></div> : null
            if (m.k === "me" && conv) return <Fragment key={key}>{dayLabel}<div className="flex flex-col gap-1.5" data-msg={m.id} data-im-turn><SenderLine m={rawOf(m)} />{rawOf(m)?.channel_message ? <div className="w-full max-w-[600px] self-end"><ChannelMessageBody message={rawOf(m)!.channel_message!} /></div> : <Me t={m.t!} files={m.files} />}<QueuedNote m={m} /></div></Fragment>
            if (m.k === "me") return <Fragment key={key}>{dayLabel}<div className="group/me relative flex flex-col gap-1.5 transition-colors" data-msg={m.id}><QuoteHead id={m.replyTo} className="self-end" /><Me t={m.t!} files={m.files} /><QueuedNote m={m} /><MeActions m={m} /></div></Fragment>
            if (m.k === "thinking") return <div key={key} className="flex flex-col gap-2"><LiveSteps />{st.draft?.text && <SteveSays><Markdown text={st.draft.text} streaming /></SteveSays>}</div>
            if (m.notice) return <Fragment key={key}>{dayLabel}<NoticeLine m={m} onOpen={showNotices} /></Fragment>
            return <Fragment key={key}>{dayLabel}<div className="group/sm relative flex min-w-0 flex-col gap-2.5 transition-colors" data-msg={m.id}>
              <div className="flex min-w-0 items-center gap-2 px-1 type-footnote text-muted-foreground" data-chat-author><span className="truncate font-medium text-foreground">{S?.n ?? "Steve"}</span>{m.created && <time className="shrink-0" dateTime={new Date(m.created).toISOString()}>{tm(m.created)}</time>}</div>
              <QuoteHead id={m.replyTo} /><StepsBlock m={m} /><SteveSays still={m.id === st.drafted}>{!m.update && !m.cardOnly && <Markdown text={linkCites(m.t ?? "", m.cites)} />}{m.item && !m.update && !m.card && (m.wasCard || list.some((x) => x.card?.item === m.item) ? <CardLink id={m.item} /> : <ItemRef id={m.item} />)}{m.card ? <ItemCard c={m.card} inText={!m.cardOnly && m.card.delivery?.text === m.t} /> : <Updates m={m} />}<OfferLine m={m} /><SuggestCard m={m} /><AskChips m={m} /><Rerun m={m} /></SteveSays>{m.cfgs && <div className="-mt-1 flex flex-col gap-2 self-start">{m.cfgs.map((p) => <ProposalCard key={p.id} p={p} inChat />)}</div>}<Cites cites={m.cites} />{conv ? <ChanOutNote m={rawOf(m)} /> : <MsgActions m={m} />}</div></Fragment>
          })}
          {!conv && away > 0 && <><SteveSays sub="离开期间汇报">你不在的时候：</SteveSays><Digest from={away} /></>}
          {!conv && <Onboarding />}
        </div>
      </div>
      {unread && <Button className="absolute bottom-36 left-1/2 -translate-x-1/2" size="sm" onClick={bottom}>新消息 ↓</Button>}
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