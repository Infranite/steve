import { useEffect, useRef, useState } from "react"
import { BugIcon, CheckIcon, CopyIcon, RefreshCwIcon } from "lucide-react"
import { debugApi, type DebugEvent, type DebugFilter, type DebugPage, type DebugSnapshot } from "@/api/debug"
import { Button } from "@/components/ui/button"
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from "@/components/ui/dialog"
import { Input } from "@/components/ui/input"
import { Spinner } from "@/components/ui/spinner"
import { useT } from "@/i18n"
import { clockDur, date, num } from "@/i18n/format"
import { cn } from "cn"
import { NativeSelect } from "@/components/ui/native-select"

const time = (at: number) => date(at, { month: "short", day: "numeric", hour: "2-digit", minute: "2-digit", second: "2-digit" })
const errorText = (error: unknown) => error instanceof Error ? error.message : String(error)
const json = (value: unknown) => JSON.stringify(value, null, 2)


export function ItemDebugDialog({ item, session, onClose }: { item: string; session?: string; onClose: () => void }) {
  const t = useT()
  /* the dialog itself takes focus: a filter that grabs it opens with a focus ring nobody asked for */
  const popup = useRef<HTMLDivElement>(null)
  return <Dialog open onOpenChange={(open) => !open && onClose()}>
    <DialogContent ref={popup} initialFocus={popup} className="flex max-h-[90dvh] flex-col gap-4 overflow-hidden p-4 outline-none sm:max-w-[900px] sm:p-6" data-debug-dialog>
      <DialogHeader className="pr-10">
        <DialogTitle className="flex items-center gap-2"><BugIcon className="size-4 text-muted-foreground" />{t("views:debug.title", { item })}</DialogTitle>
        <DialogDescription>{t("views:debug.description")}</DialogDescription>
      </DialogHeader>
      <ItemDebugPanel key={`${item}:${session ?? ""}`} item={item} session={session} />
    </DialogContent>
  </Dialog>
}

/** Reads are scoped to this mounted item and filter. Late answers cannot replace a new query. */
export function ItemDebugPanel({ item, session }: { item: string; session?: string }) {
  const t = useT()
  const [snapshot, setSnapshot] = useState<DebugSnapshot | null>(null)
  const [snapshotError, setSnapshotError] = useState("")
  const [snapshotLoading, setSnapshotLoading] = useState(true)
  const [changing, setChanging] = useState(false)
  const [refresh, setRefresh] = useState(0)
  const [now, setNow] = useState(Date.now())
  const [minutes, setMinutes] = useState(30)
  const [sessionId, setSessionId] = useState(session ?? "")
  const [kind, setKind] = useState("")
  const [severity, setSeverity] = useState("")
  const [attempt, setAttempt] = useState("")
  const [page, setPage] = useState<DebugPage | null>(null)
  const [events, setEvents] = useState<DebugEvent[]>([])
  const [eventsError, setEventsError] = useState("")
  const [eventsLoading, setEventsLoading] = useState(true)
  const [moreLoading, setMoreLoading] = useState(false)
  const generation = useRef(0)
  const mounted = useRef(true)
  const expired = useRef<number | null>(null)
  const filter: DebugFilter = { kinds: kind || undefined, severity: severity || undefined, session_id: sessionId || undefined, attempt_id: attempt.trim() || undefined }
  const filterKey = JSON.stringify(filter)

  useEffect(() => {
    mounted.current = true
    const timer = setInterval(() => setNow(Date.now()), 1000)
    return () => { mounted.current = false; clearInterval(timer); generation.current++ }
  }, [])

  useEffect(() => {
    let current = true
    const controller = new AbortController()
    setSnapshotLoading(true); setSnapshotError("")
    debugApi.explain(item, controller.signal).then((value) => { if (current) setSnapshot(value) }, (error) => { if (current) setSnapshotError(errorText(error)) }).finally(() => { if (current) setSnapshotLoading(false) })
    return () => { current = false; controller.abort() }
  }, [item, refresh])

  useEffect(() => {
    const request = ++generation.current
    const controller = new AbortController()
    setEvents([]); setPage(null); setEventsLoading(true); setMoreLoading(false); setEventsError("")
    debugApi.events(item, JSON.parse(filterKey) as DebugFilter, undefined, controller.signal).then((value) => {
      if (generation.current === request) { setPage(value); setEvents(value.events) }
    }, (error) => { if (generation.current === request) setEventsError(errorText(error)) }).finally(() => { if (generation.current === request) setEventsLoading(false) })
    return () => { controller.abort(); generation.current++ }
  }, [item, filterKey, refresh])

  const capture = snapshot?.capture
  const elapsed = capture?.active && capture.expires_at != null && now >= capture.expires_at
  useEffect(() => {
    if (elapsed && capture?.expires_at != null && expired.current !== capture.expires_at) {
      expired.current = capture.expires_at
      setRefresh((n) => n + 1)
    }
  }, [elapsed, capture?.expires_at])

  const changeCapture = async () => {
    if (changing || !snapshot?.can_capture) return
    setChanging(true); setSnapshotError("")
    try {
      const result = await debugApi.capture(item, !capture?.active, minutes)
      if (mounted.current) { setSnapshot(result); setRefresh((n) => n + 1) }
    } catch (error) { if (mounted.current) setSnapshotError(errorText(error)) }
    finally { if (mounted.current) setChanging(false) }
  }
  const more = async () => {
    if (moreLoading || !page?.next_cursor) return
    const request = generation.current
    const cursor = page.next_cursor
    setMoreLoading(true); setEventsError("")
    try {
      const value = await debugApi.events(item, filter, cursor)
      if (generation.current !== request) return
      if (value.has_more && value.next_cursor === cursor) throw new Error(t("views:debug.cursorStalled"))
      setEvents((old) => {
        const seen = new Set(old.map((event) => event.event_id))
        return [...old, ...value.events.filter((event) => !seen.has(event.event_id))]
      })
      setPage((old) => ({ ...value, gaps: [...(old?.gaps ?? []), ...value.gaps], truncated: !!old?.truncated || value.truncated }))
    } catch (error) { if (generation.current === request) setEventsError(errorText(error)) }
    finally { if (generation.current === request) setMoreLoading(false) }
  }
  const sessions = snapshot?.sessions ?? []
  const kinds = [...new Set(events.map((event) => event.kind))]
  const coverage = page?.coverage ?? snapshot?.coverage

  return <div className="min-h-0 space-y-5 overflow-y-auto pr-1" data-debug-panel>
    <section className="space-y-3 rounded-xl bg-muted/60 p-4" data-debug-state>
      <div className="flex flex-wrap items-center justify-between gap-2">
        <h3 className="type-headline">{t("views:debug.current")}</h3>
        <Button size="xs" variant="ghost" onClick={() => setRefresh((n) => n + 1)} disabled={snapshotLoading || changing} data-debug-refresh><RefreshCwIcon />{t("views:debug.refresh")}</Button>
      </div>
      {snapshotLoading && !snapshot && <p className="flex items-center gap-2 type-footnote text-muted-foreground"><Spinner className="size-3" />{t("views:debug.loading")}</p>}
      {/* the same failure the event list reports (with its retry) is said once, there; here only the state is missing */}
      {snapshotError && <p role={snapshotError === eventsError ? undefined : "alert"} className={cn("break-words type-footnote", snapshotError === eventsError ? "text-muted-foreground" : "text-destructive")} data-debug-snapshot-error>{snapshotError === eventsError ? t("views:debug.stateUnavailable") : snapshotError}</p>}
      {snapshot && <>
        <div className="flex flex-wrap gap-x-4 gap-y-1 type-footnote text-muted-foreground">
          <span>{t("views:debug.itemState", { status: snapshot.item.status })}</span>
          <span>{t("views:debug.revision", { cycle: snapshot.item.cycle, revision: snapshot.item.revision })}</span>
          <span>{t("views:debug.observed", { time: time(snapshot.observed_at) })}</span>
        </div>
        <CurrentBlockers snapshot={snapshot} />
        <div className="flex flex-col items-start gap-3 border-t border-foreground/6 pt-3 sm:flex-row sm:items-center sm:justify-between">
          <div className="min-w-0 flex-1">
            <p className="type-callout" data-debug-capture-state>{capture?.active ? elapsed ? t("views:debug.expiryChecking") : t("views:debug.recording", { duration: clockDur(Math.max(0, (capture.expires_at ?? now) - now)) }) : t("views:debug.notRecording")}</p>
            <p className="mt-1 type-footnote text-muted-foreground">{t("views:debug.captureScope")}</p>
            {capture?.stop_reason && <p className="mt-1 break-words type-footnote font-mono text-muted-foreground">{t("views:debug.stopReason", { reason: capture.stop_reason })}</p>}
          </div>
          {snapshot.can_capture ? <div className="flex items-center gap-2">
            {!capture?.active && <NativeSelect aria-label={t("views:debug.duration")} value={minutes} onChange={(event) => setMinutes(Number(event.target.value))} disabled={changing} data-debug-minutes>
              {[15, 30, 60].map((value) => <option key={value} value={value}>{t("views:debug.minutes", { count: value })}</option>)}
            </NativeSelect>}
            <Button size="sm" variant={capture?.active ? "outline" : "default"} disabled={changing || snapshotLoading} onClick={changeCapture} data-debug-toggle>{changing && <Spinner className="size-3" />}{capture?.active ? t("views:debug.stop") : t("views:debug.start")}</Button>
          </div> : <span className="type-footnote text-muted-foreground">{t("views:debug.ownerOnly")}</span>}
        </div>
        {!!capture?.dropped && <p role="status" className="type-footnote text-attention">{t("views:debug.dropped", { count: capture.dropped })}</p>}
      </>}
    </section>

    <section className="space-y-3" data-debug-events>
      <div className="flex items-center justify-between gap-2"><h3 className="type-headline">{t("views:debug.events")}</h3><span className="type-footnote tabular-nums text-muted-foreground">{t("views:debug.loaded", { count: events.length })}</span></div>
      <div className="grid grid-cols-2 gap-2 sm:grid-cols-4">
        <label className="min-w-0 space-y-1 type-footnote text-muted-foreground">{t("views:debug.session")}<NativeSelect className="w-full" value={sessionId} onChange={(event) => setSessionId(event.target.value)} data-debug-session>
          <option value="">{t("views:debug.allSessions")}</option>
          {sessionId && !sessions.some((value) => value.id === sessionId) && <option value={sessionId}>{sessionId}</option>}
          {sessions.map((value) => <option key={value.id} value={value.id}>{value.id}</option>)}
        </NativeSelect></label>
        <label className="min-w-0 space-y-1 type-footnote text-muted-foreground">{t("views:debug.kind")}<Input className="h-8 type-footnote font-mono" value={kind} onChange={(event) => setKind(event.target.value)} list="debug-event-kinds" placeholder={t("views:debug.allKinds")} data-debug-kind /><datalist id="debug-event-kinds">{kinds.map((value) => <option key={value} value={value} />)}</datalist></label>
        <label className="min-w-0 space-y-1 type-footnote text-muted-foreground">{t("views:debug.severity")}<NativeSelect className="w-full" value={severity} onChange={(event) => setSeverity(event.target.value)} data-debug-severity>
          <option value="">{t("views:debug.allLevels")}</option><option value="info">{t("views:debug.info")}</option><option value="warning">{t("views:debug.warning")}</option><option value="error">{t("views:debug.error")}</option>
        </NativeSelect></label>
        <label className="min-w-0 space-y-1 type-footnote text-muted-foreground">{t("views:debug.attempt")}<Input className="h-8 type-footnote font-mono" value={attempt} onChange={(event) => setAttempt(event.target.value)} placeholder={t("views:debug.allAttempts")} data-debug-attempt /></label>
      </div>
      <p className="type-footnote text-muted-foreground">{t("views:debug.historyHint")}</p>
      <p className="type-footnote text-muted-foreground" data-debug-delivery>{t("views:debug.deliveryHint")}</p>
      {coverage?.refresh_required && <p role="status" className="rounded-lg bg-attention/10 px-3.5 py-2.5 type-callout text-attention" data-debug-coverage-refresh>{t("views:debug.coverageRefresh")}</p>}
      {coverage?.recorder_available === false && <p role="alert" className="rounded-lg bg-destructive/10 px-3.5 py-2.5 type-callout text-destructive" data-debug-recorder-unavailable>{t("views:debug.recorderUnavailable")}</p>}
      {!!coverage?.instance_pending && coverage.instance_pending > 0 && <p role="status" className="rounded-lg bg-attention/10 px-3.5 py-2.5 type-callout text-attention" data-debug-pending>{t("views:debug.pending", { count: coverage.instance_pending })}</p>}
      {page && (page.truncated || page.gaps.length > 0) && <div role="status" className="rounded-lg bg-attention/10 px-3.5 py-2.5 type-callout text-attention" data-debug-gaps>
        <p>{t("views:debug.incomplete")}</p>
        {page.gaps.length > 0 && <details className="mt-1"><summary className="cursor-pointer">{t("views:debug.gapDetails")}</summary><pre className="mt-2 whitespace-pre-wrap break-all type-footnote font-mono">{json(page.gaps)}</pre></details>}
      </div>}
      {eventsLoading && <p className="flex items-center gap-2 py-3 type-footnote text-muted-foreground"><Spinner className="size-3" />{t("views:debug.loading")}</p>}
      {eventsError && <div role="alert" className="flex flex-wrap items-center gap-2 rounded-lg bg-destructive/10 px-3.5 py-2.5 type-callout text-destructive" data-debug-events-error><span className="min-w-0 flex-1 break-words">{eventsError}</span><Button size="xs" variant="outline" onClick={() => events.length && page?.next_cursor ? void more() : setRefresh((n) => n + 1)} disabled={moreLoading}>{t("views:debug.retry")}</Button></div>}
      {!eventsLoading && !eventsError && events.length === 0 && <p className="rounded-lg bg-muted/60 px-4 py-6 text-center type-callout text-muted-foreground" data-debug-empty>{t("views:debug.empty")}</p>}
      <ol className="space-y-2">{events.map((event) => <EventRow key={event.event_id} item={item} event={event} />)}</ol>
      {page?.has_more && page.next_cursor && <Button className="w-full" size="sm" variant="outline" disabled={moreLoading} onClick={more} data-debug-more>{moreLoading && <Spinner className="size-3" />}{t("views:debug.more")}</Button>}
      {page?.coverage.retention_days != null && <p className="pb-1 type-footnote text-muted-foreground">{t("views:debug.retention", { days: num(page.coverage.retention_days) })}</p>}
    </section>
  </div>
}

function CurrentBlockers({ snapshot }: { snapshot: DebugSnapshot }) {
  const t = useT()
  const state = snapshot.state, blockers = state?.lifecycle?.blockers, deps = state?.dependencies, lane = state?.lane
  // The domain view is an evidence projection; missing data must never become "healthy".
  return <div className="space-y-2 border-t border-foreground/6 pt-3" data-debug-blockers>
    <h4 className="type-label text-tertiary">{t("views:debug.blockers")}</h4>
    {blockers ? blockers.length ? <ul className="space-y-1 type-footnote">{blockers.map((blocker, n) => <li key={`${blocker.kind}:${n}`} className="break-words">{blocker.label}<span className="ml-2 type-footnote font-mono text-muted-foreground">{blocker.kind}</span></li>)}</ul> : <p className="type-footnote text-muted-foreground">{t("views:debug.noBlocker")}</p> : <p className="type-footnote text-muted-foreground">{t("views:debug.noState")}</p>}
    {!!deps?.open_dependencies?.length && <p className="type-footnote">{t("views:debug.dependencies", { items: deps.open_dependencies.join(" · ") })}</p>}
    {deps?.resume_mode && <p className="type-footnote text-muted-foreground">{t("views:debug.resumeMode", { mode: deps.resume_mode === "automatic" ? t("views:debug.resumeAutomatic") : deps.resume_mode === "notify" ? t("views:debug.resumeNotify") : deps.resume_mode, source: deps.resume_source ?? t("views:debug.unknown") })}</p>}
    {deps?.resume_blocker && <p className="break-words type-footnote text-attention">{deps.resume_blocker}</p>}
    {lane?.summary && <p className="break-words type-footnote">{lane.summary}</p>}
    {state?.changed_during_read && <p role="status" className="type-footnote text-attention">{t("views:debug.stateChanged")}</p>}
    {state && <details><summary className="cursor-pointer type-footnote text-muted-foreground">{t("views:debug.snapshotDetails")}</summary><pre className="mt-2 max-h-72 overflow-y-auto whitespace-pre-wrap break-all type-footnote font-mono text-muted-foreground">{json(state)}</pre></details>}
  </div>
}

function EventRow({ item, event }: { item: string; event: DebugEvent }) {
  const t = useT()
  const [copied, setCopied] = useState(false)
  const [copyError, setCopyError] = useState(false)
  const copy = async () => {
    setCopyError(false)
    try {
      if (!navigator.clipboard?.writeText) throw new Error("clipboard unavailable")
      await navigator.clipboard.writeText(json({ item_id: item, event_id: event.event_id, at: event.at, kind: event.kind, reason_code: event.reason_code, session_id: event.session_id, attempt_id: event.attempt_id, operation_id: event.operation_id, decision_id: event.decision_id, queued_id: event.queued_id, cause_id: event.cause_id }))
      setCopied(true)
    } catch { setCopyError(true) }
  }
  return <li className="rounded-xl bg-muted/50 px-3.5 py-3 anim-in" data-debug-event={event.event_id}>
    <div className="flex flex-wrap items-start gap-x-3 gap-y-1">
      <span className={cn("break-all type-footnote font-mono", event.severity === "error" ? "text-destructive" : event.severity === "warning" ? "text-attention" : "text-foreground")}>{event.kind}</span>
      <time className="type-footnote tabular-nums text-muted-foreground" dateTime={new Date(event.at).toISOString()}>{time(event.at)}</time>
      <Button size="icon-xs" variant="ghost" className="ml-auto shrink-0" aria-label={t("views:debug.copy")} onClick={copy} data-debug-copy>{copied ? <CheckIcon /> : <CopyIcon />}</Button>
    </div>
    <p className="mt-1 break-all type-footnote font-mono text-muted-foreground">{event.reason_code || t("views:debug.noReason")}</p>
    <div className="mt-1 flex flex-wrap gap-x-3 gap-y-1 type-footnote font-mono text-muted-foreground">{event.session_id && <span className="break-all">{event.session_id}</span>}{event.attempt_id && <span className="break-all">{event.attempt_id}</span>}</div>
    <details className="mt-2" data-debug-detail>
      <summary className="cursor-pointer type-footnote text-muted-foreground hover:text-foreground">{t("views:debug.evidence")}</summary>
      <pre className="mt-2 max-h-72 overflow-y-auto whitespace-pre-wrap break-all rounded-lg bg-background/70 px-3 py-2 type-footnote font-mono">{json(event)}</pre>
    </details>
    {copyError && <p role="alert" className="mt-1 type-footnote text-destructive">{t("views:debug.copyFailed")}</p>}
  </li>
}
