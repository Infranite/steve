import { messageViews, useItemView } from "@/data/sync"
import type { ConvMessage } from "@/api/conversations"
import { VirtualList } from "@/components/virtual-list"
import type { Virtualizer } from "@tanstack/react-virtual"
import { observeChatLive, presentedChatLive, useChatLive } from "@/data/live"
import { getItems } from "@/store"
import type { RFile } from "@/api"
import { Files } from "@/attach"
import { MineButton } from "@/mine"
import { cue, onCue, type HeroCue } from "@/bus"
import { ChannelMessageBody } from "@/channels/message-body"
import { ChatRail, LiveSteps, NoticeLine, RailToggle, showNotices, StepsBlock, TurnInterruption, type ProcessMemory } from "@/chat2"
import { A2uiHost } from "@/a2ui/host"
import { A2uiActionNote } from "@/a2ui/note"
import { Markdown } from "@/components/markdown"
import { Button } from "@/components/ui/button"
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuTrigger } from "@/components/ui/dropdown-menu"
import { Input } from "@/components/ui/input"
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover"
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip"
import { draftKey } from "@/composer-draft"
import { WORKSPACE_SETTING_DEFAULTS } from "@/gen"
import { ProposalCard } from "@/config-ui"
import { Cites, linkCites } from "@/knowledge/parts"
import { ActorAvatar } from "@/components/actor-avatar"
import { SteveModelControl } from "@/steve-model"
import { chatBusy, chatSend, endMeeting, focusItem, meeting, isPromoting, makeItem, decideCards, pendingCards, quoteMsg, reroute, st, stopReply, useShellSelector, tm, toast, type Item, type Msg } from "@/store"
import { ChatQueue } from "@/queue"
import { AwaySummary, type AwayItem } from "@/components/affect/away-summary"
import { MessageActions, type MessageMoreAction } from "@/components/affect/message-actions"
import { ComposerDecisions } from "@/thread/decide"
import { Spinner } from "@/components/ui/spinner"
import { fillTo, historyState, loadOlder, onJump, threadMsg, threadMessages, useHistory, useThreadIds, useThreadMessage } from "@/thread/history"
import { dayLabel, HistoryPanel, HistoryToggle } from "@/thread/history-panel"
import { readPos, savePos, type ReadPos } from "@/thread/topics"
import { useReplyReading } from "@/thread/use-reply-reading"
import { ChannelSwitcher, ChanOutNote, ConversationDraft, ConvBadgeChip, ConvSwitcher, HandlingLine, ImMsgActions, ImReadOnly, OlderRecords, RecordRow, SenderLine, useChannels, useConversation } from "@/thread/conversations"
import { weave, type Entry } from "@/thread/channel-record"
import { AskChips, CardLink, ComposerBars, ItemCard, OfferLine, QueuedNote, QuoteHead, Rerun, threadDecisions, Updates } from "@/thread/parts"
import { Composer, type ComposerHandle } from "@/composer"
import { go, ItemRef, openSheet } from "@/ui"
import { cn } from "cn"
import { ArrowDownIcon, ListChecksIcon, MessageCircleIcon, MessageSquareReplyIcon, RouteIcon, Undo2Icon } from "lucide-react"
import { Component, lazy, memo, startTransition, Suspense, useEffect, useMemo, useRef, useState, type ComponentProps, type ReactNode } from "react"
import { lang, t as tr, useT } from "@/i18n"
import { hubBase } from "@/hub"


/* ═════════ 对话 ═════════ */
/* One compact bar (ST-50): who, what Steve himself is doing right now, and the way to history and to 事项. How many items
 * he follows is on 事项, not under his name, so it does not read as “working on 8 things”. */
/* what each cue says, read when it plays so it is in the current language; "wary" counts what waits */
const CUES = ["input", "think", "success", "affirm", "inspect", "love", "failure"] as const
const cueLabel = (c: HeroCue): string | null => c === "wary" ? tr("pages:chat.waiting", { count: decideCards().length }) : (CUES as readonly string[]).includes(c) ? tr(`pages:chat.cue.${c as (typeof CUES)[number]}`) : null
/* what Steve is doing, by the phase the Hub reports */
const PHASES = ["route", "answer", "research", "item", "config", "supplement"] as const
const doingLabel = (phase: string) => tr(`pages:chat.doing.${(PHASES as readonly string[]).includes(phase) ? phase as (typeof PHASES)[number] : "answer"}`)

function Hero() {
  const S = st.bots.steve
  const t = useT()
  const [label, setLabel] = useState<string | null>(null)
  useEffect(() => {
    let timer: any
    const off = onCue((c) => { clearTimeout(timer); setLabel(cueLabel(c)); if (c !== "think" && c !== "input") timer = setTimeout(() => setLabel(null), 2600) })
    return () => { off(); clearTimeout(timer) }
  }, [])
  const activity = useChatLive((s) => s.live ? "research" : s.draft?.text ? "reply" : s.thinking ? s.phase || "answer" : "")
  const doing = activity === "reply" ? t("pages:chat.doing.reply") : activity ? doingLabel(activity) : ""
  /* while he works, the work is the state; a cue only fills the quiet moments */
  const text = doing || label || t("pages:chat.idle")
  return (
    <div className="flex min-w-0 shrink items-center gap-2.5" data-hero="compact">
      <div className="shrink-0" onDoubleClick={() => cue("love")}><ActorAvatar actorId="steve" size={28} live tooltip={false} /></div>
      <span className="shrink-0 type-headline">{S?.n ?? "Steve"}</span>
      <span className={cn("flex min-w-0 items-center gap-1.5 truncate type-callout transition-colors duration-180", doing || label ? "text-foreground/80" : "text-tertiary")} data-hero-label>
        {doing && <span className="size-1.5 shrink-0 rounded-full bg-blue dot-running" aria-hidden />}<span key={text} className="truncate anim-in">{text}</span></span>
    </div>
  )
}

/** `still`: no entrance, for the reply that takes the place of its draft */
function SteveSays({ children, sub, still }: { children: ReactNode; sub?: string; still?: boolean }) {
  return (
    <div className={cn("min-w-0 self-stretch px-1", !still && "anim-in")} data-steve-msg>
      {sub && <div className="mb-1.5 type-label text-tertiary">{sub}</div>}
      <div className="type-chat text-foreground">{children}</div>
    </div>
  )
}

const Me = ({ t, files }: { t: string; files?: RFile[] }) => <div className="flex max-w-[85%] items-start justify-end gap-2 self-end anim-in" data-me-msg>
  <div className="flex min-w-0 flex-col items-end gap-1.5">
    <Files files={files} align="end" />
    {(t !== "（附件）" || !files?.length) && <div className="min-w-0 max-w-full rounded-2xl rounded-br-md bg-muted px-4 py-2.5 type-chat text-foreground"><Markdown text={t} /></div>}
  </div>
  <ActorAvatar actorId="me" size={24} className="mt-1" />
</div>

/* the actions under Steve's replies (affectUI Message Actions): copy, quote, and the corrections of thread routing (§3.5).
 * Regenerate, rating and read-aloud are left out: Steve has nothing behind them */
/* on a touch screen (no hover) the actions show under the latest message only; tapping another one shows its own */
const TOUCH_LAST = "[@media(hover:none)]:opacity-100"
function MsgActions({ m, last }: { m: Msg; last: boolean }) {
  const fromChat = !!m.item && (m.steps?.route?.kind === "item" || !!m.updates?.length) && !m.update
  const promoting = useShellSelector(() => isPromoting(m))
  const canItem = !m.item && (!!m.chat || m.steps?.route?.kind === "research" || m.steps?.route?.kind === "answer")
  const t = useT()
  const more: MessageMoreAction[] = [
    ...(m.id ? [{ key: "quote", label: m.item ? t("pages:chat.quoteTo", { item: m.item }) : t("pages:chat.quote"), icon: <MessageSquareReplyIcon strokeWidth={1.75} />, onClick: () => quoteMsg(m.id!), attrs: { "data-quote-reply": "" } }] : []),
    ...(canItem ? [{ key: "item", label: t("pages:chat.makeItem"), icon: <ListChecksIcon strokeWidth={1.75} />, onClick: () => makeItem(m), disabled: promoting, attrs: { "data-make-item": "" } }] : []),
    ...(fromChat ? [{ key: "inline", label: t("pages:chat.toInline"), icon: <MessageCircleIcon strokeWidth={1.75} />, onClick: () => void reroute(m, "inline"), attrs: { "data-to-inline": "" } }] : []),
  ]
  return (
    <div className={cn("pointer-events-none absolute -bottom-8 left-0 right-0 opacity-0 transition-opacity [&_button]:pointer-events-auto focus-within:opacity-100 group-hover/sm:opacity-100 group-focus-within/sm:opacity-100", last && TOUCH_LAST)} data-msg-actions>
      <MessageActions text={m.t ?? ""} actions={["copy"]} quiet={false} label={t("pages:chat.msgActions")} copyLabels={{ copy: t("common:action.copy"), copied: t("common:action.copied") }} more={more} />
    </div>
  )
}

/* the menu on your own messages: quote it, or send it somewhere else than the router did */
function MeActions({ m, last }: { m: Msg; last: boolean }) {
  const t = useT()
  const [toItem, setToItem] = useState(false), [id, setId] = useState("")
  if (!m.id) return null
  const send = () => { const v = id.trim().toUpperCase().replace(/^#/, ""); if (!/^[A-Z]{1,6}-\d+$/.test(v)) { toast(t("pages:chat.badId")); return } setToItem(false); setId(""); void reroute(m, "item:" + v) }
  return (
    <div className={cn("absolute -bottom-7 right-0 flex items-center gap-0.5 opacity-0 transition-opacity focus-within:opacity-100 group-hover/me:opacity-100 group-focus-within/me:opacity-100", last && TOUCH_LAST)} data-me-actions>
      <Tooltip><TooltipTrigger render={<Button variant="ghost" size="icon-xs" aria-label={t("pages:chat.quote")} className="text-muted-foreground" onClick={() => quoteMsg(m.id!)} />}><MessageSquareReplyIcon /></TooltipTrigger><TooltipContent>{t("pages:chat.quote")}</TooltipContent></Tooltip>
      <DropdownMenu>
        <DropdownMenuTrigger render={<Button variant="ghost" size="icon-xs" aria-label={t("pages:chat.reroute")} className="text-muted-foreground" data-reroute-menu />}><RouteIcon /></DropdownMenuTrigger>
        <DropdownMenuContent align="end" className="min-w-44">
          <div className="px-2 pb-1 pt-1.5 type-label text-tertiary">{t("pages:chat.rerouteMenu")}</div>
          <DropdownMenuItem onClick={() => void reroute(m, "inline")} data-reroute="inline">{t("pages:chat.inline")}</DropdownMenuItem>
          <DropdownMenuItem onClick={() => void reroute(m, "item")} data-reroute="item">{t("pages:chat.asItem")}</DropdownMenuItem>
          <DropdownMenuItem onClick={() => setToItem(true)} data-reroute="supplement">{t("pages:chat.toItem")}</DropdownMenuItem>
        </DropdownMenuContent>
      </DropdownMenu>
      <Popover open={toItem} onOpenChange={setToItem}>
        <PopoverTrigger nativeButton={false} render={<span className="size-0" aria-hidden />} />
        <PopoverContent align="end" className="w-64 gap-3 p-3" data-reroute-popover>
          <div className="type-body font-medium">{t("pages:chat.whichItem")}</div>
          <Input autoFocus placeholder="ST-12" value={id} onChange={(e) => setId(e.target.value)} onKeyDown={(e) => e.key === "Enter" && send()} className="font-mono" data-reroute-item />
          <div className="flex justify-end gap-2"><Button size="sm" variant="ghost" onClick={() => setToItem(false)}>{t("common:action.cancel")}</Button><Button size="sm" onClick={send}>{t("pages:chat.send")}</Button></div>
        </PopoverContent>
      </Popover>
    </div>
  )
}

/* while you were away (affectUI Away Summary): what failed and what got done since you last looked. What waits on you is
 * not repeated here: it is answered right on this page, in the thread or in the card above the composer. Items still
 * running have no row either: the rail shows what runs */
function Away({ from }: { from: number }) {
  const t = useT()
  const waiting = new Set(pendingCards().map((c) => c.i.id).filter(Boolean))
  const done = getItems().filter((i) => i.st === "done" && i.updated >= from && !i.arch)
  const failed = getItems().filter((i) => i.st === "fail" && i.updated >= from && !i.arch && !waiting.has(i.id))
  const name = (i: Item) => `${i.id} ${i.t}`
  const items: AwayItem[] = [
    ...failed.map((i) => ({ id: i.id, title: name(i), kind: "failed" as const, detail: i.progress || undefined, action: t("pages:chat.away.look") })),
    ...done.map((i) => ({ id: i.id, title: name(i), kind: "done" as const, detail: i.progress || undefined })),
  ]
  if (!items.length) return null
  return (
    <div className="mt-6 w-full max-w-[560px] self-start" data-digest>
      <AwaySummary since={from} items={items} timeStyle="clock" locale={lang()} onAction={(id) => go("#/items/" + id)}
        labels={{ title: t("pages:chat.away.title"), since: () => t("pages:chat.away.since", { time: tm(from) }), done: (n) => t("pages:chat.away.done", { count: n }), needsYou: (n) => t("pages:chat.away.needsYou", { count: n }), failed: (n) => t("pages:chat.away.failed", { count: n }),
          action: { "needs-you": t("pages:chat.away.decide"), done: t("pages:chat.away.open"), failed: t("pages:chat.away.look") }, showMore: (n) => t("pages:chat.away.showMore", { count: n }), showLess: t("pages:chat.away.showLess"), gotIt: t("pages:chat.away.gotIt"),
          collapsed: (away, n) => t("pages:chat.away.collapsed", { away, count: n }), empty: t("pages:chat.away.empty") }} />
    </div>
  )
}

const SEEN = "steve-seen"

/** last time this browser looked at Steve; the digest shows when you were away longer than the setting */
function useAway() {
  const [from] = useState(() => Number(localStorage.getItem(SEEN) ?? 0))
  useEffect(() => { const mark = () => localStorage.setItem(SEEN, String(Date.now())); mark(); const t = setInterval(mark, 60_000); window.addEventListener("beforeunload", mark); return () => { clearInterval(t); window.removeEventListener("beforeunload", mark) } }, [])
  const hours = Number(st.settings.awayH ?? WORKSPACE_SETTING_DEFAULTS.awayH)
  return hours > 0 && from > 0 && Date.now() - from > hours * 3600_000 ? from : 0
}

type Anchor = { id: string; created: number; offset: number }

/** Capture the visible message before a patch or an older page changes the rows. */
class ReadingPosition extends Component<{
  rows: readonly unknown[]
  capture: () => Anchor | null
  restore: (anchor: Anchor) => void
  children: ReactNode
}> {
  getSnapshotBeforeUpdate(previous: Readonly<this["props"]>) {
    return previous.rows === this.props.rows ? null : this.props.capture()
  }

  componentDidUpdate(_previous: Readonly<this["props"]>, _state: unknown, anchor: Anchor | null) {
    if (anchor) this.props.restore(anchor)
  }

  render() { return this.props.children }
}
/** Steve's messages this close together read as one run under one name */
const RUN_GAP = 10 * 60_000
const FLASH_MS = 1600
const estimateMessage = ({ entry }: { entry: Entry<Msg> }) => {
  if (entry.k === "rec") return 100
  // Count hard line breaks and block spacing too. Capping every reply at a
  // small estimate moves the history by whole viewports when a long row lands.
  let height = 52
  const columns = entry.m.k === "me" ? 60 : 80
  for (const line of (entry.m.t ?? "").split("\n")) {
    height += line.trim() ? Math.ceil(line.length / columns) * 22 : 12
  }
  return height
}

/** `conv`: an IM conversation (`chan:<space>`, channels §2.12), read-only; without it, yours */
const Onboarding = lazy(() => import("@/onboarding").then((m) => ({ default: m.Onboarding })))

export const ChatView = memo(function ChatView({ conv }: { conv?: string }) {
  const { channels, error: channelsError, retry: retryChannels } = useChannels()
  useShellSelector(() => st.bots.steve)
  const focusId = useShellSelector(() => st.focus?.item)
  useItemView(focusId)
  useShellSelector(() => `${st.ready}|${st.onboard}|${st.quote ?? ""}|${focusId ?? ""}|${st.hold?.item ?? ""}|${hubBase()}|${st.workspace?.id ?? ""}`)
  const S = st.bots.steve, away = useAway()
  const t = useT()
  const { more, ended, loading } = useHistory()
  const { data: im, error: imError, loading: imLoading, reload: reloadIm, older: olderRecords, loadingOlder } = useConversation(conv)
  const ids = useThreadIds(!conv)
  const thread = useMemo(threadMessages, [ids])
  const list: Msg[] = conv ? im?.msgs ?? [] : thread
  const hasMessages = list.length > 0
  const woven = useMemo(() => conv && im ? weave(list, im.records) : null, [conv, im, list])
  const imDecisions = useMemo(() => new Set([...im?.raw.values() ?? []].flatMap((m) => m.meta?.chan_out?.decision ? [String(m.meta.chan_out.decision)] : [])), [im?.raw])
  const entries = useMemo(() => (woven?.entries ?? list.map((m): Entry<Msg> => ({ k: "msg", m }))).map((entry, index) => ({
    id: entry.k === "rec" ? "r" + entry.r.id : entry.m.id ?? entry.m.clientId ?? String(index), entry, index,
  })), [woven, list])
  const virtual = useRef<Virtualizer<HTMLDivElement, HTMLDivElement>>(null)
  const entryIndex = useRef(entries); entryIndex.current = entries
  const inThread = useMemo(() => conv ? new Set<string>() : threadDecisions(list), [conv, list])
  const scroller = useRef<HTMLDivElement>(null), composer = useRef<ComposerHandle | null>(null), top = useRef<HTMLDivElement>(null)
  useReplyReading(scroller, conv, ids)
  const posKey = conv ? "" : st.workspace?.id ?? ""
  const processMemory = useMemo<ProcessMemory>(() => new Map(), [posKey, conv])
  /* where you were reading last time (ST-50): reopening there, the page does not start at the bottom first */
  const saved = useRef<ReadPos | null | undefined>(undefined)
  if (saved.current === undefined) saved.current = conv ? null : readPos(posKey)
  const stuck = useRef(!saved.current || saved.current.bottom), [unread, setUnread] = useState(false)
  // Reopening an old position is a reading action, just like a history jump.
  // Resizing the virtual sizer while it mounts must not turn it into follow mode.
  const held = useRef(!stuck.current), towardLatest = useRef(false)
  const restoring = useRef(!stuck.current)
  /* new messages since you left the bottom: everything said after the latest one you had */
  const leftAt = useRef(0)
  /* where a jump came from (ST-50): back to that message, or to the latest when you were there */
  const [back, setBack] = useState<Anchor | "latest" | null>(null)
  const [flash, setFlash] = useState<string | null>(null)
  const preserving = useRef<Anchor | null>(null)
  const jumpRequest = useRef<symbol | null>(null)
  /* handlers set up once read the list as it is now */
  const shown = useRef(list); shown.current = list
  const latest = () => shown.current.findLast((m) => m.created)?.created ?? 0
  const scrollToEnd = () => {
    // The live reply is outside the virtual list. Use the actual content end, not a
    // virtualizer command that keeps reconciling offsets as draft lines arrive.
    const el = scroller.current
    if (el) el.scrollTop = el.scrollHeight
  }
  const bottom = () => {
    const jump = !stuck.current && virtual.current?.scrollElement ? virtual.current : null
    preserving.current = null
    stuck.current = true; held.current = false; towardLatest.current = false
    setUnread(false); setBack(null)
    // Let the virtualizer supersede a history scroll and settle the newly mounted tail.
    if (jump) jump.scrollToEnd()
    else scrollToEnd()
  }
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
  const leave = () => { if (stuck.current) leftAt.current = latest(); stuck.current = false }
  // An explicit quote jump or drill-down is a reading action, not a request to follow.
  const hold = () => { leave(); held.current = true; towardLatest.current = false }
  useEffect(() => {
    if (stuck.current) bottom()
    else if (restoring.current) return
    // A viewport that now fits every message has no unseen content. A small gap
    // during a history prepend is not enough to resume following: the virtual
    // sizer and its scroll correction can land in separate layout passes.
    else if (!held.current && scroller.current && scroller.current.scrollHeight <= scroller.current.clientHeight) bottom()
    else if (list.some((m) => m.created && m.created > leftAt.current && m.k !== "thinking")) setUnread(true)
  }, [list.length, list.at(-1)?.id])
  const preserveAnchor = (anchor: Anchor) => {
    if (preserving.current === anchor) return
    preserving.current = anchor
    const selector = `[data-msg="${CSS.escape(anchor.id)}"]`
    const settleAt = performance.now() + 150
    let index = -1, aligned = 0, frames = 0
    const position = () => {
      const el = scroller.current, list = virtual.current
      if (preserving.current !== anchor || !el || !list) return
      // A page prepend can move this key while the previous layout is settling.
      if (entryIndex.current[index]?.id !== anchor.id) {
        index = entryIndex.current.findIndex((row) => row.id === anchor.id)
        if (index < 0) { bottom(); return }
      }
      const row = el.querySelector<HTMLElement>(selector)
      const item = row && list.getVirtualItems().find((item) => item.index === index)
      // The outer row's DOM position can lag a new virtual range. Measure only
      // the message's local offset (e.g. after a date divider) inside that row.
      const offset = row && item ? item.start + row.offsetTop - anchor.offset : list.getOffsetForIndex(index, "start")?.[0]
      const moved = offset !== undefined && Math.abs(el.scrollTop - offset) > 1
      if (moved) list.scrollToOffset(offset)
      const landed = item && row && !moved && Math.abs(row.getBoundingClientRect().top - el.getBoundingClientRect().top - anchor.offset) <= 1
      aligned = landed ? aligned + 1 : 0
      if ((aligned < 3 || performance.now() < settleAt) && ++frames < 30) requestAnimationFrame(position)
      else preserving.current = null
    }
    // Try in the same commit as the prepend. Keep reconciling on later frames
    // if newly mounted rows still need to be measured.
    position()
  }
  /* rendered Markdown and folded steps grow after the first scroll: stay at the bottom until the user scrolls away */
  useEffect(() => {
    const el = scroller.current, inner = el?.firstElementChild; if (!el || !inner) return
    const gap = () => el.scrollHeight - el.scrollTop - el.clientHeight
    const near = () => {
      if (!restoring.current && !stuck.current && towardLatest.current && gap() < 40) bottom()
    }
    // A scroll begun in the bounded process must never change the outer conversation's follow mode.
    const inProcess = (e: Event) => (e.target as HTMLElement)?.closest?.("[data-chat-process-scroll]")
    const upward = () => { preserving.current = null; if (el.scrollTop > 0) { towardLatest.current = false; leave() } }
    const downward = () => { preserving.current = null; towardLatest.current = true; if (!stuck.current && gap() < 40) bottom() }
    let y = 0
    const wheel = (e: WheelEvent) => { if (!inProcess(e)) { if (e.deltaY < 0) upward(); else if (e.deltaY > 0) downward() } }
    const dragScrollbar = (e: PointerEvent) => {
      if (e.target === el && e.clientX >= el.getBoundingClientRect().left + el.clientWidth) {
        upward()
        towardLatest.current = true
      }
    }
    const touch = (e: TouchEvent) => { y = e.touches[0]?.clientY ?? 0 }
    const swipe = (e: TouchEvent) => { const next = e.touches[0]?.clientY ?? y; if (!inProcess(e)) { if (next > y) upward(); else if (next < y) downward() } y = next }
    const key = (e: KeyboardEvent) => {
      if (inProcess(e)) return
      if (["PageUp", "ArrowUp", "Home"].includes(e.key)) upward()
      else if (["PageDown", "ArrowDown", "End"].includes(e.key)) downward()
    }
    el.addEventListener("wheel", wheel, { passive: true }); el.addEventListener("pointerdown", dragScrollbar); el.addEventListener("touchstart", touch, { passive: true }); el.addEventListener("touchmove", swipe, { passive: true }); el.addEventListener("scroll", near); el.addEventListener("keydown", key)
    window.addEventListener("steve-open-steps", hold)
    const unfold = (e: Event) => {
      const toggle = (e.target as HTMLElement).closest?.<HTMLElement>("[data-steps-toggle]")
      if (!toggle) return
      const wasFollowing = stuck.current
      // Holding the scroll position is immediate; the jump affordance doesn't
      // have to re-render the conversation and rail in the click task.
      if (wasFollowing) startTransition(() => setBack(prev => prev ?? "latest"))
      hold()
      // Unfolding the latest run reveals its bounded panel before we hold the reading position.
      if (wasFollowing) requestAnimationFrame(() => {
        if (stuck.current || toggle.getAttribute("aria-expanded") !== "true") return
        const panel = document.getElementById(toggle.getAttribute("aria-controls") ?? "")
        const excess = panel ? panel.getBoundingClientRect().bottom - el.getBoundingClientRect().bottom + 48 : 0
        if (excess > 0) el.scrollTop += excess
      })
    }
    el.addEventListener("click", unfold)
    let height = el.clientHeight
    const ro = new ResizeObserver(() => {
      const grew = el.clientHeight > height
      height = el.clientHeight
      if (stuck.current) {
        if (gap() >= 2) scrollToEnd()
      } else if (grew && !held.current && !restoring.current && gap() < 2) {
        // The latest message is visible again because the viewport grew, not
        // because a virtual history resize temporarily reduced the scroll gap.
        bottom()
      }
    }); ro.observe(inner); ro.observe(el)
    return () => { ro.disconnect(); el.removeEventListener("wheel", wheel); el.removeEventListener("pointerdown", dragScrollbar); el.removeEventListener("touchstart", touch); el.removeEventListener("touchmove", swipe); el.removeEventListener("scroll", near); el.removeEventListener("keydown", key); window.removeEventListener("steve-open-steps", hold); el.removeEventListener("click", unfold) }
  }, [])
  /* the top of the list: the conversation before what was loaded comes in (yours only) */
  useEffect(() => {
    const el = scroller.current, t = top.current; if (conv || !el || !t) return
    const io = new IntersectionObserver((es) => {
      if (!es[0]?.isIntersecting || stuck.current || restoring.current || !historyState().more || historyState().loading) return
      void loadOlder()
    }, { root: el, rootMargin: "300px 0px 0px 0px" })
    io.observe(t); return () => io.disconnect()
  }, [conv, hasMessages])
  /* a jump (a search hit, a topic, a quote): load what lies between, show the message, offer the way back */
  const go2 = useRef<(id: string, created?: number) => void>(() => {})
  go2.current = async (id, created) => {
    const request = Symbol()
    jumpRequest.current = request
    const el = scroller.current
    const atEnd = el && el.scrollHeight - el.scrollTop - el.clientHeight < 2
    const from: Anchor | "latest" | null = stuck.current || atEnd ? "latest" : anchorNow()
    hold()
    if (!list.some((m) => m.id === id) && created != null) await fillTo(created)
    requestAnimationFrame(() => requestAnimationFrame(() => {
      if (jumpRequest.current !== request) return
      jumpRequest.current = null
      const index = entryIndex.current.findIndex((row) => row.id === id)
      if (index < 0) { toast(tr("pages:chat.notInThread")); return }
      // Paging preserves the old anchor until the virtual list can resolve the new one.
      preserving.current = null
      virtual.current?.scrollToIndex(index, { align: "center" })
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
    const show = () => { restoring.current = false; if (entryIndex.current.some((row) => row.id === pos.id)) { preserveAnchor(pos); setBack("latest") } else bottom() }
    if (list.some((m) => m.id === pos.id)) requestAnimationFrame(show)
    else void fillTo(pos.created).then(() => requestAnimationFrame(() => requestAnimationFrame(show)))
  }, [conv, st.ready, list.length])
  useEffect(() => {
    if (conv) return
    const el = scroller.current; if (!el) return
    let t: any
    const save = () => {
      if (!restored.current || restoring.current) return
      if (stuck.current) savePos(posKey, { bottom: true })
      else {
        const a = preserving.current ?? anchorNow()
        if (a) savePos(posKey, { bottom: false, ...a })
      }
    }
    const later = () => { clearTimeout(t); t = setTimeout(save, 300) }
    el.addEventListener("scroll", later, { passive: true }); window.addEventListener("pagehide", save)
    return () => { clearTimeout(t); save(); el.removeEventListener("scroll", later); window.removeEventListener("pagehide", save) }
  }, [conv, posKey])
  const lastMe = list.findLast((m) => m.k === "me")?.id, lastSteve = list.findLast((m) => m.k === "steve" && !m.notice)?.id
  const fresh = unread ? list.filter((m) => m.created && m.created > leftAt.current && m.k !== "thinking").length : 0
  return (
    <div className="flex h-full min-h-0 overflow-hidden text-foreground" data-chat-page data-chat-conversation={conv?.replace(/^chan:/, "")} data-chat-conversation-ready={conv ? !!im && !imLoading && !imError : undefined}><div className="relative flex min-w-0 flex-1 flex-col">
      <header className="flex h-13 shrink-0 items-center gap-2 border-b border-border/70 px-3 sm:px-5" data-chat-header>
        {!conv && <MineButton />}
        <Hero />
        <div className="flex-1" />
        {!conv && <HistoryToggle />}
        <RailToggle />
      </header>
      {channelsError && <div role="alert" className="mx-auto flex w-full max-w-[800px] items-center gap-2 px-4 type-footnote text-destructive sm:px-8">{channelsError}<Button size="xs" variant="ghost" onClick={retryChannels}>{t("common:action.retry")}</Button></div>}
      <ChannelSwitcher channels={channels} current={conv ? im?.badge ?? null : undefined} />
      <ConvSwitcher channels={channels} current={conv} badge={im?.badge} />
      {conv && im && <div className="mx-auto flex w-full max-w-[800px] shrink-0 px-4 pb-1 sm:px-8"><ConvBadgeChip b={im.badge} /></div>}
      {/* the conversation never scrolls sideways: wide code and tables scroll in their own box, and a pixel too wide here
       * put a horizontal scrollbar line under the messages on every swipe */}
      <div className="relative flex min-h-0 flex-1 flex-col"><div ref={scroller} tabIndex={-1} className="min-h-0 flex-1 overflow-y-auto overflow-x-hidden overscroll-none [overflow-anchor:none] outline-none" data-chat-scroll>
        <div className="mx-auto flex w-full max-w-[800px] flex-col gap-8 px-4 pb-10 pt-5 sm:px-8" data-chat-messages>
          {!conv && list.length > 0 && <div ref={top} className="-mb-4 flex min-h-5 justify-center type-footnote text-tertiary" data-chat-older={loading ? "loading" : more ? "more" : ended ? "start" : "none"}>
            {loading ? <span className="flex items-center gap-2"><Spinner className="size-3.5" />{t("pages:chat.loadingOlder")}</span> : more ? <button type="button" className="hover:text-foreground" onClick={() => void loadOlder()}>{t("pages:chat.older")}</button> : ended && <span>{t("pages:chat.start")}</span>}
          </div>}
          {conv && im && <OlderRecords more={!!im.next} loading={loadingOlder} onMore={olderRecords} />}
          {conv && imLoading && <p role="status" className="flex items-center justify-center gap-2 py-3 type-callout text-muted-foreground" data-conv-loading><Spinner className="size-3.5" />{t("common:state.loading")}</p>}
          {conv && imError && <div role="alert" className="flex flex-wrap items-center justify-center gap-2 py-3 type-callout text-destructive" data-conv-error><span>{t("pages:chat.loadFailed")}</span><Button size="sm" variant="outline" onClick={reloadIm} data-conv-retry>{t("common:action.retry")}</Button></div>}
          {conv && !imLoading && !imError && im === null && <div className="py-16 text-center type-body text-muted-foreground" data-conv-missing>{t("pages:chat.missing")}</div>}
          {entries.length > 0 && <ReadingPosition rows={entries}
            capture={() => !stuck.current && !restoring.current ? preserving.current ?? anchorNow() : null}
            restore={preserveAnchor}>
            <VirtualList rows={entries} scroll={scroller} handle={virtual} estimate={estimateMessage} gap={32} startAtEnd={stuck.current} render={({ entry, index }) => {
            const before = entries[index - 1]?.entry
            const beforeAt = before?.k === "rec" ? before.r.created : before?.m.created
            const at = entry.k === "rec" ? entry.r.created : entry.m.created
            const day = !!at && (!beforeAt || new Date(beforeAt).toDateString() !== new Date(at).toDateString())
            if (entry.k === "rec") return <><Day at={day ? at : undefined} /><RecordRow r={entry.r} provider={im?.badge.open_provider ?? im?.badge.provider} last={entry.r.id === im?.records.at(-1)?.id} /></>
            const m = entry.m, id = m.id
            return <MessageRow id={id} channel={conv ? m : undefined} previous={before?.k === "msg" ? before.m : undefined}
              day={day} last={id === (m.k === "me" ? lastMe : lastSteve)} flashing={id === flash}
              raw={id ? im?.raw.get(id) : undefined} handling={id ? woven?.byTurn.get(id)?.handling : undefined}
              openUrl={im?.badge.open_url} openProvider={im?.badge.open_provider ?? im?.badge.provider} memory={processMemory} name={S?.n ?? "Steve"} />
          }} /></ReadingPosition>}
          {conv && im ? <ConversationDraft conv={conv} decisionIds={imDecisions} provider={im.badge.open_provider ?? im.badge.provider} /> : !conv && <LiveTail memory={processMemory} />}
          {!conv && !list.length && st.ready && st.onboard < 0 && <div className="flex flex-col items-center gap-3 px-6 pt-[18vh] text-center anim-in" data-chat-empty>
            <ActorAvatar actorId="steve" size={48} tooltip={false} />
            <div className="type-title">{t("pages:chat.emptyHead", { name: S?.n ?? "Steve" })}</div>
            <p className="max-w-[460px] text-balance type-callout text-muted-foreground">{t("pages:chat.emptyBody", { name: S?.n ?? "Steve" })}</p>
          </div>}
          {!conv && away > 0 && <Away from={away} />}
          {!conv && st.onboard >= 0 && <Suspense fallback={null}><Onboarding /></Suspense>}
        </div>
      </div>
      {/* pinned to the list, not the page: the composer's height differs between phone and desktop and grows with the queue */}
      {back ? <div className="absolute bottom-3 left-1/2 flex -translate-x-1/2 items-center gap-0.5 rounded-full bg-popover p-1 shadow-lg anim-in" data-jump-bar>
          {back !== "latest" && <Button size="sm" variant="ghost" className="rounded-full" onClick={() => { setBack(null); preserveAnchor(back); if (latest() > leftAt.current) setUnread(true) }} data-jump-back><Undo2Icon />{t("pages:chat.jumpBack")}</Button>}
          <Button size="sm" variant="ghost" className="rounded-full text-muted-foreground" onClick={bottom} data-jump-latest>{fresh ? t("pages:chat.newCount", { count: fresh }) : t("pages:chat.latest")}<ArrowDownIcon /></Button>
        </div>
        : unread && <Button className="absolute bottom-3 left-1/2 -translate-x-1/2 rounded-full shadow-lg anim-in" size="sm" onClick={bottom} data-new-messages>{fresh ? t("pages:chat.newCount", { count: fresh }) : t("pages:chat.newMessages")}<ArrowDownIcon /></Button>}</div>
      <div className="mx-auto w-full max-w-[800px] shrink-0 px-4 pb-[76px] pt-3 sm:px-8 md:pb-5" data-chat-input>
        {conv ? im && <ImReadOnly b={im.badge} /> : <>
        <ComposerDecisions skip={inThread} focus={focusItem()} threadItems={threadCardItems()} />
        <ComposerBars />
        <div className="flex min-w-0 justify-start px-1" data-chat-model><SteveModelControl /></div>
        <ChatComposer draftKey={draftKey(st.workspace?.id, "chat")} handle={composer} queue={<ChatQueue composer={composer} />} placeholder={focusItem() ? t("pages:chat.placeholderFocus", { item: focusItem() }) : chatBusy() ? t("pages:chat.placeholderBusy", { name: S?.n ?? "Steve" }) : t("pages:chat.placeholder", { name: S?.n ?? "Steve" })} testId="chat" onSend={(t, files, refs, mentions, clientId) => { bottom(); return chatSend(t, files, refs, mentions, clientId) }} onStop={stopReply}
          onCommand={(c) => { if (c === "digest") { void chatSend("/汇报"); return } openSheet(c) }} />
        </>}
      </div>
    </div>{!conv && <HistoryPanel />}<ChatRail /></div>
  )
})

/** the day line in the conversation: 今天, 昨天, then the date */
const chatDay = (ts: number) => dayLabel(ts)

function Day({ at }: { at?: number }) {
  return at ? <div className="mb-8 flex items-center gap-4 type-label text-tertiary" data-chat-day><span className="h-px flex-1 bg-border/70" /><span>{chatDay(at)}</span><span className="h-px flex-1 bg-border/70" /></div> : null
}

function ChatComposer(props: ComponentProps<typeof Composer>) {
  const busy = useChatLive((s) => s.thinking || !!s.live)
  return <Composer {...props} busy={busy} />
}

const LiveTail = memo(function LiveTail({ memory }: { memory: ProcessMemory }) {
  const mid = useChatLive((s) => s.draft?.mid)
  const working = useChatLive((s) => !!(s.draft || s.live || s.thinking))
  const sending = useChatLive((s) => s.sending)
  if (!working && !sending.some((m) => !m.held)) return null
  return <div className="flex flex-col gap-2" data-chat-draft={mid} aria-busy="true">
    {sending.filter((m) => !m.held).map((m) => <Me key={m.clientId} t={m.t ?? ""} files={m.files} />)}
    {working && <>{mid && <LiveReply key={mid} mid={mid} />}<LiveSteps memory={memory} /></>}
  </div>
})

function LiveReply({ mid }: { mid: string }) {
  const [text, setText] = useState("")
  useEffect(() => {
    let shown = ""
    const publish = () => {
      const draft = presentedChatLive().draft
      if (draft?.mid !== mid || draft.text === shown) return
      // An external-store update must render synchronously; this local state
      // update can yield if input arrives while the markdown is reconciling.
      shown = draft.text
      startTransition(() => setText(draft.text))
    }
    const unsubscribe = observeChatLive(publish)
    publish()
    return unsubscribe
  }, [mid])
  return text ? <SteveSays still><Markdown text={text} streaming /></SteveSays> : null
}

const MessageRow = memo(function MessageRow({ id, channel, previous, day, last, flashing, raw, handling, openUrl, openProvider, memory, name }: {
  id?: string; channel?: Msg; previous?: Msg; day: boolean; last: boolean; flashing: boolean
  raw?: ConvMessage; handling?: ComponentProps<typeof HandlingLine>["h"]; openUrl?: string | null; openProvider?: string | null
  memory: ProcessMemory; name: string
}) {
  const selected = useThreadMessage(channel ? undefined : id)
  const m = channel ?? selected, conv = !!channel
  const drafted = useChatLive((s) => !!id && s.drafted === id)
  const t = useT()
  if (!m) return null
  const dayLabel = <Day at={day ? m.created : undefined} />

  /* the jumped-to message: a wash with a soft halo, both fading in and out together; the radius stays when it fades */
  const lit = m.id && flashing ? "bg-blue/8 shadow-[0_0_0_8px_color-mix(in_srgb,var(--blue)_8%,transparent)]" : ""
  /* a quote of the message right above says nothing the eye does not already see */
  const replyTo = m.replyTo && m.replyTo !== previous?.id ? m.replyTo : undefined
  /* his name and the time head a run of his messages, not every one of them */
  /* an item's report is the card's delivery, folded there; not a second, full copy above it */
  const delivered = !!m.card && !m.cardOnly && !!m.t && m.card.delivery?.text === m.t
  const head = !(previous?.k === "steve" && !previous.notice && !day && m.created && previous.created && m.created - previous.created < RUN_GAP)
  if (m.k === "me") {
    return <>
      {dayLabel}
      {conv ? <div className="group/me relative flex flex-col gap-1.5" data-msg={id} data-im-turn>
        <SenderLine m={raw} />
        {raw?.channel_message ? <div className="w-full max-w-[600px] self-end"><ChannelMessageBody message={raw.channel_message} provider={openProvider} /></div>
          : <Me t={m.t ?? ""} files={m.files} />}
        <HandlingLine h={handling} reacted={!!raw?.reaction && raw.reaction.state !== "cleared"} />
        <QueuedNote m={m} />
        <ImMsgActions url={raw?.channel_message?.open_url ?? openUrl} provider={raw?.channel_message?.open_provider ?? openProvider} side="end" last={last} />
      </div> : <div tabIndex={-1}
        className={cn("group/me relative flex flex-col gap-1.5 rounded-xl outline-none transition-[background-color,box-shadow] duration-260 ease-(--ease-out)", lit)}
        data-msg={id} data-flash={lit ? "" : undefined}>
        <QuoteHead id={replyTo} className="self-end" />
        {m.a2uiAction ? <A2uiActionNote a={m.a2uiAction} /> : <Me t={m.t ?? ""} files={m.files} />}
        <QueuedNote m={m} />
        <MeActions m={m} last={last} />
      </div>}
    </>
  }
  if (m.notice) return <>{dayLabel}<NoticeLine m={m} onOpen={showNotices} /></>
  return <>
    {dayLabel}
    <div tabIndex={-1}
      className={cn("group/sm relative flex min-w-0 flex-col gap-2.5 rounded-xl outline-none transition-[background-color,box-shadow] duration-260 ease-(--ease-out)", lit)}
      data-msg={id} data-attention-reply={!conv && m.k === "steve" && !drafted ? id : undefined} data-flash={lit ? "" : undefined}>
      {head && <div className="flex min-w-0 items-center gap-2 px-1 type-footnote text-tertiary" data-chat-author>
        <ActorAvatar actorId="steve" size={18} />
        <span className="truncate type-callout font-semibold text-foreground">{name}</span>
        {m.created && <time className="shrink-0" dateTime={new Date(m.created).toISOString()}>{tm(m.created)}</time>}
      </div>}
      <QuoteHead id={replyTo} />
      {conv ? <StepsBlock m={m} memory={memory} /> : <TurnInterruption m={m} />}
      <SteveSays still={drafted}>
        {!m.update && !m.cardOnly && !delivered && <A2uiHost msg={id} records={m.a2ui} readonly={conv ? t("pages:chat.readonlyUi") : undefined}>
          <Markdown text={linkCites(m.t ?? "", m.cites)} replyFiles />
        </A2uiHost>}
        {m.item && !m.update && !m.card && (m.wasCard || threadCardItems().has(m.item)
          ? <CardLink id={m.item} from={id} /> : <ItemRef id={m.item} />)}
        {m.card ? <ItemCard c={m.card} /> : <Updates m={m} />}
        <OfferLine m={m} /><AskChips m={m} conv={conv} /><Rerun m={m} conv={conv} />
      </SteveSays>
      {m.meet && meeting() && id === st.settings.onboardFrom && <Button size="sm" variant="ghost"
        className="-mt-1 self-start text-muted-foreground" onClick={() => void endMeeting()} data-meet-skip>{t("pages:chat.onboard.meetSkip")}</Button>}
      {m.cfgs && <div className="-mt-1 flex flex-col gap-2 self-start">{m.cfgs.map((p) => <ProposalCard key={p.id} p={p} inChat />)}</div>}
      <Cites cites={m.cites} />
      {!conv && <StepsBlock m={m} memory={memory} />}
      {conv ? <><ChanOutNote m={raw} /><ImMsgActions url={openUrl} side="start" last={last} /></> : <MsgActions m={m} last={last} />}
    </div>
  </>
})

const cardSets = new WeakMap<ReturnType<typeof messageViews>, Set<string>>()
function threadCardItems() {
  const views = messageViews()
  let cards = cardSets.get(views)
  if (!cards) {
    cards = new Set(views.flatMap(({ id }) => {
      const item = threadMsg(id)?.card?.item
      return item ? [item] : []
    }))
    cardSets.set(views, cards)
  }
  return cards
}
