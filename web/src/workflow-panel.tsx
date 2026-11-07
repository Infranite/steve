import { useState } from "react"
import { ChevronDownIcon, CircleCheckIcon, GitBranchIcon, PauseIcon, PlayIcon, RotateCcwIcon } from "lucide-react"
import { api } from "@/api"
import { Button } from "@/components/ui/button"
import { useT } from "@/i18n"
import { cn } from "@/lib/utils"
import { loadItem, st } from "@/store"
import { orderedUnits, workflowProgress, type Workflow } from "@/workflow"

export function WorkflowPanel({ item, workflow }: { item: string; workflow: Workflow }) {
  const t = useT()
  const [busy, setBusy] = useState(false), [error, setError] = useState("")
  const progress = workflowProgress(workflow)
  const unavailable = workflow.execution_ready === false
  const labels: Record<string, string> = {
    pending: t("views:workflow.pending"), dispatching: t("views:workflow.ready"),
    running: t("views:workflow.running"), succeeded: t("views:workflow.passed"),
    failed: t("views:workflow.failed"), cancelled: t("views:workflow.cancelled"),
    canceled: t("views:workflow.cancelled"), blocked: t("views:workflow.blocked"),
    paused: t("views:workflow.paused"),
  }
  const kinds = { work: t("views:workflow.work"), review: t("views:workflow.review"), integrate: t("views:workflow.integrate") }
  const change = async (action: "pause" | "resume" | "retry", unit_id?: string) => {
    setBusy(true); setError("")
    try {
      await api.workflowChange(item, {
        action, unit_id, expected_revision: workflow.revision,
        reason: t(`views:workflow.reason.${action}`),
      })
      await loadItem(item, true)
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e))
      // A conflicting revision should show the newer graph before the user retries.
      await loadItem(item, true)
    } finally { setBusy(false) }
  }
  return <section data-workflow className="min-w-0">
    <div className="mb-3 flex flex-wrap items-start justify-between gap-3">
      <div>
        <h2 className="flex items-center gap-2 type-headline"><GitBranchIcon className="size-4" />{t("views:workflow.title")}</h2>
        <p className="mt-1 type-footnote text-muted-foreground">{t("views:workflow.progress", { done: progress.passed, total: progress.total, running: progress.running, max: workflow.max_parallel, revision: workflow.revision })}</p>
      </div>
      <Button size="sm" variant="outline" disabled={busy || workflow.paused && unavailable} onClick={() => void change(workflow.paused ? "resume" : "pause")} data-workflow-toggle>
        {workflow.paused ? <PlayIcon /> : <PauseIcon />}{workflow.paused ? t("views:workflow.resume") : t("views:workflow.pause")}
      </Button>
    </div>
    {error && <p role="alert" className="mb-3 break-words type-callout text-destructive">{error}</p>}
    {unavailable && <p className="mb-3 rounded-xl border p-3 type-callout text-muted-foreground" role="status" data-workflow-unavailable>{t("views:workflow.unavailable")}</p>}
    {workflow.paused && !unavailable && <p className="mb-3 type-callout text-muted-foreground">{t("views:workflow.pauseHint")}</p>}
    {workflow.blockers.length > 0 && <div className="mb-4 rounded-xl border bg-muted/40 p-3" data-workflow-blockers>
      <p className="type-callout font-medium">{t("views:workflow.blockers")}</p>
      <ul className="mt-1 list-disc space-y-1 pl-4 type-footnote text-muted-foreground">{workflow.blockers.map((text, n) => <li key={n} className="break-words">{text}</li>)}</ul>
    </div>}
    <ol className="divide-y rounded-xl border px-3">
      {orderedUnits(workflow.units).map((unit) => <li key={unit.id} className="min-w-0 py-3" data-workflow-unit={unit.id}>
        <div className="flex flex-wrap items-center gap-x-2 gap-y-1 type-footnote">
          <span className={cn("rounded-md px-1.5 py-0.5", unit.status === "succeeded" ? "bg-muted text-foreground" : unit.status === "failed" ? "bg-destructive/10 text-destructive" : "bg-muted text-muted-foreground")}>{unit.status === "pending" && unavailable ? t("views:workflow.unavailableUnit") : labels[unit.status] ?? unit.status}</span>
          <span className="text-muted-foreground">{kinds[unit.kind]}</span>
          <span className="text-muted-foreground">{st.bots[unit.bot_id]?.n ?? unit.bot_id}</span>
          {!unit.required && <span className="text-muted-foreground">{t("views:workflow.optional")}</span>}
          <span className="ml-auto text-muted-foreground">{t("views:workflow.attempt", { n: unit.attempt, max: workflow.max_attempts })}</span>
        </div>
        <div className="mt-1 flex items-start justify-between gap-2">
          <h3 className="min-w-0 break-words type-callout font-medium">{unit.item_id ? <a href={`#/items/${encodeURIComponent(unit.item_id)}`} className="hover:underline">{unit.title}</a> : unit.title}</h3>
          {unit.status === "failed" && <Button size="sm" variant="ghost" disabled={busy || unavailable || unit.attempt >= workflow.max_attempts} onClick={() => void change("retry", unit.id)} data-workflow-retry={unit.id}><RotateCcwIcon />{t("views:workflow.retry")}</Button>}
        </div>
        {unit.depends_on.length > 0 && <p className="mt-1 break-words type-footnote text-muted-foreground">{t("views:workflow.depends", { names: unit.depends_on.map((id) => workflow.units.find((u) => u.id === id)?.title ?? id).join(" · ") })}</p>}
        {unit.resources.length > 0 && <p className="mt-1 break-words type-footnote text-muted-foreground">{t("views:workflow.resources", { names: unit.resources.join(" · ") })}</p>}
        {unit.reason && <p className="mt-1 break-words type-footnote text-muted-foreground">{unit.reason}</p>}
        <details className="mt-2 group" data-workflow-details>
          <summary className="flex cursor-pointer list-none items-center gap-1 type-footnote text-muted-foreground hover:text-foreground"><ChevronDownIcon className="size-3 transition-transform group-open:rotate-180" />{t("views:workflow.details")}</summary>
          <div className="mt-2 space-y-2 break-words type-footnote">
            <p className="whitespace-pre-wrap text-muted-foreground">{unit.goal}</p>
            {unit.summary && <p className="whitespace-pre-wrap">{unit.summary}</p>}
            {!!unit.outputs?.length && <div data-workflow-outputs><p className="font-medium">{t("views:workflow.outputs")}</p>{unit.outputs.map((output, n) => <div key={n} className="mt-1"><span>{output.title}</span>{output.location && <p className="break-all font-mono text-muted-foreground">{output.location}</p>}</div>)}</div>}
            {!!unit.checks?.length && <ul className="space-y-1">{unit.checks.map((check, n) => <li key={n} className={cn("flex items-start gap-1.5", !check.passed && "text-destructive")}><CircleCheckIcon className="mt-0.5 size-3 shrink-0" /><span>{check.name} · {check.passed ? t("views:workflow.checkPassed") : t("views:workflow.checkFailed")}{check.operated && ` · ${t("views:workflow.operated")}`}</span></li>)}</ul>}
          </div>
        </details>
      </li>)}
    </ol>
    {workflow.history.length > 0 && <details className="mt-3">
      <summary className="cursor-pointer type-footnote text-muted-foreground">{t("views:workflow.history", { count: workflow.history.length })}</summary>
      <ol className="mt-2 space-y-2 border-l pl-3 type-footnote">{workflow.history.slice().reverse().map((entry, n) => <li key={n} className="break-words"><span className="text-muted-foreground">{t("views:workflow.revision", { n: entry.revision })} · </span>{entry.reason}</li>)}</ol>
    </details>}
  </section>
}
