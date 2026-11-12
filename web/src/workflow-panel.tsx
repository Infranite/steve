import { useState } from "react"
import { ChevronDownIcon, CircleCheckIcon, GitBranchIcon, PauseIcon, PlayIcon, RotateCcwIcon } from "lucide-react"
import { api } from "@/api"
import { Button } from "@/components/ui/button"
import { AlertDialog, AlertDialogAction, AlertDialogCancel, AlertDialogContent, AlertDialogDescription, AlertDialogFooter, AlertDialogHeader, AlertDialogTitle } from "@/components/ui/alert-dialog"
import { useT } from "@/i18n"
import { cn } from "@/lib/utils"
import { loadItem, st } from "@/store"
import { BotAvatar } from "@/ui"
import { orderedUnits, workflowProgress, type Workflow } from "@/workflow"

export function WorkflowPanel({ item, workflow }: { item: string; workflow: Workflow }) {
  const t = useT()
  const [busy, setBusy] = useState(false), [error, setError] = useState(""), [confirmSingle, setConfirmSingle] = useState<number | null>(null), [rework, setRework] = useState<string | null>(null)
  const single = workflow.acceptance_mode === "single_session"
  const progress = workflowProgress(workflow)
  const unavailable = workflow.execution_ready === false
  const parent = st.items.find((i) => i.id === item)
  const waitingCards = workflow.units.flatMap((unit) => {
    const child = st.items.find((i) => i.id === unit.item_id)
    return child?.waits.length ? [{ unit, child, count: child.waits.length }] : []
  })
  const waitingCount = waitingCards.reduce((n, row) => n + row.count, 0)
  const affected = rework ? (() => {
    const ids = new Set([rework])
    let changed = true
    while (changed) { changed = false; for (const unit of workflow.units) if (!ids.has(unit.id) && unit.depends_on.some((id) => ids.has(id))) { ids.add(unit.id); changed = true } }
    return orderedUnits(workflow.units).filter((unit) => ids.has(unit.id))
  })() : []
  const activeAffected = affected.find((unit) => ["running", "dispatching"].includes(unit.status))
  const stoppingAffected = affected.some((unit) => unit.stop_pending)
  const otherActive = rework !== null && workflow.units.some((unit) => !affected.some((row) => row.id === unit.id) && ["running", "dispatching"].includes(unit.status))
  const labels: Record<string, string> = {
    pending: t("views:workflow.notStarted"), dispatching: t("views:workflow.dispatched"),
    running: t("views:workflow.activeUnconfirmed"), succeeded: t("views:workflow.passed"),
    failed: t("views:workflow.failed"), cancelled: t("views:workflow.cancelled"),
    canceled: t("views:workflow.cancelled"), blocked: t("views:workflow.blocked"),
    paused: t("views:workflow.paused"),
  }
  const kinds = { work: t("views:workflow.work"), review: t("views:workflow.review"), integrate: t("views:workflow.integrate") }
  const change = async (action: "pause" | "resume" | "retry" | "cancel" | "single_session", unit_id?: string, revision = workflow.revision, reason?: string) => {
    setBusy(true); setError("")
    try {
      await api.workflowChange(item, {
        action, unit_id, expected_revision: revision,
        reason: reason ?? t(`views:workflow.reason.${action}`),
      })
      await loadItem(item, true)
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e))
      // A conflicting revision should show the newer graph before the user retries.
      await loadItem(item, true)
    } finally { setBusy(false) }
  }
  const unitStatus = (unit: Workflow["units"][number]) => unit.status === "running" && unit.activity?.state === "confirmed" ? t("views:workflow.running") : labels[unit.status] ?? unit.status
  const waitingFor = (unit: Workflow["units"][number]) => unit.waiting_on ?? unit.depends_on.filter((id) => workflow.units.find((u) => u.id === id)?.status !== "succeeded")
  return <section data-workflow className="min-w-0">
    <div data-workflow-overview>
    <div className="mb-3 flex flex-wrap items-start justify-between gap-3">
      <div>
        <h2 className="flex items-center gap-2 type-headline"><GitBranchIcon className="size-4" />{t("views:workflow.title")}</h2>
        <p className="mt-1 flex flex-wrap items-center gap-1.5 break-words type-footnote text-muted-foreground"><BotAvatar k={parent?.b ?? null} size={18} />{t("views:workflow.coordinator", { name: parent ? st.bots[parent.b]?.n ?? parent.b : item })}{parent && st.bots[parent.b]?.role && <span>· {st.bots[parent.b].role}</span>}</p>
        <p className="mt-1 type-footnote text-muted-foreground">{t("views:workflow.progressSummary", { done: progress.passed, total: progress.total, confirmed: progress.running, active: progress.active_unconfirmed, waiting: progress.waiting, max: workflow.max_parallel })}</p>
      </div>
      {!single && <div className="flex flex-wrap gap-2">
        <Button size="sm" variant="outline" disabled={busy || workflow.paused && (unavailable || workflow.current_cycle === false)} onClick={() => void change(workflow.paused ? "resume" : "pause")} data-workflow-toggle>
          {workflow.paused ? <PlayIcon /> : <PauseIcon />}{workflow.paused ? t("views:workflow.resume") : t("views:workflow.pause")}
        </Button>
        {workflow.paused && <Button size="sm" variant="outline" disabled={busy} onClick={() => setConfirmSingle(workflow.revision)} data-workflow-single>{t("views:workflow.single")}</Button>}
      </div>}
    </div>
    {waitingCount > 0 && <div className="mb-3 rounded-xl border border-attention/40 bg-attention/5 p-3" data-workflow-decisions>
      <p className="type-callout font-medium">{t("views:workflow.decisions", { count: waitingCount, units: waitingCards.length })}</p>
      <div className="mt-2 flex flex-wrap gap-2">{waitingCards.map(({ unit, child }) => <a key={unit.id} className="rounded-lg border bg-background px-3 py-2 type-footnote hover:bg-accent" href={`#/items/${encodeURIComponent(child.id)}`}>{unit.title} · {t("views:workflow.handleDecision")}</a>)}</div>
    </div>}
    <AlertDialog open={confirmSingle !== null} onOpenChange={(open) => !open && setConfirmSingle(null)}>
      <AlertDialogContent className="max-h-[calc(100dvh-2rem)] w-[calc(100%-2rem)] grid-rows-[minmax(0,1fr)_auto] [overflow-wrap:anywhere]" data-workflow-single-confirm>
        <AlertDialogHeader className="min-h-0 min-w-0 overflow-y-auto">
          <AlertDialogTitle>{t("views:workflow.single")}</AlertDialogTitle>
          <AlertDialogDescription>{t("views:workflow.singleConfirm")}</AlertDialogDescription>
        </AlertDialogHeader>
        <AlertDialogFooter className="min-w-0 *:h-auto *:min-h-8 *:whitespace-normal">
          <AlertDialogCancel disabled={busy}>{t("common:action.cancel")}</AlertDialogCancel>
          <AlertDialogAction disabled={busy} onClick={() => { const revision = confirmSingle; setConfirmSingle(null); if (revision !== null) void change("single_session", undefined, revision) }} data-workflow-single-ok>{t("views:workflow.single")}</AlertDialogAction>
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
    <AlertDialog open={rework !== null} onOpenChange={(open) => !open && setRework(null)}>
      <AlertDialogContent data-workflow-rework className="max-h-[calc(100dvh-2rem)] w-[calc(100%-2rem)] overflow-y-auto [overflow-wrap:anywhere]">
        <AlertDialogHeader><AlertDialogTitle>{t("views:workflow.reworkTitle")}</AlertDialogTitle><AlertDialogDescription>{t("views:workflow.reworkDesc")}</AlertDialogDescription></AlertDialogHeader>
        <ul className="list-disc space-y-1 pl-5 type-footnote">{affected.map((unit) => <li key={unit.id}>{unit.title} · {labels[unit.status] ?? unit.status} · {t("views:workflow.budget", { n: unit.attempt, max: workflow.max_attempts })}</li>)}</ul>
        {stoppingAffected && <p className="type-footnote text-attention">{t("views:workflow.stopPending")}</p>}
        {otherActive && !activeAffected && <p className="type-footnote text-attention">{t("views:workflow.otherActive")}</p>}
        <AlertDialogFooter><AlertDialogCancel disabled={busy}>{t("common:action.cancel")}</AlertDialogCancel>
          <AlertDialogAction disabled={busy || stoppingAffected || otherActive && !activeAffected || workflow.current_cycle === false || affected.some((unit) => unit.attempt >= workflow.max_attempts)} onClick={() => {
            const id = activeAffected?.id ?? rework
            if (!id) return
            void change(activeAffected ? "cancel" : "retry", id, workflow.revision, t(activeAffected ? "views:workflow.reason.cancel" : "views:workflow.reason.rework"))
            if (!activeAffected) setRework(null)
          }} data-workflow-rework-confirm>{activeAffected ? t("views:workflow.stopAffected") : t("views:workflow.confirmRework")}</AlertDialogAction>
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
    {single && <p className="mb-3 rounded-xl border p-3 type-callout text-muted-foreground" data-workflow-single-mode>{t("views:workflow.singleHint")}</p>}
    {error && <p role="alert" className="mb-3 break-words type-callout text-destructive">{error}</p>}
    {unavailable && !single && <p className="mb-3 rounded-xl border p-3 type-callout text-muted-foreground" role="status" data-workflow-unavailable>{t("views:workflow.unavailable")}</p>}
    {workflow.current_cycle === false && <p className="mb-3 type-callout text-muted-foreground" role="status" data-workflow-prior-cycle>{t("views:workflow.priorCycle")}</p>}
    {workflow.paused && workflow.current_cycle !== false && !single && !unavailable && <p className="mb-3 type-callout text-muted-foreground">{t("views:workflow.pauseHint")}</p>}
    {workflow.blockers.length > 0 && <div className="mb-4 rounded-xl border bg-muted/40 p-3" data-workflow-blockers>
      <p className="type-callout font-medium">{t("views:workflow.blockers")}</p>
      <ul className="mt-1 list-disc space-y-1 pl-4 type-footnote text-muted-foreground">{workflow.blockers.map((text, n) => <li key={n} className="break-words">{text}</li>)}</ul>
    </div>}
    </div>
    <div data-workflow-current>
      <h3 className="mb-2 type-callout font-medium">{t("views:workflow.currentWork")}</h3>
    <ol className="divide-y rounded-xl border px-3">
      {orderedUnits(workflow.units).map((unit) => <li key={unit.id} className="min-w-0 py-3" data-workflow-unit={unit.id}>
        <div className="flex flex-wrap items-center gap-x-2 gap-y-1 type-footnote">
          <span className={cn("rounded-md px-1.5 py-0.5", unit.status === "succeeded" ? "bg-muted text-foreground" : unit.status === "failed" ? "bg-destructive/10 text-destructive" : "bg-muted text-muted-foreground")}>{unit.status === "pending" && unavailable ? t("views:workflow.unavailableUnit") : unitStatus(unit)}</span>
          <span className="text-muted-foreground">{kinds[unit.kind]}</span>
          {workflow.max_parallel > 1 && ["running", "dispatching"].includes(unit.status) && workflow.units.filter((other) => ["running", "dispatching"].includes(other.status)).length > 1 && <span className="text-muted-foreground">{t("views:workflow.parallel")}</span>}
          <span className="inline-flex items-center gap-1 text-muted-foreground"><BotAvatar k={unit.bot_id} size={16} />{st.bots[unit.bot_id]?.n ?? unit.bot_id}</span>
          {st.bots[unit.bot_id]?.on === false && <span className="text-attention">{t("views:workflow.botDisabled")}</span>}
          {!unit.required && <span className="text-muted-foreground">{t("views:workflow.optional")}</span>}
          <span className="ml-auto text-muted-foreground">{unit.attempt > 0 && <>{t("views:workflow.round", { n: unit.attempt })} · </>}{t("views:workflow.budget", { n: unit.attempt, max: workflow.max_attempts })}</span>
        </div>
        <div className="mt-1 flex items-start justify-between gap-2">
          <h3 className="min-w-0 break-words type-callout font-medium">{unit.item_id ? <a href={`#/items/${encodeURIComponent(unit.item_id)}`} className="hover:underline">{unit.title}</a> : unit.title}</h3>
          {!single && ["running", "dispatching"].includes(unit.status) && <Button size="sm" variant="ghost" className="min-h-11 sm:min-h-8" disabled={busy || workflow.current_cycle === false} onClick={() => void change("cancel", unit.id)} data-workflow-cancel={unit.id}>{t("views:workflow.cancelUnit")}</Button>}
          {!single && unit.status === "succeeded" && <Button size="sm" variant="ghost" className="min-h-11 sm:min-h-8" disabled={busy || workflow.current_cycle === false || unit.attempt >= workflow.max_attempts} onClick={() => setRework(unit.id)} data-workflow-rework={unit.id}>{t("views:workflow.rework")}</Button>}
          {!single && unit.status === "failed" && <Button size="sm" variant="ghost" disabled={busy || unavailable || workflow.current_cycle === false || unit.attempt >= workflow.max_attempts} onClick={() => void change("retry", unit.id)} data-workflow-retry={unit.id}><RotateCcwIcon />{t("views:workflow.retry")}</Button>}
        </div>
        {["pending", "blocked"].includes(unit.status) && waitingFor(unit).length > 0 && <p className="mt-1 break-words type-footnote text-muted-foreground">{t("views:workflow.waitingFor", { names: waitingFor(unit).map((id) => workflow.units.find((u) => u.id === id)?.title ?? id).join(" · ") })}</p>}
        {(unit.status === "running" || unit.status === "dispatching") && unit.activity?.state !== "confirmed" && <p className="mt-1 type-footnote text-muted-foreground">{unit.activity?.reason ?? t("views:workflow.activityUnknown")}</p>}
        {!!unit.summary && ["running", "dispatching"].includes(unit.status) && <p className="mt-1 line-clamp-2 break-words type-footnote text-muted-foreground">{unit.summary}</p>}
        {unit.stop_pending && <p className="mt-1 type-footnote text-attention">{t("views:workflow.stopPending")}</p>}
        {workflow.paused && !["succeeded", "failed", "cancelled", "canceled"].includes(unit.status) && <p className="mt-1 type-footnote text-muted-foreground">{t("views:workflow.pausedUnit")}</p>}
        {unit.resources.length > 0 && <p className="mt-1 break-words type-footnote text-muted-foreground">{t("views:workflow.resources", { names: unit.resources.join(" · ") })}</p>}
        {unit.reason && <p className="mt-1 break-words type-footnote text-muted-foreground">{unit.reason}</p>}
        <details className="mt-2 group" data-workflow-details>
          <summary className="flex cursor-pointer list-none items-center gap-1 type-footnote text-muted-foreground hover:text-foreground"><ChevronDownIcon className="size-3 transition-transform group-open:rotate-180" />{t("views:workflow.details")}</summary>
          <div className="mt-2 space-y-2 break-words type-footnote">
            <div className="flex flex-wrap gap-2">{unit.item_id && <a className="underline underline-offset-2" href={`#/items/${encodeURIComponent(unit.item_id)}`}>{t("views:workflow.openChild")}</a>}{unit.session_id && <a className="underline underline-offset-2" href={`#/sessions/${encodeURIComponent(unit.session_id)}`}>{t("views:workflow.openSession")}</a>}</div>
            <p className="whitespace-pre-wrap text-muted-foreground">{unit.goal}</p>
            {unit.summary && <p className="whitespace-pre-wrap">{unit.summary}</p>}
            {!!unit.outputs?.length && <div data-workflow-outputs><p className="font-medium">{t("views:workflow.outputs")}</p>{unit.outputs.map((output, n) => <div key={n} className="mt-1"><span>{output.title}</span>{output.location && <p className="break-all font-mono text-muted-foreground">{output.location}</p>}</div>)}</div>}
            {!!unit.checks?.length && <ul className="space-y-1">{unit.checks.map((check, n) => <li key={n} className={cn("flex items-start gap-1.5", !check.passed && "text-destructive")}><CircleCheckIcon className="mt-0.5 size-3 shrink-0" /><span>{check.name} · {check.passed ? t("views:workflow.checkPassed") : t("views:workflow.checkFailed")}{check.operated && ` · ${t("views:workflow.operated")}`}</span></li>)}</ul>}
          </div>
        </details>
      </li>)}
    </ol>
    </div>
    <div className="mt-4" data-workflow-history>
      <h3 className="type-callout font-medium">{t("views:workflow.deliveryHistory")}</h3>
      {!!workflow.history_attempts?.length && <div className="mt-2 space-y-2">{workflow.history_attempts.map((old) => <div key={old.item_id} className="rounded-lg border p-2 type-footnote"><span className="text-muted-foreground">{t("views:workflow.priorAttempt")}</span> · {old.snapshot.title} · <a className="underline underline-offset-2" href={`#/items/${encodeURIComponent(old.item_id)}`}>{old.item_id}</a>{old.snapshot.session_id && <> · <a className="underline underline-offset-2" href={`#/sessions/${encodeURIComponent(old.snapshot.session_id)}`}>{t("views:workflow.openSession")}</a></>}{old.snapshot.reason && <p className="mt-1 text-muted-foreground">{old.snapshot.reason}</p>}</div>)}</div>}
      {!!workflow.history_attempts_total && workflow.history_attempts_total > (workflow.history_attempts?.length ?? 0) && <p className="mt-2 type-footnote text-muted-foreground">{t("views:workflow.historyAttemptsShown", { shown: workflow.history_attempts?.length ?? 0, total: workflow.history_attempts_total })}</p>}
    {workflow.history.length > 0 && <details className="mt-3">
      <summary className="cursor-pointer type-footnote text-muted-foreground">{t("views:workflow.history", { count: workflow.history_total ?? workflow.history.length })}</summary>
      <ol className="mt-2 space-y-2 border-l pl-3 type-footnote">{workflow.history.slice().reverse().map((entry, n) => <li key={n} className="break-words"><span className="text-muted-foreground">{t("views:workflow.revision", { n: entry.revision })} · </span>{entry.reason}</li>)}</ol>
    </details>}
    </div>
  </section>
}
