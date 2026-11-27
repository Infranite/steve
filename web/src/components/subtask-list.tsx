/* Unified view for subagents, background tasks and harness-native workflows inside a session turn.
 * Renders only what the harness adapter actually reported — unknown usage stays omitted, and Cancel
 * is shown only when the adapter advertises per-subtask cancellation (`can_cancel`). */
import { useMemo, useState } from "react"
import { BotIcon, ChevronDownIcon, SquareIcon, TerminalIcon, WorkflowIcon } from "lucide-react"
import { api, type RSubtaskOrigin, type RSubtaskStatus, type RSubtaskUsage, type RSubtaskView } from "@/api"
import { ParallelAttempts, type Attempt, type AttemptStatus, type PacLabels } from "@/components/affect/parallel-attempts-clean"
import { Markdown } from "@/components/markdown"
import { Button } from "@/components/ui/button"
import { Trail, useTick, type TrailItem } from "@/components/work-trail"
import { isActiveSubtaskStatus } from "@/data/transcript-model"
import { useT } from "@/i18n"
import { clockDur, msDur } from "@/i18n/format"
import { fmtN, fmtUsd } from "@/stats/format"
import { toast } from "@/store"

export type SubtaskEntry = {
  subtask: RSubtaskView
  started?: number
  ended?: number
  children: TrailItem[]
  latestChildLabel?: string
}

const SEC = 1000
const fmtTime = (ms: number) => (ms < 10 * SEC ? msDur(Math.max(0, ms)) : clockDur(Math.max(0, ms)))
const clip = (s: string | null | undefined, n = 120) => {
  if (!s) return undefined
  const one = s.replace(/```[\s\S]*?```/g, " ").replace(/[`*_>#|]/g, "").replace(/\s+/g, " ").trim()
  if (!one) return undefined
  return one.length > n ? one.slice(0, n) + "…" : one
}

function originIcon(origin: RSubtaskOrigin) {
  if (origin === "background_task") return <TerminalIcon className="size-3.5" strokeWidth={1.75} aria-hidden />
  if (origin === "native_workflow") return <WorkflowIcon className="size-3.5" strokeWidth={1.75} aria-hidden />
  return <BotIcon className="size-3.5" strokeWidth={1.75} aria-hidden />
}

function attemptStatus(status: RSubtaskStatus, live: boolean): AttemptStatus {
  if (status === "running") return live ? "running" : "stopped"
  if (status === "needs_you") return "waiting"
  if (status === "queued") return "queued"
  if (status === "done") return "done"
  if (status === "failed") return "failed"
  return "stopped"
}

function costUsdOf(u: RSubtaskUsage | null | undefined): number | undefined {
  if (!u?.cost || u.cost.currency !== "USD") return undefined
  const n = Number(u.cost.decimal)
  return Number.isFinite(n) && n > 0 ? n : undefined
}

function usageTotals(u: RSubtaskUsage | null | undefined): { totalTokens?: number; costUsd?: number; contextPct?: number } {
  if (!u) return {}
  const hasParts = u.input != null || u.output != null || u.cache_read != null || u.cache_write != null || u.reasoning != null
  const sumParts = hasParts
    ? (u.input ?? 0) + (u.output ?? 0) + (u.cache_read ?? 0) + (u.cache_write ?? 0) + (u.reasoning ?? 0)
    : undefined
  const totalTokens = u.total_tokens != null ? u.total_tokens : sumParts
  const costUsd = costUsdOf(u)
  const contextPct = u.context_used != null && u.context_limit != null && u.context_limit > 0
    ? Math.min(100, Math.round((u.context_used / u.context_limit) * 100))
    : undefined
  return { totalTokens, costUsd, contextPct }
}

function formatUsageCompact(u: RSubtaskUsage | null | undefined, t: ReturnType<typeof useT>): string {
  const { totalTokens, costUsd, contextPct } = usageTotals(u)
  if (totalTokens && totalTokens > 0 && costUsd) return `${fmtN(totalTokens)} · ${fmtUsd(costUsd)}`
  if (totalTokens && totalTokens > 0) return t("core:subtask.tokens", { n: fmtN(totalTokens) })
  if (costUsd) return fmtUsd(costUsd)
  if (contextPct != null) return t("core:subtask.contextPct", { pct: contextPct })
  return ""
}

function usageDetails(u: RSubtaskUsage | null | undefined, t: ReturnType<typeof useT>): string[] {
  if (!u) return []
  const parts: string[] = []
  if (u.input != null) parts.push(t("core:subtask.inTokens", { n: fmtN(u.input) }))
  if (u.output != null) parts.push(t("core:subtask.outTokens", { n: fmtN(u.output) }))
  if (u.cache_read != null && u.cache_read > 0) parts.push(t("core:subtask.cacheTokens", { n: fmtN(u.cache_read) }))
  const usd = costUsdOf(u)
  if (usd != null) parts.push(fmtUsd(usd))
  if (u.context_used != null && u.context_limit != null && u.context_limit > 0) {
    parts.push(t("core:subtask.contextUsed", { used: fmtN(u.context_used), size: fmtN(u.context_limit) }))
  }
  return parts
}

function hasDetails(e: SubtaskEntry): boolean {
  return e.children.length > 0 || !!e.subtask.summary || !!e.subtask.error || !!e.subtask.usage
}

export function SubtaskList({
  sessionId,
  entries,
  live,
  readOnly = false,
}: {
  sessionId: string
  entries: SubtaskEntry[]
  live: boolean
  readOnly?: boolean
}) {
  const t = useT()
  const anyActive = live && entries.some((e) => isActiveSubtaskStatus(e.subtask.status))
  const now = useTick(anyActive)
  const defaultOpenId = useMemo(() => entries.find((e) => e.children.length > 0)?.subtask.id ?? null, [entries])
  const [selected, setSelected] = useState<string | null>(() => defaultOpenId)
  const [cancelling, setCancelling] = useState<Record<string, boolean>>({})

  const handleCancel = async (subtaskId: string) => {
    setCancelling((m) => ({ ...m, [subtaskId]: true }))
    try {
      await api.cancelSubtask(sessionId, subtaskId)
    } catch (err) {
      toast((err as Error).message || t("core:error.generic"))
    } finally {
      setCancelling((m) => { const next = { ...m }; delete next[subtaskId]; return next })
    }
  }

  const labels: Partial<PacLabels> = useMemo(() => ({
    working: t("core:subtask.status.running"),
    list: t("core:subtask.list"),
    status: {
      running: t("core:subtask.status.running"),
      done: t("core:subtask.status.done"),
      failed: t("core:subtask.status.failed"),
      waiting: t("core:subtask.status.needs_you"),
      queued: t("core:subtask.status.queued"),
      stopped: t("core:subtask.status.cancelled"),
    },
  }), [t])

  const attempts: Attempt[] = entries.map((entry) => {
    const s = entry.subtask
    const active = live && isActiveSubtaskStatus(s.status)
    const status = attemptStatus(s.status, live)
    const originLabel = t(`core:subtask.origin.${s.origin}`)
    const approach = [
      originLabel,
      s.actor && s.actor !== s.title ? s.actor : undefined,
      entry.children.length > 0 ? t("core:subtask.steps", { count: entry.children.length }) : undefined,
    ].filter(Boolean).join(" · ")
    const durMs = s.duration_ms != null && s.duration_ms >= 0
      ? s.duration_ms
      : active && entry.started
        ? Math.max(0, now - entry.started)
        : entry.started && entry.ended && entry.ended >= entry.started
          ? entry.ended - entry.started
          : undefined
    const durText = durMs != null ? fmtTime(durMs) : ""
    const usageText = formatUsageCompact(s.usage, t)
    const canCancel = !readOnly && !!s.can_cancel && active
    return {
      id: s.id,
      name: s.title || originLabel,
      model: s.model ?? undefined,
      approach,
      icon: originIcon(s.origin),
      status,
      progress: status === "done" ? 1 : status === "queued" ? 0 : status === "running" ? 0.55 : 0.75,
      step: s.progress || entry.latestChildLabel || t("core:subtask.status.running"),
      note: s.error || clip(s.summary) || s.progress || t(`core:subtask.status.${s.status}`),
      figures: [durText, usageText],
      action: canCancel ? (
        <Button
          size="xs"
          variant="outline"
          className="shrink-0 whitespace-nowrap"
          disabled={!!cancelling[s.id]}
          data-subtask-cancel={s.id}
          onClick={(ev) => { ev.stopPropagation(); void handleCancel(s.id) }}
        >
          <SquareIcon className="size-3" />
          {cancelling[s.id] ? t("core:subtask.cancelling") : t("core:subtask.cancel")}
        </Button>
      ) : undefined,
      attrs: {
        "data-subtask-id": s.id,
        "data-subtask-origin": s.origin,
        "data-subtask-status": s.status,
      },
    }
  })

  const openEntry = selected ? entries.find((e) => e.subtask.id === selected && hasDetails(e)) : undefined
  const detailUsage = openEntry ? usageDetails(openEntry.subtask.usage, t) : []

  const hasAnyAction = attempts.some((a) => a.action !== undefined)
  return (
    <div className={hasAnyAction ? "my-1 flex min-w-0 flex-col gap-1.5" : "my-1 flex min-w-0 flex-col gap-1.5 [--ui-pac-act-min:0px]"} data-subtasks={entries.length}>
      <ParallelAttempts
        pickable={false}
        header={null}
        attempts={attempts}
        labels={labels}
        formatCost={fmtUsd}
        selected={openEntry?.subtask.id ?? null}
        onSelect={(a) => {
          const entry = entries.find((e) => e.subtask.id === a.id)
          if (!entry || !hasDetails(entry)) return
          setSelected((cur) => (cur === a.id ? null : a.id))
        }}
      />
      {openEntry && (
        <div className="ml-2 rounded-xl border border-border/70 bg-muted/35 px-3.5 py-2.5 anim-in" data-subtask-detail={openEntry.subtask.id}>
          <div className="mb-1.5 flex flex-wrap items-center gap-x-2 gap-y-1 type-footnote text-muted-foreground">
            <span className="font-medium text-foreground/90">{openEntry.subtask.title}</span>
            <span>· {t(`core:subtask.origin.${openEntry.subtask.origin}`)}</span>
            {openEntry.subtask.model && <span className="font-mono">{openEntry.subtask.model}</span>}
            {detailUsage.length > 0 && <span className="tabular-nums" data-subtask-usage>· {detailUsage.join(" · ")}</span>}
            <button
              type="button"
              onClick={() => setSelected(null)}
              className="ml-auto inline-flex items-center gap-0.5 text-muted-foreground hover:text-foreground"
            >
              <ChevronDownIcon className="size-3.5 rotate-180" />
              {t("core:session.collapse")}
            </button>
          </div>
          {openEntry.children.length > 0 && (
            <div className="mb-1.5" data-subtask-children={openEntry.children.length}>
              <Trail items={openEntry.children} live={live && isActiveSubtaskStatus(openEntry.subtask.status)} />
            </div>
          )}
          {openEntry.subtask.error && (
            <div className="rounded-lg bg-destructive/10 px-3 py-2 type-footnote text-destructive" data-subtask-error>
              {openEntry.subtask.error}
            </div>
          )}
          {openEntry.subtask.summary && (
            <div className="max-h-56 overflow-y-auto rounded-lg bg-card/70 px-3 py-2 type-callout text-foreground/90" data-subtask-result>
              <Markdown text={openEntry.subtask.summary} />
            </div>
          )}
        </div>
      )}
    </div>
  )
}
