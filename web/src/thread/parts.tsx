/* Thread routing in the conversation (thread-routing.md §3.4–§5): the offer under an answer, suggestion cards, ask
 * options, the research being read, queued questions, quotes, flow-back from items and the focus bar. Built from the
 * existing parts (ItemRef, DecisionCard, Trace) — no new colours. */
import { useEffect, useState } from "react"
import { cn } from "cn"
import { CornerDownRightIcon, FileTextIcon, LibraryIcon, RotateCcwIcon, SearchIcon, SquareIcon, XIcon, CircleCheckIcon, BanIcon, CircleAlertIcon, MessageSquareReplyIcon, CheckIcon, ChevronDownIcon, RocketIcon } from "lucide-react"
import { Button } from "@/components/ui/button"
import { Spinner } from "@/components/ui/spinner"
import { TraceLine, TraceIcon, TraceTitle, TraceDescription } from "@/components/ui/trace"
import { Markdown } from "@/components/markdown"
import { Textarea } from "@/components/ui/textarea"
import { st, tm, emit, chatSend, takeOffer, chooseSuggest, cancelQueued, stopRun, clearFocus, focusItem, quoteMsg, decisionDraft, decide, botName, alertClosed, closeAlerts, type Msg, type SuggestChoice, type Steps, type Update, type Item, type ItemCardState } from "@/store"
import { ItemRef, DecisionCard, ItemNo, StatusDot, AlertClose, go } from "@/ui"
import { jumpToMsg, threadMsg } from "@/thread/history"

/* ───────── research ───────── */
const isRead = (n: string) => n === "Read"
const isSearch = (n: string) => n === "Grep" || n === "Glob" || n === "LS"
const isKb = (n: string) => n.includes("knowledge")
const secs = (ms: number) => `${Math.max(1, Math.round(ms / 1000))} 秒`
/** "读了 9 个文件 · 搜索 4 次 · 知识库 2 次 · 41 秒" for a finished research run */
export function researchSummary(s: Steps): string {
  const ts = s.tools, r = s.research
  const files = new Set(ts.filter((t) => isRead(t.name)).map((t) => String(t.args?.brief ?? ""))).size
  const parts = [files && `读了 ${files} 个文件`, ts.filter((t) => isSearch(t.name)).length && `搜索 ${ts.filter((t) => isSearch(t.name)).length} 次`,
    ts.filter((t) => isKb(t.name)).length && `知识库 ${ts.filter((t) => isKb(t.name)).length} 次`,
    ts.filter((t) => t.name.includes("steve_item")).length && `查事项 ${ts.filter((t) => t.name.includes("steve_item")).length} 次`].filter(Boolean) as string[]
  if (!parts.length) parts.push("没有读文件")
  const failures = ts.filter((t) => t.status === "failed").length
  if (failures) parts.push(`${failures} 次调用失败`)
  if (r?.ms != null) parts.push(secs(r.ms))
  const stop = r?.stopped ? { user: "你停下了", budget: "没读完", violation: "越界停下", restart: "Hub 重启", failed: "失败" }[r.stopped] : ""
  return [...parts, stop].filter(Boolean).join(" · ")
}
/** the icon of a research tool row */
export function researchIcon(name: string) { return isRead(name) ? FileTextIcon : isSearch(name) ? SearchIcon : isKb(name) ? LibraryIcon : FileTextIcon }
const TOOL_LABEL = (n: string) => (isRead(n) ? "读" : isSearch(n) ? "搜索" : isKb(n) ? "查知识库" : n.includes("item") ? "查事项" : n)
export const researchToolLabel = TOOL_LABEL

/** the research being read right now: one line that keeps changing, with a stop button */
export function ResearchLive() {
  const l = st.live, [, tick] = useState(0)
  useEffect(() => { if (!l) return; const t = setInterval(() => tick((x) => x + 1), 1000); return () => clearInterval(t) }, [l?.run])
  if (!l) return null
  const last = l.tools[l.tools.length - 1], I = last ? researchIcon(last.name) : FileTextIcon
  return (
    <div className="-mb-1 flex max-w-[88%] items-center gap-2 self-start px-1" data-research-live={l.run}>
      <TraceLine className="min-w-0 flex-1"><TraceIcon>{last ? <I strokeWidth={1.5} /> : <Spinner className="size-3.5" />}</TraceIcon>
        <TraceTitle className="shrink-0">{last ? (isRead(last.name) ? "正在读" : "正在" + TOOL_LABEL(last.name)) : "正在读"}</TraceTitle>
        <TraceDescription className="min-w-0 shrink truncate font-mono text-[12px]">{last?.brief ?? "…"}</TraceDescription>
        <TraceDescription className="shrink-0 tabular-nums">· 第 {l.tools.length} 个工具 · {secs(Date.now() - l.t0)}{Date.now() - l.t0 > 90_000 ? " · 还在读…" : ""}</TraceDescription></TraceLine>
      <Button size="xs" variant="outline" className="shrink-0" onClick={() => stopRun(l.run)} data-research-stop><SquareIcon className="fill-current" />停</Button>
    </div>
  )
}

/* ───────── offer · suggestion card · ask ───────── */
/** "要我改吗？ [做成事项：…] [不用]" under an answer */
export function OfferLine({ m }: { m: Msg }) {
  const o = m.offer, [busy, setBusy] = useState(false), [open, setOpen] = useState(false)
  if (!o) return null
  if (o.state === "taken") return o.item ? (st.thread.some((x) => x.card?.item === o.item) ? <CardLink id={o.item} /> : <ItemRef id={o.item} />) : null
  if (o.state === "dismissed") return null
  const act = async (take: boolean) => { setBusy(true); try { await takeOffer(m, take) } finally { setBusy(false) } }
  if (o.folded && !open) return <button type="button" className="mt-2 self-start type-footnote text-muted-foreground hover:text-foreground" onClick={() => setOpen(true)} data-offer-folded={m.id}>之前提议过：{o.title}</button>
  return (
    <div className="mt-3 flex flex-wrap items-center gap-2" data-offer={m.id}>
      <span className="type-callout text-muted-foreground">要我改吗？</span>
      <Button size="sm" variant="outline" className="max-w-full rounded-full" disabled={busy} onClick={() => void act(true)} data-offer-take><span className="truncate">做成事项：{o.title}</span></Button>
      <Button size="sm" variant="ghost" className="text-muted-foreground" disabled={busy} onClick={() => void act(false)} data-offer-dismiss>不用</Button>
    </div>
  )
}

/** a suggestion card: answer here or make it an item; greyed once expired, still clickable */
export function SuggestCard({ m }: { m: Msg }) {
  const s = m.suggest, [busy, setBusy] = useState(false)
  if (!s) return null
  const chosen = s.state === "chosen"
  const act = async (c: SuggestChoice) => { setBusy(true); try { await chooseSuggest(m, c) } finally { setBusy(false) } }
  return (
    <div className={cn("mt-3 flex flex-col gap-2", s.state === "expired" && "opacity-60")} data-suggest={m.id} data-suggest-state={s.state}>
      {chosen ? <div className="type-footnote text-muted-foreground">{s.chosen === "here" ? "你选了在这里回答" : s.item ? "" : "你选了建成事项"}</div>
        : <div className="flex flex-wrap gap-2">{s.choices.map((c) => (
          <Button key={c.id} size="sm" variant={(s.choices.some((x) => x.recommended) ? c.recommended : c.id === "here") ? "default" : "outline"} className="max-w-full rounded-full" disabled={busy} onClick={() => void act(c.id)} data-suggest-choice={c.id}><span className="truncate">{c.label}</span></Button>))}
          {s.state === "expired" && <span className="self-center type-footnote text-muted-foreground">过了 24 小时，仍可点</span>}</div>}
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
  if (!m.ask?.options?.length) return null
  if (conv) return <div className="mt-2 type-footnote text-muted-foreground" data-ask={m.id} data-ask-readonly>可以回复：{m.ask.options.join(" / ")}</div>
  const at = st.thread.findIndex((x) => x === m || (!!m.id && x.id === m.id))
  if (at >= 0 && st.thread.slice(at + 1).some((x) => x.k === "me")) return null
  const key = m.id ?? ""
  const send = async (o: string) => {
    if (answering.has(key)) return
    answering.add(key)
    try { await chatSend(o) } finally { answering.delete(key) }
  }
  return <div className="mt-3 flex flex-wrap gap-2" data-ask={m.id}>{m.ask.options.map((o) => <Button key={o} size="sm" variant="outline" className="rounded-full" onClick={() => void send(o)} data-ask-opt={o}>{o}</Button>)}</div>
}

/** "（Hub 重启，没答完）": ask the question again (your own conversation only: an IM turn is asked in its chat) */
export function Rerun({ m, conv }: { m: Msg; conv?: boolean }) {
  if (!m.rerun || conv) return null
  const q = st.thread.find((x) => x.id === m.rerun)
  return q?.t ? <div className="mt-3"><Button size="sm" variant="outline" className="rounded-full" onClick={() => void chatSend(q.t!)} data-rerun><RotateCcwIcon />重新回答</Button></div> : null
}

/* ───────── queue · quote ───────── */
/** under a user message waiting behind a research run */
export function QueuedNote({ m }: { m: Msg }) {
  if (!m.queued) return null
  const run = m.queued.run
  return <div className="flex items-center gap-2 self-end type-footnote text-muted-foreground" data-queued={m.id}><Spinner className="size-3" />排队中{run && <button type="button" className="hover:text-foreground hover:underline" onClick={() => cancelQueued(run)} data-queued-cancel>取消</button>}</div>
}

/** scroll to a message and flash it; the conversation offers the way back (ST-50) */
export function jumpTo(id: string) { jumpToMsg(id, threadMsg(id)?.created) }
const clip = (t: string, n = 24) => { const x = t.replace(/\s+/g, " ").trim(); return x.length > n ? x.slice(0, n) + "…" : x }
/** "↳ 你 18:09 说的「…」": the message this one answers or quotes */
export function QuoteHead({ id, className }: { id?: string; className?: string }) {
  if (!id) return null
  const q = threadMsg(id)
  if (!q) return null
  return (
    <button type="button" onClick={() => jumpTo(id)} className={cn("flex max-w-full items-center gap-1 self-start truncate type-footnote text-muted-foreground hover:text-foreground", className)} data-quote={id}>
      <CornerDownRightIcon className="size-3.5 shrink-0" /><span className="truncate">{q.k === "me" ? "你" : "Steve"} {tm(q.created)} 说的「{clip(q.t ?? "")}」</span>
    </button>
  )
}

/* ───────── flow-back from an item (§5.2) ───────── */
function UpdateRow({ u }: { u: Update }) {
  const i = st.items.find((x) => x.id === u.item), [full, setFull] = useState(false)
  const w = u.decision ? i?.waits.find((x) => x.decision === u.decision) : undefined
  if (u.kind === "done" || u.kind === "drop") {
    const I = u.kind === "done" ? CircleCheckIcon : BanIcon
    return <div className="flex items-center gap-2 type-callout" data-update={u.kind}><I className={cn("size-4 shrink-0", u.kind === "done" ? "text-success" : "text-muted-foreground")} strokeWidth={1.75} /><ItemNo id={u.item} /><span className="min-w-0 truncate">{u.text || (u.kind === "done" ? "完成：" : "放弃：") + (i?.t ?? "")}</span></div>
  }
  if ((u.kind === "ask" || u.kind === "fail") && w && i) return <div className="flex flex-col gap-2" data-update={u.kind}>{u.text && u.kind === "fail" && <div className="flex items-center gap-2 type-callout"><CircleAlertIcon className="size-4 text-destructive" strokeWidth={1.75} />{u.text}</div>}<DecisionCard i={i} w={w} showItem={false} /></div>
  if (u.kind === "deliver") {
    const all = full ? decisionDraft(u.decision ?? "") : undefined
    return (
      <div className="flex flex-col gap-2" data-update="deliver">
        <div className="type-footnote text-muted-foreground"><ItemNo id={u.item} /> 交付了{w ? "，等你验收" : ""}</div>
        <Markdown text={all ?? u.text ?? ""} className="type-chat" />
        {!u.full && u.decision && <button type="button" className="self-start type-footnote text-muted-foreground hover:text-foreground" onClick={() => setFull(!full)} data-update-expand>{full ? "收起" : "展开全文"}</button>}
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
  return <div className="mt-3 flex flex-col gap-3 border-l-2 border-border pl-3" data-updates={m.id}>{us.map((u, n) => <UpdateRow key={n} u={u} />)}</div>
}

/* ───────── an item's one living card (§5.2) ───────── */
type Look = { label: string; dot: string; tone: "run" | "wait" | "fail" | "done" | "quiet"; version?: string }
/** what the card says the item is doing now: live from the item, with the card for what the item no longer shows (released) */
export function cardLook(i: Item, c?: ItemCardState): Look {
  const w = i.waits[0]
  if (i.st === "done") return c?.released ? { label: "已发布", version: c.released.version, dot: "done", tone: "done" } : { label: "已完成", dot: "done", tone: "done" }
  if (i.st === "drop") return { label: "已放弃", dot: "drop", tone: "quiet" }
  if (w?.type === "fail") return { label: "出错了，等你处理", dot: "fail", tone: "fail" }
  if (w?.type === "accept") return { label: "交付了，等你验收", dot: "wait", tone: "wait" }
  if (w?.subject === "release") return { label: "等你批准发布", dot: "wait", tone: "wait" }
  if (w?.subject === "tool_call") return { label: "等你允许", dot: "wait", tone: "wait" }
  if (w?.type === "approve") return { label: "等你批准", dot: "wait", tone: "wait" }
  if (w?.type === "ask") return { label: "等你回答", dot: "wait", tone: "wait" }
  if (w) return { label: "等你", dot: "wait", tone: "wait" }
  if (i.st === "block") return { label: "等前置事项", dot: "block", tone: "quiet" }
  if (i.st === "pause") return { label: "已暂停", dot: "pause", tone: "quiet" }
  if (i.rel?.tone === "run") return { label: "正在发布", dot: "run", tone: "run" }
  return { label: "进行中", dot: "run", tone: "run" }
}
const TONE: Record<Look["tone"], string> = { run: "text-blue", wait: "text-attention", fail: "text-destructive", done: "text-success", quiet: "text-muted-foreground" }

/** the delivery summary, two lines until opened; the full text is the acceptance draft when there is one */
function DeliveryBlock({ d, open: open0 = false }: { d: NonNullable<ItemCardState["delivery"]>; open?: boolean }) {
  const [open, setOpen] = useState(open0)
  const all = open && d.decision ? decisionDraft(d.decision) : undefined
  return (
    <div className="flex min-w-0 flex-col gap-1" data-card-delivery>
      <div className="type-footnote text-muted-foreground">交付摘要</div>
      <div className={cn("min-w-0 break-words", !open && "line-clamp-2")}><Markdown text={all ?? d.text} className="type-callout" /></div>
      <button type="button" className="flex items-center gap-0.5 self-start type-footnote text-muted-foreground hover:text-foreground" onClick={() => setOpen(!open)} data-card-expand>
        {open ? "收起" : "展开"}<ChevronDownIcon className={cn("size-3.5 transition-transform", open && "rotate-180")} /></button>
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
  const [rework, setRework] = useState(false), [txt, setTxt] = useState(""), [busy, setBusy] = useState(false)
  const act = async (c: string, t?: string) => { setBusy(true); try { await decide(i.id, c, t, decision) } finally { setBusy(false) } }
  return (
    <div className="flex flex-col gap-2" data-card-accept={decision}>
      {rework && <Textarea autoFocus className="min-h-16 rounded-lg" placeholder={`哪里不对？写给${botName(i.b)}`} value={txt} onChange={(e) => setTxt(e.target.value)} data-rework />}
      <div className="flex flex-wrap justify-end gap-2">{rework
        ? <><Button size="sm" variant="ghost" onClick={() => setRework(false)}>取消</Button><Button size="sm" disabled={busy} onClick={() => void act("rework", txt.trim())}>发回返工</Button></>
        : <><Button size="sm" variant="outline" onClick={() => setRework(true)}>返工…</Button><Button size="sm" disabled={busy} onClick={() => void act("pass")} data-pass><CheckIcon />验收通过</Button></>}</div>
    </div>
  )
}

/** One card per item made from the conversation: what it is doing now, its latest delivery (folded), the reply to what you
 * last sent it, and whatever it needs from you — answerable right here. Lifecycle events update it; they are not messages. */
export function ItemCard({ c, inText = false }: { c: ItemCardState; inText?: boolean }) {
  const i = st.items.find((x) => x.id === c.item)
  if (!i) return null
  const look = cardLook(i, c), w = i.waits[0]
  const accept = w?.type === "accept" ? w : undefined
  /* the delivery the open acceptance is about, else the latest one */
  const d = c.delivery ? { ...c.delivery, decision: accept?.decision ?? c.delivery.decision } : accept ? { text: (accept.draft ?? "").split("\n\n")[0], decision: accept.decision } : undefined
  const reply = c.reply && (!c.delivery?.at || (c.reply.at ?? 0) > c.delivery.at) ? c.reply : undefined
  /* the failure is current only while the item stays stopped on it (you chose to look, it is paused): going on, being
   * resumed or finishing ends it — the Hub then sets the card back to running, cards from before that are read the same way */
  const failLine = !w && c.state === "fail" && i.st === "pause" && !alertClosed(c.decision) ? c.text : undefined
  return (
    <div className="mt-3 flex w-full max-w-[560px] flex-col gap-3 rounded-xl border bg-card p-3.5 anim-in" data-item-card={i.id} data-card-state={look.tone}>
      <button type="button" onClick={() => go("#/items/" + i.id)} className="-m-1 flex min-w-0 items-start gap-2.5 rounded-lg p-1 text-left hover:bg-accent/60" data-item-ref={i.id}>
        <span className="mt-[7px]"><StatusDot s={look.dot} /></span>
        <span className="flex min-w-0 flex-1 flex-col gap-0.5">
          <span className="truncate type-body font-medium">{i.t}</span>
          <span className="truncate type-footnote text-muted-foreground"><span className="font-mono tabular-nums">{i.id}</span> · {botName(i.b)}{look.tone === "run" && i.now ? " · " + i.now : ""}</span>
        </span>
        <span className={cn("shrink-0 whitespace-nowrap pt-0.5 type-footnote font-medium", TONE[look.tone])} data-card-status>{look.tone === "done" && c.released ? <RocketIcon className="mr-1 inline size-3.5 align-[-2px]" /> : null}{look.label}{look.version && <span className="ml-1 hidden max-w-32 truncate align-bottom font-normal sm:inline-block">{look.version}</span>}</span>
      </button>
      {d && !inText && <DeliveryBlock key={d.decision ?? d.text} d={d} open={!!d.full} />}
      {reply && <div className="flex min-w-0 flex-col gap-1 border-l-2 border-border pl-3" data-card-reply>
        <div className="type-footnote text-muted-foreground">它的回复</div><div className="line-clamp-4 min-w-0 break-words"><Markdown text={reply.text} className="type-callout" /></div></div>}
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
  const i = st.items.find((x) => x.id === id), at = st.thread.findLast((m) => m.card?.item === id)
  if (!i) return null
  return (
    <button type="button" onClick={() => (at?.id ? jumpTo(at.id) : go("#/items/" + id))} className="mt-2 flex max-w-full items-center gap-1.5 self-start truncate type-footnote text-muted-foreground hover:text-foreground" data-card-link={id}>
      <ItemNo id={id} /><span className="truncate">{i.t}</span><span className="shrink-0">· 看最新进展</span><CornerDownRightIcon className="size-3.5 shrink-0" />
    </button>
  )
}

/* ───────── above the composer ───────── */
/** the quoted message and the item the conversation is talking to */
export function ComposerBars() {
  const q = st.quote ? threadMsg(st.quote) : undefined, f = focusItem()
  const [, tick] = useState(0)
  useEffect(() => { if (!st.focus) return; const t = setInterval(() => { tick((x) => x + 1); if (st.focus && st.focus.until <= Date.now()) { st.focus = null; emit() } }, 30_000); return () => clearInterval(t) }, [st.focus?.item])
  if (!q && !f) return null
  const it = f ? st.items.find((x) => x.id === f) : undefined
  return (
    <div className="mb-2 flex flex-col gap-1.5" data-composer-bars>
      {q && <div className="flex min-w-0 items-center gap-2 rounded-xl bg-muted/60 px-3 py-1.5 type-footnote text-muted-foreground" data-quote-bar>
        <MessageSquareReplyIcon className="size-3.5 shrink-0" /><span className="min-w-0 flex-1 truncate">回复{q.item ? ` ${q.item}` : ""}：「{clip(q.t ?? "", 40)}」</span>
        <button type="button" aria-label="取消引用" onClick={() => quoteMsg(null)} className="shrink-0 hover:text-foreground"><XIcon className="size-3.5" /></button></div>}
      {f && !q?.item && <div className="flex min-w-0 items-center gap-2 rounded-xl bg-muted/60 px-3 py-1.5 type-footnote text-muted-foreground" data-focus-bar={f}>
        <span className="size-1.5 shrink-0 rounded-full bg-blue" aria-hidden /><span className="min-w-0 flex-1 truncate">正在跟 <span className="font-mono tabular-nums text-foreground">{f}</span>{it ? ` ${it.t}` : ""} 说</span>
        <button type="button" aria-label="不再跟这个事项说" onClick={clearFocus} className="shrink-0 hover:text-foreground" data-focus-close><XIcon className="size-3.5" /></button></div>}
    </div>
  )
}
