import { SessionApproval } from "@/approval"
import { useEffect, useRef, useState } from "react"
import { cn } from "cn"
import { ArrowRightLeftIcon, DownloadIcon, LockIcon, MoreHorizontalIcon, PanelRightIcon, PencilIcon, SquareIcon, TrashIcon, XIcon } from "lucide-react"
import { api } from "@/api"
import { stats, type SessionTotals } from "@/api/stats"
import { SessionCards } from "@/thread/decision-card"
import { ActorAvatar } from "@/components/actor-avatar"
import { HarnessMark } from "@/components/harness-mark"
import { PlacementWhy } from "@/bot-dispatch"
import { SessionIdentity } from "@/components/session-identity"
import { RunningNow, type RunningTask } from "@/components/affect/running-now"
import { Button } from "@/components/ui/button"
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuSeparator, DropdownMenuTrigger } from "@/components/ui/dropdown-menu"
import { Input } from "@/components/ui/input"
import { Sheet, SheetContent, SheetTitle } from "@/components/ui/sheet"
import { draftKey } from "@/composer-draft"
import { readFullTranscript, useTranscriptSelector } from "@/data/transcript"
import { activeSubtasksOf, autoModeWarningOf, type ActiveSubtask, type AutoModeClassifierWarning } from "@/data/transcript-model"
import { useIsMobile } from "@/hooks/use-mobile"
import { useT } from "@/i18n"
import { lazyView } from "@/lazy"
import { queueNote, QueueStrip } from "@/queue"
import { useHubQuery } from "@/data/use-hub-query"
import { fmtPct, fmtUsage, fmtUsageCost, fmtUsd } from "@/stats/format"
import { botName, closedSt, hm, interrupt, sessPatch, sessRemove, sessSend, st, toast, useItem, useSessionQueue, type Sess, useDeviceInventory } from "@/store"
import { Empty, go, openSheet, Page, PANEL_W, Status } from "@/ui"
import { Composer, type ComposerHandle } from "@/composer"
import { formatEffortMapping } from "@/effort-picker"
import { AutoModeClassifierAlert, RunBar, Transcript } from "@/thread/transcript"
import { ProjectChip } from "@/view-shared"

const SessionPanel = lazyView(() => import("@/session/panel").then((m) => ({ default: m.SessionPanel })))
const ItemDebugDialog = lazyView(() => import("@/debug-panel").then((m) => ({ default: m.ItemDebugDialog })))

function activeSubtasksEqual(a: ActiveSubtask[], b: ActiveSubtask[]): boolean {
  if (a.length !== b.length) return false
  for (let i = 0; i < a.length; i++) {
    const x = a[i], y = b[i]
    if (x.childSteps !== y.childSteps || x.subtask.id !== y.subtask.id || x.subtask.status !== y.subtask.status
      || x.subtask.title !== y.subtask.title || x.subtask.progress !== y.subtask.progress
      || x.subtask.origin !== y.subtask.origin || x.subtask.can_cancel !== y.subtask.can_cancel) return false
  }
  return true
}

function SessionSubtasksPill({ k, run, ro }: { k: string; run: boolean; ro?: boolean }) {
  const t = useT()
  const activeSubtasks = useTranscriptSelector(ro || !run ? null : k, activeSubtasksOf, activeSubtasksEqual)
  if (ro || !run || !activeSubtasks.length) return null
  const runningTasks: RunningTask[] = activeSubtasks.map(({ subtask, childSteps }) => ({
    id: subtask.id,
    title: subtask.title || t(`core:subtask.origin.${subtask.origin}`),
    step: subtask.progress || t(`core:subtask.origin.${subtask.origin}`),
    done: childSteps,
    total: 0,
    state: subtask.status === "needs_you" ? "needs-you" : subtask.status === "queued" ? "paused" : "running",
    resumable: false,
  }))
  const allCancellable = activeSubtasks.every(({ subtask }) => !!subtask.can_cancel)
  const focusSubtask = (id: string) => {
    const el = document.querySelector(`[data-subtask-id="${CSS.escape(id)}"]`)
    el?.scrollIntoView({ behavior: "smooth", block: "center" })
  }
  const cancelSubtask = async (id: string) => {
    try { await api.cancelSubtask(k, id) }
    catch (err) { toast((err as Error).message || t("core:error.generic")) }
  }
  return <RunningNow tasks={runningTasks} align="end" onOpen={focusSubtask} onPause={allCancellable ? cancelSubtask : undefined} labels={{ title: t("core:subtask.runningTitle"), pause: t("core:subtask.cancel") }} />
}

function autoModeWarningEqual(a: AutoModeClassifierWarning | null, b: AutoModeClassifierWarning | null): boolean {
  if (a === b) return true
  if (!a || !b) return false
  return a.stopped === b.stopped && a.harness === b.harness && a.text === b.text
}

function SessionAutoModeWarning({ k, harness }: { k: string; harness?: string }) {
  const warning = useTranscriptSelector(k, autoModeWarningOf, autoModeWarningEqual)
  if (!warning) return null
  return <AutoModeClassifierAlert warning={warning} harnessFallback={harness} className="mb-2" />
}

export default function SessionView({ k, s }: { k: string; s?: Sess }) {
  const t = useT()
  const [renaming, setRenaming] = useState(false)
  const [exporting, setExporting] = useState(false)
  const [infoOpen, setInfoOpen] = useState(false)
  const [debugOpen, setDebugOpen] = useState(false)
  const phone = useIsMobile()
  const composer = useRef<ComposerHandle | null>(null)
  const totalsResult = useHubQuery(["stats", "session", k, !!s, s?.st === "run" ? 0 : s?.at], () => (s ? stats.session(k) : Promise.resolve(null)), { events: ["stats"], debounceMs: 200, live: !!s })
  const totals = totalsResult.data ?? undefined
  useSessionQueue(s?.id)
  const scroller = useRef<HTMLDivElement>(null)
  useEffect(() => { setDebugOpen(false); setInfoOpen(false) }, [k])
  const devices = useDeviceInventory(!!s?.node)
  const i = useItem(s?.item) ?? null
  if (!s) return <Page title={t("views:session.title")} back={["#/sessions", t("views:sessions.title")]}><Empty title={t("views:session.notFound")} /></Page>
  const run = s.st === "run"
  const canHandoff = !s.ro && !!s.item && (!i || !closedSt(i.st))
  const device = s.node && devices.find((d) => d.node_id === s.node || d.id === s.node)
  const mark = <HarnessMark harness={s.harness ?? s.h} title={s.h} />
  const exportMd = async () => {
    setExporting(true)
    try {
      const turns = await readFullTranscript(k)
      const lines = turns.flatMap((turn) => turn.ev.map((e) => e[0] === "u" ? t("views:session.exportYou") + e[1] : e[0] === "hi" ? "> " + e[1] : e[0] === "a" ? e[1] : e[0] === "tool" ? "- " + e[2] + " " + e[3] : e[0] === "sys" ? "_" + e[1] + "_" : "").filter(Boolean))
      const a = document.createElement("a")
      const url = URL.createObjectURL(new Blob([`# ${s.title}\n\n`, lines.join("\n\n")], { type: "text/markdown" }))
      a.href = url; a.download = `${s.title.slice(0, 40)}.md`; a.click()
      setTimeout(() => URL.revokeObjectURL(url), 0)
      toast(t("views:session.exported"))
    } catch (error) { toast((error as Error).message || t("core:error.generic")) }
    finally { setExporting(false) }
  }
  const menu = (
    <DropdownMenu>
      <DropdownMenuTrigger render={<Button variant="ghost" size="icon-sm" aria-label={t("common:action.more")} className="text-muted-foreground" data-session-menu />}><MoreHorizontalIcon /></DropdownMenuTrigger>
      <DropdownMenuContent align="end" className="min-w-40">
        {canHandoff && <DropdownMenuItem onClick={() => openSheet("handoff", { item: s.item, session: k })} data-handoff><ArrowRightLeftIcon />{t("views:session.handoff")}</DropdownMenuItem>}
        {!s.ro && <DropdownMenuItem onClick={() => setRenaming(true)} data-rename><PencilIcon />{t("views:session.rename")}</DropdownMenuItem>}
        <DropdownMenuItem disabled={exporting} onClick={exportMd} data-export><DownloadIcon />{t("views:session.export")}</DropdownMenuItem>
        {s.item && <DropdownMenuItem onClick={() => setDebugOpen(true)} data-session-debug>{t("views:debug.open")}</DropdownMenuItem>}
        {!s.ro && s.st === "idle" && <DropdownMenuItem onClick={() => sessPatch(k, { st: "ended" }, t("views:session.ended"))} data-end><SquareIcon />{t("views:session.end")}</DropdownMenuItem>}
        {s.ro && <><DropdownMenuSeparator /><DropdownMenuItem variant="destructive" onClick={() => { go("#/sessions"); sessRemove(k) }} data-remove-import><TrashIcon />{t("views:session.remove")}</DropdownMenuItem></>}
      </DropdownMenuContent>
    </DropdownMenu>
  )
  return (
    <div className="flex h-full min-h-0">
      <div className="min-w-0 flex-1">
        <Page key={k} virtual noTitle title={t("views:session.numbered", { n: s.n })} back={i ? ["#/items/" + i.id, t("views:items.title")] : ["#/sessions", t("views:sessions.title")]} max={720} center scrollRef={scroller}
          bar={<div className="flex min-w-0 items-center gap-2" data-session-bar><span className="hidden shrink-0 sm:inline-flex" data-session-mark={s.harness ?? ""}>{mark}</span><span className="truncate type-headline">{s.n ? t("views:session.numberedBar", { n: s.n }) : ""}{renaming ? <Input autoFocus defaultValue={s.title} className="inline-flex h-7 w-56 rounded-md" data-rename-input onBlur={(e) => { const v = e.target.value.trim(); if (v && v !== s.title) sessPatch(k, { title: v }, t("views:session.renamed")); setRenaming(false) }} onKeyDown={(e) => e.key === "Enter" && (e.target as HTMLInputElement).blur()} /> : s.title}</span><Status s={s.st} sess pill className="hidden sm:inline-flex" />{i && <ProjectChip id={i.project} className="hidden shrink-0 type-footnote text-muted-foreground sm:inline-flex" />}</div>}
          actions={<><SessionSubtasksPill k={k} run={run} ro={s.ro} />{!s.ro && <SessionStats totals={totals} ctx={s.contextKnown ? s.ctx : undefined} />}{canHandoff && <Button size="sm" variant="ghost" className="hidden text-muted-foreground sm:inline-flex" onClick={() => openSheet("handoff", { item: s.item, session: k })} data-session-handoff><ArrowRightLeftIcon />{t("views:session.handoff")}</Button>}<Button size="icon-sm" variant="ghost" className="text-muted-foreground sm:hidden" aria-label={t("views:session.info")} onClick={() => setInfoOpen(true)} data-session-info><PanelRightIcon /></Button><Button size="sm" variant="ghost" className="hidden text-muted-foreground sm:inline-flex xl:hidden" onClick={() => setInfoOpen(true)} data-session-info-wide><PanelRightIcon />{t("views:session.info")}</Button>{s.ro && <span className="inline-flex items-center gap-1 type-footnote text-muted-foreground"><LockIcon className="size-3" />{t("views:session.readOnly")}</span>}{menu}</>}
          footer={
            /* the transcript's column, centred as it is: the composer and the transcript share both edges */
            <div className="mx-auto w-full max-w-[824px] shrink-0 px-4 pb-[76px] sm:px-8 lg:pb-5"><div>
              <div className="flex flex-wrap items-center gap-x-4 gap-y-1 py-2 type-footnote text-tertiary" data-session-runtime>
                {s.ro ? (
                  <span data-runtime-harness><SessionIdentity session={s} showNode={false} showStatus={false} /></span>
                ) : s.b ? (
                  <span className="inline-flex min-w-0 items-center gap-1.5 text-muted-foreground" data-runtime-bot>
                    <ActorAvatar actorId={s.b} size={16} />
                    <span className="truncate">{botName(s.b)}</span>
                  </span>
                ) : null}
                {device
                  ? <a href={"#/devices/nodes/" + encodeURIComponent(device.id)} className="hover:text-foreground" data-runtime-device>{t("views:session.runsOn", { device: device.name })}</a>
                  : s.node ? <span data-runtime-device>{t("views:session.runsOn", { device: s.node })}</span> : null}
                <SessionApproval id={k} showApplied={false} />
                <span>{t("views:session.effortLabel")} <span className="text-muted-foreground" data-session-effort-mapping>{formatEffortMapping(t, s.effortMapping, s.h, s.reasoningEffort)}</span></span>
                {/* a reported context is the composer's ring; that nothing is reported is said here */}
                {!s.ro && !s.contextKnown && <span data-session-ctx-unreported>{t("views:session.ctxLabel")} <span className="text-muted-foreground">{t("views:session.unreported")}</span></span>}
              </div>
              {s.placement && <PlacementWhy value={s.placement} />}
              {!s.ro && <SessionAutoModeWarning k={k} harness={s.h} />}
              <RunBar k={k} />
              {s.ro ? (
                <div className="flex flex-wrap items-center gap-3 rounded-xl bg-muted/60 px-4 py-3" data-readonly>
                  <LockIcon className="size-4 text-muted-foreground" /><span className="min-w-0 flex-1 type-callout text-muted-foreground">{t("views:session.readOnlyNote", { src: s.h ? t("views:session.srcLocal", { h: s.h }) : s.src })}</span>
                  <Button size="sm" variant="outline" onClick={() => openSheet("fork", { k })}>{t("views:session.toItem")}</Button>
                </div>
              ) : <Composer key={k} draftKey={draftKey(st.workspace?.id, "session", k)} handle={composer} testId="session" model={hm(s)} modelIcon={<HarnessMark harness={s.harness ?? s.h} />} placeholder={s.st === "ended" ? t("views:session.endedPlaceholder") : t("views:session.placeholder")}
                /* while a turn runs Enter steers it (插话) and ⌥ Enter queues (排队); Stop and Esc interrupt it */
                busy={run} steers onStop={() => interrupt(k)} onSend={(text, files, _refs, _mentions, _id, how) => sessSend(k, text, how, files)}
                queue={<QueueStrip sid={k} rows={s.queue} note={queueNote(s)} starting={s.starting} composer={composer} />}
                context={s.contextKnown ? { value: s.ctx / 100, title: t("views:session.ctxUsed", { pct: s.ctx }) } : undefined} />}
            </div></div>
          }>
          <div className="pt-6"><Transcript k={k} scroll={scroller} /><SessionCards k={k} /></div>
        </Page>
      </div>
      {/* the same panel as the chat rail: one width, a head on the top bar's line, the content scrolling under it */}
      <aside className={cn("hidden shrink-0 flex-col border-l border-sidebar-border bg-sidebar xl:flex", PANEL_W)} data-session-panel>
        <div className="flex h-12 shrink-0 items-center gap-2 border-b border-sidebar-border px-5 type-headline" data-session-panel-head>{t("views:session.info")}</div>
        <div className="min-h-0 flex-1 overflow-y-auto overscroll-contain px-5 pb-8 pt-5" data-session-panel-scroll>{!infoOpen && <SessionPanel k={k} tot={totals} />}</div>
      </aside>
      {debugOpen && s.item && <ItemDebugDialog item={s.item} session={k} onClose={() => setDebugOpen(false)} />}
      {/* narrower than the panel's room: the same panel as a sheet, from the bottom on a phone and from the side otherwise */}
      <Sheet open={infoOpen} onOpenChange={setInfoOpen}>
        <SheetContent side={phone ? "bottom" : "right"} showCloseButton={false} className={cn("gap-0 overflow-hidden bg-sidebar p-0", phone ? "max-h-[88dvh] rounded-t-2xl" : "w-[92vw] max-w-[380px]!")} data-session-sheet>
          <div className="flex h-12 shrink-0 items-center gap-2 border-b border-sidebar-border px-5">{mark}<SheetTitle className="min-w-0 flex-1 truncate type-headline">{t("views:sessionPanel.sheet")}</SheetTitle><Button size="icon-sm" variant="ghost" className="text-muted-foreground" aria-label={t("common:action.close")} onClick={() => setInfoOpen(false)}><XIcon /></Button></div>
          <div className="min-h-0 flex-1 overflow-y-auto overscroll-contain px-4 pb-[max(2rem,env(safe-area-inset-bottom))] pt-4" data-session-sheet-body>{infoOpen && <SessionPanel k={k} tot={totals} />}</div>
        </SheetContent>
      </Sheet>
    </div>
  )
}

/** 会话页顶栏：“本会话 估 $1.88 · 38 回合”（stats.md §6.5） */
function SessionStats({ totals, ctx }: { totals?: SessionTotals; /** the context in use, %, when the harness reports it */ ctx?: number }) {
  const t = useT()
  const ctxTag = ctx != null && <span className={cn("hidden whitespace-nowrap px-1 type-footnote tabular-nums md:inline", ctx >= 80 ? "text-attention" : "text-muted-foreground")} data-session-ctx-head={ctx}>{t("views:sessionPanel.ctxShort", { pct: ctx })}</span>
  if (!totals || (!totals.turns && !totals.usage.total && !totals.usage.reported)) return ctxTag || null
  const u = totals.usage
  const est = u.coverage.total === null ? fmtUsage(u) : u.total || u.coverage.state === "complete" ? t("stats:word.estShort", { usd: fmtUsageCost(u) }) : u.reported ? t("stats:word.quoteFloor", { usd: fmtUsd(u.reported) }) : ""
  return (<>{ctxTag}
    <span className="hidden whitespace-nowrap px-1 type-footnote tabular-nums text-muted-foreground md:inline" title={u.hit_rate != null ? t("stats:drill.sessionTitleHit", { tokens: fmtUsage(u), pct: fmtPct(u.hit_rate) }) : t("stats:drill.sessionTitle", { tokens: fmtUsage(u) })} data-session-stats>
      {est ? t("stats:drill.sessionLine", { est, count: totals.turns }) : t("stats:drill.sessionLineNoEst", { count: totals.turns })}
    </span></>
  )
}
