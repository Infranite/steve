/* Thread routing in the conversation (thread-routing.md §3.4–§5): the offer under an answer, suggestion cards, ask
 * options, the research being read, queued questions, quotes, flow-back from items and the focus bar. Built from the
 * existing parts (ItemRef, DecisionCard, Trace) — no new colours. */
import { useEffect, useLayoutEffect, useRef, useState } from "react"
import { itemDisplayState } from "@/item-workflow-relation"
import { cn } from "cn"
import { CornerDownRightIcon, FileTextIcon, LibraryIcon, RotateCcwIcon, SearchIcon, SquareIcon, XIcon, CircleCheckIcon, BanIcon, CircleAlertIcon, MessageSquareReplyIcon, CheckIcon, ChevronDownIcon, RocketIcon, TerminalIcon, PencilIcon, GlobeIcon } from "lucide-react"
import { Button } from "@/components/ui/button"
import { Spinner } from "@/components/ui/spinner"
import { Trail, useTick, type StepState, type ToolStep, type TrailItem } from "@/components/work-trail"
import { Markdown } from "@/components/markdown"
import { Textarea } from "@/components/ui/textarea"
import { clip as clipPlain } from "@/thread/topics"
import { STEVE_TOOL, st, tm, emit, chatSend, takeOffer, chooseSuggest, cancelQueued, stopRun, clearFocus, focusItem, quoteMsg, decisionDraft, decide, acceptChoosesRelease, botName, alertClosed, closeAlerts, type Msg, type SuggestChoice, type Steps, type Update, type Item, type ItemCardState } from "@/store"
import { ItemRef, DecisionCard, ItemNo, AlertClose, go, Pill } from "@/ui"
import { jumpToMsg, threadMsg } from "@/thread/history"
import { Trans } from "react-i18next"
import { flushSync } from "react-dom"
import { t as tr, useT } from "@/i18n"
import { toolClass, toolCounts, toolShort, meetStep, type ToolClass } from "@/thread/tools"

/* ───────── research ───────── */
const secs = (ms: number) => tr("common:dur.seconds", { count: Math.max(1, Math.round(ms / 1000)) })
/** "读了 9 个文件 · 搜索 4 次 · 执行 3 条命令 · 41 秒" for a finished research run */
export function researchSummary(s: Steps): string {
  const ts = s.tools, r = s.research, n = toolCounts(ts)
  /* Steve's own writes of the first meeting are not "calls": they are what the answer says it saved */
  const parts = [n.files && tr("thread:research.files", { count: n.files }), n.search && tr("thread:research.searches", { count: n.search }),
    n.exec && tr("thread:research.commands", { count: n.exec }), n.edit && tr("thread:research.edits", { count: n.edit }), n.fetch && tr("thread:research.fetches", { count: n.fetch }),
    n.kb && tr("thread:research.kb", { count: n.kb }), n.item && tr("thread:research.items", { count: n.item }), n.meet && tr("thread:research.saves", { count: n.meet }),
    n.other && tr("thread:research.calls", { count: n.other })].filter(Boolean) as string[]
  if (!parts.length) parts.push(tr("thread:research.noFiles"))
  const failures = ts.filter((t) => t.status === "failed").length
  if (failures) parts.push(tr("thread:research.failures", { count: failures }))
  if (r?.ms != null) parts.push(secs(r.ms))
  const stop = r?.stopped ? tr(`thread:research.stopped.${r.stopped}`) : ""
  return [...parts, stop].filter(Boolean).join(" · ")
}
const ICON: Partial<Record<ToolClass, typeof FileTextIcon>> = { read: FileTextIcon, search: SearchIcon, kb: LibraryIcon, exec: TerminalIcon, edit: PencilIcon, fetch: GlobeIcon, meet: CheckIcon }
/** the icon of a research tool row */
export function researchIcon(name: string, kind?: string) { return ICON[toolClass(name, kind)] ?? FileTextIcon }
/** "读" / "执行" / "保存画像": the verb of one tool row; a tool Steve has no word for goes by its own name */
export function researchToolLabel(name: string, kind?: string): string {
  const c = toolClass(name, kind), step = meetStep(name)
  return c === "meet" ? (step ? tr(`thread:research.tool.meet.${step}`) : toolShort(name)) : c === "other" ? STEVE_TOOL[toolShort(name)] ?? toolShort(name) : tr(`thread:research.tool.${c}`)
}
/** "正在读" / "正在搜索": the live line's verb for a tool */
const liveLabel = (name: string, kind?: string) => { const c = toolClass(name, kind); return c === "other" ? tr("thread:research.live.other", { name: toolShort(name) }) : tr(`thread:research.live.${c}`) }
const GROUP: Partial<Record<ToolClass, "read" | "search" | "run" | "fetch" | "edit">> = { read: "read", search: "search", exec: "run", fetch: "fetch", edit: "edit" }
/** one research call on the trail: calls of one kind in a row merge, their briefs as chips */
export function researchStep(key: string, name: string, kind: string | undefined, brief: string, state: StepState, more: Partial<ToolStep> = {}): ToolStep {
  const c = toolClass(name, kind), g = GROUP[c], label = researchToolLabel(name, kind)
  const merged = (calls: number, distinct: number, running: boolean) => g ? tr(`core:trail.group.${g}${running ? "Live" : ""}` as "core:trail.group.read", { count: g === "read" || g === "edit" ? distinct : calls }) : tr("core:trail.group.tool", { name: label, count: calls })
  return { t: "tool", key, state, verb: [liveLabel(name, kind), label], subj: brief, mono: true, group: g ?? (c === "meet" ? undefined : "tool:" + name), merged,
    chip: brief ? { label: brief, mono: true, icon: researchIcon(name, kind) } : undefined, attrs: { kind: c }, ...more }
}

/** the work under way: a clock, and the calls so far on the trail, the one running now last; with a stop button */
export function ResearchLive() {
  const l = st.live, now = useTick(!!l), t = useT()
  if (!l) return null
  const items: TrailItem[] = l.tools.map((x, n) => researchStep(String(n), x.name, x.kind, x.brief, x.ok ? "done" : "fail"))
  if (l.now) items.push(researchStep("now", l.now.name, l.now.kind, l.now.brief, "run"))
  return (
    <div className="-mb-1 flex max-w-[88%] flex-col gap-1 self-start px-1" data-research-live={l.run}>
      <div className="flex min-w-0 items-center gap-1.5 type-callout">
        <ChevronDownIcon className="size-4 shrink-0 text-tertiary" />
        <span className="shimmer shrink-0 text-foreground">{t("core:transcript.working")}</span>
        <span className="shrink-0 tabular-nums text-tertiary">{secs(now - l.t0)}</span>
        {now - l.t0 > 90_000 && <span className="min-w-0 truncate text-muted-foreground">· {t("thread:research.slow")}</span>}
        <Button size="xs" variant="ghost" className="ml-1 shrink-0 text-muted-foreground" onClick={() => stopRun(l.run)} data-research-stop><SquareIcon className="fill-current" />{t("thread:research.stop")}</Button>
      </div>
      {items.length > 0 ? <Trail items={items} live /> : <Trail items={[{ t: "line", key: "start", state: "run", label: <span className="shimmer">{t("thread:research.live.read")}</span> }]} live />}
    </div>
  )
}

/* ───────── offer · suggestion card · ask ───────── */
/** "要我改吗？ [做成事项：…] [不用]" under an answer */
export function OfferLine({ m }: { m: Msg }) {
  const o = m.offer, [busy, setBusy] = useState(false), [open, setOpen] = useState(false), t = useT()
  if (!o) return null
  if (o.state === "taken") return o.item ? (st.thread.some((x) => x.card?.item === o.item) ? <CardLink id={o.item} /> : <ItemRef id={o.item} />) : null
  if (o.state === "dismissed") return null
  const act = async (take: boolean) => { setBusy(true); try { await takeOffer(m, take) } finally { setBusy(false) } }
  if (o.folded && !open) return <button type="button" className="mt-2 self-start type-callout text-muted-foreground hover:text-foreground" onClick={() => setOpen(true)} data-offer-folded={m.id}>{t("thread:offer.folded", { title: o.title })}</button>
  return (
    <div className="mt-3 flex flex-wrap items-center gap-2 anim-in" data-offer={m.id}>
      <span className="type-callout text-muted-foreground">{t("thread:offer.question")}</span>
      <Button size="sm" variant="outline" className="max-w-full rounded-full" disabled={busy} onClick={() => void act(true)} data-offer-take><span className="truncate">{t("thread:offer.take", { title: o.title })}</span></Button>
      <Button size="sm" variant="ghost" className="text-muted-foreground" disabled={busy} onClick={() => void act(false)} data-offer-dismiss>{t("thread:offer.dismiss")}</Button>
    </div>
  )
}

/** a suggestion card: answer here or make it an item; greyed once expired, still clickable */
export function SuggestCard({ m }: { m: Msg }) {
  const s = m.suggest, [busy, setBusy] = useState(false), t = useT()
  if (!s) return null
  const chosen = s.state === "chosen"
  const act = async (c: SuggestChoice) => { setBusy(true); try { await chooseSuggest(m, c) } finally { setBusy(false) } }
  return (
    <div className={cn("mt-3 flex flex-col gap-2", s.state === "expired" && "opacity-60")} data-suggest={m.id} data-suggest-state={s.state}>
      {chosen ? <div className="type-callout text-muted-foreground">{s.chosen === "here" ? t("thread:suggest.choseHere") : s.item ? "" : t("thread:suggest.choseItem")}</div>
        : <div className="flex flex-wrap gap-2">{s.choices.map((c) => (
          <Button key={c.id} size="sm" variant={(s.choices.some((x) => x.recommended) ? c.recommended : c.id === "here") ? "default" : "outline"} className="max-w-full rounded-full" disabled={busy} onClick={() => void act(c.id)} data-suggest-choice={c.id}><span className="truncate">{c.label}</span></Button>))}
          {s.state === "expired" && <span className="self-center type-footnote text-tertiary">{t("thread:suggest.expired")}</span>}</div>}
      {s.item && (st.thread.some((x) => x.card?.item === s.item) ? <CardLink id={s.item} /> : <ItemRef id={s.item} />)}
    </div>
  )
}

/** questions answered from this page and still on their way: a second click sends nothing */
const answering = new Set<string>()
/** the options of a follow-up question: one click sends it, once; answered (you said something after it) they are gone.
 *  `conv`: an IM conversation, where the question is for the people in the chat — the options are what they can reply,
 *  not buttons that would speak in your own conversation */
export function AskChips({ m, conv }: { m: Msg; conv?: boolean }) {
  const t = useT()
  if (!m.ask?.options?.length) return null
  if (conv) return <div className="mt-2 type-callout text-muted-foreground" data-ask={m.id} data-ask-readonly>{t("thread:ask.canReply", { options: m.ask.options.join(" / ") })}</div>
  const at = st.thread.findIndex((x) => x === m || (!!m.id && x.id === m.id))
  if (at >= 0 && st.thread.slice(at + 1).some((x) => x.k === "me")) return null
  const key = m.id ?? ""
  const send = async (o: string) => {
    if (answering.has(key)) return
    answering.add(key)
    try { await chatSend(o) } finally { answering.delete(key) }
  }
  return <div className="mt-3 flex flex-wrap gap-2 anim-in" data-ask={m.id}>{m.ask.options.map((o) => <Button key={o} size="sm" variant="outline" className="rounded-full" onClick={() => void send(o)} data-ask-opt={o}>{o}</Button>)}</div>
}

/** "（Hub 重启，没答完）": ask the question again (your own conversation only: an IM turn is asked in its chat) */
export function Rerun({ m, conv }: { m: Msg; conv?: boolean }) {
  const t = useT()
  if (!m.rerun || conv) return null
  const q = st.thread.find((x) => x.id === m.rerun)
  return q?.t ? <div className="mt-3"><Button size="sm" variant="outline" className="rounded-full" onClick={() => void chatSend(q.t!)} data-rerun><RotateCcwIcon />{t("thread:rerun")}</Button></div> : null
}

/* ───────── queue · quote ───────── */
/** under a user message waiting behind a research run */
export function QueuedNote({ m }: { m: Msg }) {
  const t = useT()
  if (!m.queued) return null
  const run = m.queued.run
  return <div className="flex items-center gap-2 self-end type-footnote text-muted-foreground anim-in" data-queued={m.id}><Spinner className="size-3" />{t("thread:queued")}{run && <button type="button" className="hover:text-foreground hover:underline" onClick={() => cancelQueued(run)} data-queued-cancel>{t("common:action.cancel")}</button>}</div>
}

/** scroll to a message and flash it; the conversation offers the way back (ST-50) */
export function jumpTo(id: string) { jumpToMsg(id, threadMsg(id)?.created) }
/* a quoted message in one line, its Markdown marks dropped */
const clip = (t: string, n = 24) => clipPlain(t, n)
/** "↳ 你 18:09 说的「…」": the message this one answers or quotes */
export function QuoteHead({ id, className }: { id?: string; className?: string }) {
  const t = useT()
  if (!id) return null
  const q = threadMsg(id)
  if (!q) return null
  return (
    <button type="button" onClick={() => jumpTo(id)} className={cn("flex max-w-full items-center gap-1 self-start truncate type-footnote text-muted-foreground hover:text-foreground", className)} data-quote={id}>
      <CornerDownRightIcon className="size-3.5 shrink-0" /><span className="truncate">{t(q.k === "me" ? "thread:quote.me" : "thread:quote.steve", { time: tm(q.created), text: clip(q.t ?? "") })}</span>
    </button>
  )
}

/* ───────── flow-back from an item (§5.2) ───────── */
function UpdateRow({ u }: { u: Update }) {
  const i = st.items.find((x) => x.id === u.item), [full, setFull] = useState(false), t = useT()
  const w = u.decision ? i?.waits.find((x) => x.decision === u.decision) : undefined
  if (u.kind === "done" || u.kind === "drop") {
    const I = u.kind === "done" ? CircleCheckIcon : BanIcon
    return <div className="flex items-center gap-2 type-callout" data-update={u.kind}><I className={cn("size-4 shrink-0", u.kind === "done" ? "text-success" : "text-muted-foreground")} strokeWidth={1.75} /><ItemNo id={u.item} /><span className="min-w-0 truncate">{u.text || t(u.kind === "done" ? "thread:update.done" : "thread:update.drop", { title: i?.t ?? "" })}</span></div>
  }
  if ((u.kind === "ask" || u.kind === "fail") && w && i) return <div className="flex flex-col gap-2" data-update={u.kind}>{u.text && u.kind === "fail" && <div className="flex items-center gap-2 type-callout"><CircleAlertIcon className="size-4 text-destructive" strokeWidth={1.75} />{u.text}</div>}<DecisionCard i={i} w={w} showItem={false} /></div>
  if (u.kind === "deliver") {
    const all = full ? decisionDraft(u.decision ?? "") : undefined
    return (
      <div className="flex flex-col gap-2" data-update="deliver">
        <div className="flex items-center gap-1.5 type-label text-tertiary"><ItemNo id={u.item} />{t(w ? "thread:update.deliveredAccept" : "thread:update.delivered")}</div>
        <Markdown text={all ?? u.text ?? ""} className="type-chat" />
        {!u.full && u.decision && <button type="button" className="self-start type-callout text-muted-foreground hover:text-foreground" onClick={() => setFull(!full)} data-update-expand>{full ? t("thread:update.collapse") : t("thread:update.expandFull")}</button>}
        {w && i && <DecisionCard i={i} w={w} showItem={false} />}
      </div>
    )
  }
  /* reply, or an ask/fail already answered */
  return <div className="flex flex-col gap-1" data-update={u.kind}>{u.ref && <QuoteHead id={u.ref} />}{u.text && <Markdown text={u.text} className="type-chat" />}</div>
}
/** what an item made from the conversation sent back: one update on its own message, or the card that grows on the announcement */
export function Updates({ m }: { m: Msg }) {
  const us = [...(m.updates ?? []), ...(m.update ? [m.update] : [])]
  if (!us.length) return null
  return <div className="mt-3 flex flex-col gap-3 border-l-2 border-border pl-4" data-updates={m.id}>{us.map((u, n) => <UpdateRow key={n} u={u} />)}</div>
}

/* ───────── an item's one living card (§5.2) ───────── */
type Look = { label: string; dot: string; tone: "run" | "wait" | "fail" | "done" | "quiet"; version?: string }
/** what the card says the item is doing now: live from the item, with the card for what the item no longer shows (released) */
export function cardLook(i: Item, c?: ItemCardState): Look {
  const w = i.waits[0]
  if (i.st === "done") return c?.released ? { label: tr("thread:card.status.released"), version: c.released.version, dot: "done", tone: "done" } : { label: tr("thread:card.status.done"), dot: "done", tone: "done" }
  if (i.st === "drop") return { label: tr("thread:card.status.dropped"), dot: "drop", tone: "quiet" }
  /* closed as failed: over, like done and dropped, not a wait on you */
  if (i.st === "fail") return { label: tr("thread:card.status.closedFailed"), dot: "fail", tone: "fail" }
  if (w?.type === "fail") return { label: tr("thread:card.status.failed"), dot: "fail", tone: "fail" }
  if (w?.type === "accept") return { label: tr("thread:card.status.accept"), dot: "wait", tone: "wait" }
  if (w?.subject === "release") return { label: tr("thread:card.status.release"), dot: "wait", tone: "wait" }
  if (w?.subject === "tool_call") return { label: tr("thread:card.status.toolCall"), dot: "wait", tone: "wait" }
  if (w?.type === "approve") return { label: tr("thread:card.status.approve"), dot: "wait", tone: "wait" }
  if (w?.type === "ask") return { label: tr("thread:card.status.answer"), dot: "wait", tone: "wait" }
  if (w) return { label: tr("thread:card.status.waiting"), dot: "wait", tone: "wait" }
  /* the item page's one state: a paused graph, unavailable collaboration or an idle item is not "running" */
  const s = itemDisplayState(i)
  if (s.kind === "blocked") return { label: tr("thread:card.status.blocked"), dot: "block", tone: "quiet" }
  if (s.kind === "paused") return { label: tr("thread:card.status.paused"), dot: "pause", tone: "quiet" }
  if (s.kind === "graphPaused") return { label: tr("views:itemState.graphPaused"), dot: "pause", tone: "quiet" }
  if (s.kind === "unavailable") return { label: tr("views:itemState.unavailable"), dot: "fail", tone: "fail" }
  if (s.kind === "idle" && !i.rel) return { label: tr("views:itemState.idle"), dot: "pause", tone: "quiet" }
  if (i.rel?.tone === "run") return { label: tr("thread:card.status.releasing"), dot: "run", tone: "run" }
  return { label: tr("thread:card.status.running"), dot: "run", tone: "run" }
}
/* tinted status pills: the status token at a low wash, the token itself as text */

/* inside a card, Markdown headings are text: a summary that starts with "## …" never outranks the card's own title */
const FLAT = "[&_:is(h1,h2,h3,h4,h5,h6)]:[font:inherit]! [&_:is(h1,h2,h3,h4,h5,h6)]:mt-0! [&_:is(h1,h2,h3,h4,h5,h6)]:mb-0! [&_:is(h1,h2,h3,h4,h5,h6)]:font-medium!"
/** the delivery summary, two lines until opened; the full text is the acceptance draft when there is one. The fold eases its
 * height (open: from two lines to the content; shut: back, then the ellipsis returns); it is offered only when there is more */
function DeliveryBlock({ d, open: open0 = false }: { d: NonNullable<ItemCardState["delivery"]>; open?: boolean }) {
  /* open: what the button says; full: what the box shows (the longer text stays while the fold closes over it) */
  const [open, setOpen] = useState(open0), [full, setFull] = useState(open0), [more, setMore] = useState(open0), t = useT()
  const box = useRef<HTMLDivElement>(null), folded = useRef(0), timer = useRef(0)
  const draft = d.decision ? decisionDraft(d.decision) : undefined, fuller = !!draft && draft.trim() !== d.text.trim()
  const text = full && fuller ? draft! : d.text
  /* is anything cut off at two lines? measured while folded, and again when the width changes */
  useLayoutEffect(() => {
    const el = box.current
    if (!el || full) return
    const check = () => setMore(fuller || el.scrollHeight > el.clientHeight + 1)
    check()
    const ro = new ResizeObserver(check); ro.observe(el)
    return () => ro.disconnect()
  }, [full, fuller, d.text])
  useEffect(() => () => clearTimeout(timer.current), [])
  /* the height moves between two pixel values only: easing from "auto" would follow the content as it changes */
  const pin = (el: HTMLElement, px: number) => { el.style.transition = "none"; el.style.height = px + "px"; void el.offsetHeight; el.style.transition = "" }
  const toggle = () => {
    const el = box.current
    if (!el) return
    clearTimeout(timer.current)
    if (!open) {
      folded.current = el.clientHeight
      pin(el, folded.current)
      flushSync(() => { setOpen(true); setFull(true) })
      el.style.height = el.scrollHeight + "px"
      timer.current = window.setTimeout(() => { el.style.height = "" }, 280)
    } else {
      pin(el, el.clientHeight)
      el.style.height = (folded.current || 40) + "px"
      setOpen(false)
      timer.current = window.setTimeout(() => { flushSync(() => setFull(false)); el.style.height = "" }, 280)
    }
  }
  return (
    <div className="flex min-w-0 flex-col gap-1" data-card-delivery>
      <div className="type-label text-tertiary">{t("thread:card.delivery")}</div>
      <div ref={box} className={cn("min-h-0 min-w-0 break-words transition-[height] duration-260 ease-(--ease-out)", !full && "line-clamp-2", "overflow-hidden", FLAT)}>
        <Markdown text={text} className="type-callout" /></div>
      {more && <button type="button" className="flex items-center gap-0.5 self-start type-callout text-muted-foreground hover:text-foreground" onClick={toggle} aria-expanded={open} data-card-expand>
        {open ? t("thread:card.collapse") : t("thread:card.expand")}<ChevronDownIcon className={cn("size-3.5 transition-transform duration-260 ease-(--ease-out)", open && "rotate-180")} /></button>}
    </div>
  )
}

/** plain text whose URLs are links; a URL is ASCII, so the Chinese right after one stays text */
function Linked({ text }: { text: string }) {
  return <>{text.split(/(https?:\/\/[\w\-.~:/?#[\]@!$&'()*+,;=%]*[\w/#=&%-])/).map((p, n) => n % 2
    ? <a key={n} href={p} target="_blank" rel="noreferrer" className="text-foreground underline underline-offset-2 hover:text-blue">{p}</a> : p)}</>
}

/** accept or send back right on the card: the summary above is what was delivered */
function AcceptRow({ i, decision }: { i: Item; decision: string }) {
  const [rework, setRework] = useState(false), [txt, setTxt] = useState(""), [busy, setBusy] = useState(false), t = useT()
  const act = async (c: string, text?: string) => { setBusy(true); try { await decide(i.id, c, text, decision) } finally { setBusy(false) } }
  return (
    <div className="flex flex-col gap-2" data-card-accept={decision}>
      {rework && <Textarea autoFocus className="min-h-16 rounded-lg" placeholder={t("thread:card.reworkPlaceholder", { bot: botName(i.b) })} value={txt} onChange={(e) => setTxt(e.target.value)} data-rework />}
      <div className="flex flex-wrap justify-end gap-2">{rework
        ? <><Button size="sm" variant="ghost" onClick={() => setRework(false)}>{t("common:action.cancel")}</Button><Button size="sm" disabled={busy} onClick={() => void act("rework", txt.trim())}>{t("thread:card.sendRework")}</Button></>
        : <><Button size="sm" variant="outline" onClick={() => setRework(true)}>{t("thread:card.rework")}</Button>{acceptChoosesRelease(i)
          ? <><Button size="sm" variant="outline" disabled={busy} onClick={() => void act("pass", "no-release")} data-pass-no-release>{t("thread:card.passNoRelease")}</Button><Button size="sm" disabled={busy} onClick={() => void act("pass", "release")} data-pass data-pass-release><CheckIcon />{t("thread:card.passRelease")}</Button></>
          : <Button size="sm" disabled={busy} onClick={() => void act("pass")} data-pass><CheckIcon />{t("thread:card.pass")}</Button>}</>}</div>
    </div>
  )
}

/** One card per item made from the conversation: what it is doing now, its latest delivery (folded), the reply to what you
 * last sent it, and whatever it needs from you — answerable right here. Lifecycle events update it; they are not messages. */
export function ItemCard({ c, inText = false }: { c: ItemCardState; inText?: boolean }) {
  const i = st.items.find((x) => x.id === c.item), t = useT()
  if (!i) return null
  const look = cardLook(i, c), w = i.waits[0]
  const accept = w?.type === "accept" ? w : undefined
  /* the delivery the open acceptance is about, else the latest one */
  const d = c.delivery ? { ...c.delivery, decision: accept?.decision ?? c.delivery.decision } : accept ? { text: (accept.draft ?? "").split("\n\n")[0], decision: accept.decision } : undefined
  const reply = c.reply && (!c.delivery?.at || (c.reply.at ?? 0) > c.delivery.at) ? c.reply : undefined
  /* the failure is current only while the item stays stopped on it (you chose to look, it is paused): going on, being
   * resumed or finishing ends it (closing as failed keeps it) — the Hub then sets the card back to running, cards from before that are read the same way */
  const failLine = !w && c.state === "fail" && (i.st === "pause" || i.st === "fail") && !alertClosed(c.decision) ? c.text : undefined
  return (
    <div className="mt-3 flex w-full max-w-[560px] flex-col gap-3.5 rounded-xl bg-card p-4 shadow-[var(--lift)] anim-in" data-item-card={i.id} data-card-state={look.tone}>
      <button type="button" onClick={() => go("#/items/" + i.id)} className="-m-1.5 flex min-w-0 items-start gap-2.5 rounded-lg p-1.5 text-left hover:bg-accent/70" data-item-ref={i.id}>
        <span className="flex min-w-0 flex-1 flex-col gap-0.5">
          <span className="truncate type-headline">{i.t}</span>
          <span className="truncate type-callout text-muted-foreground"><span className="font-mono type-footnote tabular-nums">{i.id}</span> · {botName(i.b)}{look.tone === "run" && i.now ? " · " + i.now : ""}</span>
        </span>
        <Pill tone={look.tone} className="mt-0.5 h-auto max-w-[45%] py-0.5 leading-4" data-card-status>{look.tone === "done" && c.released ? <RocketIcon className="size-3.5 shrink-0" /> : look.tone === "run" ? <span className="dot-running size-1.5 shrink-0 rounded-full bg-current" aria-hidden /> : null}{look.label}{look.version && <span className="hidden min-w-0 max-w-32 truncate font-mono font-normal sm:inline">{look.version}</span>}</Pill>
      </button>
      {d && !inText && <DeliveryBlock key={d.decision ?? d.text} d={d} open={!!d.full} />}
      {reply && <div className="flex min-w-0 flex-col gap-1 rounded-lg bg-muted/60 px-3 py-2.5" data-card-reply>
        <div className="type-label text-tertiary">{t("thread:card.reply")}</div><div className={cn("line-clamp-4 min-w-0 break-words", FLAT)}><Markdown text={reply.text} className="type-callout" /></div></div>}
      {c.build && <div className="flex items-start gap-2 type-callout text-muted-foreground" data-card-build><CircleAlertIcon className="mt-0.5 size-4 shrink-0 text-attention" strokeWidth={1.75} />
        <span className="min-w-0 flex-1 [overflow-wrap:anywhere]"><Linked text={c.build.text.replace(new RegExp(`^${i.id}\\s+`), "")} /></span></div>}
      {failLine && <div className="flex items-start gap-2 type-callout text-muted-foreground" data-card-fail><CircleAlertIcon className="mt-0.5 size-4 shrink-0 text-destructive" strokeWidth={1.75} /><span className="min-w-0 flex-1">{failLine}</span>
        {c.decision && <AlertClose onClose={() => closeAlerts([c.decision!])} className="-my-0.5" />}</div>}
      {accept ? <AcceptRow i={i} decision={accept.decision} /> : w ? <div className="-mx-1.5 [&_[data-decision-wrap]]:bg-muted/40 [&_[data-decision-wrap]]:shadow-none [&_[data-decision-wrap]]:ring-0">
        <DecisionCard i={i} w={w} showItem={false} /></div> : null}
    </div>
  )
}

/** an item whose card is elsewhere in the conversation (it moved, or this message only mentions the item): one quiet line to the card */
export function CardLink({ id }: { id: string }) {
  const i = st.items.find((x) => x.id === id), at = st.thread.findLast((m) => m.card?.item === id), t = useT()
  if (!i) return null
  return (
    <button type="button" onClick={() => (at?.id ? jumpTo(at.id) : go("#/items/" + id))} className="mt-2 flex max-w-full items-center gap-1.5 self-start truncate type-callout text-muted-foreground hover:text-foreground" data-card-link={id}>
      <ItemNo id={id} /><span className="truncate">{i.t}</span><span className="shrink-0">{t("thread:card.latest")}</span><CornerDownRightIcon className="size-3.5 shrink-0" />
    </button>
  )
}

/* ───────── above the composer ───────── */
/** the quoted message and the item the conversation is talking to */
export function ComposerBars() {
  const q = st.quote ? threadMsg(st.quote) : undefined, f = focusItem()
  const [, tick] = useState(0), t = useT()
  useEffect(() => { if (!st.focus) return; const t = setInterval(() => { tick((x) => x + 1); if (st.focus && st.focus.until <= Date.now()) { st.focus = null; emit() } }, 30_000); return () => clearInterval(t) }, [st.focus?.item])
  if (!q && !f) return null
  const it = f ? st.items.find((x) => x.id === f) : undefined
  return (
    <div className="mb-2 flex flex-col gap-1.5" data-composer-bars>
      {q && <div className="flex min-h-9 min-w-0 items-center gap-2 rounded-lg bg-muted/60 px-3 py-1.5 type-callout text-muted-foreground anim-in" data-quote-bar>
        <MessageSquareReplyIcon className="size-4 shrink-0 text-tertiary" /><span className="min-w-0 flex-1 truncate">{q.item ? t("thread:composer.replyItem", { item: q.item, text: clip(q.t ?? "", 40) }) : t("thread:composer.reply", { text: clip(q.t ?? "", 40) })}</span>
        <button type="button" aria-label={t("thread:composer.unquote")} onClick={() => quoteMsg(null)} className="-mr-1 flex size-6 shrink-0 items-center justify-center rounded-md hover:bg-accent hover:text-foreground"><XIcon className="size-3.5" /></button></div>}
      {f && !q?.item && <div className="flex min-h-9 min-w-0 items-center gap-2 rounded-lg bg-muted/60 px-3 py-1.5 type-callout text-muted-foreground anim-in" data-focus-bar={f}>
        <span className="size-1.5 shrink-0 rounded-full bg-blue" aria-hidden /><span className="min-w-0 flex-1 truncate"><Trans t={t} i18nKey={it ? "thread:composer.talkingNamed" : "thread:composer.talking"} components={{ id: <span className="font-mono tabular-nums text-foreground">{f}</span>, name: <>{it?.t}</> }} /></span>
        <button type="button" aria-label={t("thread:composer.unfocus")} onClick={clearFocus} className="-mr-1 flex size-6 shrink-0 items-center justify-center rounded-md hover:bg-accent hover:text-foreground" data-focus-close><XIcon className="size-3.5" /></button></div>}
    </div>
  )
}
