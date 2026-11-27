import { getItems, getItem } from "@/store"
/* Thread routing in the conversation (thread-routing.md §3.4–§5): the offer under an answer, suggestion cards, ask
 * options, the research being read, queued questions, quotes, flow-back from items and the focus bar. Built from the
 * existing parts (ItemRef, DecisionCard, Trace) — no new colours. */
import { lazy, Suspense, useEffect, useId, useLayoutEffect, useRef, useState, type ComponentProps } from "react"
import { itemDisplayState } from "@/store"
import { cn } from "cn"
import { CornerDownRightIcon, FileTextIcon, LibraryIcon, RotateCcwIcon, SearchIcon, XIcon, CircleCheckIcon, BanIcon, CircleAlertIcon, MessageSquareReplyIcon, CheckIcon, ChevronDownIcon, RocketIcon, TerminalIcon, PencilIcon, GlobeIcon } from "lucide-react"
import type { ThreadPlanEntry, ThreadSubtask } from "@/gen"
import { SubtaskList, type SubtaskEntry } from "@/components/subtask-list"
import { Button } from "@/components/ui/button"
import { Spinner } from "@/components/ui/spinner"
import { type StepState, type ToolStep, type TrailItem } from "@/components/work-trail"
import { recoveredCalls } from "@/thread/chat-progress"
import { SteveModelControl } from "@/steve-model"
import { api } from "@/api"
import { newClientId } from "@/composer-draft"
import { chatBusy, refresh } from "@/store"
import { Markdown } from "@/components/markdown"
import { Textarea } from "@/components/ui/textarea"
import { clip as clipPlain } from "@/thread/topics"
import { GITHUB_PR_MERGE, STEVE_TOOL, st, tm, emit, chatSend, takeOffer, cancelQueued, clearFocus, focusItem, quoteMsg, decisionDraft, decide, botName, alertClosed, closeAlerts, useShellStore, type Msg, type Steps, type Update, type Item, type ItemCardState, type LiveRun } from "@/store"
import { Select, type SelectOption } from "@/components/affect/select-clean"
import { ItemRef, ItemNo, AlertClose, go, Pill } from "@/ui"
import { jumpToMsg, threadMessages, threadMsg, useThreadIds, useThreadMessage } from "@/thread/history"
import { Trans } from "react-i18next"
import { t as tr, useT } from "@/i18n"
import { toolClass, toolCounts, toolShort, meetStep, type ToolClass } from "@/thread/tools"
import { steveToolIcon } from "@/tool-icons"
import { DeliveryCards } from "@/delivery-card"
import { useItem, useItemDetail } from "@/store"
const LazyDecision = lazy(() => import("@/thread/decision-card").then(({ DecisionCard }) => ({ default: DecisionCard })))
function DecisionCard(props: ComponentProps<typeof LazyDecision>) {
  return <Suspense fallback={null}><LazyDecision {...props} /></Suspense>
}

/* ───────── research ───────── */
const secs = (ms: number) => tr("common:dur.seconds", { count: Math.max(1, Math.round(ms / 1000)) })
/** "读了 9 个文件 · 搜索 4 次 · 执行 3 条命令 · 41 秒" for a finished research run */
export function researchSummary(s: Steps, includeTime = true): string {
  const ts = s.tools, r = s.research, n = toolCounts(ts)
  /* Steve's own writes of the first meeting are not "calls": they are what the answer says it saved */
  const parts = [n.files && tr("thread:research.files", { count: n.files }), n.search && tr("thread:research.searches", { count: n.search }),
    n.exec && tr("thread:research.commands", { count: n.exec }), n.edit && tr("thread:research.edits", { count: n.edit }), n.fetch && tr("thread:research.fetches", { count: n.fetch }),
    n.kb && tr("thread:research.kb", { count: n.kb }), n.item && tr("thread:research.items", { count: n.item }), n.meet && tr("thread:research.saves", { count: n.meet }),
    n.other && tr("thread:research.calls", { count: n.other })].filter(Boolean) as string[]
  if (!parts.length) parts.push(tr("thread:research.noFiles"))
  const failures = ts.filter((t) => t.status === "failed").length
  if (failures) parts.push(tr("thread:research.failures", { count: failures }))
  if (includeTime && r?.ms != null) parts.push(secs(r.ms))
  const stop = r?.stopped ? tr(`thread:research.stopped.${r.stopped}`) : ""
  return [...parts, stop].filter(Boolean).join(" · ")
}
const ICON: Partial<Record<ToolClass, typeof FileTextIcon>> = { read: FileTextIcon, search: SearchIcon, kb: LibraryIcon, exec: TerminalIcon, edit: PencilIcon, fetch: GlobeIcon, meet: CheckIcon }
/** the icon of a research tool row */
export function researchIcon(name: string, kind?: string) { return steveToolIcon(name) ?? ICON[toolClass(name, kind)] ?? FileTextIcon }
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
  return { t: "tool", key, state, icon: researchIcon(name, kind), verb: [liveLabel(name, kind), label], subj: brief, mono: true, group: g ?? (c === "meet" ? undefined : "tool:" + name), merged,
    chip: brief ? { label: brief, mono: true, icon: researchIcon(name, kind) } : undefined, attrs: { kind: c }, ...more }
}

export function PinnedPlan({ entries }: { entries: ThreadPlanEntry[] }) {
  const t = useT()
  if (!entries.length) return null
  const done = entries.filter((e) => e.status === "completed").length
  return (
    <div className="flex flex-col gap-1" data-pinned-plan>
      <div className="flex items-center justify-between gap-2 type-footnote text-muted-foreground">
        <span className="font-medium text-foreground/85">{t("thread:progress.planTitle")}</span>
        <span className="tabular-nums">{t("thread:progress.planCount", { done, total: entries.length })}</span>
      </div>
      <ul className="flex flex-col gap-1">
        {entries.map((entry, idx) => (
          <li key={idx} className="flex items-start gap-2 type-footnote" data-plan-status={entry.status}>
            <span className="mt-0.5 flex size-3.5 shrink-0 items-center justify-center" aria-hidden>
              {entry.status === "completed" ? <CheckIcon className="size-3 text-success" strokeWidth={2.25} />
                : entry.status === "in_progress" ? <Spinner className="size-3 text-blue" />
                : entry.status === "failed" ? <XIcon className="size-3 text-destructive" strokeWidth={2.25} />
                : <span className="size-1.5 rounded-full bg-muted-foreground/45" />}
            </span>
            <span className={cn("min-w-0 wrap-anywhere", entry.status === "completed" ? "text-muted-foreground line-through decoration-muted-foreground/50" : entry.status === "in_progress" ? "font-medium text-foreground" : "text-foreground/85")}>
              {entry.content}
            </span>
          </li>
        ))}
      </ul>
    </div>
  )
}

/** Renders main-thread research subtasks via the shared affectUI `SubtaskList` (`ParallelAttempts`). */
export function ResearchSubtasks({ subtasks, live }: { subtasks: ThreadSubtask[]; live: boolean }) {
  if (!subtasks.length) return null
  const entries: SubtaskEntry[] = subtasks.map((s) => ({
    subtask: {
      id: s.id,
      origin: s.origin,
      status: s.status,
      title: s.title,
      actor: s.actor ?? null,
      progress: s.progress ?? null,
      summary: s.summary ?? null,
      error: s.error ?? null,
      can_cancel: false,
      duration_ms: s.duration_ms ?? null,
    },
    started: s.started_at,
    children: [],
  }))
  return (
    <div data-live-subtasks>
      <SubtaskList sessionId="" entries={entries} live={live} readOnly />
    </div>
  )
}

export function liveCountsSummary(l: LiveRun, t: ReturnType<typeof useT>): string | undefined {
  const activeCount = (l.active ?? (l.now ? [l.now] : [])).length
  const doneCount = l.tools.filter((x) => x.ok).length
  const failedCount = l.tools.filter((x) => !x.ok).length
  const planEntries = l.plan ?? []
  const subtasks = l.subtasks ?? []
  const activeSubtasks = subtasks.filter((s) => s.status === "running" || s.status === "queued").length
  const parts = [
    activeCount > 0 ? t("thread:progress.summary.running", { count: activeCount }) : "",
    doneCount > 0 ? t("thread:progress.summary.done", { count: doneCount }) : "",
    failedCount > 0 ? t("thread:progress.summary.failed", { count: failedCount }) : "",
    planEntries.length > 0 ? t("thread:progress.summary.plan", { done: planEntries.filter((p) => p.status === "completed").length, total: planEntries.length }) : "",
    activeSubtasks > 0 ? t("thread:progress.summary.subtasks", { count: activeSubtasks }) : "",
  ].filter(Boolean)
  return parts.length > 0 ? parts.join(t("thread:progress.summary.sep")) : undefined
}

/** The event stream determines every step; an empty list is still a running panel. */
export function researchLiveItems(l: LiveRun): TrailItem[] {
  const recovered = recoveredCalls(l.tools, x => x.ok)
  // Interleave thoughts and completed tools chronologically when timestamps are available.
  const chronological: { at?: number; order: number; item: TrailItem }[] = []
  let seq = 0
  for (const th of l.thoughts ?? []) {
    chronological.push({
      at: th.startedAt,
      order: seq++,
      item: {
        t: "think",
        key: "th:" + th.id,
        text: th.text,
        live: !th.done,
        startedAt: th.startedAt,
        at: th.at,
        ms: th.done && th.at != null ? Math.max(0, th.at - th.startedAt) : undefined,
        attrs: { "step-thought": th.id },
      },
    })
  }
  l.tools.forEach((x, n) => {
    chronological.push({
      at: x.startedAt ?? x.at,
      order: seq++,
      item: researchStep(String(n), x.name, x.kind, x.description || x.brief || "", x.ok ? "done" : "fail", {
        card: {
          name: researchToolLabel(x.name, x.kind),
          args: x.brief || undefined,
          result: x.excerpt,
          error: x.error,
          ms: x.ms,
          startedAt: x.startedAt,
        },
        attrs: { kind: toolClass(x.name, x.kind), recovered: recovered.has(n) ? "" : undefined },
      }),
    })
  })
  if (chronological.some((entry) => entry.at != null)) {
    chronological.sort((a, b) => {
      if (a.at != null && b.at != null && a.at !== b.at) return a.at - b.at
      return a.order - b.order
    })
  }
  const items: TrailItem[] = chronological.map((entry) => entry.item)
  const active = l.active ?? (l.now ? [l.now] : [])
  active.forEach((call, idx) => {
    items.push(researchStep("now:" + (call.call ?? idx), call.name, call.kind, call.description || call.brief || "", "run", {
      card: {
        name: researchToolLabel(call.name, call.kind),
        args: call.brief || undefined,
        startedAt: call.startedAt,
      },
    }))
  })
  const subtasks = l.subtasks ?? []
  if (subtasks.length > 0) {
    const runningSubtask = subtasks.some((s) => s.status === "running" || s.status === "queued" || s.status === "needs_you")
    items.push({
      t: "block",
      key: "subtasks",
      state: runningSubtask ? "run" : "done",
      body: <ResearchSubtasks subtasks={subtasks} live />,
      attrs: { "live-subtasks": "" },
    })
  }
  return items
}

/* ───────── offer · ask ───────── */
const AUTO_OFFER_BOT = "__auto__"
/** Minimum popover menu width in px so Bot descriptions and placement reasons remain readable. */
const OFFER_BOT_MENU_MIN_WIDTH = 220

/** "要我改吗？ [执行 Bot] [做成事项：…] [不用]" under an answer */
export function OfferLine({ m }: { m: Msg }) {
  const o = m.offer
  if (!o || o.state === "dismissed") return null
  if (o.state === "taken") return o.item ? (threadMessages().some((x) => x.card?.item === o.item) ? <CardLink id={o.item} from={m.id} /> : <ItemRef id={o.item} />) : null
  return <OpenOfferLine m={m} o={o} />
}

function OpenOfferLine({ m, o }: { m: Msg; o: NonNullable<Msg["offer"]> }) {
  useShellStore()
  const [busy, setBusy] = useState(false), [open, setOpen] = useState(false), [picked, setPicked] = useState(AUTO_OFFER_BOT), t = useT()
  const rejectedReason = o.rejected ? t(`manage:dispatch.reason.${o.rejected}`, { defaultValue: t("manage:dispatch.reason.bind_failed") }) : undefined
  const previewBot = !o.rejected && o.bot && st.bots[o.bot]?.on ? o.bot : undefined
  const enabledBots = Object.keys(st.bots)
    .filter((k) => st.bots[k].on)
    .sort((a, b) => Number(!!st.bots[a].main) - Number(!!st.bots[b].main) || botName(a).localeCompare(botName(b)))
  const effectivePicked = picked === AUTO_OFFER_BOT || enabledBots.includes(picked) ? picked : AUTO_OFFER_BOT
  const autoLabel = o.rejected
    ? o.bot
      ? t("thread:offer.botRejected", { bot: botName(o.bot) })
      : t("thread:offer.botAutoUnassigned")
    : previewBot
      ? o.reason === "only_eligible"
        ? t("thread:offer.botOnlyEligible", { bot: botName(previewBot) })
        : t("thread:offer.botSteveSelected", { bot: botName(previewBot) })
      : t("thread:offer.botAuto")
  const autoDesc = o.rejected
    ? [rejectedReason, t("thread:offer.botAutoUnassignedHint")].filter(Boolean).join(" · ")
    : previewBot
      ? [o.reason ? t(`manage:dispatch.selection.${o.reason}`) : undefined, o.why || undefined].filter(Boolean).join(" · ")
      : t("thread:offer.botAutoHint")
  const options: SelectOption[] = [
    { value: AUTO_OFFER_BOT, label: autoLabel, description: autoDesc || undefined },
    ...enabledBots.map((k) => ({
      value: k,
      label: st.bots[k].main ? `${botName(k)} · ${t("manage:dispatch.coordinator")}` : botName(k),
      description: st.bots[k].role || undefined,
    })),
  ]
  const usingAuto = effectivePicked === AUTO_OFFER_BOT
  const act = async (take: boolean) => {
    setBusy(true)
    try {
      await takeOffer(m, take, take && !usingAuto ? effectivePicked : undefined)
    } finally {
      setBusy(false)
    }
  }
  if (o.folded && !open) return <button type="button" className="mt-2 self-start type-callout text-muted-foreground hover:text-foreground" onClick={() => setOpen(true)} data-offer-folded={m.id}>{t("thread:offer.folded", { title: o.title })}</button>
  return (
    <div className="mt-3 flex flex-col gap-1.5 anim-in" data-offer={m.id}>
      <div className="flex flex-wrap items-center gap-2">
        <span className="type-callout text-muted-foreground">{t("thread:offer.question")}</span>
        {(enabledBots.length > 0 || previewBot || o.rejected) && (
          <Select size="sm" value={effectivePicked} options={options} onChange={setPicked} disabled={busy}
            aria-label={t("thread:offer.chooseBot")} align="start" listMinWidth={OFFER_BOT_MENU_MIN_WIDTH}
            className="min-w-0 max-w-full" triggerClassName="max-w-full" data-offer-bot={m.id} />
        )}
        <Button size="sm" variant="outline" className="max-w-full rounded-full" disabled={busy || (Boolean(o.rejected && o.bot) && usingAuto)} onClick={() => void act(true)} data-offer-take><span className="truncate">{t("thread:offer.take", { title: o.title })}</span></Button>
        <Button size="sm" variant="ghost" className="text-muted-foreground" disabled={busy} onClick={() => void act(false)} data-offer-dismiss>{t("thread:offer.dismiss")}</Button>
      </div>
      {usingAuto && o.rejected && rejectedReason && (
        <p className="type-footnote text-attention" data-offer-rejected={o.rejected}>
          {o.bot
            ? t("thread:offer.rejectedNotice", { bot: botName(o.bot), reason: rejectedReason })
            : t("thread:offer.unassignedNotice", { reason: rejectedReason })}
        </p>
      )}
      {usingAuto && !o.rejected && o.why && (
        <p className="type-footnote text-muted-foreground" data-offer-why={m.id}>{o.why}</p>
      )}
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
  useThreadIds(!conv && !!m.ask?.options.length)
  if (!m.ask?.options?.length) return null
  if (conv) return <div className="mt-2 type-callout text-muted-foreground" data-ask={m.id} data-ask-readonly>{t("thread:ask.canReply", { options: m.ask.options.join(" / ") })}</div>
  const messages = threadMessages()
  const at = messages.findIndex((x) => x === m || (!!m.id && x.id === m.id))
  if ((at >= 0 ? messages.slice(at + 1) : messages.filter(x => (x.created ?? 0) > (m.created ?? 0))).some(x => x.k === "me")) return null
  const key = m.id ?? ""
  const send = async (o: string) => {
    if (answering.has(key)) return
    answering.add(key)
    try { await chatSend(o) } finally { answering.delete(key) }
  }
  return <div className="mt-3 rounded-lg border bg-muted/35 px-3 py-2 anim-in" data-ask={m.id} data-chat-awaiting>
    <p className="flex items-center gap-2 type-callout font-medium text-attention"><CircleAlertIcon className="size-4 shrink-0" aria-hidden />{t("thread:progress.awaiting")}</p>
    <div className="mt-1 flex flex-wrap gap-2">{m.ask.options.map((o) => <Button key={o} size="sm" variant="outline" className="rounded-full" onClick={() => void send(o)} data-ask-opt={o}>{o}</Button>)}</div></div>
}

/** "（Hub 重启，没答完）": ask the question again (your own conversation only: an IM turn is asked in its chat) */
export function Rerun({ m, conv }: { m: Msg; conv?: boolean }) {
  const t = useT(), clientId = useRef(newClientId()), inFlight = useRef(false)
  const [busy, setBusy] = useState(false), [error, setError] = useState("")
  const failed = m.steps?.answer?.failed || m.steps?.research?.stopped === "failed"
  if (conv || !m.rerun && !failed) return null
  const q = threadMsg(m.rerun ?? m.origin ?? m.replyTo ?? "")
  if (q?.k !== "me" || !q.t || m.item) return null
  const retry = async () => {
    if (inFlight.current || chatBusy()) throw new Error(t("pages:models.setup.busy"))
    inFlight.current = true; setBusy(true); setError("")
    try {
      // Reuse one idempotency key after an ambiguous network failure. Address the original question
      // explicitly: neither the composer's current quote/focus nor a draft may redirect this retry.
      await api.say(q.t!, (q.files ?? []).map((f) => f.id), [], { reply_to: q.id, client_id: clientId.current })
      await refresh().catch(() => {})
    } catch (e) { const msg = e instanceof Error ? e.message : String(e); setError(msg); throw e }
    finally { inFlight.current = false; setBusy(false) }
  }
  return <div className="mt-3 flex flex-col items-start gap-2" data-model-recovery>
    <div className="flex flex-wrap items-center gap-2"><Button type="button" size="sm" variant="outline" className="rounded-full" disabled={busy || chatBusy()} onClick={() => void retry().catch(() => {})} data-rerun><RotateCcwIcon />{t("thread:rerun")}</Button><SteveModelControl retry={retry} /></div>
    {error && <p role="alert" className="type-footnote text-destructive">{error}</p>}
  </div>
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
  const q = useThreadMessage(id)
  if (!q) return null
  return (
    <button type="button" onClick={() => jumpTo(q.id!)} className={cn("flex max-w-full items-center gap-1 self-start truncate type-footnote text-muted-foreground hover:text-foreground", className)} data-quote={id}>
      <CornerDownRightIcon className="size-3.5 shrink-0" /><span className="truncate">{t(q.k === "me" ? "thread:quote.me" : "thread:quote.steve", { time: tm(q.created), text: clip(q.t ?? "") })}</span>
    </button>
  )
}

/* ───────── flow-back from an item (§5.2) ───────── */
function UpdateRow({ u }: { u: Update }) {
  const i = getItem(u.item), [full, setFull] = useState(false), t = useT()
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

/** the decisions a thread already shows in place: each item card's open one and the questions in its updates
 *  (the card above the composer leaves these out, so one question is never asked twice on the page) */
export function threadDecisions(list: Msg[]): Set<string> {
  const out = new Set<string>()
  for (const m of list) {
    if (m.card) { const w = getItems().find((x) => x.id === m.card!.item)?.waits[0]; if (w) out.add(w.decision); continue }
    for (const u of [...(m.updates ?? []), ...(m.update ? [m.update] : [])]) {
      if (u.decision && (u.kind === "ask" || u.kind === "fail" || u.kind === "deliver") && getItem(u.item)?.waits.some((w) => w.decision === u.decision)) out.add(u.decision)
    }
  }
  return out
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
  if (w?.subject === GITHUB_PR_MERGE) return { label: tr("thread:card.status.prMerge"), dot: "wait", tone: "wait" }
  if (w?.subject === "tool_call") return { label: tr("thread:card.status.toolCall"), dot: "wait", tone: "wait" }
  if (w?.type === "approve") return { label: tr("thread:card.status.approve"), dot: "wait", tone: "wait" }
  if (w?.type === "ask") return { label: tr("thread:card.status.answer"), dot: "wait", tone: "wait" }
  if (w) return { label: tr("thread:card.status.waiting"), dot: "wait", tone: "wait" }
  /* the item page's one state: a paused or idle item is not "running" */
  const s = itemDisplayState(i)
  if (s.kind === "blocked") return { label: tr("thread:card.status.blocked"), dot: "block", tone: "quiet" }
  if (s.kind === "paused") return { label: tr("thread:card.status.paused"), dot: "pause", tone: "quiet" }
  if (s.kind === "idle" && !i.rel) return { label: tr("views:itemState.idle"), dot: "pause", tone: "quiet" }
  if (i.rel?.tone === "run") return { label: tr("thread:card.status.releasing"), dot: "run", tone: "run" }
  if (s.kind === "closing") return { label: tr("views:itemState.closing"), dot: s.tone === "wait" ? "wait" : "run", tone: s.tone === "wait" ? "wait" : "run" }
  if (s.kind === "queued") return { label: tr("thread:card.status.queued"), dot: "run", tone: "run" }
  return { label: tr("thread:card.status.running"), dot: "run", tone: "run" }
}
/* tinted status pills: the status token at a low wash, the token itself as text */

/* inside a card, Markdown headings are text: a summary that starts with "## …" never outranks the card's own title */
const FLAT = "[&_:is(h1,h2,h3,h4,h5,h6)]:[font:inherit]! [&_:is(h1,h2,h3,h4,h5,h6)]:mt-0! [&_:is(h1,h2,h3,h4,h5,h6)]:mb-0! [&_:is(h1,h2,h3,h4,h5,h6)]:font-medium!"
/** A text-only preview: clamping rich Markdown can leave clipped links focusable and still mount images and widgets. */
function DeliveryBlock({ d }: { d: NonNullable<ItemCardState["delivery"]> }) {
  const [open, setOpen] = useState(false), [clipped, setClipped] = useState(false), t = useT()
  const box = useRef<HTMLDivElement>(null), id = useId()
  const draft = d.decision ? decisionDraft(d.decision) : undefined
  const full = draft?.trim() ? draft : d.text
  const preview = clipPlain(d.text.replace(/!?\[([^\]]*)\]\([^\s)]*\)/g, "$1"), 180)
  // Even the short text can wrap beyond two lines on a narrow screen.
  useLayoutEffect(() => {
    const el = box.current
    if (!el || open) return
    const check = () => setClipped(el.scrollHeight > el.clientHeight + 1)
    check()
    const ro = new ResizeObserver(check); ro.observe(el)
    return () => ro.disconnect()
  }, [open, preview])
  return (
    <div className="flex min-w-0 flex-col gap-1" data-card-delivery>
      <div className="type-label text-tertiary">{t("thread:card.delivery")}</div>
      <div id={id} ref={box} className={cn("min-w-0 [overflow-wrap:anywhere]", open ? FLAT : "line-clamp-2 type-callout")} data-card-summary>
        {open ? <Markdown text={full} className="type-callout" /> : preview}
      </div>
      {(open || clipped || preview !== full.trim()) && <Button type="button" size="sm" variant="ghost" className="-ml-2 self-start text-muted-foreground" onClick={() => setOpen(!open)} aria-expanded={open} aria-controls={id} data-card-expand>
        {t(open ? "thread:card.collapseSummary" : "thread:card.expandSummary")}<ChevronDownIcon aria-hidden className={cn("size-3.5 transition-transform", open && "rotate-180")} />
      </Button>}
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
        : <><Button size="sm" variant="outline" onClick={() => setRework(true)}>{t("thread:card.rework")}</Button><Button size="sm" disabled={busy} onClick={() => void act("pass")} data-pass><CheckIcon />{t("thread:card.pass")}</Button></>}</div>
    </div>
  )
}

/** One card per item made from the conversation: what it is doing now, its latest delivery (folded), the reply to what you
 * last sent it, and whatever it needs from you — answerable right here. Lifecycle events update it; they are not messages. */
export function ItemCard({ c }: { c: ItemCardState }) {
  const i = useItemDetail(c.item, st.ready && (!!c.delivery || c.state === "deliver" || c.state === "done")), t = useT()
  if (!i) return null
  const look = cardLook(i, c), w = i.waits[0]
  /* what it is doing now, only while it does something: a stale line under a fresh status contradicts it */
  const doing = look.tone === "run" && itemDisplayState(i).kind === "working" ? i.progress : ""
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
          <span className="truncate type-callout text-muted-foreground"><span className="font-mono type-footnote tabular-nums">{i.id}</span> · {botName(i.b)}{doing ? " · " + doing : ""}</span>
        </span>
        <Pill tone={look.tone} className="mt-0.5 h-auto max-w-[45%] py-0.5 leading-4" data-card-status>{look.tone === "done" && c.released ? <RocketIcon className="size-3.5 shrink-0" /> : look.tone === "run" ? <span className="dot-running size-1.5 shrink-0 rounded-full bg-current" aria-hidden /> : null}{look.label}{look.version && <span className="hidden min-w-0 max-w-32 truncate font-mono font-normal sm:inline">{look.version}</span>}</Pill>
      </button>
      {d && <DeliveryBlock key={d.decision ?? d.text} d={d} />}
      {i.outs.length > 0 && <DeliveryCards key={i.id} item={i.id} outs={i.outs} anns={i.anns} />}
      {reply && <div className="flex min-w-0 flex-col gap-1 rounded-lg bg-muted/60 px-3 py-2.5" data-card-reply>
        <div className="type-label text-tertiary">{t("thread:card.reply")}</div><div className={cn("line-clamp-4 min-w-0 break-words", FLAT)}><Markdown text={reply.text} className="type-callout" replyFiles /></div></div>}
      {c.build && <div className="flex items-start gap-2 type-callout text-muted-foreground" data-card-build><CircleAlertIcon className="mt-0.5 size-4 shrink-0 text-attention" strokeWidth={1.75} />
        <span className="min-w-0 flex-1 [overflow-wrap:anywhere]"><Linked text={c.build.text.replace(new RegExp(`^${i.id}\\s+`), "")} /></span></div>}
      {failLine && <div className="flex items-start gap-2 type-callout text-muted-foreground" data-card-fail><CircleAlertIcon className="mt-0.5 size-4 shrink-0 text-destructive" strokeWidth={1.75} /><span className="min-w-0 flex-1">{failLine}</span>
        {c.decision && <AlertClose onClose={() => closeAlerts([c.decision!])} className="-my-0.5" />}</div>}
      {accept ? <AcceptRow i={i} decision={accept.decision} /> : w ? <div className="-mx-1.5 [&_[data-decision-wrap]]:bg-muted/40 [&_[data-decision-wrap]]:shadow-none [&_[data-decision-wrap]]:ring-0 [&_.ui-dcc\_\_badge]:hidden" data-card-decision>
        <DecisionCard i={i} w={w} showItem={false} /></div> : null}
    </div>
  )
}

/* the items a Steve message links to (its own or a taken offer's) */
const linksOf = (m: Msg) => [m.item, m.offer?.state === "taken" ? m.offer.item : undefined].filter(Boolean)
/** whether a Steve message before `from`, since you last spoke, already links to `id`: the way to its card is said once per answer */
function linkedAbove(from: string | undefined, id: string) {
  const messages = threadMessages()
  const at = from ? messages.findIndex((m) => m.id === from) : -1
  for (let n = at - 1; n >= 0; n--) { const m = messages[n]; if (m.k === "me") return false; if (m.k === "steve" && (m.card?.item === id || linksOf(m).includes(id))) return true }
  return false
}
/** an item whose card is elsewhere in the conversation (it moved, or this message only mentions the item): one quiet line to the card.
 *  `from`: the message it is under; when one just above already points to the same card, nothing */
export function CardLink({ id, from }: { id: string; from?: string }) {
  const i = useItem(id), at = threadMessages().findLast((m) => m.card?.item === id), t = useT()
  if (!i || linkedAbove(from, id)) return null
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
  const it = f ? getItem(f) : undefined
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
